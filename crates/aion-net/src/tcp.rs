//! Orders the TCP segments of one direction of one connection into a byte stream.
//!
//! Retransmits and overlaps are trimmed. Out of order segments wait in a small buffer; if a gap does not fill
//! within `MAX_PENDING_SEGMENTS` segments (or `MAX_PENDING_BYTES`) the missing bytes are skipped and the caller
//! is told, so the frame decoder can resync instead of misreading data.

use std::collections::BTreeMap;

const MAX_PENDING_SEGMENTS: usize = 64;
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

pub enum Chunk<'a> {
    Data(&'a [u8]),
    /// Bytes were lost before the next `Data`.
    Gap,
}

#[derive(Default)]
pub struct Reassembler {
    next: Option<u32>,
    pending: BTreeMap<u32, Vec<u8>>,
    pending_bytes: usize,
    pub saw_syn: bool,
    pub gaps: u64,
    pub delivered: u64,
}

impl Reassembler {
    pub fn push(&mut self, seq: u32, syn: bool, payload: &[u8], out: &mut dyn FnMut(Chunk)) {
        if syn {
            self.saw_syn = true;
            self.next = Some(seq.wrapping_add(1));
            self.pending.clear();
            self.pending_bytes = 0;
        }
        if payload.is_empty() {
            return;
        }
        let next = *self.next.get_or_insert(seq);

        let ahead = seq.wrapping_sub(next) as i32;
        if ahead > 0 {
            if !self.pending.contains_key(&seq) {
                self.pending_bytes += payload.len();
                self.pending.insert(seq, payload.to_vec());
            }
            if self.pending.len() > MAX_PENDING_SEGMENTS || self.pending_bytes > MAX_PENDING_BYTES {
                self.skip_gap(out);
            }
            return;
        }

        // Starts at or before `next`: drop the part already delivered.
        let overlap = (-ahead) as usize;
        if overlap >= payload.len() {
            return; // pure retransmit
        }
        let fresh = &payload[overlap..];
        out(Chunk::Data(fresh));
        self.delivered += fresh.len() as u64;
        self.next = Some(next.wrapping_add(fresh.len() as u32));
        self.drain_pending(out);
    }

    fn drain_pending(&mut self, out: &mut dyn FnMut(Chunk)) {
        while let Some(next) = self.next {
            let Some((&seq, _)) = self.pending.iter().next() else { return };
            let ahead = seq.wrapping_sub(next) as i32;
            if ahead > 0 {
                return; // still a hole before the first buffered segment
            }
            let data = self.pending.remove(&seq).expect("key just seen");
            self.pending_bytes -= data.len();
            let overlap = (-ahead) as usize;
            if overlap < data.len() {
                let fresh = &data[overlap..];
                out(Chunk::Data(fresh));
                self.delivered += fresh.len() as u64;
                self.next = Some(next.wrapping_add(fresh.len() as u32));
            }
        }
    }

    fn skip_gap(&mut self, out: &mut dyn FnMut(Chunk)) {
        let Some((&seq, _)) = self.pending.iter().next() else { return };
        self.gaps += 1;
        out(Chunk::Gap);
        self.next = Some(seq);
        self.drain_pending(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(segs: &[(u32, &[u8])]) -> (Vec<u8>, u64) {
        let mut r = Reassembler::default();
        let mut bytes = Vec::new();
        let mut gaps = 0;
        for (seq, p) in segs {
            r.push(*seq, false, p, &mut |c| match c {
                Chunk::Data(d) => bytes.extend_from_slice(d),
                Chunk::Gap => gaps += 1,
            });
        }
        (bytes, gaps)
    }

    #[test]
    fn reorders_and_trims_retransmits() {
        let (bytes, gaps) = run(&[(100, b"abc"), (106, b"ghi"), (103, b"def"), (103, b"def"), (104, b"efgh")]);
        assert_eq!(bytes, b"abcdefghi");
        assert_eq!(gaps, 0);
    }

    #[test]
    fn handles_sequence_wraparound() {
        let (bytes, _) = run(&[(u32::MAX - 1, b"ab"), (0, b"cd")]);
        assert_eq!(bytes, b"abcd");
    }

    #[test]
    fn skips_a_gap_that_never_fills() {
        let mut segs: Vec<(u32, Vec<u8>)> = vec![(0, b"aa".to_vec())];
        for i in 0..70u32 {
            segs.push((10 + i * 2, b"zz".to_vec()));
        }
        let refs: Vec<(u32, &[u8])> = segs.iter().map(|(s, p)| (*s, p.as_slice())).collect();
        let (bytes, gaps) = run(&refs);
        assert_eq!(gaps, 1);
        assert_eq!(bytes.len(), 2 + 70 * 2);
    }

    #[test]
    fn syn_resets_the_stream() {
        let mut r = Reassembler::default();
        let mut bytes = Vec::new();
        let mut sink = |c: Chunk| if let Chunk::Data(d) = c { bytes.extend_from_slice(d) };
        r.push(500, false, b"old", &mut sink);
        r.push(1000, true, b"", &mut sink);
        r.push(1001, false, b"new", &mut sink);
        assert_eq!(bytes, b"oldnew");
        assert!(r.saw_syn);
    }
}
