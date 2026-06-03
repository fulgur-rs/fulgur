//! HTML → image output (PNG / lossless WebP).
//!
//! Gated behind the `image-export` cargo feature. Reuses the existing
//! layout → `Drawables` pipeline and branches at the draw stage: page 0's
//! drawables are serialized to SVG (text as glyph-outline paths), then
//! rasterized with resvg/tiny-skia and encoded.

mod b64;
mod encode;
mod glyph_path;
mod options;
mod rasterize;
mod svg_emit;

pub use options::{Background, ImageFormat, ImageOptions};
