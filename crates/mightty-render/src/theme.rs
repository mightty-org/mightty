use mightty_term::{Color, DEFAULT_BG, DEFAULT_FG};

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

/// xterm 256-colour palette with a custom base 16.
fn default_palette(i: u8) -> Rgb {
    const BASE: [u32; 16] = [
        0x3a3a3a, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xd8d8d8, //
        0x5c6370, 0xff7b86, 0xb5e890, 0xffd68a, 0x7cc4ff, 0xde9bff, 0x7fd6e0, 0xffffff,
    ];
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        0..=15 => {
            let c = BASE[i as usize];
            ((c >> 16) as u8, (c >> 8) as u8, c as u8)
        }
        16..=231 => {
            let i = i - 16;
            (LEVELS[(i / 36) as usize], LEVELS[(i / 6 % 6) as usize], LEVELS[(i % 6) as usize])
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_grays() {
        assert_eq!(default_palette(16), (0, 0, 0));
        assert_eq!(default_palette(196), (255, 0, 0));
        assert_eq!(default_palette(231), (255, 255, 255));
        assert_eq!(default_palette(232), (8, 8, 8));
        assert_eq!(default_palette(255), (238, 238, 238));
    }
}
