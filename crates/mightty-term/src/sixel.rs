//! Sixel images: `DCS P1 ; P2 ; P3 q DATA ST`. Each data character is
//! a column of six pixels, drawn in the colour chosen with `#`; `!`
//! repeats one, `$` goes back to the start of the band and `-` down to
//! the next. Decoded as the bytes arrive, then shown like any other
//! image, so it scrolls, is cleared and is drawn the same way.

use std::sync::atomic::Ordering;

use crate::Term;
use crate::colors::Rgb;
use crate::graphics::{ImageData, MAX_SIDE, SERIAL};

/// The most pixels an image may have. A few bytes can ask for a great
/// many (`!` repeats a column any number of times), so this is well
/// within what images may take as a whole.
const MAX_PIXELS: usize = 1 << 24;

/// Colour registers, each image starting from the default ones.
const REGISTERS: usize = 256;

/// The VT340's colours for the first 16 registers, in percent.
#[rustfmt::skip]
const DEFAULT_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0], [20, 20, 80], [80, 13, 13], [20, 80, 20],
    [80, 20, 80], [20, 80, 80], [80, 80, 20], [53, 53, 53],
    [26, 26, 26], [33, 33, 60], [60, 26, 26], [33, 60, 33],
    [60, 33, 60], [33, 60, 60], [60, 60, 33], [80, 80, 80],
];

/// An image being decoded.
pub(crate) struct Sixel {
    palette: [[u8; 3]; REGISTERS],
    color: usize,
    /// Where the next column goes: `y` is the top of the band.
    x: usize,
    y: usize,
    /// Pixels drawn so far, `stride` wide and `rows` high, RGBA; alpha
    /// is 0 where nothing was drawn.
    pixels: Vec<u8>,
    stride: usize,
    rows: usize,
    /// The extent drawn, and the size the raster attributes ask for.
    width: usize,
    height: usize,
    raster: (usize, usize),
    /// Pixels not drawn are left clear (`P2 = 1`), not the background.
    transparent: bool,
    max_width: usize,
    max_height: usize,
    /// The command whose numbers are being read (`"`, `#` or `!`).
    command: Option<u8>,
    params: [u32; 5],
    n_params: usize,
    repeat: usize,
}

impl Sixel {
    /// An image `P2` (the second parameter) says what to do with, no
    /// wider than `max_width` pixels.
    pub(crate) fn new(p2: u16, max_width: u32) -> Sixel {
        let mut palette = [[0; 3]; REGISTERS];
        for (register, &[r, g, b]) in palette.iter_mut().zip(&DEFAULT_PALETTE) {
            *register = [percent(r.into()), percent(g.into()), percent(b.into())];
        }
        let max_width = (max_width as usize).clamp(1, MAX_SIDE as usize);
        Sixel {
            palette,
            color: 0,
            x: 0,
            y: 0,
            pixels: Vec::new(),
            stride: 0,
            rows: 0,
            width: 0,
            height: 0,
            raster: (0, 0),
            transparent: p2 == 1,
            max_width,
            max_height: (MAX_PIXELS / max_width).min(MAX_SIDE as usize),
            command: None,
            params: [0; 5],
            n_params: 0,
            repeat: 1,
        }
    }

    pub(crate) fn put(&mut self, byte: u8) {
        if self.command.is_some() {
            match byte {
                b'0'..=b'9' => {
                    if self.n_params == 0 {
                        self.n_params = 1;
                    }
                    if let Some(p) = self.params.get_mut(self.n_params - 1) {
                        *p = p.saturating_mul(10).saturating_add(u32::from(byte - b'0'));
                    }
                    return;
                }
                b';' => {
                    self.n_params = (self.n_params.max(1) + 1).min(self.params.len() + 1);
                    if let Some(p) = self.params.get_mut(self.n_params - 1) {
                        *p = 0;
                    }
                    return;
                }
                _ => self.finish_command(),
            }
        }
        match byte {
            b'"' | b'#' | b'!' => {
                self.command = Some(byte);
                self.params = [0; 5];
                self.n_params = 0;
            }
            b'$' => self.x = 0,
            b'-' => {
                self.x = 0;
                self.y = self.y.saturating_add(6);
            }
            b'?'..=b'~' => {
                let n = std::mem::replace(&mut self.repeat, 1);
                self.draw(byte - b'?', n);
            }
            // Line ends and other controls in the data mean nothing.
            _ => {}
        }
    }

    fn finish_command(&mut self) {
        let n = self.n_params.min(self.params.len());
        let p = &self.params[..n];
        match self.command.take() {
            Some(b'!') => self.repeat = p.first().map_or(1, |&n| n.max(1) as usize),
            Some(b'#') => {
                if let Some(&register) = p.first() {
                    self.color = register as usize % REGISTERS;
                }
                if let &[_, space, a, b, c] = p
                    && let Some(rgb) = color(space, a, b, c)
                {
                    self.palette[self.color] = rgb;
                }
            }
            Some(b'"') => {
                if let &[_, _, w, h, ..] = p {
                    self.raster = (
                        (w as usize).min(self.max_width),
                        (h as usize).min(self.max_height),
                    );
                }
            }
            _ => {}
        }
    }

    /// `bits`, a column of six pixels top first, `n` times across.
    fn draw(&mut self, bits: u8, n: usize) {
        let end = self.x.saturating_add(n).min(self.max_width);
        if self.x >= end {
            self.x = self.x.saturating_add(n);
            return;
        }
        self.width = self.width.max(end);
        let lowest = (0..6).rev().find(|bit| bits & 1 << bit != 0);
        if let Some(lowest) = lowest {
            let bottom = self.y.saturating_add(lowest + 1).min(self.max_height);
            if self.y < bottom {
                self.height = self.height.max(bottom);
                self.grow(end, bottom);
                let [r, g, b] = self.palette[self.color];
                for bit in 0..6 {
                    let y = self.y + bit;
                    if bits & 1 << bit == 0 || y >= bottom {
                        continue;
                    }
                    let row =
                        &mut self.pixels[(y * self.stride + self.x) * 4..][..(end - self.x) * 4];
                    for px in row.as_chunks_mut::<4>().0 {
                        *px = [r, g, b, 255];
                    }
                }
            }
        }
        self.x = self.x.saturating_add(n);
    }

    /// Room for `width` × `height` pixels, doubling as it grows.
    fn grow(&mut self, width: usize, height: usize) {
        if width <= self.stride && height <= self.rows {
            return;
        }
        let stride = if width > self.stride {
            (self.stride * 2).max(width).max(64).min(self.max_width)
        } else {
            self.stride
        };
        let rows = if height > self.rows {
            (self.rows * 2).max(height).max(6).min(self.max_height)
        } else {
            self.rows
        };
        if stride == self.stride {
            self.pixels.resize(stride * rows * 4, 0);
        } else {
            let mut pixels = vec![0; stride * rows * 4];
            for y in 0..self.rows {
                let old = &self.pixels[y * self.stride * 4..][..self.stride * 4];
                pixels[y * stride * 4..][..self.stride * 4].copy_from_slice(old);
            }
            self.pixels = pixels;
        }
        (self.stride, self.rows) = (stride, rows);
    }

    /// The image: what was drawn, padded to the size the raster attributes
    /// ask for, but no lower than the band reached, so a size alone can't
    /// make a huge image of nothing. What wasn't drawn is `background`
    /// unless the image asked for it clear. `None` if nothing was drawn.
    pub(crate) fn finish(mut self, background: Rgb) -> Option<ImageData> {
        self.finish_command();
        if self.width == 0 {
            return None;
        }
        let reached = self.y.saturating_add(6).min(self.max_height);
        let w = self.width.max(self.raster.0);
        let h = self.height.max(self.raster.1.min(reached)).max(1);
        let fill = match self.transparent {
            true => [0; 4],
            false => [background.0, background.1, background.2, 255],
        };
        let blank: Vec<u8> = fill.repeat(w);
        let mut pixels = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let drawn = match y < self.rows {
                true => &self.pixels[y * self.stride * 4..][..self.stride.min(w) * 4],
                false => &[][..],
            };
            let start = pixels.len();
            pixels.extend_from_slice(drawn);
            if !self.transparent {
                for px in pixels[start..].as_chunks_mut::<4>().0 {
                    if px[3] == 0 {
                        *px = fill;
                    }
                }
            }
            pixels.extend_from_slice(&blank[drawn.len()..]);
        }
        Some(ImageData {
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            width: w as u32,
            height: h as u32,
            pixels,
        })
    }
}

/// A percentage as a byte.
fn percent(p: u32) -> u8 {
    ((p.min(100) * 255 + 50) / 100) as u8
}

/// A colour register's new colour: `space` 1 is HLS (hue in degrees,
/// blue at 0, lightness and saturation in percent), 2 RGB in percent.
fn color(space: u32, a: u32, b: u32, c: u32) -> Option<[u8; 3]> {
    match space {
        2 => Some([percent(a), percent(b), percent(c)]),
        1 => {
            // The DEC hue circle starts at blue where the usual one starts
            // at red: a third of the way round.
            let h = f64::from((a % 360 + 240) % 360) / 360.0;
            let (l, s) = (f64::from(b.min(100)) / 100.0, f64::from(c.min(100)) / 100.0);
            let q = if l < 0.5 {
                l * (1.0 + s)
            } else {
                l + s - l * s
            };
            let p = 2.0 * l - q;
            let channel = |t: f64| {
                let t = t.rem_euclid(1.0);
                let v = if t < 1.0 / 6.0 {
                    p + (q - p) * 6.0 * t
                } else if t < 0.5 {
                    q
                } else if t < 2.0 / 3.0 {
                    p + (q - p) * (2.0 / 3.0 - t) * 6.0
                } else {
                    p
                };
                (v * 255.0).round() as u8
            };
            Some([channel(h + 1.0 / 3.0), channel(h), channel(h - 1.0 / 3.0)])
        }
        _ => None,
    }
}

impl Term {
    /// A Sixel image ended: shown at the cursor, the cursor then on the
    /// row its last band is on (or past its right edge, with mode 8452);
    /// in sixel display mode (80), at the top left, the cursor unmoved.
    pub(crate) fn show_sixel(&mut self, sixel: Sixel) {
        let Some(image) = sixel.finish(self.colors().background) else {
            return;
        };
        let (cw, ch) = self.cell_pixels;
        let (width, height) = (image.width, image.height);
        let (row, col) = (self.cursor.row, self.cursor.col);
        if self.modes.sixel_display {
            (self.cursor.row, self.cursor.col) = (0, 0);
            self.show_image(image, true);
            (self.cursor.row, self.cursor.col) = (row, col);
            return;
        }
        self.show_image(image, true);
        let last_band = (height - 1) / 6 * 6;
        for _ in 0..last_band / ch.max(1) {
            self.index();
        }
        if self.modes.sixel_cursor_right {
            let col = col + width.div_ceil(cw.max(1)) as usize;
            self.cursor.col = col.min(self.cols - 1);
            self.cursor.pending_wrap = col >= self.cols;
        } else {
            self.cursor.col = col;
            self.cursor.pending_wrap = false;
        }
    }

    /// XTSMGRAPHICS, `CSI ? item ; action S`: the number of colour
    /// registers (item 1) and the largest image (item 2) can be read
    /// (action 1, or 4 for the most allowed) or reset (action 2), but not
    /// set (action 3). There's no ReGIS (item 3).
    pub(crate) fn graphics_attributes(&mut self, item: usize, action: usize) {
        let reply = match (item, action) {
            (1, 1 | 2 | 4) => format!("\x1b[?1;0;{REGISTERS}S"),
            (2, 1 | 2 | 4) => {
                let (w, h) = self.sixel_max_size();
                format!("\x1b[?2;0;{w};{h}S")
            }
            (1 | 2, 3) => format!("\x1b[?{item};3;0S"),
            (1 | 2, _) => format!("\x1b[?{item};2;0S"),
            _ => format!("\x1b[?{item};1;0S"),
        };
        self.respond(reply.as_bytes());
    }

    /// The widest Sixel image shown: as wide as the screen.
    pub(crate) fn sixel_max_width(&self) -> u32 {
        (self.cell_pixels.0 * self.cols as u32).clamp(1, MAX_SIDE)
    }

    /// The largest Sixel image, for XTSMGRAPHICS.
    pub(crate) fn sixel_max_size(&self) -> (u32, u32) {
        let width = self.sixel_max_width();
        (width, (MAX_PIXELS as u32 / width).min(MAX_SIDE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(p2: u16, data: &[u8]) -> Option<ImageData> {
        let mut s = Sixel::new(p2, 100);
        for &b in data {
            s.put(b);
        }
        s.finish((1, 2, 3))
    }

    fn px(image: &ImageData, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * image.width + x) * 4) as usize;
        image.pixels[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn drawing() {
        // Red, a column of 6 then one with only the top pixel; a band
        // down, blue twice by repeat.
        let image = decode(1, b"#1;2;100;0;0~@-#2;2;0;0;100!2~").unwrap();
        assert_eq!((image.width, image.height), (2, 12));
        assert_eq!(px(&image, 0, 5), [255, 0, 0, 255]);
        assert_eq!(px(&image, 1, 0), [255, 0, 0, 255]);
        assert_eq!(px(&image, 1, 1), [0; 4]);
        assert_eq!(px(&image, 1, 11), [0, 0, 255, 255]);
        // `$` draws over the same band.
        let image = decode(1, b"#1;2;100;0;0~$#2;2;0;100;0@").unwrap();
        assert_eq!(px(&image, 0, 0), [0, 255, 0, 255]);
        assert_eq!(px(&image, 0, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn background_and_size() {
        // Not clear: undrawn pixels are the background, to the raster size.
        let image = decode(0, b"\"1;1;4;3#1;2;100;100;100@").unwrap();
        assert_eq!((image.width, image.height), (4, 3));
        assert_eq!(px(&image, 0, 0), [255, 255, 255, 255]);
        assert_eq!(px(&image, 3, 2), [1, 2, 3, 255]);
        // Nothing drawn: no image, whatever size was asked for.
        assert!(decode(0, b"#1;2;0;0;0").is_none());
        assert!(decode(0, b"\"1;1;100;100000").is_none());
        // A size asked for is only filled down to the band reached.
        let image = decode(0, b"\"1;1;10;100000@").unwrap();
        assert_eq!((image.width, image.height), (10, 6));
        // The default palette: register 2 is red.
        let image = decode(1, b"#2@").unwrap();
        assert_eq!(px(&image, 0, 0), [204, 33, 33, 255]);
    }

    #[test]
    fn colors() {
        // HLS, hue from blue: 0 blue, 120 red, 240 green.
        assert_eq!(color(1, 0, 50, 100), Some([0, 0, 255]));
        assert_eq!(color(1, 120, 50, 100), Some([255, 0, 0]));
        assert_eq!(color(1, 240, 50, 100), Some([0, 255, 0]));
        assert_eq!(color(2, 100, 50, 0), Some([255, 128, 0]));
        assert_eq!(color(3, 0, 0, 0), None);
        // Numbers as large as they come don't overflow.
        assert!(color(1, u32::MAX, u32::MAX, u32::MAX).is_some());
    }

    fn term() -> Term {
        let mut t = Term::new(24, 10);
        t.set_cell_pixels(10, 20);
        t
    }

    #[test]
    fn showing() {
        // 25 × 30 pixels: 3 cells by 2. The cursor ends on the row of the
        // last band (y 24, row 1), in the column the image started in.
        let mut t = term();
        t.feed(b"ab\x1bPq\"1;1;25;30#1;2;100;0;0!25~-!25~-!25~-!25~-!25~\x1b\\");
        let p = &t.image_placements()[0];
        assert_eq!((p.line, p.col, p.cells, p.image.width), (0, 2, [3, 2], 25));
        assert_eq!(t.cursor(), (1, 2));
        // Mode 8452: past its right edge instead.
        t.feed(b"\x1b[?8452h\x1bPq#1~\x1b\\");
        assert_eq!(t.cursor(), (1, 3));
        // Mode 80: at the top left, the cursor unmoved.
        t.feed(b"\x1b[?80h\x1bP0;1q!20~-~\x1b\\");
        let p = &t.image_placements()[2];
        assert_eq!((p.line, p.col), (0, 0));
        assert_eq!(t.cursor(), (1, 3));
        // Other DCS strings aren't images.
        t.feed(b"\x1bP+q544e\x1b\\");
        assert_eq!(t.image_placements().len(), 3);
    }

    #[test]
    fn replies() {
        let mut t = term();
        t.take_responses();
        t.feed(b"\x1b[?1;1S\x1b[?2;1S\x1b[?1;3;16S\x1b[?3;1S\x1b[c");
        assert_eq!(
            t.take_responses(),
            b"\x1b[?1;0;256S\x1b[?2;0;240;10000S\x1b[?1;3;0S\x1b[?3;1;0S\x1b[?62;4;22c"
        );
    }

    #[test]
    fn limits() {
        // A repeat past the widest image stops at its edge.
        let image = decode(1, b"!100000~").unwrap();
        assert_eq!((image.width, image.height), (100, 6));
        // So does a raster size, and many bands down.
        let mut s = Sixel::new(1, 100);
        for &b in b"\"1;1;100000;100000" {
            s.put(b);
        }
        for _ in 0..1_000_000 {
            s.put(b'-');
        }
        s.put(b'~');
        let image = s.finish((0, 0, 0)).unwrap();
        assert_eq!(image.width, 100);
        assert_eq!(image.height as usize, MAX_SIDE as usize);
    }
}
