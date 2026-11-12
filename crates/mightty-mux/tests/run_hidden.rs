//! `pane.run` without a pane, in a process of its own: the hidden shell is
//! the default one, so this sets `SHELL` and `HOME` for the whole process.

use std::sync::Arc;

use mightty_json::{self as json, Value};
use mightty_mux::{Headless, Mux, handle};

fn call(mux: &Arc<Mux>, method: &str, params: &str) -> Value {
    handle(mux, method, &json::parse(params).unwrap())
        .unwrap_or_else(|e| panic!("{method}: {}", e.message))
}

#[test]
fn hidden_shell_closes_unless_kept() {
    let home = std::env::temp_dir().join(format!("mightty-test-hidden-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    // A .zshrc also stops zsh from starting its new-user wizard.
    std::fs::write(home.join(".zshrc"), "PS1='ready> '\n").unwrap();
    // SAFETY: this test is the only thread in its process reading the environment.
    unsafe {
        std::env::set_var("SHELL", "/bin/zsh");
        std::env::set_var("HOME", &home);
    }
    let mux = Mux::new(Arc::new(Headless));
    let panes = |mux: &Arc<Mux>| call(mux, "pane.list", "{}").as_array().unwrap().len();

    let out = call(&mux, "pane.run", r#"{"command": "echo one"}"#);
    assert_eq!(out["output"].as_str(), Some("one"), "{out}");
    assert!(out["pane"].is_null(), "{out}");
    assert_eq!(panes(&mux), 0);

    let out = call(&mux, "pane.run", r#"{"command": "echo two", "keep": true}"#);
    assert_eq!(out["output"].as_str(), Some("two"), "{out}");
    let id = out["pane"].as_u64().unwrap();
    assert_eq!(panes(&mux), 1);
    mux.close(id);
    let _ = std::fs::remove_dir_all(home);
}
