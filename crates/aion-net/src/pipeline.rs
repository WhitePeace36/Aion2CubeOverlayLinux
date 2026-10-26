//! Glue: raw captured packets in, parser events out.
//!
//! One `Flow` per server to client TCP stream on a game port. Client to server bytes are not decoded
//! (their bodies are encrypted) but their timing can be used for a ping readout later.
//!
//! The pipeline is generic over the parser, so the meter, the kina tracker and the cube radar share this one
//! transport, and a combined parser can feed all three from a single reassembly.

use crate::frame::{FrameDecoder, FrameStats};
use crate::net::{self, FlowKey, LinkType};
use crate::tcp::{Chunk, Reassembler};
use std::collections::HashMap;

pub const DEFAULT_PORT: u16 = 13328;

/// A diagnostic hook that sees every decoded packet before the parser: `(t_ms, opcode, body)`.
pub type PacketTap = Box<dyn FnMut(u64, u16, &[u8]) + Send>;

/// Turns decoded game packets into events.
pub trait PacketParser {
    type Event;
    fn packet(&mut self, t_ms: u64, opcode: u16, body: &[u8], out: &mut dyn FnMut(Self::Event));
}

struct Flow {
    reasm: Reassembler,
    decoder: FrameDecoder,
    first_seen_ms: u64,
    last_seen_ms: u64,
    segments: u64,
    closed: bool,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PipelineStats {
    pub packets_seen: u64,
    pub segments_server_to_client: u64,
    pub segments_client_to_server: u64,
    pub flows: usize,
    pub frames: FrameStats,
    pub tcp_gaps: u64,
}

pub struct Pipeline<P> {
    ports: Vec<u16>,
    flows: HashMap<FlowKey, Flow>,
    pub parser: P,
    pub stats: PipelineStats,
    pub tap: Option<PacketTap>,
}

impl<P: PacketParser + Default> Pipeline<P> {
    pub fn new(ports: Vec<u16>) -> Self {
        Self::with_parser(ports, P::default())
    }
}

impl<P: PacketParser> Pipeline<P> {
    pub fn with_parser(ports: Vec<u16>, parser: P) -> Self {
        Self { ports, flows: HashMap::new(), parser, stats: PipelineStats::default(), tap: None }
    }

    pub fn ports(&self) -> &[u16] {
        &self.ports
    }

    pub fn set_ports(&mut self, ports: Vec<u16>) {
        self.ports = ports;
        self.flows.clear();
    }

    /// True once at least one server to client stream on a game port has delivered data.
    pub fn has_game_flow(&self) -> bool {
        self.flows.values().any(|f| f.reasm.delivered > 0)
    }

    /// Feeds one captured link layer packet. `t_ms` is the capture timestamp in milliseconds.
    pub fn push(&mut self, link: LinkType, t_ms: u64, data: &[u8], out: &mut dyn FnMut(P::Event)) {
        self.stats.packets_seen += 1;
        let Some(seg) = net::parse(link, data) else { return };
        self.push_segment(t_ms, &seg, out);
    }

    pub fn push_segment(&mut self, t_ms: u64, seg: &net::Segment<'_>, out: &mut dyn FnMut(P::Event)) {
        let from_server = self.ports.contains(&seg.flow.src.port());
        let to_server = self.ports.contains(&seg.flow.dst.port());
        if from_server {
            self.stats.segments_server_to_client += 1;
        } else if to_server {
            self.stats.segments_client_to_server += 1;
            // A client SYN tells us a new connection starts; drop stale state for the reverse flow so the
            // next server bytes are decoded from an aligned start.
            if seg.syn {
                self.flows.remove(&seg.flow.reverse());
            }
            return;
        } else {
            return;
        }

        let flow = self.flows.entry(seg.flow).or_insert_with(|| Flow {
            reasm: Reassembler::default(),
            decoder: FrameDecoder::new(seg.syn),
            first_seen_ms: t_ms,
            last_seen_ms: t_ms,
            segments: 0,
            closed: false,
        });
        flow.last_seen_ms = t_ms;
        flow.segments += 1;
        if seg.rst || seg.fin {
            flow.closed = true;
        }

        let parser = &mut self.parser;
        let tap = &mut self.tap;
        let decoder = &mut flow.decoder;
        flow.reasm.push(seg.seq, seg.syn, seg.payload, &mut |chunk| match chunk {
            Chunk::Data(bytes) => decoder.feed(bytes, &mut |opcode, body| {
                if let Some(tap) = tap.as_mut() {
                    tap(t_ms, opcode, body);
                }
                parser.packet(t_ms, opcode, body, out);
            }),
            Chunk::Gap => decoder.reset(),
        });

        // Keep the stats cheap to read: fold frame stats and gaps on every segment.
        self.stats.flows = self.flows.len();
        let mut frames = FrameStats::default();
        let mut gaps = 0;
        for f in self.flows.values() {
            frames.add(&f.decoder.stats);
            gaps += f.reasm.gaps;
        }
        self.stats.frames = frames;
        self.stats.tcp_gaps = gaps;
    }

    /// Flows that have been silent longer than `idle_ms` are forgotten. Call this from a timer.
    pub fn expire_flows(&mut self, now_ms: u64, idle_ms: u64) {
        self.flows.retain(|_, f| !(f.closed || now_ms.saturating_sub(f.last_seen_ms) > idle_ms));
    }

    pub fn flow_summary(&self) -> Vec<FlowSummary> {
        let mut v: Vec<FlowSummary> = self
            .flows
            .iter()
            .map(|(k, f)| FlowSummary {
                server: k.src.to_string(),
                client: k.dst.to_string(),
                first_seen_ms: f.first_seen_ms,
                last_seen_ms: f.last_seen_ms,
                segments: f.segments,
                bytes: f.reasm.delivered,
                saw_syn: f.reasm.saw_syn,
                gaps: f.reasm.gaps,
                frames: f.decoder.stats,
                closed: f.closed,
            })
            .collect();
        v.sort_by_key(|f| f.first_seen_ms);
        v
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FlowSummary {
    pub server: String,
    pub client: String,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    pub segments: u64,
    pub bytes: u64,
    pub saw_syn: bool,
    pub gaps: u64,
    pub frames: FrameStats,
    pub closed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::encode_frame;
    use etherparse::PacketBuilder;

    /// Records every packet it is handed.
    #[derive(Default)]
    struct Recorder;

    impl PacketParser for Recorder {
        type Event = (u16, Vec<u8>);
        fn packet(&mut self, _t_ms: u64, opcode: u16, body: &[u8], out: &mut dyn FnMut(Self::Event)) {
            out((opcode, body.to_vec()));
        }
    }

    fn segment(seq: u32, payload: &[u8], syn: bool) -> Vec<u8> {
        let mut b = PacketBuilder::ethernet2([1; 6], [2; 6])
            .ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
            .tcp(DEFAULT_PORT, 50000, seq, 1000);
        if syn {
            b = b.syn();
        }
        let mut out = Vec::new();
        b.write(&mut out, payload).unwrap();
        out
    }

    #[test]
    fn decodes_packets_from_segments() {
        let mut pipe: Pipeline<Recorder> = Pipeline::new(vec![DEFAULT_PORT]);
        let mut stream = encode_frame(0x0036, &7u64.to_le_bytes());
        stream.extend(encode_frame(0x1B56, &[1, 2, 3]));
        let mut seen = Vec::new();
        pipe.push(LinkType::Ethernet, 1, &segment(100, &[], true), &mut |e| seen.push(e));
        pipe.push(LinkType::Ethernet, 2, &segment(101, &stream[..5], false), &mut |e| seen.push(e));
        pipe.push(LinkType::Ethernet, 3, &segment(106, &stream[5..], false), &mut |e| seen.push(e));
        assert_eq!(seen, vec![(0x0036, 7u64.to_le_bytes().to_vec()), (0x1B56, vec![1, 2, 3])]);
        assert_eq!(pipe.stats.frames.frames, 2);
        assert!(pipe.has_game_flow());
    }
}
