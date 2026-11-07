//! `04 38` damage records: the chain walk, the structural value block and the strict tail.
//!
//! Record layout on the Global client (October 2026), verified on 140 000 records from the local captures:
//!
//! ```text
//! [01 00]                               separator before a chained record (optional before the first)
//! target varint, switch varint, flags varint, actor varint, skill u32 LE, uid u8, hit_type varint
//! switch low three bits 0 or 2: no value. 4, 5, 6, 7: a value follows:
//!   [switch & 0x02] plotter: flags u8, restoration hp varint, angle u8
//!   hit id 8 bytes
//!   [switch & 0x01] 4 bytes
//!   scalar varint (the actor's power scalar, mobs 10000), damage varint (the record total)
//!   [switch & 0x08] 4 bytes (three mob hits on a player in session2; the community parsers reject the nibble)
//!   [layout 4, players only] field varint 1..=25
//!   [switch & 0x20] count varint 1..=25, then count hit varints whose sum is below the damage
//!   [switch & 0x40] count varint 1..=25, then count effect ids u32
//! trailer: end of body, or <N u8 1..=63> 00, then [flags & 0x04] one varint, then
//!          [flags & 0x10 without a plotter block] one u8
//! ```
//!
//! The `flags` varint (the "unused" field of the community parsers) is a bit set: 0x01 puts an effect id
//! list (count varint, then u32 ids) after the scalar of a no value record, 0x04 appends one varint after
//! the trailer, and 0x10 appends one byte (always `01` so far) after the trailer of a record without a
//! plotter block; with a plotter block the same flag shows as plotter bit 0x80 and nothing follows (84
//! value records over four captures: 27 with switch 0x44 and the byte, 57 with switch 0x46 and none; the
//! two flags were never set together). The first two were read by A2Tools as something else: the `01 00`
//! chain separator (which is this record's trailer, N being a small counter that reads 1..=20 in the
//! captures) and the `03 00 <varint>` "life steal" suffix (trailer N = 3 followed by the flag 0x04 varint;
//! its values exceed the hit damage, so it is not a heal and is only counted here).
//!
//! A trailer of `00 00` (N = 0) occurs only on no value records, all of them mob self casts (session1: 6,
//! session3: 85, session4: 55). It is not accepted as a boundary: zero bytes are everywhere in the body
//! (the hit id ends in `00 00 00`), so a misaligned value read would find one too easily. A no value
//! record ends the chain there with nothing lost; a value record with it is dropped. Both are counted in
//! `zero_trailers` so a client that starts sending N = 0 on value records is visible at once.

use super::{Event, HitFlags, ParserStats, Reject, EntityId, MAX_DAMAGE, MAX_ENTITY_ID, MAX_SKILL_ID};
use crate::gamedata::GameData;
use crate::wire::Cursor;

/// Bytes a no value record needs after a separator; shorter leftovers are not a record.
const MIN_RECORD: usize = 10;
/// Largest trailer counter accepted as a record boundary. The captures read 1..=20; the cap keeps a
/// chance `<byte> 00` match about half as likely as with a full 0x7F.
const MAX_TRAILER_N: u8 = 0x3F;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record {
    pub target: EntityId,
    pub actor: EntityId,
    pub skill: u32,
    pub switch: u32,
    pub flags: u32,
    pub uid: u8,
    pub hit_type: u32,
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Value {
    pub plotter: u8,
    pub restoration: u32,
    pub angle: u8,
    pub scalar: u32,
    pub damage: u32,
    pub field: Option<u32>,
    pub hits: Vec<u32>,
    pub effects: u32,
    pub tail_value: Option<u32>,
}

/// Why a tail did not end at a record boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailFail {
    /// `00 00` where the `<N> 00` boundary should be.
    Zero,
    Other,
}

impl TailFail {
    fn worst(self, other: TailFail) -> TailFail {
        if self == TailFail::Zero || other == TailFail::Zero {
            TailFail::Zero
        } else {
            TailFail::Other
        }
    }
}

/// The boundary bytes that belong to a record.
#[derive(Debug, Default)]
struct Boundary {
    end: usize,
    tail_value: Option<u32>,
    /// The flag 0x10 byte was read.
    flag_byte: bool,
}

#[derive(Debug, Default)]
struct Tail {
    boundary: Boundary,
    field: Option<u32>,
    hits: Vec<u32>,
    effects: u32,
}

fn plausible(id: u32) -> bool {
    (1..=MAX_ENTITY_ID).contains(&id)
}

/// Theostone item ids ride in the skill field; the table keys them as `id * 10 + 1`.
pub(super) fn normalise_skill(raw: u32) -> u32 {
    if (3_000_000..=3_099_999).contains(&raw) {
        raw * 10 + 1
    } else {
        raw
    }
}

fn is_separator(body: &[u8], pos: usize) -> bool {
    body.get(pos) == Some(&0x01) && body.get(pos + 1) == Some(&0x00)
}

/// Walks every record of one `04 38` body and emits the events.
pub(super) fn parse_packet(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let gd = GameData::get();
    let mut pos = 0usize;
    let mut first = true;
    while pos < body.len() {
        if is_separator(body, pos) {
            pos += 2;
            if pos >= body.len() {
                break;
            }
        }
        if !first {
            if body[pos..].starts_with(&[0x04, 0x38]) {
                // A2Tools accepts a glued packet here; its records continue after the opcode.
                pos += 2;
                continue;
            }
            if body.len() - pos < MIN_RECORD {
                stats.trailing_bytes += 1;
                break;
            }
        }
        first = false;
        stats.damage_records += 1;
        match parse_record(body, pos, stats) {
            Ok((rec, end)) => {
                emit(t_ms, &rec, gd, stats, out);
                if end <= pos {
                    break;
                }
                pos = end;
            }
            Err(reason) => {
                stats.reject(reason);
                break;
            }
        }
    }
}

/// One record starting at `start`. Returns it with the position after its trailer. The layout
/// diagnostics (pad shifts, crit heuristics, zero trailers, flag bytes) are counted on `stats` here
/// because they are decided here, before the record is accepted or dropped.
pub(super) fn parse_record(body: &[u8], start: usize, stats: &mut ParserStats) -> Result<(Record, usize), Reject> {
    let mut c = Cursor::at(body, start);
    let target = c.varint().ok_or(Reject::Truncated)?;
    if !plausible(target) {
        return Err(Reject::BadId);
    }
    let switch = c.varint().ok_or(Reject::Truncated)?;
    let low = switch & 0x07;
    if !matches!(low, 0 | 2 | 4..=7) {
        return Err(Reject::BadSwitch);
    }
    let flags = c.varint().ok_or(Reject::Truncated)?;
    let actor = c.varint().ok_or(Reject::Truncated)?;
    if !plausible(actor) {
        return Err(Reject::BadId);
    }
    let skill = normalise_skill(c.u32().ok_or(Reject::Truncated)?);
    if !(1..=MAX_SKILL_ID).contains(&skill) {
        return Err(Reject::BadSkill);
    }
    let uid = c.u8().ok_or(Reject::Truncated)?;
    let hit_type = c.varint().ok_or(Reject::Truncated)?;
    let mut rec = Record { target, actor, skill, switch, flags, uid, hit_type, value: None };

    if low == 0 || low == 2 {
        // The front is validated; the tail (hit id, scalar, lists, trailer) is walked when it fits so a
        // chained record can follow, and the chain stops here when it does not.
        return match no_value_tail(body, c.pos(), switch, flags) {
            Ok(b) => {
                stats.tail_flag_bytes += u64::from(b.flag_byte);
                Ok((rec, b.end))
            }
            Err(fail) => {
                stats.no_value_tail_failures += 1;
                stats.zero_trailers += u64::from(fail == TailFail::Zero);
                Ok((rec, body.len()))
            }
        };
    }

    let (plotter, restoration, angle) = if switch & 0x02 != 0 {
        let f = c.u8().ok_or(Reject::Truncated)?;
        let hp = c.varint().ok_or(Reject::Truncated)?;
        let a = c.u8().ok_or(Reject::Truncated)?;
        (f, hp, a)
    } else {
        (0, 0, 0)
    };
    c.bytes(8).ok_or(Reject::Truncated)?;
    if switch & 0x01 != 0 {
        c.bytes(4).ok_or(Reject::Truncated)?;
    }
    let mut first = c.varint().ok_or(Reject::Truncated)?;
    let mut after_first = c.pos();
    let mut second = c.varint().ok_or(Reject::Truncated)?;
    let mut after_second = c.pos();
    if first == 0 {
        // A zero where the scalar should be is a pad from a layout one byte off; shift by one varint.
        // Never seen on the Global client (0 of 140 000 records), so a count here means the layout moved.
        stats.pad_shifts += 1;
        let third = c.varint().ok_or(Reject::Truncated)?;
        first = second;
        second = third;
        after_first = after_second;
        after_second = c.pos();
    }

    // Structural reading: scalar then damage. The layout 4 field is tried first and dropped on a retry
    // because a spirit's (and a mob's) layout 4 record has none.
    let structural = parse_tail(body, after_second, low == 4, switch, flags, u64::from(second)).or_else(|fail| {
        if low == 4 {
            parse_tail(body, after_second, false, switch, flags, u64::from(second)).map_err(|f2| fail.worst(f2))
        } else {
            Err(fail)
        }
    });
    let (scalar, damage, tail) = match structural {
        Ok(t) => (first, second, t),
        Err(fail) => {
            // Layout 6 crit heuristic (A2Tools, chen-zilei): a crit record without a scalar carries the
            // damage first and a count second. Only tried when the structural reading does not end at a
            // record boundary, so a real small crit can never be inflated to its scalar. Never fired on
            // the Global client; a count means a scalar or a hit count may be read as damage, so look.
            let heuristic = low == 6 && hit_type == 3 && (1_000..=5_000_000).contains(&first) && second <= 25;
            let by_heuristic = if heuristic { parse_tail(body, after_first, false, switch, flags, u64::from(first)).ok() } else { None };
            match by_heuristic {
                Some(t) => {
                    stats.crit_heuristics += 1;
                    (0, first, t)
                }
                None => {
                    stats.zero_trailers += u64::from(fail == TailFail::Zero);
                    return Err(Reject::TailFailure);
                }
            }
        }
    };
    if damage == 0 || u64::from(damage) > MAX_DAMAGE {
        return Err(Reject::BadDamage);
    }
    stats.tail_flag_bytes += u64::from(tail.boundary.flag_byte);
    rec.value = Some(Value {
        plotter,
        restoration,
        angle,
        scalar,
        damage,
        field: tail.field,
        hits: tail.hits,
        effects: tail.effects,
        tail_value: tail.boundary.tail_value,
    });
    Ok((rec, tail.boundary.end))
}

/// The bytes after a no value record's hit type: hit id, scalar, optional effect list, trailer.
fn no_value_tail(body: &[u8], pos: usize, switch: u32, flags: u32) -> Result<Boundary, TailFail> {
    let mut c = Cursor::at(body, pos);
    c.bytes(8).ok_or(TailFail::Other)?;
    let scalar = c.varint().ok_or(TailFail::Other)?;
    if !(1..=1_000_000).contains(&scalar) {
        return Err(TailFail::Other);
    }
    if flags & 0x01 != 0 {
        let n = c.varint().ok_or(TailFail::Other)?;
        if !(1..=25).contains(&n) {
            return Err(TailFail::Other);
        }
        for _ in 0..n {
            c.u32().ok_or(TailFail::Other)?;
        }
    }
    trailer(body, c.pos(), switch, flags)
}

fn parse_tail(body: &[u8], pos: usize, with_field: bool, switch: u32, flags: u32, damage: u64) -> Result<Tail, TailFail> {
    let mut c = Cursor::at(body, pos);
    let mut t = Tail::default();
    if switch & 0x08 != 0 {
        c.bytes(4).ok_or(TailFail::Other)?;
    }
    if with_field {
        let f = c.varint().ok_or(TailFail::Other)?;
        if !(1..=25).contains(&f) {
            return Err(TailFail::Other);
        }
        t.field = Some(f);
    }
    if switch & 0x20 != 0 {
        let n = c.varint().ok_or(TailFail::Other)?;
        if !(1..=25).contains(&n) {
            return Err(TailFail::Other);
        }
        let mut sum = 0u64;
        for _ in 0..n {
            let h = c.varint().ok_or(TailFail::Other)?;
            sum += u64::from(h);
            t.hits.push(h);
        }
        if sum >= damage {
            return Err(TailFail::Other);
        }
    }
    if switch & 0x40 != 0 {
        let n = c.varint().ok_or(TailFail::Other)?;
        if !(1..=25).contains(&n) {
            return Err(TailFail::Other);
        }
        for _ in 0..n {
            c.u32().ok_or(TailFail::Other)?;
        }
        t.effects = n;
    }
    t.boundary = trailer(body, c.pos(), switch, flags)?;
    Ok(t)
}

/// The record boundary: end of body, a glued `04 38`, or `<N 1..=63> 00` followed by the flag 0x04
/// varint and the flag 0x10 byte that belong to this record (in that order; the two flags have never
/// been seen together). `00 00` is reported as `TailFail::Zero`, see the module doc.
fn trailer(body: &[u8], pos: usize, switch: u32, flags: u32) -> Result<Boundary, TailFail> {
    let rest = body.get(pos..).ok_or(TailFail::Other)?;
    if rest.is_empty() || rest.starts_with(&[0x04, 0x38]) {
        return Ok(Boundary { end: pos, tail_value: None, flag_byte: false });
    }
    if rest.len() < 2 || rest[1] != 0 {
        return Err(TailFail::Other);
    }
    if rest[0] == 0 {
        return Err(TailFail::Zero);
    }
    if rest[0] > MAX_TRAILER_N {
        return Err(TailFail::Other);
    }
    let mut c = Cursor::at(body, pos + 2);
    let tail_value = if flags & 0x04 != 0 { Some(c.varint().ok_or(TailFail::Other)?) } else { None };
    let flag_byte = flags & 0x10 != 0 && switch & 0x02 == 0;
    if flag_byte {
        c.u8().ok_or(TailFail::Other)?;
    }
    Ok(Boundary { end: c.pos(), tail_value, flag_byte })
}

pub(super) fn hit_flags(rec: &Record, v: &Value) -> HitFlags {
    HitFlags {
        crit: rec.hit_type == 3,
        back: v.angle == 0x01,
        front: v.angle == 0x02,
        parry: v.plotter & 0x02 != 0,
        perfect: v.plotter & 0x04 != 0,
        double: v.plotter & 0x08 != 0,
        multi: !v.hits.is_empty(),
        dot: false,
    }
}

fn emit(t_ms: u64, rec: &Record, gd: &GameData, stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    *stats.hit_types.entry(rec.hit_type).or_default() += 1;
    *stats.switch_values.entry(rec.switch).or_default() += 1;
    let (actor, target, skill) = (rec.actor, rec.target, rec.skill);

    // Spirit links: the spirit reports to its owner (1699xxxx) or the owner to the spirit (1677xxxx).
    // Neither is damage nor healing. A spirit's own record (actor == target) links nothing.
    if (16_990_000..=16_999_999).contains(&skill) {
        stats.spirit_links += 1;
        if actor != target {
            out(Event::OwnerLink { t_ms, entity: actor, owner: target });
        }
        return;
    }
    if (16_770_000..=16_779_999).contains(&skill) {
        stats.spirit_links += 1;
        if actor != target {
            out(Event::OwnerLink { t_ms, entity: target, owner: actor });
        }
        return;
    }
    if gd.restores_resource(skill) {
        stats.resource_restores += 1;
        return;
    }
    let Some(v) = &rec.value else {
        // The hit type of a no value record mirrors the value record it announces: 2 for a hit, 3 for a
        // crit (players' crit casts). Hit type 0 is a skill start that is always followed by a hit type 2
        // marker for the same use (257 pairs in session1), so it is counted and not emitted.
        match rec.hit_type {
            2 | 3 => {
                stats.casts += 1;
                out(Event::Cast { t_ms, actor, target, skill });
            }
            0 => stats.cast_starts += 1,
            1 => {
                stats.misses += 1;
                out(Event::Miss { t_ms, actor, target, skill, resist: false });
            }
            6 => {
                stats.resists += 1;
                out(Event::Miss { t_ms, actor, target, skill, resist: true });
            }
            other => *stats.unknown_hit_types.entry(other).or_default() += 1,
        }
        return;
    };
    if v.tail_value.is_some() {
        stats.tail_values += 1;
    }
    if v.effects > 0 {
        stats.effect_lists += 1;
    }
    if !matches!(rec.hit_type, 2 | 3) {
        *stats.unknown_hit_types.entry(rec.hit_type).or_default() += 1;
    }
    let flags = hit_flags(rec, v);
    let amount = u64::from(v.damage);
    if gd.is_heal_skill(skill) {
        stats.heals += 1;
        out(Event::Heal { t_ms, actor, target, skill, amount, flags });
        return;
    }
    if actor == target {
        stats.self_hits += 1;
    }
    stats.damage_events += 1;
    out(Event::Damage { t_ms, actor, target, skill, amount, flags, extra_hits: v.hits.len() as u32 });
}
