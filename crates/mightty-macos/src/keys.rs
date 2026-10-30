//! Key events → bytes for the PTY (xterm encoding).
//! TODO: kitty keyboard protocol, IME via NSTextInputClient.

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    /// Option, treated as Meta.
    pub alt: bool,
}

impl Mods {
    /// xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
}

/// Encodes a key press. `chars` is what the key typed; `base` is the same key
/// ignoring every modifier except Shift.
pub fn encode(key_code: u16, chars: &str, base: &str, mods: Mods, app_cursor: bool) -> Option<Vec<u8>> {
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
pub fn mouse_report(button: u8, action: MouseAction, col: usize, row: usize, mods: Mods, sgr: bool) -> Vec<u8> {
    let mut code = button + if mods.shift { 4 } else { 0 } + if mods.alt { 8 } else { 0 } + if mods.ctrl { 16 } else { 0 };
    if action == MouseAction::Motion {
        code += 32;
    }
    if sgr {
        let end = if action == MouseAction::Release { 'm' } else { 'M' };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: u16, chars: &str, mods: Mods) -> Vec<u8> {
        encode(code, chars, chars, mods, false).unwrap()
    }

    #[test]
    fn text_and_specials() {
        assert_eq!(key(0, "a", Mods::default()), b"a");
        assert_eq!(key(36, "\r", Mods::default()), b"\r");
        assert_eq!(key(51, "\x7f", Mods::default()), b"\x7f");
        assert_eq!(key(48, "\t", Mods { shift: true, ..Default::default() }), b"\x1b[Z");
    }

    #[test]
    fn control_and_meta() {
        let ctrl = Mods { ctrl: true, ..Default::default() };
        assert_eq!(key(8, "c", ctrl), [3]);
        assert_eq!(key(33, "[", ctrl), [0x1b]);
        let alt = Mods { alt: true, ..Default::default() };
        assert_eq!(encode(11, "∫", "b", alt, false).unwrap(), b"\x1bb");
    }

    #[test]
    fn cursor_keys() {
        assert_eq!(key(126, "", Mods::default()), b"\x1b[A");
        assert_eq!(encode(126, "", "", Mods::default(), true).unwrap(), b"\x1bOA");
        assert_eq!(key(123, "", Mods { ctrl: true, ..Default::default() }), b"\x1b[1;5D");
        assert_eq!(key(116, "", Mods::default()), b"\x1b[5~");
    }

    #[test]
    fn mouse_reports() {
        let none = Mods::default();
        assert_eq!(mouse_report(0, MouseAction::Press, 4, 9, none, true), b"\x1b[<0;5;10M");
        assert_eq!(mouse_report(0, MouseAction::Release, 4, 9, none, true), b"\x1b[<0;5;10m");
        assert_eq!(mouse_report(0, MouseAction::Motion, 0, 0, none, true), b"\x1b[<32;1;1M");
        assert_eq!(mouse_report(65, MouseAction::Press, 0, 0, Mods { ctrl: true, ..none }, true), b"\x1b[<81;1;1M");
        assert_eq!(mouse_report(0, MouseAction::Press, 0, 0, none, false), b"\x1b[M !!");
        assert_eq!(mouse_report(2, MouseAction::Release, 0, 0, none, false), b"\x1b[M#!!");
    }

    #[test]
    fn paste_modes() {
        assert_eq!(paste("a\nb", false), b"a\rb");
        assert_eq!(paste("x\x1b[201~y", true), b"\x1b[200~x[201~y\x1b[201~");
    }
}
