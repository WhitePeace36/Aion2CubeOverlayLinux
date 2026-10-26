//! Game packets to tracker events.
//!
//! A cube alert needs few packets: the heartbeat (a server clock and proof the game is connected), the map load
//! (ids are reissued per map, and it carries your position), the self record (which entity you are), the world
//! object records (`object.rs`) and the records that carry positions (`position.rs`). Everything else is only
//! counted per opcode, for `replay --opcodes` and the Setup view.

use crate::geo::Vec3;
use crate::opcodes as op;
use crate::wire::Cursor;
use crate::{object, position};
use serde::Serialize;
use std::collections::BTreeMap;

pub type EntityId = u32;
pub const MAX_ENTITY_ID: EntityId = 9_999_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveReason {
    /// The object left your view range; it still exists.
    LeftView,
    /// Opened (a cube) or destroyed.
    Opened,
    Other(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionSource {
    /// `2a 38` buff record.
    Buff,
    /// `2b 38` buff record.
    BuffAlt,
    /// `21 36` map load.
    MapLoad,
    /// `23 36` teleport.
    Teleport,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Event {
    /// The server heartbeat, about twenty per second while connected.
    Heartbeat { t_ms: u64, server_ms: u64 },
    /// A loading screen: `map_id` is the map now loaded.
    ZoneChange { t_ms: u64, map_id: u32 },
    /// The local character.
    SelfInfo { t_ms: u64, id: EntityId, name: String, server: u16, level: u32 },
    /// A world object came into view.
    ObjectSpawn { t_ms: u64, id: EntityId, kind: u16, template: u32, pos: Vec3, heading: f32, state: u8, created_ms: Option<u64> },
    ObjectRemoved { t_ms: u64, id: EntityId, reason: RemoveReason },
    OpenStart { t_ms: u64, object: EntityId, actor: EntityId },
    /// `item` is what the opening took; 0 when the record carried no tail.
    OpenDone { t_ms: u64, object: EntityId, actor: EntityId, count: u32, item: u32 },
    /// A position fix. `entity` is `None` for records that only ever describe the local player.
    Position { t_ms: u64, entity: Option<EntityId>, pos: Vec3, source: PositionSource },
}

impl Event {
    pub fn t_ms(&self) -> u64 {
        match self {
            Event::Heartbeat { t_ms, .. }
            | Event::ZoneChange { t_ms, .. }
            | Event::SelfInfo { t_ms, .. }
            | Event::ObjectSpawn { t_ms, .. }
            | Event::ObjectRemoved { t_ms, .. }
            | Event::OpenStart { t_ms, .. }
            | Event::OpenDone { t_ms, .. }
            | Event::Position { t_ms, .. } => *t_ms,
        }
    }

    /// Short lower case name of the variant, as used by `replay --events <type>`.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Heartbeat { .. } => "heartbeat",
            Event::ZoneChange { .. } => "zone",
            Event::SelfInfo { .. } => "self",
            Event::ObjectSpawn { .. } => "spawn",
            Event::ObjectRemoved { .. } => "removed",
            Event::OpenStart { .. } | Event::OpenDone { .. } => "open",
            Event::Position { .. } => "position",
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParserStats {
    pub packets: u64,
    pub heartbeats: u64,
    pub first_heartbeat_ms: Option<u64>,
    pub last_heartbeat_ms: Option<u64>,
    /// Server clock from the latest heartbeat.
    pub server_time_ms: Option<u64>,
    pub zone_changes: u64,
    pub map_rejected: u64,
    pub self_records: u64,
    pub self_rejected: u64,
    pub object_spawns: u64,
    pub object_removals: u64,
    pub opens_started: u64,
    pub opens_done: u64,
    /// Object records (`34 36`, `35 36`, `38 36`, `3a 36`) that did not validate.
    pub object_rejected: u64,
    pub position_fixes: u64,
    pub teleports: u64,
    /// Buff records without the position tail. Many are normal; all of them means the layout moved.
    pub buffs_without_position: u64,
    /// Load and teleport records naming the local player whose position did not validate.
    pub position_rejected: u64,
    #[serde(skip)]
    pub opcode_counts: BTreeMap<u16, u64>,
    #[serde(skip)]
    pub opcode_bytes: BTreeMap<u16, u64>,
}

#[derive(Default)]
pub struct Parser {
    pub stats: ParserStats,
}

impl Parser {
    /// Decodes one packet. `t_ms` is the capture time in Unix milliseconds.
    pub fn packet(&mut self, t_ms: u64, opcode: u16, body: &[u8], out: &mut dyn FnMut(Event)) {
        let s = &mut self.stats;
        s.packets += 1;
        *s.opcode_counts.entry(opcode).or_default() += 1;
        *s.opcode_bytes.entry(opcode).or_default() += body.len() as u64;
        match opcode {
            op::HEARTBEAT => {
                s.heartbeats += 1;
                s.first_heartbeat_ms.get_or_insert(t_ms);
                s.last_heartbeat_ms = Some(t_ms);
                let server_ms = Cursor::new(body).u64().unwrap_or(0);
                if server_ms > 0 {
                    s.server_time_ms = Some(server_ms);
                }
                out(Event::Heartbeat { t_ms, server_ms });
            }
            op::MAP_LOAD => map_load(t_ms, body, s, out),
            op::TELEPORT => position::teleport(t_ms, body, s, out),
            op::SELF_INFO => self_info(t_ms, body, s, out),
            op::OBJECT_SPAWN => object::spawn(t_ms, body, s, out),
            op::OBJECT_REMOVED => object::removed(t_ms, body, s, out),
            op::OPEN_START => object::open(t_ms, body, false, s, out),
            op::OPEN_DONE => object::open(t_ms, body, true, s, out),
            op::BUFF_POSITION | op::BUFF_POSITION_ALT => position::buff(t_ms, opcode, body, s, out),
            _ => {}
        }
    }
}

/// `21 36 <u32 load count> <u32 map id> <u32> <u32> <x y z f32> <heading f32> ...`.
fn map_load(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let (Some(_count), Some(map_id)) = (c.u32(), c.u32()) else {
        stats.map_rejected += 1;
        return;
    };
    if !(1..=9_999_999).contains(&map_id) {
        stats.map_rejected += 1;
        return;
    }
    stats.zone_changes += 1;
    out(Event::ZoneChange { t_ms, map_id });
    if let Some(pos) = position::map_load_position(body) {
        stats.position_fixes += 1;
        out(Event::Position { t_ms, entity: None, pos, source: PositionSource::MapLoad });
    }
}

/// `33 36 <id varint> <mask1 u32> <mask2 u8> [mask2 & 1: <len u8> <name>] <server u16> <job u32> <u8>
/// <level u32>`, the same reading as the aion2dps meter and the aion2kina tracker.
fn self_info(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let parsed = (|| {
        let mut c = Cursor::new(body);
        let id = c.varint()?;
        if !(1..=MAX_ENTITY_ID).contains(&id) {
            return None;
        }
        let _mask1 = c.u32()?;
        let mask2 = c.u8()?;
        if mask2 & 0x01 == 0 {
            return None;
        }
        let len = usize::from(c.u8()?);
        if !(1..=48).contains(&len) {
            return None;
        }
        let name = valid_name(c.bytes(len)?)?.to_string();
        let server = c.u16()?;
        if !(1000..=2999).contains(&server) {
            return None;
        }
        let mut level = 0;
        if let (Some(_job), Some(_), Some(lv)) = (c.u32(), c.u8(), c.u32()) {
            if (1..=99).contains(&lv) {
                level = lv;
            }
        }
        Some((id, name, server, level))
    })();
    match parsed {
        Some((id, name, server, level)) => {
            stats.self_records += 1;
            out(Event::SelfInfo { t_ms, id, name, server, level });
        }
        None => stats.self_rejected += 1,
    }
}

/// A character name: 1 to 16 letters or digits, at least one letter.
pub fn valid_name(field: &[u8]) -> Option<&str> {
    let name = std::str::from_utf8(field).ok()?;
    let chars = name.chars().count();
    let ok = (1..=16).contains(&chars) && name.chars().all(char::is_alphanumeric) && name.chars().any(char::is_alphabetic);
    ok.then_some(name)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn decode(opcode: u16, body: &[u8]) -> (Vec<Event>, ParserStats) {
        let mut p = Parser::default();
        let mut events = Vec::new();
        p.packet(1_000, opcode, body, &mut |e| events.push(e));
        (events, p.stats)
    }

    #[test]
    fn dispatches_the_object_records() {
        let (events, stats) = decode(op::OBJECT_SPAWN, &hex(crate::object::tests::CUBE_SPAWN));
        assert!(matches!(events[..], [Event::ObjectSpawn { id: 65_509, template: 1_200_039, .. }]), "{events:?}");
        assert_eq!(stats.object_spawns, 1);
        assert_eq!(stats.opcode_counts.get(&op::OBJECT_SPAWN), Some(&1));
        let (events, _) = decode(op::OPEN_DONE, &hex("c3af03dc470100000023c59a25"));
        assert_eq!(events, vec![Event::OpenDone { t_ms: 1_000, object: 55_235, actor: 9_180, count: 1, item: 630_900_003 }]);
    }

    #[test]
    fn map_loads_carry_the_own_position() {
        let (events, stats) = decode(op::MAP_LOAD, &hex("0b000000f20300003a03870000000000587274c71c5ea74700bc4e4680e333430200000000000000000000000000"));
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], Event::ZoneChange { t_ms: 1_000, map_id: 1010 });
        assert!(matches!(events[1], Event::Position { entity: None, source: PositionSource::MapLoad, .. }));
        assert_eq!(stats.position_fixes, 1);
        // A load without a position (zeros) is still a zone change.
        let mut body = 1u32.to_le_bytes().to_vec();
        body.extend_from_slice(&600_072u32.to_le_bytes());
        body.extend_from_slice(&[0; 44]);
        let (events, _) = decode(op::MAP_LOAD, &body);
        assert_eq!(events, vec![Event::ZoneChange { t_ms: 1_000, map_id: 600_072 }]);
        let (events, stats) = decode(op::MAP_LOAD, &[1, 0]);
        assert!(events.is_empty());
        assert_eq!(stats.map_rejected, 1);
    }

    #[test]
    fn reads_the_self_record_and_heartbeats() {
        // id 14513, masks, "Seraphel", server 2314, job 15, a byte, level 45.
        let mut body = hex("b1715e91c1283708");
        body.extend_from_slice(b"Seraphel");
        body.extend_from_slice(&hex("0a090f00000000"));
        body.extend_from_slice(&45u32.to_le_bytes());
        let (events, stats) = decode(op::SELF_INFO, &body);
        assert_eq!(events, vec![Event::SelfInfo { t_ms: 1_000, id: 14_513, name: "Seraphel".into(), server: 2314, level: 45 }]);
        assert_eq!(stats.self_records, 1);
        let (events, stats) = decode(op::HEARTBEAT, &1_791_722_111_217u64.to_le_bytes());
        assert_eq!(events, vec![Event::Heartbeat { t_ms: 1_000, server_ms: 1_791_722_111_217 }]);
        assert_eq!(stats.server_time_ms, Some(1_791_722_111_217));
    }

    #[test]
    fn names_are_letters_and_digits() {
        assert_eq!(valid_name(b"Ari"), Some("Ari"));
        assert_eq!(valid_name(b"1234"), None);
        assert_eq!(valid_name(b"a b"), None);
    }
}
