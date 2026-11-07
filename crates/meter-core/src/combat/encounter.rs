//! One encounter: the accumulators behind a `Snapshot`, the main target choice and the end state.
//!
//! Everything here is keyed by entity id and updated in O(1) per event. Names, ownership and classes
//! are resolved when the snapshot is built (see `stats.rs`), so late identities and id changes need
//! no rewrite of what was already counted.

use crate::parser::{EntityId, HitFlags};
use crate::snapshot::{EndReason, TimelinePoint};
use std::collections::HashMap;

/// Buckets past this point are not added (a two hour cap on the timeline).
pub const MAX_TIMELINE_S: u64 = 2 * 60 * 60;
/// Cast records of one skill by one actor closer than this are one use: a buff on five party members
/// or an AoE on three mobs sends one no value record per target with the same timestamp (session2:
/// 2 585 of 7 352 cast groups hold two or more records, groups of five being the party buffs).
pub const CAST_DEDUP_MS: u64 = 100;

/// What an accumulator was created for. Summons fold into their owner at snapshot time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccKind {
    Player,
    Summon,
}

/// Hit counters shared by the per skill rows and the per target totals.
#[derive(Debug, Clone, Default)]
pub struct SkillAcc {
    pub damage: u64,
    pub hits: u64,
    /// Hits that were not DoT ticks; the base for the crit and positional rates.
    pub direct_hits: u64,
    pub crits: u64,
    pub max_hit: u64,
    pub min_hit: u64,
    pub back: u64,
    pub front: u64,
    pub parry: u64,
    pub perfect: u64,
    pub double: u64,
    pub multi: u64,
}

impl SkillAcc {
    pub fn add(&mut self, amount: u64, flags: &HitFlags) {
        self.damage += amount;
        self.hits += 1;
        self.direct_hits += u64::from(!flags.dot);
        self.crits += u64::from(flags.crit);
        self.back += u64::from(flags.back);
        self.front += u64::from(flags.front);
        self.parry += u64::from(flags.parry);
        self.perfect += u64::from(flags.perfect);
        self.double += u64::from(flags.double);
        self.multi += u64::from(flags.multi);
        self.max_hit = self.max_hit.max(amount);
        self.min_hit = if self.hits == 1 { amount } else { self.min_hit.min(amount) };
    }

    pub fn merge(&mut self, o: &SkillAcc) {
        if o.hits == 0 {
            return;
        }
        self.min_hit = if self.hits == 0 { o.min_hit } else { self.min_hit.min(o.min_hit) };
        self.damage += o.damage;
        self.hits += o.hits;
        self.direct_hits += o.direct_hits;
        self.crits += o.crits;
        self.back += o.back;
        self.front += o.front;
        self.parry += o.parry;
        self.perfect += o.perfect;
        self.double += o.double;
        self.multi += o.multi;
        self.max_hit = self.max_hit.max(o.max_hit);
    }
}

/// One actor's damage on one target.
#[derive(Debug, Clone, Default)]
pub struct TargetAcc {
    pub total: SkillAcc,
    /// Keyed by (base skill, dot).
    pub skills: HashMap<(u32, bool), SkillAcc>,
    /// Sparse one second buckets from the encounter start, in time order.
    pub timeline: Vec<TimelinePoint>,
    pub last_hit_ms: Option<u64>,
    /// Sum of the gaps between consecutive hits that were short enough to count as activity.
    pub active_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ActorAcc {
    pub kind: AccKind,
    pub class_hint: Option<u32>,
    pub targets: HashMap<EntityId, TargetAcc>,
    pub damage_all: u64,
    pub casts: u64,
    pub skill_casts: HashMap<u32, u64>,
    pub healing: u64,
    pub heal_skills: HashMap<u32, u64>,
    pub damage_taken: u64,
    pub deaths: u32,
}

impl ActorAcc {
    fn new(kind: AccKind) -> Self {
        ActorAcc {
            kind,
            class_hint: None,
            targets: HashMap::new(),
            damage_all: 0,
            casts: 0,
            skill_casts: HashMap::new(),
            healing: 0,
            heal_skills: HashMap::new(),
            damage_taken: 0,
            deaths: 0,
        }
    }
}

/// Damage taken by one target from every counted actor.
#[derive(Debug, Clone)]
pub struct TargetTotals {
    pub damage: u64,
    pub last_hit_ms: u64,
    pub is_boss: bool,
    pub is_dummy: bool,
    /// Order of first hit, the tie breaker when two targets took the same damage.
    pub order: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct DeathRecord {
    pub t_ms: u64,
    pub id: EntityId,
    pub is_player: bool,
}

/// A counted damage record, with the skill already reduced to its base code.
pub struct Hit<'a> {
    pub t_ms: u64,
    pub actor: EntityId,
    pub actor_kind: AccKind,
    pub class_hint: Option<u32>,
    pub target: EntityId,
    pub target_is_boss: bool,
    pub target_is_dummy: bool,
    pub skill: u32,
    pub amount: u64,
    pub flags: &'a HitFlags,
}

pub struct Encounter {
    pub id: u64,
    pub started_ms: u64,
    /// Last counted damage on any target; drives the idle rules.
    pub last_damage_ms: u64,
    /// Last counted damage or heal.
    pub last_event_ms: u64,
    pub ended: Option<(EndReason, u64)>,
    pub map_id: Option<u32>,
    pub map_name: Option<String>,
    pub is_instance: bool,
    /// The boss with the most damage taken, else the most damaged target.
    pub main: Option<EntityId>,
    pub actors: HashMap<EntityId, ActorAcc>,
    pub targets: HashMap<EntityId, TargetTotals>,
    pub deaths: Vec<DeathRecord>,
    /// Time of the last cast record per (actor, base skill), for the cast dedup.
    last_casts: HashMap<(EntityId, u32), u64>,
}

impl Encounter {
    pub fn new(id: u64, started_ms: u64, map_id: Option<u32>, map_name: Option<String>, is_instance: bool) -> Self {
        Encounter {
            id,
            started_ms,
            last_damage_ms: started_ms,
            last_event_ms: started_ms,
            ended: None,
            map_id,
            map_name,
            is_instance,
            main: None,
            actors: HashMap::new(),
            targets: HashMap::new(),
            deaths: Vec::new(),
            last_casts: HashMap::new(),
        }
    }

    /// Time of the last player death recorded in this encounter.
    pub fn last_player_death_ms(&self) -> Option<u64> {
        self.deaths.iter().rev().find(|d| d.is_player).map(|d| d.t_ms)
    }

    pub fn end(&mut self, reason: EndReason, t_ms: u64) {
        if self.ended.is_none() {
            self.ended = Some((reason, t_ms));
        }
    }

    fn main_totals(&self) -> Option<&TargetTotals> {
        self.main.and_then(|m| self.targets.get(&m))
    }

    pub fn main_is_boss(&self) -> bool {
        self.main_totals().map(|t| t.is_boss).unwrap_or(false)
    }

    pub fn main_is_dummy(&self) -> bool {
        self.main_totals().map(|t| t.is_dummy).unwrap_or(false)
    }

    /// Last counted damage on the main target by anyone.
    pub fn main_last_hit_ms(&self) -> u64 {
        self.main_totals().map(|t| t.last_hit_ms).unwrap_or(self.last_damage_ms)
    }

    /// Encounter start to the last hit on the main target. Frozen once the encounter ends because
    /// nothing is counted after that.
    pub fn duration_ms(&self) -> u64 {
        self.main_last_hit_ms().saturating_sub(self.started_ms)
    }

    pub fn add_damage(&mut self, hit: &Hit, active_gap_ms: u64) {
        self.last_damage_ms = self.last_damage_ms.max(hit.t_ms);
        self.last_event_ms = self.last_event_ms.max(hit.t_ms);

        let order = self.targets.len();
        let tt = self.targets.entry(hit.target).or_insert_with(|| TargetTotals {
            damage: 0,
            last_hit_ms: hit.t_ms,
            is_boss: hit.target_is_boss,
            is_dummy: hit.target_is_dummy,
            order,
        });
        tt.damage += hit.amount;
        tt.last_hit_ms = tt.last_hit_ms.max(hit.t_ms);
        tt.is_boss = hit.target_is_boss;
        tt.is_dummy = hit.target_is_dummy;
        let target_damage = tt.damage;

        let acc = self.actors.entry(hit.actor).or_insert_with(|| ActorAcc::new(hit.actor_kind));
        if acc.class_hint.is_none() {
            acc.class_hint = hit.class_hint;
        }
        acc.damage_all += hit.amount;
        let ta = acc.targets.entry(hit.target).or_default();
        ta.total.add(hit.amount, hit.flags);
        ta.skills.entry((hit.skill, hit.flags.dot)).or_default().add(hit.amount, hit.flags);
        if let Some(last) = ta.last_hit_ms {
            let gap = hit.t_ms.saturating_sub(last);
            if gap <= active_gap_ms {
                ta.active_ms += gap;
            }
        }
        ta.last_hit_ms = Some(ta.last_hit_ms.unwrap_or(0).max(hit.t_ms));
        let bucket = hit.t_ms.saturating_sub(self.started_ms) / 1000;
        if bucket <= MAX_TIMELINE_S {
            add_bucket(&mut ta.timeline, bucket as u32, hit.amount);
        }

        self.select_main(hit.target, target_damage, hit.target_is_boss);
    }

    /// Keeps `main` on the boss with the most damage, else the most damaged target, in O(1).
    fn select_main(&mut self, target: EntityId, damage: u64, is_boss: bool) {
        let Some(main) = self.main else {
            self.main = Some(target);
            return;
        };
        if main == target {
            return;
        }
        let (main_is_boss, main_damage) = self.targets.get(&main).map(|t| (t.is_boss, t.damage)).unwrap_or((false, 0));
        let better = match (is_boss, main_is_boss) {
            (true, false) => true,
            (false, true) => false,
            _ => damage > main_damage,
        };
        if better {
            self.main = Some(target);
        }
    }

    fn recompute_main(&mut self) {
        self.main = self
            .targets
            .iter()
            .max_by_key(|(_, t)| (t.is_boss, t.damage, std::cmp::Reverse(t.order)))
            .map(|(id, _)| *id);
    }

    pub fn add_heal(&mut self, t_ms: u64, actor: EntityId, kind: AccKind, skill: u32, amount: u64) {
        self.last_event_ms = self.last_event_ms.max(t_ms);
        let acc = self.actors.entry(actor).or_insert_with(|| ActorAcc::new(kind));
        acc.healing += amount;
        *acc.heal_skills.entry(skill).or_default() += amount;
    }

    /// Counts a cast record; a repeat of the same skill within `CAST_DEDUP_MS` is the same use.
    pub fn add_cast(&mut self, t_ms: u64, actor: EntityId, kind: AccKind, skill: u32) {
        match self.last_casts.get(&(actor, skill)) {
            Some(last) if t_ms.saturating_sub(*last) <= CAST_DEDUP_MS => return,
            _ => {}
        }
        self.last_casts.insert((actor, skill), t_ms);
        let acc = self.actors.entry(actor).or_insert_with(|| ActorAcc::new(kind));
        acc.casts += 1;
        *acc.skill_casts.entry(skill).or_default() += 1;
    }

    /// Damage a player received from a mob.
    pub fn add_damage_taken(&mut self, target: EntityId, amount: u64) {
        let acc = self.actors.entry(target).or_insert_with(|| ActorAcc::new(AccKind::Player));
        acc.damage_taken += amount;
    }

    pub fn add_death(&mut self, t_ms: u64, id: EntityId, is_player: bool) {
        self.deaths.push(DeathRecord { t_ms, id, is_player });
        if is_player {
            self.actors.entry(id).or_insert_with(|| ActorAcc::new(AccKind::Player)).deaths += 1;
        }
    }

    /// Drops an actor whose damage must not count (an inferred player that turned out to be a mob).
    pub fn remove_actor(&mut self, id: EntityId) -> bool {
        let Some(acc) = self.actors.remove(&id) else { return false };
        for (target, ta) in &acc.targets {
            let empty = match self.targets.get_mut(target) {
                Some(tt) => {
                    tt.damage = tt.damage.saturating_sub(ta.total.damage);
                    tt.damage == 0
                }
                None => false,
            };
            if empty {
                self.targets.remove(target);
            }
        }
        self.recompute_main();
        true
    }
}

/// Adds damage to a one second bucket of a sparse timeline kept in time order.
fn add_bucket(timeline: &mut Vec<TimelinePoint>, t_s: u32, amount: u64) {
    match timeline.last_mut() {
        Some(p) if p.t_s == t_s => p.damage += amount,
        Some(p) if p.t_s > t_s => match timeline.binary_search_by_key(&t_s, |p| p.t_s) {
            Ok(i) => timeline[i].damage += amount,
            Err(i) => timeline.insert(i, TimelinePoint { t_s, damage: amount }),
        },
        _ => timeline.push(TimelinePoint { t_s, damage: amount }),
    }
}
