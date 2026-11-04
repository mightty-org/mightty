//! JSON-RPC 2.0 over a Unix socket, one JSON message per line.
//!
//! The socket lives in a per-user directory with mode 0700, so only the
//! user who started mightty can connect.

use std::env;
use std::fs::{self, DirBuilder};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::{process, thread};

use mightty_json::{self as json, Value};

use crate::{EVENTS, Mux, RpcError, rpc};

unsafe extern "C" {
    fn getuid() -> u32;
}

/// Per-user directory for sockets and shell-integration files.
pub fn runtime_dir() -> PathBuf {
    let base = env::var_os("XDG_RUNTIME_DIR").or_else(|| env::var_os("TMPDIR")).unwrap_or_else(|| "/tmp".into());
    // SAFETY: getuid cannot fail.
    PathBuf::from(base).join(format!("mightty-{}", unsafe { getuid() }))
}

/// [`runtime_dir`], created if missing, once it checks out as a real
/// directory (not a symlink) owned by this user. With a shared base like
/// `/tmp`, someone else could create it first and plant files or sockets.
pub(crate) fn private_dir() -> io::Result<PathBuf> {
    let dir = runtime_dir();
    ensure_private(&dir)?;
    Ok(dir)
}

fn ensure_private(dir: &Path) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let meta = fs::symlink_metadata(dir)?;
    // SAFETY: getuid cannot fail.
    if !meta.is_dir() || meta.uid() != unsafe { getuid() } {
        let message = format!("{} is not a directory owned by you; refusing to use it", dir.display());
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Binds `<runtime_dir>/<pid>.sock` and serves `mux` on it from background
/// threads. Panes spawned afterwards get the path in `MIGHTTY_SOCKET`.
pub fn listen(mux: Arc<Mux>) -> io::Result<PathBuf> {
    let path = private_dir()?.join(format!("{}.sock", process::id()));
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let _ = mux.socket.set(path.clone());
    thread::Builder::new().name("control-socket".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let mux = mux.clone();
            let _ = thread::Builder::new().name("control-client".into()).spawn(move || serve(&mux, stream));
        }
    })?;
    Ok(path)
}

/// Numbers connections, for event subscriptions.
static CONNECTIONS: AtomicU64 = AtomicU64::new(1);

/// Replies and events go out through one writer thread, so a slow client
/// holds up neither its own requests nor the panes emitting events.
fn serve(mux: &Arc<Mux>, stream: UnixStream) {
    let Ok(read_half) = stream.try_clone() else { return };
    let connection = CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    let (to, outbox) = mpsc::channel::<Value>();
    let mut writer = stream;
    let sent = thread::Builder::new().name("control-writer".into()).spawn(move || {
        for message in outbox {
            if writeln!(writer, "{message}").is_err() {
                let _ = writer.shutdown(std::net::Shutdown::Both);
                return;
            }
        }
    });
    if sent.is_err() {
        return;
    }
    for line in BufReader::new(read_half).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = respond(mux, &line, connection, &to)
            && to.send(reply).is_err()
        {
            break;
        }
    }
    mux.events().unsubscribe(connection);
}

/// Handles one JSON-RPC message from `connection`, whose events go `to`.
/// Returns `None` for notifications.
fn respond(mux: &Arc<Mux>, line: &str, connection: u64, to: &Sender<Value>) -> Option<Value> {
    let request = match json::parse(line) {
        Ok(request) => request,
        Err(e) => return Some(error_reply(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let id = request.get("id").cloned();
    let Some(method) = request["method"].as_str() else {
        return Some(error_reply(id.unwrap_or_default(), -32600, "invalid request"));
    };
    let params = &request["params"];
    let result = match method {
        "events.subscribe" => subscribe(mux, connection, to, params),
        "events.unsubscribe" => {
            mux.events().unsubscribe(connection);
            Ok(Value::Null)
        }
        _ => rpc::handle(mux, method, params),
    };
    let id = id?;
    Some(match result {
        Ok(result) => Value::object([("jsonrpc", "2.0".into()), ("id", id), ("result", result)]),
        Err(e) => error_reply(id, e.code, &e.message),
    })
}

/// `events`: names, `prefix.*` or `*`; all of them when missing.
fn subscribe(mux: &Mux, connection: u64, to: &Sender<Value>, params: &Value) -> Result<Value, RpcError> {
    let patterns = match params.get("events") {
        None | Some(Value::Null) => vec!["*".to_owned()],
        Some(events) => events
            .as_array()
            .and_then(|events| events.iter().map(|e| e.as_str().map(str::to_owned)).collect())
            .ok_or_else(|| RpcError { code: -32602, message: "`events` must be an array of event names".into() })?,
    };
    mux.events().subscribe(connection, patterns, to.clone()).map_err(|message| RpcError { code: -32602, message })?;
    Ok(Value::object([("events", Value::Array(EVENTS.iter().map(|&e| e.into()).collect()))]))
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    let error = Value::object([("code", code.into()), ("message", message.into())]);
    Value::object([("jsonrpc", "2.0".into()), ("id", id), ("error", error)])
}

/// Why a [`Client::call`] failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The connection broke, typically because that mightty quit.
    Disconnected(String),
    /// mightty answered with an error.
    Rpc(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Disconnected(m) | CallError::Rpc(m) => f.write_str(m),
        }
    }
}

impl From<CallError> for String {
    fn from(e: CallError) -> Self {
        e.to_string()
    }
}

/// A connection to a running mightty.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// Events that arrived while waiting for a reply.
    events: VecDeque<Value>,
}

impl Client {
    pub fn connect(path: &Path) -> io::Result<Self> {
        Self::from_stream(UnixStream::connect(path)?)
    }

    fn from_stream(writer: UnixStream) -> io::Result<Self> {
        Ok(Self { reader: BufReader::new(writer.try_clone()?), writer, next_id: 1, events: VecDeque::new() })
    }

    /// `$MIGHTTY_SOCKET` if set (inside a mightty pane), otherwise the most
    /// recently started mightty that still answers. Stale sockets are removed.
    pub fn discover() -> io::Result<Self> {
        if let Some(path) = env::var_os("MIGHTTY_SOCKET") {
            return Self::connect(Path::new(&path));
        }
        let mut sockets: Vec<_> = fs::read_dir(private_dir()?)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "sock"))
            .filter_map(|p| Some((fs::metadata(&p).ok()?.modified().ok()?, p)))
            .collect();
        sockets.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, path) in sockets {
            match Self::connect(&path) {
                Ok(client) => return Ok(client),
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                    let _ = fs::remove_file(&path);
                }
                Err(_) => {}
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "no running mightty found"))
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Value::object([
            ("jsonrpc", "2.0".into()),
            ("id", id.into()),
            ("method", method.into()),
            ("params", params),
        ]);
        let disconnected = |e: io::Error| CallError::Disconnected(e.to_string());
        writeln!(self.writer, "{request}").map_err(disconnected)?;
        let reply = loop {
            let message = self.read()?;
            if message.get("id").is_some() {
                break message;
            }
            self.events.push_back(message);
        };
        match reply.get("error") {
            Some(error) => Err(CallError::Rpc(error["message"].as_str().unwrap_or("unknown error").to_owned())),
            None => Ok(reply["result"].clone()),
        }
    }

    /// The next event after `events.subscribe`, as `(event, params)`; blocks until one comes.
    pub fn next_event(&mut self) -> Result<(String, Value), CallError> {
        let event = match self.events.pop_front() {
            Some(event) => event,
            None => self.read()?,
        };
        Ok((event["method"].as_str().unwrap_or_default().to_owned(), event["params"].clone()))
    }

    fn read(&mut self) -> Result<Value, CallError> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).map_err(|e| CallError::Disconnected(e.to_string()))? == 0 {
            return Err(CallError::Disconnected("mightty closed the connection".into()));
        }
        json::parse(&line).map_err(|e| CallError::Rpc(format!("invalid message: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Headless, SpawnOptions};
    use std::time::{Duration, Instant};

    #[test]
    fn events_reach_subscribers() {
        let mux = Mux::new(Arc::new(Headless));
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let served = mux.clone();
        thread::spawn(move || serve(&served, theirs));
        let mut client = Client::from_stream(ours).unwrap();

        let unknown = Value::object([("events", vec!["pane.nope".into()].into())]);
        assert!(matches!(client.call("events.subscribe", unknown), Err(CallError::Rpc(m)) if m.contains("pane.nope")));
        let events = Value::object([("events", vec!["pane.*".into()].into())]);
        client.call("events.subscribe", events).unwrap();

        let command = r"printf '\033]2;hello\007'; exit 3";
        let pane = mux.spawn(SpawnOptions { command: Some(command.into()), ..Default::default() }).unwrap();
        let mut seen = Vec::new();
        loop {
            let (event, params) = client.next_event().unwrap();
            assert_eq!(params["pane"].as_u64(), Some(pane.id), "{event} {params}");
            match event.as_str() {
                "pane.title" => assert_eq!(params["title"].as_str(), Some("hello")),
                "pane.exited" => assert_eq!(params["exit_code"].as_i64(), Some(3)),
                _ => {}
            }
            let done = event == "pane.exited";
            seen.push(event);
            if done {
                break;
            }
        }
        seen.retain(|e| e != "pane.status" && e != "pane.cwd");
        assert_eq!(seen, ["pane.opened", "pane.title", "pane.exited"]);
        // Replies still come while events do.
        assert_eq!(client.call("pane.list", Value::Null).unwrap().as_array().map(<[_]>::len), Some(1));

        // Closing the connection unsubscribes it.
        assert!(mux.events().active());
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(5);
        while mux.events().active() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!mux.events().active());
    }

    #[test]
    fn private_dir_is_checked() {
        let base = env::temp_dir().join(format!("mightty-test-{}", process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();

        let fresh = base.join("fresh");
        ensure_private(&fresh).unwrap();
        assert_eq!(fs::metadata(&fresh).unwrap().mode() & 0o777, 0o700);

        let loose = base.join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o777)).unwrap();
        ensure_private(&loose).unwrap();
        assert_eq!(fs::metadata(&loose).unwrap().mode() & 0o777, 0o700);

        let link = base.join("link");
        std::os::unix::fs::symlink(&fresh, &link).unwrap();
        assert_eq!(ensure_private(&link).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        let file = base.join("file");
        fs::write(&file, "").unwrap();
        assert!(ensure_private(&file).is_err());

        fs::remove_dir_all(&base).unwrap();
    }
}
