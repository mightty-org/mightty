//! macOS front end: Cocoa window, Metal renderer, CoreText glyphs, all
//! through hand-written FFI to the system frameworks.

#![cfg(target_os = "macos")]

mod app;
mod config;
mod ffi;
mod font;
mod keys;
mod renderer;
mod snapshot;

pub use app::run;
pub use config::Config;
