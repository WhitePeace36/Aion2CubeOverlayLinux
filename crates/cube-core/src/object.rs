//! World object records: `34 36` spawn, `35 36` removal, `38 36` and `3a 36` opening.
//!
//! ```text
//! 34 36  id varint, kind u16, template u32, x f32, y f32, z f32, f32, heading f32, u8, u8, state u8, created u64 ms, ...
//! 35 36  id varint, reason u8 (01 left view, 04 opened or destroyed), ...
//! 38 36  object varint, actor varint, 01
//! 3a 36  object varint, actor varint, count u32, item u32
//! ```
//! Layouts from Cube Watch, verified by the dr89mh radar on Global build 25800038 (2026-10-13). A Hidden Cube is a
//! spawn whose template is in the cube table (`cubes.rs`); everything else here (gathering nodes, chests) is only
//! counted. `created` is the Unix ms time the object spawned: static objects carry the server start, cubes the
//! moment they appeared.
//!
//! Every reader counts the bodies it drops, so a patch that moves a layout shows up as a rising counter on the
//! Setup view instead of as cubes quietly never appearing.

use crate::geo::Vec3;
use crate::parser::{EntityId, Event, ParserStats, RemoveReason, MAX_ENTITY_ID};
use crate::wire::Cursor;

/// Bytes after the id varint up to and including the heading.
const SPAWN_FIXED: usize = 26;
/// Bytes after the id varint up to the end of the created time.
const SPAWN_WITH_CREATED: usize = 37;
/// A created time outside these bounds (2020 to 2052) is a misread and dropped.
const CREATED_RANGE: std::ops::RangeInclusive<u64> = 1_600_000_000_000..=2_600_000_000_000;

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSpawn {
    pub id: EntityId,
    pub kind: u16,
    pub template: u32,
    pub pos: Vec3,
    pub heading: f32,
    pub state: u8,
    pub created_ms: Option<u64>,
}

fn valid_id(id: u32) -> Option<EntityId> {
    (1..=MAX_ENTITY_ID).contains(&id).then_some(id)
}

pub fn parse_spawn(body: &[u8]) -> Option<ObjectSpawn> {
    let mut c = Cursor::new(body);
    let id = valid_id(c.varint()?)?;
    let o = c.pos();
    if body.len() < o + SPAWN_FIXED {
        return None;
    }
    let kind = c.u16()?;
    let template = c.u32()?;
    if template == 0 {
        return None;
    }
    let pos = Vec3::new(c.f32()?, c.f32()?, c.f32()?);
    if !pos.in_bounds() {
        return None;
    }
    let _unknown = c.f32()?;
    let heading = c.f32()?;
    let (mut state, mut created_ms) = (0, None);
    if body.len() >= o + SPAWN_WITH_CREATED {
        c.skip(2)?;
        state = c.u8()?;
        created_ms = c.u64().filter(|t| CREATED_RANGE.contains(t));
    }
    Some(ObjectSpawn { id, kind, template, pos, heading, state, created_ms })
}

pub(crate) fn spawn(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let Some(s) = parse_spawn(body) else {
        stats.object_rejected += 1;
        return;
    };
    stats.object_spawns += 1;
    out(Event::ObjectSpawn { t_ms, id: s.id, kind: s.kind, template: s.template, pos: s.pos, heading: s.heading, state: s.state, created_ms: s.created_ms });
}

pub(crate) fn removed(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let (Some(id), Some(reason)) = (c.varint().and_then(valid_id), c.u8()) else {
        stats.object_rejected += 1;
        return;
    };
    stats.object_removals += 1;
    let reason = match reason {
        0x01 => RemoveReason::LeftView,
        0x04 => RemoveReason::Opened,
        other => RemoveReason::Other(other),
    };
    out(Event::ObjectRemoved { t_ms, id, reason });
}

/// `38 36` (done = false) and `3a 36` (done = true).
pub(crate) fn open(t_ms: u64, body: &[u8], done: bool, stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let (Some(object), Some(actor)) = (c.varint().and_then(valid_id), c.varint().and_then(valid_id)) else {
        stats.object_rejected += 1;
        return;
    };
    if !done {
        stats.opens_started += 1;
        out(Event::OpenStart { t_ms, object, actor });
        return;
    }
    stats.opens_done += 1;
    let (count, item) = match (c.u32(), c.u32()) {
        (Some(count), Some(item)) => (count, item),
        _ => (0, 0),
    };
    out(Event::OpenDone { t_ms, object, actor, count, item });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::parser::tests::hex;

    // dr89mh fixtures/cubes/2026-10-13_verteron_two_cubes.json: cube 65509, template 1200039, opened by entity 9180.
    pub(crate) const CUBE_SPAWN: &str = "e5ff030400a74f1200001d44c780a5f647006471460000000000000000010101aae4a534a101000000000000000000002c4d4502070000";
    const CUBE_SPAWN_TURNED: &str = "868d020400a74f1200a0b447c7b26c034800de964600000000002490430101014ab6a734a101000000000000000000002c4d4502070000";

    #[test]
    fn reads_a_cube_spawn() {
        let s = parse_spawn(&hex(CUBE_SPAWN)).unwrap();
        assert_eq!(s.id, 65_509);
        assert_eq!(s.kind, 4);
        assert_eq!(s.template, 1_200_039);
        assert_eq!(s.pos, Vec3::new(-50_205.0, 126_283.0, 15_449.0));
        assert_eq!(s.heading, 0.0);
        assert_eq!(s.state, 1);
        assert_eq!(s.created_ms, Some(1_791_884_649_642));
        let s = parse_spawn(&hex(CUBE_SPAWN_TURNED)).unwrap();
        assert_eq!(s.id, 34_438);
        assert!((s.heading - 288.28).abs() < 0.01, "{}", s.heading);
    }

    #[test]
    fn spawns_without_the_tail_still_read() {
        let body = hex(CUBE_SPAWN);
        let s = parse_spawn(&body[..3 + SPAWN_FIXED]).unwrap();
        assert_eq!(s.template, 1_200_039);
        assert_eq!(s.created_ms, None);
        assert!(parse_spawn(&body[..3 + SPAWN_FIXED - 1]).is_none(), "one byte short of the heading");
    }

    #[test]
    fn rejects_spawns_that_do_not_validate() {
        let mut stats = ParserStats::default();
        let mut events = Vec::new();
        let mut body = hex(CUBE_SPAWN);
        body[9..13].copy_from_slice(&f32::NAN.to_le_bytes());
        spawn(1, &body, &mut stats, &mut |e| events.push(e));
        let mut zero_template = hex(CUBE_SPAWN);
        zero_template[5..9].fill(0);
        spawn(1, &zero_template, &mut stats, &mut |e| events.push(e));
        spawn(1, &[0x00, 0x04, 0x00], &mut stats, &mut |e| events.push(e));
        assert!(events.is_empty());
        assert_eq!(stats.object_rejected, 3);
    }

    #[test]
    fn reads_the_opening_and_the_removal() {
        let mut stats = ParserStats::default();
        let mut events = Vec::new();
        open(1, &hex("e5ff03dc4701"), false, &mut stats, &mut |e| events.push(e));
        open(2, &hex("e5ff03dc470100000023c59a25"), true, &mut stats, &mut |e| events.push(e));
        removed(3, &hex("e5ff03040300"), &mut stats, &mut |e| events.push(e));
        removed(4, &hex("97ff030101"), &mut stats, &mut |e| events.push(e));
        assert_eq!(
            events,
            vec![
                Event::OpenStart { t_ms: 1, object: 65_509, actor: 9_180 },
                Event::OpenDone { t_ms: 2, object: 65_509, actor: 9_180, count: 1, item: 630_900_003 },
                Event::ObjectRemoved { t_ms: 3, id: 65_509, reason: RemoveReason::Opened },
                Event::ObjectRemoved { t_ms: 4, id: 65_431, reason: RemoveReason::LeftView },
            ]
        );
        assert_eq!((stats.opens_started, stats.opens_done, stats.object_removals, stats.object_rejected), (1, 1, 2, 0));
    }
}
