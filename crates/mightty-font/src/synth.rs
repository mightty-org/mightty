//! Styles a family lacks, made from the regular face: bold by growing each
//! outline outward, italic by slanting it.

use crate::face::{Point, Segment};

/// The slant of a synthetic italic: 12°, as FreeType and CoreText use.
pub const OBLIQUE: f32 = 0.2126;

/// How far synthetic bold grows outlines on each side, as a fraction of the
/// em: strokes thicken by twice this.
pub const EMBOLDEN: f32 = 1.0 / 48.0;

/// Moves `outline` by (`dx`, `dy`).
pub fn translate(outline: &mut [Segment], dx: f32, dy: f32) {
    for segment in outline {
        for p in points(segment) {
            p.x += dx;
            p.y += dy;
        }
    }
}

/// Slants `outline` right by `shear` (x moves by `shear` × y).
pub fn oblique(outline: &mut [Segment], shear: f32) {
    for segment in outline {
        for p in points(segment) {
            p.x += shear * p.y;
        }
    }
}

/// Grows `outline` by `amount` (font units) on every side: each point moves
/// outward along the bisector of its two edges, far enough that both edges
/// move by `amount`.
pub fn embolden(outline: &mut [Segment], amount: f32) {
    // Which way is outward depends on how the outer contours wind: the
    // whole outline's area is positive when they run anticlockwise.
    let mut area = 0.0;
    let mut pen = Point { x: 0.0, y: 0.0 };
    for segment in outline.iter_mut() {
        if let Segment::MoveTo(p) = segment {
            pen = *p;
            continue;
        }
        for p in points(segment) {
            area += pen.x * p.y - p.x * pen.y;
            pen = *p;
        }
    }
    let outward = if area >= 0.0 { 1.0 } else { -1.0 };

    let mut start = 0;
    while start < outline.len() {
        let end = outline[start + 1..].iter().position(|s| matches!(s, Segment::MoveTo(_))).map_or(outline.len(), |i| start + 1 + i);
        grow_contour(&mut outline[start..end], amount * outward);
        start = end;
    }
}

fn grow_contour(contour: &mut [Segment], amount: f32) {
    let original: Vec<Point> = contour.iter_mut().flat_map(points).map(|p| *p).collect();
    // Runs of the same point (a contour ends where it starts) move as one,
    // between the points before and after the run.
    let mut runs: Vec<Point> = Vec::new();
    let mut run_of = Vec::with_capacity(original.len());
    for (i, &p) in original.iter().enumerate() {
        if i == 0 || p != original[i - 1] {
            runs.push(p);
        }
        run_of.push(runs.len() - 1);
    }
    if runs.len() > 1 && runs[0] == runs[runs.len() - 1] {
        let last = runs.len() - 1;
        runs.pop();
        for r in run_of.iter_mut().filter(|r| **r == last) {
            *r = 0;
        }
    }
    let m = runs.len();
    if m < 3 {
        return;
    }
    let unit = |from: Point, to: Point| {
        let (dx, dy) = (to.x - from.x, to.y - from.y);
        let len = (dx * dx + dy * dy).sqrt();
        // The normal on the outward side of an edge running anticlockwise.
        (dy / len, -dx / len)
    };
    let moved: Vec<Point> = (0..m)
        .map(|r| {
            let (prev, p, next) = (runs[(r + m - 1) % m], runs[r], runs[(r + 1) % m]);
            let (a, b) = (unit(prev, p), unit(p, next));
            let (sx, sy) = (a.0 + b.0, a.1 + b.1);
            // 1 + cos of the turn: small at a hairpin, where the bisector
            // would shoot far out; limited there.
            let d = (1.0 + a.0 * b.0 + a.1 * b.1).max(0.25);
            Point { x: p.x + sx * amount / d, y: p.y + sy * amount / d }
        })
        .collect();
    for (p, r) in contour.iter_mut().flat_map(points).zip(run_of) {
        *p = moved[r];
    }
}

/// A segment's points: controls, then the end.
fn points(segment: &mut Segment) -> impl Iterator<Item = &mut Point> {
    let list: Vec<&mut Point> = match segment {
        Segment::MoveTo(p) | Segment::LineTo(p) => vec![p],
        Segment::QuadTo(c, p) => vec![c, p],
        Segment::CubicTo(c1, c2, p) => vec![c1, c2, p],
        Segment::Close => vec![],
    };
    list.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rasterize;

    fn p(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    fn square(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Segment> {
        vec![Segment::MoveTo(p(x0, y0)), Segment::LineTo(p(x1, y0)), Segment::LineTo(p(x1, y1)), Segment::LineTo(p(x0, y1)), Segment::LineTo(p(x0, y0)), Segment::Close]
    }

    fn ink(outline: &[Segment]) -> f32 {
        rasterize(outline, 1.0, 0.0, 10.0, 10, 10).iter().map(|&c| c as f32 / 255.0).sum()
    }

    #[test]
    fn bold_grows_either_winding_outward() {
        // A 4×4 square grows to 5×5 whichever way it runs (edge pixels
        // half covered round up a little).
        for mut outline in [square(3.0, 3.0, 7.0, 7.0), square(7.0, 3.0, 3.0, 7.0)] {
            embolden(&mut outline, 0.5);
            assert!((ink(&outline) - 25.0).abs() < 0.1, "{}", ink(&outline));
        }
        // A ring (a hole wound the other way): the stroke thickens, the hole shrinks.
        let mut ring = square(1.0, 1.0, 9.0, 9.0);
        ring.extend(square(7.0, 3.0, 3.0, 7.0));
        embolden(&mut ring, 0.5);
        assert!((ink(&ring) - (81.0 - 9.0)).abs() < 0.1, "{}", ink(&ring));
    }

    /// Linear in the points: a contour of 65535 points, nearly all the
    /// same (once quadratic), takes no time.
    #[test]
    fn many_repeated_points() {
        let mut outline = vec![Segment::MoveTo(p(0.0, 0.0)), Segment::LineTo(p(4.0, 0.0)), Segment::LineTo(p(4.0, 4.0))];
        outline.extend(std::iter::repeat_n(Segment::LineTo(p(0.0, 4.0)), 65_535));
        outline.push(Segment::Close);
        let started = std::time::Instant::now();
        embolden(&mut outline, 0.5);
        assert!(started.elapsed().as_millis() < 500);
        assert_eq!(outline[100], Segment::LineTo(p(-0.5, 4.5)));
    }

    #[test]
    fn oblique_slants_right() {
        let mut outline = square(0.0, 0.0, 2.0, 10.0);
        oblique(&mut outline, 0.2);
        assert_eq!(outline[2], Segment::LineTo(p(4.0, 10.0)));
        assert_eq!(outline[0], Segment::MoveTo(p(0.0, 0.0)));
    }
}
