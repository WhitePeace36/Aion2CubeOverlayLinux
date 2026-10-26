//! Server to client opcodes, written as the two bytes appear on the wire: `0xAABB` means the bytes `AA BB`.
//!
//! Heartbeat, map load, teleport and self record are community findings shared with the aion2dps meter and the
//! aion2kina tracker. The object and position records were verified by the dr89mh/aion2-cube-radar captures on
//! Global build 25800038 (2026-10-13). They move when the game patches; `replay --opcodes` on a fresh capture is
//! the way to find them again (docs/PROTOCOL.md).

pub const HEARTBEAT: u16 = 0x0036;
pub const MAP_LOAD: u16 = 0x2136;
/// Position or teleport of an entity; entity 0 is the local player.
pub const TELEPORT: u16 = 0x2336;
pub const SELF_INFO: u16 = 0x3336;
/// A world object (gadget) comes into view: gathering nodes, chests, Hidden Cubes.
pub const OBJECT_SPAWN: u16 = 0x3436;
/// A world object leaves view (reason 01) or is opened or destroyed (reason 04).
pub const OBJECT_REMOVED: u16 = 0x3536;
/// Someone starts opening an object.
pub const OPEN_START: u16 = 0x3836;
/// Someone finished opening an object; the tail names the item it took (a Hidden Cube consumes its key).
pub const OPEN_DONE: u16 = 0x3A36;
/// Buff record ending in the caster's position.
pub const BUFF_POSITION: u16 = 0x2A38;
/// Second buff record shape, also ending in the caster's position.
pub const BUFF_POSITION_ALT: u16 = 0x2B38;

/// LZ4 bundle marker in the opcode position: the frame holds more frames, compressed.
pub const BUNDLE: u16 = 0xFFFF;

pub fn name(op: u16) -> &'static str {
    match op {
        HEARTBEAT => "Heartbeat",
        MAP_LOAD => "MapLoad",
        TELEPORT => "Teleport",
        SELF_INFO => "SelfInfo",
        OBJECT_SPAWN => "ObjectSpawn",
        OBJECT_REMOVED => "ObjectRemoved",
        OPEN_START => "OpenStart",
        OPEN_DONE => "OpenDone",
        BUFF_POSITION => "BuffPosition",
        BUFF_POSITION_ALT => "BuffPosition2",
        _ => "",
    }
}

/// `34 36` style rendering of an opcode.
pub fn hex(op: u16) -> String {
    format!("{:02X} {:02X}", op >> 8, op & 0xFF)
}

/// Parses `3436`, `34 36` or `34-36`.
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
        assert_eq!(hex(OBJECT_SPAWN), "34 36");
        assert_eq!(parse("34 36"), Some(OBJECT_SPAWN));
        assert_eq!(parse("3a36"), Some(OPEN_DONE));
        assert_eq!(parse("2a-38"), Some(BUFF_POSITION));
        assert_eq!(parse("436"), None);
        assert_eq!(name(OBJECT_REMOVED), "ObjectRemoved");
    }
}
