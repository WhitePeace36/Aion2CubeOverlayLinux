//! meter-core: pure Rust decoding of Aion 2 game traffic into combat events, plus the combat tracker.
//!
//! Pipeline: link layer (`net`) to TCP byte stream (`tcp`) to game frames (`frame`) to packets (`parser`)
//! to encounters (`combat`). Nothing here touches the network or the game; the Tauri shell feeds captured
//! bytes in and reads snapshots out, and `bin/replay.rs` does the same from a pcap file.

pub mod combat;
pub mod gamedata;
pub mod opcodes;
pub mod parser;
pub mod pipeline;
pub mod snapshot;

/// The shared transport layers, under the paths this crate always used.
pub use aion_net::{capture_file, frame, net, tcp, wire};
