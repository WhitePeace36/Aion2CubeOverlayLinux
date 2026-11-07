//! Glue: raw captured packets in, events out. The transport lives in `aion-net`; this names its pipeline
//! with this crate's parser.

use crate::parser::{Event, Parser};

pub use aion_net::pipeline::{FlowSummary, PacketParser, PacketTap, PipelineStats, DEFAULT_PORT};

pub type Pipeline = aion_net::pipeline::Pipeline<Parser>;

impl PacketParser for Parser {
    type Event = Event;

    fn packet(&mut self, t_ms: u64, opcode: u16, body: &[u8], out: &mut dyn FnMut(Event)) {
        Parser::packet(self, t_ms, opcode, body, out);
    }
}
