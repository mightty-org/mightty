//! Automatic shell integration. Scripts are embedded in the binary and
//! written to the private runtime directory on first use, so nothing is installed.

use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::OnceLock;

use crate::socket::private_dir;

/// Script files, by their path in the integration directory.
const FILES: &[(&str, &str)] = &[
    ("zsh/.zshenv", include_str!("../shell/zsh/.zshenv")),
    ("mightty.zsh", include_str!("../shell/mightty.zsh")),
    ("bash/bootstrap.bash", include_str!("../shell/bash/bootstrap.bash")),
    ("mightty.bash", include_str!("../shell/mightty.bash")),
    ("fish/vendor_conf.d/mightty.fish", include_str!("../shell/fish/vendor_conf.d/mightty.fish")),
];

/// Hooks integration into `cmd` if `shell` is one we support (zsh, bash,
/// fish), returning the nonce its marks will carry (see
/// `Term::set_mark_nonce`). Each shell is pointed at a bootstrap that runs
/// the user's startup files as usual and then loads the integration,
/// without touching any of the user's files:
///
/// - zsh reads `$ZDOTDIR/.zshenv` first, so ZDOTDIR points at ours, which
///   restores the user's ZDOTDIR.
/// - bash reads `--rcfile` in place of ~/.bashrc, in an interactive shell
///   that isn't a login shell (`--posix` with `$ENV`, which Ghostty uses,
///   isn't read by macOS's bash 3.2), so it starts as one with ours, which
///   reads the login shell's files.
/// - fish loads `fish/vendor_conf.d` from each of `$XDG_DATA_DIRS`, so our
///   directory goes first there, and takes itself back out.
///
/// bash and fish get it only when `interactive`: there's nothing to mark
/// in a shell running a command.
pub fn integrate(cmd: &mut Command, shell: &str, interactive: bool) -> Option<String> {
    let name = Path::new(shell).file_name().and_then(OsStr::to_str)?;
    if !(name == "zsh" || interactive && (name == "bash" || name == "fish")) {
        return None;
    }
    let dir = install()?;
    let nonce = nonce()?;
    match name {
        "zsh" => {
            if let Some(original) = env::var_os("ZDOTDIR") {
                cmd.env("MIGHTTY_ZDOTDIR", original);
            }
            cmd.env("ZDOTDIR", dir.join("zsh"));
        }
        "bash" => {
            // No leading dash: a login shell would skip the rcfile.
            cmd.arg0("bash").arg("--rcfile").arg(dir.join("bash/bootstrap.bash")).env("MIGHTTY_BASH_LOGIN", "1");
        }
        _ => {
            // The XDG default when unset.
            let rest = env::var("XDG_DATA_DIRS").ok().filter(|d| !d.is_empty()).unwrap_or("/usr/local/share:/usr/share".into());
            // fish looks in `<entry>/fish/vendor_conf.d`.
            cmd.env("XDG_DATA_DIRS", format!("{}:{rest}", dir.display())).env("MIGHTTY_FISH_XDG_DATA_DIR", dir);
        }
    }
    cmd.env("MIGHTTY_SHELL_INTEGRATION_DIR", dir).env("MIGHTTY_NONCE", &nonce);
    Some(nonce)
}

/// 128 random bits as hex.
pub(crate) fn nonce() -> Option<String> {
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
        for (path, text) in FILES {
            let path = dir.join(path);
            fs::create_dir_all(path.parent()?).ok()?;
            fs::write(path, text).ok()?;
        }
        Some(dir)
    })
    .as_deref()
}
