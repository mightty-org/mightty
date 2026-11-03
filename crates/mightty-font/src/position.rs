//! Placing a cluster's glyphs: each advances the pen, except marks
//! (accents and other combining characters), which sit on the glyph before
//! them. Where the font says how (`GPOS` mark-to-base and mark-to-mark
//! anchors), as it says; otherwise centred over it.

use crate::face::{Bytes, Face, Point, Segment};
use crate::shape::{Shaped, class_of, coverage};

/// A glyph and where its origin goes, in font units from the cluster's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub glyph: u16,
    pub x: f32,
    pub y: f32,
}

impl Face {
    /// `glyphs` placed: the glyphs and the width they take together.
    pub fn position(&self, glyphs: &[Shaped]) -> (Vec<Placed>, f32) {
        let b = self.bytes();
        let mark_lookups: Vec<u16> = self.gpos_table().map(|gpos| crate::shape::lookups_of(b, gpos, &[*b"mark", *b"mkmk"])).unwrap_or_default();
        let mut placed: Vec<Placed> = Vec::with_capacity(glyphs.len());
        let mut pen = 0.0;
        // The last glyph that wasn't a mark, by index into `placed`.
        let mut base: Option<usize> = None;
        for s in glyphs {
            let g = s.glyph;
            if self.is_mark(g)
                && let Some(bi) = base
            {
                let previous = placed.len() - 1;
                // On the mark before it if the font says how, else on the base.
                let anchored = (previous != bi)
                    .then(|| self.attach(&mark_lookups, 6, g, placed[previous].glyph).map(|(dx, dy)| (placed[previous].x + dx, placed[previous].y + dy)))
                    .flatten()
                    .or_else(|| self.attach(&mark_lookups, 4, g, placed[bi].glyph).map(|(dx, dy)| (placed[bi].x + dx, placed[bi].y + dy)));
                let (x, y) = anchored.unwrap_or_else(|| self.centred(g, placed[bi]));
                placed.push(Placed { glyph: g, x, y });
                continue;
            }
            placed.push(Placed { glyph: g, x: pen, y: 0.0 });
            pen += self.advance(g);
            base = Some(placed.len() - 1);
        }
        (placed, pen)
    }

    /// A mark by its GDEF class, or, without one, one that doesn't advance.
    fn is_mark(&self, glyph: u16) -> bool {
        let b = self.bytes();
        match self.gdef_table().and_then(|gdef| b.u16(gdef + 4).filter(|&o| o != 0).map(|o| gdef + o as usize)) {
            Some(classes) => class_of(b, classes, glyph) == 3,
            None => self.advance(glyph) == 0.0,
        }
    }

    /// Where `mark` goes relative to `on` by the first mark-to-base (`kind`
    /// 4) or mark-to-mark (6) lookup that has both: the anchors' difference.
    fn attach(&self, lookups: &[u16], kind: u16, mark: u16, on: u16) -> Option<(f32, f32)> {
        let b = self.bytes();
        let gpos = self.gpos_table()?;
        let list = gpos + b.u16(gpos + 8)? as usize;
        lookups.iter().find_map(|&index| {
            let at = list + b.u16(list + 2 + 2 * index as usize)? as usize;
            let (lookup_kind, count) = (b.u16(at)?, b.u16(at + 4)? as usize);
            (0..count.min(256)).find_map(|i| {
                let mut s = at + b.u16(at + 6 + 2 * i)? as usize;
                let mut lookup_kind = lookup_kind;
                if lookup_kind == 9 {
                    // Extension: the real type and a 32-bit offset.
                    lookup_kind = b.u16(s + 2)?;
                    s += b.u32(s + 4)? as usize;
                }
                (lookup_kind == kind && b.u16(s)? == 1).then_some(())?;
                anchors(b, s, mark, on)
            })
        })
    }

    /// Without anchors: `mark` centred over `base`'s outline, at the height
    /// it was drawn for.
    fn centred(&self, mark: u16, base: Placed) -> (f32, f32) {
        let (Some(m), Some(bb)) = (self.bounds(mark), self.bounds(base.glyph)) else { return (base.x, base.y) };
        let centre = |(lo, hi): (Point, Point)| (lo.x + hi.x) / 2.0;
        (base.x + centre(bb) - centre(m), base.y)
    }

    /// The corners of `glyph`'s outline, from its points.
    pub fn bounds(&self, glyph: u16) -> Option<(Point, Point)> {
        let outline = self.outline(glyph).ok()?;
        let mut points = outline.iter().flat_map(|s| match *s {
            Segment::MoveTo(p) | Segment::LineTo(p) => vec![p],
            Segment::QuadTo(c, p) => vec![c, p],
            Segment::CubicTo(c1, c2, p) => vec![c1, c2, p],
            Segment::Close => vec![],
        });
        let first = points.next()?;
        Some(points.fold((first, first), |(lo, hi), p| (Point { x: lo.x.min(p.x), y: lo.y.min(p.y) }, Point { x: hi.x.max(p.x), y: hi.y.max(p.y) })))
    }
}

/// A MarkBasePos or MarkMarkPos (format 1) subtable's offset for `mark` on
/// `on`: the base's anchor for the mark's class, less the mark's anchor.
fn anchors(b: Bytes<'_>, s: usize, mark: u16, on: u16) -> Option<(f32, f32)> {
    let mark_index = coverage(b, s + b.u16(s + 2)? as usize, mark)?;
    let base_index = coverage(b, s + b.u16(s + 4)? as usize, on)?;
    let classes = b.u16(s + 6)? as usize;
    let (marks, bases) = (s + b.u16(s + 8)? as usize, s + b.u16(s + 10)? as usize);
    (mark_index < b.u16(marks)? as usize && base_index < b.u16(bases)? as usize).then_some(())?;
    let record = marks + 2 + 4 * mark_index;
    let class = b.u16(record)? as usize;
    (class < classes).then_some(())?;
    let mark_anchor = anchor(b, marks + b.u16(record + 2)? as usize)?;
    let offset = b.u16(bases + 2 + 2 * (base_index * classes + class))?;
    (offset != 0).then_some(())?;
    let base_anchor = anchor(b, bases + offset as usize)?;
    Some((base_anchor.0 - mark_anchor.0, base_anchor.1 - mark_anchor.1))
}

/// An Anchor table's point (formats 1 to 3 start alike).
fn anchor(b: Bytes<'_>, at: usize) -> Option<(f32, f32)> {
    (1..=3).contains(&b.u16(at)?).then_some(())?;
    Some((b.i16(at + 2)? as f32, b.i16(at + 4)? as f32))
}
