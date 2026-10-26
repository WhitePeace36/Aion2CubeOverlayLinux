//! Cube spots: every place a cube was seen, per map.
//!
//! Cubes spawn at fixed spots (the community maps list about 3 700), so the spots seen over a few sessions draw
//! your own map of where to look, without any third party map data. A spot is a map and a position; a later
//! sighting within a few metres is the same spot. The terminal app saves the book in `spots.json`.

use crate::geo::Vec3;
use serde::{Deserialize, Serialize};

/// Two sightings this close (planar, in game units) are the same spot.
pub const SAME_SPOT_UNITS: f32 = 300.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spot {
    pub map_id: u32,
    /// Template of the latest cube seen here.
    pub template: u32,
    pub pos: Vec3,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// Separate cubes seen here; a cube coming back into view is not counted again.
    pub sightings: u32,
    #[serde(default)]
    pub opened_by_you: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SpotBook {
    spots: Vec<Spot>,
    changed: bool,
}

impl SpotBook {
    pub fn new(spots: Vec<Spot>) -> SpotBook {
        SpotBook { spots, changed: false }
    }

    fn find(&mut self, map_id: u32, pos: &Vec3) -> Option<&mut Spot> {
        self.spots.iter_mut().find(|s| s.map_id == map_id && s.pos.planar_distance(pos) <= SAME_SPOT_UNITS)
    }

    /// Records a cube seen at `pos`. `new_cube` counts a sighting; a cube back in view only refreshes the time.
    pub fn sighting(&mut self, map_id: u32, template: u32, pos: Vec3, t_ms: u64, new_cube: bool) {
        match self.find(map_id, &pos) {
            Some(s) => {
                s.template = template;
                s.pos = pos;
                s.last_seen_ms = s.last_seen_ms.max(t_ms);
                if new_cube {
                    s.sightings += 1;
                }
            }
            None => self.spots.push(Spot { map_id, template, pos, first_seen_ms: t_ms, last_seen_ms: t_ms, sightings: 1, opened_by_you: 0 }),
        }
        self.changed = true;
    }

    pub fn opened_by_you(&mut self, map_id: u32, pos: &Vec3) {
        if let Some(s) = self.find(map_id, pos) {
            s.opened_by_you += 1;
            self.changed = true;
        }
    }

    pub fn for_map(&self, map_id: u32) -> Vec<Spot> {
        self.spots.iter().filter(|s| s.map_id == map_id).cloned().collect()
    }

    pub fn all(&self) -> &[Spot] {
        &self.spots
    }

    /// Every spot when something changed since the last call, for saving.
    pub fn take_changed(&mut self) -> Option<Vec<Spot>> {
        std::mem::take(&mut self.changed).then(|| self.spots.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearby_sightings_are_one_spot() {
        let mut book = SpotBook::default();
        book.sighting(1010, 1_200_039, Vec3::new(-50_205.0, 126_283.0, 15_449.0), 1_000, true);
        book.sighting(1010, 1_200_041, Vec3::new(-50_105.0, 126_283.0, 15_449.0), 2_000, true);
        book.sighting(1010, 1_200_041, Vec3::new(-50_105.0, 126_283.0, 15_449.0), 3_000, false);
        book.sighting(1110, 1_301_199, Vec3::new(-50_105.0, 126_283.0, 15_449.0), 4_000, true);
        assert_eq!(book.all().len(), 2, "another map is another spot");
        let s = &book.for_map(1010)[0];
        assert_eq!((s.sightings, s.template, s.first_seen_ms, s.last_seen_ms), (2, 1_200_041, 1_000, 3_000));
        book.opened_by_you(1010, &Vec3::new(-50_000.0, 126_300.0, 15_400.0));
        assert_eq!(book.for_map(1010)[0].opened_by_you, 1);
        assert!(book.take_changed().is_some());
        assert!(book.take_changed().is_none(), "nothing new since");
    }

    #[test]
    fn spots_serialise_for_the_saved_file() {
        let spot = Spot { map_id: 1010, template: 7, pos: Vec3::new(1.0, 2.0, 3.0), first_seen_ms: 5, last_seen_ms: 6, sightings: 1, opened_by_you: 0 };
        let json = serde_json::to_string(&spot).unwrap();
        assert!(json.contains(r#""mapId":1010"#), "{json}");
        assert_eq!(serde_json::from_str::<Spot>(&json).unwrap(), spot);
    }
}
