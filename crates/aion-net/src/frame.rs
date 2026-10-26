//! Splits one server to client byte stream into game packets.
//!
//! ```text
//! frame   = varint len, [one byte in 0xF0..=0xFE], payload       frame bytes on the wire = len + varint bytes - 4
//! payload = opcode (2 bytes) body
//!         | FF FF, u32 raw_size, LZ4 block                        the block decompresses to more frames
//! ```
//! `0x00` bytes between frames are padding. The server sends a heartbeat frame about twenty times a second
//! (`0E 00 36` + u64 server time in ms), so after any loss the decoder hunts for that signature instead of
//! guessing at varints, which would turn garbage into phantom damage.

use crate::wire;

/// The opcode of an LZ4 bundle frame (`FF FF`).
pub const BUNDLE: u16 = 0xFFFF;
/// Largest frame we accept. Anything bigger is treated as a lost sync.
pub const MAX_FRAME: usize = 65_535;
/// A frame that claims more than this and is not an LZ4 bundle is not waited for.
const MAX_UNBUNDLED_WAIT: usize = 16 * 1024;
/// Decompressed size cap for one bundle.
const MAX_BUNDLE_RAW: usize = 8 * 1024 * 1024;
/// Bundles inside bundles: how deep we follow.
const MAX_DEPTH: u8 = 4;
/// If no heartbeat is seen in this many bytes the signature probably changed; fall back to byte resync.
const MAX_HUNT_BYTES: usize = 256 * 1024;
/// Heartbeat frames: 14 byte length (8 byte body) and 6 byte length (empty body).
pub const HEARTBEAT_SIGNATURES: [[u8; 3]; 2] = [[0x0E, 0x00, 0x36], [0x06, 0x00, 0x36]];

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct FrameStats {
    pub frames: u64,
    pub bundles: u64,
    pub bad_bundles: u64,
    pub resyncs: u64,
    pub hunts: u64,
    pub dropped_bytes: u64,
}

impl FrameStats {
    pub fn add(&mut self, o: &FrameStats) {
        self.frames += o.frames;
        self.bundles += o.bundles;
        self.bad_bundles += o.bad_bundles;
        self.resyncs += o.resyncs;
        self.hunts += o.hunts;
        self.dropped_bytes += o.dropped_bytes;
    }
}

pub struct FrameDecoder {
    buf: Vec<u8>,
    hunting: bool,
    hunted: usize,
    pub stats: FrameStats,
}

impl FrameDecoder {
    /// `aligned`: the stream was seen from its first byte (SYN observed). Otherwise the first bytes may start
    /// mid frame, so the decoder begins by hunting for a heartbeat.
    pub fn new(aligned: bool) -> Self {
        Self { buf: Vec::with_capacity(64 * 1024), hunting: !aligned, hunted: 0, stats: FrameStats::default() }
    }

    /// The TCP layer skipped lost bytes: whatever comes next is not a frame boundary.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.start_hunt();
    }

    pub fn is_hunting(&self) -> bool {
        self.hunting
    }

    fn start_hunt(&mut self) {
        if !self.hunting {
            self.stats.hunts += 1;
        }
        self.hunting = true;
        self.hunted = 0;
    }

    /// Feeds stream bytes and calls `on_packet(opcode, body)` for every complete packet, including those
    /// unpacked from bundles. Opcodes are `0xAABB` for wire bytes `AA BB`.
    pub fn feed(&mut self, data: &[u8], on_packet: &mut dyn FnMut(u16, &[u8])) {
        self.buf.extend_from_slice(data);
        let buf = std::mem::take(&mut self.buf);
        let mut pos = 0;

        while pos < buf.len() {
            let span = &buf[pos..];

            if self.hunting {
                match find_heartbeat(span) {
                    Some(i) => {
                        pos += i;
                        self.stats.dropped_bytes += i as u64;
                        self.hunting = false;
                        self.hunted = 0;
                        continue;
                    }
                    None => {
                        // Keep the last two bytes: a signature may straddle two segments.
                        let skip = span.len().saturating_sub(2);
                        pos += skip;
                        self.stats.dropped_bytes += skip as u64;
                        self.hunted += skip;
                        if self.hunted > MAX_HUNT_BYTES {
                            self.hunting = false;
                        }
                        break;
                    }
                }
            }

            if span[0] == 0x00 {
                pos += 1; // padding
                continue;
            }

            let Some((len, vlen)) = wire::peek_varint(span, 0) else {
                if span.len() < 5 {
                    break; // need more bytes to finish the varint
                }
                pos += self.resync();
                continue;
            };

            let size = (len as usize + vlen).saturating_sub(4);
            if len < 6 || size > MAX_FRAME || size < vlen + 2 {
                pos += self.resync();
                continue;
            }
            if size > span.len() {
                if size > MAX_UNBUNDLED_WAIT && !is_bundle(span, vlen) {
                    pos += self.resync();
                    continue;
                }
                break; // wait for the rest of the frame
            }

            self.handle_frame(&span[..size], vlen, 0, on_packet);
            pos += size;
        }

        self.buf = buf;
        self.buf.drain(..pos);
    }

    /// Skips one byte and goes looking for the next heartbeat.
    fn resync(&mut self) -> usize {
        self.stats.resyncs += 1;
        self.stats.dropped_bytes += 1;
        if self.hunted <= MAX_HUNT_BYTES {
            self.start_hunt();
        }
        1
    }

    fn handle_frame(&mut self, frame: &[u8], vlen: usize, depth: u8, on_packet: &mut dyn FnMut(u16, &[u8])) {
        self.stats.frames += 1;
        let mut o = vlen;
        if matches!(frame.get(o), Some(0xF0..=0xFE)) {
            o += 1;
        }
        if o + 2 > frame.len() {
            return;
        }

        if frame[o] == 0xFF && frame[o + 1] == 0xFF {
            self.stats.bundles += 1;
            o += 2;
            let Some(raw_bytes) = frame.get(o..o + 4) else {
                self.stats.bad_bundles += 1;
                return;
            };
            let raw = u32::from_le_bytes([raw_bytes[0], raw_bytes[1], raw_bytes[2], raw_bytes[3]]) as usize;
            o += 4;
            if raw == 0 || raw > MAX_BUNDLE_RAW || depth >= MAX_DEPTH {
                self.stats.bad_bundles += 1;
                return;
            }
            let mut out = vec![0u8; raw];
            match lz4_flex::block::decompress_into(&frame[o..], &mut out) {
                Ok(n) if n > 0 => {
                    out.truncate(n);
                    self.walk_bundle(&out, depth + 1, on_packet);
                }
                _ => self.stats.bad_bundles += 1,
            }
            return;
        }

        let opcode = u16::from_be_bytes([frame[o], frame[o + 1]]);
        on_packet(opcode, &frame[o + 2..]);
    }

    /// Frames inside a decompressed bundle. No resync here: an invalid length ends the bundle.
    fn walk_bundle(&mut self, data: &[u8], depth: u8, on_packet: &mut dyn FnMut(u16, &[u8])) {
        let mut pos = 0;
        while pos < data.len() {
            if data[pos] == 0x00 {
                pos += 1;
                continue;
            }
            let span = &data[pos..];
            let Some((len, vlen)) = wire::peek_varint(span, 0) else { return };
            let size = (len as usize + vlen).saturating_sub(4);
            if len < 6 || size < vlen + 2 || size > span.len() {
                return;
            }
            self.handle_frame(&span[..size], vlen, depth, on_packet);
            pos += size;
        }
    }
}

fn find_heartbeat(span: &[u8]) -> Option<usize> {
    HEARTBEAT_SIGNATURES
        .iter()
        .filter_map(|sig| span.windows(sig.len()).position(|w| w == sig))
        .min()
}

fn is_bundle(frame: &[u8], vlen: usize) -> bool {
    let mut o = vlen;
    if matches!(frame.get(o), Some(0xF0..=0xFE)) {
        o += 1;
    }
    frame.get(o) == Some(&0xFF) && frame.get(o + 1) == Some(&0xFF)
}

/// Encodes one frame the way the server does. Used by tests and the demo source.
pub fn encode_frame(opcode: u16, body: &[u8]) -> Vec<u8> {
    let payload_len = 2 + body.len();
    // size = len + vlen - 4 and size = vlen + payload_len, so len = payload_len + 4.
    let len = (payload_len + 4) as u64;
    let mut out = wire::encode_varint(len);
    out.extend_from_slice(&opcode.to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// Wraps already encoded frames into one LZ4 bundle frame.
pub fn encode_bundle(frames: &[u8]) -> Vec<u8> {
    let compressed = lz4_flex::block::compress(frames);
    let mut body = (frames.len() as u32).to_le_bytes().to_vec();
    body.extend_from_slice(&compressed);
    encode_frame(BUNDLE, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(dec: &mut FrameDecoder, data: &[u8]) -> Vec<(u16, Vec<u8>)> {
        let mut got = Vec::new();
        dec.feed(data, &mut |op, body| got.push((op, body.to_vec())));
        got
    }

    fn heartbeat(ms: u64) -> Vec<u8> {
        encode_frame(0x0036, &ms.to_le_bytes())
    }

    #[test]
    fn heartbeat_encodes_like_the_wire() {
        assert_eq!(&heartbeat(0)[..3], &[0x0E, 0x00, 0x36]);
        assert_eq!(heartbeat(0).len(), 11);
    }

    #[test]
    fn splits_frames_fed_in_pieces() {
        let mut stream = heartbeat(1);
        stream.extend(encode_frame(0x0438, &[1, 2, 3, 4]));
        stream.push(0); // padding
        stream.extend(heartbeat(2));

        let mut dec = FrameDecoder::new(true);
        let mut got = Vec::new();
        for chunk in stream.chunks(3) {
            got.extend(collect(&mut dec, chunk));
        }
        let ops: Vec<u16> = got.iter().map(|g| g.0).collect();
        assert_eq!(ops, [0x0036, 0x0438, 0x0036]);
        assert_eq!(got[1].1, [1, 2, 3, 4]);
        assert_eq!(dec.stats.resyncs, 0);
    }

    #[test]
    fn unpacks_lz4_bundles() {
        let mut inner = encode_frame(0x0438, &[9, 9, 9]);
        inner.extend(encode_frame(0x0538, &[7]));
        let bundle = encode_bundle(&inner);

        let mut dec = FrameDecoder::new(true);
        let got = collect(&mut dec, &bundle);
        let ops: Vec<u16> = got.iter().map(|g| g.0).collect();
        assert_eq!(ops, [0x0438, 0x0538]);
        assert_eq!(got[0].1, [9, 9, 9]);
        assert_eq!(dec.stats.bundles, 1);
        assert_eq!(dec.stats.bad_bundles, 0);
    }

    #[test]
    fn nested_bundles_and_bad_bundles() {
        let inner = encode_frame(0x0438, &[1]);
        let nested = encode_bundle(&encode_bundle(&inner));
        let mut dec = FrameDecoder::new(true);
        let ops: Vec<u16> = collect(&mut dec, &nested).iter().map(|g| g.0).collect();
        assert_eq!(ops, [0x0438]);
        assert_eq!(dec.stats.bundles, 2);

        let mut garbage = (12u32).to_le_bytes().to_vec();
        garbage.extend_from_slice(&[0xFF, 0x00, 0x12]);
        let bad = encode_frame(0xFFFF, &garbage);
        let mut dec = FrameDecoder::new(true);
        assert!(collect(&mut dec, &bad).is_empty());
        assert_eq!(dec.stats.bad_bundles, 1);
    }

    #[test]
    fn resyncs_on_heartbeat_after_joining_mid_stream() {
        let mut stream = vec![0x83, 0x44, 0x12, 0x99]; // tail of a frame we never saw the start of
        stream.extend(heartbeat(5));
        stream.extend(encode_frame(0x0438, &[5]));

        let mut dec = FrameDecoder::new(false);
        let ops: Vec<u16> = collect(&mut dec, &stream).iter().map(|g| g.0).collect();
        assert_eq!(ops, [0x0036, 0x0438]);
        assert_eq!(dec.stats.dropped_bytes, 4);
    }

    #[test]
    fn signature_straddling_two_segments_is_found() {
        let mut stream = vec![0x01, 0x02];
        stream.extend(heartbeat(9));
        let mut dec = FrameDecoder::new(false);
        let mut got = collect(&mut dec, &stream[..3]); // 01 02 0E
        got.extend(collect(&mut dec, &stream[3..]));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 0x0036);
    }

    #[test]
    fn garbage_in_an_aligned_stream_triggers_resync() {
        let mut stream = heartbeat(1);
        stream.extend_from_slice(&[0xFE, 0xFF, 0xFF, 0xFF, 0x0F, 0x00, 0x00, 0x00]); // absurd length
        stream.extend(heartbeat(2));
        let mut dec = FrameDecoder::new(true);
        let ops: Vec<u16> = collect(&mut dec, &stream).iter().map(|g| g.0).collect();
        assert_eq!(ops, [0x0036, 0x0036]);
        assert!(dec.stats.resyncs >= 1);
    }

    #[test]
    fn optional_prefix_byte_is_skipped() {
        let body = [0xAA, 0xBB];
        let payload_len = 1 + 2 + body.len(); // prefix byte + opcode + body
        let mut frame = wire::encode_varint((payload_len + 4) as u64);
        frame.push(0xF3);
        frame.extend_from_slice(&[0x04, 0x38]);
        frame.extend_from_slice(&body);
        let mut dec = FrameDecoder::new(true);
        let got = collect(&mut dec, &frame);
        assert_eq!(got, vec![(0x0438, vec![0xAA, 0xBB])]);
    }
}
