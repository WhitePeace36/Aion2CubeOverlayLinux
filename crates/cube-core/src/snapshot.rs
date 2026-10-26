//! The read model: what the tracker hands to the terminal app. Serialised camelCase, like the aion2kina files.

use crate::cubes::Evidence;
use crate::gamedata::MapInfo;
use crate::geo::Vec3;
use crate::parser::{EntityId, PositionSource};
use crate::spots::Spot;
use serde::{Deserialize, Serialize};

/// One point of your path on the current map.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrailPoint {
    pub t_ms: u64,
    pub pos: Vec3,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Character {
    pub name: String,
    pub server: u16,
    /// 0 when the self record carried none.
    pub level: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CubeState {
    /// In view: the server sent it and has not taken it back.
    Visible,
    /// Someone is opening it right now.
    Opening,
    /// It left your view range; it was still there when it did.
    OutOfView,
    Opened,
    /// You came back near where it was and the server did not send it again: someone opened it meanwhile.
    Gone,
}

impl CubeState {
    /// Still there to be opened, as far as the radar knows.
    pub fn is_open_to_take(self) -> bool {
        matches!(self, CubeState::Visible | CubeState::Opening | CubeState::OutOfView)
    }
}

/// The character's last known position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Fix {
    pub pos: Vec3,
    pub t_ms: u64,
    pub source: PositionSource,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CubeView {
    pub id: EntityId,
    pub template: u32,
    pub zone: Option<String>,
    pub evidence: Option<Evidence>,
    pub pos: Vec3,
    pub state: CubeState,
    /// Who opened or is opening it.
    pub opener: Option<EntityId>,
    pub opened_by_you: bool,
    pub first_seen_ms: u64,
    /// Time of the last state change.
    pub state_ms: u64,
    /// Time since the cube spawned, by the server clock, when the spawn carried it.
    pub age_ms: Option<u64>,
    /// Planar distance from your last fix, in metres.
    pub distance_m: Option<f32>,
    /// Height of the cube above you (negative: below), in metres.
    pub height_m: Option<f32>,
    /// Compass bearing from you to the cube in degrees, 0 north.
    pub bearing: Option<f32>,
    /// The fix behind the distance is older than a few seconds: you may have moved since.
    pub estimated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    Appeared,
    LeftView,
    BackInView,
    OpeningStarted,
    Opened,
    Gone,
    /// A template was learned from a key opening.
    Learned,
}

/// One line of the cube log.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub t_ms: u64,
    pub kind: LogKind,
    pub cube: EntityId,
    pub template: u32,
    pub map_id: Option<u32>,
    pub pos: Vec3,
    pub distance_m: Option<f32>,
    pub actor: Option<EntityId>,
    pub by_you: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// A cube came into view for the first time.
    Appeared,
    /// A cube seen before you left its map is there again.
    StillThere,
    OpenedByYou,
    OpenedByOther,
    /// A new cube template was learned (the cube itself is already opened).
    Learned,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub t_ms: u64,
    pub kind: AlertKind,
    pub cube: CubeView,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub cubes_seen: u32,
    pub opened_by_you: u32,
    pub opened_by_others: u32,
    pub templates_learned: u32,
    /// Key openings of objects that spawned before the capture started, so their template stayed unknown.
    pub key_opens_unknown: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarSnapshot {
    pub now_ms: u64,
    pub map: Option<MapInfo>,
    pub character: Option<Character>,
    pub self_id: Option<EntityId>,
    /// The self id was guessed from the buff records because no self record arrived yet.
    pub self_guessed: bool,
    pub me: Option<Fix>,
    /// Cubes still there first (nearest first), then the opened and gone ones (newest first).
    pub cubes: Vec<CubeView>,
    /// Oldest first.
    pub log: Vec<LogEntry>,
    pub counts: Counts,
    pub objects_in_view: usize,
    /// Your path on the current map, oldest first.
    pub trail: Vec<TrailPoint>,
    /// Every spot a cube was seen at on the current map, this run and the saved ones.
    pub spots: Vec<Spot>,
}

impl RadarSnapshot {
    /// Cubes that are still there to be opened.
    pub fn open_cubes(&self) -> impl Iterator<Item = &CubeView> {
        self.cubes.iter().filter(|c| c.state.is_open_to_take())
    }
}
