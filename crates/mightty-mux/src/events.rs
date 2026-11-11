//! Events pushed to socket connections that ask for them with
//! `events.subscribe`, as JSON-RPC notifications whose method is the event.
//!
//! | event              | params                                          |
//! |--------------------|-------------------------------------------------|
//! | `pane.opened`      | `pane`, `command`                               |
//! | `pane.closed`      | `pane`                                          |
//! | `pane.exited`      | `pane`, `exit_code`                             |
//! | `pane.focused`     | `pane`: keys go to it now                        |
//! | `pane.title`       | `pane`, `title`                                 |
//! | `pane.cwd`         | `pane`, `cwd`                                   |
//! | `pane.status`      | `pane`, `status`, `attention` (as in `pane.list`) |
//! | `command.started`  | `pane`, `command` (shell integration)           |
//! | `command.finished` | `pane`, `command`, `exit_code`, `duration_ms`   |

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};

use mightty_json::Value;

pub const EVENTS: &[&str] = &[
    "pane.opened",
    "pane.closed",
    "pane.exited",
    "pane.focused",
    "pane.title",
    "pane.cwd",
    "pane.status",
    "command.started",
    "command.finished",
];

/// Which connections want which events.
#[derive(Default)]
pub struct Events {
    subscribers: Mutex<Vec<Subscriber>>,
    /// Any subscribers at all: lets emitters skip building params.
    any: AtomicBool,
}

struct Subscriber {
    connection: u64,
    /// Event names, or prefixes ending in `.*`.
    patterns: Vec<String>,
    to: SyncSender<Value>,
}

impl Subscriber {
    fn wants(&self, event: &str) -> bool {
        self.patterns.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => event.starts_with(prefix),
            None => p == event,
        })
    }
}

impl Events {
    /// Sends `connection` the events `patterns` match (all of them for
    /// `*`), replacing what it asked for before. An unknown name is an error.
    pub fn subscribe(
        &self,
        connection: u64,
        patterns: Vec<String>,
        to: SyncSender<Value>,
    ) -> Result<(), String> {
        for p in &patterns {
            let known = match p.strip_suffix('*') {
                Some(prefix) => EVENTS.iter().any(|e| e.starts_with(prefix)),
                None => EVENTS.contains(&p.as_str()),
            };
            if !known {
                return Err(format!(
                    "unknown event `{p}`; events are {}",
                    EVENTS.join(", ")
                ));
            }
        }
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|s| s.connection != connection);
        if !patterns.is_empty() {
            subscribers.push(Subscriber {
                connection,
                patterns,
                to,
            });
        }
        self.any.store(!subscribers.is_empty(), Ordering::Relaxed);
        Ok(())
    }

    pub fn unsubscribe(&self, connection: u64) {
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|s| s.connection != connection);
        self.any.store(!subscribers.is_empty(), Ordering::Relaxed);
    }

    /// Whether anyone is listening; `emit` is cheap either way.
    pub fn active(&self) -> bool {
        self.any.load(Ordering::Relaxed)
    }

    /// Sends `event` to its subscribers; `params` is only called if there are any.
    pub fn emit(&self, event: &str, params: impl FnOnce() -> Value) {
        if !self.active() {
            return;
        }
        let mut subscribers = self.subscribers.lock().unwrap();
        if !subscribers.iter().any(|s| s.wants(event)) {
            return;
        }
        let message = Value::object([
            ("jsonrpc", "2.0".into()),
            ("method", event.into()),
            ("params", params()),
        ]);
        // A connection that has gone away stops listening; one that has
        // fallen behind misses the event.
        subscribers.retain(|s| {
            !s.wants(event)
                || !matches!(
                    s.to.try_send(message.clone()),
                    Err(TrySendError::Disconnected(_))
                )
        });
        self.any.store(!subscribers.is_empty(), Ordering::Relaxed);
    }
}
