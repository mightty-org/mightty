//! Windows, tabs and splits saved across launches (`window-save-state`):
//! where each window was, its tabs, how its panes were split and where each
//! pane's shell was. Programs and scrollback aren't kept; panes start fresh
//! shells in their old directories.

use std::fs;
use std::io;
use std::path::PathBuf;

use mightty_json::{self as json, Value};

use crate::split::Axis;

/// Bumped when the format changes; other versions are ignored.
const VERSION: u64 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// Windows with their tabs; the last one was in front.
    pub groups: Vec<Group>,
}

/// A window, or several as tabs of one.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// x, y, width, height on screen, in points.
    pub frame: [f64; 4],
    pub tabs: Vec<Tab>,
    /// The tab in front.
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub layout: Layout,
    /// The pane keys went to, in reading order.
    pub focused: usize,
    pub zoomed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    /// `title`: one the user gave it (Change Title…), if any.
    Pane {
        cwd: Option<String>,
        title: Option<String>,
    },
    Split {
        axis: Axis,
        ratio: f64,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    /// The first pane's directory: where the space's first shell starts.
    pub fn first_cwd(&self) -> Option<&str> {
        match self {
            Layout::Pane { cwd, .. } => cwd.as_deref(),
            Layout::Split { first, .. } => first.first_cwd(),
        }
    }

    /// The panes' titles, first to last as `Node::leaves` lists them.
    pub fn titles(&self) -> Vec<Option<&str>> {
        match self {
            Layout::Pane { title, .. } => vec![title.as_deref()],
            Layout::Split { first, second, .. } => [first.titles(), second.titles()].concat(),
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Layout::Pane { cwd, title } => {
                Value::object([("cwd", cwd.clone().into()), ("title", title.clone().into())])
            }
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => Value::object([
                (
                    "axis",
                    if *axis == Axis::Row { "row" } else { "column" }.into(),
                ),
                ("ratio", (*ratio).into()),
                ("first", first.to_json()),
                ("second", second.to_json()),
            ]),
        }
    }

    fn from_json(v: &Value, depth: usize) -> Option<Self> {
        // A file that nests deeper than any real layout is damaged.
        if depth > 64 {
            return None;
        }
        if v.get("axis").is_none() {
            let text = |key: &str| v[key].as_str().map(str::to_owned);
            return Some(Layout::Pane {
                cwd: text("cwd"),
                title: text("title"),
            });
        }
        let axis = match v["axis"].as_str()? {
            "row" => Axis::Row,
            "column" => Axis::Column,
            _ => return None,
        };
        Some(Layout::Split {
            axis,
            ratio: v["ratio"].as_f64()?.clamp(0.05, 0.95),
            first: Box::new(Self::from_json(&v["first"], depth + 1)?),
            second: Box::new(Self::from_json(&v["second"], depth + 1)?),
        })
    }
}

impl Session {
    pub fn to_json(&self) -> String {
        let groups = self.groups.iter().map(|g| {
            let tabs = g.tabs.iter().map(|t| {
                Value::object([
                    ("layout", t.layout.to_json()),
                    ("focused", t.focused.into()),
                    ("zoomed", t.zoomed.into()),
                ])
            });
            Value::object([
                (
                    "frame",
                    g.frame
                        .iter()
                        .map(|&n| n.into())
                        .collect::<Vec<Value>>()
                        .into(),
                ),
                ("tabs", tabs.collect::<Vec<_>>().into()),
                ("selected", g.selected.into()),
            ])
        });
        Value::object([
            ("version", VERSION.into()),
            ("groups", groups.collect::<Vec<_>>().into()),
        ])
        .to_string()
    }

    /// `None` for a file that isn't a session this version reads.
    pub fn from_json(text: &str) -> Option<Self> {
        let v = json::parse(text).ok()?;
        if v["version"].as_u64() != Some(VERSION) {
            return None;
        }
        let mut groups = Vec::new();
        for g in v["groups"].as_array()? {
            let frame: Vec<f64> = g["frame"]
                .as_array()
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_f64)
                .collect();
            let tabs: Vec<Tab> = g["tabs"]
                .as_array()
                .unwrap_or_default()
                .iter()
                .filter_map(|t| {
                    let layout = Layout::from_json(&t["layout"], 0)?;
                    Some(Tab {
                        layout,
                        focused: t["focused"].as_u64().unwrap_or(0) as usize,
                        zoomed: t["zoomed"].as_bool() == Some(true),
                    })
                })
                .collect();
            if frame.len() != 4 || tabs.is_empty() {
                continue;
            }
            let selected = (g["selected"].as_u64().unwrap_or(0) as usize).min(tabs.len() - 1);
            groups.push(Group {
                frame: [frame[0], frame[1], frame[2], frame[3]],
                tabs,
                selected,
            });
        }
        Some(Session { groups })
    }
}

/// `~/Library/Application Support/mightty/session.json`.
pub fn path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/mightty/session.json"))
}

pub fn load() -> Option<Session> {
    Session::from_json(&fs::read_to_string(path()?).ok()?)
}

/// Writes `text` whole: to a temporary file, then renamed over the old one.
pub fn save(text: &str) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::other("no home directory"))?;
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no directory"))?;
    fs::create_dir_all(dir)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, text)?;
    fs::rename(temporary, path)
}

/// Forgets the saved session (with `window-save-state = never`).
pub fn remove() {
    if let Some(path) = path() {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(cwd: &str) -> Box<Layout> {
        Box::new(Layout::Pane {
            cwd: Some(cwd.into()),
            title: None,
        })
    }

    #[test]
    fn round_trips() {
        let layout = Layout::Split {
            axis: Axis::Row,
            ratio: 0.25,
            first: pane("/a"),
            second: Box::new(Layout::Split {
                axis: Axis::Column,
                ratio: 0.5,
                first: pane("/b c"),
                second: Box::new(Layout::Pane {
                    cwd: None,
                    title: Some("logs".into()),
                }),
            }),
        };
        let session = Session {
            groups: vec![
                Group {
                    frame: [10.0, 20.0, 800.0, 600.0],
                    tabs: vec![
                        Tab {
                            layout,
                            focused: 2,
                            zoomed: false,
                        },
                        Tab {
                            layout: *pane("/d"),
                            focused: 0,
                            zoomed: true,
                        },
                    ],
                    selected: 1,
                },
                Group {
                    frame: [0.0, 0.0, 500.0, 400.0],
                    tabs: vec![Tab {
                        layout: *pane("/e"),
                        focused: 0,
                        zoomed: false,
                    }],
                    selected: 0,
                },
            ],
        };
        assert_eq!(
            Session::from_json(&session.to_json()),
            Some(session.clone())
        );
        assert_eq!(session.groups[0].tabs[0].layout.first_cwd(), Some("/a"));
        assert_eq!(
            session.groups[0].tabs[0].layout.titles(),
            [None, None, Some("logs")]
        );
    }

    #[test]
    fn damaged_files_are_ignored() {
        assert_eq!(Session::from_json("not json"), None);
        assert_eq!(Session::from_json(r#"{"version": 99, "groups": []}"#), None);
        // A group missing its frame is skipped; the rest load.
        let text = r#"{"version": 1, "groups": [
            {"tabs": [{"layout": {"cwd": "/x"}}]},
            {"frame": [0, 0, 100, 100], "tabs": [{"layout": {"cwd": "/y"}}], "selected": 7}
        ]}"#;
        let session = Session::from_json(text).unwrap();
        assert_eq!(session.groups.len(), 1);
        assert_eq!(
            (
                session.groups[0].selected,
                session.groups[0].tabs[0].layout.first_cwd()
            ),
            (0, Some("/y"))
        );
        // Absurd nesting.
        let deep = format!(
            r#"{{"version": 1, "groups": [{{"frame": [0,0,1,1], "tabs": [{{"layout": {}}}]}}]}}"#,
            "{\"axis\": \"row\", \"ratio\": 0.5, \"second\": {}, \"first\": ".repeat(100)
                + "{}"
                + &"}".repeat(100)
        );
        assert_eq!(Session::from_json(&deep).map(|s| s.groups.len()), Some(0));
    }
}
