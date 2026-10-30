pub(crate) const REPLACEMENT: char = '\u{FFFD}';

/// Incremental UTF-8 decoder. Invalid input yields U+FFFD.
#[derive(Clone, Debug, Default)]
pub(crate) struct Decoder {
    cp: u32,
    need: u8,
    min: u32,
}

impl Decoder {
    pub fn pending(&self) -> bool {
        self.need > 0
    }

    pub fn reset(&mut self) {
        self.need = 0;
    }

    /// Feeds a lead byte (>= 0x80). Returns a char only for invalid leads.
    pub fn start(&mut self, b: u8) -> Option<char> {
        let (need, init, min) = match b {
            0xC2..=0xDF => (1, b & 0x1F, 0x80),
            0xE0..=0xEF => (2, b & 0x0F, 0x800),
            0xF0..=0xF4 => (3, b & 0x07, 0x1_0000),
            _ => return Some(REPLACEMENT),
        };
        self.need = need;
        self.cp = u32::from(init);
        self.min = min;
        None
    }

    /// Feeds a continuation byte (`0b10xx_xxxx`).
    pub fn advance(&mut self, b: u8) -> Option<char> {
        self.cp = (self.cp << 6) | u32::from(b & 0x3F);
        self.need -= 1;
        if self.need > 0 {
            return None;
        }
        let c = if self.cp < self.min { None } else { char::from_u32(self.cp) };
        Some(c.unwrap_or(REPLACEMENT))
    }
}
