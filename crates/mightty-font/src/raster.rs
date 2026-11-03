//! Anti-aliased coverage of an outline: how much of each pixel the shape
//! covers, exactly. Each edge adds the signed area it sweeps to the pixels
//! it crosses; a running sum along each row then gives the coverage (the
//! accumulation method of Raph Levien's font-rs). Overlapping contours add
//! up as the non-zero winding rule needs, clamped to full.

use crate::face::{Point, Segment};

/// Curves are flattened into lines no further than this from them, in
/// pixels: as coarse as CoreText's, whose glyphs ours should match.
const TOLERANCE: f32 = 0.2;

/// Coverage (0–255) of `outline`, in font units scaled by `scale` pixels per
/// unit, with its origin at pixel (`x`, `y`) of a `width`×`height` image whose
/// rows run top to bottom. Whatever falls outside the image is cut off.
pub fn rasterize(outline: &[Segment], scale: f32, x: f32, y: f32, width: usize, height: usize) -> Vec<u8> {
    let mut acc = Accumulator::new(width, height);
    let to_pixels = |p: Point| Point { x: x + p.x * scale, y: y - p.y * scale };
    let (mut start, mut pen) = (None, Point { x: 0.0, y: 0.0 });
    for &segment in outline {
        match segment {
            Segment::MoveTo(p) => {
                if let Some(s) = start {
                    acc.line(pen, s);
                }
                pen = to_pixels(p);
                start = Some(pen);
            }
            Segment::LineTo(p) => {
                let p = to_pixels(p);
                acc.line(pen, p);
                pen = p;
            }
            Segment::QuadTo(c, p) => {
                let (c, p) = (to_pixels(c), to_pixels(p));
                acc.quad(pen, c, p);
                pen = p;
            }
            Segment::CubicTo(c1, c2, p) => {
                let (c1, c2, p) = (to_pixels(c1), to_pixels(c2), to_pixels(p));
                acc.cubic(pen, c1, c2, p);
                pen = p;
            }
            Segment::Close => {
                if let Some(s) = start.take() {
                    acc.line(pen, s);
                    pen = s;
                }
            }
        }
    }
    if let Some(s) = start {
        acc.line(pen, s);
    }
    acc.coverage()
}

struct Accumulator {
    width: usize,
    height: usize,
    /// Rows `width + 2` wide: an edge's area can spill two pixels right of
    /// where it is, and stays in its own row.
    stride: usize,
    area: Vec<f32>,
}

impl Accumulator {
    fn new(width: usize, height: usize) -> Self {
        let stride = width + 2;
        Self { width, height, stride, area: vec![0.0; stride * height] }
    }

    fn quad(&mut self, p0: Point, p1: Point, p2: Point) {
        // How far the curve bends from the chord sets how many lines it needs.
        let (dx, dy) = (p0.x - 2.0 * p1.x + p2.x, p0.y - 2.0 * p1.y + p2.y);
        let deviation = (dx * dx + dy * dy).sqrt() / 4.0;
        let n = ((deviation / TOLERANCE).sqrt().ceil() as usize).clamp(1, 64);
        let mut from = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let to = Point { x: u * u * p0.x + 2.0 * u * t * p1.x + t * t * p2.x, y: u * u * p0.y + 2.0 * u * t * p1.y + t * t * p2.y };
            self.line(from, to);
            from = to;
        }
    }

    fn cubic(&mut self, p0: Point, p1: Point, p2: Point, p3: Point) {
        // Lines n to a curve stray at most ¾·d/n² from it, d the larger
        // second difference of its points.
        let second = |a: Point, b: Point, c: Point| {
            let (dx, dy) = (a.x - 2.0 * b.x + c.x, a.y - 2.0 * b.y + c.y);
            (dx * dx + dy * dy).sqrt()
        };
        let d = second(p0, p1, p2).max(second(p1, p2, p3));
        let n = ((0.75 * d / TOLERANCE).sqrt().ceil() as usize).clamp(1, 64);
        let mut from = p0;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, e) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            let to = Point { x: a * p0.x + b * p1.x + c * p2.x + e * p3.x, y: a * p0.y + b * p1.y + c * p2.y + e * p3.y };
            self.line(from, to);
            from = to;
        }
    }

    fn line(&mut self, p0: Point, p1: Point) {
        if p0.y == p1.y || !(p0.x.is_finite() && p0.y.is_finite() && p1.x.is_finite() && p1.y.is_finite()) {
            return;
        }
        // Clipped to the image's sides: split where it crosses one, and the
        // part outside laid flat along it. That keeps each row's coverage
        // balanced and what's inside exact.
        for side in [0.0, self.width as f32] {
            if (p0.x - side) * (p1.x - side) < 0.0 {
                let t = (side - p0.x) / (p1.x - p0.x);
                let crossing = Point { x: side, y: p0.y + t * (p1.y - p0.y) };
                self.line(p0, crossing);
                self.line(crossing, p1);
                return;
            }
        }
        let clamp = |p: Point| Point { x: p.x.clamp(0.0, self.width as f32), y: p.y };
        let (p0, p1) = (clamp(p0), clamp(p1));
        let (dir, top, bottom) = if p0.y < p1.y { (1.0, p0, p1) } else { (-1.0, p1, p0) };
        let dxdy = (bottom.x - top.x) / (bottom.y - top.y);
        let first = top.y.max(0.0);
        let mut x = top.x + (first - top.y) * dxdy;
        let rows = (first as usize)..(bottom.y.ceil().max(0.0) as usize).min(self.height);
        for row in rows {
            let row_top = (row as f32).max(top.y);
            let row_bottom = ((row + 1) as f32).min(bottom.y);
            let dy = row_bottom - row_top;
            let x_next = x + dxdy * dy;
            let d = dy * dir;
            let line = row * self.stride;
            let (x0, x1) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let x0_floor = x0.floor();
            let x0i = x0_floor as usize;
            let x1_ceil = x1.ceil();
            let x1i = x1_ceil as usize;
            if x1i <= x0i + 1 {
                // Within one pixel: split by where its middle is.
                let middle = 0.5 * (x + x_next) - x0_floor;
                self.area[line + x0i] += d - d * middle;
                self.area[line + x0i + 1] += d * middle;
            } else {
                // Across several: a triangle in the first, slices of the
                // ramp between, and a triangle in the last.
                let s = (x1 - x0).recip();
                let x0f = x0 - x0_floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1_ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.area[line + x0i] += d * a0;
                if x1i == x0i + 2 {
                    self.area[line + x0i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.area[line + x0i + 1] += d * (a1 - a0);
                    for xi in x0i + 2..x1i - 1 {
                        self.area[line + xi] += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.area[line + x1i - 1] += d * (1.0 - a2 - am);
                }
                self.area[line + x1i] += d * am;
            }
            x = x_next;
        }
    }

    fn coverage(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width * self.height);
        for row in self.area.chunks_exact(self.stride) {
            let mut sum = 0.0f32;
            for &a in &row[..self.width] {
                sum += a;
                out.push((sum.abs().min(1.0) * 255.0 + 0.5) as u8);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    fn square(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Segment> {
        vec![
            Segment::MoveTo(p(x0, y0)),
            Segment::LineTo(p(x1, y0)),
            Segment::LineTo(p(x1, y1)),
            Segment::LineTo(p(x0, y1)),
            Segment::Close,
        ]
    }

    #[test]
    fn pixel_aligned_square() {
        // Font units with y up; origin at the image's bottom left.
        let out = rasterize(&square(1.0, 1.0, 3.0, 3.0), 1.0, 0.0, 4.0, 4, 4);
        assert_eq!(out, [0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn partial_pixels_are_partly_covered() {
        // Half a pixel wide: half coverage, whichever way it's drawn.
        for outline in [square(1.0, 0.0, 1.5, 1.0), square(1.5, 1.0, 1.0, 0.0)] {
            assert_eq!(rasterize(&outline, 1.0, 0.0, 1.0, 3, 1), [0, 128, 0]);
        }
        // A quarter of a pixel.
        assert_eq!(rasterize(&square(0.5, 0.5, 1.0, 1.0), 1.0, 0.0, 1.0, 1, 1), [64]);
    }

    #[test]
    fn overlaps_and_holes() {
        // Two overlapping squares wound alike stay fully covered, not doubled.
        let mut both = square(0.0, 0.0, 2.0, 1.0);
        both.extend(square(1.0, 0.0, 3.0, 1.0));
        assert_eq!(rasterize(&both, 1.0, 0.0, 1.0, 3, 1), [255, 255, 255]);
        // A hole wound the other way.
        let mut hole = square(0.0, 0.0, 3.0, 1.0);
        hole.extend([Segment::MoveTo(p(1.0, 0.0)), Segment::LineTo(p(1.0, 1.0)), Segment::LineTo(p(2.0, 1.0)), Segment::LineTo(p(2.0, 0.0)), Segment::Close]);
        assert_eq!(rasterize(&hole, 1.0, 0.0, 1.0, 3, 1), [255, 0, 255]);
    }

    #[test]
    fn outside_the_image_is_cut_off() {
        // Sticks out on every side; whatever's inside is full.
        assert_eq!(rasterize(&square(-5.0, -5.0, 10.0, 10.0), 1.0, 0.0, 2.0, 2, 2), [255; 4]);
        // Entirely outside: nothing, and no panic.
        assert_eq!(rasterize(&square(20.0, 20.0, 30.0, 30.0), 1.0, 0.0, 2.0, 2, 2), [0; 4]);
        assert_eq!(rasterize(&square(-30.0, 0.0, -20.0, 1.0), 1.0, 0.0, 2.0, 2, 2), [0; 4]);
    }

    /// A slanted edge leaving the image keeps its slope inside it.
    #[test]
    fn slanted_edges_are_clipped_not_bent() {
        // A parallelogram leaning right, sticking out of a 4-pixel-wide image
        // on the left at the bottom and on the right at the top.
        let lean = vec![
            Segment::MoveTo(p(-2.0, 0.0)),
            Segment::LineTo(p(1.0, 0.0)),
            Segment::LineTo(p(5.0, 4.0)),
            Segment::LineTo(p(2.0, 4.0)),
            Segment::Close,
        ];
        let clipped = rasterize(&lean, 1.0, 0.0, 4.0, 4, 4);
        // The same shape in a wide image, its middle 4 columns cut out.
        let wide = rasterize(&lean, 1.0, 4.0, 4.0, 12, 4);
        let middle: Vec<u8> = wide.chunks(12).flat_map(|row| row[4..8].to_vec()).collect();
        assert_eq!(clipped, middle);
    }

    #[test]
    fn curves_are_smooth() {
        // A disc of radius 4 from four quadratic arcs: about π·16 ≈ 50 pixels of coverage.
        let disc = vec![
            Segment::MoveTo(p(8.0, 4.0)),
            Segment::QuadTo(p(8.0, 8.0), p(4.0, 8.0)),
            Segment::QuadTo(p(0.0, 8.0), p(0.0, 4.0)),
            Segment::QuadTo(p(0.0, 0.0), p(4.0, 0.0)),
            Segment::QuadTo(p(8.0, 0.0), p(8.0, 4.0)),
            Segment::Close,
        ];
        let total: f32 = rasterize(&disc, 1.0, 0.0, 8.0, 8, 8).iter().map(|&c| c as f32 / 255.0).sum();
        // Quadratic arcs bulge past a true circle: between the circle and the square.
        assert!((50.0..58.0).contains(&total), "{total}");

        // Cubic arcs with the usual 0.5523 handles are within 0.03% of a
        // circle; flattening loses up to 5% at this size.
        let k = 4.0 * 0.5523;
        let disc = vec![
            Segment::MoveTo(p(8.0, 4.0)),
            Segment::CubicTo(p(8.0, 4.0 + k), p(4.0 + k, 8.0), p(4.0, 8.0)),
            Segment::CubicTo(p(4.0 - k, 8.0), p(0.0, 4.0 + k), p(0.0, 4.0)),
            Segment::CubicTo(p(0.0, 4.0 - k), p(4.0 - k, 0.0), p(4.0, 0.0)),
            Segment::CubicTo(p(4.0 + k, 0.0), p(8.0, 4.0 - k), p(8.0, 4.0)),
            Segment::Close,
        ];
        let total: f32 = rasterize(&disc, 1.0, 0.0, 8.0, 8, 8).iter().map(|&c| c as f32 / 255.0).sum();
        assert!((47.7..50.4).contains(&total), "{total}");
    }
}
