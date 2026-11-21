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
mod images;
mod install;
mod keys;
mod latency;
mod linkhint;
mod notify;
mod open;
mod palette;
mod renderer;
mod scrollbar;
mod session;
mod settings;
mod snapshot;
mod split;
mod tabbar;
mod update;

pub use app::run;
pub use config::Config;
pub use images::{
    HELPER_ARG as IMAGE_HELPER_ARG, TimedOut, decode_in as decode_image_in,
    helper_main as image_helper_main,
};
pub use settings::{export as settings_export, main as settings_main};
