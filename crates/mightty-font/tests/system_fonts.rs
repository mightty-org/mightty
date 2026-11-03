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
    assert!(advance('M') > 0.0);
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
    let width = (face.advance(face.glyph('H').unwrap()) * scale).ceil() as usize;
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

/// CFF outlines: cubic curves from charstrings.
#[test]
fn cff_fonts() {
    if let Some(data) = load("/System/Library/Fonts/Supplemental/STIXGeneral.otf") {
        let face = Face::parse(data, 0).unwrap();
        let o = face.outline(face.glyph('o').unwrap()).unwrap();
        assert!(o.iter().any(|s| matches!(s, Segment::CubicTo(..))));
        assert_eq!(o.iter().filter(|s| matches!(s, Segment::MoveTo(_))).count(), 2);
        assert!(face.advance(face.glyph('W').unwrap()) > face.advance(face.glyph('i').unwrap()));
        assert!(face.outline(face.glyph(' ').unwrap()).unwrap().is_empty());
    }
    // CID-keyed: glyphs pick their subroutines through FDSelect.
    if let Some(data) = load("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc") {
        let face = Face::parse(data, 0).unwrap();
        for c in ['A', 'あ', '字', '漢'] {
            assert!(face.outline(face.glyph(c).unwrap()).unwrap().len() > 8, "{c}");
        }
    }
}

/// Variable fonts: outlines and advances move along the axes; the default
/// position is the font as stored.
#[test]
fn variable_fonts() {
    if let Some(data) = load("/System/Library/Fonts/SFNSMono.ttf") {
        let mut face = Face::parse(data, 0).unwrap();
        let wght = *face.axes().iter().find(|a| &a.tag == b"wght").unwrap();
        let (e, m) = (face.glyph('e').unwrap(), face.glyph('M').unwrap());
        let light = face.outline(e).unwrap();
        let ink = |o: &[Segment]| rasterize(o, 0.02, 0.0, 40.0, 40, 50).iter().map(|&v| v as u32).sum::<u32>();
        face.set_variations(&[(*b"wght", wght.default)]);
        assert_eq!(face.outline(e).unwrap(), light);
        face.set_variations(&[(*b"wght", wght.max)]);
        let heavy = face.outline(e).unwrap();
        // Points move; none are added.
        assert_eq!(heavy.len(), light.len());
        assert!(ink(&heavy) > ink(&light) * 3 / 2, "{} {}", ink(&heavy), ink(&light));
        // Monospaced at every weight (HVAR).
        assert_eq!(face.advance(e), face.advance(m));
        // Past the end of the axis is the end.
        face.set_variations(&[(*b"wght", wght.max * 10.0)]);
        assert_eq!(face.outline(e).unwrap(), heavy);
    }
    // avar remaps the axes, MVAR moves the metrics; an optical size axis.
    if let Some(data) = load("/System/Library/Fonts/NewYork.ttf") {
        let mut face = Face::parse(data, 0).unwrap();
        let g = face.glyph('a').unwrap();
        let large = face.outline(g).unwrap();
        face.set_variations(&[(*b"opsz", 12.0)]);
        assert_ne!(face.outline(g).unwrap(), large);
    }
    // No HVAR: advances move with the outline's phantom points.
    if let Some(data) = load("/Library/Fonts/Manrope[wght].ttf") {
        let mut face = Face::parse(data, 0).unwrap();
        let g = face.glyph('e').unwrap();
        let thin = face.advance(g);
        face.set_variations(&[(*b"wght", 800.0)]);
        assert!(face.advance(g) > thin, "{} {thin}", face.advance(g));
    }
}

/// `data` damaged many ways: cut short anywhere, and with bytes overwritten
/// (only within `tables`, if any named, to aim at their parsers). Each
/// copy must be an error or a glyph, never a panic or a hang.
fn fuzz(data: &[u8], tables: &[&[u8; 4]], variations: &[([u8; 4], f32)]) {
    let exercise = |bytes: Vec<u8>| {
        let Ok(mut face) = Face::parse(Arc::from(bytes), 0) else { return };
        face.set_variations(variations);
        let _ = face.postscript_name();
        let _ = face.metrics();
        for c in ['A', 'g', 'é', '@', '─', '\u{10FFFF}'] {
            if let Some(g) = face.glyph(c) {
                let _ = face.advance(g);
                if let Ok(outline) = face.outline(g) {
                    rasterize(&outline, 0.02, 0.0, 20.0, 12, 24);
                }
            }
        }
    };
    for len in (0..data.len()).step_by(data.len() / 500 + 1) {
        exercise(data[..len].to_vec());
    }
    // Where to overwrite: the named tables, from the table directory.
    let be = |at: usize, n: usize| data[at..at + n].iter().fold(0usize, |v, &b| (v << 8) | b as usize);
    let ranges: Vec<(usize, usize)> = if tables.is_empty() {
        vec![(0, data.len())]
    } else {
        (0..be(4, 2)).map(|i| 12 + 16 * i).filter(|&r| tables.iter().any(|t| &data[r..r + 4] == *t)).map(|r| (be(r + 8, 4), be(r + 12, 4))).collect()
    };
    assert!(!ranges.is_empty());
    // A small deterministic generator, so failures repeat.
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
            let (start, len) = ranges[next() as usize % ranges.len()];
            bytes[start + (next() % len as u64) as usize] = next() as u8;
        }
        exercise(bytes);
    }
}

#[test]
fn damaged_files_dont_panic() {
    if let Some(data) = load("/System/Library/Fonts/Monaco.ttf") {
        fuzz(&data, &[], &[]);
    }
    if let Some(data) = load("/System/Library/Fonts/Supplemental/STIXGeneralBol.otf") {
        fuzz(&data, &[b"CFF "], &[]);
    }
    if let Some(data) = load("/System/Library/Fonts/SFNSMono.ttf") {
        fuzz(&data, &[b"fvar", b"gvar", b"HVAR", b"MVAR"], &[(*b"wght", 700.0)]);
    }
}

/// A minimal font: glyph 0 empty, then `glyphs` as given, with outlines
/// from `glyf` or, if `cff` is given, from that CFF table.
fn font(glyphs: &[Vec<u8>], cff: Option<Vec<u8>>) -> Vec<u8> {
    let be16 = |v: u16| v.to_be_bytes();
    let count = cff.as_ref().map_or(glyphs.len() as u16 + 1, |_| 2);
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
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![(b"cmap", cmap), (b"head", head), (b"hhea", hhea), (b"hmtx", hmtx), (b"maxp", maxp)];
    let version = match cff {
        Some(cff) => {
            tables.push((b"CFF ", cff));
            0x4F54_544Fu32
        }
        None => {
            let (mut glyf, mut loca) = (Vec::new(), vec![0u8; 8]);
            for g in glyphs {
                glyf.extend(g);
                loca.extend((glyf.len() as u32).to_be_bytes());
            }
            tables.extend([(b"glyf", glyf), (b"loca", loca)]);
            0x0001_0000
        }
    };
    let mut out = [version.to_be_bytes().to_vec(), be16(tables.len() as u16).to_vec(), vec![0; 6]].concat();
    let mut offset = 12 + 16 * tables.len();
    let mut body: Vec<u8> = Vec::new();
    for (tag, data) in &tables {
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
    let face = Face::parse(Arc::from(font(&[square_glyph(), composite_glyph(1, 2)], None)), 0).unwrap();
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
    let face = Face::parse(Arc::from(font(&glyphs, None)), 0).unwrap();
    let started = std::time::Instant::now();
    assert!(face.outline(9).is_err());
    assert!(started.elapsed().as_secs() < 1);
}

/// A CFF table: glyph 0 empty, glyph 1 `program`, and `subrs` global subroutines.
fn cff(program: &[u8], subrs: &[Vec<u8>]) -> Vec<u8> {
    // An INDEX with one-byte count and four-byte offsets.
    let index = |items: &[Vec<u8>]| {
        let mut out = (items.len() as u16).to_be_bytes().to_vec();
        if items.is_empty() {
            return out;
        }
        out.push(4);
        let mut at = 1u32;
        out.extend(at.to_be_bytes());
        for item in items {
            at += item.len() as u32;
            out.extend(at.to_be_bytes());
        }
        items.iter().for_each(|i| out.extend(i));
        out
    };
    let names = index(&[b"A".to_vec()]);
    let strings = index(&[]);
    let globals = index(subrs);
    // The top DICT's CharStrings offset, as a five-byte number so its size is fixed.
    let top_len = index(&[vec![0; 6]]).len();
    let char_strings = 4 + names.len() + top_len + strings.len() + globals.len();
    let mut top = vec![29];
    top.extend((char_strings as u32).to_be_bytes());
    top.push(17);
    [vec![1, 0, 4, 4], names, index(&[top]), strings, globals, index(&[vec![14], program.to_vec()])].concat()
}

#[test]
fn cff_charstrings_run() {
    // 100 100 rmoveto, 300 0 rlineto, 0 300 rlineto, endchar: a triangle.
    let program = [139 + 100, 139 + 100, 21, 247, 300u16.wrapping_sub(108) as u8, 139, 5, 139, 247, 300u16.wrapping_sub(108) as u8, 5, 14];
    let face = Face::parse(Arc::from(font(&[], Some(cff(&program, &[])))), 0).unwrap();
    let o = face.outline(1).unwrap();
    let p = |x, y| mightty_font::Point { x, y };
    assert_eq!(o, [Segment::MoveTo(p(100.0, 100.0)), Segment::LineTo(p(400.0, 100.0)), Segment::LineTo(p(400.0, 400.0)), Segment::Close]);
}

/// Subroutines calling each other ten times at each of ten levels would
/// run 10¹⁰ operators: stopped quickly instead.
#[test]
fn charstring_bombs_are_refused() {
    // Subroutine k calls k + 1 ten times; the last draws a line. Numbers
    // are stored minus 107 and pushed as one byte (n + 139).
    let call = |k: i32| [(k - 107 + 139) as u8, 29];
    let mut subrs: Vec<Vec<u8>> = (0..9).map(|k| [call(k + 1).repeat(10), vec![11]].concat()).collect();
    subrs.push(vec![140, 140, 5, 11]);
    let program = [[139, 139, 21].as_slice(), &call(0), &[14]].concat();
    let face = Face::parse(Arc::from(font(&[], Some(cff(&program, &subrs)))), 0).unwrap();
    let started = std::time::Instant::now();
    assert!(face.outline(1).is_err());
    assert!(started.elapsed().as_secs() < 1);
}
