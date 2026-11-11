//! Colours programs set and ask about, over the theme's: the palette
//! (OSC 4, reset by OSC 104), and the foreground, background and cursor
//! (OSC 10, 11, 12, reset by OSC 110, 111, 112). RIS resets them all.

use crate::{DEFAULT_BG, DEFAULT_FG, Term};

pub type Rgb = (u8, u8, u8);

/// The colours text is drawn in: the theme's, from
/// [`TermOptions`](crate::TermOptions), or as programs set them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Colors {
    pub palette: [Rgb; 256],
    pub foreground: Rgb,
    pub background: Rgb,
    /// `None`: the colour of the text under the cursor.
    pub cursor: Option<Rgb>,
}

impl Default for Colors {
    fn default() -> Self {
        Self { palette: std::array::from_fn(|i| default_palette(i as u8)), foreground: DEFAULT_FG, background: DEFAULT_BG, cursor: None }
    }
}

/// xterm 256-colour palette with a custom base 16.
pub fn default_palette(i: u8) -> Rgb {
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

/// Where programs' colours are kept, after the palette's 256.
pub(crate) const FOREGROUND: usize = 256;
const BACKGROUND: usize = 257;
const CURSOR: usize = 258;
/// Program-set colours by index; `None`: the theme's.
pub(crate) type SetColors = Box<[Option<Rgb>; 259]>;

/// An X11 colour spec, as xterm takes: `rgb:r/g/b` with 1–4 hex digits
/// each, or `#rgb` with 1–4 digits each, the most significant bits.
fn parse_color(spec: &[u8]) -> Option<Rgb> {
    let hex = |digits: &[u8]| -> Option<u32> {
        if !(1..=4).contains(&digits.len()) || !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
    };
    if let Some(rest) = spec.strip_prefix(b"rgb:") {
        let parts: Vec<&[u8]> = rest.split(|&b| b == b'/').collect();
        let [r, g, b] = parts[..] else { return None };
        // Scaled: f, ff, fff and ffff are all full.
        let scale = |digits: &[u8]| -> Option<u8> {
            let max = (1 << (4 * digits.len())) - 1;
            Some(((hex(digits)? * 255 + max / 2) / max) as u8)
        };
        return Some((scale(r)?, scale(g)?, scale(b)?));
    }
    let digits = spec.strip_prefix(b"#")?;
    let n = digits.len() / 3;
    if digits.len() % 3 != 0 || n == 0 {
        return None;
    }
    let top = |at: usize| -> Option<u8> {
        let v = hex(&digits[at * n..(at + 1) * n])?;
        Some(if n == 1 { v << 4 } else { v >> (4 * (n - 2)) } as u8)
    };
    Some((top(0)?, top(1)?, top(2)?))
}

impl Term {
    /// The colours to draw with: the theme's, with what programs set.
    pub fn colors(&self) -> Colors {
        let set = &self.set_colors;
        Colors {
            palette: std::array::from_fn(|i| set[i].unwrap_or(self.theme.palette[i])),
            foreground: set[FOREGROUND].unwrap_or(self.theme.foreground),
            background: set[BACKGROUND].unwrap_or(self.theme.background),
            cursor: set[CURSOR].or(self.theme.cursor),
        }
    }

    /// Colour `index` as reported: a cursor following the text reports the foreground.
    fn color(&self, index: usize) -> Rgb {
        let colors = self.colors();
        match index {
            FOREGROUND => colors.foreground,
            BACKGROUND => colors.background,
            CURSOR => colors.cursor.unwrap_or(colors.foreground),
            i => colors.palette[i],
        }
    }

    /// Sets colour `index` to `spec`, or reports it, after `prefix`, for `?`.
    fn set_or_report(&mut self, prefix: &str, index: usize, spec: &[u8], terminator: &[u8]) {
        if spec == b"?" {
            let (r, g, b) = self.color(index);
            self.respond(format!("\x1b]{prefix};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}").as_bytes());
            self.respond(terminator);
        } else if let Some(rgb) = parse_color(spec) {
            self.set_colors[index] = Some(rgb);
        }
    }

    /// OSC 4 ; index ; spec [; index ; spec …].
    pub(crate) fn osc_palette(&mut self, pairs: &[&[u8]], terminator: &[u8]) {
        for pair in pairs.chunks_exact(2) {
            let Some(i) = number(pair[0]).filter(|&i| i < 256) else { continue };
            self.set_or_report(&format!("4;{i}"), i, pair[1], terminator);
        }
    }

    /// OSC 10, 11 or 12 ; spec [; spec …]: as in xterm, each spec after the
    /// first goes to the next of foreground, background and cursor.
    pub(crate) fn osc_dynamic(&mut self, code: usize, specs: &[&[u8]], terminator: &[u8]) {
        for (code, spec) in (code..=12).zip(specs) {
            self.set_or_report(&code.to_string(), FOREGROUND + code - 10, spec, terminator);
        }
    }

    /// OSC 104 [; index …]: palette entries back to the theme's; all without any.
    pub(crate) fn osc_reset_palette(&mut self, indices: &[&[u8]]) {
        if indices.iter().all(|i| i.is_empty()) {
            self.set_colors[..256].fill(None);
        }
        for i in indices.iter().filter_map(|i| number(i)).filter(|&i| i < 256) {
            self.set_colors[i] = None;
        }
    }

    /// OSC 110, 111 or 112: the foreground, background or cursor back to the theme's.
    pub(crate) fn osc_reset_dynamic(&mut self, code: usize) {
        self.set_colors[FOREGROUND + code - 110] = None;
    }
}

fn number(digits: &[u8]) -> Option<usize> {
    std::str::from_utf8(digits).ok()?.parse().ok()
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

    #[test]
    fn color_specs() {
        assert_eq!(parse_color(b"rgb:ff/80/00"), Some((255, 128, 0)));
        assert_eq!(parse_color(b"rgb:f/8/0"), Some((255, 136, 0)));
        assert_eq!(parse_color(b"rgb:ffff/8080/0000"), Some((255, 128, 0)));
        assert_eq!(parse_color(b"#ff8000"), Some((255, 128, 0)));
        assert_eq!(parse_color(b"#f80"), Some((0xf0, 0x80, 0)));
        assert_eq!(parse_color(b"#fff888000"), Some((255, 0x88, 0)));
        assert_eq!(parse_color(b"#ffff88880000"), Some((255, 0x88, 0)));
        for bad in [&b"rgb:ff/80"[..], b"rgb:ff/80/00/00", b"rgb:fffff/0/0", b"rgb:gg/0/0", b"#ff80", b"#", b"red", b""] {
            assert_eq!(parse_color(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }
}
