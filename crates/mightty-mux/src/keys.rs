/// Bytes for a named key, e.g. `enter`, `up`, `ctrl-c`, `alt-b`, `f5`.
/// Arrow keys follow application cursor mode when `app_cursor` is set.
pub fn key_bytes(name: &str, app_cursor: bool) -> Option<Vec<u8>> {
    let name = name.trim().to_ascii_lowercase();
    let arrow = |letter: u8| vec![0x1b, if app_cursor { b'O' } else { b'[' }, letter];
    let bytes = match name.as_str() {
        "enter" | "return" => b"\r".to_vec(),
        "tab" => b"\t".to_vec(),
        "shift-tab" | "backtab" => b"\x1b[Z".to_vec(),
        "escape" | "esc" => b"\x1b".to_vec(),
        "backspace" => b"\x7f".to_vec(),
        "space" => b" ".to_vec(),
        "up" => arrow(b'A'),
        "down" => arrow(b'B'),
        "right" => arrow(b'C'),
        "left" => arrow(b'D'),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "insert" => b"\x1b[2~".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        _ => {
            if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                let code = match n {
                    5 => 15,
                    6..=10 => n + 11,
                    11 | 12 => n + 12,
                    _ => return None,
                };
                return Some(format!("\x1b[{code}~").into_bytes());
            }
            let (prefix, key) = name.split_once(['-', '+'])?;
            let mut key_chars = key.chars();
            let (Some(c), None) = (key_chars.next(), key_chars.next()) else {
                return None;
            };
            match prefix {
                "ctrl" | "c" => vec![match c {
                    'a'..='z' => c as u8 - b'a' + 1,
                    '@' | ' ' => 0,
                    '[' => 0x1b,
                    '\\' => 0x1c,
                    ']' => 0x1d,
                    '^' => 0x1e,
                    '_' => 0x1f,
                    _ => return None,
                }],
                "alt" | "meta" | "m" => {
                    let mut bytes = vec![0x1b];
                    bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                    bytes
                }
                _ => return None,
            }
        }
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(key_bytes("Enter", false).unwrap(), b"\r");
        assert_eq!(key_bytes("ctrl-c", false).unwrap(), [3]);
        assert_eq!(key_bytes("ctrl+d", false).unwrap(), [4]);
        assert_eq!(key_bytes("alt-b", false).unwrap(), b"\x1bb");
        assert_eq!(key_bytes("up", false).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes("up", true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes("f5", false).unwrap(), b"\x1b[15~");
        assert_eq!(key_bytes("f12", false).unwrap(), b"\x1b[24~");
        assert!(key_bytes("hyper-x", false).is_none());
        assert!(key_bytes("ctrl-cc", false).is_none());
    }
}
