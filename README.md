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
| `mightty-font` | Font files and glyph rasterization in plain Rust (TrueType and CFF outlines, variable fonts, synthetic bold and italic, colour emoji: sbix, CBDT and COLR with its own PNG and DEFLATE decoders, OpenType shaping: GSUB substitutions and GPOS mark positioning), for platforms without CoreText |
| `mightty-ffi` | **libmightty**: stable C ABI over the core (`include/mightty.h`) |
| `mightty-json` | Minimal JSON value, parser and serializer |
| `mightty-mux` | Pane multiplexer, JSON-RPC control socket, zsh, bash and fish shell integration |
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

As an app, with an icon in the Dock and banners when a pane needs you while
you're elsewhere (macOS delivers notifications only to app bundles):

```sh
scripts/bundle.sh                       # target/release/mightty.app, signed ad hoc
scripts/bundle.sh --install             # copied to ~/Applications
```

Clicking a banner brings its pane to the front. The icon is drawn by
`scripts/make-icon.py` (`scripts/make-icon.sh` rebuilds `assets/mightty.icns`).

Settings live in `~/.config/mightty/config` (or `$XDG_CONFIG_HOME/mightty/config`),
one `name = value` per line, `#` for comments. Every setting also works as a
`--name=value` flag, which wins over the file. Names are Ghostty's where the
meaning is the same. `mightty --help` lists them all with current values.

```
# ~/.config/mightty/config
font-family = JetBrains Mono
font-size = 13
window-width = 120
window-height = 36
scrollback-lines = 50000
theme = Dracula
cursor-color = #ff79c6
```

| Setting | Default | |
|---|---|---|
| `font-family`, `font-size` | `Menlo`, `14` | Font and size in points. Repeat `font-family` for fallbacks, tried in order before the ones macOS picks; each source (the file, then flags) gives the whole list, and an empty value clears it, as in Ghostty. A name mightty can't find is reported |
| `font-family-bold`, `font-family-italic`, `font-family-bold-italic` | from `font-family` | A different family for bold or italic text; unset, the family's own bold and italic, or ones made from it |
| `font-feature` | | OpenType features, repeated: `ss01` (or `+ss01`) on, `-calt` off, `cv01=2` an alternate; they apply to single characters (`zero`) as well as ligatures. An empty value clears them |
| `font-variation`, `font-variation-bold`, `font-variation-italic`, `font-variation-bold-italic` | | Variable font axes, repeated: `wght=450`, `slnt=-10` |
| `window-width`, `window-height` | `100`, `30` | Size of new windows, in cells |
| `scrollback-lines` | `10000` | Lines kept above the screen |
| `window-vsync` | `true` | Show frames only at a display refresh; `false` cuts keypress-to-screen time (below) but may tear |
| `macos-option-as-alt` | by layout | `true`, `false`, `left` or `right`: Option keys that act as Alt instead of typing characters (å, `@`, dead keys). Unset: `true` on the US and ABC layouts, else `false`, as in Ghostty |
| `confirm-close-surface` | `true` | Ask before closing a pane (⌘W), a window or tab, or mightty: `true` while a command or agent runs in it, `always`, or `false`, as in Ghostty |
| `font-ligatures` | `true` | Draw the font's ligatures and contextual alternates (`->`, `!=`, `===` in fonts such as JetBrains Mono and Fira Code), each kept to the cells it covers; never across the cursor or a change of colour or style |
| `font-backend` | `coretext` | `mightty` draws glyphs with mightty's own rasterizer (`mightty-font`: TrueType and CFF fonts, variable fonts, colour emoji, ligatures, accents, and the fallback fonts CoreText picks; CoreText still finds fonts and draws fonts in Apple's own formats: `morx` composition, as in Menlo and Apple Color Emoji, and `hvgl` outlines). For testing: the rasterizer is for platforms without CoreText |
| `window-save-state` | `default` | Reopen windows, tabs and splits where they were, each pane a new shell in its old directory: `always`, `never`, or `default`, which follows macOS (only when "Close windows when quitting an application" is off in System Settings → Desktop & Dock), as in Ghostty. Saved in `~/Library/Application Support/mightty/session.json` |
| `background-opacity` | `1` | Opacity of the default background, 0 to 1; below 1 the desktop shows through (cells with their own background stay solid) |
| `background-blur` | `false` | Blur what shows through a translucent background: `true` (radius 20), `false`, or a radius in points |
| `window-padding-x`, `window-padding-y` | `6`, `6` | Space around the text, in points |
| `cursor-style` | `block` | `block`, `bar`, `underline` or `block_hollow`, unless the program picks one |
| `cursor-style-blink` | as the program asks | `true` or `false`; typing keeps it on for a moment |
| `adjust-cell-width`, `adjust-cell-height` | `0` | Wider or taller cells than the font gives: points (`1`, `-1`) or a percentage (`10%`); taller cells space lines out |
| `unfocused-split-opacity` | `0.7` | Panes keys aren't going to are dimmed to this, 0.15 to 1 |
| `clipboard-write`, `clipboard-read` | `allow`, `ask` | Programs setting and reading the clipboard through OSC 52, as vim, tmux and ssh sessions do to copy: `allow`, `ask` (a sheet with the text, Allow and Deny) or `deny`, as in Ghostty |
| `copy-on-select` | `false` | Copy text to the clipboard as soon as it's selected (a drag, or a double- or triple-click) |
| `mouse-hide-while-typing` | `false` | Hide the pointer while typing, until the mouse moves |
| `mouse-scroll-multiplier` | `1` | Scroll this many times as far as the wheel or trackpad says |
| `keybind` | below | `TRIGGER=ACTION`: a key and what it does (see Key bindings) |
| `theme` | | Colours from `themes/NAME` in the config directory, or a path. `light:NAME,dark:NAME` follows macOS's appearance and switches when it changes |
| `config-file` | | Another config file to read after this one, relative to it (`?PATH` if it may be missing), as in Ghostty; in the file, or `--config-file` |
| `background`, `foreground` | `#1e1e1e`, `#d8d8d8` | Default colours |
| `cursor-color`, `cursor-text` | text colour, cell background | Cursor and the text under a block cursor |
| `selection-background`, `selection-foreground` | `#3e4451`, unchanged | Selected text |
| `search-background`, `search-foreground` | `#e5c07b`, `#1e1e1e` | Search matches |
| `search-selected-background`, `search-selected-foreground` | `#ff8c32`, `#1e1e1e` | The match the search is on |
| `palette` | xterm | `palette = N=#rrggbb`, one line per entry |
| `command`, `working-directory` | login shell, home | First window only |

Themes are config files containing only colour settings, in the same format
as Ghostty's, so Ghostty's ~800 bundled themes work unchanged: copy one from
`/Applications/Ghostty.app/Contents/Resources/ghostty/themes/` into
`~/.config/mightty/themes/`. Settings after `theme` override it.

⌘, (mightty → Settings…) opens the config file in your text editor, creating
it first if there isn't one. ⌘⇧, (Reload Configuration), or `kill -USR2` as in
Ghostty, applies the files again to what's open: colours, fonts, `window-vsync`
and scrollback change in place, flags still win, and window size, `command` and
`working-directory` apply to new windows.

### Key bindings

`keybind = TRIGGER=ACTION` binds a key, as in Ghostty; repeat it for more.
A trigger is modifiers (`cmd`, `ctrl`, `alt`, `shift`) and a key joined by
`+`: a character as typed without modifiers (`t`, `[`, `=`) or a name
(`enter`, `tab`, `space`, `escape`, `backspace`, `delete`, arrows `up`/`down`/
`left`/`right`, `home`, `end`, `page_up`, `page_down`, `f1`…`f12`, `plus`).
`TRIGGER=unbind` removes a binding, and `keybind = clear` removes them all,
defaults included. Menu items show the first key bound to their action.

```
keybind = cmd+shift+t=new_tab
keybind = ctrl+shift+h=text:echo hello\r
keybind = cmd+k=unbind
```

| Action | Default | |
|---|---|---|
| `new_window`, `new_tab` | ⌘N, ⌘T | |
| `new_split:right`, `new_split:down` | ⌘D, ⌘⇧D | |
| `close_surface`, `close_window`, `minimize_window`, `quit` | ⌘W, ⌘⇧W, ⌘M, ⌘Q | |
| `copy_to_clipboard`, `paste_from_clipboard` | ⌘C, ⌘V | |
| `start_search`, `navigate_search:next`, `navigate_search:previous` | ⌘F, ⌘G, ⌘⇧G | |
| `goto_split:previous`, `goto_split:next` | ⌘[, ⌘] | |
| `goto_split:left` (`right`, `up`, `down`) | ⌘⌥ and an arrow | |
| `resize_split:left,10` (`right`, `up`, `down`; points) | ⌘⌃ and an arrow | |
| `toggle_split_zoom`, `equalize_splits` | ⌘⇧↩, ⌘⌃= | |
| `goto_tab:N`, `last_tab`, `previous_tab`, `next_tab` | ⌘1…⌘8, ⌘9, ⌘⇧[, ⌘⇧] | |
| `jump_to_attention` | ⌘⇧A | The pane waiting longest for you |
| `open_config`, `reload_config` | ⌘,, ⌘⇧, | |
| `increase_font_size:1`, `decrease_font_size:1`, `reset_font_size` | ⌘=, ⌘-, ⌘0 | The pane keys go to, in points |
| `clear_screen` | ⌘K | History and the screen above the prompt |
| `scroll_to_top`, `scroll_to_bottom`, `scroll_page_up`, `scroll_page_down` | ⌘Home, ⌘End, ⌘PgUp, ⌘PgDn | |
| `text:…`, `csi:…`, `esc:…` | | Send bytes to the program: `text:` takes `\n`, `\r`, `\t`, `\e`, `\\`, `\xHH`; `csi:2J` sends `ESC [ 2J` |
| `ignore` | | The key does nothing |

A typo in the config file is reported with its line number (on stderr, and in
a sheet when reloading) and the rest still applies; a bad flag stops with an error.
`--config-file=PATH` loads an extra file and `--config-default-files=false`
skips the default one (the benchmark suite uses it).

```sh
mightty --window-width=120 --window-height=40 -e htop
```

`MIGHTTY_SNAPSHOT=path` writes every rendered frame to a PNG, for debugging
and visual tests.

## Agents and scripting

Every mightty serves a JSON-RPC control socket (per-user directory, mode
0700; mightty refuses a directory someone else owns or a symlink). Panes are owned by the mux, not the windows, so anything that speaks
the protocol can open, drive and read them.

```sh
mightty ctl list                                  # panes, state, window/tab (* where keys go), cwd
mightty ctl run --pane 1 -- make test             # types it, waits, prints output, exits with its code
P=$(mightty ctl spawn -- npm run dev)             # new window
mightty ctl spawn --right -- cargo watch          # split right of this pane (--down, --tab; --beside N)
mightty ctl focus $P                              # bring it to the front
mightty ctl attention                             # panes needing you, longest waiting first
mightty ctl notify "Approve the migration?"       # flag this pane as needing you
mightty ctl wait $P --text "ready" && mightty ctl read $P
mightty ctl keys $P ctrl-c
mightty ctl watch command.finished pane.title      # events as JSON lines (default: all)
```

**Events.** A connection that calls `events.subscribe` (`{"events":
["pane.*", "command.finished"]}`, or no params for all) is sent JSON-RPC
notifications named after the event: `pane.opened`, `pane.closed`,
`pane.exited` (`exit_code`), `pane.focused`, `pane.title`, `pane.cwd`,
`pane.status` (`status`, `attention`), and with shell integration
`command.started` and `command.finished` (`command`, `exit_code`,
`duration_ms`). Each carries the `pane` id. Replies and events share one
writer per connection, so a slow listener holds up only itself.

**AI agents** connect through MCP:

```sh
claude mcp add mightty -- mightty mcp
```

Tools: `run_command` (output + exit code; reuses a hidden shell so `cd`
persists), `open_pane`, `read_pane`, `list_commands`, `wait_for`,
`send_text`, `send_keys`, `list_panes`, `focus_pane`, `notify_user`, `close_pane`. Panes an
agent opens go in a split beside the agent's own pane, further ones stacking
below the first, without taking the keyboard from you (`placement` asks for
a tab or window instead), and stay open after they exit. `list_panes` says
which window and tab each pane is in and which one you're focused on. With
no mightty running, `mightty mcp` hosts headless panes itself.

**Agents that need you** are easy to miss in a background tab. mightty
recognizes agents it runs (`claude`, `codex`, `gemini`, `aider`, `opencode`,
…, by command name) and tells **working** (output flowing: their spinner)
from **waiting** (gone quiet after working). A pane you aren't looking at
needs you when its agent stops working, it rings the bell, or it asks for a
notification (OSC 9, OSC 777, `mightty ctl notify`, `notify_user`). It gets
an orange border and a ● on its tab or window title, the Dock icon shows how
many panes need you (bouncing once while mightty is in the background; the
app bundle also shows a banner), and
⌘⇧A jumps to the one that has waited longest. `list_panes` reports all of
this, so agents can watch each other. For Claude Code to say exactly when it
needs a permission, add a notification hook to `~/.claude/settings.json`:

```json
{ "hooks": { "Notification": [{ "hooks": [{ "type": "command", "command": "mightty ctl notify 'Claude needs you'" }] }] } }
```

**Shell integration** (zsh, bash 3.2 and later, fish 3.0 and later) loads
automatically in every pane, with no dotfile changes: it marks prompts, commands, output and exit codes (OSC 133,
OSC 633;E, OSC 7). That's what lets `run`, `commands` and `run_command`
return exact output and exit codes. Marks carry a random per-shell nonce
that child processes never see, and mightty ignores marks without it, so a
program's output can't fake a finished command. Each shell is pointed at a
small bootstrap that runs your startup files as usual, then loads the
integration: zsh through `ZDOTDIR`, bash through `--rcfile` (it then reads
the login files, `/etc/profile` and the first of `~/.bash_profile`,
`~/.bash_login`, `~/.profile`; bash-preexec users keep theirs), fish
through `XDG_DATA_DIRS`. To use it in another terminal, source
`crates/mightty-mux/shell/mightty.{zsh,bash}` or
`crates/mightty-mux/shell/fish/vendor_conf.d/mightty.fish`.

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
| launch → command running (median) | **67 ms** | 139 ms | 394 ms |
| **Idle (10 s)** | | | |
| CPU, avg | **0.0 %** | **0.0 %** | **0.0 %** |
| RSS, median | **51 MiB** | 89 MiB | 94 MiB |
| footprint (≈PSS), median | **28 MiB** | 65 MiB | 68 MiB |
| **Under load (throughput runs)** | | | |
| CPU, avg | **85.1 %** | 230.7 % | 113.1 % |
| RSS, median | **80 MiB** | 151 MiB | 83 MiB |
| footprint (≈PSS), median | **71 MiB** | 129 MiB | 79 MiB |
| **Distribution under load** | | | |
| CPU avg | **85 %** | 231 % | 113 % |
| CPU P50 | **86 %** | 230 % | 116 % |
| CPU P75 | **89 %** | 243 % | 123 % |
| CPU P90 | **91 %** | 250 % | 128 % |
| CPU P95 | **93 %** | 254 % | 131 % |
| RSS avg | **76 MiB** | 156 MiB | 90 MiB |
| RSS P50 | **80 MiB** | 151 MiB | 83 MiB |
| RSS P75 | **88 MiB** | 162 MiB | 104 MiB |
| RSS P90 | **88 MiB** | 189 MiB | 109 MiB |
| RSS P95 | **88 MiB** | 200 MiB | 109 MiB |
| footprint avg | **67 MiB** | 136 MiB | 79 MiB |
| footprint P50 | **71 MiB** | 129 MiB | 79 MiB |
| footprint P75 | **75 MiB** | 146 MiB | 79 MiB |
| footprint P90 | **76 MiB** | 169 MiB | 79 MiB |
| footprint P95 | **76 MiB** | 178 MiB | 79 MiB |
| **Memory after output** | | | |
| RSS after ~140 MB | **61 MiB** | 141 MiB | 95 MiB |
| footprint after ~140 MB | **56 MiB** | 77 MiB | 79 MiB |
| RSS after ~260 MB | **61 MiB** | 141 MiB | 91 MiB |
| footprint after ~260 MB | **56 MiB** | 78 MiB | 79 MiB |
| **Throughput** (median) | | | |
| plain ASCII | **213 MiB/s** | 200 MiB/s | 99 MiB/s |
| SGR-colour heavy | **169 MiB/s** | 158 MiB/s | 83 MiB/s |
| CJK / Unicode | **158 MiB/s** | 152 MiB/s | 86 MiB/s |
| random cursor addressing (4 MB) | **169 MiB/s** | 156 MiB/s | 40 MiB/s |
| **Response latency** (DSR round trip) | | | |
| idle, median | **0.218 ms** | 0.297 ms | 0.329 ms |
| idle, P95 | **0.440 ms** | 0.590 ms | 0.714 ms |
| under flood, median | **0.040 ms** | 0.200 ms | 4.035 ms |
| under flood, P95 | **0.077 ms** | 0.400 ms | 6.878 ms |
| **Steady 250 lines/s** | | | |
| CPU, avg | 7.9 % | 45.0 % | **4.5 %** |

Absolute throughput moves with machine load from run to run (this run's
ASCII medians are lower for all three than the previous one); compare
across a row of one run. The steady 250 lines/s row above predates frame
pacing (below), when it was mightty's one loss; in a later mightty–kitty
run it was 4.9 % against kitty's 8.5 %. That kitty figure differs a lot
from the 4.5 % above, so treat this row as noisy.

```sh
python3 -I bench/suite.py [ROUNDS] [STARTUPS] [TERMINALS]  # full suite above; launches each terminal itself
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
   - ✅ Grapheme clusters: combining marks, joined emoji, skin tones and flags stay in one cell, drawn with CoreText shaping; other fonts fill in missing characters
   - ✅ Bold and italic for families without them: made from the regular face (grown outlines, 12° slant)
   - ✅ Ligatures (`font-ligatures`, on): `->`, `!=`, `===` and the rest in fonts that have them, drawn as one tile over exactly the cells they cover, shaped once per run of text and kept
   - ✅ IME (`NSTextInputClient`): input methods and their candidate window; text being composed is drawn underlined at the cursor
   - ✅ Kitty keyboard protocol, all five enhancements (disambiguate, event types, alternate keys, every key as an escape code, associated text), with a flag stack per screen: Neovim, Helix, fish and Kakoune can tell Ctrl-I from Tab, Esc from Alt, and see key releases; ⌘ chords no menu item uses reach them as Super
   - ✅ Search (Cmd-F): scrollback and screen, across wrapped lines, smart case, a match count, kept up to date as output arrives; Return/Cmd-G for older matches, Shift-Return/Cmd-Shift-G for newer, Esc to close; colours from the theme (`search-*`)
   - ✅ Links: hold Cmd to underline an OSC 8 hyperlink or a URL in the text and see where it goes; Cmd-click opens web and mail links and local documents, media and folders (anything else, such as apps, scripts and executables, is shown in Finder instead); other schemes are ignored
3. ✅ libmightty C ABI, pkg-config, install script
4. ✅ Mux, control socket, `mightty ctl`, zsh integration, MCP server, multi-window
5. ✅ Agent status detection (working / waiting for you), attention queue (⌘⇧A), Dock badge, OSC 9 / 777 notifications; notification banners (app bundle: `scripts/bundle.sh`)
6. ✅ bash and fish integration, config file, native tabs: ⌘T, ⌘⇧[ / ⌘⇧], ⌘1…⌘9; splits (agents open theirs beside them): ⌘D right, ⌘⇧D down, ⌘[ / ⌘] or ⌘⌥ and an arrow to move between them, ⌘⇧↩ to zoom one, ⌘⌃= or double-clicking a divider to even them out, dividers drag or move with ⌘⌃ and an arrow, ⌘W closes one, ⌘⇧W the window; panes keys aren't going to are dimmed; new tabs, windows and splits start in the current directory; windows, tabs and splits come back on relaunch (`window-save-state`)
7. Linux (raw Wayland protocol), own font rasterizer (in progress: TrueType and CFF outlines, variable fonts, synthetic bold and italic, colour emoji, ligatures and mark positioning ✅, matching CoreText within anti-aliasing; next Linux font discovery), Windows
