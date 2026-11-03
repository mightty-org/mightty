//! Pane multiplexer. Owns every pane (a PTY plus its terminal state)
//! independently of any window and serves them over a JSON-RPC control
//! socket. The GUI is one client of it; `mightty ctl` and the MCP server are
//! others.

mod keys;
mod rpc;
mod shell;
mod socket;
mod status;

use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use mightty_pty::{Pty, WindowSize};
use mightty_term::{Term, TermOptions};

pub use keys::key_bytes;
pub use rpc::{RpcError, handle};
pub use socket::{CallError, Client, listen, runtime_dir};
pub use status::{AGENTS, Attention, Reason, Status, is_agent};
use status::Activity;

pub type PaneId = u64;

/// How the embedding front end reacts to pane events. Called from pane
/// reader threads, so implementations must hand work to their UI thread.
pub trait Host: Send + Sync {
    /// New output has been parsed into the pane's terminal.
    fn pane_output(&self, _id: PaneId) {}
    /// The pane's process exited.
    fn pane_exited(&self, _id: PaneId) {}
    /// The pane was closed through the API; drop any window showing it.
    fn pane_closed(&self, _id: PaneId) {}
    /// Its status or attention changed (see [`Pane::status`], [`Pane::attention`]).
    fn pane_status(&self, _id: PaneId) {}
    /// Shows the pane, where `placement` says. Returns false if the host has no UI.
    fn show_pane(&self, _id: PaneId, _placement: Placement) -> bool {
        false
    }
    /// Where each shown pane is.
    fn locations(&self) -> Vec<(PaneId, Location)> {
        Vec::new()
    }
    /// Brings the pane to the front and gives it the keyboard. False if it isn't shown.
    fn focus_pane(&self, _id: PaneId) -> bool {
        false
    }
}

/// Where to show a new pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    /// A window of its own.
    #[default]
    Window,
    /// A tab of the window `beside` is in.
    Tab { beside: Option<PaneId> },
    /// A split next to `beside`: to its right, or with `down` below it.
    Split { beside: Option<PaneId>, down: bool },
}

impl Placement {
    /// From its name in the API (`window`, `tab`, `right`, `down`); `beside`
    /// defaults to the pane the user is in.
    pub fn parse(name: &str, beside: Option<PaneId>) -> Option<Self> {
        Some(match name {
            "window" => Placement::Window,
            "tab" => Placement::Tab { beside },
            "right" => Placement::Split { beside, down: false },
            "down" => Placement::Split { beside, down: true },
            _ => return None,
        })
    }
}

/// Where a shown pane is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    /// The window (or tab) it is in, by the window server's number.
    pub window: u64,
    /// Its tab's position in the window's tab bar, from 0.
    pub tab: usize,
    /// Keys go to it.
    pub focused: bool,
}

/// A host with no UI: the MCP server's fallback, and tests.
pub struct Headless;

impl Host for Headless {}

#[derive(Clone, Debug)]
pub struct SpawnOptions {
    /// Command line run with `$SHELL -l -c`; `None` starts an interactive login shell.
    pub command: Option<String>,
    /// Defaults to `$SHELL`.
    pub shell: Option<String>,
    /// Defaults to `$HOME`.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    /// Ask the host to show the pane, at `placement`.
    pub visible: bool,
    pub placement: Placement,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            command: None,
            shell: None,
            cwd: None,
            env: Vec::new(),
            cols: 100,
            rows: 30,
            visible: false,
            placement: Placement::Window,
        }
    }
}

pub struct Pane {
    pub id: PaneId,
    pub term: Arc<Mutex<Term>>,
    /// What was launched, for listings.
    pub command: String,
    writer: Mutex<File>,
    pty: Mutex<Pty>,
    /// `Some(exit code)` once the process is gone; the code is `None` if a signal killed it.
    exit: Mutex<Option<Option<i32>>>,
    visible: AtomicBool,
    /// Output generation counter, bumped after every read, for waiters.
    changed: (Mutex<u64>, Condvar),
    activity: Mutex<Activity>,
    /// The user is looking at it: it can't need them.
    focused: AtomicBool,
}

impl Pane {
    /// Sends input to the pane's process.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.writer.lock().unwrap().write_all(bytes)
    }

    /// Resizes the terminal and tells the process (SIGWINCH).
    pub fn resize(&self, cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) {
        let mut term = self.term.lock().unwrap();
        if (term.cols(), term.rows()) == (cols.into(), rows.into()) {
            return;
        }
        term.resize(cols.into(), rows.into());
        drop(term);
        let _ = self.pty.lock().unwrap().resize(WindowSize { cols, rows, pixel_width, pixel_height });
    }

    /// `None` while running; `Some(code)` after exit, where `code` is `None` for a signal.
    pub fn exit_status(&self) -> Option<Option<i32>> {
        *self.exit.lock().unwrap()
    }

    pub fn is_alive(&self) -> bool {
        self.exit_status().is_none()
    }

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Relaxed);
    }

    pub fn status(&self) -> Status {
        self.activity.lock().unwrap().status
    }

    /// Why it needs the user, if it does.
    pub fn attention(&self) -> Option<Attention> {
        self.activity.lock().unwrap().attention.clone()
    }

    /// Whether the user is looking at it. Looking clears its attention;
    /// returns whether that changed.
    pub fn set_focused(&self, focused: bool) -> bool {
        self.focused.store(focused, Ordering::Relaxed);
        focused && self.activity.lock().unwrap().clear()
    }

    /// Asks for the user, unless they're looking at it. Returns whether it changed anything.
    pub fn request_attention(&self, reason: Reason) -> bool {
        !self.focused.load(Ordering::Relaxed) && self.activity.lock().unwrap().raise(reason, Instant::now())
    }

    /// Re-derives the status (see [`status`]). Returns whether it or attention changed.
    fn update_status(&self) -> bool {
        let (running, integrated) = {
            let term = self.term.lock().unwrap();
            (term.running_command(), !term.marks().is_empty())
        };
        // Without shell integration, whatever is in the foreground other than the shell.
        let foreground = if running.is_some() || !integrated { self.pty.lock().unwrap().foreground() } else { None };
        let busy = running.is_some() || (!integrated && foreground.as_ref().is_some_and(|f| f.name != self.command));
        // A pane started with a command line is running that, at first.
        let command = running.as_deref().or((!integrated).then_some(self.command.as_str()));
        let agent = is_agent(command, foreground.as_ref());
        let focused = self.focused.load(Ordering::Relaxed);
        self.activity.lock().unwrap().update(Instant::now(), !self.is_alive(), busy, agent, focused)
    }

    /// Current output generation.
    pub fn generation(&self) -> u64 {
        *self.changed.0.lock().unwrap()
    }

    /// Blocks until the generation moves past `seen` or `timeout` elapses.
    pub fn wait_output(&self, seen: u64, timeout: Duration) -> u64 {
        let (lock, cv) = &self.changed;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv.wait_timeout_while(guard, timeout, |g| *g == seen).unwrap();
        *guard
    }

    fn notify(&self) {
        let (lock, cv) = &self.changed;
        *lock.lock().unwrap() += 1;
        cv.notify_all();
    }
}

pub struct Mux {
    panes: Mutex<BTreeMap<PaneId, Arc<Pane>>>,
    next_id: AtomicU64,
    host: Arc<dyn Host>,
    socket: OnceLock<PathBuf>,
    /// For every pane's terminal.
    options: TermOptions,
}

impl Mux {
    pub fn new(host: Arc<dyn Host>) -> Arc<Self> {
        Self::with_options(host, TermOptions::default())
    }

    pub fn with_options(host: Arc<dyn Host>, options: TermOptions) -> Arc<Self> {
        let mux = Arc::new(Self { panes: Mutex::default(), next_id: AtomicU64::new(1), host, socket: OnceLock::new(), options });
        spawn_monitor(Arc::downgrade(&mux));
        mux
    }

    /// Panes needing the user, the longest waiting first.
    pub fn attention_queue(&self) -> Vec<Arc<Pane>> {
        let mut waiting: Vec<_> = self.panes().into_iter().filter_map(|p| Some((p.attention()?.since, p))).collect();
        waiting.sort_by_key(|w| w.0);
        waiting.into_iter().map(|w| w.1).collect()
    }

    /// The control socket, once `listen` has bound it.
    pub fn socket_path(&self) -> Option<&Path> {
        self.socket.get().map(PathBuf::as_path)
    }

    pub fn spawn(&self, opts: SpawnOptions) -> io::Result<Arc<Pane>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let shell = opts.shell.clone().unwrap_or_else(default_shell);
        let name = Path::new(&shell).file_name().map_or("sh".into(), |n| n.to_string_lossy().into_owned());
        let mut cmd = Command::new(&shell);
        let label = match &opts.command {
            Some(line) => {
                cmd.args(["-l", "-c", line]);
                line.clone()
            }
            None => {
                cmd.arg0(format!("-{name}")); // leading dash: login shell
                name
            }
        };
        let home = env::var_os("HOME").map(PathBuf::from);
        if let Some(cwd) = opts.cwd.clone().or(home) {
            cmd.current_dir(cwd);
        }
        cmd.envs(opts.env.iter().map(|(k, v)| (k, v)));
        cmd.env("MIGHTTY_PANE_ID", id.to_string());
        if let Some(socket) = self.socket_path() {
            cmd.env("MIGHTTY_SOCKET", socket);
        }
        let nonce = shell::integrate(&mut cmd, &shell, opts.command.is_none());

        let pty = Pty::spawn(cmd, WindowSize { cols: opts.cols, rows: opts.rows, ..Default::default() })?;
        let reader = pty.reader()?;
        let mut term = Term::with_options(opts.cols.into(), opts.rows.into(), &self.options);
        if let Some(nonce) = &nonce {
            term.set_mark_nonce(nonce);
        }
        let pane = Arc::new(Pane {
            id,
            term: Arc::new(Mutex::new(term)),
            command: label,
            writer: Mutex::new(pty.writer()?),
            pty: Mutex::new(pty),
            exit: Mutex::new(None),
            visible: AtomicBool::new(false),
            changed: (Mutex::new(0), Condvar::new()),
            activity: Mutex::new(Activity::new()),
            focused: AtomicBool::new(false),
        });
        self.panes.lock().unwrap().insert(id, pane.clone());
        spawn_reader(pane.clone(), reader, self.host.clone());
        if opts.visible {
            pane.set_visible(self.host.show_pane(id, opts.placement));
        }
        Ok(pane)
    }

    pub fn pane(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.lock().unwrap().get(&id).cloned()
    }

    pub fn host(&self) -> &dyn Host {
        &*self.host
    }

    pub fn panes(&self) -> Vec<Arc<Pane>> {
        self.panes.lock().unwrap().values().cloned().collect()
    }

    /// Kills the pane's process and forgets the pane. False if unknown.
    pub fn close(&self, id: PaneId) -> bool {
        let Some(pane) = self.panes.lock().unwrap().remove(&id) else { return false };
        if pane.is_alive() {
            let _ = pane.pty.lock().unwrap().kill();
        }
        self.host.pane_closed(id);
        true
    }
}

/// How often statuses are re-derived: agents going quiet produce no event.
const STATUS_INTERVAL: Duration = Duration::from_millis(250);

/// Keeps pane statuses current while `mux` lives.
fn spawn_monitor(mux: Weak<Mux>) {
    thread::Builder::new()
        .name("pane-status".into())
        .spawn(move || {
            while let Some(mux) = mux.upgrade() {
                for pane in mux.panes() {
                    if pane.update_status() {
                        mux.host.pane_status(pane.id);
                    }
                }
                drop(mux);
                thread::sleep(STATUS_INTERVAL);
            }
        })
        .expect("failed to spawn status thread");
}

/// `$SHELL`, else the account's shell (apps opened from Finder may not
/// get `$SHELL`), else sh.
fn default_shell() -> String {
    env::var("SHELL").ok().filter(|s| !s.is_empty()).or_else(account_shell).unwrap_or_else(|| "/bin/sh".into())
}

fn account_shell() -> Option<String> {
    use std::ffi::{CStr, c_char};
    #[repr(C)]
    struct Passwd {
        name: *const c_char,
        passwd: *const c_char,
        uid: u32,
        gid: u32,
        #[cfg(target_os = "macos")]
        change: i64,
        #[cfg(target_os = "macos")]
        class: *const c_char,
        gecos: *const c_char,
        dir: *const c_char,
        shell: *const c_char,
    }
    unsafe extern "C" {
        fn getuid() -> u32;
        fn getpwuid(uid: u32) -> *const Passwd;
    }
    // SAFETY: getpwuid returns null or a valid entry, read before any other call.
    unsafe {
        let entry = getpwuid(getuid());
        if entry.is_null() || (*entry).shell.is_null() {
            return None;
        }
        Some(CStr::from_ptr((*entry).shell).to_string_lossy().into_owned()).filter(|s| !s.is_empty())
    }
}

/// Reads and parses on one thread. macOS hands out at most 1 KiB per PTY
/// read, and the writer refills the kernel buffer while we parse, so a
/// separate parser thread only adds hand-off cost (measured with
/// `examples/pipeline.rs`); keeping the parser fast is what matters.
fn spawn_reader(pane: Arc<Pane>, mut reader: File, host: Arc<dyn Host>) {
    thread::Builder::new()
        .name(format!("pane-{}", pane.id))
        .spawn(move || {
            let mut buf = vec![0; 64 * 1024];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    // EIO once the child side closes (Linux).
                    Err(_) => break,
                };
                let (replies, bells, notifications) = {
                    let mut term = pane.term.lock().unwrap();
                    term.feed(&buf[..n]);
                    (term.take_responses(), term.bells(), term.take_notifications())
                };
                if !replies.is_empty() {
                    let _ = pane.write(&replies);
                }
                pane.notify();
                host.pane_output(pane.id);
                let focused = pane.focused.load(Ordering::Relaxed);
                if pane.activity.lock().unwrap().output(Instant::now(), bells, notifications, focused) {
                    host.pane_status(pane.id);
                }
            }
            let code = pane.pty.lock().unwrap().wait().ok().and_then(|status| status.code());
            *pane.exit.lock().unwrap() = Some(code);
            pane.notify();
            if pane.update_status() {
                host.pane_status(pane.id);
            }
            host.pane_exited(pane.id);
        })
        .expect("failed to spawn pane reader thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_shell_exists() {
        let shell = account_shell().expect("an account shell");
        assert!(Path::new(&shell).exists(), "{shell}");
    }

    #[test]
    fn placements_by_name() {
        assert_eq!(Placement::parse("window", Some(3)), Some(Placement::Window));
        assert_eq!(Placement::parse("tab", Some(3)), Some(Placement::Tab { beside: Some(3) }));
        assert_eq!(Placement::parse("right", None), Some(Placement::Split { beside: None, down: false }));
        assert_eq!(Placement::parse("down", Some(1)), Some(Placement::Split { beside: Some(1), down: true }));
        assert_eq!(Placement::parse("left", None), None);
    }

    /// Without a UI, a visible pane isn't shown, wherever it was asked for.
    #[test]
    fn headless_spawns_stay_hidden() {
        let mux = Mux::new(Arc::new(Headless));
        let params = mightty_json::parse(r#"{"command":"sleep 5","placement":"right","beside":1}"#).unwrap();
        let pane = handle(&mux, "pane.spawn", &params).unwrap();
        assert_eq!(pane["visible"].as_bool(), Some(false));
        assert!(pane["window"].is_null());
        let bad = mightty_json::parse(r#"{"command":"true","placement":"sideways"}"#).unwrap();
        assert!(handle(&mux, "pane.spawn", &bad).unwrap_err().message.contains("unknown placement"));
        assert!(handle(&mux, "pane.focus", &pane_param(pane["id"].as_u64().unwrap())).is_err());
        mux.close(pane["id"].as_u64().unwrap());
    }

    fn pane_param(id: u64) -> mightty_json::Value {
        mightty_json::Value::object([("pane", id.into())])
    }
}
