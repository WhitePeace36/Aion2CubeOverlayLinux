//! Builds the `Snapshot` read model from an encounter's accumulators and the entity registry.
//!
//! Stats are keyed by entity id while the fight runs; names, classes and ownership are resolved here.
//! An identity that arrives late still names the earlier damage, a player who changed id keeps one
//! row (rows are grouped by name), and summons fold into their owner.

use super::encounter::{AccKind, ActorAcc, Encounter, TargetAcc};
use super::entities::Registry;
use crate::gamedata::GameData;
use crate::parser::EntityId;
use crate::snapshot::{ActorStats, DeathInfo, SkillStats, Snapshot, TargetDamage, TargetInfo, TimelinePoint};
use std::collections::{BTreeMap, HashMap};

/// Durations are floored here for every per second rate.
pub const MIN_DURATION_MS: u64 = 1_000;

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 / b as f64
    }
}

/// The accumulators that make up one actor row.
struct Row<'a> {
    id: EntityId,
    name: String,
    accs: Vec<&'a ActorAcc>,
}

/// Names for the player ids of an encounter that no identity has named: the one roster member of the
/// same class whose name has no id yet, when that id is the only unnamed player of the class in the
/// fight. This is the meter started mid session case (session1 12:33: three party members fought as
/// #8173, #2737 and #15079 because their identities predate the capture while the roster listed them
/// with their classes). Only inside an instance, where nobody but the party is around, or before the
/// first map load (the map is unknown, which is itself the mid session signature): in the open world a
/// stranger of the same class could take the name.
fn roster_fallback_names(enc: &Encounter, reg: &Registry) -> HashMap<EntityId, String> {
    let mut out = HashMap::new();
    if !(enc.is_instance || enc.map_id.is_none()) {
        return out;
    }
    let mut unnamed_by_class: HashMap<u32, Vec<EntityId>> = HashMap::new();
    for (id, acc) in &enc.actors {
        if acc.kind != AccKind::Player || reg.name_of(*id).is_some() {
            continue;
        }
        if let Some(class) = reg.class_of(*id).or(acc.class_hint) {
            unnamed_by_class.entry(class).or_default().push(*id);
        }
    }
    for (class, ids) in unnamed_by_class {
        if let ([id], [name]) = (ids.as_slice(), reg.unidentified_party_members(class).as_slice()) {
            out.insert(*id, (*name).to_owned());
        }
    }
    out
}

pub fn build(enc: &Encounter, reg: &Registry, active: bool) -> Snapshot {
    let data = GameData::get();
    let main = enc.main;
    let duration_ms = enc.duration_ms();
    let per_s = 1000.0 / duration_ms.max(MIN_DURATION_MS) as f64;

    let fallback = roster_fallback_names(enc, reg);
    let fallback_of_class = |class: u32| -> Option<EntityId> {
        let mut found: Vec<EntityId> = fallback.keys().copied().filter(|id| reg.class_of(*id).or_else(|| enc.actors.get(id).and_then(|a| a.class_hint)) == Some(class)).collect();
        found.sort_unstable();
        match found.as_slice() {
            [id] => Some(*id),
            _ => None,
        }
    };

    let mut rows: Vec<Row> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (id, acc) in &enc.actors {
        let owner = match acc.kind {
            AccKind::Player => Some(*id),
            AccKind::Summon => reg.resolve_owner(*id).or_else(|| {
                let hint = acc.class_hint.or_else(|| reg.entity(*id).and_then(|e| e.class_hint));
                hint.and_then(|c| reg.only_player_of_class(c).or_else(|| fallback_of_class(c)))
            }),
        };
        // An orphan summon (its owner chain ends nowhere and its class has no single named player)
        // stays visible under its own id rather than vanishing while its damage sits in the target's
        // total (PROTOCOL.md D106, D124); a later link or identity folds it into the owner.
        let owner = owner.unwrap_or(*id);
        let (name, display_id) = match reg.name_of(owner) {
            Some(n) => (n.to_owned(), reg.current_id(n).unwrap_or(owner)),
            None => match fallback.get(&owner) {
                Some(n) => (n.clone(), owner),
                None => (format!("#{owner}"), owner),
            },
        };
        let slot = match index.get(&name) {
            Some(i) => *i,
            None => {
                index.insert(name.clone(), rows.len());
                rows.push(Row { id: display_id, name, accs: Vec::new() });
                rows.len() - 1
            }
        };
        rows[slot].accs.push(acc);
    }

    let mut actors: Vec<ActorStats> = rows.iter().map(|row| build_actor(row, enc, reg, data, per_s)).collect();
    let party_main: u64 = actors.iter().map(|a| a.damage).sum();
    let total_damage: u64 = actors.iter().map(|a| a.damage_all_targets).sum();
    for a in &mut actors {
        a.share = ratio(a.damage, party_main);
    }
    actors.sort_by(|a, b| b.damage.cmp(&a.damage).then(b.damage_all_targets.cmp(&a.damage_all_targets)).then(a.name.cmp(&b.name)));

    let mut targets: Vec<TargetInfo> = enc
        .targets
        .iter()
        .map(|(id, tt)| {
            let e = reg.entity(*id);
            let dead = e.map(|e| e.dead).unwrap_or(false) || enc.deaths.iter().any(|d| d.id == *id && !d.is_player);
            let max_hp = e.and_then(|e| e.max_hp);
            let cur_hp = if dead { Some(0) } else { e.and_then(|e| e.cur_hp) };
            let hp_pct = match (cur_hp, max_hp) {
                (Some(c), Some(m)) if m > 0 => Some((c as f64 / m as f64).clamp(0.0, 1.0)),
                _ => None,
            };
            TargetInfo {
                id: *id,
                npc_code: e.and_then(|e| e.npc_code),
                name: reg.display_name(*id),
                is_boss: tt.is_boss,
                is_dummy: tt.is_dummy,
                cur_hp,
                max_hp,
                hp_pct,
                damage_taken: tt.damage,
                dead,
            }
        })
        .collect();
    targets.sort_by(|a, b| b.is_boss.cmp(&a.is_boss).then(b.damage_taken.cmp(&a.damage_taken)).then(a.id.cmp(&b.id)));
    let boss = main.and_then(|m| targets.iter().find(|t| t.id == m).cloned());

    let deaths = enc
        .deaths
        .iter()
        .map(|d| DeathInfo {
            t_ms: d.t_ms,
            id: d.id,
            name: fallback.get(&d.id).cloned().unwrap_or_else(|| reg.display_name(d.id)),
            is_player: d.is_player,
        })
        .collect();

    Snapshot {
        id: enc.id,
        active,
        end_reason: enc.ended.map(|(r, _)| r),
        started_ms: enc.started_ms,
        last_event_ms: enc.last_event_ms,
        duration_ms,
        map_id: enc.map_id,
        map_name: enc.map_name.clone(),
        is_instance: enc.is_instance,
        boss,
        targets,
        total_damage,
        party_dps: party_main as f64 * per_s,
        actors,
        deaths,
        self_id: reg.self_id(),
    }
}

fn build_actor(row: &Row, enc: &Encounter, reg: &Registry, data: &GameData, per_s: f64) -> ActorStats {
    let main = enc.main;
    let mut on_main = TargetAcc::default();
    let mut buckets: BTreeMap<u32, u64> = BTreeMap::new();
    let mut per_target: HashMap<EntityId, u64> = HashMap::new();
    let mut skill_casts: HashMap<u32, u64> = HashMap::new();
    let mut heal_skills: HashMap<u32, u64> = HashMap::new();
    let (mut damage_all, mut casts, mut healing, mut damage_taken, mut deaths) = (0u64, 0u64, 0u64, 0u64, 0u32);
    let mut class_hint = None;
    for acc in &row.accs {
        damage_all += acc.damage_all;
        casts += acc.casts;
        healing += acc.healing;
        damage_taken += acc.damage_taken;
        deaths += acc.deaths;
        class_hint = class_hint.or(acc.class_hint);
        for (k, v) in &acc.skill_casts {
            *skill_casts.entry(*k).or_default() += v;
        }
        for (k, v) in &acc.heal_skills {
            *heal_skills.entry(*k).or_default() += v;
        }
        for (tid, ta) in &acc.targets {
            *per_target.entry(*tid).or_default() += ta.total.damage;
            if Some(*tid) == main {
                on_main.total.merge(&ta.total);
                for (k, s) in &ta.skills {
                    on_main.skills.entry(*k).or_default().merge(s);
                }
                for p in &ta.timeline {
                    *buckets.entry(p.t_s).or_default() += p.damage;
                }
                on_main.active_ms += ta.active_ms;
            }
        }
    }

    let total = &on_main.total;
    let damage = total.damage;
    let direct = total.direct_hits;
    let active_ms = on_main.active_ms.min(enc.duration_ms());

    let mut skills: Vec<SkillStats> = on_main
        .skills
        .iter()
        .map(|((base, dot), s)| {
            let mut name = data.skill_name(*base);
            if *dot {
                name.push_str(" (DoT)");
            }
            SkillStats {
                skill_id: *base,
                name,
                icon: data.skill_icon(*base),
                damage: s.damage,
                share: ratio(s.damage, damage),
                casts: if *dot { 0 } else { skill_casts.get(base).copied().unwrap_or(0) },
                hits: s.hits,
                crits: s.crits,
                crit_rate: ratio(s.crits, s.direct_hits),
                max_hit: s.max_hit,
                min_hit: if s.hits > 0 { s.min_hit } else { 0 },
                avg_hit: if s.hits > 0 { s.damage as f64 / s.hits as f64 } else { 0.0 },
                back: s.back,
                front: s.front,
                parry: s.parry,
                perfect: s.perfect,
                double: s.double,
                multi: s.multi,
                dot: *dot,
                healing: if *dot { 0 } else { heal_skills.get(base).copied().unwrap_or(0) },
            }
        })
        .collect();
    for (base, amount) in &heal_skills {
        if !skills.iter().any(|s| s.skill_id == *base && !s.dot) {
            skills.push(SkillStats {
                skill_id: *base,
                name: data.skill_name(*base),
                icon: data.skill_icon(*base),
                casts: skill_casts.get(base).copied().unwrap_or(0),
                healing: *amount,
                ..Default::default()
            });
        }
    }
    skills.sort_by(|a, b| b.damage.cmp(&a.damage).then(b.healing.cmp(&a.healing)).then(a.skill_id.cmp(&b.skill_id)));

    let mut targets: Vec<TargetDamage> = per_target
        .iter()
        .map(|(id, d)| TargetDamage { id: *id, name: reg.display_name(*id), damage: *d, share: ratio(*d, damage_all) })
        .collect();
    targets.sort_by(|a, b| b.damage.cmp(&a.damage).then(a.id.cmp(&b.id)));

    let timeline: Vec<TimelinePoint> = buckets.into_iter().map(|(t_s, damage)| TimelinePoint { t_s, damage }).collect();

    let member = reg.party_member(&row.name);
    let entity = reg.entity(row.id);
    let is_self = reg.self_id() == Some(row.id) || reg.self_name() == Some(row.name.as_str());
    let class_id = member
        .filter(|m| m.class_id != 0)
        .map(|m| m.class_id)
        .or_else(|| reg.class_of(row.id))
        .or(class_hint);
    let level = member.filter(|m| m.level > 0).map(|m| m.level).or_else(|| entity.and_then(|e| e.level));

    ActorStats {
        id: row.id,
        name: row.name.clone(),
        class_id,
        class_name: class_id.and_then(|c| data.class_name(c)).map(str::to_owned),
        level,
        is_self,
        is_party: member.is_some(),
        is_player: true,
        combat_power: member.map(|m| m.combat_power),
        gear_score: member.map(|m| m.gear_score),
        damage,
        damage_all_targets: damage_all,
        dps: damage as f64 * per_s,
        active_ms,
        active_dps: damage as f64 * 1000.0 / active_ms.max(MIN_DURATION_MS) as f64,
        share: 0.0,
        casts,
        hits: total.hits,
        crits: total.crits,
        crit_rate: ratio(total.crits, direct),
        back_rate: ratio(total.back, direct),
        front_rate: ratio(total.front, direct),
        parry_rate: ratio(total.parry, direct),
        perfect_rate: ratio(total.perfect, direct),
        double_rate: ratio(total.double, direct),
        multi_rate: ratio(total.multi, direct),
        max_hit: total.max_hit,
        healing,
        hps: healing as f64 * per_s,
        damage_taken,
        deaths,
        skills,
        targets,
        timeline,
    }
}

/// Compact text for the replay tool and the copy summary: boss, map, duration and end reason on the
/// first line, then one line per actor.
pub fn describe(snap: &Snapshot) -> String {
    let boss = match &snap.boss {
        Some(b) => {
            let hp = match (b.cur_hp, b.max_hp) {
                (Some(c), Some(m)) if m > 0 => format!(" {c}/{m} ({:.0}%)", c as f64 / m as f64 * 100.0),
                _ => String::new(),
            };
            format!("{}{hp}{}", b.name, if b.dead { " dead" } else { "" })
        }
        None => "no target".to_owned(),
    };
    let map = snap.map_name.clone().unwrap_or_else(|| "unknown map".to_owned());
    let instance = if snap.is_instance { " (instance)" } else { "" };
    let end = match snap.end_reason {
        Some(r) => serde_json::to_string(&r).unwrap_or_default().trim_matches('"').to_owned(),
        None if snap.active => "live".to_owned(),
        None => "ended".to_owned(),
    };
    let mut out = format!(
        "#{} {boss} | {map}{instance} | {} | {end} | party dps {:.0}\n",
        snap.id,
        format_duration(snap.duration_ms),
        snap.party_dps
    );
    for a in &snap.actors {
        out.push_str(&format!(
            "  {:<18} {:<12} {:>12} dmg {:>10.0} dps {:>5.1}%\n",
            a.name,
            a.class_name.as_deref().unwrap_or("?"),
            a.damage,
            a.dps,
            a.share * 100.0
        ));
    }
    out
}

/// `m:ss`, or `h:mm:ss` past an hour.
pub fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
