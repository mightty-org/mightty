//! Opening links the user Cmd-clicked. A link's visible text needn't match
//! its target (OSC 8), so what a click may do is limited by the target:
//! web and mail links open, local files open unless they would run
//! something, which they are shown in Finder instead, and other schemes
//! (which can trigger actions in apps that registered them) do nothing.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::ffi::*;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Open(String),
    /// A local file, by its real path (links resolved): what was checked is
    /// what gets opened.
    OpenFile(PathBuf),
    /// Select it in Finder instead of opening it.
    Reveal(PathBuf),
    Refuse,
}

const OPEN_SCHEMES: &[&str] = &["http", "https", "mailto", "ftp", "sftp"];

/// What a click on a link to `uri` does. `host` is this machine's name, for
/// `file://host/path` links (`ls --hyperlink` writes those); `resolve` gives
/// a path's real location (`None` if it doesn't exist), and `runs` whether
/// opening that would run a program.
pub fn decide(uri: &str, host: &str, resolve: impl Fn(&Path) -> Option<PathBuf>, runs: impl Fn(&Path) -> bool) -> Action {
    let Some((scheme, rest)) = uri.split_once(':') else { return Action::Refuse };
    let scheme = scheme.to_ascii_lowercase();
    if OPEN_SCHEMES.contains(&scheme.as_str()) {
        return Action::Open(uri.to_owned());
    }
    if scheme != "file" {
        return Action::Refuse;
    }
    // A query or fragment would make the path checked differ from the one opened.
    let Some(rest) = rest.strip_prefix("//").filter(|r| !r.contains(['?', '#'])) else { return Action::Refuse };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let local = authority.is_empty() || authority.eq_ignore_ascii_case("localhost") || authority.eq_ignore_ascii_case(host);
    let Some(path) = percent_decode(path).filter(|p| local && p.starts_with('/')) else { return Action::Refuse };
    let Some(real) = resolve(Path::new(&path)) else { return Action::Refuse };
    if runs(&real) { Action::Reveal(real) } else { Action::OpenFile(real) }
}

/// Kinds of file whose default app runs, installs or launches something,
/// or follows a link inside it (`.webloc` and friends, which could reach
/// schemes `decide` refuses).
const RUNS: &[&str] = &[
    "app", "command", "tool", "terminal", "workflow", "action", "pkg", "mpkg", "prefpane", "saver", "jar", "webloc",
    "inetloc", "fileloc", "url", "shortcut", "scpt", "applescript", "osax", "kext", "dmg",
];

/// Whether opening `path` (a real path, from `resolve`) would launch a
/// program: a kind in `RUNS`, a package (`package` asks AppKit), or an
/// executable file. Unreadable counts as running.
pub fn runs_with(path: &Path, package: impl Fn(&Path) -> bool) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return true };
    let kind = path.extension().is_some_and(|e| RUNS.iter().any(|x| e.eq_ignore_ascii_case(x)));
    kind || (meta.is_dir() && package(path)) || (meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

pub fn resolve(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Carries out `decide` for `uri`; refused links beep.
pub fn open(uri: &str) {
    let host = hostname();
    // SAFETY: main thread; documented signatures.
    unsafe {
        let workspace: Id = msg!(class(c"NSWorkspace"), c"sharedWorkspace");
        let package = |path: &Path| {
            let yes: Bool = msg!(workspace, c"isFilePackageAtPath:", nsstring(&path.to_string_lossy()));
            yes != NO
        };
        let url: Id = match decide(uri, &host, resolve, |p| runs_with(p, package)) {
            Action::Open(uri) => msg!(class(c"NSURL"), c"URLWithString:", nsstring(&uri)),
            Action::OpenFile(path) => msg!(class(c"NSURL"), c"fileURLWithPath:", nsstring(&path.to_string_lossy())),
            Action::Reveal(path) => {
                let url: Id = msg!(class(c"NSURL"), c"fileURLWithPath:", nsstring(&path.to_string_lossy()));
                let urls: Id = msg!(class(c"NSArray"), c"arrayWithObject:", url);
                call!(workspace, c"activateFileViewerSelectingURLs:", urls);
                return;
            }
            Action::Refuse => NIL,
        };
        let opened: Bool = if url.is_null() { NO } else { msg!(workspace, c"openURL:", url) };
        if opened == NO {
            NSBeep();
        }
    }
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its length.
    let ok = unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let len = buf.iter().position(|&b| b == 0).unwrap_or(0);
    if ok { String::from_utf8_lossy(&buf[..len]).into_owned() } else { String::new() }
}

unsafe extern "C" {
    fn gethostname(name: *mut std::ffi::c_char, len: usize) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decides_by_target() {
        let exists = |p: &Path| Some(p.to_path_buf());
        let never = |_: &Path| false;
        let d = |uri| decide(uri, "mac", exists, never);
        assert_eq!(d("https://example.com/a"), Action::Open("https://example.com/a".into()));
        assert_eq!(d("MAILTO:me@x.org"), Action::Open("MAILTO:me@x.org".into()));
        // Schemes apps register can do things: not followed.
        assert_eq!(d("x-apple.systempreferences:com.apple"), Action::Refuse);
        assert_eq!(d("javascript:alert(1)"), Action::Refuse);
        assert_eq!(d("no scheme"), Action::Refuse);
        // Local files open by their decoded path, from this host only.
        assert_eq!(d("file:///tmp/a%20b.txt"), Action::OpenFile("/tmp/a b.txt".into()));
        assert_eq!(d("file://mac/tmp/x"), Action::OpenFile("/tmp/x".into()));
        assert_eq!(d("file://other/tmp/x"), Action::Refuse);
        // A query or fragment could make what's checked differ from what opens.
        assert_eq!(d("file:///Applications/Calculator.app?x"), Action::Refuse);
        assert_eq!(d("file:///Applications/Calculator.app#x"), Action::Refuse);
        // Missing files are refused rather than opened unchecked.
        assert_eq!(decide("file:///nope", "mac", |_| None, never), Action::Refuse);
        // A file that would run something is shown instead, by its real path.
        let app = |p: &Path| p.extension().is_some_and(|e| e == "app");
        let real = |_: &Path| Some(PathBuf::from("/Applications/Calculator.app"));
        assert_eq!(decide("file:///tmp/innocent", "mac", real, app), Action::Reveal("/Applications/Calculator.app".into()));
    }

    #[test]
    fn executables_and_packages_run() {
        let no_package = |_: &Path| false;
        assert!(runs_with(Path::new("/bin/sh"), no_package));
        assert!(!runs_with(Path::new("/etc/hosts"), no_package));
        assert!(!runs_with(Path::new("/tmp"), no_package));
        assert!(runs_with(Path::new("/tmp"), |_| true));
        // Unreadable fails closed.
        assert!(runs_with(Path::new("/no/such/file"), no_package));
        let dir = std::env::temp_dir().join(format!("mightty-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let webloc = dir.join("x.WEBLOC");
        std::fs::write(&webloc, "").unwrap();
        assert!(runs_with(&webloc, no_package));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn symlinks_resolve_to_their_target() {
        let dir = std::env::temp_dir().join(format!("mightty-open-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("innocent.txt");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/bin/sh", &link).unwrap();
        let uri = format!("file://{}", link.display());
        let action = decide(&uri, "mac", resolve, |p| runs_with(p, |_| false));
        assert_eq!(action, Action::Reveal(resolve(Path::new("/bin/sh")).unwrap()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
