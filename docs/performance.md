# Performance

How Mightty compares with Ghostty and kitty, how each figure is measured,
and what was done to get there. The same results, with less detail, are on
[mightty.org](https://mightty.org/docs/performance/).

There are two benchmarks, and they measure differently: `bench/suite.py`
launches every terminal fresh, and `bench/compare.sh` runs Mightty as a pane
of a copy that is already open. Quote figures from one of them, not both.

Throughput benchmark modelled on vtebench: each terminal `cat`s six 16 MiB
workloads at 120×40, Menlo 14, timed from inside the terminal. Terminals are
interleaved round by round (5 rounds × 3 runs) so they share system
conditions; medians on an M2, macOS 15:

| workload | mightty | Ghostty 1.4.0 | kitty 0.49.2 | vs Ghostty | vs kitty |
|---|---|---|---|---|---|
| ascii | 237 MiB/s | 168 | 101 | **1.41×** | **2.35×** |
| truecolor | 202 | 156 | 101 | **1.30×** | **2.00×** |
| unicode | 166 | 151 | 89 | **1.10×** | **1.87×** |
| cursor_motion | 186 | 171 | 95 | **1.09×** | **1.95×** |
| scroll_region | 257 | 239 | 114 | **1.08×** | **2.25×** |
| long_lines | 257 | 280 | 132 | 0.92×* | **1.95×** |

\* Not a real difference: pooled over 63 runs, mightty and Ghostty have
medians of 258 and 263 MiB/s (ratio 0.98, 95% CI 0.89–1.08). Both sit at the
macOS PTY ceiling, where single runs range from 150 to 320 MiB/s.

Ghostty and kitty are launched fresh for each round with their shipped
defaults apart from grid and font (kitty with `--config NONE`). mightty is
not: `bench/compare.sh` runs it as a pane of the mightty already open
(`mightty ctl spawn`), with that instance's settings, so these numbers don't
compare like for like. The full suite below launches all three fresh; there
plain ASCII comes out at 239, 213 and 103 MiB/s. Single runs swing ±10% on a
busy machine; long_lines and scroll_region sit at the PTY ceiling for both
terminals and trade places between runs.

## Full suite

`bench/suite.py` launches each terminal fresh (120×40, Menlo 14, otherwise
shipped defaults), runs `bench/probe.py` inside it, and samples the
terminal's own process every 50 ms with `proc_pid_rusage`. Terminals take
turns round by round: 10 startups and 3 sessions each, M2, macOS 15.

- **Footprint** is macOS's physical footprint (Activity Monitor's "Memory"),
  the nearest thing to Linux's PSS, which macOS doesn't have.
- **Startup** is launch until the terminal's command runs in a ready PTY.
- **Response latency** is a cursor-position query's round trip, measured
  from inside the terminal. Under flood, a separate `cat` loop writes as fast
  as the terminal reads; the kernel caps the backlog ahead of each query at
  a few KiB, so this measures how responsive a busy terminal stays.
- **Memory after output** counts all output since launch. Scrollback
  defaults differ (mightty 10k lines, Ghostty 10 MB, kitty 2k lines).

| | mightty | ghostty | kitty |
|---|---|---|---|
| **Startup** | | | |
| launch → command running (median) | **57 ms** | 157 ms | 370 ms |
| **Idle (10 s)** | | | |
| CPU, avg | **0.1 %** | 0.5 % | 0.5 % |
| RSS, median | **58 MiB** | 91 MiB | 100 MiB |
| footprint (≈PSS), median | **41 MiB** | 72 MiB | 102 MiB |
| **Under load (throughput runs)** | | | |
| CPU, avg | **89.0 %** | 227.7 % | 117.0 % |
| RSS, median | 95 MiB | 150 MiB | **91 MiB** |
| footprint (≈PSS), median | **78 MiB** | 130 MiB | 116 MiB |
| **Distribution under load** | | | |
| CPU avg | **89 %** | 228 % | 117 % |
| CPU P50 | **89 %** | 227 % | 118 % |
| CPU P75 | **91 %** | 240 % | 123 % |
| CPU P90 | **95 %** | 250 % | 127 % |
| CPU P95 | **96 %** | 264 % | 128 % |
| RSS avg | **90 MiB** | 158 MiB | 93 MiB |
| RSS P50 | 95 MiB | 150 MiB | **91 MiB** |
| RSS P75 | 105 MiB | 164 MiB | **99 MiB** |
| RSS P90 | 107 MiB | 191 MiB | **99 MiB** |
| RSS P95 | 107 MiB | 201 MiB | **99 MiB** |
| footprint avg | **73 MiB** | 140 MiB | 116 MiB |
| footprint P50 | **78 MiB** | 130 MiB | 116 MiB |
| footprint P75 | **85 MiB** | 148 MiB | 116 MiB |
| footprint P90 | **85 MiB** | 171 MiB | 118 MiB |
| footprint P95 | **87 MiB** | 180 MiB | 119 MiB |
| **Memory after output** | | | |
| RSS after ~140 MB | **65 MiB** | 138 MiB | 91 MiB |
| footprint after ~140 MB | **53 MiB** | 79 MiB | 113 MiB |
| RSS after ~260 MB | **68 MiB** | 139 MiB | 90 MiB |
| footprint after ~260 MB | **55 MiB** | 82 MiB | 113 MiB |
| **Throughput** (median) | | | |
| plain ASCII | **239 MiB/s** | 213 MiB/s | 103 MiB/s |
| SGR-colour heavy | **186 MiB/s** | 162 MiB/s | 121 MiB/s |
| CJK / Unicode | **162 MiB/s** | 150 MiB/s | 87 MiB/s |
| random cursor addressing (4 MB) | **171 MiB/s** | 136 MiB/s | 93 MiB/s |
| **Response latency** (DSR round trip) | | | |
| idle, median | **0.135 ms** | 0.193 ms | 0.196 ms |
| idle, P95 | **0.336 ms** | 0.532 ms | 0.348 ms |
| under flood, median | **0.046 ms** | 0.202 ms | 4.413 ms |
| under flood, P95 | **0.111 ms** | 0.405 ms | 7.338 ms |
| **Steady 250 lines/s** | | | |
| CPU, avg | **8.8 %** | 28.6 % | 9.4 % |

Absolute numbers move with the machine from run to run, memory too: in an
earlier run idle footprint was 28, 65 and 68 MiB, and the same build of
mightty measured again beside this one idles where this table says.
Compare across a row of one run.

```sh
python3 -I bench/suite.py [ROUNDS] [STARTUPS] [TERMINALS]  # full suite above; launches each terminal itself
bench/compare.sh                                       # throughput only; needs a running mightty; uses Ghostty and kitty if installed
cargo run --release -p mightty-term --example feed -- bench/data/*.txt     # core only, no PTY
cargo run --release -p mightty-mux --example pipeline -- $PWD/bench/data/*.txt
cargo run --release -p mightty-term --example digest -- bench/data/*.txt   # a fingerprint of the terminal's state: the same before and after a change that shouldn't show
```

What made the difference, measured along the way:

- **The PTY is the ceiling.** macOS hands out at most 1 KiB per read
  (`examples/read_sizes.rs`), so end to end tops out around 200–300 MiB/s for
  everyone. Reading and parsing on one thread is right: the writer refills
  the kernel buffer while we parse; a separate parser thread only added
  hand-off cost. Non-blocking reads drained to `EAGAIN` were slower still
  (reader and writer contend for the tty lock), and raising the reader
  thread's QoS class made no measurable difference.
- **Parser fast paths**: printable ASCII is found 8 bytes at a time and
  handed over as one run; complete CSI sequences parse in a tight loop
  instead of the byte-wise state machine; other UTF-8 is validated and
  decoded in one pass.
- **Rows are written in bulk** and the **screen is a ring**: a scroll is a
  pop and a push, and rows stay short until written, so scrolling costs
  ~10 ns per line instead of ~100.
- **8-byte cells**: a character plus a packed style id and layout flags;
  colours and attributes live once in a per-terminal style table. Styles
  are interned through a small cache of recent ones (a fresh colour per
  cell, as in truecolor art, never touches a big index) and unused styles
  are dropped when the table doubles.
- **No branches on character width or UTF-8 length**: CJK and emoji text
  mixes them unpredictably, and a mispredict per character cost more than
  the work. Character starts are found 64 bytes at a time, sequences are
  decoded from one 4-byte load, and every character writes a provisional
  spacer cell instead of branching on whether it is wide.
- **The renderer snapshots** the visible cells under the terminal lock and
  builds the frame after releasing it, so drawing never stalls the reader.
- **Memory** (footprint under load 101 → 71 MiB, idle 56 → 28 MiB): the
  glyph atlas starts at 512² and doubles only when full (it was a fixed
  2048², 16 MiB), the window keeps two drawables instead of three (each is
  a full window image, ~11 MiB on Retina), vertex buffers and quad lists
  are reused across frames, and the style table is compacted in place
  through a bitmap of live ids and sized exactly, where a doubling `Vec`
  plus a rebuilt copy briefly needed several times the live styles.
- **Frame pacing**: output redraws wait for the display's next refresh on
  a timer instead of in `nextDrawable`. Before, the main thread spent ~99 %
  of a steady 250 lines/s stream blocked there, so keystrokes queued behind
  it; now it is idle ~98 % of the time, and CPU fell from ~4.4 % to ~3.4 %
  in an A/B run. Output after a pause (such as a typed key's echo) is
  drawn at once; during a stream it waits at most one refresh.

**Keypress to screen** (mightty only, 60 Hz display, 200 keys each): the
probe types keys into the window through the app's event queue and reads
Metal's presented time for the first frame showing each echo. It can't
see the keyboard or the window server before the event reaches the app,
and it can't run in other terminals; comparing those needs a camera or a
tool like [Typometer](https://github.com/pavelfatin/typometer).

Median time to screen, with the echo's frame built ~3 ms after the key
while busy, ~0.3 ms when idle:

| | `window-vsync=true` (default) | `window-vsync=false` |
|---|---|---|
| idle | 24.9 ms | 8.0 ms |
| while streaming 250 lines/s | 33–37 ms | 11–13 ms |
| while flooded | 36.1 ms | 8.9 ms |

```sh
MIGHTTY_KEY_LATENCY=keys.tsv target/release/mightty -e python3 -I bench/keys.py [idle|stream|flood]
```

After a key press the window draws ~2 ms later, once the echo is in,
instead of at the next refresh. With vsync on that barely shows: each
frame still queues for a refresh, behind any frame already waiting (idle
that is ~1.5 refreshes). With it off, macOS takes the newest frame, so the
queue goes away, at the risk of tearing (mostly fullscreen; untested).
The default follows kitty (`sync_to_monitor`) and Ghostty (`window-vsync`),
which moved to on after macOS bugs with it off. These are Metal's
presented times; a camera should confirm them.

Next: SIMD (NEON) for the SGR-heavy path.
