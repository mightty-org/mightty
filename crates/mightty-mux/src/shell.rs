//! Automatic shell integration. Scripts are embedded in the binary and
//! written to the private runtime directory on first use, so nothing is installed.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::socket::private_dir;

const ZSHENV: &str = include_str!("../shell/zsh/.zshenv");
const ZSH_INTEGRATION: &str = include_str!("../shell/mightty.zsh");

/// Hooks integration into `cmd` if `shell` is one we support (zsh for now),
/// returning the nonce its marks will carry (see `Term::set_mark_nonce`).
///
/// zsh reads `$ZDOTDIR/.zshenv` first, so pointing ZDOTDIR at our bootstrap
/// lets it restore the user's ZDOTDIR, run their startup files and then load
/// the integration, without touching any of the user's files.
pub fn integrate(cmd: &mut Command, shell: &str) -> Option<String> {
    if Path::new(shell).file_name() != Some(OsStr::new("zsh")) {
        return None;
    }
    let dir = install()?;
    let nonce = nonce()?;
    if let Some(original) = env::var_os("ZDOTDIR") {
        cmd.env("MIGHTTY_ZDOTDIR", original);
    }
    cmd.env("ZDOTDIR", dir.join("zsh")).env("MIGHTTY_SHELL_INTEGRATION_DIR", dir).env("MIGHTTY_NONCE", &nonce);
    Some(nonce)
}

/// 128 random bits as hex.
fn nonce() -> Option<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom").ok()?.read_exact(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn install() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        // Inside the checked private directory, so no one else can have
        // put anything here for the shell to run.
        let dir = private_dir().ok()?.join("shell");
        fs::create_dir_all(dir.join("zsh")).ok()?;
        fs::write(dir.join("zsh/.zshenv"), ZSHENV).ok()?;
        fs::write(dir.join("mightty.zsh"), ZSH_INTEGRATION).ok()?;
        Some(dir)
    })
    .as_deref()
}
