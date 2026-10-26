//! Link layer to TCP segment, for the link types Npcap and tcpdump produce.

use etherparse::{NetSlice, SlicedPacket, TransportSlice};
use std::net::{IpAddr, SocketAddr};

/// pcap link types we understand (see tcpdump.org/linktypes.html).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkType {
    Ethernet,
    /// BSD loopback and the Npcap loopback adapter: a 4 byte address family header, then IP.
    Null,
    /// Raw IPv4 or IPv6 with no link header.
    Raw,
    /// Linux "cooked" capture (tcpdump -i any).
    LinuxSll,
}

impl LinkType {
    pub fn from_pcap(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Ethernet),
            0 | 108 => Some(Self::Null),
            12 | 101 | 228 | 229 => Some(Self::Raw),
            113 => Some(Self::LinuxSll),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlowKey {
    pub src: SocketAddr,
    pub dst: SocketAddr,
}

impl FlowKey {
    /// The same connection seen from the other side.
    pub fn reverse(&self) -> FlowKey {
        FlowKey { src: self.dst, dst: self.src }
    }
}

#[derive(Debug)]
pub struct Segment<'a> {
    pub flow: FlowKey,
    pub seq: u32,
    pub syn: bool,
    pub fin: bool,
    pub rst: bool,
    pub payload: &'a [u8],
}

pub fn parse(link: LinkType, data: &[u8]) -> Option<Segment<'_>> {
    let sliced = match link {
        LinkType::Ethernet => SlicedPacket::from_ethernet(data).ok()?,
        LinkType::Null => SlicedPacket::from_ip(data.get(4..)?).ok()?,
        LinkType::Raw => SlicedPacket::from_ip(data).ok()?,
        LinkType::LinuxSll => SlicedPacket::from_linux_sll(data).ok()?,
    };
    let (src_ip, dst_ip): (IpAddr, IpAddr) = match sliced.net? {
        NetSlice::Ipv4(v4) => (v4.header().source_addr().into(), v4.header().destination_addr().into()),
        NetSlice::Ipv6(v6) => (v6.header().source_addr().into(), v6.header().destination_addr().into()),
        _ => return None,
    };
    let TransportSlice::Tcp(tcp) = sliced.transport? else { return None };
    Some(Segment {
        flow: FlowKey {
            src: SocketAddr::new(src_ip, tcp.source_port()),
            dst: SocketAddr::new(dst_ip, tcp.destination_port()),
        },
        seq: tcp.sequence_number(),
        syn: tcp.syn(),
        fin: tcp.fin(),
        rst: tcp.rst(),
        payload: tcp.payload(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use etherparse::PacketBuilder;

    #[test]
    fn parses_an_ethernet_tcp_segment() {
        let builder = PacketBuilder::ethernet2([1, 2, 3, 4, 5, 6], [7, 8, 9, 10, 11, 12])
            .ipv4([10, 0, 0, 1], [10, 0, 0, 2], 64)
            .tcp(13328, 50000, 1234, 1000)
            .syn();
        let mut out = Vec::new();
        builder.write(&mut out, b"hello").unwrap();
        let seg = parse(LinkType::Ethernet, &out).unwrap();
        assert_eq!(seg.flow.src.port(), 13328);
        assert_eq!(seg.flow.dst.port(), 50000);
        assert_eq!(seg.seq, 1234);
        assert!(seg.syn);
        assert_eq!(seg.payload, b"hello");
    }

    #[test]
    fn parses_null_loopback_framing() {
        let builder = PacketBuilder::ipv4([127, 0, 0, 1], [127, 0, 0, 1], 64).tcp(13328, 40000, 1, 1000);
        let mut ip = Vec::new();
        builder.write(&mut ip, b"x").unwrap();
        let mut out = 2u32.to_ne_bytes().to_vec(); // AF_INET
        out.extend(ip);
        let seg = parse(LinkType::Null, &out).unwrap();
        assert_eq!(seg.payload, b"x");
        assert_eq!(seg.flow.src.port(), 13328);
    }
}
