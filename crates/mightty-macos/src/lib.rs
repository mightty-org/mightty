//! macOS front end: Cocoa window, Metal renderer, CoreText glyphs, all
//! through hand-written FFI to the system frameworks.

#![cfg(target_os = "macos")]

mod app;
mod bindings;
mod config;
mod ffi;
mod findbar;
mod font;
mod hotkey;
mod keys;
mod latency;
mod linkhint;
mod notify;
mod open;
mod renderer;
mod session;
mod snapshot;
mod split;

pub use app::run;
pub use config::Config;
