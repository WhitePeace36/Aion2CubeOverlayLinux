//! `02 97` party roster.
//!
//! ```text
//! party_key u32, party_name <len u8 1..=40> <utf8>, party_size u8 1..=12, dungeon_id u32, 2 bytes,
//! leader_dbid u64, 3 bytes, member_count varint 1..=12, then per member:
//!   presence_mask u8, slot u8, dbid u64 (high u16 = world id), nickname <len u8> <utf8> (len 0 = vacant),
//!   class u32, level u32 1..=200, gear_score u32 0..=1 000 000, ... world id u16 ... , role u8,
//!   combat_power u64 <= 100 000 000, variable tail
//! ```
//! The stretch between gear score and combat power and the member tail are not fixed width (A2Tools,
//! 15 August 2026 capture; the same in the local captures), so combat power is found through the world id
//! anchor and the next member is reacquired by its header.

use super::{valid_name, PartyMember};
use crate::gamedata::GameData;
use crate::wire::Cursor;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Roster {
    pub members: Vec<PartyMember>,
    pub dungeon_id: u32,
    pub complete: bool,
}

pub(super) fn parse_at(data: &[u8], at: usize) -> Option<Roster> {
    let gd = GameData::get();
    let mut c = Cursor::at(data, at);
    let _party_key = c.u32()?;
    let name_len = usize::from(c.u8()?);
    if !(1..=40).contains(&name_len) {
        return None;
    }
    std::str::from_utf8(c.bytes(name_len)?).ok()?;
    let party_size = c.u8()?;
    if !(1..=12).contains(&party_size) {
        return None;
    }
    let dungeon_id = c.u32()?;
    c.skip(2 + 8 + 3)?;
    let count = c.varint()?;
    if !(1..=12).contains(&count) {
        return None;
    }
    let mut o = c.pos();
    let mut members = Vec::new();
    let mut complete = false;
    for _ in 0..count {
        if o + 20 > data.len() {
            break;
        }
        let slot = data[o + 1];
        let mut m = Cursor::at(data, o + 2);
        let dbid = m.u64()?;
        let world_id = (dbid >> 48) as u16;
        let nick_len = usize::from(m.u8()?);
        if nick_len == 0 {
            // A vacant slot; later slots may still be filled.
            match find_later_member(data, m.pos(), slot, count) {
                Some(next) => {
                    o = next;
                    continue;
                }
                None => {
                    complete = true;
                    break;
                }
            }
        }
        if nick_len > 40 {
            break;
        }
        let Some(name) = m.bytes(nick_len).and_then(valid_name) else { break };
        let name = name.to_string();
        let (Some(job), Some(level), Some(gear_score)) = (m.u32(), m.u32(), m.u32()) else { break };
        if !(1..=200).contains(&level) || gear_score > 1_000_000 {
            break;
        }
        let Some(anchor) = find_u16(data, m.pos(), m.pos() + 10, world_id) else { break };
        let mut p = Cursor::at(data, anchor + 2 + 2 + 1);
        let Some(combat_power) = p.u64() else { break };
        if combat_power > 100_000_000 {
            break;
        }
        members.push(PartyMember {
            name,
            job,
            class_id: gd.class_from_job(job).unwrap_or(0),
            level,
            gear_score,
            combat_power,
        });
        if u32::from(slot) >= count {
            complete = true;
            break;
        }
        match find_member_within(data, p.pos(), 32, slot.wrapping_add(1)).or_else(|| find_vacant_slot(data, p.pos(), slot.wrapping_add(1))) {
            Some(next) => o = next,
            None => break,
        }
    }
    if members.is_empty() {
        return None;
    }
    Some(Roster { members, dungeon_id, complete })
}

fn find_u16(data: &[u8], from: usize, to: usize, wanted: u16) -> Option<usize> {
    let end = to.min(data.len().checked_sub(2)?);
    (from..=end).find(|&i| u16::from_le_bytes([data[i], data[i + 1]]) == wanted)
}

/// A named member in a slot after `slot`, past the vacant records between (about 36 bytes each).
fn find_later_member(data: &[u8], from: usize, slot: u8, count: u32) -> Option<usize> {
    let last = u8::try_from(count).ok()?;
    (slot.checked_add(1)?..=last).find_map(|next| find_member_within(data, from, 48 * usize::from(next - slot), next))
}

/// A vacant record for `expected_slot` near `from`: a zero mask, the slot, a zero dbid and an empty name.
fn find_vacant_slot(data: &[u8], from: usize, expected_slot: u8) -> Option<usize> {
    let end = (from + 32).min(data.len().checked_sub(11)?);
    (from..=end).find(|&i| data[i] == 0 && data[i + 1] == expected_slot && data[i + 2..i + 11].iter().all(|&b| b == 0))
}

/// The header of a filled member record: `<mask != 0> <slot> <dbid with a world id 1..=9999> <len 1..=40>
/// <character name>`. The name must be a real one (A2Tools accepts any UTF-8 there, which matches ten
/// bytes early on the `.. 01 01 1E 02 ..` tail of the local captures).
fn find_member_within(data: &[u8], from: usize, span: usize, expected_slot: u8) -> Option<usize> {
    let end = (from + span).min(data.len().checked_sub(12)?);
    for i in from..=end {
        if data[i] == 0 || data[i + 1] != expected_slot {
            continue;
        }
        let world_id = u16::from_le_bytes([data[i + 8], data[i + 9]]);
        if world_id == 0 || world_id > 9_999 {
            continue;
        }
        let name_len = usize::from(data[i + 10]);
        if name_len == 0 || name_len > 40 {
            continue;
        }
        if data.get(i + 11..i + 11 + name_len).and_then(valid_name).is_none() {
            continue;
        }
        return Some(i);
    }
    None
}
