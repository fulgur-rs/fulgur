//! Inline SVG drawn as pixels, for the pictures whose rendering is `raster`.
//!
//! The image is the picture of the CSS pixel grid with `svg_raster_scale`
//! device pixels per CSS pixel, so a larger scale keeps the same drawing and
//! gives it more pixels.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};

use fulgur_core::{Error, Result};
use krilla::geom::Size;
use krilla::image::Image;
use raikiri_html::{InlineSvg, Page};
use raikiri_svg::{SvgDocument, SvgRootStyle, SvgViewport};

use crate::paint::raster::krilla_image;

/// Upper bound on the pixel bytes of the rasters that stay cached.
const MAX_CACHED_RASTER_BYTES: usize = 64 * 1024 * 1024;

/// A rasterized SVG, ready to be drawn.
#[derive(Clone)]
pub(super) struct Raster {
    image: Image,
    /// The size to draw it at, in CSS pixels.
    size: Size,
}

impl Raster {
    pub(super) fn image(&self) -> Image {
        self.image.clone()
    }

    pub(super) fn size(&self) -> Size {
        self.size
    }
}

/// What a raster depends on: the prepared source, the host opacity removed
/// from the root, the viewport and the scale, as bits.
type Key = (String, Option<u32>, u32, u32, u32);

/// Rasters by what they depend on. One is kept only when it is asked for
/// again, so one-off SVGs never pile up, and the kept ones stay under a pixel
/// budget.
#[derive(Default)]
pub(super) struct RasterMemo {
    rasters: HashMap<Key, Raster>,
    seen: HashSet<u64>,
    hasher: RandomState,
    kept_bytes: usize,
}

impl RasterMemo {
    /// The raster of the prepared source of `svg` in a box of `size` CSS
    /// pixels at `scale` device pixels per CSS pixel.
    ///
    /// The raster leaves out the host opacity of the root; the caller applies
    /// it around the image.
    pub(super) fn get_or_build(
        &mut self,
        page: &Page<'_>,
        svg: &InlineSvg,
        size: Size,
        scale: f32,
    ) -> Result<Raster> {
        let key: Key = (
            svg.source.clone(),
            svg.host_opacity.map(f32::to_bits),
            size.width().to_bits(),
            size.height().to_bits(),
            scale.to_bits(),
        );
        if let Some(raster) = self.rasters.get(&key) {
            return Ok(raster.clone());
        }
        let (raster, bytes) = build(page, svg, size, scale)?;
        let repeated = !self.seen.insert(self.hasher.hash_one(&key));
        if repeated && self.kept_bytes + bytes <= MAX_CACHED_RASTER_BYTES {
            self.kept_bytes += bytes;
            self.rasters.insert(key, raster.clone());
        }
        Ok(raster)
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.rasters.len()
    }
}

/// Rasterizes `svg`, with the pixel bytes it holds.
fn build(page: &Page<'_>, svg: &InlineSvg, size: Size, scale: f32) -> Result<(Raster, usize)> {
    let fonts = page.svg_fonts();
    let root_style = SvgRootStyle {
        opacity: svg.host_opacity.unwrap_or(1.0),
        // The host opacity is applied around the image, which holds none.
        neutralize_root_opacity: svg.host_opacity.is_some(),
        ..SvgRootStyle::default()
    };
    let pixels = SvgDocument::parse(svg.source.as_bytes())
        .and_then(|document| {
            document.rasterize_with_fonts(
                SvgViewport {
                    width: size.width(),
                    height: size.height(),
                },
                root_style,
                Some(&fonts),
                scale,
                None,
            )
        })
        .map_err(|error| Error::Layout(format!("inline SVG raster: {error}")))?;
    let bytes = pixels.rgba.len();
    let drawn = Size::from_wh(pixels.width as f32 / scale, pixels.height as f32 / scale)
        .ok_or_else(|| Error::PdfGeneration("invalid SVG raster size".into()))?;
    Ok((
        Raster {
            image: krilla_image(&pixels, true)?,
            size: drawn,
        },
        bytes,
    ))
}
