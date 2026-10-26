//! World positions, distances and compass directions.
//!
//! Game units are centimetres (Unreal): the character stands about 200 units from a cube while opening it. The
//! server decides what is "nearby" with a planar distance (cubes arrive at about 10 000 units, 100 m, whatever
//! the height difference), so the radar does too. North is world -Y and east is +X, as Cube Watch and the dr89mh
//! radar read the map; that orientation is not confirmed in game yet.

use serde::{Deserialize, Serialize};

pub const UNITS_PER_METRE: f32 = 100.0;
/// Positions outside these bounds are misreads.
const MAX_PLANAR: f32 = 420_000.0;
const MAX_HEIGHT: f32 = 120_000.0;
/// Some records carry a small vector instead of a world position; nothing in the world sits this close to 0.
const MIN_FROM_ORIGIN: f32 = 2_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { x, y, z }
    }

    /// Finite and inside the map bounds. Object spawns are checked with this.
    pub fn in_bounds(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite() && self.x.abs() < MAX_PLANAR && self.y.abs() < MAX_PLANAR && self.z.abs() < MAX_HEIGHT
    }

    /// In bounds and not a small vector near the origin. Position fixes of the character are checked with this.
    pub fn is_world_position(&self) -> bool {
        self.in_bounds() && self.x.hypot(self.y) > MIN_FROM_ORIGIN
    }

    /// Distance on the map, ignoring height, in game units.
    pub fn planar_distance(&self, other: &Vec3) -> f32 {
        (other.x - self.x).hypot(other.y - self.y)
    }

    /// Compass bearing from `self` to `other` in degrees, 0 north, 90 east.
    pub fn bearing_to(&self, other: &Vec3) -> f32 {
        let north = -(other.y - self.y);
        let east = other.x - self.x;
        let deg = east.atan2(north).to_degrees();
        if deg < 0.0 {
            deg + 360.0
        } else {
            deg
        }
    }
}

/// `N`, `NE`, `E`, ... for a bearing in degrees.
pub fn compass(bearing: f32) -> &'static str {
    const NAMES: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    NAMES[((bearing.rem_euclid(360.0) / 45.0).round() as usize) % 8]
}

/// An arrow pointing along a compass bearing, for the cube list.
pub fn arrow(bearing: f32) -> &'static str {
    const ARROWS: [&str; 8] = ["↑", "↗", "→", "↘", "↓", "↙", "←", "↖"];
    ARROWS[((bearing.rem_euclid(360.0) / 45.0).round() as usize) % 8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_are_planar() {
        let a = Vec3::new(0.0, 0.0, 0.0);
        let b = Vec3::new(3_000.0, 4_000.0, 9_000.0);
        assert_eq!(a.planar_distance(&b), 5_000.0);
    }

    #[test]
    fn north_is_minus_y() {
        let me = Vec3::new(10_000.0, 10_000.0, 0.0);
        assert_eq!(me.bearing_to(&Vec3::new(10_000.0, 5_000.0, 0.0)), 0.0);
        assert_eq!(me.bearing_to(&Vec3::new(15_000.0, 10_000.0, 0.0)), 90.0);
        assert_eq!(me.bearing_to(&Vec3::new(10_000.0, 15_000.0, 0.0)), 180.0);
        assert_eq!(me.bearing_to(&Vec3::new(5_000.0, 10_000.0, 0.0)), 270.0);
        assert_eq!(compass(44.0), "NE");
        assert_eq!(compass(359.0), "N");
        assert_eq!(arrow(180.0), "↓");
    }

    #[test]
    fn rejects_misreads() {
        assert!(Vec3::new(-50_205.0, 126_283.0, 15_449.0).is_world_position());
        assert!(!Vec3::new(-108.0, -31.0, -95.0).is_world_position(), "a small vector");
        assert!(Vec3::new(-108.0, -31.0, -95.0).in_bounds());
        assert!(!Vec3::new(f32::NAN, 0.0, 0.0).in_bounds());
        assert!(!Vec3::new(1e9, 0.0, 0.0).in_bounds());
    }
}
