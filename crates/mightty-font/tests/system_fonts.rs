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
