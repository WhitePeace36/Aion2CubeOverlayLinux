//! Server to client opcodes, written as the two bytes appear on the wire: `0xAABB` means the bytes `AA BB`.
//!
//! These are community findings for the Global client (NA/EU, October 2026). They move when the game patches;
//! `replay --opcodes` on a fresh capture is the way to find them again.

pub const HEARTBEAT: u16 = 0x0036;
pub const PONG: u16 = 0x0336;
pub const MAP_LOAD: u16 = 0x2136;
/// Position or teleport of an entity; entity 0 is the local player. Counted only.
pub const TELEPORT: u16 = 0x2336;
pub const SELF_INFO: u16 = 0x3336;
pub const SPAWN: u16 = 0x4136;
pub const DEATH: u16 = 0x4236;
/// Other player record before the June 2026 client; accepted as an alias of `PLAYER_INFO`.
pub const PLAYER_INFO_LEGACY: u16 = 0x4436;
pub const PLAYER_INFO: u16 = 0x4536;
pub const CAST: u16 = 0x0238;
pub const DAMAGE: u16 = 0x0438;
pub const DOT: u16 = 0x0538;
/// Skill use record naming the caster: `<entity varint> <skill u32 LE> <u16 counter>`. Only the id is used.
pub const PARTY_SCOPE: u16 = 0x0638;
pub const REMAIN_HP: u16 = 0x008D;
/// Relation record: `<entity varint> <4 bytes> <other varint> [<server u16> <name> ...]`.
pub const OWNERSHIP: u16 = 0x048D;
pub const HP_UPDATE: u16 = 0x1B92;
pub const PARTY_ROSTER: u16 = 0x0297;

/// LZ4 bundle marker in the opcode position: the frame holds more frames, compressed.
pub const BUNDLE: u16 = 0xFFFF;

pub fn name(op: u16) -> &'static str {
    match op {
        HEARTBEAT => "Heartbeat",
        PONG => "Pong",
        MAP_LOAD => "MapLoad",
        TELEPORT => "Teleport",
        SELF_INFO => "SelfInfo",
        SPAWN => "Spawn",
        DEATH => "Death",
        PLAYER_INFO_LEGACY => "PlayerInfoOld",
        PLAYER_INFO => "PlayerInfo",
        CAST => "Cast",
        DAMAGE => "Damage",
        DOT => "Dot",
        PARTY_SCOPE => "SkillUse",
        REMAIN_HP => "RemainHp",
        OWNERSHIP => "Ownership",
        HP_UPDATE => "HpUpdate",
        PARTY_ROSTER => "PartyRoster",
        _ => "",
    }
}

/// `04 38` style rendering of an opcode.
pub fn hex(op: u16) -> String {
    format!("{:02X} {:02X}", op >> 8, op & 0xFF)
}

/// Parses `0438`, `04 38` or `04-38`.
pub fn parse(s: &str) -> Option<u16> {
    let clean: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if clean.len() != 4 {
        return None;
    }
    u16::from_str_radix(&clean, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_order_round_trips() {
        assert_eq!(hex(DAMAGE), "04 38");
        assert_eq!(parse("04 38"), Some(DAMAGE));
        assert_eq!(parse("0438"), Some(DAMAGE));
        assert_eq!(parse("04-38"), Some(DAMAGE));
        assert_eq!(parse("438"), None);
        assert_eq!(name(REMAIN_HP), "RemainHp");
    }
}
