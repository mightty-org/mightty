//! Font loading and glyph rasterisation: through CoreText, or with
//! `font-backend = mightty` through our own `mightty-font`, CoreText then
//! only finding font files and drawing what ours can't yet. Either way,
//! bold and italic a family lacks are made from its regular face.
//!
//! Each glyph is drawn into a cell-sized (or two-cell) BGRA tile, so the
//! renderer can place tiles on the grid without per-glyph metrics.

use std::cell::RefCell;
use std::ptr;
use std::sync::OnceLock;

use mightty_font::{EMBOLDEN, Face, FontData, OBLIQUE};

use crate::config::{FontBackend, Fonts};
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

/// The widest tile drawn: the glyph atlas's largest side (`ATLAS_MAX` in
/// the renderer).
pub const MAX_TILE_WIDTH: u32 = 2048;
/// Cells one ligature tile covers at most; real ligatures span a few, and
/// longer runs of changed glyphs are split.
const MAX_LIGATURE_CELLS: u32 = 16;

pub struct RasterGlyph {
    /// Premultiplied BGRA, top row first.
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Colour bitmap (emoji) rather than a coverage mask.
    pub color: bool,
}

/// Cells a font draws as one: a ligature, or glyphs it changed by
/// context (coding fonts draw `->` as a spacer and an arrow reaching back
/// over it), so they're drawn together in one tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ligature {
    /// The first cell, counted in the text shaped.
    pub start: usize,
    pub cells: usize,
    /// Glyphs and the cell, counted from `start`, each is drawn in.
    pub glyphs: Box<[(u16, u16)]>,
}

pub struct Font {
    /// Indexed by `Style`: regular, bold, italic, bold italic.
    variants: [CTFontRef; 4],
    /// With `font-backend = mightty`, the same faces read by us, where we
    /// can read them; also indexed by `Style`.
    own: [Option<Face>; 4],
    /// The `Style` bits each variant lacks and has made up.
    synthetic: [u8; 4],
    /// `font-family` after the first: tried in order before CoreText picks.
    chain: Vec<CTFontRef>,
    /// The built-in Symbols Nerd Font, for icons in the Private Use Area
    /// the fonts above lack (prompts, file listers, editor plugins); null
    /// if it couldn't be loaded.
    symbols: CTFontRef,
    /// Features our shaper applies (`font-feature`, `font-ligatures`).
    shaping: Vec<[u8; 4]>,
    /// Whether runs of text need shaping: ligatures or features are on.
    shapes: bool,
    /// With `font-backend = mightty`, the fallback fonts CoreText picked
    /// (emoji, CJK) read by us, by PostScript name; `None` where we can't.
    fallbacks: RefCell<Vec<(String, Option<Face>)>>,
    /// Font files open, shared between faces (styles often share a .ttc).
    files: RefCell<Vec<(String, FontData)>>,
    backend: FontBackend,
    /// In pixels.
    size: f64,
    colorspace: CGColorSpaceRef,
    pub cell_width: u32,
    pub cell_height: u32,
    /// Baseline distance from the top of the cell, in pixels.
    pub baseline: u32,
}

impl Font {
    /// Resizes cells (`adjust-cell-width`, `adjust-cell-height`), keeping
    /// text centred vertically in the new height.
    pub fn resize_cells(&mut self, width: u32, height: u32) {
        let shift = (height as i64 - self.cell_height as i64) / 2;
        self.baseline = (self.baseline as i64 + shift).clamp(1, height as i64) as u32;
        (self.cell_width, self.cell_height) = (width, height);
    }

    /// `name` alone, with ligatures. `size` is in pixels (points × backing scale).
    #[cfg(test)]
    pub fn new(name: &str, size: f64, backend: FontBackend) -> Self {
        Self::with(&Fonts { families: vec![name.into()], ..Fonts::default() }, size, backend, true)
    }

    /// The fonts `fonts` describes, at `size` pixels.
    pub fn with(fonts: &Fonts, size: f64, backend: FontBackend, ligatures: bool) -> Self {
        // `font-ligatures = false` turns the ligature features off, unless set.
        let mut features = fonts.features.clone();
        if !ligatures {
            for tag in [*b"liga", *b"clig", *b"calt", *b"dlig"] {
                if !features.iter().any(|f| f.0 == tag) {
                    features.push((tag, 0));
                }
            }
        }
        let mut shaping: Vec<[u8; 4]> = mightty_font::LIGATURES.to_vec();
        for &(tag, value) in &features {
            shaping.retain(|&t| t != tag);
            if value > 0 {
                shaping.push(tag);
            }
        }
        let shapes = ligatures || fonts.features.iter().any(|f| f.1 > 0);
        // SAFETY: CoreText/CoreGraphics calls with valid arguments; every
        // created object is owned by `Font` and released in `Drop`.
        unsafe {
            let first = fonts.families.first().map_or("Menlo", String::as_str);
            let base = open_font(first, size);
            // Bold and italic come from the family's unvaried font, so its
            // own weights stand unless `font-variation-bold` and so on say.
            let variant = |style: usize, traits: u32| {
                let font = match &fonts.styled[style - 1] {
                    Some(name) => open_font(name, size),
                    None => {
                        let font = CTFontCreateCopyWithSymbolicTraits(base, 0.0, ptr::null(), traits, traits);
                        if font.is_null() { CFRetain(base) } else { font }
                    }
                };
                with_settings(font, &fonts.variations[style], &features)
            };
            let regular = with_settings(CFRetain(base), &fonts.variations[0], &features);
            let bold = variant(1, kCTFontTraitBold);
            let italic = variant(2, kCTFontTraitItalic);
            let bold_italic = variant(3, kCTFontTraitBold | kCTFontTraitItalic);
            CFRelease(base);
            let chain = fonts.families.iter().skip(1).map(|name| with_settings(open_font(name, size), &[], &features)).collect();

            let mut variants = [regular, bold, italic, bold_italic];
            let mut synthetic = [0; 4];
            for (i, font) in variants.iter_mut().enumerate() {
                let traits = CTFontGetSymbolicTraits(*font);
                let has = Style::new(traits & kCTFontTraitBold != 0, traits & kCTFontTraitItalic != 0).0;
                synthetic[i] = i as u8 & !has;
                // Italic is the regular slanted; bold is drawn stroked.
                if synthetic[i] & Style::ITALIC != 0 {
                    let slant = CGAffineTransform { a: 1.0, b: 0.0, c: OBLIQUE as f64, d: 1.0, tx: 0.0, ty: 0.0 };
                    let slanted = CTFontCreateCopyWithAttributes(*font, 0.0, &slant, ptr::null());
                    if !slanted.is_null() {
                        CFRelease(*font);
                        *font = slanted;
                    }
                }
            }
            let regular = variants[0];
            let mut files = Vec::new();
            let own = match backend {
                FontBackend::CoreText => [None, None, None, None],
                FontBackend::Mightty => variants.map(|font| own_face(font, &mut files)),
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
            // CoreText's metrics carry float noise: a height of exactly 36
            // can come back a hair over, and mustn't round up to 37.
            let cell_height = (ascent + descent + leading - 0.001).ceil().max(1.0) as u32;
            let baseline = ((cell_height as f64 - ascent - descent) / 2.0 + ascent).round() as u32;

            Self {
                variants,
                own,
                synthetic,
                fallbacks: RefCell::new(Vec::new()),
                files: RefCell::new(files),
                chain,
                symbols: symbols_font(size),
                shaping,
                shapes,
                backend,
                size,
                colorspace: CGColorSpaceCreateDeviceRGB(),
                cell_width,
                cell_height,
                baseline,
            }
        }
    }

    /// Whether runs of text are shaped: ligatures, or features such as
    /// `ss01` or `zero` that change single glyphs, are on.
    pub fn shapes(&self) -> bool {
        self.shapes
    }

    /// Renders `c` into a tile `cells` wide. Returns `None` when no font has the glyph.
    pub fn rasterize(&self, c: char, style: Style, cells: u32) -> Option<RasterGlyph> {
        if let Some(face) = &self.own[style.0 as usize]
            && let Some(glyph) = face.glyph(c)
            && let Some(raster) = self.draw_own(face, glyph, style, cells)
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
            let has = |font: CTFontRef, glyphs: &mut [u16; 2]| CTFontGetGlyphsForCharacters(font, units.as_ptr(), glyphs.as_mut_ptr(), n) && glyphs[0] != 0;
            if !has(base, &mut glyphs) {
                if let Some(&font) = self.chain.iter().find(|&&font| has(font, &mut glyphs)) {
                    // The next `font-family` that has it.
                    fallback = CFRetain(font);
                } else if is_private_use(c) && !self.symbols.is_null() && has(self.symbols, &mut glyphs) {
                    fallback = CFRetain(self.symbols);
                } else {
                    // Let CoreText pick a font that covers the character (CJK, emoji, symbols).
                    let mut buf = [0u8; 4];
                    let s = cfstring(c.encode_utf8(&mut buf));
                    fallback = CTFontCreateForString(base, s, CFRange { location: 0, length: n });
                    CFRelease(s);
                    if fallback.is_null() || !has(fallback, &mut glyphs) {
                        if !fallback.is_null() {
                            CFRelease(fallback);
                        }
                        return None;
                    }
                }
            }
            if !fallback.is_null() && fallback == self.symbols {
                CFRelease(fallback);
                return self.draw_icon(glyphs[0], style, cells);
            }
            let font = if fallback.is_null() { base } else { fallback };
            // Ours draws what CoreText picked, if it can read it.
            if !fallback.is_null() && self.backend == FontBackend::Mightty {
                let raster = self.fallback_face(fallback, |face| self.draw_own(face, glyphs[0], style, cells));
                if let Some(raster) = raster {
                    CFRelease(fallback);
                    return Some(raster);
                }
            }
            let color = CTFontGetSymbolicTraits(font) & kCTFontTraitColorGlyphs != 0;
            let mut advance = CGSize::default();
            CTFontGetAdvancesForGlyphs(font, 0, glyphs.as_ptr(), &mut advance, 1);
            let raster = self.draw(cells, advance.width, style, |ctx, origin| {
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
        if self.backend == FontBackend::Mightty
            && let Some(raster) = self.draw_cluster_own(text, style, cells)
        {
            return Some(raster);
        }
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
            // Too wide for its cells (an emoji the shell counts as one cell
            // wide): shrunk about the cell's middle, rather than cut off.
            let room = (self.cell_width * cells) as f64;
            let scale = if width > room * 1.05 { room / width } else { 1.0 };
            let middle = self.cell_height as f64 / 2.0;
            let raster = self.draw(cells, width * scale, style, |ctx, origin| {
                if scale < 1.0 {
                    CGContextTranslateCTM(ctx, origin.x, middle);
                    CGContextScaleCTM(ctx, scale, scale);
                    CGContextSetTextPosition(ctx, 0.0, origin.y - middle);
                } else {
                    CGContextSetTextPosition(ctx, origin.x, origin.y);
                }
                CTLineDraw(line, ctx);
            });
            CFRelease(line);
            // Masks are drawn in white, so any tint means colour glyphs (emoji).
            raster.map(|r| RasterGlyph { color: r.pixels.chunks_exact(4).any(|p| p[0] != p[1] || p[1] != p[2]), ..r })
        }
    }

    /// Where `text`, a character a cell in `style`, has ligatures: runs of
    /// cells whose glyphs the font's substitutions changed.
    pub fn ligatures(&self, text: &[char], style: Style) -> Vec<Ligature> {
        let own = self.own[style.0 as usize].as_ref();
        let (shaped, nominal): (Vec<(u16, usize)>, Vec<u16>) = match own {
            Some(face) => (
                face.shape(text, &self.shaping).iter().map(|s| (s.glyph, s.cluster as usize)).collect(),
                text.iter().map(|&c| face.glyph(c).unwrap_or(0)).collect(),
            ),
            // SAFETY: a live CTFont of ours.
            None => unsafe { self.shape_coretext(text, self.variants[style.0 as usize]) },
        };
        let mut per_char: Vec<Vec<u16>> = vec![Vec::new(); text.len()];
        for &(glyph, i) in &shaped {
            if let Some(list) = per_char.get_mut(i) {
                list.push(glyph);
            }
        }
        // Characters the font lacks (glyph 0) are left to fallback fonts.
        let changed: Vec<bool> = (0..text.len()).map(|i| nominal[i] != 0 && !per_char[i].contains(&0) && per_char[i] != [nominal[i]]).collect();
        // Runs of changed glyphs can be long (`====…` in Fira Code), and
        // each is one tile: cut them so a tile fits the glyph atlas.
        let longest = (MAX_TILE_WIDTH / self.cell_width.max(1)).clamp(1, MAX_LIGATURE_CELLS) as usize;
        let mut out = Vec::new();
        let mut i = 0;
        while i < text.len() {
            if !changed[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < text.len() && changed[i] && i - start < longest {
                i += 1;
            }
            let glyphs = (start..i).flat_map(|c| per_char[c].iter().map(move |&g| (g, (c - start) as u16))).collect();
            out.push(Ligature { start, cells: i - start, glyphs });
        }
        out
    }

    /// `text` shaped by CoreText in `font`: each glyph and the character
    /// it starts at. Glyphs of fallback fonts come back as glyph 0, and the
    /// glyphs CoreText would draw for each character alone as well.
    unsafe fn shape_coretext(&self, text: &[char], font: CTFontRef) -> (Vec<(u16, usize)>, Vec<u16>) {
        let string: String = text.iter().collect();
        // UTF-16 offsets to character indices.
        let mut char_at = Vec::new();
        for (i, c) in text.iter().enumerate() {
            char_at.extend(std::iter::repeat_n(i, c.len_utf16()));
        }
        let mut shaped = Vec::new();
        let mut nominal = Vec::with_capacity(text.len());
        // SAFETY: CoreFoundation/CoreText calls with valid arguments; every
        // object created here is released before returning, runs and their
        // attributes belong to the line.
        unsafe {
            for c in text {
                let mut units = [0u16; 2];
                let units = c.encode_utf16(&mut units);
                let mut glyphs = [0u16; 2];
                let found = CTFontGetGlyphsForCharacters(font, units.as_ptr(), glyphs.as_mut_ptr(), units.len() as isize);
                nominal.push(if found { glyphs[0] } else { 0 });
            }
            let keys = [kCTFontAttributeName];
            let values = [font];
            let attributes = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            );
            let cf = cfstring(&string);
            let attributed = CFAttributedStringCreate(ptr::null(), cf, attributes);
            CFRelease(cf);
            CFRelease(attributes);
            if attributed.is_null() {
                return (shaped, nominal);
            }
            let line = CTLineCreateWithAttributedString(attributed);
            CFRelease(attributed);
            if line.is_null() {
                return (shaped, nominal);
            }
            let runs = CTLineGetGlyphRuns(line);
            for r in 0..CFArrayGetCount(runs) {
                let run = CFArrayGetValueAtIndex(runs, r);
                let n = CTRunGetGlyphCount(run).max(0);
                let mut glyphs = vec![0u16; n as usize];
                let mut indices = vec![0isize; n as usize];
                CTRunGetGlyphs(run, CFRange { location: 0, length: n }, glyphs.as_mut_ptr());
                CTRunGetStringIndices(run, CFRange { location: 0, length: n }, indices.as_mut_ptr());
                let run_font = CFDictionaryGetValue(CTRunGetAttributes(run), kCTFontAttributeName);
                let ours = !run_font.is_null() && CFEqual(run_font, font);
                for (g, i) in glyphs.into_iter().zip(indices) {
                    if let Some(&c) = usize::try_from(i).ok().and_then(|i| char_at.get(i)) {
                        shaped.push((if ours { g } else { 0 }, c));
                    }
                }
            }
            CFRelease(line);
        }
        (shaped, nominal)
    }

    /// A ligature's glyphs drawn together into a tile `cells` wide, each
    /// placed in its cell as `rasterize` places a lone glyph.
    pub fn rasterize_ligature(&self, glyphs: &[(u16, u16)], style: Style, cells: u32) -> Option<RasterGlyph> {
        let (cw, width) = (self.cell_width as f32, self.cell_width * cells);
        if let Some(face) = &self.own[style.0 as usize] {
            // One outline, so pieces meeting at a cell edge join without a seam.
            let em = face.metrics().units_per_em as f32;
            let scale = self.size as f32 / em;
            let synthetic = self.synthetic[style.0 as usize];
            let mut outline = Vec::new();
            for &(glyph, cell) in glyphs {
                let x = cell as f32 * cw + ((cw - face.advance(glyph) * scale) / 2.0).max(0.0).round();
                let mut part = face.outline(glyph).ok()?;
                if synthetic & Style::ITALIC != 0 {
                    mightty_font::oblique(&mut part, OBLIQUE);
                }
                if synthetic & Style::BOLD != 0 {
                    mightty_font::embolden(&mut part, EMBOLDEN * em);
                }
                mightty_font::translate(&mut part, x / scale, 0.0);
                outline.extend(part);
            }
            let coverage = mightty_font::rasterize(&outline, scale, 0.0, self.baseline as f32, width as usize, self.cell_height as usize);
            let pixels = coverage.iter().flat_map(|&c| [c; 4]).collect();
            return Some(RasterGlyph { pixels, width, height: self.cell_height, color: false });
        }
        let font = self.variants[style.0 as usize];
        let ids: Vec<u16> = glyphs.iter().map(|g| g.0).collect();
        let mut advances = vec![CGSize::default(); ids.len()];
        // SAFETY: buffers sized for the glyphs; the font is ours.
        unsafe { CTFontGetAdvancesForGlyphs(font, 0, ids.as_ptr(), advances.as_mut_ptr(), ids.len() as isize) };
        let positions: Vec<CGPoint> = glyphs
            .iter()
            .zip(&advances)
            .map(|(&(_, cell), a)| CGPoint { x: cell as f64 * cw as f64 + ((cw as f64 - a.width) / 2.0).max(0.0).round(), y: 0.0 })
            .collect();
        // SAFETY: as `rasterize`; positions are relative to the tile's origin.
        unsafe {
            self.draw(cells, width as f64, style, |ctx, origin| {
                let placed: Vec<CGPoint> = positions.iter().map(|p| CGPoint { x: origin.x + p.x, y: origin.y }).collect();
                CTFontDrawGlyphs(font, ids.as_ptr(), placed.as_ptr(), ids.len(), ctx);
            })
        }
    }

    /// A cluster shaped, placed and drawn by us: in our face for `style` if
    /// it has every character, else in the fallback font CoreText picks for
    /// the cluster. `None` leaves it to CoreText: fonts composing with
    /// Apple's `morx` (Menlo, Apple Color Emoji), and sequences the font
    /// doesn't join into one colour glyph.
    fn draw_cluster_own(&self, text: &str, style: Style, cells: u32) -> Option<RasterGlyph> {
        let chars: Vec<char> = text.chars().collect();
        let draw = |face: &Face| -> Option<RasterGlyph> {
            if face.uses_morx() {
                return None;
            }
            // Joiners and variation selectors a font doesn't map only steer
            // substitution; any other character it lacks is another font's.
            let invisible = |c: char| matches!(c, '\u{200C}' | '\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}');
            let shaped: Vec<_> = face.shape(&chars, &self.shaping).into_iter().filter(|s| s.glyph != 0 || !invisible(chars[s.cluster as usize])).collect();
            if shaped.is_empty() || shaped.iter().any(|s| s.glyph == 0) {
                return None;
            }
            let (width, height) = (self.cell_width * cells, self.cell_height);
            let em = face.metrics().units_per_em as f32;
            let scale = self.size as f32 / em;
            let (placed, advance) = face.position(&shaped);
            let x = ((width as f32 - advance * scale) / 2.0).max(0.0).round();
            let baseline = self.baseline as f32;
            if let [one] = placed[..]
                && let Some(mut pixels) = mightty_font::draw_color(face, one.glyph, self.size as f32, x, baseline, width as usize, height as usize)
            {
                for p in pixels.chunks_exact_mut(4) {
                    p.swap(0, 2);
                }
                return Some(RasterGlyph { pixels, width, height, color: true });
            }
            // Several colour glyphs: a sequence the font didn't join.
            if placed.iter().any(|p| face.color_glyph(p.glyph, self.size as f32).is_some()) {
                return None;
            }
            let synthetic = self.synthetic[style.0 as usize];
            let mut outline = Vec::new();
            for p in &placed {
                let mut part = face.outline(p.glyph).ok()?;
                if synthetic & Style::ITALIC != 0 {
                    mightty_font::oblique(&mut part, OBLIQUE);
                }
                if synthetic & Style::BOLD != 0 {
                    mightty_font::embolden(&mut part, EMBOLDEN * em);
                }
                mightty_font::translate(&mut part, p.x, p.y);
                outline.extend(part);
            }
            let coverage = mightty_font::rasterize(&outline, scale, x, baseline, width as usize, height as usize);
            Some(RasterGlyph { pixels: coverage.iter().flat_map(|&c| [c; 4]).collect(), width, height, color: false })
        };
        // VS16 asks for the emoji, which a text face's glyph isn't.
        if let Some(face) = &self.own[style.0 as usize]
            && (!chars.contains(&'\u{FE0F}') || face.glyph(chars[0]).is_some_and(|g| face.color_glyph(g, self.size as f32).is_some()))
            && chars.iter().all(|&c| face.glyph(c).is_some() || matches!(c, '\u{200C}' | '\u{200D}' | '\u{FE00}'..='\u{FE0F}'))
        {
            return draw(face);
        }
        // SAFETY: CoreText calls with valid arguments; the fallback font is released.
        unsafe {
            let cf = cfstring(text);
            let length = text.encode_utf16().count() as isize;
            let fallback = CTFontCreateForString(self.variants[style.0 as usize], cf, CFRange { location: 0, length });
            CFRelease(cf);
            if fallback.is_null() {
                return None;
            }
            let raster = self.fallback_face(fallback, draw);
            CFRelease(fallback);
            raster
        }
    }

    /// `f` with our face for the fallback font CoreText picked, read once.
    fn fallback_face<T>(&self, font: CTFontRef, f: impl FnOnce(&Face) -> Option<T>) -> Option<T> {
        // SAFETY: a live CTFont; the copied name is released.
        let name = unsafe {
            let name = CTFontCopyPostScriptName(font);
            let ps = string_from_ns(name as Id);
            CFRelease(name);
            ps
        };
        let mut fallbacks = self.fallbacks.borrow_mut();
        let i = match fallbacks.iter().position(|(n, _)| *n == name) {
            Some(i) => i,
            None => {
                let face = own_face(font, &mut self.files.borrow_mut());
                fallbacks.push((name, face));
                fallbacks.len() - 1
            }
        };
        f(fallbacks[i].1.as_ref()?)
    }

    /// `glyph` of `face` drawn by our rasterizer, placed as `draw` places
    /// CoreText's: on the baseline, centred when narrower than the tile.
    /// In colour if the face has it so (emoji).
    fn draw_own(&self, face: &Face, glyph: u16, style: Style, cells: u32) -> Option<RasterGlyph> {
        let (width, height) = (self.cell_width * cells, self.cell_height);
        let em = face.metrics().units_per_em as f32;
        let scale = self.size as f32 / em;
        let advance = face.advance(glyph) * scale;
        let x = ((width as f32 - advance) / 2.0).max(0.0).round();
        let baseline = self.baseline as f32;
        if let Some(mut pixels) = mightty_font::draw_color(face, glyph, self.size as f32, x, baseline, width as usize, height as usize) {
            // RGBA to BGRA.
            for p in pixels.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            return Some(RasterGlyph { pixels, width, height, color: true });
        }
        let mut outline = face.outline(glyph).ok()?;
        let synthetic = self.synthetic[style.0 as usize];
        if synthetic & Style::ITALIC != 0 {
            mightty_font::oblique(&mut outline, OBLIQUE);
        }
        if synthetic & Style::BOLD != 0 {
            mightty_font::embolden(&mut outline, EMBOLDEN * em);
        }
        let coverage = mightty_font::rasterize(&outline, scale, x, baseline, width as usize, height as usize);
        // White, premultiplied: every channel is the coverage.
        let pixels = coverage.iter().flat_map(|&c| [c; 4]).collect();
        Some(RasterGlyph { pixels, width, height, color: false })
    }

    /// A tile `cells` wide with `draw` called on it at the baseline origin
    /// for content `width` pixels wide, centred when narrower than the tile
    /// (wide CJK, emoji), stroked as well as filled for synthetic bold.
    /// `color` is left false.
    /// An icon from the built-in symbols font, whose glyphs are an em wide
    /// whatever the cell: shrunk to fit `cells`, never enlarged, and centred.
    fn draw_icon(&self, glyph: u16, style: Style, cells: u32) -> Option<RasterGlyph> {
        let (width, height) = ((self.cell_width * cells) as f64, self.cell_height as f64);
        // SAFETY: `symbols` is live; the scaled copy is released here.
        unsafe {
            let ink = CTFontGetBoundingRectsForGlyphs(self.symbols, 0, &glyph, ptr::null_mut(), 1);
            // A pixel clear each side, so anti-aliasing isn't cut off.
            let scale = if ink.size.width > 0.0 && ink.size.height > 0.0 {
                ((width - 2.0) / ink.size.width).min((height - 2.0) / ink.size.height).min(1.0)
            } else {
                1.0
            };
            let font = CTFontCreateCopyWithAttributes(self.symbols, self.size * scale, ptr::null(), ptr::null());
            if font.is_null() {
                return None;
            }
            let origin = CGPoint {
                x: ((width - ink.size.width * scale) / 2.0 - ink.origin.x * scale).round(),
                y: ((height - ink.size.height * scale) / 2.0 - ink.origin.y * scale).round(),
            };
            let raster = self.draw(cells, width, style, |ctx, _| CTFontDrawGlyphs(font, &glyph, &origin, 1, ctx));
            CFRelease(font);
            raster
        }
    }

    unsafe fn draw(&self, cells: u32, width: f64, style: Style, draw: impl FnOnce(CGContextRef, CGPoint)) -> Option<RasterGlyph> {
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
            if self.synthetic[style.0 as usize] & Style::BOLD != 0 {
                // The stroke straddles the outline: half of it grows the glyph.
                CGContextSetTextDrawingMode(ctx, kCGTextFillStroke);
                CGContextSetRGBStrokeColor(ctx, 1.0, 1.0, 1.0, 1.0);
                CGContextSetLineWidth(ctx, 2.0 * EMBOLDEN as f64 * self.size);
            }
            let x = ((tile_width as f64 - width) / 2.0).max(0.0).round();
            // CoreGraphics' origin is bottom-left.
            let y = (height - self.baseline) as f64;
            draw(ctx, CGPoint { x, y });
            CGContextRelease(ctx);
        }
        Some(RasterGlyph { pixels, width: tile_width, height, color: false })
    }
}

/// `name` at `size` pixels: a family, or a PostScript name. CoreText falls
/// back to a default font for names it doesn't know (never null), so that
/// is reported.
unsafe fn open_font(name: &str, size: f64) -> CTFontRef {
    // SAFETY: as documented; the strings are released here.
    unsafe {
        let cf = cfstring(name);
        let font = CTFontCreateWithName(cf, size, ptr::null());
        CFRelease(cf);
        let names = [CTFontCopyFamilyName(font), CTFontCopyPostScriptName(font)];
        let found = names.iter().any(|&n| !n.is_null() && string_from_ns(n as Id).eq_ignore_ascii_case(name));
        for n in names.into_iter().filter(|n| !n.is_null()) {
            CFRelease(n);
        }
        if !found {
            eprintln!("mightty: no font named `{name}`; using the default");
        }
        font
    }
}

/// Symbols Nerd Font Mono, built in as Ghostty and kitty do, so Nerd Font
/// icons show without installing anything (see assets/fonts for its licence).
static SYMBOLS: &[u8] = include_bytes!("../../../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// The Unicode Private Use Areas, where Nerd Font icons live.
pub fn is_private_use(c: char) -> bool {
    matches!(c, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..)
}

/// The built-in symbols font at `size` pixels, or null.
fn symbols_font(size: f64) -> CTFontRef {
    // A CTFontDescriptor, made once: immutable, so safe to share.
    static DESCRIPTOR: OnceLock<usize> = OnceLock::new();
    let descriptor = *DESCRIPTOR.get_or_init(|| {
        // SAFETY: `SYMBOLS` is static, so the data can borrow it without
        // copying; both objects live for the rest of the process.
        unsafe {
            let data = CFDataCreateWithBytesNoCopy(ptr::null(), SYMBOLS.as_ptr(), SYMBOLS.len() as isize, kCFAllocatorNull);
            if data.is_null() {
                return 0;
            }
            CTFontManagerCreateFontDescriptorFromData(data) as usize
        }
    });
    if descriptor == 0 {
        return ptr::null();
    }
    // SAFETY: a live descriptor; the font is the caller's to release.
    unsafe { CTFontCreateWithFontDescriptor(descriptor as CFTypeRef, size, ptr::null()) }
}

/// `font` (consumed) with `font-variation` axes and `font-feature`s set.
unsafe fn with_settings(font: CTFontRef, variations: &[([u8; 4], f64)], features: &[([u8; 4], u32)]) -> CTFontRef {
    if variations.is_empty() && features.is_empty() {
        return font;
    }
    // SAFETY: CoreFoundation calls with valid arguments; everything created
    // here is released, the dictionaries and arrays retain what they hold.
    unsafe {
        let dict = |keys: &[CFTypeRef], values: &[CFTypeRef]| {
            CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                keys.len() as isize,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            )
        };
        let int = |v: i64| CFNumberCreate(ptr::null(), kCFNumberSInt64Type, (&raw const v).cast());
        let float = |v: f64| CFNumberCreate(ptr::null(), kCFNumberFloat64Type, (&raw const v).cast());
        let mut owned = Vec::new();
        let mut keys = Vec::new();
        let mut values = Vec::new();
        if !variations.is_empty() {
            let tags: Vec<CFTypeRef> = variations.iter().map(|(t, _)| int(u32::from_be_bytes(*t) as i64)).collect();
            let amounts: Vec<CFTypeRef> = variations.iter().map(|&(_, v)| float(v)).collect();
            let axes = dict(&tags, &amounts);
            owned.extend(tags.into_iter().chain(amounts));
            keys.push(kCTFontVariationAttribute);
            values.push(axes);
            owned.push(axes);
        }
        if !features.is_empty() {
            let settings: Vec<CFTypeRef> = features
                .iter()
                .map(|&(tag, value)| {
                    let (name, number) = (cfstring(std::str::from_utf8(&tag).unwrap_or("    ")), int(value as i64));
                    let setting = dict(&[kCTFontOpenTypeFeatureTag, kCTFontOpenTypeFeatureValue], &[name, number]);
                    owned.extend([name, number]);
                    setting
                })
                .collect();
            let array = CFArrayCreate(ptr::null(), settings.as_ptr(), settings.len() as isize, (&raw const kCFTypeArrayCallBacks).cast());
            owned.extend(settings);
            keys.push(kCTFontFeatureSettingsAttribute);
            values.push(array);
            owned.push(array);
        }
        let attributes = dict(&keys, &values);
        let descriptor = CTFontDescriptorCreateWithAttributes(attributes);
        let changed = CTFontCreateCopyWithAttributes(font, 0.0, ptr::null(), descriptor);
        for object in owned.into_iter().chain([attributes, descriptor]) {
            CFRelease(object);
        }
        if changed.is_null() {
            font
        } else {
            CFRelease(font);
            changed
        }
    }
}

/// Our face for CoreText's `font`: the face it was loaded from, found by
/// its file (opened once, in `files`) and PostScript name, and for a
/// variable font set to the same position. `None` where we can't read it
/// (CFF2 outlines, for one), which leaves it to CoreText.
fn own_face(font: CTFontRef, files: &mut Vec<(String, FontData)>) -> Option<Face> {
    {
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
                let data = FontData::map(path.as_ref()).ok()?;
                files.push((path.clone(), data.clone()));
                data
            }
        };
        let faces = Face::count(&data);
        let face = (0..faces).find_map(|i| Face::parse(data.clone(), i).ok().filter(|f| f.postscript_name().as_deref() == Some(name.as_str())));
        let mut face = face.or_else(|| if faces == 1 { Face::parse(data.clone(), 0).ok() } else { None });
        match &mut face {
            None => eprintln!("mightty: font-backend=mightty can't read {name} ({path}); CoreText draws it"),
            // CoreText names the axes it moved; none means the defaults,
            // whatever the PostScript name says.
            // SAFETY: a live CTFont.
            Some(face) if !face.axes().is_empty() => face.set_variations(&unsafe { variation(font) }),
            Some(_) => {}
        }
        face
    }
}

/// `font`'s position on each variation axis it sets: (tag, value).
unsafe fn variation(font: CTFontRef) -> Vec<([u8; 4], f32)> {
    // SAFETY: the dictionary is copied here and released; its keys and
    // values are CFNumbers it owns while it lives.
    unsafe {
        let dict = CTFontCopyVariation(font);
        if dict.is_null() {
            return Vec::new();
        }
        let n = CFDictionaryGetCount(dict).max(0) as usize;
        let (mut keys, mut values) = (vec![ptr::null(); n], vec![ptr::null(); n]);
        CFDictionaryGetKeysAndValues(dict, keys.as_mut_ptr(), values.as_mut_ptr());
        let settings = keys
            .iter()
            .zip(&values)
            .filter_map(|(&key, &value)| {
                // Keys are the axes' tags as 32-bit numbers.
                let (mut tag, mut v) = (0i64, 0f64);
                (CFNumberGetValue(key, kCFNumberSInt64Type, (&raw mut tag).cast())
                    && CFNumberGetValue(value, kCFNumberFloat64Type, (&raw mut v).cast()))
                .then(|| ((tag as u32).to_be_bytes(), v as f32))
            })
            .collect();
        CFRelease(dict);
        settings
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: we own one reference to each.
        unsafe {
            for font in self.variants.into_iter().chain(self.chain.iter().copied()) {
                CFRelease(font);
            }
            if !self.symbols.is_null() {
                CFRelease(self.symbols);
            }
            CGColorSpaceRelease(self.colorspace);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLES: [Style; 4] = [Style(0), Style(Style::BOLD), Style(Style::ITALIC), Style(Style::BOLD | Style::ITALIC)];

    /// How far our glyphs may be from CoreText's, as shares of their ink:
    /// any one glyph, the average glyph, and all of them together.
    struct Limits {
        worst: f64,
        mean: f64,
        total: f64,
    }

    /// Edges anti-aliased a little differently, no more: a misplaced or
    /// misread glyph differs by far more of its ink. Small glyphs (a
    /// colon's dots) are mostly edge, so one may differ more.
    const CLOSE: Limits = Limits { worst: 0.10, mean: 0.02, total: 0.02 };

    /// Our rasterizer against CoreText on font `name` at `size`: the same
    /// cell, and glyphs within `limits` of each other. Skipped where the
    /// font isn't installed.
    fn compare(name: &str, size: f64, styles: &[Style], chars: impl Iterator<Item = char> + Clone, limits: Limits) {
        let theirs = Font::new(name, size, FontBackend::CoreText);
        // SAFETY: a live CTFont; the copied name is released.
        let found = unsafe {
            let ps = CTFontCopyPostScriptName(theirs.variants[0]);
            let found = string_from_ns(ps as Id);
            CFRelease(ps);
            found
        };
        if !found.starts_with(name) {
            eprintln!("{name} isn't installed; skipped");
            return;
        }
        let ours = Font::new(name, size, FontBackend::Mightty);
        assert_eq!((ours.cell_width, ours.cell_height, ours.baseline), (theirs.cell_width, theirs.cell_height, theirs.baseline), "{name} {size}");
        let mut worst = (0.0f64, ' ', Style::default());
        let (mut sum, mut count, mut ink) = (0.0, 0, (0u64, 0u64));
        for &style in styles {
            assert!(ours.own[style.0 as usize].is_some(), "{name} {style:?} is read by us");
            for c in chars.clone() {
                let a = theirs.rasterize(c, style, 1).unwrap();
                let b = ours.rasterize(c, style, 1).unwrap();
                let total: u64 = a.pixels.iter().step_by(4).map(|&p| p as u64).sum();
                let diff: u64 = a.pixels.iter().zip(&b.pixels).step_by(4).map(|(&p, &q)| (p as i32 - q as i32).unsigned_abs() as u64).sum();
                ink = (ink.0 + total, ink.1 + b.pixels.iter().step_by(4).map(|&p| p as u64).sum::<u64>());
                let relative = diff as f64 / total.max(1) as f64;
                (sum, count) = (sum + relative, count + 1);
                if relative > worst.0 {
                    worst = (relative, c, style);
                }
            }
        }
        let mean = sum / count as f64;
        let total = ink.1 as f64 / ink.0 as f64 - 1.0;
        assert!(worst.0 < limits.worst, "{name} {size}: {:?} {:?} differs by {:.1}% of its ink", worst.1, worst.2, worst.0 * 100.0);
        assert!(mean < limits.mean, "{name} {size}: glyphs differ by {:.1}% of their ink on average", mean * 100.0);
        assert!(total.abs() < limits.total, "{name} {size}: {:+.1}% ink in all", total * 100.0);
    }

    fn ascii() -> impl Iterator<Item = char> + Clone {
        (0x21u8..0x7f).map(char::from)
    }

    #[test]
    fn matches_coretext() {
        for size in [24.0, 28.0, 33.0] {
            compare("Menlo", size, &STYLES, ascii().chain(['é', 'ß', '─', '█', '→']), CLOSE);
        }
    }

    /// CFF outlines, CID-keyed (Hiragino) and name-keyed (Kohinoor).
    #[test]
    fn matches_coretext_cff() {
        for size in [24.0, 33.0] {
            compare("HiraginoSans-W3", size, &STYLES[..1], ascii().chain("あア字漢".chars()), CLOSE);
            compare("KohinoorDevanagari-Regular", size, &STYLES[..1], ascii().chain("कख".chars()), CLOSE);
        }
    }

    /// Monaco has no bold or italic: both are made up, by CoreText's
    /// stroke and slant there and by our outline growing and slant here.
    /// CoreText fills and strokes in two anti-aliased passes, which leaves
    /// edges lumpier than growing the outline: bold adds the same ink in
    /// all, spread a little differently.
    #[test]
    fn matches_coretext_synthetic() {
        for size in [24.0, 33.0] {
            compare("Monaco", size, &STYLES[..1], ascii(), CLOSE);
            compare("Monaco", size, &STYLES[2..3], ascii(), CLOSE);
            compare("Monaco", size, &[STYLES[1], STYLES[3]], ascii(), Limits { worst: 0.2, mean: 0.08, ..CLOSE });
        }
    }

    /// Emoji from the fallback font CoreText picks (Apple Color Emoji),
    /// drawn by us from its PNG images: the same colours in the same place,
    /// scaled a little differently. CJK fallbacks are drawn by us too.
    #[test]
    fn matches_coretext_fallbacks() {
        for size in [24.0, 33.0, 50.0] {
            let theirs = Font::new("Menlo", size, FontBackend::CoreText);
            let ours = Font::new("Menlo", size, FontBackend::Mightty);
            for c in ['😀', '🍎', '👍', '🚀', '⌚', 'あ', '字'] {
                let a = theirs.rasterize(c, Style(0), 2).unwrap();
                let b = ours.rasterize(c, Style(0), 2).unwrap();
                assert_eq!(a.color, b.color, "{c}");
                let total: u64 = a.pixels.iter().map(|&p| p as u64).sum();
                let diff: u64 = a.pixels.iter().zip(&b.pixels).map(|(&p, &q)| (p as i32 - q as i32).unsigned_abs() as u64).sum();
                let relative = diff as f64 / total as f64;
                assert!(relative < 0.08, "{size} {c}: differs by {:.1}%", relative * 100.0);
            }
        }
    }

    /// Nerd Font icons come from the built-in symbols font, with either
    /// backend: git branch, folder, Apple logo (Font Logos), a Material icon.
    #[test]
    fn emoji_in_one_cell_fit_it() {
        let font = Font::new("Menlo", 32.0, FontBackend::CoreText);
        for text in ["\u{2764}\u{FE0F}", "\u{2600}\u{FE0F}"] {
            let raster = font.rasterize_cluster(text, Style(0), 1).unwrap();
            assert!(raster.color, "{text}");
            let (w, h) = (raster.width as usize, raster.height as usize);
            let inked = |x: usize, y: usize| raster.pixels[(y * w + x) * 4 + 3] > 0;
            let columns: Vec<usize> = (0..w).filter(|&x| (0..h).any(|y| inked(x, y))).collect();
            let rows = (0..h).filter(|&y| (0..w).any(|x| inked(x, y))).count();
            // Shrunk to the cell's width, square as an emoji is; cut off,
            // it would be half as wide as it's tall.
            assert!(columns.len() * 10 >= w * 8, "{text}: {} of {w} columns", columns.len());
            assert!(rows * 10 <= columns.len() * 12, "{text}: {rows} rows, {} columns", columns.len());
        }
    }

    #[test]
    fn nerd_font_icons() {
        for backend in [FontBackend::CoreText, FontBackend::Mightty] {
            let font = Font::new("Menlo", 32.0, backend);
            for c in ['\u{F418}', '\u{F07B}', '\u{F179}', '\u{F0001}'] {
                let raster = font.rasterize(c, Style(0), 1).unwrap_or_else(|| panic!("{:X}", c as u32));
                let ink = raster.pixels.iter().filter(|&&p| p > 0).count();
                assert!(ink > 50, "U+{:X}: {ink} pixels drawn", c as u32);
                // Whole: no solid ink in the tile's edge columns, where it would be cut.
                let (w, h) = (raster.width as usize, raster.height as usize);
                let edge = (0..h).any(|y| raster.pixels[(y * w) * 4 + 3] > 128 || raster.pixels[(y * w + w - 1) * 4 + 3] > 128);
                assert!(!edge, "U+{:X} touches the cell's edge", c as u32);
                let wide = font.rasterize(c, Style(0), 2).unwrap();
                let wide_ink = wide.pixels.iter().filter(|&&p| p > 0).count();
                assert!(wide_ink > ink, "U+{:X}: two cells draw it no larger", c as u32);
            }
        }
    }

    /// A long run of changed glyphs (`====…` in Fira Code) is cut into
    /// tiles that fit the glyph atlas, however large the font. Skipped
    /// where the font isn't installed.
    #[test]
    fn long_ligature_runs_fit_the_atlas() {
        let name = "FiraCode-Regular";
        for size in [28.0, 400.0] {
            let font = Font::new(name, size, FontBackend::CoreText);
            // SAFETY: a live CTFont; the copied name is released.
            let found = unsafe {
                let ps = CTFontCopyPostScriptName(font.variants[0]);
                let found = string_from_ns(ps as Id);
                CFRelease(ps);
                found
            };
            if found != name {
                return eprintln!("{name} isn't installed; skipped");
            }
            let run: Vec<char> = "=".repeat(200).chars().collect();
            let ligatures = font.ligatures(&run, Style(0));
            assert!(ligatures.len() > 1, "{size}: one tile for 200 cells");
            for lig in &ligatures {
                assert!(lig.cells as u32 * font.cell_width <= MAX_TILE_WIDTH, "{size}: {} cells", lig.cells);
            }
        }
    }

    /// Coding fonts' ligatures (contextual alternates): shaped by our GSUB
    /// as by CoreText, and drawn alike. Skipped where the fonts aren't installed.
    #[test]
    fn ligatures_match_coretext() {
        let texts = ["->", "a != b", "===", "<=>", "www", "/* x */", "|>", "::", "0x1F", "<!--", "a->b->c", "=>>", "fi", "plain"];
        for name in ["JetBrainsMono-Regular", "FiraCode-Regular"] {
            let theirs = Font::new(name, 28.0, FontBackend::CoreText);
            // SAFETY: a live CTFont; the copied name is released.
            let found = unsafe {
                let ps = CTFontCopyPostScriptName(theirs.variants[0]);
                let found = string_from_ns(ps as Id);
                CFRelease(ps);
                found
            };
            if found != name {
                eprintln!("{name} isn't installed; skipped");
                continue;
            }
            let ours = Font::new(name, 28.0, FontBackend::Mightty);
            for style in STYLES {
                for text in texts {
                    let chars: Vec<char> = text.chars().collect();
                    let (a, b) = (theirs.ligatures(&chars, style), ours.ligatures(&chars, style));
                    assert_eq!(a, b, "{name} {style:?} {text}");
                    for lig in &a {
                        let (x, y) = (theirs.rasterize_ligature(&lig.glyphs, style, lig.cells as u32).unwrap(), ours.rasterize_ligature(&lig.glyphs, style, lig.cells as u32).unwrap());
                        let total: u64 = x.pixels.iter().step_by(4).map(|&p| p as u64).sum();
                        let diff: u64 = x.pixels.iter().zip(&y.pixels).step_by(4).map(|(&p, &q)| (p as i32 - q as i32).unsigned_abs() as u64).sum();
                        let limit = if ours.synthetic[style.0 as usize] & Style::BOLD != 0 { 0.2 } else { CLOSE.worst };
                        assert!((diff as f64) < total as f64 * limit, "{name} {style:?} {text}: differs by {:.1}%", diff as f64 / total as f64 * 100.0);
                    }
                }
            }
            // An arrow: two cells drawn as one.
            let arrow = ours.ligatures(&['-', '>'], Style::default());
            assert_eq!((arrow.len(), arrow[0].start, arrow[0].cells), (1, 0, 2));
            assert!(ours.ligatures(&"plain".chars().collect::<Vec<_>>(), Style::default()).is_empty());
        }
        // A font without them: none.
        let menlo = Font::new("Menlo", 28.0, FontBackend::Mightty);
        assert!(menlo.ligatures(&['-', '>', '!', '='], Style::default()).is_empty());
    }

    /// Clusters (a letter and its accents) shaped and placed by us in
    /// OpenType fonts, by their anchors: drawn as CoreText draws them, give
    /// or take its centring them a pixel over for the accents' widths.
    /// Fonts composing with Apple's `morx` are left to CoreText.
    #[test]
    fn clusters_match_coretext() {
        let texts = ["e\u{301}", "a\u{308}", "n\u{303}", "e\u{301}\u{302}", "A\u{30A}", "x\u{301}"];
        for name in ["JetBrainsMono-Regular", "FiraCode-Regular", "Menlo"] {
            let theirs = Font::new(name, 28.0, FontBackend::CoreText);
            let ours = Font::new(name, 28.0, FontBackend::Mightty);
            if ours.own[0].as_ref().and_then(|f| f.postscript_name()).as_deref() != Some(name) {
                eprintln!("{name} isn't installed; skipped");
                continue;
            }
            let morx = ours.own[0].as_ref().is_some_and(|f| f.uses_morx());
            for text in texts {
                assert_eq!(ours.draw_cluster_own(text, Style::default(), 1).is_some(), !morx, "{name} {text:?}");
                let (a, b) = (theirs.rasterize_cluster(text, Style::default(), 1).unwrap(), ours.rasterize_cluster(text, Style::default(), 1).unwrap());
                let total: u64 = a.pixels.iter().step_by(4).map(|&p| p as u64).sum();
                let diff: u64 = a.pixels.iter().zip(&b.pixels).step_by(4).map(|(&p, &q)| (p as i32 - q as i32).unsigned_abs() as u64).sum();
                assert!((diff as f64) < total as f64 * 0.12, "{name} {text:?}: differs by {:.1}%", diff as f64 / total as f64 * 100.0);
            }
        }
    }

    /// A variable font: Bold is a position on its weight axis; it has no
    /// italic, so italic is made up. Its regular is ExtraLight, strokes a
    /// pixel wide, where the same edge differences are more of the ink.
    #[test]
    fn matches_coretext_variable() {
        for size in [24.0, 33.0] {
            compare("Manrope", size, &STYLES, ascii(), Limits { worst: 0.12, ..CLOSE });
        }
    }
}
