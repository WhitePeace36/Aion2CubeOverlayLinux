//! cube-core: pure Rust decoding of Aion 2 game traffic into Hidden Cube sightings, plus the radar tracker.
//!
//! Pipeline: link layer (`net`) to TCP byte stream (`tcp`) to game frames (`frame`) to packets (`parser`, with
//! `object` and `position`) to the radar (`tracker`). Nothing here touches the network or the game; the terminal
//! app feeds captured bytes in and reads snapshots out, and `bin/replay.rs` does the same from a pcap file.
//!
//! The transport layers (`net`, `tcp`, `frame`, `wire`, `capture_file` and the generic pipeline) live in
//! `aion-net`, shared with the meter and the kina tracker.

pub mod cubes;
pub mod gamedata;
pub mod geo;
pub mod object;
pub mod opcodes;
pub mod parser;
pub mod pipeline;
pub mod position;
pub mod snapshot;
pub mod spots;
pub mod tracker;

/// The shared transport layers, under the paths this crate always used.
pub use aion_net::{capture_file, frame, net, tcp, wire};
