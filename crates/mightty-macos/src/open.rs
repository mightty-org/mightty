//! Opening links the user Cmd-clicked. A link's visible text needn't match
//! its target (OSC 8), so what a click may do is limited by the target:
//! web and mail links open; local files open only if they are of a kind
//! known to be safe (documents, media, folders) and are shown in Finder
//! otherwise; other schemes (which can trigger actions in apps that
//! registered them) do nothing.

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
/// a path's real location (`None` if it doesn't exist), and `safe` whether
/// opening that is known not to run anything.
pub fn decide(uri: &str, host: &str, resolve: impl Fn(&Path) -> Option<PathBuf>, safe: impl Fn(&Path) -> bool) -> Action {
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
    if safe(&real) { Action::OpenFile(real) } else { Action::Reveal(real) }
}

/// Kinds of file whose default app runs, installs or launches something,
/// or follows a link inside it; never opened, whatever their type says.
const RUNS: &[&str] = &[
    "app", "command", "tool", "terminal", "workflow", "action", "pkg", "mpkg", "prefpane", "saver", "jar", "webloc",
    "inetloc", "fileloc", "url", "shortcut", "scpt", "applescript", "osax", "kext", "dmg",
];

/// Uniform type identifiers a file may conform to, to be opened.
const SAFE_TYPES: &[&str] = &["public.plain-text", "public.json", "public.image", "public.audiovisual-content", "com.adobe.pdf"];
/// ...and may not (scripts are text; property lists, like `.terminal`
/// settings, can carry commands).
const UNSAFE_TYPES: &[&str] = &["public.script", "public.executable", "com.apple.property-list", "com.apple.package"];

/// Whether opening `path` (a real path, from `resolve`) is known not to
/// run anything: a plain folder, or a non-executable file of a type in
/// `SAFE_TYPES` and none in `UNSAFE_TYPES`. `conforms` asks macOS whether
/// the file's type conforms to an identifier. Anything unknown is unsafe.
pub fn safe_with(path: &Path, conforms: impl Fn(&str) -> bool) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if path.extension().is_some_and(|e| RUNS.iter().any(|x| e.eq_ignore_ascii_case(x))) || UNSAFE_TYPES.iter().any(|t| conforms(t)) {
        return false;
    }
    if meta.is_dir() {
        return conforms("public.folder");
    }
    meta.is_file() && meta.permissions().mode() & 0o111 == 0 && SAFE_TYPES.iter().any(|t| conforms(t))
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
        let safe = |path: &Path| safe_with(path, |id| conforms(path, id));
        let url: Id = match decide(uri, &host, resolve, safe) {
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

/// Whether the file at `path` has a type conforming to `identifier`.
fn conforms(path: &Path, identifier: &str) -> bool {
    // SAFETY: Foundation and UniformTypeIdentifiers calls with documented signatures.
    unsafe {
        let url: Id = msg!(class(c"NSURL"), c"fileURLWithPath:", nsstring(&path.to_string_lossy()));
        let mut kind: Id = NIL;
        let found: Bool = msg!(url, c"getResourceValue:forKey:error:", &mut kind as *mut Id, NSURLContentTypeKey, NIL);
        let wanted: Id = msg!(class(c"UTType"), c"typeWithIdentifier:", nsstring(identifier));
        if found == NO || kind.is_null() || wanted.is_null() {
            return false;
        }
        let yes: Bool = msg!(kind, c"conformsToType:", wanted);
        yes != NO
    }
}

#[link(name = "UniformTypeIdentifiers", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {
    static NSURLContentTypeKey: Id;
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
        let always = |_: &Path| true;
        let d = |uri| decide(uri, "mac", exists, always);
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
        assert_eq!(decide("file:///nope", "mac", |_| None, always), Action::Refuse);
        // Anything not known safe is shown instead, by its real path.
        let real = |_: &Path| Some(PathBuf::from("/Applications/Calculator.app"));
        assert_eq!(decide("file:///tmp/innocent", "mac", real, |_| false), Action::Reveal("/Applications/Calculator.app".into()));
    }

    /// With the real type checks: only known-safe kinds open.
    #[test]
    fn only_safe_kinds_open() {
        let dir = std::env::temp_dir().join(format!("mightty-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, mode: u32| {
            let path = dir.join(name);
            std::fs::write(&path, "x\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            resolve(&path).unwrap()
        };
        let safe = |path: &Path| safe_with(path, |id| conforms(path, id));
        assert!(safe(&file("notes.txt", 0o644)));
        assert!(safe(&file("data.json", 0o644)));
        assert!(safe(&resolve(&dir).unwrap()));
        // Scripts are text but run in their default apps (Python Launcher, Terminal).
        assert!(!safe(&file("tool.py", 0o644)));
        assert!(!safe(&file("run.sh", 0o644)));
        assert!(!safe(&file("x.command", 0o644)));
        assert!(!safe(&file("x.terminal", 0o644)));
        assert!(!safe(&file("x.WEBLOC", 0o644)));
        assert!(!safe(&file("x.plist", 0o644)));
        // Executable, or of no known kind.
        assert!(!safe(&file("notes2.txt", 0o755)));
        assert!(!safe(&file("blob.unknownext", 0o644)));
        assert!(!safe(&resolve(Path::new("/bin/sh")).unwrap()));
        assert!(!safe(Path::new("/no/such/file")));
        if let Some(app) = resolve(Path::new("/System/Applications/Calculator.app")) {
            assert!(!safe(&app));
        }
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
        let action = decide(&uri, "mac", resolve, |p| safe_with(p, |id| conforms(p, id)));
        assert_eq!(action, Action::Reveal(resolve(Path::new("/bin/sh")).unwrap()));
        let _ = std::fs::remove_dir_all(dir);
    }
}
