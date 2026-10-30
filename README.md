# mightty

A fast, lean, GPU-rendered terminal built for the agent era.

## Ground rules

- **Zero third-party crates.** Only Rust `std` and APIs the OS ships
  (Cocoa/Metal/CoreText, Wayland/X11, Win32/D3D, `openpty`, ConPTY).
  `scripts/check-no-deps.sh` enforces it.
- **Small core, everything else over one protocol.** Plugins, the CLI, and AI
  agents all talk JSON-RPC to the same control socket. Plugins are separate
  processes, so there is no embedded runtime and a crashing plugin can't take
  the terminal down.
- **The core knows nothing about AI.** It exposes structured context (OSC 133
  command blocks, cwd, scrollback) and an MCP server; agents bring their own
  networking.

## Layout

| Crate | Role |
|---|---|
| `mightty-vt` | Byte-level VT parser (Williams state machine + UTF-8, sub-params, APC) |
| `mightty-term` | Terminal state: grids, scrollback, cursor, modes, OSC 133 marks |
| `mightty-pty` | PTY spawning via hand-written `openpty` FFI |
| `mightty-render` | Platform-independent rendering: palette, procedural box drawing and block elements |
| `mightty-ffi` | **libmightty**: stable C ABI over the core (`include/mightty.h`) |
| `mightty-json` | Minimal JSON value, parser and serializer |
| `mightty-mux` | Pane multiplexer, JSON-RPC control socket, zsh shell integration |
| `mightty-mcp` | MCP server (stdio) exposing panes to AI agents |
| `mightty-macos` | Cocoa window via raw `objc_msgSend`, Metal renderer, CoreText glyph atlas |
| `mightty` | The binary: window by default, `run` for headless |

## Try it

```sh
cargo run --release                     # open a window with your login shell
cargo run --release -- -e htop          # open a window running a program; closes when it exits
cargo run --release -- run -- ls -la    # headless: run, print the final screen
cargo test
```

Settings are command line flags written `--name=value`; names follow
Ghostty's where the meaning is the same, and the same names will work in a
config file. `mightty --help` lists them with their defaults.

| Setting | Default | |
|---|---|---|
| `--font-family` | `Menlo` | Font family |
| `--font-size` | `14` | Points |
| `--window-width`, `--window-height` | `100`, `30` | Size of new windows, in cells |
| `--command` | login shell | Shell command line for the first window |
| `--working-directory` | home | Working directory of the first window |

```sh
mightty --window-width=120 --window-height=40 -e htop
```

`MIGHTTY_SNAPSHOT=path` writes every rendered frame to a PNG, for debugging
and visual tests.

## Agents and scripting

Every mightty serves a JSON-RPC control socket (per-user directory, mode
0700). Panes are owned by the mux, not the windows, so anything that speaks
the protocol can open, drive and read them.

```sh
mightty ctl list                                  # panes, state, cwd
mightty ctl run --pane 1 -- make test             # types it, waits, prints output, exits with its code
P=$(mightty ctl spawn -- npm run dev)             # new window
mightty ctl wait $P --text "ready" && mightty ctl read $P
mightty ctl keys $P ctrl-c
```

**AI agents** connect through MCP:

```sh
claude mcp add mightty -- mightty mcp
```

Tools: `run_command` (output + exit code; reuses a hidden shell so `cd`
persists), `open_pane`, `read_pane`, `list_commands`, `wait_for`,
`send_text`, `send_keys`, `list_panes`, `close_pane`. Panes an agent opens
appear as windows and stay open after they exit. With no mightty running,
`mightty mcp` hosts headless panes itself.

**Shell integration** (zsh) loads automatically in every pane, with no
dotfile changes: it marks prompts, commands, output and exit codes (OSC 133,
OSC 633;E, OSC 7). That's what lets `run`, `commands` and `run_command`
return exact output and exit codes. To use it in another terminal, source
`crates/mightty-mux/shell/mightty.zsh`.

## Performance

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

Each terminal runs with its shipped defaults apart from grid and font
(kitty with `--config NONE`). Single runs swing ±10% on a busy machine; long_lines and scroll_region
sit at the PTY ceiling for both terminals and trade places between runs.

### Full suite

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
| launch → command running (median) | **51 ms** | 129 ms | 383 ms |
| **Idle (10 s)** | | | |
| CPU, avg | **0.1 %** | 0.5 % | 0.5 % |
| RSS, median | **57 MiB** | 95 MiB | 99 MiB |
| footprint (≈PSS), median | **56 MiB** | 74 MiB | 102 MiB |
| **Under load (throughput runs)** | | | |
| CPU, avg | **88.2 %** | 230.0 % | 119.5 % |
| RSS, median | 106 MiB | 155 MiB | **90 MiB** |
| footprint (≈PSS), median | **101 MiB** | 133 MiB | 115 MiB |
| **Distribution under load** | | | |
| CPU avg | **88 %** | 230 % | 120 % |
| CPU P50 | **88 %** | 230 % | 121 % |
| CPU P75 | **90 %** | 238 % | 125 % |
| CPU P90 | **96 %** | 245 % | 128 % |
| CPU P95 | **98 %** | 248 % | 130 % |
| RSS avg | 95 MiB | 164 MiB | **92 MiB** |
| RSS P50 | 106 MiB | 155 MiB | **90 MiB** |
| RSS P75 | 114 MiB | 171 MiB | **97 MiB** |
| RSS P90 | 117 MiB | 197 MiB | **97 MiB** |
| RSS P95 | 118 MiB | 208 MiB | **98 MiB** |
| footprint avg | **96 MiB** | 141 MiB | 115 MiB |
| footprint P50 | **101 MiB** | 133 MiB | 115 MiB |
| footprint P75 | **104 MiB** | 149 MiB | 116 MiB |
| footprint P90 | **104 MiB** | 174 MiB | 117 MiB |
| footprint P95 | **104 MiB** | 181 MiB | 117 MiB |
| **Memory after output** | | | |
| RSS after ~140 MB | **68 MiB** | 149 MiB | 109 MiB |
| footprint after ~140 MB | 83 MiB | **81 MiB** | 112 MiB |
| RSS after ~260 MB | **68 MiB** | 148 MiB | 88 MiB |
| footprint after ~260 MB | 84 MiB | **83 MiB** | 113 MiB |
| **Throughput** (median) | | | |
| plain ASCII | **261 MiB/s** | 239 MiB/s | 102 MiB/s |
| SGR-colour heavy | **200 MiB/s** | 169 MiB/s | 121 MiB/s |
| CJK / Unicode | **164 MiB/s** | 157 MiB/s | 89 MiB/s |
| random cursor addressing (4 MB) | **175 MiB/s** | 168 MiB/s | 95 MiB/s |
| **Response latency** (DSR round trip) | | | |
| idle, median | **0.099 ms** | 0.162 ms | 0.170 ms |
| idle, P95 | **0.205 ms** | 0.340 ms | 0.300 ms |
| under flood, median | **0.037 ms** | 0.204 ms | 4.410 ms |
| under flood, P95 | **0.064 ms** | 0.373 ms | 6.809 ms |
| **Steady 250 lines/s** | | | |
| CPU, avg | **7.2 %** | 31.9 % | 10.9 % |

```sh
python3 -I bench/suite.py [ROUNDS] [STARTUPS]          # full suite below; launches each terminal itself
bench/compare.sh                                       # throughput only; needs a running mightty; uses Ghostty and kitty if installed
cargo run --release -p mightty-term --example feed -- bench/data/*.txt     # core only, no PTY
cargo run --release -p mightty-mux --example pipeline -- $PWD/bench/data/*.txt
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
  cell, as in truecolor art, never touches a big index) and the table is
  garbage-collected from live cells when it doubles.
- **No branches on character width or UTF-8 length**: CJK and emoji text
  mixes them unpredictably, and a mispredict per character cost more than
  the work. Character starts are found 64 bytes at a time, sequences are
  decoded from one 4-byte load, and every character writes a provisional
  spacer cell instead of branching on whether it is wide.
- **The renderer snapshots** the visible cells under the terminal lock and
  builds the frame after releasing it, so drawing never stalls the reader.

Next: keypress-to-screen latency, and SIMD (NEON) for the SGR-heavy path.

## libmightty (C ABI)

The core (parser + screen state, no I/O, no threads) is also a C library, so
anything that speaks C can embed it: Swift/GTK front ends, Python or Node
tooling, test harnesses, WebAssembly.

```sh
scripts/install-lib.sh /usr/local        # libmightty.{a,dylib|so}, mightty.h, mightty.pc
cc app.c $(pkg-config --cflags --libs mightty)
```

Rules the API follows (details at the top of `mightty.h`): opaque handles,
caller-owned buffers with size queries, UTF-8 with explicit lengths, growable
structs carrying a `size` field, error codes instead of unwinding, and an ABI
version. `cargo test` compiles and runs a C11 program against the header and
checks the header compiles as C++.

## Roadmap

1. ✅ VT parser, terminal state, PTY, headless runner
2. ✅ macOS window: Cocoa via raw `objc_msgSend`, Metal renderer, CoreText glyph atlas, keyboard input
   - ✅ Procedural box drawing, selection (click-drag, double-click word, triple-click line) + copy, mouse reporting
   - Next: IME (`NSTextInputClient`), URL hints, search
3. ✅ libmightty C ABI, pkg-config, install script
4. ✅ Mux, control socket, `mightty ctl`, zsh integration, MCP server, multi-window
5. Agent status detection (working / waiting for you), attention queue, notifications
6. Tabs/splits, config file, bash/fish integration
7. Linux (raw Wayland protocol), own font rasterizer, Windows
