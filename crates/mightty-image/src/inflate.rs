//! DEFLATE decompression (RFC 1951) inside a zlib stream (RFC 1950), as PNG
//! images store their pixels. Input comes from font files, so it's
//! untrusted: every read is checked and the output has a hard limit, so a
//! small stream can't expand without end.

/// Reads bits least significant first, as DEFLATE packs them.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    buffer: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, at: 0, buffer: 0, count: 0 }
    }

    /// Makes at least `n` (≤ 32) bits available, if the input has them.
    fn fill(&mut self, n: u32) -> Option<()> {
        while self.count < n {
            let byte = *self.data.get(self.at)?;
            self.at += 1;
            self.buffer |= (byte as u64) << self.count;
            self.count += 8;
        }
        Some(())
    }

    fn take(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        self.fill(n)?;
        let v = (self.buffer & ((1 << n) - 1)) as u32;
        self.buffer >>= n;
        self.count -= n;
        Some(v)
    }

    /// Skips to the next byte boundary.
    fn align(&mut self) {
        let extra = self.count % 8;
        self.buffer >>= extra;
        self.count -= extra;
    }
}

/// A canonical Huffman code, decoded a bit at a time from the counts of
/// codes of each length (the classic `puff` way: small and safe).
struct Huffman {
    /// Codes of each length, 0 to 15.
    counts: [u16; 16],
    /// Symbols in code order.
    symbols: Vec<u16>,
}

impl Huffman {
    /// From each symbol's code length; `None` if the lengths over-subscribe.
    fn new(lengths: &[u8]) -> Option<Huffman> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut left: i32 = 1;
        for &c in &counts[1..] {
            left = left * 2 - c as i32;
            if left < 0 {
                return None;
            }
        }
        let mut offsets = [0u16; 16];
        for len in 1..15 {
            offsets[len + 1] = offsets[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = symbol as u16;
                offsets[l as usize] += 1;
            }
        }
        Some(Huffman { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

const LENGTH_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DISTANCE_BASE: [u16; 30] =
    [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DISTANCE_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// The bytes a zlib stream holds, or `None` if it's damaged or would be
/// longer than `limit`.
pub fn zlib(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let (cmf, flags) = (*data.first()?, *data.get(1)?);
    // Deflate, a window of 32K at most, a valid check, no preset dictionary.
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || !(cmf as u16 * 256 + flags as u16).is_multiple_of(31) || flags & 0x20 != 0 {
        return None;
    }
    let out = inflate(&data[2..], limit)?;
    // The Adler-32 checksum after it isn't checked: a damaged image is
    // caught well enough by its size and filters, and some encoders get it
    // wrong.
    Some(out)
}

/// The bytes a raw DEFLATE stream holds, or `None` if it's damaged or would
/// be longer than `limit`.
pub(crate) fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut bits = Bits::new(data);
    let mut out = Vec::new();
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            // Stored: a length, its complement, then the bytes as they are.
            0 => {
                bits.align();
                let len = bits.take(16)? as usize;
                if bits.take(16)? as usize != !len & 0xFFFF {
                    return None;
                }
                // What's left in the bit buffer is whole bytes here.
                for _ in 0..len {
                    out.push(bits.take(8)? as u8);
                }
                if out.len() > limit {
                    return None;
                }
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let literals = Huffman::new(&lengths)?;
                let distances = Huffman::new(&[5; 30])?;
                block(&mut bits, &mut out, &literals, &distances, limit)?;
            }
            2 => {
                let (literals, distances) = dynamic_codes(&mut bits)?;
                block(&mut bits, &mut out, &literals, &distances, limit)?;
            }
            _ => return None,
        }
        if last {
            return Some(out);
        }
    }
}

/// A dynamic block's codes, themselves Huffman-coded.
fn dynamic_codes(bits: &mut Bits<'_>) -> Option<(Huffman, Huffman)> {
    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
    let literal_count = bits.take(5)? as usize + 257;
    let distance_count = bits.take(5)? as usize + 1;
    let code_count = bits.take(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return None;
    }
    let mut code_lengths = [0u8; 19];
    for &i in &ORDER[..code_count] {
        code_lengths[i] = bits.take(3)? as u8;
    }
    let code = Huffman::new(&code_lengths)?;
    let mut lengths = vec![0u8; literal_count + distance_count];
    let mut i = 0;
    while i < lengths.len() {
        let symbol = code.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => (*lengths.get(i.checked_sub(1)?)?, 3 + bits.take(2)? as usize),
            17 => (0, 3 + bits.take(3)? as usize),
            18 => (0, 11 + bits.take(7)? as usize),
            _ => return None,
        };
        if i + repeat > lengths.len() {
            return None;
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    // A block must be able to end.
    if lengths[256] == 0 {
        return None;
    }
    Some((Huffman::new(&lengths[..literal_count])?, Huffman::new(&lengths[literal_count..])?))
}

fn block(bits: &mut Bits<'_>, out: &mut Vec<u8>, literals: &Huffman, distances: &Huffman, limit: usize) -> Option<()> {
    loop {
        let symbol = literals.decode(bits)? as usize;
        match symbol {
            0..=255 => out.push(symbol as u8),
            256 => return Some(()),
            257..=285 => {
                let i = symbol - 257;
                let len = LENGTH_BASE[i] as usize + bits.take(LENGTH_EXTRA[i] as u32)? as usize;
                let d = distances.decode(bits)? as usize;
                if d >= 30 {
                    return None;
                }
                let distance = DISTANCE_BASE[d] as usize + bits.take(DISTANCE_EXTRA[d] as u32)? as usize;
                if distance > out.len() {
                    return None;
                }
                // Byte by byte: the copy may overlap what it writes.
                let start = out.len() - distance;
                for k in 0..len {
                    let byte = out[start + k];
                    out.push(byte);
                }
            }
            _ => return None,
        }
        if out.len() > limit {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED: [u8; 16] = [0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x00, 0x3a, 0x2e, 0x06, 0x7d];

    /// Streams from Python's zlib, of known text.
    #[test]
    fn stored_fixed_and_dynamic_blocks() {
        // Stored (level 0).
        let stored = [0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o', 0x06, 0x2c, 0x02, 0x15];
        assert_eq!(zlib(&stored, 100).unwrap(), b"hello");
        // Fixed codes, with back-references.
        assert_eq!(zlib(&FIXED, 100).unwrap(), b"hello hello hello");
        // Over the limit: refused, not cut short.
        assert_eq!(zlib(&FIXED, 10), None);
        // Dynamic codes: 600 letters from a skewed alphabet.
        let mut seed = 1u32;
        let text: Vec<u8> = (0..600)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345) & 0x7fff_ffff;
                b"eeeeeeettttaaoinshrdlu  "[(seed >> 16) as usize % 24]
            })
            .collect();
        let dynamic = [0x78, 0xda, 0x2d, 0x52, 0x41, 0x0a, 0x04, 0x31, 0x08, 0xfb, 0x8a, 0x5f, 0x13, 0x1a, 0xb0, 0x20, 0x0a, 0x6a, 0xff, 0xbf, 0x71, 0xd8, 0xb9, 0x54, 0xad, 0x8d, 0x49, 0x9c, 0x2b, 0x73, 0x8e, 0x16, 0x14, 0x29, 0xaa, 0x26, 0x13, 0x83, 0x3b, 0x12, 0x81, 0xb0, 0x19, 0x37, 0xb0, 0x3a, 0x39, 0x70, 0x7f, 0x31, 0xf7, 0x29, 0x6a, 0x06, 0x5a, 0x82, 0x31, 0x0c, 0x43, 0x01, 0xbf, 0x11, 0x81, 0x6e, 0x94, 0x5f, 0xde, 0x67, 0xb6, 0x88, 0xd1, 0x93, 0x7b, 0x16, 0xea, 0x7d, 0x17, 0x72, 0x40, 0x50, 0xbe, 0xed, 0x71, 0xdc, 0x47, 0x04, 0x56, 0xf3, 0xce, 0x28, 0xbb, 0xc2, 0x71, 0x58, 0x18, 0xb6, 0x70, 0x2a, 0xd2, 0x3a, 0x1f, 0x52, 0xf1, 0x44, 0xe6, 0xb2, 0x4c, 0x18, 0x65, 0x03, 0x09, 0x7d, 0xd0, 0x44, 0x91, 0x42, 0x36, 0x10, 0xbc, 0xe9, 0x00, 0x85, 0x68, 0x7c, 0x6c, 0xf4, 0x36, 0x15, 0x8c, 0xcb, 0x9b, 0xf6, 0x1d, 0x0b, 0x1f, 0xe5, 0x9b, 0x44, 0x10, 0x4c, 0x17, 0x86, 0xa9, 0xba, 0x26, 0xf5, 0x32, 0x4c, 0x98, 0xdc, 0x95, 0xb3, 0x33, 0xc0, 0x8b, 0x36, 0xa6, 0x4b, 0xdc, 0x64, 0x11, 0xd3, 0x72, 0xe2, 0x54, 0xde, 0xc0, 0x6d, 0x67, 0xbb, 0xc3, 0x09, 0x10, 0x54, 0x77, 0xc8, 0x6f, 0xcc, 0x7a, 0x68, 0x98, 0xe3, 0x69, 0x53, 0x90, 0x5b, 0x23, 0x64, 0x8a, 0x03, 0x75, 0x2d, 0x72, 0x72, 0xbc, 0x9e, 0x55, 0x0a, 0x98, 0x9d, 0xa2, 0x82, 0xba, 0xfa, 0x39, 0x94, 0x98, 0xb3, 0x27, 0x3d, 0x7b, 0x3a, 0x4f, 0x5e, 0xb5, 0x06, 0x79, 0xe9, 0x39, 0xc8, 0x27, 0xab, 0xa3, 0x57, 0x6e, 0x5f, 0xa3, 0x27, 0x77, 0x75, 0xab, 0xbe, 0x8f, 0x14, 0x37, 0x66, 0xe2, 0x46, 0x35, 0xc5, 0x95, 0x91, 0xd9, 0x4d, 0x2e, 0x8e, 0x72, 0xc2, 0x87, 0xb4, 0x66, 0xe5, 0xd0, 0x1d, 0x12, 0x48, 0xad, 0x9d, 0x70, 0xf5, 0x9d, 0xe5, 0x78, 0x0c, 0xe6, 0xd1, 0x67, 0xa9, 0xee, 0x68, 0x76, 0xc8, 0x4b, 0xb4, 0xd0, 0xa3, 0xa2, 0xff, 0x42, 0xbf, 0x62, 0x28, 0x61, 0xc6, 0xaa, 0x9a, 0xfb, 0x58, 0xae, 0x6b, 0xb2, 0xfd, 0x49, 0x13, 0x92, 0xd3, 0xf9, 0x6f, 0x30, 0x96, 0xb7, 0x18, 0x72, 0x25, 0xb9, 0x26, 0x6e, 0x7b, 0xbd, 0xb9, 0xf6, 0xf0, 0x03, 0xd8, 0x7b, 0xea, 0xde];
        assert_eq!((dynamic[2] >> 1) & 3, 2, "a dynamic block");
        assert_eq!(zlib(&dynamic, 1000).unwrap(), text);
    }

    #[test]
    fn damaged_streams_are_refused() {
        // A fixed block opening with a copy (length code 257, distance
        // code 0): it would reach before the start.
        assert_eq!(inflate(&[0x03, 0x02], 100), None);
        assert_eq!(zlib(&[0x78], 100), None);
        assert_eq!(zlib(&[0x78, 0x9d, 0x00], 100), None);
        // Every truncation and many corruptions of a real stream, no panics.
        for len in 0..FIXED.len() {
            let _ = zlib(&FIXED[..len], 1000);
        }
        for i in 0..FIXED.len() {
            for v in 0..=255u8 {
                let mut d = FIXED;
                d[i] = v;
                let _ = zlib(&d, 1000);
            }
        }
    }
}
