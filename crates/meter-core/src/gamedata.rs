//! Game data tables: NPC codes, skill ids, classes, dungeons. Embedded at compile time from `data/`.
//!
//! Sources and licenses are listed in `docs/DATA_SOURCES.md`. Lookups never fail: unknown codes get a
//! `#code` placeholder so the UI always has something to show.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NpcInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_boss: bool,
    #[serde(default)]
    pub is_dummy: bool,
    #[serde(default)]
    pub dungeon_id: Option<u32>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassInfo {
    pub id: u32,
    pub name: String,
    pub name_ko: String,
    pub icon_code: String,
    /// Inclusive range of the job byte in identity and roster packets.
    pub job_bytes: [u32; 2],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DungeonInfo {
    pub name: String,
    #[serde(default)]
    pub difficulty: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
}

pub struct GameData {
    npcs: HashMap<u32, NpcInfo>,
    skills: HashMap<u32, String>,
    skill_icons: HashMap<u32, String>,
    classes: Vec<ClassInfo>,
    dot_skills: HashSet<u32>,
    heal_skills: HashSet<u32>,
    resource_skills: HashSet<u32>,
    dungeons: HashMap<u32, DungeonInfo>,
    open_world_maps: HashSet<u32>,
    player_summon_npcs: HashSet<u32>,
}

static DATA: OnceLock<GameData> = OnceLock::new();

fn parse_map<T: for<'de> Deserialize<'de>>(json: &str) -> HashMap<u32, T> {
    let raw: HashMap<String, T> = serde_json::from_str(json).expect("embedded table is valid JSON");
    raw.into_iter().filter_map(|(k, v)| k.parse::<u32>().ok().map(|k| (k, v))).collect()
}

fn parse_set(json: &str) -> HashSet<u32> {
    let raw: Vec<u64> = serde_json::from_str(json).expect("embedded id list is valid JSON");
    raw.into_iter().filter_map(|v| u32::try_from(v).ok()).collect()
}

impl GameData {
    pub fn get() -> &'static GameData {
        DATA.get_or_init(GameData::load_embedded)
    }

    fn load_embedded() -> GameData {
        GameData {
            npcs: parse_map(include_str!("../data/npcs.json")),
            skills: parse_map(include_str!("../data/skills.json")),
            skill_icons: parse_map(include_str!("../data/skill_icons.json")),
            classes: serde_json::from_str(include_str!("../data/classes.json")).expect("classes.json"),
            dot_skills: parse_set(include_str!("../data/dot_skill_ids.json")),
            heal_skills: parse_set(include_str!("../data/heal_skill_ids.json")),
            resource_skills: parse_set(include_str!("../data/resource_skill_ids.json")),
            dungeons: parse_map(include_str!("../data/dungeons.json")),
            open_world_maps: parse_set(include_str!("../data/open_world_maps.json")),
            player_summon_npcs: parse_set(include_str!("../data/player_summon_npcs.json")),
        }
    }

    // NPCs

    pub fn npc(&self, code: u32) -> Option<&NpcInfo> {
        self.npcs.get(&code)
    }

    pub fn npc_name(&self, code: u32) -> String {
        match self.npcs.get(&code) {
            Some(n) if !n.name.is_empty() => n.name.clone(),
            _ => format!("#{code}"),
        }
    }

    pub fn is_boss(&self, code: u32) -> bool {
        self.npcs.get(&code).map(|n| n.is_boss).unwrap_or(false)
    }

    pub fn is_dummy(&self, code: u32) -> bool {
        self.npcs.get(&code).map(|n| n.is_dummy).unwrap_or(false)
    }

    /// NPC templates that a player can summon (pets, spirits, turrets). Anything else is never a summon.
    pub fn is_player_summon_npc(&self, code: u32) -> bool {
        self.player_summon_npcs.contains(&code)
    }

    pub fn dungeon_of_npc(&self, code: u32) -> Option<u32> {
        self.npcs.get(&code).and_then(|n| n.dungeon_id)
    }

    // Skills

    /// Name for a skill id as it appears in packets: exact, then the 8 digit base code, then the theostone
    /// form, then a placeholder.
    pub fn skill_name(&self, id: u32) -> String {
        if let Some(n) = self.skills.get(&id) {
            return n.clone();
        }
        let base = Self::base_skill(id);
        if base != id {
            if let Some(n) = self.skills.get(&base) {
                return n.clone();
            }
        }
        if (3_000_000..=3_099_999).contains(&id) {
            if let Some(n) = self.skills.get(&(id * 10 + 1)) {
                return n.clone();
            }
        }
        format!("#{id}")
    }

    pub fn has_skill(&self, id: u32) -> bool {
        self.skills.contains_key(&id)
    }

    /// 8 digit class skill codes carry a specialisation suffix in the last four digits; the base is the
    /// code rounded down to 10,000. Other codes are their own base.
    pub fn base_skill(id: u32) -> u32 {
        if (10_000_000..=29_999_999).contains(&id) {
            id - id % 10_000
        } else {
            id
        }
    }

    /// Specialisation slots (1..5) encoded in the suffix digits of an 8 digit skill code.
    pub fn spec_slots(id: u32) -> Vec<u8> {
        if !(10_000_000..=29_999_999).contains(&id) {
            return Vec::new();
        }
        let mut suffix = (id % 10_000) / 10;
        let mut slots = Vec::new();
        while suffix > 0 {
            let d = (suffix % 10) as u8;
            if !(1..=5).contains(&d) {
                return Vec::new();
            }
            slots.push(d);
            suffix /= 10;
        }
        slots.reverse();
        slots
    }

    /// Icon file stem for a skill, keyed by the first four digits of an 8 digit code, else derived from the
    /// class icon code and skill number.
    pub fn skill_icon(&self, id: u32) -> Option<String> {
        if !(10_000_000..=29_999_999).contains(&id) {
            return None;
        }
        let key = id / 10_000;
        if let Some(stem) = self.skill_icons.get(&key) {
            return Some(stem.clone());
        }
        let class_id = id / 1_000_000;
        let sub = key % 100;
        let class = self.classes.iter().find(|c| c.id == class_id)?;
        if sub >= 70 {
            Some(format!("Passive_ICON_{}_SKILL_{:03}", class.icon_code, sub - 70))
        } else {
            Some(format!("ICON_{}_SKILL_{:03}", class.icon_code, sub))
        }
    }

    pub fn is_dot_skill(&self, id: u32) -> bool {
        self.dot_skills.contains(&id)
    }

    pub fn is_heal_skill(&self, id: u32) -> bool {
        self.heal_skills.contains(&Self::base_skill(id)) || self.heal_skills.contains(&id)
    }

    pub fn restores_resource(&self, id: u32) -> bool {
        self.resource_skills.contains(&id) || self.resource_skills.contains(&Self::base_skill(id))
    }

    // Classes

    pub fn classes(&self) -> &[ClassInfo] {
        &self.classes
    }

    pub fn class_name(&self, class_id: u32) -> Option<&str> {
        self.classes.iter().find(|c| c.id == class_id).map(|c| c.name.as_str())
    }

    /// Class from the job value in identity and party packets (blocks of four per class).
    pub fn class_from_job(&self, job: u32) -> Option<u32> {
        self.classes.iter().find(|c| job >= c.job_bytes[0] && job <= c.job_bytes[1]).map(|c| c.id)
    }

    /// Class a skill belongs to, when the code identifies one. Shared skills with skill number 00 and
    /// pet skills in the Elementalist prefix do not identify a class.
    pub fn class_from_skill(&self, id: u32) -> Option<u32> {
        if (11_000_000..=19_999_999).contains(&id) {
            let class_id = id / 1_000_000;
            let sub = (id / 10_000) % 100;
            if sub == 0 {
                return None;
            }
            if class_id == 16 {
                const ELEMENTALIST_SUBS: &[u32] =
                    &[1, 2, 3, 4, 5, 6, 7, 8, 14, 15, 17, 19, 21, 22, 23, 24, 25, 26, 30, 31, 32, 34, 35, 36, 37, 70, 71, 72, 73, 74, 75, 76, 80];
                if !ELEMENTALIST_SUBS.contains(&sub) {
                    return None;
                }
            }
            return Some(class_id);
        }
        // Spirit skills belong to the Elementalist.
        if (100_510..=103_500).contains(&id) || (109_300..=109_362).contains(&id) {
            return Some(16);
        }
        None
    }

    // Maps

    pub fn dungeon(&self, map_id: u32) -> Option<&DungeonInfo> {
        self.dungeons.get(&map_id)
    }

    pub fn map_name(&self, map_id: u32) -> String {
        match self.dungeons.get(&map_id) {
            Some(d) => d.name.clone(),
            None if self.is_open_world_map(map_id) => format!("Open world {map_id}"),
            None => format!("Map {map_id}"),
        }
    }

    /// Instance (dungeon) map ids live in 600000..699999; Nightmare, Raid and Ascension ids are also instances.
    pub fn is_instance_map(&self, map_id: u32) -> bool {
        (600_000..=699_999).contains(&map_id)
            || map_id == 200_003
            || (910_000..=919_999).contains(&map_id)
            || (self.dungeons.contains_key(&map_id) && !self.open_world_maps.contains(&map_id))
    }

    pub fn is_open_world_map(&self, map_id: u32) -> bool {
        self.open_world_maps.contains(&map_id)
    }

    pub fn counts(&self) -> (usize, usize, usize, usize) {
        (self.npcs.len(), self.skills.len(), self.dot_skills.len(), self.dungeons.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_load_and_look_up() {
        let d = GameData::get();
        let (npcs, skills, dots, dungeons) = d.counts();
        assert!(npcs > 9_000 && skills > 9_000 && dots > 2_000 && dungeons >= 60, "{npcs} {skills} {dots} {dungeons}");
        assert_eq!(d.npc_name(2300243), "Awakened Alturadon");
        assert!(d.is_boss(2300243));
        assert!(d.is_dummy(2000000));
        assert!(!d.is_boss(2000002));
        assert_eq!(d.npc_name(1), "#1");
        assert_eq!(d.skill_name(11020000), "Keen Strike");
        assert_eq!(d.skill_name(11020047), d.skill_name(11020000));
        assert_eq!(d.class_name(11), Some("Gladiator"));
        assert_eq!(d.class_from_job(15), Some(14));
        assert_eq!(d.class_from_job(46), Some(19));
        assert_eq!(d.class_from_skill(11020047), Some(11));
        assert_eq!(d.class_from_skill(12000001), None);
        assert_eq!(d.dungeon(600143).map(|x| x.name.as_str()), Some("Azure Breath Island"));
        assert!(d.is_instance_map(600072));
        assert!(!d.is_instance_map(1110));
    }

    #[test]
    fn skill_helpers() {
        assert_eq!(GameData::base_skill(11010047), 11010000);
        assert_eq!(GameData::base_skill(100011), 100011);
        assert_eq!(GameData::spec_slots(11010230), vec![2, 3]);
        assert_eq!(GameData::spec_slots(11010047), vec![4]);
        assert_eq!(GameData::spec_slots(11010000), Vec::<u8>::new());
        let d = GameData::get();
        assert!(d.skill_icon(11020000).is_some());
        assert!(d.is_heal_skill(18120047));
    }
}
