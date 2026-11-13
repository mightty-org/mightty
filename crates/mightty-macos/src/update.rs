//! Looking for a newer version. GitHub is asked for the latest release of
//! the repository, on a thread of its own; nothing is downloaded or
//! installed. The answer is only believed as far as a version number: the
//! addresses offered are made here, not taken from it.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;

use crate::ffi::*;

/// The running version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The latest release, as GitHub's API describes it (public, no key needed).
const LATEST: &str = "https://api.github.com/repos/mightty-org/mightty/releases/latest";

/// How a check ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A release newer than this one: its version, as `1.2.3`.
    Newer(String),
    UpToDate,
    /// It couldn't be found out, and why.
    Failed(String),
}

/// What the last check found and whether the user asked for it, until the
/// main thread takes it.
static FOUND: Mutex<Option<(Outcome, bool)>> = Mutex::new(None);

/// Checks in the background and hands the outcome to `done` on the main
/// thread (see `take`). `manual` says the user asked, so wants an answer
/// either way.
pub fn check(manual: bool, done: extern "C" fn(*mut c_void)) {
    let spawned = thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            // `MIGHTTY_UPDATE_FROM=0.0.1` checks as that version would, to
            // see what a newer release looks like before there is one.
            let running = std::env::var("MIGHTTY_UPDATE_FROM");
            let outcome = match fetch(LATEST, 1 << 20) {
                Ok(body) => compare(&body, running.as_deref().unwrap_or(VERSION)),
                Err(why) => Outcome::Failed(why),
            };
            *FOUND.lock().unwrap() = Some((outcome, manual));
            run_on_main(done);
        });
    if spawned.is_err() {
        *FOUND.lock().unwrap() = Some((
            Outcome::Failed("Mightty couldn't start the check.".into()),
            manual,
        ));
        run_on_main(done);
    }
}

/// A newer version unpacked and checked beside the app, waiting to be
/// swapped in: its version, where it is, and the app it replaces.
pub type Staged = (String, PathBuf, PathBuf);

/// What the last `install` came to, until the main thread takes it.
static INSTALLED: Mutex<Option<Result<Staged, String>>> = Mutex::new(None);

/// Downloads version `version` in the background, unpacks it beside `app`
/// and checks it (see `install::stage`), then calls `done` on the main
/// thread (see `take_installed`). Nothing is replaced yet.
pub fn install(version: String, app: PathBuf, done: extern "C" fn(*mut c_void)) {
    let work = move || {
        let base = format!("https://github.com/mightty-org/mightty/releases/download/v{version}");
        // A release's archive is a few megabytes.
        let zip = fetch(&format!("{base}/Mightty.zip"), 256 << 20)?;
        let sums = fetch(&format!("{base}/SHA256SUMS"), 1 << 16)?;
        let sums =
            String::from_utf8(sums).map_err(|_| "The release's checksum file is unreadable.")?;
        let new = crate::install::stage(&version, &app, &zip, &sums)?;
        Ok((version, new, app))
    };
    let spawned = thread::Builder::new()
        .name("update-install".into())
        .spawn(move || {
            *INSTALLED.lock().unwrap() = Some(work());
            run_on_main(done);
        });
    if spawned.is_err() {
        *INSTALLED.lock().unwrap() = Some(Err("Mightty couldn't start the download.".into()));
        run_on_main(done);
    }
}

/// What `install` came to, once.
pub fn take_installed() -> Option<Result<Staged, String>> {
    INSTALLED.lock().unwrap().take()
}

/// The outcome `check` left, once.
pub fn take() -> Option<(Outcome, bool)> {
    FOUND.lock().unwrap().take()
}

/// Where version `version` is downloaded from, and where its notes are.
pub fn addresses(version: &str) -> (String, String) {
    let base = "https://github.com/mightty-org/mightty/releases";
    (
        format!("{base}/download/v{version}/Mightty.dmg"),
        format!("{base}/tag/v{version}"),
    )
}

/// What GitHub's answer `body` means for someone running `current`.
fn compare(body: &[u8], current: &str) -> Outcome {
    let tag = std::str::from_utf8(body)
        .ok()
        .and_then(|text| mightty_json::parse(text).ok())
        .and_then(|release| release["tag_name"].as_str().map(str::to_owned));
    let Some(latest) = tag.as_deref().and_then(version) else {
        return Outcome::Failed("GitHub's response didn't include a version.".into());
    };
    match version(current) {
        Some(running) if latest > running => {
            Outcome::Newer(format!("{}.{}.{}", latest.0, latest.1, latest.2))
        }
        _ => Outcome::UpToDate,
    }
}

/// `1.2.3` or `v1.2.3` as numbers. Nothing else is a version: a tag with
/// more in it (a pre-release, or anything odd) is not one to offer.
fn version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut number = || {
        let part = parts.next()?;
        (!part.is_empty() && part.len() <= 6 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let found = (number()?, number()?, number()?);
    parts.next().is_none().then_some(found)
}

/// The bytes at `address`, `limit` at most, read with the system's networking.
fn fetch(address: &str, limit: usize) -> Result<Vec<u8>, String> {
    // SAFETY: documented signatures; Foundation's URL loading may be used
    // from any thread, and the pool frees what it autoreleases here.
    unsafe {
        let pool = objc_autoreleasePoolPush();
        let url: Id = msg!(class(c"NSURL"), c"URLWithString:", nsstring(address));
        let mut error: Id = NIL;
        let data: Id = msg!(
            class(c"NSData"),
            c"dataWithContentsOfURL:options:error:",
            url,
            0usize,
            &mut error as *mut Id
        );
        let result = if data.is_null() {
            let why = if error.is_null() {
                "No response from the server.".to_owned()
            } else {
                string_from_ns(msg!(error, c"localizedDescription"))
            };
            Err(why)
        } else {
            let (bytes, len): (*const u8, usize) = (msg!(data, c"bytes"), msg!(data, c"length"));
            if bytes.is_null() || len > limit {
                Err("The download was larger than expected.".to_owned())
            } else {
                Ok(std::slice::from_raw_parts(bytes, len).to_vec())
            }
        };
        objc_autoreleasePoolPop(pool);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asks GitHub for real: `cargo test -p mightty-macos live_answer -- --ignored`.
    #[test]
    #[ignore = "needs the network"]
    fn live_answer() {
        let body = fetch(LATEST, 1 << 20).expect("an answer from GitHub");
        assert_eq!(
            compare(&body, "0.0.1"),
            Outcome::Newer(latest_for_test(&body))
        );
        assert_eq!(compare(&body, "999.0.0"), Outcome::UpToDate);
    }

    fn latest_for_test(body: &[u8]) -> String {
        let release = mightty_json::parse(std::str::from_utf8(body).unwrap()).unwrap();
        release["tag_name"]
            .as_str()
            .unwrap()
            .trim_start_matches('v')
            .to_owned()
    }

    #[test]
    fn versions() {
        assert_eq!(version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(version("12.0.345"), Some((12, 0, 345)));
        for odd in [
            "",
            "v",
            "1.2",
            "1.2.3.4",
            "v1.2.3-beta.1",
            "1.2.x",
            "1..3",
            "v1.2.3/../x",
            "9999999.0.0",
        ] {
            assert_eq!(version(odd), None, "{odd}");
        }
        assert!(version("0.10.0") > version("0.9.9"));
    }

    #[test]
    fn outcomes() {
        let answer =
            |tag: &str| format!(r#"{{"tag_name": "{tag}", "html_url": "https://evil.example/x"}}"#);
        assert_eq!(
            compare(answer("v0.2.0").as_bytes(), "0.1.0"),
            Outcome::Newer("0.2.0".into())
        );
        assert_eq!(
            compare(answer("v0.1.0").as_bytes(), "0.1.0"),
            Outcome::UpToDate
        );
        assert_eq!(
            compare(answer("v0.1.0").as_bytes(), "0.3.0"),
            Outcome::UpToDate
        );
        // Only a plain version is offered, whatever the answer says.
        for odd in ["v0.2.0-beta.1", "latest", "v9.9.9/../../evil"] {
            assert!(
                matches!(compare(answer(odd).as_bytes(), "0.1.0"), Outcome::Failed(_)),
                "{odd}"
            );
        }
        assert!(matches!(
            compare(b"<html>rate limited</html>", "0.1.0"),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            compare(&[0xff, 0xfe], "0.1.0"),
            Outcome::Failed(_)
        ));
        // The addresses are ours, made from the version alone.
        let (download, notes) = addresses("0.2.0");
        assert_eq!(
            download,
            "https://github.com/mightty-org/mightty/releases/download/v0.2.0/Mightty.dmg"
        );
        assert_eq!(
            notes,
            "https://github.com/mightty-org/mightty/releases/tag/v0.2.0"
        );
    }
}
