//! Settings for the GUI. Each one is declared once in [`SETTINGS`]; the
//! config file, command line flags (`--name=value`) and the help text all
//! come from that table.
//!
//! Later sources win: built-in defaults, then the config file
//! (`$XDG_CONFIG_HOME/mightty/config`, else `~/.config/mightty/config`),
//! then flags. The file has one `name = value` per line and `#` comment
//! lines; a setting given twice keeps the last value.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use mightty_render::{Rgb, Theme};

use crate::bindings::Bindings;
use mightty_term::{Colors, DEFAULT_SCROLLBACK, TermOptions};

pub struct Config {
    pub fonts: Fonts,
    /// Points.
    pub font_size: f64,
    pub cols: u16,
    pub rows: u16,
    /// Shell command line for the first window instead of a login shell;
    /// the window closes when it exits.
    pub command: Option<String>,
    /// Working directory for the first window; defaults to `$HOME`.
    pub cwd: Option<PathBuf>,
    pub theme: Theme,
    pub scrollback_lines: usize,
    /// Show frames only at a display refresh: no tearing, but up to a
    /// refresh or two more latency.
    pub vsync: bool,
    /// Which Option keys act as Alt (Meta) instead of typing special
    /// characters; `None`: decided by the keyboard layout.
    pub option_as_alt: Option<OptionAsAlt>,
    pub confirm_close: ConfirmClose,
    pub save_state: SaveState,
    pub font_backend: FontBackend,
    /// `grapheme-width`: whether VS16 emoji take two cells (unicode) or one (legacy).
    pub wide_emoji: bool,
    pub colorspace: Colorspace,
    /// Draw the font's ligatures and contextual alternates (`->` as an arrow).
    pub ligatures: bool,
    /// `macos-auto-secure-input`: Secure Keyboard Entry at password prompts.
    pub auto_secure_input: bool,
    /// `undo-timeout`: how long a closed pane's shell keeps running, for ⌘Z to bring it back.
    pub undo_timeout: std::time::Duration,
    /// `bell-audio-path`: the sound a bell plays with `bell-features` audio.
    pub bell_audio: Option<PathBuf>,
    /// `quick-terminal-position`: the screen edge the quick terminal drops from.
    pub quick_position: QuickPosition,
    /// `quick-terminal-autohide`: the quick terminal hides when another app or window takes the keys.
    pub quick_autohide: bool,
    /// `quick-terminal-size`: how deep it comes in from its edge, and how
    /// wide along it (all of it when unset).
    pub quick_size: (Extent, Option<Extent>),
    /// `keybind`: the defaults, changed by each setting in order.
    pub keybinds: Bindings,
    /// How panes look beyond colours and fonts.
    pub look: Look,
    /// `background-blur`: blur radius behind a translucent window, in points; 0 for none.
    pub background_blur: u32,
    /// `unfocused-split-opacity`: panes keys aren't going to are dimmed to this.
    pub unfocused_split_opacity: f64,
    /// Clipboard and mouse settings.
    pub behaviour: Behaviour,
    /// Where this came from, to load it again (see `Config::reload`): the
    /// default file, and the command line.
    default_file: Option<PathBuf>,
    args: Vec<String>,
    /// The next `font-family` starts a new list: each source (a file, the
    /// flags) gives the whole list, as in Ghostty.
    fresh_families: bool,
    /// macOS is in dark mode, for `theme = light:…,dark:…`.
    dark_mode: bool,
    /// `theme` names a light and a dark theme, so the settings are loaded
    /// again when macOS changes between them.
    pub follows_appearance: bool,
    /// `theme` as it was given, to show and to change.
    pub theme_name: Option<String>,
    /// Files being read, innermost last, to catch `config-file` loops.
    including: Vec<PathBuf>,
}

/// `config-file` nesting deeper than this is taken for a mistake.
const MAX_INCLUDE_DEPTH: usize = 10;

/// Whether macOS is in dark mode (System Settings → Appearance), read from
/// the user's global preferences, so it works before the app starts.
pub fn system_dark() -> bool {
    use crate::ffi::*;
    // SAFETY: CoreFoundation calls with valid arguments; the copied value
    // and the key are released here.
    unsafe {
        CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
        let key = cfstring("AppleInterfaceStyle");
        let value = CFPreferencesCopyAppValue(key, kCFPreferencesAnyApplication);
        CFRelease(key);
        if value.is_null() {
            return false; // light: the key is only set for dark
        }
        let dark = string_from_ns(value as Id).eq_ignore_ascii_case("dark");
        CFRelease(value);
        dark
    }
}

/// Which fonts, and how they're set up: `font-family` and the settings
/// around it.
#[derive(Clone, Debug, PartialEq)]
pub struct Fonts {
    /// `font-family`, repeated: the font, then fallbacks tried in order
    /// before the ones macOS picks.
    pub families: Vec<String>,
    /// `font-family-bold`, `-italic` and `-bold-italic`, by `Style` bits
    /// minus one; unset ones come from the first family.
    pub styled: [Option<String>; 3],
    /// `font-feature`: OpenType features and values (0 off, 1 on, more for
    /// alternates), in order.
    pub features: Vec<([u8; 4], u32)>,
    /// `font-variation`, `-bold`, `-italic` and `-bold-italic`, by `Style`
    /// bits: axis tags and values.
    pub variations: [Vec<([u8; 4], f64)>; 4],
}

impl Default for Fonts {
    fn default() -> Self {
        Self {
            families: vec!["Menlo".into()],
            styled: Default::default(),
            features: Vec::new(),
            variations: Default::default(),
        }
    }
}

/// An OpenType tag: four characters, shorter ones padded with spaces.
fn tag(v: &str) -> Result<[u8; 4], String> {
    let bytes = v.as_bytes();
    if bytes.is_empty() || bytes.len() > 4 || !bytes.iter().all(|b| b.is_ascii_alphanumeric()) {
        return Err(format!("`{v}` isn't an OpenType tag such as ss01 or wght"));
    }
    let mut out = [b' '; 4];
    out[..bytes.len()].copy_from_slice(bytes);
    Ok(out)
}

/// `font-feature`: `ss01` or `+ss01` (on), `-calt` (off), `cv01=2`.
fn feature(v: &str) -> Result<([u8; 4], u32), String> {
    if let Some(off) = v.strip_prefix('-') {
        return Ok((tag(off)?, 0));
    }
    let v = v.strip_prefix('+').unwrap_or(v);
    match v.split_once('=') {
        Some((name, value)) => Ok((
            tag(name.trim())?,
            value
                .trim()
                .parse()
                .map_err(|_| "expects a whole number after =")?,
        )),
        None => Ok((tag(v)?, 1)),
    }
}

/// `font-variation`: `wght=450`.
fn variation(v: &str) -> Result<([u8; 4], f64), String> {
    let (name, value) = v
        .split_once('=')
        .ok_or("expects AXIS=VALUE, such as wght=450")?;
    let value = value
        .trim()
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite())
        .ok_or("expects a number after =")?;
    Ok((tag(name.trim())?, value))
}

/// How panes treat the clipboard and the mouse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Behaviour {
    /// `clipboard-read`: programs reading the clipboard (OSC 52).
    pub clipboard_read: Permission,
    /// `clipboard-write`: programs setting it.
    pub clipboard_write: Permission,
    /// `copy-on-select`: selected text goes to the clipboard at once.
    pub copy_on_select: bool,
    /// `clipboard-paste-protection`: ask before pasting text that would
    /// run commands or act as keys (see `keys::paste_is_risky`).
    pub paste_protection: bool,
    /// `mouse-hide-while-typing`.
    pub hide_mouse: bool,
    /// `scrollbar`: system (an overlay scroller while scrolling) or never.
    pub scrollbar: bool,
    /// `macos-titlebar-style`.
    pub titlebar: Titlebar,
    /// `bell-features`: what a bell does.
    pub bell: Bell,
    /// `password-mask`: a `*` at the cursor for each character typed at a
    /// password prompt, drawn by mightty and sent nowhere.
    pub password_mask: bool,
    /// `mouse-scroll-multiplier`: lines scrolled per line the wheel or
    /// trackpad reports.
    pub scroll_multiplier: f64,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            clipboard_read: Permission::Ask,
            clipboard_write: Permission::Allow,
            copy_on_select: false,
            paste_protection: true,
            hide_mouse: false,
            scrollbar: true,
            titlebar: Titlebar::Native,
            bell: Bell {
                system: true,
                audio: false,
                attention: false,
                title: true,
                border: false,
            },
            password_mask: true,
            scroll_multiplier: 1.0,
        }
    }
}

/// Whether programs may do something: as in Ghostty's `clipboard-read`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Allow,
    Ask,
    Deny,
}

impl Permission {
    fn parse(v: &str) -> Result<Self, String> {
        match v {
            "allow" => Ok(Self::Allow),
            "ask" => Ok(Self::Ask),
            "deny" => Ok(Self::Deny),
            _ => Err("expects allow, ask or deny".into()),
        }
    }

    fn show(self) -> String {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
        .into()
    }
}

/// Settings the renderer draws with, apart from colours and fonts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// `background-opacity`: of the default background, 0 to 1.
    pub background_opacity: f64,
    /// `window-padding-x` and `window-padding-y`, in points.
    pub padding: (f64, f64),
    pub cursor_style: CursorStyle,
    /// `cursor-style-blink`; unset: as the program asks (DECSCUSR), else steady.
    pub cursor_blink: Option<bool>,
    /// `adjust-cell-width` and `adjust-cell-height`.
    pub cell_adjust: (Adjust, Adjust),
}

impl Default for Look {
    fn default() -> Self {
        Self {
            background_opacity: 1.0,
            padding: (6.0, 6.0),
            cursor_style: CursorStyle::Block,
            cursor_blink: None,
            cell_adjust: (Adjust::Points(0.0), Adjust::Points(0.0)),
        }
    }
}

/// `cursor-style`, as in Ghostty: the cursor unless the program picks one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

/// A change to the cell size: points (`2`, `-1`) or a percentage (`10%`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Adjust {
    Points(f64),
    Percent(f64),
}

/// Font sizes allowed, in points.
pub const MIN_FONT_SIZE: f64 = 4.0;
pub const MAX_FONT_SIZE: f64 = 200.0;
/// The largest cell side an adjustment can make, in pixels: glyph tiles
/// are several cells, four bytes a pixel.
const MAX_CELL_PIXELS: f64 = 1024.0;

impl Adjust {
    /// `pixels` adjusted, at `scale` pixels per point; 1 to [`MAX_CELL_PIXELS`].
    pub fn apply(self, pixels: u32, scale: f64) -> u32 {
        let adjusted = match self {
            Self::Points(p) => pixels as f64 + p * scale,
            Self::Percent(p) => pixels as f64 * (1.0 + p / 100.0),
        };
        adjusted.round().clamp(1.0, MAX_CELL_PIXELS) as u32
    }

    fn parse(v: &str) -> Result<Self, String> {
        let number = |n: &str| n.trim().parse().ok().filter(|n: &f64| n.is_finite());
        match v.strip_suffix('%') {
            Some(p) => number(p).filter(|&p| p > -100.0).map(Self::Percent),
            None => number(v).map(Self::Points),
        }
        .ok_or_else(|| "expects points like 2 or -1, or a percentage like 10%".into())
    }

    fn show(self) -> String {
        match self {
            Self::Points(p) => p.to_string(),
            Self::Percent(p) => format!("{p}%"),
        }
    }
}

/// `bell-features`, as in Ghostty: what a bell (BEL) does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bell {
    /// The system's alert sound.
    pub system: bool,
    /// `bell-audio-path`'s sound.
    pub audio: bool,
    /// The Dock icon bounces once while mightty isn't the active app.
    pub attention: bool,
    /// 🔔 on the pane's tab and window title until it's typed in or looked at.
    pub title: bool,
    /// The pane's edge flashes.
    pub border: bool,
}

/// `macos-titlebar-style`, as in Ghostty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Titlebar {
    /// The system's.
    Native,
    /// In the terminal's background colour, the window one surface.
    Transparent,
    /// None: the terminal reaches the top; its top padding drags the window.
    Hidden,
    /// mightty's own tab bar in the title bar, beside the window's buttons.
    Tabs,
}

/// `quick-terminal-position`, as in Ghostty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickPosition {
    Top,
    Bottom,
    Left,
    Right,
    Center,
}

/// A length on the screen: a share of it, or points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extent {
    Percent(f64),
    Points(f64),
}

impl Extent {
    fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (number, percent) = match (s.strip_suffix('%'), s.strip_suffix("px")) {
            (Some(n), _) => (n, true),
            (None, Some(n)) => (n, false),
            _ => {
                return Err(format!(
                    "`{s}`: expects a percentage (40%) or points (500px)"
                ));
            }
        };
        let n: f64 = number
            .trim()
            .parse()
            .map_err(|_| format!("`{s}`: not a number"))?;
        match percent {
            true if n > 0.0 && n <= 100.0 => Ok(Self::Percent(n)),
            false if n > 0.0 => Ok(Self::Points(n)),
            _ => Err(format!("`{s}`: out of range")),
        }
    }

    /// Points, out of `whole`.
    pub fn of(self, whole: f64) -> f64 {
        match self {
            Self::Percent(p) => whole * p / 100.0,
            Self::Points(p) => p.min(whole),
        }
    }

    fn show(self) -> String {
        match self {
            Self::Percent(p) => format!("{p}%"),
            Self::Points(p) => format!("{p}px"),
        }
    }
}

/// `window-colorspace`: what colours (theme, images) are taken to be in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colorspace {
    Srgb,
    /// Wide gamut: the same numbers, more saturated on a P3 display.
    DisplayP3,
}

/// `font-backend`: who reads fonts and draws glyphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontBackend {
    CoreText,
    /// mightty's own (`mightty-font`), for TrueType fonts; CoreText still
    /// finds font files and draws what that can't yet (emoji, clusters).
    Mightty,
}

/// `window-save-state`, as in Ghostty: whether windows, tabs and splits
/// come back the next time mightty opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveState {
    /// As macOS does for apps: unless "Close windows when quitting an
    /// application" is on (System Settings → Desktop & Dock).
    Default,
    Never,
    Always,
}

/// `confirm-close-surface`, as in Ghostty: when closing a pane, window or
/// mightty asks first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmClose {
    Never,
    /// When a command or agent is running in it.
    Running,
    Always,
}

/// `macos-option-as-alt`, as in Ghostty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionAsAlt {
    None,
    Both,
    Left,
    Right,
}

impl OptionAsAlt {
    /// Whether Option acts as Alt for a key press with Option held and
    /// these NSEvent modifier flags, `us_layout` saying whether a US keyboard layout is
    /// active (used when the setting is unset).
    pub fn applies(setting: Option<Self>, flags: usize, us_layout: impl FnOnce() -> bool) -> bool {
        // Device-dependent bits for the left and right Option keys.
        const LEFT: usize = 0x20;
        const RIGHT: usize = 0x40;
        match setting {
            // Elsewhere Option types characters the layout needs, like `@`.
            None => us_layout(),
            Some(Self::None) => false,
            Some(Self::Both) => true,
            Some(Self::Left) => flags & LEFT != 0,
            Some(Self::Right) => flags & RIGHT != 0,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fonts: Fonts::default(),
            fresh_families: true,
            dark_mode: false,
            follows_appearance: false,
            theme_name: None,
            including: Vec::new(),
            font_size: 14.0,
            cols: 100,
            rows: 30,
            command: None,
            cwd: None,
            theme: Theme::default(),
            scrollback_lines: DEFAULT_SCROLLBACK,
            vsync: true,
            option_as_alt: None,
            confirm_close: ConfirmClose::Running,
            save_state: SaveState::Default,
            font_backend: FontBackend::CoreText,
            wide_emoji: true,
            colorspace: Colorspace::Srgb,
            ligatures: true,
            auto_secure_input: true,
            undo_timeout: std::time::Duration::from_secs(5),
            bell_audio: None,
            quick_position: QuickPosition::Top,
            quick_autohide: true,
            quick_size: (Extent::Percent(40.0), None),
            keybinds: Bindings::default(),
            look: Look::default(),
            background_blur: 0,
            unfocused_split_opacity: 0.7,
            behaviour: Behaviour::default(),
            default_file: None,
            args: Vec::new(),
        }
    }
}

struct Setting {
    name: &'static str,
    /// Placeholder for the value in help text.
    value: &'static str,
    help: &'static str,
    /// Allowed in theme files.
    color: bool,
    set: fn(&mut Config, &str) -> Result<(), String>,
    /// The current value for help text, or `None` when unset.
    get: fn(&Config) -> Option<String>,
}

/// A setting as `mightty config` shows it.
pub struct SettingView {
    pub name: &'static str,
    /// What its value is, as help text names it: `BOOL`, `SIZE`, `NAME`, ….
    pub value: &'static str,
    pub help: &'static str,
    pub current: Option<String>,
    /// Given once a line, any number of times (`keybind = …`), so edited in the file.
    pub many: bool,
}

/// Settings given any number of times.
const MANY: &[&str] = &[
    "keybind",
    "palette",
    "font-feature",
    "font-variation",
    "font-variation-bold",
    "font-variation-italic",
    "font-variation-bold-italic",
];

/// The values a setting with a fixed few takes, for completing them;
/// empty for the rest. `bell-features` takes a comma list of these.
pub fn choices(name: &str) -> &'static [&'static str] {
    match name {
        "copy-on-select" => &["true", "false", "clipboard"],
        "macos-option-as-alt" => &["true", "false", "left", "right"],
        "confirm-close-surface" => &["true", "false", "always"],
        "background-blur" => &["true", "false"],
        "quick-terminal-position" => &["top", "bottom", "left", "right", "center"],
        "font-backend" => &["coretext", "mightty"],
        "grapheme-width" => &["unicode", "legacy"],
        "window-colorspace" => &["srgb", "display-p3"],
        "window-save-state" => &["default", "never", "always"],
        "cursor-style" => &["block", "bar", "underline", "block_hollow"],
        "clipboard-read" | "clipboard-write" => &["allow", "ask", "deny"],
        "macos-titlebar-style" => &["native", "transparent", "tabs", "hidden"],
        "scrollbar" => &["system", "never"],
        "bell-features" => &[
            "system",
            "audio",
            "attention",
            "title",
            "border",
            "no-system",
            "no-audio",
            "no-attention",
            "no-title",
            "no-border",
        ],
        _ => match SETTINGS.iter().find(|s| s.name == name) {
            Some(s) if s.value == "BOOL" => &["true", "false"],
            _ => &[],
        },
    }
}

/// Whether `value` is one `name` takes, saying why not.
pub fn check(name: &str, value: &str) -> Result<(), String> {
    Config::default().set(name, value)
}

/// The file a theme `name` is read from: the user's, else the bundled one.
pub fn theme_path(name: &str) -> Option<PathBuf> {
    let own = config_dir()?.join("themes").join(name);
    if own.is_file() {
        return Some(own);
    }
    bundled_themes()
        .map(|dir| dir.join(name))
        .filter(|p| p.is_file())
}

/// Sets `name` to `value` in the config file at `path`, its comments and
/// order kept: the last line setting it is changed, or one is added at the
/// end. With `None`, every line setting it goes, back to its default.
pub fn write_setting(path: &Path, name: &str, value: Option<&str>) -> std::io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let sets = |line: &str| {
        let line = line.trim_start();
        !line.starts_with('#')
            && line
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == name)
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    match value {
        Some(value) => {
            let line = format!("{name} = {value}");
            match lines.iter().rposition(|l| sets(l)) {
                Some(at) => lines[at] = line,
                None => lines.push(line),
            }
        }
        None => lines.retain(|l| !sets(l)),
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut out = lines.join("\n");
    out.push('\n');
    fs::write(path, out)
}

/// Names are Ghostty's where the meaning is the same, so Ghostty theme
/// files work unchanged.
const SETTINGS: &[Setting] = &[
    Setting {
        name: "font-family",
        value: "NAME",
        help: "font family; repeat for fallbacks, tried in order (an empty value clears the list)",
        color: false,
        set: |c, v| {
            if std::mem::take(&mut c.fresh_families) || v.is_empty() {
                c.fonts.families.clear();
            }
            if !v.is_empty() {
                c.fonts.families.push(v.into());
            }
            Ok(())
        },
        get: |c| Some(c.fonts.families.join(", ")),
    },
    Setting {
        name: "font-family-bold",
        value: "NAME",
        help: "family for bold text (default: the font-family's bold, or made from it)",
        color: false,
        set: |c, v| {
            c.fonts.styled[0] = Some(v.into()).filter(|v: &String| !v.is_empty());
            Ok(())
        },
        get: |c| c.fonts.styled[0].clone(),
    },
    Setting {
        name: "font-family-italic",
        value: "NAME",
        help: "family for italic text",
        color: false,
        set: |c, v| {
            c.fonts.styled[1] = Some(v.into()).filter(|v: &String| !v.is_empty());
            Ok(())
        },
        get: |c| c.fonts.styled[1].clone(),
    },
    Setting {
        name: "font-family-bold-italic",
        value: "NAME",
        help: "family for bold italic text",
        color: false,
        set: |c, v| {
            c.fonts.styled[2] = Some(v.into()).filter(|v: &String| !v.is_empty());
            Ok(())
        },
        get: |c| c.fonts.styled[2].clone(),
    },
    Setting {
        name: "font-feature",
        value: "FEATURE",
        help: "OpenType feature: ss01 or +ss01 on, -calt off, cv01=2 an alternate; repeat for more \
               (an empty value clears them)",
        color: false,
        set: |c, v| {
            if v.is_empty() {
                c.fonts.features.clear();
            } else {
                let (tag, value) = feature(v)?;
                c.fonts.features.retain(|f| f.0 != tag);
                c.fonts.features.push((tag, value));
            }
            Ok(())
        },
        get: |c| {
            let show = |(tag, value): &([u8; 4], u32)| {
                let name = String::from_utf8_lossy(tag).trim_end().to_owned();
                match value {
                    0 => format!("-{name}"),
                    1 => name,
                    n => format!("{name}={n}"),
                }
            };
            (!c.fonts.features.is_empty()).then(|| {
                c.fonts
                    .features
                    .iter()
                    .map(show)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        },
    },
    Setting {
        name: "font-variation",
        value: "AXIS=VALUE",
        help: "variable font axis for regular text, such as wght=450; repeat for more axes (empty clears)",
        color: false,
        set: |c, v| set_variation(c, 0, v),
        get: |c| show_variations(&c.fonts.variations[0]),
    },
    Setting {
        name: "font-variation-bold",
        value: "AXIS=VALUE",
        help: "variable font axis for bold text",
        color: false,
        set: |c, v| set_variation(c, 1, v),
        get: |c| show_variations(&c.fonts.variations[1]),
    },
    Setting {
        name: "font-variation-italic",
        value: "AXIS=VALUE",
        help: "variable font axis for italic text",
        color: false,
        set: |c, v| set_variation(c, 2, v),
        get: |c| show_variations(&c.fonts.variations[2]),
    },
    Setting {
        name: "font-variation-bold-italic",
        value: "AXIS=VALUE",
        help: "variable font axis for bold italic text",
        color: false,
        set: |c, v| set_variation(c, 3, v),
        get: |c| show_variations(&c.fonts.variations[3]),
    },
    Setting {
        name: "font-size",
        value: "POINTS",
        help: "font size",
        color: false,
        set: |c, v| {
            let range = MIN_FONT_SIZE..=MAX_FONT_SIZE;
            c.font_size = v
                .parse()
                .ok()
                .filter(|p| range.contains(p))
                .ok_or(format!("expects {MIN_FONT_SIZE} to {MAX_FONT_SIZE}"))?;
            Ok(())
        },
        get: |c| Some(c.font_size.to_string()),
    },
    Setting {
        name: "font-ligatures",
        value: "BOOL",
        help: "draw the font's ligatures, such as -> and != as arrows and signs, in fonts that have them",
        color: false,
        set: |c, v| {
            c.ligatures = v.parse().map_err(|_| "expects true or false")?;
            Ok(())
        },
        get: |c| Some(c.ligatures.to_string()),
    },
    Setting {
        name: "window-width",
        value: "COLUMNS",
        help: "width of new windows, in cells",
        color: false,
        set: |c, v| {
            c.cols = cells(v)?;
            Ok(())
        },
        get: |c| Some(c.cols.to_string()),
    },
    Setting {
        name: "window-height",
        value: "ROWS",
        help: "height of new windows, in cells",
        color: false,
        set: |c, v| {
            c.rows = cells(v)?;
            Ok(())
        },
        get: |c| Some(c.rows.to_string()),
    },
    Setting {
        name: "scrollback-lines",
        value: "LINES",
        help: "lines kept above the screen",
        color: false,
        set: |c, v| {
            c.scrollback_lines = v.parse().map_err(|_| "expects a whole number")?;
            Ok(())
        },
        get: |c| Some(c.scrollback_lines.to_string()),
    },
    Setting {
        name: "window-vsync",
        value: "BOOL",
        help: "show frames only at a display refresh; false is lower latency but may tear",
        color: false,
        set: |c, v| {
            c.vsync = v.parse().map_err(|_| "expects true or false")?;
            Ok(())
        },
        get: |c| Some(c.vsync.to_string()),
    },
    Setting {
        name: "macos-auto-secure-input",
        value: "BOOL",
        help: "turn on Secure Keyboard Entry while a program reads a password (input not echoed), so other apps \
               can't read the keys, as in Ghostty",
        color: false,
        set: |c, v| {
            c.auto_secure_input = v.parse().map_err(|_| "expects true or false")?;
            Ok(())
        },
        get: |c| Some(c.auto_secure_input.to_string()),
    },
    Setting {
        name: "undo-timeout",
        value: "TIME",
        help: "how long a closed tab or pane keeps running, hidden, so undo (⌘Z) brings it back as it was: \
               5s, 500ms, 1m; 0 for never, as in Ghostty. Reopening (⌘⇧T) works after, with new shells",
        color: false,
        set: |c, v| {
            let v = v.trim();
            let (number, unit) = match (
                v.strip_suffix("ms"),
                v.strip_suffix('s'),
                v.strip_suffix('m'),
            ) {
                (Some(n), ..) => (n, 0.001),
                (None, Some(n), _) => (n, 1.0),
                (None, None, Some(n)) => (n, 60.0),
                _ if v == "0" => ("0", 1.0),
                _ => return Err("expects a time: 5s, 500ms or 1m".into()),
            };
            let n: f64 = number
                .trim()
                .parse()
                .map_err(|_| "expects a time: 5s, 500ms or 1m")?;
            if !(0.0..=3600.0).contains(&(n * unit)) {
                return Err("expects a time up to an hour".into());
            }
            c.undo_timeout = std::time::Duration::from_secs_f64(n * unit);
            Ok(())
        },
        get: |c| Some(format!("{}ms", c.undo_timeout.as_millis())),
    },
    Setting {
        name: "quick-terminal-position",
        value: "EDGE",
        help: "top, bottom, left, right or center: where the quick terminal (toggle_quick_terminal) appears, as \
               in Ghostty",
        color: false,
        set: |c, v| {
            c.quick_position = match v {
                "top" => QuickPosition::Top,
                "bottom" => QuickPosition::Bottom,
                "left" => QuickPosition::Left,
                "right" => QuickPosition::Right,
                "center" => QuickPosition::Center,
                _ => return Err("expects top, bottom, left, right or center".into()),
            };
            Ok(())
        },
        get: |c| Some(format!("{:?}", c.quick_position).to_lowercase()),
    },
    Setting {
        name: "quick-terminal-size",
        value: "SIZE[,SIZE]",
        help: "how far the quick terminal comes in from its edge, then how much of the edge it runs along \
               (all of it when left out): a percentage of the screen (40%) or points (500px), as in Ghostty; \
               centred, its width then height",
        color: false,
        set: |c, v| {
            let (deep, along) = match v.split_once(',') {
                Some((deep, along)) => (Extent::parse(deep)?, Some(Extent::parse(along)?)),
                None => (Extent::parse(v)?, None),
            };
            c.quick_size = (deep, along);
            Ok(())
        },
        get: |c| {
            Some(match c.quick_size {
                (deep, Some(along)) => format!("{},{}", deep.show(), along.show()),
                (deep, None) => deep.show(),
            })
        },
    },
    Setting {
        name: "quick-terminal-autohide",
        value: "BOOL",
        help: "hide the quick terminal when another window or app takes the keys",
        color: false,
        set: |c, v| {
            c.quick_autohide = v.parse().map_err(|_| "expects true or false")?;
            Ok(())
        },
        get: |c| Some(c.quick_autohide.to_string()),
    },
    Setting {
        name: "macos-option-as-alt",
        value: "WHICH",
        help: "true, false, left or right: Option keys that act as Alt instead of typing characters \
               (default: true on US layouts, else false)",
        color: false,
        set: |c, v| {
            c.option_as_alt = Some(match v {
                "true" => OptionAsAlt::Both,
                "false" => OptionAsAlt::None,
                "left" => OptionAsAlt::Left,
                "right" => OptionAsAlt::Right,
                _ => return Err("expects true, false, left or right".into()),
            });
            Ok(())
        },
        get: |c| {
            c.option_as_alt.map(|o| {
                match o {
                    OptionAsAlt::Both => "true",
                    OptionAsAlt::None => "false",
                    OptionAsAlt::Left => "left",
                    OptionAsAlt::Right => "right",
                }
                .into()
            })
        },
    },
    Setting {
        name: "confirm-close-surface",
        value: "WHEN",
        help: "true, false or always: ask before closing a pane, window or mightty \
               (true: only while a command or agent runs in it)",
        color: false,
        set: |c, v| {
            c.confirm_close = match v {
                "true" => ConfirmClose::Running,
                "false" => ConfirmClose::Never,
                "always" => ConfirmClose::Always,
                _ => return Err("expects true, false or always".into()),
            };
            Ok(())
        },
        get: |c| {
            Some(
                match c.confirm_close {
                    ConfirmClose::Running => "true",
                    ConfirmClose::Never => "false",
                    ConfirmClose::Always => "always",
                }
                .into(),
            )
        },
    },
    Setting {
        name: "font-backend",
        value: "NAME",
        help: "coretext or mightty: who draws glyphs (mightty: our own rasterizer, TrueType fonts; for testing)",
        color: false,
        set: |c, v| {
            c.font_backend = match v {
                "coretext" => FontBackend::CoreText,
                "mightty" => FontBackend::Mightty,
                _ => return Err("expects coretext or mightty".into()),
            };
            Ok(())
        },
        get: |c| {
            Some(
                if c.font_backend == FontBackend::Mightty {
                    "mightty"
                } else {
                    "coretext"
                }
                .into(),
            )
        },
    },
    Setting {
        name: "grapheme-width",
        value: "METHOD",
        help: "unicode or legacy: unicode draws emoji written with VS16 (\u{2764}\u{FE0F}) two cells wide, as Ghostty \
               and kitty do; legacy keeps them one, as shells count them, so command lines with them redraw cleanly",
        color: false,
        set: |c, v| {
            c.wide_emoji = match v {
                "unicode" => true,
                "legacy" => false,
                _ => return Err("expects unicode or legacy".into()),
            };
            Ok(())
        },
        get: |c| Some(if c.wide_emoji { "unicode" } else { "legacy" }.into()),
    },
    Setting {
        name: "window-colorspace",
        value: "SPACE",
        help: "srgb or display-p3: the colour space colours and images are taken to be in, as in Ghostty; \
               display-p3 shows them more saturated on a wide-gamut display",
        color: false,
        set: |c, v| {
            c.colorspace = match v {
                "srgb" => Colorspace::Srgb,
                "display-p3" => Colorspace::DisplayP3,
                _ => return Err("expects srgb or display-p3".into()),
            };
            Ok(())
        },
        get: |c| {
            Some(
                if c.colorspace == Colorspace::DisplayP3 {
                    "display-p3"
                } else {
                    "srgb"
                }
                .into(),
            )
        },
    },
    Setting {
        name: "window-save-state",
        value: "WHEN",
        help: "default, never or always: reopen windows, tabs and splits in their directories \
               (default: unless \"Close windows when quitting an application\" is on in System Settings)",
        color: false,
        set: |c, v| {
            c.save_state = match v {
                "default" => SaveState::Default,
                "never" => SaveState::Never,
                "always" => SaveState::Always,
                _ => return Err("expects default, never or always".into()),
            };
            Ok(())
        },
        get: |c| {
            Some(
                match c.save_state {
                    SaveState::Default => "default",
                    SaveState::Never => "never",
                    SaveState::Always => "always",
                }
                .into(),
            )
        },
    },
    Setting {
        name: "background-opacity",
        value: "0-1",
        help: "opacity of the default background; below 1 the desktop shows through",
        color: false,
        set: |c, v| {
            c.look.background_opacity = fraction(v, 0.0)?;
            Ok(())
        },
        get: |c| Some(c.look.background_opacity.to_string()),
    },
    Setting {
        name: "background-blur",
        value: "RADIUS",
        help: "blur behind a translucent background: true, false or a radius in points",
        color: false,
        set: |c, v| {
            c.background_blur = match v {
                "true" => 20,
                "false" => 0,
                _ => v
                    .parse()
                    .ok()
                    .filter(|&r| r <= 100)
                    .ok_or("expects true, false or a radius from 0 to 100")?,
            };
            Ok(())
        },
        get: |c| Some(c.background_blur.to_string()),
    },
    Setting {
        name: "window-padding-x",
        value: "POINTS",
        help: "space left and right of the text",
        color: false,
        set: |c, v| {
            c.look.padding.0 = padding(v)?;
            Ok(())
        },
        get: |c| Some(c.look.padding.0.to_string()),
    },
    Setting {
        name: "window-padding-y",
        value: "POINTS",
        help: "space above and below the text",
        color: false,
        set: |c, v| {
            c.look.padding.1 = padding(v)?;
            Ok(())
        },
        get: |c| Some(c.look.padding.1.to_string()),
    },
    Setting {
        name: "cursor-style",
        value: "STYLE",
        help: "block, bar, underline or block_hollow, unless the program picks one",
        color: false,
        set: |c, v| {
            c.look.cursor_style = match v {
                "block" => CursorStyle::Block,
                "bar" => CursorStyle::Bar,
                "underline" => CursorStyle::Underline,
                "block_hollow" => CursorStyle::BlockHollow,
                _ => return Err("expects block, bar, underline or block_hollow".into()),
            };
            Ok(())
        },
        get: |c| {
            Some(
                match c.look.cursor_style {
                    CursorStyle::Block => "block",
                    CursorStyle::Bar => "bar",
                    CursorStyle::Underline => "underline",
                    CursorStyle::BlockHollow => "block_hollow",
                }
                .into(),
            )
        },
    },
    Setting {
        name: "cursor-style-blink",
        value: "BOOL",
        help: "blink the cursor (default: only when the program asks)",
        color: false,
        set: |c, v| {
            c.look.cursor_blink = Some(v.parse().map_err(|_| "expects true or false")?);
            Ok(())
        },
        get: |c| c.look.cursor_blink.map(|b| b.to_string()),
    },
    Setting {
        name: "adjust-cell-width",
        value: "POINTS|N%",
        help: "make cells wider or narrower than the font says: points (1, -1) or a percentage (10%)",
        color: false,
        set: |c, v| {
            c.look.cell_adjust.0 = Adjust::parse(v)?;
            Ok(())
        },
        get: |c| Some(c.look.cell_adjust.0.show()),
    },
    Setting {
        name: "adjust-cell-height",
        value: "POINTS|N%",
        help: "make cells taller or shorter than the font says, for more space between lines: points or a percentage",
        color: false,
        set: |c, v| {
            c.look.cell_adjust.1 = Adjust::parse(v)?;
            Ok(())
        },
        get: |c| Some(c.look.cell_adjust.1.show()),
    },
    Setting {
        name: "unfocused-split-opacity",
        value: "0.15-1",
        help: "dim panes keys aren't going to, when a window has several",
        color: false,
        set: |c, v| {
            c.unfocused_split_opacity = fraction(v, 0.15)?;
            Ok(())
        },
        get: |c| Some(c.unfocused_split_opacity.to_string()),
    },
    Setting {
        name: "clipboard-read",
        value: "WHEN",
        help: "allow, ask or deny: programs reading the clipboard (OSC 52)",
        color: false,
        set: |c, v| {
            c.behaviour.clipboard_read = Permission::parse(v)?;
            Ok(())
        },
        get: |c| Some(c.behaviour.clipboard_read.show()),
    },
    Setting {
        name: "clipboard-write",
        value: "WHEN",
        help: "allow, ask or deny: programs setting the clipboard (OSC 52), as vim, tmux and ssh sessions do to copy",
        color: false,
        set: |c, v| {
            c.behaviour.clipboard_write = Permission::parse(v)?;
            Ok(())
        },
        get: |c| Some(c.behaviour.clipboard_write.show()),
    },
    Setting {
        name: "copy-on-select",
        value: "BOOL",
        help: "copy text to the clipboard as soon as it's selected (true or clipboard)",
        color: false,
        set: |c, v| {
            c.behaviour.copy_on_select = match v {
                "true" | "clipboard" => true,
                "false" => false,
                _ => return Err("expects true, false or clipboard".into()),
            };
            Ok(())
        },
        get: |c| Some(c.behaviour.copy_on_select.to_string()),
    },
    Setting {
        name: "clipboard-paste-protection",
        value: "BOOL",
        help: "ask before pasting text with line breaks a shell would run at once, or with control characters",
        color: false,
        set: |c, v| {
            c.behaviour.paste_protection = v.parse().map_err(|_| "expects true or false")?;
            Ok(())
        },
        get: |c| Some(c.behaviour.paste_protection.to_string()),
    },
    Setting {
        name: "mouse-hide-while-typing",
        value: "BOOL",
        help: "hide the mouse pointer while typing, until it moves",
        color: false,
        set: |c, v| {
            c.behaviour.hide_mouse = v.parse().map_err(|_| "expects true or false")?;
            Ok(())
        },
        get: |c| Some(c.behaviour.hide_mouse.to_string()),
    },
    Setting {
        name: "bell-features",
        value: "FEATURES",
        help: "what a bell does, as in Ghostty: a comma list of system (the alert sound), audio (bell-audio-path), \
               attention (bounce the Dock icon while in the background), title (🔔 on the title until the pane is \
               typed in or looked at) and border (flash the pane's edge); no-NAME turns one off. Default: system,title",
        color: false,
        set: |c, v| {
            let bell = &mut c.behaviour.bell;
            for item in v.split(',').map(str::trim).filter(|i| !i.is_empty()) {
                let (name, on) = match item.strip_prefix("no-") {
                    Some(name) => (name, false),
                    None => (item, true),
                };
                *match name {
                    "system" => &mut bell.system,
                    "audio" => &mut bell.audio,
                    "attention" => &mut bell.attention,
                    "title" => &mut bell.title,
                    "border" => &mut bell.border,
                    _ => {
                        return Err(format!(
                            "unknown feature `{name}`: system, audio, attention, title or border"
                        ));
                    }
                } = on;
            }
            Ok(())
        },
        get: |c| {
            let b = c.behaviour.bell;
            let list = [
                ("system", b.system),
                ("audio", b.audio),
                ("attention", b.attention),
                ("title", b.title),
                ("border", b.border),
            ];
            Some(
                list.iter()
                    .map(|&(n, on)| if on { n.to_owned() } else { format!("no-{n}") })
                    .collect::<Vec<_>>()
                    .join(","),
            )
        },
    },
    Setting {
        name: "bell-audio-path",
        value: "PATH",
        help: "a sound file a bell plays with bell-features audio (any format macOS plays: aiff, wav, mp3, m4a)",
        color: false,
        set: |c, v| {
            c.bell_audio = (!v.is_empty()).then(|| expand_home(v));
            Ok(())
        },
        get: |c| c.bell_audio.as_ref().map(|p| p.display().to_string()),
    },
    Setting {
        name: "macos-titlebar-style",
        value: "STYLE",
        help: "native, transparent (in the terminal's background colour), tabs (tabs in the title bar) or hidden \
               (none, the window dragged by its top padding; tabs, if any, at the top), as in Ghostty",
        color: false,
        set: |c, v| {
            c.behaviour.titlebar = match v {
                "native" => Titlebar::Native,
                "transparent" => Titlebar::Transparent,
                "hidden" => Titlebar::Hidden,
                "tabs" => Titlebar::Tabs,
                _ => return Err("expects native, transparent, tabs or hidden".into()),
            };
            Ok(())
        },
        get: |c| Some(format!("{:?}", c.behaviour.titlebar).to_lowercase()),
    },
    Setting {
        name: "scrollbar",
        value: "WHEN",
        help: "system or never: system shows macOS's overlay scrollbar at a pane's right edge while scrolling \
               through history, to drag or click, as in Ghostty",
        color: false,
        set: |c, v| {
            c.behaviour.scrollbar = match v {
                "system" => true,
                "never" => false,
                _ => return Err("expects system or never".into()),
            };
            Ok(())
        },
        get: |c| {
            Some(
                if c.behaviour.scrollbar {
                    "system"
                } else {
                    "never"
                }
                .into(),
            )
        },
    },
    Setting {
        name: "password-mask",
        value: "BOOL",
        help: "show a * for each character typed at a password prompt (a program reading a line without echoing \
               it, as sudo and ssh do), drawn at the cursor by mightty only, so typing can be seen to work",
        color: false,
        set: |c, v| {
            c.behaviour.password_mask = v.parse().map_err(|_| "expects true or false")?;
            Ok(())
        },
        get: |c| Some(c.behaviour.password_mask.to_string()),
    },
    Setting {
        name: "mouse-scroll-multiplier",
        value: "N",
        help: "scroll this many times as far as the wheel or trackpad says (0.01 to 10000)",
        color: false,
        set: |c, v| {
            c.behaviour.scroll_multiplier = v
                .parse()
                .ok()
                .filter(|m: &f64| (0.01..=10000.0).contains(m))
                .ok_or("expects a number from 0.01 to 10000")?;
            Ok(())
        },
        get: |c| Some(c.behaviour.scroll_multiplier.to_string()),
    },
    Setting {
        name: "keybind",
        value: "TRIGGER=ACTION",
        help: "bind keys to an action, as in Ghostty: cmd+shift+t=new_tab, ctrl+k=unbind, or clear \
               to drop every binding; repeat for more",
        color: false,
        set: |c, v| c.keybinds.apply(v),
        get: |_| None,
    },
    Setting {
        name: "theme",
        value: "NAME",
        help: "colours from themes/NAME in the config directory, or a path; light:NAME,dark:NAME \
               follows macOS's appearance",
        color: false,
        set: |c, v| {
            c.theme_name = (!v.is_empty()).then(|| v.to_owned());
            let pick = |side: &str| {
                v.split(',')
                    .find_map(|part| part.trim().strip_prefix(side))
                    .map(str::trim)
            };
            match (pick("light:"), pick("dark:")) {
                (Some(light), Some(dark)) => {
                    c.follows_appearance = true;
                    c.load_theme(if c.dark_mode { dark } else { light })
                }
                (None, None) => {
                    c.follows_appearance = false;
                    c.load_theme(v)
                }
                _ => Err("expects NAME, or light:NAME,dark:NAME".into()),
            }
        },
        get: |c| c.theme_name.clone(),
    },
    Setting {
        name: "background",
        value: "COLOR",
        help: "default background",
        color: true,
        set: |c, v| {
            c.theme.background = color(v)?;
            Ok(())
        },
        get: |c| Some(hex(c.theme.background)),
    },
    Setting {
        name: "foreground",
        value: "COLOR",
        help: "default text colour",
        color: true,
        set: |c, v| {
            c.theme.foreground = color(v)?;
            Ok(())
        },
        get: |c| Some(hex(c.theme.foreground)),
    },
    Setting {
        name: "cursor-color",
        value: "COLOR",
        help: "cursor (default: the text colour under it)",
        color: true,
        set: |c, v| {
            c.theme.cursor = Some(color(v)?);
            Ok(())
        },
        get: |c| c.theme.cursor.map(hex),
    },
    Setting {
        name: "cursor-text",
        value: "COLOR",
        help: "text under a block cursor (default: the cell's background)",
        color: true,
        set: |c, v| {
            c.theme.cursor_text = Some(color(v)?);
            Ok(())
        },
        get: |c| c.theme.cursor_text.map(hex),
    },
    Setting {
        name: "selection-background",
        value: "COLOR",
        help: "background of selected text",
        color: true,
        set: |c, v| {
            c.theme.selection_background = color(v)?;
            Ok(())
        },
        get: |c| Some(hex(c.theme.selection_background)),
    },
    Setting {
        name: "selection-foreground",
        value: "COLOR",
        help: "selected text (default: unchanged)",
        color: true,
        set: |c, v| {
            c.theme.selection_foreground = Some(color(v)?);
            Ok(())
        },
        get: |c| c.theme.selection_foreground.map(hex),
    },
    Setting {
        name: "search-foreground",
        value: "COLOR",
        help: "text of search matches",
        color: true,
        set: |c, v| {
            c.theme.search_foreground = color(v)?;
            Ok(())
        },
        get: |c| Some(hex(c.theme.search_foreground)),
    },
    Setting {
        name: "search-background",
        value: "COLOR",
        help: "background of search matches",
        color: true,
        set: |c, v| {
            c.theme.search_background = color(v)?;
            Ok(())
        },
        get: |c| Some(hex(c.theme.search_background)),
    },
    Setting {
        name: "search-selected-foreground",
        value: "COLOR",
        help: "text of the selected search match",
        color: true,
        set: |c, v| {
            c.theme.search_selected_foreground = color(v)?;
            Ok(())
        },
        get: |c| Some(hex(c.theme.search_selected_foreground)),
    },
    Setting {
        name: "search-selected-background",
        value: "COLOR",
        help: "background of the selected search match",
        color: true,
        set: |c, v| {
            c.theme.search_selected_background = color(v)?;
            Ok(())
        },
        get: |c| Some(hex(c.theme.search_selected_background)),
    },
    Setting {
        name: "palette",
        value: "N=COLOR",
        help: "palette entry N (0-255); repeat for more entries",
        color: true,
        set: |c, v| {
            let (index, value) = v.split_once('=').ok_or("expects N=COLOR")?;
            let index: u8 = index
                .trim()
                .parse()
                .map_err(|_| "expects an index from 0 to 255")?;
            c.theme.palette[index as usize] = color(value.trim())?;
            Ok(())
        },
        get: |_| None,
    },
    Setting {
        name: "command",
        value: "LINE",
        help: "run this shell command line in the first window instead of a login shell",
        color: false,
        set: |c, v| {
            c.command = Some(v.into());
            Ok(())
        },
        get: |c| c.command.clone(),
    },
    Setting {
        name: "working-directory",
        value: "DIR",
        help: "working directory of the first window (default: home)",
        color: false,
        set: |c, v| {
            c.cwd = Some(expand_home(v));
            Ok(())
        },
        get: |c| c.cwd.as_ref().map(|d| d.display().to_string()),
    },
];

/// Flags about config files themselves; read before any file is loaded.
const FILE_FLAGS: [(&str, &str); 2] = [
    (
        "config-file=PATH",
        "also load this config file, after the default one",
    ),
    ("config-default-files=false", "skip the default config file"),
];

fn set_variation(c: &mut Config, style: usize, v: &str) -> Result<(), String> {
    let list = &mut c.fonts.variations[style];
    if v.is_empty() {
        list.clear();
    } else {
        let (tag, value) = variation(v)?;
        list.retain(|a| a.0 != tag);
        list.push((tag, value));
    }
    Ok(())
}

fn show_variations(list: &[([u8; 4], f64)]) -> Option<String> {
    let show = |(tag, value): &([u8; 4], f64)| {
        format!("{}={value}", String::from_utf8_lossy(tag).trim_end())
    };
    (!list.is_empty()).then(|| list.iter().map(show).collect::<Vec<_>>().join(", "))
}

/// A number from `min` to 1.
fn fraction(v: &str, min: f64) -> Result<f64, String> {
    v.parse()
        .ok()
        .filter(|f: &f64| (min..=1.0).contains(f))
        .ok_or_else(|| format!("expects a number from {min} to 1"))
}

/// Points of padding, 0 to 200.
fn padding(v: &str) -> Result<f64, String> {
    v.parse()
        .ok()
        .filter(|p: &f64| (0.0..=200.0).contains(p))
        .ok_or_else(|| "expects points from 0 to 200".into())
}

fn cells(v: &str) -> Result<u16, String> {
    v.parse()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| "expects a whole number above 0".into())
}

/// `#rrggbb` or `rrggbb`.
fn color(v: &str) -> Result<Rgb, String> {
    let digits = v.strip_prefix('#').unwrap_or(v);
    let n = (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(digits, 16).ok())
        .flatten()
        .ok_or("expects a colour like #1e1e1e")?;
    Ok(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn hex((r, g, b): Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Where the bundled themes are: in the app's resources, or the source
/// tree's for a build run from `target`.
pub fn bundled_themes() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let in_app = exe.parent()?.parent()?.join("Resources").join("themes");
    let in_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes");
    [in_app, in_tree].into_iter().find(|dir| dir.is_dir())
}

/// Every theme by name, sorted ignoring case: the user's (`themes/` in the
/// config directory) and the bundled ones, a user's hiding a bundled one
/// of the same name.
pub fn theme_names() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for dir in [config_dir().map(|d| d.join("themes")), bundled_themes()]
        .into_iter()
        .flatten()
    {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') && entry.path().is_file() && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.sort_by_key(|n| n.to_lowercase());
    names
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// `$XDG_CONFIG_HOME/mightty`, else `~/.config/mightty`.
pub fn config_dir() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    base.or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))
        .map(|d| d.join("mightty"))
}

impl Config {
    /// Defaults, then config files, then `args`. Problems in a config file
    /// come back as warnings so a typo doesn't stop the terminal opening;
    /// bad flags are errors.
    pub fn load(args: &[String]) -> Result<(Self, Vec<String>), String> {
        Self::load_from(config_dir().map(|d| d.join("config")), args)
    }

    fn load_from(
        default_file: Option<PathBuf>,
        args: &[String],
    ) -> Result<(Self, Vec<String>), String> {
        let flags = &args[..args.iter().position(|a| a == "-e").unwrap_or(args.len())];
        let mut files = Vec::new();
        let mut default_files = true;
        for arg in flags {
            if let Some(v) = arg.strip_prefix("--config-default-files=") {
                default_files = v
                    .parse()
                    .map_err(|_| "config-default-files: expects true or false")?;
            } else if let Some(v) = arg.strip_prefix("--config-file=") {
                files.push(expand_home(v));
            }
        }

        let mut config = Self {
            default_file: default_file.clone(),
            args: args.to_vec(),
            dark_mode: system_dark(),
            ..Self::default()
        };
        let mut warnings = Vec::new();
        if default_files && let Some(path) = default_file {
            warnings.extend(config.apply_file(&path, true));
        }
        for path in files {
            let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            warnings.extend(config.apply_text(&text, &path, false));
        }
        config.apply_args(args)?;
        Ok((config, warnings))
    }

    /// Loads the settings again from where these came from: the config
    /// files as they are now, then the same flags.
    pub fn reload(&self) -> Result<(Self, Vec<String>), String> {
        Self::load_from(self.default_file.clone(), &self.args)
    }

    /// The default config file, unless `--config-default-files=false`.
    pub fn file(&self) -> Option<&Path> {
        let skipped = self
            .args
            .iter()
            .any(|a| a == "--config-default-files=false");
        self.default_file.as_deref().filter(|_| !skipped)
    }

    /// Applies the config file at `path`, returning warnings; a missing
    /// file is one unless `optional`.
    fn apply_file(&mut self, path: &Path, optional: bool) -> Vec<String> {
        // The same file however it's reached (`parts/../config`, links).
        let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if self.including.contains(&real) {
            return vec![format!(
                "{}: included again from itself (a config-file loop)",
                path.display()
            )];
        }
        if self.including.len() >= MAX_INCLUDE_DEPTH {
            return vec![format!(
                "{}: config-file nested more than {MAX_INCLUDE_DEPTH} deep",
                path.display()
            )];
        }
        match fs::read_to_string(path) {
            Ok(text) => {
                self.including.push(real);
                let warnings = self.apply_text(&text, path, false);
                self.including.pop();
                warnings
            }
            Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => vec![format!("{}: {e}", path.display())],
        }
    }

    /// Applies `name = value` lines, returning a warning per bad line.
    /// `config-file = PATH` lines (relative to `origin`'s directory; `?PATH`
    /// if it may be missing) are read after the rest, as in Ghostty.
    /// `colors_only` is for theme files.
    fn apply_text(&mut self, text: &str, origin: &Path, colors_only: bool) -> Vec<String> {
        let mut warnings = Vec::new();
        if !colors_only {
            self.fresh_families = true;
        }
        let mut includes = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let result = match line.split_once('=') {
                Some((name, value)) if name.trim() == "config-file" && !colors_only => {
                    let value = value.trim();
                    let value = value
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .unwrap_or(value);
                    let (optional, value) = value
                        .strip_prefix('?')
                        .map_or((false, value), |v| (true, v));
                    let path = expand_home(value);
                    let dir = origin.parent().unwrap_or(Path::new(""));
                    includes.push((dir.join(path), optional));
                    Ok(())
                }
                Some((name, value)) => {
                    let value = value.trim();
                    let value = value
                        .strip_prefix('"')
                        .and_then(|v| v.strip_suffix('"'))
                        .unwrap_or(value);
                    self.set_checked(name.trim(), value, colors_only)
                }
                None => Err("expected `name = value`".into()),
            };
            if let Err(e) = result {
                warnings.push(format!("{}:{}: {e}", origin.display(), n + 1));
            }
        }
        for (path, optional) in includes {
            warnings.extend(self.apply_file(&path, optional));
        }
        warnings
    }

    fn load_theme(&mut self, name: &str) -> Result<(), String> {
        let path = if name.contains('/') {
            expand_home(name)
        } else {
            // The user's own first, then those bundled.
            let own = config_dir()
                .ok_or("no config directory (HOME is unset)")?
                .join("themes")
                .join(name);
            match bundled_themes().map(|dir| dir.join(name)) {
                Some(bundled) if !own.exists() && bundled.exists() => bundled,
                _ => own,
            }
        };
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        match self.apply_text(&text, &path, true).as_slice() {
            [] => Ok(()),
            problems => Err(problems.join("; ")),
        }
    }

    /// Sets the setting `name` from its text form.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        self.set_checked(name, value, false)
    }

    fn set_checked(&mut self, name: &str, value: &str, colors_only: bool) -> Result<(), String> {
        let setting = SETTINGS
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("unknown setting `{name}`"))?;
        if colors_only && !setting.color {
            return Err(format!(
                "`{name}` isn't a colour setting, so it can't be in a theme"
            ));
        }
        (setting.set)(self, value).map_err(|e| format!("{name}: {e}"))
    }

    /// Applies `--name=value` flags, then `-e program [args...]`, which takes
    /// the rest of the arguments and runs in the current directory unless
    /// `--working-directory` says otherwise.
    pub fn apply_args(&mut self, args: &[String]) -> Result<(), String> {
        self.fresh_families = true;
        for (i, arg) in args.iter().enumerate() {
            if arg == "-e" {
                let program = &args[i + 1..];
                if program.is_empty() {
                    return Err("-e expects a program".into());
                }
                self.command = Some(format!(
                    "exec {}",
                    program
                        .iter()
                        .map(|a| shell_quote(a))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
                if self.cwd.is_none() {
                    self.cwd = env::current_dir().ok();
                }
                break;
            }
            let (name, value) = arg
                .strip_prefix("--")
                .and_then(|a| a.split_once('='))
                .ok_or_else(|| {
                    format!("unexpected argument `{arg}`; settings are written --name=value")
                })?;
            if !matches!(name, "config-file" | "config-default-files") {
                self.set(name, value)?;
            }
        }
        Ok(())
    }

    pub fn term_options(&self) -> TermOptions {
        TermOptions {
            scrollback_lines: self.scrollback_lines,
            colors: Colors {
                palette: self.theme.palette,
                foreground: self.theme.foreground,
                background: self.theme.background,
                cursor: self.theme.cursor,
            },
            wide_emoji: self.wide_emoji,
        }
    }

    /// Where settings come from, then one line per setting with its
    /// current value.
    /// Every setting with its current value, for `mightty config`.
    pub fn settings(&self) -> Vec<SettingView> {
        SETTINGS
            .iter()
            .map(|s| SettingView {
                name: s.name,
                value: s.value,
                help: s.help,
                current: (s.get)(self),
                many: MANY.contains(&s.name),
            })
            .collect()
    }

    pub fn help(&self) -> String {
        let file = config_dir().map_or("~/.config/mightty".into(), |d| d.display().to_string());
        let mut out = format!(
            "Settings are read from {file}/config (`name = value` lines),\n\
             then from --name=value flags, which win. [current value]\n\n"
        );
        let mut flags: Vec<(String, String)> = SETTINGS
            .iter()
            .map(|s| (format!("--{}={}", s.name, s.value), s.help.to_owned()))
            .collect();
        for (s, value) in SETTINGS.iter().zip(&mut flags) {
            if let Some(current) = (s.get)(self) {
                value.1 += &format!(" [{current}]");
            }
        }
        flags.extend(
            FILE_FLAGS
                .iter()
                .map(|(flag, help)| (format!("--{flag}"), (*help).to_owned())),
        );
        let width = flags.iter().map(|(f, _)| f.len()).max().unwrap_or(0);
        for (flag, help) in flags {
            out += &format!("  {flag:width$}  {help}\n");
        }
        out
    }
}

/// `arg` as one word for `sh -c`.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b))
    {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// A fresh directory for config files.
    fn scratch(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("mightty-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn flags_set_settings() {
        let mut c = Config::default();
        c.apply_args(&args(&[
            "--window-width=120",
            "--window-height=40",
            "--font-family=SF Mono",
            "--font-size=13.5",
            "--window-vsync=false",
        ]))
        .unwrap();
        assert_eq!(
            (
                c.cols,
                c.rows,
                c.fonts.families[0].as_str(),
                c.font_size,
                c.vsync
            ),
            (120, 40, "SF Mono", 13.5, false)
        );
        c.apply_args(&args(&["--macos-option-as-alt=left"]))
            .unwrap();
        assert_eq!(c.option_as_alt, Some(OptionAsAlt::Left));
    }

    #[test]
    fn confirm_close_values() {
        let mut c = Config::default();
        assert_eq!(c.confirm_close, ConfirmClose::Running);
        for (value, expected) in [
            ("always", ConfirmClose::Always),
            ("false", ConfirmClose::Never),
            ("true", ConfirmClose::Running),
        ] {
            c.apply_args(&args(&[&format!("--confirm-close-surface={value}")]))
                .unwrap();
            assert_eq!(c.confirm_close, expected);
        }
        assert!(
            c.apply_args(&args(&["--confirm-close-surface=sometimes"]))
                .is_err()
        );
    }

    #[test]
    fn save_state_values() {
        let mut c = Config::default();
        assert_eq!(c.save_state, SaveState::Default);
        c.apply_args(&args(&["--window-save-state=always"]))
            .unwrap();
        assert_eq!(c.save_state, SaveState::Always);
        assert!(
            c.apply_args(&args(&["--window-save-state=sometimes"]))
                .is_err()
        );
    }

    #[test]
    fn option_as_alt_picks_keys() {
        let (left, right) = (0x20 | 1 << 19, 0x40 | 1 << 19);
        let us = || true;
        assert!(
            OptionAsAlt::applies(None, left, us) && !OptionAsAlt::applies(None, left, || false)
        );
        assert!(OptionAsAlt::applies(Some(OptionAsAlt::Left), left, us));
        assert!(!OptionAsAlt::applies(Some(OptionAsAlt::Left), right, us));
        assert!(OptionAsAlt::applies(Some(OptionAsAlt::Right), right, us));
        assert!(!OptionAsAlt::applies(Some(OptionAsAlt::None), left, us));
        assert!(OptionAsAlt::applies(Some(OptionAsAlt::Both), right, || {
            false
        }));
    }

    #[test]
    fn bad_flags_are_rejected() {
        for bad in [
            "--window-width=0",
            "--window-width=x",
            "--font-size=-1",
            "--font-size=1000",
            "--window-vsync=no",
            "--macos-option-as-alt=both",
            "--nope=1",
            "--window-width",
            "stray",
            "--background=#12345",
            "--background=+12345",
            "--palette=256=#000000",
            "--palette=#000000",
        ] {
            assert!(
                Config::default().apply_args(&args(&[bad])).is_err(),
                "{bad}"
            );
        }
        assert!(Config::default().apply_args(&args(&["-e"])).is_err());
    }

    #[test]
    fn e_takes_the_rest() {
        let mut c = Config::default();
        c.apply_args(&args(&[
            "--working-directory=/tmp",
            "-e",
            "echo",
            "it's",
            "--window-width=5",
        ]))
        .unwrap();
        assert_eq!(
            c.command.as_deref(),
            Some("exec echo 'it'\\''s' --window-width=5")
        );
        assert_eq!(c.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(c.cols, 100);
    }

    #[test]
    fn file_then_flags() {
        let dir = scratch("file");
        let file = dir.join("config");
        fs::write(
            &file,
            "# comment\n\nwindow-width = 90\nfont-family = \"Iosevka Term\"\nbackground = 101010\n\
             palette = 1=#ff0000\nwindow-height = 0\nnonsense\nwindow-width = 91\n",
        )
        .unwrap();
        let (c, warnings) =
            Config::load_from(Some(file.clone()), &args(&["--window-width=120"])).unwrap();
        assert_eq!(
            (c.cols, c.rows, c.fonts.families.join(",")),
            (120, 30, "Iosevka Term".into())
        );
        assert_eq!(
            (c.theme.background, c.theme.palette[1]),
            ((0x10, 0x10, 0x10), (0xff, 0, 0))
        );
        assert_eq!(c.term_options().colors.background, (0x10, 0x10, 0x10));
        let at = |line| format!("{}:{line}: ", file.display());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].starts_with(&at(7)) && warnings[0].contains("window-height"));
        assert!(warnings[1].starts_with(&at(8)));

        // Skipping the default file, and loading another one.
        let (c, _) =
            Config::load_from(Some(file.clone()), &args(&["--config-default-files=false"]))
                .unwrap();
        assert_eq!(c.cols, 100);
        let other = dir.join("other");
        fs::write(&other, "window-width = 70\n").unwrap();
        let flag = format!("--config-file={}", other.display());
        let (c, _) = Config::load_from(Some(file), &args(&[&flag])).unwrap();
        assert_eq!(c.cols, 70);
        assert!(Config::load_from(None, &args(&["--config-file=/nonexistent/mightty"])).is_err());
        let (c, warnings) = Config::load_from(Some(dir.join("missing")), &[]).unwrap();
        assert!(warnings.is_empty() && c.cols == 100);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_are_written_in_place() {
        let dir = scratch("write");
        let file = dir.join("config");
        fs::write(
            &file,
            "# mine\nfont-size = 13\n# font-size = 9\nkeybind = cmd+t=new_tab\nfont-size=14\n",
        )
        .unwrap();
        write_setting(&file, "font-size", Some("15")).unwrap();
        write_setting(&file, "theme", Some("Nord")).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "# mine\nfont-size = 13\n# font-size = 9\nkeybind = cmd+t=new_tab\nfont-size = 15\ntheme = Nord\n"
        );
        write_setting(&file, "font-size", None).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "# mine\n# font-size = 9\nkeybind = cmd+t=new_tab\ntheme = Nord\n"
        );
        assert!(check("font-size", "abc").is_err() && check("font-size", "16").is_ok());
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn behaviour_settings() {
        let mut c = Config::default();
        assert_eq!(
            (c.behaviour.clipboard_read, c.behaviour.clipboard_write),
            (Permission::Ask, Permission::Allow)
        );
        c.apply_args(&args(&[
            "--clipboard-read=deny",
            "--clipboard-write=ask",
            "--copy-on-select=clipboard",
            "--clipboard-paste-protection=false",
            "--mouse-hide-while-typing=true",
            "--mouse-scroll-multiplier=3",
            "--scrollbar=never",
            "--macos-titlebar-style=hidden",
            "--bell-features=no-system,border",
        ]))
        .unwrap();
        let b = c.behaviour;
        assert!(!b.paste_protection && Config::default().behaviour.paste_protection);
        assert!(!b.scrollbar && Config::default().behaviour.scrollbar);
        assert_eq!(
            (b.titlebar, Config::default().behaviour.titlebar),
            (Titlebar::Hidden, Titlebar::Native)
        );
        assert_eq!(
            b.bell,
            Bell {
                system: false,
                audio: false,
                attention: false,
                title: true,
                border: true
            }
        );
        assert_eq!(
            (
                b.clipboard_read,
                b.clipboard_write,
                b.copy_on_select,
                b.hide_mouse,
                b.scroll_multiplier
            ),
            (Permission::Deny, Permission::Ask, true, true, 3.0)
        );
        for bad in [
            "--clipboard-read=sometimes",
            "--mouse-scroll-multiplier=0",
            "--copy-on-select=primary",
            "--scrollbar=auto",
            "--macos-titlebar-style=chrome",
            "--bell-features=loud",
        ] {
            assert!(
                Config::default().apply_args(&args(&[bad])).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn config_file_includes() {
        let dir = scratch("include");
        fs::create_dir_all(dir.join("parts")).unwrap();
        fs::write(dir.join("config"), "config-file = parts/look\nconfig-file = ?parts/missing\nconfig-file = parts/gone\nwindow-width = 90\n").unwrap();
        // Included files are read after the including one, so they win.
        fs::write(
            dir.join("parts/look"),
            "window-width = 70\nconfig-file = ../config\n",
        )
        .unwrap();
        let (c, warnings) = Config::load_from(Some(dir.join("config")), &[]).unwrap();
        assert_eq!(c.cols, 70);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("loop")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("gone")), "{warnings:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn theme_follows_appearance() {
        let dir = scratch("appearance");
        fs::write(dir.join("day"), "background = #ffffff\n").unwrap();
        fs::write(dir.join("night"), "background = #000000\n").unwrap();
        let value = format!("--theme=light:{0}/day, dark:{0}/night", dir.display());
        for (dark, background) in [(false, (255, 255, 255)), (true, (0, 0, 0))] {
            let mut c = Config {
                dark_mode: dark,
                ..Config::default()
            };
            c.apply_args(&args(&[&value])).unwrap();
            assert_eq!(
                (c.theme.background, c.follows_appearance),
                (background, true)
            );
        }
        let mut c = Config::default();
        c.apply_args(&args(&[&format!("--theme={}/day", dir.display())]))
            .unwrap();
        assert!(!c.follows_appearance);
        assert!(
            Config::default()
                .apply_args(&args(&["--theme=light:day"]))
                .is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn font_settings() {
        let dir = scratch("fonts");
        let file = dir.join("config");
        fs::write(
            &file,
            "font-family = JetBrains Mono\nfont-family = Symbols Nerd Font\nfont-family-bold = Iosevka Heavy\n\
             font-feature = ss01\nfont-feature = -calt\nfont-feature = cv01=2\nfont-feature = +ss01\n\
             font-variation = wght=450\nfont-variation-italic = slnt=-10\n",
        )
        .unwrap();
        let (c, warnings) = Config::load_from(Some(file.clone()), &[]).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(c.fonts.families, ["JetBrains Mono", "Symbols Nerd Font"]);
        assert_eq!(c.fonts.styled, [Some("Iosevka Heavy".into()), None, None]);
        assert_eq!(
            c.fonts.features,
            [(*b"calt", 0), (*b"cv01", 2), (*b"ss01", 1)]
        );
        assert_eq!(c.fonts.variations[0], [(*b"wght", 450.0)]);
        assert_eq!(c.fonts.variations[2], [(*b"slnt", -10.0)]);
        // Flags start a new list of families; an empty one clears it.
        let (c, _) =
            Config::load_from(Some(file.clone()), &args(&["--font-family=Menlo"])).unwrap();
        assert_eq!(c.fonts.families, ["Menlo"]);
        let (c, _) = Config::load_from(
            Some(file),
            &args(&["--font-family=", "--font-family=A", "--font-family=B"]),
        )
        .unwrap();
        assert_eq!(c.fonts.families, ["A", "B"]);
        for bad in [
            "--font-feature=toolong",
            "--font-feature=cv01=x",
            "--font-variation=wght",
            "--font-variation=wg!t=1",
        ] {
            assert!(
                Config::default().apply_args(&args(&[bad])).is_err(),
                "{bad}"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn look_settings() {
        let mut c = Config::default();
        c.apply_args(&args(&[
            "--background-opacity=0.85",
            "--background-blur=true",
            "--window-padding-x=12",
            "--cursor-style=bar",
            "--cursor-style-blink=true",
            "--adjust-cell-height=10%",
            "--adjust-cell-width=-1",
            "--unfocused-split-opacity=0.5",
            "--window-colorspace=display-p3",
            "--quick-terminal-position=bottom",
            "--quick-terminal-autohide=false",
            "--quick-terminal-size=30%,800px",
            "--undo-timeout=1.5s",
        ]))
        .unwrap();
        assert_eq!(c.undo_timeout, std::time::Duration::from_millis(1500));
        let mut d = Config::default();
        d.apply_args(&args(&["--undo-timeout=250ms"])).unwrap();
        assert_eq!(d.undo_timeout, std::time::Duration::from_millis(250));
        d.apply_args(&args(&["--undo-timeout=0"])).unwrap();
        assert_eq!(d.undo_timeout, std::time::Duration::ZERO);
        assert_eq!(
            (c.quick_position, c.quick_autohide),
            (QuickPosition::Bottom, false)
        );
        assert_eq!(
            c.quick_size,
            (Extent::Percent(30.0), Some(Extent::Points(800.0)))
        );
        assert_eq!(
            (
                Extent::Percent(30.0).of(1000.0),
                Extent::Points(800.0).of(600.0)
            ),
            (300.0, 600.0)
        );
        assert_eq!(c.colorspace, Colorspace::DisplayP3);
        assert_eq!(
            (c.look.background_opacity, c.background_blur, c.look.padding),
            (0.85, 20, (12.0, 6.0))
        );
        assert_eq!(
            (c.look.cursor_style, c.look.cursor_blink),
            (CursorStyle::Bar, Some(true))
        );
        assert_eq!(
            c.look.cell_adjust,
            (Adjust::Points(-1.0), Adjust::Percent(10.0))
        );
        assert_eq!(c.unfocused_split_opacity, 0.5);
        assert_eq!(Adjust::Percent(10.0).apply(30, 2.0), 33);
        assert_eq!(Adjust::Points(-1.0).apply(16, 2.0), 14);
        assert_eq!(Adjust::Points(-50.0).apply(16, 2.0), 1);
        assert_eq!(Config::default().colorspace, Colorspace::Srgb);
        for bad in [
            "--background-opacity=1.5",
            "--undo-timeout=5",
            "--undo-timeout=2h",
            "--window-colorspace=p3",
            "--quick-terminal-position=middle",
            "--quick-terminal-size=0%",
            "--quick-terminal-size=40",
            "--cursor-style=beam",
            "--adjust-cell-height=-100%",
            "--unfocused-split-opacity=0.1",
        ] {
            assert!(
                Config::default().apply_args(&args(&[bad])).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn reload_reads_the_file_again_and_keeps_flags() {
        let dir = scratch("reload");
        let file = dir.join("config");
        fs::write(&file, "font-size = 12\nwindow-width = 90\n").unwrap();
        let (c, _) = Config::load_from(Some(file.clone()), &args(&["--window-width=120"])).unwrap();
        assert_eq!(c.file(), Some(file.as_path()));
        fs::write(&file, "font-size = 16\nwindow-width = 80\nbogus = 1\n").unwrap();
        let (c, warnings) = c.reload().unwrap();
        assert_eq!((c.font_size, c.cols), (16.0, 120));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let (c, _) =
            Config::load_from(Some(file), &args(&["--config-default-files=false"])).unwrap();
        assert_eq!(c.file(), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn themes_set_colors_only() {
        let dir = scratch("theme");
        // The shape of a Ghostty theme file.
        let theme = dir.join("dusk");
        fs::write(
            &theme,
            "palette = 0=#1d1f21\npalette = 15=#ffffff\nbackground = #1d1f21\nforeground = #c5c8c6\n\
             cursor-color = #c5c8c6\ncursor-text = #1d1f21\nselection-background = #373b41\n\
             selection-foreground = #c5c8c6\nsearch-background = #f0c674\nsearch-selected-background = #de935f\n",
        )
        .unwrap();
        let mut c = Config::default();
        c.set("theme", theme.to_str().unwrap()).unwrap();
        c.set("foreground", "#000001").unwrap(); // later settings win
        assert_eq!(c.theme.background, (0x1d, 0x1f, 0x21));
        assert_eq!(c.theme.foreground, (0, 0, 1));
        assert_eq!(c.theme.palette[15], (0xff, 0xff, 0xff));
        assert_eq!(c.theme.cursor, Some((0xc5, 0xc8, 0xc6)));
        assert_eq!(c.theme.selection_foreground, Some((0xc5, 0xc8, 0xc6)));
        assert_eq!(
            (
                c.theme.search_background,
                c.theme.search_selected_background
            ),
            ((0xf0, 0xc6, 0x74), (0xde, 0x93, 0x5f))
        );

        let sneaky = dir.join("sneaky");
        fs::write(&sneaky, "background = #000000\ncommand = rm -rf ~\n").unwrap();
        let mut c = Config::default();
        let err = c.set("theme", sneaky.to_str().unwrap()).unwrap_err();
        assert!(err.contains(":2:") && err.contains("colour"), "{err}");
        assert_eq!(c.command, None);
        assert!(
            Config::default()
                .set("theme", "/nonexistent/theme")
                .is_err()
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn help_lists_every_setting() {
        let help = Config::default().help();
        for s in SETTINGS {
            assert!(help.contains(&format!("--{}=", s.name)), "{}", s.name);
        }
        assert!(
            help.contains("--config-file=PATH")
                && help.contains("[Menlo]")
                && help.contains("[#1e1e1e]")
        );
    }
}
