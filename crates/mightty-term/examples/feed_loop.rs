//! Feeds one workload repeatedly for `secs` seconds, for profilers.
//! usage: feed_loop FILE SECS
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read workload");
    let until = Instant::now() + Duration::from_secs(args[2].parse().unwrap_or(5));
    let mut term = mightty_term::Term::new(120, 40);
    while Instant::now() < until {
        for chunk in data.chunks(64 * 1024) {
            term.feed(chunk);
        }
    }
}
