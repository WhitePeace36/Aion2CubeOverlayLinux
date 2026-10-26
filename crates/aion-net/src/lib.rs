//! aion-net: the transport layers every Aion 2 decoder here shares.
//!
//! Link layer (`net`) to TCP byte stream (`tcp`) to game frames (`frame`, with `wire` for varints) to packets
//! (`pipeline`, generic over the parser). `capture_file` reads saved pcap and pcapng files. These came from the
//! aion2dps meter, where the meter, the kina tracker and the cube radar each kept an identical copy.

pub mod capture_file;
pub mod frame;
pub mod net;
pub mod pipeline;
pub mod tcp;
pub mod wire;
