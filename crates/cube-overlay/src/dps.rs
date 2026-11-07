//! The DPS meter's numbers: every character's damage on one enemy, in three modes.
//!
//! - **Damage**: everything the character dealt to that enemy.
//! - **DPS**: that damage divided by the time the enemy has been under attack (from the first hit anyone landed
//!   on it to the last), so every row shares one clock and the rows add up to the enemy's damage per second.
//! - **5 s DPS**: a moving average, the damage dealt to the enemy in the last 5 seconds divided by 5 (by the time
//!   since the first hit while the fight is younger than that).
//!
//! The meter tracker (aion2-tui's `meter-core`, unchanged) decides what counts: who is a player, summons folded
//! into their owner, false hits dropped, encounters started and ended. This module watches its snapshots ten
//! times a second and turns the growth of each character's damage on each enemy into timed hits, which gives the
//! 5 s window and the time under attack per enemy.
//!
//! Which enemy: by default the one you hit last (so switching to an add switches the meter), back to the
//! encounter's main target (the boss) when yours died or you stopped hitting it. Clicking the enemy's name pins
//! another one.

use meter_core::parser::EntityId;
use meter_core::snapshot::{Snapshot, TargetInfo};
use std::collections::{HashMap, VecDeque};

pub const WINDOW_MS: u64 = 5_000;
/// Your last target stays the meter's for this long after your last hit on it.
const SELF_TARGET_KEEP_MS: u64 = 15_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Damage,
    Average,
    Recent,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Damage, Mode::Average, Mode::Recent];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Damage => "Damage",
            Mode::Average => "DPS",
            Mode::Recent => "5 s DPS",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Mode::Damage => "damage",
            Mode::Average => "dps",
            Mode::Recent => "5s",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "damage" | "dmg" | "total" => Some(Mode::Damage),
            "dps" | "average" | "overall" => Some(Mode::Average),
            "5s" | "recent" | "5s-dps" | "moving" => Some(Mode::Recent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Auto,
    Pinned(EntityId),
}

#[derive(Debug, Clone, Copy)]
struct Attacked {
    first_ms: u64,
    last_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: EntityId,
    pub name: String,
    pub class_id: Option<u32>,
    pub is_self: bool,
    /// In the mode's unit: damage, or damage per second.
    pub value: f64,
    /// Damage on the enemy, whatever the mode.
    pub damage: u64,
    /// Share of the rows' total, 0 to 1.
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub mode: Mode,
    pub target: Option<TargetInfo>,
    /// The enemy was picked by hand rather than followed.
    pub pinned: bool,
    /// How long the enemy has been under attack.
    pub attacked_ms: u64,
    /// The encounter is still running.
    pub active: bool,
    /// Highest value first.
    pub rows: Vec<Row>,
    pub total: f64,
}

#[derive(Default)]
pub struct Ledger {
    encounter: Option<u64>,
    /// Damage seen so far per (character, enemy), to turn growth into hits.
    seen: HashMap<(EntityId, EntityId), u64>,
    /// Hits of the last few seconds: time, character, enemy, damage.
    recent: VecDeque<(u64, EntityId, EntityId, u64)>,
    attacked: HashMap<EntityId, Attacked>,
    /// The enemy you hit last, and when.
    self_target: Option<(EntityId, u64)>,
}

impl Ledger {
    fn clear(&mut self) {
        *self = Ledger::default();
    }

    fn update(&mut self, snap: &Snapshot, now_ms: u64) {
        if self.encounter != Some(snap.id) {
            self.clear();
            self.encounter = Some(snap.id);
        }
        for actor in &snap.actors {
            for t in &actor.targets {
                let before = self.seen.insert((actor.id, t.id), t.damage).unwrap_or(0);
                if t.damage <= before {
                    continue;
                }
                self.recent.push_back((now_ms, actor.id, t.id, t.damage - before));
                let a = self.attacked.entry(t.id).or_insert(Attacked { first_ms: now_ms, last_ms: now_ms });
                a.last_ms = a.last_ms.max(now_ms);
                if actor.is_self {
                    self.self_target = Some((t.id, now_ms));
                }
            }
        }
        while self.recent.front().is_some_and(|h| h.0 + WINDOW_MS < now_ms) {
            self.recent.pop_front();
        }
    }

    fn recent_damage(&self, actor: EntityId, target: EntityId, now_ms: u64) -> u64 {
        self.recent.iter().filter(|h| h.1 == actor && h.2 == target && h.0 + WINDOW_MS > now_ms).map(|h| h.3).sum()
    }
}

pub struct DpsMeter {
    pub mode: Mode,
    pick: Pick,
    ledger: Ledger,
    snap: Option<Snapshot>,
    now_ms: u64,
}

impl DpsMeter {
    pub fn new(mode: Mode) -> DpsMeter {
        DpsMeter { mode, pick: Pick::Auto, ledger: Ledger::default(), snap: None, now_ms: 0 }
    }

    /// Feeds the meter tracker's snapshot, about ten times a second.
    pub fn update(&mut self, snap: Option<Snapshot>, now_ms: u64) {
        self.now_ms = now_ms;
        match &snap {
            Some(s) => self.ledger.update(s, now_ms),
            None => self.ledger.clear(),
        }
        if let (Pick::Pinned(id), Some(s)) = (self.pick, &snap) {
            if !s.targets.iter().any(|t| t.id == id) {
                self.pick = Pick::Auto;
            }
        }
        if snap.is_none() {
            self.pick = Pick::Auto;
        }
        self.snap = snap;
    }

    /// Forgets the numbers (the tracker's encounter is reset by the caller).
    pub fn clear(&mut self) {
        self.ledger.clear();
        self.snap = None;
        self.pick = Pick::Auto;
    }

    fn target(&self) -> Option<&TargetInfo> {
        let snap = self.snap.as_ref()?;
        let find = |id: EntityId| snap.targets.iter().find(|t| t.id == id);
        if let Pick::Pinned(id) = self.pick {
            if let Some(t) = find(id) {
                return Some(t);
            }
        }
        let main = snap.boss.as_ref().and_then(|b| find(b.id)).or(snap.targets.first());
        let yours = self
            .ledger
            .self_target
            .filter(|(_, at)| self.now_ms.saturating_sub(*at) <= SELF_TARGET_KEEP_MS || !snap.active)
            .and_then(|(id, _)| find(id));
        match (yours, main) {
            // Your enemy died while the main one fights on: back to the main one.
            (Some(y), Some(m)) if y.dead && !m.dead && y.id != m.id => Some(m),
            (Some(y), _) => Some(y),
            (None, m) => m,
        }
    }

    /// Auto, then each enemy of the encounter in turn, then auto again.
    pub fn cycle_target(&mut self) {
        let Some(snap) = &self.snap else { return };
        let ids: Vec<EntityId> = snap.targets.iter().map(|t| t.id).collect();
        self.pick = match self.pick {
            Pick::Auto => ids.first().map_or(Pick::Auto, |id| Pick::Pinned(*id)),
            Pick::Pinned(id) => match ids.iter().position(|i| *i == id) {
                Some(i) if i + 1 < ids.len() => Pick::Pinned(ids[i + 1]),
                _ => Pick::Auto,
            },
        };
    }

    pub fn view(&self) -> View {
        let empty = View { mode: self.mode, target: None, pinned: false, attacked_ms: 0, active: false, rows: Vec::new(), total: 0.0 };
        let (Some(snap), Some(target)) = (&self.snap, self.target()) else { return empty };
        let attacked = self.ledger.attacked.get(&target.id).copied();
        let attacked_ms = attacked.map_or(0, |a| a.last_ms.saturating_sub(a.first_ms));
        // While the fight is younger than the window, average over the time it has lasted.
        let window_ms = attacked.map_or(WINDOW_MS, |a| self.now_ms.saturating_sub(a.first_ms).clamp(1_000, WINDOW_MS));
        let mut rows: Vec<Row> = snap
            .actors
            .iter()
            .filter_map(|a| {
                let damage = a.targets.iter().find(|t| t.id == target.id)?.damage;
                if damage == 0 {
                    return None;
                }
                let value = match self.mode {
                    Mode::Damage => damage as f64,
                    Mode::Average => damage as f64 * 1000.0 / attacked_ms.max(1_000) as f64,
                    Mode::Recent => self.ledger.recent_damage(a.id, target.id, self.now_ms) as f64 * 1000.0 / window_ms as f64,
                };
                Some(Row { id: a.id, name: a.name.clone(), class_id: a.class_id, is_self: a.is_self, value, damage, share: 0.0 })
            })
            .collect();
        rows.sort_by(|a, b| b.value.total_cmp(&a.value).then(b.damage.cmp(&a.damage)).then(a.name.cmp(&b.name)));
        let total: f64 = rows.iter().map(|r| r.value).sum();
        for r in &mut rows {
            r.share = if total > 0.0 { r.value / total } else { 0.0 };
        }
        View { mode: self.mode, target: Some(target.clone()), pinned: matches!(self.pick, Pick::Pinned(_)), attacked_ms, active: snap.active, rows, total }
    }
}

/// "1.23M", "45.6k", "812".
pub fn short_number(v: f64) -> String {
    let v = v.max(0.0);
    if v >= 1e9 {
        format!("{:.2}B", v / 1e9)
    } else if v >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if v >= 1e4 {
        format!("{:.1}k", v / 1e3)
    } else if v >= 1e3 {
        format!("{:.2}k", v / 1e3)
    } else {
        format!("{v:.0}")
    }
}

/// "1:05", "12:40".
pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use meter_core::snapshot::{ActorStats, TargetDamage};

    const BOSS: EntityId = 900;
    const ADD: EntityId = 901;

    fn target(id: EntityId, name: &str, dead: bool) -> TargetInfo {
        TargetInfo { id, name: name.into(), is_boss: id == BOSS, dead, ..TargetInfo::default() }
    }

    fn actor(id: EntityId, name: &str, is_self: bool, on: &[(EntityId, u64)]) -> ActorStats {
        ActorStats {
            id,
            name: name.into(),
            is_self,
            is_player: true,
            targets: on.iter().map(|(t, d)| TargetDamage { id: *t, name: String::new(), damage: *d, share: 0.0 }).collect(),
            ..ActorStats::default()
        }
    }

    fn snap(id: u64, actors: Vec<ActorStats>, add_dead: bool) -> Snapshot {
        let boss = target(BOSS, "Boss", false);
        Snapshot { id, active: true, boss: Some(boss.clone()), targets: vec![boss, target(ADD, "Add", add_dead)], actors, ..Snapshot::default() }
    }

    /// Ten seconds: A deals 1000 per second on the boss throughout, B 3000 per second for the first five
    /// seconds and nothing after; you (C) hit the add.
    fn fight(m: &mut DpsMeter) {
        let t0 = 1_000_000;
        for s in 0..=10u64 {
            let a = 1000 * s;
            let b = 3000 * s.min(5);
            let c = 500 * s;
            let s_ = snap(1, vec![actor(1, "A", false, &[(BOSS, a)]), actor(2, "B", false, &[(BOSS, b)]), actor(3, "C", true, &[(ADD, c)])], false);
            m.update(Some(s_), t0 + s * 1000);
        }
    }

    #[test]
    fn the_three_modes() {
        let mut m = DpsMeter::new(Mode::Damage);
        fight(&mut m);
        m.cycle_target(); // pin the boss: auto would follow you to the add
        let v = m.view();
        assert_eq!(v.target.as_ref().unwrap().id, BOSS);
        assert!(v.pinned);
        assert_eq!(v.rows.iter().map(|r| (r.name.as_str(), r.value)).collect::<Vec<_>>(), vec![("B", 15_000.0), ("A", 10_000.0)]);
        assert_eq!(v.total, 25_000.0);
        // The first damage shows up at the 1 s snapshot, the last at 10 s. (Live, snapshots come every 100 ms.)
        assert_eq!(v.attacked_ms, 9_000);

        m.mode = Mode::Average;
        let v = m.view();
        assert_eq!(v.rows.iter().map(|r| (r.name.as_str(), r.value.round())).collect::<Vec<_>>(), vec![("B", 1_667.0), ("A", 1_111.0)]);

        m.mode = Mode::Recent;
        let v = m.view();
        // The last five seconds: A 1000 per second, B nothing.
        assert_eq!(v.rows.iter().map(|r| (r.name.as_str(), r.value)).collect::<Vec<_>>(), vec![("A", 1_000.0), ("B", 0.0)]);
        assert_eq!(v.rows[0].share, 1.0);
    }

    #[test]
    fn early_in_a_fight_the_window_is_the_fight() {
        let mut m = DpsMeter::new(Mode::Recent);
        let t0 = 5_000_000;
        m.update(Some(snap(1, vec![actor(1, "A", false, &[(BOSS, 0)])], false)), t0);
        m.update(Some(snap(1, vec![actor(1, "A", false, &[(BOSS, 2_000)])], false)), t0 + 1_000);
        m.update(Some(snap(1, vec![actor(1, "A", false, &[(BOSS, 4_000)])], false)), t0 + 2_000);
        // Hits seen at 1 s and 2 s, the enemy first hit at 1 s: 4 000 over max(1 s, 1 s) ...
        let v = m.view();
        assert_eq!(v.rows[0].value, 4_000.0);
        m.update(Some(snap(1, vec![actor(1, "A", false, &[(BOSS, 6_000)])], false)), t0 + 3_000);
        // ... and 6 000 over the 2 s since then.
        assert_eq!(m.view().rows[0].value, 3_000.0);
    }

    #[test]
    fn follows_your_target_and_back_to_the_boss_when_it_dies() {
        let mut m = DpsMeter::new(Mode::Damage);
        fight(&mut m);
        assert_eq!(m.view().target.unwrap().id, ADD, "you hit the add last");
        assert!(!m.view().pinned);
        let s = snap(1, vec![actor(1, "A", false, &[(BOSS, 11_000)]), actor(3, "C", true, &[(ADD, 5_000)])], true);
        m.update(Some(s), 1_011_000);
        assert_eq!(m.view().target.unwrap().id, BOSS, "the add died");
    }

    #[test]
    fn cycling_goes_through_every_enemy_and_back_to_auto() {
        let mut m = DpsMeter::new(Mode::Damage);
        fight(&mut m);
        m.cycle_target();
        assert_eq!(m.view().target.unwrap().id, BOSS);
        m.cycle_target();
        assert_eq!(m.view().target.unwrap().id, ADD);
        assert!(m.view().pinned);
        m.cycle_target();
        assert!(!m.view().pinned);
    }

    #[test]
    fn a_new_encounter_starts_from_zero() {
        let mut m = DpsMeter::new(Mode::Recent);
        fight(&mut m);
        m.update(Some(snap(2, vec![actor(1, "A", false, &[(BOSS, 500)])], false)), 1_012_000);
        let v = m.view();
        assert_eq!(v.rows.len(), 1);
        assert_eq!(v.rows[0].damage, 500);
        assert_eq!(v.rows[0].value, 500.0, "500 in the first second, not the old encounter's 11 000");
        m.update(None, 1_013_000);
        assert!(m.view().target.is_none());
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(short_number(812.4), "812");
        assert_eq!(short_number(4_560.0), "4.56k");
        assert_eq!(short_number(45_600.0), "45.6k");
        assert_eq!(short_number(1_234_567.0), "1.23M");
        assert_eq!(short_number(2.5e9), "2.50B");
        assert_eq!(clock(65_000), "1:05");
    }
}
