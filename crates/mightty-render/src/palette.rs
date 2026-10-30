/// Background of selected cells. TODO: move into a theme.
pub const SELECTION_BG: (u8, u8, u8) = (0x3e, 0x44, 0x51);

/// xterm 256-colour palette with a custom base 16.
pub fn palette(i: u8) -> (u8, u8, u8) {
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
        assert_eq!(palette(16), (0, 0, 0));
        assert_eq!(palette(196), (255, 0, 0));
        assert_eq!(palette(231), (255, 255, 255));
        assert_eq!(palette(232), (8, 8, 8));
        assert_eq!(palette(255), (238, 238, 238));
    }
}
