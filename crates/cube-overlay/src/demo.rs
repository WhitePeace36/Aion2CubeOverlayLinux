//! Demo data: a scripted cube hunt in Verteron, played through the real tracker so the overlay can be seen
//! without the game. Adapted from aion2-tui's cube demo, with cubes on ledges and in hollows so the height
//! display has something to show. Each round lasts 75 seconds:
//!
//! - 1.5 s  cube C appears 56 m north, 9 m below you in a hollow (from round 2: "still there")
//! - 4 s    cube A appears 92 m south east, 4 m up a slope
//! - 14 s   cube B spawns 82 m south on a ledge 23 m above you; another player opens it at 20 s
//! - 26 s   cube C leaves view as you walk away
//! - 33 s   you open cube A
//! - 72 s   you enter a dungeon; the next round starts back in Verteron

use crate::source::{Health, Source, Status};
use cube_core::cubes::{CubeBook, KEY_ITEM};
use cube_core::geo::Vec3;
use cube_core::parser::{EntityId, Event, PositionSource, RemoveReason};
use cube_core::snapshot::{Alert, RadarSnapshot};
use cube_core::spots::Spot;
use cube_core::tracker::{Tracker, TrackerConfig};

pub const ROUND_MS: u64 = 75_000;
const VERTERON: u32 = 1010;
const DUNGEON: u32 = 600_072;
const ME: EntityId = 9_180;
const OTHER_PLAYER: EntityId = 5_709;
const CUBE: u32 = 1_200_039;
const CUBE_ALT: u32 = 1_200_041;
const NODE: u32 = 1_500_041;
/// The server clock runs this far ahead of the PC clock, to show that ages use the server's.
const SERVER_AHEAD_MS: u64 = 1_500;

const START: Vec3 = Vec3 { x: -62_000.0, y: 118_000.0, z: 15_000.0 };
const AT_A: Vec3 = Vec3 { x: -54_150.0, y: 122_500.0, z: 15_400.0 };
const END: Vec3 = Vec3 { x: -46_000.0, y: 130_000.0, z: 15_600.0 };
const CUBE_A: Vec3 = Vec3 { x: -54_000.0, y: 122_500.0, z: 15_400.0 };
const CUBE_B: Vec3 = Vec3 { x: -57_000.0, y: 128_000.0, z: 17_400.0 };
const CUBE_C: Vec3 = Vec3 { x: -61_000.0, y: 112_500.0, z: 14_100.0 };

fn lerp(a: Vec3, b: Vec3, f: f32) -> Vec3 {
    let f = f.clamp(0.0, 1.0);
    Vec3::new(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f, a.z + (b.z - a.z) * f)
}

/// Where the character is `s` seconds into a round: walk to cube A, open it, walk on.
fn me_at(s: f32) -> Vec3 {
    match s {
        s if s <= 33.0 => lerp(START, AT_A, s / 33.0),
        s if s <= 38.0 => AT_A,
        s => lerp(AT_A, END, (s - 38.0) / 34.0),
    }
}

fn script(base: u64, round: u32, c_created: u64) -> Vec<Event> {
    let at = |s: f64| base + (s * 1000.0) as u64;
    let id = |n: u32| 40_000 + round * 10 + n;
    let spawn = |t_ms, id, template, pos, created_ms| Event::ObjectSpawn { t_ms, id, kind: 4, template, pos, heading: 0.0, state: 1, created_ms };
    let mut events = vec![
        Event::ZoneChange { t_ms: at(0.0), map_id: VERTERON },
        Event::Position { t_ms: at(0.0), entity: None, pos: START, source: PositionSource::MapLoad },
        Event::SelfInfo { t_ms: at(0.5), id: ME, name: "Seraphel".into(), server: 2314, level: 45 },
        spawn(at(1.5), id(3), CUBE_ALT, CUBE_C, Some(c_created)),
        spawn(at(4.0), id(1), CUBE, CUBE_A, Some(at(4.0) + SERVER_AHEAD_MS - 160_000)),
        spawn(at(8.0), id(5), NODE, Vec3::new(-59_000.0, 117_000.0, 15_100.0), Some(at(8.0) - 3_600_000)),
        spawn(at(14.0), id(2), CUBE, CUBE_B, Some(at(13.0) + SERVER_AHEAD_MS)),
        Event::OpenStart { t_ms: at(20.0), object: id(2), actor: OTHER_PLAYER },
        Event::OpenDone { t_ms: at(24.25), object: id(2), actor: OTHER_PLAYER, count: 1, item: KEY_ITEM },
        Event::ObjectRemoved { t_ms: at(24.9), id: id(2), reason: RemoveReason::Opened },
        Event::ObjectRemoved { t_ms: at(26.0), id: id(3), reason: RemoveReason::LeftView },
        Event::OpenStart { t_ms: at(33.0), object: id(1), actor: ME },
        Event::OpenDone { t_ms: at(37.25), object: id(1), actor: ME, count: 1, item: KEY_ITEM },
        Event::ObjectRemoved { t_ms: at(38.0), id: id(1), reason: RemoveReason::Opened },
        Event::ZoneChange { t_ms: at(72.0), map_id: DUNGEON },
    ];
    // Buff records carry your position about every second while fighting; the heartbeat carries the server clock.
    for s in 1..72 {
        events.push(Event::Position { t_ms: at(s as f64), entity: Some(ME), pos: me_at(s as f32), source: PositionSource::Buff });
    }
    for s in 0..75 {
        events.push(Event::Heartbeat { t_ms: at(s as f64), server_ms: at(s as f64) + SERVER_AHEAD_MS });
    }
    events.sort_by_key(Event::t_ms);
    events
}

/// Spots "saved by earlier sessions", so the radar shows some history.
fn earlier_spots(now_ms: u64) -> Vec<Spot> {
    let day = 86_400_000;
    [(-58_500.0, 113_800.0, 15_050.0, 3), (-59_200.0, 121_600.0, 15_900.0, 2), (-52_400.0, 126_100.0, 16_400.0, 1), (-64_800.0, 121_900.0, 14_700.0, 4)]
        .into_iter()
        .map(|(x, y, z, sightings)| Spot {
            map_id: VERTERON,
            template: CUBE,
            pos: Vec3::new(x, y, z),
            first_seen_ms: now_ms - 6 * day,
            last_seen_ms: now_ms - day,
            sightings,
            opened_by_you: sightings / 2,
        })
        .collect()
}

pub struct Demo {
    tracker: Tracker,
    base_ms: u64,
    round: u32,
    c_created: u64,
    events: Vec<Event>,
    next: usize,
    now_ms: u64,
    alerts: Vec<Alert>,
}

impl Demo {
    pub fn new(now_ms: u64, book: CubeBook) -> Demo {
        let c_created = now_ms + SERVER_AHEAD_MS - 400_000;
        let mut tracker = Tracker::new(TrackerConfig::default(), book);
        tracker.set_spots(earlier_spots(now_ms));
        Demo { tracker, base_ms: now_ms, round: 0, c_created, events: script(now_ms, 0, c_created), next: 0, now_ms, alerts: Vec::new() }
    }
}

impl Source for Demo {
    fn step(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        while now_ms >= self.base_ms + ROUND_MS {
            for e in &self.events[self.next..] {
                self.tracker.push(e);
            }
            self.base_ms += ROUND_MS;
            self.round += 1;
            self.events = script(self.base_ms, self.round, self.c_created);
            self.next = 0;
        }
        while let Some(e) = self.events.get(self.next).filter(|e| e.t_ms() <= now_ms) {
            self.tracker.push(e);
            self.next += 1;
        }
        self.tracker.tick(now_ms);
        self.alerts.extend(self.tracker.drain_alerts());
    }

    fn snapshot(&self) -> RadarSnapshot {
        self.tracker.snapshot(self.now_ms)
    }

    fn drain_alerts(&mut self) -> Vec<Alert> {
        std::mem::take(&mut self.alerts)
    }

    fn status(&self) -> Status {
        Status { text: format!("demo data, round {}", self.round + 1), health: Health::Good }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cube_core::snapshot::{AlertKind, CubeState};

    const START_MS: u64 = 1_791_722_000_000;

    #[test]
    fn a_round_raises_every_kind_of_alert() {
        let mut demo = Demo::new(START_MS, CubeBook::default());
        let mut kinds = Vec::new();
        for i in 0..=(2 * ROUND_MS / 250) {
            demo.step(START_MS + i * 250);
            kinds.extend(demo.drain_alerts().into_iter().map(|a| a.kind));
        }
        use AlertKind::*;
        assert_eq!(kinds, vec![Appeared, Appeared, Appeared, OpenedByOther, OpenedByYou, StillThere, Appeared, Appeared, OpenedByOther, OpenedByYou]);
    }

    #[test]
    fn the_cubes_sit_above_and_below_you() {
        let mut demo = Demo::new(START_MS, CubeBook::default());
        demo.step(START_MS + 15_000);
        let s = demo.snapshot();
        let height = |template, pos: Vec3| s.cubes.iter().find(|c| c.template == template && c.pos == pos).and_then(|c| c.height_m).unwrap();
        assert!(height(CUBE, CUBE_B) > 15.0, "cube B is on a ledge");
        assert!(height(CUBE_ALT, CUBE_C) < -5.0, "cube C is in a hollow");
        demo.step(START_MS + 27_000);
        let c = demo.snapshot().cubes.into_iter().find(|c| c.template == CUBE_ALT).unwrap();
        assert_eq!(c.state, CubeState::OutOfView);
    }
}
