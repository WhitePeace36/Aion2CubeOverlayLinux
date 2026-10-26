//! Reads saved captures (pcap or pcapng, as written by Wireshark, tcpdump or Npcap).

use anyhow::Context;

/// Calls `f(link_type_code, t_ms, data)` for every packet in a pcap or pcapng file.
pub fn read_packets(bytes: &[u8], mut f: impl FnMut(u32, u64, &[u8])) -> anyhow::Result<()> {
    if bytes.len() >= 4 && bytes[..4] == [0x0A, 0x0D, 0x0D, 0x0A] {
        use pcap_file::pcapng::{Block, PcapNgReader};
        let mut r = PcapNgReader::new(std::io::Cursor::new(bytes)).context("pcapng header")?;
        let mut links: Vec<u32> = Vec::new();
        while let Some(block) = r.next_block() {
            match block.context("pcapng block")? {
                Block::InterfaceDescription(idb) => links.push(u32::from(idb.linktype)),
                Block::EnhancedPacket(epb) => {
                    let link = links.get(epb.interface_id as usize).copied().unwrap_or(1);
                    f(link, epb.timestamp.as_millis() as u64, &epb.data);
                }
                _ => {}
            }
        }
    } else {
        use pcap_file::pcap::PcapReader;
        let mut r = PcapReader::new(std::io::Cursor::new(bytes)).context("pcap header")?;
        let link = u32::from(r.header().datalink);
        // A capture still being written ends in a partial record; stop there.
        while let Some(Ok(pkt)) = r.next_packet() {
            f(link, pkt.timestamp.as_millis() as u64, &pkt.data);
        }
    }
    Ok(())
}
