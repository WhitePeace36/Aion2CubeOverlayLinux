//! Map names, embedded at compile time from `data/` (the same tables as the aion2dps meter, see
//! docs/DATA_SOURCES.md), plus the open world zones identified in captures.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Open world map ids whose zone is known from captures (dr89mh FINDINGS P7). The dungeon table names
/// instances only.
const ZONES: &[(u32, &str)] = &[(1010, "Verteron")];

#[derive(Debug, Clone, Default, Deserialize)]
struct DungeonEntry {
    name: String,
    #[serde(default)]
    tier: Option<String>,
}

/// What the TUI shows for a map.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapInfo {
    pub id: u32,
    pub name: String,
    /// Difficulty or stage, for example "Conquest [Normal]".
    pub tier: Option<String>,
    pub is_instance: bool,
}

pub struct GameData {
    dungeons: HashMap<u32, DungeonEntry>,
    open_world: HashSet<u32>,
}

impl GameData {
    pub fn get() -> &'static GameData {
        static DATA: OnceLock<GameData> = OnceLock::new();
        DATA.get_or_init(|| {
            let raw: HashMap<String, DungeonEntry> = serde_json::from_str(include_str!("../data/dungeons.json")).expect("embedded table is valid JSON");
            let dungeons = raw.into_iter().filter_map(|(k, v)| k.parse::<u32>().ok().map(|k| (k, v))).collect();
            let ids: Vec<i64> = serde_json::from_str(include_str!("../data/open_world_maps.json")).expect("embedded table is valid JSON");
            let open_world = ids.into_iter().filter_map(|v| u32::try_from(v).ok()).collect();
            GameData { dungeons, open_world }
        })
    }

    /// Instance (dungeon) map ids live in 600000..699999; Nightmare, Raid and Ascension ids are instances too.
    pub fn is_instance(&self, map_id: u32) -> bool {
        (600_000..=699_999).contains(&map_id)
            || map_id == 200_003
            || (910_000..=919_999).contains(&map_id)
            || (self.dungeons.contains_key(&map_id) && !self.open_world.contains(&map_id))
    }

    /// The zone name of an open world map, when known.
    pub fn zone(&self, map_id: u32) -> Option<&'static str> {
        ZONES.iter().find(|(id, _)| *id == map_id).map(|(_, name)| *name)
    }

    pub fn map(&self, map_id: u32) -> MapInfo {
        let is_instance = self.is_instance(map_id);
        if let Some(zone) = self.zone(map_id) {
            return MapInfo { id: map_id, name: zone.into(), tier: None, is_instance };
        }
        match self.dungeons.get(&map_id) {
            Some(d) => MapInfo { id: map_id, name: d.name.clone(), tier: d.tier.clone(), is_instance },
            None => {
                let name = if self.open_world.contains(&map_id) {
                    format!("Open world {map_id}")
                } else if is_instance {
                    format!("Instance {map_id}")
                } else {
                    format!("Map {map_id}")
                };
                MapInfo { id: map_id, name, tier: None, is_instance }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_zones_and_dungeons() {
        let gd = GameData::get();
        let verteron = gd.map(1010);
        assert_eq!(verteron.name, "Verteron");
        assert!(!verteron.is_instance);
        assert_eq!(gd.zone(1010), Some("Verteron"));
        let m = gd.map(600_072);
        assert_eq!(m.name, "Vakron Sky Island");
        assert!(m.is_instance);
        assert_eq!(gd.map(1110).name, "Open world 1110");
        assert_eq!(gd.map(4242).name, "Map 4242");
    }
}
