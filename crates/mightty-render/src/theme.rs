use mightty_term::{Color, DEFAULT_BG, DEFAULT_FG, default_palette};

pub type Rgb = (u8, u8, u8);

/// Colours the front end draws with. Field names match the settings that
/// set them (`cursor-color` → `cursor`), which are Ghostty's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub foreground: Rgb,
    pub background: Rgb,
    /// `None`: the colour of the text under the cursor.
    pub cursor: Option<Rgb>,
    /// Text under a block cursor; `None`: the cell's background.
    pub cursor_text: Option<Rgb>,
    pub selection_background: Rgb,
    /// `None`: selected text keeps its colour.
    pub selection_foreground: Option<Rgb>,
    /// Search matches, and the selected one (the match the search is on).
    pub search_foreground: Rgb,
    pub search_background: Rgb,
    pub search_selected_foreground: Rgb,
    pub search_selected_background: Rgb,
    pub palette: [Rgb; 256],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            foreground: DEFAULT_FG,
            background: DEFAULT_BG,
            cursor: None,
            cursor_text: None,
            selection_background: (0x3e, 0x44, 0x51),
            selection_foreground: None,
            search_foreground: (0x1e, 0x1e, 0x1e),
            search_background: (0xe5, 0xc0, 0x7b),
            search_selected_foreground: (0x1e, 0x1e, 0x1e),
            search_selected_background: (0xff, 0x8c, 0x32),
            palette: std::array::from_fn(|i| default_palette(i as u8)),
        }
    }
}

impl Theme {
    /// `color` as drawn, with `default` standing in for [`Color::Default`].
    pub fn resolve(&self, color: Color, default: Rgb) -> Rgb {
        match color {
            Color::Default => default,
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(i) => self.palette[i as usize],
        }
    }
}
