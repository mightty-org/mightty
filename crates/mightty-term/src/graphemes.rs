use std::collections::HashMap;

use crate::cell::{Cell, Flags, Row};

/// Cells holding a grapheme cluster (a character plus combining marks,
/// emoji modifiers, joined emoji, a flag's second letter) store an id in
/// place of the character, as `FIRST + id`, with [`Flags::GRAPHEME`] set.
/// The id moves with the cell through scrolling, insertion and reflow.
const FIRST: u32 = 0x10_0000;
/// Ids available: U+100000..=U+10FFFF.
const IDS: usize = 0x1_0000;

/// Interned clusters, so repeated ones share an id. Ids no cell uses are
/// reclaimed when the table runs out.
#[derive(Clone, Default)]
pub(crate) struct Graphemes {
    texts: Vec<Box<str>>,
    ids: HashMap<Box<str>, u32>,
    free: Vec<u32>,
}

impl Graphemes {
    /// The cluster in `cell`, if it holds one.
    pub(crate) fn get(&self, cell: Cell) -> Option<&str> {
        if !cell.flags().contains(Flags::GRAPHEME) {
            return None;
        }
        self.texts.get((cell.c as u32 - FIRST) as usize).map(|s| &**s)
    }

    /// The character `cell` stands for: its own, or its cluster's first.
    pub(crate) fn base(&self, cell: Cell) -> char {
        self.get(cell).and_then(|s| s.chars().next()).unwrap_or(cell.c)
    }

    /// The stand-in character for `text`, or `None` once every id is in use.
    pub(crate) fn intern(&mut self, text: &str) -> Option<char> {
        let id = match self.ids.get(text) {
            Some(&id) => id,
            None => {
                let id = match self.free.pop() {
                    Some(id) => {
                        self.texts[id as usize] = text.into();
                        id
                    }
                    None if self.texts.len() < IDS => {
                        self.texts.push(text.into());
                        self.texts.len() as u32 - 1
                    }
                    None => return None,
                };
                self.ids.insert(text.into(), id);
                id
            }
        };
        char::from_u32(FIRST + id)
    }

    pub(crate) fn is_full(&self) -> bool {
        self.free.is_empty() && self.texts.len() == IDS
    }

    /// Frees the ids no cell in `rows` uses.
    pub(crate) fn collect<'a>(&mut self, rows: impl Iterator<Item = &'a Row>) {
        let mut live = vec![false; self.texts.len()];
        for row in rows {
            for cell in &row.cells {
                if cell.flags().contains(Flags::GRAPHEME) {
                    live[(cell.c as u32 - FIRST) as usize] = true;
                }
            }
        }
        self.free.clear();
        for (id, live) in live.into_iter().enumerate() {
            if !live {
                let text = std::mem::take(&mut self.texts[id]);
                self.ids.remove(&text);
                self.free.push(id as u32);
            }
        }
    }
}
