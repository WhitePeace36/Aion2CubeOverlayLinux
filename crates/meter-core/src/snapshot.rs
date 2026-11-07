//! The read model the UI renders: one `Snapshot` per encounter, produced by the combat tracker.
//!
//! Everything here is plain data, serialised camelCase, mirrored in `src/shared/types.ts`. Keep the two in sync.

use crate::parser::EntityId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    BossDied,
    Wipe,
    Idle,
    ZoneChange,
    Manual,
    DummyCap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: u64,
    pub active: bool,
    pub end_reason: Option<EndReason>,
    /// Capture time of the first damage event, Unix ms.
    pub started_ms: u64,
    /// Capture time of the last damage or heal event.
    pub last_event_ms: u64,
    /// Last counted hit on the main target minus `started_ms` (hits on adds and heals do not extend it),
    /// floored at 1 s for the rates, frozen once the encounter ends.
    pub duration_ms: u64,
    pub map_id: Option<u32>,
    pub map_name: Option<String>,
    pub is_instance: bool,
    /// The main target: the boss with the most damage taken, else the most damaged target.
    pub boss: Option<TargetInfo>,
    pub targets: Vec<TargetInfo>,
    /// Damage by players (and their summons) on all targets.
    pub total_damage: u64,
    /// Damage by players on the main target divided by the encounter duration.
    pub party_dps: f64,
    /// Sorted by damage, highest first.
    pub actors: Vec<ActorStats>,
    pub deaths: Vec<DeathInfo>,
    pub self_id: Option<EntityId>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub id: EntityId,
    pub npc_code: Option<u32>,
    pub name: String,
    pub is_boss: bool,
    pub is_dummy: bool,
    pub cur_hp: Option<u64>,
    pub max_hp: Option<u64>,
    pub hp_pct: Option<f64>,
    pub damage_taken: u64,
    pub dead: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorStats {
    pub id: EntityId,
    pub name: String,
    pub class_id: Option<u32>,
    pub class_name: Option<String>,
    pub level: Option<u32>,
    pub is_self: bool,
    pub is_party: bool,
    pub is_player: bool,
    pub combat_power: Option<u64>,
    pub gear_score: Option<u32>,
    /// Damage on the main target (summons folded in).
    pub damage: u64,
    /// Damage on every target.
    pub damage_all_targets: u64,
    pub dps: f64,
    /// Milliseconds this actor was dealing damage (gaps over 5 s excluded).
    pub active_ms: u64,
    pub active_dps: f64,
    /// Share of the party's damage on the main target, 0..1.
    pub share: f64,
    pub casts: u64,
    pub hits: u64,
    pub crits: u64,
    pub crit_rate: f64,
    pub back_rate: f64,
    pub front_rate: f64,
    pub parry_rate: f64,
    pub perfect_rate: f64,
    pub double_rate: f64,
    pub multi_rate: f64,
    pub max_hit: u64,
    pub healing: u64,
    pub hps: f64,
    pub damage_taken: u64,
    pub deaths: u32,
    /// Sorted by damage, highest first.
    pub skills: Vec<SkillStats>,
    pub targets: Vec<TargetDamage>,
    /// One second buckets of damage on the main target, from the encounter start.
    pub timeline: Vec<TimelinePoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillStats {
    pub skill_id: u32,
    pub name: String,
    pub icon: Option<String>,
    pub damage: u64,
    /// Share of this actor's damage, 0..1.
    pub share: f64,
    pub casts: u64,
    pub hits: u64,
    pub crits: u64,
    pub crit_rate: f64,
    pub max_hit: u64,
    pub min_hit: u64,
    pub avg_hit: f64,
    pub back: u64,
    pub front: u64,
    pub parry: u64,
    pub perfect: u64,
    pub double: u64,
    pub multi: u64,
    pub dot: bool,
    pub healing: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDamage {
    pub id: EntityId,
    pub name: String,
    pub damage: u64,
    pub share: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePoint {
    pub t_s: u32,
    pub damage: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeathInfo {
    pub t_ms: u64,
    pub id: EntityId,
    pub name: String,
    pub is_player: bool,
}

/// Compact form for history lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncounterSummary {
    pub id: u64,
    pub started_ms: u64,
    pub duration_ms: u64,
    pub map_name: Option<String>,
    pub boss_name: Option<String>,
    pub is_boss: bool,
    pub end_reason: Option<EndReason>,
    pub total_damage: u64,
    pub party_dps: f64,
    pub actors: Vec<ActorBrief>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorBrief {
    pub name: String,
    pub class_name: Option<String>,
    pub damage: u64,
    pub dps: f64,
    pub share: f64,
}

impl Snapshot {
    pub fn summary(&self) -> EncounterSummary {
        EncounterSummary {
            id: self.id,
            started_ms: self.started_ms,
            duration_ms: self.duration_ms,
            map_name: self.map_name.clone(),
            boss_name: self.boss.as_ref().map(|b| b.name.clone()),
            is_boss: self.boss.as_ref().map(|b| b.is_boss).unwrap_or(false),
            end_reason: self.end_reason,
            total_damage: self.total_damage,
            party_dps: self.party_dps,
            actors: self
                .actors
                .iter()
                .filter(|a| a.is_player)
                .map(|a| ActorBrief {
                    name: a.name.clone(),
                    class_name: a.class_name.clone(),
                    damage: a.damage,
                    dps: a.dps,
                    share: a.share,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_camel_case() {
        let s = Snapshot { id: 3, party_dps: 12.5, ..Default::default() };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"partyDps\":12.5"));
        assert!(json.contains("\"endReason\":null"));
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn end_reason_is_snake_case() {
        assert_eq!(serde_json::to_string(&EndReason::BossDied).unwrap(), "\"boss_died\"");
    }
}
