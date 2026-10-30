//! Rendering pieces shared by every front end: no GPU or OS code here.

pub mod boxdraw;
mod palette;

pub use palette::{SELECTION_BG, palette};
