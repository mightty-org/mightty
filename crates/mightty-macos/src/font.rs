//! Font loading and glyph rasterisation through CoreText.
//!
//! Each glyph is drawn into a cell-sized (or two-cell) BGRA tile, so the
//! renderer can place tiles on the grid without per-glyph metrics.
//! TODO: replace with our own rasteriser behind this same interface.

use std::ptr;

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
    colorspace: CGColorSpaceRef,
    pub cell_width: u32,
    pub cell_height: u32,
    /// Baseline distance from the top of the cell, in pixels.
    pub baseline: u32,
}

impl Font {
    /// `size` is in pixels (points × backing scale).
    pub fn new(name: &str, size: f64) -> Self {
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

            let ascent = CTFontGetAscent(regular);
            let descent = CTFontGetDescent(regular);
            let leading = CTFontGetLeading(regular);
            let mut glyph = 0u16;
            let mut advance = CGSize::default();
            CTFontGetGlyphsForCharacters(regular, &(b'M' as u16), &mut glyph, 1);
            CTFontGetAdvancesForGlyphs(regular, 0, &glyph, &mut advance, 1);

            let cell_width = advance.width.round().max(1.0) as u32;
            let cell_height = (ascent + descent + leading).ceil().max(1.0) as u32;
            let baseline = ((cell_height as f64 - ascent - descent) / 2.0 + ascent).round() as u32;

            Self {
                variants: [regular, bold, italic, bold_italic],
                colorspace: CGColorSpaceCreateDeviceRGB(),
                cell_width,
                cell_height,
                baseline,
            }
        }
    }

    /// Renders `c` into a tile `cells` wide. Returns `None` when no font has the glyph.
    pub fn rasterize(&self, c: char, style: Style, cells: u32) -> Option<RasterGlyph> {
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
