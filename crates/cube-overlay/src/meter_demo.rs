//! Demo fights for the meter, played through the real meter tracker so the whole path is exercised.
//!
//! Five players fight Awakened Alturadon (8 000 000 HP), and the fight ends when it dies (about 85 s), then
//! 12 s of pause before the next round with a fresh boss:
//! - Seraphel (you, Sorcerer) switches to a Draconute Ranger add from 38 s until it dies at about 48 s;
//! - Kaelith (Gladiator) bursts at more than twice his usual damage from 20 s to 30 s;
//! - Mirro (Ranger) joins at 12 s;
//! - Vexa (Assassin) dies at 50 s and is back at 62 s.

use meter_core::parser::{DeathKind, EntityId, Event, HitFlags};

const BOSS_CODE: u32 = 2_300_243;
const ADD_CODE: u32 = 2_000_002;
const BOSS_HP: u64 = 8_000_000;
const ADD_HP: u64 = 650_000;
const PAUSE_MS: u64 = 12_000;
const STEP_MS: u64 = 100;
/// A dungeon map, so the meter uses the instance idle timeout.
const MAP: u32 = 600_143;

pub const SELF_ID: EntityId = 7_001;

struct Player {
    id: EntityId,
    name: &'static str,
    class_id: u32,
    /// Damage per second on the boss.
    dps: f64,
    /// Milliseconds between hits.
    every_ms: u64,
    skills: [u32; 4],
}

const PLAYERS: [Player; 5] = [
    Player { id: SELF_ID, name: "Seraphel", class_id: 15, dps: 21_000.0, every_ms: 900, skills: [15_010_000, 15_020_000, 15_030_000, 15_050_000] },
    Player { id: 7_002, name: "Kaelith", class_id: 11, dps: 19_000.0, every_ms: 600, skills: [11_010_000, 11_020_000, 11_030_000, 11_040_000] },
    Player { id: 7_003, name: "Mirro", class_id: 14, dps: 23_000.0, every_ms: 700, skills: [14_010_000, 14_020_000, 14_030_000, 14_040_000] },
    Player { id: 7_004, name: "Vexa", class_id: 13, dps: 20_000.0, every_ms: 500, skills: [13_010_000, 13_020_000, 13_030_000, 13_040_000] },
    Player { id: 7_005, name: "Orin", class_id: 12, dps: 11_000.0, every_ms: 800, skills: [12_010_000, 12_020_000, 12_030_000, 12_040_000] },
];

/// A small deterministic generator, so tests see the same fight every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub struct MeterDemo {
    /// Start of the current round.
    round_ms: u64,
    round: u32,
    /// Simulated up to here.
    t_ms: u64,
    boss_dealt: u64,
    add_dealt: u64,
    add_spawned: bool,
    add_dead: bool,
    boss_dead_ms: Option<u64>,
    next_hit: [u64; 5],
    last_hp_ms: u64,
    rng: Rng,
}

impl MeterDemo {
    pub fn new(now_ms: u64) -> MeterDemo {
        let mut d = MeterDemo {
            round_ms: now_ms,
            round: 0,
            t_ms: now_ms,
            boss_dealt: 0,
            add_dealt: 0,
            add_spawned: false,
            add_dead: false,
            boss_dead_ms: None,
            next_hit: [0; 5],
            last_hp_ms: 0,
            rng: Rng(0x9E37_79B9_7F4A_7C15),
        };
        d.start_round(now_ms);
        d
    }

    fn boss(&self) -> EntityId {
        60_000 + self.round * 10
    }

    fn add(&self) -> EntityId {
        60_001 + self.round * 10
    }

    fn start_round(&mut self, at: u64) {
        self.round_ms = at;
        self.boss_dealt = 0;
        self.add_dealt = 0;
        self.add_spawned = false;
        self.add_dead = false;
        self.boss_dead_ms = None;
        self.last_hp_ms = at;
        for (i, p) in PLAYERS.iter().enumerate() {
            self.next_hit[i] = at + 1_000 + (i as u64 * 137) % p.every_ms;
        }
    }

    /// Events from the last call up to `now_ms`, in time order.
    pub fn advance(&mut self, now_ms: u64, out: &mut Vec<Event>) {
        while self.t_ms + STEP_MS <= now_ms {
            let t = self.t_ms + STEP_MS;
            self.slice(t, out);
            self.t_ms = t;
        }
    }

    fn slice(&mut self, t: u64, out: &mut Vec<Event>) {
        let s = t.saturating_sub(self.round_ms);
        if let Some(dead) = self.boss_dead_ms {
            if t >= dead + PAUSE_MS {
                self.round += 1;
                self.start_round(t);
            }
            return;
        }
        if s == STEP_MS {
            out.push(Event::ZoneChange { t_ms: t, map_id: MAP });
            for p in &PLAYERS {
                out.push(Event::Identity { t_ms: t, id: p.id, name: p.name.into(), server: 1001, job: 0, class_id: p.class_id, level: 45, is_self: p.id == SELF_ID });
            }
            out.push(Event::NpcSpawn { t_ms: t, id: self.boss(), npc_code: BOSS_CODE, kind: 0x0C, cur_hp: Some(BOSS_HP), max_hp: Some(BOSS_HP), owner: None });
        }
        if !self.add_spawned && s >= 37_000 {
            self.add_spawned = true;
            out.push(Event::NpcSpawn { t_ms: t, id: self.add(), npc_code: ADD_CODE, kind: 0x0C, cur_hp: Some(ADD_HP), max_hp: Some(ADD_HP), owner: None });
        }
        for (i, p) in PLAYERS.iter().enumerate() {
            if t < self.next_hit[i] {
                continue;
            }
            self.next_hit[i] = t + p.every_ms + (self.rng.next() * 200.0) as u64;
            let joined = p.id != 7_003 || s >= 12_000;
            let dead = p.id == 7_004 && (50_000..62_000).contains(&s);
            if !joined || dead {
                continue;
            }
            let on_add = p.id == SELF_ID && self.add_spawned && !self.add_dead && s >= 38_000;
            let burst = if p.id == 7_002 && (20_000..30_000).contains(&s) { 2.3 } else { 1.0 };
            let crit = self.rng.next() < 0.25;
            let spread = 0.6 + self.rng.next() * 0.8;
            let amount = (p.dps * p.every_ms as f64 / 1000.0 * burst * spread * if crit { 1.8 } else { 1.0 }) as u64;
            let skill = p.skills[(self.rng.next() * 4.0) as usize % 4];
            let flags = HitFlags { crit, back: self.rng.next() < 0.3, ..HitFlags::default() };
            let target = if on_add { self.add() } else { self.boss() };
            out.push(Event::Damage { t_ms: t, actor: p.id, target, skill, amount, flags, extra_hits: 0 });
            if on_add {
                self.add_dealt += amount;
                if self.add_dealt >= ADD_HP {
                    self.add_dead = true;
                    out.push(Event::RemainHp { t_ms: t, id: self.add(), cur_hp: 0, max_hp: Some(ADD_HP) });
                    out.push(Event::Death { t_ms: t, id: self.add(), kind: DeathKind::Died });
                }
            } else {
                self.boss_dealt += amount;
            }
        }
        if t >= self.last_hp_ms + 500 || self.boss_dealt >= BOSS_HP {
            self.last_hp_ms = t;
            let left = BOSS_HP.saturating_sub(self.boss_dealt);
            out.push(Event::RemainHp { t_ms: t, id: self.boss(), cur_hp: left, max_hp: Some(BOSS_HP) });
            if left == 0 {
                out.push(Event::Death { t_ms: t, id: self.boss(), kind: DeathKind::Died });
                self.boss_dead_ms = Some(t);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meter_core::combat::Tracker;
    use meter_core::gamedata::GameData;

    #[test]
    fn the_skills_are_player_damage_skills() {
        let data = GameData::get();
        for p in &PLAYERS {
            for s in p.skills {
                assert!(!data.is_heal_skill(s), "{s} is a heal");
                assert_eq!(data.class_from_skill(s), Some(p.class_id), "{s}");
            }
        }
        assert!(data.is_boss(BOSS_CODE));
    }

    #[test]
    fn a_round_is_one_boss_fight_with_every_player() {
        let start = 1_792_000_000_000;
        let mut demo = MeterDemo::new(start);
        let mut tracker = Tracker::default();
        let mut events = Vec::new();
        demo.advance(start + 45_000, &mut events);
        for e in events.drain(..) {
            tracker.push(&e);
        }
        let snap = tracker.snapshot().expect("a fight is on");
        assert!(snap.active);
        assert_eq!(snap.boss.as_ref().unwrap().name, "Awakened Alturadon");
        assert_eq!(snap.actors.len(), 5);
        let me = snap.actors.iter().find(|a| a.is_self).unwrap();
        assert_eq!(me.name, "Seraphel");
        assert_eq!(me.targets.len(), 2, "the boss and the add");

        demo.advance(start + 140_000, &mut events);
        let boss = demo_boss(0);
        let death = events.iter().position(|e| matches!(e, Event::Death { id, .. } if *id == boss)).expect("the boss dies");
        for e in &events[..=death] {
            tracker.push(e);
        }
        let done = tracker.snapshot().unwrap();
        assert!(!done.active, "the fight ended with the boss");
        assert!(done.duration_ms > 70_000 && done.duration_ms < 110_000, "{}", done.duration_ms);
        // The next round starts after the pause with a new boss.
        for e in &events[death + 1..] {
            tracker.push(e);
        }
        let next = tracker.snapshot().unwrap();
        assert!(next.active && next.boss.as_ref().unwrap().id == demo_boss(1));
    }

    fn demo_boss(round: u32) -> EntityId {
        60_000 + round * 10
    }
}
