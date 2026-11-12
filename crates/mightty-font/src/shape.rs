//! Glyph substitution (`GSUB`): what turns `->` into an arrow in a coding
//! font, joins emoji sequences and composes accented letters. The lookups
//! of the features asked for run over the text's glyphs in order:
//! single, multiple, alternate and ligature substitutions, in context or
//! chained context (lookup types 1 to 7; type 8, reverse chaining, is only
//! used by Arabic). Fonts are untrusted, so nested lookups and the total
//! work are limited.

use crate::face::{Bytes, Face};

/// A glyph after substitution, and the character it came from: the first
/// of several, for a ligature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shaped {
    pub glyph: u16,
    /// Index into the text of the character this glyph starts at.
    pub cluster: u32,
}

/// The features a terminal turns on: composition, required and standard
/// ligatures, and contextual alternates (how coding fonts do theirs).
pub const LIGATURES: [[u8; 4]; 5] = [*b"ccmp", *b"rlig", *b"liga", *b"clig", *b"calt"];

/// Features without ligatures: what `font-ligatures = false` keeps.
pub const NO_LIGATURES: [[u8; 4]; 2] = [*b"ccmp", *b"rlig"];

/// Nested lookups (context calling context…): real fonts nest one or two.
const MAX_NESTING: usize = 4;
/// Steps one shaping may take, all lookups and positions together: a
/// character's worth, and a cap. Real text takes a few hundred a
/// character at most (a line of nothing but ligatures).
const WORK_PER_CHAR: usize = 1024;
const MAX_WORK: usize = 1 << 20;
/// What calling a nested lookup costs, in steps.
const NESTED_CALL: usize = 16;
/// Subtables read of one lookup: real fonts have a few, a handful of
/// dozen at most.
const MAX_SUBTABLES: usize = 512;
/// Steps reading a font's lookups and their starting glyphs may take,
/// once: JetBrains Mono and Fira Code take about 3000.
const PREPARE_WORK: usize = 1 << 20;
/// Glyphs a buffer may grow to (multiple substitutions add some).
const MAX_GLYPHS_PER_CHAR: usize = 4;

impl Face {
    /// `text`'s glyphs after the substitutions of `features`. Characters the
    /// face lacks are glyph 0.
    pub fn shape(&self, text: &[char], features: &[[u8; 4]]) -> Vec<Shaped> {
        let mut buffer: Vec<Shaped> = text
            .iter()
            .enumerate()
            .map(|(i, &c)| Shaped {
                glyph: self.glyph(c).unwrap_or(0),
                cluster: i as u32,
            })
            .collect();
        if let Some(gsub) = self.gsub_table() {
            let b = self.bytes();
            let gdef = self.gdef_table();
            let prepared = self.prepared(|| prepare(b, gsub, self.glyph_count()));
            let budget = (4 + text.len()).saturating_mul(WORK_PER_CHAR).min(MAX_WORK);
            let mut run = Run {
                b,
                prepared,
                gdef,
                work: 0,
                budget,
                limit: text.len().max(1) * MAX_GLYPHS_PER_CHAR,
            };
            for lookup in lookups_of(b, gsub, features) {
                let starts = prepared
                    .starts
                    .get(lookup as usize)
                    .and_then(Option::as_deref);
                // Most lookups can't start at most glyphs: skip those quickly.
                if starts.is_some_and(|set| !buffer.iter().any(|s| has(set, s.glyph))) {
                    continue;
                }
                run.apply_lookup(&mut buffer, lookup, starts);
                if run.work > run.budget {
                    break;
                }
            }
        }
        buffer
    }
}

/// The lookups `features` use in a GSUB or GPOS `table`, for the default
/// script and language, in the order they run (lookup list order).
pub(crate) fn lookups_of(b: Bytes<'_>, gsub: usize, features: &[[u8; 4]]) -> Vec<u16> {
    (|| {
        let scripts = gsub + b.u16(gsub + 4)? as usize;
        let feature_list = gsub + b.u16(gsub + 6)? as usize;
        let count = b.u16(scripts)? as usize;
        let script_at = |tag: &[u8; 4]| {
            (0..count).find(|&i| b.slice(scripts + 2 + 6 * i, 4).is_some_and(|t| t.0 == tag))
        };
        let i = script_at(b"DFLT")
            .or_else(|| script_at(b"latn"))
            .or((count > 0).then_some(0))?;
        let script = scripts + b.u16(scripts + 6 + 6 * i)? as usize;
        let lang = script + b.u16(script)? as usize;
        if lang == script {
            return None;
        }
        let required = b.u16(lang + 2)?;
        let n = b.u16(lang + 4)? as usize;
        let indices = (0..n)
            .filter_map(|k| b.u16(lang + 6 + 2 * k))
            .chain((required != 0xFFFF).then_some(required));
        let mut out = Vec::new();
        for f in indices {
            let record = feature_list + 2 + 6 * f as usize;
            if !features
                .iter()
                .any(|t| b.slice(record, 4).is_some_and(|r| r.0 == t))
            {
                continue;
            }
            let feature = feature_list + b.u16(record + 4)? as usize;
            let lookups = b.u16(feature + 2)? as usize;
            out.extend((0..lookups.min(4096)).filter_map(|k| b.u16(feature + 4 + 2 * k)));
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    })()
    .unwrap_or_default()
}

struct Run<'a> {
    b: Bytes<'a>,
    prepared: &'a Prepared,
    gdef: Option<usize>,
    work: usize,
    /// The most steps this shaping may take.
    budget: usize,
    /// The most glyphs the buffer may hold.
    limit: usize,
}

/// A lookup's type, flags and subtables (extensions unwrapped).
pub(crate) struct Lookup {
    kind: u16,
    flags: u16,
    subtables: Vec<usize>,
}

impl<'a> Run<'a> {
    /// Lookup `index`, as read once for the face.
    fn lookup(&self, index: u16) -> Option<&'a Lookup> {
        let prepared: &'a Prepared = self.prepared;
        prepared.lookups.get(index as usize)?.as_ref()
    }

    /// Runs lookup `index` along the buffer. `starts`: the glyphs it can
    /// start at, if known.
    fn apply_lookup(&mut self, buffer: &mut Vec<Shaped>, index: u16, starts: Option<&[u64]>) {
        let Some(lookup) = self.lookup(index) else {
            return;
        };
        let mut i = 0;
        while i < buffer.len() && self.work <= self.budget {
            self.work += 1;
            if starts.is_some_and(|set| !has(set, buffer[i].glyph))
                || self.ignored(lookup.flags, buffer[i].glyph)
            {
                i += 1;
                continue;
            }
            i = match self.apply_at(buffer, lookup, i, 0) {
                Some(next) => next.max(i + 1),
                None => i + 1,
            };
        }
    }

    /// Applies the first subtable of `lookup` that matches at `i`; where to
    /// go on from, if one did.
    fn apply_at(
        &mut self,
        buffer: &mut Vec<Shaped>,
        lookup: &Lookup,
        i: usize,
        depth: usize,
    ) -> Option<usize> {
        for &s in &lookup.subtables {
            self.work += 1;
            if self.work > self.budget {
                return None;
            }
            if let Some(next) = self.subtable(buffer, lookup, s, i, depth) {
                return Some(next);
            }
        }
        None
    }

    fn subtable(
        &mut self,
        buffer: &mut Vec<Shaped>,
        lookup: &Lookup,
        s: usize,
        i: usize,
        depth: usize,
    ) -> Option<usize> {
        let b = self.b;
        let glyph = buffer.get(i)?.glyph;
        let format = b.u16(s)?;
        match (lookup.kind, format) {
            // Single: by a delta, or from a list.
            (1, 1) => {
                coverage(b, s + b.u16(s + 2)? as usize, glyph)?;
                buffer[i].glyph = glyph.wrapping_add(b.u16(s + 4)?);
                Some(i + 1)
            }
            (1, 2) => {
                let k = coverage(b, s + b.u16(s + 2)? as usize, glyph)?;
                (k < b.u16(s + 4)? as usize).then_some(())?;
                buffer[i].glyph = b.u16(s + 6 + 2 * k)?;
                Some(i + 1)
            }
            // Multiple: one glyph for several (or none).
            (2, 1) => {
                let k = coverage(b, s + b.u16(s + 2)? as usize, glyph)?;
                (k < b.u16(s + 4)? as usize).then_some(())?;
                let sequence = s + b.u16(s + 6 + 2 * k)? as usize;
                let n = b.u16(sequence)? as usize;
                if buffer.len() + n > self.limit + 1 {
                    return None;
                }
                let glyphs: Vec<u16> = (0..n)
                    .map(|j| b.u16(sequence + 2 + 2 * j))
                    .collect::<Option<_>>()?;
                let cluster = buffer[i].cluster;
                buffer.splice(i..=i, glyphs.iter().map(|&g| Shaped { glyph: g, cluster }));
                Some(i + n)
            }
            // Alternate: the first alternate.
            (3, 1) => {
                let k = coverage(b, s + b.u16(s + 2)? as usize, glyph)?;
                (k < b.u16(s + 4)? as usize).then_some(())?;
                let set = s + b.u16(s + 6 + 2 * k)? as usize;
                (b.u16(set)? > 0).then_some(())?;
                buffer[i].glyph = b.u16(set + 2)?;
                Some(i + 1)
            }
            // Ligature: several glyphs for one, the longest that matches first.
            (4, 1) => {
                let k = coverage(b, s + b.u16(s + 2)? as usize, glyph)?;
                (k < b.u16(s + 4)? as usize).then_some(())?;
                let set = s + b.u16(s + 6 + 2 * k)? as usize;
                for l in 0..b.u16(set)? as usize {
                    self.work += 1;
                    if self.work > self.budget {
                        return None;
                    }
                    let lig = set + b.u16(set + 2 + 2 * l)? as usize;
                    let (result, count) = (b.u16(lig)?, b.u16(lig + 2)? as usize);
                    if count == 0 {
                        continue;
                    }
                    let Some(positions) =
                        self.match_sequence(buffer, lookup.flags, i, count - 1, |j, g| {
                            b.u16(lig + 4 + 2 * j) == Some(g)
                        })
                    else {
                        continue;
                    };
                    buffer[i].glyph = result;
                    for &p in positions.iter().rev() {
                        buffer.remove(p);
                    }
                    return Some(i + 1);
                }
                None
            }
            (5, _) | (6, _) => self.context(buffer, lookup, s, i, depth),
            _ => None,
        }
    }

    /// Context (5) and chained context (6) substitutions, formats 1 to 3:
    /// match a sequence (and in a chain, what comes before and after it),
    /// then run other lookups at places in it.
    fn context(
        &mut self,
        buffer: &mut Vec<Shaped>,
        lookup: &Lookup,
        s: usize,
        i: usize,
        depth: usize,
    ) -> Option<usize> {
        let b = self.b;
        let chained = lookup.kind == 6;
        let flags = lookup.flags;
        let glyph = buffer[i].glyph;
        // Each rule: backtrack, input (after the first), lookahead, and records.
        let (input, records) = match b.u16(s)? {
            // By glyph.
            1 => {
                let k = coverage(b, s + b.u16(s + 2)? as usize, glyph)?;
                (k < b.u16(s + 4)? as usize).then_some(())?;
                let set = s + b.u16(s + 6 + 2 * k)? as usize;
                self.first_rule(buffer, flags, i, set, chained, |at, g| b.u16(at) == Some(g))?
            }
            // By class.
            2 => {
                coverage(b, s + b.u16(s + 2)? as usize, glyph)?;
                let (back_classes, input_classes, ahead_classes, sets_at) = if chained {
                    let at = |o| b.u16(s + o).map(|v| s + v as usize);
                    (at(4)?, at(6)?, at(8)?, s + 10)
                } else {
                    let c = s + b.u16(s + 4)? as usize;
                    (c, c, c, s + 6)
                };
                let class = class_of(b, input_classes, glyph);
                (class < b.u16(sets_at)?).then_some(())?;
                let set_offset = b.u16(sets_at + 2 + 2 * class as usize)?;
                (set_offset != 0).then_some(())?;
                let set = s + set_offset as usize;
                let classes = [back_classes, input_classes, ahead_classes];
                self.first_rule_by_class(buffer, flags, i, set, chained, classes)?
            }
            // By coverage, one table per position. Chained: backtrack,
            // input and lookahead lists, then records; plain: glyph count,
            // record count, the input list, then records.
            3 => {
                let list = |at: usize| Some((b.u16(at)? as usize, at + 2));
                let covered = |(n, base): (usize, usize), j: usize, g: u16| {
                    j < n
                        && b.u16(base + 2 * j)
                            .and_then(|o| coverage(b, s + o as usize, g))
                            .is_some()
                };
                let (back, input_list, ahead, records) = if chained {
                    let back = list(s + 2)?;
                    let input_list = list(back.1 + 2 * back.0)?;
                    let ahead = list(input_list.1 + 2 * input_list.0)?;
                    let records_at = ahead.1 + 2 * ahead.0;
                    (
                        back,
                        input_list,
                        ahead,
                        (records_at + 2, b.u16(records_at)? as usize),
                    )
                } else {
                    let n = b.u16(s + 2)? as usize;
                    (
                        (0, 0),
                        (n, s + 6),
                        (0, 0),
                        (s + 6 + 2 * n, b.u16(s + 4)? as usize),
                    )
                };
                (input_list.0 > 0 && covered(input_list, 0, glyph)).then_some(())?;
                let input = self.match_sequence(buffer, flags, i, input_list.0 - 1, |j, g| {
                    covered(input_list, j + 1, g)
                })?;
                self.match_back(buffer, flags, i, back.0, |j, g| covered(back, j, g))?;
                let last = input.last().copied().unwrap_or(i);
                self.match_sequence(buffer, flags, last, ahead.0, |j, g| covered(ahead, j, g))?;
                (input, records)
            }
            _ => return None,
        };
        // Positions of the input sequence: `i` and the matched rest.
        let mut positions: Vec<usize> = std::iter::once(i).chain(input).collect();
        let end = positions.last().copied().unwrap_or(i) + 1;
        let before = buffer.len();
        if depth < MAX_NESTING {
            let (at, n) = records;
            for r in 0..n.min(64) {
                let (Some(sequence), Some(index)) = (b.u16(at + 4 * r), b.u16(at + 4 * r + 2))
                else {
                    break;
                };
                let Some(&p) = positions.get(sequence as usize) else {
                    continue;
                };
                self.work += NESTED_CALL;
                if self.work > self.budget {
                    break;
                }
                let Some(nested) = self.lookup(index) else {
                    continue;
                };
                let len = buffer.len();
                self.apply_at(buffer, nested, p, depth + 1);
                // Glyphs added or taken out move the positions after.
                let delta = buffer.len() as isize - len as isize;
                if delta != 0 {
                    for q in positions.iter_mut().filter(|q| **q > p) {
                        *q = (*q as isize + delta).max(p as isize) as usize;
                    }
                }
            }
        }
        Some((end as isize + buffer.len() as isize - before as isize).max(i as isize + 1) as usize)
    }

    /// Format 1 rules: the first whose glyph sequences match.
    #[allow(clippy::type_complexity)]
    fn first_rule(
        &mut self,
        buffer: &[Shaped],
        flags: u16,
        i: usize,
        set: usize,
        chained: bool,
        is: impl Fn(usize, u16) -> bool,
    ) -> Option<(Vec<usize>, (usize, usize))> {
        let b = self.b;
        for r in 0..b.u16(set)? as usize {
            self.work += 1;
            if self.work > self.budget {
                return None;
            }
            let rule = set + b.u16(set + 2 + 2 * r)? as usize;
            if let Some(m) = self.match_rule(buffer, flags, i, rule, chained, |_, at, g| is(at, g))
            {
                return Some(m);
            }
        }
        None
    }

    /// Format 2 rules: the first whose class sequences match.
    #[allow(clippy::type_complexity)]
    fn first_rule_by_class(
        &mut self,
        buffer: &[Shaped],
        flags: u16,
        i: usize,
        set: usize,
        chained: bool,
        classes: [usize; 3],
    ) -> Option<(Vec<usize>, (usize, usize))> {
        let b = self.b;
        for r in 0..b.u16(set)? as usize {
            self.work += 1;
            if self.work > self.budget {
                return None;
            }
            let rule = set + b.u16(set + 2 + 2 * r)? as usize;
            let matches =
                |part: usize, at: usize, g: u16| b.u16(at) == Some(class_of(b, classes[part], g));
            if let Some(m) = self.match_rule(buffer, flags, i, rule, chained, matches) {
                return Some(m);
            }
        }
        None
    }

    /// A rule of glyphs or classes (read by `is(part, offset, glyph)`, part
    /// 0 backtrack, 1 input, 2 lookahead): the input positions and records.
    #[allow(clippy::type_complexity)]
    fn match_rule(
        &mut self,
        buffer: &[Shaped],
        flags: u16,
        i: usize,
        rule: usize,
        chained: bool,
        is: impl Fn(usize, usize, u16) -> bool,
    ) -> Option<(Vec<usize>, (usize, usize))> {
        let b = self.b;
        if !chained {
            // Glyph count, record count, input (after the first), records.
            let (n, records) = (b.u16(rule)? as usize, b.u16(rule + 2)? as usize);
            (n > 0).then_some(())?;
            let input =
                self.match_sequence(buffer, flags, i, n - 1, |j, g| is(1, rule + 4 + 2 * j, g))?;
            return Some((input, (rule + 4 + 2 * (n - 1), records)));
        }
        let back = b.u16(rule)? as usize;
        self.match_back(buffer, flags, i, back, |j, g| is(0, rule + 2 + 2 * j, g))?;
        let at = rule + 2 + 2 * back;
        let n = b.u16(at)? as usize;
        (n > 0).then_some(())?;
        let input =
            self.match_sequence(buffer, flags, i, n - 1, |j, g| is(1, at + 2 + 2 * j, g))?;
        let at = at + 2 + 2 * (n - 1);
        let ahead = b.u16(at)? as usize;
        let last = input.last().copied().unwrap_or(i);
        self.match_sequence(buffer, flags, last, ahead, |j, g| is(2, at + 2 + 2 * j, g))?;
        let at = at + 2 + 2 * ahead;
        Some((input, (at + 2, b.u16(at)? as usize)))
    }

    /// `n` glyphs after `i` (skipping those the flags ignore) for which
    /// `is(j, glyph)` holds; their positions.
    fn match_sequence(
        &mut self,
        buffer: &[Shaped],
        flags: u16,
        i: usize,
        n: usize,
        is: impl Fn(usize, u16) -> bool,
    ) -> Option<Vec<usize>> {
        let mut positions = Vec::with_capacity(n);
        let mut p = i;
        for j in 0..n {
            p += 1;
            while p < buffer.len() && self.ignored(flags, buffer[p].glyph) {
                p += 1;
                self.work += 1;
            }
            self.work += 1;
            if self.work > self.budget || p >= buffer.len() || !is(j, buffer[p].glyph) {
                return None;
            }
            positions.push(p);
        }
        Some(positions)
    }

    /// The `n` glyphs before `i`, nearest first.
    fn match_back(
        &mut self,
        buffer: &[Shaped],
        flags: u16,
        i: usize,
        n: usize,
        is: impl Fn(usize, u16) -> bool,
    ) -> Option<()> {
        let mut p = i;
        for j in 0..n {
            loop {
                p = p.checked_sub(1)?;
                self.work += 1;
                if self.work > self.budget {
                    return None;
                }
                if !self.ignored(flags, buffer[p].glyph) {
                    break;
                }
            }
            if !is(j, buffer[p].glyph) {
                return None;
            }
        }
        Some(())
    }

    /// Whether lookup `flags` skip `glyph`: base glyphs, ligatures or marks
    /// by their GDEF class, or marks of another attachment class.
    fn ignored(&self, flags: u16, glyph: u16) -> bool {
        const IGNORE_BASE: u16 = 0x2;
        const IGNORE_LIGATURES: u16 = 0x4;
        const IGNORE_MARKS: u16 = 0x8;
        if flags & 0xFF0E == 0 {
            return false;
        }
        let Some(gdef) = self.gdef else { return false };
        let b = self.b;
        let class = b
            .u16(gdef + 4)
            .filter(|&o| o != 0)
            .map_or(0, |o| class_of(b, gdef + o as usize, glyph));
        match class {
            1 => flags & IGNORE_BASE != 0,
            2 => flags & IGNORE_LIGATURES != 0,
            3 => {
                if flags & IGNORE_MARKS != 0 {
                    return true;
                }
                let wanted = flags >> 8;
                wanted != 0
                    && b.u16(gdef + 10)
                        .filter(|&o| o != 0)
                        .map_or(0, |o| class_of(b, gdef + o as usize, glyph))
                        != wanted
            }
            _ => false,
        }
    }
}

/// A face's lookups, read once: each one's subtables, and the glyphs it
/// can start at (a bit a glyph) where that's known. Lookups that would
/// take too long to read are left out.
pub(crate) struct Prepared {
    lookups: Vec<Option<Lookup>>,
    starts: Vec<Option<Box<[u64]>>>,
}

fn has(set: &[u64], glyph: u16) -> bool {
    set.get(glyph as usize / 64)
        .is_some_and(|w| w >> (glyph % 64) & 1 != 0)
}

/// Reads GSUB's lookups for a font of `glyphs` glyphs, within `PREPARE_WORK`.
fn prepare(b: Bytes<'_>, gsub: usize, glyphs: u16) -> Prepared {
    let mut work = 0usize;
    let count = (|| {
        let list = gsub + b.u16(gsub + 8)? as usize;
        Some((list, b.u16(list)?))
    })();
    let Some((list, count)) = count else {
        return Prepared {
            lookups: Vec::new(),
            starts: Vec::new(),
        };
    };
    let mut lookups = Vec::with_capacity(count as usize);
    let mut starts = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        work += 1;
        let lookup = (work <= PREPARE_WORK)
            .then(|| read_lookup(b, list, index, &mut work))
            .flatten();
        let start = lookup.as_ref().and_then(|lookup| {
            // Each set counts too: 8 KB a lookup, and a font can have 65,535.
            let words = (glyphs as usize).div_ceil(64);
            work += words;
            if work > PREPARE_WORK {
                return None;
            }
            let mut set = vec![0u64; words].into_boxed_slice();
            for &s in &lookup.subtables {
                add_covered(
                    b,
                    first_coverage(b, lookup.kind, s)?,
                    &mut set,
                    glyphs,
                    &mut work,
                )?;
            }
            Some(set)
        });
        lookups.push(lookup.filter(|_| work <= PREPARE_WORK));
        starts.push(start.filter(|_| work <= PREPARE_WORK));
    }
    Prepared { lookups, starts }
}

/// Lookup `index` of the list at `list`, counting what reading it takes.
fn read_lookup(b: Bytes<'_>, list: usize, index: usize, work: &mut usize) -> Option<Lookup> {
    let at = list + b.u16(list + 2 + 2 * index)? as usize;
    let (mut kind, flags, count) = (b.u16(at)?, b.u16(at + 2)?, b.u16(at + 4)? as usize);
    if count > MAX_SUBTABLES {
        return None;
    }
    *work += count;
    let mut subtables: Vec<usize> = (0..count)
        .filter_map(|i| Some(at + b.u16(at + 6 + 2 * i)? as usize))
        .collect();
    if kind == 7 {
        // Extension: the real type, and 32-bit offsets.
        kind = subtables.first().and_then(|&s| b.u16(s + 2))?;
        subtables = subtables
            .iter()
            .filter_map(|&s| (b.u16(s + 2)? == kind).then(|| Some(s + b.u32(s + 4)? as usize))?)
            .collect();
    }
    Some(Lookup {
        kind,
        flags,
        subtables,
    })
}

/// The Coverage table of the first glyph a subtable matches.
fn first_coverage(b: Bytes<'_>, kind: u16, s: usize) -> Option<usize> {
    let offset = match (kind, b.u16(s)?) {
        (1..=4, _) | (5 | 6, 1 | 2) => b.u16(s + 2)?,
        // Context format 3: glyph count, record count, then coverages.
        (5, 3) => b.u16(s + 6)?,
        // Chained format 3: after the backtrack coverages, the input's.
        (6, 3) => {
            let back = b.u16(s + 2)? as usize;
            b.u16(s + 6 + 2 * back)?
        }
        _ => return None,
    };
    Some(s + offset as usize)
}

/// Sets the bits of the glyphs (fewer than `glyphs`) a Coverage table
/// covers, counting each in `work`; gives up past `PREPARE_WORK`.
fn add_covered(
    b: Bytes<'_>,
    at: usize,
    set: &mut [u64],
    glyphs: u16,
    work: &mut usize,
) -> Option<()> {
    let mut add = |g: u16| {
        if let Some(word) = set.get_mut(g as usize / 64) {
            *word |= 1 << (g % 64);
        }
    };
    let count = b.u16(at + 2)? as usize;
    *work += count;
    if *work > PREPARE_WORK {
        return None;
    }
    match b.u16(at)? {
        1 => (0..count).for_each(|i| b.u16(at + 4 + 2 * i).into_iter().for_each(&mut add)),
        2 => {
            for i in 0..count {
                let (start, end) = (
                    b.u16(at + 4 + 6 * i)?,
                    b.u16(at + 6 + 6 * i)?.min(glyphs.saturating_sub(1)),
                );
                *work += (end as usize + 1).saturating_sub(start as usize);
                if *work > PREPARE_WORK {
                    return None;
                }
                (start..=end).for_each(&mut add);
            }
        }
        _ => return None,
    }
    Some(())
}

/// `glyph`'s index in a Coverage table, if it's covered.
pub(crate) fn coverage(b: Bytes<'_>, at: usize, glyph: u16) -> Option<usize> {
    let count = b.u16(at + 2)? as usize;
    match b.u16(at)? {
        1 => {
            let (mut lo, mut hi) = (0, count);
            while lo < hi {
                let mid = (lo + hi) / 2;
                match b.u16(at + 4 + 2 * mid)?.cmp(&glyph) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                    std::cmp::Ordering::Equal => return Some(mid),
                }
            }
            None
        }
        2 => {
            let (mut lo, mut hi) = (0, count);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let range = at + 4 + 6 * mid;
                let (start, end) = (b.u16(range)?, b.u16(range + 2)?);
                if end < glyph {
                    lo = mid + 1;
                } else if start > glyph {
                    hi = mid;
                } else {
                    return Some(b.u16(range + 4)? as usize + (glyph - start) as usize);
                }
            }
            None
        }
        _ => None,
    }
}

/// `glyph`'s class in a ClassDef table: 0 when it isn't listed.
pub(crate) fn class_of(b: Bytes<'_>, at: usize, glyph: u16) -> u16 {
    (|| match b.u16(at)? {
        1 => {
            let (start, count) = (b.u16(at + 2)?, b.u16(at + 4)?);
            let i = glyph.checked_sub(start)?;
            (i < count).then(|| b.u16(at + 6 + 2 * i as usize))?
        }
        2 => {
            let count = b.u16(at + 2)? as usize;
            let (mut lo, mut hi) = (0, count);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let range = at + 4 + 6 * mid;
                let (start, end) = (b.u16(range)?, b.u16(range + 2)?);
                if end < glyph {
                    lo = mid + 1;
                } else if start > glyph {
                    hi = mid;
                } else {
                    return b.u16(range + 4);
                }
            }
            None
        }
        _ => None,
    })()
    .unwrap_or(0)
}
