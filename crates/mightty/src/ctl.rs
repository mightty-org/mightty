//! `mightty ctl`: command-line client for the control socket.

use std::process::ExitCode;

use mightty_json::{self as json, Value};
use mightty_mux::Client;

pub const USAGE: &str = "\
usage: mightty ctl <command> [args]

  info                                  version, pid and socket of the running mightty
  list                                  list panes
  spawn [--hidden] [--cwd DIR] [--tab | --right | --down] [--beside N] [-- CMD]
                                        open a pane (an interactive shell without CMD), in a
                                        window, a tab or a split beside pane N (default: this one)
  run [--pane N] [--timeout MS] -- CMD  run CMD in a shell pane; prints output, exits with its code
  send N TEXT [--enter]                 type TEXT into pane N
  keys N KEY...                         press keys: enter, ctrl-c, up, f5, alt-b, ...
  read N [--all] [--lines N]            print pane N's screen (or all scrollback)
  commands N [--limit N]                recent commands in pane N with output and exit codes
  wait N [--text T] [--command] [--exit] [--timeout MS]
  focus N                               bring pane N to the front
  notify [N] [TEXT]                     mark pane N (default: this one) as needing you,
                                        e.g. from an agent's notification hook
  attention                             panes needing you, the longest waiting first
  close N                               close pane N
  call METHOD [JSON]                    raw JSON-RPC call

Connects to $MIGHTTY_SOCKET, or the most recent running mightty.";

pub fn main(args: &[String]) -> Result<ExitCode, String> {
    let Some((command, rest)) = args.split_first() else {
        println!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let mut client = Client::discover().map_err(|e| format!("{e} (is mightty running?)"))?;
    let mut opts = Opts::new(rest);

    match command.as_str() {
        "info" => print_json(&client.call("app.info", Value::Null)?),
        "list" => {
            let panes = client.call("pane.list", Value::Null)?;
            println!("{:<4} {:<8} {:<9} {:<30} CWD", "ID", "STATE", "WINDOW", "COMMAND");
            for p in panes.as_array().unwrap_or_default() {
                let state = match (p["alive"].as_bool(), p["exit_code"].as_i64()) {
                    (Some(true), _) => p["status"].as_str().unwrap_or("running").to_owned(),
                    (_, Some(code)) => format!("exit {code}"),
                    _ => "exited".to_owned(),
                };
                // ! when it needs you.
                let state = if p["attention"].is_null() { state } else { format!("{state}!") };
                // Window number, tab, and * where keys go.
                let window = match p["window"].as_u64() {
                    Some(w) => format!("{w}/{}{}", p["tab"], if p["focused"].as_bool() == Some(true) { "*" } else { "" }),
                    None if p["visible"].as_bool() == Some(true) => "yes".to_owned(),
                    None => "-".to_owned(),
                };
                let title = p["title"].as_str().filter(|t| !t.is_empty()).or(p["command"].as_str()).unwrap_or("");
                let cwd = p["cwd"].as_str().unwrap_or("");
                println!("{:<4} {:<8} {:<9} {:<30} {cwd}", p["id"].to_string(), state, window, truncate(title, 30));
            }
        }
        "spawn" => {
            let mut params = Value::object([("visible", (!opts.flag("--hidden")).into())]);
            if let Some(cwd) = opts.value("--cwd") {
                params.insert("cwd", cwd);
            }
            if let Some(placement) = ["tab", "right", "down"].into_iter().find(|p| opts.flag(&format!("--{p}"))) {
                params.insert("placement", placement);
                // Beside the pane this runs in, unless told otherwise.
                let beside = opts.value("--beside").or_else(|| std::env::var("MIGHTTY_PANE_ID").ok());
                if let Some(beside) = beside {
                    params.insert("beside", number(&beside)?);
                }
            }
            if let Some(cmd) = opts.command() {
                params.insert("command", cmd);
            }
            opts.finish()?;
            println!("{}", client.call("pane.spawn", params)?["id"]);
        }
        "run" => {
            let mut params = Value::object::<&str>([]);
            if let Some(pane) = opts.value("--pane") {
                params.insert("pane", number(&pane)?);
            }
            if let Some(timeout) = opts.value("--timeout") {
                params.insert("timeout_ms", number(&timeout)?);
            }
            let cmd = opts.command().ok_or("run needs a command after --")?;
            params.insert("command", cmd);
            opts.finish()?;
            let result = client.call("pane.run", params)?;
            println!("{}", result["output"].as_str().unwrap_or_default());
            if result["timed_out"].as_bool() == Some(true) {
                eprintln!("mightty: still running in pane {}", result["pane"]);
                return Ok(ExitCode::from(124));
            }
            return Ok(ExitCode::from(result["exit_code"].as_i64().unwrap_or(0).clamp(0, 255) as u8));
        }
        "send" => {
            let pane = opts.pane()?;
            let enter = opts.flag("--enter");
            let text = opts.positional().ok_or("send needs TEXT")?;
            opts.finish()?;
            client.call("pane.send_text", obj(pane, [("text", text.into()), ("enter", enter.into())]))?;
        }
        "keys" => {
            let pane = opts.pane()?;
            let keys: Vec<Value> = std::iter::from_fn(|| opts.positional()).map(Into::into).collect();
            if keys.is_empty() {
                return Err("keys needs at least one key".into());
            }
            client.call("pane.send_keys", obj(pane, [("keys", keys.into())]))?;
        }
        "read" => {
            let pane = opts.pane()?;
            let source = if opts.flag("--all") { "all" } else { "screen" };
            let mut params = obj(pane, [("source", source.into())]);
            if let Some(lines) = opts.value("--lines") {
                params.insert("max_lines", number(&lines)?);
            }
            opts.finish()?;
            println!("{}", client.call("pane.read", params)?["text"].as_str().unwrap_or_default());
        }
        "commands" => {
            let pane = opts.pane()?;
            let mut params = obj(pane, []);
            if let Some(limit) = opts.value("--limit") {
                params.insert("limit", number(&limit)?);
            }
            opts.finish()?;
            for block in client.call("pane.commands", params)?.as_array().unwrap_or_default() {
                let status = match (block["running"].as_bool(), block["exit_code"].as_i64()) {
                    (Some(true), _) => "running".to_owned(),
                    (_, Some(code)) => format!("exit {code}"),
                    _ => "done".to_owned(),
                };
                println!("$ {}  [{status}]", block["command"].as_str().unwrap_or("?"));
                let output = block["output"].as_str().unwrap_or_default();
                if !output.is_empty() {
                    println!("{output}");
                }
            }
        }
        "wait" => {
            let pane = opts.pane()?;
            let mut params = obj(pane, [("command_finished", opts.flag("--command").into()), ("exited", opts.flag("--exit").into())]);
            if let Some(text) = opts.value("--text") {
                params.insert("text", text);
            }
            if let Some(timeout) = opts.value("--timeout") {
                params.insert("timeout_ms", number(&timeout)?);
            }
            opts.finish()?;
            let result = client.call("pane.wait", params)?;
            print_json(&result);
            if result["matched"].is_null() {
                return Ok(ExitCode::from(124));
            }
        }
        "notify" => {
            // A leading number is the pane; otherwise the one this runs in.
            let pane = match opts.args.first().filter(|a| a.parse::<u64>().is_ok()) {
                Some(_) => opts.pane()?,
                None => number(&std::env::var("MIGHTTY_PANE_ID").map_err(|_| "notify needs a pane id outside mightty")?)?,
            };
            let mut params = obj(pane, []);
            if let Some(text) = opts.positional() {
                params.insert("text", text);
            }
            opts.finish()?;
            client.call("pane.notify", params)?;
        }
        "attention" => {
            opts.finish()?;
            for p in client.call("attention.list", Value::Null)?.as_array().unwrap_or_default() {
                let seconds = p["attention"]["waiting_ms"].as_u64().unwrap_or(0) / 1000;
                println!("{:<4} {:>5}s  {}", p["id"].to_string(), seconds, p["attention"]["message"].as_str().unwrap_or(""));
            }
        }
        "focus" => {
            let pane = opts.pane()?;
            opts.finish()?;
            client.call("pane.focus", obj(pane, []))?;
        }
        "close" => {
            let pane = opts.pane()?;
            opts.finish()?;
            client.call("pane.close", obj(pane, []))?;
        }
        "call" => {
            let method = opts.positional().ok_or("call needs METHOD")?;
            let params = match opts.positional() {
                Some(raw) => json::parse(&raw).map_err(|e| format!("invalid JSON params: {e}"))?,
                None => Value::Null,
            };
            opts.finish()?;
            print_json(&client.call(&method, params)?);
        }
        other => return Err(format!("unknown ctl command `{other}`\n\n{USAGE}")),
    }
    Ok(ExitCode::SUCCESS)
}

fn obj<const N: usize>(pane: u64, rest: [(&str, Value); N]) -> Value {
    let mut params = Value::object(rest);
    params.insert("pane", pane);
    params
}

fn number(s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("expected a number, got `{s}`"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_owned() } else { s.chars().take(max - 1).chain(['…']).collect() }
}

fn print_json(value: &Value) {
    if !value.is_null() {
        println!("{value}");
    }
}

/// Tiny argument scanner: flags and `--key value` anywhere, positionals in
/// order, everything after `--` is a command line.
struct Opts {
    args: Vec<String>,
    command: Option<String>,
}

impl Opts {
    fn new(args: &[String]) -> Self {
        match args.iter().position(|a| a == "--") {
            Some(i) => Self { args: args[..i].to_vec(), command: Some(args[i + 1..].join(" ")).filter(|c| !c.is_empty()) },
            None => Self { args: args.to_vec(), command: None },
        }
    }

    fn flag(&mut self, name: &str) -> bool {
        let before = self.args.len();
        self.args.retain(|a| a != name);
        self.args.len() != before
    }

    fn value(&mut self, name: &str) -> Option<String> {
        let i = self.args.iter().position(|a| a == name)?;
        let value = self.args.get(i + 1).cloned()?;
        self.args.drain(i..=i + 1);
        Some(value)
    }

    fn positional(&mut self) -> Option<String> {
        let i = self.args.iter().position(|a| !a.starts_with("--"))?;
        Some(self.args.remove(i))
    }

    fn pane(&mut self) -> Result<u64, String> {
        number(&self.positional().ok_or("missing pane id")?)
    }

    fn command(&mut self) -> Option<String> {
        self.command.take()
    }

    fn finish(&self) -> Result<(), String> {
        match self.args.first() {
            Some(extra) => Err(format!("unexpected argument `{extra}`")),
            None => Ok(()),
        }
    }
}
