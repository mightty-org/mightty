//! Fonts without the platform's text stack: reading font files and
//! rasterizing glyphs, for platforms without CoreText. macOS uses CoreText
//! by default and this only with `font-backend = mightty`.

mod cff;
mod color;
mod data;
mod face;
mod inflate;
mod png;
mod position;
mod raster;
mod shape;
mod synth;
mod var;

pub use color::{ColorGlyph, draw_color};
pub use data::FontData;
pub use face::{Error, Face, Metrics, Point, Segment};
pub use png::Image;
pub use position::Placed;
pub use raster::rasterize;
pub use shape::{LIGATURES, NO_LIGATURES, Shaped};
pub use synth::{EMBOLDEN, OBLIQUE, embolden, oblique, translate};
pub use var::Axis;
