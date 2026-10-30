//! Core throughput: feeds workload files into a `Term` in 64 KiB chunks, with
//! no PTY or rendering involved. Best of 5 runs.
//!
//! usage: cargo run --release -p mightty-term --example feed -- bench/data/*.txt

use std::path::Path;
use std::time::Instant;

use mightty_term::Term;

fn main() {
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).expect("read workload");
        let best = (0..5)
            .map(|_| {
                let mut term = Term::new(120, 40);
                let start = Instant::now();
                for chunk in data.chunks(64 * 1024) {
                    term.feed(chunk);
                }
                start.elapsed().as_secs_f64()
            })
            .fold(f64::MAX, f64::min);
        let name = Path::new(&path).file_stem().unwrap().to_string_lossy();
        println!("{name:14} {:8.1} MiB/s", data.len() as f64 / 1048576.0 / best);
    }
}
