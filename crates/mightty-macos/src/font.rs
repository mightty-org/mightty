//! Font loading and glyph rasterisation: through CoreText, or with
//! `font-backend = mightty` through our own `mightty-font`, CoreText then
//! only finding font files and drawing what ours can't yet.
//!
//! Each glyph is drawn into a cell-sized (or two-cell) BGRA tile, so the
//! renderer can place tiles on the grid without per-glyph metrics.

use std::fs;
use std::ptr;
use std::sync::Arc;

use mightty_font::Face;

use crate::config::FontBackend;
use crate::ffi::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style(u8);

impl Style {
    pub const BOLD: u8 = 1;
    pub const ITALIC: u8 = 2;

    pub fn new(bold: bool, italic: bool) -> Self {
        Self((bold as u8 * Self::BOLD) | (italic as u8 * Self::ITALIC))
    }
}

pub struct RasterGlyph {
    /// Premultiplied BGRA, top row first.
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Colour bitmap (emoji) rather than a coverage mask.
    pub color: bool,
}

pub struct Font {
    /// Indexed by `Style`: regular, bold, italic, bold italic.
    variants: [CTFontRef; 4],
    /// With `font-backend = mightty`, the same faces read by us, where we
    /// can read them; also indexed by `Style`.
    own: [Option<Face>; 4],
    /// In pixels.
    size: f64,
    colorspace: CGColorSpaceRef,
    pub cell_width: u32,
    pub cell_height: u32,
    /// Baseline distance from the top of the cell, in pixels.
    pub baseline: u32,
}

impl Font {
    /// `size` is in pixels (points × backing scale).
    pub fn new(name: &str, size: f64, backend: FontBackend) -> Self {
        // SAFETY: CoreText/CoreGraphics calls with valid arguments; every
        // created object is owned by `Font` and released in `Drop`.
        unsafe {
            let cf_name = cfstring(name);
            // Falls back to a default font for unknown names, never null.
            let regular = CTFontCreateWithName(cf_name, size, ptr::null());
            CFRelease(cf_name);

            let variant = |traits: u32| {
                let font = CTFontCreateCopyWithSymbolicTraits(regular, 0.0, ptr::null(), traits, traits);
                if font.is_null() { CFRetain(regular) } else { font }
            };
            let bold = variant(kCTFontTraitBold);
            let italic = variant(kCTFontTraitItalic);
            let bold_italic = variant(kCTFontTraitBold | kCTFontTraitItalic);

            let variants = [regular, bold, italic, bold_italic];
            let own = match backend {
                FontBackend::CoreText => [None, None, None, None],
                FontBackend::Mightty => own_faces(&variants),
            };
            let (ascent, descent, leading, m_advance) = match &own[0] {
                // Our reading of the same numbers.
                Some(face) => {
                    let m = face.metrics();
                    let scale = size / m.units_per_em as f64;
                    let m_advance = face.glyph('M').map_or(size / 2.0, |g| face.advance(g) as f64 * scale);
                    (m.ascender as f64 * scale, -m.descender as f64 * scale, m.line_gap as f64 * scale, m_advance)
                }
                None => {
                    let mut glyph = 0u16;
                    let mut advance = CGSize::default();
                    CTFontGetGlyphsForCharacters(regular, &(b'M' as u16), &mut glyph, 1);
                    CTFontGetAdvancesForGlyphs(regular, 0, &glyph, &mut advance, 1);
                    (CTFontGetAscent(regular), CTFontGetDescent(regular), CTFontGetLeading(regular), advance.width)
                }
            };

            let cell_width = m_advance.round().max(1.0) as u32;
            let cell_height = (ascent + descent + leading).ceil().max(1.0) as u32;
            let baseline = ((cell_height as f64 - ascent - descent) / 2.0 + ascent).round() as u32;

            Self {
                variants,
                own,
                size,
                colorspace: CGColorSpaceCreateDeviceRGB(),
                cell_width,
                cell_height,
                baseline,
            }
        }
    }

    /// Renders `c` into a tile `cells` wide. Returns `None` when no font has the glyph.
    pub fn rasterize(&self, c: char, style: Style, cells: u32) -> Option<RasterGlyph> {
        if let Some(face) = &self.own[style.0 as usize]
            && let Some(glyph) = face.glyph(c)
            && let Some(raster) = self.draw_own(face, glyph, cells)
        {
            return Some(raster);
        }
        let mut units = [0u16; 2];
        let units = c.encode_utf16(&mut units);
        let n = units.len() as isize;
        let mut glyphs = [0u16; 2];
        let base = self.variants[style.0 as usize];

        // SAFETY: buffers are sized for `n` code units; fonts and contexts
        // created here are released before returning.
        unsafe {
            let mut fallback: CTFontRef = ptr::null();
            if !CTFontGetGlyphsForCharacters(base, units.as_ptr(), glyphs.as_mut_ptr(), n) || glyphs[0] == 0 {
                // Let CoreText pick a font that covers the character (CJK, emoji, symbols).
                let mut buf = [0u8; 4];
                let s = cfstring(c.encode_utf8(&mut buf));
                fallback = CTFontCreateForString(base, s, CFRange { location: 0, length: n });
                CFRelease(s);
                if fallback.is_null()
                    || !CTFontGetGlyphsForCharacters(fallback, units.as_ptr(), glyphs.as_mut_ptr(), n)
                    || glyphs[0] == 0
                {
                    if !fallback.is_null() {
                        CFRelease(fallback);
                    }
                    return None;
                }
            }
            let font = if fallback.is_null() { base } else { fallback };
            let color = CTFontGetSymbolicTraits(font) & kCTFontTraitColorGlyphs != 0;
            let mut advance = CGSize::default();
            CTFontGetAdvancesForGlyphs(font, 0, glyphs.as_ptr(), &mut advance, 1);
            let raster = self.draw(cells, advance.width, |ctx, origin| {
                CTFontDrawGlyphs(font, glyphs.as_ptr(), &origin, 1, ctx);
            });
            if !fallback.is_null() {
                CFRelease(fallback);
            }
            raster.map(|r| RasterGlyph { color, ..r })
        }
    }

    /// Renders a grapheme cluster (several characters in one cell, such as
    /// joined emoji, a flag or a letter with combining marks) through
    /// CoreText's line layout, which shapes it and picks fallback fonts.
    pub fn rasterize_cluster(&self, text: &str, style: Style, cells: u32) -> Option<RasterGlyph> {
        // SAFETY: CoreFoundation/CoreText calls with valid arguments; every
        // object created here is released before returning.
        unsafe {
            let keys = [kCTFontAttributeName, kCTForegroundColorFromContextAttributeName];
            let values = [self.variants[style.0 as usize], kCFBooleanTrue];
            let attributes = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                2,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            );
            let string = cfstring(text);
            let attributed = CFAttributedStringCreate(ptr::null(), string, attributes);
            CFRelease(string);
            CFRelease(attributes);
            if attributed.is_null() {
                return None;
            }
            let line = CTLineCreateWithAttributedString(attributed);
            CFRelease(attributed);
            if line.is_null() {
                return None;
            }
            let width = CTLineGetTypographicBounds(line, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            let raster = self.draw(cells, width, |ctx, origin| {
                CGContextSetTextPosition(ctx, origin.x, origin.y);
                CTLineDraw(line, ctx);
            });
            CFRelease(line);
            // Masks are drawn in white, so any tint means colour glyphs (emoji).
            raster.map(|r| RasterGlyph { color: r.pixels.chunks_exact(4).any(|p| p[0] != p[1] || p[1] != p[2]), ..r })
        }
    }

    /// `glyph` of `face` drawn by our rasterizer, placed as `draw` places
    /// CoreText's: on the baseline, centred when narrower than the tile.
    fn draw_own(&self, face: &Face, glyph: u16, cells: u32) -> Option<RasterGlyph> {
        let (width, height) = (self.cell_width * cells, self.cell_height);
        let scale = (self.size / face.metrics().units_per_em as f64) as f32;
        let advance = face.advance(glyph) as f32 * scale;
        let x = ((width as f32 - advance) / 2.0).max(0.0).round();
        let outline = face.outline(glyph).ok()?;
        let coverage = mightty_font::rasterize(&outline, scale, x, self.baseline as f32, width as usize, height as usize);
        // White, premultiplied: every channel is the coverage.
        let pixels = coverage.iter().flat_map(|&c| [c; 4]).collect();
        Some(RasterGlyph { pixels, width, height, color: false })
    }

    /// A tile `cells` wide with `draw` called on it at the baseline origin
    /// for content `width` pixels wide, centred when narrower than the tile
    /// (wide CJK, emoji). `color` is left false.
    unsafe fn draw(&self, cells: u32, width: f64, draw: impl FnOnce(CGContextRef, CGPoint)) -> Option<RasterGlyph> {
        let (tile_width, height) = (self.cell_width * cells, self.cell_height);
        let mut pixels = vec![0u8; (tile_width * height * 4) as usize];
        // SAFETY: the context draws into `pixels`, sized for it, and is released here.
        unsafe {
            let ctx = CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                tile_width as usize,
                height as usize,
                8,
                tile_width as usize * 4,
                self.colorspace,
                kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
            );
            if ctx.is_null() {
                return None;
            }
            CGContextSetAllowsFontSmoothing(ctx, false);
            CGContextSetShouldSmoothFonts(ctx, false);
            CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);
            let x = ((tile_width as f64 - width) / 2.0).max(0.0).round();
            // CoreGraphics' origin is bottom-left.
            let y = (height - self.baseline) as f64;
            draw(ctx, CGPoint { x, y });
            CGContextRelease(ctx);
        }
        Some(RasterGlyph { pixels, width: tile_width, height, color: false })
    }
}

/// Our faces for CoreText's `fonts`: the face each one was loaded from,
/// found by its file and PostScript name. `None` where we can't read it
/// (CFF outlines, for now), which leaves that style to CoreText.
fn own_faces(fonts: &[CTFontRef; 4]) -> [Option<Face>; 4] {
    // Styles often share a file (a .ttc collection).
    let mut files: Vec<(String, Arc<[u8]>)> = Vec::new();
    fonts.map(|font| {
        // SAFETY: a live CTFont; copied objects are released here.
        let (path, name) = unsafe {
            let url = CTFontCopyAttribute(font, kCTFontURLAttribute);
            if url.is_null() {
                return None;
            }
            let path = string_from_ns(msg!(url as Id, c"path"));
            CFRelease(url);
            let name = CTFontCopyPostScriptName(font);
            let ps = string_from_ns(name as Id);
            CFRelease(name);
            (path, ps)
        };
        let data = match files.iter().find(|f| f.0 == path) {
            Some((_, data)) => data.clone(),
            None => {
                let data: Arc<[u8]> = fs::read(&path).ok()?.into();
                files.push((path.clone(), data.clone()));
                data
            }
        };
        let faces = Face::count(&data);
        let face = (0..faces).find_map(|i| Face::parse(data.clone(), i).ok().filter(|f| f.postscript_name().as_deref() == Some(name.as_str())));
        let face = face.or_else(|| if faces == 1 { Face::parse(data.clone(), 0).ok() } else { None });
        if face.is_none() {
            eprintln!("mightty: font-backend=mightty can't read {name} ({path}); CoreText draws it");
        }
        face
    })
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: we own one reference to each.
        unsafe {
            for font in self.variants {
                CFRelease(font);
            }
            CGColorSpaceRelease(self.colorspace);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Our rasterizer against CoreText on the same font: same cell, and
    /// glyphs within a little anti-aliasing of each other.
    #[test]
    fn matches_coretext() {
        for size in [24.0, 28.0, 33.0] {
            let theirs = Font::new("Menlo", size, FontBackend::CoreText);
            let ours = Font::new("Menlo", size, FontBackend::Mightty);
            assert!(ours.own.iter().all(Option::is_some), "Menlo's four styles are TrueType");
            assert_eq!((ours.cell_width, ours.cell_height, ours.baseline), (theirs.cell_width, theirs.cell_height, theirs.baseline), "{size}");
            let mut worst = (0.0f64, ' ', Style::default());
            let (mut sum, mut count) = (0.0, 0);
            for style in [Style::new(false, false), Style::new(true, false), Style::new(false, true), Style::new(true, true)] {
                for c in (0x21u8..0x7f).map(char::from).chain(['é', 'ß', '─', '█', '→']) {
                    let a = theirs.rasterize(c, style, 1).unwrap();
                    let b = ours.rasterize(c, style, 1).unwrap();
                    let total: u64 = a.pixels.iter().step_by(4).map(|&p| p as u64).sum();
                    let diff: u64 = a.pixels.iter().zip(&b.pixels).step_by(4).map(|(&p, &q)| (p as i32 - q as i32).unsigned_abs() as u64).sum();
                    // Differences relative to the glyph's ink.
                    let relative = diff as f64 / total.max(1) as f64;
                    (sum, count) = (sum + relative, count + 1);
                    if relative > worst.0 {
                        worst = (relative, c, style);
                    }
                }
            }
            // Edges anti-aliased a little differently, no more: a misplaced
            // or misread glyph differs by far more of its ink.
            let mean = sum / count as f64;
            assert!(worst.0 < 0.08, "size {size}: {:?} {:?} differs by {:.1}% of its ink", worst.1, worst.2, worst.0 * 100.0);
            assert!(mean < 0.02, "size {size}: glyphs differ by {:.1}% of their ink on average", mean * 100.0);
        }
    }
}
