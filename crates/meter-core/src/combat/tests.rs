//! Tracker rules on synthetic event sequences.

use super::*;
use crate::parser::{DeathKind, Event, HitFlags, PartyMember};
use crate::snapshot::{ActorStats, EndReason, Snapshot};

/// Awakened Alturadon, a boss in the NPC table.
const BOSS_CODE: u32 = 2_300_243;
/// Draconute Ranger, plain trash.
const TRASH_CODE: u32 = 2_000_002;
/// Punching Bag, a dummy.
const DUMMY_CODE: u32 = 2_000_000;
/// A player summon template.
const SUMMON_CODE: u32 = 2_920_011;
/// Vakron Sky Island (instance) and an open world map.
const MAP_INSTANCE: u32 = 600_072;
const MAP_INSTANCE_2: u32 = 600_012;
const MAP_OPEN: u32 = 1_110;
/// Keen Strike with a specialisation suffix (Gladiator), its base, Recuperation (Cleric heal) and a
/// spirit skill (Elementalist).
const STRIKE: u32 = 11_020_047;
const STRIKE_BASE: u32 = 11_020_000;
const HEAL: u32 = 18_120_047;
const SPIRIT_SKILL: u32 = 100_600;
const NPC_SKILL: u32 = 1_234_567;

const ME: u32 = 1;
const ALLY: u32 = 2;
const BOSS: u32 = 100;
const TRASH: u32 = 200;
const T0: u64 = 1_760_000_000_000;

fn at(s: f64) -> u64 {
    (T0 as f64 + s * 1000.0).round() as u64
}

fn identity(t_ms: u64, id: u32, name: &str, class_id: u32, is_self: bool) -> Event {
    Event::Identity { t_ms, id, name: name.into(), server: 1001, job: 0, class_id, level: 50, is_self }
}

/// The owner name a summon spawn carries: no server, job or level.
fn weak_identity(t_ms: u64, id: u32, name: &str) -> Event {
    Event::Identity { t_ms, id, name: name.into(), server: 0, job: 0, class_id: 0, level: 0, is_self: false }
}

fn spawn_hp(t_ms: u64, id: u32, npc_code: u32, hp: u64) -> Event {
    Event::NpcSpawn { t_ms, id, npc_code, kind: 0x0C, cur_hp: Some(hp), max_hp: Some(hp), owner: None }
}

fn spawn(t_ms: u64, id: u32, npc_code: u32) -> Event {
    spawn_hp(t_ms, id, npc_code, 1_000_000)
}

fn summon(t_ms: u64, id: u32, owner: Option<u32>) -> Event {
    Event::NpcSpawn { t_ms, id, npc_code: SUMMON_CODE, kind: 0x5F, cur_hp: Some(10_000), max_hp: Some(10_000), owner }
}

fn hit_flags(t_ms: u64, actor: u32, target: u32, skill: u32, amount: u64, flags: HitFlags) -> Event {
    Event::Damage { t_ms, actor, target, skill, amount, flags, extra_hits: 0 }
}

fn hit(t_ms: u64, actor: u32, target: u32, skill: u32, amount: u64) -> Event {
    hit_flags(t_ms, actor, target, skill, amount, HitFlags::default())
}

fn heal(t_ms: u64, actor: u32, target: u32, skill: u32, amount: u64) -> Event {
    Event::Heal { t_ms, actor, target, skill, amount, flags: HitFlags::default() }
}

fn cast(t_ms: u64, actor: u32, target: u32, skill: u32) -> Event {
    Event::Cast { t_ms, actor, target, skill }
}

fn death(t_ms: u64, id: u32) -> Event {
    Event::Death { t_ms, id, kind: DeathKind::Died }
}

fn hp(t_ms: u64, id: u32, cur_hp: u64) -> Event {
    Event::RemainHp { t_ms, id, cur_hp, max_hp: None }
}

fn zone(t_ms: u64, map_id: u32) -> Event {
    Event::ZoneChange { t_ms, map_id }
}

fn party(t_ms: u64, members: &[(&str, u32)]) -> Event {
    Event::Party {
        t_ms,
        members: members
            .iter()
            .map(|(name, class_id)| PartyMember {
                name: (*name).into(),
                job: 0,
                class_id: *class_id,
                level: 50,
                gear_score: 1_200,
                combat_power: 50_000,
            })
            .collect(),
    }
}

fn feed(tr: &mut Tracker, events: &[Event]) {
    for e in events {
        tr.push(e);
    }
}

/// Me (Gladiator, self) and Ally (Cleric) in a party inside an instance, a boss spawned.
fn boss_setup(tr: &mut Tracker) {
    feed(
        tr,
        &[
            zone(at(-10.0), MAP_INSTANCE),
            identity(at(-9.0), ME, "Me", 11, true),
            identity(at(-9.0), ALLY, "Ally", 17, false),
            party(at(-8.0), &[("Me", 11), ("Ally", 17)]),
            spawn(at(-5.0), BOSS, BOSS_CODE),
        ],
    );
}

fn boss_tracker() -> Tracker {
    let mut tr = Tracker::default();
    boss_setup(&mut tr);
    tr
}

fn snap(tr: &Tracker) -> Snapshot {
    tr.snapshot().expect("a snapshot")
}

fn actor<'a>(s: &'a Snapshot, name: &str) -> &'a ActorStats {
    s.actors.iter().find(|a| a.name == name).unwrap_or_else(|| panic!("no actor {name} in {:?}", s.actors.iter().map(|a| &a.name).collect::<Vec<_>>()))
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn idle_tracker_has_no_snapshot_and_last_after_end() {
    let mut tr = boss_tracker();
    assert!(tr.snapshot().is_none());
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000)]);
    let live = snap(&tr);
    assert!(live.active && live.end_reason.is_none());
    assert_eq!(live.id, 1);
    feed(&mut tr, &[death(at(5.0), BOSS)]);
    let last = snap(&tr);
    assert!(!last.active);
    assert_eq!(last.end_reason, Some(EndReason::BossDied));
    assert_eq!(last.id, 1);
    assert!(!tr.is_live());
}

#[test]
fn boss_kill_ends_with_boss_died_and_freezes_duration() {
    let mut tr = boss_tracker();
    feed(
        &mut tr,
        &[
            hit(at(0.0), ME, BOSS, STRIKE, 1000),
            hit(at(2.0), ALLY, BOSS, STRIKE, 500),
            hit(at(4.0), ME, BOSS, STRIKE, 1000),
            death(at(6.5), BOSS),
        ],
    );
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::BossDied));
    assert_eq!(s.duration_ms, 4000, "duration runs to the last hit, not the death");
    assert_eq!(s.started_ms, at(0.0));
    let boss = s.boss.as_ref().expect("boss");
    assert!(boss.is_boss && boss.dead);
    assert_eq!(boss.name, "Awakened Alturadon");
    assert_eq!(boss.cur_hp, Some(0));
    assert_eq!(boss.damage_taken, 2500);
    assert_eq!(s.map_name.as_deref(), Some("Vakron Sky Island"));
    assert!(s.is_instance);
    assert_eq!(s.deaths.len(), 1);
    assert_eq!(s.deaths[0].name, "Awakened Alturadon");

    // Late ticks on the dead boss are dropped and do not open a new encounter; time does not move it.
    feed(&mut tr, &[hit(at(7.0), ME, BOSS, STRIKE, 300)]);
    tr.tick(at(500.0));
    let again = snap(&tr);
    assert_eq!(again.id, 1);
    assert_eq!(again.duration_ms, 4000);
    assert_eq!(tr.stats.damage_dropped, 1);
    assert_eq!(tr.drain_finished().len(), 1);
}

#[test]
fn trash_then_boss_clears_short_trash_and_reuses_the_id() {
    let mut tr = boss_tracker();
    feed(
        &mut tr,
        &[
            spawn(at(-1.0), TRASH, TRASH_CODE),
            hit(at(0.0), ME, TRASH, STRIKE, 700),
            hit(at(2.0), ME, TRASH, STRIKE, 700),
            hit(at(5.0), ME, BOSS, STRIKE, 1000),
        ],
    );
    let s = snap(&tr);
    assert_eq!(s.id, 1, "a trash prelude under 15 s is dropped and its id reused");
    assert_eq!(s.started_ms, at(5.0));
    assert_eq!(s.total_damage, 1000);
    assert_eq!(s.targets.len(), 1);
    assert_eq!(actor(&s, "Me").damage, 1000);
    assert!(tr.drain_finished().is_empty());
}

#[test]
fn trash_then_boss_keeps_long_trash_as_its_own_encounter() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[spawn(at(-1.0), TRASH, TRASH_CODE), hit(at(0.0), ME, TRASH, STRIKE, 700), hit(at(16.0), ME, TRASH, STRIKE, 700)]);
    feed(&mut tr, &[hit(at(18.0), ME, BOSS, STRIKE, 1000)]);
    let finished = tr.drain_finished();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].id, 1);
    assert_eq!(finished[0].duration_ms, 16_000);
    assert_eq!(finished[0].total_damage, 1400);
    assert!(!finished[0].boss.as_ref().unwrap().is_boss);
    let live = snap(&tr);
    assert_eq!(live.id, 2);
    assert_eq!(live.total_damage, 1000);
}

#[test]
fn boss_with_adds_keeps_the_boss_as_main_target() {
    let mut tr = boss_tracker();
    feed(
        &mut tr,
        &[
            hit(at(0.0), ME, BOSS, STRIKE, 100),
            spawn(at(1.0), TRASH, TRASH_CODE),
            hit(at(2.0), ME, TRASH, STRIKE, 5000),
            hit(at(3.0), ALLY, TRASH, STRIKE, 5000),
        ],
    );
    let s = snap(&tr);
    assert_eq!(s.boss.as_ref().unwrap().id, BOSS);
    assert_eq!(s.targets.len(), 2);
    let me = actor(&s, "Me");
    assert_eq!(me.damage, 100);
    assert_eq!(me.damage_all_targets, 5100);
    assert_eq!(me.targets.len(), 2);
    assert_eq!(me.targets[0].damage, 5000, "targets are sorted by damage");
    assert_eq!(s.total_damage, 10_100);
    assert_eq!(s.duration_ms, 0, "only the boss hit counts for the duration");
}

#[test]
fn wipe_when_every_party_player_is_dead() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(1.0), ALLY, BOSS, STRIKE, 1000), death(at(20.0), ME)]);
    assert!(snap(&tr).active, "one death is not a wipe");
    feed(&mut tr, &[death(at(25.0), ALLY)]);
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Wipe));
    assert_eq!(actor(&s, "Me").deaths, 1);
    assert_eq!(actor(&s, "Ally").deaths, 1);
    assert_eq!(s.deaths.len(), 2);
    assert!(s.deaths.iter().all(|d| d.is_player));
    assert_eq!(s.deaths[0].name, "Me");
}

#[test]
fn boss_hp_back_at_max_after_quiet_ends_the_attempt() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hp(at(0.5), BOSS, 999_000), hp(at(3.0), BOSS, 1_000_000)]);
    assert!(snap(&tr).active, "full HP within 5 s of the last hit is not a reset");
    feed(&mut tr, &[hp(at(6.0), BOSS, 1_000_000)]);
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Wipe));
    assert_eq!(s.id, 1);
    feed(&mut tr, &[hit(at(30.0), ALLY, BOSS, STRIKE, 10)]);
    let s2 = snap(&tr);
    assert!(s2.active);
    assert_eq!(s2.id, 2, "the next hit opens attempt two");
    assert_eq!(s2.boss.as_ref().unwrap().hp_pct, Some(1.0));
}

#[test]
fn idle_timeout_depends_on_boss_instance_and_dummy() {
    // A living boss in an instance: 180 s.
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000)]);
    tr.tick(at(179.0));
    assert!(snap(&tr).active);
    tr.tick(at(180.0));
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Idle));
    assert_eq!(s.duration_ms, 0);

    // Trash in an instance: the open world timeout.
    let mut tr = boss_tracker();
    feed(&mut tr, &[spawn(at(-1.0), TRASH, TRASH_CODE), hit(at(0.0), ME, TRASH, STRIKE, 1000)]);
    tr.tick(at(19.0));
    assert!(snap(&tr).active);
    tr.tick(at(20.0));
    assert_eq!(snap(&tr).end_reason, Some(EndReason::Idle));

    // A boss in the open world: the open world timeout.
    let mut tr = Tracker::default();
    feed(&mut tr, &[zone(at(-10.0), MAP_OPEN), identity(at(-9.0), ME, "Me", 11, true), spawn(at(-5.0), BOSS, BOSS_CODE)]);
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000)]);
    tr.tick(at(19.0));
    assert!(snap(&tr).active);
    tr.tick(at(20.0));
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Idle));
    assert!(!s.is_instance);

    // A dummy: 10 s.
    let mut tr = boss_tracker();
    feed(&mut tr, &[spawn(at(-1.0), 300, DUMMY_CODE), hit(at(0.0), ME, 300, STRIKE, 1000)]);
    tr.tick(at(9.0));
    assert!(snap(&tr).active);
    tr.tick(at(10.0));
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Idle));
    assert!(s.boss.as_ref().unwrap().is_dummy);

    // The event clock alone ends a stale encounter too, and the hit that does so opens the next one.
    let mut tr = boss_tracker();
    feed(&mut tr, &[spawn(at(-1.0), 300, DUMMY_CODE), hit(at(0.0), ME, 300, STRIKE, 1000)]);
    feed(&mut tr, &[hit(at(10.0), ME, 300, STRIKE, 1000)]);
    let s = snap(&tr);
    assert!(s.active);
    assert_eq!(s.id, 2);
    assert_eq!(s.started_ms, at(10.0));
}

#[test]
fn dummy_cap_ends_with_dummy_cap() {
    let mut tr = Tracker::new(TrackerConfig { dummy_cap_ms: 30_000, ..TrackerConfig::default() });
    boss_setup(&mut tr);
    feed(&mut tr, &[spawn(at(-1.0), 300, DUMMY_CODE)]);
    for i in 0..6 {
        feed(&mut tr, &[hit(at(i as f64 * 5.0), ME, 300, STRIKE, 1000)]);
    }
    tr.tick(at(29.0));
    assert!(snap(&tr).active);
    tr.tick(at(30.0));
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::DummyCap));
    assert_eq!(s.duration_ms, 25_000);
}

#[test]
fn zone_change_ends_and_a_teleport_does_not() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), zone(at(1.0), MAP_INSTANCE)]);
    assert!(snap(&tr).active, "a load of the same map is a teleport");
    feed(&mut tr, &[zone(at(2.0), MAP_INSTANCE_2)]);
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::ZoneChange));
    assert_eq!(s.map_name.as_deref(), Some("Vakron Sky Island"), "the snapshot keeps the map it was fought on");
    assert_eq!(tr.map_id(), Some(MAP_INSTANCE_2));
    // The new map is used by the next encounter; names survive the zone change.
    feed(&mut tr, &[spawn(at(3.0), BOSS, BOSS_CODE), hit(at(4.0), ME, BOSS, STRIKE, 10)]);
    let s = snap(&tr);
    assert_eq!(s.map_name.as_deref(), Some("Urugugu Canyon"));
    assert_eq!(actor(&s, "Me").name, "Me");
}

#[test]
fn manual_reset_ends_and_clears_when_idle() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000)]);
    tr.tick(at(3.0));
    tr.reset();
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Manual));
    assert!(!s.active);
    tr.reset();
    assert!(tr.snapshot().is_none(), "a reset with nothing live clears the display");
}

#[test]
fn summon_damage_folds_into_the_owner_from_the_spawn() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[summon(at(-1.0), 50, Some(ME)), hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(1.0), 50, BOSS, SPIRIT_SKILL, 400)]);
    let s = snap(&tr);
    assert_eq!(s.actors.len(), 1, "no row for the summon");
    let me = actor(&s, "Me");
    assert_eq!(me.damage, 1400);
    assert_eq!(me.hits, 2);
    assert_eq!(me.skills.len(), 2);
    assert_eq!(s.total_damage, 1400);
    assert_eq!(me.class_name.as_deref(), Some("Gladiator"));
}

#[test]
fn summon_damage_folds_into_the_owner_from_an_owner_link() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[summon(at(-1.0), 50, None), hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(1.0), 50, BOSS, SPIRIT_SKILL, 400)]);
    let s = snap(&tr);
    assert_eq!(actor(&s, "Me").damage, 1000, "an orphan of another class stays unattributed");
    // The orphan keeps its own row rather than vanishing: the boss took the hit and the rows add up.
    assert_eq!(s.actors.len(), 2);
    let orphan = actor(&s, "#50");
    assert_eq!((orphan.damage, orphan.is_player, orphan.is_party, orphan.class_name.as_deref()), (400, true, false, Some("Elementalist")));
    assert_eq!(s.boss.as_ref().unwrap().damage_taken, 1400);
    assert_eq!(s.total_damage, 1400);
    feed(&mut tr, &[Event::OwnerLink { t_ms: at(2.0), entity: 50, owner: ME }]);
    let s = snap(&tr);
    assert_eq!(actor(&s, "Me").damage, 1400, "the link applies to damage already counted");
    assert_eq!(s.actors.len(), 1, "and the orphan row is gone");
    assert_eq!(s.total_damage, 1400);
}

#[test]
fn unowned_summon_merges_into_the_only_player_of_its_class() {
    let mut tr = Tracker::default();
    feed(
        &mut tr,
        &[
            zone(at(-10.0), MAP_INSTANCE),
            identity(at(-9.0), ME, "Me", 16, true),
            identity(at(-9.0), ALLY, "Ally", 17, false),
            party(at(-8.0), &[("Me", 16), ("Ally", 17)]),
            spawn(at(-5.0), BOSS, BOSS_CODE),
            summon(at(-1.0), 50, None),
            hit(at(0.0), 50, BOSS, SPIRIT_SKILL, 400),
        ],
    );
    let s = snap(&tr);
    assert_eq!(s.actors.len(), 1);
    let me = actor(&s, "Me");
    assert_eq!(me.damage, 400);
    assert_eq!(me.id, ME);
    assert!(me.is_self);

    // A second Elementalist makes it ambiguous: the spirit is shown under its own id, not dropped.
    feed(&mut tr, &[identity(at(1.0), 3, "Other", 16, false), party(at(1.0), &[("Me", 16), ("Ally", 17), ("Other", 16)])]);
    let s = snap(&tr);
    assert_eq!(s.actors.len(), 1);
    assert_eq!((actor(&s, "#50").damage, actor(&s, "#50").is_self), (400, false));
    assert_eq!(s.total_damage, 400);
}

#[test]
fn weak_identity_never_reclassifies_a_spawned_mob_or_replaces_a_name() {
    let mut tr = boss_tracker();
    // A summon spawn whose misread owner id is the boss: the boss stays a boss and the fight goes on.
    feed(&mut tr, &[weak_identity(at(-1.0), BOSS, "Hawkeye1"), hit(at(0.0), ME, BOSS, STRIKE, 1000)]);
    let s = snap(&tr);
    assert_eq!(s.boss.as_ref().unwrap().name, "Awakened Alturadon");
    assert!(s.boss.as_ref().unwrap().is_boss);
    assert_eq!(actor(&s, "Me").damage, 1000);
    feed(&mut tr, &[death(at(1.0), BOSS)]);
    assert_eq!(snap(&tr).end_reason, Some(EndReason::BossDied), "the boss death still ends it");
    // The same for a spawned summon, and for a player with an authoritative name (V104: "Bowmaster"
    // keeps its name over the pet spawn's "Hawkeye1").
    let mut tr = boss_tracker();
    feed(&mut tr, &[summon(at(-1.0), 50, Some(ME)), weak_identity(at(-0.5), 50, "Hawkeye1"), weak_identity(at(-0.5), ALLY, "Hawkeye1")]);
    feed(&mut tr, &[hit(at(0.0), 50, BOSS, SPIRIT_SKILL, 400), hit(at(1.0), ALLY, BOSS, STRIKE, 300)]);
    let s = snap(&tr);
    assert_eq!(actor(&s, "Me").damage, 400, "the summon still folds into Me");
    assert_eq!(actor(&s, "Ally").damage, 300);
    assert!(s.actors.iter().all(|a| a.name != "Hawkeye1"), "{:?}", s.actors.iter().map(|a| &a.name).collect::<Vec<_>>());
    // An id nobody has named yet takes it.
    feed(&mut tr, &[weak_identity(at(2.0), 7, "Hawkeye1"), hit(at(3.0), 7, BOSS, STRIKE, 200)]);
    assert_eq!(actor(&snap(&tr), "Hawkeye1").damage, 200);
}

#[test]
fn heal_skill_ticks_on_an_unclassified_target_are_not_damage() {
    let mut tr = boss_tracker();
    let dot = HitFlags { dot: true, ..HitFlags::default() };
    // A Recuperation HoT tick arrives as DoT damage on an id the registry has not classified (a party
    // member who has not acted yet): it opens nothing.
    feed(&mut tr, &[hit_flags(at(0.0), ALLY, 9, HEAL, 220, dot)]);
    assert!(tr.snapshot().is_none());
    assert_eq!(tr.stats.damage_dropped, 1);
    // Nor does it count inside a fight: no target "#9" and no row for Ally.
    feed(&mut tr, &[hit(at(1.0), ME, BOSS, STRIKE, 1000), hit_flags(at(2.0), ALLY, 9, HEAL, 220, dot)]);
    let s = snap(&tr);
    assert_eq!(s.targets.len(), 1);
    assert_eq!(s.total_damage, 1000);
    assert_eq!(s.actors.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), vec!["Me"]);
    assert_eq!(tr.stats.damage_dropped, 2);
}

#[test]
fn main_target_promoted_to_boss_mid_fight_keeps_the_encounter() {
    let mut tr = Tracker::default();
    // An unknown code with 5 M HP at spawn is trash; the meter started mid session and the first HP
    // report above 10 M promotes it. The next hit must continue the encounter, not restart it.
    feed(&mut tr, &[zone(at(-10.0), MAP_INSTANCE), identity(at(-9.0), ME, "Me", 11, true), spawn_hp(at(-5.0), 400, 9_999_999, 5_000_000)]);
    feed(&mut tr, &[hit(at(0.0), ME, 400, STRIKE, 1000)]);
    assert!(!snap(&tr).boss.as_ref().unwrap().is_boss);
    feed(&mut tr, &[hp(at(1.0), 400, 20_000_000), hit(at(2.0), ME, 400, STRIKE, 1000)]);
    let s = snap(&tr);
    assert_eq!((s.id, s.started_ms), (1, at(0.0)));
    assert!(s.boss.as_ref().unwrap().is_boss);
    assert_eq!(actor(&s, "Me").damage, 2000);
    assert_eq!(tr.stats.encounters_started, 1);
    tr.tick(at(100.0));
    assert!(snap(&tr).active, "and it gets the instance timeout from now on");
}

#[test]
fn divine_aura_ticks_from_an_unspawned_id_fold_into_the_only_cleric() {
    let mut tr = boss_tracker();
    // session3 13:55: a Cleric casts Divine Aura (17150340) and an entity the capture never saw spawn
    // ticks 17150342 on the boss every 600 ms. It is the Cleric's effect entity, not a sixth player.
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), cast(at(0.5), ALLY, ALLY, 17_150_340), cast(at(0.6), 70, BOSS, 17_150_342)]);
    feed(&mut tr, &[hit(at(1.0), 70, BOSS, 17_150_342, 300), hit(at(1.6), 70, BOSS, 17_150_342, 300)]);
    let s = snap(&tr);
    assert_eq!(s.actors.len(), 2, "{:?}", s.actors.iter().map(|a| &a.name).collect::<Vec<_>>());
    assert_eq!(actor(&s, "Ally").damage, 600);
    assert!(actor(&s, "Ally").skills.iter().any(|k| k.name == "Divine Aura"));
    assert_eq!(s.total_damage, 1600);
}

#[test]
fn self_hint_marks_the_local_player_until_the_self_record() {
    let mut tr = Tracker::default();
    feed(&mut tr, &[zone(at(-10.0), MAP_INSTANCE), identity(at(-9.0), ALLY, "Ally", 17, false), party(at(-8.0), &[("Me", 11), ("Ally", 17)]), spawn(at(-5.0), BOSS, BOSS_CODE)]);
    assert_eq!(tr.self_id(), None);
    tr.set_self_hint(Some(8));
    assert_eq!(tr.self_id(), Some(8));
    feed(&mut tr, &[hit(at(0.0), 8, BOSS, STRIKE, 1000), hit(at(1.0), ALLY, BOSS, STRIKE, 500)]);
    let s = snap(&tr);
    assert_eq!(s.self_id, Some(8));
    let me = actor(&s, "Me");
    assert!(me.is_self && me.is_party, "the roster fallback names the row and the hint marks it self");
    assert_eq!(me.id, 8);
    assert!(!actor(&s, "Ally").is_self);
    // Withdrawn on a map load, nothing is self until the next hint.
    tr.set_self_hint(None);
    assert_eq!(tr.self_id(), None);
    // The self record wins over any later hint.
    feed(&mut tr, &[identity(at(2.0), 9, "Me", 11, true)]);
    tr.set_self_hint(Some(8));
    assert_eq!(tr.self_id(), Some(9));
    assert_eq!(snap(&tr).self_id, Some(9));
}

#[test]
fn player_id_change_keeps_one_row() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), identity(at(1.0), 7, "Me", 11, true), hit(at(2.0), 7, BOSS, STRIKE, 500)]);
    let s = snap(&tr);
    assert_eq!(s.actors.len(), 1);
    let me = actor(&s, "Me");
    assert_eq!(me.id, 7);
    assert_eq!(me.damage, 1500);
    assert_eq!(me.hits, 2);
    assert!(me.is_self && me.is_party);
    assert_eq!(me.timeline.len(), 2);
    assert_eq!(s.self_id, Some(7));
    assert_eq!(tr.self_id(), Some(7));
    assert_eq!(tr.self_name(), Some("Me"));
}

#[test]
fn self_heal_is_healing_not_damage() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(1.0), ME, ME, HEAL, 500), heal(at(2.0), ALLY, ME, HEAL, 300)]);
    let s = snap(&tr);
    let me = actor(&s, "Me");
    assert_eq!(me.damage, 1000);
    assert_eq!(me.healing, 500);
    assert_eq!(s.targets.len(), 1, "the player is not a target");
    let ally = actor(&s, "Ally");
    assert_eq!(ally.healing, 300);
    assert_eq!(ally.damage, 0);
    assert_eq!(ally.skills.len(), 1);
    assert_eq!(ally.skills[0].name, "Recuperation");
    assert_eq!(ally.skills[0].healing, 300);
    assert_eq!(s.duration_ms, 0, "heals do not extend the fight");
    assert!(approx(ally.hps, 300.0));
}

#[test]
fn npc_skill_on_a_player_is_damage_taken() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(1.0), BOSS, ME, NPC_SKILL, 300), hit(at(1.5), BOSS, ALLY, NPC_SKILL, 200)]);
    let s = snap(&tr);
    assert_eq!(actor(&s, "Me").damage_taken, 300);
    let ally = actor(&s, "Ally");
    assert_eq!(ally.damage_taken, 200);
    assert_eq!(ally.damage, 0);
    assert_eq!(s.total_damage, 1000);
    assert_eq!(s.actors.len(), 2, "the boss is not an actor");
    assert_eq!(tr.stats.damage_taken, 2);
}

#[test]
fn dps_share_active_time_and_timeline() {
    let mut tr = boss_tracker();
    feed(
        &mut tr,
        &[
            hit(at(0.0), ME, BOSS, STRIKE, 1000),
            hit(at(1.0), ME, BOSS, STRIKE, 2000),
            hit(at(2.0), ME, BOSS, STRIKE, 3000),
            hit(at(4.0), ALLY, BOSS, STRIKE, 4000),
        ],
    );
    let s = snap(&tr);
    assert_eq!(s.duration_ms, 4000);
    let me = actor(&s, "Me");
    assert_eq!(me.damage, 6000);
    assert!(approx(me.dps, 1500.0));
    assert!(approx(me.share, 0.6));
    assert_eq!(me.active_ms, 2000);
    assert!(approx(me.active_dps, 3000.0));
    assert_eq!(me.timeline.len(), 3);
    assert_eq!((me.timeline[1].t_s, me.timeline[1].damage), (1, 2000));
    let ally = actor(&s, "Ally");
    assert!(approx(ally.dps, 1000.0));
    assert!(approx(ally.share, 0.4));
    assert_eq!(ally.active_ms, 0);
    assert!(approx(ally.active_dps, 4000.0), "a single hit is floored to one second");
    assert_eq!(ally.timeline, vec![crate::snapshot::TimelinePoint { t_s: 4, damage: 4000 }]);
    assert!(approx(s.party_dps, 2500.0));
    assert_eq!(s.actors[0].name, "Me", "sorted by damage");

    // A gap longer than the active gap does not count.
    feed(&mut tr, &[hit(at(20.0), ME, BOSS, STRIKE, 1000)]);
    let me = snap(&tr);
    assert_eq!(actor(&me, "Me").active_ms, 2000);
    assert_eq!(me.duration_ms, 20_000);
}

#[test]
fn short_fights_floor_the_duration_at_one_second() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(0.2), ME, BOSS, STRIKE, 1000)]);
    let s = snap(&tr);
    assert_eq!(s.duration_ms, 200);
    assert!(approx(actor(&s, "Me").dps, 2000.0));
    assert!(approx(s.party_dps, 2000.0));
}

#[test]
fn dot_and_direct_rows_are_separate() {
    let mut tr = boss_tracker();
    let crit = HitFlags { crit: true, back: true, ..HitFlags::default() };
    let dot = HitFlags { dot: true, ..HitFlags::default() };
    feed(
        &mut tr,
        &[
            cast(at(0.0), ME, BOSS, STRIKE),
            hit_flags(at(0.1), ME, BOSS, STRIKE, 1000, crit),
            hit_flags(at(1.0), ME, BOSS, STRIKE, 200, dot),
            hit_flags(at(2.0), ME, BOSS, STRIKE, 200, dot),
            hit(at(3.0), ME, BOSS, 11_020_000, 500),
        ],
    );
    let s = snap(&tr);
    let me = actor(&s, "Me");
    assert_eq!(me.skills.len(), 2);
    let direct = &me.skills[0];
    assert_eq!((direct.skill_id, direct.name.as_str(), direct.dot), (STRIKE_BASE, "Keen Strike", false));
    assert_eq!((direct.damage, direct.hits, direct.crits, direct.casts, direct.back), (1500, 2, 1, 1, 1));
    assert_eq!((direct.max_hit, direct.min_hit), (1000, 500));
    assert!(approx(direct.avg_hit, 750.0));
    assert!(approx(direct.crit_rate, 0.5));
    assert!(direct.icon.is_some());
    let dots = &me.skills[1];
    assert_eq!((dots.skill_id, dots.name.as_str(), dots.dot), (STRIKE_BASE, "Keen Strike (DoT)", true));
    assert_eq!((dots.damage, dots.hits, dots.casts), (400, 2, 0));
    assert_eq!(me.hits, 4);
    assert_eq!(me.crits, 1);
    assert!(approx(me.crit_rate, 0.5), "rates are per direct hit");
    assert!(approx(me.back_rate, 0.5));
    assert_eq!(me.casts, 1, "the opener cast before the first hit is counted");
    assert_eq!(me.max_hit, 1000);
}

#[test]
fn casts_long_before_the_fight_are_not_counted() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[cast(at(-5.0), ME, BOSS, STRIKE), cast(at(-1.0), ALLY, BOSS, STRIKE), hit(at(0.0), ME, BOSS, STRIKE, 1000)]);
    let s = snap(&tr);
    assert_eq!(actor(&s, "Me").casts, 0);
    assert_eq!(actor(&s, "Ally").casts, 1);
}

#[test]
fn drain_finished_returns_each_encounter_once() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), death(at(1.0), BOSS)]);
    feed(&mut tr, &[spawn(at(10.0), 101, BOSS_CODE), hit(at(11.0), ME, 101, STRIKE, 1000)]);
    assert_eq!(tr.drain_finished().iter().map(|s| s.id).collect::<Vec<_>>(), vec![1]);
    assert!(tr.drain_finished().is_empty());
    feed(&mut tr, &[death(at(12.0), 101)]);
    let second = tr.drain_finished();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].id, 2);
    assert_eq!(second[0].end_reason, Some(EndReason::BossDied));
    assert!(tr.drain_finished().is_empty());
    assert_eq!(tr.stats.encounters_finished, 2);
}

#[test]
fn short_trash_fights_are_shown_but_not_filed() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[spawn(at(-1.0), TRASH, TRASH_CODE), hit(at(0.0), ME, TRASH, STRIKE, 1000), hit(at(2.0), ME, TRASH, STRIKE, 1000)]);
    tr.tick(at(30.0));
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Idle));
    assert_eq!(s.duration_ms, 2000);
    assert!(tr.drain_finished().is_empty(), "under 5 s and no boss");
}

#[test]
fn inferred_player_damage_is_removed_when_its_spawn_arrives() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(1.0), 300, BOSS, STRIKE, 9000)]);
    let s = snap(&tr);
    assert_eq!(actor(&s, "#300").damage, 9000, "a class skill user with no spawn counts as a player");
    assert!(actor(&s, "#300").is_player);
    assert_eq!(s.boss.as_ref().unwrap().damage_taken, 10_000);
    feed(&mut tr, &[spawn(at(2.0), 300, TRASH_CODE)]);
    let s = snap(&tr);
    assert_eq!(s.actors.len(), 1);
    assert_eq!(s.boss.as_ref().unwrap().damage_taken, 1000);
    assert_eq!(s.total_damage, 1000);
    assert_eq!(tr.stats.inferred_players_removed, 1);
    // From now on it is a mob: its hits on players are damage taken.
    feed(&mut tr, &[hit(at(3.0), 300, ME, STRIKE, 50)]);
    assert_eq!(actor(&snap(&tr), "Me").damage_taken, 50);
}

#[test]
fn late_identity_names_earlier_damage() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), 5, BOSS, STRIKE, 1000)]);
    let s = snap(&tr);
    let unknown = actor(&s, "#5");
    assert_eq!(unknown.class_name.as_deref(), Some("Gladiator"), "class from the skill band");
    assert!(!unknown.is_party);
    feed(&mut tr, &[identity(at(1.0), 5, "Late", 11, false), party(at(1.0), &[("Me", 11), ("Ally", 17), ("Late", 11)])]);
    let s = snap(&tr);
    let late = actor(&s, "Late");
    assert_eq!(late.damage, 1000);
    assert!(late.is_party);
    assert_eq!(late.gear_score, Some(1_200));
    assert_eq!(late.combat_power, Some(50_000));
    assert_eq!(late.level, Some(50));
}

#[test]
fn records_that_are_not_damage_are_dropped() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000)]);
    let before = tr.stats.damage_counted;
    feed(
        &mut tr,
        &[
            hit(at(1.0), ME, ALLY, STRIKE, 500),
            hit(at(1.1), 999, BOSS, 55, 5_000_000),
            heal(at(1.2), BOSS, ME, HEAL, 1000),
            hit(at(1.3), ME, BOSS, 16_990_003, 1000),
            hit(at(1.4), ME, BOSS, STRIKE, 0),
            hit(at(1.5), BOSS, BOSS, NPC_SKILL, 1000),
        ],
    );
    assert_eq!(tr.stats.damage_counted, before);
    let s = snap(&tr);
    assert_eq!(s.total_damage, 1000);
    assert_eq!(actor(&s, "Me").healing, 0);
    assert_eq!(s.actors.len(), 1);
}

#[test]
fn a_possible_boss_is_promoted_by_its_hp() {
    let mut tr = Tracker::default();
    feed(&mut tr, &[zone(at(-10.0), MAP_INSTANCE), identity(at(-9.0), ME, "Me", 11, true), spawn_hp(at(-5.0), 400, 9_999_999, 50_000_000)]);
    feed(&mut tr, &[hit(at(0.0), ME, 400, STRIKE, 1000)]);
    let s = snap(&tr);
    let boss = s.boss.as_ref().unwrap();
    assert!(boss.is_boss);
    assert_eq!(boss.name, "#9999999");
    assert_eq!(boss.max_hp, Some(50_000_000));
    tr.tick(at(100.0));
    assert!(snap(&tr).active, "it gets the instance timeout");
}

#[test]
fn describe_lists_the_fight() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(2.0), ALLY, BOSS, STRIKE, 3000), death(at(3.0), BOSS)]);
    let text = describe(&snap(&tr));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].contains("Awakened Alturadon") && lines[0].contains("Vakron Sky Island") && lines[0].contains("0:02") && lines[0].contains("boss_died"), "{text}");
    assert!(lines[1].contains("Ally") && lines[1].contains("Cleric") && lines[1].contains("75.0%"), "{text}");
    assert!(lines[2].contains("Me") && lines[2].contains("Gladiator"), "{text}");
    assert_eq!(format_duration(3_723_000), "1:02:03");
}

#[test]
fn timeline_stops_after_two_hours_and_names_survive_a_long_fight() {
    let mut tr = boss_tracker();
    // A hit every 100 s stays inside the instance timeout; 2.5 hours in all.
    for i in 0..=90 {
        feed(&mut tr, &[hit(at(i as f64 * 100.0), ME, BOSS, STRIKE, 10)]);
    }
    let s = snap(&tr);
    assert!(s.active);
    let me = actor(&s, "Me");
    assert_eq!(me.damage, 910);
    assert_eq!(me.timeline.len(), 73, "buckets past two hours are not added");
    assert_eq!(me.timeline.last().unwrap().t_s, 7200);
    assert_eq!(me.name, "Me");
    assert_eq!(s.duration_ms, 9_000_000);
}

#[test]
fn a_spawn_with_the_local_player_id_is_ignored() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), spawn(at(1.0), ME, TRASH_CODE), hit(at(2.0), ME, BOSS, STRIKE, 1000)]);
    let s = snap(&tr);
    let me = actor(&s, "Me");
    assert_eq!(me.damage, 2000);
    assert!(me.is_self);
    assert_eq!(tr.self_id(), Some(ME));
}

fn hp_update(t_ms: u64, id: u32, cur_hp: u64, max_hp: u64) -> Event {
    Event::HpUpdate { t_ms, id, cur_hp, max_hp }
}

fn effect(t_ms: u64, id: u32, npc_code: u32, owner: Option<u32>) -> Event {
    Event::NpcSpawn { t_ms, id, npc_code, kind: 0x1C, cur_hp: Some(500), max_hp: Some(500), owner }
}

/// Cleric class skill, for an actor that is not Me or Ally.
const CLERIC_SKILL: u32 = 17_020_047;
/// A Fire Spirit's attack (value records only ever come from spirits) and the owner's summon cast.
const FIRE_SPIRIT_ATTACK: u32 = 16_100_000;
const SUMMON_WATER_SPIRIT: u32 = 16_110_240;
/// Combustion, an Elementalist player skill.
const COMBUSTION: u32 = 16_040_240;

#[test]
fn player_hp_zero_is_a_death_and_only_a_later_hp_report_or_action_revives() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(0.5), ALLY, BOSS, STRIKE, 1000), hp(at(10.0), ME, 0)]);
    let s = snap(&tr);
    assert!(s.active, "one dead player is not a wipe");
    assert_eq!(actor(&s, "Me").deaths, 1);
    assert_eq!(s.deaths.len(), 1);
    assert!(s.deaths[0].is_player && s.deaths[0].name == "Me" && s.deaths[0].t_ms == at(10.0));
    // The HP update that repeats the zero, an in flight hit and a self cast right after death are not a
    // resurrection, so the next zero is a repeat and not a second death.
    feed(&mut tr, &[hit(at(10.05), ME, BOSS, STRIKE, 100), cast(at(10.15), ME, ME, STRIKE), hp_update(at(10.3), ME, 0, 20_000), hp(at(10.4), ME, 0)]);
    assert_eq!(actor(&snap(&tr), "Me").deaths, 1);
    assert_eq!(actor(&snap(&tr), "Me").damage, 1100, "the in flight hit still counts");
    // HP back above zero is a resurrection; the next zero is a real death.
    feed(&mut tr, &[hp_update(at(14.0), ME, 20_000, 20_000), hp(at(15.0), ME, 0)]);
    assert_eq!(actor(&snap(&tr), "Me").deaths, 2);
    // A direct hit well after the death report is proof of life too.
    feed(&mut tr, &[hit(at(21.0), ME, BOSS, STRIKE, 100), hp(at(22.0), ME, 0)]);
    assert_eq!(actor(&snap(&tr), "Me").deaths, 3);
    assert!(snap(&tr).active);
    // A death record for the same player is a duplicate of the HP report within five seconds.
    feed(&mut tr, &[death(at(23.0), ME)]);
    assert_eq!(actor(&snap(&tr), "Me").deaths, 3);
}

#[test]
fn wipe_from_hp_zero_needs_every_roster_member_identified() {
    let mut tr = Tracker::default();
    feed(
        &mut tr,
        &[
            zone(at(-10.0), MAP_INSTANCE),
            identity(at(-9.0), ME, "Me", 11, true),
            identity(at(-9.0), ALLY, "Ally", 17, false),
            party(at(-8.0), &[("Me", 11), ("Ally", 17), ("Third", 12)]),
            spawn(at(-5.0), BOSS, BOSS_CODE),
            hit(at(0.0), ME, BOSS, STRIKE, 1000),
            hit(at(1.0), ALLY, BOSS, STRIKE, 1000),
            hp(at(10.0), ME, 0),
            hp(at(11.0), ALLY, 0),
        ],
    );
    assert!(snap(&tr).active, "Third has no id yet and may be alive");
    feed(&mut tr, &[identity(at(12.0), 3, "Third", 12, false), hp(at(13.0), 3, 0)]);
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Wipe));
    assert_eq!(s.deaths.len(), 3);
    assert_eq!(s.duration_ms, 1000, "the duration runs to the last hit");
}

#[test]
fn boss_hp_reset_right_after_a_player_death_is_a_wipe() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), hit(at(1.0), ALLY, BOSS, STRIKE, 1000), hp(at(1.5), BOSS, 500_000)]);
    feed(&mut tr, &[hp(at(2.0), ME, 0), hp(at(2.1), BOSS, 1_000_000)]);
    let s = snap(&tr);
    assert_eq!(s.end_reason, Some(EndReason::Wipe));
    let boss = s.boss.as_ref().unwrap();
    assert!(!boss.dead);
    assert_eq!(boss.hp_pct, Some(1.0));
    assert_eq!(s.deaths.len(), 1);
    assert_eq!(s.duration_ms, 1000);
    // The next pull is attempt two.
    feed(&mut tr, &[hit(at(30.0), ME, BOSS, STRIKE, 10)]);
    assert_eq!(snap(&tr).id, 2);
}

#[test]
fn effect_entities_are_npcs_unless_owned() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1000), effect(at(0.5), 300, 2_999_999, None)]);
    feed(&mut tr, &[hit(at(1.0), ME, 300, STRIKE, 200), hit(at(2.0), 300, ME, NPC_SKILL, 50)]);
    let s = snap(&tr);
    assert_eq!(s.targets.len(), 2, "an unowned 0x1C add is a target");
    let me = actor(&s, "Me");
    assert_eq!(me.damage, 1000);
    assert_eq!(me.damage_all_targets, 1200);
    assert_eq!(me.damage_taken, 50, "and its hits on players are damage taken");
    assert_eq!(s.actors.len(), 1);
    // An owned one is the owner's summon.
    feed(&mut tr, &[effect(at(3.0), 301, 2_999_999, Some(ALLY)), hit(at(4.0), 301, BOSS, NPC_SKILL, 100)]);
    let s = snap(&tr);
    assert_eq!(actor(&s, "Ally").damage, 100);
    assert_eq!(s.actors.len(), 2);
}

#[test]
fn spirit_skill_value_records_make_an_unknown_id_a_summon_but_casts_do_not() {
    let mut tr = Tracker::default();
    feed(
        &mut tr,
        &[
            zone(at(-10.0), MAP_INSTANCE),
            identity(at(-9.0), ME, "Me", 16, true),
            party(at(-8.0), &[("Me", 16)]),
            spawn(at(-5.0), BOSS, BOSS_CODE),
            hit(at(0.0), ME, BOSS, COMBUSTION, 1000),
            // An unidentified Elementalist announces its summon; its spirit then attacks.
            cast(at(0.5), 60, 60, SUMMON_WATER_SPIRIT),
            hit(at(1.0), 61, BOSS, FIRE_SPIRIT_ATTACK, 400),
            hit(at(1.5), 62, BOSS, 100_014, 300),
        ],
    );
    let s = snap(&tr);
    assert_eq!(s.actors.len(), 1, "the orphan spirit and pet fold into the only Elementalist: {:?}", s.actors.iter().map(|a| &a.name).collect::<Vec<_>>());
    assert_eq!(actor(&s, "Me").damage, 1700);
    // The announcing id is still unknown: its first value record with a player skill makes it a player.
    feed(&mut tr, &[hit(at(2.0), 60, BOSS, COMBUSTION, 250)]);
    let s = snap(&tr);
    assert_eq!(s.actors.len(), 2);
    assert_eq!(actor(&s, "#60").damage, 250);
    assert_eq!(actor(&s, "#60").class_name.as_deref(), Some("Elementalist"));
    assert_eq!(actor(&s, "Me").damage, 1700, "a second Elementalist does not claim the spirits (they stay with the named one)");
}

#[test]
fn unnamed_player_takes_the_roster_name_of_its_class_inside_an_instance() {
    let setup = |map: Option<u32>| {
        let mut tr = Tracker::default();
        if let Some(map) = map {
            feed(&mut tr, &[zone(at(-10.0), map)]);
        }
        feed(
            &mut tr,
            &[
                identity(at(-9.0), ME, "Me", 11, true),
                party(at(-8.0), &[("Me", 11), ("Ally", 17)]),
                spawn(at(-5.0), BOSS, BOSS_CODE),
                hit(at(0.0), ME, BOSS, STRIKE, 1000),
                hit(at(1.0), 9, BOSS, CLERIC_SKILL, 500),
            ],
        );
        tr
    };
    let mut tr = setup(Some(MAP_INSTANCE));
    let s = snap(&tr);
    let ally = actor(&s, "Ally");
    assert_eq!(ally.id, 9);
    assert_eq!(ally.damage, 500);
    assert!(ally.is_party);
    assert_eq!(ally.class_name.as_deref(), Some("Cleric"));
    assert_eq!(ally.level, Some(50));
    // A second unnamed Cleric makes it ambiguous.
    feed(&mut tr, &[hit(at(2.0), 10, BOSS, CLERIC_SKILL, 300)]);
    let s = snap(&tr);
    assert!(actor(&s, "#9").damage == 500 && actor(&s, "#10").damage == 300);
    // The real identity under another id takes the name back for good.
    let mut tr = setup(Some(MAP_INSTANCE));
    feed(&mut tr, &[identity(at(2.0), 11, "Ally", 17, false), hit(at(3.0), 11, BOSS, CLERIC_SKILL, 50)]);
    let s = snap(&tr);
    assert_eq!(actor(&s, "Ally").id, 11);
    assert_eq!(actor(&s, "Ally").damage, 50);
    assert_eq!(actor(&s, "#9").damage, 500);
    // Before any map load the meter started mid session and the fallback applies as well.
    assert_eq!(actor(&snap(&setup(None)), "Ally").id, 9);
    // In the open world a stranger of the same class must not take the name.
    assert_eq!(actor(&snap(&setup(Some(MAP_OPEN))), "#9").damage, 500);
}

#[test]
fn cast_records_of_one_skill_within_100_ms_are_one_cast() {
    let mut tr = boss_tracker();
    feed(
        &mut tr,
        &[
            hit(at(0.0), ME, BOSS, STRIKE, 1000),
            cast(at(1.0), ME, BOSS, STRIKE),
            cast(at(1.0), ME, TRASH, STRIKE),
            cast(at(1.05), ME, ALLY, STRIKE),
            cast(at(1.3), ME, BOSS, STRIKE),
            cast(at(1.3), ME, ME, 11_030_000),
            cast(at(1.3), ALLY, ME, STRIKE),
        ],
    );
    let s = snap(&tr);
    let me = actor(&s, "Me");
    assert_eq!(me.casts, 3, "two uses of Keen Strike and one of another skill");
    assert_eq!(me.skills.iter().find(|k| k.skill_id == STRIKE_BASE).unwrap().casts, 2);
    assert_eq!(actor(&s, "Ally").casts, 1);
}

/// Replays the local captures (git ignored, skipped when absent) through the pipeline and the tracker,
/// with the time rules driven by the capture clock, and prints what it found (`--nocapture`).
#[test]
fn replays_local_captures() {
    use crate::net::LinkType;
    use crate::pipeline::{Pipeline, DEFAULT_PORT};
    use pcap_file::pcap::PcapReader;
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures");
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<_> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "pcap").unwrap_or(false))
        .collect();
    files.sort();
    for path in files {
        let bytes = std::fs::read(&path).expect("read capture");
        let mut reader = PcapReader::new(std::io::Cursor::new(bytes)).expect("pcap header");
        let link = LinkType::from_pcap(u32::from(reader.header().datalink)).expect("known link type");
        let mut pipe = Pipeline::new(vec![DEFAULT_PORT]);
        let mut tr = Tracker::default();
        let mut events = Vec::new();
        let mut last_tick = 0u64;
        let mut finished = Vec::new();
        while let Some(pkt) = reader.next_packet() {
            let Ok(pkt) = pkt else { break };
            let t_ms = pkt.timestamp.as_millis() as u64;
            pipe.push(link, t_ms, &pkt.data, &mut |ev| events.push(ev));
            tr.set_self_hint(pipe.parser.stats.hint_self_id);
            for ev in events.drain(..) {
                tr.push(&ev);
            }
            if t_ms.saturating_sub(last_tick) >= 250 {
                tr.tick(t_ms);
                last_tick = t_ms;
                finished.extend(tr.drain_finished());
            }
        }
        tr.tick(last_tick + 600_000);
        finished.extend(tr.drain_finished());
        eprintln!("{}: {} encounters filed, {:?}", path.display(), finished.len(), tr.stats);
        for s in &finished {
            eprint!("{}", describe(s));
            for a in &s.actors {
                assert!(a.max_hit < 100_000_000, "phantom hit {} by {} in {}", a.max_hit, a.name, path.display());
                assert!(a.is_player);
            }
            let boss = s.boss.as_ref().expect("a filed encounter has a main target");
            let on_main: u64 = s.actors.iter().map(|a| a.damage).sum();
            assert!(on_main <= boss.damage_taken, "player damage on the main target never exceeds what it took");
        }
    }
}

#[test]
fn snapshot_is_fast_for_a_full_party() {
    let mut tr = Tracker::default();
    let mut events = vec![zone(at(-10.0), MAP_INSTANCE), spawn(at(-5.0), BOSS, BOSS_CODE)];
    let names = ["A", "B", "C", "D", "E", "F"];
    for (i, n) in names.iter().enumerate() {
        events.push(identity(at(-9.0), 10 + i as u32, n, 11 + (i as u32 % 9), i == 0));
    }
    events.push(party(at(-8.0), &names.iter().enumerate().map(|(i, n)| (*n, 11 + (i as u32 % 9))).collect::<Vec<_>>()));
    // Forty Gladiator skill bases that are not heal skills (11120000 and 11380000 are, and a heal skill
    // on a mob is dropped rather than counted).
    let skills: Vec<u32> = (0u32..).map(|k| 11_000_000 + k * 10_000).filter(|s| !GameData::get().is_heal_skill(*s)).take(40).collect();
    for step in 0..6000u32 {
        let actor = 10 + step % 6;
        let skill = skills[((step / 6) % 40) as usize];
        events.push(hit(at(step as f64 * 0.1), actor, BOSS, skill, 1000 + step as u64));
    }
    feed(&mut tr, &events);
    let started = std::time::Instant::now();
    let mut s = None;
    for _ in 0..20 {
        s = tr.snapshot();
    }
    let elapsed = started.elapsed();
    let s = s.unwrap();
    assert_eq!(s.actors.len(), 6);
    assert_eq!(s.actors[0].skills.len(), 40);
    eprintln!("20 snapshots in {elapsed:?}");
    assert!(elapsed < std::time::Duration::from_millis(200), "snapshot too slow: {elapsed:?}");
}

/// A Templar band code that spawned mobs fired 2 to 10 times a session in the captures.
const MOB_CLASS_CODE: u32 = 12_070_007;
/// A mob whose spawn the meter never saw: it was in view before the meter started.
const UNSEEN_MOB: u32 = 900;

#[test]
fn a_mob_whose_spawn_was_missed_stays_a_target_when_it_fires_a_class_skill() {
    // aion2dps encounter 827 (25 October 2026): five players on #22503, which then fired a Templar band
    // code and was taken for a player; every later hit was dropped and the meter went quiet after 7 s.
    let mut tr = boss_tracker();
    feed(
        &mut tr,
        &[
            hit(at(0.0), ME, UNSEEN_MOB, STRIKE, 1_000),
            hp(at(0.0), UNSEEN_MOB, 99_000),
            cast(at(1.0), UNSEEN_MOB, ME, MOB_CLASS_CODE),
            hit(at(1.5), UNSEEN_MOB, ME, MOB_CLASS_CODE, 500),
            hit(at(2.0), ME, UNSEEN_MOB, STRIKE, 1_000),
            hit(at(3.0), ALLY, UNSEEN_MOB, STRIKE, 700),
        ],
    );
    let s = snap(&tr);
    assert_eq!(actor(&s, "Me").damage, 2_000, "every hit on the mob counts");
    assert_eq!(actor(&s, "Ally").damage, 700);
    assert!(s.actors.iter().all(|a| a.name != "#900"), "the mob is no player row");
    assert_eq!(actor(&s, "Me").damage_taken, 500, "its class coded hit is damage taken");
}

#[test]
fn an_id_with_more_hp_than_any_player_is_a_mob_even_after_a_class_skill() {
    let mut tr = boss_tracker();
    feed(&mut tr, &[hit(at(0.0), ME, BOSS, STRIKE, 1_000), cast(at(0.5), UNSEEN_MOB, ME, MOB_CLASS_CODE)]);
    assert!(snap(&tr).actors.iter().any(|a| a.name == "#900"), "taken for a player by its class skill");
    feed(&mut tr, &[hp_update(at(1.0), UNSEEN_MOB, 11_627_619, 15_501_964), hit(at(2.0), ME, UNSEEN_MOB, STRIKE, 1_000)]);
    let s = snap(&tr);
    assert!(s.actors.iter().all(|a| a.name != "#900"), "15.5 M HP is no player");
    assert_eq!(actor(&s, "Me").damage_all_targets, 2_000, "and the hit on it counts");
    assert_eq!(tr.stats.inferred_players_removed, 1);
}

#[test]
fn hits_on_players_stay_out_of_the_meter() {
    // PvP: a named enemy, and an unnamed one taken for a player by its class skill with player HP.
    let mut tr = boss_tracker();
    feed(
        &mut tr,
        &[
            identity(at(-3.0), 300, "Enemy", 12, false),
            hit(at(0.0), ME, BOSS, STRIKE, 1_000),
            hit(at(0.5), ME, 300, STRIKE, 5_000),
            cast(at(0.6), 301, ME, MOB_CLASS_CODE),
            hp(at(0.7), 301, 25_000),
            hit(at(1.0), ME, 301, STRIKE, 5_000),
        ],
    );
    let s = snap(&tr);
    assert_eq!(actor(&s, "Me").damage_all_targets, 1_000, "only the boss hit counts");
    assert!(actor(&s, "#301").is_player);
}
