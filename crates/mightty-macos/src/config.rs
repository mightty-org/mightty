//! Settings for the GUI. Each one is declared once in [`SETTINGS`]; command
//! line flags (`--name=value`) and the help text come from that table, and
//! a config file will too.

use std::env;
use std::path::PathBuf;

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
}

impl Default for Config {
    fn default() -> Self {
        Self { font: "Menlo".into(), font_size: 14.0, cols: 100, rows: 30, command: None, cwd: None }
    }
}

struct Setting {
    name: &'static str,
    /// Placeholder for the value in help text.
    value: &'static str,
    help: &'static str,
    set: fn(&mut Config, &str) -> Result<(), String>,
    /// The current value for help text, or `None` when unset.
    get: fn(&Config) -> Option<String>,
}

/// Names follow Ghostty's where the meaning is the same.
const SETTINGS: &[Setting] = &[
    Setting {
        name: "font-family",
        value: "NAME",
        help: "font family",
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
        set: |c, v| {
            c.font_size = v.parse().ok().filter(|&p: &f64| p.is_finite() && p > 0.0).ok_or("expects a positive number")?;
            Ok(())
        },
        get: |c| Some(c.font_size.to_string()),
    },
    Setting {
        name: "window-width",
        value: "COLUMNS",
        help: "width of new windows, in cells",
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
        set: |c, v| {
            c.rows = cells(v)?;
            Ok(())
        },
        get: |c| Some(c.rows.to_string()),
    },
    Setting {
        name: "command",
        value: "LINE",
        help: "run this shell command line in the first window instead of a login shell",
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
        set: |c, v| {
            c.cwd = Some(v.into());
            Ok(())
        },
        get: |c| c.cwd.as_ref().map(|d| d.display().to_string()),
    },
];

fn cells(v: &str) -> Result<u16, String> {
    v.parse().ok().filter(|&n| n > 0).ok_or_else(|| "expects a whole number above 0".into())
}

impl Config {
    /// Sets the setting `name` from its text form.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        let setting = SETTINGS.iter().find(|s| s.name == name).ok_or_else(|| format!("unknown setting `{name}`"))?;
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
            self.set(name, value)?;
        }
        Ok(())
    }

    /// One line per setting, with its current value.
    pub fn help(&self) -> String {
        let flags: Vec<String> = SETTINGS.iter().map(|s| format!("--{}={}", s.name, s.value)).collect();
        let width = flags.iter().map(String::len).max().unwrap_or(0);
        let mut out = String::new();
        for (setting, flag) in SETTINGS.iter().zip(&flags) {
            out += &format!("  {flag:width$}  {}", setting.help);
            if let Some(value) = (setting.get)(self) {
                out += &format!(" [{value}]");
            }
            out.push('\n');
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

    #[test]
    fn flags_set_settings() {
        let mut c = Config::default();
        c.apply_args(&args(&["--window-width=120", "--window-height=40", "--font-family=SF Mono", "--font-size=13.5"]))
            .unwrap();
        assert_eq!((c.cols, c.rows, c.font.as_str(), c.font_size), (120, 40, "SF Mono", 13.5));
    }

    #[test]
    fn bad_flags_are_rejected() {
        for bad in ["--window-width=0", "--window-width=x", "--font-size=-1", "--nope=1", "--window-width", "stray"] {
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
    fn help_lists_every_setting() {
        let help = Config::default().help();
        for s in SETTINGS {
            assert!(help.contains(&format!("--{}=", s.name)));
        }
        assert!(help.contains("[Menlo]") && !help.contains("--command=LINE  run this shell command line in the first window instead of a login shell ["));
    }
}
