//! Commands that run a while need the user when they finish. In a test
//! binary of its own: the threshold is process-wide.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mightty_mux::{Headless, Mux, Reason, SpawnOptions, set_notify_after};

fn wait_for(what: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if what() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn long_commands_need_the_user_when_they_finish() {
    let home = std::env::temp_dir().join(format!("mightty-test-finish-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".zshrc"), "PS1='ready> '\n").unwrap();
    set_notify_after(Some(Duration::from_millis(300)));
    let mux = Mux::new(Arc::new(Headless));
    let pane = mux
        .spawn(SpawnOptions {
            shell: Some("/bin/zsh".into()),
            env: vec![("HOME".into(), home.display().to_string())],
            cols: 80,
            rows: 10,
            ..Default::default()
        })
        .unwrap();
    let screen = || pane.term.lock().unwrap().screen_text();
    assert!(wait_for(|| screen().contains("ready>")), "{}", screen());

    // Quick: nothing.
    pane.write(b"true\r").unwrap();
    assert!(wait_for(|| screen().matches("ready>").count() == 2));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(pane.attention(), None);

    // Long, and failing.
    pane.write(b"sleep 0.5; false\r").unwrap();
    assert!(wait_for(|| pane.attention().is_some()));
    let reason = pane.attention().unwrap().reason;
    let Reason::Finished {
        command,
        exit_code,
        took,
    } = &reason
    else {
        panic!("{reason:?}");
    };
    assert_eq!(command.as_deref(), Some("sleep 0.5; false"));
    assert_eq!(*exit_code, Some(1));
    assert!(*took >= Duration::from_millis(500), "{took:?}");
    assert!(
        reason
            .message()
            .starts_with("“sleep 0.5; false” failed with exit code 1 after")
    );

    // Not in the pane the user is looking at.
    pane.set_focused(true);
    pane.write(b"sleep 0.5\r").unwrap();
    assert!(wait_for(|| screen().matches("ready>").count() == 4));
    assert_eq!(pane.attention(), None);

    // And not at all once turned off.
    pane.set_focused(false);
    set_notify_after(None);
    pane.write(b"sleep 0.5\r").unwrap();
    assert!(wait_for(|| screen().matches("ready>").count() == 5));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(pane.attention(), None);
    let _ = std::fs::remove_dir_all(&home);
}
