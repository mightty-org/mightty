use std::ops::BitOr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags(pub(crate) u16);

impl Flags {
    pub const BOLD: Self = Self(1 << 0);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINE: Self = Self(1 << 3);
    pub const BLINK: Self = Self(1 << 4);
    pub const INVERSE: Self = Self(1 << 5);
    pub const HIDDEN: Self = Self(1 << 6);
    pub const STRIKE: Self = Self(1 << 7);
    /// First cell of a double-width character.
    pub const WIDE: Self = Self(1 << 8);
    /// Second cell of a double-width character; holds no glyph.
    pub const WIDE_SPACER: Self = Self(1 << 9);
    /// Blank left in the last column when a wide character wrapped to the next row.
    pub const WRAP_SPACER: Self = Self(1 << 10);

    pub const fn empty() -> Self {
        Self(0)
    }

    /// Raw bits, for FFI. Stable: the C header mirrors these values.
    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl BitOr for Flags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Colours and attributes shared by many cells, stored once per terminal in a
/// [`Styles`](crate::Styles) table; cells refer to it by [`StyleId`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// Attributes only (`BOLD` ..= `STRIKE`); layout flags live on the cell.
    pub flags: Flags,
}

impl Style {
    /// Blank cells erased under this style keep only its background (BCE).
    pub fn erased(self) -> Self {
        Self { bg: self.bg, ..Self::default() }
    }

    /// All fields in one integer: the style table's hash key.
    pub(crate) fn key(self) -> u64 {
        fn color(c: Color) -> u64 {
            match c {
                Color::Default => 0,
                Color::Indexed(n) => 1 << 24 | n as u64,
                Color::Rgb(r, g, b) => 2 << 24 | (r as u64) << 16 | (g as u64) << 8 | b as u64,
            }
        }
        color(self.fg) | color(self.bg) << 26 | (self.flags.bits() as u64 & 0xff) << 52
    }
}

/// Index into a terminal's style table. `StyleId::DEFAULT` is always the
/// default style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StyleId(pub(crate) u32);

impl StyleId {
    pub const DEFAULT: Self = Self(0);
}

/// Layout flags packed into a cell's top bits, above the style id.
const LAYOUT_SHIFT: u32 = 29;
const STYLE_MASK: u32 = (1 << LAYOUT_SHIFT) - 1;
const LAYOUT_FLAGS: u16 = Flags::WIDE.0 | Flags::WIDE_SPACER.0 | Flags::WRAP_SPACER.0;

/// One screen cell: 8 bytes, a character and a packed `(layout flags, style id)`.
/// Colours and attributes are looked up with [`Term::style`](crate::Term::style),
/// or all at once with [`Term::resolve`](crate::Term::resolve).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub c: char,
    packed: u32,
}

impl Cell {
    /// An empty cell with the default style.
    pub const BLANK: Cell = Cell { c: ' ', packed: 0 };

    /// `flags` keeps only the layout bits (`WIDE`, `WIDE_SPACER`, `WRAP_SPACER`).
    #[inline]
    pub const fn new(c: char, style: StyleId, flags: Flags) -> Self {
        let layout = ((flags.0 & LAYOUT_FLAGS) >> 8) as u32;
        Cell { c, packed: layout << LAYOUT_SHIFT | style.0 }
    }

    #[inline]
    pub const fn style(self) -> StyleId {
        StyleId(self.packed & STYLE_MASK)
    }

    /// Layout flags: `WIDE`, `WIDE_SPACER`, `WRAP_SPACER`.
    #[inline]
    pub const fn flags(self) -> Flags {
        Flags(((self.packed >> LAYOUT_SHIFT) as u16) << 8)
    }

    pub(crate) fn set_style(&mut self, style: StyleId) {
        self.packed = self.packed & !STYLE_MASK | style.0;
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::BLANK
    }
}

impl std::fmt::Debug for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cell({:?}, {:?}, {:?})", self.c, self.style(), self.flags())
    }
}

/// A cell with its style looked up: what renderers and the C API consume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyledCell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    /// Attributes and layout flags together.
    pub flags: Flags,
}

impl StyledCell {
    pub const BLANK: StyledCell = StyledCell { c: ' ', fg: Color::Default, bg: Color::Default, flags: Flags::empty() };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Cells from column 0. A row may be shorter than the screen is wide:
    /// rows stay short until written, and missing cells are `Cell::BLANK`.
    pub cells: Vec<Cell>,
    /// The line continues on the next row because of autowrap (not a hard newline).
    pub wrapped: bool,
}

impl Row {
    pub fn new(cols: usize, blank: Cell) -> Self {
        Self { cells: vec![blank; cols], wrapped: false }
    }

    /// The cell at `col`, blank past the row's end.
    pub fn cell(&self, col: usize) -> &Cell {
        self.cells.get(col).unwrap_or(&Cell::BLANK)
    }

    /// No text and no attributes anywhere in the row.
    pub fn is_blank(&self) -> bool {
        !self.wrapped && self.cells.iter().all(|c| *c == Cell::default())
    }
}
