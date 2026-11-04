# Plugins — specification (draft)

Status: **deferred** draft, protocol version 1. Nothing here is
implemented except the control-socket methods and events it builds on;
until plugins are needed, scripts use `mightty ctl` and `mightty ctl watch`.

## Goals

- Extend mightty in any language: a plugin is a program that reads and
  writes JSON lines. No embedded runtime, no SDK required.
- **You know what a plugin can do, and it can't do more.** Every plugin
  declares permissions; you approve them before it first runs; mightty
  enforces them twice: in the API (calls it wasn't granted fail) and in an
  OS sandbox (files, network and programs it wasn't granted are out of
  reach, whatever its code does).
- A plugin can't make the terminal slower or take it down. Nothing on the
  input, parse or render path waits on a plugin; a plugin that crashes,
  hangs or floods is contained and reported.
- One protocol: the same methods and events as `mightty ctl` and the MCP
  server, plus what only a resident process needs (actions, titles, badges,
  drawing).
- Installing a plugin is copying a directory; enabling one is a config line.

## Non-goals (v1)

- Intercepting keystrokes or rewriting output before it is drawn: both put
  a process round trip on the hot path. Plugins react to events, and get
  keys only while their own overlay has the focus.
- A plugin registry or `install` command.

## Concepts

| Term | Meaning |
|---|---|
| plugin | A directory with a `plugin.json` manifest and a program |
| host | The mightty process that runs the plugin |
| permission | Something a plugin may do, declared in its manifest, granted by you |
| action | A named command a plugin offers; in the Plugins menu, bindable to keys |
| event | A notification the host sends about panes and commands |
| overlay | A box a plugin draws over a pane, with terminal escape sequences |
| decoration | A highlight or margin mark a plugin puts on a pane's lines |

## Packaging

```
~/.config/mightty/plugins/git-title/
  plugin.json
  git-title.py
```

`plugin.json`:

```json
{
  "name": "git-title",
  "version": "1.2.0",
  "description": "Shows the git branch in tab titles",
  "protocol": 1,
  "command": ["/usr/bin/python3", "git-title.py"],
  "activation": "startup",
  "permissions": {
    "panes": ["metadata", "decorate"],
    "files": { "read": ["$PANE_CWDS"] },
    "exec": ["/usr/bin/git"]
  },
  "actions": [
    { "name": "copy-branch", "title": "Copy Branch Name", "keys": "cmd+shift+b" }
  ],
  "options": [
    { "name": "max-length", "default": "30", "help": "Longest branch name shown" }
  ]
}
```

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | `[a-z0-9-]+`, unique; must match the directory name |
| `version` | no | Shown in listings and the permission prompt |
| `description` | no | Shown in listings and the permission prompt |
| `protocol` | yes | Protocol major version the plugin speaks (`1`) |
| `command` | yes | argv; relative paths resolve against the plugin directory, which is also the working directory |
| `activation` | no | `startup` (default), or `action`: start on first use of one of its actions |
| `permissions` | no | What it needs (below); none means it can only talk to the host and read its own directory |
| `actions` | no | Declared up front, so menus and key bindings exist before it runs |
| `options` | no | Settings it accepts, with defaults and help, validated by the host |

The manifest is declarative, so the host can ask for permissions, build
menus, validate config and start plugins lazily without running anything.

## Permissions

### What can be asked for

**Host API** (enforced by the host; a call without the permission fails
with `-32003`):

| Permission | Grants | Risk shown |
|---|---|---|
| `panes: metadata` | `pane.list`, titles, cwds, command lines, exit codes, status; the matching events | low |
| `panes: text` | `pane.read`, command output in `pane.commands`, `pane.wait` on text | **reads everything on screen and in scrollback, including secrets** |
| `panes: decorate` | `pane.set_title`, `pane.set_badge`, decorations | low |
| `panes: input` | `pane.send_text`, `pane.send_keys`, `pane.run`, `pane.spawn`, `pane.close` | **can run any command as you, outside the sandbox** |
| `draw` | Overlays | low (overlays are always labelled, below) |
| `notify` | `app.notify`, `pane.notify` | low |
| `clipboard: read` / `clipboard: write` | `clipboard.read`, `clipboard.write` | read: medium |

**System** (enforced by the OS sandbox):

| Permission | Grants |
|---|---|
| `files: { read: [paths] }` | Reading those paths and below |
| `files: { write: [paths] }` | Writing (and reading) those paths and below |
| `exec: [programs]` | Running those programs (absolute paths), inside the same sandbox |
| `network: true` | Outbound network connections |

Paths may start with `~/`. `$PANE_CWDS` means "the working directory of any
pane": the sandbox can't follow it, so it is granted as your home directory
read-only, and the prompt says so.

Always allowed, without asking: reading the plugin's own directory, reading
and writing its data directory (`MIGHTTY_PLUGIN_DATA`) and a private temp
directory, reading the system's libraries and the `command` interpreter,
and talking to the host over stdio.

### Asking

Before a plugin first runs, and again whenever an update asks for more than
was granted, mightty shows a sheet:

```
┌───────────────────────────────────────────────────────┐
│  Allow "git-title" 1.2.0 to run?                      │
│  Shows the git branch in tab titles                   │
│  ~/.config/mightty/plugins/git-title                  │
│                                                       │
│  It will be able to:                                  │
│   • see pane titles, directories and commands         │
│   • change pane titles and badges                     │
│   • read files in your home folder                    │
│   • run /usr/bin/git                                  │
│                                                       │
│                         [ Don't Allow ]  [ Allow ]    │
└───────────────────────────────────────────────────────┘
```

High-risk permissions are listed first, in bold, with the risk in words.
Don't Allow leaves the plugin stopped (state `denied`); Plugins → git-title
→ Permissions… asks again, shows what is granted, and revokes it.

Grants are kept in `<state>/plugins/grants.json`, keyed by the plugin's
directory and name, with exactly what was granted. A plugin runs with the
intersection of what it declares and what was granted: a manifest that
grows gets the old grant until you approve the new one, and the prompt
shows only what is new.

Config can pre-approve, for dotfile setups on a new machine (whoever can
write your config can already run anything as you):

```
plugin-allow = git-title:panes.metadata,panes.decorate,exec:/usr/bin/git
```

### Sandbox

The plugin process runs in an OS sandbox built from its granted system
permissions. Denied by default:

- every file outside the always-allowed set and its `files` grants: your
  home directory, other plugins' data, mightty's own config and grants file;
- **the control socket**, so a plugin can't bypass its permissions by
  talking to mightty the way `ctl` does (`MIGHTTY_SOCKET` isn't passed);
- running programs other than its `command` and its `exec` grants;
- network, unless granted;
- other processes: signals, debugging, Apple Events, most system services.

Per platform:

- **macOS:** the Seatbelt sandbox (`sandbox_init` with a generated SBPL
  profile, applied in the child before `exec`). Apple has marked the API
  deprecated but it remains what Chromium, Bazel and coding agents use; if
  it stops working, plugins don't start (see below).
- **Linux:** Landlock for files and TCP, seccomp for system calls, a new
  mount and network namespace where Landlock can't express a rule (the
  control socket's directory isn't mounted).
- **Windows:** an AppContainer.

If the sandbox can't be set up (an old kernel without Landlock, say), the
plugin isn't started, and the problem says why. Running one unsandboxed
takes an explicit, per-plugin line:

```
plugin-unsandboxed = my-plugin
```

The sandbox doesn't make `panes: input` safe: typing into a shell is
running commands as you. That is why it is a high-risk permission.

## Enabling and configuring

```
plugin = git-title                 # ~/.config/mightty/plugins/git-title
plugin = ~/src/my-plugin           # or a path to a plugin directory
git-title.max-length = 40          # plugin options: <plugin>.<option>
keybind = cmd+shift+b=plugin:git-title/copy-branch
```

- `plugin` repeats; order is start order. An empty `plugin =` clears the list.
- Built-in settings never contain a dot, so `<plugin>.<option>` can't clash.
  Unknown options, and options for plugins that aren't enabled, are config
  problems.
- A manifest's `keys` is a default binding; a `keybind` in the config wins,
  and `keybind = cmd+shift+b=unbind` removes it.
- Reloading the config starts newly listed plugins, stops removed ones, and
  sends `config.changed` to the rest.

The plugin directory must be owned by you and not writable by group or
others, otherwise it is refused: someone else could swap its code.

## Process and transport

The host starts `command`, sandboxed, with:

- **stdin/stdout:** the JSON-RPC channel.
- **stderr:** appended to `<state>/plugins/<name>.log`, rotated at 1 MiB.
  `<state>` is `~/Library/Application Support/mightty` on macOS,
  `$XDG_STATE_HOME/mightty` elsewhere.
- **Environment:** a minimal one (`PATH`, `HOME`, `LANG`, `TMPDIR`), plus
  `MIGHTTY_PLUGIN` (its name), `MIGHTTY_PLUGIN_DIR`, `MIGHTTY_PLUGIN_DATA`.
  Not your shell's environment: it often holds tokens.
- **Process group:** its own, so stopping it stops its children too.

stdio, not the socket, because the host then knows which plugin is speaking:
permissions, actions, titles, badges, decorations and overlays belong to the
plugin that made them, and go when it exits.

### Framing

JSON-RPC 2.0, one message per line (UTF-8, no embedded newlines), as on the
control socket. Both sides send requests, notifications and replies. A line
over 1 MiB is a protocol error.

## Lifecycle

```
host                                   plugin
 | -- initialize (request) ----------->  |
 | <---------------- result ----------   |
 | -- initialized (notification) ----->  |
 |  ... events, requests both ways ...   |
 | -- shutdown (notification) -------->  |
 |    close stdin; SIGTERM after 2 s; SIGKILL after 5 s
```

`initialize` params:

```json
{
  "protocol": 1,
  "host": { "name": "mightty", "version": "0.2.0" },
  "granted": { "panes": ["metadata", "decorate"], "exec": ["/usr/bin/git"] },
  "options": { "max-length": "40" },
  "appearance": "dark"
}
```

Result:

```json
{ "events": ["pane.cwd", "pane.focused", "command.finished"] }
```

`events` subscribes, as `events.subscribe` does. Events need the permission
for what they reveal (`pane.title` needs `panes: metadata`); asking for
others is an error. A plugin that hasn't answered `initialize` in 5 s is
stopped and marked failed. One whose `protocol` major is higher than the
host's isn't started.

### Crashes and restarts

A plugin that exits on its own is restarted after 1 s, then 2, 4, 8 … up to
60 s. After 5 exits within 5 minutes it is left stopped and marked
**failed**, with the last lines of its log in Plugins → Show Problems.
Plugins → Restart clears that.

When it exits, what it owned goes: titles, badges, decorations and overlays
are removed; its actions stay in menus, greyed out, until it is back.

## Drawing

Two ways, both drawn by mightty's own renderer, so a plugin never touches
the GPU or the terminal state, and can't slow a frame down: plugin updates
are applied between frames.

### Overlays (`draw`)

A box over a pane that the plugin fills with ordinary terminal output:
colours, bold, box drawing, emoji, hyperlinks, all of VT. Each overlay is a
small terminal with no process behind it; the plugin is its program.
Pickers, hint labels, previews, dashboards, HUDs.

| Method | Params | Result |
|---|---|---|
| `overlay.open` | `pane`, `width`, `height` (cells), `anchor?` (`center`, `top`, `bottom`, `top-right`, …; default `center`), `focus?` (default `true`), `title?` | `overlay` id, actual `width` and `height` (clipped to the pane) |
| `overlay.write` | `overlay`, `data` (text with escape sequences) | |
| `overlay.resize` | `overlay`, `width`, `height`, `anchor?` | actual size |
| `overlay.close` | `overlay` | |

Events to the plugin:

| Event | Params |
|---|---|
| `overlay.input` | `overlay`, `text` or `key` (as in `pane.send_keys`: `enter`, `ctrl-c`, `up`, …), `mods` |
| `overlay.mouse` | `overlay`, `kind` (`press`, `release`, `move`, `scroll`), `col`, `row`, `button` |
| `overlay.closed` | `overlay`, `reason` (`plugin`, `user`, `pane-closed`) |

- A focused overlay gets the pane's keys and mouse until it closes; this is
  the only way a plugin receives keys. Esc closes it unless the plugin has
  asked for Esc (`overlay.open` with `keys: ["escape"]`), and ⌘. always does.
- An unfocused overlay is a HUD: it doesn't take input, and clicks pass
  through.
- **Overlays are always framed and labelled with the plugin's name**, in a
  border the plugin can't draw over or hide. A plugin can't pass off its UI
  as a program's: a fake `sudo` password prompt would say `git-title` on it.
- At most 4 overlays per plugin, each at most the pane's size.

### Decorations (`panes: decorate`)

Marks on a pane's lines that move with them as output scrolls, without
changing the text.

| Method | Params | Result |
|---|---|---|
| `pane.decorate` | `pane`, `items`: list of `{line, from?, to?, style}` | `decoration` id |
| `pane.undecorate` | `decoration` | |

- `line` is an absolute line, as in `pane.commands` and command events.
  `from`/`to` are columns; omitted, it is the whole line.
- `style`: `underline` (`color?`, `curly?`), `background` (`color`),
  `foreground` (`color`), or `gutter` (`text` of at most 2 cells, `color?`):
  a mark in the left margin, e.g. ✗ beside a failed command.
- Colors are names from the theme (`red`, `bright-green`, `accent`, …) or
  `#rrggbb`. Theme names follow light and dark mode by themselves.
- Decorations go when their lines leave the scrollback, on `clear`, or with
  their plugin.

### Titles and badges (`panes: decorate`)

| Method | Params | Effect |
|---|---|---|
| `pane.set_title` | `pane`, `title` (`null` clears) | Replaces the title shown for the pane. The program's own (OSC 2) is still in `pane.list` and events |
| `pane.set_badge` | `pane`, `text` (`null` clears), `color?` | Short text after the title, e.g. `main ✓`; at most 32 characters |

When several plugins set a title or badge for one pane, the most recent
wins; when it clears it or exits, the one before shows again.

## Host methods (plugin → host)

Each needs the permission in the table above. Besides drawing:

| Method | Permission | |
|---|---|---|
| `app.info`, `config.get` | none | Host version; effective built-in settings and its own options |
| `pane.list`, `pane.commands` (without output), `attention.list` | `panes: metadata` | |
| `pane.read`, `pane.commands` (with output), `pane.wait` (text) | `panes: text` | |
| `pane.send_text`, `pane.send_keys`, `pane.run`, `pane.spawn`, `pane.close`, `pane.focus` | `panes: input` | |
| `pane.notify`, `app.notify` (`title`, `body`, `pane?`) | `notify` | |
| `clipboard.read`, `clipboard.write` (`text`) | `clipboard` | |
| `events.subscribe`, `events.unsubscribe` | per event | |
| `action.set_enabled` (`action`, `enabled`) | none | Greys out a menu item |

## Plugin methods (host → plugin)

| Message | Kind | Params |
|---|---|---|
| `initialize` | request | above; answer within 5 s |
| `initialized` | notification | |
| `action.invoked` | notification | `action`, `pane` (focused pane, if any) |
| `config.changed` | notification | `options`, `appearance` |
| `permissions.changed` | notification | `granted` (after a revoke the plugin is restarted instead) |
| `shutdown` | notification | exit within 2 s |
| events | notification | `pane.*`, `command.*`, `overlay.*` |

The host never waits on a plugin's answer except to `initialize`.

## Actions

- Each declared action is a Plugins menu item, under a submenu named after
  the plugin: Plugins → git-title → Copy Branch Name.
- Bindable: `keybind = TRIGGER=plugin:<plugin>/<action>`.
- With `activation: action`, the first invocation starts the plugin (asking
  for permissions first, if needed) and `action.invoked` follows
  `initialized`.

## Limits

| What | Limit | When exceeded |
|---|---|---|
| Line length | 1 MiB | Connection closed, plugin restarted |
| Requests in flight | 64 | Error `-32005` |
| Queued messages to a plugin | 10 000 | Oldest events dropped; it gets `events.dropped` with a count |
| Titles, badges, decorations | 20 updates per second per pane | Coalesced: the last one wins |
| Decorated lines | 10 000 per pane per plugin | Error `-32006` |
| Overlay output | 4 MiB per second per plugin | Read slower (back-pressure) |
| `initialize` answer | 5 s | Stopped, marked failed |

## Errors

Standard JSON-RPC codes (`-32700`, `-32600`, `-32601`, `-32602`), plus:

| Code | Meaning |
|---|---|
| `-32000` | The operation failed (message says why) |
| `-32001` | No such pane, overlay or decoration |
| `-32003` | Permission not granted (message names it) |
| `-32005` | Too many requests in flight |
| `-32006` | Over a size limit |

Denied calls are also written to the plugin's log, so its author sees them.

## Versioning

- `protocol` is a major version. New methods, params, events, permissions
  and result fields come without changing it; plugins ignore what they
  don't know.
- Removing or changing the meaning of anything bumps it. The host supports
  the previous major for at least one release.

## Inspecting

- `mightty ctl plugins`: name, version, state (`running`, `starting`,
  `stopped`, `denied`, `failed`), pid, restarts, granted permissions, log.
- Plugins menu: each plugin with its state; Permissions…, Restart, Open Log;
  Reload Plugins.
- Bad manifests and plugin options are config problems, shown with the rest
  on launch and reload.

## Example

A plugin (Python, stdlib only) that marks failed commands with a ✗ in the
margin and a badge. Manifest permissions: `"panes": ["metadata", "decorate"]`.

```python
import json, sys

def send(msg):
    print(json.dumps({"jsonrpc": "2.0", **msg}), flush=True)

for line in sys.stdin:
    msg = json.loads(line)
    method, p = msg.get("method"), msg.get("params", {})
    if method == "initialize":
        send({"id": msg["id"], "result": {"events": ["command.finished"]}})
    elif method == "command.finished":
        failed = p["exit_code"] not in (0, None)
        send({"method": "pane.set_badge",
              "params": {"pane": p["pane"], "text": f"✗ {p['exit_code']}" if failed else None}})
        if failed:
            send({"method": "pane.decorate", "params": {"pane": p["pane"], "items": [
                {"line": p["prompt_line"], "style": {"gutter": "✗", "color": "red"}}]}})
    elif method == "shutdown":
        break
```

`command.finished` gains `prompt_line` and `output_line` for this.

## v2 candidates

- **Status bar:** per-window segments plugins fill.
- **Command palette:** actions searchable by title.
- **Text triggers:** `pane.watch {pattern}` sends `pane.matched` when output
  matches, matched off the render path.
- **Network by host:** `network: ["api.github.com"]` through a host-side
  proxy, since OS sandboxes filter by address, not name.
