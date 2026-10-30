//! JSON-RPC 2.0 over a Unix socket, one JSON message per line.
//!
//! The socket lives in a per-user directory with mode 0700, so only the
//! user who started mightty can connect.

use std::env;
use std::fs::{self, DirBuilder};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{process, thread};

use mightty_json::{self as json, Value};

use crate::{Mux, rpc};

unsafe extern "C" {
    fn getuid() -> u32;
}

/// Per-user directory for sockets and shell-integration files.
pub fn runtime_dir() -> PathBuf {
    let base = env::var_os("XDG_RUNTIME_DIR").or_else(|| env::var_os("TMPDIR")).unwrap_or_else(|| "/tmp".into());
    // SAFETY: getuid cannot fail.
    PathBuf::from(base).join(format!("mightty-{}", unsafe { getuid() }))
}

fn private_dir() -> io::Result<PathBuf> {
    let dir = runtime_dir();
    DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
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

fn serve(mux: &Arc<Mux>, stream: UnixStream) {
    let Ok(read_half) = stream.try_clone() else { return };
    let mut writer = stream;
    for line in BufReader::new(read_half).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = respond(mux, &line)
            && writeln!(writer, "{reply}").is_err()
        {
            return;
        }
    }
}

/// Handles one JSON-RPC message. Returns `None` for notifications.
pub fn respond(mux: &Arc<Mux>, line: &str) -> Option<Value> {
    let request = match json::parse(line) {
        Ok(request) => request,
        Err(e) => return Some(error_reply(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    let id = request.get("id").cloned();
    let Some(method) = request["method"].as_str() else {
        return Some(error_reply(id.unwrap_or_default(), -32600, "invalid request"));
    };
    let result = rpc::handle(mux, method, &request["params"]);
    let id = id?;
    Some(match result {
        Ok(result) => Value::object([("jsonrpc", "2.0".into()), ("id", id), ("result", result)]),
        Err(e) => error_reply(id, e.code, &e.message),
    })
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
}

impl Client {
    pub fn connect(path: &Path) -> io::Result<Self> {
        let writer = UnixStream::connect(path)?;
        Ok(Self { reader: BufReader::new(writer.try_clone()?), writer, next_id: 1 })
    }

    /// `$MIGHTTY_SOCKET` if set (inside a mightty pane), otherwise the most
    /// recently started mightty that still answers. Stale sockets are removed.
    pub fn discover() -> io::Result<Self> {
        if let Some(path) = env::var_os("MIGHTTY_SOCKET") {
            return Self::connect(Path::new(&path));
        }
        let mut sockets: Vec<_> = fs::read_dir(runtime_dir())?
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
        let mut line = String::new();
        if self.reader.read_line(&mut line).map_err(disconnected)? == 0 {
            return Err(CallError::Disconnected("mightty closed the connection".into()));
        }
        let reply = json::parse(&line).map_err(|e| CallError::Rpc(format!("invalid reply: {e}")))?;
        match reply.get("error") {
            Some(error) => Err(CallError::Rpc(error["message"].as_str().unwrap_or("unknown error").to_owned())),
            None => Ok(reply["result"].clone()),
        }
    }
}
