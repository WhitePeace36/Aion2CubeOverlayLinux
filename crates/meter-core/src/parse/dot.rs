//! `05 38` damage over time and heal over time ticks.
//!
//! ```text
//! target varint, effect u8, actor varint, unknown varint, effect id u32 LE, amount varint, trailing bytes
//! ```
//! The effect byte is matched exactly, never as a bit mask: 0x0B (heal over time) has bit 0x02 set and a
//! mask would turn every HoT tick on a party member into damage. The effect id is the abnormal id times
//! 100 plus an index; the skill is `id / 100`.

use super::{Event, HitFlags, ParserStats, MAX_DAMAGE, MAX_ENTITY_ID, MAX_SKILL_ID};
use crate::gamedata::GameData;
use crate::wire::Cursor;

enum Kind {
    Damage,
    /// Effect 0x03: accepted by the KR meters, never listed by the Global ones; counted separately.
    Damage03,
    Heal,
    Hot,
}

pub(super) fn parse(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut c = Cursor::new(body);
    let (Some(target), Some(effect)) = (c.varint(), c.u8()) else {
        stats.dot_rejected += 1;
        return;
    };
    let kind = match effect {
        0x02 | 0x0A => Kind::Damage,
        0x03 => Kind::Damage03,
        0x01 | 0x09 => Kind::Heal,
        0x0B => Kind::Hot,
        _ => {
            stats.dot_ignored += 1;
            return;
        }
    };
    let (Some(actor), Some(_unknown), Some(raw), Some(amount)) = (c.varint(), c.varint(), c.u32(), c.varint()) else {
        stats.dot_rejected += 1;
        return;
    };
    let skill = raw / 100;
    let ids_ok = (1..=MAX_ENTITY_ID).contains(&target) && (1..=MAX_ENTITY_ID).contains(&actor);
    if !ids_ok || !(1..=MAX_SKILL_ID).contains(&skill) || amount == 0 || u64::from(amount) > MAX_DAMAGE {
        stats.dot_rejected += 1;
        return;
    }
    let flags = HitFlags { dot: true, ..HitFlags::default() };
    let amount = u64::from(amount);
    match kind {
        Kind::Heal | Kind::Hot => {
            stats.dot_heals += 1;
            out(Event::Heal { t_ms, actor, target, skill, amount, flags });
        }
        Kind::Damage | Kind::Damage03 => {
            if actor == target {
                stats.dot_rejected += 1;
                return;
            }
            if matches!(kind, Kind::Damage03) {
                stats.dot_effect_03 += 1;
            }
            if !GameData::get().is_dot_skill(skill) {
                stats.dot_unlisted += 1;
            }
            stats.dot_ticks += 1;
            out(Event::Damage { t_ms, actor, target, skill, amount, flags, extra_hits: 0 });
        }
    }
}
