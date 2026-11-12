//! A fingerprint of everything a `Term` shows for some output, to check a
//! change to the core changes nothing a caller can see: run it before and
//! after, and compare. Each file is fed at several sizes and in several
//! chunkings, with a resize halfway, and so is a stream of generated
//! sequences of every kind the terminal knows.
//!
//! usage: cargo run --release -p mightty-term --example digest -- bench/data/*.txt

use std::fmt::{self, Write};
use std::path::Path;

use mightty_term::{Cell, Term};

/// FNV-1a, fed through `write!` so nothing is allocated per cell.
struct Hash(u64);

impl Write for Hash {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.bytes(s.as_bytes());
        Ok(())
    }
}

impl Hash {
    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }

    /// What the terminal handed out since the last call.
    fn events(&mut self, term: &mut Term) {
        self.bytes(&term.take_responses());
        let _ = write!(
            self,
            "{:?}{:?}{:?}{:?}",
            term.take_bell(),
            term.take_notifications(),
            term.take_command_events(),
            term.take_clipboard_requests(),
        );
    }

    /// Everything that can be read from `term`.
    fn state(&mut self, term: &Term) {
        let _ = write!(
            self,
            "{:?}{:?}{:?}{:?}{:?}{:?}{:?}{}{}{}{}",
            term.cursor(),
            term.modes(),
            term.title(),
            term.cwd(),
            term.cwd_host(),
            term.marks(),
            term.command_blocks(),
            term.is_alt_screen(),
            term.key_flags(),
            term.bells(),
            term.lines_scrolled(),
        );
        // History too on the alternate screen, where `Term::line` hides it.
        for row in term.scrollback().iter().chain(term.lines()) {
            // Cells never written read as blank: a short row is a padded one.
            let used = row
                .cells
                .iter()
                .rposition(|&c| c != Cell::BLANK)
                .map_or(0, |i| i + 1);
            let _ = write!(self, "\n{}", row.wrapped);
            for &cell in &row.cells[..used] {
                let styled = term.resolve(cell);
                let _ = write!(
                    self,
                    "{styled:?}{:?}{:?}",
                    term.cell_text(cell),
                    term.link(styled.link)
                );
            }
        }
        for p in term.image_placements() {
            let _ = write!(
                self,
                "{}{}{:?}{:?}{:?}{:?}{}{}{}",
                p.line,
                p.col,
                p.source,
                p.offset,
                p.size,
                p.cells,
                p.z,
                p.image.width,
                p.image.height
            );
            self.bytes(&p.image.pixels);
        }
        let _ = write!(self, "{}{}", term.screen_text(), term.history_text().len());
    }
}

/// `data` fed to a `cols`×`rows` terminal `chunk` bytes at a time, resized
/// halfway: its state then and at the end, and all it handed out.
fn run(data: &[u8], cols: usize, rows: usize, chunk: usize) -> u64 {
    let mut hash = Hash(0xcbf2_9ce4_8422_2325);
    let mut term = Term::new(cols, rows);
    let half = data.len() / 2 / chunk * chunk;
    for (i, piece) in data.chunks(chunk).enumerate() {
        if i * chunk == half {
            hash.state(&term);
            term.resize(cols - 13, rows + 5);
        }
        term.feed(piece);
        hash.events(&mut term);
        // As the app does when output pauses: nothing it shows may change.
        if i % 1000 == 999 {
            term.trim_history();
        }
    }
    hash.state(&term);
    hash.0
}

/// A stream of sequences of every kind, with text between, from `seed`.
fn generated(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    const TEXT: &[&str] = &[
        "hello world ",
        "ls -la\r\n",
        "中文字符",
        "e\u{301}",
        "👨‍👩‍👧‍👦",
        "🇻🇳",
        "❤️",
        "\t",
        "\r\n",
        "\r",
        "\n",
        "\x08",
        "a\u{200d}b",
        "ｆｕｌｌ",
        "x",
        "The quick brown fox ",
        "\u{1f600}",
        "é",
        "\x07",
        "\x0e",
        "\x0f",
    ];
    const FINALS: &[u8] = b"@ABCDEFGHIJKLMPSTXZ`abcdefghlmnpqrstu";
    const PRIVATE: &[u16] = &[
        1, 6, 7, 12, 25, 47, 1000, 1002, 1004, 1006, 1047, 1049, 2004, 2026, 2027,
    ];
    let mut out = Vec::with_capacity(len + 64);
    while out.len() < len {
        match next(20) {
            0..=7 => out.extend(TEXT[next(TEXT.len() as u64) as usize].as_bytes()),
            8..=10 => {
                // CSI with a few parameters.
                out.extend(b"\x1b[");
                for i in 0..next(4) {
                    if i > 0 {
                        out.push(b';');
                    }
                    out.extend(next(60).to_string().as_bytes());
                }
                out.push(FINALS[next(FINALS.len() as u64) as usize]);
            }
            11..=12 => {
                // SGR, colours of every form.
                let sgr = match next(6) {
                    0 => format!("\x1b[{}m", next(110)),
                    1 => format!("\x1b[38;5;{}m", next(256)),
                    2 => format!("\x1b[48;2;{};{};{}m", next(256), next(256), next(256)),
                    3 => format!("\x1b[4:{}m", next(6)),
                    4 => format!("\x1b[58:2::{}:{}:{}m", next(256), next(256), next(256)),
                    _ => "\x1b[0m".to_owned(),
                };
                out.extend(sgr.as_bytes());
            }
            13 => {
                let mode = PRIVATE[next(PRIVATE.len() as u64) as usize];
                let set = if next(2) == 0 { 'h' } else { 'l' };
                out.extend(format!("\x1b[?{mode}{set}").as_bytes());
            }
            14 => {
                let osc = match next(8) {
                    0 => format!("\x1b]0;title {}\x07", next(99)),
                    1 => format!("\x1b]8;;https://example.com/{}\x1b\\", next(9)),
                    2 => "\x1b]8;;\x1b\\".to_owned(),
                    3 => "\x1b]133;A\x07$ \x1b]133;B\x07".to_owned(),
                    4 => format!("\x1b]133;C\x07out\r\n\x1b]133;D;{}\x07", next(3)),
                    5 => format!("\x1b]7;file://host/tmp/d{}\x07", next(9)),
                    6 => format!("\x1b]9;note {}\x07\x1b]4;{};?\x07", next(9), next(16)),
                    _ => format!("\x1b]11;?\x07\x1b]52;c;aGk=\x07\x1b]{};x\x07", next(120)),
                };
                out.extend(osc.as_bytes());
            }
            15 => {
                let esc: &[u8] = match next(10) {
                    0 => b"\x1b7",
                    1 => b"\x1b8",
                    2 => b"\x1bM",
                    3 => b"\x1bD",
                    4 => b"\x1bE",
                    5 => b"\x1b(0",
                    6 => b"\x1b(B",
                    7 => b"\x1b[6n\x1b[c\x1b[>c",
                    8 => b"\x1b[?u\x1b[>1u\x1b[<u",
                    _ => b"\x1bH",
                };
                out.extend(esc);
            }
            16 => {
                // Kitty graphics: a 1×1 image, placed, sometimes deleted.
                let g = match next(4) {
                    0 => format!("\x1b_Ga=T,f=32,s=1,v=1,i={};/wAA/w==\x1b\\", 1 + next(5)),
                    1 => format!("\x1b_Ga=p,i={},c={},r=1\x1b\\", 1 + next(5), 1 + next(4)),
                    2 => format!("\x1b_Ga=d,d=i,i={}\x1b\\", 1 + next(5)),
                    _ => "\x1bPq#0;2;0;0;0\x1b\\".to_owned(),
                };
                out.extend(g.as_bytes());
            }
            17 => {
                // A region, and scrolling in it.
                let top = 1 + next(10);
                out.extend(format!("\x1b[{};{}r", top, top + 1 + next(20)).as_bytes());
            }
            18 if next(40) == 0 => out.extend(b"\x1bc"),
            // Bytes of any kind, damaged UTF-8 among them.
            _ => out.extend((0..next(6)).map(|_| next(256) as u8)),
        }
    }
    out
}

fn main() {
    let mut inputs: Vec<(String, Vec<u8>)> = std::env::args()
        .skip(1)
        .map(|path| {
            let name = Path::new(&path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read(&path).expect("read input"))
        })
        .collect();
    for seed in 1..=4u64 {
        inputs.push((
            format!("generated-{seed}"),
            generated(seed * 0x9e37_79b9_7f4a_7c15, 4 << 20),
        ));
    }
    for (name, data) in inputs {
        let mut all = Hash(0);
        for (cols, rows) in [(80, 24), (120, 40), (211, 57)] {
            // A byte at a time over the start only: every split of every
            // sequence, without taking minutes.
            for (chunk, limit) in [
                (1, 1 << 18),
                (7, 1 << 20),
                (1021, usize::MAX),
                (1 << 16, usize::MAX),
            ] {
                let data = &data[..data.len().min(limit)];
                let _ = write!(all, "{}", run(data, cols, rows, chunk));
            }
        }
        println!("{name:20} {:016x}", all.0);
    }
}
