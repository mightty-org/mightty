//! Installing a newer version in place of the running one. The download
//! is unpacked beside the app, checked, and swapped in when Mightty quits.
//!
//! What's checked before anything is replaced: the archive's checksum, and
//! then, which is what counts, that macOS finds the app signed by
//! Mightty's Developer ID team and notarized by Apple, and that it is
//! Mightty, in the version asked for. An update that fails any of these is
//! thrown away. The checks are macOS's own tools, by their full paths.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The Developer ID team releases are signed by.
const TEAM: &str = "BY3RZ5F2L7";
const BUNDLE_ID: &str = "org.mightty.Mightty";

/// The folder beside the app that an update is unpacked in.
const STAGING: &str = ".mightty-update";

/// The app bundle this process runs from, if it can be replaced: a bundle
/// (not `cargo run`), where it really is (macOS runs an app opened from a
/// download or a disk image from a read-only copy elsewhere), in a folder
/// that can be written to.
pub fn target() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // Mightty.app/Contents/MacOS/mightty
    let app = exe
        .ancestors()
        .nth(3)
        .filter(|app| app.extension().is_some_and(|e| e == "app"))
        .ok_or("Mightty isn't running from an app bundle.")?;
    if app
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return Err("Move Mightty to the Applications folder first.".into());
    }
    let folder = app.parent().ok_or("The app isn't in a folder.")?;
    let probe = folder.join(format!("{STAGING}-{}", std::process::id()));
    fs::create_dir(&probe).map_err(|_| format!("{} can't be written to.", folder.display()))?;
    let _ = fs::remove_dir(&probe);
    Ok(app.to_owned())
}

/// Unpacks `zip` (a release's `Mightty.zip`) beside `app` and checks it is
/// version `version` of Mightty, signed and notarized, with the checksum
/// `sums` (the release's `SHA256SUMS`) gives. Returns the new app, ready
/// for `swap`.
pub fn stage(version: &str, app: &Path, zip: &[u8], sums: &str) -> Result<PathBuf, String> {
    let staging = staging(app)?;
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let staged = (|| {
        let archive = staging.join("Mightty.zip");
        fs::write(&archive, zip).map_err(|e| e.to_string())?;
        let expected = sums
            .lines()
            .find_map(|line| line.strip_suffix("  Mightty.zip"))
            .ok_or("The release has no checksum for this download.")?;
        let sum = run("/usr/bin/shasum", &["-a", "256"], &archive)?;
        if sum.split_whitespace().next() != Some(expected.trim()) {
            return Err("The download was damaged.".to_owned());
        }
        let unpacked = staging.join("new");
        // ditto keeps what the signature covers: links, attributes, the ticket.
        let status = Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(&archive)
            .arg(&unpacked)
            .stdin(Stdio::null())
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("The download couldn't be unpacked.".into());
        }
        let _ = fs::remove_file(&archive);
        let new = unpacked.join("Mightty.app");
        verify(&new, version)?;
        Ok(new)
    })();
    if staged.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    staged
}

/// Whether macOS finds `app` to be Mightty `version`, signed by the
/// Developer ID team and notarized.
fn verify(app: &Path, version: &str) -> Result<(), String> {
    // Apple's own form for "a Developer ID certificate of this team".
    let requirement = format!(
        "-R=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] exists \
         and certificate leaf[subject.OU] = \"{TEAM}\""
    );
    run(
        "/usr/bin/codesign",
        &["--verify", "--deep", "--strict", &requirement],
        app,
    )
    .map_err(|_| "The update isn't signed by Mightty's developer.")?;
    run("/usr/sbin/spctl", &["--assess", "--type", "execute"], app)
        .map_err(|_| "The update isn't notarized by Apple.")?;
    let info = app.join("Contents/Info.plist");
    let read = |key: &str| {
        run(
            "/usr/bin/plutil",
            &["-extract", key, "raw", "-o", "-"],
            &info,
        )
    };
    if read("CFBundleIdentifier")?.trim() != BUNDLE_ID {
        return Err("The download isn't Mightty.".into());
    }
    if read("CFBundleShortVersionString")?.trim() != version {
        return Err("The download is a different version from the one offered.".into());
    }
    Ok(())
}

/// Puts `new` (from `stage`) in place of `app`, which is kept until the
/// new one is there and put back if it can't be.
pub fn swap(new: &Path, app: &Path) -> Result<(), String> {
    let staging = staging(app)?;
    let old = staging.join("old.app");
    fs::rename(app, &old).map_err(|e| format!("The old version couldn't be moved: {e}."))?;
    if let Err(e) = fs::rename(new, app) {
        let _ = fs::rename(&old, app);
        return Err(format!(
            "The new version couldn't be moved into place: {e}."
        ));
    }
    let _ = fs::remove_dir_all(&staging);
    Ok(())
}

/// Removes what an update that was never installed left beside `app`.
pub fn clean(app: &Path) {
    if let Ok(staging) = staging(app) {
        let _ = fs::remove_dir_all(staging);
    }
}

/// Opens `app` once this process has gone.
pub fn relaunch(app: &Path) {
    let script = format!(
        "while /bin/kill -0 {} 2>/dev/null; do /bin/sleep 0.1; done; exec /usr/bin/open \"$1\"",
        std::process::id()
    );
    let _ = Command::new("/bin/sh")
        .args(["-c", &script, "sh"])
        .arg(app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn staging(app: &Path) -> Result<PathBuf, String> {
    Ok(app
        .parent()
        .ok_or("The app isn't in a folder.")?
        .join(STAGING))
}

/// What `tool args path` prints, if it succeeds.
fn run(tool: &str, args: &[&str], path: &Path) -> Result<String, String> {
    let output = Command::new(tool)
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{tool}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{tool} refused {}", path.display()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder standing in for Applications, with an app in it to replace.
    fn applications(name: &str) -> (PathBuf, PathBuf) {
        let folder =
            std::env::temp_dir().join(format!("mightty-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        let app = folder.join("Mightty.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::write(app.join("Contents/old"), "old").unwrap();
        (folder, app)
    }

    #[test]
    fn a_wrong_checksum_or_archive_is_thrown_away() {
        let (folder, app) = applications("bad");
        let sums =
            "0000000000000000000000000000000000000000000000000000000000000000  Mightty.zip\n";
        let why = stage("9.9.9", &app, b"not a zip", sums).unwrap_err();
        assert!(why.contains("damaged"), "{why}");
        assert!(stage("9.9.9", &app, b"x", "abc  Other.zip\n").is_err());
        // Nothing is left behind, and the app is untouched.
        assert!(!folder.join(STAGING).exists());
        assert!(app.join("Contents/old").exists());
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn swapping_puts_the_old_one_back_when_it_fails() {
        let (folder, app) = applications("swap");
        fs::create_dir(folder.join(STAGING)).unwrap();
        // No new app to move in: the old one stays where it was.
        assert!(swap(&folder.join(STAGING).join("new/Mightty.app"), &app).is_err());
        assert!(app.join("Contents/old").exists());
        // With one, it takes the old one's place and the staging goes.
        let new = folder.join(STAGING).join("new/Mightty.app");
        fs::create_dir_all(new.join("Contents")).unwrap();
        fs::write(new.join("Contents/new"), "new").unwrap();
        swap(&new, &app).unwrap();
        assert!(app.join("Contents/new").exists() && !app.join("Contents/old").exists());
        assert!(!folder.join(STAGING).exists());
        let _ = fs::remove_dir_all(folder);
    }

    /// A local build, signed ad hoc and not by the Developer ID, is
    /// refused even with the right checksum (after `scripts/bundle.sh`):
    /// `cargo test -p mightty-macos local_build -- --ignored`.
    #[test]
    #[ignore = "needs target/release/Mightty.app"]
    fn local_build_is_refused() {
        let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/release/Mightty.app");
        assert!(built.exists(), "run scripts/bundle.sh first");
        let (folder, app) = applications("local");
        let archive = folder.join("built.zip");
        let zipped = Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&built)
            .arg(&archive)
            .status()
            .unwrap();
        assert!(zipped.success());
        let sum = run("/usr/bin/shasum", &["-a", "256"], &archive).unwrap();
        let sums = format!("{}  Mightty.zip\n", sum.split_whitespace().next().unwrap());
        let version = env!("CARGO_PKG_VERSION");
        let why = stage(version, &app, &fs::read(&archive).unwrap(), &sums).unwrap_err();
        assert!(why.contains("signed"), "{why}");
        assert!(!folder.join(STAGING).exists() && app.join("Contents/old").exists());
        let _ = fs::remove_dir_all(folder);
    }

    /// The real 0.1.0 release, downloaded, checked and swapped in for a
    /// stand-in; and the same archive refused as another version:
    /// `cargo test -p mightty-macos real_release -- --ignored`.
    #[test]
    #[ignore = "needs the network"]
    fn real_release() {
        let base = "https://github.com/mightty-org/mightty/releases/download/v0.1.0";
        let get = |name: &str| {
            let out = Command::new("/usr/bin/curl")
                .args(["-sL", "--fail", &format!("{base}/{name}")])
                .output()
                .unwrap();
            assert!(out.status.success(), "{name}");
            out.stdout
        };
        let (zip, sums) = (
            get("Mightty.zip"),
            String::from_utf8(get("SHA256SUMS")).unwrap(),
        );
        let (folder, app) = applications("real");
        let why = stage("0.2.0", &app, &zip, &sums).unwrap_err();
        assert!(why.contains("different version"), "{why}");
        let new = stage("0.1.0", &app, &zip, &sums).unwrap();
        swap(&new, &app).unwrap();
        assert!(app.join("Contents/MacOS/mightty").exists());
        assert!(verify(&app, "0.1.0").is_ok());
        let _ = fs::remove_dir_all(folder);
    }
}
