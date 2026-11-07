//! `41 36` entity spawn: mobs, summons, spirits and skill effect entities.
//!
//! ```text
//! id varint, kind u8, flags u8, subtree u8, [subtree & 0x01: len u8, name], npc code u24 LE, 00 xx 02,
//! x f32, y f32, z f32, heading f32, ..., 01 <cur hp varint> <max hp varint>, ..., abnormal list,
//! ..., 07 02 06|01 <caster u32 LE>, [kind 0x5F: 06 <parent u32> <legion u32> <u16 0> <server u16> <len> <legion>]
//! ```
//! The community parsers read `kind flags subtree code[0]` as a u32 mask; the kind is its low byte and
//! bit 0x0010 (flags & 0x10) announces the parent block. The code is found structurally first and by the
//! bounded marker scan of the spec when a name shifts it.

use super::{valid_name, EntityId, Event, ParserStats, MAX_ENTITY_ID};
use crate::gamedata::GameData;
use crate::wire::{self, Cursor};

/// Kinds whose spawn names an owner: summon or pet, other players' spirits and ground spells, effects.
const OWNED_KINDS: [u8; 5] = [0x5F, 0x1F, 0x1D, 0x5D, 0x1C];
/// Entry of a permanent abnormal (length -1, end 2100-01-01); the caster varint follows it.
const ABNORMAL_ANCHOR: [u8; 8] = [0x80, 0x75, 0xD5, 0x2A, 0xBB, 0x03, 0x00, 0x00];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Spawn {
    pub id: EntityId,
    pub raw_id: u32,
    pub kind: u8,
    pub flags: u8,
    pub npc_code: u32,
    pub name: Option<String>,
    pub cur_hp: Option<u64>,
    pub max_hp: Option<u64>,
    pub owner: Option<EntityId>,
}

/// Ids above 1 000 000 are folded by every community parser; the origin of the fold is unexplained.
pub(super) fn fold(id: u32) -> u32 {
    if id > 1_000_000 {
        (id & 0x3FFF) | 0x4000
    } else {
        id
    }
}

pub(super) fn parse_body(body: &[u8]) -> Option<Spawn> {
    let mut c = Cursor::new(body);
    let raw_id = c.varint()?;
    if raw_id == 0 {
        return None;
    }
    let id = fold(raw_id);
    if !(1..=MAX_ENTITY_ID).contains(&id) {
        return None;
    }
    let mask_at = c.pos();
    let kind = c.u8()?;
    let flags = c.u8().unwrap_or(0);
    let mut spawn = Spawn { id, raw_id, kind, flags, npc_code: 0, name: None, cur_hp: None, max_hp: None, owner: None };

    // The inline name sits behind the u16 mask on this client; the u32 position is tried second.
    let mut after_name = mask_at + 3;
    for subtree in [mask_at + 2, mask_at + 4] {
        if let Some((name, end)) = name_at(body, subtree) {
            spawn.name = Some(name);
            after_name = end;
            break;
        }
    }

    // NPC code: structurally right after the subtree byte and name, else the first `00 (00|40) 02` marker
    // between 3 and 60 bytes after the mask.
    let marker = if matches!(body.get(after_name + 3..after_name + 6), Some([0x00, _, 0x02])) {
        Some(after_name + 3)
    } else {
        (mask_at + 6..=(mask_at + 60).min(body.len().saturating_sub(3)))
            .find(|&i| body[i] == 0x00 && matches!(body[i + 1], 0x00 | 0x40) && body[i + 2] == 0x02)
    };
    if let Some(m) = marker {
        let code = Cursor::at(body, m - 3).u24().unwrap_or(0);
        spawn.npc_code = code;
        // HP: the first `01 <cur> <max>` with max >= cur > 0 within 64 bytes after the marker.
        let hp_from = m + 3;
        let hp_to = (hp_from + 64).min(body.len());
        for i in hp_from..hp_to {
            if body[i] != 0x01 {
                continue;
            }
            let mut h = Cursor::at(body, i + 1);
            if let (Some(cur), Some(max)) = (h.varint(), h.varint()) {
                if cur > 0 && max >= cur {
                    spawn.cur_hp = Some(u64::from(cur));
                    spawn.max_hp = Some(u64::from(max));
                    break;
                }
            }
        }
    }

    if OWNED_KINDS.contains(&kind) {
        let candidate = parent_block(body, mask_at, id, kind, flags)
            .or_else(|| anchor_owner(body, mask_at, id))
            .or_else(|| caster_field(body, mask_at, id));
        spawn.owner = candidate;
    }
    Some(spawn)
}

fn name_at(body: &[u8], subtree: usize) -> Option<(String, usize)> {
    if body.get(subtree)? & 0x01 == 0 {
        return None;
    }
    let len = usize::from(*body.get(subtree + 1)?);
    if !(1..=48).contains(&len) {
        return None;
    }
    let field = body.get(subtree + 2..subtree + 2 + len)?;
    Some((valid_name(field)?.to_string(), subtree + 2 + len))
}

fn owner_ok(owner: u32, this: EntityId) -> Option<EntityId> {
    ((1..=MAX_ENTITY_ID).contains(&owner) && owner != this).then_some(owner)
}

/// Summons (0x5F) with flags bit 0x10 carry their owner in a block preceded by 0x06:
/// `<parent u32> <legion id u32> <u16 0> <server u16 1..=9999> <len u8 <= 40> <legion name>`.
fn parent_block(body: &[u8], from: usize, this: EntityId, kind: u8, flags: u8) -> Option<EntityId> {
    if kind != 0x5F || flags & 0x10 == 0 {
        return None;
    }
    let mut i = from.max(1);
    while i + 13 <= body.len() {
        if body[i - 1] == 0x06 {
            let parent = u32::from_le_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]);
            let pad = u16::from_le_bytes([body[i + 8], body[i + 9]]);
            let server = u16::from_le_bytes([body[i + 10], body[i + 11]]);
            let len = usize::from(body[i + 12]);
            if pad == 0 && (1..=9_999).contains(&server) && len <= 40 {
                if let Some(field) = body.get(i + 13..i + 13 + len) {
                    if std::str::from_utf8(field).is_ok() {
                        if let Some(o) = owner_ok(parent, this) {
                            return Some(o);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// The caster varint after the permanent abnormal entry, within 120 bytes of the mask.
fn anchor_owner(body: &[u8], from: usize, this: EntityId) -> Option<EntityId> {
    let last = body.len().checked_sub(ABNORMAL_ANCHOR.len())?.min(from + 120);
    (from..=last)
        .filter(|&i| body[i..].starts_with(&ABNORMAL_ANCHOR))
        .find_map(|i| wire::peek_varint(body, i + ABNORMAL_ANCHOR.len()).and_then(|(v, _)| owner_ok(v, this)))
}

/// `07 02 06 <u32>` or `07 02 01 <u32>` after the position, within 240 bytes of the mask.
fn caster_field(body: &[u8], from: usize, this: EntityId) -> Option<EntityId> {
    let end = body.len().min(from + 240);
    let window = body.get(from..end)?;
    let at = window.windows(3).position(|w| w[0] == 0x07 && w[1] == 0x02 && matches!(w[2], 0x06 | 0x01))?;
    let i = from + at + 3;
    let caster = u32::from_le_bytes(body.get(i..i + 4)?.try_into().ok()?);
    owner_ok(caster, this)
}

pub(super) fn parse(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let Some(s) = parse_body(body) else {
        stats.spawn_rejected += 1;
        return;
    };
    stats.spawns += 1;
    if s.raw_id != s.id {
        stats.spawn_folded += 1;
    }
    if s.npc_code == 0 {
        stats.spawn_no_code += 1;
    }
    if s.owner.is_some() {
        stats.spawn_owners += 1;
    }
    out(Event::NpcSpawn { t_ms, id: s.id, npc_code: s.npc_code, kind: s.kind, cur_hp: s.cur_hp, max_hp: s.max_hp, owner: s.owner });
    // A summon's inline name is its owner's character name (A2Tools `name_summon_owner`): with the owner
    // id resolved, the pair names a player whose own record the capture may have missed. Only for kind
    // 0x5F (an effect entity's caster can be a boss while its name is the player it targets) and only
    // when the template is one a player can summon, or the record carried no code: a mob's summon is
    // named after the player it was sent against, never after its owner (PROTOCOL.md 9.1; A2Tools V101,
    // Blazing Totem 2920063 naming "Abcd"). The tracker treats the record as weak on top of that.
    if let (Some(name), Some(owner), 0x5F) = (s.name, s.owner, s.kind) {
        if s.npc_code == 0 || GameData::get().is_player_summon_npc(s.npc_code) {
            stats.spawn_names += 1;
            out(Event::Identity { t_ms, id: owner, name, server: 0, job: 0, class_id: 0, level: 0, is_self: false });
        } else {
            stats.spawn_names_rejected += 1;
        }
    }
}
