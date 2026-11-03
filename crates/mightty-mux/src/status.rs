//! What a pane is doing, for the user and for agents: whether an AI agent
//! in it is working or waiting, and whether it needs the user.
//!
//! Agents don't say when they're waiting (most only notify terminals they
//! know), but they redraw a spinner while they work: an agent pane with
//! recent output is working, and one gone quiet is waiting.

use std::time::{Duration, Instant};

use mightty_pty::Foreground;
use mightty_term::Notification;

/// Programs treated as AI agents, by command name.
pub const AGENTS: &[&str] = &[
    "claude", "codex", "gemini", "aider", "opencode", "amp", "goose", "cursor-agent", "copilot", "crush", "qwen", "droid",
];

/// Output this recent means an agent is working.
const QUIET: Duration = Duration::from_millis(1500);
/// How long an agent must have worked for going quiet to need the user:
/// less is an echo, or its start-up screen.
const MIN_WORK: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// At a shell prompt.
    Idle,
    /// Running a command that isn't an agent.
    Running,
    /// An agent, producing output.
    Working,
    /// An agent that has gone quiet: waiting for the user, or done.
    Waiting,
    Exited,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Running => "running",
            Status::Working => "working",
            Status::Waiting => "waiting",
            Status::Exited => "exited",
        }
    }
}

/// Why a pane needs the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    Bell,
    /// The program asked for a notification (OSC 9, OSC 777).
    Notification(Notification),
    /// An agent stopped working.
    Waiting,
    /// Through the API (`pane.notify`), e.g. from an agent's hook.
    Asked(String),
}

impl Reason {
    pub fn name(&self) -> &'static str {
        match self {
            Reason::Bell => "bell",
            Reason::Notification(_) => "notification",
            Reason::Waiting => "waiting",
            Reason::Asked(_) => "asked",
        }
    }

    /// What to show the user.
    pub fn message(&self) -> String {
        match self {
            Reason::Bell => "Bell".into(),
            Reason::Notification(n) if n.title.is_empty() => n.body.clone(),
            Reason::Notification(n) => format!("{}: {}", n.title, n.body),
            Reason::Waiting => "Waiting for you".into(),
            Reason::Asked(text) => text.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attention {
    pub reason: Reason,
    /// When the pane first needed the user: the queue's order.
    pub since: Instant,
}

/// A pane's activity, kept up to date by its reader and the status monitor.
#[derive(Debug)]
pub(crate) struct Activity {
    last_output: Option<Instant>,
    /// When the current stretch of output began.
    busy_since: Option<Instant>,
    pub(crate) status: Status,
    bells: u64,
    pub(crate) attention: Option<Attention>,
}

impl Activity {
    pub(crate) fn new() -> Self {
        Self { last_output: None, busy_since: None, status: Status::Idle, bells: 0, attention: None }
    }

    /// Output arrived at `now`, the terminal having rung `bells` bells so
    /// far and asked for `notifications`. Returns whether attention changed.
    pub(crate) fn output(&mut self, now: Instant, bells: u64, mut notifications: Vec<Notification>, focused: bool) -> bool {
        if self.last_output.is_none_or(|at| now - at > QUIET) {
            self.busy_since = Some(now);
        }
        self.last_output = Some(now);
        let mut reason = (bells > self.bells).then_some(Reason::Bell);
        self.bells = bells;
        if let Some(notification) = notifications.pop() {
            reason = Some(Reason::Notification(notification));
        }
        match reason {
            Some(reason) if !focused => self.raise(reason, now),
            _ => false,
        }
    }

    /// Re-derives the status at `now`: whether the process has `exited`,
    /// a command is `running`, and it's an `agent`. Returns whether the
    /// status or attention changed.
    pub(crate) fn update(&mut self, now: Instant, exited: bool, running: bool, agent: bool, focused: bool) -> bool {
        let recent = self.last_output.is_some_and(|at| now - at < QUIET);
        let status = match (exited, agent && running, running) {
            (true, ..) => Status::Exited,
            (_, true, _) if recent => Status::Working,
            (_, true, _) => Status::Waiting,
            (_, _, true) => Status::Running,
            _ => Status::Idle,
        };
        let mut changed = status != self.status;
        if self.status == Status::Working && status == Status::Waiting && !focused {
            let worked = match (self.busy_since, self.last_output) {
                (Some(start), Some(end)) => end - start >= MIN_WORK,
                _ => false,
            };
            if worked {
                changed |= self.raise(Reason::Waiting, now);
            }
        }
        self.status = status;
        changed
    }

    /// Returns whether anything changed. A pane already waiting keeps its
    /// place in the queue.
    pub(crate) fn raise(&mut self, reason: Reason, now: Instant) -> bool {
        match &mut self.attention {
            Some(attention) if attention.reason == reason => false,
            Some(attention) => {
                attention.reason = reason;
                true
            }
            None => {
                self.attention = Some(Attention { reason, since: now });
                true
            }
        }
    }

    pub(crate) fn clear(&mut self) -> bool {
        self.attention.take().is_some()
    }
}

/// Interpreters an agent may run under: the script names the agent.
const INTERPRETERS: &[&str] = &["node", "bun", "deno", "python", "python3"];

/// Whether `command_line` (from shell integration, or what the pane was
/// started with), or the terminal's `foreground` process, is an agent.
pub fn is_agent(command_line: Option<&str>, foreground: Option<&Foreground>) -> bool {
    let base = |w: &str| w.rsplit('/').next().unwrap_or(w).to_owned();
    // The program: past `VAR=value` assignments, without its directory.
    let program = command_line.and_then(|c| c.split_whitespace().find(|w| !w.contains('='))).map(base);
    // A symlinked binary can be named after its version: its arguments say what it was run as.
    let run_as = foreground.and_then(|f| f.args.first()).map(|a| base(a));
    let script = foreground
        .filter(|_| run_as.as_deref().is_some_and(|r| INTERPRETERS.contains(&r)))
        .and_then(|f| f.args.get(1))
        .map(|a| base(a).split('.').next().unwrap_or_default().to_owned());
    let name = foreground.map(|f| f.name.clone());
    [program, run_as, script, name].into_iter().flatten().any(|n| AGENTS.contains(&n.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn process(name: &str, args: &[&str]) -> Foreground {
        Foreground { name: name.into(), args: args.iter().map(|a| a.to_string()).collect() }
    }

    #[test]
    fn agents_by_name() {
        assert!(is_agent(Some("claude --resume"), None));
        assert!(is_agent(Some("FOO=1 ~/.local/bin/codex"), None));
        assert!(is_agent(None, Some(&process("gemini", &[]))));
        // Claude Code's binary is named after its version, behind a `claude` symlink.
        assert!(is_agent(Some("zsh"), Some(&process("2.1.316", &["claude", "--resume"]))));
        // Installed with npm: node running the agent's script.
        assert!(is_agent(None, Some(&process("node", &["node", "/usr/local/lib/node_modules/@openai/codex/bin/codex.js"]))));
        assert!(!is_agent(None, Some(&process("node", &["node", "server.js"]))));
        assert!(!is_agent(Some("vim claude.md"), Some(&process("vim", &["vim", "claude.md"]))));
        assert!(!is_agent(None, None));
    }

    #[test]
    fn an_agent_that_stops_working_needs_you() {
        let t = Instant::now();
        let mut a = Activity::new();
        // Working for 4 s, a spinner frame every 100 ms.
        for ms in (0..=4000).step_by(100) {
            a.output(at(t, ms), 0, Vec::new(), false);
            a.update(at(t, ms), false, true, true, false);
        }
        assert_eq!(a.status, Status::Working);
        assert!(a.attention.is_none());
        assert!(a.update(at(t, 6000), false, true, true, false));
        assert_eq!(a.status, Status::Waiting);
        assert_eq!(a.attention.as_ref().map(|a| a.reason.clone()), Some(Reason::Waiting));
        assert!(a.clear());
        assert!(!a.clear());
    }

    #[test]
    fn short_bursts_and_watched_panes_dont() {
        let t = Instant::now();
        let mut a = Activity::new();
        // An echo, or an agent's start-up screen.
        a.output(at(t, 0), 0, Vec::new(), false);
        a.update(at(t, 500), false, true, true, false);
        a.update(at(t, 3000), false, true, true, false);
        assert_eq!(a.status, Status::Waiting);
        assert!(a.attention.is_none());
        // The user is watching it work and stop.
        let mut a = Activity::new();
        for ms in (0..=4000).step_by(100) {
            a.output(at(t, ms), 0, Vec::new(), true);
            a.update(at(t, ms), false, true, true, true);
        }
        a.update(at(t, 6000), false, true, true, true);
        assert!(a.attention.is_none());
        // Not an agent: running, never waiting.
        let mut a = Activity::new();
        a.output(at(t, 0), 0, Vec::new(), false);
        a.update(at(t, 6000), false, true, false, false);
        assert_eq!(a.status, Status::Running);
    }

    #[test]
    fn bells_and_notifications() {
        let t = Instant::now();
        let mut a = Activity::new();
        assert!(a.output(at(t, 0), 1, Vec::new(), false));
        assert_eq!(a.attention.as_ref().map(|a| a.reason.name()), Some("bell"));
        let since = a.attention.as_ref().unwrap().since;
        // A later notification takes over the reason, keeping its place in the queue.
        let note = Notification { title: "Build".into(), body: "done".into() };
        assert!(a.output(at(t, 100), 1, vec![note], false));
        let attention = a.attention.clone().unwrap();
        assert_eq!((attention.reason.message(), attention.since), ("Build: done".to_owned(), since));
        // Not while the user is looking.
        let mut a = Activity::new();
        assert!(!a.output(at(t, 0), 1, Vec::new(), true));
        assert!(a.attention.is_none());
    }
}
