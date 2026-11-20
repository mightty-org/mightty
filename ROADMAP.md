# Roadmap

What Mightty doesn't do yet. Nothing here has a date. Items move up when
someone needs them, and a pull request for any of them is welcome.

## Next

Small changes that most people would notice.

| Feature | Why |
|---|---|
| In-band resize reports (mode 2048) | Full-screen programs get the new size in their input, in order with keys, instead of from a signal. |
| Copy with colours | Copied text also goes on the clipboard as HTML, so pasting into Notes, Mail or Slack keeps its colours. |

## Later

Larger features, or ones fewer people need.

**Images**

- Inline images (`OSC 1337 File=`), used by `imgcat` and many command-line tools.
- Sixel, which tools such as chafa and lsix use.

**Keys**

- Several actions on one key.
- Key tables: modes of bindings, like tmux's prefix key.
- A catch-all binding for keys that are otherwise unbound.

**Windows, tabs and splits**

- Drag a split to rearrange it, or into another tab or window.
- Tab colours, set by right-clicking a tab.
- Rename a tab by double-clicking it.
- When a pane's program exits, show its status in a bar at the bottom of the pane, red if it failed.
- On quit, step through the windows with programs still running instead of asking once.
- Middle-click to close a tab, or to paste.

**macOS**

- Set Mightty as the default terminal.
- Shortcuts and AppleScript: open windows, tabs and splits, send text and read output.

**Terminal**

- Rich desktop notifications (OSC 99): a title, urgency and buttons, where OSC 9 and OSC 777 carry only text.
- Focus and visibility reports (mode 2033, `CSI ? 998 n`), so programs can stop redrawing while hidden.
- Soft reset (DECSTR, `CSI ! p`), and the remaining xterm queries (DECXCPR, `CSI 19 t`).
- Unicode 18 character widths.
- Compress idle scrollback in memory.

## Not planned yet

These are possible, but there's little to gain for now.

| Feature | Why not yet |
|---|---|
| Text sizing (OSC 66) | Few programs use it yet. |
| Clipboard with types (OSC 5522) | Few programs use it, and OSC 52 covers what they need. |
| tmux control mode | A large feature, used by few people. |
| Installing terminfo on ssh hosts | Mightty reports itself as `xterm-256color`, which every host already knows. |
| Screen checksums (DECRQCRA, XTCHECKSUM) | Used by terminal test suites, not by programs. |
| ⌘Z to undo an edit in the command | ⌘Z already reopens a closed tab. Ctrl-_ undoes in zsh and bash. |
| Editing the command in a text box | Multi-line editing with the mouse means replacing the shell's own line editor: a large change. |
| Linux and Windows | Planned, after the macOS app. |
