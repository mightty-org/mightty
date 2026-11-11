//! Byte-level VT parser after Paul Williams' DEC ANSI state machine
//! (<https://vt100.net/emu/dec_ansi_parser>), extended with UTF-8 decoding in
//! the ground state, colon sub-parameters, and APC capture (kitty graphics).
//!
//! 8-bit C1 controls are not recognised: in a UTF-8 stream those bytes belong
//! to multi-byte characters.

mod params;
mod utf8;

pub use params::{MAX_PARAMS, Params};

const MAX_INTERMEDIATES: usize = 2;
const MAX_OSC_PARAMS: usize = 16;
/// Large enough for OSC 52 clipboard payloads.
const MAX_STRING_LEN: usize = 1 << 20;

/// Receives parsed actions. Every method defaults to a no-op.
#[allow(unused_variables)]
pub trait Perform {
    /// A printable character.
    fn print(&mut self, c: char) {}
    /// A run of printable characters with no controls in between. The
    /// parser uses this for plain text, which is most of what programs print;
    /// override it to handle runs in bulk.
    fn print_str(&mut self, s: &str) {
        for c in s.chars() {
            self.print(c);
        }
    }
    /// Like `print_str`, for text known to be printable ASCII (0x20..=0x7E).
    fn print_ascii(&mut self, s: &str) {
        self.print_str(s);
    }
    /// Like `print_str`, for text that needed UTF-8 decoding anyway.
    fn print_chars(&mut self, chars: &[char]) {
        for &c in chars {
            self.print(c);
        }
    }
    /// A C0 control byte.
    fn execute(&mut self, byte: u8) {}
    /// `ESC [ params intermediates action`. `ignore` is set when limits overflowed.
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {}
    /// `ESC intermediates byte`.
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {}
    /// `ESC ] p1 ; p2 ; ... ST`, split on `;`.
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {}
    /// Start of a DCS string; data follows through `put` until `unhook`.
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8) {}
    fn put(&mut self, byte: u8) {}
    fn unhook(&mut self) {}
    /// `ESC _ data ST`.
    fn apc_dispatch(&mut self, data: &[u8]) {}
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmString,
    ApcString,
}

#[derive(Clone, Debug, Default)]
pub struct Parser {
    state: State,
    params: Params,
    intermediates: [u8; MAX_INTERMEDIATES],
    n_intermediates: usize,
    ignoring: bool,
    string: Vec<u8>,
    utf8: utf8::Decoder,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance<P: Perform>(&mut self, p: &mut P, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            // Fast path: hand runs of text over in bulk instead of stepping
            // the state machine byte by byte. ASCII goes as `&str`; anything
            // else is validated and decoded in one pass.
            if self.state == State::Ground && !self.utf8.pending() {
                let rest = &bytes[i..];
                let ascii = printable_ascii_len(rest);
                if ascii > 0 {
                    // SAFETY: printable ASCII is valid UTF-8.
                    p.print_ascii(unsafe { std::str::from_utf8_unchecked(&rest[..ascii]) });
                    i += ascii;
                    continue;
                }
                if rest[0] >= 0x80 {
                    let used = decode_run(p, rest);
                    if used > 0 {
                        i += used;
                        continue;
                    }
                }
            }
            // Fast path: a complete CSI sequence parsed in one tight loop.
            if self.state == State::Ground
                && !self.utf8.pending()
                && bytes[i] == 0x1B
                && bytes.get(i + 1) == Some(&b'[')
                && let Some(len) = self.csi_fast(p, &bytes[i + 2..])
            {
                i += 2 + len;
                continue;
            }
            self.byte(p, bytes[i]);
            i += 1;
        }
    }

    /// Parses `params intermediates final` of a CSI sequence directly from
    /// `rest` (the bytes after `ESC [`) and dispatches it. Returns its length,
    /// or `None` for anything unusual (C0 controls inside, malformed or
    /// incomplete), which the state machine then handles byte by byte.
    fn csi_fast<P: Perform>(&mut self, p: &mut P, rest: &[u8]) -> Option<usize> {
        self.params.clear();
        self.n_intermediates = 0;
        self.ignoring = false;
        let mut in_intermediates = false;
        for (j, &b) in rest.iter().enumerate() {
            match b {
                b'0'..=b';' if !in_intermediates => self.params.push_byte(b),
                0x3C..=0x3F if j == 0 => self.collect(b),
                0x20..=0x2F => {
                    in_intermediates = true;
                    self.collect(b);
                }
                0x40..=0x7E => {
                    self.params.finish();
                    p.csi_dispatch(
                        &self.params,
                        &self.intermediates[..self.n_intermediates],
                        self.ignoring,
                        b,
                    );
                    return Some(j + 1);
                }
                _ => return None,
            }
        }
        None
    }

    fn byte<P: Perform>(&mut self, p: &mut P, b: u8) {
        if self.state == State::Ground {
            return self.ground(p, b);
        }

        // Transitions valid from any state.
        match b {
            0x18 | 0x1A => {
                self.leave(p, false);
                p.execute(b);
                self.state = State::Ground;
                return;
            }
            0x1B => {
                self.leave(p, true);
                self.enter_escape();
                return;
            }
            _ => {}
        }

        let c0 = matches!(b, 0x00..=0x17 | 0x19 | 0x1C..=0x1F);
        match self.state {
            State::Ground => unreachable!(),
            State::Escape => match b {
                _ if c0 => p.execute(b),
                0x20..=0x2F => {
                    self.collect(b);
                    self.state = State::EscapeIntermediate;
                }
                b'P' => self.enter(State::DcsEntry),
                b'[' => self.enter(State::CsiEntry),
                b']' => self.enter(State::OscString),
                b'_' => self.enter(State::ApcString),
                b'X' | b'^' => self.state = State::SosPmString,
                0x30..=0x7E => self.esc_dispatch(p, b),
                _ => {}
            },
            State::EscapeIntermediate => match b {
                _ if c0 => p.execute(b),
                0x20..=0x2F => self.collect(b),
                0x30..=0x7E => self.esc_dispatch(p, b),
                _ => {}
            },
            State::CsiEntry => match b {
                _ if c0 => p.execute(b),
                0x20..=0x2F => {
                    self.collect(b);
                    self.state = State::CsiIntermediate;
                }
                0x30..=0x3B => {
                    self.params.push_byte(b);
                    self.state = State::CsiParam;
                }
                0x3C..=0x3F => {
                    self.collect(b);
                    self.state = State::CsiParam;
                }
                0x40..=0x7E => self.csi_dispatch(p, b),
                _ => {}
            },
            State::CsiParam => match b {
                _ if c0 => p.execute(b),
                0x30..=0x3B => self.params.push_byte(b),
                0x3C..=0x3F => self.state = State::CsiIgnore,
                0x20..=0x2F => {
                    self.collect(b);
                    self.state = State::CsiIntermediate;
                }
                0x40..=0x7E => self.csi_dispatch(p, b),
                _ => {}
            },
            State::CsiIntermediate => match b {
                _ if c0 => p.execute(b),
                0x20..=0x2F => self.collect(b),
                0x30..=0x3F => self.state = State::CsiIgnore,
                0x40..=0x7E => self.csi_dispatch(p, b),
                _ => {}
            },
            State::CsiIgnore => match b {
                _ if c0 => p.execute(b),
                0x40..=0x7E => self.state = State::Ground,
                _ => {}
            },
            State::DcsEntry => match b {
                0x20..=0x2F => {
                    self.collect(b);
                    self.state = State::DcsIntermediate;
                }
                0x30..=0x3B => {
                    self.params.push_byte(b);
                    self.state = State::DcsParam;
                }
                0x3C..=0x3F => {
                    self.collect(b);
                    self.state = State::DcsParam;
                }
                0x40..=0x7E => self.hook(p, b),
                _ => {}
            },
            State::DcsParam => match b {
                0x30..=0x3B => self.params.push_byte(b),
                0x3C..=0x3F => self.state = State::DcsIgnore,
                0x20..=0x2F => {
                    self.collect(b);
                    self.state = State::DcsIntermediate;
                }
                0x40..=0x7E => self.hook(p, b),
                _ => {}
            },
            State::DcsIntermediate => match b {
                0x20..=0x2F => self.collect(b),
                0x30..=0x3F => self.state = State::DcsIgnore,
                0x40..=0x7E => self.hook(p, b),
                _ => {}
            },
            State::DcsPassthrough => {
                if b != 0x7F {
                    p.put(b);
                }
            }
            State::OscString => match b {
                0x07 => {
                    self.osc_dispatch(p, true);
                    self.state = State::Ground;
                }
                0x00..=0x1F => {}
                _ => self.push_string(b),
            },
            State::ApcString => match b {
                0x07 => {
                    p.apc_dispatch(&self.string);
                    self.state = State::Ground;
                }
                0x00..=0x1F => {}
                _ => self.push_string(b),
            },
            State::DcsIgnore | State::SosPmString => {}
        }
    }

    fn ground<P: Perform>(&mut self, p: &mut P, b: u8) {
        if self.utf8.pending() {
            if b & 0xC0 == 0x80 {
                if let Some(c) = self.utf8.advance(b) {
                    p.print(c);
                }
                return;
            }
            self.utf8.reset();
            p.print(utf8::REPLACEMENT);
        }
        match b {
            0x1B => self.enter_escape(),
            0x00..=0x1F => p.execute(b),
            0x20..=0x7E => p.print(b as char),
            0x7F => {}
            _ => {
                if let Some(c) = self.utf8.start(b) {
                    p.print(c);
                }
            }
        }
    }

    /// Exit actions for string states. `terminated` is false on CAN/SUB.
    fn leave<P: Perform>(&mut self, p: &mut P, terminated: bool) {
        match self.state {
            State::DcsPassthrough => p.unhook(),
            State::OscString if terminated => self.osc_dispatch(p, false),
            State::ApcString if terminated => p.apc_dispatch(&self.string),
            _ => {}
        }
    }

    fn enter_escape(&mut self) {
        self.n_intermediates = 0;
        self.ignoring = false;
        self.state = State::Escape;
    }

    fn enter(&mut self, state: State) {
        self.params.clear();
        self.string.clear();
        self.state = state;
    }

    fn collect(&mut self, b: u8) {
        if self.n_intermediates < MAX_INTERMEDIATES {
            self.intermediates[self.n_intermediates] = b;
            self.n_intermediates += 1;
        } else {
            self.ignoring = true;
        }
    }

    fn push_string(&mut self, b: u8) {
        if self.string.len() < MAX_STRING_LEN {
            self.string.push(b);
        }
    }

    fn esc_dispatch<P: Perform>(&mut self, p: &mut P, b: u8) {
        p.esc_dispatch(
            &self.intermediates[..self.n_intermediates],
            self.ignoring,
            b,
        );
        self.state = State::Ground;
    }

    fn csi_dispatch<P: Perform>(&mut self, p: &mut P, b: u8) {
        self.params.finish();
        p.csi_dispatch(
            &self.params,
            &self.intermediates[..self.n_intermediates],
            self.ignoring,
            b,
        );
        self.state = State::Ground;
    }

    fn hook<P: Perform>(&mut self, p: &mut P, b: u8) {
        self.params.finish();
        p.hook(
            &self.params,
            &self.intermediates[..self.n_intermediates],
            self.ignoring,
            b,
        );
        self.state = State::DcsPassthrough;
    }

    fn osc_dispatch<P: Perform>(&mut self, p: &mut P, bell_terminated: bool) {
        let mut parts: [&[u8]; MAX_OSC_PARAMS] = [&[]; MAX_OSC_PARAMS];
        let mut n = 0;
        let mut rest = &self.string[..];
        loop {
            // The last slot swallows the remainder, `;` included (URIs, base64).
            let split = if n == MAX_OSC_PARAMS - 1 {
                None
            } else {
                rest.iter().position(|&c| c == b';')
            };
            match split {
                Some(i) => {
                    parts[n] = &rest[..i];
                    rest = &rest[i + 1..];
                    n += 1;
                }
                None => {
                    parts[n] = rest;
                    n += 1;
                    break;
                }
            }
        }
        p.osc_dispatch(&parts[..n], bell_terminated);
    }
}

/// Length of the run of printable ASCII (0x20..=0x7E) at the start of
/// `bytes`, checked a 64-bit word at a time.
fn printable_ascii_len(bytes: &[u8]) -> usize {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let mut i = 0;
    while let Some(chunk) = bytes.get(i..i + 8) {
        let w = u64::from_le_bytes(chunk.try_into().unwrap_or_default());
        // A high bit marks (at least) the first byte < 0x20 or >= 0x7F;
        // borrows and carries only spill into later bytes.
        let below_space = w.wrapping_sub(ONES * 0x20) & !w;
        let at_or_above_del = w.wrapping_add(ONES) | w;
        if (below_space | at_or_above_del) & HIGH != 0 {
            break;
        }
        i += 8;
    }
    i + bytes[i..]
        .iter()
        .position(|&b| !(0x20..0x7F).contains(&b))
        .unwrap_or(bytes.len() - i)
}

/// Decodes valid UTF-8 from the start of `text` and prints it in batches.
/// Stops at the first invalid or incomplete sequence, which the byte-wise
/// decoder then handles (replacement characters, sequences split across
/// reads). Returns the number of bytes consumed.
/// Bit `i` set when `window[i]` starts a character, i.e. is not a UTF-8
/// continuation byte (`0b10xx_xxxx`). Eight bytes at a time, no branches.
#[inline]
fn lead_bytes(window: &[u8]) -> u64 {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    const LOW: u64 = 0x7F7F_7F7F_7F7F_7F7F;
    let mut mask = 0;
    for (k, chunk) in window.chunks_exact(8).take(8).enumerate() {
        let x = u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        let t = (x & 0xC0C0_C0C0_C0C0_C0C0) ^ HIGH; // zero bytes are continuations
        let nonzero = (((t & LOW) + LOW) | t) & HIGH;
        // Gather each byte's flag into one bit per byte.
        mask |= ((nonzero >> 7).wrapping_mul(0x0102_0408_1020_4080) >> 56) << (8 * k);
    }
    mask
}

/// Decodes the UTF-8 sequence at the top of `w` (four bytes, big-endian)
/// without branching on its length: CJK and emoji text mixes 1-, 2-, 3- and
/// 4-byte sequences unpredictably, and a mispredicted branch per character
/// costs more than the whole decode. `None` for controls, DEL and anything
/// malformed (overlong, surrogate, truncated, past U+10FFFF); the careful
/// decoder handles those.
#[inline]
fn decode4(w: u32) -> Option<(u32, usize)> {
    const LEAD_MASK: [u32; 5] = [0, 0x7F, 0x1F, 0x0F, 0x07];
    const CONT_MASK: [u32; 5] = [0, 0, 0x00C0_0000, 0x00C0_C000, 0x00C0_C0C0];
    const CONT_BITS: [u32; 5] = [0, 0, 0x0080_0000, 0x0080_8000, 0x0080_8080];
    // Smallest code point each length may encode; 0x20 also rejects C0 controls.
    const MIN: [u32; 5] = [u32::MAX, 0x20, 0x80, 0x800, 0x1_0000];
    // Sequence length from the first byte's leading ones, kept to arithmetic:
    // the next character's load waits on it. A stray continuation byte (one
    // leading one) or 0xF8.. gets length 0, which the checks below reject.
    let ones = ((w >> 24) as u8).leading_ones() as usize;
    let len = (ones + (ones == 0) as usize) * (ones != 1 && ones <= 4) as usize;
    let cp = (((w >> 24) & LEAD_MASK[len]) << 18
        | (w >> 16 & 0x3F) << 12
        | (w >> 8 & 0x3F) << 6
        | (w & 0x3F))
        >> (6 * (4 - len as u32));
    let ok = (w & CONT_MASK[len] == CONT_BITS[len])
        & (cp >= MIN[len])
        & (cp <= 0x10_FFFF)
        & ((cp ^ 0xD800) >= 0x800)
        & (cp != 0x7F);
    ok.then_some((cp, len))
}

fn decode_run<P: Perform>(p: &mut P, text: &[u8]) -> usize {
    let mut chars = ['\0'; 256];
    let mut n = 0;
    let mut j = 0;
    let cont = |b: u8| b & 0xC0 == 0x80;
    let mut push = |p: &mut P, c: char| {
        chars[n] = c;
        n += 1;
        if n == chars.len() {
            p.print_chars(&chars);
            n = 0;
        }
    };
    while j < text.len() {
        // Fast path, a 64-byte window at a time. Where characters start is
        // found for the whole window up front, so stepping to the next one
        // is a shift and a count of zeros instead of waiting for this one
        // to decode; decoding then runs off that critical path. Anything
        // decode4 rejects, or a length that disagrees with where the next
        // character starts, drops to the careful decoder below.
        if let Some(window) = text.get(j..j + 64) {
            let leads = lead_bytes(window);
            let mut at = 0;
            while at < 60 {
                let next = at + 1 + (leads >> (at + 1)).trailing_zeros() as usize;
                let w = u32::from_be_bytes([
                    window[at],
                    window[at + 1],
                    window[at + 2],
                    window[at + 3],
                ]);
                match decode4(w) {
                    Some((cp, len)) if at + len == next => {
                        // SAFETY: decode4 only accepts Unicode scalar values.
                        push(p, unsafe { char::from_u32_unchecked(cp) });
                        at = next;
                    }
                    _ => break,
                }
            }
            j += at;
            if at >= 60 {
                continue;
            }
        }
        let Some(&b0) = text.get(j) else { break };
        let fast = text
            .get(j..j + 4)
            .and_then(|b| decode4(u32::from_be_bytes([b[0], b[1], b[2], b[3]])));
        let (cp, len) = if let Some(decoded) = fast {
            decoded
        } else if b0 < 0x80 {
            if b0 < 0x20 || b0 == 0x7F {
                break; // controls go through the state machine
            }
            (u32::from(b0), 1)
        } else if b0 < 0xE0 {
            match text.get(j + 1) {
                Some(&b1) if b0 >= 0xC2 && cont(b1) => {
                    ((u32::from(b0 & 0x1F) << 6) | u32::from(b1 & 0x3F), 2)
                }
                _ => break,
            }
        } else if b0 < 0xF0 {
            match text.get(j + 1..j + 3) {
                Some(&[b1, b2]) if cont(b1) && cont(b2) => {
                    let cp = (u32::from(b0 & 0x0F) << 12)
                        | (u32::from(b1 & 0x3F) << 6)
                        | u32::from(b2 & 0x3F);
                    if cp < 0x800 || (0xD800..0xE000).contains(&cp) {
                        break;
                    }
                    (cp, 3)
                }
                _ => break,
            }
        } else if b0 <= 0xF4 {
            match text.get(j + 1..j + 4) {
                Some(&[b1, b2, b3]) if cont(b1) && cont(b2) && cont(b3) => {
                    let cp = (u32::from(b0 & 0x07) << 18)
                        | (u32::from(b1 & 0x3F) << 12)
                        | (u32::from(b2 & 0x3F) << 6)
                        | u32::from(b3 & 0x3F);
                    if !(0x1_0000..=0x10_FFFF).contains(&cp) {
                        break;
                    }
                    (cp, 4)
                }
                _ => break,
            }
        } else {
            break;
        };
        // SAFETY: overlong forms, surrogates and values past U+10FFFF were
        // rejected above, so `cp` is a Unicode scalar value.
        push(p, unsafe { char::from_u32_unchecked(cp) });
        j += len;
    }
    if n > 0 {
        p.print_chars(&chars[..n]);
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode4_matches_std() {
        let interesting = [
            0x00, 0x1F, 0x41, 0x7F, 0x80, 0x8F, 0x90, 0x9F, 0xA0, 0xBF, 0xC0, 0xF4, 0xFF,
        ];
        for b0 in 0..=255u8 {
            for b1 in interesting {
                for b2 in interesting {
                    for b3 in interesting {
                        let bytes = [b0, b1, b2, b3];
                        let expected = bytes
                            .utf8_chunks()
                            .next()
                            .and_then(|chunk| chunk.valid().chars().next());
                        let expected = expected
                            .filter(|c| !c.is_ascii_control())
                            .map(|c| (c as u32, c.len_utf8()));
                        assert_eq!(decode4(u32::from_be_bytes(bytes)), expected, "{bytes:02x?}");
                    }
                }
            }
        }
        for c in ['é', '中', '😀', '\u{10FFFF}', '\u{FFFD}'] {
            let mut bytes = [b'a'; 4];
            c.encode_utf8(&mut bytes);
            assert_eq!(
                decode4(u32::from_be_bytes(bytes)),
                Some((c as u32, c.len_utf8()))
            );
        }
    }

    #[derive(Default)]
    struct Log(Vec<String>);

    impl Perform for Log {
        fn print(&mut self, c: char) {
            self.0.push(format!("print {c}"));
        }
        fn execute(&mut self, b: u8) {
            self.0.push(format!("exec {b:#04x}"));
        }
        fn csi_dispatch(&mut self, params: &Params, inter: &[u8], ignore: bool, action: u8) {
            let groups: Vec<_> = params.iter().map(|g| format!("{g:?}")).collect();
            let inter = String::from_utf8_lossy(inter);
            self.0.push(format!(
                "csi {inter}{} [{}] {ignore}",
                action as char,
                groups.join(",")
            ));
        }
        fn esc_dispatch(&mut self, inter: &[u8], _: bool, b: u8) {
            self.0.push(format!(
                "esc {}{}",
                String::from_utf8_lossy(inter),
                b as char
            ));
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
            let parts: Vec<_> = params
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            self.0.push(format!("osc {} {bell}", parts.join("|")));
        }
        fn hook(&mut self, _: &Params, _: &[u8], _: bool, action: u8) {
            self.0.push(format!("hook {}", action as char));
        }
        fn put(&mut self, b: u8) {
            self.0.push(format!("put {}", b as char));
        }
        fn unhook(&mut self) {
            self.0.push("unhook".into());
        }
        fn apc_dispatch(&mut self, data: &[u8]) {
            self.0
                .push(format!("apc {}", String::from_utf8_lossy(data)));
        }
    }

    fn parse(input: &[u8]) -> Vec<String> {
        let mut log = Log::default();
        Parser::new().advance(&mut log, input);
        log.0
    }

    #[test]
    fn prints_utf8_and_controls() {
        assert_eq!(
            parse("a\u{e9}\r\n\u{1F600}".as_bytes()),
            [
                "print a",
                "print \u{e9}",
                "exec 0x0d",
                "exec 0x0a",
                "print \u{1F600}"
            ]
        );
    }

    #[test]
    fn utf8_split_across_chunks() {
        let mut log = Log::default();
        let mut parser = Parser::new();
        let bytes = "ab😀c".as_bytes();
        parser.advance(&mut log, &bytes[..4]);
        parser.advance(&mut log, &bytes[4..]);
        assert_eq!(log.0, ["print a", "print b", "print 😀", "print c"]);
    }

    #[test]
    fn invalid_utf8_inside_text_runs() {
        // Overlong, surrogate and truncated sequences between valid text.
        assert_eq!(
            parse(b"a\xc0\xafb"),
            ["print a", "print \u{FFFD}", "print \u{FFFD}", "print b"]
        );
        assert_eq!(parse(b"\xed\xa0\x80x"), ["print \u{FFFD}", "print x"]);
        assert_eq!(
            parse("é中😀".as_bytes()),
            ["print é", "print 中", "print 😀"]
        );
    }

    #[test]
    fn bulk_decode_matches_bytewise() {
        // Long runs take the windowed fast path; feeding one byte at a time
        // takes the state machine. Both must see the same characters.
        let pieces: [&[u8]; 12] = [
            b"a",
            "é".as_bytes(),
            "中".as_bytes(),
            "😀".as_bytes(),
            b"\n",
            b"\x80",
            b"\xe4\xb8",
            b"\x1b[1m",
            b"\xc0\xaf",
            b"\xf4\x90\x80\x80",
            b"\x7f",
            "\u{85}".as_bytes(),
        ];
        let mut seed = 7u32;
        for _ in 0..300 {
            let mut input = Vec::new();
            for _ in 0..120 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                // Mostly valid text, so the fast path gets long stretches.
                let k = (seed >> 16) as usize % 40;
                input.extend_from_slice(
                    pieces[if k < 36 {
                        k % 4
                    } else {
                        4 + (seed >> 8) as usize % 8
                    }],
                );
            }
            let mut bytewise = Log::default();
            let mut parser = Parser::new();
            for b in &input {
                parser.advance(&mut bytewise, std::slice::from_ref(b));
            }
            assert_eq!(parse(&input), bytewise.0, "{input:02x?}");
        }
    }

    #[test]
    fn printable_ascii_scan() {
        for len in 0..40 {
            for bad in [0x00u8, 0x1F, 0x7F, 0x80, 0xFF] {
                let mut bytes = vec![b'a'; len];
                bytes.push(bad);
                bytes.extend_from_slice(b"tail");
                assert_eq!(printable_ascii_len(&bytes), len, "len {len} bad {bad:#x}");
            }
            assert_eq!(printable_ascii_len(&vec![b'~'; len]), len);
        }
        assert_eq!(printable_ascii_len(b" !~"), 3);
    }

    #[test]
    fn invalid_utf8_becomes_replacement() {
        assert_eq!(
            parse(b"\xff\xc3a"),
            ["print \u{FFFD}", "print \u{FFFD}", "print a"]
        );
    }

    #[test]
    fn csi_params_and_subparams() {
        assert_eq!(parse(b"\x1b[1;2H"), ["csi H [[1],[2]] false"]);
        assert_eq!(parse(b"\x1b[m"), ["csi m [] false"]);
        assert_eq!(parse(b"\x1b[;5H"), ["csi H [[0],[5]] false"]);
        assert_eq!(
            parse(b"\x1b[38:2::255:0:10m"),
            ["csi m [[38, 2, 0, 255, 0, 10]] false"]
        );
        assert_eq!(parse(b"\x1b[?1049h"), ["csi ?h [[1049]] false"]);
        assert_eq!(parse(b"\x1b[2 q"), ["csi  q [[2]] false"]);
    }

    #[test]
    fn c0_inside_csi_executes() {
        assert_eq!(
            parse(b"\x1b[1\n;2H"),
            ["exec 0x0a", "csi H [[1],[2]] false"]
        );
    }

    #[test]
    fn osc_with_bel_and_st() {
        assert_eq!(parse(b"\x1b]0;hi;there\x07"), ["osc 0|hi|there true"]);
        assert_eq!(
            parse(b"\x1b]133;D;0\x1b\\"),
            ["osc 133|D|0 false", "esc \\"]
        );
    }

    #[test]
    fn escape_and_charset() {
        assert_eq!(parse(b"\x1b7\x1b(0"), ["esc 7", "esc (0"]);
    }

    #[test]
    fn dcs_and_apc() {
        assert_eq!(
            parse(b"\x1bPqab\x1b\\"),
            ["hook q", "put a", "put b", "unhook", "esc \\"]
        );
        assert_eq!(
            parse(b"\x1b_Gf=24;AAAA\x1b\\"),
            ["apc Gf=24;AAAA", "esc \\"]
        );
    }

    #[test]
    fn can_aborts_sequence() {
        assert_eq!(parse(b"\x1b[12\x18x"), ["exec 0x18", "print x"]);
    }
}
