//! Turns parser events into the cube radar: which cubes are around, where you are, and what to alert on.
//!
//! Rules (docs/PROTOCOL.md has the evidence):
//!
//! - The server sends a cube's spawn when you come within about 100 m of it, or one second after it spawns
//!   inside that range, standing still or not. So a spawn with a cube template is the "nearby" alert; no
//!   position is needed for it.
//! - One alert per cube: the id is the key. A cube sent again after leaving view is the same cube; a spawn
//!   under an id whose cube was opened or gone is a new one (ids are reused).
//! - Ids are reissued on every map change, so a load of a different map clears everything. A load of the same
//!   map is a teleport inside the zone and keeps the cubes. Cubes left behind are remembered by template, spawn
//!   time and position: back from a dungeon, the same cube is "still there", not a new one (the 2026-10-11
//!   captures show one cube re-sent after four dungeon runs).
//! - Opening is `38 36` (started) then `3a 36` (done, with the key item) then `35 36` reason 04 (removed). A cube
//!   that left view and is not sent again although you came within 80 m was opened by someone meanwhile.
//! - Your entity id comes from the self record. Until one arrives (the tracker started mid zone) it is guessed
//!   as the most frequent caster of the buff records that carry a position.
//! - A key opening of an object whose template is not in the book teaches the book that template.

use crate::cubes::{CubeBook, CubeTemplate, Evidence, KEY_ITEM};
use crate::gamedata::GameData;
use crate::geo::{Vec3, UNITS_PER_METRE};
use crate::parser::{EntityId, Event, PositionSource, RemoveReason};
use crate::snapshot::{Alert, AlertKind, Character, Counts, CubeState, CubeView, Fix, LogEntry, LogKind, RadarSnapshot, TrailPoint};
use crate::spots::{Spot, SpotBook};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct TrackerConfig {
    /// A fix older than this makes distances an estimate.
    pub fix_fresh_ms: u64,
    /// Opened and gone cubes stay in the list this long.
    pub keep_gone_ms: u64,
    /// Cubes that left view stay in the list this long.
    pub keep_out_of_view_ms: u64,
    /// Within this planar distance a cube that left view would be sent again if it were still there.
    pub gone_check_units: f32,
    /// How long the server gets to send it again before the cube counts as gone.
    pub gone_check_ms: u64,
    /// Buff fixes an entity needs before it is guessed to be you.
    pub self_votes: u32,
    pub max_log: usize,
    /// Non cube objects remembered for learning templates; the map is cleared when it grows past this.
    pub max_objects: usize,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            fix_fresh_ms: 10_000,
            keep_gone_ms: 60_000,
            keep_out_of_view_ms: 30 * 60_000,
            gone_check_units: 8_000.0,
            gone_check_ms: 3_000,
            self_votes: 3,
            max_log: 1_000,
            max_objects: 20_000,
        }
    }
}

#[derive(Debug, Clone)]
struct Cube {
    id: EntityId,
    template: u32,
    pos: Vec3,
    created_ms: Option<u64>,
    first_seen_ms: u64,
    state: CubeState,
    state_ms: u64,
    opener: Option<EntityId>,
    /// When a fix first put you within the gone check distance while the cube was out of view.
    near_since_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct ObjectSeen {
    template: u32,
    pos: Vec3,
}

/// A cube still there when you left its map.
#[derive(Debug, Clone, Copy)]
struct LeftBehind {
    map_id: u32,
    template: u32,
    pos: Vec3,
    created_ms: u64,
    first_seen_ms: u64,
}

/// A spawn this close to a remembered cube with the same template and spawn time is that cube.
const SAME_CUBE_UNITS: f32 = 100.0;
const MAX_LEFT_BEHIND: usize = 64;
/// A fix this far (planar, or this much up or down) from the last path point adds a point to the path.
const TRAIL_STEP_UNITS: f32 = 150.0;
const TRAIL_STEP_HEIGHT: f32 = 300.0;
const MAX_TRAIL: usize = 5_000;

pub struct Tracker {
    config: TrackerConfig,
    book: CubeBook,
    map_id: Option<u32>,
    character: Option<Character>,
    self_id: Option<EntityId>,
    votes: HashMap<EntityId, u32>,
    me: Option<Fix>,
    cubes: HashMap<EntityId, Cube>,
    objects: HashMap<EntityId, ObjectSeen>,
    left_behind: Vec<LeftBehind>,
    spots: SpotBook,
    /// Your path on the current map, from the position fixes.
    trail: Vec<TrailPoint>,
    log: Vec<LogEntry>,
    alerts: Vec<Alert>,
    learned: Vec<CubeTemplate>,
    counts: Counts,
    /// Server clock minus capture clock, from the latest heartbeat.
    server_offset_ms: Option<i64>,
    last_seen_ms: u64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new(TrackerConfig::default(), CubeBook::default())
    }
}

impl Tracker {
    pub fn new(config: TrackerConfig, book: CubeBook) -> Self {
        Self {
            config,
            book,
            map_id: None,
            character: None,
            self_id: None,
            votes: HashMap::new(),
            me: None,
            cubes: HashMap::new(),
            objects: HashMap::new(),
            left_behind: Vec::new(),
            spots: SpotBook::default(),
            trail: Vec::new(),
            log: Vec::new(),
            alerts: Vec::new(),
            learned: Vec::new(),
            counts: Counts::default(),
            server_offset_ms: None,
            last_seen_ms: 0,
        }
    }

    pub fn book(&self) -> &CubeBook {
        &self.book
    }

    /// Starts from spots saved by an earlier run.
    pub fn set_spots(&mut self, spots: Vec<Spot>) {
        self.spots = SpotBook::new(spots);
    }

    /// Every spot when one was added or changed since the last call, for saving.
    pub fn take_spots_changed(&mut self) -> Option<Vec<Spot>> {
        self.spots.take_changed()
    }

    /// Records a cube at its spot on the current map.
    fn spot_seen(&mut self, template: u32, pos: Vec3, t_ms: u64, new_cube: bool) {
        if let Some(map_id) = self.map_id {
            self.spots.sighting(map_id, template, pos, t_ms, new_cube);
        }
    }

    pub fn character(&self) -> Option<&Character> {
        self.character.as_ref()
    }

    pub fn map_id(&self) -> Option<u32> {
        self.map_id
    }

    pub fn last_seen_ms(&self) -> u64 {
        self.last_seen_ms
    }

    /// Your last position fix.
    pub fn me(&self) -> Option<Fix> {
        self.me
    }

    /// Your entity id: from the self record, else the guess from the buff records.
    pub fn self_id(&self) -> Option<EntityId> {
        self.self_id.or_else(|| self.voted_self())
    }

    fn voted_self(&self) -> Option<EntityId> {
        let mut best: Option<(EntityId, u32)> = None;
        let mut tied = false;
        for (&id, &n) in &self.votes {
            match best {
                Some((_, b)) if n == b => tied = true,
                Some((_, b)) if n < b => {}
                _ => {
                    best = Some((id, n));
                    tied = false;
                }
            }
        }
        best.filter(|&(_, n)| n >= self.config.self_votes && !tied).map(|(id, _)| id)
    }

    pub fn push(&mut self, event: &Event) {
        let t_ms = event.t_ms();
        self.last_seen_ms = self.last_seen_ms.max(t_ms);
        match event {
            Event::Heartbeat { server_ms, .. } => {
                if *server_ms > 0 {
                    self.server_offset_ms = Some(*server_ms as i64 - t_ms as i64);
                }
            }
            Event::ZoneChange { map_id, .. } => {
                if self.map_id != Some(*map_id) {
                    self.remember_cubes();
                    self.cubes.clear();
                    self.objects.clear();
                    self.votes.clear();
                    self.me = None;
                    self.trail.clear();
                }
                self.map_id = Some(*map_id);
            }
            Event::SelfInfo { id, name, server, level, .. } => {
                self.self_id = Some(*id);
                self.votes.clear();
                self.character = Some(Character { name: name.clone(), server: *server, level: *level });
            }
            Event::Position { entity, pos, source, .. } => {
                let fix = Fix { pos: *pos, t_ms, source: *source };
                match entity {
                    None => self.set_me(fix),
                    Some(e) if Some(*e) == self.self_id => self.set_me(fix),
                    Some(e) if self.self_id.is_none() => {
                        if matches!(source, PositionSource::Buff | PositionSource::BuffAlt) {
                            *self.votes.entry(*e).or_default() += 1;
                        }
                        if self.voted_self() == Some(*e) {
                            self.set_me(fix);
                        }
                    }
                    Some(_) => {}
                }
            }
            Event::ObjectSpawn { id, template, pos, created_ms, .. } => self.on_spawn(t_ms, *id, *template, *pos, *created_ms),
            Event::ObjectRemoved { id, reason, .. } => self.on_removed(t_ms, *id, *reason),
            Event::OpenStart { object, actor, .. } => {
                if let Some(c) = self.cubes.get_mut(object).filter(|c| c.state.is_open_to_take()) {
                    c.state = CubeState::Opening;
                    c.state_ms = t_ms;
                    c.opener = Some(*actor);
                    self.log_cube(t_ms, *object, LogKind::OpeningStarted, Some(*actor));
                }
            }
            Event::OpenDone { object, actor, item, .. } => {
                if self.cubes.contains_key(object) {
                    self.finish_open(t_ms, *object, Some(*actor));
                } else if *item == KEY_ITEM {
                    self.learn(t_ms, *object, *actor);
                }
            }
        }
    }

    fn set_me(&mut self, fix: Fix) {
        if self.me.is_some_and(|m| m.t_ms > fix.t_ms) {
            return;
        }
        self.me = Some(fix);
        let moved = self.trail.last().is_none_or(|p| p.pos.planar_distance(&fix.pos) >= TRAIL_STEP_UNITS || (p.pos.z - fix.pos.z).abs() >= TRAIL_STEP_HEIGHT);
        if moved {
            self.trail.push(TrailPoint { t_ms: fix.t_ms, pos: fix.pos });
            if self.trail.len() > MAX_TRAIL {
                let excess = self.trail.len() - MAX_TRAIL;
                self.trail.drain(..excess);
            }
        }
        for c in self.cubes.values_mut().filter(|c| c.state == CubeState::OutOfView) {
            if c.pos.planar_distance(&fix.pos) <= self.config.gone_check_units {
                c.near_since_ms.get_or_insert(fix.t_ms);
            } else {
                c.near_since_ms = None;
            }
        }
    }

    fn on_spawn(&mut self, t_ms: u64, id: EntityId, template: u32, pos: Vec3, created_ms: Option<u64>) {
        if !self.book.is_cube(template) {
            if self.objects.len() >= self.config.max_objects {
                self.objects.clear();
            }
            self.objects.insert(id, ObjectSeen { template, pos });
            // The id now names another object: a cube that had it is no longer there.
            if self.cubes.get(&id).is_some_and(|c| c.state.is_open_to_take()) {
                self.set_state(t_ms, id, CubeState::Gone, LogKind::Gone, None);
            }
            return;
        }
        self.objects.remove(&id);
        let known = self.cubes.get_mut(&id).filter(|c| c.state.is_open_to_take() && c.template == template).map(|c| {
            c.pos = pos;
            c.created_ms = created_ms.or(c.created_ms);
            c.state == CubeState::OutOfView
        });
        if let Some(back_in_view) = known {
            self.spot_seen(template, pos, t_ms, false);
            if back_in_view {
                self.set_state(t_ms, id, CubeState::Visible, LogKind::BackInView, None);
            }
            return;
        }
        let earlier = self.take_left_behind(template, pos, created_ms);
        let first_seen_ms = earlier.map_or(t_ms, |e| e.first_seen_ms);
        let cube = Cube { id, template, pos, created_ms, first_seen_ms, state: CubeState::Visible, state_ms: t_ms, opener: None, near_since_ms: None };
        self.cubes.insert(id, cube);
        self.spot_seen(template, pos, t_ms, earlier.is_none());
        if earlier.is_some() {
            self.log_cube(t_ms, id, LogKind::BackInView, None);
            self.alert(t_ms, id, AlertKind::StillThere);
            return;
        }
        self.counts.cubes_seen += 1;
        self.log_cube(t_ms, id, LogKind::Appeared, None);
        self.alert(t_ms, id, AlertKind::Appeared);
    }

    /// Keeps the cubes that were still there, before a map change clears them.
    fn remember_cubes(&mut self) {
        let Some(map_id) = self.map_id else { return };
        for c in self.cubes.values().filter(|c| c.state.is_open_to_take()) {
            if let Some(created_ms) = c.created_ms {
                self.left_behind.push(LeftBehind { map_id, template: c.template, pos: c.pos, created_ms, first_seen_ms: c.first_seen_ms });
            }
        }
        if self.left_behind.len() > MAX_LEFT_BEHIND {
            let excess = self.left_behind.len() - MAX_LEFT_BEHIND;
            self.left_behind.drain(..excess);
        }
    }

    fn take_left_behind(&mut self, template: u32, pos: Vec3, created_ms: Option<u64>) -> Option<LeftBehind> {
        let created_ms = created_ms?;
        let map_id = self.map_id?;
        let i = self
            .left_behind
            .iter()
            .position(|l| l.map_id == map_id && l.template == template && l.created_ms == created_ms && l.pos.planar_distance(&pos) <= SAME_CUBE_UNITS)?;
        Some(self.left_behind.remove(i))
    }

    fn on_removed(&mut self, t_ms: u64, id: EntityId, reason: RemoveReason) {
        self.objects.remove(&id);
        let Some(state) = self.cubes.get(&id).map(|c| c.state) else { return };
        match reason {
            RemoveReason::LeftView if matches!(state, CubeState::Visible | CubeState::Opening) => {
                self.set_state(t_ms, id, CubeState::OutOfView, LogKind::LeftView, None);
            }
            RemoveReason::LeftView => {}
            RemoveReason::Opened => self.finish_open(t_ms, id, None),
            RemoveReason::Other(_) if state.is_open_to_take() => self.set_state(t_ms, id, CubeState::Gone, LogKind::Gone, None),
            RemoveReason::Other(_) => {}
        }
    }

    fn set_state(&mut self, t_ms: u64, id: EntityId, state: CubeState, kind: LogKind, actor: Option<EntityId>) {
        if let Some(c) = self.cubes.get_mut(&id) {
            c.state = state;
            c.state_ms = t_ms;
            c.near_since_ms = None;
            self.log_cube(t_ms, id, kind, actor);
        }
    }

    /// The cube was opened, by `actor` when the record named one, else by whoever started opening it.
    fn finish_open(&mut self, t_ms: u64, id: EntityId, actor: Option<EntityId>) {
        let me = self.self_id();
        let Some(c) = self.cubes.get_mut(&id) else { return };
        if matches!(c.state, CubeState::Opened | CubeState::Gone) {
            if c.opener.is_none() {
                c.opener = actor;
            }
            return;
        }
        c.opener = actor.or(c.opener);
        let (opener, pos) = (c.opener, c.pos);
        let by_you = opener.is_some() && opener == me;
        if by_you {
            self.counts.opened_by_you += 1;
            if let Some(map_id) = self.map_id {
                self.spots.opened_by_you(map_id, &pos);
            }
        } else {
            self.counts.opened_by_others += 1;
        }
        self.set_state(t_ms, id, CubeState::Opened, LogKind::Opened, opener);
        self.alert(t_ms, id, if by_you { AlertKind::OpenedByYou } else { AlertKind::OpenedByOther });
    }

    /// A key opening of an object that is not a known cube: its template is a cube template.
    fn learn(&mut self, t_ms: u64, id: EntityId, actor: EntityId) {
        let Some(obj) = self.objects.remove(&id) else {
            self.counts.key_opens_unknown += 1;
            return;
        };
        let zone = self.map_id.and_then(|m| GameData::get().zone(m)).map(String::from);
        let entry = CubeTemplate { template: obj.template, zone, evidence: Evidence::Learned };
        if self.book.add(entry.clone()) {
            self.learned.push(entry);
            self.counts.templates_learned += 1;
        }
        let cube = Cube { id, template: obj.template, pos: obj.pos, created_ms: None, first_seen_ms: t_ms, state: CubeState::Opened, state_ms: t_ms, opener: Some(actor), near_since_ms: None };
        self.cubes.insert(id, cube);
        self.spot_seen(obj.template, obj.pos, t_ms, true);
        if Some(actor) == self.self_id() {
            self.counts.opened_by_you += 1;
            if let Some(map_id) = self.map_id {
                self.spots.opened_by_you(map_id, &obj.pos);
            }
        } else {
            self.counts.opened_by_others += 1;
        }
        self.log_cube(t_ms, id, LogKind::Learned, Some(actor));
        self.alert(t_ms, id, AlertKind::Learned);
    }

    fn log_cube(&mut self, t_ms: u64, id: EntityId, kind: LogKind, actor: Option<EntityId>) {
        let Some(c) = self.cubes.get(&id) else { return };
        let by_you = actor.is_some() && actor == self.self_id();
        let distance_m = self.me.map(|m| m.pos.planar_distance(&c.pos) / UNITS_PER_METRE);
        self.log.push(LogEntry { t_ms, kind, cube: id, template: c.template, map_id: self.map_id, pos: c.pos, distance_m, actor, by_you });
        if self.log.len() > self.config.max_log {
            let excess = self.log.len() - self.config.max_log;
            self.log.drain(..excess);
        }
    }

    fn alert(&mut self, t_ms: u64, id: EntityId, kind: AlertKind) {
        if let Some(view) = self.cubes.get(&id).map(|c| self.view(c, t_ms)) {
            self.alerts.push(Alert { t_ms, kind, cube: view });
        }
    }

    /// Ages the radar: cubes that should have been sent again are gone, old ones leave the list.
    pub fn tick(&mut self, now_ms: u64) {
        let gone: Vec<EntityId> = self
            .cubes
            .values()
            .filter(|c| c.state == CubeState::OutOfView && c.near_since_ms.is_some_and(|t| now_ms.saturating_sub(t) >= self.config.gone_check_ms))
            .map(|c| c.id)
            .collect();
        for id in gone {
            self.set_state(now_ms, id, CubeState::Gone, LogKind::Gone, None);
        }
        let config = &self.config;
        self.cubes.retain(|_, c| {
            let age = now_ms.saturating_sub(c.state_ms);
            match c.state {
                CubeState::Opened | CubeState::Gone => age < config.keep_gone_ms,
                CubeState::OutOfView => age < config.keep_out_of_view_ms,
                CubeState::Visible | CubeState::Opening => true,
            }
        });
    }

    fn view(&self, c: &Cube, now_ms: u64) -> CubeView {
        let template = self.book.get(c.template);
        let (distance_m, height_m, bearing, estimated) = match self.me {
            Some(me) => (
                Some(me.pos.planar_distance(&c.pos) / UNITS_PER_METRE),
                Some((c.pos.z - me.pos.z) / UNITS_PER_METRE),
                Some(me.pos.bearing_to(&c.pos)),
                now_ms.saturating_sub(me.t_ms) > self.config.fix_fresh_ms,
            ),
            None => (None, None, None, false),
        };
        let server_now = (now_ms as i64 + self.server_offset_ms.unwrap_or(0)).max(0) as u64;
        CubeView {
            id: c.id,
            template: c.template,
            zone: template.and_then(|t| t.zone.clone()),
            evidence: template.map(|t| t.evidence),
            pos: c.pos,
            state: c.state,
            opener: c.opener,
            opened_by_you: c.opener.is_some() && c.opener == self.self_id(),
            first_seen_ms: c.first_seen_ms,
            state_ms: c.state_ms,
            age_ms: c.created_ms.map(|created| server_now.saturating_sub(created)),
            distance_m,
            height_m,
            bearing,
            estimated,
        }
    }

    pub fn snapshot(&self, now_ms: u64) -> RadarSnapshot {
        let mut cubes: Vec<CubeView> = self.cubes.values().map(|c| self.view(c, now_ms)).collect();
        cubes.sort_by(|a, b| {
            b.state
                .is_open_to_take()
                .cmp(&a.state.is_open_to_take())
                .then_with(|| match (a.state.is_open_to_take(), a.distance_m, b.distance_m) {
                    (true, Some(x), Some(y)) => x.total_cmp(&y),
                    (true, _, _) => a.first_seen_ms.cmp(&b.first_seen_ms),
                    (false, _, _) => b.state_ms.cmp(&a.state_ms),
                })
                .then(a.id.cmp(&b.id))
        });
        RadarSnapshot {
            now_ms,
            map: self.map_id.map(|m| GameData::get().map(m)),
            character: self.character.clone(),
            self_id: self.self_id(),
            self_guessed: self.self_id.is_none() && self.voted_self().is_some(),
            me: self.me,
            cubes,
            log: self.log.clone(),
            counts: self.counts.clone(),
            objects_in_view: self.objects.len(),
            trail: self.trail.clone(),
            spots: self.map_id.map(|m| self.spots.for_map(m)).unwrap_or_default(),
        }
    }

    /// Alerts raised since the last call, oldest first.
    pub fn drain_alerts(&mut self) -> Vec<Alert> {
        std::mem::take(&mut self.alerts)
    }

    /// Templates learned since the last call, for saving.
    pub fn drain_learned(&mut self) -> Vec<CubeTemplate> {
        std::mem::take(&mut self.learned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::tests::hex;
    use crate::parser::Parser;

    const VERTERON: u32 = 1010;

    /// dr89mh fixtures/cubes/2026-10-13_verteron_two_cubes.json without the loot records: two cubes found and
    /// opened with a key by entity 9180, with the buff records that carry its position.
    const TWO_CUBES: &[(u64, u16, &str)] = &[
        (1_791_884_796_014, 0x2A38, "dc470111d107e7000000c8000000000000005d1ea834a1010000dc470100d98f62c7613ae14776f28d46"),
        (1_791_884_810_211, 0x3436, "e5ff030400a74f1200001d44c780a5f647006471460000000000000000010101aae4a534a101000000000000000000002c4d4502070000"),
        (1_791_884_810_365, 0x2A38, "dc470111d207e7000000c8000000000000006c56a834a1010000dc470100846757c75bdae4470e3c8b46"),
        (1_791_884_819_863, 0x2A38, "dc470111d60702e1f505ffffffffffffffff8075d52abb030000dc470100096a58c7fb59ed4793db7646"),
        (1_791_884_822_412, 0x3436, "868d020400a74f1200a0b447c7b26c034800de964600000000002490430101014ab6a734a101000000000000000000002c4d4502070000"),
        (1_791_884_824_212, 0x2A38, "dc470111d707e7000000c800000000000000868ca834a1010000dc470100d40246c7fc11f64765e47146"),
        (1_791_884_830_559, 0x2A38, "dc470111d807c80000002c01000000000000b7a5a834a1010000dc470100c1cf44c7488cf64700587246"),
        (1_791_884_833_267, 0x3836, "e5ff03dc4701"),
        (1_791_884_837_514, 0x3A36, "e5ff03dc470100000023c59a25"),
        (1_791_884_838_212, 0x3536, "e5ff03040300"),
        (1_791_884_862_663, 0x2A38, "dc470111db07e7000000c800000000000000b722a934a1010000dc4701006ddc46c7425703486e029746"),
        (1_791_884_863_915, 0x3836, "868d02dc4701"),
        (1_791_884_868_164, 0x3A36, "868d02dc470100000023c59a25"),
        (1_791_884_868_866, 0x3536, "868d02040300"),
    ];

    fn feed(tracker: &mut Tracker, frames: &[(u64, u16, &str)]) {
        let mut parser = Parser::default();
        for &(t, op, body) in frames {
            let mut events = Vec::new();
            parser.packet(t, op, &hex(body), &mut |e| events.push(e));
            for e in &events {
                tracker.push(e);
            }
        }
    }

    fn spawn(t_ms: u64, id: EntityId, template: u32, x: f32, y: f32) -> Event {
        Event::ObjectSpawn { t_ms, id, kind: 4, template, pos: Vec3::new(x, y, 1_000.0), heading: 0.0, state: 1, created_ms: None }
    }

    fn fix(t_ms: u64, x: f32, y: f32) -> Event {
        Event::Position { t_ms, entity: None, pos: Vec3::new(x, y, 1_000.0), source: PositionSource::Teleport }
    }

    fn removed(t_ms: u64, id: EntityId, reason: RemoveReason) -> Event {
        Event::ObjectRemoved { t_ms, id, reason }
    }

    #[test]
    fn two_real_cubes_from_spawn_to_opened() {
        let mut t = Tracker::default();
        feed(&mut t, &TWO_CUBES[..2]);
        let alerts = t.drain_alerts();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, AlertKind::Appeared);
        assert_eq!(alerts[0].cube.id, 65_509);
        assert_eq!(alerts[0].cube.zone.as_deref(), Some("Verteron"));
        assert_eq!(alerts[0].cube.distance_m, None, "one buff fix is not enough to know who you are");

        feed(&mut t, &TWO_CUBES[2..5]);
        let alerts = t.drain_alerts();
        assert_eq!(alerts.len(), 1);
        let second = &alerts[0].cube;
        assert_eq!(second.id, 34_438);
        // The last fix before the spawn was 13 738 units away (dr89mh FINDINGS B8).
        let d = second.distance_m.unwrap();
        assert!((137.0..138.0).contains(&d), "{d}");
        assert_eq!(t.self_id(), Some(9_180), "guessed from three buff fixes");

        feed(&mut t, &TWO_CUBES[5..]);
        let alerts = t.drain_alerts();
        assert_eq!(alerts.iter().map(|a| a.kind).collect::<Vec<_>>(), vec![AlertKind::OpenedByYou, AlertKind::OpenedByYou]);
        let s = t.snapshot(1_791_884_870_000);
        assert!(s.self_guessed);
        assert_eq!(s.counts.cubes_seen, 2);
        assert_eq!(s.counts.opened_by_you, 2);
        assert!(s.cubes.iter().all(|c| c.state == CubeState::Opened && c.opened_by_you));
        let kinds: Vec<LogKind> = s.log.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, vec![LogKind::Appeared, LogKind::Appeared, LogKind::OpeningStarted, LogKind::Opened, LogKind::OpeningStarted, LogKind::Opened]);
        // You stood about 2 m from the cube while opening it (FINDINGS B7).
        let opened = s.log.iter().find(|l| l.kind == LogKind::Opened).unwrap();
        assert!(opened.distance_m.unwrap() < 3.0, "{:?}", opened.distance_m);
    }

    #[test]
    fn each_cube_alerts_once() {
        let mut t = Tracker::default();
        t.push(&spawn(1_000, 500, 1_200_039, 10_000.0, 10_000.0));
        t.push(&removed(2_000, 500, RemoveReason::LeftView));
        t.push(&spawn(3_000, 500, 1_200_039, 10_000.0, 10_000.0));
        assert_eq!(t.drain_alerts().len(), 1, "back in view is the same cube");
        t.push(&removed(4_000, 500, RemoveReason::Opened));
        assert_eq!(t.drain_alerts()[0].kind, AlertKind::OpenedByOther);
        t.push(&spawn(5_000, 500, 1_200_041, 20_000.0, 20_000.0));
        assert_eq!(t.drain_alerts()[0].kind, AlertKind::Appeared, "a reused id is a new cube");
        t.push(&spawn(6_000, 501, 1_500_041, 10_000.0, 10_000.0));
        assert!(t.drain_alerts().is_empty(), "a gathering node is no cube");
        assert_eq!(t.snapshot(6_000).objects_in_view, 1);
    }

    #[test]
    fn a_cube_not_sent_again_nearby_is_gone() {
        let mut t = Tracker::default();
        t.push(&fix(0, 0.0, 30_000.0));
        t.push(&spawn(1_000, 500, 1_200_039, 0.0, 20_000.0));
        t.push(&removed(2_000, 500, RemoveReason::LeftView));
        t.push(&fix(10_000, 0.0, 26_000.0)); // 60 m away: would be sent again
        t.tick(12_000);
        assert_eq!(t.snapshot(12_000).cubes[0].state, CubeState::OutOfView, "the server gets three seconds");
        t.tick(13_000);
        let s = t.snapshot(13_000);
        assert_eq!(s.cubes[0].state, CubeState::Gone);
        assert_eq!(s.log.last().unwrap().kind, LogKind::Gone);
        t.tick(13_000 + TrackerConfig::default().keep_gone_ms);
        assert!(t.snapshot(80_000).cubes.is_empty(), "gone cubes leave the list after a minute");
    }

    #[test]
    fn a_cube_sent_again_is_not_gone() {
        let mut t = Tracker::default();
        t.push(&spawn(1_000, 500, 1_200_039, 0.0, 20_000.0));
        t.push(&removed(2_000, 500, RemoveReason::LeftView));
        t.push(&fix(10_000, 0.0, 26_000.0));
        t.push(&spawn(11_000, 500, 1_200_039, 0.0, 20_000.0));
        t.tick(20_000);
        assert_eq!(t.snapshot(20_000).cubes[0].state, CubeState::Visible);
    }

    #[test]
    fn learns_a_template_from_a_key_opening() {
        let mut t = Tracker::default();
        t.push(&Event::ZoneChange { t_ms: 0, map_id: VERTERON });
        t.push(&Event::SelfInfo { t_ms: 1, id: 9_180, name: "Seraphel".into(), server: 2314, level: 45 });
        t.push(&spawn(1_000, 700, 1_400_123, 5_000.0, 5_000.0));
        t.push(&Event::OpenDone { t_ms: 5_000, object: 700, actor: 9_180, count: 1, item: KEY_ITEM });
        let alerts = t.drain_alerts();
        assert_eq!(alerts[0].kind, AlertKind::Learned);
        let learned = t.drain_learned();
        assert_eq!(learned, vec![CubeTemplate { template: 1_400_123, zone: Some("Verteron".into()), evidence: Evidence::Learned }]);
        assert_eq!(t.snapshot(5_000).counts.opened_by_you, 1);
        t.push(&spawn(9_000, 701, 1_400_123, 6_000.0, 6_000.0));
        assert_eq!(t.drain_alerts()[0].kind, AlertKind::Appeared, "the next one is a cube from the start");
        // A key opening of an object that spawned before the capture cannot teach anything.
        t.push(&Event::OpenDone { t_ms: 10_000, object: 999, actor: 9_180, count: 1, item: KEY_ITEM });
        assert_eq!(t.snapshot(10_000).counts.key_opens_unknown, 1);
        // Gathering nodes take other items.
        t.push(&spawn(11_000, 702, 1_500_000, 6_000.0, 6_000.0));
        t.push(&Event::OpenDone { t_ms: 12_000, object: 702, actor: 9_180, count: 2, item: 0 });
        assert!(t.drain_learned().is_empty());
    }

    #[test]
    fn a_new_map_clears_the_radar_and_the_same_map_keeps_it() {
        let mut t = Tracker::default();
        t.push(&Event::ZoneChange { t_ms: 0, map_id: VERTERON });
        t.push(&spawn(1_000, 500, 1_200_039, 0.0, 20_000.0));
        t.push(&Event::ZoneChange { t_ms: 2_000, map_id: VERTERON });
        assert_eq!(t.snapshot(2_000).cubes.len(), 1, "a teleport inside the zone");
        t.push(&Event::ZoneChange { t_ms: 3_000, map_id: 600_072 });
        let s = t.snapshot(3_000);
        assert!(s.cubes.is_empty());
        assert!(s.me.is_none());
        assert_eq!(s.map.unwrap().name, "Vakron Sky Island");
        assert_eq!(s.log.len(), 1, "the log keeps the history");
    }

    #[test]
    fn a_cube_left_behind_is_still_there_when_you_return() {
        // Template 1301199 in map 1110, from the aion2dps captures of 2026-10-11: sent 0.9 s after it spawned,
        // then sent again, unchanged, each time the character came back from a dungeon.
        let body = hex(concat!(
            "b6f903", "0400", "cfda1300", "e18f2148", "78c42bc7", "000c1246", "00000000", "00c0a842", "010101", "195bff2aa1010000", "0000000000000000", "021dbce3070000"
        ));
        let mut parser = Parser::default();
        let mut t = Tracker::default();
        let mut step = |t: &mut Tracker, t_ms: u64, op: u16, body: &[u8]| {
            let mut events = Vec::new();
            parser.packet(t_ms, op, body, &mut |e| events.push(e));
            events.iter().for_each(|e| t.push(e));
        };
        let zone = |map_id: u32| [1u32.to_le_bytes(), map_id.to_le_bytes()].concat();
        step(&mut t, 1_791_722_671_591, 0x2136, &zone(1110));
        step(&mut t, 1_791_722_740_386, 0x3436, &body);
        assert_eq!(t.drain_alerts()[0].kind, AlertKind::Appeared);
        step(&mut t, 1_791_722_782_608, 0x2136, &zone(600_072));
        assert!(t.snapshot(1_791_722_782_608).cubes.is_empty());
        step(&mut t, 1_791_722_888_086, 0x2136, &zone(1110));
        step(&mut t, 1_791_722_894_334, 0x3436, &body);
        let alerts = t.drain_alerts();
        assert_eq!(alerts[0].kind, AlertKind::StillThere);
        assert_eq!(alerts[0].cube.first_seen_ms, 1_791_722_740_386);
        let s = t.snapshot(1_791_722_894_334);
        assert_eq!(s.counts.cubes_seen, 1);
        assert_eq!(s.log.iter().map(|l| l.kind).collect::<Vec<_>>(), vec![LogKind::Appeared, LogKind::BackInView]);
        // The same spot with another spawn time is a new cube.
        let mut respawned = body.clone();
        respawned[34] ^= 0x01;
        step(&mut t, 1_791_722_949_785, 0x2136, &zone(600_012));
        step(&mut t, 1_791_722_967_783, 0x2136, &zone(1110));
        step(&mut t, 1_791_722_970_000, 0x3436, &respawned);
        assert_eq!(t.drain_alerts()[0].kind, AlertKind::Appeared);
    }

    #[test]
    fn the_self_record_beats_the_guess() {
        let mut t = Tracker::default();
        let buff = |t_ms, entity, x| Event::Position { t_ms, entity: Some(entity), pos: Vec3::new(x, 50_000.0, 0.0), source: PositionSource::Buff };
        for i in 0..3 {
            t.push(&buff(i, 42, 10_000.0));
        }
        assert_eq!(t.self_id(), Some(42));
        t.push(&Event::SelfInfo { t_ms: 10, id: 9_180, name: "Ari".into(), server: 2314, level: 1 });
        t.push(&buff(11, 42, 20_000.0));
        assert_eq!(t.snapshot(11).me.unwrap().pos.x, 10_000.0, "entity 42 is someone else now");
        t.push(&buff(12, 9_180, 30_000.0));
        let s = t.snapshot(12);
        assert_eq!(s.me.unwrap().pos.x, 30_000.0);
        assert!(!s.self_guessed);
        assert_eq!(s.character.unwrap().name, "Ari");
    }

    #[test]
    fn a_tied_vote_guesses_nobody() {
        let mut t = Tracker::default();
        for i in 0..3 {
            for e in [1, 2] {
                t.push(&Event::Position { t_ms: i, entity: Some(e), pos: Vec3::new(10_000.0, 10_000.0, 0.0), source: PositionSource::Buff });
            }
        }
        assert_eq!(t.self_id(), None);
    }

    #[test]
    fn age_uses_the_server_clock_and_distance_marks_old_fixes() {
        let mut t = Tracker::default();
        // The server clock runs 5 s ahead of this PC.
        t.push(&Event::Heartbeat { t_ms: 100_000, server_ms: 105_000 });
        t.push(&fix(100_000, 0.0, 0.0));
        let mut e = spawn(100_000, 500, 1_200_039, 3_000.0, -4_000.0);
        if let Event::ObjectSpawn { created_ms, .. } = &mut e {
            *created_ms = Some(45_000);
        }
        t.push(&e);
        let c = t.snapshot(101_000).cubes[0].clone();
        assert_eq!(c.age_ms, Some(61_000));
        assert_eq!(c.distance_m, Some(50.0));
        assert!(!c.estimated);
        assert_eq!(crate::geo::compass(c.bearing.unwrap()), "NE");
        assert!(t.snapshot(111_000).cubes[0].estimated);
    }

    #[test]
    fn keeps_your_path_and_the_spots_for_the_map() {
        let mut t = Tracker::default();
        t.set_spots(vec![Spot { map_id: VERTERON, template: 1_200_039, pos: Vec3::new(0.0, 40_000.0, 1_000.0), first_seen_ms: 1, last_seen_ms: 1, sightings: 3, opened_by_you: 1 }]);
        t.push(&Event::ZoneChange { t_ms: 0, map_id: VERTERON });
        t.push(&Event::SelfInfo { t_ms: 0, id: 9_180, name: "Ari".into(), server: 2314, level: 1 });
        t.push(&fix(1_000, 0.0, 30_000.0));
        t.push(&fix(2_000, 50.0, 30_000.0));
        t.push(&fix(3_000, 0.0, 29_000.0));
        assert_eq!(t.snapshot(3_000).trail.len(), 2, "a step under 1.5 m adds no point");
        assert!(t.take_spots_changed().is_none(), "loaded spots are not news");
        // A cube at the saved spot counts there; one elsewhere is a new spot.
        t.push(&spawn(4_000, 500, 1_200_039, 100.0, 40_000.0));
        t.push(&spawn(5_000, 501, 1_200_041, 9_000.0, 25_000.0));
        t.push(&Event::OpenDone { t_ms: 6_000, object: 501, actor: 9_180, count: 1, item: KEY_ITEM });
        let s = t.snapshot(6_000);
        assert_eq!(s.spots.len(), 2);
        let saved = s.spots.iter().find(|p| p.sightings == 4).unwrap();
        assert_eq!(saved.last_seen_ms, 4_000);
        let new = s.spots.iter().find(|p| p.sightings == 1).unwrap();
        assert_eq!(new.opened_by_you, 1);
        assert_eq!(t.take_spots_changed().map(|all| all.len()), Some(2));
        // The spots of another map are kept but not shown, the path starts over.
        t.push(&Event::ZoneChange { t_ms: 7_000, map_id: 600_072 });
        let s = t.snapshot(7_000);
        assert!(s.spots.is_empty() && s.trail.is_empty());
    }

    #[test]
    fn a_learned_cube_sits_where_the_object_was() {
        let mut t = Tracker::default();
        t.push(&Event::ZoneChange { t_ms: 0, map_id: VERTERON });
        t.push(&spawn(1_000, 700, 1_400_123, 5_000.0, 6_000.0));
        t.push(&Event::OpenDone { t_ms: 5_000, object: 700, actor: 42, count: 1, item: KEY_ITEM });
        let s = t.snapshot(5_000);
        assert_eq!(s.cubes[0].pos, Vec3::new(5_000.0, 6_000.0, 1_000.0));
        assert_eq!(s.spots[0].pos, Vec3::new(5_000.0, 6_000.0, 1_000.0));
    }

    #[test]
    fn open_cubes_sort_nearest_first() {
        let mut t = Tracker::default();
        t.push(&fix(0, 0.0, 0.0));
        t.push(&spawn(1, 1, 1_200_039, 9_000.0, 0.0));
        t.push(&spawn(2, 2, 1_200_039, 3_000.0, 0.0));
        t.push(&spawn(3, 3, 1_200_039, 5_000.0, 0.0));
        t.push(&removed(4, 3, RemoveReason::Opened));
        let ids: Vec<EntityId> = t.snapshot(5).cubes.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![2, 1, 3]);
        assert_eq!(t.snapshot(5).open_cubes().count(), 2);
    }
}
