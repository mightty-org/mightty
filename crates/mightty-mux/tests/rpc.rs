//! Drives real shells through the control API, in-process and over the socket.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mightty_json::{self as json, Value};
use mightty_mux::{Client, Headless, Mux, handle, listen};

fn call(mux: &Arc<Mux>, method: &str, params: &str) -> Value {
    handle(mux, method, &json::parse(params).unwrap())
        .unwrap_or_else(|e| panic!("{method}: {}", e.message))
}

#[test]
fn spawn_read_wait_exit() {
    let mux = Mux::new(Arc::new(Headless));
    let pane = call(
        &mux,
        "pane.spawn",
        r#"{"command": "printf 'hello\\n'; read x; echo got $x; exit 3", "shell": "/bin/sh", "visible": false}"#,
    );
    let id = pane["id"].as_u64().unwrap();
    let waited = call(
        &mux,
        "pane.wait",
        &format!(r#"{{"pane": {id}, "text": "hello"}}"#),
    );
    assert_eq!(waited["matched"].as_str(), Some("text"));

    call(
        &mux,
        "pane.send_text",
        &format!(r#"{{"pane": {id}, "text": "abc", "enter": true}}"#),
    );
    let waited = call(
        &mux,
        "pane.wait",
        &format!(r#"{{"pane": {id}, "exited": true}}"#),
    );
    assert_eq!(waited["exit_code"].as_i64(), Some(3));

    let read = call(&mux, "pane.read", &format!(r#"{{"pane": {id}}}"#));
    assert_eq!(read["text"].as_str(), Some("hello\nabc\ngot abc"));
    let listed = call(&mux, "pane.list", "{}");
    assert_eq!(
        listed.as_array().unwrap()[0]["alive"].as_bool(),
        Some(false)
    );
    call(&mux, "pane.close", &format!(r#"{{"pane": {id}}}"#));
    assert!(call(&mux, "pane.list", "{}").as_array().unwrap().is_empty());
}

/// A program can close the terminal and go on running; the pane stays
/// usable meanwhile.
#[test]
fn pane_answers_while_its_program_runs_detached() {
    let mux = Mux::new(Arc::new(Headless));
    let pane = call(
        &mux,
        "pane.spawn",
        r#"{"command": "exec </dev/null >/dev/null 2>&1; sleep 2", "shell": "/bin/sh", "visible": false}"#,
    );
    let pane = mux.pane(pane["id"].as_u64().unwrap()).unwrap();
    // Time for the reader to see the terminal closed.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let start = std::time::Instant::now();
    pane.hides_input();
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    mux.close(pane.id);
}

#[test]
fn errors_are_reported() {
    let mux = Mux::new(Arc::new(Headless));
    let err = handle(&mux, "pane.read", &json::parse(r#"{"pane": 99}"#).unwrap()).unwrap_err();
    assert_eq!(err.code, -32602);
    assert_eq!(handle(&mux, "nope", &Value::Null).unwrap_err().code, -32601);
}

const ZSH_7: &str = "exit_with_7() { return 7 }";
const BASH_7: &str = "exit_with_7() { return 7; }";
const ECHO_NONCE: &str = "echo ${MIGHTTY_NONCE:-hidden}";

/// `pane.run` with zsh shell integration, isolated from the user's dotfiles.
#[test]
fn run_uses_zsh_integration() {
    // A .zshrc also stops zsh from starting its new-user wizard.
    check_integration(
        "/bin/zsh",
        &[(".zshrc", "PS1='ready> '\n")],
        ZSH_7,
        ECHO_NONCE,
    );
}

/// The same with bash, macOS's 3.2 and a current one: the bootstrap still
/// runs the user's profile and keeps their PROMPT_COMMAND, and a pipeline
/// is one command.
#[test]
fn run_uses_bash_integration() {
    for shell in ["/bin/bash", "/opt/homebrew/bin/bash"] {
        let profile = "PS1='ready> '\nPROMPT_COMMAND='_seen=1;'\n";
        let Some((mux, id)) =
            check_integration(shell, &[(".bash_profile", profile)], BASH_7, ECHO_NONCE)
        else {
            continue;
        };
        let out = call(
            &mux,
            "pane.run",
            &format!(r#"{{"pane": {id}, "command": "echo ${{_seen:-none}}; cd /tmp"}}"#),
        );
        assert_eq!(out["output"].as_str(), Some("1"), "{shell}: {out}");
        let out = call(
            &mux,
            "pane.run",
            &format!(r#"{{"pane": {id}, "command": "echo abc | tr a x | tr b y"}}"#),
        );
        assert_eq!(
            (out["output"].as_str(), out["command"].as_str()),
            (Some("xyc"), Some("echo abc | tr a x | tr b y")),
            "{shell}: {out}"
        );
        let commands = call(
            &mux,
            "pane.commands",
            &format!(r#"{{"pane": {id}, "limit": 10}}"#),
        );
        assert_eq!(commands.as_array().unwrap().len(), 4, "{shell}: {commands}");
        assert!(cwd(&mux, id).ends_with("/tmp"), "{shell}");
        mux.close(id);
    }
}

/// A command bash doesn't save to history is still the one reported, not
/// the last one saved.
#[test]
fn bash_command_kept_out_of_history() {
    for shell in ["/bin/bash", "/opt/homebrew/bin/bash"] {
        let profile = "PS1='ready> '\nHISTCONTROL=ignorespace\n";
        let Some((mux, id)) =
            check_integration(shell, &[(".bash_profile", profile)], BASH_7, ECHO_NONCE)
        else {
            continue;
        };
        let out = call(
            &mux,
            "pane.run",
            &format!(r#"{{"pane": {id}, "command": " echo unsaved"}}"#),
        );
        assert_eq!(
            out["command"].as_str(),
            Some("echo unsaved"),
            "{shell}: {out}"
        );
        mux.close(id);
    }
}

/// bash 5.1+ can keep PROMPT_COMMAND as an array, each element run in turn.
#[test]
fn bash_prompt_command_array() {
    let profile = "PS1='ready> '\nPROMPT_COMMAND=('_seen=1' '_also=2')\n";
    let shell = "/opt/homebrew/bin/bash";
    let Some((mux, id)) =
        check_integration(shell, &[(".bash_profile", profile)], BASH_7, ECHO_NONCE)
    else {
        return;
    };
    let out = call(
        &mux,
        "pane.run",
        &format!(r#"{{"pane": {id}, "command": "echo $_seen$_also"}}"#),
    );
    assert_eq!(out["output"].as_str(), Some("12"), "{out}");
    mux.close(id);
}

#[test]
fn run_uses_fish_integration() {
    let config = "set -g fish_greeting\nfunction fish_prompt; echo -n 'ready> '; end\n";
    let fn_7 = "function exit_with_7; return 7; end";
    let echo_nonce = "set -q MIGHTTY_NONCE; and echo $MIGHTTY_NONCE; or echo hidden";
    let files = [(".config/fish/config.fish", config)];
    let Some((mux, id)) = check_integration("/opt/homebrew/bin/fish", &files, fn_7, echo_nonce)
    else {
        return;
    };
    // Our directory is out of XDG_DATA_DIRS again for what the shell runs.
    let leak =
        "cd /tmp; string match -q '*mightty*' -- $XDG_DATA_DIRS; and echo leaked; or echo clean";
    let out = call(
        &mux,
        "pane.run",
        &format!(r#"{{"pane": {id}, "command": "{leak}"}}"#),
    );
    assert_eq!(out["output"].as_str(), Some("clean"), "{out}");
    assert!(cwd(&mux, id).ends_with("/tmp"));
    mux.close(id);
}

/// A directory whose name could end the shell's report of it early
/// doesn't put the nonce on screen.
#[test]
fn odd_directory_keeps_the_nonce_hidden() {
    let shells = [
        ("/bin/zsh", ".zshrc", "PS1='ready> '\n"),
        ("/bin/bash", ".bash_profile", "PS1='ready> '\n"),
        (
            "/opt/homebrew/bin/fish",
            ".config/fish/config.fish",
            "set -g fish_greeting\nfunction fish_prompt; echo -n 'ready> '; end\n",
        ),
    ];
    for (shell, file, profile) in shells {
        if !Path::new(shell).exists() {
            continue;
        }
        let name = Path::new(shell).file_name().unwrap().to_str().unwrap();
        let home =
            std::env::temp_dir().join(format!("mightty-test-odd-{name}-{}", std::process::id()));
        let odd = home.join("a\x07b\x1b\\c");
        std::fs::create_dir_all(&odd).unwrap();
        let path = home.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, profile).unwrap();
        let mux = Mux::new(Arc::new(Headless));
        let params = Value::object([
            ("shell", shell.into()),
            ("visible", false.into()),
            ("cwd", odd.to_str().unwrap().into()),
            (
                "env",
                Value::object([("HOME", home.to_str().unwrap().into())]),
            ),
        ]);
        let id = handle(&mux, "pane.spawn", &params).unwrap()["id"]
            .as_u64()
            .unwrap();
        call(
            &mux,
            "pane.wait",
            &format!(r#"{{"pane": {id}, "text": "ready>"}}"#),
        );
        let screen = call(&mux, "pane.read", &format!(r#"{{"pane": {id}}}"#));
        assert!(!screen.to_string().contains("nonce"), "{shell}: {screen}");
        mux.close(id);
        let _ = std::fs::remove_dir_all(home);
    }
}

fn cwd(mux: &Arc<Mux>, id: u64) -> String {
    let panes = call(mux, "pane.list", "{}");
    let pane = panes
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"].as_u64() == Some(id))
        .unwrap()
        .clone();
    pane["cwd"].as_str().unwrap_or_default().to_owned()
}

/// Runs commands through `shell`'s integration in a home holding `files`;
/// in that shell's syntax, `fn_7` defines `exit_with_7` and `echo_nonce`
/// prints `$MIGHTTY_NONCE` or "hidden". Returns the mux and pane for more
/// checks, or `None` if the shell isn't installed.
fn check_integration(
    shell: &str,
    files: &[(&str, &str)],
    fn_7: &str,
    echo_nonce: &str,
) -> Option<(Arc<Mux>, u64)> {
    if !Path::new(shell).exists() {
        return None;
    }
    let name = Path::new(shell).file_name().unwrap().to_str().unwrap();
    // Tests run in parallel: a home of their own each.
    static HOMES: AtomicUsize = AtomicUsize::new(0);
    let n = HOMES.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!(
        "mightty-test-home-{name}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    for (file, text) in files {
        let path = home.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mux = Mux::new(Arc::new(Headless));
    let params = format!(
        r#"{{"shell": "{shell}", "visible": false, "env": {{"HOME": "{}"}}}}"#,
        home.display()
    );
    let id = call(&mux, "pane.spawn", &params)["id"].as_u64().unwrap();
    let ready = call(
        &mux,
        "pane.wait",
        &format!(r#"{{"pane": {id}, "text": "ready>"}}"#),
    );
    let screen = call(&mux, "pane.read", &format!(r#"{{"pane": {id}}}"#));
    assert_eq!(
        ready["matched"].as_str(),
        Some("text"),
        "{shell} screen: {screen}"
    );

    let out = call(
        &mux,
        "pane.run",
        &format!(r#"{{"pane": {id}, "command": "echo one; echo two; false"}}"#),
    );
    assert_eq!(out["output"].as_str(), Some("one\ntwo"), "{shell}: {out}");
    assert_eq!(out["exit_code"].as_i64(), Some(1));
    assert_eq!(out["command"].as_str(), Some("echo one; echo two; false"));
    assert_eq!(out["timed_out"].as_bool(), Some(false));

    // Output imitating a finished command doesn't end the run early, and
    // the nonce that real marks carry isn't visible to the command.
    let spoof = format!(r#"printf '\\e]133;D;0\\a'; sleep 0.3; {echo_nonce}; {fn_7}; exit_with_7"#);
    let out = call(
        &mux,
        "pane.run",
        &format!(r#"{{"pane": {id}, "command": "{spoof}"}}"#),
    );
    assert_eq!(out["output"].as_str(), Some("hidden"), "{shell}: {out}");
    assert_eq!(out["exit_code"].as_i64(), Some(7));

    let commands = call(&mux, "pane.commands", &format!(r#"{{"pane": {id}}}"#));
    assert_eq!(commands.as_array().unwrap().len(), 2, "{shell}: {commands}");
    let _ = std::fs::remove_dir_all(home);
    Some((mux, id))
}

#[test]
fn socket_round_trip() {
    let mux = Mux::new(Arc::new(Headless));
    let path = listen(mux.clone()).unwrap();
    let mut client = Client::connect(&path).unwrap();
    let info = client.call("app.info", Value::Null).unwrap();
    assert_eq!(info["pid"].as_u64(), Some(std::process::id().into()));
    assert!(
        client
            .call("pane.read", json::parse(r#"{"pane": 5}"#).unwrap())
            .unwrap_err()
            .to_string()
            .contains("no pane 5")
    );
    let _ = std::fs::remove_file(path);
}
