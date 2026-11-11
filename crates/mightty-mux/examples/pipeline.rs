//! PTY pipeline throughput without any UI: how close each stage gets to the
//! raw PTY ceiling. Median of 7 runs per workload.
//!
//! usage: cargo run --release -p mightty-mux --example pipeline -- bench/data/*.txt

use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mightty_mux::{Headless, Mux, SpawnOptions};
use mightty_pty::{Pty, WindowSize};
use mightty_term::Term;

const SIZE: WindowSize = WindowSize {
    cols: 120,
    rows: 40,
    pixel_width: 0,
    pixel_height: 0,
};

/// `cat file` in a PTY; `parse` decides what happens to each read.
fn direct(file: &str, mut parse: impl FnMut(&[u8])) -> Duration {
    let mut cmd = Command::new("cat");
    cmd.arg(file);
    let start = Instant::now();
    let mut pty = Pty::spawn(cmd, SIZE).unwrap();
    let mut reader = pty.reader().unwrap();
    let mut buf = vec![0; 64 * 1024];
    while let Ok(n) = reader.read(&mut buf) {
        if n == 0 {
            break;
        }
        parse(&buf[..n]);
    }
    let _ = pty.wait();
    start.elapsed()
}

fn mux(file: &str) -> Duration {
    let file = std::fs::canonicalize(file).unwrap();
    let file = file.display();
    let mux = Mux::new(Arc::new(Headless));
    let opts = SpawnOptions {
        command: Some(format!("exec cat '{file}'")),
        shell: Some("/bin/sh".into()),
        cols: 120,
        rows: 40,
        ..Default::default()
    };
    let start = Instant::now();
    let pane = mux.spawn(opts).unwrap();
    let mut seen = 0;
    while pane.is_alive() {
        seen = pane.wait_output(seen, Duration::from_millis(50));
    }
    start.elapsed()
}

fn median(mut f: impl FnMut() -> Duration) -> f64 {
    let mut runs: Vec<f64> = (0..7).map(|_| f().as_secs_f64()).collect();
    runs.sort_by(f64::total_cmp);
    runs[runs.len() / 2]
}

fn main() {
    println!(
        "{:14} {:>12} {:>14} {:>12}",
        "workload", "raw ceiling", "single thread", "mux"
    );
    for file in std::env::args().skip(1) {
        let mib = std::fs::metadata(&file).unwrap().len() as f64 / 1048576.0;
        let raw = median(|| direct(&file, |_| {}));
        let single = median(|| {
            let mut term = Term::new(120, 40);
            direct(&file, |bytes| term.feed(bytes))
        });
        let muxed = median(|| mux(&file));
        let name = Path::new(&file).file_stem().unwrap().to_string_lossy();
        println!(
            "{name:14} {:>8.0} MiB/s {:>10.0} MiB/s {:>8.0} MiB/s",
            mib / raw,
            mib / single,
            mib / muxed
        );
    }
}
