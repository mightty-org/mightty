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
    /// The cell holds a grapheme cluster: read it with
    /// [`Term::cell_text`](crate::Term::cell_text), not from `c`.
    pub const GRAPHEME: Self = Self(1 << 11);

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
    /// Attributes only (`BOLD` ..= `STRIKE`), and in bits 12 to 14 the
    /// [`underline`](Self::underline) style; layout flags live on the cell.
    pub flags: Flags,
    /// OSC 8 hyperlink, 0 for none: look it up with
    /// [`Term::link`](crate::Term::link).
    pub link: u16,
    /// SGR 58; `Default` for the text's colour.
    pub underline_color: Color,
}

/// Where a style keeps its underline style, as `Underline as u16 - 1`:
/// in its flags, so a style stays 16 bytes.
const UNDERLINE_SHIFT: u16 = 12;
const UNDERLINE_BITS: u16 = 0b111 << UNDERLINE_SHIFT;

/// Underline styles, as SGR 4:1 to 4:5 number them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Underline {
    #[default]
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

impl Style {
    /// How `UNDERLINE` is drawn (SGR 4:n); `Single` when it's off.
    pub fn underline(self) -> Underline {
        match (self.flags.0 & UNDERLINE_BITS) >> UNDERLINE_SHIFT {
            1 => Underline::Double,
            2 => Underline::Curly,
            3 => Underline::Dotted,
            4 => Underline::Dashed,
            _ => Underline::Single,
        }
    }

    /// Turns the underline on in `underline`'s style, or off for `None`.
    pub fn set_underline(&mut self, underline: Option<Underline>) {
        self.flags.0 &= !(UNDERLINE_BITS | Flags::UNDERLINE.0);
        if let Some(underline) = underline {
            self.flags.0 |= Flags::UNDERLINE.0 | (underline as u16 - 1) << UNDERLINE_SHIFT;
        }
    }

    /// The flags a cell shows: without the underline style's bits.
    pub(crate) fn attributes(self) -> Flags {
        Flags(self.flags.0 & !UNDERLINE_BITS)
    }

    /// Blank cells erased under this style keep only its background (BCE).
    pub fn erased(self) -> Self {
        Self { bg: self.bg, ..Self::default() }
    }

    /// Whether [`key`](Self::key) tells this style from every other.
    pub(crate) fn key_is_exact(self) -> bool {
        self.link == 0 && self.underline_color == Color::Default
    }

    /// The style table's hash key: every field exactly but `link` and
    /// `underline_color`, which are only mixed in, so a key that isn't
    /// [exact](Self::key_is_exact) must be checked against the style itself.
    pub(crate) fn key(self) -> u64 {
        fn color(c: Color) -> u64 {
            match c {
                Color::Default => 0,
                Color::Indexed(n) => 1 << 24 | n as u64,
                Color::Rgb(r, g, b) => 2 << 24 | (r as u64) << 16 | (g as u64) << 8 | b as u64,
            }
        }
        let underline = ((self.flags.0 & UNDERLINE_BITS) >> UNDERLINE_SHIFT) as u64;
        let key = color(self.fg) | color(self.bg) << 26 | (self.flags.bits() as u64 & 0xff) << 52 | underline << 60;
        if self.key_is_exact() {
            return key;
        }
        let mixed = (self.link as u64) << 26 | color(self.underline_color);
        key ^ mixed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1 << 63
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
const LAYOUT_SHIFT: u32 = 28;
const STYLE_MASK: u32 = (1 << LAYOUT_SHIFT) - 1;
const LAYOUT_FLAGS: u16 = Flags::WIDE.0 | Flags::WIDE_SPACER.0 | Flags::WRAP_SPACER.0 | Flags::GRAPHEME.0;

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

    /// `flags` keeps only the layout bits (`WIDE`, `WIDE_SPACER`, `WRAP_SPACER`, `GRAPHEME`).
    #[inline]
    pub const fn new(c: char, style: StyleId, flags: Flags) -> Self {
        let layout = ((flags.0 & LAYOUT_FLAGS) >> 8) as u32;
        Cell { c, packed: layout << LAYOUT_SHIFT | style.0 }
    }

    #[inline]
    pub const fn style(self) -> StyleId {
        StyleId(self.packed & STYLE_MASK)
    }

    /// Layout flags: `WIDE`, `WIDE_SPACER`, `WRAP_SPACER`, `GRAPHEME`.
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
    /// OSC 8 hyperlink, 0 for none.
    pub link: u16,
    pub underline: Underline,
    /// `Default` for the text's colour.
    pub underline_color: Color,
}

impl StyledCell {
    pub const BLANK: StyledCell = StyledCell {
        c: ' ',
        fg: Color::Default,
        bg: Color::Default,
        flags: Flags::empty(),
        link: 0,
        underline: Underline::Single,
        underline_color: Color::Default,
    };
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
