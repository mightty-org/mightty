//! Control API methods. One implementation, reached over the socket
//! (`mightty ctl`, plugins) or in-process (headless MCP server).
//!
//! | method            | params                                                        |
//! |-------------------|---------------------------------------------------------------|
//! | `app.info`        |                                                               |
//! | `pane.list`       |                                                               |
//! | `pane.spawn`      | `command?`, `shell?`, `cwd?`, `env?`, `cols?`, `rows?`, `visible?` (true), `placement?` (`window` / `tab` / `right` / `down`), `beside?` (a pane) |
//! | `pane.focus`      | `pane`                                                        |
//! | `pane.send_text`  | `pane`, `text`, `enter?`                                      |
//! | `pane.send_keys`  | `pane`, `keys` (names like `enter`, `ctrl-c`, `up`)           |
//! | `pane.read`       | `pane`, `source?` (`screen` / `all`), `max_lines?`            |
//! | `pane.commands`   | `pane`, `limit?`, `max_output?`                               |
//! | `pane.wait`       | `pane`, any of `text` / `command_finished` / `exited`, `timeout_ms?` |
//! | `pane.run`        | `pane?`, `command`, `timeout_ms?`, `max_output?`              |
//! | `pane.close`      | `pane`                                                        |

use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mightty_json::Value;
use mightty_term::{CommandBlock, MarkKind, Term};

use crate::{Location, Mux, Pane, PaneId, Placement, SpawnOptions, key_bytes};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    fn invalid(message: impl Into<String>) -> Self {
        Self { code: -32602, message: message.into() }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self { code: -32000, message: message.into() }
    }
}

type Result<T> = std::result::Result<T, RpcError>;

const DEFAULT_WAIT: Duration = Duration::from_secs(10);
const DEFAULT_RUN_WAIT: Duration = Duration::from_secs(60);
const MAX_WAIT: Duration = Duration::from_secs(600);
const DEFAULT_MAX_OUTPUT: usize = 8000;

pub fn handle(mux: &Arc<Mux>, method: &str, params: &Value) -> Result<Value> {
    match method {
        "app.info" => Ok(Value::object([
            ("name", "mightty".into()),
            ("version", env!("CARGO_PKG_VERSION").into()),
            ("pid", process::id().into()),
            ("socket", mux.socket_path().map(|p| p.display().to_string()).into()),
        ])),
        "pane.list" => {
            let locations = mux.host().locations();
            Ok(Value::Array(mux.panes().iter().map(|p| info(p, location(&locations, p.id))).collect()))
        }
        "pane.spawn" => {
            let pane = spawn(mux, params)?;
            Ok(info(&pane, location(&mux.host().locations(), pane.id)))
        }
        "pane.focus" => {
            let pane = pane(mux, params)?;
            if !mux.host().focus_pane(pane.id) {
                return Err(RpcError::failed(format!("pane {} isn't shown in a window", pane.id)));
            }
            Ok(Value::Null)
        }
        "pane.send_text" => {
            let pane = pane(mux, params)?;
            let mut text = params["text"].as_str().ok_or_else(|| RpcError::invalid("`text` is required"))?.to_owned();
            if params["enter"].as_bool() == Some(true) {
                text.push('\r');
            }
            write(&pane, text.as_bytes())?;
            Ok(Value::Null)
        }
        "pane.send_keys" => {
            let pane = pane(mux, params)?;
            let keys = params["keys"].as_array().ok_or_else(|| RpcError::invalid("`keys` must be an array"))?;
            let app_cursor = pane.term.lock().unwrap().modes().app_cursor;
            let mut bytes = Vec::new();
            for key in keys {
                let name = key.as_str().unwrap_or_default();
                bytes.extend(key_bytes(name, app_cursor).ok_or_else(|| RpcError::invalid(format!("unknown key `{name}`")))?);
            }
            write(&pane, &bytes)?;
            Ok(Value::Null)
        }
        "pane.read" => read(&*pane(mux, params)?, params),
        "pane.commands" => {
            let pane = pane(mux, params)?;
            let limit = params["limit"].as_u64().unwrap_or(5) as usize;
            let max_output = max_output(params);
            let term = pane.term.lock().unwrap();
            let blocks = term.command_blocks();
            let skip = blocks.len().saturating_sub(limit);
            Ok(Value::Array(blocks[skip..].iter().map(|b| block_info(&term, b, max_output)).collect()))
        }
        "pane.wait" => wait(&*pane(mux, params)?, params),
        "pane.run" => run(mux, params),
        "pane.close" => {
            let id = pane_id(params)?;
            if !mux.close(id) {
                return Err(RpcError::invalid(format!("no pane {id}")));
            }
            Ok(Value::Null)
        }
        _ => Err(RpcError { code: -32601, message: format!("unknown method `{method}`") }),
    }
}

fn pane_id(params: &Value) -> Result<u64> {
    params["pane"].as_u64().ok_or_else(|| RpcError::invalid("`pane` (a pane id) is required"))
}

fn pane(mux: &Mux, params: &Value) -> Result<Arc<Pane>> {
    let id = pane_id(params)?;
    mux.pane(id).ok_or_else(|| RpcError::invalid(format!("no pane {id}")))
}

fn write(pane: &Pane, bytes: &[u8]) -> Result<()> {
    if !pane.is_alive() {
        return Err(RpcError::failed(format!("pane {} has exited", pane.id)));
    }
    pane.write(bytes).map_err(|e| RpcError::failed(e.to_string()))
}

fn timeout(params: &Value, default: Duration) -> Duration {
    params["timeout_ms"].as_u64().map_or(default, Duration::from_millis).min(MAX_WAIT)
}

fn max_output(params: &Value) -> usize {
    params["max_output"].as_u64().map_or(DEFAULT_MAX_OUTPUT, |n| n as usize)
}

/// Keeps the end of `text`, which is where command output matters most.
fn tail(text: &str, max_chars: usize) -> (String, bool) {
    let count = text.chars().count();
    if count <= max_chars {
        return (text.to_owned(), false);
    }
    (text.chars().skip(count - max_chars).collect(), true)
}

fn location(locations: &[(PaneId, Location)], id: PaneId) -> Option<Location> {
    locations.iter().find(|l| l.0 == id).map(|l| l.1)
}

fn info(pane: &Pane, location: Option<Location>) -> Value {
    let term = pane.term.lock().unwrap();
    let exit = pane.exit_status();
    let mut info = Value::object([
        ("id", pane.id.into()),
        ("command", pane.command.as_str().into()),
        ("title", term.title().into()),
        ("cwd", term.cwd().into()),
        ("cols", term.cols().into()),
        ("rows", term.rows().into()),
        ("alive", exit.is_none().into()),
        ("exit_code", exit.flatten().into()),
        ("visible", pane.is_visible().into()),
        ("shell_integration", (!term.marks().is_empty()).into()),
    ]);
    if let Some(at) = location {
        info.insert("window", at.window);
        info.insert("tab", at.tab);
        info.insert("focused", at.focused);
    }
    info
}

fn block_info(term: &Term, block: &CommandBlock, max_output: usize) -> Value {
    let (output, truncated) = tail(&term.block_output(block), max_output);
    Value::object([
        ("command", block.command.clone().into()),
        ("running", block.end_line.is_none().into()),
        ("exit_code", block.exit_code.into()),
        ("output", output.into()),
        ("truncated", truncated.into()),
    ])
}

fn spawn(mux: &Mux, params: &Value) -> Result<Arc<Pane>> {
    let opts = SpawnOptions {
        command: params["command"].as_str().map(str::to_owned),
        shell: params["shell"].as_str().map(str::to_owned),
        cwd: params["cwd"].as_str().map(Into::into),
        env: match &params["env"] {
            Value::Object(vars) => vars.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect(),
            _ => Vec::new(),
        },
        cols: params["cols"].as_u64().map_or(120, |n| n.clamp(10, 1000) as u16),
        rows: params["rows"].as_u64().map_or(40, |n| n.clamp(4, 1000) as u16),
        visible: params["visible"].as_bool().unwrap_or(true),
        placement: {
            let beside = params["beside"].as_u64();
            let name = params["placement"].as_str().unwrap_or("window");
            Placement::parse(name, beside)
                .ok_or_else(|| RpcError::invalid(format!("unknown placement `{name}` (window, tab, right, down)")))?
        },
    };
    mux.spawn(opts).map_err(|e| RpcError::failed(format!("spawn failed: {e}")))
}

fn read(pane: &Pane, params: &Value) -> Result<Value> {
    let term = pane.term.lock().unwrap();
    let text = match params["source"].as_str().unwrap_or("screen") {
        "screen" => term.screen_text(),
        "all" | "scrollback" => {
            let last = term.lines_scrolled() + term.rows() as u64;
            term.text_range((term.first_line(), 0), (last, usize::MAX))
        }
        other => return Err(RpcError::invalid(format!("unknown source `{other}` (screen, all)"))),
    };
    let text = text.trim_end();
    let max_lines = params["max_lines"].as_u64().map_or(500, |n| n as usize);
    let lines: Vec<&str> = text.lines().collect();
    let skip = lines.len().saturating_sub(max_lines);
    Ok(Value::object([
        ("text", lines[skip..].join("\n").into()),
        ("truncated", (skip > 0).into()),
        ("cursor_row", term.cursor().0.into()),
        ("cursor_col", term.cursor().1.into()),
        ("alt_screen", term.is_alt_screen().into()),
    ]))
}

fn finished_commands(term: &Term) -> usize {
    term.marks().iter().filter(|m| matches!(m.kind, MarkKind::CommandEnd { .. })).count()
}

/// Re-checks `done` whenever the pane produces output, until it returns a
/// value or `timeout` passes. Checks are throttled so heavy output can't
/// turn waiting into a busy loop.
fn wait_until(pane: &Pane, timeout: Duration, mut done: impl FnMut(&Pane) -> Option<Value>) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    let mut seen = pane.generation();
    loop {
        if let Some(result) = done(pane) {
            return Some(result);
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        seen = pane.wait_output(seen, deadline - now);
        std::thread::sleep(Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn wait(pane: &Pane, params: &Value) -> Result<Value> {
    let text = params["text"].as_str().filter(|t| !t.is_empty());
    let command_finished = params["command_finished"].as_bool() == Some(true);
    let exited = params["exited"].as_bool() == Some(true);
    if text.is_none() && !command_finished && !exited {
        return Err(RpcError::invalid("give at least one of `text`, `command_finished`, `exited`"));
    }
    let baseline = finished_commands(&pane.term.lock().unwrap());
    let result = wait_until(pane, timeout(params, DEFAULT_WAIT), |pane| {
        if exited && let Some(code) = pane.exit_status() {
            return Some(Value::object([("matched", "exited".into()), ("exit_code", code.into())]));
        }
        let term = pane.term.lock().unwrap();
        if command_finished && finished_commands(&term) > baseline {
            let exit_code = term.command_blocks().last().and_then(|b| b.exit_code);
            return Some(Value::object([("matched", "command_finished".into()), ("exit_code", exit_code.into())]));
        }
        if let Some(text) = text {
            let last = term.lines_scrolled() + term.rows() as u64;
            if term.text_range((term.first_line(), 0), (last, usize::MAX)).contains(text) {
                return Some(Value::object([("matched", "text".into())]));
            }
        }
        if !pane.is_alive() {
            // Nothing more will happen.
            return Some(Value::object([("matched", Value::Null), ("exited", true.into())]));
        }
        None
    });
    Ok(result.unwrap_or_else(|| Value::object([("matched", Value::Null), ("timed_out", true.into())])))
}

/// Types a command into a shell pane and waits for it to finish, using shell
/// integration to find its output and exit code. Without `pane`, starts a
/// hidden shell first.
fn run(mux: &Arc<Mux>, params: &Value) -> Result<Value> {
    let command = params["command"].as_str().filter(|c| !c.trim().is_empty());
    let command = command.ok_or_else(|| RpcError::invalid("`command` is required"))?;
    let pane = match params.get("pane").filter(|v| !v.is_null()) {
        Some(_) => pane(mux, params)?,
        None => {
            let pane = mux
                .spawn(SpawnOptions { cols: 120, rows: 40, ..Default::default() })
                .map_err(|e| RpcError::failed(format!("spawn failed: {e}")))?;
            // Wait for the first prompt so the command isn't typed into shell startup.
            let ready = wait_until(&pane, Duration::from_secs(10), |p| {
                (!p.term.lock().unwrap().marks().is_empty()).then_some(Value::Null)
            });
            if ready.is_none() {
                mux.close(pane.id);
                return Err(RpcError::failed("shell integration did not start (only zsh is supported so far)"));
            }
            pane
        }
    };
    if pane.term.lock().unwrap().marks().is_empty() {
        return Err(RpcError::failed(format!(
            "pane {} has no shell integration; use pane.send_text and pane.wait instead",
            pane.id
        )));
    }
    let baseline = {
        let term = pane.term.lock().unwrap();
        // Typing into a running program would hand it the command instead.
        if term.marks().last().is_some_and(|m| m.kind == MarkKind::OutputStart) {
            return Err(RpcError::failed(format!("pane {} is still running a command", pane.id)));
        }
        finished_commands(&term)
    };
    write(&pane, format!("{command}\r").as_bytes())?;
    let max_output = max_output(params);
    let finished = wait_until(&pane, timeout(params, DEFAULT_RUN_WAIT), |pane| {
        let term = pane.term.lock().unwrap();
        (finished_commands(&term) > baseline || !pane.is_alive()).then_some(Value::Null)
    });
    let term = pane.term.lock().unwrap();
    let mut result = match term.command_blocks().last() {
        Some(block) => block_info(&term, block, max_output),
        None => Value::object([("output", term.screen_text().into())]),
    };
    result.insert("pane", pane.id);
    result.insert("timed_out", finished.is_none());
    Ok(result)
}
