//! Primitive readers for the Aion 2 wire format.
//!
//! Multi byte integers are little endian. Most counters and ids are LEB128 varints (7 bits per byte, high bit
//! means "more"). A `Cursor` never panics: every read returns `None` on truncation and leaves the position alone.

#[derive(Debug, Clone, Copy)]
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn at(buf: &'a [u8], pos: usize) -> Self {
        Self { buf, pos: pos.min(buf.len()) }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos.min(self.buf.len());
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    pub fn peek_u8(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    pub fn peek_bytes(&self, n: usize) -> Option<&'a [u8]> {
        self.buf.get(self.pos..self.pos + n)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        if self.pos + n > self.buf.len() {
            return None;
        }
        self.pos += n;
        Some(())
    }

    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.buf.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }

    pub fn u8(&mut self) -> Option<u8> {
        let v = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    pub fn u16(&mut self) -> Option<u16> {
        let s = self.bytes(2)?;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }

    pub fn u24(&mut self) -> Option<u32> {
        let s = self.bytes(3)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], 0]))
    }

    pub fn u32(&mut self) -> Option<u32> {
        let s = self.bytes(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    pub fn u64(&mut self) -> Option<u64> {
        let s = self.bytes(8)?;
        Some(u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
    }

    pub fn f32(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.u32()?))
    }

    /// Unsigned LEB128 varint, at most 5 bytes (32 bits).
    pub fn varint(&mut self) -> Option<u32> {
        let (v, n) = peek_varint(self.buf, self.pos)?;
        self.pos += n;
        Some(v)
    }

    /// Unsigned LEB128 varint, at most 10 bytes (64 bits).
    pub fn varint64(&mut self) -> Option<u64> {
        let mut value: u64 = 0;
        for i in 0..10 {
            let byte = *self.buf.get(self.pos + i)?;
            value |= ((byte & 0x7F) as u64) << (7 * i);
            if byte & 0x80 == 0 {
                self.pos += i + 1;
                return Some(value);
            }
        }
        None
    }

    /// A `u8` length prefix followed by that many UTF-8 bytes.
    pub fn str8(&mut self) -> Option<&'a str> {
        let n = self.u8()? as usize;
        let s = self.bytes(n)?;
        std::str::from_utf8(s).ok()
    }
}

/// Varint at `pos` and its encoded length, without a cursor.
pub fn peek_varint(buf: &[u8], pos: usize) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    for i in 0..5 {
        let byte = *buf.get(pos + i)?;
        if i == 4 && byte > 0x0F {
            return None; // would overflow 32 bits
        }
        value |= ((byte & 0x7F) as u32) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

pub fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u32> {
    let (v, n) = peek_varint(buf, *pos)?;
    *pos += n;
    Some(v)
}

/// LEB128 encoding, used by tests and the demo stream.
pub fn encode_varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, 16_383, 16_384, u32::MAX as u64, 1 << 40] {
            let enc = encode_varint(v);
            let mut c = Cursor::new(&enc);
            assert_eq!(c.varint64(), Some(v), "value {v}");
            assert!(c.is_empty());
            if v <= u32::MAX as u64 {
                let mut c = Cursor::new(&enc);
                assert_eq!(c.varint(), Some(v as u32));
            }
        }
    }

    #[test]
    fn known_encodings() {
        let b = [0x0E, 0xAC, 0x02, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F];
        let mut c = Cursor::new(&b);
        assert_eq!(c.varint(), Some(14));
        assert_eq!(c.varint(), Some(300));
        assert_eq!(c.varint(), Some(u32::MAX));
        assert!(c.is_empty());
    }

    #[test]
    fn truncated_reads_do_not_advance() {
        let b = [0x80, 0x80];
        let mut c = Cursor::new(&b);
        assert_eq!(c.varint(), None);
        assert_eq!(c.pos(), 0);
        assert_eq!(c.u32(), None);
        assert_eq!(c.pos(), 0);
        assert_eq!(c.u16(), Some(0x8080));
        assert_eq!(c.u8(), None);
    }

    #[test]
    fn little_endian_fixed_width() {
        let b = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let mut c = Cursor::new(&b);
        assert_eq!(c.u16(), Some(0x0201));
        assert_eq!(c.u24(), Some(0x050403));
        c.set_pos(0);
        assert_eq!(c.u32(), Some(0x04030201));
        c.set_pos(0);
        assert_eq!(c.u64(), Some(0x0807060504030201));
    }

    #[test]
    fn str8_reads_prefixed_utf8() {
        let b = [0x03, b'a', b'b', b'c', 0x00];
        let mut c = Cursor::new(&b);
        assert_eq!(c.str8(), Some("abc"));
        assert_eq!(c.remaining(), 1);
    }
}
