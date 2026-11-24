# Roadmap

What Mightty doesn't do yet. Nothing here has a date. Items move up when
someone needs them, and a pull request for any of them is welcome.

## Next

Small changes that most people would notice. Nothing is waiting here
now: the next items come from Later.

## Later

Larger features, or ones fewer people need.

**Keys**

- Several actions on one key.
- Key tables: modes of bindings, such as a prefix key that changes what the next key does.
- A catch-all binding for keys that are otherwise unbound.

**Windows, tabs and splits**

- Drag a split to rearrange it, or into another tab or window.
- On quit, step through the windows with programs still running instead of asking once.
- Middle-click to close a tab, or to paste.

**macOS**

- Set Mightty as the default terminal.
- Shortcuts and AppleScript: open windows, tabs and splits, send text and read output.

**Terminal**

- Rich desktop notifications (OSC 99): a title, urgency and buttons, where OSC 9 and OSC 777 carry only text.
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
