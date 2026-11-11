//! Standard base64 (RFC 4648), for OSC 52 clipboard text and kitty graphics.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Each byte's value in `ALPHABET`, or 64 for bytes outside it: images
/// arrive as megabytes of base64.
const VALUES: [u8; 256] = {
    let mut values = [64u8; 256];
    let mut i = 0;
    while i < 64 {
        values[ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    values
};

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Padding is optional; whitespace and any other character are an error.
pub fn decode(text: &[u8]) -> Option<Vec<u8>> {
    let text = text
        .strip_suffix(b"==")
        .or_else(|| text.strip_suffix(b"="))
        .unwrap_or(text);
    if text.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    for chunk in text.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let v = VALUES[c as usize];
            if v == 64 {
                return None;
            }
            n |= (v as u32) << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for text in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            "héllo ✓".as_bytes(),
        ] {
            assert_eq!(decode(encode(text).as_bytes()).as_deref(), Some(text));
        }
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(decode(b"Zm8"), Some(b"fo".to_vec()));
        assert_eq!(decode(b"Zm9v!"), None);
        assert_eq!(decode(b"Z"), None);
    }
}
