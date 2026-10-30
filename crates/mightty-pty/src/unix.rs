use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::ptr;

use crate::WindowSize;

#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

impl From<WindowSize> for Winsize {
    fn from(s: WindowSize) -> Self {
        Self { ws_row: s.rows, ws_col: s.cols, ws_xpixel: s.pixel_width, ws_ypixel: s.pixel_height }
    }
}

#[cfg(target_os = "macos")]
const TIOCSCTTY: c_ulong = 0x2000_7461;
#[cfg(target_os = "macos")]
const TIOCSWINSZ: c_ulong = 0x8008_7467;
#[cfg(target_os = "linux")]
const TIOCSCTTY: c_ulong = 0x540E;
#[cfg(target_os = "linux")]
const TIOCSWINSZ: c_ulong = 0x5414;

const F_SETFD: c_int = 2;
const FD_CLOEXEC: c_int = 1;

#[cfg_attr(target_os = "linux", link(name = "util"))]
unsafe extern "C" {
    fn openpty(
        master: *mut c_int,
        slave: *mut c_int,
        name: *mut c_char,
        termp: *const c_void,
        winp: *const Winsize,
    ) -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn setsid() -> c_int;
}

pub struct Pty {
    master: File,
    child: Child,
}

impl Pty {
    /// Spawns `cmd` as a session leader with a new PTY as its controlling terminal.
    pub fn spawn(mut cmd: Command, size: WindowSize) -> io::Result<Self> {
        let (mut master, mut slave) = (-1, -1);
        let ws = Winsize::from(size);
        // SAFETY: out-pointers are valid; name and termios may be null.
        if unsafe { openpty(&mut master, &mut slave, ptr::null_mut(), ptr::null(), &ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openpty succeeded, so both descriptors are open and owned by us.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        set_cloexec(&master)?;
        set_cloexec(&slave)?;

        cmd.stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "mightty")
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

        // Runs in the child after fork, once stdio is the PTY slave.
        // Only async-signal-safe calls are allowed here.
        let make_controlling_tty = || {
            // SAFETY: plain syscalls on fd 0, which std has set to the slave.
            if unsafe { setsid() } < 0 || unsafe { ioctl(0, TIOCSCTTY, 0 as c_int) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        };
        // SAFETY: the hook only makes async-signal-safe syscalls.
        unsafe { cmd.pre_exec(make_controlling_tty) };

        let child = cmd.spawn()?;
        // Close our copies of the slave so reads on the master end when the child exits.
        drop(cmd);
        Ok(Self { master: File::from(master), child })
    }

    pub fn resize(&self, size: WindowSize) -> io::Result<()> {
        let ws = Winsize::from(size);
        // SAFETY: valid fd and pointer to a properly laid out winsize.
        if unsafe { ioctl(self.master.as_raw_fd(), TIOCSWINSZ, &ws as *const Winsize) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Output from the child. Reads end with `Ok(0)` or `EIO` once it exits.
    pub fn reader(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    /// Input to the child.
    pub fn writer(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }

    pub fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }
}

fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: valid fd; F_SETFD takes an int argument.
    if unsafe { fcntl(fd.as_raw_fd(), F_SETFD, FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn child_sees_a_tty_with_our_size() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "stty size; tty -s && echo tty"]);
        let mut pty = Pty::spawn(cmd, WindowSize { cols: 77, rows: 13, ..Default::default() }).unwrap();
        let mut out = Vec::new();
        let mut reader = pty.reader().unwrap();
        let mut buf = [0; 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
        assert!(pty.wait().unwrap().success());
        assert_eq!(String::from_utf8_lossy(&out), "13 77\r\ntty\r\n");
    }
}
