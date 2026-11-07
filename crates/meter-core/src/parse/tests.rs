//! Parser unit tests: community test vectors (docs/PROTOCOL.md section 17, adapted to our events), records
//! from the local Global client captures, and a fuzz pass that must never panic.

use super::*;
use crate::opcodes as op;
use crate::wire::encode_varint;

fn hex(s: &str) -> Vec<u8> {
    let clean: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    assert!(clean.len().is_multiple_of(2), "odd hex length in {s}");
    (0..clean.len()).step_by(2).map(|i| u8::from_str_radix(&clean[i..i + 2], 16).expect("hex")).collect()
}

fn run(opcode: u16, body: &[u8]) -> (Vec<Event>, ParserStats) {
    let mut p = Parser::default();
    let mut out = Vec::new();
    p.packet(1000, opcode, body, &mut |e| out.push(e));
    (out, p.stats)
}

fn damage(s: &str) -> (Vec<Event>, ParserStats) {
    run(op::DAMAGE, &hex(s))
}

/// The one Damage event of a packet, with its flags and extra hit count.
fn one_damage(s: &str) -> (EntityId, EntityId, u32, u64, HitFlags, u32) {
    let (events, stats) = damage(s);
    assert_eq!(stats.damage_rejected, 0, "rejected {:?} for {s}", stats.rejected);
    assert_eq!(events.len(), 1, "events {events:?} for {s}");
    match &events[0] {
        Event::Damage { actor, target, skill, amount, flags, extra_hits, .. } => (*actor, *target, *skill, *amount, *flags, *extra_hits),
        other => panic!("not a damage event: {other:?}"),
    }
}

fn flags(crit: bool, back: bool, front: bool, parry: bool, perfect: bool, double: bool, multi: bool) -> HitFlags {
    HitFlags { crit, back, front, parry, perfect, double, multi, dot: false }
}

// ---------------------------------------------------------------- names

#[test]
fn names_are_one_to_sixteen_letters_or_digits_in_any_script() {
    for ok in ["A", "é", "あ", "ApexZ", "Amber1", "Zoë", "Ñandú", "さくら", "桜子", "전사", "Abcdefghijkl", "Abcdefghijklmnop"] {
        assert_eq!(valid_name(ok.as_bytes()), Some(ok), "{ok}");
    }
    for bad in [&b"Abcdefghijklmnopq"[..], b"12345", b"Apex Z", b"ApexZ\x06", b"\x05ApexZ", b"", &[0xC3], b"$x9a2", b"a b"] {
        assert_eq!(valid_name(bad), None, "{bad:?}");
    }
}

// ---------------------------------------------------------------- 04 38 community vectors

#[test]
fn a2tools_additional_hits_are_read_from_the_record_tail() {
    // V43: layout 6 crit with two additional hits of 24; the value 1700 is the record total.
    let (actor, target, skill, amount, f, extra) = one_damage("b1ea013600f30a40c0f4007a038000010b199b5f01000000ac52a40d021818");
    assert_eq!((actor, target, skill, amount, extra), (1395, 30001, 16040000, 1700, 2));
    assert_eq!(f, flags(true, true, false, false, false, false, true));
    // V44: layout 4 with the player field and one additional hit.
    let (actor, target, skill, amount, f, extra) = one_damage("b1ea013400f30ae0b7f800cd028bd3276101000000ac52d330010159");
    assert_eq!((actor, target, skill, amount, extra), (1395, 30001, 16300000, 6227, 1));
    assert!(!f.crit && f.multi);
    // V45: a spirit's layout 4 record without the field.
    let (actor, target, skill, amount, _, extra) = one_damage("fe9e0224009e9b01c3f8f5000302792b156002000000ac52e10301060200");
    assert_eq!((actor, target, skill, amount, extra), (19870, 36734, 16120003, 481, 1));
    // V47: layout 4, no lists, trailer 03 00.
    let (_, _, _, amount, _, extra) = one_damage("fe9e0204009e9b01c3f8f5000302792b156003000000ac52b7030300");
    assert_eq!((amount, extra), (439, 0));
    // V48: layout 6 with plotter flag 0x80 (mirrors switch bit 0x10) and the back angle.
    let (_, _, _, amount, f, _) = one_damage("b1ea011600f30a40c0f40063028000010b199b5f01000000ac52d007");
    assert_eq!(amount, 976);
    assert_eq!(f, flags(false, true, false, false, false, false, false));
}

#[test]
fn a2tools_v46_pet_record_with_three_additional_hits() {
    // V46: a pet's (skill 100033) layout 4 record with switch 0x24: the field 3, then the bit 0x20 list
    // of three hits of 3 (sum 9 below the 228 total), trailer 01 00.
    let (actor, target, skill, amount, f, extra) = one_damage("fe9e0224009e9b01c18601001702a7a2980001000000ac52e401030303030100");
    assert_eq!((actor, target, skill, amount, extra), (19870, 36734, 100033, 228, 3));
    assert!(f.multi && !f.crit);
}

#[test]
fn synthetic_v46_without_bit_0x20_is_dropped_not_guessed() {
    // The same bytes with the switch mangled to 0x04: the tail `03 03 03 03 01 00` then carries a hit
    // list the switch does not announce, which the Global layout never does, so the strict parser drops
    // the record and counts it instead of reading 3 as the layout 4 field and guessing at the rest.
    let (events, stats) = damage("fe9e0204009e9b01c18601001702a7a2980001000000ac52e401030303030100");
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(stats.rejected.tail_failure, 1);
    assert_eq!(stats.zero_trailers, 0);
}

#[test]
fn a2tools_hit_flags_are_read_from_player_records() {
    // V49 parry + front, V50 crit + parry + front, V51 perfect, V52 double.
    let (actor, target, skill, amount, f, _) = one_damage("e9de020600c80b9147ff003f02020002aff4b76301000000e4506a0100");
    assert_eq!((actor, target, skill, amount), (1480, 44905, 16730001, 106));
    assert_eq!(f, flags(false, false, true, true, false, false, false));
    let (actor, target, skill, amount, f, _) = one_damage("fcc2010600bf6bc759d1003203020002c711c75101000000904e660100");
    assert_eq!((actor, target, skill, amount), (13759, 24956, 13720007, 102));
    assert_eq!(f, flags(true, false, true, true, false, false, false));
    let (actor, target, skill, amount, f, _) = one_damage("cac20406009e389147ff00e102040000aff4b76301000000a4587f0100");
    assert_eq!((actor, target, skill, amount), (7198, 74058, 16730001, 127));
    assert_eq!(f, flags(false, false, false, false, true, false, false));
    let (actor, target, skill, amount, f, _) = one_damage("bfe6020600d23310ffd6006f0208000055a2fb5302000000aa56d20e0200");
    assert_eq!((actor, target, skill, amount), (6610, 45887, 14090000, 1874));
    assert_eq!(f, flags(false, false, false, false, false, true, false));
}

#[test]
fn a2tools_hits_received_and_restoration_hp_is_a_varint() {
    // V53: shield block flag (not carried by HitFlags), front, 74 damage from a 7 digit NPC skill.
    let (actor, target, skill, amount, f, _) = one_damage("ca6f0600ca9804b8c112000102010002ebab530701000000904e4a0100");
    assert_eq!((actor, target, skill, amount), (68682, 14282, 1229240, 74));
    assert!(f.front && !f.parry && !f.perfect);
    // V55: regeneration flag with a one byte HP varint (13).
    let (_, _, _, amount, f, _) = one_damage("df7e0600a3b103b0b512000302200d02cbf84e0701000000904e440100");
    assert_eq!((amount, f.front), (68, true));
    // V58 and V59: regeneration with a two byte HP varint (177 and 204); the value still lands right.
    let (_, _, _, amount, f, _) = one_damage("ed080600f0ef04bab51200010220b10102b3fc4e0701000000904ef9060100");
    assert_eq!((amount, f.front), (889, true));
    let (_, _, _, amount, f, _) = one_damage("ee0d0600849d03bab51200010220cc0101b3fc4e0701000000904efc070100");
    assert_eq!((amount, f.back), (1020, true));
}

#[test]
fn a2tools_misses_and_resists_count_without_damage() {
    // V60: a hit, then the resist of the same skill. The resist record's `01 c9 e1 f5 05` after the
    // scalar is the effect id list announced by bit 0x01 of the flags varint (one id, 100000201).
    let (_, _, skill, amount, _, _) = one_damage("a8a3041400be77802a04015d02109aa06501000000dc51b80f010100");
    assert_eq!((skill, amount), (17050240, 1976));
    let (events, stats) = damage("a8a3040001be77802a04015d06139aa06501000000dc5101c9e1f5050100");
    assert_eq!(events, vec![Event::Miss { t_ms: 1000, actor: 15294, target: 70056, skill: 17050240, resist: true }]);
    assert_eq!((stats.resists, stats.damage_rejected), (1, 0));
    // V61: a miss.
    let (events, stats) = damage("e981020000899803172df9000101079d556101000000904e0100");
    assert_eq!(events, vec![Event::Miss { t_ms: 1000, actor: 52233, target: 33001, skill: 16330007, resist: false }]);
    assert_eq!(stats.misses, 1);
}

#[test]
fn a2tools_spirit_links_and_mp_restores_are_not_damage_or_healing() {
    // V63, V64: a Wind Spirit (16990003) and a Water Spirit (16990002) report to their owner 10137.
    let (events, _) = damage("994f0400c9dc03333f03010602f7af446501000000ac52670100");
    assert_eq!(events, vec![Event::OwnerLink { t_ms: 1000, entity: 61001, owner: 10137 }]);
    let (events, _) = damage("994f0400ca8203323f0301060293af446501000000ac52140100");
    assert_eq!(events, vec![Event::OwnerLink { t_ms: 1000, entity: 49482, owner: 10137 }]);
    // V65: a self cast MP restore (15760007) is neither.
    let (events, stats) = damage("e3150400e315877af0005302c7dcef5d01000000865d1e0100");
    assert!(events.is_empty());
    assert_eq!(stats.resource_restores, 1);
    // V67: the owner to spirit link band (1677xxxx) links target (spirit) to actor (owner).
    let (events, _) = damage("ccc8010400fc7ad1e3ff000102affdf46301000000e65bf4010100");
    assert_eq!(events, vec![Event::OwnerLink { t_ms: 1000, entity: 25676, owner: 15740 }]);
}

#[test]
fn chen_zilei_damage_vectors() {
    // V68: a crit for 31337.
    let (actor, target, skill, amount, f, _) = one_damage("89 27 06 00 2a e0 26 a8 00 01 03 00 00 00 00 00 00 00 00 00 00 00 b9 60 e9 f4 01");
    assert_eq!((actor, target, skill, amount, f.crit), (42, 5001, 11020000, 31337, true));
    // V69: two chained records.
    let (events, stats) = damage(
        "89 27 06 00 2a e0 26 a8 00 01 01 00 00 00 00 00 00 00 00 00 00 00 b9 60 64 01 00 8a 27 06 00 2a e0 26 a8 00 01 01 00 00 00 00 00 00 00 00 00 00 00 b9 60 c8 01",
    );
    assert_eq!(stats.damage_records, 2);
    let amounts: Vec<(EntityId, u64)> = events.iter().filter_map(|e| if let Event::Damage { target, amount, .. } = e { Some((*target, *amount)) } else { None }).collect();
    assert_eq!(amounts, vec![(5001, 100), (5002, 200)]);
    // V70: garbage is rejected without events.
    let (events, stats) = damage("FF FF FF FF FF 00");
    assert!(events.is_empty());
    assert_eq!(stats.damage_rejected, 1);
    // V71: a heal skill in a damage packet is a heal (hit type 1 on a value record is tolerated).
    let (events, _) = damage("4d 06 00 2a 43 7d 14 01 01 01 00 00 00 00 00 00 00 00 00 00 00 b9 60 a0 1f");
    assert!(matches!(events[..], [Event::Heal { actor: 42, target: 77, skill: 18120003, amount: 4000, .. }]), "{events:?}");
    // V72: a spirit link skill yields only the link.
    let (events, _) = damage("4d 06 00 2a 31 3f 03 01 01 01 00 00 00 00 00 00 00 00 00 00 00 b9 60 a0 1f");
    assert_eq!(events, vec![Event::OwnerLink { t_ms: 1000, entity: 42, owner: 77 }]);
}

#[test]
fn noia2_documented_sample_with_four_additional_hits() {
    // V73: 338097 total, 4 hits of 6935, perfect + double, back.
    let (actor, target, skill, amount, f, extra) =
        one_damage("91 C1 02 26 00 A1 14 48 F1 CA 00 20 02 0C 00 01 2C 40 46 4F 01 00 00 00 8C 91 01 B1 D1 14 04 97 36 97 36 97 36 97 36 01 00");
    assert_eq!((actor, target, skill, amount, extra), (2593, 41105, 13300040, 338097, 4));
    assert_eq!(f, flags(false, true, false, false, true, true, true));
}

// ---------------------------------------------------------------- 04 38 records from the local captures

#[test]
fn capture_record_gale_arrow_with_three_additional_hits() {
    // session1 at 1791722006319. The damage varint is `A9 39` = 7337: 0x39 has no continuation bit, so
    // the following `03` is the additional hit count, not a third damage byte (reading it as one gives the
    // 56489 that was first expected for this record). Three hits of `DD 05` = 733 follow, sum 2199 < 7337.
    let (actor, target, skill, amount, f, extra) =
        one_damage("FB F3 01 36 00 ED 3F 84 4E D7 00 D8 02 84 00 02 6B 36 1A 54 01 00 00 00 9E 55 A9 39 03 DD 05 DD 05 DD 05 01 00");
    assert_eq!((actor, target, skill, amount, extra), (8173, 31227, 14110340, 7337, 3));
    assert_eq!(f, flags(false, false, true, false, true, false, true));
}

#[test]
fn capture_record_no_value_record_is_a_cast() {
    // session1 at 1791722006319, the marker sent with the hit above (same uid D8).
    let (events, stats) = damage("FB F3 01 00 00 ED 3F 84 4E D7 00 D8 02 65 36 1A 54 01 00 00 00 9E 55 01 00");
    assert_eq!(events, vec![Event::Cast { t_ms: 1000, actor: 8173, target: 31227, skill: 14110340 }]);
    assert_eq!((stats.casts, stats.damage_rejected), (1, 0));
}

#[test]
fn capture_record_mob_hit_layout_4_without_field() {
    // session1 at 1791722006359: a mob (scalar 10000) hitting a player with skill 0x186E06 = 1601030
    // (the decimal 1600006 first expected for it is a misread of the hex). Layout 4 without the player
    // field; the trailer is `03 00`, which A2Tools would take for a life steal marker with nothing behind.
    let (actor, target, skill, amount, f, extra) = one_damage("D0 19 04 00 FB F3 01 06 6E 18 00 37 02 7B FA 8A 09 03 00 00 00 90 4E 88 0A 03 00");
    assert_eq!((actor, target, skill, amount, extra), (31227, 3280, 1601030, 1288, 0));
    assert_eq!(f, HitFlags::default());
}

#[test]
fn capture_record_mob_hit_from_the_back() {
    // session1 at 1791722004619: skill 0x186DFC = 1601020 (not 1600004), angle 1 = back.
    let (actor, target, skill, amount, f, _) = one_damage("ED 8A 01 06 00 AA E5 02 FC 6D 18 00 01 02 00 00 01 7B F6 8A 09 01 00 00 00 90 4E B5 08 01 00");
    assert_eq!((actor, target, skill, amount), (45738, 17773, 1601020, 1077));
    assert_eq!(f, flags(false, true, false, false, false, false, false));
}

#[test]
fn capture_record_snare_shot_with_two_additional_hits() {
    // session1 at 1791722006359: skill `36 9C D7 00` = 0xD79C36 = 14130230 Snare Shot (the task's 14112310
    // was a typo), 2003 total with two hits of 200.
    let (actor, target, skill, amount, f, extra) =
        one_damage("FB F3 01 26 00 D0 19 36 9C D7 00 92 02 00 00 02 4B AB 38 54 01 00 00 00 FE 53 D3 0F 02 C8 01 C8 01 01 00");
    assert_eq!((actor, target, skill, amount, extra), (3280, 31227, 14130230, 2003, 2));
    assert_eq!(f, flags(false, false, true, false, false, false, true));
}

#[test]
fn capture_record_flags_bit_4_adds_one_varint_after_the_trailer() {
    // session1 at 1791722006859: the flags varint is 04 and `F0 01` (240) follows the `01 00` trailer.
    // A2Tools reads `01 00` as a chain separator and 240 as the next target; the packet is one record.
    let (events, stats) = damage("FB F3 01 06 04 ED 3F E7 6B DB 00 E3 02 00 00 02 47 26 B6 55 01 00 00 00 FA 60 9D 09 01 00 F0 01");
    assert!(matches!(events[..], [Event::Damage { actor: 8173, target: 31227, skill: 14380007, amount: 1181, .. }]), "{events:?}");
    assert_eq!((stats.damage_records, stats.damage_rejected, stats.tail_values), (1, 0, 1));
}

#[test]
fn capture_shape_trailer_three_with_flags_bit_4_is_not_a_life_steal() {
    // Shape of session1 at 1791722410005 (crit 698 on the boss by a Ranger, trailer `03 00`, then 857):
    // every `03 00 <varint>` in the captures sits on a record with flags bit 0x04, and the value exceeds
    // the hit, so it is the flag varint, not A2Tools' life steal heal.
    let (events, stats) = damage("B2 E2 02 16 04 9F 11 9A A8 DA 00 10 03 80 00 02 33 DC 69 55 03 00 00 00 B2 69 BA 05 03 00 D9 06");
    assert!(matches!(events[..], [Event::Damage { actor: 2207, target: 45362, skill: 14330010, amount: 698, flags: HitFlags { crit: true, front: true, .. }, .. }]), "{events:?}");
    assert_eq!((stats.heals, stats.tail_values, stats.damage_rejected), (0, 1, 0));
}

#[test]
fn capture_record_flags_bit_0x10_adds_one_byte_after_the_trailer_without_a_plotter() {
    // session1 at 1791723278264: switch 0x44, flags 0x10, a mob hit of 1360 on a player with one effect
    // id, trailer `01 00`, then the flag byte `01` (27 of 27 such records over four captures). Nothing
    // is left over, so the packet is not a trailing bytes case.
    let (events, stats) = damage("8C 09 44 10 CA DC 01 DE 6D 18 00 06 02 C3 EA 8A 09 01 00 00 00 90 4E D0 0A 01 77 B7 A3 6F 01 00 01");
    assert!(matches!(events[..], [Event::Damage { actor: 28234, target: 1164, skill: 1600990, amount: 1360, .. }]), "{events:?}");
    assert_eq!((stats.damage_records, stats.damage_rejected, stats.trailing_bytes, stats.tail_flag_bytes), (1, 0, 0, 1));
    // A chained record after the flag byte is reached.
    let (events, stats) = damage(
        "8C 09 44 10 CA DC 01 DE 6D 18 00 06 02 C3 EA 8A 09 01 00 00 00 90 4E D0 0A 01 77 B7 A3 6F 01 00 01 01 00 e9de020600c80b9147ff003f02020002aff4b76301000000e4506a0100",
    );
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(events[1], Event::Damage { actor: 1480, amount: 106, .. }));
    assert_eq!((stats.damage_records, stats.damage_rejected, stats.trailing_bytes), (2, 0, 0));
    // session1 at 1791723236613: switch 0x46 with the same flag carries it as plotter bit 0x80 (0x82)
    // and nothing follows the trailer; a flag byte must not be demanded there.
    let (events, stats) = damage("B9 41 46 10 CA A6 01 E8 6D 18 00 0A 02 82 00 02 AB EE 8A 09 01 00 00 00 90 4E 01 01 13 B7 A3 6F 01 00");
    assert!(matches!(events[..], [Event::Damage { actor: 21322, target: 8377, amount: 1, flags: HitFlags { front: true, parry: true, .. }, .. }]), "{events:?}");
    assert_eq!((stats.damage_rejected, stats.trailing_bytes, stats.tail_flag_bytes), (0, 0, 0));
}

#[test]
fn zero_trailer_stops_a_no_value_chain_and_rejects_a_value_record() {
    // session1 at 1791723142873: mob 43032 casting 1220410 on itself, trailer `00 00`. The cast is
    // emitted, the chain stops, both counters show it.
    let (events, stats) = damage("98 D0 02 00 00 98 D0 02 3A 9F 12 00 02 02 D1 32 46 07 01 00 00 00 90 4E 00 00");
    assert_eq!(events, vec![Event::Cast { t_ms: 1000, actor: 43032, target: 43032, skill: 1220410 }]);
    assert_eq!((stats.damage_rejected, stats.no_value_tail_failures, stats.zero_trailers, stats.trailing_bytes), (0, 1, 1, 0));
    // V49 with its trailer rewritten to `00 00`: never seen on a value record; dropped and counted, not
    // taken for a boundary.
    let (events, stats) = damage("e9de020600c80b9147ff003f02020002aff4b76301000000e4506a 0000");
    assert!(events.is_empty(), "{events:?}");
    assert_eq!((stats.rejected.tail_failure, stats.zero_trailers), (1, 1));
}

#[test]
fn trailer_counter_is_capped_near_the_observed_maximum() {
    // N reads 1..=20 in the captures; 0x3F still passes, 0x40 and above does not.
    for (n, ok) in [("14", true), ("3f", true), ("40", false), ("7f", false)] {
        let (events, stats) = damage(&format!("e9de020600c80b9147ff003f02020002aff4b76301000000e4506a {n}00"));
        assert_eq!(events.len(), usize::from(ok), "N {n}: {events:?}");
        assert_eq!(stats.rejected.tail_failure, u64::from(!ok), "N {n}");
    }
}

#[test]
fn capture_record_resist_with_effect_list() {
    // session1 at 1791722025965: hit type 6, flags bit 0x01, one effect id after the scalar.
    let (events, stats) = damage("D0 19 00 01 FB F3 01 1A 6E 18 00 3D 06 3E 02 8B 09 02 00 00 00 90 4E 01 06 4D F4 00 02 00");
    assert_eq!(events, vec![Event::Miss { t_ms: 1000, actor: 31227, target: 3280, skill: 1601050, resist: true }]);
    assert_eq!(stats.damage_rejected, 0);
}

#[test]
fn capture_record_switch_bit_0x40_effect_list_after_the_value() {
    // session1 at 1791723183164: switch 0x44, one u32 effect id after the damage, trailer 01 00.
    let (actor, target, skill, amount, _, _) = one_damage("8C 09 44 00 C6 91 02 3C 9F 12 00 08 02 C7 32 46 07 01 00 00 00 90 4E 87 08 01 13 B7 A3 6F 01 00");
    assert_eq!((actor, target, skill, amount), (35014, 1164, 1220412, 1031));
    // session2 at 1791724414779: switch 0x4E adds four bytes after the damage (bit 0x08) before the list.
    let (actor, target, skill, amount, f, _) =
        one_damage("1E 4E 00 87 EA 01 A2 6D 18 00 6A 02 00 00 02 53 D3 8A 09 01 00 00 00 90 4E C6 21 5A 03 00 00 01 13 B7 A3 6F 01 00");
    assert_eq!((actor, target, skill, amount, f.front), (29959, 30, 1600930, 4294, true));
}

#[test]
fn capture_record_spirit_self_report_of_100000_is_dropped() {
    // session1 at 1791722527900: a spirit's 16990004 record on itself reads 100000; it is a link record
    // with actor == target and yields nothing.
    let (events, stats) = damage("CD 8E 03 04 00 CD 8E 03 34 3F 03 01 01 02 5C B0 44 65 01 00 00 00 AF 5D A0 8D 06 01 00");
    assert!(events.is_empty(), "{events:?}");
    assert_eq!((stats.spirit_links, stats.damage_rejected), (1, 0));
}

#[test]
fn capture_record_crit_cast_marker_and_skill_start() {
    // session1 at 1791722395908: a no value record with hit type 3 is the marker of a crit hit.
    let (events, _) = damage("B2 E2 02 00 00 B8 28 F6 59 C8 00 09 03 3A 24 43 4E 01 00 00 00 9E 55 01 00");
    assert_eq!(events, vec![Event::Cast { t_ms: 1000, actor: 5176, target: 45362, skill: 13130230 }]);
    // session1 at 1791722006359: hit type 0 announces a skill whose hit type 2 marker follows; not emitted.
    let (events, stats) = damage("D0 19 00 00 FB F3 01 06 6E 18 00 37 00 6D FA 8A 09 02 00 00 00 90 4E 02 00");
    assert!(events.is_empty());
    assert_eq!((stats.cast_starts, stats.casts, stats.damage_rejected), (1, 0, 0));
}

#[test]
fn pad_fallback_and_guarded_crit_heuristic() {
    // A zero where the scalar sits is a pad: the next two varints are scalar and damage (V49 with a pad).
    // Both fallbacks are counted, because on the Global client they never fire and a count is the
    // first sign that a scalar or a hit count is being read as damage.
    let (events, stats) = damage("e9de020600c80b9147ff003f02020002aff4b76301000000 00 e450 6a 0100");
    assert!(matches!(events[..], [Event::Damage { amount: 106, .. }]), "{events:?}");
    assert_eq!((stats.pad_shifts, stats.crit_heuristics), (1, 0));
    // A layout 6 crit without a scalar: structurally scalar 10000 and damage 1, whose tail `00` is no
    // boundary, so the heuristic reads 10000 as the damage with a clean `01 00` behind it.
    let (events, stats) = damage("e9de020600c80b9147ff003f03020002aff4b76301000000 904e 0100");
    assert!(matches!(events[..], [Event::Damage { amount: 10000, flags: HitFlags { crit: true, .. }, .. }]), "{events:?}");
    assert_eq!((stats.pad_shifts, stats.crit_heuristics), (0, 1));
    // A real small crit keeps its value: the structural tail is clean, so the heuristic never runs.
    let (events, stats) = damage("e9de020600c80b9147ff003f03020002aff4b76301000000 e450 14 0100");
    assert!(matches!(events[..], [Event::Damage { amount: 20, flags: HitFlags { crit: true, .. }, .. }]), "{events:?}");
    assert_eq!((stats.pad_shifts, stats.crit_heuristics), (0, 0));
}

#[test]
fn damage_validation_rejects_by_reason() {
    let cases = [
        ("00 06 00 c80b 9147ff00 3f 02 020002 aff4b76301000000 e450 6a 0100", "bad_id"),
        ("e9de02 03 00 c80b 9147ff00 3f 02 aff4b76301000000 e450 6a 0100", "bad_switch"),
        ("e9de02 06 00 c80b 00000000 3f 02 020002 aff4b76301000000 e450 6a 0100", "bad_skill"),
        ("e9de02 06 00 c80b 9147ff00 3f 02 020002 aff4b76301000000 e450 00 0100", "bad_damage"),
        ("e9de02 06 00 c80b 9147ff00 3f 02 020002 aff4b76301000000 e450 80c2d72f 0100", "bad_damage"),
        ("e9de02 06 00 c80b 9147ff00 3f 02 020002 aff4b76301000000 e450 6a 0177", "tail_failure"),
        ("e9de02 06 00 c80b 9147ff00 3f 02 020002 aff4b763", "truncated"),
    ];
    for (body, reason) in cases {
        let (events, stats) = damage(body);
        assert!(events.is_empty(), "{body}: {events:?}");
        assert_eq!(stats.damage_rejected, 1, "{body}");
        let r = &stats.rejected;
        let got = match reason {
            "bad_id" => r.bad_id,
            "bad_switch" => r.bad_switch,
            "bad_skill" => r.bad_skill,
            "bad_damage" => r.bad_damage,
            "tail_failure" => r.tail_failure,
            _ => r.truncated,
        };
        assert_eq!(got, 1, "{body} should be {reason}: {r:?}");
    }
}

#[test]
fn theostone_item_ids_are_rewritten() {
    assert_eq!(super::damage::normalise_skill(3_000_017), 30_000_171);
    assert_eq!(super::damage::normalise_skill(14_110_340), 14_110_340);
}

// ---------------------------------------------------------------- 05 38

#[test]
fn dot_vectors_and_capture_ticks() {
    // V77: a heal tick from effect 190000131 -> skill 1900001 (Restore HP), 7351.
    let (events, stats) = run(op::DOT, &hex("b12701b127b601032c530bb739"));
    assert!(matches!(events[..], [Event::Heal { actor: 5041, target: 5041, skill: 1900001, amount: 7351, flags: HitFlags { dot: true, .. }, .. }]), "{events:?}");
    assert_eq!(stats.dot_heals, 1);
    // V78: a Burn tick, effect 120001211 -> 1200012.
    let (events, _) = run(op::DOT, &hex("812a0abdd1019002bb1227073332931200"));
    assert!(matches!(events[..], [Event::Damage { actor: 26813, target: 5377, skill: 1200012, amount: 51, flags: HitFlags { dot: true, .. }, extra_hits: 0, .. }]), "{events:?}");
    // V79: a spirit's tick, 16001109, 375.
    let (events, _) = run(op::DOT, &hex("88fd020accc801881241c15f5ff7025828f400"));
    assert!(matches!(events[..], [Event::Damage { actor: 25676, target: 48776, skill: 16001109, amount: 375, .. }]), "{events:?}");
    // session1: Jointstrike Curse tick (0A), a self heal (09), a HoT tick (0B), a self damage tick
    // (rejected), status (00) and buff (08) ticks (ignored).
    let (events, _) = run(op::DOT, &hex("FB F3 01 0A E7 75 E8 02 B3 AF 33 60 BF 04 62 47 F6 00"));
    assert!(matches!(events[..], [Event::Damage { actor: 15079, target: 31227, skill: 16140000, amount: 575, .. }]), "{events:?}");
    let (events, _) = run(op::DOT, &hex("ED 3F 09 ED 3F 8B 02 5F B2 FC 0B A0 0F DD AF 1E 00"));
    assert!(matches!(events[..], [Event::Heal { actor: 8173, target: 8173, skill: 2011101, amount: 1952, .. }]), "{events:?}");
    let (events, _) = run(op::DOT, &hex("ED 3F 0B ED 3F 8B 02 5F B2 FC 0B 9A 0C 86 03 DD AF 1E 00"));
    assert!(matches!(events[..], [Event::Heal { actor: 8173, target: 8173, skill: 2011101, amount: 1562, .. }]), "{events:?}");
    let (events, stats) = run(op::DOT, &hex("B1 15 0A B1 15 C3 0A 60 B2 FC 0B D3 02 DD AF 1E 00"));
    assert!(events.is_empty());
    assert_eq!(stats.dot_rejected, 1);
    let (events, stats) = run(op::DOT, &hex("FB F3 01 00 FB F3 01 01 0A E1 F5 05"));
    assert!(events.is_empty());
    assert_eq!(stats.dot_ignored, 1);
    let (events, _) = run(op::DOT, &hex("E7 75 08 E7 75 AC 0A 95 D3 27 61 E2 B7 F8 00"));
    assert!(events.is_empty());
}

// ---------------------------------------------------------------- identity

#[test]
fn self_record_says_server_class_and_level() {
    // V81 (A2Tools): Naicha on Kaisinel, Cleric, level 28.
    let (events, stats) = run(op::SELF_INFO, &hex("ed745e91c12837064e616963686118051e000000011c0000007f0100007f0100001c000000d002040000000000"));
    assert_eq!(
        events,
        vec![Event::Identity { t_ms: 1000, id: 14957, name: "Naicha".into(), server: 1304, job: 30, class_id: 17, level: 28, is_self: true }]
    );
    assert_eq!(stats.self_identities, 1);
    // V84: a two letter name.
    let (events, _) = run(op::SELF_INFO, &hex("ed745e91c1283702587918051e000000011c000000"));
    assert!(matches!(&events[..], [Event::Identity { id: 14957, name, level: 28, .. }] if name == "Xy"));
    // session1: the local player, Ranger (job 15), level 45, server 2314.
    let (events, _) = run(op::SELF_INFO, &hex("B1 71 5E 91 C1 28 37 03 41 72 69 0A 09 0F 00 00 00 02 2D 00 00 00 83 06 00 00 83 06 00 00"));
    assert!(matches!(&events[..], [Event::Identity { id: 14513, name, server: 2314, job: 15, class_id: 14, level: 45, is_self: true, .. }] if name == "Ari"), "{events:?}");
}

#[test]
fn a_self_record_needs_a_server() {
    // V82: a `1d 37` record ending `33 36 33 36`, scanned as an unlisted packet; the second `33 36` reads
    // entity 16 with a two letter name but no server behind it.
    let body = hex("101d37f5282703ab13e776e776111d37fc792f031557ff33363336101d37a3132703c6b679be97bc111d37ab262f032250d8722f722f");
    let (events, stats) = run(0x1D37, &body);
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(stats.identities, 0);
    // A direct self packet without a server is rejected too.
    let (events, stats) = run(op::SELF_INFO, &hex("2a 00 00 00 00 01 05 5a 69 6c 65 69"));
    assert!(events.is_empty());
    assert_eq!(stats.identity_rejected, 1);
}

#[test]
fn player_records_from_the_capture_and_the_vectors() {
    // session1: Tavish, Ranger (job 15 after the name), server 2315 found before the legion "Lanterns".
    let body = hex(
        "D0 19 03 20 A0 01 07 06 54 61 76 69 73 68 0F 00 00 00 02 01 40 92 FE BD A3 47 4C B9 40 C7 17 F5 0D 47 5D F7 DC 42 D0 0C 01 DA 25 D3 64 1B 0D 00 00 1B 0D 00 00 00 00 00 00 00 00 00 00 28 CC 01 00 C0 06 02 00 C0 2B 00 00 F0 49 02 00 01 00 00 00 A0 86 01 00 A0 86 01 00 F9 42 09 00 F9 42 09 00 01 01 A4 67 65 02 A8 FF 4F 01 0B 09 B8 00 00 00 00 00 0B 09 08 4C 61 6E 74 65 72 6E 73 02 00 01 00 00 00 00 00 00 00 00 00 00 00",
    );
    let (events, _) = run(op::PLAYER_INFO, &body);
    assert!(matches!(&events[..], [Event::Identity { id: 3280, name, server: 2315, job: 15, class_id: 14, level: 0, is_self: false, .. }] if name == "Tavish"), "{events:?}");
    // V85: a one letter name and an unknown class.
    let (events, _) = run(op::PLAYER_INFO, &hex("d74800000000070141000000000000000000"));
    assert!(matches!(&events[..], [Event::Identity { id: 9303, name, class_id: 0, is_self: false, .. }] if name == "A"), "{events:?}");
    // The legacy opcode is an alias.
    let (events, _) = run(op::PLAYER_INFO_LEGACY, &hex("d74800000000070141000000000000000000"));
    assert_eq!(events.len(), 1);
    // V95: a Korean name.
    let (events, _) = run(op::PLAYER_INFO, &hex("4d 00 00 00 00 01 0c ec 97 98 eb a6 ac ec 8b 9c ec 98 a8 e8 03 18 00 00 00"));
    assert!(matches!(&events[..], [Event::Identity { id: 77, name, .. }] if name == "엘리시온"), "{events:?}");
}

#[test]
fn placeholder_and_junk_names_are_rejected() {
    // V94: `$x9a2`, `a b`, `1234`.
    for body in [
        "4d 00 00 00 00 01 05 24 78 39 61 32 e8 03 18 00 00 00",
        "4d 00 00 00 00 01 03 61 20 62 e8 03 18 00 00 00",
        "4d 00 00 00 00 01 04 31 32 33 34 e8 03 18 00 00 00",
    ] {
        let (events, stats) = run(op::PLAYER_INFO, &hex(body));
        assert!(events.is_empty(), "{body}");
        assert_eq!(stats.identity_rejected, 1);
    }
}

#[test]
fn identities_are_found_inside_other_packets_and_embedded_bundles() {
    // A player record mid body under an unlisted opcode: session1's Tavish record (job 15 right after the
    // name) between junk bytes.
    let mut body = vec![7u8; 9];
    body.extend(hex("45 36"));
    body.extend(hex(
        "D0 19 03 20 A0 01 07 06 54 61 76 69 73 68 0F 00 00 00 02 01 40 92 FE BD A3 47 4C B9 40 C7 17 F5 0D 47 5D F7 DC 42 D0 0C 01 DA 25 D3 64 1B 0D 00 00 1B 0D 00 00 00 00 00 00 00 00 00 00 28 CC 01 00 C0 06 02 00 C0 2B 00 00 F0 49 02 00 01 00 00 00 A0 86 01 00 A0 86 01 00 F9 42 09 00 F9 42 09 00 01 01 A4 67 65 02 A8 FF 4F 01 0B 09 B8 00 00 00 00 00 0B 09 08 4C 61 6E 74 65 72 6E 73 02 00 01",
    ));
    body.extend([0u8; 8]);
    let (events, stats) = run(0x1234, &body);
    assert!(matches!(&events[..], [Event::Identity { id: 3280, name, server: 2315, job: 15, class_id: 14, is_self: false, .. }] if name == "Tavish"), "{events:?}");
    assert_eq!(stats.embedded_identities, 1);
    // V92 (chen-zilei, derived): the bytes after the name are not a known class (`e8 03 18 00` is its
    // server then job shape), so the strict scan leaves it alone; the same body as a direct `45 36`
    // packet is still read.
    let mut body = vec![7u8; 9];
    body.extend(hex("45 36 4d 00 00 00 00 01 06 46 61 65 6c 69 73 e8 03 18 00 00 00"));
    body.extend([0u8; 8]);
    let (events, _) = run(0x1234, &body);
    assert!(events.is_empty(), "{events:?}");
    let (events, _) = run(op::PLAYER_INFO, &hex("4d 00 00 00 00 01 06 46 61 65 6c 69 73 e8 03 18 00 00 00"));
    assert!(matches!(&events[..], [Event::Identity { id: 77, name, .. }] if name == "Faelis"), "{events:?}");
    // A chance `45 36` mid body followed by a plausible id, a mask with bit 0, a one letter name and
    // junk is exactly what the class check is for.
    let (events, _) = run(0x1234, &hex("07 07 07 07 07 07 07 07 07 45 36 05 11 22 33 44 01 01 41 99 88 77 66 00 00 00 00"));
    assert!(events.is_empty(), "{events:?}");
    // V93: a self record inside an LZ4 bundle inside a packet body.
    let mut inner = vec![1u8; 5];
    inner.extend(hex("33 36 2a 00 00 00 00 01 05 5a 69 6c 65 69 e8 03 18 00 00 00"));
    let block = lz4_flex::block::compress(&inner);
    let mut bundle = vec![0xFF, 0xFF];
    bundle.extend((inner.len() as u32).to_le_bytes());
    bundle.extend(&block);
    let mut body = vec![9u8; 6];
    body.extend(encode_varint(bundle.len() as u64 + 4));
    body.extend(&bundle);
    body.extend([0u8; 4]);
    let (events, stats) = run(0x1234, &body);
    assert!(
        matches!(&events[..], [Event::Identity { id: 42, name, server: 1000, job: 24, class_id: 16, is_self: true, .. }] if name == "Zilei"),
        "{events:?}"
    );
    assert_eq!((stats.embedded_bundles, stats.embedded_identities), (1, 1));
}

// ---------------------------------------------------------------- roster

fn party(body: &[u8]) -> Vec<PartyMember> {
    let (events, stats) = run(op::PARTY_ROSTER, body);
    assert_eq!(stats.rosters, 1, "no roster: {events:?}");
    match events.into_iter().next() {
        Some(Event::Party { members, .. }) => members,
        other => panic!("{other:?}"),
    }
}

#[test]
fn roster_reads_members_past_vacant_slots() {
    // V116 (A2Tools, 15 August 2026): slots 1 and 5 filled, 2 to 4 vacant.
    let mut body = hex("a5 f9 08 00 0b 38 35 30 4b 20 e8 bf 9e e5 88 b7 05 63 28 09 00 00 03 85 4b 01 00 00 00 f6 03 1f 02 00 05");
    body.extend(hex("0c 01 85 4b 01 00 00 00 f6 03 0f e4 b9 9d e5 b7 9e e4 be 9d e7 84 b6 e5 9c a8 06 00 00 00 32 00 00 00 07 17 00 00 f6 03 f6 03 04 38 39 0d 00 00 00 00 00 00 01 01"));
    for slot in 2u8..=4 {
        let mut vacant = vec![0u8, slot];
        vacant.extend([0u8; 21]);
        vacant.push(0x04);
        vacant.extend([0u8; 12]);
        body.extend(vacant);
    }
    body.extend(hex("0e 05 62 64 01 00 00 00 f6 03 02 4d 37 0e 00 00 00 32 00 00 00 5e 16 00 00 f6 03 f6 03 04 6a 03 0d 00 00 00 00 00 00 01 01 02 0e 00 36"));
    let members = party(&body);
    assert_eq!(members.len(), 2, "{members:?}");
    assert_eq!(
        members[0],
        PartyMember { name: "九州依然在".into(), job: 6, class_id: 11, level: 50, gear_score: 5895, combat_power: 866616 }
    );
    assert_eq!(members[1], PartyMember { name: "M7".into(), job: 14, class_id: 14, level: 50, gear_score: 5726, combat_power: 852842 });
    // V118: a one member roster.
    let members = party(&hex("01000000012e0600000000000000000000000000000000000101010100000000001905 0245661e0000002d0000006400000019050000000102030000000000000000"));
    assert_eq!(members, vec![PartyMember { name: "Ef".into(), job: 30, class_id: 17, level: 45, gear_score: 100, combat_power: 197121 }]);
}

#[test]
fn roster_from_the_capture_has_five_members_with_a_variable_tail() {
    // session1 at 1791722014218, Urugugu Canyon (600012): five members; the fifth has an extra byte
    // between gear score and the world id anchor.
    let members = party(&hex(
        "D8 1A 0F 00 0C 4C 65 61 76 69 6E 67 20 6E 6F 77 2E 05 CC 27 09 00 00 03 B1 A1 03 00 00 00 FE 08 FF 01 02 05 1E 01 B1 A1 03 00 00 00 FE 08 06 43 6F 72 76 69 6E 10 00 00 00 2D 00 00 00 D0 08 00 00 FE 08 E6 10 04 E4 9F 01 00 00 00 00 00 00 02 00 00 00 00 00 00 00 01 01 1E 02 7B FE 02 00 00 00 0B 09 06 59 6C 76 61 6E 65 08 00 00 00 2D 00 00 00 35 06 00 00 0B 09 E6 10 04 9C 20 01 00 00 00 00 00 00 15 00 00 00 00 00 00 00 01 01 1E 03 F5 2B 03 00 00 00 0B 09 06 54 61 76 69 73 68 0F 00 00 00 2D 00 00 00 CD 05 00 00 0B 09 E6 10 04 D9 0B 01 00 00 00 00 00 00 07 00 00 00 00 00 00 00 01 01 1C 04 09 40 03 00 00 00 0A 09 03 41 72 69 0F 00 00 00 2D 00 00 00 60 06 00 00 0A 09 E6 10 04 F1 29 01 00 00 00 00 00 00 10 00 00 00 00 00 00 00 01 01 1E 05 CD C3 02 00 00 00 21 05 05 42 72 61 6B 6B 16 00 00 00 2D 00 00 00 58 06 00 00 03 21 05 E6 10 04 34 15 01 00 00 00 00 00 00 49 00 00 00 00 00 00 00 01 02 00 09",
    ));
    let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Corvin", "Ylvane", "Tavish", "Ari", "Brakk"]);
    assert_eq!(members[0], PartyMember { name: "Corvin".into(), job: 16, class_id: 14, level: 45, gear_score: 2256, combat_power: 106468 });
    assert_eq!(members[1], PartyMember { name: "Ylvane".into(), job: 8, class_id: 11, level: 45, gear_score: 1589, combat_power: 73884 });
    assert_eq!(members[4], PartyMember { name: "Brakk".into(), job: 22, class_id: 16, level: 45, gear_score: 1624, combat_power: 70964 });
    assert!(members.iter().all(|m| (11..=19).contains(&m.class_id)));
    // The same party after Corvin left: slot 2 vacant, four members.
    let members = party(&hex(
        "D8 1A 0F 00 0C 4C 65 61 76 69 6E 67 20 6E 6F 77 2E 05 CC 27 09 00 00 03 7B FE 02 00 00 00 0B 09 FB 01 03 05 1C 01 7B FE 02 00 00 00 0B 09 06 59 6C 76 61 6E 65 08 00 00 00 2D 00 00 00 35 06 00 00 0B 09 E6 10 04 9C 20 01 00 00 00 00 00 00 15 00 00 00 00 00 00 00 01 01 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 00 00 00 00 00 00 00 00 00 00 00 1E 03 F5 2B 03 00 00 00 0B 09 06 54 61 76 69 73 68 0F 00 00 00 2D 00 00 00 CD 05 00 00 0B 09 E6 10 04 D9 0B 01 00 00 00 00 00 00 07 00 00 00 00 00 00 00 01 02 1C 04 09 40 03 00 00 00 0A 09 03 41 72 69 0F 00 00 00 2D 00 00 00 60 06 00 00 0A 09 E6 10 04 F1 29 01 00 00 00 00 00 00 10 00 00 00 00 00 00 00 01 01 1E 05 CD C3 02 00 00 00 21 05 05 42 72 61 6B 6B 16 00 00 00 2D 00 00 00 58 06 00 00 21 05 E6 10 04 34 15 01 00 00 00 00 00 00 49 00 00 00 00 00 00 00 01 02 00 04",
    ));
    let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Ylvane", "Tavish", "Ari", "Brakk"]);
}

#[test]
fn roster_header_rejects_junk() {
    let (events, stats) = run(op::PARTY_ROSTER, &hex("00 00 00 00 00 05 ff ff ff ff"));
    assert!(events.is_empty());
    assert_eq!(stats.roster_rejected, 1);
}

// ---------------------------------------------------------------- spawn

fn spawn(body: &[u8]) -> Vec<Event> {
    run(op::SPAWN, body).0
}

#[test]
fn spawns_from_the_capture() {
    // A Water Spirit (kind 0x5F) of the Elementalist 15079: the parent block, the abnormal anchor and the
    // caster field all name the owner.
    let events = spawn(&hex(
        "F4 95 02 5F 10 00 C6 8E 2C 00 40 02 80 CA A0 47 00 E3 37 C7 00 26 0E 47 16 37 45 43 3E 8C 01 9D 6D 9D 6D 78 0F 00 00 78 0F 00 00 00 00 00 00 00 00 00 00 00 00 00 00 30 1E 02 00 64 00 00 00 F0 49 02 00 01 00 00 00 00 00 00 00 A0 86 01 00 00 00 00 00 1F 01 09 00 01 01 01 11 01 44 3A 9A 09 FF FF FF FF FF FF FF FF 80 75 D5 2A BB 03 00 00 E7 75 0E 02 46 04 A1 47 17 25 37 C7 00 F6 0D 47 07 02 06 E7 3A 00 00 0E 00 00 00 00 00 21 05 0C 4C 61 73 74 41 6C 6C 69 61 6E 63 65 01 00 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 03 CD 00 DC 00 00 00 D0 00 3E 01 00 00 D6 00 32 00 00 00 2D 00 00 00 E5 1D 03 00 00",
    ));
    assert_eq!(
        events,
        vec![Event::NpcSpawn { t_ms: 1000, id: 35572, npc_code: 2920134, kind: 0x5F, cur_hp: Some(13981), max_hp: Some(13981), owner: Some(15079) }]
    );
    // A Ranger's Explosion Trap (kind 0x1F) carrying the caster's name inline; the code follows the name
    // and the caster field names 2207. No weak identity for an effect entity.
    let events = spawn(&hex(
        "FE 91 03 1F 00 01 09 51 75 69 6C 6C 66 65 72 6E AC 90 2C 00 00 02 00 25 14 C7 00 7A BC C6 00 D2 58 47 CA AD 84 43 B3 BC 01 D1 80 01 D1 80 01 94 0E 00 00 94 0E 00 00 00 00 00 00 00 00 00 00 00 00 00 00 A8 87 02 00 64 00 00 00 F0 49 02 00 01 00 00 00 00 00 00 00 A0 86 01 00 00 00 00 00 FC AA 09 00 01 01 11 01 81 96 98 00 FF FF FF FF FF FF FF FF 80 75 D5 2A BB 03 00 00 FE 91 03 01 02 00 25 14 C7 00 7A BC C6 00 D2 58 47 07 02 06 9F 08 00 00 02 CD 00 14 07 00 00 D0 00 58 01 00 00 2D 00 00 00 00",
    ));
    assert_eq!(
        events,
        vec![Event::NpcSpawn { t_ms: 1000, id: 51454, npc_code: 2920620, kind: 0x1F, cur_hp: Some(16465), max_hp: Some(16465), owner: Some(2207) }]
    );
    // Vakron (boss 2300812), kind 0x0C, 9 860 000 HP, no owner.
    let events = spawn(&hex(
        "B2 E2 02 0C 32 00 8C 1B 23 00 00 02 0A 3A 18 C7 5E C6 B8 C6 00 D9 58 47 00 98 B2 43 00 FE 01 A0 E7 D9 04 A0 E7 D9 04 64 00 00 00 64 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 40 9C 00 00 40 9C 00 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 06 02 11 01 81 96 98 00 FF FF FF FF FF FF FF FF 80 75 D5 2A BB 03 00 00 B2 E2 02 01 14 0A 3A 18 C7 5E C6 B8 C6 00 D9 58 47 11 02 84 96 98 00 FF FF FF FF FF FF FF FF 80 75 D5 2A BB 03 00 00 B2 E2 02 01 0A 3A 18 C7 5E C6 B8 C6 00 D9 58 47 01 00 36 00 00 00 03 01 B0 04 00 00 B0 04 00 00 FF 4D 0F 00 C1 F5 09 5D 00",
    ));
    assert_eq!(
        events,
        vec![Event::NpcSpawn { t_ms: 1000, id: 45362, npc_code: 2300812, kind: 0x0C, cur_hp: Some(9860000), max_hp: Some(9860000), owner: None }]
    );
    assert!(crate::gamedata::GameData::get().is_boss(2300812));
    // An open world NPC with kind 4 and marker `00 00 02`.
    let events = spawn(&hex(
        "CB 91 01 04 20 00 DD 26 2D 00 00 02 D8 22 C2 47 7C 22 3E 48 00 5E 8A 46 00 00 00 00 00 00 01 E8 CF 01 E8 CF 01 64 00 00 00 64 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 64 00 00 00 64 00 00 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 04 02 11 01 81 96 98 00 FF FF FF FF FF FF FF FF 80 75 D5 2A BB 03 00 00 CB 91 01 01 00",
    ));
    assert_eq!(events, vec![Event::NpcSpawn { t_ms: 1000, id: 18635, npc_code: 2959069, kind: 0x04, cur_hp: Some(26600), max_hp: Some(26600), owner: None }]);
}

#[test]
fn a2tools_spawn_vectors_link_spirits_and_ground_spells_to_their_caster() {
    // V100: another player's spirit, linked through the abnormal anchor; no NPC code in the record.
    let mut body = hex("dc f1 02 1f 10 00 c6");
    body.extend([0x22u8; 24]);
    body.extend(hex("80 75 d5 2a bb 03 00 00 bc 31 0c 02"));
    let events = spawn(&body);
    assert!(matches!(events[..], [Event::NpcSpawn { id: 47324, kind: 0x1F, npc_code: 0, owner: Some(6332), .. }]), "{events:?}");
    body[0] = 0xdd;
    body[3] = 0x0c;
    let events = spawn(&body);
    assert!(matches!(events[..], [Event::NpcSpawn { id: 47325, kind: 0x0C, owner: None, .. }]), "{events:?}");

    // V102: a Sorcerer's ground spell names itself at the anchor; the caster field has the owner.
    let middle = hex(
        "1F00004B8E2C004002000CF0C7CFA9D0C70090C04672498542642F01A925A9258E0800008E08000000000000000000000000000010E9010064000000F04902000100000000000000A08601000000000090D00300010101110181969800FFFFFFFFFFFFFFFF8075D52ABB030000",
    );
    let build = |id: &str, caster: &str, marker: &str| {
        let mut b = hex(id);
        b.extend(&middle);
        b.extend(hex(id));
        b.extend(hex("0102000CF0C7CFA9D0C70090C046"));
        b.extend(hex(marker));
        b.extend(hex(caster));
        b.extend(hex("02CD002800"));
        b
    };
    let events = spawn(&build("C98E01", "3F370000", "070206"));
    assert!(matches!(events[..], [Event::NpcSpawn { id: 18249, npc_code: 2920011, kind: 0x1F, owner: Some(14143), .. }]), "{events:?}");
    let events = spawn(&build("CA8E01", "4A470000", "070206"));
    assert!(matches!(events[..], [Event::NpcSpawn { id: 18250, owner: None, .. }]), "{events:?}");
    let events = spawn(&build("CC8E01", "BF000000", "070201"));
    assert!(matches!(events[..], [Event::NpcSpawn { id: 18252, owner: Some(191), .. }]), "{events:?}");
}

#[test]
fn chen_zilei_spawn_vectors_fold_large_ids() {
    // V111: NPC 2000002 with 48000 of 50000 HP.
    let mut tail = hex("0c 00 00 00 00 07 00 82 84 1e 00 40 02");
    tail.extend([0u8; 12]);
    tail.extend(hex("05 01 80 f7 02 d0 86 03"));
    let mut body = hex("89 27");
    body.extend(&tail);
    let events = spawn(&body);
    assert_eq!(events, vec![Event::NpcSpawn { t_ms: 1000, id: 5001, npc_code: 2000002, kind: 0x0C, cur_hp: Some(48000), max_hp: Some(50000), owner: None }]);
    // V112: an id above one million folds.
    let mut body = encode_varint(3_000_123);
    body.extend(&tail);
    let (events, stats) = run(op::SPAWN, &body);
    assert!(matches!(events[..], [Event::NpcSpawn { id, .. }] if id == (3_000_123 & 0x3FFF) | 0x4000), "{events:?}");
    assert_eq!(stats.spawn_folded, 1);
    // V113: kind 0x5F without any owner path.
    let mut body = hex("89 27");
    body.extend(&tail);
    body[2] = 0x5f;
    let events = spawn(&body);
    assert!(matches!(events[..], [Event::NpcSpawn { id: 5001, kind: 0x5F, owner: None, .. }]), "{events:?}");
}

#[test]
fn a_summon_with_an_inline_name_names_its_owner() {
    // A kind 0x5F spawn with the owner's name inline and the caster field: the owner id gets a weak
    // identity record (server, job and level 0).
    let mut body = hex("a7 b0 01 5f 10 01 08 48 61 77 6b 65 79 65 31 ac 90 2c 00 40 02");
    body.extend([0u8; 20]);
    body.extend(hex("01 e8 cf 01 e8 cf 01 00 00 07 02 06 c8 05 00 00"));
    let (events, stats) = run(op::SPAWN, &body);
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(events[0], Event::NpcSpawn { id: 22567, npc_code: 2920620, kind: 0x5F, owner: Some(1480), .. }), "{events:?}");
    assert!(matches!(&events[1], Event::Identity { id: 1480, name, server: 0, job: 0, class_id: 0, level: 0, is_self: false, .. } if name == "Hawkeye1"), "{events:?}");
    assert_eq!(stats.spawn_names, 1);
    // V101: a mob's summon (Blazing Totem 2920063, in the NPC table but not a player summon template)
    // carrying a player's name is not that player's pet; no identity for the "owner" (PROTOCOL.md 9.1).
    let mut body = hex("a7 b0 01 5f 10 01 04 41 62 63 64 7f 8e 2c 00 40 02");
    body.extend([0u8; 20]);
    body.extend(hex("01 e8 cf 01 e8 cf 01 00 00 07 02 06 38 0e 00 00"));
    let (events, stats) = run(op::SPAWN, &body);
    assert!(matches!(events[..], [Event::NpcSpawn { id: 22567, npc_code: 2920063, kind: 0x5F, owner: Some(3640), .. }]), "{events:?}");
    assert_eq!((stats.spawn_names, stats.spawn_names_rejected), (0, 1));
    assert!(!crate::gamedata::GameData::get().is_player_summon_npc(2920063));
}

// ---------------------------------------------------------------- death, map, hp, ownership, pong

#[test]
fn death_flags() {
    let (events, _) = run(op::DEATH, &hex("D1 D6 01 00 03"));
    assert_eq!(events, vec![Event::Death { t_ms: 1000, id: 27473, kind: DeathKind::Died }]);
    let (events, _) = run(op::DEATH, &hex("CC C8 01 00 07"));
    assert_eq!(events, vec![Event::Death { t_ms: 1000, id: 25676, kind: DeathKind::Gone }]);
    let (events, _) = run(op::DEATH, &hex("85 E1 02 00 01"));
    assert_eq!(events, vec![Event::Death { t_ms: 1000, id: 45189, kind: DeathKind::LoadedDead }]);
    let (events, _) = run(op::DEATH, &hex("85 E1 02 00"));
    assert!(events.is_empty());
}

#[test]
fn map_loads() {
    // session1: Vakron Sky Island; V138: Fire Temple; V139: 600123.
    let (events, _) = run(op::MAP_LOAD, &hex("01 00 00 00 08 28 09 00 DA D6 23 00 00 00 00 00 AC 4A 15 47 BA EE 62 47 00 68 21 47 B5 0C A6 C2 00 01 E7 3F F3 2A A1 01 00 00 00 4F 00 00"));
    assert_eq!(events, vec![Event::ZoneChange { t_ms: 1000, map_id: 600072 }]);
    let (events, _) = run(op::MAP_LOAD, &hex("01000000d52709003b1a350000000000f7e646460d7fb0c60080b045409da54200000000000000000000004f0000"));
    assert_eq!(events, vec![Event::ZoneChange { t_ms: 1000, map_id: 600021 }]);
    let (events, _) = run(op::MAP_LOAD, &hex("03 00 00 00 3b 28 09 00 00 00 00 00 00 00 00 00"));
    assert_eq!(events, vec![Event::ZoneChange { t_ms: 1000, map_id: 600123 }]);
}

#[test]
fn remaining_hp_reads_kind_zero_of_the_u64_list() {
    // An NPC: mask 2, one u64 entry of kind 0 (the `02 01 00 <u64>` shape of the community parsers).
    let (events, _) = run(op::REMAIN_HP, &hex("ED 8A 01 02 01 00 57 42 00 00 00 00 00 00"));
    assert_eq!(events, vec![Event::RemainHp { t_ms: 1000, id: 17773, cur_hp: 16983, max_hp: None }]);
    // V140: A2Tools' dummy HP sub record.
    let (events, _) = run(op::REMAIN_HP, &hex("EF C9 01 02 01 00 DF 12 00 00 00 00 00 00"));
    assert_eq!(events, vec![Event::RemainHp { t_ms: 1000, id: 25839, cur_hp: 4831, max_hp: None }]);
    // The local player: mask 1 carries only u32 resources (MP) and yields nothing; mask 3 carries both.
    let (events, _) = run(op::REMAIN_HP, &hex("ED 3F 01 01 01 E8 0D 00 00"));
    assert!(events.is_empty());
    let (events, _) = run(op::REMAIN_HP, &hex("ED 3F 03 01 01 A1 0B 00 00 01 00 43 35 00 00 00 00 00 00"));
    assert_eq!(events, vec![Event::RemainHp { t_ms: 1000, id: 8173, cur_hp: 13635, max_hp: None }]);
    // Two u64 entries, only kind 0 is HP.
    let (events, _) = run(op::REMAIN_HP, &hex("A5 4D 03 02 01 A3 0E 00 00 08 A3 0E 00 00 02 00 0E 3C 00 00 00 00 00 00 07 0E 3C 00 00 00 00 00 00"));
    assert_eq!(events, vec![Event::RemainHp { t_ms: 1000, id: 9893, cur_hp: 15374, max_hp: None }]);
    // A value above 100 million is a misparse.
    let (events, _) = run(op::REMAIN_HP, &hex("ED 8A 01 02 01 00 57 42 00 00 01 00 00 00"));
    assert!(events.is_empty());
}

#[test]
fn world_readers_count_what_they_drop() {
    // Death: truncated, and an id of zero.
    let (events, stats) = run(op::DEATH, &hex("85 E1 02 00"));
    assert!(events.is_empty());
    assert_eq!(stats.death_rejected, 1);
    let (_, stats) = run(op::DEATH, &hex("00 00 03"));
    assert_eq!(stats.death_rejected, 1);
    // Map load: a map id of zero, and a short body.
    let (events, stats) = run(op::MAP_LOAD, &hex("01 00 00 00 00 00 00 00"));
    assert!(events.is_empty());
    assert_eq!((stats.map_rejected, stats.zone_changes), (1, 0));
    let (_, stats) = run(op::MAP_LOAD, &hex("01 00 00"));
    assert_eq!(stats.map_rejected, 1);
    // Remaining HP: a mask above 3, a list longer than 16, a truncated list, a value above 100 million.
    for body in ["ED 8A 01 05", "ED 8A 01 02 11", "ED 8A 01 02 01 00 57 42", "ED 8A 01 02 01 00 57 42 00 00 01 00 00 00"] {
        let (events, stats) = run(op::REMAIN_HP, &hex(body));
        assert!(events.is_empty(), "{body}");
        assert_eq!((stats.hp_rejected, stats.remain_hp), (1, 0), "{body}");
    }
    // HP update: a max of zero, and a truncated body.
    for body in ["D0 19 C6 4E 00", "D0 19 C6"] {
        let (events, stats) = run(op::HP_UPDATE, &hex(body));
        assert!(events.is_empty(), "{body}");
        assert_eq!(stats.hp_rejected, 1, "{body}");
    }
    // Pong: a head that is not zero, and a short body.
    for body in ["01 00 15 B1 E4 ED 03 00 00 00 11 3F F4 2A A1 01 00 00", "00 00 15 B1"] {
        let (events, stats) = run(op::PONG, &hex(body));
        assert!(events.is_empty(), "{body}");
        assert_eq!(stats.pong_rejected, 1, "{body}");
    }
}

#[test]
fn hp_updates_and_pong() {
    let (events, _) = run(op::HP_UPDATE, &hex("D0 19 C6 4E D3 64 1B 0D 00 00 1B 0D 00 00 00 00 00 00 00 00 00 00 C0 2B 00 00 F0 49 02 00 01"));
    assert_eq!(events, vec![Event::HpUpdate { t_ms: 1000, id: 3280, cur_hp: 10054, max_hp: 12883 }]);
    let (events, _) = run(op::PONG, &hex("00 00 15 B1 E4 ED 03 00 00 00 11 3F F4 2A A1 01 00 00"));
    assert!(matches!(events[..], [Event::Pong { client_ms: 16876089621, server_ms, .. }] if server_ms.abs_diff(1791722012237) < 1000), "{events:?}");
}

#[test]
fn ownership_records() {
    // V114: a summon's owner with server and name.
    let (events, stats) = run(op::OWNERSHIP, &hex("ec de 02 72 28 e9 00 ae 0b 19 05 05 41 70 65 78 5a 06 56 65 6e 74 75 73 01 00 00 00"));
    assert_eq!(events, vec![Event::OwnerLink { t_ms: 1000, entity: 44908, owner: 1454 }]);
    assert_eq!(stats.owner_links, 1);
    // The same with a broken name length is not one.
    let (events, _) = run(op::OWNERSHIP, &hex("ec de 02 72 28 e9 00 ae 0b 19 05 07 41 70 65 78 5a 06 56 65 6e 74 75 73 01 00 00 00"));
    assert!(events.is_empty());
    // session1: a kill relation (player 8173, mob 31227, no server) and a despawn record are ignored.
    for body in ["ED 3F E8 6D 18 00 FB F3 01 00 00 00 00 00 00 BA B2 8C 01 00 00 00 00 01 02", "F0 83 02 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00"] {
        let (events, stats) = run(op::OWNERSHIP, &hex(body));
        assert!(events.is_empty(), "{body}");
        assert_eq!(stats.ownership_ignored, 1);
    }
}

#[test]
fn skill_use_records_hint_the_local_player() {
    let mut p = Parser::default();
    let mut out = Vec::new();
    let me = hex("ED 3F E5 9F D5 00 D6 00"); // 8173 with a Ranger skill
    let other = hex("B8 19 E5 9F D5 00 D6 00"); // 3256 with the same skill
    let mob = hex("9A AF 02 38 6E 18 00 18 00"); // 38810 with a 7 digit skill
    for _ in 0..100 {
        p.packet(1, op::PARTY_SCOPE, &mob, &mut |e| out.push(e));
    }
    for _ in 0..5 {
        p.packet(1, op::PARTY_SCOPE, &other, &mut |e| out.push(e));
    }
    for _ in 0..30 {
        p.packet(1, op::PARTY_SCOPE, &me, &mut |e| out.push(e));
    }
    assert_eq!(p.stats.hint_self_id, Some(8173));
    for _ in 0..5 {
        p.packet(1, op::PARTY_SCOPE, &other, &mut |e| out.push(e));
    }
    assert_eq!(p.stats.hint_self_id, None, "no longer four times the runner up");
    for _ in 0..30 {
        p.packet(1, op::PARTY_SCOPE, &me, &mut |e| out.push(e));
    }
    assert_eq!(p.stats.hint_self_id, Some(8173));
    p.packet(1, op::MAP_LOAD, &hex("03 00 00 00 3b 28 09 00 00 00 00 00 00 00 00 00"), &mut |e| out.push(e));
    assert_eq!(p.stats.hint_self_id, None);
    assert!(out.iter().all(|e| matches!(e, Event::ZoneChange { .. })));
}

// ---------------------------------------------------------------- never panic

const VECTORS: &[&str] = &[
    "b1ea013600f30a40c0f4007a038000010b199b5f01000000ac52a40d021818",
    "fe9e0204009e9b01c18601001702a7a2980001000000ac52e401030303030100",
    "a8a3040001be77802a04015d06139aa06501000000dc5101c9e1f5050100",
    "91 C1 02 26 00 A1 14 48 F1 CA 00 20 02 0C 00 01 2C 40 46 4F 01 00 00 00 8C 91 01 B1 D1 14 04 97 36 97 36 97 36 97 36 01 00",
    "FB F3 01 36 00 ED 3F 84 4E D7 00 D8 02 84 00 02 6B 36 1A 54 01 00 00 00 9E 55 A9 39 03 DD 05 DD 05 DD 05 01 00",
    "FB F3 01 06 04 ED 3F E7 6B DB 00 E3 02 00 00 02 47 26 B6 55 01 00 00 00 FA 60 9D 09 01 00 F0 01",
    "1E 4E 00 87 EA 01 A2 6D 18 00 6A 02 00 00 02 53 D3 8A 09 01 00 00 00 90 4E C6 21 5A 03 00 00 01 13 B7 A3 6F 01 00",
    "b12701b127b601032c530bb739",
    "ed745e91c12837064e616963686118051e000000011c0000007f0100007f0100001c000000d002040000000000",
    "D0 19 03 20 A0 01 07 06 54 61 76 69 73 68 0F 00 00 00 02 01 40 92 FE BD A3 47 4C B9 40 C7 17 F5 0D 47 5D F7 DC 42 D0 0C 01 DA 25 D3 64 1B 0D 00 00 1B 0D 00 00 00 00 00 00 00 00 00 00 28 CC 01 00 C0 06 02 00 C0 2B 00 00 F0 49 02 00 01 00 00 00 A0 86 01 00 A0 86 01 00 F9 42 09 00 F9 42 09 00 01 01 A4 67 65 02 A8 FF 4F 01 0B 09 B8 00 00 00 00 00 0B 09 08 4C 61 6E 74 65 72 6E 73 02 00 01",
    "01000000012e0600000000000000000000000000000000000101010100000000001905 0245661e0000002d0000006400000019050000000102030000000000000000",
    "F4 95 02 5F 10 00 C6 8E 2C 00 40 02 80 CA A0 47 00 E3 37 C7 00 26 0E 47 16 37 45 43 3E 8C 01 9D 6D 9D 6D 78 0F 00 00 78 0F 00 00 00 00 00 00 00 00 00 00 00 00 00 00 30 1E 02 00 64 00 00 00 F0 49 02 00 01 00 00 00 00 00 00 00 A0 86 01 00 00 00 00 00 1F 01 09 00 01 01 01 11 01 44 3A 9A 09 FF FF FF FF FF FF FF FF 80 75 D5 2A BB 03 00 00 E7 75 0E 02 46 04 A1 47 17 25 37 C7 00 F6 0D 47 07 02 06 E7 3A 00 00 0E 00 00 00 00 00 21 05 0C 4C 61 73 74 41 6C 6C 69 61 6E 63 65 01 00",
    "FE 91 03 1F 00 01 09 51 75 69 6C 6C 66 65 72 6E AC 90 2C 00 00 02 00 25 14 C7",
    "A5 4D 03 02 01 A3 0E 00 00 08 A3 0E 00 00 02 00 0E 3C 00 00 00 00 00 00 07 0E 3C 00 00 00 00 00 00",
    "ec de 02 72 28 e9 00 ae 0b 19 05 05 41 70 65 78 5a 06 56 65 6e 74 75 73 01 00 00 00",
    "00 00 15 B1 E4 ED 03 00 00 00 11 3F F4 2A A1 01 00 00",
    "D8 1A 0F 00 0C 4C 65 61 76 69 6E 67 20 6E 6F 77 2E 05 CC 27 09 00 00 03 B1 A1 03 00 00 00 FE 08 FF 01 02 05 1E 01 B1 A1 03 00 00 00 FE 08 06 43 6F 72 76 69 6E 10 00 00 00 2D 00 00 00 D0 08 00 00 FE 08 E6 10 04 E4 9F 01 00 00 00 00 00 00 02 00 00 00 00 00 00 00 01 01 1E 02 7B FE 02 00 00 00 0B 09 06 59 6C 76 61 6E 65 08 00 00 00 2D 00 00 00 35 06 00 00 0B 09 E6 10 04 9C 20 01 00 00 00 00 00 00 15 00 00 00 00 00 00 00 01 01",
    "44 36 9e 9b 01 03 e3 81 82",
    "45 36 05 00 02 4d 37 00",
];

const OPCODES: &[u16] = &[
    op::HEARTBEAT,
    op::DAMAGE,
    op::DOT,
    op::SELF_INFO,
    op::PLAYER_INFO,
    op::PLAYER_INFO_LEGACY,
    op::PARTY_ROSTER,
    op::SPAWN,
    op::DEATH,
    op::MAP_LOAD,
    op::REMAIN_HP,
    op::HP_UPDATE,
    op::OWNERSHIP,
    op::PONG,
    op::PARTY_SCOPE,
    0x1234,
];

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[test]
fn the_parser_never_panics_on_random_or_truncated_input() {
    let mut p = Parser::default();
    let mut sink = |_e: Event| {};
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for &opcode in OPCODES {
        for _ in 0..1500 {
            let len = (rng.next() % 96) as usize;
            let mut body: Vec<u8> = (0..len).map(|_| (rng.next() >> 24) as u8).collect();
            // Sprinkle the bytes the scanners key on so the deeper paths run too.
            if len > 8 {
                let at = (rng.next() as usize) % (len - 2);
                let marker: [u8; 2] = match rng.next() % 6 {
                    0 => [0x33, 0x36],
                    1 => [0x45, 0x36],
                    2 => [0x02, 0x97],
                    3 => [0xFF, 0xFF],
                    4 => [0x01, 0x00],
                    _ => [0x07, 0x02],
                };
                body[at..at + 2].copy_from_slice(&marker);
            }
            p.packet(1, opcode, &body, &mut sink);
        }
        for v in VECTORS {
            let full = hex(v);
            for cut in 0..=full.len() {
                p.packet(1, opcode, &full[..cut], &mut sink);
            }
            for _ in 0..64 {
                let mut mutated = full.clone();
                let at = (rng.next() as usize) % mutated.len();
                mutated[at] = (rng.next() >> 16) as u8;
                p.packet(1, opcode, &mutated, &mut sink);
            }
        }
    }
    assert!(p.stats.packets > 0);
}

#[test]
fn every_damage_amount_from_the_fuzz_stays_bounded() {
    // Whatever random bytes produce, no value above the cap and no zero id ever reaches an event.
    let mut p = Parser::default();
    let mut rng = XorShift(42);
    let mut worst = 0u64;
    let mut sink = |e: Event| {
        if let Event::Damage { amount, actor, target, .. } | Event::Heal { amount, actor, target, .. } = e {
            assert!(actor != 0 && target != 0);
            worst = worst.max(amount);
        }
    };
    for _ in 0..20_000 {
        let len = 20 + (rng.next() % 40) as usize;
        let body: Vec<u8> = (0..len).map(|_| (rng.next() >> 24) as u8).collect();
        p.packet(1, op::DAMAGE, &body, &mut sink);
        p.packet(1, op::DOT, &body, &mut sink);
    }
    assert!(worst <= MAX_DAMAGE);
}
