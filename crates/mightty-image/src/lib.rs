//! Image decoding without the platform's image stack, shared by fonts
//! (colour emoji) and the terminal (kitty graphics): PNG, and zlib streams.

pub mod inflate;
pub mod png;

pub use png::Image;
