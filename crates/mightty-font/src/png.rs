//! PNG images, as colour emoji fonts store them (sbix, CBDT): every colour
//! type and bit depth, palettes, transparency and interlacing, decoded to
//! premultiplied RGBA. Images come from font files, so sizes are limited
//! and damage is an error.

use crate::inflate;

/// An image, premultiplied RGBA, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The largest side an image may have: emoji are a few hundred pixels.
const MAX_SIDE: u32 = 4096;

pub(crate) fn decode(data: &[u8]) -> Option<Image> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
    if data.get(..8)? != SIGNATURE {
        return None;
    }
    let be32 = |at: usize| -> Option<u32> { Some(u32::from_be_bytes(data.get(at..at.checked_add(4)?)?.try_into().ok()?)) };
    let mut at = 8;
    let mut header: Option<Header> = None;
    let (mut palette, mut transparency, mut compressed): (&[u8], &[u8], Vec<u8>) = (&[], &[], Vec::new());
    loop {
        let len = be32(at)? as usize;
        let kind = data.get(at + 4..at + 8)?;
        let body = data.get(at + 8..(at + 8).checked_add(len)?)?;
        // The CRC after each chunk isn't checked, as in most decoders' release builds.
        at = at + 12 + len;
        match kind {
            b"IHDR" => header = Some(Header::parse(body)?),
            b"PLTE" => palette = body,
            b"tRNS" => transparency = body,
            b"IDAT" => compressed.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
    }
    let h = header?;
    // The filtered data: each row a filter byte and its bytes, per pass.
    let limit = Header::passes(h.interlaced, h.width, h.height).map(|(w, rows)| if w == 0 { 0 } else { rows * (1 + h.row_bytes(w)) }).sum();
    let raw = inflate::zlib(&compressed, limit)?;
    if raw.len() != limit {
        return None;
    }
    let mut pixels = vec![0u8; h.width as usize * h.height as usize * 4];
    let mut at = 0;
    for (pass, (w, rows)) in Header::passes(h.interlaced, h.width, h.height).enumerate() {
        if w == 0 {
            continue;
        }
        let row_bytes = h.row_bytes(w);
        let bpp = h.bytes_per_pixel();
        let mut previous = vec![0u8; row_bytes];
        for y in 0..rows {
            let filter = raw[at];
            let mut row = raw[at + 1..at + 1 + row_bytes].to_vec();
            at += 1 + row_bytes;
            unfilter(filter, &mut row, &previous, bpp)?;
            for x in 0..w {
                let rgba = h.pixel(&row, x, palette, transparency)?;
                let (px, py) = Header::place(h.interlaced, pass, x, y);
                let i = (py * h.width as usize + px) * 4;
                let a = rgba[3] as u32;
                // Premultiplied, rounded.
                let pre = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                pixels[i..i + 4].copy_from_slice(&[pre(rgba[0]), pre(rgba[1]), pre(rgba[2]), rgba[3]]);
            }
            previous = row;
        }
    }
    Some(Image { width: h.width, height: h.height, pixels })
}

struct Header {
    width: u32,
    height: u32,
    depth: u8,
    color: u8,
    interlaced: bool,
}

/// Adam7's passes: where each starts and how far apart its pixels are.
const ADAM7: [(usize, usize, usize, usize); 7] = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];

impl Header {
    fn parse(b: &[u8]) -> Option<Header> {
        let width = u32::from_be_bytes(b.get(0..4)?.try_into().ok()?);
        let height = u32::from_be_bytes(b.get(4..8)?.try_into().ok()?);
        let (depth, color, compression, filter, interlace) = (*b.get(8)?, *b.get(9)?, *b.get(10)?, *b.get(11)?, *b.get(12)?);
        let valid = match color {
            0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
            3 => matches!(depth, 1 | 2 | 4 | 8),
            2 | 4 | 6 => matches!(depth, 8 | 16),
            _ => false,
        };
        let sized = (1..=MAX_SIDE).contains(&width) && (1..=MAX_SIDE).contains(&height);
        (valid && sized && compression == 0 && filter == 0 && interlace <= 1).then_some(Header { width, height, depth, color, interlaced: interlace == 1 })
    }

    fn channels(&self) -> usize {
        match self.color {
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 1,
        }
    }

    fn row_bytes(&self, width: usize) -> usize {
        (width * self.channels() * self.depth as usize).div_ceil(8)
    }

    /// For filtering: whole bytes per pixel, at least one.
    fn bytes_per_pixel(&self) -> usize {
        (self.channels() * self.depth as usize).div_ceil(8)
    }

    /// Each pass's width and rows: one pass of the whole image unless interlaced.
    fn passes(interlaced: bool, width: u32, height: u32) -> impl Iterator<Item = (usize, usize)> {
        let (w, h) = (width as usize, height as usize);
        let passes: &[(usize, usize, usize, usize)] = if interlaced { &ADAM7 } else { &[(0, 0, 1, 1)] };
        passes.iter().map(move |&(x0, y0, dx, dy)| {
            let (pw, ph) = ((w + dx - 1 - x0) / dx, (h + dy - 1 - y0) / dy);
            if w <= x0 || h <= y0 { (0, 0) } else { (pw, ph) }
        })
    }

    fn place(interlaced: bool, pass: usize, x: usize, y: usize) -> (usize, usize) {
        if !interlaced {
            return (x, y);
        }
        let (x0, y0, dx, dy) = ADAM7[pass];
        (x0 + x * dx, y0 + y * dy)
    }

    /// Pixel `x` of an unfiltered row as straight RGBA.
    fn pixel(&self, row: &[u8], x: usize, palette: &[u8], transparency: &[u8]) -> Option<[u8; 4]> {
        let depth = self.depth as usize;
        // Sample `i` of the pixel at its own depth, and scaled to 8 bits.
        let sample = |i: usize| -> Option<(u16, u8)> {
            let index = x * self.channels() + i;
            match depth {
                16 => {
                    let v = u16::from_be_bytes([*row.get(2 * index)?, *row.get(2 * index + 1)?]);
                    Some((v, (v >> 8) as u8))
                }
                8 => row.get(index).map(|&v| (v as u16, v)),
                _ => {
                    let bit = index * depth;
                    let v = (*row.get(bit / 8)? >> (8 - depth - bit % 8)) & ((1 << depth) - 1) as u8;
                    Some((v as u16, if self.color == 3 { v } else { (v as u32 * 255 / ((1 << depth) - 1)) as u8 }))
                }
            }
        };
        let key = |i: usize| transparency.get(2 * i..2 * i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        Some(match self.color {
            0 => {
                let (raw, g) = sample(0)?;
                [g, g, g, if key(0) == Some(raw) { 0 } else { 255 }]
            }
            2 => {
                let ((r0, r), (g0, g), (b0, b)) = (sample(0)?, sample(1)?, sample(2)?);
                let clear = key(0) == Some(r0) && key(1) == Some(g0) && key(2) == Some(b0);
                [r, g, b, if clear { 0 } else { 255 }]
            }
            3 => {
                let i = sample(0)?.1 as usize;
                let c = palette.get(3 * i..3 * i + 3)?;
                [c[0], c[1], c[2], transparency.get(i).copied().unwrap_or(255)]
            }
            4 => {
                let g = sample(0)?.1;
                [g, g, g, sample(1)?.1]
            }
            _ => [sample(0)?.1, sample(1)?.1, sample(2)?.1, sample(3)?.1],
        })
    }
}

fn unfilter(filter: u8, row: &mut [u8], previous: &[u8], bpp: usize) -> Option<()> {
    for i in 0..row.len() {
        let left = if i >= bpp { row[i - bpp] } else { 0 };
        let up = previous[i];
        let up_left = if i >= bpp { previous[i - bpp] } else { 0 };
        let predicted = match filter {
            0 => 0,
            1 => left,
            2 => up,
            3 => ((left as u16 + up as u16) / 2) as u8,
            4 => {
                let p = left as i16 + up as i16 - up_left as i16;
                let (pa, pb, pc) = ((p - left as i16).abs(), (p - up as i16).abs(), (p - up_left as i16).abs());
                if pa <= pb && pa <= pc {
                    left
                } else if pb <= pc {
                    up
                } else {
                    up_left
                }
            }
            _ => return None,
        };
        row[i] = row[i].wrapping_add(predicted);
    }
    Some(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PNG of `rows` (each a filter byte and its bytes), stored without
    /// compression: enough to test the decoder on any layout.
    pub(crate) fn png(width: u32, height: u32, depth: u8, color: u8, interlace: u8, extra: &[(&[u8; 4], Vec<u8>)], rows: &[u8]) -> Vec<u8> {
        let chunk = |kind: &[u8; 4], body: &[u8]| [&(body.len() as u32).to_be_bytes()[..], kind, body, &[0; 4]].concat();
        let mut ihdr = [width.to_be_bytes(), height.to_be_bytes()].concat();
        ihdr.extend([depth, color, 0, 0, interlace]);
        // zlib: header, stored blocks of at most 65535 bytes, a dummy checksum.
        let mut z = vec![0x78, 0x01];
        let blocks: Vec<&[u8]> = if rows.is_empty() { vec![&[]] } else { rows.chunks(65535).collect() };
        for (i, block) in blocks.iter().enumerate() {
            z.push((i + 1 == blocks.len()) as u8);
            z.extend((block.len() as u16).to_le_bytes());
            z.extend((!(block.len() as u16)).to_le_bytes());
            z.extend(*block);
        }
        z.extend([0; 4]);
        let mut out = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
        out.extend(chunk(b"IHDR", &ihdr));
        for (kind, body) in extra {
            out.extend(chunk(kind, body));
        }
        out.extend(chunk(b"IDAT", &z));
        out.extend(chunk(b"IEND", &[]));
        out
    }

    #[test]
    fn colour_types_and_depths() {
        // RGBA, 8 bits: one half-transparent red pixel, premultiplied.
        let image = decode(&png(1, 1, 8, 6, 0, &[], &[0, 255, 0, 0, 128])).unwrap();
        assert_eq!(image.pixels, [128, 0, 0, 128]);
        // Grey, 1 bit: black and white.
        let image = decode(&png(2, 1, 1, 0, 0, &[], &[0, 0b0100_0000])).unwrap();
        assert_eq!(image.pixels, [0, 0, 0, 255, 255, 255, 255, 255]);
        // Palette, 2 bits, with transparency for entry 1.
        let palette = vec![10, 20, 30, 40, 50, 60];
        let image = decode(&png(2, 1, 2, 3, 0, &[(b"PLTE", palette), (b"tRNS", vec![255, 0])], &[0, 0b0001_0000])).unwrap();
        assert_eq!(image.pixels, [10, 20, 30, 255, 0, 0, 0, 0]);
        // RGB, 16 bits: the high bytes.
        let image = decode(&png(1, 1, 16, 2, 0, &[], &[0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc])).unwrap();
        assert_eq!(image.pixels, [0x12, 0x56, 0x9a, 255]);
    }

    #[test]
    fn filters() {
        // Grey 8-bit, 3×2: Sub on the first row, Up on the second, then Paeth.
        let rows = [1, 10, 5, 5, 2, 1, 1, 1];
        let image = decode(&png(3, 2, 8, 0, 0, &[], &rows)).unwrap();
        let grey: Vec<u8> = image.pixels.chunks(4).map(|p| p[0]).collect();
        assert_eq!(grey, [10, 15, 20, 11, 16, 21]);
        let rows = [4, 10, 5, 5, 3, 2, 2, 2];
        let image = decode(&png(3, 2, 8, 0, 0, &[], &rows)).unwrap();
        let grey: Vec<u8> = image.pixels.chunks(4).map(|p| p[0]).collect();
        // Paeth from nothing above is Sub: 10, 15, 20; Average: (left + up) / 2 added.
        assert_eq!(grey, [10, 15, 20, 7, 13, 18]);
    }

    #[test]
    fn interlaced_matches_plain() {
        // 9×9 grey, each pixel its own index: the same image either way.
        let value = |x: usize, y: usize| (y * 9 + x) as u8;
        let mut plain = Vec::new();
        for y in 0..9 {
            plain.push(0);
            plain.extend((0..9).map(|x| value(x, y)));
        }
        let mut interlaced = Vec::new();
        for &(x0, y0, dx, dy) in &ADAM7 {
            for y in (y0..9).step_by(dy) {
                interlaced.push(0);
                interlaced.extend((x0..9).step_by(dx).map(|x| value(x, y)));
            }
        }
        assert_eq!(decode(&png(9, 9, 8, 0, 1, &[], &interlaced)), decode(&png(9, 9, 8, 0, 0, &[], &plain)));
    }

    /// A 12×12 RGBA image from Python's zlib (dynamic codes, every filter),
    /// damaged a byte at a time every way: errors or images, never panics.
    #[test]
    fn damaged_compressed_images_dont_panic() {
        let real = [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 12, 0, 0, 0, 12, 8, 6, 0, 0, 0, 86, 117, 92, 231, 0, 0, 0, 230, 73, 68, 65, 84, 120, 218, 21, 207, 33, 150, 67, 33, 16, 5, 209, 78, 50, 226, 75, 36, 18, 217, 18, 137, 68, 34, 145, 44, 135, 157, 103, 110, 68, 157, 42, 195, 233, 71, 68, 220, 251, 196, 253, 22, 84, 221, 56, 209, 245, 224, 137, 165, 55, 31, 188, 226, 185, 223, 7, 229, 185, 183, 114, 67, 234, 206, 3, 83, 47, 222, 56, 250, 29, 197, 131, 114, 111, 225, 138, 166, 147, 59, 134, 158, 188, 176, 245, 225, 79, 84, 147, 170, 11, 168, 186, 113, 162, 235, 193, 19, 75, 111, 62, 248, 139, 230, 2, 74, 51, 137, 27, 82, 119, 30, 152, 122, 241, 198, 209, 17, 233, 65, 154, 196, 21, 77, 39, 119, 12, 61, 121, 97, 235, 147, 191, 79, 119, 147, 186, 11, 168, 186, 113, 162, 235, 193, 19, 75, 111, 62, 120, 199, 112, 1, 101, 152, 196, 13, 169, 59, 15, 76, 189, 120, 227, 232, 79, 76, 15, 166, 73, 92, 209, 116, 114, 199, 208, 147, 23, 182, 62, 243, 247, 233, 101, 210, 114, 1, 85, 55, 78, 116, 61, 120, 98, 233, 205, 7, 17, 219, 5, 148, 109, 18, 55, 164, 238, 60, 48, 245, 226, 141, 163, 95, 113, 60, 56, 38, 113, 69, 211, 201, 29, 67, 79, 94, 216, 250, 240, 63, 85, 150, 241, 54, 132, 50, 80, 41, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130];
        let image = decode(&real).unwrap();
        assert_eq!((image.width, image.height), (12, 12));
        for i in 0..real.len() {
            for v in [0, 1, 2, 0x0f, 0x10, 0x40, 0x7f, 0x80, 0xc0, 0xf0, 0xfe, 0xff, real[i] ^ 1, real[i] ^ 0x80] {
                let mut d = real;
                d[i] = v;
                let _ = decode(&d);
            }
        }
    }

    #[test]
    fn damaged_images_are_refused() {
        let good = png(2, 2, 8, 6, 0, &[], &[0; 18]);
        assert!(decode(&good).is_some());
        // Too little pixel data, too much, a bad filter, an absurd size.
        assert_eq!(decode(&png(2, 2, 8, 6, 0, &[], &[0; 17])), None);
        assert_eq!(decode(&png(2, 2, 8, 6, 0, &[], &[0; 19])), None);
        assert_eq!(decode(&png(1, 1, 8, 0, 0, &[], &[9, 0])), None);
        assert_eq!(decode(&png(100_000, 1, 8, 0, 0, &[], &[])), None);
        // A palette index past the palette.
        assert_eq!(decode(&png(1, 1, 8, 3, 0, &[(b"PLTE", vec![1, 2, 3])], &[0, 1])), None);
        for len in 0..good.len() {
            let _ = decode(&good[..len]);
        }
        for i in 0..good.len() {
            for v in [0, 1, 0x7f, 0x80, 0xff] {
                let mut d = good.clone();
                d[i] = v;
                let _ = decode(&d);
            }
        }
    }
}
