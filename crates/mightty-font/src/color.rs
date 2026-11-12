//! Colour glyphs (emoji): PNG images at a few sizes (`sbix`, Apple's;
//! `CBDT`/`CBLC`, Google's), or layers of outlines each in one colour
//! (`COLR` version 0, colours from `CPAL`). Drawn into premultiplied RGBA
//! tiles: images scaled to the size wanted, layers rasterized and laid one
//! over another.

use crate::face::{Bytes, Face};
use crate::raster::rasterize;
use mightty_image::png::{self, Image};

/// Where a face's colour tables are, if it has them.
#[derive(Clone, Copy, Default)]
pub(crate) struct Tables {
    pub(crate) sbix: Option<(usize, usize)>,
    pub(crate) cblc: Option<(usize, usize)>,
    pub(crate) cbdt: Option<(usize, usize)>,
    pub(crate) colr: Option<(usize, usize)>,
    pub(crate) cpal: Option<(usize, usize)>,
}

/// A colour glyph as stored.
#[derive(Clone, Debug, PartialEq)]
pub enum ColorGlyph {
    /// An image drawn for `ppem` pixels per em, its top left corner
    /// `left` right of the origin and `top` above the baseline, in its pixels.
    Bitmap {
        image: Image,
        ppem: f32,
        left: f32,
        top: f32,
    },
    /// Outline glyphs drawn in order, each in a colour (straight RGBA), or
    /// the text's colour where `None`.
    Layers(Vec<(u16, Option<[u8; 4]>)>),
}

impl Face {
    /// `glyph` in colour, if the face has it so: the image nearest
    /// `ppem` pixels per em (the smallest at least as large, scaled down).
    pub fn color_glyph(&self, glyph: u16, ppem: f32) -> Option<ColorGlyph> {
        let b = self.bytes();
        let t = self.color_tables();
        if let Some(sbix) = t.sbix
            && let Some(g) = sbix_glyph(b, sbix, self.glyph_count(), glyph, ppem)
        {
            return Some(g);
        }
        if let (Some(cblc), Some(cbdt)) = (t.cblc, t.cbdt)
            && let Some(g) = cbdt_glyph(b, cblc, cbdt, glyph, ppem)
        {
            return Some(g);
        }
        if let (Some(colr), Some(cpal)) = (t.colr, t.cpal) {
            return colr_layers(b, colr, cpal, glyph).map(ColorGlyph::Layers);
        }
        None
    }
}

/// Image data larger than this is refused before decoding.
const MAX_IMAGE_BYTES: usize = 1 << 22;

/// Strikes are sorted by nothing in particular: the smallest at least
/// `ppem`, else the largest.
fn pick<T: Copy>(strikes: impl Iterator<Item = (f32, T)>, ppem: f32) -> Option<(f32, T)> {
    let mut best: Option<(f32, T)> = None;
    for (size, strike) in strikes {
        let better = match best {
            None => true,
            Some((b, _)) if b >= ppem => size >= ppem && size < b,
            Some((b, _)) => size > b,
        };
        if better {
            best = Some((size, strike));
        }
    }
    best
}

fn sbix_glyph(
    b: Bytes<'_>,
    (at, _): (usize, usize),
    glyphs: u16,
    glyph: u16,
    ppem: f32,
) -> Option<ColorGlyph> {
    let count = b.u32(at + 4)? as usize;
    let strikes = (0..count.min(256)).filter_map(|i| {
        let strike = at + b.u32(at + 8 + 4 * i)? as usize;
        Some((b.u16(strike)? as f32, strike))
    });
    // Strikes may lack a glyph; try the best first, then any other.
    let (size, strike) = pick(strikes.clone(), ppem)?;
    std::iter::once((size, strike))
        .chain(strikes)
        .find_map(|(size, strike)| {
            if size == 0.0 || glyph >= glyphs {
                return None;
            }
            // 'dupe' points at another glyph's image, once.
            let mut g = glyph;
            for _ in 0..2 {
                let (start, end) = (
                    b.u32(strike + 4 + 4 * g as usize)? as usize,
                    b.u32(strike + 8 + 4 * g as usize)? as usize,
                );
                if end <= start + 8 || end - start > MAX_IMAGE_BYTES {
                    return None;
                }
                let data = b.slice(strike + start, end - start)?;
                let (x, y) = (data.i16(0)? as f32, data.i16(2)? as f32);
                match data.slice(4, 4)?.0 {
                    b"png " => {
                        let image = png::decode(&data.0[8..])?;
                        // The image's bottom left sits at (x, y), and Apple's
                        // renderer draws it 1/8 em lower than that: CoreText
                        // puts Apple Color Emoji's images from -0.125 em at
                        // every size, where its offsets and glyf boxes say 0.
                        return Some(ColorGlyph::Bitmap {
                            top: y + image.height as f32 - size / 8.0,
                            image,
                            ppem: size,
                            left: x,
                        });
                    }
                    b"dupe" if g == glyph => g = data.u16(8)?.min(glyphs - 1),
                    _ => return None,
                }
            }
            None
        })
}

fn cbdt_glyph(
    b: Bytes<'_>,
    (cblc, _): (usize, usize),
    (cbdt, _): (usize, usize),
    glyph: u16,
    ppem: f32,
) -> Option<ColorGlyph> {
    let sizes = b.u32(cblc + 4)? as usize;
    let strikes = (0..sizes.min(256)).filter_map(|i| {
        let size = cblc + 8 + 48 * i;
        let (first, last) = (b.u16(size + 40)?, b.u16(size + 42)?);
        (first..=last).contains(&glyph).then_some(())?;
        Some((b.u8(size + 45)? as f32, size))
    });
    let (strike_ppem, size) = pick(strikes, ppem)?;
    let array = cblc + b.u32(size)? as usize;
    let subtables = b.u32(size + 8)? as usize;
    let entry = (0..subtables.min(65536))
        .map(|i| array + 8 * i)
        .find(|&e| {
            let (first, last) = (b.u16(e).unwrap_or(1), b.u16(e + 2).unwrap_or(0));
            (first..=last).contains(&glyph)
        })?;
    let first = b.u16(entry)?;
    let sub = array + b.u32(entry + 4)? as usize;
    let (index_format, image_format, image_data) = (
        b.u16(sub)?,
        b.u16(sub + 2)?,
        cbdt + b.u32(sub + 4)? as usize,
    );
    let i = (glyph - first) as usize;
    // Where the glyph's data is, and big metrics from the index if it has them.
    let (offset, index_metrics) = match index_format {
        1 => (b.u32(sub + 8 + 4 * i)? as usize, None),
        2 => (b.u32(sub + 8)? as usize * i, Some(sub + 12)),
        3 => (b.u16(sub + 8 + 2 * i)? as usize, None),
        4 | 5 => {
            let (pairs, start) = if index_format == 4 {
                (b.u32(sub + 8)? as usize, sub + 12)
            } else {
                (b.u32(sub + 20)? as usize, sub + 24)
            };
            let k = (0..pairs.min(65536)).find(|&k| {
                b.u16(start + if index_format == 4 { 4 * k } else { 2 * k }) == Some(glyph)
            })?;
            if index_format == 4 {
                (b.u16(start + 4 * k + 2)? as usize, None)
            } else {
                (b.u32(sub + 8)? as usize * k, Some(sub + 12))
            }
        }
        _ => return None,
    };
    let at = image_data.checked_add(offset)?;
    // Metrics: (width, height, left bearing, top bearing).
    let big = |m: usize| Some((b.u8(m + 2)? as i8 as f32, b.u8(m + 3)? as i8 as f32));
    let (bearings, data) = match image_format {
        17 => (
            (b.u8(at + 2)? as i8 as f32, b.u8(at + 3)? as i8 as f32),
            at + 5,
        ),
        18 => (big(at)?, at + 8),
        19 => (big(index_metrics?)?, at),
        _ => return None,
    };
    let len = b.u32(data)? as usize;
    if len > MAX_IMAGE_BYTES {
        return None;
    }
    let image = png::decode(b.slice(data + 4, len)?.0)?;
    Some(ColorGlyph::Bitmap {
        image,
        ppem: strike_ppem,
        left: bearings.0,
        top: bearings.1,
    })
}

/// Layers a colour glyph may have; emoji fonts use a few dozen at most.
const MAX_LAYERS: usize = 512;

fn colr_layers(
    b: Bytes<'_>,
    (colr, _): (usize, usize),
    (cpal, _): (usize, usize),
    glyph: u16,
) -> Option<Vec<(u16, Option<[u8; 4]>)>> {
    let (bases, base_at, layer_at, layer_count) = (
        b.u16(colr + 2)? as usize,
        colr + b.u32(colr + 4)? as usize,
        colr + b.u32(colr + 8)? as usize,
        b.u16(colr + 12)? as usize,
    );
    // Base glyph records are sorted by glyph.
    let (mut lo, mut hi) = (0, bases);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if b.u16(base_at + 6 * mid)? < glyph {
            lo = mid + 1
        } else {
            hi = mid
        }
    }
    let record = base_at + 6 * lo;
    if lo == bases || b.u16(record)? != glyph {
        return None;
    }
    let (first, count) = (b.u16(record + 2)? as usize, b.u16(record + 4)? as usize);
    // Each layer is rasterized over the whole tile.
    if first + count > layer_count || count > MAX_LAYERS {
        return None;
    }
    // Palette 0.
    let (entries, records, first_record) = (
        b.u16(cpal + 2)?,
        cpal + b.u32(cpal + 8)? as usize,
        b.u16(cpal + 12)? as usize,
    );
    (first..first + count)
        .map(|i| {
            let layer = layer_at + 4 * i;
            let (g, palette) = (b.u16(layer)?, b.u16(layer + 2)?);
            let color = match palette {
                0xFFFF => None,
                p if p < entries => {
                    let c = records + 4 * (first_record + p as usize);
                    // Stored blue, green, red, alpha.
                    Some([b.u8(c + 2)?, b.u8(c + 1)?, b.u8(c)?, b.u8(c + 3)?])
                }
                _ => return None,
            };
            Some((g, color))
        })
        .collect()
}

/// `glyph` in colour, `size` pixels per em, its origin at pixel (`x`, `y`)
/// of a `width`×`height` premultiplied RGBA tile; `None` if the face has
/// no colour for it. Layers in the text's colour are drawn white.
pub fn draw_color(
    face: &Face,
    glyph: u16,
    size: f32,
    x: f32,
    y: f32,
    width: usize,
    height: usize,
) -> Option<Vec<u8>> {
    let mut tile = vec![0u8; width * height * 4];
    match face.color_glyph(glyph, size)? {
        ColorGlyph::Bitmap {
            image,
            ppem,
            left,
            top,
        } => {
            let scale = size / ppem;
            draw_image(
                &mut tile,
                width,
                height,
                &image,
                x + left * scale,
                y - top * scale,
                scale,
            );
        }
        ColorGlyph::Layers(layers) => {
            let scale = size / face.metrics().units_per_em as f32;
            for (g, color) in layers {
                let outline = face.outline(g).ok()?;
                let coverage = rasterize(&outline, scale, x, y, width, height);
                let [r, gr, bl, a] = color.unwrap_or([255; 4]);
                for (px, &c) in tile.chunks_exact_mut(4).zip(&coverage) {
                    // Source over, premultiplied: the layer's alpha times coverage.
                    let alpha = a as u32 * c as u32 / 255;
                    let keep = 255 - alpha;
                    for (d, s) in px.iter_mut().zip([r, gr, bl, 255]) {
                        *d = ((s as u32 * alpha + *d as u32 * keep + 127) / 255) as u8;
                    }
                }
            }
        }
    }
    Some(tile)
}

/// `image` scaled by `scale` with its top left at (`left`, `top`), laid
/// over `tile`: each pixel the average of the image's area it covers.
fn draw_image(
    tile: &mut [u8],
    width: usize,
    height: usize,
    image: &Image,
    left: f32,
    top: f32,
    scale: f32,
) {
    if !(scale.is_finite() && scale > 0.0 && left.is_finite() && top.is_finite()) {
        return;
    }
    let (iw, ih) = (image.width as f32, image.height as f32);
    let span = |start: f32, len: f32, limit: usize| {
        (
            start.floor().max(0.0) as usize,
            ((start + len * scale).ceil().max(0.0) as usize).min(limit),
        )
    };
    let (x0, x1) = span(left, iw, width);
    let (y0, y1) = span(top, ih, height);
    // How much of source pixels `i`… a destination pixel covers, along one axis.
    let weights = |d: usize, origin: f32, limit: f32| {
        let (a, b) = (
            ((d as f32 - origin) / scale).max(0.0),
            ((d as f32 + 1.0 - origin) / scale).min(limit),
        );
        (a.floor() as usize..b.ceil() as usize)
            .map(move |i| (i, (b.min(i as f32 + 1.0) - a.max(i as f32)).max(0.0)))
    };
    let area = scale * scale;
    for dy in y0..y1 {
        for dx in x0..x1 {
            let mut sum = [0.0f32; 4];
            for (sy, wy) in weights(dy, top, ih) {
                for (sx, wx) in weights(dx, left, iw) {
                    let p = &image.pixels[(sy * image.width as usize + sx) * 4..][..4];
                    for c in 0..4 {
                        sum[c] += p[c] as f32 * wx * wy;
                    }
                }
            }
            let d = &mut tile[(dy * width + dx) * 4..][..4];
            let alpha = (sum[3] * area).min(255.0);
            for c in 0..4 {
                let s = (sum[c] * area).min(255.0);
                d[c] = (s + d[c] as f32 * (1.0 - alpha / 255.0)).round().min(255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Image {
        Image {
            width: w,
            height: h,
            pixels: rgba.repeat((w * h) as usize),
        }
    }

    #[test]
    fn images_scale_by_area() {
        // A 4×4 red image halved into a 2×2 tile: full red.
        let mut tile = vec![0; 2 * 2 * 4];
        draw_image(
            &mut tile,
            2,
            2,
            &solid(4, 4, [255, 0, 0, 255]),
            0.0,
            0.0,
            0.5,
        );
        assert_eq!(tile, [255, 0, 0, 255].repeat(4));
        // Half a pixel in: the edge pixels half covered.
        let mut tile = vec![0; 3 * 4];
        draw_image(
            &mut tile,
            3,
            1,
            &solid(2, 1, [0, 0, 200, 200]),
            0.5,
            0.0,
            1.0,
        );
        assert_eq!(tile, [0, 0, 100, 100, 0, 0, 200, 200, 0, 0, 100, 100]);
        // Off the tile: nothing, no panic.
        let mut tile = vec![0; 4];
        draw_image(&mut tile, 1, 1, &solid(2, 2, [9; 4]), -10.0, 40.0, 3.0);
        assert_eq!(tile, [0; 4]);
    }

    #[test]
    fn strikes_pick_the_next_size_up() {
        let sizes = [20.0, 64.0, 32.0, 160.0];
        let best = |ppem| pick(sizes.iter().map(|&s| (s, s)), ppem).unwrap().0;
        assert_eq!(
            (best(24.0), best(32.0), best(10.0), best(400.0)),
            (32.0, 32.0, 20.0, 160.0)
        );
    }
}
