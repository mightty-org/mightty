//! The sandboxed process the app decodes inline images in.

#![cfg(target_os = "macos")]

use std::path::Path;

use mightty_term::DecodeLimits;

/// A 2×1 GIF: a red pixel, then a blue one.
const GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x02, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xFF, 0x00, 0x00,
    0x00, 0x00, 0xFF, 0x2C, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44,
    0x0A, 0x00, 0x3B,
];

fn decode(data: &[u8], max_side: u32, max_bytes: usize) -> Option<(u32, u32, Vec<u8>)> {
    let limits = DecodeLimits {
        max_side,
        max_bytes,
        shown_side: 100,
    };
    mightty_macos::decode_image_in(Path::new(env!("CARGO_BIN_EXE_mightty")), data, limits).unwrap()
}

#[test]
fn decodes_in_its_own_process() {
    let (w, h, pixels) = decode(GIF, 100, 100).unwrap();
    assert_eq!((w, h), (2, 1));
    assert_eq!(pixels, [255, 0, 0, 255, 0, 0, 255, 255]);
    // Too large, or not an image: no answer.
    assert!(decode(GIF, 100, 7).is_none());
    assert!(decode(b"hello", 100, 100).is_none());
    assert!(decode(&[], 100, 100).is_none());
}
