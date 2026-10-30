//! Automatic shell integration. Scripts are embedded in the binary and
//! written to the runtime directory on first use, so nothing is installed.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::socket::runtime_dir;

const ZSHENV: &str = include_str!("../shell/zsh/.zshenv");
const ZSH_INTEGRATION: &str = include_str!("../shell/mightty.zsh");

/// Hooks integration into `cmd` if `shell` is one we support (zsh for now).
///
/// zsh reads `$ZDOTDIR/.zshenv` first, so pointing ZDOTDIR at our bootstrap
/// lets it restore the user's ZDOTDIR, run their startup files and then load
/// the integration, without touching any of the user's files.
pub fn integrate(cmd: &mut Command, shell: &str) {
    if Path::new(shell).file_name() != Some(OsStr::new("zsh")) {
        return;
    }
    let Some(dir) = install() else { return };
    if let Some(original) = env::var_os("ZDOTDIR") {
        cmd.env("MIGHTTY_ZDOTDIR", original);
    }
    cmd.env("ZDOTDIR", dir.join("zsh")).env("MIGHTTY_SHELL_INTEGRATION_DIR", dir);
}

fn install() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = runtime_dir().join("shell");
        fs::create_dir_all(dir.join("zsh")).ok()?;
        fs::write(dir.join("zsh/.zshenv"), ZSHENV).ok()?;
        fs::write(dir.join("mightty.zsh"), ZSH_INTEGRATION).ok()?;
        Some(dir)
    })
    .as_deref()
}
