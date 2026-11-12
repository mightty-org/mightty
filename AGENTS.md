# Notes for coding agents

Mightty is a terminal emulator for macOS in Rust: hand-written bindings to
Cocoa, Metal and CoreText, and no third-party crates. Read `README.md` for
the crate layout and `CONTRIBUTING.md` for the rules. These notes add what
an agent tends to get wrong.

## Commands

```sh
cargo build --release
cargo test --workspace                          # about 20 s
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all                                 # CI runs cargo fmt --all --check
scripts/check-no-deps.sh
target/release/mightty run -- PROGRAM            # run headless, print the screen
scripts/bundle.sh --install                     # Mightty.app in ~/Applications
```

## Don't

- **Don't add a dependency**, not even a dev-dependency or build tool.
  Write what you need in-tree. `scripts/check-no-deps.sh` fails otherwise.
- **Don't quit or kill a running Mightty.** You may be running inside it, and
  it holds the user's shells. To try a build, open it as a new instance:
  `open -n ~/Applications/Mightty.app`.
- **Don't put AI features in the core crates** (`mightty-vt`, `mightty-term`,
  `mightty-mux`). Agents talk to Mightty through MCP and the control socket.

## Where things are

- Escape sequences: parsed in `mightty-vt`, applied in
  `crates/mightty-term/src/lib.rs`. Kitty graphics are in `graphics.rs`.
- The app: `crates/mightty-macos/src/app.rs`. Objective-C calls go through
  the `msg!` and `call!` macros and the declarations in `ffi.rs`.
- Settings: `SETTINGS` in `crates/mightty-macos/src/config.rs`. Actions and
  default keys: `crates/mightty-macos/src/bindings.rs`.
- Control socket methods: `crates/mightty-mux/src/rpc.rs`. `mightty ctl`:
  `crates/mightty/src/ctl.rs`. MCP tools: `crates/mightty-mcp/src/lib.rs`.
- Benchmarks: `bench/`. Read `README.md`'s Performance section before
  quoting numbers; the two benchmark scripts measure differently.

## Conventions

- Format with `cargo fmt --all` (rustfmt defaults). Keep `#[rustfmt::skip]`
  for data tables only.
- Every `unsafe` block has a `// SAFETY:` comment.
- Comments explain why. Match the density and wording of the file you're in.
- Settings and actions need help text written for users: it's shown in
  `mightty config` and exported (`mightty config --export`) to build the
  website's reference.
- Commit subjects describe the change in sentence case, with no prefix and
  no full stop. Bodies are wrapped at 72 columns.
- Test terminal behaviour by feeding bytes to a `Term` and checking the
  grid; see `crates/mightty-term/src/tests.rs`.
