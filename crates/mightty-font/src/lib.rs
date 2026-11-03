//! Fonts without the platform's text stack: reading font files and
//! rasterizing glyphs, for platforms without CoreText. macOS uses CoreText
//! by default and this only with `font-backend = mightty`.

mod cff;
mod color;
mod data;
mod face;
mod inflate;
mod png;
mod raster;
mod synth;
mod var;

pub use color::{ColorGlyph, draw_color};
pub use data::FontData;
pub use face::{Error, Face, Metrics, Point, Segment};
pub use png::Image;
pub use raster::rasterize;
pub use synth::{EMBOLDEN, OBLIQUE, embolden, oblique};
pub use var::Axis;
