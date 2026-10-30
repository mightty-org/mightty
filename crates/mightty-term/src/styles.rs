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
/// of the cache gets a second id; the table is rebuilt from the cells
/// still alive when it grows (see `Term::collect_styles`), which merges
/// such duplicates and drops styles no cell uses.
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
        if slot.0 != key {
            *slot = (key, self.list.len() as u32);
            self.list.push(style);
        }
        StyleId(slot.1)
    }

    /// Rebuilds the table from the styles `rows` use, renumbering their
    /// cells: drops unused styles and merges duplicates.
    pub(crate) fn collect<'a>(&mut self, rows: impl Iterator<Item = &'a mut Row>) {
        let old = std::mem::replace(self, Self::new());
        let mut remap = vec![u32::MAX; old.list.len()];
        remap[0] = 0;
        for row in rows {
            for cell in &mut row.cells {
                let id = cell.style().0 as usize;
                if id != 0 {
                    if remap[id] == u32::MAX {
                        remap[id] = self.intern(old.list[id]).0;
                    }
                    cell.set_style(StyleId(remap[id]));
                }
            }
        }
    }
}
