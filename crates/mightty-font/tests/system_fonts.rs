//! Against fonts macOS ships; each test is skipped where its font isn't installed.

use std::sync::Arc;

use mightty_font::{Face, Segment, rasterize};

fn load(path: &str) -> Option<Arc<[u8]>> {
    std::fs::read(path).ok().map(Arc::from)
}

#[test]
fn menlo_collection() {
    let Some(data) = load("/System/Library/Fonts/Menlo.ttc") else { return };
    let count = Face::count(&data);
    assert!(count >= 4, "{count}");
    let names: Vec<String> = (0..count).map(|i| Face::parse(data.clone(), i).unwrap().postscript_name().unwrap()).collect();
    for name in ["Menlo-Regular", "Menlo-Bold", "Menlo-Italic", "Menlo-BoldItalic"] {
        assert!(names.iter().any(|n| n == name), "{names:?}");
    }
    let face = Face::parse(data.clone(), names.iter().position(|n| n == "Menlo-Regular").unwrap() as u32).unwrap();
    let m = face.metrics();
    assert!(m.units_per_em >= 1000 && m.ascender > 0 && m.descender < 0, "{m:?}");
    // Monospaced: every character advances alike.
    let advance = |c| face.advance(face.glyph(c).unwrap());
    assert!(advance('M') > 0);
    assert!(['i', 'W', '0', '{', 'é', '─'].iter().all(|&c| advance(c) == advance('M')));
    // Not in Menlo: left to fallback fonts.
    assert_eq!(face.glyph('一'), None);
    assert_eq!(face.glyph('😀'), None);
    // Curves, closed contours; the space has none.
    let o = face.outline(face.glyph('o').unwrap()).unwrap();
    assert!(o.iter().any(|s| matches!(s, Segment::QuadTo(..))));
    assert_eq!(o.iter().filter(|s| matches!(s, Segment::MoveTo(_))).count(), 2);
    assert!(face.outline(face.glyph(' ').unwrap()).unwrap().is_empty());
    // 'é' is a composite (e + accent) in many fonts: either way it has more contours than 'e'.
    let contours = |c| face.outline(face.glyph(c).unwrap()).unwrap().iter().filter(|s| matches!(s, Segment::MoveTo(_))).count();
    assert!(contours('é') > contours('e'));
}

#[test]
fn rasterized_h_has_two_stems_and_a_bar() {
    let Some(data) = load("/System/Library/Fonts/Monaco.ttf") else { return };
    let face = Face::parse(data, 0).unwrap();
    let m = face.metrics();
    let size = 40.0;
    let scale = size / m.units_per_em as f32;
    let width = (face.advance(face.glyph('H').unwrap()) as f32 * scale).ceil() as usize;
    let baseline = (m.ascender as f32 * scale).round();
    let height = (baseline - m.descender as f32 * scale).ceil() as usize;
    let pixels = rasterize(&face.outline(face.glyph('H').unwrap()).unwrap(), scale, 0.0, baseline, width, height);
    let at = |x: usize, y: usize| pixels[y * width + x];
    // A row through the upper half: dark, light, dark.
    let row: Vec<u8> = (0..width).map(|x| at(x, height / 4)).collect();
    let dark: Vec<bool> = row.iter().map(|&c| c > 200).collect();
    let runs = dark.windows(2).filter(|w| w[0] != w[1]).count();
    assert_eq!(runs, 4, "{row:?}");
    // The bar: a dark row near the middle, all the way across the stems.
    let middle = (height / 4..height * 3 / 4).any(|y| (width / 4..width * 3 / 4).all(|x| at(x, y) > 200));
    assert!(middle);
    // Nothing above the cap height (lower than the ascender) or below the baseline.
    let below = baseline as usize + 1;
    assert!((0..width).all(|x| at(x, 0) == 0 && (below..height).all(|y| at(x, y) == 0)));
}

/// Damaged files are errors, never panics or hangs.
#[test]
fn damaged_files_dont_panic() {
    let Some(data) = load("/System/Library/Fonts/Monaco.ttf") else { return };
    let exercise = |bytes: Vec<u8>| {
        let Ok(face) = Face::parse(Arc::from(bytes), 0) else { return };
        let _ = face.postscript_name();
        for c in ['A', 'g', 'é', '@', '─', '\u{10FFFF}'] {
            if let Some(g) = face.glyph(c) {
                let _ = face.advance(g);
                if let Ok(outline) = face.outline(g) {
                    rasterize(&outline, 0.02, 0.0, 20.0, 12, 24);
                }
            }
        }
    };
    // Cut short anywhere.
    for len in (0..data.len()).step_by(data.len() / 500 + 1) {
        exercise(data[..len].to_vec());
    }
    // Bytes overwritten: a small deterministic generator, so failures repeat.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..2000 {
        let mut bytes = data.to_vec();
        for _ in 0..8 {
            let at = (next() % bytes.len() as u64) as usize;
            bytes[at] = next() as u8;
        }
        exercise(bytes);
    }
}

/// A minimal TrueType font: glyph 0 empty, then `glyphs` as given.
fn font(glyphs: &[Vec<u8>]) -> Vec<u8> {
    let be16 = |v: u16| v.to_be_bytes();
    let count = glyphs.len() as u16 + 1;
    let mut head = vec![0u8; 54];
    head[18..20].copy_from_slice(&be16(1000));
    head[50..52].copy_from_slice(&be16(1)); // long loca
    let mut hhea = vec![0u8; 36];
    hhea[34..36].copy_from_slice(&be16(1));
    let mut maxp = vec![0u8; 6];
    maxp[4..6].copy_from_slice(&be16(count));
    // cmap: one format 4 subtable mapping nothing but the final 0xFFFF segment.
    let mut cmap = [be16(0), be16(1), be16(3), be16(1)].concat();
    cmap.extend(12u32.to_be_bytes());
    cmap.extend([be16(4), be16(24), be16(0), be16(2), be16(2), be16(0), be16(0)].concat());
    cmap.extend([be16(0xFFFF), be16(0), be16(0xFFFF), be16(1), be16(0)].concat());
    let hmtx = [be16(500), be16(0)].concat();
    let (mut glyf, mut loca) = (Vec::new(), vec![0u8; 8]);
    for g in glyphs {
        glyf.extend(g);
        loca.extend((glyf.len() as u32).to_be_bytes());
    }
    let tables: [(&[u8; 4], &[u8]); 7] =
        [(b"cmap", &cmap), (b"glyf", &glyf), (b"head", &head), (b"hhea", &hhea), (b"hmtx", &hmtx), (b"loca", &loca), (b"maxp", &maxp)];
    let mut out = [0x0001_0000u32.to_be_bytes().to_vec(), be16(tables.len() as u16).to_vec(), vec![0; 6]].concat();
    let mut offset = 12 + 16 * tables.len();
    let mut body: Vec<u8> = Vec::new();
    for (tag, data) in tables {
        out.extend(*tag);
        out.extend([0u32.to_be_bytes(), (offset as u32).to_be_bytes(), (data.len() as u32).to_be_bytes()].concat());
        body.extend(data);
        offset += data.len();
    }
    out.extend(body);
    out
}

/// A 100-unit square.
fn square_glyph() -> Vec<u8> {
    let mut g = [1i16, 0, 0, 100, 100, 3, 0].iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<u8>>();
    g.extend([1u8; 4]); // on-curve points, word deltas
    g.extend([0i16, 100, 0, -100, 0, 0, 100, 0].iter().flat_map(|v| v.to_be_bytes()));
    g
}

/// `copies` references to glyph `of`, all at the origin.
fn composite_glyph(of: u16, copies: usize) -> Vec<u8> {
    let mut g = [-1i16, 0, 0, 100, 100].iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<u8>>();
    for i in 0..copies {
        let flags: u16 = if i + 1 < copies { 0x0022 } else { 0x0002 }; // MORE, ARGS_ARE_XY
        g.extend(flags.to_be_bytes());
        g.extend(of.to_be_bytes());
        g.extend([0, 0]);
    }
    g
}

#[test]
fn composites_are_expanded() {
    let face = Face::parse(Arc::from(font(&[square_glyph(), composite_glyph(1, 2)])), 0).unwrap();
    let moves = |g| face.outline(g).unwrap().iter().filter(|s| matches!(s, Segment::MoveTo(_))).count();
    assert_eq!((moves(1), moves(2)), (1, 2));
}

/// Composites fanning out at every level would expand into 100⁸ squares:
/// refused quickly instead.
#[test]
fn composite_bombs_are_refused() {
    // Glyph 1 is the square; glyph n + 1 holds 100 copies of glyph n.
    let mut glyphs = vec![square_glyph()];
    for n in 1..=8 {
        glyphs.push(composite_glyph(n, 100));
    }
    let face = Face::parse(Arc::from(font(&glyphs)), 0).unwrap();
    let started = std::time::Instant::now();
    assert!(face.outline(9).is_err());
    assert!(started.elapsed().as_secs() < 1);
}
