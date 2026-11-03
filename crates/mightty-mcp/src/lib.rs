//! Model Context Protocol server (stdio transport) exposing mightty panes to
//! AI agents as tools. It talks to the running mightty through its control
//! socket, so panes it opens appear as windows you can watch; with no
//! mightty running it hosts a headless mux in-process instead.
//!
//! Register with an MCP client, e.g. `claude mcp add mightty -- mightty mcp`.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use mightty_json::{self as json, Value};
use mightty_mux::{CallError, Client, Headless, Mux, handle};

/// Protocol revisions we can speak, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "\
Terminal panes in mightty. Use run_command for one-off shell commands: it returns output and exit \
code. For long-running processes (dev servers, watchers, REPLs) use open_pane, then wait_for and \
read_pane to follow them, send_text/send_keys to interact, and close_pane when done. Panes opened \
here are visible to the user: by default in a split beside your own pane, stacking further ones \
below the last; open_pane's placement can ask for a tab or window instead. focus_pane brings a \
pane to the front when you want the user to look at it.";

pub enum Backend {
    /// A running mightty, over its control socket.
    Remote(Client),
    /// No mightty running: panes live in this process, without windows.
    Local(Arc<Mux>),
}

impl Backend {
    pub fn connect() -> Self {
        match Client::discover() {
            Ok(client) => Backend::Remote(client),
            Err(_) => Backend::Local(Mux::new(Arc::new(Headless))),
        }
    }

}

struct Server {
    backend: Backend,
    /// Hidden shell reused by `run_command` calls that don't name a pane,
    /// so `cd` and environment changes carry over between calls.
    default_pane: Option<u64>,
    /// The pane the agent runs in, when that's in this mightty
    /// (`MIGHTTY_PANE_ID`): panes it opens go beside it.
    own_pane: Option<u64>,
    /// The last pane opened in a split by default: the next one stacks below it.
    last_split: Option<u64>,
}

/// Serves MCP over newline-delimited JSON-RPC until `input` closes.
pub fn serve(backend: Backend, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let own_pane = match &backend {
        Backend::Remote(_) => std::env::var("MIGHTTY_PANE_ID").ok().and_then(|id| id.parse().ok()),
        Backend::Local(_) => None,
    };
    let mut server = Server { backend, default_pane: None, own_pane, last_split: None };
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.message(&line) {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}

impl Server {
    /// Calls the backend. If the mightty we were connected to has quit, finds
    /// the one running now (it may have been reopened) and retries, unless
    /// the request names a pane: pane ids don't carry over between instances.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let client = match &mut self.backend {
            Backend::Local(mux) => return handle(mux, method, &params).map_err(|e| e.message),
            Backend::Remote(client) => client,
        };
        match client.call(method, params.clone()) {
            Err(CallError::Disconnected(_)) => {
                let fresh = Client::discover().map_err(|_| "mightty was closed; reopen it to use its panes".to_owned())?;
                *client = fresh;
                // Pane ids were for the old instance.
                (self.default_pane, self.own_pane, self.last_split) = (None, None, None);
                if !params["pane"].is_null() {
                    return Err("mightty was restarted and pane ids changed; call list_panes".into());
                }
                client.call(method, params).map_err(String::from)
            }
            result => result.map_err(String::from),
        }
    }

    fn message(&mut self, line: &str) -> Option<Value> {
        let message = match json::parse(line) {
            Ok(m) => m,
            Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        // Responses to our (nonexistent) requests and notifications need no reply.
        let id = message.get("id").cloned()?;
        let method = message["method"].as_str()?;
        let params = &message["params"];
        let result = match method {
            "initialize" => {
                let requested = params["protocolVersion"].as_str().unwrap_or_default();
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == requested).unwrap_or(&PROTOCOL_VERSIONS[0]);
                Value::object([
                    ("protocolVersion", (*version).into()),
                    ("capabilities", Value::object([("tools", Value::object([("listChanged", false.into())]))])),
                    (
                        "serverInfo",
                        Value::object([("name", "mightty".into()), ("version", env!("CARGO_PKG_VERSION").into())]),
                    ),
                    ("instructions", INSTRUCTIONS.into()),
                ])
            }
            "ping" => Value::object::<&str>([]),
            "tools/list" => Value::object([("tools", tools())]),
            "tools/call" => self.call_tool(params["name"].as_str().unwrap_or_default(), &params["arguments"]),
            _ => return Some(error(id, -32601, &format!("unknown method `{method}`"))),
        };
        Some(Value::object([("jsonrpc", "2.0".into()), ("id", id), ("result", result)]))
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Value {
        let args = if args.is_null() { Value::object::<&str>([]) } else { args.clone() };
        let outcome = match name {
            "list_panes" => self.call("pane.list", args).map(|v| v.to_string()),
            "open_pane" => self.open_pane(args),
            "focus_pane" => self.call("pane.focus", args).map(|_| "focused".into()),
            "run_command" => self.run_command(args),
            "send_text" => self.call("pane.send_text", args).map(|_| "sent".into()),
            "send_keys" => self.call("pane.send_keys", args).map(|_| "sent".into()),
            "read_pane" => self.call("pane.read", args).map(|v| {
                let text = v["text"].as_str().unwrap_or_default();
                if v["truncated"].as_bool() == Some(true) { format!("(earlier lines omitted)\n{text}") } else { text.into() }
            }),
            "list_commands" => self.call("pane.commands", args).map(|v| {
                v.as_array().unwrap_or_default().iter().map(format_block).collect::<Vec<_>>().join("\n\n")
            }),
            "wait_for" => self.call("pane.wait", args).map(|v| v.to_string()),
            "close_pane" => self.call("pane.close", args).map(|_| "closed".into()),
            _ => Err(format!("unknown tool `{name}`")),
        };
        let (text, is_error) = match outcome {
            Ok(text) => (text, false),
            Err(message) => (message, true),
        };
        let content = Value::object([("type", "text".into()), ("text", text.into())]);
        Value::object([("content", vec![content].into()), ("isError", is_error.into())])
    }

    /// Without a placement, a visible pane goes in a split: below the last
    /// one opened that way while it's shown, else right of the agent's pane.
    /// Not knowing either, in a window.
    fn open_pane(&mut self, mut args: Value) -> Result<String, String> {
        let default = args["placement"].is_null() && args["visible"].as_bool() != Some(false);
        if default {
            let shown: Vec<u64> = self.call("pane.list", Value::Null)?.as_array().unwrap_or_default().iter()
                .filter(|p| p["window"].as_u64().is_some())
                .filter_map(|p| p["id"].as_u64())
                .collect();
            let stack = self.last_split.filter(|id| shown.contains(id));
            match (stack, self.own_pane.filter(|id| shown.contains(id))) {
                (Some(last), _) => {
                    args.insert("placement", "down");
                    args.insert("beside", last);
                }
                (None, Some(own)) => {
                    args.insert("placement", "right");
                    args.insert("beside", own);
                }
                (None, None) => {}
            }
        }
        let pane = self.call("pane.spawn", args.clone())?;
        if default && !args["placement"].is_null() {
            self.last_split = pane["id"].as_u64();
        }
        Ok(pane.to_string())
    }

    fn run_command(&mut self, mut args: Value) -> Result<String, String> {
        if args["pane"].is_null()
            && let Some(id) = self.default_pane
        {
            let alive = self.call("pane.list", Value::Null)?.as_array().unwrap_or_default().iter().any(|p| {
                p["id"].as_u64() == Some(id) && p["alive"].as_bool() == Some(true)
            });
            if alive {
                args.insert("pane", id);
            }
        }
        let result = self.call("pane.run", args.clone())?;
        if args["pane"].is_null() {
            self.default_pane = result["pane"].as_u64();
        }
        let mut text = format_block(&result);
        let pane = result["pane"].as_u64().unwrap_or_default();
        if result["timed_out"].as_bool() == Some(true) {
            text.push_str(&format!(
                "\n[still running in pane {pane}; use wait_for or read_pane to follow it, send_keys [\"ctrl-c\"] to stop it]"
            ));
        } else {
            text.push_str(&format!("\n[pane {pane}]"));
        }
        Ok(text)
    }
}

fn format_block(block: &Value) -> String {
    let mut text = format!("$ {}\n", block["command"].as_str().unwrap_or("?"));
    if block["truncated"].as_bool() == Some(true) {
        text.push_str("(earlier output omitted)\n");
    }
    let output = block["output"].as_str().unwrap_or_default();
    if !output.is_empty() {
        text.push_str(output);
        text.push('\n');
    }
    match (block["running"].as_bool(), block["exit_code"].as_i64()) {
        (Some(true), _) => text.push_str("[running]"),
        (_, Some(code)) => text.push_str(&format!("[exit code {code}]")),
        _ => text.push_str("[finished]"),
    }
    text
}

fn error(id: Value, code: i64, message: &str) -> Value {
    let error = Value::object([("code", code.into()), ("message", message.into())]);
    Value::object([("jsonrpc", "2.0".into()), ("id", id), ("error", error)])
}

/// Tool definitions: name, description, read-only, JSON schema.
fn tools() -> Value {
    let pane = || ("pane", prop("integer", "Pane id, from list_panes or open_pane."));
    let defs: Vec<(&str, &str, bool, Value)> = vec![
        ("list_panes", "List terminal panes with their id, command, title, working directory, whether they are still running, and where they are shown: window, tab and whether the user is focused on it.", true, schema(vec![], &[])),
        (
            "run_command",
            "Run a shell command and wait for it to finish; returns its output and exit code. Without `pane`, runs in a hidden shell kept for this session, so `cd` and exports persist between calls. Requires zsh.",
            false,
            schema(vec![
                ("command", prop("string", "Shell command line.")),
                ("pane", prop("integer", "Shell pane to run in. Optional.")),
                ("timeout_ms", prop("integer", "How long to wait before returning while it keeps running (default 60000).")),
            ], &["command"]),
        ),
        (
            "open_pane",
            "Open a new terminal pane, visible to the user in mightty. Use for long-running processes such as dev servers, watchers or REPLs. By default it goes in a split beside your own pane, further ones stacking below it; it doesn't take the keyboard from the user.",
            false,
            schema(vec![
                ("command", prop("string", "Shell command to run; the pane ends when it exits. Omit for an interactive shell.")),
                ("cwd", prop("string", "Working directory (default: home).")),
                ("visible", prop("boolean", "Show it to the user (default true).")),
                ("placement", Value::object([
                    ("type", "string".into()),
                    ("enum", vec!["right".into(), "down".into(), "tab".into(), "window".into()].into()),
                    ("description", "Where to show it: a split right of or below `beside`, a new tab beside it, or its own window.".into()),
                ])),
                ("beside", prop("integer", "Pane to open it next to (default: the pane the user is in).")),
            ], &[]),
        ),
        (
            "read_pane",
            "Read the text in a pane: the visible screen, or everything including scrollback.",
            true,
            schema(vec![
                pane(),
                ("source", Value::object([("type", "string".into()), ("enum", vec!["screen".into(), "all".into()].into()), ("description", "screen (default) or all.".into())])),
                ("max_lines", prop("integer", "Return at most this many lines, from the end (default 500).")),
            ], &["pane"]),
        ),
        (
            "list_commands",
            "Recent commands run in a shell pane, each with its output and exit code.",
            true,
            schema(vec![pane(), ("limit", prop("integer", "How many, most recent last (default 5)."))], &["pane"]),
        ),
        (
            "wait_for",
            "Wait until text appears in a pane, its current command finishes, or it exits.",
            true,
            schema(vec![
                pane(),
                ("text", prop("string", "Text to wait for, searched in screen and scrollback.")),
                ("command_finished", prop("boolean", "Wait for the running command to finish.")),
                ("exited", prop("boolean", "Wait for the pane's process to exit.")),
                ("timeout_ms", prop("integer", "Give up after this long (default 10000).")),
            ], &["pane"]),
        ),
        (
            "send_text",
            "Type text into a pane, optionally followed by Enter.",
            false,
            schema(vec![pane(), ("text", prop("string", "Text to type.")), ("enter", prop("boolean", "Press Enter afterwards."))], &["pane", "text"]),
        ),
        (
            "send_keys",
            "Press keys in a pane, e.g. [\"ctrl-c\"], [\"up\", \"enter\"], [\"escape\", \":\", \"q\"]. Names: enter, tab, escape, backspace, space, up, down, left, right, home, end, pageup, pagedown, delete, f1-f12, ctrl-<key>, alt-<key>.",
            false,
            schema(vec![pane(), ("keys", Value::object([("type", "array".into()), ("items", Value::object([("type", "string".into())]))]))], &["pane", "keys"]),
        ),
        (
            "focus_pane",
            "Bring a pane to the front and give it the keyboard, to show the user something in it.",
            false,
            schema(vec![pane()], &["pane"]),
        ),
        ("close_pane", "Close a pane, ending its process.", false, schema(vec![pane()], &["pane"])),
    ];
    Value::Array(
        defs.into_iter()
            .map(|(name, description, read_only, input_schema)| {
                Value::object([
                    ("name", name.into()),
                    ("description", description.into()),
                    ("inputSchema", input_schema),
                    ("annotations", Value::object([("readOnlyHint", read_only.into())])),
                ])
            })
            .collect(),
    )
}

fn prop(kind: &str, description: &str) -> Value {
    Value::object([("type", kind.into()), ("description", description.into())])
}

fn schema(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    Value::object([
        ("type", "object".into()),
        ("properties", Value::object(properties)),
        ("required", Value::Array(required.iter().map(|r| (*r).into()).collect())),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(lines: &[&str]) -> Vec<Value> {
        let input = lines.join("\n");
        let mut output = Vec::new();
        serve(Backend::Local(Mux::new(Arc::new(Headless))), input.as_bytes(), &mut output).unwrap();
        String::from_utf8(output).unwrap().lines().map(|l| json::parse(l).unwrap()).collect()
    }

    #[test]
    fn handshake_and_tools() {
        let replies = session(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"bogus"}"#,
        ]);
        assert_eq!(replies.len(), 3, "notifications get no reply");
        assert_eq!(replies[0]["result"]["protocolVersion"].as_str(), Some("2025-06-18"));
        let tools = replies[1]["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"].as_str() == Some("run_command")));
        assert!(tools.iter().all(|t| t["inputSchema"]["type"].as_str() == Some("object")));
        assert_eq!(replies[2]["error"]["code"].as_i64(), Some(-32601));
    }

    #[test]
    fn tools_drive_a_pane() {
        let replies = session(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"open_pane","arguments":{"command":"echo from-mcp; sleep 5","visible":false}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"wait_for","arguments":{"pane":1,"text":"from-mcp"}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_pane","arguments":{"pane":1}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"close_pane","arguments":{"pane":1}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"read_pane","arguments":{"pane":1}}}"#,
        ]);
        let text = |i: usize| replies[i]["result"]["content"].as_array().unwrap()[0]["text"].as_str().unwrap().to_owned();
        assert!(text(1).contains(r#""matched":"text""#));
        assert_eq!(text(2), "from-mcp");
        assert_eq!(replies[3]["result"]["isError"].as_bool(), Some(false));
        assert_eq!(replies[4]["result"]["isError"].as_bool(), Some(true));
        assert!(text(4).contains("no pane 1"));
    }
}
