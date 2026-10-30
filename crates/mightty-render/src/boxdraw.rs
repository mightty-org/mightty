//! Box drawing (U+2500–257F) and block elements (U+2580–259F), drawn from
//! geometry instead of the font so lines meet exactly at cell edges.

/// Coverage mask for `c` in a `width`×`height` cell: one byte per pixel,
/// top row first. `stroke` is the light line thickness in pixels.
/// Returns `None` for characters this module doesn't draw.
pub fn draw(c: char, width: u32, height: u32, stroke: u32) -> Option<Vec<u8>> {
    let cp = c as u32;
    if !(0x2500..=0x259F).contains(&cp) || width == 0 || height == 0 {
        return None;
    }
    let mut canvas = Canvas { w: width as i32, h: height as i32, data: vec![0; (width * height) as usize] };
    let light = stroke.max(1) as i32;
    match cp {
        0x2500..=0x257F => draw_box(&mut canvas, cp, light),
        _ => draw_block(&mut canvas, cp),
    }
    Some(canvas.data)
}

struct Canvas {
    w: i32,
    h: i32,
    data: Vec<u8>,
}

impl Canvas {
    /// Fills `[x0, x1) × [y0, y1)`, clipped to the cell.
    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: u8) {
        for y in y0.max(0)..y1.min(self.h) {
            for x in x0.max(0)..x1.min(self.w) {
                let p = &mut self.data[(y * self.w + x) as usize];
                *p = (*p).max(alpha);
            }
        }
    }

    /// `rect` with main/cross axes swapped when `vertical`.
    fn axis_rect(&mut self, vertical: bool, m0: i32, m1: i32, c0: i32, c1: i32) {
        if vertical { self.rect(c0, m0, c1, m1, 255) } else { self.rect(m0, c0, m1, c1, 255) }
    }

    /// Anti-aliased shape: `f` maps a pixel centre to coverage in 0..=1.
    fn shade(&mut self, f: impl Fn(f32, f32) -> f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let a = (f(x as f32 + 0.5, y as f32 + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8;
                let p = &mut self.data[(y * self.w + x) as usize];
                *p = (*p).max(a);
            }
        }
    }
}

/// Line weights per character as "up right down left": 0 none, 1 light,
/// 2 heavy, 3 double. Empty entries are dashes, arcs and diagonals.
#[rustfmt::skip]
const LINES: [&str; 128] = [
    "0101", "0202", "1010", "2020", "", "", "", "", "", "", "", "", "0110", "0210", "0120", "0220", // 2500
    "0011", "0012", "0021", "0022", "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002", "1110", "1210", "2110", "1120", // 2510
    "2120", "2210", "1220", "2220", "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", "0111", "0112", "0211", "0212", // 2520
    "0121", "0122", "0221", "0222", "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", "1111", "1112", "1211", "1212", // 2530
    "2111", "1121", "2121", "2112", "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", "", "", "", "", // 2540
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130", // 2550
    "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", "3101", "3303", "1313", "3131", "3333", "", "", "", // 2560
    "", "", "", "", "0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010", // 2570
];

fn draw_box(canvas: &mut Canvas, cp: u32, light: i32) {
    let spec = LINES[(cp - 0x2500) as usize];
    if let [up, right, down, left] = spec.as_bytes() {
        let [up, right, down, left] = [up, right, down, left].map(|b| b - b'0');
        segments(canvas, false, left, right, up, down, light);
        segments(canvas, true, up, down, left, right, light);
        return;
    }
    match cp {
        // (vertical, weight, dash count)
        0x2504..=0x250B => {
            let i = cp - 0x2504;
            dashes(canvas, i % 4 >= 2, if i.is_multiple_of(2) { 1 } else { 2 }, if i < 4 { 3 } else { 4 }, light);
        }
        0x254C..=0x254F => {
            let i = cp - 0x254C;
            dashes(canvas, i >= 2, if i.is_multiple_of(2) { 1 } else { 2 }, 2, light);
        }
        0x256D..=0x2570 => arc(canvas, cp, light),
        _ => diagonals(canvas, cp, light),
    }
}

/// Draws the two half-lines along one axis. `before`/`after` are the weights
/// toward the start/end of the axis (left/right, or up/down when
/// `vertical`); `neg`/`pos` are the perpendicular weights (up/down, or left/right).
fn segments(canvas: &mut Canvas, vertical: bool, before: u8, after: u8, neg: u8, pos: u8, light: i32) {
    let (len, cm, cc) = if vertical {
        (canvas.h, canvas.h / 2, canvas.w / 2)
    } else {
        (canvas.w, canvas.w / 2, canvas.h / 2)
    };
    let l = light;
    let thick = |w: u8| if w == 2 { 2 * l } else { l };
    let single = |w: u8| w == 1 || w == 2;
    // Where perpendicular strokes sit on this axis, so ours can meet them.
    let lo = [neg, pos].into_iter().filter(|&w| single(w)).map(|w| cm - thick(w) / 2).min();
    let hi = [neg, pos].into_iter().filter(|&w| single(w)).map(|w| cm - thick(w) / 2 + thick(w)).max();
    let perp_double = neg == 3 || pos == 3;
    // Main-axis start of the near and far strokes of a perpendicular double line.
    let near = cm - l - l / 2;
    let far = cm + l - l / 2;

    for (weight, toward_end) in [(before, false), (after, true)] {
        match weight {
            0 => {}
            1 | 2 => {
                let t = thick(weight);
                let c0 = cc - t / 2;
                if toward_end {
                    let start = if perp_double { far } else { lo.unwrap_or(cm - t / 2) };
                    canvas.axis_rect(vertical, start, len, c0, c0 + t);
                } else {
                    let end = if perp_double { near + l } else { hi.unwrap_or(cm - t / 2 + t) };
                    canvas.axis_rect(vertical, 0, end, c0, c0 + t);
                }
            }
            _ => {
                // Double: the stroke on each side turns the corner on the
                // outside or stops short on the inside of a perpendicular double.
                for (c0, own, other) in [(cc - l - l / 2, neg, pos), (cc + l - l / 2, pos, neg)] {
                    let (m0, m1) = if toward_end {
                        let start = if own == 3 {
                            far
                        } else if other == 3 {
                            near
                        } else {
                            lo.unwrap_or(near)
                        };
                        (start, len)
                    } else {
                        let end = if own == 3 {
                            near + l
                        } else if other == 3 {
                            far + l
                        } else {
                            hi.unwrap_or(far + l)
                        };
                        (0, end)
                    };
                    canvas.axis_rect(vertical, m0, m1, c0, c0 + l);
                }
            }
        }
    }
}

fn dashes(canvas: &mut Canvas, vertical: bool, weight: u8, count: i32, light: i32) {
    let (len, cc) = if vertical { (canvas.h, canvas.w / 2) } else { (canvas.w, canvas.h / 2) };
    let t = if weight == 2 { 2 * light } else { light };
    let segment = len as f32 / count as f32;
    let gap = (segment / 4.0).max(1.0);
    for i in 0..count {
        let m0 = (i as f32 * segment + gap / 2.0).round() as i32;
        let m1 = ((i + 1) as f32 * segment - gap / 2.0).round() as i32;
        canvas.axis_rect(vertical, m0, m1, cc - t / 2, cc - t / 2 + t);
    }
}

/// Rounded corners ╭ ╮ ╯ ╰: a quarter circle plus straight runs to the edges.
fn arc(canvas: &mut Canvas, cp: u32, light: i32) {
    let (sx, sy) = match cp {
        0x256D => (1.0, 1.0),
        0x256E => (-1.0, 1.0),
        0x256F => (-1.0, -1.0),
        _ => (1.0, -1.0),
    };
    let l = light as f32;
    // Centre lines of the strokes, matching `segments`.
    let mid_x = (canvas.w / 2 - light / 2) as f32 + l / 2.0;
    let mid_y = (canvas.h / 2 - light / 2) as f32 + l / 2.0;
    let r = mid_x.min(mid_y);
    let (ox, oy) = (mid_x + sx * r, mid_y + sy * r);
    canvas.shade(|x, y| {
        if (x - ox) * sx > 0.0 || (y - oy) * sy > 0.0 {
            return 0.0;
        }
        let d = ((x - ox).powi(2) + (y - oy).powi(2)).sqrt();
        l / 2.0 + 0.5 - (d - r).abs()
    });
    let (cx, cy) = (canvas.w / 2 - light / 2, canvas.h / 2 - light / 2);
    let (ox, oy) = (ox.round() as i32, oy.round() as i32);
    if sx > 0.0 {
        canvas.rect(ox, cy, canvas.w, cy + light, 255);
    } else {
        canvas.rect(0, cy, ox, cy + light, 255);
    }
    if sy > 0.0 {
        canvas.rect(cx, oy, cx + light, canvas.h, 255);
    } else {
        canvas.rect(cx, 0, cx + light, oy, 255);
    }
}

/// ╱ ╲ ╳, corner to corner.
fn diagonals(canvas: &mut Canvas, cp: u32, light: i32) {
    let (w, h) = (canvas.w as f32, canvas.h as f32);
    let half = light as f32 / 2.0 + 0.5;
    let line = |(x1, y1): (f32, f32), (x2, y2): (f32, f32)| {
        let len = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
        move |x: f32, y: f32| half - ((y2 - y1) * x - (x2 - x1) * y + x2 * y1 - y2 * x1).abs() / len
    };
    if cp == 0x2571 || cp == 0x2573 {
        canvas.shade(line((w, 0.0), (0.0, h)));
    }
    if cp == 0x2572 || cp == 0x2573 {
        canvas.shade(line((0.0, 0.0), (w, h)));
    }
}

fn draw_block(canvas: &mut Canvas, cp: u32) {
    let (w, h) = (canvas.w, canvas.h);
    let eighth_y = |n: u32| (h * n as i32 + 4) / 8;
    let eighth_x = |n: u32| (w * n as i32 + 4) / 8;
    match cp {
        0x2580 => canvas.rect(0, 0, w, eighth_y(4), 255),
        0x2581..=0x2588 => canvas.rect(0, h - eighth_y(cp - 0x2580), w, h, 255),
        0x2589..=0x258F => canvas.rect(0, 0, eighth_x(8 - (cp - 0x2588)), h, 255),
        0x2590 => canvas.rect(eighth_x(4), 0, w, h, 255),
        0x2591..=0x2593 => canvas.rect(0, 0, w, h, (64 * (cp - 0x2590)) as u8),
        0x2594 => canvas.rect(0, 0, w, eighth_y(1), 255),
        0x2595 => canvas.rect(w - eighth_x(1), 0, w, h, 255),
        _ => {
            // Quadrants; bits: 1 upper-left, 2 upper-right, 4 lower-left, 8 lower-right.
            const QUADRANTS: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];
            let bits = QUADRANTS[(cp - 0x2596) as usize];
            let (mx, my) = (w / 2, h / 2);
            for (bit, x0, y0, x1, y1) in [(1, 0, 0, mx, my), (2, mx, 0, w, my), (4, 0, my, mx, h), (8, mx, my, w, h)] {
                if bits & bit != 0 {
                    canvas.rect(x0, y0, x1, y1, 255);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 10;
    const H: u32 = 20;

    fn mask(c: char) -> Vec<u8> {
        draw(c, W, H, 2).unwrap()
    }

    fn at(m: &[u8], x: u32, y: u32) -> u8 {
        m[(y * W + x) as usize]
    }

    #[test]
    fn covers_the_whole_range() {
        for cp in 0x2500..=0x259F {
            let m = mask(char::from_u32(cp).unwrap());
            assert!(m.iter().any(|&a| a > 0), "U+{cp:04X} drew nothing");
        }
        assert!(draw('a', W, H, 2).is_none());
    }

    #[test]
    fn lines_reach_the_edges() {
        let h = mask('─');
        assert_eq!(at(&h, 0, H / 2), 255);
        assert_eq!(at(&h, W - 1, H / 2), 255);
        assert_eq!(at(&h, W / 2, 0), 0);
        let v = mask('│');
        assert_eq!(at(&v, W / 2, 0), 255);
        assert_eq!(at(&v, W / 2, H - 1), 255);
    }

    #[test]
    fn corners_join() {
        let m = mask('┌');
        assert_eq!(at(&m, W - 1, H / 2), 255); // right arm
        assert_eq!(at(&m, W / 2, H - 1), 255); // down arm
        assert_eq!(at(&m, 0, H / 2), 0); // no left arm
        assert_eq!(at(&m, W / 2, 0), 0); // no up arm
    }

    #[test]
    fn double_lines_have_a_gap() {
        let m = mask('═');
        let column: Vec<u8> = (0..H).map(|y| at(&m, 0, y)).collect();
        let runs = column.windows(2).filter(|w| w[0] == 0 && w[1] == 255).count();
        assert_eq!(runs, 2, "two separate strokes: {column:?}");
    }

    #[test]
    fn blocks() {
        let m = mask('▄');
        assert_eq!(at(&m, 0, 0), 0);
        assert_eq!(at(&m, 0, H - 1), 255);
        assert!(mask('█').iter().all(|&a| a == 255));
        assert!(mask('░').iter().all(|&a| a == 64));
    }
}
