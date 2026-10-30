//! How read size and throughput respond to pausing between PTY reads.
use std::io::Read;
use std::process::Command;
use std::time::{Duration, Instant};

use mightty_pty::{Pty, WindowSize};

fn main() {
    let file = std::env::args().nth(1).unwrap();
    let mib = std::fs::metadata(&file).unwrap().len() as f64 / 1048576.0;
    for pause_us in [0u64, 10, 25, 50, 100, 200] {
        let mut cmd = Command::new("cat");
        cmd.arg(&file);
        let start = Instant::now();
        let mut pty = Pty::spawn(cmd, WindowSize { cols: 120, rows: 40, ..Default::default() }).unwrap();
        let mut reader = pty.reader().unwrap();
        let mut buf = vec![0; 64 * 1024];
        let (mut reads, mut max) = (0u64, 0usize);
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            reads += 1;
            max = max.max(n);
            if pause_us > 0 {
                std::thread::sleep(Duration::from_micros(pause_us));
            }
        }
        let _ = pty.wait();
        let secs = start.elapsed().as_secs_f64();
        println!("pause {pause_us:>3}us: {:6.0} MiB/s, {reads:>6} reads, avg {:>5} B, max {max} B", mib / secs, (mib * 1048576.0 / reads as f64) as u64);
    }
}
