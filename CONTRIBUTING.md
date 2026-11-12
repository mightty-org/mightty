# Contributing to Mightty

Thanks for helping. Bug reports, fixes and features are all welcome. For
anything bigger than a fix, open an issue first, so we can agree on the
approach before you spend time on it.

## Building

You need macOS 12 or later, the Xcode command line tools
(`xcode-select --install`) and Rust from [rustup.rs](https://rustup.rs).

```sh
cargo run --release                 # a window with your login shell
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all                     # rustfmt's default style
scripts/check-no-deps.sh            # fails if a third-party crate crept in
scripts/bundle.sh --install         # Mightty.app in ~/Applications
```

`mightty run -- PROGRAM` runs a program in a terminal with no window and
prints what it left on screen, which is the quickest way to check terminal
behaviour by hand.

## Rules the code follows

- **No third-party crates.** Only Rust's standard library and what macOS
  ships (Cocoa, Metal, CoreText, `openpty`), with the bindings written by
  hand. That includes tests and build tools. If you need something a crate
  would give you, write the part you need in the crate it belongs to.
- **The core knows nothing about AI.** Agents get structured context (prompt
  marks, the control socket, MCP) and do their own networking.
- **Everything goes through the control socket.** The app, `mightty ctl` and
  the MCP server use the same JSON-RPC calls. A feature scripts should reach
  is a socket method first.
- **Output is untrusted.** Anything a program prints, including over ssh, can
  be hostile. Bound sizes, strip control characters from titles, and don't
  let output trigger actions without the user.

## Writing the code

- Run `cargo fmt --all` before you commit; CI checks it. Data tables that
  read better several entries to a line, such as the Unicode tables in
  `width.rs`, are marked `#[rustfmt::skip]`.
- Match the code around you in naming and comments.
- Every `unsafe` block has a `// SAFETY:` comment saying why it's sound.
- Comments say why, not what. Items get a `///` doc comment when their name
  doesn't say enough.
- New behaviour gets a test. Terminal behaviour is tested in `mightty-term`
  by feeding bytes and checking the screen; the control socket in
  `crates/mightty-mux/tests`.
- A new setting goes in `SETTINGS` in `crates/mightty-macos/src/config.rs`
  with its help text, and a new action in `ACTIONS` in `bindings.rs`. The
  help text is what `mightty config` shows and what the website's reference
  is built from, so write it for users.
- Use Ghostty's name for a setting when it means the same thing.

## Commits and pull requests

Changes reach `main` through pull requests, which need CI to pass: formatting,
tests, clippy with warnings as errors, and the no-dependencies check. They're
merged by squash or rebase, so `main` has no merge commits.

Write commit subjects as a short description of the change, in sentence
case, without a trailing full stop: "Kitty graphics animation",
"Quick terminal: give the keys back before hiding". In the body, say what
changed and why, wrapped at 72 columns.

## Licence

By contributing, you agree that your work is released under the
[MIT licence](LICENSE).
