//! Entity registry: what each id is (player, NPC, summon), its name, class, owner and HP, plus the
//! party roster and the local player.
//!
//! Ids are session scoped and reused, so a spawn always overrides what was known about an id. Names
//! are kept apart from the entity records and stay resolvable for a while after their id was retired,
//! so the rows of a finished fight still carry the right names.

use crate::gamedata::GameData;
use crate::parser::{DeathKind, EntityId, PartyMember};
use std::collections::HashMap;

/// Entities silent for this long are forgotten.
const ENTITY_TTL_MS: u64 = 60 * 60 * 1000;
/// Names stay resolvable this long after their id was retired.
const NAME_TTL_MS: u64 = 45 * 60 * 1000;
const SWEEP_INTERVAL_MS: u64 = 60 * 1000;
/// Spawn kinds that are summons, pets and spirits (0x5F summon or pet, 0x1F/0x1D/0x5D other players'
/// spirits and ground spells, PROTOCOL.md 9.1).
const SUMMON_KINDS: [u8; 4] = [0x5F, 0x1F, 0x1D, 0x5D];
/// Kind 0x1C is a "skill effect entity parented to the target" (PROTOCOL.md 9.1, 13.1). On the Global
/// client it also covers boss adds with NPC codes and millions of HP: Divine Auldor (Specimen) at
/// 9 860 000, Vakron's Thorn Vines at 7 540 000 and Sharp Aulak at 435 000 in session1, none of them
/// with an owner (0 of 160 spawns of that kind carried one). Those hit players (1600990, 1604373) and
/// get hit, so a 0x1C spawn is a summon only when the parser found an owner for it.
const EFFECT_KIND: u8 = 0x1C;
/// Unknown NPC codes with more HP than this are treated as possible bosses.
const POSSIBLE_BOSS_HP: u64 = 10_000_000;
/// No player has more HP than this: the 46 players in the four October 2026 captures had 10 641 to 29 549,
/// the mobs 43 292 to 149 688 832 (and objects a few points). Ten times the highest player keeps PvP safe:
/// an id with more is a mob whatever skills it uses.
const PLAYER_MAX_HP: u64 = 300_000;
/// Duplicate death reports within this window are ignored.
const DEATH_DEDUP_MS: u64 = 5_000;
/// A player that acts this long after its death report has been resurrected. Shorter than this it is
/// an in flight hit or a passive: session2 13:30:39, the local player's HP 0 report is followed by a 2797 hit on
/// Vakron 50 ms later and by three self targeted casts within 150 ms.
const DEATH_GRACE_MS: u64 = 5_000;
/// Owner chains longer than this are not followed.
const MAX_OWNER_HOPS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Unknown,
    Player,
    Npc,
    Summon,
}

#[derive(Debug, Clone)]
pub struct Entity {
    pub kind: Kind,
    /// Classified by what it did, not by a spawn or an identity: a player by its skill band, a mob by the
    /// hits players landed on it or by its HP.
    pub inferred: bool,
    pub npc_code: Option<u32>,
    pub is_boss: bool,
    pub is_dummy: bool,
    pub owner: Option<EntityId>,
    pub class_id: Option<u32>,
    /// Class suggested by the skills the entity used, when no identity or roster gave one.
    pub class_hint: Option<u32>,
    pub level: Option<u32>,
    pub cur_hp: Option<u64>,
    pub max_hp: Option<u64>,
    /// The max HP came from a spawn or an HP update, not from the highest value seen.
    pub max_hp_known: bool,
    pub dead: bool,
    /// Removed from the world (despawned); later hits on it are not counted.
    pub gone: bool,
    pub last_death_ms: Option<u64>,
    pub last_seen_ms: u64,
}

impl Entity {
    fn new(t_ms: u64) -> Self {
        Entity {
            kind: Kind::Unknown,
            inferred: false,
            npc_code: None,
            is_boss: false,
            is_dummy: false,
            owner: None,
            class_id: None,
            class_hint: None,
            level: None,
            cur_hp: None,
            max_hp: None,
            max_hp_known: false,
            dead: false,
            gone: false,
            last_death_ms: None,
            last_seen_ms: t_ms,
        }
    }

    /// A boss for encounter purposes: flagged in the table (or promoted by HP) and not a dummy.
    pub fn is_live_boss(&self) -> bool {
        self.is_boss && !self.is_dummy
    }
}

/// The fields of a spawn event the registry needs.
#[derive(Debug, Clone, Copy)]
pub struct Spawn {
    pub t_ms: u64,
    pub id: EntityId,
    pub npc_code: u32,
    pub kind: u8,
    pub cur_hp: Option<u64>,
    pub max_hp: Option<u64>,
    pub owner: Option<EntityId>,
}

#[derive(Debug, Clone)]
struct NameRecord {
    name: String,
    /// Set when the id stopped being this player (reissued, respawned, or silent for too long).
    retired_ms: Option<u64>,
}

/// What a death event meant for the entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathOutcome {
    /// A duplicate report or a flag that is not an event.
    Ignored,
    Died,
    Gone,
}

/// Player band skills: class skills, pet skills and Theostone item skills.
pub fn is_player_skill(skill: u32) -> bool {
    (10_000_000..=19_999_999).contains(&skill) || (100_000..=199_999).contains(&skill) || (3_000_000..=3_099_999).contains(&skill)
}

/// Seven digit NPC skill codes (players fire a few of these too, so they only mark unknown actors).
pub fn is_npc_skill(skill: u32) -> bool {
    (1_000_000..=9_999_999).contains(&skill) && !is_player_skill(skill)
}

/// Skills whose value records come from an Elementalist's spirit, never from a player: the spirit
/// attack codes named after the summon skill (1610 Fire, 1611 Water, 1613 Earth, 1625 Ancient), the
/// shared 1600 code and the six digit pet skills. In session1 and session2 (both captures, 1 050 value
/// records) every one of these was fired by a spawned 0x5F/0x1F/0x1D entity or by an id with no spawn
/// at all; players only cast them (no value records). 1612 (Wind) and 1615 are left out because players
/// deal damage with them too (14 and 5 value records per capture).
pub fn is_spirit_skill(skill: u32) -> bool {
    (16_000_000..=16_009_999).contains(&skill)
        || (16_100_000..=16_119_999).contains(&skill)
        || (16_130_000..=16_139_999).contains(&skill)
        || (16_250_000..=16_259_999).contains(&skill)
        || (100_000..=199_999).contains(&skill)
}

/// Skills whose value records come from a class's skill effect entity, never from the player: the
/// Cleric's Divine Aura (17150000). The Cleric casts 17150340 (a no value record) and a 0x1F entity
/// spawned beside it then ticks 17150342 on the mobs every 600 ms (sessions 3 and 4: 44 aura entities,
/// all of their value records from spawned entities or from ids with no spawn at all, none from a
/// named player). An aura whose spawn the capture missed must not become a sixth "player" row.
pub fn is_effect_entity_skill(skill: u32) -> bool {
    (17_150_000..=17_159_999).contains(&skill)
}

/// Spirit to owner link records: neither damage nor heal, but they tell who owns the spirit.
pub fn is_spirit_to_owner_link(skill: u32) -> bool {
    (16_990_000..=16_999_999).contains(&skill)
}

/// Owner to spirit link records.
pub fn is_owner_to_spirit_link(skill: u32) -> bool {
    (16_770_000..=16_779_999).contains(&skill)
}

/// Class suggested by a skill code. The 10 prefix and the spirit skills belong to the Elementalist.
pub fn class_hint_from_skill(data: &GameData, skill: u32) -> Option<u32> {
    data.class_from_skill(skill).or(if (10_000_000..=10_999_999).contains(&skill) || is_spirit_skill(skill) { Some(16) } else { None })
}

#[derive(Default)]
pub struct Registry {
    entities: HashMap<EntityId, Entity>,
    names: HashMap<EntityId, NameRecord>,
    /// The id a player name currently lives under.
    by_name: HashMap<String, EntityId>,
    party: Vec<PartyMember>,
    party_index: HashMap<String, usize>,
    /// From a self identity record.
    self_id: Option<EntityId>,
    /// The parser's `06 38` vote for the local player, used only while `self_id` is unknown.
    self_hint: Option<EntityId>,
    self_name: Option<String>,
    last_sweep_ms: u64,
}

impl Registry {
    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.entities.get(&id)
    }

    /// The kind of an id. The local player is always a player.
    pub fn kind(&self, id: EntityId) -> Kind {
        if self.self_id() == Some(id) {
            return Kind::Player;
        }
        self.entities.get(&id).map(|e| e.kind).unwrap_or(Kind::Unknown)
    }

    pub fn is_player(&self, id: EntityId) -> bool {
        self.kind(id) == Kind::Player
    }

    fn touch(&mut self, id: EntityId, t_ms: u64) -> &mut Entity {
        let e = self.entities.entry(id).or_insert_with(|| Entity::new(t_ms));
        e.last_seen_ms = e.last_seen_ms.max(t_ms);
        e
    }

    /// Marks the id as seen, and alive when it is a player acting well after its last death report (a
    /// dead player cannot act, but its in flight hits and passives still land for a moment).
    pub fn seen(&mut self, id: EntityId, t_ms: u64) {
        let e = self.touch(id, t_ms);
        if e.kind == Kind::Player && e.dead && e.last_death_ms.map(|d| t_ms.saturating_sub(d) > DEATH_GRACE_MS).unwrap_or(true) {
            e.dead = false;
        }
    }

    /// Records the class a skill suggests for an entity.
    pub fn note_skill(&mut self, id: EntityId, skill: u32, t_ms: u64) {
        let hint = class_hint_from_skill(GameData::get(), skill);
        if hint.is_some() {
            let e = self.touch(id, t_ms);
            if e.class_hint.is_none() {
                e.class_hint = hint;
            }
        }
    }

    /// The player name an id carries, current or retired.
    pub fn name_of(&self, id: EntityId) -> Option<&str> {
        self.names.get(&id).map(|r| r.name.as_str())
    }

    /// The id a player name currently lives under.
    pub fn current_id(&self, name: &str) -> Option<EntityId> {
        self.by_name.get(name).copied()
    }

    /// The local player's id: from the self identity record, else the parser's hint.
    pub fn self_id(&self) -> Option<EntityId> {
        self.self_id.or(self.self_hint)
    }

    /// The parser's local player hint (`ParserStats::hint_self_id`). It stands in for the self id until
    /// a self record names the local player, so a meter started mid session marks the right row as
    /// self; `None` withdraws it (the parser drops the hint on every map load).
    pub fn set_self_hint(&mut self, id: Option<EntityId>) {
        self.self_hint = id.filter(|i| *i != 0);
    }

    pub fn self_name(&self) -> Option<&str> {
        self.self_name.as_deref()
    }

    pub fn party_member(&self, name: &str) -> Option<&PartyMember> {
        self.party_index.get(name).map(|i| &self.party[*i])
    }

    pub fn is_party_name(&self, name: &str) -> bool {
        self.party_index.contains_key(name)
    }

    /// Marks an id as a player by skill band. Does nothing to an id that already has a kind.
    pub fn infer_player(&mut self, id: EntityId, t_ms: u64) {
        let e = self.touch(id, t_ms);
        if e.kind == Kind::Unknown {
            e.kind = Kind::Player;
            e.inferred = true;
        }
    }

    /// A target players landed counted hits on, with no spawn or identity behind it, is a mob from then
    /// on. A mob whose spawn the meter missed (it came into view before the meter started) fires player
    /// band codes too: 12070007, 12750001 and 12130040 (Templar) and 18730002 in the four captures, 2 to
    /// 122 times a session. Taken for a player, every later hit on it was dropped as player to player and
    /// the meter went quiet seven seconds into a fight (aion2dps encounter 827, 25 October 2026). An
    /// identity still makes the id a player, so PvP on named players stays out of the meter.
    pub fn hostile(&mut self, id: EntityId, t_ms: u64) {
        if self.self_id() == Some(id) {
            return;
        }
        let e = self.touch(id, t_ms);
        if e.kind == Kind::Unknown {
            e.kind = Kind::Npc;
            e.inferred = true;
        }
    }

    /// Whether the id is a player only by its skill band.
    pub fn is_inferred_player(&self, id: EntityId) -> bool {
        self.self_id() != Some(id) && self.entities.get(&id).is_some_and(|e| e.kind == Kind::Player && e.inferred)
    }

    /// Marks an id as a summon by skill (a spirit attack or an effect entity tick from an id whose spawn
    /// was never seen, which happens when the meter starts mid session). `class_hint` is the owner's
    /// class the skill suggests, so the orphan can fold into the one player of that class. Its owner may
    /// still arrive through a link record.
    pub fn infer_summon(&mut self, id: EntityId, t_ms: u64, class_hint: Option<u32>) {
        let e = self.touch(id, t_ms);
        if e.kind == Kind::Unknown {
            e.kind = Kind::Summon;
            e.inferred = true;
            e.class_hint = class_hint;
        }
    }

    /// A weak identity: the owner name a summon spawn carries, with no server, job or level behind it.
    /// It names an id that is not yet a named player. A spawned mob or summon keeps its kind (a misread
    /// owner id could be a live boss, after which every hit on it would be dropped as player to player),
    /// and a current name is never replaced by it (A2Tools V104 keeps the authoritative "Bowmaster" over
    /// the pet spawn's "Hawkeye1"). Returns whether it was applied.
    pub fn weak_identity(&mut self, t_ms: u64, id: EntityId, name: &str) -> bool {
        if self.entities.get(&id).is_some_and(|e| matches!(e.kind, Kind::Npc | Kind::Summon)) {
            return false;
        }
        if self.names.get(&id).is_some_and(|r| r.retired_ms.is_none() && r.name != name) {
            return false;
        }
        self.identity(t_ms, id, name, None, 0, false);
        true
    }

    /// An identity packet. A known name under a new id moves the name there; the old id keeps it as a
    /// retired name so earlier stats still resolve.
    pub fn identity(&mut self, t_ms: u64, id: EntityId, name: &str, class_id: Option<u32>, level: u32, is_self: bool) {
        if let Some(old) = self.by_name.get(name).copied() {
            if old != id {
                if let Some(rec) = self.names.get_mut(&old) {
                    rec.retired_ms.get_or_insert(t_ms);
                }
            }
        }
        if let Some(prev) = self.names.get(&id) {
            if prev.name != name && self.by_name.get(&prev.name) == Some(&id) {
                self.by_name.remove(&prev.name);
            }
        }
        self.by_name.insert(name.to_owned(), id);
        self.names.insert(id, NameRecord { name: name.to_owned(), retired_ms: None });
        let e = self.touch(id, t_ms);
        e.kind = Kind::Player;
        e.inferred = false;
        e.npc_code = None;
        e.owner = None;
        e.is_boss = false;
        e.is_dummy = false;
        e.gone = false;
        e.dead = false;
        if let Some(c) = class_id.filter(|c| *c != 0) {
            e.class_id = Some(c);
        }
        if level > 0 {
            e.level = Some(level);
        }
        if is_self {
            self.self_id = Some(id);
            self.self_name = Some(name.to_owned());
        }
    }

    /// A complete roster replaces the party. Members map to ids by name only.
    pub fn set_party(&mut self, members: &[PartyMember]) {
        self.party = members.to_vec();
        self.party_index = members.iter().enumerate().map(|(i, m)| (m.name.clone(), i)).collect();
        for m in members {
            if let Some(id) = self.by_name.get(&m.name).copied() {
                if let Some(e) = self.entities.get_mut(&id) {
                    if m.class_id != 0 {
                        e.class_id = Some(m.class_id);
                    }
                    if m.level > 0 {
                        e.level = Some(m.level);
                    }
                }
            }
        }
    }

    /// A spawn always reclassifies the id as a non player. Returns true when the id had been treated
    /// as a player or a summon by skill inference only, so its damage can be removed.
    pub fn spawn(&mut self, s: Spawn) -> bool {
        if self.self_id() == Some(s.id) {
            // The local player is always a player; a spawn with its id is a folded id collision.
            return false;
        }
        let data = GameData::get();
        let owner = s.owner.filter(|o| *o != s.id && *o != 0 && self.entities.get(o).map(|e| e.kind != Kind::Npc).unwrap_or(true));
        let summon =
            SUMMON_KINDS.contains(&s.kind) || data.is_player_summon_npc(s.npc_code) || (s.kind == EFFECT_KIND && owner.is_some());
        let was_inferred = matches!(self.entities.get(&s.id), Some(e) if e.inferred && matches!(e.kind, Kind::Player | Kind::Summon));
        if let Some(rec) = self.names.get_mut(&s.id) {
            rec.retired_ms.get_or_insert(s.t_ms);
        }
        let known = data.npc(s.npc_code).is_some();
        let is_dummy = data.is_dummy(s.npc_code);
        let promoted = !known && s.max_hp.unwrap_or(0) > POSSIBLE_BOSS_HP;
        let is_boss = (data.is_boss(s.npc_code) || promoted) && !is_dummy;
        let mut e = Entity::new(s.t_ms);
        e.kind = if summon { Kind::Summon } else { Kind::Npc };
        e.npc_code = Some(s.npc_code);
        e.is_boss = is_boss;
        e.is_dummy = is_dummy;
        e.owner = if summon { owner } else { None };
        e.cur_hp = s.cur_hp;
        e.max_hp = s.max_hp.or(s.cur_hp);
        e.max_hp_known = s.max_hp.is_some();
        self.entities.insert(s.id, e);
        was_inferred
    }

    /// An ownership record. Never applied to a spawned mob or a named player; an inferred player
    /// becomes a summon (summons are never players). Returns whether the link was taken.
    pub fn owner_link(&mut self, t_ms: u64, entity: EntityId, owner: EntityId) -> bool {
        if entity == owner || entity == 0 || owner == 0 || self.self_id() == Some(entity) {
            return false;
        }
        if self.entities.get(&owner).map(|e| e.kind == Kind::Npc).unwrap_or(false) {
            return false;
        }
        let data = GameData::get();
        let e = self.touch(entity, t_ms);
        match e.kind {
            Kind::Npc if !e.npc_code.map(|c| data.is_player_summon_npc(c)).unwrap_or(false) => return false,
            Kind::Player if !e.inferred => return false,
            _ => {}
        }
        e.kind = Kind::Summon;
        e.inferred = false;
        e.owner = Some(owner);
        true
    }

    pub fn death(&mut self, t_ms: u64, id: EntityId, kind: DeathKind) -> DeathOutcome {
        let e = self.touch(id, t_ms);
        match kind {
            DeathKind::Died => {
                if e.last_death_ms.map(|d| t_ms.saturating_sub(d) < DEATH_DEDUP_MS).unwrap_or(false) {
                    return DeathOutcome::Ignored;
                }
                e.dead = true;
                e.last_death_ms = Some(t_ms);
                DeathOutcome::Died
            }
            DeathKind::Gone => {
                e.gone = true;
                DeathOutcome::Gone
            }
            DeathKind::LoadedDead => {
                e.dead = true;
                DeathOutcome::Ignored
            }
            DeathKind::Other(_) => DeathOutcome::Ignored,
        }
    }

    /// HP from a remaining HP or HP update packet. The max is raised to any observed current value.
    /// Returns true when a player just died: the Global client reports player deaths as HP 0 here and
    /// never with a `42 36` flag 3 (0 of 74 death records in session1 and session2 name a player, while
    /// the 70 player HP 0 reports match the one shots and the resurrections that follow).
    pub fn hp(&mut self, t_ms: u64, id: EntityId, cur_hp: u64, max_hp: Option<u64>) -> bool {
        let is_self = self.self_id() == Some(id);
        let e = self.touch(id, t_ms);
        e.cur_hp = Some(cur_hp);
        if let Some(m) = max_hp.filter(|m| *m > 0) {
            e.max_hp = Some(m);
            e.max_hp_known = true;
        }
        if e.max_hp.map(|m| m < cur_hp).unwrap_or(true) && cur_hp > 0 {
            e.max_hp = Some(cur_hp);
        }
        // More HP than a player can have: a mob, even one that was taken for a player by its skills.
        let unnamed = e.kind == Kind::Unknown || (e.kind == Kind::Player && e.inferred);
        if unnamed && !is_self && e.max_hp.is_some_and(|m| m > PLAYER_MAX_HP) {
            e.kind = Kind::Npc;
            e.inferred = true;
            e.class_hint = None;
            e.dead = false;
        }
        if e.kind == Kind::Player || is_self {
            if cur_hp == 0 {
                if !e.dead {
                    e.dead = true;
                    e.last_death_ms = Some(t_ms);
                    return true;
                }
            } else {
                e.dead = false;
            }
            return false;
        }
        if e.kind == Kind::Npc && !e.is_dummy && !e.is_boss {
            let known = e.npc_code.map(|c| GameData::get().npc(c).is_some()).unwrap_or(true);
            if !known && e.max_hp.unwrap_or(0) > POSSIBLE_BOSS_HP {
                e.is_boss = true;
            }
        }
        false
    }

    /// Ids are reissued on a map change: NPCs, summons and unnamed players are gone, named players
    /// keep their names (their new ids arrive with the next identity packets) and come back alive.
    pub fn zone_change(&mut self, t_ms: u64) {
        self.entities.retain(|_, e| e.kind == Kind::Player && !e.inferred);
        for e in self.entities.values_mut() {
            e.dead = false;
            e.gone = false;
            e.last_seen_ms = e.last_seen_ms.max(t_ms);
        }
    }

    /// Forgets entities silent for an hour and names retired for 45 minutes. Cheap when called often.
    pub fn sweep(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_sweep_ms) < SWEEP_INTERVAL_MS {
            return;
        }
        self.last_sweep_ms = now_ms;
        let self_id = self.self_id();
        let mut retired: Vec<EntityId> = Vec::new();
        self.entities.retain(|id, e| {
            let keep = Some(*id) == self_id || now_ms.saturating_sub(e.last_seen_ms) < ENTITY_TTL_MS;
            if !keep {
                retired.push(*id);
            }
            keep
        });
        for id in retired {
            if let Some(rec) = self.names.get_mut(&id) {
                rec.retired_ms.get_or_insert(now_ms);
            }
        }
        let by_name = &mut self.by_name;
        self.names.retain(|id, rec| {
            let keep = rec.retired_ms.map(|r| now_ms.saturating_sub(r) < NAME_TTL_MS).unwrap_or(true);
            if !keep && by_name.get(&rec.name) == Some(id) {
                by_name.remove(&rec.name);
            }
            keep
        });
    }

    /// The player a summon ultimately belongs to, following owner links. `None` for a player, an NPC,
    /// or a summon whose chain ends nowhere. An owner id that was never seen is still returned: it is
    /// a player we have no identity for yet.
    pub fn resolve_owner(&self, id: EntityId) -> Option<EntityId> {
        let mut cur = id;
        for _ in 0..MAX_OWNER_HOPS {
            if self.self_id() == Some(cur) {
                return (cur != id).then_some(cur);
            }
            let Some(e) = self.entities.get(&cur) else {
                return (cur != id).then_some(cur);
            };
            match e.kind {
                Kind::Summon => match e.owner {
                    Some(o) if o != cur => cur = o,
                    _ => return None,
                },
                Kind::Player | Kind::Unknown => return (cur != id).then_some(cur),
                Kind::Npc => return None,
            }
        }
        None
    }

    /// Class of an id: identity, then roster by name, then the skills it used.
    pub fn class_of(&self, id: EntityId) -> Option<u32> {
        let e = self.entities.get(&id);
        if let Some(c) = e.and_then(|e| e.class_id) {
            return Some(c);
        }
        if let Some(m) = self.name_of(id).and_then(|n| self.party_member(n)) {
            if m.class_id != 0 {
                return Some(m.class_id);
            }
        }
        e.and_then(|e| e.class_hint)
    }

    /// The one named player of a class, for merging an unowned summon: party members first, then
    /// every known player. `None` when there is no candidate or more than one.
    pub fn only_player_of_class(&self, class_id: u32) -> Option<EntityId> {
        let candidates: Vec<EntityId> = self
            .by_name
            .values()
            .copied()
            .filter(|id| self.entities.get(id).map(|e| e.kind == Kind::Player && !e.inferred).unwrap_or(false))
            .filter(|id| self.class_of(*id) == Some(class_id))
            .collect();
        let in_party: Vec<EntityId> = candidates
            .iter()
            .copied()
            .filter(|id| Some(*id) == self.self_id() || self.name_of(*id).map(|n| self.is_party_name(n)).unwrap_or(false))
            .collect();
        match (in_party.len(), candidates.len()) {
            (1, _) => Some(in_party[0]),
            (0, 1) => Some(candidates[0]),
            _ => None,
        }
    }

    /// The ids of the known party players: the local player and every roster member whose current id
    /// is known.
    pub fn party_ids(&self) -> Vec<EntityId> {
        let mut ids: Vec<EntityId> = self.self_id().into_iter().collect();
        for m in &self.party {
            if let Some(id) = self.by_name.get(&m.name).copied() {
                if !ids.contains(&id) && self.entities.get(&id).map(|e| e.kind == Kind::Player).unwrap_or(false) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    /// Every party player is dead. Needs an id for every roster member (the local player when there is
    /// no roster): a member whose id is unknown may well be alive, and a false wipe splits a kill in two.
    pub fn all_party_dead(&self) -> bool {
        let ids = self.party_ids();
        !ids.is_empty() && ids.len() >= self.party.len() && ids.iter().all(|id| self.entities.get(id).map(|e| e.dead).unwrap_or(false))
    }

    /// Roster members of a class whose name has no live player id yet (never identified in this
    /// session, or their id was reissued and the new identity is still to come).
    pub fn unidentified_party_members(&self, class_id: u32) -> Vec<&str> {
        self.party
            .iter()
            .filter(|m| m.class_id == class_id)
            .filter(|m| {
                !self.by_name.get(&m.name).is_some_and(|id| self.entities.get(id).map(|e| e.kind == Kind::Player).unwrap_or(false))
            })
            .map(|m| m.name.as_str())
            .collect()
    }

    /// Display name for any id: the player name, the NPC name from the table, or a placeholder.
    pub fn display_name(&self, id: EntityId) -> String {
        if let Some(n) = self.name_of(id) {
            return n.to_owned();
        }
        if let Some(code) = self.entities.get(&id).and_then(|e| e.npc_code) {
            return GameData::get().npc_name(code);
        }
        format!("#{id}")
    }
}
