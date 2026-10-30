//! Drives real shells through the control API, in-process and over the socket.

use std::path::Path;
use std::sync::Arc;

use mightty_json::{self as json, Value};
use mightty_mux::{Client, Headless, Mux, handle, listen};

fn call(mux: &Arc<Mux>, method: &str, params: &str) -> Value {
    handle(mux, method, &json::parse(params).unwrap()).unwrap_or_else(|e| panic!("{method}: {}", e.message))
}

#[test]
fn spawn_read_wait_exit() {
    let mux = Mux::new(Arc::new(Headless));
    let pane = call(&mux, "pane.spawn", r#"{"command": "printf 'hello\\n'; read x; echo got $x; exit 3", "shell": "/bin/sh", "visible": false}"#);
    let id = pane["id"].as_u64().unwrap();
    let waited = call(&mux, "pane.wait", &format!(r#"{{"pane": {id}, "text": "hello"}}"#));
    assert_eq!(waited["matched"].as_str(), Some("text"));

    call(&mux, "pane.send_text", &format!(r#"{{"pane": {id}, "text": "abc", "enter": true}}"#));
    let waited = call(&mux, "pane.wait", &format!(r#"{{"pane": {id}, "exited": true}}"#));
    assert_eq!(waited["exit_code"].as_i64(), Some(3));

    let read = call(&mux, "pane.read", &format!(r#"{{"pane": {id}}}"#));
    assert_eq!(read["text"].as_str(), Some("hello\nabc\ngot abc"));
    let listed = call(&mux, "pane.list", "{}");
    assert_eq!(listed.as_array().unwrap()[0]["alive"].as_bool(), Some(false));
    call(&mux, "pane.close", &format!(r#"{{"pane": {id}}}"#));
    assert!(call(&mux, "pane.list", "{}").as_array().unwrap().is_empty());
}

#[test]
fn errors_are_reported() {
    let mux = Mux::new(Arc::new(Headless));
    let err = handle(&mux, "pane.read", &json::parse(r#"{"pane": 99}"#).unwrap()).unwrap_err();
    assert_eq!(err.code, -32602);
    assert_eq!(handle(&mux, "nope", &Value::Null).unwrap_err().code, -32601);
}

/// `pane.run` with zsh shell integration, isolated from the user's dotfiles.
#[test]
fn run_uses_shell_integration() {
    if !Path::new("/bin/zsh").exists() {
        return;
    }
    let home = std::env::temp_dir().join(format!("mightty-test-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    // A .zshrc also stops zsh from starting its new-user wizard.
    std::fs::write(home.join(".zshrc"), "PS1='ready> '\n").unwrap();
    let mux = Mux::new(Arc::new(Headless));
    let params = format!(r#"{{"shell": "/bin/zsh", "visible": false, "env": {{"HOME": "{}"}}}}"#, home.display());
    let id = call(&mux, "pane.spawn", &params)["id"].as_u64().unwrap();
    let ready = call(&mux, "pane.wait", &format!(r#"{{"pane": {id}, "text": "ready>"}}"#));
    let screen = call(&mux, "pane.read", &format!(r#"{{"pane": {id}}}"#));
    assert_eq!(ready["matched"].as_str(), Some("text"), "screen: {screen}");

    let out = call(&mux, "pane.run", &format!(r#"{{"pane": {id}, "command": "echo one; echo two; false"}}"#));
    assert_eq!(out["output"].as_str(), Some("one\ntwo"));
    assert_eq!(out["exit_code"].as_i64(), Some(1));
    assert_eq!(out["command"].as_str(), Some("echo one; echo two; false"));
    assert_eq!(out["timed_out"].as_bool(), Some(false));

    // Output imitating a finished command doesn't end the run early, and
    // the nonce that real marks carry isn't visible to the command.
    let spoof = r#"printf '\\e]133;D;0\\a'; sleep 0.3; echo ${MIGHTTY_NONCE:-hidden}; exit_with_7() { return 7 }; exit_with_7"#;
    let out = call(&mux, "pane.run", &format!(r#"{{"pane": {id}, "command": "{spoof}"}}"#));
    assert_eq!(out["output"].as_str(), Some("hidden"), "{out}");
    assert_eq!(out["exit_code"].as_i64(), Some(7));

    let commands = call(&mux, "pane.commands", &format!(r#"{{"pane": {id}}}"#));
    assert_eq!(commands.as_array().unwrap().len(), 2);
    mux.close(id);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn socket_round_trip() {
    let mux = Mux::new(Arc::new(Headless));
    let path = listen(mux.clone()).unwrap();
    let mut client = Client::connect(&path).unwrap();
    let info = client.call("app.info", Value::Null).unwrap();
    assert_eq!(info["pid"].as_u64(), Some(std::process::id().into()));
    assert!(client.call("pane.read", json::parse(r#"{"pane": 5}"#).unwrap()).unwrap_err().to_string().contains("no pane 5"));
    let _ = std::fs::remove_file(path);
}
