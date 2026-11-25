//! Mightty as the terminal other apps open things with: what a file or
//! link opened from Finder, a browser or `open -a` runs, and making Mightty
//! the default app for them.
//!
//! An `ssh://` link can come from any web page, so only a plain host, user
//! and port go through: never anything ssh would read as an option.

use std::path::{Path, PathBuf};

use crate::ffi::*;

/// What opening a file or link does: a new window running `command`
/// (through the login shell) in `cwd`, or a shell there when `command` is
/// `None`.
#[derive(Debug, PartialEq, Eq)]
pub struct Opened {
    pub command: Option<String>,
    pub cwd: Option<PathBuf>,
    /// The window's title.
    pub title: String,
    /// Kept open when it ends, whatever `close-on-exit` says: a script's
    /// last lines stay readable.
    pub hold: bool,
}

/// The file types Mightty runs, and the link scheme: what "Make Mightty the
/// Default Terminal" claims. Scripts ending in `.sh` stay with the editor
/// they open in; Mightty is only offered for them in Open With.
const DEFAULT_TYPES: &[&str] = &["com.apple.terminal.shell-script", "public.unix-executable"];
const DEFAULT_SCHEME: &str = "ssh";

/// What opening `path` does: a folder gets a shell in it; a program or
/// script is run in its folder. Anything else isn't Mightty's to open.
/// `is_dir` and `is_file` say what `path` is.
pub fn for_file(path: &Path, is_dir: bool, is_file: bool) -> Result<Opened, String> {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    if is_dir {
        return Ok(Opened {
            command: None,
            cwd: Some(path.to_path_buf()),
            title: name,
            hold: false,
        });
    }
    if !is_file {
        return Err(format!("{}: not a file or folder", path.display()));
    }
    let text = path
        .to_str()
        .ok_or_else(|| format!("{}: the path isn't UTF-8", path.display()))?;
    // The login shell may be fish, where a backslash escapes even inside
    // single quotes: a name with one could end the quoting. Control
    // characters have no business in a script's path either.
    if text.contains('\\') || text.chars().any(char::is_control) {
        return Err(format!(
            "{}: its path has a backslash or control character",
            path.display()
        ));
    }
    Ok(Opened {
        // The pane ends with the script and shows how it did; one that
        // isn't executable says so there.
        command: Some(shell_quote(text)),
        cwd: path.parent().map(Path::to_path_buf),
        title: name,
        hold: true,
    })
}

/// What opening `url`, an `ssh://[user@]host[:port]` link, does: `ssh` to
/// it. `None` for anything else, or with characters a host name or user
/// can't have.
pub fn for_ssh(url: &str) -> Option<Opened> {
    let rest = url
        .get(..6)
        .filter(|scheme| scheme.eq_ignore_ascii_case("ssh://"))
        .map(|_| &url[6..])?;
    // A trailing slash is allowed; a path isn't.
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (user, server) = match rest.rsplit_once('@') {
        Some((user, server)) => (Some(user), server),
        None => (None, rest),
    };
    // `[::1]:22` or `host:22`.
    let (host, port) = if let Some(inner) = server.strip_prefix('[') {
        let (host, after) = inner.split_once(']')?;
        if !host
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
            || host.is_empty()
        {
            return None;
        }
        // Only a port may follow.
        if !after.is_empty() && !after.starts_with(':') {
            return None;
        }
        (host, after.strip_prefix(':'))
    } else {
        match server.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (server, None),
        }
    };
    let plain = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    if !server.starts_with('[') && !plain(host) {
        return None;
    }
    if user.is_some_and(|u| !plain(u)) {
        return None;
    }
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok().filter(|&p| p > 0)?),
        None => None,
    };
    let destination = match user {
        Some(user) => format!("{user}@{host}"),
        None => host.to_owned(),
    };
    let mut command = String::from("ssh");
    if let Some(port) = port {
        command += &format!(" -p {port}");
    }
    // `--`: whatever follows is the destination, never an option.
    command += &format!(" -- {}", shell_quote(&destination));
    Some(Opened {
        command: Some(command),
        cwd: None,
        title: destination,
        hold: false,
    })
}

/// `text` as one word for the shell: as it is when that's safe, so the
/// exit bar shows it plainly, and quoted otherwise.
/// Whether `path` came from the Internet: macOS marks such files.
pub fn downloaded(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: two C strings; with no buffer, only the size is asked for.
    let size = unsafe {
        getxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            0,
        )
    };
    size >= 0
}

fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ',' | '@' | ':')
        });
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// Makes Mightty the app that runs `.command` files and programs opened
/// from Finder, and that opens `ssh://` links; macOS asks the user to
/// confirm the links. `done` is then called on the main thread with what
/// failed, if anything.
pub fn make_default(done: fn(Vec<String>)) {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Told what failed.
    type Done = fn(Vec<String>);
    static DONE: Mutex<Option<Done>> = Mutex::new(None);
    static FAILED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static LEFT: AtomicUsize = AtomicUsize::new(0);
    /// From the request until its answer has been reported.
    static BUSY: AtomicBool = AtomicBool::new(false);
    static FINISHED: Block = Block::new(finished);

    /// Each request's completion handler, on a queue of AppKit's.
    unsafe extern "C" fn finished(_: *const Block, error: Id) {
        if !error.is_null() {
            // SAFETY: an NSError; documented signatures.
            let text = unsafe { string_from_ns(msg!(error, c"localizedDescription")) };
            FAILED.lock().unwrap().push(text);
        }
        if LEFT.fetch_sub(1, Ordering::AcqRel) == 1 {
            run_on_main(report);
        }
    }
    extern "C" fn report(_: *mut std::ffi::c_void) {
        let failed = std::mem::take(&mut *FAILED.lock().unwrap());
        let done = DONE.lock().unwrap().take();
        BUSY.store(false, Ordering::Release);
        if let Some(done) = done {
            done(failed);
        }
    }

    // One request at a time.
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    *DONE.lock().unwrap() = Some(done);
    // SAFETY: main thread; documented signatures (macOS 12). The block is
    // global and captures nothing.
    unsafe {
        let workspace: Id = msg!(class(c"NSWorkspace"), c"sharedWorkspace");
        let bundle: Id = msg!(class(c"NSBundle"), c"mainBundle");
        let app: Id = msg!(bundle, c"bundleURL");
        let types: Vec<Id> = DEFAULT_TYPES
            .iter()
            .map(|id| msg!(class(c"UTType"), c"typeWithIdentifier:", nsstring(id)))
            .filter(|t: &Id| !t.is_null())
            .collect();
        LEFT.store(types.len() + 1, Ordering::Release);
        for kind in types {
            call!(
                workspace,
                c"setDefaultApplicationAtURL:toOpenContentType:completionHandler:",
                app,
                kind,
                &FINISHED
            );
        }
        call!(
            workspace,
            c"setDefaultApplicationAtURL:toOpenURLsWithScheme:completionHandler:",
            app,
            nsstring(DEFAULT_SCHEME),
            &FINISHED
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_get_a_shell_and_files_run() {
        let dir = for_file(Path::new("/Users/me/src"), true, false).unwrap();
        assert_eq!(dir.command, None);
        assert_eq!(dir.cwd, Some(PathBuf::from("/Users/me/src")));
        assert_eq!(dir.title, "src");

        let script = for_file(Path::new("/Users/me/it's here.command"), false, true).unwrap();
        assert_eq!(
            script.command.as_deref(),
            Some(r"'/Users/me/it'\''s here.command'")
        );
        let plain = for_file(Path::new("/Users/me/build.command"), false, true).unwrap();
        assert!(for_file(Path::new("/dev/null"), false, false).is_err());
        // Would end the quoting in fish.
        assert!(for_file(Path::new("/tmp/x\\';touch pwned;#"), false, true).is_err());
        assert!(for_file(Path::new("/tmp/a\nb"), false, true).is_err());
        assert_eq!(plain.command.as_deref(), Some("/Users/me/build.command"));
        assert_eq!(script.cwd, Some(PathBuf::from("/Users/me")));
        assert!(script.hold);
    }

    #[test]
    fn ssh_links() {
        let run = |url| for_ssh(url).and_then(|o| o.command);
        assert_eq!(
            run("ssh://example.com").as_deref(),
            Some("ssh -- example.com")
        );
        assert_eq!(
            run("ssh://me@example.com:2222/").as_deref(),
            Some("ssh -p 2222 -- me@example.com")
        );
        assert_eq!(run("SSH://Host").as_deref(), Some("ssh -- Host"));
        assert_eq!(
            run("ssh://root@[fe80::1]:22").as_deref(),
            Some("ssh -p 22 -- root@fe80::1")
        );
        // Options, shell characters, paths and bad ports don't get through.
        for bad in [
            "ssh://-oProxyCommand=touch%20x",
            "ssh://-oProxyCommand=x@host",
            "ssh://host;rm",
            "ssh://host/path",
            "ssh://me@host:0",
            "ssh://me@host:99999",
            "ssh://me@host:",
            "ssh://$(id)@host",
            "ssh://",
            "ssh://[]",
            "ssh://[-x]",
            "telnet://host",
            "ssh://a@b@c",
            "ssh://[::1]junk",
            "ssh://[::1]/x",
        ] {
            assert_eq!(for_ssh(bad), None, "{bad}");
        }
    }
}
