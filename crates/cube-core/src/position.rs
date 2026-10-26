//! Records that carry the character's own position.
//!
//! The server never echoes your own movement, and what the client sends is encrypted, so the radar takes the
//! position from records that happen to carry it (dr89mh FINDINGS P1 to P7, Global build 25800038):
//!
//! ```text
//! 2a 38  ..., caster varint, 01 00, x f32, y f32, z f32            buff record, position of the caster
//! 2b 38  ..., caster varint, u8, u32, 02, x f32, y f32, z f32      buff record, position of the caster
//! 21 36  load count u32, map u32, u32, u32, x f32, y f32, z f32, heading f32, ...   own position after a load
//! 23 36  entity varint (0 = you), 5 bytes, x f32, y f32, z f32, ...                 own position after a teleport
//! ```
//! Buff records name other entities too; the tracker keeps only the ones for the local character. They come
//! every few seconds in a fight and every 5 to 20 seconds while walking, so the distance shown is as old as the
//! last fix and the views say so.

use crate::geo::Vec3;
use crate::parser::{EntityId, Event, ParserStats, PositionSource, MAX_ENTITY_ID};
use crate::wire::{self, Cursor};

const XYZ: usize = 12;

fn xyz_at(body: &[u8], at: usize) -> Option<Vec3> {
    let mut c = Cursor::at(body, at);
    if c.remaining() < XYZ {
        return None;
    }
    let pos = Vec3::new(c.f32()?, c.f32()?, c.f32()?);
    pos.is_world_position().then_some(pos)
}

/// The caster varint ends `gap` bytes before the trailing x y z. A start is only accepted where the byte before it
/// has no continuation bit, otherwise it would belong to the varint (the last byte of a long varint is itself a
/// valid short one).
fn caster_tail(body: &[u8], gap: usize) -> Option<(EntityId, Vec3)> {
    let end = body.len().checked_sub(XYZ + gap)?;
    for width in 1..=3 {
        let Some(start) = end.checked_sub(width).filter(|&s| s >= 1) else { continue };
        if body[start - 1] & 0x80 != 0 {
            continue;
        }
        if let Some((caster, n)) = wire::peek_varint(body, start) {
            if start + n == end && (1..=MAX_ENTITY_ID).contains(&caster) {
                return Some((caster, xyz_at(body, body.len() - XYZ)?));
            }
        }
    }
    None
}

/// Entity and position from a `2a 38` or `2b 38` body, when it has the position tail.
pub fn buff_position(opcode: u16, body: &[u8]) -> Option<(EntityId, Vec3)> {
    let n = body.len();
    match opcode {
        crate::opcodes::BUFF_POSITION if n >= 16 && body[n - 14] == 0x01 && body[n - 13] == 0x00 => caster_tail(body, 2),
        crate::opcodes::BUFF_POSITION_ALT if n >= 20 && body[n - 13] == 0x02 => caster_tail(body, 6),
        _ => None,
    }
}

pub(crate) fn buff(t_ms: u64, opcode: u16, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let source = if opcode == crate::opcodes::BUFF_POSITION { PositionSource::Buff } else { PositionSource::BuffAlt };
    match buff_position(opcode, body) {
        Some((entity, pos)) => {
            stats.position_fixes += 1;
            out(Event::Position { t_ms, entity: Some(entity), pos, source });
        }
        // Not every buff record ends in a position, so this is a count, not a failure.
        None => stats.buffs_without_position += 1,
    }
}

/// `23 36`: only entity 0, the local player, is read.
pub(crate) fn teleport(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    if body.first() != Some(&0) {
        return;
    }
    match xyz_at(body, 6) {
        Some(pos) => {
            stats.teleports += 1;
            out(Event::Position { t_ms, entity: None, pos, source: PositionSource::Teleport });
        }
        None => stats.position_rejected += 1,
    }
}

/// Own position from a `21 36` body, at offset 16.
pub fn map_load_position(body: &[u8]) -> Option<Vec3> {
    xyz_at(body, 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcodes as op;
    use crate::parser::tests::hex;

    // dr89mh fixtures/selfpos/2026-10-13_position_sources.json and fixtures/cubes/*.json, own entity 9180.
    #[test]
    fn reads_the_caster_tail_of_both_buff_records() {
        let (e, p) = buff_position(op::BUFF_POSITION, &hex("dc4701119003e8030000881300000000000023519634a1010000dc470100b3984a46a2799fc7513e5944")).unwrap();
        assert_eq!(e, 9_180);
        assert!((p.x - 12_966.17).abs() < 0.01 && (p.y + 81_651.27).abs() < 0.01 && (p.z - 868.97).abs() < 0.01, "{p:?}");
        let (e, p) = buff_position(op::BUFF_POSITION_ALT, &hex("dc47138403e7223508102700000000000028619634a1010000dc4701171dd20002e7404a4695a19fc700405944")).unwrap();
        assert_eq!(e, 9_180);
        assert!((p.x - 12_944.23).abs() < 0.01 && (p.y + 81_731.16).abs() < 0.01, "{p:?}");
    }

    #[test]
    fn names_other_casters_with_long_ids() {
        // A two byte caster after a byte with the continuation bit, and a three byte one.
        let (e, _) = buff_position(op::BUFF_POSITION, &hex("963501139611abf8290b88130000000000000320a634a101000096350111cc1d01004a7b73c7a3bfeb47e55c8e46")).unwrap();
        assert_eq!(e, 3_788);
        let (e, _) = buff_position(op::BUFF_POSITION, &hex("d8820101130137e2f505b80b000000000000271aa634a1010000963501d0dc150100558673c7bfc6eb4700588e46")).unwrap();
        assert_eq!(e, 355_920);
        let (e, _) = buff_position(op::BUFF_POSITION_ALT, &hex("9360138806b393d70a6009000000000000ada8a534a101000093600ac58e150102ff6169c7983c0448002c8246")).unwrap();
        assert_eq!(e, 12_307);
    }

    #[test]
    fn other_shapes_carry_no_position() {
        assert_eq!(buff_position(op::BUFF_POSITION, &[0u8; 15]), None);
        assert_eq!(buff_position(op::BUFF_POSITION, &[0u8; 40]), None, "no 01 00 before the tail");
        let mut small = hex("dc4701119003e8030000881300000000000023519634a1010000dc470100");
        small.extend_from_slice(&[0, 0, 0xD8, 0xC2, 0, 0, 0xF8, 0xC1, 0, 0, 0xBE, 0xC2]);
        assert_eq!(buff_position(op::BUFF_POSITION, &small), None, "a small vector is not a world position");
        assert_eq!(buff_position(op::OBJECT_SPAWN, &hex("dc4701119003e8030000881300000000000023519634a1010000dc470100b3984a46a2799fc7513e5944")), None);
    }

    #[test]
    fn reads_the_own_position_after_a_load_and_a_teleport() {
        let load = hex("0b000000f20300003a03870000000000587274c71c5ea74700bc4e4680e333430200000000000000000000000000");
        let p = map_load_position(&load).unwrap();
        assert!((p.x + 62_578.34).abs() < 0.01 && (p.y - 85_692.22).abs() < 0.01 && (p.z - 13_231.0).abs() < 0.01, "{p:?}");
        let mut stats = ParserStats::default();
        let mut events = Vec::new();
        teleport(5, &hex("000000000000587274c71c5ea74700bc4e4605000000a805"), &mut stats, &mut |e| events.push(e));
        assert_eq!(events, vec![Event::Position { t_ms: 5, entity: None, pos: p, source: PositionSource::Teleport }]);
        teleport(6, &hex("070000000000587274c71c5ea74700bc4e4605000000a805"), &mut stats, &mut |e| events.push(e));
        assert_eq!(events.len(), 1, "another entity's teleport is not ours");
        assert_eq!(stats.teleports, 1);
    }
}
