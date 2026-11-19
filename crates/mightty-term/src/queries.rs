//! Replies to the queries programs send to find out what the terminal
//! supports: XTGETTCAP (`DCS + q`), which asks for terminfo capabilities by
//! name, and DECRQSS (`DCS $ q`), which asks for a setting's current value.

use crate::cell::{Color, Flags, Style};

/// The capabilities XTGETTCAP answers: name, and value (`None` for a
/// boolean). The values are the bytes a program sends, so ESC is `\x1b`
/// where terminfo source would write `\E`. They describe what Mightty does
/// beyond the `xterm-256color` entry its programs get as TERM.
#[rustfmt::skip]
const CAPABILITIES: &[(&str, Option<&str>)] = &[
    ("TN", Some("xterm-256color")),
    ("name", Some("xterm-256color")),
    ("Co", Some("256")),
    ("colors", Some("256")),
    // Truecolour, by the two names programs look for.
    ("RGB", None),
    ("Tc", None),
    ("setrgbf", Some("\x1b[38;2;%p1%d;%p2%d;%p3%dm")),
    ("setrgbb", Some("\x1b[48;2;%p1%d;%p2%d;%p3%dm")),
    // Styled and coloured underlines.
    ("Su", None),
    ("Smulx", Some("\x1b[4:%p1%dm")),
    ("Setulc", Some("\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m")),
    // Cursor shape.
    ("Ss", Some("\x1b[%p1%d q")),
    ("Se", Some("\x1b[0 q")),
    // Strikethrough.
    ("smxx", Some("\x1b[9m")),
    ("rmxx", Some("\x1b[29m")),
    // Clipboard (OSC 52).
    ("Ms", Some("\x1b]52;%p1%s;%p2%s\x07")),
    // Synchronized output (mode 2026).
    ("Sync", Some("\x1b[?2026%?%p1%{1}%-%tl%eh%;")),
    // Bracketed paste.
    ("BE", Some("\x1b[?2004h")),
    ("BD", Some("\x1b[?2004l")),
    ("PS", Some("\x1b[200~")),
    ("PE", Some("\x1b[201~")),
];

/// The reply to `DCS + q names ST`: `names` is hex-encoded capability names
/// separated by `;`. Each name gets its own reply: `DCS 1 + r name=value
/// ST`, `DCS 1 + r name ST` for a boolean, or `DCS 0 + r name ST` when it
/// isn't one Mightty has, with names and values hex-encoded.
pub(crate) fn xtgettcap(names: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for hex_name in names.split(|&b| b == b';').filter(|n| !n.is_empty()) {
        let found = unhex(hex_name).and_then(|name| {
            CAPABILITIES
                .iter()
                .find(|(n, _)| n.as_bytes() == name.as_slice())
        });
        out.extend_from_slice(match found {
            Some(_) => b"\x1bP1+r",
            None => b"\x1bP0+r",
        });
        // Echoed as sent, since it's only hex digits when it was found.
        match found {
            Some((name, value)) => {
                out.extend(hex(name.as_bytes()));
                if let Some(value) = value {
                    out.push(b'=');
                    out.extend(hex(value.as_bytes()));
                }
            }
            None => out.extend(hex_name.iter().filter(|b| b.is_ascii_hexdigit())),
        }
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// The SGR parameters that set `style`'s attributes and colours, from a
/// reset: what DECRQSS reports for `m`. Colours use semicolons, the form
/// programs match when they check that truecolour stuck.
pub(crate) fn sgr(style: &Style) -> String {
    let mut parts = vec!["0".to_string()];
    let flags = style.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::BLINK, "5"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKE, "9"),
    ] {
        if flags.contains(flag) {
            parts.push(code.into());
        }
    }
    if flags.contains(Flags::UNDERLINE) {
        parts.push(match style.underline() as u8 {
            1 => "4".into(),
            n => format!("4:{n}"),
        });
    }
    let color = |c: Color, base: u8, bright: u8, extended: u8| match c {
        Color::Default => None,
        Color::Indexed(n) if n < 8 && base != 0 => Some(format!("{}", base + n)),
        Color::Indexed(n) if n < 16 && bright != 0 => Some(format!("{}", bright + n - 8)),
        Color::Indexed(n) => Some(format!("{extended};5;{n}")),
        Color::Rgb(r, g, b) => Some(format!("{extended};2;{r};{g};{b}")),
    };
    parts.extend(color(style.fg, 30, 90, 38));
    parts.extend(color(style.bg, 40, 100, 48));
    parts.extend(color(style.underline_color, 0, 0, 58));
    parts.join(";")
}

fn hex(bytes: &[u8]) -> impl Iterator<Item = u8> + '_ {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    bytes
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 15)]])
}

fn unhex(hex: &[u8]) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.chunks(2)
        .map(|pair| {
            let digit = |b: u8| (b as char).to_digit(16);
            Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
        })
        .collect()
}
