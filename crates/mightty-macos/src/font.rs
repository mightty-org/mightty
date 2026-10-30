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

            let (width, height) = (self.cell_width * cells, self.cell_height);
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            let ctx = CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width as usize,
                height as usize,
                8,
                width as usize * 4,
                self.colorspace,
                kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
            );
            if ctx.is_null() {
                if !fallback.is_null() {
                    CFRelease(fallback);
                }
                return None;
            }
            CGContextSetAllowsFontSmoothing(ctx, false);
            CGContextSetShouldSmoothFonts(ctx, false);
            CGContextSetRGBFillColor(ctx, 1.0, 1.0, 1.0, 1.0);

            // Centre glyphs narrower than their tile (wide CJK, emoji).
            let mut advance = CGSize::default();
            CTFontGetAdvancesForGlyphs(font, 0, glyphs.as_ptr(), &mut advance, 1);
            let x = ((width as f64 - advance.width) / 2.0).max(0.0).round();
            // CoreGraphics' origin is bottom-left.
            let y = (height - self.baseline) as f64;
            CTFontDrawGlyphs(font, glyphs.as_ptr(), &CGPoint { x, y }, 1, ctx);

            CGContextRelease(ctx);
            if !fallback.is_null() {
                CFRelease(fallback);
            }
            Some(RasterGlyph { pixels, width, height, color })
        }
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
