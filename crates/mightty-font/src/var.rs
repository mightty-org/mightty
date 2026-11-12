//! Variable fonts: one file holding a range of designs along axes such as
//! weight. A position is set in each axis's own units (`wght` 700), turned
//! into a normalized coordinate from -1 to 1 (`fvar`, remapped by `avar`),
//! and every outline point, advance and metric moves by deltas weighted by
//! how close that position is to the regions where they apply (`gvar` for
//! outlines, `HVAR` for advances, `MVAR` for metrics).

use crate::face::{Bytes, Point};

/// One axis of variation, in its own units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    /// Such as `wght`, `wdth`, `slnt`, `ital`, `opsz`.
    pub tag: [u8; 4],
    pub min: f32,
    pub default: f32,
    pub max: f32,
}

/// Points × tuples one glyph's deltas may take: real glyphs need a few
/// thousand.
const MAX_DELTA_WORK: usize = 1 << 20;

fn fixed(b: Bytes<'_>, at: usize) -> Option<f32> {
    Some(b.u32(at)? as i32 as f32 / 65536.0)
}

fn f2dot14(b: Bytes<'_>, at: usize) -> Option<f32> {
    Some(b.i16(at)? as f32 / 16384.0)
}

/// Axes a variable font may have; the most real fonts have is about 15.
const MAX_AXES: usize = 64;

/// The axes `fvar` at `at` declares; `None` if any makes no sense.
pub(crate) fn axes(b: Bytes<'_>, at: usize) -> Option<Vec<Axis>> {
    let (first, count, size) = (
        b.u16(at + 4)? as usize,
        b.u16(at + 8)? as usize,
        b.u16(at + 10)? as usize,
    );
    // Every delta is weighed per axis: more than real fonts have makes the
    // font not variable.
    if size < 20 || count > MAX_AXES {
        return None;
    }
    (0..count)
        .map(|i| {
            let a = at + first + i * size;
            let tag = b.slice(a, 4)?.0.try_into().ok()?;
            let axis = Axis {
                tag,
                min: fixed(b, a + 4)?,
                default: fixed(b, a + 8)?,
                max: fixed(b, a + 12)?,
            };
            // A damaged axis makes the font not variable at all.
            (axis.min <= axis.default && axis.default <= axis.max).then_some(axis)
        })
        .collect()
}

/// `settings` (axis tag and value) as normalized coordinates, one per
/// axis; axes not named stay at their defaults (0).
pub(crate) fn normalize(
    b: Bytes<'_>,
    axes: &[Axis],
    avar: Option<usize>,
    settings: &[([u8; 4], f32)],
) -> Vec<f32> {
    let mut coords: Vec<f32> = axes
        .iter()
        .map(|axis| {
            let Some(&(_, value)) = settings
                .iter()
                .rev()
                .find(|s| s.0 == axis.tag && s.1.is_finite())
            else {
                return 0.0;
            };
            let v = value.clamp(axis.min, axis.max);
            if v < axis.default {
                (v - axis.default) / (axis.default - axis.min)
            } else if v > axis.default {
                (v - axis.default) / (axis.max - axis.default)
            } else {
                0.0
            }
        })
        .collect();
    if let Some(avar) = avar {
        remap(b, avar, &mut coords);
    }
    // Coordinates are 2.14 fixed point; rounding as fonts are designed for.
    for c in &mut coords {
        *c = (c.clamp(-1.0, 1.0) * 16384.0).round() / 16384.0;
    }
    coords
}

/// `avar`'s piecewise-linear maps, one per axis.
fn remap(b: Bytes<'_>, avar: usize, coords: &mut [f32]) -> Option<()> {
    if b.u16(avar + 6)? as usize != coords.len() {
        return None;
    }
    let mut at = avar + 8;
    for c in coords.iter_mut() {
        let pairs = b.u16(at)? as usize;
        let map: Vec<(f32, f32)> = (0..pairs)
            .map(|i| Some((f2dot14(b, at + 2 + 4 * i)?, f2dot14(b, at + 4 + 4 * i)?)))
            .collect::<Option<_>>()?;
        at += 2 + 4 * pairs;
        if let Some(i) = map.iter().position(|&(from, _)| from >= *c) {
            *c = match i {
                0 => map[0].1,
                _ => {
                    let ((x0, y0), (x1, y1)) = (map[i - 1], map[i]);
                    if x1 == x0 {
                        y1
                    } else {
                        y0 + (*c - x0) * (y1 - y0) / (x1 - x0)
                    }
                }
            };
        }
    }
    Some(())
}

/// How much a region (per axis: start, peak, end) applies at `coords`.
fn region_scalar(coords: &[f32], region: impl Iterator<Item = (f32, f32, f32)>) -> f32 {
    let mut scalar = 1.0;
    for (&c, (start, peak, end)) in coords.iter().zip(region) {
        if peak == 0.0 || start > peak || peak > end || (start < 0.0 && end > 0.0) {
            continue;
        }
        if c < start || c > end {
            return 0.0;
        }
        if c == peak {
            continue;
        }
        scalar *= if c < peak {
            (c - start) / (peak - start)
        } else {
            (end - c) / (end - peak)
        };
    }
    scalar
}

/// An ItemVariationStore (inside HVAR and MVAR): rows of deltas, each
/// weighted by how much its region applies.
pub(crate) fn item_delta(
    b: Bytes<'_>,
    store: usize,
    coords: &[f32],
    outer: u16,
    inner: u16,
) -> Option<f32> {
    let regions = store + b.u32(store + 2)? as usize;
    let (axis_count, region_count) = (b.u16(regions)? as usize, b.u16(regions + 2)?);
    if axis_count != coords.len() {
        return None;
    }
    if outer >= b.u16(store + 6)? {
        return None;
    }
    let data = store + b.u32(store + 8 + 4 * outer as usize)? as usize;
    let (items, word_count, region_index_count) =
        (b.u16(data)?, b.u16(data + 2)?, b.u16(data + 4)? as usize);
    if inner >= items {
        return None;
    }
    let long = word_count & 0x8000 != 0;
    let words = (word_count & 0x7FFF) as usize;
    if words > region_index_count {
        return None;
    }
    let (big, small) = if long { (4, 2) } else { (2, 1) };
    let row_size = words * big + (region_index_count - words) * small;
    let row = data + 6 + 2 * region_index_count + inner as usize * row_size;
    let mut total = 0.0;
    let mut at = row;
    for k in 0..region_index_count {
        let delta = match (k < words, long) {
            (true, true) => b.u32(at)? as i32 as f32,
            (true, false) | (false, true) => b.i16(at)? as f32,
            (false, false) => b.u8(at)? as i8 as f32,
        };
        at += if k < words { big } else { small };
        let region = b.u16(data + 6 + 2 * k)?;
        if region >= region_count {
            return None;
        }
        let r = regions + 4 + region as usize * axis_count * 6;
        let triples = (0..axis_count).map(|i| {
            let a = r + 6 * i;
            (
                f2dot14(b, a).unwrap_or(0.0),
                f2dot14(b, a + 2).unwrap_or(0.0),
                f2dot14(b, a + 4).unwrap_or(0.0),
            )
        });
        b.slice(r, axis_count * 6)?;
        total += delta * region_scalar(coords, triples);
    }
    Some(total)
}

/// A DeltaSetIndexMap entry: the (outer, inner) rows for `i`.
pub(crate) fn delta_set_index(b: Bytes<'_>, map: usize, i: u32) -> Option<(u16, u16)> {
    let (format, entry_format) = (b.u8(map)?, b.u8(map + 1)?);
    let (count, data) = match format {
        0 => (b.u16(map + 2)? as u32, map + 4),
        1 => (b.u32(map + 2)?, map + 6),
        _ => return None,
    };
    if count == 0 {
        return None;
    }
    let size = ((entry_format >> 4) & 3) as usize + 1;
    let inner_bits = (entry_format & 0x0F) as u32 + 1;
    let at = data + i.min(count - 1) as usize * size;
    let entry = b
        .slice(at, size)?
        .0
        .iter()
        .fold(0u32, |v, &byte| (v << 8) | byte as u32);
    Some((
        (entry >> inner_bits) as u16,
        (entry & ((1 << inner_bits) - 1)) as u16,
    ))
}

/// `gvar`'s deltas for `glyph`'s `points` points (four phantom points
/// included, last), at `coords`. `original` gives the outline's points and
/// contour ends, so points without deltas of their own can follow their
/// neighbours'; composite glyphs pass `None`.
pub(crate) fn glyph_deltas(
    b: Bytes<'_>,
    gvar: usize,
    coords: &[f32],
    glyph: u16,
    points: usize,
    original: Option<(&[Point], &[usize])>,
) -> Option<Vec<Point>> {
    let axis_count = b.u16(gvar + 4)? as usize;
    if axis_count != coords.len() {
        return None;
    }
    let shared_count = b.u16(gvar + 6)? as usize;
    let shared = gvar + b.u32(gvar + 8)? as usize;
    let glyphs = b.u16(gvar + 12)?;
    let long = b.u16(gvar + 14)? & 1 != 0;
    let array = gvar + b.u32(gvar + 16)? as usize;
    let mut deltas = vec![Point { x: 0.0, y: 0.0 }; points];
    if glyph >= glyphs {
        return Some(deltas);
    }
    let g = glyph as usize;
    let (start, end) = if long {
        (
            b.u32(gvar + 20 + 4 * g)? as usize,
            b.u32(gvar + 24 + 4 * g)? as usize,
        )
    } else {
        (
            2 * b.u16(gvar + 20 + 2 * g)? as usize,
            2 * b.u16(gvar + 22 + 2 * g)? as usize,
        )
    };
    if end <= start {
        return Some(deltas);
    }
    let data = b.slice(array + start, end - start)?;
    let count = data.u16(0)?;
    let tuples = (count & 0x0FFF) as usize;
    let mut serialized = data.u16(2)? as usize;
    let shared_points = if count & 0x8000 != 0 {
        let (p, next) = packed_points(data, serialized, points)?;
        serialized = next;
        Some(p)
    } else {
        None
    };
    // Each tuple that applies costs a pass over the points; a damaged font
    // could ask for thousands of tuples over thousands of points.
    let mut work = 0usize;
    let mut header = 4;
    for _ in 0..tuples {
        let size = data.u16(header)? as usize;
        let index = data.u16(header + 2)?;
        header += 4;
        let peak: Vec<f32> = if index & 0x8000 != 0 {
            header += 2 * axis_count;
            (0..axis_count)
                .map(|i| f2dot14(data, header - 2 * axis_count + 2 * i))
                .collect::<Option<_>>()?
        } else {
            let i = (index & 0x0FFF) as usize;
            if i >= shared_count {
                return None;
            }
            (0..axis_count)
                .map(|a| f2dot14(b, shared + 2 * (i * axis_count + a)))
                .collect::<Option<_>>()?
        };
        let intermediate: Option<(Vec<f32>, Vec<f32>)> = if index & 0x4000 != 0 {
            let at = header;
            header += 4 * axis_count;
            let read = |from: usize| {
                (0..axis_count)
                    .map(|i| f2dot14(data, from + 2 * i))
                    .collect::<Option<Vec<_>>>()
            };
            Some((read(at)?, read(at + 2 * axis_count)?))
        } else {
            None
        };
        let tuple = data.slice(serialized, size)?;
        serialized += size;
        let scalar = match &intermediate {
            Some((s, e)) => region_scalar(coords, (0..axis_count).map(|i| (s[i], peak[i], e[i]))),
            // Without an intermediate region, from 0 to the peak.
            None => region_scalar(coords, peak.iter().map(|&p| (p.min(0.0), p, p.max(0.0)))),
        };
        if scalar == 0.0 {
            continue;
        }
        work += points;
        if work > MAX_DELTA_WORK {
            return None;
        }
        let (private, at) = if index & 0x2000 != 0 {
            packed_points(tuple, 0, points).map(|(p, at)| (Some(p), at))?
        } else {
            (None, 0)
        };
        let which = private.as_ref().or(shared_points.as_ref())?;
        let n = which.as_ref().map_or(points, Vec::len);
        let (xs, at) = packed_deltas(tuple, at, n)?;
        let (ys, _) = packed_deltas(tuple, at, n)?;
        match which {
            None => {
                for (i, d) in deltas.iter_mut().enumerate() {
                    d.x += scalar * xs[i];
                    d.y += scalar * ys[i];
                }
            }
            Some(listed) => {
                let mut tuple_deltas = vec![None; points];
                for (k, &p) in listed.iter().enumerate() {
                    if let Some(slot) = tuple_deltas.get_mut(p as usize) {
                        *slot = Some(Point { x: xs[k], y: ys[k] });
                    }
                }
                if let Some((outline, ends)) = original {
                    infer(&mut tuple_deltas, outline, ends);
                }
                for (d, t) in deltas.iter_mut().zip(tuple_deltas) {
                    if let Some(t) = t {
                        d.x += scalar * t.x;
                        d.y += scalar * t.y;
                    }
                }
            }
        }
    }
    Some(deltas)
}

/// Packed point numbers at `at`: `None` for all points, else the list;
/// and where the data after them starts.
#[allow(clippy::type_complexity)]
fn packed_points(b: Bytes<'_>, mut at: usize, points: usize) -> Option<(Option<Vec<u16>>, usize)> {
    let first = b.u8(at)? as usize;
    at += 1;
    let count = if first & 0x80 != 0 {
        at += 1;
        ((first & 0x7F) << 8) | b.u8(at - 1)? as usize
    } else {
        first
    };
    if count == 0 {
        return Some((None, at));
    }
    if count > points {
        return None;
    }
    let mut list = Vec::with_capacity(count);
    let mut value = 0u16;
    while list.len() < count {
        let control = b.u8(at)?;
        at += 1;
        let run = (control & 0x7F) as usize + 1;
        for _ in 0..run.min(count - list.len()) {
            let delta = if control & 0x80 != 0 {
                at += 2;
                b.u16(at - 2)?
            } else {
                at += 1;
                b.u8(at - 1)? as u16
            };
            value = value.wrapping_add(delta);
            list.push(value);
        }
    }
    Some((Some(list), at))
}

/// `n` packed deltas at `at`, and where the data after them starts.
fn packed_deltas(b: Bytes<'_>, mut at: usize, n: usize) -> Option<(Vec<f32>, usize)> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let control = b.u8(at)?;
        at += 1;
        let run = ((control & 0x3F) as usize + 1).min(n - out.len());
        for _ in 0..run {
            let v = if control & 0x80 != 0 {
                0.0
            } else if control & 0x40 != 0 {
                at += 2;
                b.i16(at - 2)? as f32
            } else {
                at += 1;
                b.u8(at - 1)? as i8 as f32
            };
            out.push(v);
        }
    }
    Some((out, at))
}

/// Deltas for the points of each contour that have none, from the nearest
/// ones before and after that do: interpolated when the point lies between
/// them, else the nearer one's (an outline's untouched points follow).
fn infer(deltas: &mut [Option<Point>], outline: &[Point], ends: &[usize]) {
    let mut first = 0;
    for &last in ends {
        let range = first..=last;
        first = last + 1;
        if last >= outline.len() || last >= deltas.len() {
            return;
        }
        let touched: Vec<usize> = range.clone().filter(|&i| deltas[i].is_some()).collect();
        if touched.is_empty() || touched.len() == range.clone().count() {
            continue;
        }
        let mut inferred = Vec::new();
        // How many touched points come before `i`: one walk along both.
        let mut k = 0;
        for i in range.clone() {
            if deltas[i].is_some() {
                k += 1;
                continue;
            }
            // The touched points before and after `i`, around the contour.
            let before = if k > 0 {
                touched[k - 1]
            } else {
                touched[touched.len() - 1]
            };
            let after = touched.get(k).copied().unwrap_or(touched[0]);
            let (db, da) = (
                deltas[before].unwrap_or(Point { x: 0.0, y: 0.0 }),
                deltas[after].unwrap_or(Point { x: 0.0, y: 0.0 }),
            );
            let axis = |c: fn(Point) -> f32| {
                let (cb, ca, ci) = (c(outline[before]), c(outline[after]), c(outline[i]));
                let (dbv, dav) = (c(db), c(da));
                if cb == ca {
                    return if dbv == dav { dbv } else { 0.0 };
                }
                let ((lo, dlo), (hi, dhi)) = if cb < ca {
                    ((cb, dbv), (ca, dav))
                } else {
                    ((ca, dav), (cb, dbv))
                };
                if ci <= lo {
                    dlo
                } else if ci >= hi {
                    dhi
                } else {
                    dlo + (ci - lo) * (dhi - dlo) / (hi - lo)
                }
            };
            inferred.push((
                i,
                Point {
                    x: axis(|p| p.x),
                    y: axis(|p| p.y),
                },
            ));
        }
        for (i, d) in inferred {
            deltas[i] = Some(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    #[test]
    fn untouched_points_follow_their_neighbours() {
        // A square's corners 0 and 2 moved; 1 lies between them in both
        // axes and is interpolated, 3 is outside and takes the nearer's.
        let outline = [p(0.0, 0.0), p(50.0, 50.0), p(100.0, 100.0), p(200.0, -10.0)];
        let mut deltas = [Some(p(10.0, 0.0)), None, Some(p(20.0, 4.0)), None];
        infer(&mut deltas, &outline, &[3]);
        assert_eq!(deltas[1], Some(p(15.0, 2.0)));
        assert_eq!(deltas[3], Some(p(20.0, 0.0)));
    }

    /// Linear in the points: a contour of 65535 with every other one
    /// touched (once quadratic) takes no time.
    #[test]
    fn inferring_is_linear() {
        let n = 65_535;
        let outline: Vec<Point> = (0..n).map(|i| p(i as f32, 0.0)).collect();
        let mut deltas: Vec<Option<Point>> = (0..n)
            .map(|i| (i % 2 == 0).then_some(p(1.0, 0.0)))
            .collect();
        let started = std::time::Instant::now();
        infer(&mut deltas, &outline, &[n - 1]);
        assert!(started.elapsed().as_millis() < 500);
        assert!(deltas.iter().all(|d| *d == Some(p(1.0, 0.0))));
    }

    #[test]
    fn regions_weigh_by_distance_from_the_peak() {
        let at = |c: f32| region_scalar(&[c], [(0.0, 1.0, 1.0)].into_iter());
        assert_eq!((at(0.0), at(0.5), at(1.0), at(-0.5)), (0.0, 0.5, 1.0, 0.0));
        // Peaked in the middle of an intermediate region.
        let mid = |c: f32| region_scalar(&[c], [(0.25, 0.5, 1.0)].into_iter());
        assert_eq!(
            (mid(0.375), mid(0.5), mid(0.75), mid(0.125)),
            (0.5, 1.0, 0.5, 0.0)
        );
    }

    #[test]
    fn packed_runs() {
        // Points: a run of 3 bytes (1, +2, +3 → 1, 3, 6). Deltas: two zero,
        // then two words.
        let bytes = [3, 0x02, 1, 2, 3, 0x81, 0x41, 0x01, 0x00, 0xFF, 0xFF];
        let b = Bytes(&bytes);
        let (points, at) = packed_points(b, 0, 10).unwrap();
        assert_eq!(points, Some(vec![1, 3, 6]));
        assert_eq!(
            packed_deltas(b, at, 4).unwrap(),
            (vec![0.0, 0.0, 256.0, -1.0], bytes.len())
        );
    }
}
