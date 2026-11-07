//! Death, map load, remaining HP, HP update, ownership and pong records.
//!
//! Every reader counts the bodies it drops (`death_rejected`, `map_rejected`, `hp_rejected`,
//! `pong_rejected`): a patch that moves one of these layouts then shows up as a rising counter instead
//! of as deaths and HP reports quietly falling to zero, which the summary could not tell from a calm
//! session. On the local captures the counters read 0 (the `00 8D` layout was checked on 11 414
//! session1 packets without a structural mismatch).

use super::{valid_name, DeathKind, Event, ParserStats, MAX_ENTITY_ID};
use crate::wire::Cursor;

/// `42 36 <id varint> <varint 0> <flag varint>`: 3 died, 7 gone from the world, 1 loaded dead.
pub(super) fn death(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let (Some(raw), Some(_zero), Some(flag)) = (c.varint(), c.varint(), c.varint()) else {
        stats.death_rejected += 1;
        return;
    };
    let id = super::spawn::fold(raw);
    if !(1..=MAX_ENTITY_ID).contains(&id) {
        stats.death_rejected += 1;
        return;
    }
    let kind = match flag {
        3 => DeathKind::Died,
        7 => DeathKind::Gone,
        1 => DeathKind::LoadedDead,
        other => DeathKind::Other(other),
    };
    stats.deaths += 1;
    out(Event::Death { t_ms, id, kind });
}

/// `21 36 <u32 load count> <u32 map id> ...`. Returns true when a zone change was emitted.
pub(super) fn map_load(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) -> bool {
    let mut c = Cursor::new(body);
    let (Some(_count), Some(map_id)) = (c.u32(), c.u32()) else {
        stats.map_rejected += 1;
        return false;
    };
    if !(1..=9_999_999).contains(&map_id) {
        stats.map_rejected += 1;
        return false;
    }
    stats.zone_changes += 1;
    out(Event::ZoneChange { t_ms, map_id });
    true
}

/// `00 8D <entity varint> <mask varint> [mask & 1: n varint, n x (kind u8, u32 LE)] [mask & 2: m varint,
/// m x (kind u8, u64 LE)]`. Kind 0 of the u64 list is the current HP (NPCs: `02 01 00 <u64>`, the shape
/// the community parsers describe; players get it with mask 3 after an MP entry). The u32 list carries
/// MP and other resources and is not used. A kind 0 value above 100 million is a misparse and counts as
/// a rejection too, even though the rest of the list is still walked.
pub(super) fn remain_hp(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let (Some(id), Some(mask)) = (c.varint(), c.varint()) else {
        stats.hp_rejected += 1;
        return;
    };
    if !(1..=MAX_ENTITY_ID).contains(&id) || mask == 0 || mask > 3 {
        stats.hp_rejected += 1;
        return;
    }
    if mask & 0x01 != 0 {
        let Some(n) = c.varint() else {
            stats.hp_rejected += 1;
            return;
        };
        if n > 16 {
            stats.hp_rejected += 1;
            return;
        }
        for _ in 0..n {
            if c.u8().is_none() || c.u32().is_none() {
                stats.hp_rejected += 1;
                return;
            }
        }
    }
    if mask & 0x02 != 0 {
        let Some(m) = c.varint() else {
            stats.hp_rejected += 1;
            return;
        };
        if m > 16 {
            stats.hp_rejected += 1;
            return;
        }
        for _ in 0..m {
            let (Some(kind), Some(value)) = (c.u8(), c.u64()) else {
                stats.hp_rejected += 1;
                return;
            };
            if kind == 0 {
                if value <= 100_000_000 {
                    stats.remain_hp += 1;
                    out(Event::RemainHp { t_ms, id, cur_hp: value, max_hp: None });
                } else {
                    stats.hp_rejected += 1;
                }
            }
        }
    }
}

/// `1B 92 <entity varint> <hp varint> <max hp varint 1..=50 000 000> ...`.
pub(super) fn hp_update(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let (Some(id), Some(cur), Some(max)) = (c.varint(), c.varint(), c.varint()) else {
        stats.hp_rejected += 1;
        return;
    };
    if !(1..=MAX_ENTITY_ID).contains(&id) || !(1..=50_000_000).contains(&max) || cur > 100_000_000 {
        stats.hp_rejected += 1;
        return;
    }
    stats.hp_updates += 1;
    out(Event::HpUpdate { t_ms, id, cur_hp: u64::from(cur), max_hp: u64::from(max) });
}

/// `04 8D <entity varint> <4 bytes> <owner varint> <server u16 1000..=2999> [<len u8> <name>] ...`: the
/// summon to owner form. The same opcode also carries kill and loot relations whose second entity has no
/// server and name (and all zero records for a despawn); those are ignored.
pub(super) fn ownership(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let link = (|| {
        let entity = c.varint()?;
        c.bytes(4)?;
        let owner = c.varint()?;
        let server = c.u16()?;
        if !(1..=MAX_ENTITY_ID).contains(&entity) || !(1..=MAX_ENTITY_ID).contains(&owner) || owner == entity {
            return None;
        }
        if !(1000..=2999).contains(&server) {
            return None;
        }
        let len = usize::from(c.u8()?);
        if len != 0 {
            valid_name(c.bytes(len)?)?;
        }
        Some((entity, owner))
    })();
    match link {
        Some((entity, owner)) => {
            stats.owner_links += 1;
            out(Event::OwnerLink { t_ms, entity, owner });
        }
        None => stats.ownership_ignored += 1,
    }
}

/// `03 36 00 00 <client i64> <server i64>`.
pub(super) fn pong(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let (Some(head), Some(client_ms), Some(server_ms)) = (c.u16(), c.u64(), c.u64()) else {
        stats.pong_rejected += 1;
        return;
    };
    if head != 0 {
        stats.pong_rejected += 1;
        return;
    }
    stats.pongs += 1;
    out(Event::Pong { t_ms, client_ms, server_ms });
}
