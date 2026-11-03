//! Pseudo-terminals. Unix uses `openpty(3)` through hand-written FFI;
//! Windows (ConPTY) is not implemented yet.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{Foreground, Pty};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowSize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}
