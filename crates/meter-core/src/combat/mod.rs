//! Combat tracker: entities, names, encounters and statistics. Events in, `Snapshot`s out.
//!
//! API contract (used by the Tauri shell and the replay tool):
//! - `push` consumes parser events in capture order.
//! - `tick` applies time based rules (idle timeout, dummy cap) and is called a few times a second.
//! - `snapshot` renders the live (or last) encounter; `drain_finished` hands finished encounters to history.
//!
//! Rules, in short (docs/DESIGN.md and PROTOCOL.md sections 13 to 15 and 19):
//! - An encounter starts at the first counted damage by a player or a player's summon on an NPC. The
//!   main target is the boss with the most damage taken, else the most damaged target. The first hit
//!   on a boss while the encounter held only trash clears the trash and restarts at that hit.
//! - It ends when the main boss dies, on a map change, when every party player is dead (player deaths
//!   arrive as HP 0 reports on the Global client), when the boss is back at full HP after five seconds
//!   without hits or right after a player death (the server resets the boss within 100 ms of a wipe),
//!   after an idle timeout (instance timeout for a living boss inside an instance, open world timeout
//!   otherwise, ten seconds for dummies), when the dummy cap is reached, or by hand.
//! - Phantom damage guards: only players (identity, roster, the local player, or a player band skill
//!   with no spawn) and their summons deal damage; a spawn always reclassifies an id as a non player
//!   and removes damage counted for a skill inferred player; a weak identity (the owner name of a
//!   summon spawn) never reclassifies a spawned entity; hits on dead or despawned targets, records
//!   between two players without a heal skill, heal skills on any non player target, spirit links and
//!   resource restores are dropped.
//! - Orphan summons (no owner link and no single named player of their class) keep a row under their
//!   own id until a link or identity folds them into the owner; they are never dropped.

mod encounter;
mod entities;
mod stats;
#[cfg(test)]
mod tests;

use crate::gamedata::GameData;
use crate::parser::{DeathKind, EntityId, Event, HitFlags};
use crate::snapshot::{EndReason, Snapshot};
use encounter::{AccKind, Encounter, Hit};
use entities::{DeathOutcome, Kind, Registry, Spawn};
use std::collections::VecDeque;

pub use stats::{describe, format_duration};

/// Dummy encounters end after this long without damage.
const DUMMY_IDLE_MS: u64 = 10_000;
/// Casts this long before the first hit belong to the fight (an opener is cast before it lands).
const CAST_LOOKBACK_MS: u64 = 3_000;
const CAST_BUFFER: usize = 64;
/// A boss back at full HP this long after the last hit means the attempt was reset.
const HP_RESET_QUIET_MS: u64 = 5_000;
/// A boss back at full HP this soon after a player death is a wipe reset too: in session2 Vakron one
/// shot three players at 13:30:39.016 and reported 9 860 000 of 9 860 000 at 13:30:39.166, 100 ms
/// after the party's last hit, with no death record of its own and a same map reload 3 s later.
const HP_RESET_AFTER_DEATH_MS: u64 = 10_000;
/// Trash encounters shorter than this are dropped when a boss pull replaces them.
const MIN_TRASH_KEEP_MS: u64 = 15_000;
/// Finished encounters without a boss shorter than this are not handed to history.
const MIN_FILED_MS: u64 = 5_000;

#[derive(Debug, Clone, PartialEq)]
pub struct TrackerConfig {
    /// No damage for this long ends an encounter in an instance (dungeon).
    pub idle_timeout_instance_ms: u64,
    /// No damage for this long ends an encounter in the open world.
    pub idle_timeout_open_world_ms: u64,
    /// Training dummy encounters end after this long; 0 disables the cap.
    pub dummy_cap_ms: u64,
    /// Gaps between an actor's hits longer than this do not count as active time.
    pub active_gap_ms: u64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self { idle_timeout_instance_ms: 180_000, idle_timeout_open_world_ms: 20_000, dummy_cap_ms: 0, active_gap_ms: 5_000 }
    }
}

/// Counters for the diagnostics page and the replay tool.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct TrackerStats {
    pub damage_counted: u64,
    pub damage_dropped: u64,
    pub damage_taken: u64,
    pub heals: u64,
    pub self_heals: u64,
    pub casts: u64,
    pub inferred_players_removed: u64,
    pub encounters_started: u64,
    pub encounters_finished: u64,
}

/// What an actor is for the purpose of one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Player,
    Summon,
    Npc,
    Unknown,
}

/// The shape of the record an actor is classified for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordKind {
    /// A value record that is not a DoT tick: proof that the actor is alive.
    Direct,
    Dot,
    /// A no value record. Dead players' passives still announce, and an Elementalist announces the
    /// spirit skills its spirit then deals damage with, so a cast proves neither life nor kind.
    Cast,
}

/// A cast kept until an encounter opens that it may belong to.
struct PendingCast {
    t_ms: u64,
    actor: EntityId,
    kind: AccKind,
    skill: u32,
}

pub struct Tracker {
    config: TrackerConfig,
    next_id: u64,
    /// The latest time seen, from events or ticks.
    now_ms: u64,
    registry: Registry,
    live: Option<Encounter>,
    /// The last ended encounter, shown until the next one starts.
    last: Option<Snapshot>,
    finished: Vec<Snapshot>,
    map_id: Option<u32>,
    recent_casts: VecDeque<PendingCast>,
    pub stats: TrackerStats,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new(TrackerConfig::default())
    }
}

impl Tracker {
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            next_id: 1,
            now_ms: 0,
            registry: Registry::default(),
            live: None,
            last: None,
            finished: Vec::new(),
            map_id: None,
            recent_casts: VecDeque::new(),
            stats: TrackerStats::default(),
        }
    }

    pub fn config(&self) -> &TrackerConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: TrackerConfig) {
        self.config = config;
    }

    /// Feeds one decoded event.
    pub fn push(&mut self, event: &Event) {
        let t = event.t_ms();
        self.now_ms = self.now_ms.max(t);
        self.apply_time_rules(t);
        match event {
            Event::Damage { t_ms, actor, target, skill, amount, flags, .. } => self.on_damage(*t_ms, *actor, *target, *skill, *amount, flags),
            Event::Heal { t_ms, actor, target, skill, amount, flags } => self.on_heal(*t_ms, *actor, *target, *skill, *amount, flags),
            Event::Cast { t_ms, actor, skill, .. } => self.on_cast(*t_ms, *actor, *skill),
            Event::Miss { .. } => {}
            Event::Identity { t_ms, id, name, server, job, class_id, level, is_self } => {
                // A record without server, job and level is the owner name of a summon spawn (or a
                // player record the scan could not read past the name): weak evidence, never allowed to
                // reclassify a spawned mob or replace a current name.
                let weak = *server == 0 && *job == 0 && *level == 0 && !*is_self;
                if weak {
                    self.registry.weak_identity(*t_ms, *id, name);
                } else {
                    let class = if *class_id != 0 { Some(*class_id) } else { GameData::get().class_from_job(*job) };
                    self.registry.identity(*t_ms, *id, name, class, *level, *is_self);
                }
            }
            Event::NpcSpawn { t_ms, id, npc_code, kind, cur_hp, max_hp, owner } => self.on_spawn(Spawn {
                t_ms: *t_ms,
                id: *id,
                npc_code: *npc_code,
                kind: *kind,
                cur_hp: *cur_hp,
                max_hp: *max_hp,
                owner: *owner,
            }),
            Event::OwnerLink { t_ms, entity, owner } => self.on_owner_link(*t_ms, *entity, *owner),
            Event::Death { t_ms, id, kind } => self.on_death(*t_ms, *id, *kind),
            Event::RemainHp { t_ms, id, cur_hp, max_hp } => self.on_hp(*t_ms, *id, *cur_hp, *max_hp),
            Event::HpUpdate { t_ms, id, cur_hp, max_hp } => self.on_hp(*t_ms, *id, *cur_hp, Some(*max_hp)),
            Event::ZoneChange { t_ms, map_id } => self.on_zone_change(*t_ms, *map_id),
            Event::Party { members, .. } => self.registry.set_party(members),
            Event::Pong { .. } => {}
        }
        self.registry.sweep(self.now_ms);
    }

    /// Applies time based rules. `now_ms` is on the capture clock (the last packet time, or wall time when
    /// live and idle).
    pub fn tick(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
        let now = self.now_ms;
        self.apply_time_rules(now);
        self.registry.sweep(now);
    }

    /// The live encounter, or the last one if none is active.
    pub fn snapshot(&self) -> Option<Snapshot> {
        match &self.live {
            Some(e) => Some(stats::build(e, &self.registry, true)),
            None => self.last.clone(),
        }
    }

    /// Ends the current encounter by hand (hotkey or button). With nothing live, the last encounter is
    /// cleared from the display.
    pub fn reset(&mut self) {
        if self.live.is_some() {
            let t = self.now_ms;
            self.end_live(EndReason::Manual, t);
        } else {
            self.last = None;
        }
    }

    /// Encounters that ended since the last call, oldest first.
    pub fn drain_finished(&mut self) -> Vec<Snapshot> {
        std::mem::take(&mut self.finished)
    }

    pub fn self_id(&self) -> Option<EntityId> {
        self.registry.self_id()
    }

    /// Feeds the parser's local player hint (`ParserStats::hint_self_id`, the `06 38` skill use vote).
    /// The registry uses it as the self id only until a self identity record arrives, so a fight that
    /// started before the first `33 36` still marks the local player's row as self. Call it after each
    /// packet batch (the hint changes rarely and is `None` after every map load).
    pub fn set_self_hint(&mut self, id: Option<EntityId>) {
        self.registry.set_self_hint(id);
    }

    pub fn self_name(&self) -> Option<&str> {
        self.registry.self_name()
    }

    pub fn next_encounter_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Continues numbering after encounters loaded from history.
    pub fn set_next_encounter_id(&mut self, id: u64) {
        self.next_id = self.next_id.max(id);
    }

    /// The map the player is on, from the last map load.
    pub fn map_id(&self) -> Option<u32> {
        self.map_id
    }

    /// True while an encounter is open.
    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    // Events

    fn on_damage(&mut self, t: u64, actor: EntityId, target: EntityId, skill: u32, amount: u64, flags: &HitFlags) {
        if actor == 0 || target == 0 || amount == 0 {
            self.stats.damage_dropped += 1;
            return;
        }
        let data = GameData::get();
        if entities::is_spirit_to_owner_link(skill) {
            if self.registry.is_player(target) {
                self.registry.owner_link(t, actor, target);
            }
            return;
        }
        if entities::is_owner_to_spirit_link(skill) {
            if self.registry.is_player(actor) {
                self.registry.owner_link(t, target, actor);
            }
            return;
        }
        if data.restores_resource(skill) {
            return;
        }

        let role = self.actor_role(t, actor, skill, if flags.dot { RecordKind::Dot } else { RecordKind::Direct });
        let target_kind = self.registry.kind(target);
        match role {
            Role::Player | Role::Summon => {
                let kind = if role == Role::Player { AccKind::Player } else { AccKind::Summon };
                match target_kind {
                    Kind::Player => {
                        if actor == target {
                            self.stats.self_heals += 1;
                            self.heal(t, actor, kind, skill, amount);
                        } else if data.is_heal_skill(skill) {
                            self.stats.heals += 1;
                            self.heal(t, actor, kind, skill, amount);
                        } else {
                            // A friendly action without a heal skill, or PvP: not damage on a mob.
                            self.stats.damage_dropped += 1;
                        }
                    }
                    Kind::Npc | Kind::Unknown => {
                        if data.is_heal_skill(skill) {
                            // A heal never damages a mob. HoT ticks of Recuperation arrive from `05 38`
                            // as DoT damage with the heal skill id; on a party member whose id is not
                            // classified yet (meter started mid session) they would otherwise count as
                            // damage on "#id" and could even open an encounter. The `04 38` path
                            // already turns these skills into Heal events, so both paths agree now.
                            self.stats.damage_dropped += 1;
                            return;
                        }
                        let dead = self.registry.entity(target).map(|e| e.dead || e.gone).unwrap_or(false);
                        // A dead player's in flight hits land for a moment after its HP 0 report. They
                        // count while the fight runs, but they never open one: after a wipe (session4,
                        // Atiel at 8 percent, every player dead) they would start a two second encounter.
                        let dead_actor = self.live.is_none() && self.registry.entity(actor).is_some_and(|e| e.dead);
                        if dead || dead_actor || actor == target {
                            self.stats.damage_dropped += 1;
                        } else {
                            // A target under fire stays known, however long the fight runs, and stays a
                            // target whatever player band skills it fires later.
                            self.registry.seen(target, t);
                            self.registry.hostile(target, t);
                            self.count_damage(t, actor, kind, target, skill, amount, flags);
                        }
                    }
                    Kind::Summon => self.stats.damage_dropped += 1,
                }
            }
            Role::Npc => {
                if target_kind == Kind::Player && actor != target {
                    self.damage_taken(target, amount);
                } else {
                    self.stats.damage_dropped += 1;
                }
            }
            Role::Unknown => self.stats.damage_dropped += 1,
        }
    }

    /// Classifies the actor of a record, inferring a summon from a spirit skill value record and a
    /// player from a player band skill when the id has no kind yet. Players that act directly are alive.
    fn actor_role(&mut self, t: u64, actor: EntityId, skill: u32, record: RecordKind) -> Role {
        match self.registry.kind(actor) {
            Kind::Player => {
                if record == RecordKind::Direct {
                    self.registry.seen(actor, t);
                }
                self.registry.note_skill(actor, skill, t);
                Role::Player
            }
            Kind::Summon => {
                self.registry.note_skill(actor, skill, t);
                Role::Summon
            }
            Kind::Npc => Role::Npc,
            Kind::Unknown => {
                if entities::is_spirit_skill(skill) || entities::is_effect_entity_skill(skill) {
                    // Session1 12:33:27: an Elementalist (id 15079, never identified in the capture) announces
                    // "Summon: Water Spirit" 16110240 as a no value record and the spirit then deals
                    // damage with 1611xxxx value records, so only a value record makes the id a spirit.
                    // The same for a Cleric's Divine Aura: the player casts 17150340, the aura entity
                    // ticks 17150342 (session3 13:55, id 16581 with no spawn in the capture).
                    if record == RecordKind::Cast {
                        return Role::Unknown;
                    }
                    self.registry.infer_summon(actor, t, entities::class_hint_from_skill(GameData::get(), skill));
                    Role::Summon
                } else if entities::is_player_skill(skill) {
                    self.registry.infer_player(actor, t);
                    self.registry.note_skill(actor, skill, t);
                    Role::Player
                } else if entities::is_npc_skill(skill) {
                    Role::Npc
                } else {
                    Role::Unknown
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn count_damage(&mut self, t: u64, actor: EntityId, kind: AccKind, target: EntityId, skill: u32, amount: u64, flags: &HitFlags) {
        let data = GameData::get();
        let (target_is_boss, target_is_dummy) =
            self.registry.entity(target).map(|e| (e.is_live_boss(), e.is_dummy)).unwrap_or((false, false));

        match &self.live {
            None => self.open_encounter(t, None),
            Some(live) if target_is_boss && live.main.is_some() && live.main != Some(target) && !live.main_is_boss() => {
                // The encounter held only trash: clear it and restart at the first boss hit. The trash
                // is kept as its own encounter when it was long enough to be worth a history entry.
                // The main target itself becoming a boss between two hits (an unknown code promoted by
                // a later HP report, or a spawn arriving after the first hit) is not a new pull: the
                // hit below refreshes its boss flag and the encounter goes on.
                let mut old = self.live.take().expect("live encounter");
                old.end(EndReason::Idle, old.last_damage_ms);
                let reuse = if old.duration_ms() >= MIN_TRASH_KEEP_MS {
                    self.finish(old);
                    None
                } else {
                    Some(old.id)
                };
                self.open_encounter(t, reuse);
            }
            Some(_) => {}
        }

        let hit = Hit {
            t_ms: t,
            actor,
            actor_kind: kind,
            class_hint: entities::class_hint_from_skill(data, skill),
            target,
            target_is_boss,
            target_is_dummy,
            skill: GameData::base_skill(skill),
            amount,
            flags,
        };
        let gap = self.config.active_gap_ms;
        if let Some(live) = self.live.as_mut() {
            live.add_damage(&hit, gap);
        }
        self.stats.damage_counted += 1;
    }

    fn open_encounter(&mut self, t: u64, reuse_id: Option<u64>) {
        let id = reuse_id.unwrap_or_else(|| self.next_encounter_id());
        let data = GameData::get();
        let map_name = self.map_id.map(|m| data.map_name(m));
        let is_instance = self.map_id.map(|m| data.is_instance_map(m)).unwrap_or(false);
        let mut enc = Encounter::new(id, t, self.map_id, map_name, is_instance);
        for c in self.recent_casts.drain(..) {
            if t.saturating_sub(c.t_ms) <= CAST_LOOKBACK_MS {
                enc.add_cast(c.t_ms, c.actor, c.kind, c.skill);
                self.stats.casts += 1;
            }
        }
        self.live = Some(enc);
        self.last = None;
        self.stats.encounters_started += 1;
    }

    fn heal(&mut self, t: u64, actor: EntityId, kind: AccKind, skill: u32, amount: u64) {
        if let Some(live) = self.live.as_mut() {
            live.add_heal(t, actor, kind, GameData::base_skill(skill), amount);
        }
    }

    fn damage_taken(&mut self, target: EntityId, amount: u64) {
        if let Some(live) = self.live.as_mut() {
            live.add_damage_taken(target, amount);
            self.stats.damage_taken += 1;
        }
    }

    fn on_heal(&mut self, t: u64, actor: EntityId, target: EntityId, skill: u32, amount: u64, flags: &HitFlags) {
        if actor == 0 || target == 0 || amount == 0 || self.live.is_none() {
            return;
        }
        if !self.registry.is_player(target) {
            return;
        }
        match self.actor_role(t, actor, skill, if flags.dot { RecordKind::Dot } else { RecordKind::Direct }) {
            Role::Player => {
                self.stats.heals += 1;
                self.heal(t, actor, AccKind::Player, skill, amount);
            }
            Role::Summon => {
                self.stats.heals += 1;
                self.heal(t, actor, AccKind::Summon, skill, amount);
            }
            // A mob is never a healer.
            Role::Npc | Role::Unknown => {}
        }
    }

    fn on_cast(&mut self, t: u64, actor: EntityId, skill: u32) {
        if actor == 0 {
            return;
        }
        let kind = match self.actor_role(t, actor, skill, RecordKind::Cast) {
            Role::Player => AccKind::Player,
            Role::Summon => AccKind::Summon,
            Role::Npc | Role::Unknown => return,
        };
        let skill = GameData::base_skill(skill);
        match self.live.as_mut() {
            Some(live) => {
                live.add_cast(t, actor, kind, skill);
                self.stats.casts += 1;
            }
            None => {
                // Kept for the encounter the next hit opens.
                while self.recent_casts.front().map(|c| t.saturating_sub(c.t_ms) > CAST_LOOKBACK_MS).unwrap_or(false)
                    || self.recent_casts.len() >= CAST_BUFFER
                {
                    self.recent_casts.pop_front();
                }
                self.recent_casts.push_back(PendingCast { t_ms: t, actor, kind, skill });
            }
        }
    }

    fn on_spawn(&mut self, spawn: Spawn) {
        let was_inferred = self.registry.spawn(spawn);
        let new_kind = self.registry.kind(spawn.id);
        let Some(acc) = self.live.as_mut().and_then(|live| live.actors.get_mut(&spawn.id)) else { return };
        if new_kind == Kind::Summon {
            // The id turned out to be a summon: its damage folds into the owner from now on.
            acc.kind = AccKind::Summon;
        } else if was_inferred {
            // A mob that happened to use a class or spirit skill before its spawn arrived.
            self.remove_inferred(spawn.id);
        }
        // A named player's id respawning as a mob keeps its rows; the registry retired the name.
    }

    /// Drops the rows of an id that was taken for a player (or a summon) by its skills and turned out to be
    /// a mob, and the encounter with them when nothing else was hit.
    fn remove_inferred(&mut self, id: EntityId) {
        let Some(live) = self.live.as_mut() else { return };
        if !live.actors.contains_key(&id) {
            return;
        }
        live.remove_actor(id);
        self.stats.inferred_players_removed += 1;
        if live.targets.is_empty() {
            self.live = None;
        }
    }

    fn on_owner_link(&mut self, t: u64, entity: EntityId, owner: EntityId) {
        if !self.registry.owner_link(t, entity, owner) {
            return;
        }
        if let Some(acc) = self.live.as_mut().and_then(|l| l.actors.get_mut(&entity)) {
            acc.kind = AccKind::Summon;
        }
    }

    fn on_death(&mut self, t: u64, id: EntityId, kind: DeathKind) {
        if self.registry.death(t, id, kind) != DeathOutcome::Died {
            return;
        }
        let is_player = self.registry.is_player(id);
        let Some(live) = self.live.as_mut() else { return };
        if is_player {
            live.add_death(t, id, true);
            if self.registry.all_party_dead() {
                self.end_live(EndReason::Wipe, t);
            }
        } else if live.main == Some(id) && live.main_is_boss() {
            live.add_death(t, id, false);
            self.end_live(EndReason::BossDied, t);
        } else if live.targets.contains_key(&id) {
            live.add_death(t, id, false);
        }
    }

    fn on_hp(&mut self, t: u64, id: EntityId, cur_hp: u64, max_hp: Option<u64>) {
        let was_inferred = self.registry.is_inferred_player(id);
        let died = self.registry.hp(t, id, cur_hp, max_hp);
        if was_inferred && !self.registry.is_player(id) {
            // More HP than any player: a mob that fired a class skill before anyone hit it.
            self.remove_inferred(id);
        }
        if died {
            // A player's HP dropped to zero: that is how the Global client reports a player death.
            if let Some(live) = self.live.as_mut() {
                live.add_death(t, id, true);
            }
            if self.registry.all_party_dead() {
                self.end_live(EndReason::Wipe, t);
            }
            return;
        }
        let Some(live) = self.live.as_ref() else { return };
        if live.main != Some(id) || !live.main_is_boss() {
            return;
        }
        let Some(e) = self.registry.entity(id) else { return };
        let Some(max) = e.max_hp.filter(|_| e.max_hp_known) else { return };
        if cur_hp < max || cur_hp == 0 {
            return;
        }
        // The boss is back at full HP without having died: the attempt was reset. Either the party has
        // been off it for a while, or it just lost a player (the server resets within 100 ms of a wipe).
        let quiet = t.saturating_sub(live.main_last_hit_ms()) >= HP_RESET_QUIET_MS;
        let after_death = live.last_player_death_ms().is_some_and(|d| t.saturating_sub(d) <= HP_RESET_AFTER_DEATH_MS);
        if quiet || after_death {
            self.end_live(EndReason::Wipe, t);
        }
    }

    fn on_zone_change(&mut self, t: u64, map_id: u32) {
        if self.map_id == Some(map_id) {
            // A second load of the same map is a teleport and changes nothing: ids are not reissued by
            // it (session3 13:42:39 and session4 14:10:56 keep every player id across the reload) and a
            // boss fight can be live through it (session3 13:56:04: the local player dies at 13:55:56, reloads eight
            // seconds later with the same id 10524 while the party keeps hitting Gelcos, id 33579
            // throughout, which dies at 13:56:17). Ending or resetting here would split that kill. A
            // wipe's reload is caught by the HP reset and all party dead rules instead.
            return;
        }
        if self.live.is_some() {
            self.end_live(EndReason::ZoneChange, t);
        }
        self.map_id = Some(map_id);
        self.registry.zone_change(t);
    }

    // Encounter lifecycle

    /// Idle timeouts and the dummy cap, at the given clock.
    fn apply_time_rules(&mut self, now: u64) {
        let Some(live) = self.live.as_ref() else { return };
        let is_dummy = live.main_is_dummy();
        let boss_alive = live.main_is_boss()
            && live.main.and_then(|m| self.registry.entity(m)).map(|e| !e.dead && !e.gone).unwrap_or(true);
        let last = live.last_damage_ms;
        let started = live.started_ms;
        if is_dummy && self.config.dummy_cap_ms > 0 && now.saturating_sub(started) >= self.config.dummy_cap_ms {
            self.end_live(EndReason::DummyCap, last);
            return;
        }
        let timeout = if is_dummy {
            DUMMY_IDLE_MS
        } else if boss_alive && live.is_instance {
            self.config.idle_timeout_instance_ms
        } else {
            self.config.idle_timeout_open_world_ms
        };
        if now.saturating_sub(last) >= timeout {
            self.end_live(EndReason::Idle, last);
        }
    }

    fn end_live(&mut self, reason: EndReason, t: u64) {
        if let Some(mut enc) = self.live.take() {
            enc.end(reason, t);
            self.finish(enc);
        }
    }

    /// Freezes an ended encounter: it becomes the displayed one and, when it is a boss fight or long
    /// enough to matter, goes to history.
    fn finish(&mut self, enc: Encounter) {
        let snap = stats::build(&enc, &self.registry, false);
        let worth_filing = snap.boss.as_ref().map(|b| b.is_boss).unwrap_or(false) || snap.duration_ms >= MIN_FILED_MS;
        if worth_filing {
            self.finished.push(snap.clone());
            self.stats.encounters_finished += 1;
        }
        self.last = Some(snap);
    }
}
