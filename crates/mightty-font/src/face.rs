//! TrueType and OpenType font files (the `sfnt` format): what a terminal
//! needs from one face, which is metrics, the character-to-glyph map,
//! advances and outlines. Font files come from disk and may be damaged, so
//! every read is bounds-checked and a bad table is an error, not a panic.

use std::fmt;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a font file, or not one we read (yet): CFF outlines, for one.
    Unsupported(&'static str),
    /// A table runs past the end of the file, or a value makes no sense.
    Damaged(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported(what) => write!(f, "unsupported font: {what}"),
            Error::Damaged(what) => write!(f, "damaged font: {what}"),
        }
    }
}

impl std::error::Error for Error {}

/// Big-endian reads that fail instead of panicking.
#[derive(Clone, Copy)]
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn u8(self, at: usize) -> Option<u8> {
        self.0.get(at).copied()
    }
    fn u16(self, at: usize) -> Option<u16> {
        Some(u16::from_be_bytes(self.0.get(at..at.checked_add(2)?)?.try_into().ok()?))
    }
    fn i16(self, at: usize) -> Option<i16> {
        self.u16(at).map(|v| v as i16)
    }
    fn u32(self, at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(self.0.get(at..at.checked_add(4)?)?.try_into().ok()?))
    }
    fn slice(self, at: usize, len: usize) -> Option<Bytes<'a>> {
        self.0.get(at..at.checked_add(len)?).map(Bytes)
    }
}

/// A point on an outline, in font units (y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    MoveTo(Point),
    LineTo(Point),
    /// A quadratic Bézier curve: control point, then end.
    QuadTo(Point, Point),
    Close,
}

/// Vertical metrics, in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub units_per_em: u16,
    /// Above the baseline (positive).
    pub ascender: i16,
    /// Below the baseline (negative).
    pub descender: i16,
    pub line_gap: i16,
}

/// One face of a font file. Owns the file's bytes, shared between the
/// faces of a collection.
#[derive(Clone)]
pub struct Face {
    data: Arc<[u8]>,
    /// Table offsets and lengths in `data`.
    glyf: (usize, usize),
    hmtx: (usize, usize),
    loca: (usize, usize),
    name: Option<(usize, usize)>,
    metrics: Metrics,
    num_glyphs: u16,
    num_h_metrics: u16,
    long_loca: bool,
    /// The cmap subtable used, from `cmap.0`.
    char_map: usize,
}

/// Composite glyphs can contain composites; real fonts nest a few deep.
const MAX_COMPONENT_DEPTH: u32 = 8;
/// Components in one glyph, counting nested ones: a damaged or hostile font
/// could otherwise fan out at every level and multiply into billions.
const MAX_COMPONENTS: u32 = 1024;
/// Segments in one glyph's outline; real glyphs have a few hundred.
const MAX_SEGMENTS: usize = 1 << 16;

impl Face {
    /// How many faces `data` holds: a collection (`.ttc`) holds several.
    pub fn count(data: &[u8]) -> u32 {
        let b = Bytes(data);
        match b.u32(0) {
            Some(0x7474_6366) => b.u32(8).unwrap_or(0), // 'ttcf'
            Some(_) => 1,
            None => 0,
        }
    }

    /// Face `index` of the font file `data`.
    pub fn parse(data: Arc<[u8]>, index: u32) -> Result<Face, Error> {
        let b = Bytes(&data);
        let start = match b.u32(0).ok_or(Error::Unsupported("too short"))? {
            0x7474_6366 => {
                let count = b.u32(8).ok_or(Error::Damaged("collection header"))?;
                if index >= count {
                    return Err(Error::Damaged("no such face in the collection"));
                }
                b.u32(12 + 4 * index as usize).ok_or(Error::Damaged("collection header"))? as usize
            }
            _ if index > 0 => return Err(Error::Damaged("no such face in the file")),
            _ => 0,
        };
        match b.u32(start) {
            Some(0x0001_0000 | 0x7472_7565) => {} // TrueType outlines ('true' on old Macs)
            Some(0x4F54_544F) => return Err(Error::Unsupported("CFF outlines")), // 'OTTO'
            _ => return Err(Error::Unsupported("not a TrueType or OpenType font")),
        }
        let tables = b.u16(start + 4).ok_or(Error::Damaged("table directory"))? as usize;
        let table = |tag: &[u8; 4]| -> Option<(usize, usize)> {
            (0..tables).find_map(|i| {
                let record = start + 12 + 16 * i;
                (b.slice(record, 4)?.0 == tag).then_some(())?;
                let (offset, length) = (b.u32(record + 8)? as usize, b.u32(record + 12)? as usize);
                // The whole table must be in the file.
                b.slice(offset, length)?;
                Some((offset, length))
            })
        };
        let required = |tag: &[u8; 4], what: &'static str| table(tag).ok_or(Error::Damaged(what));
        let head = required(b"head", "no head table")?;
        let hhea = required(b"hhea", "no hhea table")?;
        let maxp = required(b"maxp", "no maxp table")?;
        let glyf = table(b"glyf").ok_or(Error::Unsupported("no glyf table (bitmap-only or CFF font)"))?;

        let units_per_em = b.u16(head.0 + 18).filter(|&u| (16..=16384).contains(&u)).ok_or(Error::Damaged("units per em"))?;
        let long_loca = b.i16(head.0 + 50).ok_or(Error::Damaged("head"))? != 0;
        let metrics = Metrics {
            units_per_em,
            ascender: b.i16(hhea.0 + 4).ok_or(Error::Damaged("hhea"))?,
            descender: b.i16(hhea.0 + 6).ok_or(Error::Damaged("hhea"))?,
            line_gap: b.i16(hhea.0 + 8).ok_or(Error::Damaged("hhea"))?,
        };
        let num_h_metrics = b.u16(hhea.0 + 34).filter(|&n| n > 0).ok_or(Error::Damaged("hhea"))?;
        let num_glyphs = b.u16(maxp.0 + 4).ok_or(Error::Damaged("maxp"))?;
        let cmap = required(b"cmap", "no cmap table")?;
        let char_map = pick_char_map(b, cmap.0).ok_or(Error::Unsupported("no Unicode character map"))?;
        let hmtx = required(b"hmtx", "no hmtx table")?;
        if hmtx.1 < 4 * num_h_metrics as usize {
            return Err(Error::Damaged("hmtx shorter than hhea says"));
        }
        let loca = required(b"loca", "no loca table")?;
        let entry = if long_loca { 4 } else { 2 };
        if loca.1 < entry * (num_glyphs as usize + 1) {
            return Err(Error::Damaged("loca shorter than maxp says"));
        }
        let name = table(b"name");
        Ok(Face { glyf, hmtx, loca, name, metrics, num_glyphs, num_h_metrics, long_loca, char_map, data })
    }

    fn bytes(&self) -> Bytes<'_> {
        Bytes(&self.data)
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// The glyph for `c`, or `None` when the face hasn't one.
    pub fn glyph(&self, c: char) -> Option<u16> {
        let b = self.bytes();
        let at = self.char_map;
        let c = c as u32;
        let glyph = match b.u16(at)? {
            4 => {
                let c = u16::try_from(c).ok()?;
                let segments = b.u16(at + 6)? as usize / 2;
                let ends = at + 14;
                let starts = ends + 2 * segments + 2;
                let deltas = starts + 2 * segments;
                let ranges = deltas + 2 * segments;
                // The first segment whose end is at least `c`.
                let (mut lo, mut hi) = (0, segments);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if b.u16(ends + 2 * mid)? < c { lo = mid + 1 } else { hi = mid }
                }
                let i = lo;
                if i == segments || b.u16(starts + 2 * i)? > c {
                    return None;
                }
                let (start, delta, range) = (b.u16(starts + 2 * i)?, b.u16(deltas + 2 * i)?, b.u16(ranges + 2 * i)?);
                if range == 0 {
                    c.wrapping_add(delta)
                } else {
                    let index = ranges + 2 * i + range as usize + 2 * (c - start) as usize;
                    match b.u16(index)? {
                        0 => 0,
                        g => g.wrapping_add(delta),
                    }
                }
            }
            12 => {
                let groups = b.u32(at + 12)? as usize;
                let (mut lo, mut hi) = (0, groups);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let group = at + 16 + 12 * mid;
                    if b.u32(group + 4)? < c { lo = mid + 1 } else { hi = mid }
                }
                let group = at + 16 + 12 * lo;
                if lo == groups || b.u32(group)? > c {
                    return None;
                }
                u16::try_from(b.u32(group + 8)?.checked_add(c - b.u32(group)?)?).ok()?
            }
            _ => return None,
        };
        (glyph != 0 && glyph < self.num_glyphs).then_some(glyph)
    }

    /// How far `glyph` advances the pen, in font units.
    pub fn advance(&self, glyph: u16) -> u16 {
        let i = glyph.min(self.num_h_metrics - 1) as usize;
        self.bytes().u16(self.hmtx.0 + 4 * i).unwrap_or(0)
    }

    /// The PostScript name (name ID 6), which tells the faces of a collection apart.
    pub fn postscript_name(&self) -> Option<String> {
        let b = self.bytes();
        let (at, _) = self.name?;
        let count = b.u16(at + 2)? as usize;
        let strings = at + b.u16(at + 4)? as usize;
        (0..count).find_map(|i| {
            let record = at + 6 + 12 * i;
            let (platform, id) = (b.u16(record)?, b.u16(record + 6)?);
            (id == 6).then_some(())?;
            let (len, offset) = (b.u16(record + 8)? as usize, b.u16(record + 10)? as usize);
            let raw = b.slice(strings + offset, len)?.0;
            match platform {
                // UTF-16BE.
                0 | 3 => String::from_utf16(&raw.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect::<Vec<_>>()).ok(),
                // Mac Roman; PostScript names are ASCII.
                1 => raw.is_ascii().then(|| String::from_utf8_lossy(raw).into_owned()),
                _ => None,
            }
        })
    }

    /// `glyph`'s outline, in font units; empty for a blank glyph (space).
    pub fn outline(&self, glyph: u16) -> Result<Vec<Segment>, Error> {
        let mut out = Vec::new();
        let mut budget = MAX_COMPONENTS;
        self.add_outline(glyph, Transform::IDENTITY, 0, &mut budget, &mut out)?;
        Ok(out)
    }

    /// Where `glyph`'s data is in `glyf`; `None` for an empty glyph.
    fn glyph_data(&self, glyph: u16) -> Result<Option<Bytes<'_>>, Error> {
        if glyph >= self.num_glyphs {
            return Err(Error::Damaged("glyph index past the end"));
        }
        let b = self.bytes();
        let i = glyph as usize;
        let damaged = Error::Damaged("loca");
        let (start, end) = if self.long_loca {
            (b.u32(self.loca.0 + 4 * i).ok_or(damaged.clone())? as usize, b.u32(self.loca.0 + 4 * i + 4).ok_or(damaged)? as usize)
        } else {
            (2 * b.u16(self.loca.0 + 2 * i).ok_or(damaged.clone())? as usize, 2 * b.u16(self.loca.0 + 2 * i + 2).ok_or(damaged)? as usize)
        };
        if start == end {
            return Ok(None);
        }
        if start > end || end > self.glyf.1 {
            return Err(Error::Damaged("loca points outside glyf"));
        }
        Ok(Some(b.slice(self.glyf.0 + start, end - start).ok_or(Error::Damaged("glyf"))?))
    }

    /// `budget` is how many more components the whole glyph may use.
    fn add_outline(&self, glyph: u16, transform: Transform, depth: u32, budget: &mut u32, out: &mut Vec<Segment>) -> Result<(), Error> {
        let Some(g) = self.glyph_data(glyph)? else { return Ok(()) };
        let contours = g.i16(0).ok_or(Error::Damaged("glyph header"))?;
        if contours >= 0 {
            simple_glyph(g, contours as usize, transform, out).ok_or(Error::Damaged("simple glyph"))?;
            if out.len() > MAX_SEGMENTS {
                return Err(Error::Damaged("glyph outline too large"));
            }
            return Ok(());
        }
        if depth >= MAX_COMPONENT_DEPTH {
            return Err(Error::Damaged("composite glyphs nest too deep"));
        }
        // Composite: other glyphs, each moved and possibly scaled.
        const ARG_WORDS: u16 = 0x0001;
        const ARGS_ARE_XY: u16 = 0x0002;
        const SCALE: u16 = 0x0008;
        const MORE: u16 = 0x0020;
        const XY_SCALE: u16 = 0x0040;
        const TWO_BY_TWO: u16 = 0x0080;
        let damaged = || Error::Damaged("composite glyph");
        let mut at = 10;
        loop {
            let flags = g.u16(at).ok_or_else(damaged)?;
            let component = g.u16(at + 2).ok_or_else(damaged)?;
            at += 4;
            let (dx, dy) = if flags & ARG_WORDS != 0 {
                at += 4;
                (g.i16(at - 4).ok_or_else(damaged)? as f32, g.i16(at - 2).ok_or_else(damaged)? as f32)
            } else {
                at += 2;
                (g.u8(at - 2).ok_or_else(damaged)? as i8 as f32, g.u8(at - 1).ok_or_else(damaged)? as i8 as f32)
            };
            // Anchoring by matching points (rare) is placed at the origin.
            let (dx, dy) = if flags & ARGS_ARE_XY != 0 { (dx, dy) } else { (0.0, 0.0) };
            let f2dot14 = |at: usize| g.i16(at).map(|v| v as f32 / 16384.0).ok_or_else(damaged);
            let (a, b, c, d) = if flags & SCALE != 0 {
                at += 2;
                let s = f2dot14(at - 2)?;
                (s, 0.0, 0.0, s)
            } else if flags & XY_SCALE != 0 {
                at += 4;
                (f2dot14(at - 4)?, 0.0, 0.0, f2dot14(at - 2)?)
            } else if flags & TWO_BY_TWO != 0 {
                at += 8;
                (f2dot14(at - 8)?, f2dot14(at - 6)?, f2dot14(at - 4)?, f2dot14(at - 2)?)
            } else {
                (1.0, 0.0, 0.0, 1.0)
            };
            let inner = Transform { a, b, c, d, dx, dy };
            *budget = budget.checked_sub(1).ok_or(Error::Damaged("composite glyph has too many components"))?;
            self.add_outline(component, transform.then(inner), depth + 1, budget, out)?;
            if flags & MORE == 0 {
                return Ok(());
            }
        }
    }
}

/// x' = a·x + c·y + dx, y' = b·x + d·y + dy.
#[derive(Clone, Copy)]
struct Transform {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    dx: f32,
    dy: f32,
}

impl Transform {
    const IDENTITY: Transform = Transform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, dx: 0.0, dy: 0.0 };

    fn apply(self, p: Point) -> Point {
        Point { x: self.a * p.x + self.c * p.y + self.dx, y: self.b * p.x + self.d * p.y + self.dy }
    }

    /// `inner` applied first, then `self`.
    fn then(self, inner: Transform) -> Transform {
        Transform {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            dx: self.a * inner.dx + self.c * inner.dy + self.dx,
            dy: self.b * inner.dx + self.d * inner.dy + self.dy,
        }
    }
}

/// A glyph made of contours of points on and off the curve: two points off
/// the curve in a row have an implied one on it, half way between.
fn simple_glyph(g: Bytes<'_>, contours: usize, transform: Transform, out: &mut Vec<Segment>) -> Option<()> {
    const ON_CURVE: u8 = 0x01;
    const X_SHORT: u8 = 0x02;
    const Y_SHORT: u8 = 0x04;
    const REPEAT: u8 = 0x08;
    const X_SAME_OR_POSITIVE: u8 = 0x10;
    const Y_SAME_OR_POSITIVE: u8 = 0x20;
    if contours == 0 {
        return Some(());
    }
    let ends: Vec<usize> = (0..contours).map(|i| g.u16(10 + 2 * i).map(usize::from)).collect::<Option<_>>()?;
    // Ends must rise; the last is the point count.
    if ends.windows(2).any(|w| w[1] <= w[0]) {
        return None;
    }
    let points = ends[contours - 1] + 1;
    let instructions = g.u16(10 + 2 * contours)? as usize;
    let mut at = 12 + 2 * contours + instructions;
    let mut flags = Vec::with_capacity(points);
    while flags.len() < points {
        let flag = g.u8(at)?;
        at += 1;
        let repeat = if flag & REPEAT != 0 {
            at += 1;
            g.u8(at - 1)? as usize + 1
        } else {
            1
        };
        flags.extend(std::iter::repeat_n(flag, repeat.min(points - flags.len())));
    }
    let mut coordinates = |short: u8, same_or_positive: u8| -> Option<Vec<f32>> {
        let mut value = 0i32;
        let mut out = Vec::with_capacity(points);
        for &flag in &flags {
            if flag & short != 0 {
                let delta = g.u8(at)? as i32;
                at += 1;
                value += if flag & same_or_positive != 0 { delta } else { -delta };
            } else if flag & same_or_positive == 0 {
                value += g.i16(at)? as i32;
                at += 2;
            }
            out.push(value as f32);
        }
        Some(out)
    };
    let xs = coordinates(X_SHORT, X_SAME_OR_POSITIVE)?;
    let ys = coordinates(Y_SHORT, Y_SAME_OR_POSITIVE)?;
    let point = |i: usize| transform.apply(Point { x: xs[i], y: ys[i] });
    let mid = |p: Point, q: Point| Point { x: (p.x + q.x) / 2.0, y: (p.y + q.y) / 2.0 };

    let mut first = 0;
    for &last in &ends {
        let range = first..=last;
        first = last + 1;
        let contour: Vec<(Point, bool)> = range.map(|i| (point(i), flags[i] & ON_CURVE != 0)).collect();
        if contour.is_empty() {
            continue;
        }
        // Start on the curve: at an on-curve point, else half way between
        // the last point and the first (all off the curve).
        let n = contour.len();
        let (start, begin, steps) = match contour.iter().position(|p| p.1) {
            Some(i) => (contour[i].0, i + 1, n - 1),
            None => (mid(contour[n - 1].0, contour[0].0), 0, n),
        };
        out.push(Segment::MoveTo(start));
        let mut control: Option<Point> = None;
        for k in 0..steps {
            let (p, on) = contour[(begin + k) % n];
            match (on, control) {
                (true, None) => out.push(Segment::LineTo(p)),
                (true, Some(c)) => out.push(Segment::QuadTo(c, p)),
                (false, Some(c)) => out.push(Segment::QuadTo(c, mid(c, p))),
                (false, None) => {}
            }
            control = (!on).then_some(p);
        }
        // Back to the start.
        out.push(match control {
            Some(c) => Segment::QuadTo(c, start),
            None => Segment::LineTo(start),
        });
        out.push(Segment::Close);
    }
    Some(())
}

/// The cmap subtable to use: full Unicode (format 12) if there is one,
/// else the Basic Multilingual Plane (format 4).
fn pick_char_map(b: Bytes<'_>, cmap: usize) -> Option<usize> {
    let count = b.u16(cmap + 2)? as usize;
    let mut best: Option<(u8, usize)> = None;
    for i in 0..count {
        let record = cmap + 4 + 8 * i;
        let (platform, encoding) = (b.u16(record)?, b.u16(record + 2)?);
        let at = cmap + b.u32(record + 4)? as usize;
        let format = b.u16(at)?;
        let rank = match (platform, encoding, format) {
            (3, 10, 12) | (0, 4 | 6, 12) => 2,
            (3, 1, 4) | (0, 0..=3, 4) => 1,
            _ => continue,
        };
        if best.is_none_or(|(r, _)| rank > r) {
            best = Some((rank, at));
        }
    }
    best.map(|(_, at)| at)
}
