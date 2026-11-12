//! `mightty config`: the settings, in the terminal. Every setting by
//! section, with its value and help; Return edits one (toggles a BOOL),
//! values a setting takes from a few (or a theme or font) suggested as
//! they're typed, → or Tab taking the suggestion,
//! `d` puts it back to its default, `/` searches, `e` opens the config file
//! in `$EDITOR`. The theme and the fonts get a picker that previews each on
//! this pane as the selection moves: a theme by OSC 4 and 10–12, a font by
//! OSC 50, mightty's own; both undone on leaving. Changes
//! are written into the config file, its comments and order kept; a
//! running mightty applies them as the file changes.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::Command;

use crate::config::{self, Config, SettingView};

/// `struct termios` on macOS.
#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    iflag: u64,
    oflag: u64,
    cflag: u64,
    lflag: u64,
    cc: [u8; 20],
    ispeed: u64,
    ospeed: u64,
}

#[repr(C)]
#[derive(Default)]
struct Winsize {
    rows: u16,
    cols: u16,
    x: u16,
    y: u16,
}

unsafe extern "C" {
    fn tcgetattr(fd: i32, termios: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, when: i32, termios: *const Termios) -> i32;
    fn cfmakeraw(termios: *mut Termios);
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

const TIOCGWINSZ: u64 = 0x4008_7468;
const VMIN: usize = 16;
const VTIME: usize = 17;

/// The terminal in raw mode, on the alternate screen, until dropped.
struct Raw {
    saved: Termios,
}

impl Raw {
    fn enter() -> io::Result<Self> {
        // SAFETY: a `struct termios` filled by tcgetattr, then set back as changed.
        unsafe {
            let mut saved: Termios = std::mem::zeroed();
            if tcgetattr(0, &mut saved) != 0 {
                return Err(io::Error::other("mightty config needs a terminal"));
            }
            let mut raw = saved;
            cfmakeraw(&mut raw);
            // Reads return after a tenth of a second without keys, so a resize redraws.
            raw.cc[VMIN] = 0;
            raw.cc[VTIME] = 1;
            tcsetattr(0, 0, &raw);
            print!("\x1b[?1049h\x1b[?25l");
            io::stdout().flush()?;
            Ok(Self { saved })
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        print!("\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = io::stdout().flush();
        // SAFETY: the settings saved on entering.
        unsafe { tcsetattr(0, 0, &self.saved) };
    }
}

fn size() -> (usize, usize) {
    let mut ws = Winsize::default();
    // SAFETY: TIOCGWINSZ fills a `struct winsize`.
    unsafe { ioctl(1, TIOCGWINSZ, &mut ws as *mut Winsize) };
    (ws.cols.max(40) as usize, ws.rows.max(10) as usize)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Escape,
    Backspace,
    Tab,
    Char(char),
    Ctrl(char),
}

/// The keys in `bytes`, as a terminal sends them.
fn keys(bytes: &[u8]) -> Vec<Key> {
    let mut out = Vec::new();
    let text = String::from_utf8_lossy(bytes);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(match c {
            '\x1b' if chars.peek().is_some_and(|&n| n == '[' || n == 'O') => {
                chars.next();
                let mut seq = String::new();
                for n in chars.by_ref() {
                    seq.push(n);
                    if n.is_ascii_alphabetic() || n == '~' {
                        break;
                    }
                }
                match seq.as_str() {
                    "A" => Key::Up,
                    "B" => Key::Down,
                    "C" => Key::Right,
                    "D" => Key::Left,
                    "H" | "1~" => Key::Home,
                    "F" | "4~" => Key::End,
                    "5~" => Key::PageUp,
                    "6~" => Key::PageDown,
                    _ => continue,
                }
            }
            '\x1b' => Key::Escape,
            '\r' | '\n' => Key::Enter,
            '\x7f' | '\x08' => Key::Backspace,
            '\t' => Key::Tab,
            c if (c as u32) < 0x20 => Key::Ctrl((b'a' + c as u8 - 1) as char),
            c => Key::Char(c),
        });
    }
    out
}

/// A setting's section, by its name.
fn section(name: &str) -> &'static str {
    match name {
        "theme" | "background" | "foreground" | "cursor-color" | "cursor-text" | "palette"
        | "window-colorspace" => "Colours",
        n if n.starts_with("selection-") || n.starts_with("search-") => "Colours",
        n if n.starts_with("font") || n.starts_with("adjust-cell") || n == "grapheme-width" => {
            "Font"
        }
        n if n.starts_with("quick-terminal") => "Quick terminal",
        n if n.starts_with("bell") => "Bell",
        "keybind" | "macos-option-as-alt" => "Keys",
        n if n.starts_with("window-")
            || n.starts_with("background-")
            || n.starts_with("cursor-style")
            || n == "macos-titlebar-style"
            || n == "unfocused-split-opacity"
            || n == "scrollbar" =>
        {
            "Window"
        }
        n if n.starts_with("clipboard")
            || n.starts_with("mouse-")
            || n == "copy-on-select"
            || n == "password-mask"
            || n == "macos-auto-secure-input" =>
        {
            "Behaviour"
        }
        _ => "General",
    }
}

const SECTIONS: &[&str] = &[
    "Colours",
    "Font",
    "Window",
    "General",
    "Behaviour",
    "Keys",
    "Bell",
    "Quick terminal",
];

/// What a picker picks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    Theme,
    /// For `font-family` (or a bold or italic one); monospaced ones only unless `all`.
    Font {
        setting: &'static str,
        all: bool,
    },
}

enum Mode {
    List,
    /// Typing a search.
    Search,
    /// Typing a value for the selected setting, and the suggestion picked.
    Edit {
        buffer: String,
        pick: usize,
    },
    /// Choosing a theme or a font: the filter typed, the one selected.
    Pick {
        what: Pick,
        filter: String,
        selected: usize,
    },
}

struct App {
    path: PathBuf,
    settings: Vec<SettingView>,
    /// Indexes into `settings`, by section, matching the search.
    shown: Vec<usize>,
    selected: usize,
    top: usize,
    query: String,
    mode: Mode,
    status: String,
    themes: Vec<String>,
    /// A theme or font previewed on this pane, so leaving can undo it.
    previewing: bool,
    previewing_font: bool,
    /// The fonts installed, by family, and those monospaced, once wanted.
    fonts: Option<Vec<String>>,
    mono: Option<Vec<String>>,
}

/// The families of `fonts` whose regular face is monospaced: those made for terminals.
fn monospaced(fonts: &[String]) -> Vec<String> {
    use crate::ffi::{CFRelease, CTFontCreateWithName, CTFontGetSymbolicTraits, cfstring};
    const MONO_SPACE: u32 = 1 << 10; // kCTFontTraitMonoSpace
    fonts
        .iter()
        .filter(|name| {
            // SAFETY: a CFString and a CTFont made here and released.
            unsafe {
                let cf = cfstring(name);
                let font = CTFontCreateWithName(cf, 12.0, std::ptr::null());
                CFRelease(cf);
                if font.is_null() {
                    return false;
                }
                let mono = CTFontGetSymbolicTraits(font) & MONO_SPACE != 0;
                CFRelease(font);
                mono
            }
        })
        .cloned()
        .collect()
}

/// The font families installed, sorted.
fn font_families() -> Vec<String> {
    use crate::ffi::{
        CFArrayGetCount, CFArrayGetValueAtIndex, CFRelease,
        CTFontManagerCopyAvailableFontFamilyNames, string_from_ns,
    };
    // SAFETY: a CFArray of CFStrings (bridged to NSString), released after.
    unsafe {
        let names = CTFontManagerCopyAvailableFontFamilyNames();
        if names.is_null() {
            return Vec::new();
        }
        let mut out: Vec<String> = (0..CFArrayGetCount(names))
            .map(|i| string_from_ns(CFArrayGetValueAtIndex(names, i) as _))
            .filter(|n| !n.starts_with('.'))
            .collect();
        CFRelease(names);
        out.sort_by_key(|n| n.to_lowercase());
        out.dedup();
        out
    }
}

/// `mightty config --export`: every setting (its section, value kind,
/// default, choices and help) and every action (its default key and what
/// it does), as JSON, for the docs.
pub fn export() -> String {
    use crate::bindings::{ACTIONS, Action, Bindings};
    use mightty_json::Value;
    let defaults = Config::default();
    let settings: Vec<Value> = defaults
        .settings()
        .into_iter()
        .map(|s| {
            let choices: Vec<Value> = config::choices(s.name)
                .iter()
                .map(|c| Value::from(*c))
                .collect();
            Value::object([
                ("name", Value::from(s.name)),
                ("section", Value::from(section(s.name))),
                ("value", Value::from(s.value)),
                ("default", Value::from(s.current)),
                ("choices", Value::from(choices)),
                ("many", Value::from(s.many)),
                ("help", Value::from(s.help)),
            ])
        })
        .collect();
    let bindings = Bindings::default();
    let actions: Vec<Value> = ACTIONS
        .iter()
        .map(|&(name, help)| {
            let key = Action::parse(name)
                .ok()
                .and_then(|a| bindings.trigger_for(&a))
                .map(crate::palette::symbols);
            Value::object([
                ("name", Value::from(name)),
                ("key", Value::from(key)),
                ("help", Value::from(help)),
            ])
        })
        .collect();
    let sections: Vec<Value> = SECTIONS.iter().map(|s| Value::from(*s)).collect();
    Value::object([
        ("version", Value::from(env!("CARGO_PKG_VERSION"))),
        ("sections", Value::from(sections)),
        ("settings", Value::from(settings)),
        ("actions", Value::from(actions)),
    ])
    .to_string()
}

pub fn main() -> io::Result<()> {
    let path = config::config_dir()
        .ok_or_else(|| io::Error::other("HOME is unset"))?
        .join("config");
    let mut app = App {
        path,
        settings: Vec::new(),
        shown: Vec::new(),
        selected: 0,
        top: 0,
        query: String::new(),
        mode: Mode::List,
        status: String::new(),
        themes: config::theme_names(),
        previewing: false,
        previewing_font: false,
        fonts: None,
        mono: None,
    };
    app.reload();
    let raw = Raw::enter()?;
    let mut buf = [0u8; 256];
    // Drawn after keys, or the window being resized; not while idle.
    let (mut drawn, mut dirty) = (None, true);
    loop {
        let now = size();
        if dirty || drawn != Some(now) {
            app.draw();
            (drawn, dirty) = (Some(now), false);
        }
        let n = io::stdin().read(&mut buf)?;
        if n == 0 {
            continue;
        }
        dirty = true;
        let mut quit = false;
        for key in keys(&buf[..n]) {
            quit |= app.key(key);
        }
        if quit {
            break;
        }
    }
    app.preview(None);
    drop(raw);
    Ok(())
}

impl App {
    /// The settings as the file has them now.
    fn reload(&mut self) {
        let config = match Config::load(&[]) {
            Ok((config, _)) => config,
            Err(e) => {
                self.status = e;
                Config::default()
            }
        };
        self.settings = config.settings();
        self.filter();
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        self.shown = SECTIONS
            .iter()
            .flat_map(|&sect| {
                let query = &query;
                self.settings.iter().enumerate().filter(move |(_, s)| {
                    section(s.name) == sect
                        && (query.is_empty()
                            || s.name.contains(query.as_str())
                            || s.help.to_lowercase().contains(query.as_str()))
                })
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
    }

    fn current(&self) -> Option<&SettingView> {
        self.shown.get(self.selected).map(|&i| &self.settings[i])
    }

    /// What a picker lists.
    fn items(&mut self, what: Pick) -> Vec<String> {
        match what {
            Pick::Theme => self.themes.clone(),
            Pick::Font { all: true, .. } => self.fonts.get_or_insert_with(font_families).clone(),
            Pick::Font { all: false, .. } => {
                if self.mono.is_none() {
                    let fonts = self.fonts.get_or_insert_with(font_families).clone();
                    self.mono = Some(monospaced(&fonts));
                }
                self.mono.clone().unwrap_or_default()
            }
        }
    }

    /// Those holding `filter`, ignoring case.
    fn matching(&mut self, what: Pick, filter: &str) -> Vec<String> {
        let filter = filter.to_lowercase();
        self.items(what)
            .into_iter()
            .filter(|i| i.to_lowercase().contains(&filter))
            .collect()
    }

    /// Handles `key`; true to quit.
    fn key(&mut self, key: Key) -> bool {
        match &mut self.mode {
            Mode::List => match key {
                Key::Char('q') | Key::Escape | Key::Ctrl('c') => return true,
                Key::Up | Key::Char('k') => self.selected = self.selected.saturating_sub(1),
                Key::Down | Key::Char('j') => {
                    self.selected = (self.selected + 1).min(self.shown.len().saturating_sub(1))
                }
                Key::PageUp => self.selected = self.selected.saturating_sub(10),
                Key::PageDown => {
                    self.selected = (self.selected + 10).min(self.shown.len().saturating_sub(1))
                }
                Key::Home => self.selected = 0,
                Key::End => self.selected = self.shown.len().saturating_sub(1),
                Key::Tab => self.next_section(),
                // A fresh search each time.
                Key::Char('/') => {
                    self.query.clear();
                    self.filter();
                    self.mode = Mode::Search;
                }
                Key::Char('t') => self.open_picker(Pick::Theme),
                Key::Char('e') => self.edit_file(),
                Key::Char('d') => self.reset(),
                Key::Enter => self.start_edit(),
                _ => {}
            },
            Mode::Search => match key {
                // Return keeps what it found; Escape goes back to everything.
                Key::Enter => self.mode = Mode::List,
                Key::Escape => {
                    self.query.clear();
                    self.filter();
                    self.mode = Mode::List;
                }
                Key::Backspace => {
                    self.query.pop();
                    self.filter();
                }
                Key::Char(c) => {
                    self.query.push(c);
                    self.selected = 0;
                    self.filter();
                }
                _ => {}
            },
            Mode::Edit { buffer, pick } => match key {
                Key::Escape => self.mode = Mode::List,
                Key::Backspace => {
                    buffer.pop();
                    *pick = 0;
                }
                Key::Ctrl('u') => {
                    buffer.clear();
                    *pick = 0;
                }
                Key::Char(c) => {
                    buffer.push(c);
                    *pick = 0;
                }
                Key::Up => *pick = pick.saturating_sub(1),
                Key::Down => *pick += 1,
                // The suggestion taken, as typed out.
                Key::Right | Key::Tab => {
                    let (buffer, pick) = (buffer.clone(), *pick);
                    let (head, _, matches) = self.suggest(&buffer);
                    if let Some(found) = matches.get(pick.min(matches.len().saturating_sub(1))) {
                        self.mode = Mode::Edit {
                            buffer: format!("{head}{found}"),
                            pick: 0,
                        };
                    }
                }
                Key::Enter => {
                    let value = buffer.trim().to_owned();
                    let Some(name) = self.current().map(|s| s.name) else {
                        return false;
                    };
                    // A font that isn't installed would only fall back to the default.
                    if name.starts_with("font-family") && !value.is_empty() {
                        let fonts = self.fonts.get_or_insert_with(font_families);
                        if !fonts.iter().any(|f| f.eq_ignore_ascii_case(&value)) {
                            self.status =
                                format!("{name}: no font family named {value} is installed");
                            return false;
                        }
                    }
                    self.mode = Mode::List;
                    self.save(name, Some(&value));
                }
                _ => {}
            },
            Mode::Pick {
                what,
                filter,
                selected,
            } => {
                let what_now = *what;
                match key {
                    Key::Escape | Key::Ctrl('c') => {
                        self.mode = Mode::List;
                        self.preview(None);
                        return false;
                    }
                    // Monospaced fonts only, or all of them.
                    Key::Tab => {
                        if let Pick::Font { all, .. } = what {
                            *all = !*all;
                            *selected = 0;
                        }
                    }
                    Key::Up => *selected = selected.saturating_sub(1),
                    Key::Down => *selected += 1,
                    Key::PageUp => *selected = selected.saturating_sub(10),
                    Key::PageDown => *selected += 10,
                    Key::Backspace => {
                        filter.pop();
                        *selected = 0;
                    }
                    Key::Char(c) => {
                        filter.push(c);
                        *selected = 0;
                    }
                    Key::Enter => {
                        let (filter, at) = (filter.clone(), *selected);
                        let name = self.matching(what_now, &filter).get(at).cloned();
                        self.mode = Mode::List;
                        // The window takes it as mightty reloads; this pane's preview goes.
                        self.preview(None);
                        if let Some(name) = name {
                            let setting = match what_now {
                                Pick::Theme => "theme",
                                Pick::Font { setting, .. } => setting,
                            };
                            self.save(setting, Some(&name));
                        }
                        return false;
                    }
                    _ => {}
                }
                let Mode::Pick {
                    what,
                    filter,
                    selected,
                } = &self.mode
                else {
                    return false;
                };
                let (what, filter, at) = (*what, filter.clone(), *selected);
                let matching = self.matching(what, &filter);
                let at = at.min(matching.len().saturating_sub(1));
                if let Mode::Pick { selected, .. } = &mut self.mode {
                    *selected = at;
                }
                let name = matching.get(at).cloned();
                self.preview_pick(what, name.as_deref());
            }
        }
        false
    }

    fn next_section(&mut self) {
        let Some(here) = self.current().map(|s| section(s.name)) else {
            return;
        };
        let next = self.shown.iter().position(|&i| {
            section(self.settings[i].name) != here
                && self.shown.iter().position(|&j| j == i) > Some(self.selected)
        });
        self.selected = next.unwrap_or(0);
    }

    fn start_edit(&mut self) {
        let Some(s) = self.current() else { return };
        if s.name == "theme" {
            return self.open_picker(Pick::Theme);
        }
        if s.name.starts_with("font-family") {
            let setting = s.name;
            return self.open_picker(Pick::Font {
                setting,
                all: false,
            });
        }
        if s.many {
            self.status = format!(
                "{} can be given many times: edit it in the file (e)",
                s.name
            );
            return;
        }
        if s.value == "BOOL" {
            let on = s.current.as_deref() == Some("true");
            let name = s.name;
            return self.save(name, Some(if on { "false" } else { "true" }));
        }
        self.mode = Mode::Edit {
            buffer: s.current.clone().unwrap_or_default(),
            pick: 0,
        };
    }

    /// The values the selected setting can take, if from a few: its
    /// choices, the themes, or the fonts installed.
    fn options(&mut self) -> Vec<String> {
        let Some(name) = self.current().map(|s| s.name) else {
            return Vec::new();
        };
        if name == "theme" {
            return self.themes.clone();
        }
        if name.starts_with("font-family") {
            return self.fonts.get_or_insert_with(font_families).clone();
        }
        config::choices(name)
            .iter()
            .map(|c| (*c).to_owned())
            .collect()
    }

    /// For what's typed: the part kept (before the last comma, in a comma
    /// list), the word being typed, and the values it could be: those
    /// starting with it first, then those holding it, ignoring case.
    fn suggest(&mut self, buffer: &str) -> (String, String, Vec<String>) {
        let options = self.options();
        let list = self.current().is_some_and(|s| s.name == "bell-features");
        let (head, word) = match buffer.rfind(',').filter(|_| list) {
            Some(at) => (
                buffer[..=at].to_owned(),
                buffer[at + 1..].trim_start().to_owned(),
            ),
            None => (String::new(), buffer.to_owned()),
        };
        let lower = word.to_lowercase();
        let (mut starting, mut holding) = (Vec::new(), Vec::new());
        for option in options {
            let o = option.to_lowercase();
            if o.starts_with(&lower) {
                starting.push(option);
            } else if !lower.is_empty() && o.contains(&lower) {
                holding.push(option);
            }
        }
        starting.extend(holding);
        (head, word, starting)
    }

    /// A picker, on the current value.
    fn open_picker(&mut self, what: Pick) {
        let setting = match what {
            Pick::Theme => "theme",
            Pick::Font { setting, .. } => setting,
        };
        let current = self
            .settings
            .iter()
            .find(|s| s.name == setting)
            .and_then(|s| s.current.clone());
        let items = self.items(what);
        let selected = current
            .and_then(|c| items.iter().position(|t| t.eq_ignore_ascii_case(&c)))
            .unwrap_or(0);
        self.mode = Mode::Pick {
            what,
            filter: String::new(),
            selected,
        };
        let name = items.get(selected).cloned();
        self.preview_pick(what, name.as_deref());
    }

    /// Shows `name` on this pane, as a theme or a font, as `what` picks.
    fn preview_pick(&mut self, what: Pick, name: Option<&str>) {
        match what {
            Pick::Theme => self.preview(name),
            Pick::Font { .. } => {
                // mightty's OSC 50: this pane's font family, the configured one when empty.
                print!("\x1b]50;{}\x07", name.unwrap_or(""));
                self.previewing_font = name.is_some();
            }
        }
    }

    /// Checks and writes `name = value` (or takes it out, for `None`).
    fn save(&mut self, name: &str, value: Option<&str>) {
        if let Some(Err(e)) = value.map(|value| config::check(name, value)) {
            self.status = e;
            return;
        }
        self.status = match config::write_setting(&self.path, name, value) {
            Ok(()) => match value {
                Some(value) => format!("Saved {name} = {value}"),
                None => format!("{name} is back to its default"),
            },
            Err(e) => format!("{}: {e}", self.path.display()),
        };
        self.reload();
    }

    fn reset(&mut self) {
        if let Some(name) = self.current().map(|s| s.name) {
            self.save(name, None);
        }
    }

    /// The config file in `$VISUAL` or `$EDITOR`, through the login shell
    /// for its environment; back here after.
    fn edit_file(&mut self) {
        print!("\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = io::stdout().flush();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let status = Command::new(shell)
            .args(["-lic", config::EDIT_COMMAND])
            .env("MIGHTTY_EDIT", &self.path)
            .status();
        print!("\x1b[?1049h\x1b[?25l");
        let _ = io::stdout().flush();
        self.status = match status {
            Ok(_) => "Back from the editor".into(),
            Err(e) => format!("The editor couldn't start: {e}"),
        };
        self.reload();
    }

    /// Shows theme `name` on this pane (its colours, by OSC), or the
    /// window's own colours again.
    fn preview(&mut self, name: Option<&str>) {
        let mut out = String::new();
        if name.is_none() && std::mem::take(&mut self.previewing_font) {
            out += "\x1b]50;\x07";
        }
        if self.previewing || name.is_some() {
            out += "\x1b]104\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07";
            self.previewing = false;
        }
        if let Some(text) = name
            .and_then(config::theme_path)
            .and_then(|p| std::fs::read_to_string(p).ok())
        {
            for line in text.lines() {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let value = value.trim();
                match key.trim() {
                    "palette" => {
                        if let Some((n, color)) = value.split_once('=') {
                            out += &format!("\x1b]4;{};{}\x07", n.trim(), color.trim());
                        }
                    }
                    "foreground" => out += &format!("\x1b]10;{value}\x07"),
                    "background" => out += &format!("\x1b]11;{value}\x07"),
                    "cursor-color" => out += &format!("\x1b]12;{value}\x07"),
                    _ => {}
                }
            }
            self.previewing = true;
        }
        print!("{out}");
    }

    fn draw(&mut self) {
        let (cols, rows) = size();
        let mut out = String::from("\x1b[H\x1b[0m");
        let line = |out: &mut String, text: &str| {
            out.push_str(text);
            out.push_str("\x1b[0m\x1b[K\r\n");
        };
        let fit = |text: &str, width: usize| -> String {
            let mut t: String = text.chars().take(width).collect();
            let n = t.chars().count();
            if n < width {
                t.extend(std::iter::repeat_n(' ', width - n));
            }
            t
        };
        // The file, home as ~, cut to fit.
        let home = std::env::var("HOME").unwrap_or_default();
        let shown = self.path.display().to_string();
        let shown = match shown.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => shown,
        };
        line(
            &mut out,
            &format!(
                "\x1b[1m Mightty settings\x1b[0m\x1b[2m  {}",
                fit(&shown, cols.saturating_sub(20))
            ),
        );
        line(&mut out, "");
        let body = rows.saturating_sub(7);
        match &self.mode {
            Mode::Pick {
                what,
                filter,
                selected,
            } => {
                let (what, filter, selected) = (*what, filter.clone(), &selected.clone());
                let matching = self.matching(what, &filter);
                let title = match what {
                    Pick::Theme => format!("Theme  {} themes", matching.len()),
                    Pick::Font { setting, all } => format!(
                        "{setting}  {} {}fonts",
                        matching.len(),
                        if all { "" } else { "monospaced " }
                    ),
                };
                line(
                    &mut out,
                    &format!(" \x1b[1m{title}\x1b[0m   filter: {filter}\x1b[7m \x1b[0m"),
                );
                let top = selected
                    .saturating_sub(body.saturating_sub(1) / 2)
                    .min(matching.len().saturating_sub(body.saturating_sub(1)));
                let list = 34.min(cols / 2);
                for r in 0..body.saturating_sub(1) {
                    let name = matching.get(top + r).map(String::as_str).unwrap_or("");
                    let mark = if top + r == *selected { "\x1b[7m" } else { "" };
                    // A sample beside the list: the palette, and text in it.
                    let font = matches!(what, Pick::Font { .. });
                    let sample = match r {
                        1 if !font => (0..8).map(|i| format!("\x1b[48;5;{i}m   ")).collect::<String>() + "\x1b[0m",
                        2 if !font => (8..16).map(|i| format!("\x1b[48;5;{i}m   ")).collect::<String>() + "\x1b[0m",
                        1 => "The quick brown fox jumps over the lazy dog".into(),
                        2 => "0O 1lI| {}[]() -> => != === >= <= ::".into(),
                        11 if font => "\u{f07c} \u{e0a0} \u{f489} \u{e7a8} \u{f121}   日本語 中文 한국어   ✓ ✗ ❯".into(),
                        4 => "\x1b[35mfn\x1b[0m \x1b[34mmain\x1b[0m() {".into(),
                        5 => "    \x1b[33mlet\x1b[0m greeting = \x1b[32m\"hello\"\x1b[0m;  \x1b[90m// a comment\x1b[0m".into(),
                        6 => "    \x1b[36mprintln!\x1b[0m(\x1b[32m\"{greeting}, {}\"\x1b[0m, \x1b[31m42\x1b[0m);".into(),
                        7 => "}".into(),
                        9 => "\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m".into(),
                        _ => String::new(),
                    };
                    line(
                        &mut out,
                        &format!(" {mark}{}\x1b[0m   {sample}", fit(name, list)),
                    );
                }
            }
            _ => {
                let Self {
                    shown,
                    settings,
                    selected,
                    top,
                    ..
                } = &mut *self;
                // Rows: section headings, then settings; scrolled to keep the selection in view.
                let mut lines: Vec<(Option<usize>, String)> = Vec::new();
                let mut last = "";
                for (k, &i) in shown.iter().enumerate() {
                    let s = &settings[i];
                    let sect = section(s.name);
                    if sect != last {
                        lines.push((None, format!("\x1b[1;4m{sect}\x1b[0m")));
                        last = sect;
                    }
                    let value = match (&s.current, s.many) {
                        (_, true) => "\x1b[2m(in the file)".to_owned(),
                        (Some(v), _) => v.clone(),
                        (None, _) => "\x1b[2m—".to_owned(),
                    };
                    let row = format!(
                        "  {}  {}",
                        fit(s.name, 28),
                        fit(&value, cols.saturating_sub(34))
                    );
                    lines.push((Some(k), row));
                }
                let at = lines
                    .iter()
                    .position(|l| l.0 == Some(*selected))
                    .unwrap_or(0);
                if at < *top {
                    *top = at.saturating_sub(1);
                } else if at >= *top + body {
                    *top = at + 1 - body;
                }
                for r in 0..body {
                    match lines.get(*top + r) {
                        Some((Some(k), row)) if *k == *selected => line(
                            &mut out,
                            &format!("\x1b[7m {}", fit(row, cols.saturating_sub(1))),
                        ),
                        Some((_, row)) => line(&mut out, &format!(" {row}")),
                        None => line(&mut out, ""),
                    }
                }
            }
        }
        // The selected setting's help, then what was done, then the keys.
        let help = match &self.mode {
            Mode::Pick {
                what: Pick::Theme, ..
            } => "↑↓ preview   type to filter   ↵ use this theme   esc back".to_owned(),
            Mode::Pick { .. } => {
                "↑↓ preview   type to filter   tab monospaced/all   ↵ use this font   esc back"
                    .to_owned()
            }
            _ => self
                .current()
                .map(|s| format!("{}: {}", s.name, s.help))
                .unwrap_or_default(),
        };
        line(
            &mut out,
            &format!("\x1b[2m{}", fit(&help, cols.saturating_sub(1))),
        );
        let editing = match &self.mode {
            Mode::Edit { buffer, pick } => Some((buffer.clone(), *pick)),
            _ => None,
        };
        let prompt = match (&self.mode, editing) {
            (Mode::Search, _) => format!("/{}\x1b[7m \x1b[0m", self.query),
            (_, Some((buffer, pick))) => {
                let name = self.current().map_or("", |s| s.name);
                let (_, word, matches) = self.suggest(&buffer);
                let pick = pick.min(matches.len().saturating_sub(1));
                // The rest of the picked suggestion, dim after the caret, when it starts with what's typed.
                let ghost = matches
                    .get(pick)
                    .filter(|m| m.to_lowercase().starts_with(&word.to_lowercase()))
                    .map(|m| m.chars().skip(word.chars().count()).collect::<String>())
                    .unwrap_or_default();
                let mut candidates = String::new();
                let mut used = 0;
                for (i, m) in matches.iter().enumerate().skip(pick.saturating_sub(3)) {
                    if used + m.chars().count() + 3 > cols.saturating_sub(4) {
                        candidates += "\x1b[2m…";
                        break;
                    }
                    used += m.chars().count() + 2;
                    candidates += &if i == pick {
                        format!("\x1b[7m {m} \x1b[0m")
                    } else {
                        format!(" {m} ")
                    };
                }
                let keys = if matches.is_empty() {
                    "↵ save  esc cancel  ^U clear"
                } else {
                    "→ complete  ↑↓ choose  ↵ save  esc cancel"
                };
                // The suggestions on the help line, above the value being typed.
                if !matches.is_empty() {
                    let at = out.rfind("\r\n").unwrap_or(0);
                    let start = out[..at].rfind("\r\n").map_or(0, |i| i + 2);
                    out.replace_range(start..at + 2, &format!("{candidates}\x1b[0m\x1b[K\r\n"));
                }
                // The caret on the suggestion's first letter, the word read whole.
                let mut ghost = ghost.chars();
                let caret = match ghost.next() {
                    Some(c) => format!("\x1b[7;2m{c}\x1b[0m\x1b[2m{}", ghost.as_str()),
                    None => "\x1b[7m \x1b[0m".to_owned(),
                };
                format!("{name} = {buffer}{caret}\x1b[0m   \x1b[2m{keys}")
            }
            _ => format!("\x1b[33m{}", self.status),
        };
        line(&mut out, &prompt);
        out += &format!(
            "\x1b[2m{}\x1b[0m\x1b[K",
            fit(
                " ↵ edit   / search   t themes   d default   e edit file   tab next section   q quit",
                cols.saturating_sub(1)
            )
        );
        out += "\x1b[J";
        print!("{out}");
        let _ = io::stdout().flush();
    }
}
