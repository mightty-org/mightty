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
use mightty_term::{DEFAULT_SCROLLBACK, TermOptions};

pub struct Config {
    pub font: String,
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
    /// Draw the font's ligatures and contextual alternates (`->` as an arrow).
    pub ligatures: bool,
    /// `keybind`: the defaults, changed by each setting in order.
    pub keybinds: Bindings,
    /// How panes look beyond colours and fonts.
    pub look: Look,
    /// `background-blur`: blur radius behind a translucent window, in points; 0 for none.
    pub background_blur: u32,
    /// `unfocused-split-opacity`: panes keys aren't going to are dimmed to this.
    pub unfocused_split_opacity: f64,
    /// Where this came from, to load it again (see `Config::reload`): the
    /// default file, and the command line.
    default_file: Option<PathBuf>,
    args: Vec<String>,
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

impl Adjust {
    /// `pixels` adjusted, at `scale` pixels per point; at least 1.
    pub fn apply(self, pixels: u32, scale: f64) -> u32 {
        let adjusted = match self {
            Self::Points(p) => pixels as f64 + p * scale,
            Self::Percent(p) => pixels as f64 * (1.0 + p / 100.0),
        };
        adjusted.round().max(1.0) as u32
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
            font: "Menlo".into(),
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
            ligatures: true,
            keybinds: Bindings::default(),
            look: Look::default(),
            background_blur: 0,
            unfocused_split_opacity: 0.7,
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

/// Names are Ghostty's where the meaning is the same, so Ghostty theme
/// files work unchanged.
const SETTINGS: &[Setting] = &[
    Setting {
        name: "font-family",
        value: "NAME",
        help: "font family",
        color: false,
        set: |c, v| {
            c.font = v.into();
            Ok(())
        },
        get: |c| Some(c.font.clone()),
    },
    Setting {
        name: "font-size",
        value: "POINTS",
        help: "font size",
        color: false,
        set: |c, v| {
            c.font_size = v.parse().ok().filter(|&p: &f64| p.is_finite() && p > 0.0).ok_or("expects a positive number")?;
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
        get: |c| Some(if c.font_backend == FontBackend::Mightty { "mightty" } else { "coretext" }.into()),
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
                _ => v.parse().ok().filter(|&r| r <= 100).ok_or("expects true, false or a radius from 0 to 100")?,
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
        help: "colours from themes/NAME in the config directory, or a path",
        color: false,
        set: |c, v| c.load_theme(v),
        get: |_| None,
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
            let index: u8 = index.trim().parse().map_err(|_| "expects an index from 0 to 255")?;
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
    ("config-file=PATH", "also load this config file, after the default one"),
    ("config-default-files=false", "skip the default config file"),
];

/// A number from `min` to 1.
fn fraction(v: &str, min: f64) -> Result<f64, String> {
    v.parse().ok().filter(|f: &f64| (min..=1.0).contains(f)).ok_or_else(|| format!("expects a number from {min} to 1"))
}

/// Points of padding, 0 to 200.
fn padding(v: &str) -> Result<f64, String> {
    v.parse().ok().filter(|p: &f64| (0.0..=200.0).contains(p)).ok_or_else(|| "expects points from 0 to 200".into())
}

fn cells(v: &str) -> Result<u16, String> {
    v.parse().ok().filter(|&n| n > 0).ok_or_else(|| "expects a whole number above 0".into())
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

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// `$XDG_CONFIG_HOME/mightty`, else `~/.config/mightty`.
pub fn config_dir() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()).map(PathBuf::from);
    base.or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config"))).map(|d| d.join("mightty"))
}

impl Config {
    /// Defaults, then config files, then `args`. Problems in a config file
    /// come back as warnings so a typo doesn't stop the terminal opening;
    /// bad flags are errors.
    pub fn load(args: &[String]) -> Result<(Self, Vec<String>), String> {
        Self::load_from(config_dir().map(|d| d.join("config")), args)
    }

    fn load_from(default_file: Option<PathBuf>, args: &[String]) -> Result<(Self, Vec<String>), String> {
        let flags = &args[..args.iter().position(|a| a == "-e").unwrap_or(args.len())];
        let mut files = Vec::new();
        let mut default_files = true;
        for arg in flags {
            if let Some(v) = arg.strip_prefix("--config-default-files=") {
                default_files = v.parse().map_err(|_| "config-default-files: expects true or false")?;
            } else if let Some(v) = arg.strip_prefix("--config-file=") {
                files.push(expand_home(v));
            }
        }

        let mut config = Self { default_file: default_file.clone(), args: args.to_vec(), ..Self::default() };
        let mut warnings = Vec::new();
        if default_files && let Some(path) = default_file {
            match fs::read_to_string(&path) {
                Ok(text) => warnings.extend(config.apply_text(&text, &path, false)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warnings.push(format!("{}: {e}", path.display())),
            }
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
        let skipped = self.args.iter().any(|a| a == "--config-default-files=false");
        self.default_file.as_deref().filter(|_| !skipped)
    }

    /// Applies `name = value` lines, returning a warning per bad line.
    /// `colors_only` is for theme files.
    fn apply_text(&mut self, text: &str, origin: &Path, colors_only: bool) -> Vec<String> {
        let mut warnings = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let result = match line.split_once('=') {
                Some((name, value)) => {
                    let value = value.trim();
                    let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
                    self.set_checked(name.trim(), value, colors_only)
                }
                None => Err("expected `name = value`".into()),
            };
            if let Err(e) = result {
                warnings.push(format!("{}:{}: {e}", origin.display(), n + 1));
            }
        }
        warnings
    }

    fn load_theme(&mut self, name: &str) -> Result<(), String> {
        let path = if name.contains('/') {
            expand_home(name)
        } else {
            config_dir().ok_or("no config directory (HOME is unset)")?.join("themes").join(name)
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
        let setting = SETTINGS.iter().find(|s| s.name == name).ok_or_else(|| format!("unknown setting `{name}`"))?;
        if colors_only && !setting.color {
            return Err(format!("`{name}` isn't a colour setting, so it can't be in a theme"));
        }
        (setting.set)(self, value).map_err(|e| format!("{name}: {e}"))
    }

    /// Applies `--name=value` flags, then `-e program [args...]`, which takes
    /// the rest of the arguments and runs in the current directory unless
    /// `--working-directory` says otherwise.
    pub fn apply_args(&mut self, args: &[String]) -> Result<(), String> {
        for (i, arg) in args.iter().enumerate() {
            if arg == "-e" {
                let program = &args[i + 1..];
                if program.is_empty() {
                    return Err("-e expects a program".into());
                }
                self.command = Some(format!("exec {}", program.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")));
                if self.cwd.is_none() {
                    self.cwd = env::current_dir().ok();
                }
                break;
            }
            let (name, value) = arg
                .strip_prefix("--")
                .and_then(|a| a.split_once('='))
                .ok_or_else(|| format!("unexpected argument `{arg}`; settings are written --name=value"))?;
            if !matches!(name, "config-file" | "config-default-files") {
                self.set(name, value)?;
            }
        }
        Ok(())
    }

    pub fn term_options(&self) -> TermOptions {
        TermOptions {
            scrollback_lines: self.scrollback_lines,
            foreground: self.theme.foreground,
            background: self.theme.background,
        }
    }

    /// Where settings come from, then one line per setting with its
    /// current value.
    pub fn help(&self) -> String {
        let file = config_dir().map_or("~/.config/mightty".into(), |d| d.display().to_string());
        let mut out = format!(
            "Settings are read from {file}/config (`name = value` lines),\n\
             then from --name=value flags, which win. [current value]\n\n"
        );
        let mut flags: Vec<(String, String)> =
            SETTINGS.iter().map(|s| (format!("--{}={}", s.name, s.value), s.help.to_owned())).collect();
        for (s, value) in SETTINGS.iter().zip(&mut flags) {
            if let Some(current) = (s.get)(self) {
                value.1 += &format!(" [{current}]");
            }
        }
        flags.extend(FILE_FLAGS.iter().map(|(flag, help)| (format!("--{flag}"), (*help).to_owned())));
        let width = flags.iter().map(|(f, _)| f.len()).max().unwrap_or(0);
        for (flag, help) in flags {
            out += &format!("  {flag:width$}  {help}\n");
        }
        out
    }
}

/// `arg` as one word for `sh -c`.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b)) {
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
        c.apply_args(&args(&["--window-width=120", "--window-height=40", "--font-family=SF Mono", "--font-size=13.5", "--window-vsync=false"]))
            .unwrap();
        assert_eq!((c.cols, c.rows, c.font.as_str(), c.font_size, c.vsync), (120, 40, "SF Mono", 13.5, false));
        c.apply_args(&args(&["--macos-option-as-alt=left"])).unwrap();
        assert_eq!(c.option_as_alt, Some(OptionAsAlt::Left));
    }

    #[test]
    fn confirm_close_values() {
        let mut c = Config::default();
        assert_eq!(c.confirm_close, ConfirmClose::Running);
        for (value, expected) in [("always", ConfirmClose::Always), ("false", ConfirmClose::Never), ("true", ConfirmClose::Running)] {
            c.apply_args(&args(&[&format!("--confirm-close-surface={value}")])).unwrap();
            assert_eq!(c.confirm_close, expected);
        }
        assert!(c.apply_args(&args(&["--confirm-close-surface=sometimes"])).is_err());
    }

    #[test]
    fn save_state_values() {
        let mut c = Config::default();
        assert_eq!(c.save_state, SaveState::Default);
        c.apply_args(&args(&["--window-save-state=always"])).unwrap();
        assert_eq!(c.save_state, SaveState::Always);
        assert!(c.apply_args(&args(&["--window-save-state=sometimes"])).is_err());
    }

    #[test]
    fn option_as_alt_picks_keys() {
        let (left, right) = (0x20 | 1 << 19, 0x40 | 1 << 19);
        let us = || true;
        assert!(OptionAsAlt::applies(None, left, us) && !OptionAsAlt::applies(None, left, || false));
        assert!(OptionAsAlt::applies(Some(OptionAsAlt::Left), left, us));
        assert!(!OptionAsAlt::applies(Some(OptionAsAlt::Left), right, us));
        assert!(OptionAsAlt::applies(Some(OptionAsAlt::Right), right, us));
        assert!(!OptionAsAlt::applies(Some(OptionAsAlt::None), left, us));
        assert!(OptionAsAlt::applies(Some(OptionAsAlt::Both), right, || false));
    }

    #[test]
    fn bad_flags_are_rejected() {
        for bad in [
            "--window-width=0",
            "--window-width=x",
            "--font-size=-1",
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
            assert!(Config::default().apply_args(&args(&[bad])).is_err(), "{bad}");
        }
        assert!(Config::default().apply_args(&args(&["-e"])).is_err());
    }

    #[test]
    fn e_takes_the_rest() {
        let mut c = Config::default();
        c.apply_args(&args(&["--working-directory=/tmp", "-e", "echo", "it's", "--window-width=5"])).unwrap();
        assert_eq!(c.command.as_deref(), Some("exec echo 'it'\\''s' --window-width=5"));
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
        let (c, warnings) = Config::load_from(Some(file.clone()), &args(&["--window-width=120"])).unwrap();
        assert_eq!((c.cols, c.rows, c.font.as_str()), (120, 30, "Iosevka Term"));
        assert_eq!((c.theme.background, c.theme.palette[1]), ((0x10, 0x10, 0x10), (0xff, 0, 0)));
        assert_eq!(c.term_options().background, (0x10, 0x10, 0x10));
        let at = |line| format!("{}:{line}: ", file.display());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].starts_with(&at(7)) && warnings[0].contains("window-height"));
        assert!(warnings[1].starts_with(&at(8)));

        // Skipping the default file, and loading another one.
        let (c, _) = Config::load_from(Some(file.clone()), &args(&["--config-default-files=false"])).unwrap();
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
        ]))
        .unwrap();
        assert_eq!((c.look.background_opacity, c.background_blur, c.look.padding), (0.85, 20, (12.0, 6.0)));
        assert_eq!((c.look.cursor_style, c.look.cursor_blink), (CursorStyle::Bar, Some(true)));
        assert_eq!(c.look.cell_adjust, (Adjust::Points(-1.0), Adjust::Percent(10.0)));
        assert_eq!(c.unfocused_split_opacity, 0.5);
        assert_eq!(Adjust::Percent(10.0).apply(30, 2.0), 33);
        assert_eq!(Adjust::Points(-1.0).apply(16, 2.0), 14);
        assert_eq!(Adjust::Points(-50.0).apply(16, 2.0), 1);
        for bad in ["--background-opacity=1.5", "--cursor-style=beam", "--adjust-cell-height=-100%", "--unfocused-split-opacity=0.1"] {
            assert!(Config::default().apply_args(&args(&[bad])).is_err(), "{bad}");
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
        let (c, _) = Config::load_from(Some(file), &args(&["--config-default-files=false"])).unwrap();
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
        assert_eq!((c.theme.search_background, c.theme.search_selected_background), ((0xf0, 0xc6, 0x74), (0xde, 0x93, 0x5f)));

        let sneaky = dir.join("sneaky");
        fs::write(&sneaky, "background = #000000\ncommand = rm -rf ~\n").unwrap();
        let mut c = Config::default();
        let err = c.set("theme", sneaky.to_str().unwrap()).unwrap_err();
        assert!(err.contains(":2:") && err.contains("colour"), "{err}");
        assert_eq!(c.command, None);
        assert!(Config::default().set("theme", "/nonexistent/theme").is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn help_lists_every_setting() {
        let help = Config::default().help();
        for s in SETTINGS {
            assert!(help.contains(&format!("--{}=", s.name)), "{}", s.name);
        }
        assert!(help.contains("--config-file=PATH") && help.contains("[Menlo]") && help.contains("[#1e1e1e]"));
    }
}
