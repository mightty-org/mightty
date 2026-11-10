use crate::cell::{Row, Style, StyleId};

/// Slots in the recently-used style cache (a power of two).
const RECENT: usize = 4096;
const EMPTY: u64 = u64::MAX; // no style has this key (keys use 60 bits)

/// Interned styles. Cells hold a [`StyleId`] instead of colours inline,
/// which halves the cell size.
///
/// Lookup goes through a small direct-mapped cache of recent styles rather
/// than a full index: output with a fresh colour per cell (truecolor
/// gradients, images as half blocks) would otherwise grow an index to a
/// million entries and miss cache on every insert. A style that fell out
/// of the cache gets a second id. When the table grows, styles no cell
/// uses are dropped and the rest renumbered in place (see
/// `Term::collect_styles`); duplicates stay, bounded by the cells holding
/// them.
#[derive(Clone)]
pub struct Styles {
    list: Vec<Style>,
    recent: Box<[(u64, u32)]>,
}

impl Styles {
    pub(crate) fn new() -> Self {
        let mut styles = Self { list: Vec::new(), recent: vec![(EMPTY, 0); RECENT].into_boxed_slice() };
        styles.list.push(Style::default());
        styles
    }

    #[inline]
    pub fn get(&self, id: StyleId) -> &Style {
        &self.list[id.0 as usize]
    }

    /// Entries in the table, including duplicates and unused ones awaiting collection.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Entries the table has room for before it reallocates.
    pub fn capacity(&self) -> usize {
        self.list.capacity()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    #[inline]
    pub(crate) fn intern(&mut self, style: Style) -> StyleId {
        let key = style.key();
        if key == 0 {
            // Always id 0, so blank cells compare equal to `Cell::BLANK`.
            return StyleId::DEFAULT;
        }
        let slot = &mut self.recent[(key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 52) as usize % RECENT];
        // Some keys aren't exact (see `Style::key`); such styles are rare.
        if slot.0 != key || (!style.key_is_exact() && self.list[slot.1 as usize] != style) {
            *slot = (key, self.list.len() as u32);
            self.list.push(style);
        }
        StyleId(slot.1)
    }

    /// Whether any style in the table refers to hyperlink `n`, for each `n`.
    pub(crate) fn links(&self) -> Vec<bool> {
        let mut used = vec![false; u16::MAX as usize + 1];
        for style in &self.list {
            used[style.link as usize] = true;
        }
        used
    }

    /// Marks the styles `rows` use; the default style is always kept.
    pub(crate) fn live<'a>(&self, rows: impl Iterator<Item = &'a Row>) -> Live {
        let mut bits = vec![0u64; self.list.len().div_ceil(64)];
        bits[0] = 1;
        for row in rows {
            for cell in &row.cells {
                let id = cell.style().0 as usize;
                bits[id / 64] |= 1 << (id % 64);
            }
        }
        let mut ranks = Vec::with_capacity(bits.len());
        let mut n = 0;
        for word in &bits {
            ranks.push(n);
            n += word.count_ones();
        }
        Live { bits, ranks, len: n as usize }
    }

    /// Drops styles `live` doesn't hold and renumbers `rows` (the same rows
    /// `live` came from). Order is kept, so the table compacts in place
    /// without a second copy. Leaves room for at least `capacity` styles,
    /// so the table won't reallocate before it is next collected.
    pub(crate) fn compact<'a>(&mut self, live: &Live, rows: impl Iterator<Item = &'a mut Row>, capacity: usize) {
        let mut kept = 0;
        for id in 0..self.list.len() {
            if live.contains(id) {
                self.list[kept] = self.list[id];
                kept += 1;
            }
        }
        self.list.truncate(kept);
        for row in rows {
            for cell in &mut row.cells {
                let id = cell.style().0 as usize;
                if id != 0 {
                    cell.set_style(StyleId(live.rank(id)));
                }
            }
        }
        self.recent.fill((EMPTY, 0));
        // Reallocating copies the table, so only to grow, or to give back
        // more than half of it.
        if self.list.capacity() > 2 * capacity {
            self.list.shrink_to(capacity);
        }
        self.list.reserve_exact(capacity.saturating_sub(kept));
    }
}

/// Style ids in use: a bitmap, with a running count per word so an id's new
/// number (its rank among live ids) takes one popcount.
pub(crate) struct Live {
    bits: Vec<u64>,
    ranks: Vec<u32>,
    len: usize,
}

impl Live {
    /// Live styles, including the default.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    fn contains(&self, id: usize) -> bool {
        self.bits[id / 64] & (1 << (id % 64)) != 0
    }

    fn rank(&self, id: usize) -> u32 {
        let below = self.bits[id / 64] & ((1 << (id % 64)) - 1);
        self.ranks[id / 64] + below.count_ones()
    }
}
