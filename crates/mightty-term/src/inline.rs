//! Inline images, as `imgcat` sends them: OSC 1337 `File=ARGS:BASE64`,
//! or in parts, `MultipartFile=ARGS`, then `FilePart=BASE64` as many
//! times as it takes, then `FileEnd`. The image is shown at the cursor
//! and kept like an image sent with the kitty graphics protocol, so it
//! scrolls, is cleared and is drawn the same way.
//!
//! Only images shown inline (`inline=1`) are taken. A file sent to be
//! downloaded is ignored: output doesn't write files.

use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use crate::graphics::{ImageData, MAX_SIDE, MAX_STORED, SERIAL, png_fits};
use crate::{Term, base64};

/// The most image data taken, once decoded from base64.
const MAX_FILE: usize = 48 << 20;

/// Decodes an image file in a format other than PNG, which Mightty
/// decodes itself: its width, height and premultiplied RGBA pixels, top
/// row first, if it's an image within `limits`.
pub type ImageDecoder = fn(data: &[u8], limits: DecodeLimits) -> Option<(u32, u32, Vec<u8>)>;

/// The images an [`ImageDecoder`] decodes.
#[derive(Clone, Copy, Debug)]
pub struct DecodeLimits {
    /// The most pixels an image may have a side, as its file gives it.
    pub max_side: u32,
    /// The most bytes an image may take as RGBA, as its file gives it:
    /// checked before decoding, so a small file can't make a huge image.
    pub max_bytes: usize,
    /// The longest side shown: a larger image may come back shrunk to it,
    /// its aspect kept.
    pub shown_side: u32,
}

static DECODER: OnceLock<ImageDecoder> = OnceLock::new();

/// Sets the decoder for formats other than PNG, for every terminal. The
/// first one set stays. Without one, inline images must be PNGs.
pub fn set_image_decoder(decoder: ImageDecoder) {
    let _ = DECODER.set(decoder);
}

/// An image arriving in parts.
pub(crate) struct Multipart {
    args: Vec<u8>,
    data: Vec<u8>,
    /// Base64 not yet decoded: less than a quantum of four.
    carry: Vec<u8>,
    too_big: bool,
}

/// A width or height asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Size {
    Auto,
    Cells(u32),
    Pixels(u32),
    Percent(u32),
}

impl Size {
    fn parse(value: &[u8]) -> Option<Size> {
        let text = std::str::from_utf8(value).ok()?;
        let number = |digits: &str| digits.parse::<u32>().ok();
        Some(if text == "auto" {
            Size::Auto
        } else if let Some(n) = text.strip_suffix("px") {
            Size::Pixels(number(n)?)
        } else if let Some(n) = text.strip_suffix('%') {
            Size::Percent(number(n)?)
        } else {
            Size::Cells(number(text)?)
        })
    }

    /// In pixels: `cell` is a cell's size that way, `screen` the screen's.
    fn pixels(self, cell: u32, screen: u32) -> Option<u32> {
        match self {
            Size::Auto => None,
            Size::Cells(n) => Some(n.saturating_mul(cell)),
            Size::Pixels(n) => Some(n),
            Size::Percent(p) => Some((u64::from(screen) * u64::from(p.min(1000)) / 100) as u32),
        }
    }
}

/// What a `File=` or `MultipartFile=` asks for.
#[derive(Debug, PartialEq)]
struct Args {
    inline: bool,
    width: Size,
    height: Size,
    keep_aspect: bool,
    cursor_stays: bool,
}

impl Args {
    /// `key=value;key=value`; unknown keys are ignored.
    fn parse(args: &[u8]) -> Args {
        let mut a = Args {
            inline: false,
            width: Size::Auto,
            height: Size::Auto,
            keep_aspect: true,
            cursor_stays: false,
        };
        for pair in args.split(|&b| b == b';') {
            let Some(at) = pair.iter().position(|&b| b == b'=') else {
                continue;
            };
            let (key, value) = (&pair[..at], &pair[at + 1..]);
            match key {
                b"inline" => a.inline = value == b"1",
                b"width" => a.width = Size::parse(value).unwrap_or(Size::Auto),
                b"height" => a.height = Size::parse(value).unwrap_or(Size::Auto),
                b"preserveAspectRatio" => a.keep_aspect = value != b"0",
                b"doNotMoveCursor" => a.cursor_stays = value == b"1",
                _ => {}
            }
        }
        a
    }
}

impl Term {
    /// OSC 1337 with what follows `1337;`, its parts joined again.
    pub(crate) fn inline_image(&mut self, text: &[u8]) {
        if let Some(rest) = text.strip_prefix(b"File=") {
            self.multipart = None;
            let at = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
            let (args, payload) = (&rest[..at], rest.get(at + 1..).unwrap_or_default());
            if let Some(data) = base64::decode(payload).filter(|d| d.len() <= MAX_FILE) {
                self.show_file(args, &data);
            }
        } else if let Some(args) = text.strip_prefix(b"MultipartFile=") {
            // A download isn't kept, so its parts needn't be either.
            self.multipart = Args::parse(args).inline.then(|| Multipart {
                args: args.to_vec(),
                data: Vec::new(),
                carry: Vec::new(),
                too_big: false,
            });
        } else if let Some(payload) = text.strip_prefix(b"FilePart=") {
            if let Some(part) = &mut self.multipart {
                part.push(payload);
            }
        } else if text == b"FileEnd"
            && let Some(part) = self.multipart.take()
            && let Some((args, data)) = part.finish()
        {
            self.show_file(&args, &data);
        }
    }

    fn show_file(&mut self, args: &[u8], data: &[u8]) {
        let args = Args::parse(args);
        if !args.inline {
            return;
        }
        let (cw, ch) = self.cell_pixels;
        let screen = (cw * self.cols as u32, ch * self.rows as u32);
        let Some((width, height, pixels)) = decode(data, screen.0.max(screen.1)) else {
            return;
        };
        let (w, h) = fit(&args, (width, height), (cw, ch), screen);
        let pixels = if (w, h) == (width, height) {
            pixels
        } else {
            resample(&pixels, (width, height), (w, h))
        };
        let image = ImageData {
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            width: w,
            height: h,
            pixels,
        };
        self.show_image(image, args.cursor_stays);
    }
}

impl Multipart {
    fn push(&mut self, payload: &[u8]) {
        if self.too_big || self.data.len() > MAX_FILE || payload.len() > MAX_FILE * 4 / 3 + 4 {
            self.too_big = true;
            return;
        }
        self.carry.extend_from_slice(payload);
        let whole = self.carry.len() / 4 * 4;
        match base64::decode(&self.carry[..whole]) {
            Some(bytes) => self.data.extend(bytes),
            None => self.too_big = true,
        }
        self.carry.drain(..whole);
    }

    fn finish(self) -> Option<(Vec<u8>, Vec<u8>)> {
        let mut data = self.data;
        if !self.carry.is_empty() {
            data.extend(base64::decode(&self.carry)?);
        }
        (!self.too_big && data.len() <= MAX_FILE).then_some((self.args, data))
    }
}

/// `data` as pixels: a PNG by Mightty's own decoder, anything else by the
/// one set with [`set_image_decoder`], which may shrink it to `shown_side`.
fn decode(data: &[u8], shown_side: u32) -> Option<(u32, u32, Vec<u8>)> {
    let (width, height, pixels) = if data.starts_with(b"\x89PNG") {
        if !png_fits(data) {
            return None;
        }
        let image = mightty_image::png::decode_limited(data, MAX_SIDE)?;
        (image.width, image.height, image.pixels)
    } else {
        let limits = DecodeLimits {
            max_side: MAX_SIDE,
            max_bytes: MAX_STORED,
            shown_side: shown_side.clamp(1, MAX_SIDE),
        };
        (DECODER.get()?)(data, limits)?
    };
    let sane = (1..=MAX_SIDE).contains(&width)
        && (1..=MAX_SIDE).contains(&height)
        && pixels.len() == width as usize * height as usize * 4
        && pixels.len() <= MAX_STORED;
    sane.then_some((width, height, pixels))
}

/// The size to show an image of `size` pixels at, as `args` ask: its own
/// size by default. Never larger than the screen, which also bounds what
/// a tiny image asked to be shown huge costs. Keeps its aspect unless both
/// sides are given and `preserveAspectRatio=0`.
fn fit(args: &Args, size: (u32, u32), cell: (u32, u32), screen: (u32, u32)) -> (u32, u32) {
    let (iw, ih) = (u64::from(size.0), u64::from(size.1));
    let width = args.width.pixels(cell.0, screen.0).map(u64::from);
    let height = args.height.pixels(cell.1, screen.1).map(u64::from);
    // Within `w` × `h`, the image's aspect kept.
    let within = |w: u64, h: u64| {
        if w * ih <= h * iw {
            (w, (w * ih).div_ceil(iw))
        } else {
            ((h * iw).div_ceil(ih), h)
        }
    };
    let (w, h) = match (width, height) {
        (None, None) => (iw, ih),
        (Some(w), None) => (w, (w * ih).div_ceil(iw)),
        (None, Some(h)) => ((h * iw).div_ceil(ih), h),
        (Some(w), Some(h)) if args.keep_aspect => within(w, h),
        (Some(w), Some(h)) => (w, h),
    };
    let (sw, sh) = (u64::from(screen.0).max(1), u64::from(screen.1).max(1));
    let (w, h) = if w <= sw && h <= sh {
        (w, h)
    } else if width.is_some() && height.is_some() && !args.keep_aspect {
        (w.min(sw), h.min(sh))
    } else {
        within(sw, sh)
    };
    // Within the limits for any image, keeping what aspect it has.
    let side = u64::from(MAX_SIDE);
    let (w, h) = if w > side || h > side {
        let scale = side as f64 / w.max(h) as f64;
        ((w as f64 * scale) as u64, (h as f64 * scale) as u64)
    } else {
        (w, h)
    };
    let (mut w, mut h) = (w.max(1), h.max(1));
    while w * h * 4 > MAX_STORED as u64 {
        (w, h) = ((w / 2).max(1), (h / 2).max(1));
    }
    (w as u32, h as u32)
}

/// `pixels` (`from` wide and high, premultiplied RGBA) at size `to`: each
/// new pixel the average of the ones it covers, or the nearest one when
/// it's made larger.
fn resample(pixels: &[u8], from: (u32, u32), to: (u32, u32)) -> Vec<u8> {
    let (fw, fh) = (from.0 as usize, from.1 as usize);
    let (tw, th) = (to.0 as usize, to.1 as usize);
    // The source columns or rows `i` of `n` covers, out of `of`.
    let span = |i: usize, n: usize, of: usize| {
        let start = i * of / n;
        let end = ((i + 1) * of).div_ceil(n).clamp(start + 1, of);
        start..end
    };
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        let rows = span(y, th, fh);
        for x in 0..tw {
            let cols = span(x, tw, fw);
            let mut sum = [0u64; 4];
            for row in rows.clone() {
                let line = &pixels[(row * fw + cols.start) * 4..(row * fw + cols.end) * 4];
                for px in line.as_chunks::<4>().0 {
                    for (s, &v) in sum.iter_mut().zip(px) {
                        *s += u64::from(v);
                    }
                }
            }
            let n = (rows.len() * cols.len()) as u64;
            out.extend(sum.map(|s| ((s + n / 2) / n) as u8));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Args {
        Args::parse(text.as_bytes())
    }

    #[test]
    fn reading_args() {
        assert_eq!(
            args("name=eC5wbmc=;size=10;width=50%;height=3;inline=1;doNotMoveCursor=1"),
            Args {
                inline: true,
                width: Size::Percent(50),
                height: Size::Cells(3),
                keep_aspect: true,
                cursor_stays: true,
            }
        );
        let a = args("width=120px;height=auto;preserveAspectRatio=0;inline=0");
        assert_eq!(
            (a.width, a.height, a.keep_aspect, a.inline),
            (Size::Pixels(120), Size::Auto, false, false)
        );
        assert_eq!(args("width=wide").width, Size::Auto);
    }

    #[test]
    fn sizes() {
        let (cell, screen) = ((10, 20), (800, 480));
        let fit = |text: &str, size| fit(&args(text), size, cell, screen);
        // Its own size, unless that's bigger than the screen.
        assert_eq!(fit("", (100, 50)), (100, 50));
        assert_eq!(fit("", (1600, 400)), (800, 200));
        assert_eq!(fit("", (400, 960)), (200, 480));
        // One side given: the other keeps the aspect.
        assert_eq!(fit("width=20", (100, 50)), (200, 100));
        assert_eq!(fit("height=50%", (100, 50)), (480, 240));
        assert_eq!(fit("width=40px", (100, 50)), (40, 20));
        // Both: within them, or stretched to them.
        assert_eq!(fit("width=200px;height=200px", (100, 50)), (200, 100));
        assert_eq!(
            fit("width=200px;height=200px;preserveAspectRatio=0", (100, 50)),
            (200, 200)
        );
        // Never larger than the screen.
        assert_eq!(fit("width=100000px", (100, 50)), (800, 400));
        assert_eq!(fit("height=10000px", (100, 50)), (800, 400));
        assert_eq!(
            fit("width=9000px;height=100px;preserveAspectRatio=0", (1, 1)),
            (800, 100)
        );
    }

    #[test]
    fn showing_images() {
        let mut t = Term::new(24, 10);
        t.set_cell_pixels(10, 20);
        let png = base64::encode(&crate::graphics::tests::png(25, 30));
        // At the cursor, its own size; the cursor moves past it.
        t.feed(format!("ab\x1b]1337;File=size=1;inline=1:{png}\x07").as_bytes());
        let p = &t.image_placements()[0];
        assert_eq!((p.line, p.col, p.cells, p.image.width), (0, 2, [3, 2], 25));
        assert_eq!(t.cursor(), (1, 5));
        // In parts, two cells wide, the cursor left where it was.
        let (a, b) = png.split_at(10);
        for part in [
            "MultipartFile=width=2;inline=1;doNotMoveCursor=1".to_string(),
            format!("FilePart={a}"),
            format!("FilePart={b}"),
            "FileEnd".into(),
        ] {
            t.feed(format!("\x1b]1337;{part}\x07").as_bytes());
        }
        let p = &t.image_placements()[1];
        assert_eq!((p.image.width, p.image.height, p.cells), (20, 24, [2, 2]));
        assert_eq!(t.cursor(), (1, 5));
        // Not shown: a download, data that isn't an image, a part on its own.
        t.feed(format!("\x1b]1337;File=inline=0:{png}\x07").as_bytes());
        t.feed(b"\x1b]1337;File=inline=1:aGVsbG8=\x07");
        t.feed(format!("\x1b]1337;FilePart={png}\x07\x1b]1337;FileEnd\x07").as_bytes());
        assert_eq!(t.image_placements().len(), 2);
    }

    #[test]
    fn resampling() {
        // 2×2 to 1×1: the average.
        let px = [
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        assert_eq!(resample(&px, (2, 2), (1, 1)), [128, 128, 128, 255]);
        // 1×1 to 2×1: repeated.
        assert_eq!(
            resample(&[1, 2, 3, 4], (1, 1), (2, 1)),
            [1, 2, 3, 4, 1, 2, 3, 4]
        );
    }
}
