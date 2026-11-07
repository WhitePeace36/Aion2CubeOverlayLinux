//! `33 36` self record and `45 36` (legacy `44 36`) other player record, plus the scan for the same records
//! riding inside other packets and inside LZ4 bundles embedded in packet bodies.
//!
//! ```text
//! id varint, mask1 u32, mask2 u8, [mask2 & 0x01: len u8, name]
//! self record:   server u16 LE (1000..=2999), job u32 LE, u8, level u32 LE
//! player record: job u32 LE, ... (position, HP, abnormal list; the server id sits before the legion name)
//! ```

use super::{roster, valid_name, EntityId, Event, ParserStats, MAX_ENTITY_ID};
use crate::frame::FrameDecoder;
use crate::gamedata::GameData;
use crate::opcodes as op;
use crate::wire::{self, Cursor};

/// A name field is 1 to 16 characters of up to 4 UTF-8 bytes each, but never longer than this.
const NAME_FIELD_BYTES: std::ops::RangeInclusive<usize> = 1..=48;
/// Decompressed size cap for a bundle found inside a packet body.
const MAX_EMBEDDED_RAW: usize = 1024 * 1024;
/// How far after the player name the server id and legion name are looked for.
const LEGION_SCAN: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Identity {
    pub id: EntityId,
    pub name: String,
    pub server: u16,
    pub job: u32,
    pub class_id: u32,
    pub level: u32,
    pub is_self: bool,
}

impl Identity {
    fn event(self, t_ms: u64) -> Event {
        Event::Identity {
            t_ms,
            id: self.id,
            name: self.name,
            server: self.server,
            job: self.job,
            class_id: self.class_id,
            level: self.level,
            is_self: self.is_self,
        }
    }
}

/// Parses a record whose fields start at `at`. `strict` is for scans: a self record must then carry a
/// known class after its server, and a player record a known class right after its name (every one of
/// the 48 `45 36` records of session1 does; a chance `45 36` mid body followed by an odd byte, a length
/// of 1 and a letter would otherwise pass on the name alone and turn a mob into a player in the
/// tracker). Direct packets are not filtered this way. Returns the record and the position after the
/// fields that were read.
pub(super) fn parse_at(body: &[u8], at: usize, is_self: bool, strict: bool) -> Option<(Identity, usize)> {
    let gd = GameData::get();
    let mut c = Cursor::at(body, at);
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
    if !NAME_FIELD_BYTES.contains(&len) {
        return None;
    }
    let name = valid_name(c.bytes(len)?)?.to_string();
    let mut ident = Identity { id, name, server: 0, job: 0, class_id: 0, level: 0, is_self };
    if is_self {
        // A self record without a server after the name is not one: a `1d 37` record ending `33 36`
        // once read as entity 16 with a two letter name (A2Tools, 2026-10-05).
        let server = c.u16()?;
        if !(1000..=2999).contains(&server) {
            return None;
        }
        ident.server = server;
        if let Some(job) = c.u32() {
            ident.job = job;
            ident.class_id = gd.class_from_job(job).unwrap_or(0);
            if let (Some(_), Some(level)) = (c.u8(), c.u32()) {
                if (1..=99).contains(&level) {
                    ident.level = level;
                }
            }
        }
        if strict && ident.class_id == 0 {
            return None;
        }
        return Some((ident, c.pos()));
    }
    let after_name = c.pos();
    if let Some(job) = c.u32() {
        ident.job = job;
        ident.class_id = gd.class_from_job(job).unwrap_or(0);
    }
    if strict && ident.class_id == 0 {
        return None;
    }
    ident.server = find_server(body, after_name).unwrap_or(0);
    Some((ident, after_name))
}

/// The server id of a player record: a u16 in the Global range directly followed by a legion name field.
fn find_server(body: &[u8], from: usize) -> Option<u16> {
    let end = body.len().min(from + LEGION_SCAN);
    let mut i = from;
    while i + 4 <= end {
        let server = u16::from_le_bytes([body[i], body[i + 1]]);
        if (1000..=2999).contains(&server) {
            let len = usize::from(body[i + 2]);
            if (2..=24).contains(&len) {
                if let Some(field) = body.get(i + 3..i + 3 + len) {
                    if std::str::from_utf8(field).is_ok_and(|s| s.chars().all(|ch| ch.is_alphanumeric() || ch == ' ')) {
                        return Some(server);
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Identity records already emitted from one packet; LZ4 keeps a short record as a literal, so a bundled
/// copy also shows up in the raw scan of the same body.
type Seen = Vec<(EntityId, String)>;

fn emit(t_ms: u64, ident: Identity, embedded: bool, stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    stats.identities += 1;
    if ident.is_self {
        stats.self_identities += 1;
    }
    if embedded {
        stats.embedded_identities += 1;
    }
    out(ident.event(t_ms));
}

/// The body of a `33 36`, `45 36` or `44 36` packet.
pub(super) fn parse_direct(t_ms: u64, body: &[u8], is_self: bool, stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    match parse_at(body, 0, is_self, false) {
        Some((ident, _)) => emit(t_ms, ident, false, stats, out),
        None => stats.identity_rejected += 1,
    }
}

/// Identity and roster records at any offset of a buffer that is not one of their packets.
pub(super) fn scan_embedded(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut seen = Seen::new();
    scan_records(t_ms, body, &mut seen, stats, out);
    scan_bundles_seen(t_ms, body, &mut seen, stats, out);
}

fn emit_once(t_ms: u64, ident: Identity, seen: &mut Seen, stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let key = (ident.id, ident.name.clone());
    if !seen.contains(&key) {
        seen.push(key);
        emit(t_ms, ident, true, stats, out);
    }
}

fn scan_records(t_ms: u64, body: &[u8], seen: &mut Seen, stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut i = 0;
    while i + 2 < body.len() {
        let first = body[i];
        if body[i + 1] == 0x36 && matches!(first, 0x33 | 0x45 | 0x44) {
            if let Some((ident, end)) = parse_at(body, i + 2, first == 0x33, true) {
                emit_once(t_ms, ident, seen, stats, out);
                i = end.max(i + 1);
                continue;
            }
        }
        i += 1;
    }
    if body.len() >= 32 {
        for i in 0..body.len() - 2 {
            if body[i] == 0x02 && body[i + 1] == 0x97 {
                if let Some(r) = roster::parse_at(body, i + 2) {
                    stats.rosters += 1;
                    stats.embedded_rosters += 1;
                    out(Event::Party { t_ms, members: r.members });
                    break;
                }
            }
        }
    }
}

/// LZ4 bundles inside a packet body: `<varint len> FF FF <raw size u32> <block>`, sized like a frame. The
/// self record repeats this way every few minutes (A2Tools, 2026-10-04). Only identity and roster records
/// are read from the content.
pub(super) fn scan_bundles(t_ms: u64, body: &[u8], stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    scan_bundles_seen(t_ms, body, &mut Seen::new(), stats, out);
}

fn scan_bundles_seen(t_ms: u64, body: &[u8], seen: &mut Seen, stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut i = 1;
    while i + 8 < body.len() {
        if body[i] != 0xFF || body[i + 1] != 0xFF {
            i += 1;
            continue;
        }
        let found = (1..=3usize).rev().find_map(|n| {
            let at = i.checked_sub(n)?;
            let (len, vlen) = wire::peek_varint(body, at)?;
            if vlen != n || len < 12 {
                return None;
            }
            let size = (len as usize + vlen).checked_sub(4)?;
            let end = at + size;
            if end > body.len() || end <= i + 6 {
                return None;
            }
            let raw = u32::from_le_bytes([body[i + 2], body[i + 3], body[i + 4], body[i + 5]]) as usize;
            if raw == 0 || raw > MAX_EMBEDDED_RAW {
                return None;
            }
            let mut data = vec![0u8; raw];
            let n = lz4_flex::block::decompress_into(&body[i + 6..end], &mut data).ok()?;
            if n == 0 {
                return None;
            }
            data.truncate(n);
            Some((end, data))
        });
        match found {
            Some((end, data)) => {
                stats.embedded_bundles += 1;
                walk_bundle_content(t_ms, &data, seen, stats, out);
                i = end;
            }
            None => i += 1,
        }
    }
}

/// Decompressed bundle content: its frames are walked for direct identity and roster packets, and the
/// whole buffer is scanned for records that sit mid body.
fn walk_bundle_content(t_ms: u64, data: &[u8], seen: &mut Seen, stats: &mut ParserStats, out: &mut dyn FnMut(Event)) {
    let mut dec = FrameDecoder::new(true);
    let mut inner: Vec<(u16, Vec<u8>)> = Vec::new();
    dec.feed(data, &mut |opcode, body| inner.push((opcode, body.to_vec())));
    for (opcode, body) in &inner {
        match *opcode {
            op::SELF_INFO | op::PLAYER_INFO | op::PLAYER_INFO_LEGACY => {
                if let Some((ident, _)) = parse_at(body, 0, *opcode == op::SELF_INFO, true) {
                    emit_once(t_ms, ident, seen, stats, out);
                }
            }
            op::PARTY_ROSTER => {
                if let Some(r) = roster::parse_at(body, 0) {
                    stats.rosters += 1;
                    stats.embedded_rosters += 1;
                    out(Event::Party { t_ms, members: r.members });
                }
            }
            _ => {}
        }
    }
    scan_records(t_ms, data, seen, stats, out);
}
