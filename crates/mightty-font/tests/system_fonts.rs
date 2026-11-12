//! Against fonts macOS ships; each test is skipped where its font isn't installed.

use mightty_font::{ColorGlyph, Face, FontData, LIGATURES, Segment, draw_color, rasterize};

fn load(path: &str) -> Option<FontData> {
    FontData::map(path.as_ref()).ok()
}

#[test]
fn menlo_collection() {
    let Some(data) = load("/System/Library/Fonts/Menlo.ttc") else {
        return;
    };
    let count = Face::count(&data);
    assert!(count >= 4, "{count}");
    let names: Vec<String> = (0..count)
        .map(|i| {
            Face::parse(data.clone(), i)
                .unwrap()
                .postscript_name()
                .unwrap()
        })
        .collect();
    for name in [
        "Menlo-Regular",
        "Menlo-Bold",
        "Menlo-Italic",
        "Menlo-BoldItalic",
    ] {
        assert!(names.iter().any(|n| n == name), "{names:?}");
    }
    let face = Face::parse(
        data.clone(),
        names.iter().position(|n| n == "Menlo-Regular").unwrap() as u32,
    )
    .unwrap();
    let m = face.metrics();
    assert!(
        m.units_per_em >= 1000 && m.ascender > 0 && m.descender < 0,
        "{m:?}"
    );
    // Monospaced: every character advances alike.
    let advance = |c| face.advance(face.glyph(c).unwrap());
    assert!(advance('M') > 0.0);
    assert!(
        ['i', 'W', '0', '{', 'é', '─']
            .iter()
            .all(|&c| advance(c) == advance('M'))
    );
    // Not in Menlo: left to fallback fonts.
    assert_eq!(face.glyph('一'), None);
    assert_eq!(face.glyph('😀'), None);
    // Curves, closed contours; the space has none.
    let o = face.outline(face.glyph('o').unwrap()).unwrap();
    assert!(o.iter().any(|s| matches!(s, Segment::QuadTo(..))));
    assert_eq!(
        o.iter().filter(|s| matches!(s, Segment::MoveTo(_))).count(),
        2
    );
    assert!(face.outline(face.glyph(' ').unwrap()).unwrap().is_empty());
    // 'é' is a composite (e + accent) in many fonts: either way it has more contours than 'e'.
    let contours = |c| {
        face.outline(face.glyph(c).unwrap())
            .unwrap()
            .iter()
            .filter(|s| matches!(s, Segment::MoveTo(_)))
            .count()
    };
    assert!(contours('é') > contours('e'));
}

/// A collection claiming four billion faces holds what it has room to list.
#[test]
fn collection_count_is_bounded() {
    let mut ttc = b"ttcf\0\x01\0\0\xff\xff\xff\xff".to_vec();
    ttc.extend([0; 8]);
    assert_eq!(Face::count(&ttc), 2);
    ttc.extend(vec![0; 1 << 16]);
    assert_eq!(Face::count(&ttc), 1024);
}

#[test]
fn rasterized_h_has_two_stems_and_a_bar() {
    let Some(data) = load("/System/Library/Fonts/Monaco.ttf") else {
        return;
    };
    let face = Face::parse(data, 0).unwrap();
    let m = face.metrics();
    let size = 40.0;
    let scale = size / m.units_per_em as f32;
    let width = (face.advance(face.glyph('H').unwrap()) * scale).ceil() as usize;
    let baseline = (m.ascender as f32 * scale).round();
    let height = (baseline - m.descender as f32 * scale).ceil() as usize;
    let pixels = rasterize(
        &face.outline(face.glyph('H').unwrap()).unwrap(),
        scale,
        0.0,
        baseline,
        width,
        height,
    );
    let at = |x: usize, y: usize| pixels[y * width + x];
    // A row through the upper half: dark, light, dark.
    let row: Vec<u8> = (0..width).map(|x| at(x, height / 4)).collect();
    let dark: Vec<bool> = row.iter().map(|&c| c > 200).collect();
    let runs = dark.windows(2).filter(|w| w[0] != w[1]).count();
    assert_eq!(runs, 4, "{row:?}");
    // The bar: a dark row near the middle, all the way across the stems.
    let middle =
        (height / 4..height * 3 / 4).any(|y| (width / 4..width * 3 / 4).all(|x| at(x, y) > 200));
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
        assert_eq!(
            o.iter().filter(|s| matches!(s, Segment::MoveTo(_))).count(),
            2
        );
        assert!(face.advance(face.glyph('W').unwrap()) > face.advance(face.glyph('i').unwrap()));
        assert!(face.outline(face.glyph(' ').unwrap()).unwrap().is_empty());
    }
    // CID-keyed: glyphs pick their subroutines through FDSelect.
    if let Some(data) = load("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc") {
        let face = Face::parse(data, 0).unwrap();
        for c in ['A', 'あ', '字', '漢'] {
            assert!(
                face.outline(face.glyph(c).unwrap()).unwrap().len() > 8,
                "{c}"
            );
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
        let ink = |o: &[Segment]| {
            rasterize(o, 0.02, 0.0, 40.0, 40, 50)
                .iter()
                .map(|&v| v as u32)
                .sum::<u32>()
        };
        face.set_variations(&[(*b"wght", wght.default)]);
        assert_eq!(face.outline(e).unwrap(), light);
        face.set_variations(&[(*b"wght", wght.max)]);
        let heavy = face.outline(e).unwrap();
        // Points move; none are added.
        assert_eq!(heavy.len(), light.len());
        assert!(
            ink(&heavy) > ink(&light) * 3 / 2,
            "{} {}",
            ink(&heavy),
            ink(&light)
        );
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
/// `rounds` times (only within `tables`, if any named, to aim at their
/// parsers). Each copy must be an error or a glyph, never a panic or a hang.
fn fuzz(data: &[u8], tables: &[&[u8; 4]], variations: &[([u8; 4], f32)], rounds: usize) {
    let exercise = |bytes: Vec<u8>| {
        let Ok(mut face) = Face::parse(FontData::from(bytes), 0) else {
            return;
        };
        face.set_variations(variations);
        let _ = face.postscript_name();
        let _ = face.metrics();
        for text in ["->", "a != b === c", "e\u{301}\u{302}", "www <!-- -->"] {
            let chars: Vec<char> = text.chars().collect();
            let shaped = face.shape(&chars, &LIGATURES);
            let _ = face.position(&shaped);
        }
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
    let be = |at: usize, n: usize| {
        data[at..at + n]
            .iter()
            .fold(0usize, |v, &b| (v << 8) | b as usize)
    };
    let ranges: Vec<(usize, usize)> = if tables.is_empty() {
        vec![(0, data.len())]
    } else {
        (0..be(4, 2))
            .map(|i| 12 + 16 * i)
            .filter(|&r| tables.iter().any(|t| &data[r..r + 4] == *t))
            .map(|r| (be(r + 8, 4), be(r + 12, 4)))
            .collect()
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
    for _ in 0..rounds {
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
        fuzz(&data, &[], &[], 2000);
    }
    if let Some(data) = load("/System/Library/Fonts/Supplemental/STIXGeneralBol.otf") {
        fuzz(&data, &[b"CFF "], &[], 2000);
    }
    if let Some(data) = load("/System/Library/Fonts/SFNSMono.ttf") {
        fuzz(
            &data,
            &[b"fvar", b"gvar", b"HVAR", b"MVAR"],
            &[(*b"wght", 700.0)],
            2000,
        );
    }
    // Substitution and positioning: a coding font's ligatures and anchors
    // (fewer rounds: each copy works out its 460 lookups afresh).
    if let Some(data) = user_font("JetBrainsMono-Regular.ttf") {
        fuzz(&data, &[b"GSUB", b"GPOS", b"GDEF"], &[], 300);
    }
}

/// A minimal font: glyph 0 empty, then `glyphs` as given, with outlines
/// from `glyf` or, if `cff` is given, from that CFF table.
fn font(glyphs: &[Vec<u8>], cff: Option<Vec<u8>>) -> Vec<u8> {
    font_with(glyphs, cff, Vec::new())
}

/// The same, with `extra` tables.
fn font_with(glyphs: &[Vec<u8>], cff: Option<Vec<u8>>, extra: Vec<(&[u8; 4], Vec<u8>)>) -> Vec<u8> {
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
    cmap.extend(
        [
            be16(4),
            be16(24),
            be16(0),
            be16(2),
            be16(2),
            be16(0),
            be16(0),
        ]
        .concat(),
    );
    cmap.extend([be16(0xFFFF), be16(0), be16(0xFFFF), be16(1), be16(0)].concat());
    let hmtx = [be16(500), be16(0)].concat();
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"cmap", cmap),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"maxp", maxp),
    ];
    tables.extend(extra);
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
    let mut out = [
        version.to_be_bytes().to_vec(),
        be16(tables.len() as u16).to_vec(),
        vec![0; 6],
    ]
    .concat();
    let mut offset = 12 + 16 * tables.len();
    let mut body: Vec<u8> = Vec::new();
    for (tag, data) in &tables {
        out.extend(*tag);
        out.extend(
            [
                0u32.to_be_bytes(),
                (offset as u32).to_be_bytes(),
                (data.len() as u32).to_be_bytes(),
            ]
            .concat(),
        );
        body.extend(data);
        offset += data.len();
    }
    out.extend(body);
    out
}

/// A 100-unit square.
fn square_glyph() -> Vec<u8> {
    let mut g = [1i16, 0, 0, 100, 100, 3, 0]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect::<Vec<u8>>();
    g.extend([1u8; 4]); // on-curve points, word deltas
    g.extend(
        [0i16, 100, 0, -100, 0, 0, 100, 0]
            .iter()
            .flat_map(|v| v.to_be_bytes()),
    );
    g
}

/// `copies` references to glyph `of`, all at the origin.
fn composite_glyph(of: u16, copies: usize) -> Vec<u8> {
    let mut g = [-1i16, 0, 0, 100, 100]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect::<Vec<u8>>();
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
    let face = Face::parse(
        FontData::from(font(&[square_glyph(), composite_glyph(1, 2)], None)),
        0,
    )
    .unwrap();
    let moves = |g| {
        face.outline(g)
            .unwrap()
            .iter()
            .filter(|s| matches!(s, Segment::MoveTo(_)))
            .count()
    };
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
    let face = Face::parse(FontData::from(font(&glyphs, None)), 0).unwrap();
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
    [
        vec![1, 0, 4, 4],
        names,
        index(&[top]),
        strings,
        globals,
        index(&[vec![14], program.to_vec()]),
    ]
    .concat()
}

#[test]
fn cff_charstrings_run() {
    // 100 100 rmoveto, 300 0 rlineto, 0 300 rlineto, endchar: a triangle.
    let program = [
        139 + 100,
        139 + 100,
        21,
        247,
        300u16.wrapping_sub(108) as u8,
        139,
        5,
        139,
        247,
        300u16.wrapping_sub(108) as u8,
        5,
        14,
    ];
    let face = Face::parse(FontData::from(font(&[], Some(cff(&program, &[])))), 0).unwrap();
    let o = face.outline(1).unwrap();
    let p = |x, y| mightty_font::Point { x, y };
    assert_eq!(
        o,
        [
            Segment::MoveTo(p(100.0, 100.0)),
            Segment::LineTo(p(400.0, 100.0)),
            Segment::LineTo(p(400.0, 400.0)),
            Segment::Close
        ]
    );
}

/// Subroutines calling each other ten times at each of ten levels would
/// run 10¹⁰ operators: stopped quickly instead.
#[test]
fn charstring_bombs_are_refused() {
    // Subroutine k calls k + 1 ten times; the last draws a line. Numbers
    // are stored minus 107 and pushed as one byte (n + 139).
    let call = |k: i32| [(k - 107 + 139) as u8, 29];
    let mut subrs: Vec<Vec<u8>> = (0..9)
        .map(|k| [call(k + 1).repeat(10), vec![11]].concat())
        .collect();
    subrs.push(vec![140, 140, 5, 11]);
    let program = [[139, 139, 21].as_slice(), &call(0), &[14]].concat();
    let face = Face::parse(FontData::from(font(&[], Some(cff(&program, &subrs)))), 0).unwrap();
    let started = std::time::Instant::now();
    assert!(face.outline(1).is_err());
    assert!(started.elapsed().as_secs() < 1);
}

/// A PNG of `width`×`height` pixels all `rgba`, stored without compression.
fn png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let chunk = |kind: &[u8; 4], body: &[u8]| {
        [&(body.len() as u32).to_be_bytes()[..], kind, body, &[0; 4]].concat()
    };
    let mut ihdr = [width.to_be_bytes(), height.to_be_bytes()].concat();
    ihdr.extend([8, 6, 0, 0, 0]);
    let mut rows = Vec::new();
    for _ in 0..height {
        rows.push(0);
        rows.extend(rgba.repeat(width as usize));
    }
    let mut z = vec![0x78, 0x01, 1];
    z.extend((rows.len() as u16).to_le_bytes());
    z.extend((!(rows.len() as u16)).to_le_bytes());
    z.extend(&rows);
    z.extend([0; 4]);
    [
        vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'],
        chunk(b"IHDR", &ihdr),
        chunk(b"IDAT", &z),
        chunk(b"IEND", &[]),
    ]
    .concat()
}

/// Glyphs 1 and 2 as images in an sbix strike at 20 ppem: a red 4×2 image
/// whose bottom left is at (1, 3), and a 'dupe' of it.
fn sbix_font() -> Vec<u8> {
    let image = [
        &1i16.to_be_bytes()[..],
        &3i16.to_be_bytes(),
        b"png ",
        &png(4, 2, [255, 0, 0, 255]),
    ]
    .concat();
    let dupe = [
        &0i16.to_be_bytes()[..],
        &0i16.to_be_bytes(),
        b"dupe",
        &1u16.to_be_bytes(),
    ]
    .concat();
    // Strike: ppem, ppi, offsets for glyphs 0..=2 and the end.
    let start = 4 + 4 * 4;
    let offsets = [
        start,
        start,
        start + image.len(),
        start + image.len() + dupe.len(),
    ];
    let mut strike = [20u16.to_be_bytes(), 72u16.to_be_bytes()].concat();
    offsets
        .iter()
        .for_each(|&o| strike.extend((o as u32).to_be_bytes()));
    strike.extend(&image);
    strike.extend(&dupe);
    let sbix = [
        &1u16.to_be_bytes()[..],
        &1u16.to_be_bytes(),
        &1u32.to_be_bytes(),
        &12u32.to_be_bytes(),
        &strike,
    ]
    .concat();
    font_with(&[vec![], vec![]], None, vec![(b"sbix", sbix)])
}

/// Glyph 1 as a CBDT image at 20 ppem: a green 3×3 image, left bearing
/// 2, top bearing 15 (index format 1, image format 17).
fn cbdt_font() -> Vec<u8> {
    let image = png(3, 3, [0, 255, 0, 255]);
    let mut cbdt = vec![0, 3, 0, 0];
    cbdt.extend([3, 3, 2, 15, 3]); // small metrics: height, width, bearings, advance
    cbdt.extend((image.len() as u32).to_be_bytes());
    cbdt.extend(&image);
    let end = cbdt.len() as u32;
    // CBLC: header, one BitmapSize (48 bytes), its subtable array and subtable.
    let mut cblc = vec![0, 3, 0, 0, 0, 0, 0, 1];
    let array = 8 + 48;
    let mut size = vec![0u8; 48];
    size[0..4].copy_from_slice(&(array as u32).to_be_bytes());
    size[8..12].copy_from_slice(&1u32.to_be_bytes());
    size[40..42].copy_from_slice(&1u16.to_be_bytes());
    size[42..44].copy_from_slice(&1u16.to_be_bytes());
    size[44] = 20;
    size[45] = 20;
    size[46] = 32;
    cblc.extend(size);
    cblc.extend([0, 1, 0, 1, 0, 0, 0, 8]); // glyphs 1..=1, subtable 8 bytes on
    cblc.extend([0, 1, 0, 17]); // index format 1, image format 17
    cblc.extend(4u32.to_be_bytes()); // images start after CBDT's header
    cblc.extend([0u32.to_be_bytes(), (end - 4).to_be_bytes()].concat());
    font_with(
        &[vec![], vec![]],
        None,
        vec![(b"CBLC", cblc), (b"CBDT", cbdt)],
    )
}

/// Glyph 2 as COLR layers of glyph 1 (a 100-unit square): palette entry 0
/// (blue, half transparent), then the text's colour.
fn colr_font() -> Vec<u8> {
    let colr = [0u16.to_be_bytes(), 1u16.to_be_bytes()]
        .concat()
        .into_iter()
        .chain(14u32.to_be_bytes())
        .chain(20u32.to_be_bytes())
        .chain(2u16.to_be_bytes())
        .chain([0, 2, 0, 0, 0, 2]) // base glyph 2: layers 0, 1
        .chain([0, 1, 0, 0, 0, 1, 0xFF, 0xFF])
        .collect();
    let cpal = [0, 0, 0, 1, 0, 1, 0, 1, 0, 0, 0, 14, 0, 0, 255, 0, 0, 128].to_vec();
    font_with(
        &[square_glyph(), composite_glyph(1, 1)],
        None,
        vec![(b"COLR", colr), (b"CPAL", cpal)],
    )
}

#[test]
fn colour_glyphs() {
    // sbix: the image and its duplicate, placed by the image's origin
    // and drawn 1/8 em lower, as Apple's renderer does.
    let face = Face::parse(FontData::from(sbix_font()), 0).unwrap();
    for g in [1, 2] {
        let Some(ColorGlyph::Bitmap {
            image,
            ppem,
            left,
            top,
        }) = face.color_glyph(g, 20.0)
        else {
            panic!("glyph {g}")
        };
        assert_eq!(
            (image.width, image.height, ppem, left, top),
            (4, 2, 20.0, 1.0, 3.0 + 2.0 - 2.5)
        );
    }
    // At 40 pixels, twice the size: 8×4 red, from x 2 and y 20 - 5.
    let tile = draw_color(&face, 1, 40.0, 0.0, 20.0, 12, 24).unwrap();
    let red = |x: usize, y: usize| tile[(y * 12 + x) * 4..][..4] == [255, 0, 0, 255];
    assert!(red(2, 15) && red(9, 18) && !red(1, 15) && !red(10, 15) && !red(2, 14) && !red(2, 19));

    // CBDT: bearings from the small metrics.
    let face = Face::parse(FontData::from(cbdt_font()), 0).unwrap();
    let Some(ColorGlyph::Bitmap {
        image, left, top, ..
    }) = face.color_glyph(1, 20.0)
    else {
        panic!()
    };
    assert_eq!(
        (image.pixels[..4].to_vec(), left, top),
        (vec![0, 255, 0, 255], 2.0, 15.0)
    );
    assert!(face.outline(1).unwrap().is_empty());

    // COLR: a half-transparent blue square, then the text's colour (white) over it.
    let face = Face::parse(FontData::from(colr_font()), 0).unwrap();
    assert_eq!(
        face.color_glyph(2, 20.0),
        Some(ColorGlyph::Layers(vec![
            (1, Some([0, 0, 255, 128])),
            (1, None)
        ]))
    );
    assert_eq!(face.color_glyph(1, 20.0), None);
    let tile = draw_color(&face, 2, 100.0, 0.0, 10.0, 10, 10).unwrap();
    assert_eq!(&tile[..4], [255, 255, 255, 255]);
}

#[test]
fn damaged_colour_fonts_dont_panic() {
    for bytes in [sbix_font(), cbdt_font(), colr_font()] {
        for i in 0..bytes.len() {
            for v in [0, 1, 0x7f, 0x80, 0xfe, 0xff] {
                let mut d = bytes.clone();
                d[i] = v;
                let Ok(face) = Face::parse(FontData::from(d), 0) else {
                    continue;
                };
                for g in 0..3 {
                    let _ = face.color_glyph(g, 20.0);
                    let _ = draw_color(&face, g, 30.0, 0.0, 20.0, 16, 24);
                }
            }
        }
    }
}

/// Apple Color Emoji: sbix strikes from 20 to 160 ppem, a 180 MB file read
/// through a mapping.
#[test]
fn apple_color_emoji() {
    let Some(data) = load("/System/Library/Fonts/Apple Color Emoji.ttc") else {
        return;
    };
    let face = Face::parse(data, 0).unwrap();
    let g = face.glyph('😀').unwrap();
    let Some(ColorGlyph::Bitmap { image, ppem, .. }) = face.color_glyph(g, 33.0) else {
        panic!()
    };
    assert_eq!((image.width, image.height, ppem), (40, 40, 40.0));
    // Every strike's image for a sample of glyphs decodes.
    for g in (1..face.glyph_count()).step_by(97) {
        for ppem in [20.0, 64.0, 160.0] {
            if let Some(glyph) = face.color_glyph(g, ppem) {
                assert!(matches!(glyph, ColorGlyph::Bitmap { .. }));
            }
        }
    }
}

/// A font installed for the user, as Homebrew's font casks put them.
fn user_font(file: &str) -> Option<FontData> {
    let home = std::env::var_os("HOME")?;
    FontData::map(&std::path::Path::new(&home).join("Library/Fonts").join(file)).ok()
}

/// Coding fonts' ligatures are contextual alternates: a glyph a character,
/// changed where they make a ligature, as the fonts' own GSUB says.
#[test]
fn ligature_fonts() {
    for file in ["JetBrainsMono-Regular.ttf", "FiraCode-Regular.ttf"] {
        let Some(data) = user_font(file) else {
            continue;
        };
        let face = Face::parse(data, 0).unwrap();
        let shape = |text: &str| {
            let chars: Vec<char> = text.chars().collect();
            let nominal: Vec<u16> = chars.iter().map(|&c| face.glyph(c).unwrap()).collect();
            let shaped = face.shape(&chars, &LIGATURES);
            assert_eq!(
                shaped
                    .iter()
                    .map(|s| s.cluster as usize)
                    .collect::<Vec<_>>(),
                (0..chars.len()).collect::<Vec<_>>(),
                "{file} {text}"
            );
            (shaped.iter().map(|s| s.glyph).collect::<Vec<_>>(), nominal)
        };
        for text in ["->", "!=", "===", "<=>"] {
            let (shaped, nominal) = shape(text);
            assert!(
                shaped.iter().zip(&nominal).all(|(a, b)| a != b),
                "{file} {text}: {shaped:?} {nominal:?}"
            );
        }
        for text in ["plain", "a b", "- >"] {
            let (shaped, nominal) = shape(text);
            assert_eq!(shaped, nominal, "{file} {text}");
        }
        // Without the features, nothing changes.
        let chars: Vec<char> = "->".chars().collect();
        assert_eq!(
            face.shape(&chars, &[])
                .iter()
                .map(|s| s.glyph)
                .collect::<Vec<_>>(),
            chars
                .iter()
                .map(|&c| face.glyph(c).unwrap())
                .collect::<Vec<_>>()
        );
        // An accent sits above its letter, by the font's anchors.
        let chars: Vec<char> = "e\u{301}".chars().collect();
        let (placed, width) = face.position(&face.shape(&chars, &LIGATURES));
        assert_eq!(width, face.advance(face.glyph('e').unwrap()));
        let (lo, hi) = face.bounds(placed[1].glyph).unwrap();
        let centre = placed[1].x + (lo.x + hi.x) / 2.0;
        assert!(
            centre > 0.0 && centre < width && placed[1].y + lo.y > 0.0,
            "{placed:?}"
        );
    }
}

/// A GSUB whose one lookup (chained context, format 3, on glyph 0) calls
/// itself 64 times: nested as deep as allowed, that's 64⁴ calls a glyph.
fn recursive_gsub() -> Vec<u8> {
    let be = |v: u16| v.to_be_bytes();
    // Header (10), script list (8) + script (4) + lang sys (8) at 10,
    // feature list (8) + feature (6) at 30, lookup list at 44.
    let mut g = [be(1), be(0), be(10), be(30), be(44)].concat();
    g.extend(
        [
            be(1),
            *b"DF",
            *b"LT",
            be(8),
            be(4),
            be(0),
            be(0),
            be(0xFFFF),
            be(1),
            be(0),
        ]
        .concat(),
    );
    g.extend([be(1), *b"li", *b"ga", be(8), be(0), be(1), be(0)].concat());
    // Lookup list (4) at 44, lookup (8) at 48, subtable at 56.
    g.extend([be(1), be(4), be(6), be(0), be(1), be(8)].concat());
    let records = 64u16;
    let coverage = 2 + 2 + 2 + 2 + 2 + 2 + 4 * records;
    g.extend([be(3), be(0), be(1), be(coverage), be(0), be(records)].concat());
    for _ in 0..records {
        g.extend([be(0), be(0)].concat());
    }
    g.extend([be(1), be(1), be(0)].concat());
    g
}

#[test]
fn substitution_bombs_are_refused() {
    let face = Face::parse(
        FontData::from(font_with(&[], None, vec![(b"GSUB", recursive_gsub())])),
        0,
    )
    .unwrap();
    let text = vec!['x'; 200];
    let started = std::time::Instant::now();
    let shaped = face.shape(&text, &LIGATURES);
    assert_eq!(shaped.len(), 200);
    assert!(started.elapsed().as_secs() < 1, "{:?}", started.elapsed());
}

/// A GSUB listing 30000 lookups, all one lookup of 512 single
/// substitutions on every glyph: 15 million subtables to read.
fn huge_gsub() -> Vec<u8> {
    let be = |v: u16| v.to_be_bytes();
    let lookups: u16 = 30_000;
    let features: u16 = 4096;
    // Header, script list, lang sys, feature list at 30, its feature at 38.
    let mut g = [be(1), be(0), be(10), be(30), be(0)].concat();
    g.extend(
        [
            be(1),
            *b"DF",
            *b"LT",
            be(8),
            be(4),
            be(0),
            be(0),
            be(0xFFFF),
            be(1),
            be(0),
        ]
        .concat(),
    );
    g.extend([be(1), *b"li", *b"ga", be(8), be(0), be(features)].concat());
    (0..features).for_each(|i| g.extend(be(i)));
    let list = g.len();
    g[8..10].copy_from_slice(&be(list as u16));
    let lookup = 2 + 2 * lookups as usize;
    g.extend(be(lookups));
    (0..lookups).for_each(|_| g.extend(be(lookup as u16)));
    let subtables: u16 = 512;
    let subtable = 6 + 2 * subtables;
    g.extend([be(1), be(0), be(subtables)].concat());
    (0..subtables).for_each(|_| g.extend(be(subtable)));
    // Single substitution format 1 (delta 0) over a coverage of all glyphs.
    g.extend([be(1), be(6), be(0), be(2), be(1), be(0), be(0xFFFF), be(0)].concat());
    g
}

#[test]
fn huge_lookup_lists_are_read_within_a_budget() {
    let face = Face::parse(
        FontData::from(font_with(&[], None, vec![(b"GSUB", huge_gsub())])),
        0,
    )
    .unwrap();
    let started = std::time::Instant::now();
    let shaped = face.shape(&['x'; 100], &LIGATURES);
    assert_eq!(shaped.len(), 100);
    assert!(started.elapsed().as_secs() < 1, "{:?}", started.elapsed());
}
