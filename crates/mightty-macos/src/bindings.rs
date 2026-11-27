//! Key bindings: `keybind = TRIGGER=ACTION` settings
//! (`cmd+shift+d=new_split:down`), the defaults, and matching key presses.
//!
//! A trigger is modifiers and a key joined by `+`. Keys are a character
//! as typed without modifiers (`t`, `[`, `=`) or a name (`enter`,
//! `page_up`, `f5`, …); `plus` is `+`. Menu items show the first trigger
//! bound to their action.
//!
//! A binding can be keys pressed one after another (`ctrl+a>c`), and can
//! do several actions in order (`clear_screen; text:\x0c`). One written
//! `NAME/TRIGGER` belongs to the key table NAME, which `key_table:NAME`
//! switches to: its keys act until Escape. `catch_all` as the trigger
//! binds every key a table (or the main one) leaves unbound, but for
//! Command keys, which stay the menus'.

use crate::split::Direction;

/// Modifier bits, as in NSEvent's modifier flags shifted down.
pub const SHIFT: u8 = 1;
pub const CTRL: u8 = 2;
pub const ALT: u8 = 4;
pub const CMD: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Lowercase, as typed with no modifiers on the current layout.
    Char(char),
    Named(Named),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Enter,
    Tab,
    Space,
    Backspace,
    Delete,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// F1 to F12.
    F(u8),
}

/// Names for keys, and characters that are awkward in a trigger.
const NAMES: &[(&str, Key)] = &[
    ("enter", Key::Named(Named::Enter)),
    ("return", Key::Named(Named::Enter)),
    ("tab", Key::Named(Named::Tab)),
    ("space", Key::Named(Named::Space)),
    ("backspace", Key::Named(Named::Backspace)),
    ("delete", Key::Named(Named::Delete)),
    ("escape", Key::Named(Named::Escape)),
    ("up", Key::Named(Named::Up)),
    ("down", Key::Named(Named::Down)),
    ("left", Key::Named(Named::Left)),
    ("right", Key::Named(Named::Right)),
    ("home", Key::Named(Named::Home)),
    ("end", Key::Named(Named::End)),
    ("page_up", Key::Named(Named::PageUp)),
    ("page_down", Key::Named(Named::PageDown)),
    ("plus", Key::Char('+')),
    ("minus", Key::Char('-')),
    ("equal", Key::Char('=')),
    ("comma", Key::Char(',')),
    ("period", Key::Char('.')),
    ("slash", Key::Char('/')),
    ("backslash", Key::Char('\\')),
    ("semicolon", Key::Char(';')),
    ("apostrophe", Key::Char('\'')),
    ("grave_accent", Key::Char('`')),
    ("left_bracket", Key::Char('[')),
    ("right_bracket", Key::Char(']')),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub mods: u8,
    pub key: Key,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `key_table:NAME`: the keys act as that table binds them, until
    /// Escape or `end_key_table`.
    KeyTable(String),
    EndKeyTable,
    NewWindow,
    NewTab,
    /// `new_split:right` or `new_split:down`.
    NewSplit(Direction),
    CloseSurface,
    CloseWindow,
    MinimizeWindow,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    Copy,
    Cut,
    Paste,
    StartSearch,
    /// `navigate_search:next` (+1) or `:previous` (-1).
    NavigateSearch(isize),
    /// `goto_split:previous` / `:next`.
    GotoSplitStep(isize),
    /// `goto_split:left` and the other directions.
    GotoSplit(Direction),
    ToggleSplitZoom,
    EqualizeSplits,
    /// `resize_split:DIRECTION,POINTS` (points default to 10).
    ResizeSplit(Direction, f64),
    /// `goto_tab:N`, from 1.
    GotoTab(usize),
    /// `set_tab_color:NAME`, a built-in colour or one of `tab-color`'s, or `:none`.
    SetTabColor(Option<String>),
    PreviousTab,
    NextTab,
    LastTab,
    JumpToAttention,
    /// The config file in `$EDITOR`, in a new tab.
    OpenConfig,
    /// The settings, in a window of their own (`mightty config`).
    OpenSettings,
    ReloadConfig,
    /// Secure Keyboard Entry, on or off.
    ToggleSecureInput,
    /// Makes Mightty the app that runs scripts and opens `ssh://` links.
    MakeDefaultTerminal,
    /// The About panel: version and licence.
    About,
    /// Asks whether a newer version has been released, and says.
    CheckForUpdates,
    /// The quick terminal: a window dropping down from the top of the screen.
    ToggleQuickTerminal,
    /// The command palette: actions by name.
    ToggleCommandPalette,
    /// Asks for a title for the pane, shown in its tab and window.
    PromptSurfaceTitle,
    /// The key window's tab, out into a window of its own.
    MoveTabToNewWindow,
    /// Closes the key window's other tabs.
    CloseOtherTabs,
    /// Brings back the last tab, pane or window closed, still running (see `undo-timeout`).
    Undo,
    /// Reopens the last closed: running still, or new shells where they were.
    ReopenClosed,
    /// Points; `increase_font_size:N` and `decrease_font_size:N`.
    ChangeFontSize(f64),
    ResetFontSize,
    ClearScreen,
    ScrollToTop,
    ScrollToBottom,
    ScrollPageUp,
    ScrollPageDown,
    /// `jump_to_prompt:N`: N prompts up (negative) or down.
    JumpToPrompt(isize),
    /// `text:…`, `csi:…`, `esc:…`: bytes sent to the program.
    Text(Vec<u8>),
    /// The key does nothing, not even reach the program.
    Ignore,
}

impl Action {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (name, arg) = match s.split_once(':') {
            Some((name, arg)) => (name, Some(arg)),
            None => (s, None),
        };
        let direction = |arg: &str| match arg {
            "left" => Ok(Direction::Left),
            "right" => Ok(Direction::Right),
            "up" | "top" => Ok(Direction::Up),
            "down" | "bottom" => Ok(Direction::Down),
            _ => Err(format!("`{name}`: expects left, right, up or down")),
        };
        let points = |arg: Option<&str>| {
            arg.map_or(Ok(1.0), |a| {
                a.parse()
                    .ok()
                    .filter(|p: &f64| p.is_finite() && *p > 0.0)
                    .ok_or(format!("`{name}`: expects a number of points"))
            })
        };
        let bare = |action: Self| match arg {
            None => Ok(action),
            Some(_) => Err(format!("`{name}` takes no argument")),
        };
        match (name, arg) {
            ("new_window", _) => bare(Self::NewWindow),
            ("new_tab", _) => bare(Self::NewTab),
            ("new_split", Some("right")) => Ok(Self::NewSplit(Direction::Right)),
            ("new_split", Some("down")) => Ok(Self::NewSplit(Direction::Down)),
            ("new_split", _) => Err("`new_split`: expects right or down".into()),
            ("close_surface", _) => bare(Self::CloseSurface),
            ("close_window", _) => bare(Self::CloseWindow),
            ("minimize_window", _) => bare(Self::MinimizeWindow),
            ("hide", _) => bare(Self::Hide),
            ("hide_others", _) => bare(Self::HideOthers),
            ("show_all", _) => bare(Self::ShowAll),
            ("quit", _) => bare(Self::Quit),
            ("copy_to_clipboard", _) => bare(Self::Copy),
            ("cut_to_clipboard", _) => bare(Self::Cut),
            ("paste_from_clipboard", _) => bare(Self::Paste),
            ("start_search", _) => bare(Self::StartSearch),
            ("navigate_search", Some("next")) => Ok(Self::NavigateSearch(1)),
            ("navigate_search", Some("previous")) => Ok(Self::NavigateSearch(-1)),
            ("navigate_search", _) => Err("`navigate_search`: expects next or previous".into()),
            ("goto_split", Some("previous")) => Ok(Self::GotoSplitStep(-1)),
            ("goto_split", Some("next")) => Ok(Self::GotoSplitStep(1)),
            ("goto_split", Some(arg)) => Ok(Self::GotoSplit(direction(arg)?)),
            ("resize_split", Some(arg)) => {
                let (dir, amount) = match arg.split_once(',') {
                    Some((dir, amount)) => (dir, Some(amount)),
                    None => (arg, None),
                };
                Ok(Self::ResizeSplit(
                    direction(dir)?,
                    amount.map_or(Ok(10.0), |a| points(Some(a)))?,
                ))
            }
            ("toggle_split_zoom", _) => bare(Self::ToggleSplitZoom),
            ("equalize_splits", _) => bare(Self::EqualizeSplits),
            ("set_tab_color", Some(name)) if name.trim().eq_ignore_ascii_case("none") => {
                Ok(Self::SetTabColor(None))
            }
            ("set_tab_color", Some(name)) if !name.trim().is_empty() => {
                Ok(Self::SetTabColor(Some(name.trim().to_owned())))
            }
            ("set_tab_color", _) => Err("`set_tab_color`: expects a colour's name, or none".into()),
            ("goto_tab", Some(n)) => n
                .parse()
                .ok()
                .filter(|&n| n > 0)
                .map(Self::GotoTab)
                .ok_or("`goto_tab`: expects a tab number from 1".into()),
            ("previous_tab", _) => bare(Self::PreviousTab),
            ("next_tab", _) => bare(Self::NextTab),
            ("last_tab", _) => bare(Self::LastTab),
            ("jump_to_attention", _) => bare(Self::JumpToAttention),
            ("open_config", _) => bare(Self::OpenConfig),
            ("open_settings", _) => bare(Self::OpenSettings),
            ("reload_config", _) => bare(Self::ReloadConfig),
            ("toggle_secure_input", _) => bare(Self::ToggleSecureInput),
            ("make_default_terminal", _) => bare(Self::MakeDefaultTerminal),
            ("about", _) => bare(Self::About),
            ("check_for_updates", _) => bare(Self::CheckForUpdates),
            ("toggle_quick_terminal", _) => bare(Self::ToggleQuickTerminal),
            ("toggle_command_palette", _) => bare(Self::ToggleCommandPalette),
            ("prompt_surface_title", _) => bare(Self::PromptSurfaceTitle),
            ("undo", _) => bare(Self::Undo),
            ("move_tab_to_new_window", _) => bare(Self::MoveTabToNewWindow),
            ("close_other_tabs", _) => bare(Self::CloseOtherTabs),
            ("reopen_closed", _) => bare(Self::ReopenClosed),
            ("increase_font_size", _) => Ok(Self::ChangeFontSize(points(arg)?)),
            ("decrease_font_size", _) => Ok(Self::ChangeFontSize(-points(arg)?)),
            ("reset_font_size", _) => bare(Self::ResetFontSize),
            ("clear_screen", _) => bare(Self::ClearScreen),
            ("scroll_to_top", _) => bare(Self::ScrollToTop),
            ("scroll_to_bottom", _) => bare(Self::ScrollToBottom),
            ("scroll_page_up", _) => bare(Self::ScrollPageUp),
            ("scroll_page_down", _) => bare(Self::ScrollPageDown),
            ("jump_to_prompt", Some(n)) => n
                .parse()
                .ok()
                .filter(|&n: &isize| n != 0)
                .map(Self::JumpToPrompt)
                .ok_or(
                    "`jump_to_prompt`: expects a number of prompts, -1 for the one before".into(),
                ),
            ("text", Some(text)) => Ok(Self::Text(unescape(text)?)),
            ("csi", Some(text)) => Ok(Self::Text([b"\x1b[".as_slice(), text.as_bytes()].concat())),
            ("esc", Some(text)) => Ok(Self::Text([b"\x1b".as_slice(), text.as_bytes()].concat())),
            ("ignore", _) => bare(Self::Ignore),
            ("key_table", Some(table)) if table_name(table) => Ok(Self::KeyTable(table.into())),
            ("key_table", _) => {
                Err("`key_table`: expects a table's name, of letters, digits, - and _".into())
            }
            ("end_key_table", _) => bare(Self::EndKeyTable),
            _ => Err(format!("unknown action `{s}`")),
        }
    }
}

/// Whether `name` can name a key table.
fn table_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Splits the actions of a binding at each `;` that starts another
/// action: `csi:1;5A` is one action, `clear_screen; text:\x0c` two.
fn split_actions(text: &str) -> Vec<&str> {
    let starts_action = |rest: &str| {
        let word = rest
            .trim_start()
            .split([':', ';'])
            .next()
            .unwrap_or("")
            .trim_end();
        ACTIONS
            .iter()
            .any(|(name, _)| name.split(':').next() == Some(word))
    };
    let mut parts = Vec::new();
    let mut start = 0;
    for (at, _) in text.match_indices(';') {
        if starts_action(&text[at + 1..]) {
            parts.push(text[start..at].trim());
            start = at + 1;
        }
    }
    parts.push(text[start..].trim());
    parts
}

/// `\n`, `\r`, `\t`, `\e`, `\\` and `\xHH` in `text:` actions.
fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                let byte = (hex.len() == 2)
                    .then(|| u8::from_str_radix(&hex, 16).ok())
                    .flatten();
                out.push(byte.ok_or("`\\x` expects two hex digits")?);
            }
            _ => return Err("unknown escape; use \\n, \\r, \\t, \\e, \\\\ or \\xHH".into()),
        }
    }
    Ok(out)
}

/// As it's written in a `keybind`: `ctrl+grave_accent`.
impl std::fmt::Display for Trigger {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        for (bit, name) in [(CMD, "cmd"), (CTRL, "ctrl"), (ALT, "alt"), (SHIFT, "shift")] {
            if self.mods & bit != 0 {
                write!(f, "{name}+")?;
            }
        }
        match NAMES.iter().find(|(_, key)| *key == self.key) {
            Some((name, _)) => f.write_str(name),
            None => match self.key {
                Key::Named(Named::F(n)) => write!(f, "f{n}"),
                Key::Char(c) => write!(f, "{c}"),
                Key::Named(n) => write!(f, "{n:?}"),
            },
        }
    }
}

impl Trigger {
    pub fn parse(s: &str) -> Result<Self, String> {
        // A trailing `++` is the `+` key itself.
        let (mods, key) = match s.strip_suffix("++") {
            Some(mods) => (mods, "+"),
            None => s.rsplit_once('+').unwrap_or(("", s)),
        };
        let mut bits = 0;
        for m in mods.split('+').filter(|m| !m.is_empty()) {
            bits |= match m {
                "cmd" | "command" | "super" => CMD,
                "ctrl" | "control" => CTRL,
                "alt" | "opt" | "option" => ALT,
                "shift" => SHIFT,
                _ => {
                    return Err(format!(
                        "unknown modifier `{m}`; use cmd, ctrl, alt or shift"
                    ));
                }
            };
        }
        let key = if let Some(&(_, key)) = NAMES.iter().find(|(name, _)| *name == key) {
            key
        } else if let Some(n) = key
            .strip_prefix('f')
            .and_then(|n| n.parse().ok())
            .filter(|n| (1..=12).contains(n))
        {
            Key::Named(Named::F(n))
        } else {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Char(c.to_lowercase().next().unwrap_or(c)),
                _ => return Err(format!("unknown key `{key}`")),
            }
        };
        Ok(Self { mods: bits, key })
    }

    /// A key press: macOS key code, the character the key types with no
    /// modifiers, and modifier bits.
    pub fn from_event(key_code: u16, unmodified: &str, mods: u8) -> Option<Self> {
        let named = match key_code {
            36 | 76 => Named::Enter,
            48 => Named::Tab,
            49 => Named::Space,
            51 => Named::Backspace,
            117 => Named::Delete,
            53 => Named::Escape,
            126 => Named::Up,
            125 => Named::Down,
            123 => Named::Left,
            124 => Named::Right,
            115 => Named::Home,
            119 => Named::End,
            116 => Named::PageUp,
            121 => Named::PageDown,
            122 => Named::F(1),
            120 => Named::F(2),
            99 => Named::F(3),
            118 => Named::F(4),
            96 => Named::F(5),
            97 => Named::F(6),
            98 => Named::F(7),
            100 => Named::F(8),
            101 => Named::F(9),
            109 => Named::F(10),
            103 => Named::F(11),
            111 => Named::F(12),
            _ => {
                let c = unmodified.chars().next()?;
                return Some(Self {
                    mods,
                    key: Key::Char(c.to_lowercase().next().unwrap_or(c)),
                });
            }
        };
        Some(Self {
            mods,
            key: Key::Named(named),
        })
    }

    /// The menu item key equivalent: its string and modifier bits.
    pub fn menu_key(&self) -> String {
        let named = |n: Named| match n {
            Named::Enter => '\r',
            Named::Tab => '\t',
            Named::Space => ' ',
            Named::Backspace => '\u{8}',
            Named::Delete => '\u{F728}',
            Named::Escape => '\u{1b}',
            Named::Up => '\u{F700}',
            Named::Down => '\u{F701}',
            Named::Left => '\u{F702}',
            Named::Right => '\u{F703}',
            Named::Home => '\u{F729}',
            Named::End => '\u{F72B}',
            Named::PageUp => '\u{F72C}',
            Named::PageDown => '\u{F72D}',
            Named::F(n) => char::from_u32(0xF703 + n as u32).unwrap_or(' '),
        };
        match self.key {
            Key::Char(c) => c.to_string(),
            Key::Named(n) => named(n).to_string(),
        }
    }
}

/// Every action, as a `keybind` names it (with an example argument where
/// it takes one), and what it does: for `mightty config --export` and so
/// the docs.
pub const ACTIONS: &[(&str, &str)] = &[
    (
        "new_window",
        "A new window, its shell where the current one is",
    ),
    ("new_tab", "A new tab, its shell where the current one is"),
    (
        "new_split:right",
        "Splits the pane, the new one to the right",
    ),
    ("new_split:down", "Splits the pane, the new one below"),
    (
        "close_surface",
        "Closes the pane, or its tab or window with its last pane",
    ),
    ("close_window", "Closes the window"),
    ("minimize_window", "Minimizes the window"),
    ("hide", "Hides Mightty's windows until it's chosen again"),
    ("hide_others", "Hides every other app's windows"),
    ("show_all", "Shows every hidden app's windows again"),
    ("quit", "Quits Mightty"),
    ("copy_to_clipboard", "Copies the selection"),
    (
        "cut_to_clipboard",
        "Copies the selection and deletes it, when it's in the command being typed",
    ),
    (
        "paste_from_clipboard",
        "Pastes, asking first when it could run commands",
    ),
    ("start_search", "Finds text in the pane and its history"),
    ("navigate_search:next", "The next match (or previous)"),
    ("goto_split:next", "The next pane (or previous)"),
    (
        "goto_split:left",
        "The pane to the left (or right, up, down)",
    ),
    (
        "resize_split:left,10",
        "Moves the nearest divider, in points",
    ),
    (
        "toggle_split_zoom",
        "The pane alone fills the window, or not",
    ),
    (
        "equalize_splits",
        "Shares the window evenly between its panes",
    ),
    ("goto_tab:1", "Tab N, from 1"),
    (
        "set_tab_color:red",
        "Marks the tab with a colour: red, orange, yellow, green, blue, purple, gray or one of \
         tab-color's; none takes it off",
    ),
    ("previous_tab", "The tab to the left"),
    ("next_tab", "The tab to the right"),
    ("last_tab", "The last tab"),
    (
        "move_tab_to_new_window",
        "The tab out into a window of its own",
    ),
    ("close_other_tabs", "Closes the window's other tabs"),
    (
        "undo",
        "Brings back the last tab, pane or window closed, still running, within undo-timeout",
    ),
    (
        "reopen_closed",
        "Reopens the last tab, pane or window closed, running still or with new shells",
    ),
    (
        "prompt_surface_title",
        "A title for the pane, shown in its tab and window",
    ),
    (
        "jump_to_attention",
        "The pane that has waited longest for you",
    ),
    (
        "toggle_command_palette",
        "Every action by name, filtered as you type",
    ),
    (
        "toggle_quick_terminal",
        "A terminal that drops from the top of the screen, over any app",
    ),
    (
        "toggle_secure_input",
        "Secure Keyboard Entry: other apps can't read what you type",
    ),
    (
        "make_default_terminal",
        "Makes Mightty the app that runs .command files and programs opened from Finder, and opens ssh:// links",
    ),
    ("about", "Shows Mightty's version and licence"),
    (
        "check_for_updates",
        "Checks for a newer version of Mightty and offers to install it",
    ),
    ("open_settings", "The settings, in a window of their own"),
    (
        "open_config",
        "The config file in your $EDITOR, in a new tab",
    ),
    ("reload_config", "Applies the config file now"),
    (
        "increase_font_size:1",
        "A bigger font in the pane, by points (or smaller)",
    ),
    ("reset_font_size", "The pane's font back to font-size"),
    (
        "clear_screen",
        "Clears history and the screen above the prompt",
    ),
    ("scroll_to_top", "To the top of the history"),
    ("scroll_to_bottom", "Back to the prompt"),
    ("scroll_page_up", "A page up the history"),
    ("scroll_page_down", "A page down the history"),
    (
        "jump_to_prompt:-1",
        "To the prompt before, or with 1 the one after; needs shell integration",
    ),
    (
        "text:hello\\n",
        "Sends text to the program (\\n, \\r, \\t, \\e, \\xHH)",
    ),
    ("csi:2J", "Sends ESC [ and the text"),
    ("esc:d", "Sends ESC and the text"),
    (
        "key_table:resize",
        "Switches to a key table, whose keys act until Escape: bind them as resize/left=…",
    ),
    ("end_key_table", "Leaves the key table, as Escape does"),
    ("ignore", "The key does nothing, not even reach the program"),
];

/// One `keybind`: keys pressed one after another, in a key table or the
/// main one, and what they do, in order.
#[derive(Clone, Debug, PartialEq)]
struct Binding {
    table: Option<String>,
    keys: Vec<Trigger>,
    actions: Vec<Action>,
}

/// The bindings in effect: defaults, then `keybind` settings in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Bindings {
    keys: Vec<Binding>,
    /// `global:TRIGGER=ACTION`: keys that act whichever app is active.
    global: Vec<(Trigger, Action)>,
    /// `catch_all=ACTION`, by key table (`None`: the main one).
    catch_all: Vec<(Option<String>, Vec<Action>)>,
}

impl Default for Bindings {
    fn default() -> Self {
        use Action::*;
        let mut list = vec![
            ("cmd+n", NewWindow),
            ("cmd+t", NewTab),
            ("cmd+d", NewSplit(Direction::Right)),
            ("cmd+shift+d", NewSplit(Direction::Down)),
            ("cmd+w", CloseSurface),
            ("cmd+shift+w", CloseWindow),
            ("cmd+m", MinimizeWindow),
            ("cmd+h", Hide),
            ("cmd+alt+h", HideOthers),
            ("cmd+q", Quit),
            ("cmd+c", Copy),
            ("cmd+x", Cut),
            ("cmd+v", Paste),
            ("cmd+f", StartSearch),
            ("cmd+g", NavigateSearch(1)),
            ("cmd+shift+g", NavigateSearch(-1)),
            ("cmd+shift+a", JumpToAttention),
            ("cmd+shift+enter", ToggleSplitZoom),
            ("cmd+ctrl+=", EqualizeSplits),
            ("cmd+[", GotoSplitStep(-1)),
            ("cmd+]", GotoSplitStep(1)),
            ("cmd+alt+left", GotoSplit(Direction::Left)),
            ("cmd+alt+right", GotoSplit(Direction::Right)),
            ("cmd+alt+up", GotoSplit(Direction::Up)),
            ("cmd+alt+down", GotoSplit(Direction::Down)),
            ("cmd+ctrl+left", ResizeSplit(Direction::Left, 10.0)),
            ("cmd+ctrl+right", ResizeSplit(Direction::Right, 10.0)),
            ("cmd+ctrl+up", ResizeSplit(Direction::Up, 10.0)),
            ("cmd+ctrl+down", ResizeSplit(Direction::Down, 10.0)),
            ("cmd+shift+[", PreviousTab),
            ("cmd+shift+]", NextTab),
            ("cmd+9", LastTab),
            ("cmd+,", OpenSettings),
            ("cmd+shift+,", ReloadConfig),
            ("cmd+shift+p", ToggleCommandPalette),
            ("cmd+z", Undo),
            ("cmd+shift+t", ReopenClosed),
            ("cmd+=", ChangeFontSize(1.0)),
            ("cmd+shift+=", ChangeFontSize(1.0)),
            ("cmd+-", ChangeFontSize(-1.0)),
            ("cmd+0", ResetFontSize),
            ("cmd+k", ClearScreen),
            ("cmd+home", ScrollToTop),
            ("cmd+end", ScrollToBottom),
            ("cmd+page_up", ScrollPageUp),
            ("cmd+page_down", ScrollPageDown),
            ("cmd+up", JumpToPrompt(-1)),
            ("cmd+down", JumpToPrompt(1)),
            ("cmd+left", Text(b"\x01".to_vec())),
            ("cmd+right", Text(b"\x05".to_vec())),
            ("alt+left", Text(b"\x1bb".to_vec())),
            ("alt+right", Text(b"\x1bf".to_vec())),
            ("cmd+backspace", Text(b"\x15".to_vec())),
            ("alt+delete", Text(b"\x1bd".to_vec())),
            ("cmd+delete", Text(b"\x0b".to_vec())),
        ];
        let tabs: Vec<(String, Action)> =
            (1..=8).map(|n| (format!("cmd+{n}"), GotoTab(n))).collect();
        let mut bindings: Vec<(Trigger, Action)> = list
            .drain(..)
            .map(|(t, a)| (Trigger::parse(t).expect("a valid default trigger"), a))
            .collect();
        bindings.extend(
            tabs.into_iter()
                .map(|(t, a)| (Trigger::parse(&t).expect("a valid default trigger"), a)),
        );
        Self {
            keys: bindings
                .into_iter()
                .map(|(trigger, action)| Binding {
                    table: None,
                    keys: vec![trigger],
                    actions: vec![action],
                })
                .collect(),
            global: Vec::new(),
            catch_all: Vec::new(),
        }
    }
}

/// What keys pressed so far are bound to.
#[derive(Debug, PartialEq)]
pub enum Lookup<'a> {
    Actions(&'a [Action]),
    /// The start of longer bindings: the next key decides.
    Prefix,
    Unbound,
}

impl Bindings {
    /// Applies one `keybind` value: `TRIGGER=ACTION`, `TRIGGER=unbind`, or
    /// `clear`, which drops every binding (the defaults too). A trigger
    /// starting `global:` acts whichever app is active. Triggers joined
    /// by `>` are pressed one after another, and `NAME/` before them
    /// puts the binding in a key table. Actions joined by `;` are done in
    /// order.
    pub fn apply(&mut self, value: &str) -> Result<(), String> {
        if value == "clear" {
            self.keys.clear();
            self.global.clear();
            self.catch_all.clear();
            return Ok(());
        }
        // The first `=` that isn't a trigger's key: after `+`, or after
        // `>` or `/` when another `=` follows (`ctrl+a>==…`, `resize/==…`).
        let is_key = |i: usize| {
            value[..i].ends_with('+')
                || (value[..i].ends_with(['>', '/']) && value[i + 1..].starts_with('='))
        };
        let at = value
            .char_indices()
            .find(|&(i, c)| c == '=' && i > 0 && !is_key(i))
            .map(|(i, _)| i)
            .ok_or("expects TRIGGER=ACTION, such as cmd+t=new_tab")?;
        let (trigger, action) = (value[..at].trim(), value[at + 1..].trim());
        if let Some(trigger) = trigger.strip_prefix("global:") {
            let trigger = Trigger::parse(trigger)?;
            self.global.retain(|(t, _)| *t != trigger);
            if action != "unbind" {
                let mut actions = split_actions(action);
                if actions.len() > 1 {
                    return Err("global keys take one action".into());
                }
                let action = Action::parse(actions.remove(0))?;
                if matches!(action, Action::KeyTable(_) | Action::EndKeyTable) {
                    return Err("global keys can't switch key tables".into());
                }
                self.global.push((trigger, action));
            }
            return Ok(());
        }
        // `resize/left`, but not `cmd+/` or `/`.
        let (table, keys) = match trigger.split_once('/') {
            Some((name, keys)) if !name.contains(['+', '>']) && !keys.is_empty() => {
                if !table_name(name) {
                    return Err(format!(
                        "`{name}`: a key table's name has letters, digits, - and _"
                    ));
                }
                (Some(name.to_owned()), keys)
            }
            _ => (None, trigger),
        };
        if keys.trim() == "catch_all" {
            self.catch_all.retain(|(t, _)| *t != table);
            if action != "unbind" {
                let actions = split_actions(action)
                    .into_iter()
                    .map(Action::parse)
                    .collect::<Result<Vec<_>, _>>()?;
                self.catch_all.push((table, actions));
            }
            return Ok(());
        }
        if keys.split('>').any(|k| k.trim() == "catch_all") {
            return Err("`catch_all` is a trigger of its own, not one of a sequence".into());
        }
        let keys = keys
            .split('>')
            .map(|k| Trigger::parse(k.trim()))
            .collect::<Result<Vec<_>, _>>()?;
        if action == "unbind" {
            // These keys, and longer bindings that start with them; the
            // shorter ones they start with stay.
            self.keys
                .retain(|b| b.table != table || !b.keys.starts_with(&keys));
            return Ok(());
        }
        let actions = split_actions(action)
            .into_iter()
            .map(Action::parse)
            .collect::<Result<Vec<_>, _>>()?;
        // A binding replaces those it would hide or be hidden by: the same
        // keys, ones that start with them, or ones they start with.
        self.keys.retain(|b| {
            b.table != table || !(b.keys.starts_with(&keys) || keys.starts_with(&b.keys))
        });
        self.keys.push(Binding {
            table,
            keys,
            actions,
        });
        Ok(())
    }

    /// The `global:` bindings.
    pub fn global(&self) -> &[(Trigger, Action)] {
        &self.global
    }

    /// What `keys`, pressed in turn, do in `table` (`None`: the main one).
    pub fn lookup(&self, table: Option<&str>, keys: &[Trigger]) -> Lookup<'_> {
        let mut prefix = false;
        for b in self.keys.iter().filter(|b| b.table.as_deref() == table) {
            if b.keys == keys {
                return Lookup::Actions(&b.actions);
            }
            prefix |= b.keys.len() > keys.len() && b.keys.starts_with(keys);
        }
        if prefix {
            Lookup::Prefix
        } else {
            Lookup::Unbound
        }
    }

    /// Whether the key table `name` has any bindings.
    pub fn has_table(&self, name: &str) -> bool {
        self.keys.iter().any(|b| b.table.as_deref() == Some(name))
            || self
                .catch_all
                .iter()
                .any(|(t, _)| t.as_deref() == Some(name))
    }

    /// What `table` (`None`: the main one) binds the keys it leaves
    /// unbound to, if anything.
    pub fn catch_all(&self, table: Option<&str>) -> Option<&[Action]> {
        self.catch_all
            .iter()
            .find(|(t, _)| t.as_deref() == table)
            .map(|(_, actions)| &actions[..])
    }

    /// What `trigger` alone does in the main table.
    #[cfg(test)]
    pub fn find(&self, trigger: Trigger) -> Option<&[Action]> {
        match self.lookup(None, &[trigger]) {
            Lookup::Actions(actions) => Some(actions),
            _ => None,
        }
    }

    /// The first key bound to `action` alone, for its menu item: one
    /// key, in the main table, doing nothing else.
    pub fn trigger_for(&self, action: &Action) -> Option<Trigger> {
        self.keys
            .iter()
            .find(|b| {
                b.table.is_none()
                    && b.keys.len() == 1
                    && b.actions.len() == 1
                    && b.actions[0] == *action
            })
            .map(|b| b.keys[0])
    }
}

/// What the keys pressed lately mean: a key table switched to, and the
/// first keys of a binding still waiting for the rest.
#[derive(Debug, Default)]
pub struct KeyState {
    table: Option<String>,
    pending: Vec<Trigger>,
}

/// What a key press does, by `KeyState::press`.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Run(Vec<Action>),
    /// Used up: the start of a binding, or Escape leaving one or a table.
    Taken,
    /// Used up, though bound to nothing: in a key table, or after the
    /// start of a binding. Worth a beep.
    Refused,
    /// Not a binding's: the key goes on as usual.
    Pass,
}

impl KeyState {
    pub fn press(&mut self, bindings: &Bindings, trigger: Trigger) -> Outcome {
        let mut keys = self.pending.clone();
        keys.push(trigger);
        match bindings.lookup(self.table.as_deref(), &keys) {
            Lookup::Actions(actions) => {
                self.pending.clear();
                Outcome::Run(actions.to_vec())
            }
            Lookup::Prefix => {
                self.pending = keys;
                Outcome::Taken
            }
            Lookup::Unbound => {
                let waiting = !std::mem::take(&mut self.pending).is_empty();
                let escape = trigger
                    == Trigger {
                        mods: 0,
                        key: Key::Named(Named::Escape),
                    };
                if escape && (waiting || self.table.is_some()) {
                    // Escape ends what was started, then the table.
                    if !waiting {
                        self.table = None;
                    }
                    Outcome::Taken
                } else if waiting {
                    Outcome::Refused
                } else if trigger.mods & CMD != 0 {
                    // The menus' Command keys still work.
                    Outcome::Pass
                } else if let Some(actions) = bindings.catch_all(self.table.as_deref()) {
                    Outcome::Run(actions.to_vec())
                } else if self.table.is_some() {
                    Outcome::Refused
                } else {
                    Outcome::Pass
                }
            }
        }
    }

    /// Switches to the key table `name`; false if it binds nothing.
    pub fn enter(&mut self, bindings: &Bindings, name: &str) -> bool {
        self.pending.clear();
        let known = bindings.has_table(name);
        if known {
            self.table = Some(name.to_owned());
        }
        known
    }

    /// Drops the keys waiting for the rest of a binding.
    pub fn cancel(&mut self) {
        self.pending.clear();
    }

    pub fn end(&mut self) {
        self.pending.clear();
        self.table = None;
    }

    /// The key table, and the keys waiting for the rest of a binding.
    pub fn status(&self) -> (Option<&str>, &[Trigger]) {
        (self.table.as_deref(), &self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(s: &str) -> Trigger {
        Trigger::parse(s).unwrap()
    }

    #[test]
    fn triggers() {
        assert_eq!(
            trigger("cmd+shift+d"),
            Trigger {
                mods: CMD | SHIFT,
                key: Key::Char('d')
            }
        );
        assert_eq!(
            trigger("ctrl+alt+page_up"),
            Trigger {
                mods: CTRL | ALT,
                key: Key::Named(Named::PageUp)
            }
        );
        assert_eq!(
            trigger("cmd++"),
            Trigger {
                mods: CMD,
                key: Key::Char('+')
            }
        );
        assert_eq!(trigger("cmd+plus"), trigger("cmd++"));
        assert_eq!(trigger("super+T"), trigger("cmd+t"));
        assert_eq!(
            trigger("f5"),
            Trigger {
                mods: 0,
                key: Key::Named(Named::F(5))
            }
        );
        assert!(Trigger::parse("hyper+t").is_err());
        assert!(Trigger::parse("cmd+tee").is_err());
    }

    #[test]
    fn events_match_triggers() {
        assert_eq!(Trigger::from_event(17, "t", CMD), Some(trigger("cmd+t")));
        assert_eq!(
            Trigger::from_event(33, "[", CMD | SHIFT),
            Some(trigger("cmd+shift+["))
        );
        assert_eq!(
            Trigger::from_event(116, "\u{F72C}", CMD),
            Some(trigger("cmd+page_up"))
        );
        assert_eq!(
            Trigger::from_event(36, "\r", CMD | SHIFT),
            Some(trigger("cmd+shift+enter"))
        );
    }

    #[test]
    fn actions() {
        assert_eq!(
            Action::parse("new_split:down"),
            Ok(Action::NewSplit(Direction::Down))
        );
        assert_eq!(
            Action::parse("resize_split:up,25"),
            Ok(Action::ResizeSplit(Direction::Up, 25.0))
        );
        assert_eq!(
            Action::parse("resize_split:left"),
            Ok(Action::ResizeSplit(Direction::Left, 10.0))
        );
        assert_eq!(
            Action::parse("decrease_font_size:2"),
            Ok(Action::ChangeFontSize(-2.0))
        );
        assert_eq!(
            Action::parse("increase_font_size"),
            Ok(Action::ChangeFontSize(1.0))
        );
        assert_eq!(Action::parse("goto_tab:3"), Ok(Action::GotoTab(3)));
        assert_eq!(
            Action::parse("set_tab_color:Prod"),
            Ok(Action::SetTabColor(Some("Prod".into())))
        );
        assert_eq!(
            Action::parse("set_tab_color:none"),
            Ok(Action::SetTabColor(None))
        );
        assert!(Action::parse("set_tab_color").is_err());
        assert_eq!(
            Action::parse(r"text:\x1b[A\n"),
            Ok(Action::Text(b"\x1b[A\n".to_vec()))
        );
        assert_eq!(
            Action::parse("csi:2J"),
            Ok(Action::Text(b"\x1b[2J".to_vec()))
        );
        for bad in [
            "new_split:left",
            "goto_tab:0",
            "new_tab:now",
            "text:\\q",
            "fly",
        ] {
            assert!(Action::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keybind_values() {
        let mut b = Bindings::default();
        assert_eq!(b.find(trigger("cmd+t")), Some(&[Action::NewTab][..]));
        assert_eq!(b.find(trigger("cmd+3")), Some(&[Action::GotoTab(3)][..]));
        assert_eq!(
            b.find(trigger("alt+left")),
            Some(&[Action::Text(b"\x1bb".to_vec())][..])
        );
        assert_eq!(
            b.find(trigger("cmd+right")),
            Some(&[Action::Text(b"\x05".to_vec())][..])
        );
        b.apply("cmd+t=new_window").unwrap();
        assert_eq!(b.find(trigger("cmd+t")), Some(&[Action::NewWindow][..]));
        assert_eq!(b.trigger_for(&Action::NewWindow), Some(trigger("cmd+n")));
        b.apply("cmd+n=unbind").unwrap();
        assert_eq!(b.trigger_for(&Action::NewWindow), Some(trigger("cmd+t")));
        b.apply("cmd+==reset_font_size").unwrap();
        assert_eq!(b.find(trigger("cmd+=")), Some(&[Action::ResetFontSize][..]));
        b.apply("ctrl+shift+t = new_tab").unwrap();
        assert_eq!(b.find(trigger("ctrl+shift+t")), Some(&[Action::NewTab][..]));
        assert!(b.apply("cmd+t").is_err());
        assert!(b.apply("cmd+t=fly").is_err());
        // Global keys are kept apart: they act from any app.
        b.apply("global:ctrl+grave_accent=toggle_quick_terminal")
            .unwrap();
        assert_eq!(
            b.global(),
            [(trigger("ctrl+grave_accent"), Action::ToggleQuickTerminal)]
        );
        assert_eq!(b.global()[0].0.to_string(), "ctrl+grave_accent");
        assert_eq!(trigger("cmd+shift+f5").to_string(), "cmd+shift+f5");
        assert_eq!(b.find(trigger("ctrl+grave_accent")), None);
        b.apply("global:ctrl+grave_accent=unbind").unwrap();
        assert!(b.global().is_empty());
        b.apply("clear").unwrap();
        assert_eq!(b.find(trigger("cmd+w")), None);
    }

    #[test]
    fn several_actions() {
        let mut b = Bindings::default();
        b.apply("cmd+k=clear_screen; text:\\x0c").unwrap();
        assert_eq!(
            b.find(trigger("cmd+k")),
            Some(&[Action::ClearScreen, Action::Text(b"\x0c".to_vec())][..])
        );
        // A `;` that doesn't start an action is the argument's.
        b.apply("ctrl+up=csi:1;5A").unwrap();
        assert_eq!(
            b.find(trigger("ctrl+up")),
            Some(&[Action::Text(b"\x1b[1;5A".to_vec())][..])
        );
        b.apply("ctrl+left=csi:1;5D ; new_tab").unwrap();
        assert_eq!(
            b.find(trigger("ctrl+left")),
            Some(&[Action::Text(b"\x1b[1;5D".to_vec()), Action::NewTab][..])
        );
        // Menus show only keys doing their action alone.
        assert_eq!(b.trigger_for(&Action::ClearScreen), None);
        assert!(b.apply("cmd+k=clear_screen; fly").is_err());
        assert!(
            b.apply("global:ctrl+grave_accent=new_tab; new_window")
                .is_err()
        );
    }

    #[test]
    fn sequences_and_tables() {
        let mut b = Bindings::default();
        b.apply("ctrl+a>c=new_tab").unwrap();
        b.apply("ctrl+a>ctrl+a=text:\\x01").unwrap();
        b.apply("ctrl+a>r=key_table:resize").unwrap();
        b.apply("resize/left=resize_split:left,10").unwrap();
        b.apply("resize/enter=end_key_table").unwrap();
        b.apply("resize//=equalize_splits").unwrap();
        // `cmd+/` and `cmd+>=…` are keys, not a table or a sequence.
        b.apply("cmd+/=new_window").unwrap();
        assert_eq!(b.find(trigger("cmd+/")), Some(&[Action::NewWindow][..]));
        b.apply("ctrl+b>==reset_font_size").unwrap();
        assert_eq!(
            b.lookup(None, &[trigger("ctrl+b"), trigger("=")]),
            Lookup::Actions(&[Action::ResetFontSize])
        );
        assert!(b.apply("re size/left=new_tab").is_err());
        assert!(b.apply("ctrl+a>r=key_table:a b").is_err());
        assert!(b.apply("global:ctrl+a=key_table:resize").is_err());

        let a = trigger("ctrl+a");
        assert_eq!(b.lookup(None, &[a]), Lookup::Prefix);
        assert_eq!(
            b.lookup(None, &[a, trigger("c")]),
            Lookup::Actions(&[Action::NewTab])
        );
        assert!(b.has_table("resize"));
        assert!(!b.has_table("move"));

        let mut keys = KeyState::default();
        let press = |keys: &mut KeyState, t: &str| keys.press(&b, trigger(t));
        // Keys not bound go on as usual.
        assert_eq!(press(&mut keys, "x"), Outcome::Pass);
        assert_eq!(press(&mut keys, "ctrl+a"), Outcome::Taken);
        assert_eq!(keys.status(), (None, &[a][..]));
        assert_eq!(press(&mut keys, "c"), Outcome::Run(vec![Action::NewTab]));
        assert_eq!(keys.status(), (None, &[][..]));
        // After a start, an unbound key ends it, and Escape cancels it.
        press(&mut keys, "ctrl+a");
        assert_eq!(press(&mut keys, "z"), Outcome::Refused);
        assert_eq!(press(&mut keys, "c"), Outcome::Pass);
        press(&mut keys, "ctrl+a");
        assert_eq!(press(&mut keys, "escape"), Outcome::Taken);
        assert_eq!(keys.status(), (None, &[][..]));
        // A table's keys act until Escape; others are refused, but for
        // the menus' Command keys.
        assert!(!keys.enter(&b, "move"));
        assert!(keys.enter(&b, "resize"));
        assert_eq!(
            press(&mut keys, "left"),
            Outcome::Run(vec![Action::ResizeSplit(Direction::Left, 10.0)])
        );
        assert_eq!(
            press(&mut keys, "left"),
            Outcome::Run(vec![Action::ResizeSplit(Direction::Left, 10.0)])
        );
        assert_eq!(press(&mut keys, "c"), Outcome::Refused);
        assert_eq!(press(&mut keys, "cmd+w"), Outcome::Pass);
        assert_eq!(press(&mut keys, "escape"), Outcome::Taken);
        assert_eq!(keys.status(), (None, &[][..]));
        assert_eq!(press(&mut keys, "left"), Outcome::Pass);
        keys.enter(&b, "resize");
        keys.end();
        assert_eq!(keys.status(), (None, &[][..]));

        // A binding replaces those that would hide it, or that it would.
        b.apply("ctrl+a=new_window").unwrap();
        assert_eq!(b.find(a), Some(&[Action::NewWindow][..]));
        assert_eq!(b.lookup(None, &[a, trigger("c")]), Lookup::Unbound);
        b.apply("ctrl+a>c=new_tab").unwrap();
        assert_eq!(b.find(a), None);
        b.apply("ctrl+a>c=unbind").unwrap();
        assert_eq!(b.lookup(None, &[a]), Lookup::Unbound);
        // Tables are kept apart.
        assert!(b.has_table("resize"));
        // Unbinding a longer binding keeps the one it starts with.
        b.apply("cmd+k>x=new_tab").unwrap();
        b.apply("cmd+k=clear_screen").unwrap();
        b.apply("cmd+k>x=unbind").unwrap();
        assert_eq!(b.find(trigger("cmd+k")), Some(&[Action::ClearScreen][..]));
        // A sequence ending in `/` isn't a table.
        b.apply("g>/=new_tab").unwrap();
        assert_eq!(
            b.lookup(None, &[trigger("g"), trigger("/")]),
            Lookup::Actions(&[Action::NewTab])
        );
        // A bad action leaves the bindings as they were.
        assert!(b.apply("cmd+k=fly").is_err());
        assert_eq!(b.find(trigger("cmd+k")), Some(&[Action::ClearScreen][..]));
    }

    #[test]
    fn catch_all() {
        let mut b = Bindings::default();
        b.apply("ctrl+a>r=key_table:resize").unwrap();
        b.apply("resize/left=resize_split:left,10").unwrap();
        b.apply("resize/catch_all=end_key_table").unwrap();
        assert!(b.apply("ctrl+a>catch_all=new_tab").is_err());
        assert!(b.apply("global:catch_all=new_tab").is_err());

        let mut keys = KeyState::default();
        let press = |keys: &mut KeyState, t: &str| keys.press(&b, trigger(t));
        // Without one in the main table, keys go on as usual.
        assert_eq!(press(&mut keys, "x"), Outcome::Pass);
        keys.enter(&b, "resize");
        assert_eq!(
            press(&mut keys, "left"),
            Outcome::Run(vec![Action::ResizeSplit(Direction::Left, 10.0)])
        );
        assert_eq!(
            press(&mut keys, "x"),
            Outcome::Run(vec![Action::EndKeyTable])
        );
        // Command keys stay the menus', and Escape still leaves.
        assert_eq!(press(&mut keys, "cmd+w"), Outcome::Pass);
        assert_eq!(press(&mut keys, "escape"), Outcome::Taken);
        assert_eq!(keys.status(), (None, &[][..]));
        // After the start of a binding, a key that doesn't finish it
        // is still refused.
        assert_eq!(press(&mut keys, "ctrl+a"), Outcome::Taken);
        assert_eq!(press(&mut keys, "z"), Outcome::Refused);

        // A table bound only by its catch-all is a table.
        b.apply("swallow/catch_all=ignore").unwrap();
        assert!(b.has_table("swallow"));
        b.apply("catch_all=ignore").unwrap();
        assert_eq!(b.catch_all(None), Some(&[Action::Ignore][..]));
        assert_eq!(
            KeyState::default().press(&b, trigger("x")),
            Outcome::Run(vec![Action::Ignore])
        );
        b.apply("catch_all=unbind").unwrap();
        assert_eq!(b.catch_all(None), None);
        b.apply("clear").unwrap();
        assert!(!b.has_table("swallow"));
    }

    #[test]
    fn documented_actions_parse() {
        for (name, _) in ACTIONS {
            assert!(Action::parse(name).is_ok(), "{name}");
        }
    }
}
