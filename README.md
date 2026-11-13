<p align="center">
  <img src="assets/icon.png" width="128" height="128" alt="Mightty">
</p>

<h1 align="center">Mightty</h1>

<p align="center">
  The fast, lightweight terminal for macOS.<br>
  <a href="https://mightty.org">Website</a> ·
  <a href="https://mightty.org/download/">Download</a> ·
  <a href="https://mightty.org/docs/install/">Install</a> ·
  <a href="https://mightty.org/docs/">Documentation</a> ·
  <a href="https://mightty.org/changelog/">Changelog</a>
</p>

<p align="center">
  <a href="https://github.com/mightty-org/mightty/actions/workflows/ci.yml"><img src="https://github.com/mightty-org/mightty/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
</p>

Mightty is a GPU-rendered terminal emulator for macOS, written in Rust with
no third-party dependencies. It talks to AppKit, Metal and CoreText directly,
starts in under 60 ms, and stays responsive under heavy output. It is also
built to work with coding agents: it knows when one is waiting for you, and
agents and scripts can drive it through MCP and a control socket.

## Features

- **Fast and light.** 239 MiB/s of plain text, a window with a shell in
  57 ms, and 41 MiB of memory at idle. See [Performance](#performance).
- **Tabs, splits and a quick terminal.** Tabs can be dragged between
  windows, a closed tab can be brought back with its programs still
  running, and a terminal can slide down over any app on a global key.
- **Aware of coding agents.** Mightty marks the pane where Claude Code,
  Codex or another agent is waiting, counts it on the Dock icon and sends a
  notification. `mightty mcp` lets an agent open panes, run commands and
  read their output.
- **Scriptable.** Everything the app can do to a pane is available over a
  JSON-RPC control socket and from `mightty ctl`.
- **Modern terminal protocols.** Kitty graphics and keyboard protocols,
  Unicode 17 with grapheme clusters, styled underlines, hyperlinks, and
  shell integration for zsh, bash and fish with no dotfile changes.
- **Settings you can browse.** A settings screen in a terminal tab with
  completion and live previews, backed by a plain config file that uses
  Ghostty's names. 819 themes are built in.
- **Embeddable core.** The parser and terminal state are available as
  `libmightty`, a C library with a stable ABI.

## Install

Download the latest release from
[mightty.org/download](https://mightty.org/download/), open the disk image
and drag Mightty to Applications. Mightty runs on macOS 12 or later, on
Apple Silicon and Intel, and updates itself from then on.

To build from source, you need the Xcode command line tools and Rust from
[rustup.rs](https://rustup.rs):

```sh
git clone https://github.com/mightty-org/mightty.git
cd mightty
scripts/bundle.sh --install     # builds Mightty.app and copies it to ~/Applications
```

During development:

```sh
cargo run --release                     # open a window with your login shell
cargo run --release -- -e htop          # open a window running a program
cargo run --release -- run -- ls -la    # no window: run a program and print its screen
cargo test --workspace
```

## Documentation

The documentation is at [mightty.org/docs](https://mightty.org/docs/):

| | |
|---|---|
| [First steps](https://mightty.org/docs/getting-started/) | The keys you will use every day |
| [Settings](https://mightty.org/docs/settings/) | The settings screen, the config file and every setting |
| [Key bindings](https://mightty.org/docs/keybindings/) | Changing keys, and every action |
| [Agents and scripting](https://mightty.org/docs/agents/) | Agent attention, MCP and `mightty ctl` |
| [Protocols](https://mightty.org/docs/protocols/) | The escape sequences Mightty supports |
| [Architecture](https://mightty.org/docs/architecture/) | Layers, threads, data flow and trust boundaries |

`mightty --help` lists every setting with its current value, and
`mightty ctl` lists every control command.

## Performance

Medians from `bench/suite.py` on an M2 with macOS 15. Each terminal is
launched fresh at 120×40 in Menlo 14 with its other settings at their
defaults.

| | Mightty | Ghostty 1.4.0 | kitty 0.49.2 |
|---|---|---|---|
| Plain-text throughput | **239 MiB/s** | 213 MiB/s | 103 MiB/s |
| Time to open a window with a shell | **57 ms** | 157 ms | 370 ms |
| Memory at idle | **41 MiB** | 72 MiB | 102 MiB |
| CPU under load | **89 %** | 228 % | 117 % |
| Response time under heavy output | **0.05 ms** | 0.20 ms | 4.41 ms |

[docs/performance.md](docs/performance.md) has the full results, the method,
and the design decisions behind them.

## Design

Three rules shape the code:

- **No third-party crates.** Mightty uses only Rust's standard library and
  the APIs macOS ships, through hand-written bindings.
  `scripts/check-no-deps.sh` enforces this in CI.
- **A small core, with everything else over one protocol.** The app, the
  command-line client and AI agents all use the same JSON-RPC control
  socket. Panes belong to a multiplexer, not to windows.
- **The core knows nothing about AI.** It exposes structured context, such
  as command boundaries, exit codes and the working directory, and agents
  bring their own networking.

### Repository layout

| Crate | Role |
|---|---|
| `mightty-vt` | Escape-sequence parser |
| `mightty-term` | Terminal state: screens, scrollback, modes, prompt marks, kitty graphics |
| `mightty-pty` | Pseudo-terminals |
| `mightty-mux` | Pane multiplexer, control socket and shell integration |
| `mightty-mcp` | MCP server |
| `mightty-json` | JSON parser and serializer |
| `mightty-render` | Platform-independent drawing: palette, box drawing and block elements |
| `mightty-font` | Font reader and rasterizer, for platforms without CoreText |
| `mightty-image` | PNG and DEFLATE decoders |
| `mightty-macos` | The macOS app: windows, input, the Metal renderer and the CoreText glyph atlas |
| `mightty-ffi` | `libmightty`, the C library |
| `mightty` | The `mightty` binary |

Benchmarks are in `bench/`, and scripts for bundling, releasing and
installing the library are in `scripts/`.

## libmightty

The parser and the terminal state are also a C library. It does no I/O and
starts no threads: you feed it bytes and read back cells.

```sh
scripts/install-lib.sh /usr/local        # libmightty.a, libmightty.dylib, mightty.h, mightty.pc
cc app.c $(pkg-config --cflags --libs mightty)
```

The API uses opaque handles, caller-owned buffers with size queries, UTF-8
with explicit lengths, structs that carry their own size, error codes, and
an ABI version. The rules are documented at the top of
[`mightty.h`](crates/mightty-ffi/include/mightty.h). `cargo test` compiles
and runs a C program against the header.

## Status

Mightty is released for macOS. The current version is on the
[download page](https://mightty.org/download/), and the
[changelog](https://mightty.org/changelog/) lists what each version changed.

| Area | Status |
|---|---|
| Terminal core, parser and pseudo-terminals | Done |
| macOS app: Metal renderer, tabs, splits, search, links, input methods | Done |
| Kitty graphics and keyboard protocols | Done |
| Multiplexer, control socket, `mightty ctl` and MCP server | Done |
| Agent attention, notifications and shell integration | Done |
| `libmightty` C library | Done |
| Signed and notarized releases, as a disk image | Done |
| About panel, update check and in-app updates | Done |
| Linux and Windows | Planned |

## Contributing

Contributions are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) explains how
to build and what the project expects of a change, and
[RELEASING.md](RELEASING.md) describes how releases are made. Report
security problems privately, as [SECURITY.md](SECURITY.md) describes. Coding
agents should read [AGENTS.md](AGENTS.md).

## License

Mightty is released under the [MIT License](LICENSE).

The bundled themes are MIT-licensed by their authors
(`assets/THEMES-LICENSE`). Symbols Nerd Font is MIT-licensed
(`assets/fonts/SymbolsNerdFont-LICENSE`), and the icon sets in it keep their
own licences, listed in `assets/fonts/SymbolsNerdFont-README.md`.
