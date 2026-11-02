//! Pane multiplexer. Owns every pane (a PTY plus its terminal state)
//! independently of any window and serves them over a JSON-RPC control
//! socket. The GUI is one client of it; `mightty ctl` and the MCP server are
//! others.

mod keys;
mod rpc;
mod shell;
mod socket;

use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use mightty_pty::{Pty, WindowSize};
use mightty_term::{Term, TermOptions};

pub use keys::key_bytes;
pub use rpc::{RpcError, handle};
pub use socket::{CallError, Client, listen, runtime_dir};

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
    /// Shows the pane in a window. Returns false if the host has no UI.
    fn show_pane(&self, _id: PaneId) -> bool {
        false
    }
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
    /// Ask the host to show the pane in a window.
    pub visible: bool,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self { command: None, shell: None, cwd: None, env: Vec::new(), cols: 100, rows: 30, visible: false }
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
        Arc::new(Self { panes: Mutex::default(), next_id: AtomicU64::new(1), host, socket: OnceLock::new(), options })
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
        let nonce = shell::integrate(&mut cmd, &shell);

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
        });
        self.panes.lock().unwrap().insert(id, pane.clone());
        spawn_reader(pane.clone(), reader, self.host.clone());
        if opts.visible {
            pane.set_visible(self.host.show_pane(id));
        }
        Ok(pane)
    }

    pub fn pane(&self, id: PaneId) -> Option<Arc<Pane>> {
        self.panes.lock().unwrap().get(&id).cloned()
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

fn default_shell() -> String {
    env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
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
                let replies = {
                    let mut term = pane.term.lock().unwrap();
                    term.feed(&buf[..n]);
                    term.take_responses()
                };
                if !replies.is_empty() {
                    let _ = pane.write(&replies);
                }
                pane.notify();
                host.pane_output(pane.id);
            }
            let code = pane.pty.lock().unwrap().wait().ok().and_then(|status| status.code());
            *pane.exit.lock().unwrap() = Some(code);
            pane.notify();
            host.pane_exited(pane.id);
        })
        .expect("failed to spawn pane reader thread");
}
