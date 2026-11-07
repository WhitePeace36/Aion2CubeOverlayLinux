//! Game packets to combat events.
//!
//! Layouts are community findings (see docs/PROTOCOL.md) checked against the Global client captures in
//! `captures/`. The decoders are deliberately strict: a record that does not validate is dropped and
//! counted, because a misread varint shows up as a phantom million damage hit. Per packet decoders live in
//! `src/parse/`; this file holds the event model, the statistics and the opcode dispatch.

use crate::opcodes as op;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

#[path = "parse/damage.rs"]
mod damage;
#[path = "parse/dot.rs"]
mod dot;
#[path = "parse/identity.rs"]
mod identity;
#[path = "parse/roster.rs"]
mod roster;
#[path = "parse/spawn.rs"]
mod spawn;
#[path = "parse/world.rs"]
mod world;
#[cfg(test)]
#[path = "parse/tests.rs"]
mod tests;

pub type EntityId = u32;

/// Entity ids on the wire are session scoped varints in this range; anything else is a misread.
pub const MAX_ENTITY_ID: EntityId = 9_999_999;
/// Skill ids: 6 digit pet skills up to 9 digit effect derived ids.
pub const MAX_SKILL_ID: u32 = 299_999_999;
/// Largest single damage or heal value accepted from a record.
pub const MAX_DAMAGE: u64 = 99_999_999;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct HitFlags {
    pub crit: bool,
    pub back: bool,
    pub front: bool,
    pub parry: bool,
    pub perfect: bool,
    pub double: bool,
    pub multi: bool,
    pub dot: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PartyMember {
    pub name: String,
    /// Raw job value from the packet (blocks of four per class, see gamedata::class_from_job).
    pub job: u32,
    /// Class id 11..19 derived from `job`, 0 when unknown.
    pub class_id: u32,
    pub level: u32,
    pub gear_score: u32,
    pub combat_power: u64,
}

/// Death packet flags: 3 = died in combat, 7 = removed from the world, 1 = loaded already dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeathKind {
    Died,
    Gone,
    LoadedDead,
    Other(u32),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    /// A hit that carried a value. `amount` is the record total (additional hits are already included).
    Damage { t_ms: u64, actor: EntityId, target: EntityId, skill: u32, amount: u64, flags: HitFlags, extra_hits: u32 },
    Heal { t_ms: u64, actor: EntityId, target: EntityId, skill: u32, amount: u64, flags: HitFlags },
    /// A no value record: the skill was cast (or started) and no damage number was sent.
    Cast { t_ms: u64, actor: EntityId, target: EntityId, skill: u32 },
    /// A no value record with hit type miss or resist.
    Miss { t_ms: u64, actor: EntityId, target: EntityId, skill: u32, resist: bool },
    /// A player named by the game. Self and player records carry server, job and level; a summon spawn
    /// (of a player summon template) that names its owner yields a weak record for the owner id with
    /// server, job and level 0, which the tracker never lets reclassify a spawned entity or replace a
    /// current name.
    Identity { t_ms: u64, id: EntityId, name: String, server: u16, job: u32, class_id: u32, level: u32, is_self: bool },
    /// A monster or other NPC appeared. `npc_code` is its template id, the key into the NPC name tables
    /// (0 when the record carried none, which happens for some summons and effect entities).
    NpcSpawn { t_ms: u64, id: EntityId, npc_code: u32, kind: u8, cur_hp: Option<u64>, max_hp: Option<u64>, owner: Option<EntityId> },
    /// A summon or effect entity belongs to an owner (ownership record or spirit link).
    OwnerLink { t_ms: u64, entity: EntityId, owner: EntityId },
    Death { t_ms: u64, id: EntityId, kind: DeathKind },
    RemainHp { t_ms: u64, id: EntityId, cur_hp: u64, max_hp: Option<u64> },
    /// HP and max HP update for an entity.
    HpUpdate { t_ms: u64, id: EntityId, cur_hp: u64, max_hp: u64 },
    /// A loading screen: `map_id` is the map now loaded.
    ZoneChange { t_ms: u64, map_id: u32 },
    /// The party list, by character name (the roster carries account ids, not entity ids).
    Party { t_ms: u64, members: Vec<PartyMember> },
    /// Answer to the client's ping.
    Pong { t_ms: u64, client_ms: u64, server_ms: u64 },
}

impl Event {
    pub fn t_ms(&self) -> u64 {
        match self {
            Event::Damage { t_ms, .. }
            | Event::Heal { t_ms, .. }
            | Event::Cast { t_ms, .. }
            | Event::Miss { t_ms, .. }
            | Event::Identity { t_ms, .. }
            | Event::NpcSpawn { t_ms, .. }
            | Event::OwnerLink { t_ms, .. }
            | Event::Death { t_ms, .. }
            | Event::RemainHp { t_ms, .. }
            | Event::HpUpdate { t_ms, .. }
            | Event::ZoneChange { t_ms, .. }
            | Event::Party { t_ms, .. }
            | Event::Pong { t_ms, .. } => *t_ms,
        }
    }

    /// Short lower case name of the variant, as used by `replay --events <type>`.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Damage { .. } => "damage",
            Event::Heal { .. } => "heal",
            Event::Cast { .. } => "cast",
            Event::Miss { .. } => "miss",
            Event::Identity { .. } => "identity",
            Event::NpcSpawn { .. } => "spawn",
            Event::OwnerLink { .. } => "owner",
            Event::Death { .. } => "death",
            Event::RemainHp { .. } => "hp",
            Event::HpUpdate { .. } => "hp",
            Event::ZoneChange { .. } => "zone",
            Event::Party { .. } => "party",
            Event::Pong { .. } => "pong",
        }
    }
}

/// Why a `04 38` record was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// A read ran past the end of the body.
    Truncated,
    /// Target or actor outside 1..=9 999 999.
    BadId,
    /// Switch low nibble outside {0, 2, 4, 5, 6, 7}.
    BadSwitch,
    /// Skill outside 1..=299 999 999.
    BadSkill,
    /// Damage zero or above 99 999 999.
    BadDamage,
    /// The bytes after the value did not end at a record boundary under any accepted layout.
    TailFailure,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct RejectCounts {
    pub truncated: u64,
    pub bad_id: u64,
    pub bad_switch: u64,
    pub bad_skill: u64,
    pub bad_damage: u64,
    pub tail_failure: u64,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ParserStats {
    pub packets: u64,
    pub heartbeats: u64,
    pub first_heartbeat_ms: Option<u64>,
    pub last_heartbeat_ms: Option<u64>,
    /// Server clock from the last heartbeat body (Unix ms), when present.
    pub server_time_ms: Option<u64>,
    /// `04 38` records attempted (value and no value records, every record of a chain).
    pub damage_records: u64,
    /// Records dropped for any reason; the breakdown is in `rejected`.
    pub damage_rejected: u64,
    pub rejected: RejectCounts,
    /// Packet count per opcode, for reverse engineering (`replay --opcodes`).
    pub opcode_counts: BTreeMap<u16, u64>,
    pub opcode_bytes: BTreeMap<u16, u64>,

    // 04 38 outcomes
    /// Damage events from `04 38` value records.
    pub damage_events: u64,
    /// Heal events from `04 38` value records with a heal skill.
    pub heals: u64,
    pub casts: u64,
    /// No value records with hit type 0: a skill start whose hit type 2 marker follows; not emitted.
    pub cast_starts: u64,
    pub misses: u64,
    pub resists: u64,
    /// Spirit link records (skills 1699xxxx and 1677xxxx), turned into owner links or dropped.
    pub spirit_links: u64,
    /// Records with a resource restore skill (MP, DP and so on), dropped.
    pub resource_restores: u64,
    /// Value records with actor == target, emitted as Damage for the tracker to classify.
    pub self_hits: u64,
    /// Records whose `unused` field had bit 0x04 and so carried one varint after the trailer.
    pub tail_values: u64,
    /// Records with an effect id list (switch bit 0x40 or `unused` bit 0x01).
    pub effect_lists: u64,
    /// Value records with flags bit 0x10 and no plotter block, which carry one byte after the trailer.
    pub tail_flag_bytes: u64,
    /// Packets with leftover bytes after the last record that were too short to be another record.
    /// Reads 0 on every local capture; anything else is an unexplained layout change.
    pub trailing_bytes: u64,
    /// Value records whose first varint after the hit id was 0, read as a pad and shifted by one varint.
    /// Never seen on the Global client; a count means the value block moved and a scalar or a count may
    /// be read as damage, so look at the records before trusting them.
    pub pad_shifts: u64,
    /// Value records whose damage came from the layout 6 crit heuristic (the structural reading did not
    /// end at a boundary). Never fired on the Global client; the same warning as for `pad_shifts`.
    pub crit_heuristics: u64,
    /// No value records whose tail (hit id, scalar, effect list, trailer) did not validate. The cast or
    /// miss is still emitted and the chain stops there, so anything behind it is not read.
    pub no_value_tail_failures: u64,
    /// Records whose tail reached `00 00` where a `<N> 00` boundary was expected. On the Global client
    /// these are mob self casts (no value records, counted above as well); a value record with that
    /// trailer is rejected as a tail failure and would show up here.
    pub zero_trailers: u64,
    /// Hit type values seen on value records and no value records.
    pub hit_types: BTreeMap<u32, u64>,
    /// Hit types other than 0, 1, 2, 3, 6 on no value records (they produce no event) and other than 2, 3
    /// on value records (emitted anyway, the value is there).
    pub unknown_hit_types: BTreeMap<u32, u64>,
    pub switch_values: BTreeMap<u32, u64>,

    // 05 38
    pub dot_ticks: u64,
    pub dot_heals: u64,
    /// Ticks with effect byte 0x03, counted as damage but kept visible until confirmed.
    pub dot_effect_03: u64,
    pub dot_ignored: u64,
    pub dot_rejected: u64,
    /// Damage ticks whose skill is not in the DoT allowlist (diagnostic only, they are still counted).
    pub dot_unlisted: u64,

    // identity and roster
    pub identities: u64,
    pub self_identities: u64,
    pub identity_rejected: u64,
    pub embedded_identities: u64,
    pub embedded_rosters: u64,
    /// LZ4 bundles found inside packet bodies and opened for identity records.
    pub embedded_bundles: u64,
    pub rosters: u64,
    pub roster_rejected: u64,

    // world
    pub spawns: u64,
    pub spawn_rejected: u64,
    pub spawn_no_code: u64,
    pub spawn_owners: u64,
    pub spawn_names: u64,
    /// Summon spawns with an inline name and an owner whose template is not one a player can summon:
    /// a mob's summon named after the player it was sent after, no identity emitted.
    pub spawn_names_rejected: u64,
    pub spawn_folded: u64,
    pub deaths: u64,
    pub death_rejected: u64,
    pub zone_changes: u64,
    pub map_rejected: u64,
    pub remain_hp: u64,
    pub hp_updates: u64,
    /// `00 8D` and `1B 92` bodies that did not validate, plus kind 0 values above 100 million.
    pub hp_rejected: u64,
    pub owner_links: u64,
    pub ownership_ignored: u64,
    pub pongs: u64,
    pub pong_rejected: u64,
    /// Entity named most often by the last 200 `06 38` skill use records with a player skill, when it
    /// leads clearly (at least 24 records and four times the runner up). A fallback for the local player
    /// id when the meter started mid session and the self record is still to come.
    pub hint_self_id: Option<EntityId>,
}

impl ParserStats {
    pub(crate) fn reject(&mut self, reason: Reject) {
        self.damage_rejected += 1;
        match reason {
            Reject::Truncated => self.rejected.truncated += 1,
            Reject::BadId => self.rejected.bad_id += 1,
            Reject::BadSwitch => self.rejected.bad_switch += 1,
            Reject::BadSkill => self.rejected.bad_skill += 1,
            Reject::BadDamage => self.rejected.bad_damage += 1,
            Reject::TailFailure => self.rejected.tail_failure += 1,
        }
    }
}

/// How many recent `06 38` records vote for the local player id.
const SCOPE_WINDOW: usize = 200;

#[derive(Default)]
pub struct Parser {
    pub stats: ParserStats,
    /// Entities of the last `SCOPE_WINDOW` `06 38` records with a player skill.
    scope_window: VecDeque<EntityId>,
}

impl Parser {
    pub fn packet(&mut self, t_ms: u64, opcode: u16, body: &[u8], out: &mut dyn FnMut(Event)) {
        self.stats.packets += 1;
        *self.stats.opcode_counts.entry(opcode).or_default() += 1;
        *self.stats.opcode_bytes.entry(opcode).or_default() += body.len() as u64;
        let stats = &mut self.stats;
        match opcode {
            op::HEARTBEAT => {
                stats.heartbeats += 1;
                stats.first_heartbeat_ms.get_or_insert(t_ms);
                stats.last_heartbeat_ms = Some(t_ms);
                if let Some(b) = body.get(..8) {
                    stats.server_time_ms = Some(u64::from_le_bytes(b.try_into().expect("8 bytes")));
                }
            }
            op::DAMAGE => damage::parse_packet(t_ms, body, stats, out),
            op::DOT => dot::parse(t_ms, body, stats, out),
            op::SELF_INFO => {
                identity::parse_direct(t_ms, body, true, stats, out);
                identity::scan_bundles(t_ms, body, stats, out);
            }
            op::PLAYER_INFO | op::PLAYER_INFO_LEGACY => {
                identity::parse_direct(t_ms, body, false, stats, out);
                identity::scan_bundles(t_ms, body, stats, out);
            }
            op::PARTY_ROSTER => match roster::parse_at(body, 0) {
                Some(r) => {
                    stats.rosters += 1;
                    out(Event::Party { t_ms, members: r.members });
                }
                None => stats.roster_rejected += 1,
            },
            op::SPAWN => spawn::parse(t_ms, body, stats, out),
            op::DEATH => world::death(t_ms, body, stats, out),
            op::MAP_LOAD => {
                if world::map_load(t_ms, body, stats, out) {
                    // Entity ids are reissued on a zone change, so the skill use tally starts over.
                    self.scope_window.clear();
                    self.stats.hint_self_id = None;
                }
            }
            op::REMAIN_HP => world::remain_hp(t_ms, body, stats, out),
            op::HP_UPDATE => world::hp_update(t_ms, body, stats, out),
            op::OWNERSHIP => world::ownership(t_ms, body, stats, out),
            op::PONG => world::pong(t_ms, body, stats, out),
            op::PARTY_SCOPE => self.note_skill_use(body),
            _ if body.len() >= 16 => identity::scan_embedded(t_ms, body, stats, out),
            _ => {}
        }
    }

    /// `06 38 <entity varint> <skill u32 LE> ...`: tallies entities using player skills (10 000 000 to
    /// 19 999 999) over a sliding window and keeps `hint_self_id` on the clear leader. On the Global
    /// client the local player is the only player named this way; mobs are named too but with 7 digit
    /// skills. The window matters because the local id changes more often than the map does.
    fn note_skill_use(&mut self, body: &[u8]) {
        let mut c = crate::wire::Cursor::new(body);
        let Some(id) = c.varint() else { return };
        let Some(skill) = c.u32() else { return };
        if !(1..=MAX_ENTITY_ID).contains(&id) || !(10_000_000..=19_999_999).contains(&skill) {
            return;
        }
        if self.scope_window.len() >= SCOPE_WINDOW {
            self.scope_window.pop_front();
        }
        self.scope_window.push_back(id);
        let mut counts: BTreeMap<EntityId, u64> = BTreeMap::new();
        for &e in &self.scope_window {
            *counts.entry(e).or_default() += 1;
        }
        let mut top: Option<(EntityId, u64)> = None;
        let mut second = 0u64;
        for (&e, &n) in &counts {
            match top {
                Some((_, best)) if n <= best => second = second.max(n),
                Some((_, best)) => {
                    second = best;
                    top = Some((e, n));
                }
                None => top = Some((e, n)),
            }
        }
        self.stats.hint_self_id = match top {
            Some((e, n)) if n >= 24 && n >= 4 * second.max(1) => Some(e),
            _ => None,
        };
    }
}

/// A character name as the game states it: 1 to 16 characters, letters or digits in any script, at least
/// one letter. The `$` tutorial placeholder and anything with punctuation or control bytes fails.
pub fn valid_name(field: &[u8]) -> Option<&str> {
    let name = std::str::from_utf8(field).ok()?;
    let chars = name.chars().count();
    let ok = (1..=16).contains(&chars) && name.chars().all(char::is_alphanumeric) && name.chars().any(char::is_alphabetic);
    ok.then_some(name)
}
