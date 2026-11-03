//! CFF outlines (OpenType fonts whose file starts 'OTTO'): glyphs as Type 2
//! charstrings, small programs of cubic curves that can call shared
//! subroutines. Both kinds of CFF are read: name-keyed, with one private
//! dictionary, and CID-keyed (most CJK fonts), whose glyphs each pick one
//! of several. Programs are untrusted, so the interpreter limits the
//! stack, how deep calls nest and how many operators a glyph may run.

use crate::face::{Bytes, Error, Point, Segment};

/// Arguments on the stack at once; Type 2 allows 48.
const MAX_STACK: usize = 48;
/// Subroutine calls in progress; Type 2 allows 10.
const MAX_CALL_DEPTH: usize = 10;
/// Operators one glyph may run, subroutines included: real glyphs run a few
/// hundred, and calls could otherwise multiply like composite glyphs.
const MAX_OPERATORS: u32 = 20_000;

/// A CFF INDEX: `count` objects, located by offsets.
#[derive(Clone, Copy, Default)]
struct Index {
    count: usize,
    /// Where the offsets start, each `offset_size` bytes.
    offsets: usize,
    offset_size: usize,
    /// Offsets are counted from one byte before here.
    data: usize,
}

impl Index {
    /// The INDEX at `at`, and where the bytes after it start.
    fn parse(b: Bytes<'_>, at: usize) -> Option<(Index, usize)> {
        let count = b.u16(at)? as usize;
        if count == 0 {
            return Some((Index::default(), at + 2));
        }
        let offset_size = b.u8(at + 2)? as usize;
        if !(1..=4).contains(&offset_size) {
            return None;
        }
        let offsets = at + 3;
        let data = offsets + (count + 1) * offset_size - 1;
        let index = Index { count, offsets, offset_size, data };
        let end = data + index.offset(b, count)?;
        b.slice(at, end - at)?;
        Some((index, end))
    }

    fn offset(&self, b: Bytes<'_>, i: usize) -> Option<usize> {
        let at = self.offsets + i * self.offset_size;
        let raw = b.slice(at, self.offset_size)?.0;
        Some(raw.iter().fold(0usize, |v, &byte| (v << 8) | byte as usize))
    }

    fn get<'a>(&self, b: Bytes<'a>, i: usize) -> Option<Bytes<'a>> {
        if i >= self.count {
            return None;
        }
        let (start, end) = (self.offset(b, i)?, self.offset(b, i + 1)?);
        if start == 0 || end < start {
            return None;
        }
        b.slice(self.data + start, end - start)
    }

    /// Subroutine numbers are stored minus a bias that depends on the count.
    fn bias(&self) -> i32 {
        match self.count {
            0..1240 => 107,
            1240..33900 => 1131,
            _ => 32768,
        }
    }
}

/// A DICT's entries: operator (12 x as 1200 + x) and its operands.
fn dict(d: Bytes<'_>) -> Option<Vec<(u16, Vec<f64>)>> {
    let mut entries = Vec::new();
    let mut operands = Vec::new();
    let mut at = 0;
    while at < d.0.len() {
        let b0 = d.u8(at)?;
        at += 1;
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    at += 1;
                    1200 + d.u8(at - 1)? as u16
                } else {
                    b0 as u16
                };
                entries.push((op, std::mem::take(&mut operands)));
            }
            28 => {
                operands.push(d.i16(at)? as f64);
                at += 2;
            }
            29 => {
                operands.push(d.u32(at)? as i32 as f64);
                at += 4;
            }
            30 => {
                // A real number in nibbles; its value isn't needed here.
                while d.u8(at)? & 0x0F != 0x0F && d.u8(at)? & 0xF0 != 0xF0 {
                    at += 1;
                }
                at += 1;
                operands.push(0.0);
            }
            32..=246 => operands.push(b0 as f64 - 139.0),
            247..=250 => {
                operands.push((b0 as f64 - 247.0) * 256.0 + d.u8(at)? as f64 + 108.0);
                at += 1;
            }
            251..=254 => {
                operands.push(-(b0 as f64 - 251.0) * 256.0 - d.u8(at)? as f64 - 108.0);
                at += 1;
            }
            _ => return None,
        }
        if operands.len() > MAX_STACK {
            return None;
        }
    }
    Some(entries)
}

fn lookup(entries: &[(u16, Vec<f64>)], op: u16) -> Option<&[f64]> {
    entries.iter().find(|e| e.0 == op).map(|e| e.1.as_slice())
}

/// An offset operand, which must be a non-negative whole number.
fn offset(v: f64) -> Option<usize> {
    (v >= 0.0 && v.fract() == 0.0).then_some(v as usize)
}

/// The local subroutines a private DICT at (`size`, `at`) names, if any.
fn private_subrs(b: Bytes<'_>, private: &[f64]) -> Option<Index> {
    let [size, at] = private else { return None };
    let (size, at) = (offset(*size)?, offset(*at)?);
    let entries = dict(b.slice(at, size)?)?;
    let subrs = offset(*lookup(&entries, 19)?.first()?)?;
    Some(Index::parse(b, at.checked_add(subrs)?)?.0)
}

#[derive(Clone)]
pub(crate) struct Cff {
    /// The CFF table's offset in the file; every offset here is from it.
    table: usize,
    length: usize,
    char_strings: Index,
    global_subrs: Index,
    /// Name-keyed: the one set of local subroutines.
    local_subrs: Option<Index>,
    /// CID-keyed: which font DICT each glyph uses, and each one's subroutines.
    cid: Option<(usize, Vec<Option<Index>>)>,
}

impl Cff {
    pub(crate) fn parse(file: Bytes<'_>, (table, length): (usize, usize)) -> Result<Cff, Error> {
        let damaged = || Error::Damaged("CFF");
        let b = file.slice(table, length).ok_or_else(damaged)?;
        if b.u8(0) != Some(1) {
            return Err(Error::Unsupported("CFF2 outlines"));
        }
        let header = b.u8(2).ok_or_else(damaged)? as usize;
        let (_names, at) = Index::parse(b, header).ok_or_else(damaged)?;
        let (top, at) = Index::parse(b, at).ok_or_else(damaged)?;
        let (_strings, at) = Index::parse(b, at).ok_or_else(damaged)?;
        let (global_subrs, _) = Index::parse(b, at).ok_or_else(damaged)?;
        let top = dict(top.get(b, 0).ok_or_else(damaged)?).ok_or_else(damaged)?;
        if lookup(&top, 1206).is_some_and(|t| t != [2.0]) {
            return Err(Error::Unsupported("CFF charstrings other than Type 2"));
        }
        let char_strings = lookup(&top, 17).and_then(|v| offset(*v.first()?)).ok_or_else(damaged)?;
        let (char_strings, _) = Index::parse(b, char_strings).ok_or_else(damaged)?;
        let (mut local_subrs, mut cid) = (None, None);
        if lookup(&top, 1230).is_some() {
            // CID-keyed: an array of font DICTs, each with a private DICT.
            let array = lookup(&top, 1236).and_then(|v| offset(*v.first()?)).ok_or_else(damaged)?;
            let select = lookup(&top, 1237).and_then(|v| offset(*v.first()?)).ok_or_else(damaged)?;
            let (array, _) = Index::parse(b, array).ok_or_else(damaged)?;
            let subrs = (0..array.count)
                .map(|i| {
                    let fd = dict(array.get(b, i).ok_or_else(damaged)?).ok_or_else(damaged)?;
                    Ok(lookup(&fd, 18).and_then(|private| private_subrs(b, private)))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            cid = Some((select, subrs));
        } else if let Some(private) = lookup(&top, 18) {
            local_subrs = private_subrs(b, private);
        }
        Ok(Cff { table, length, char_strings, global_subrs, local_subrs, cid })
    }

    pub(crate) fn glyph_count(&self) -> usize {
        self.char_strings.count
    }

    /// Which font DICT `glyph` uses (FDSelect formats 0 and 3).
    fn font_dict(&self, b: Bytes<'_>, select: usize, glyph: u16) -> Option<usize> {
        match b.u8(select)? {
            0 => b.u8(select + 1 + glyph as usize).map(usize::from),
            3 => {
                let ranges = b.u16(select + 1)? as usize;
                (0..ranges).find_map(|i| {
                    let range = select + 3 + 3 * i;
                    let (first, next) = (b.u16(range)?, b.u16(range + 3)?);
                    (first..next).contains(&glyph).then(|| b.u8(range + 2).map(usize::from))?
                })
            }
            _ => None,
        }
    }

    pub(crate) fn outline(&self, file: Bytes<'_>, glyph: u16, out: &mut Vec<Segment>) -> Result<(), Error> {
        let damaged = || Error::Damaged("CFF charstring");
        let b = file.slice(self.table, self.length).ok_or_else(damaged)?;
        let program = self.char_strings.get(b, glyph as usize).ok_or(Error::Damaged("glyph index past the end"))?;
        let local = match &self.cid {
            Some((select, subrs)) => {
                let fd = self.font_dict(b, *select, glyph).ok_or(Error::Damaged("CFF FDSelect"))?;
                *subrs.get(fd).ok_or(Error::Damaged("CFF FDSelect"))?
            }
            None => self.local_subrs,
        };
        let mut run = Run {
            b,
            global: self.global_subrs,
            local: local.unwrap_or_default(),
            out,
            stack: Vec::new(),
            pen: Point { x: 0.0, y: 0.0 },
            stems: 0,
            width_done: false,
            open: false,
            operators: 0,
            ended: false,
        };
        run.run(program, 0).ok_or_else(damaged)?;
        run.close();
        Ok(())
    }
}

/// One glyph's program running.
struct Run<'a, 'o> {
    b: Bytes<'a>,
    global: Index,
    local: Index,
    out: &'o mut Vec<Segment>,
    stack: Vec<f32>,
    pen: Point,
    stems: usize,
    /// The advance width that may lead the first stack-clearing operator
    /// has been dealt with (it's ignored: hmtx has the same number).
    width_done: bool,
    open: bool,
    operators: u32,
    ended: bool,
}

impl<'a> Run<'a, '_> {
    /// Drops the advance width: present when the operator has one more
    /// argument than it takes (`even`: it takes an even number).
    fn width(&mut self, even: bool) {
        if !self.width_done {
            self.width_done = true;
            if (self.stack.len() % 2 == 1) == even && !self.stack.is_empty() {
                self.stack.remove(0);
            }
        }
    }

    fn close(&mut self) {
        if self.open {
            self.out.push(Segment::Close);
            self.open = false;
        }
    }

    fn move_to(&mut self, dx: f32, dy: f32) {
        self.close();
        self.pen = Point { x: self.pen.x + dx, y: self.pen.y + dy };
        self.out.push(Segment::MoveTo(self.pen));
        self.open = true;
    }

    fn line_to(&mut self, dx: f32, dy: f32) {
        self.pen = Point { x: self.pen.x + dx, y: self.pen.y + dy };
        self.out.push(Segment::LineTo(self.pen));
    }

    fn curve_to(&mut self, d: [f32; 6]) {
        let c1 = Point { x: self.pen.x + d[0], y: self.pen.y + d[1] };
        let c2 = Point { x: c1.x + d[2], y: c1.y + d[3] };
        self.pen = Point { x: c2.x + d[4], y: c2.y + d[5] };
        self.out.push(Segment::CubicTo(c1, c2, self.pen));
    }

    fn run(&mut self, program: Bytes<'a>, depth: usize) -> Option<()> {
        if depth > MAX_CALL_DEPTH {
            return None;
        }
        let mut at = 0;
        while at < program.0.len() && !self.ended {
            let b0 = program.u8(at)?;
            at += 1;
            match b0 {
                28 => {
                    self.push(program.i16(at)? as f32)?;
                    at += 2;
                    continue;
                }
                32..=246 => {
                    self.push(b0 as f32 - 139.0)?;
                    continue;
                }
                247..=250 => {
                    self.push((b0 as f32 - 247.0) * 256.0 + program.u8(at)? as f32 + 108.0)?;
                    at += 1;
                    continue;
                }
                251..=254 => {
                    self.push(-(b0 as f32 - 251.0) * 256.0 - program.u8(at)? as f32 - 108.0)?;
                    at += 1;
                    continue;
                }
                255 => {
                    self.push(program.u32(at)? as i32 as f32 / 65536.0)?;
                    at += 4;
                    continue;
                }
                _ => {}
            }
            self.operators += 1;
            if self.operators > MAX_OPERATORS || self.out.len() > crate::face::MAX_SEGMENTS {
                return None;
            }
            let s = std::mem::take(&mut self.stack);
            let n = s.len();
            match b0 {
                // Stem hints: only their count matters, to size hint masks.
                1 | 3 | 18 | 23 => {
                    self.stack = s;
                    self.width(true);
                    self.stems += self.stack.len() / 2;
                    self.stack.clear();
                }
                19 | 20 => {
                    // Arguments left on the stack are vertical stems.
                    self.stack = s;
                    self.width(true);
                    self.stems += self.stack.len() / 2;
                    self.stack.clear();
                    at += self.stems.div_ceil(8);
                }
                21 => {
                    self.stack = s;
                    self.width(true);
                    let &[dx, dy] = &self.stack[..] else { return None };
                    self.stack.clear();
                    self.move_to(dx, dy);
                }
                22 | 4 => {
                    self.stack = s;
                    self.width(false);
                    let &[d] = &self.stack[..] else { return None };
                    self.stack.clear();
                    if b0 == 22 { self.move_to(d, 0.0) } else { self.move_to(0.0, d) }
                }
                5 => {
                    self.need_open()?;
                    if !n.is_multiple_of(2) {
                        return None;
                    }
                    for p in s.chunks_exact(2) {
                        self.line_to(p[0], p[1]);
                    }
                }
                // Alternating horizontal and vertical lines.
                6 | 7 => {
                    self.need_open()?;
                    let mut horizontal = b0 == 6;
                    for &d in &s {
                        if horizontal { self.line_to(d, 0.0) } else { self.line_to(0.0, d) }
                        horizontal = !horizontal;
                    }
                }
                8 => {
                    self.need_open()?;
                    if !n.is_multiple_of(6) {
                        return None;
                    }
                    for c in s.chunks_exact(6) {
                        self.curve_to([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                }
                // Curves then a line.
                24 => {
                    self.need_open()?;
                    if n < 8 || !(n - 2).is_multiple_of(6) {
                        return None;
                    }
                    for c in s[..n - 2].chunks_exact(6) {
                        self.curve_to([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                    self.line_to(s[n - 2], s[n - 1]);
                }
                // Lines then a curve.
                25 => {
                    self.need_open()?;
                    if n < 8 || !(n - 6).is_multiple_of(2) {
                        return None;
                    }
                    for p in s[..n - 6].chunks_exact(2) {
                        self.line_to(p[0], p[1]);
                    }
                    let c = &s[n - 6..];
                    self.curve_to([c[0], c[1], c[2], c[3], c[4], c[5]]);
                }
                // Curves starting and ending vertical (26) or horizontal (27),
                // the first perhaps leaning by a leading argument.
                26 | 27 => {
                    self.need_open()?;
                    let (mut lean, rest) = if n % 4 == 1 { (s[0], &s[1..]) } else { (0.0, &s[..]) };
                    if rest.is_empty() || !rest.len().is_multiple_of(4) {
                        return None;
                    }
                    for c in rest.chunks_exact(4) {
                        if b0 == 26 {
                            self.curve_to([lean, c[0], c[1], c[2], 0.0, c[3]]);
                        } else {
                            self.curve_to([c[0], lean, c[1], c[2], c[3], 0.0]);
                        }
                        lean = 0.0;
                    }
                }
                // Curves alternating between starting vertical and horizontal,
                // the last perhaps ending off the axis by a fifth argument.
                30 | 31 => {
                    self.need_open()?;
                    if n < 4 || n % 4 > 1 {
                        return None;
                    }
                    let mut horizontal = b0 == 31;
                    let mut i = 0;
                    while i + 4 <= n {
                        let extra = if n - i == 5 { s[i + 4] } else { 0.0 };
                        let c = &s[i..i + 4];
                        if horizontal {
                            self.curve_to([c[0], 0.0, c[1], c[2], extra, c[3]]);
                        } else {
                            self.curve_to([0.0, c[0], c[1], c[2], c[3], extra]);
                        }
                        horizontal = !horizontal;
                        i += 4;
                    }
                }
                10 | 29 => {
                    let mut s = s;
                    let index = if b0 == 10 { self.local } else { self.global };
                    let number = s.pop()? as i32 + index.bias();
                    let subr = index.get(self.b, usize::try_from(number).ok()?)?;
                    self.stack = s;
                    self.run(subr, depth + 1)?;
                }
                11 => {
                    self.stack = s;
                    return Some(());
                }
                14 => {
                    self.stack = s;
                    self.width(true);
                    // Four arguments would compose an accented letter (the
                    // retired 'seac'), which OpenType fonts don't use.
                    if !self.stack.is_empty() {
                        return None;
                    }
                    self.ended = true;
                }
                12 => {
                    let op = program.u8(at)?;
                    at += 1;
                    self.flex(op, &s)?;
                }
                _ => return None,
            }
        }
        Some(())
    }

    fn push(&mut self, v: f32) -> Option<()> {
        (self.stack.len() < MAX_STACK).then(|| self.stack.push(v))
    }

    /// Drawing before any moveto is an error.
    fn need_open(&self) -> Option<()> {
        self.open.then_some(())
    }

    /// The flex operators: two curves meeting near a line, drawn as curves.
    fn flex(&mut self, op: u8, s: &[f32]) -> Option<()> {
        self.need_open()?;
        let start = self.pen;
        match (op, s.len()) {
            // flex: both curves in full, then a depth ignored here.
            (35, 13) => {
                self.curve_to([s[0], s[1], s[2], s[3], s[4], s[5]]);
                self.curve_to([s[6], s[7], s[8], s[9], s[10], s[11]]);
            }
            // hflex: horizontal ends, the middle raised by dy2.
            (34, 7) => {
                self.curve_to([s[0], 0.0, s[1], s[2], s[3], 0.0]);
                self.curve_to([s[4], 0.0, s[5], start.y - self.pen.y, s[6], 0.0]);
            }
            // hflex1: horizontal ends, back to the starting height.
            (36, 9) => {
                self.curve_to([s[0], s[1], s[2], s[3], s[4], 0.0]);
                self.curve_to([s[5], 0.0, s[6], s[7], s[8], 0.0]);
                self.pen.y = start.y;
                if let Some(Segment::CubicTo(_, _, end)) = self.out.last_mut() {
                    end.y = start.y;
                }
            }
            // flex1: the last coordinate along whichever axis moved more;
            // the other returns to the start.
            (37, 11) => {
                self.curve_to([s[0], s[1], s[2], s[3], s[4], s[5]]);
                let (c1, c2) = (Point { x: self.pen.x + s[6], y: self.pen.y + s[7] }, Point { x: self.pen.x + s[6] + s[8], y: self.pen.y + s[7] + s[9] });
                let (dx, dy) = (c2.x - start.x, c2.y - start.y);
                let end = if dx.abs() > dy.abs() { Point { x: c2.x + s[10], y: start.y } } else { Point { x: start.x, y: c2.y + s[10] } };
                self.pen = end;
                self.out.push(Segment::CubicTo(c1, c2, end));
            }
            // Arithmetic and storage operators: unused by real fonts.
            _ => return None,
        }
        Some(())
    }
}
