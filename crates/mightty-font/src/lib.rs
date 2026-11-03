//! Fonts without the platform's text stack: reading font files and
//! rasterizing glyphs, for platforms without CoreText. macOS uses CoreText
//! by default and this only with `font-backend = mightty`.

mod face;
mod raster;

pub use face::{Error, Face, Metrics, Point, Segment};
pub use raster::rasterize;
