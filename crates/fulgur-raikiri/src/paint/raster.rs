//! Cached raster pixels inside the existing page paint stack.

use std::collections::HashMap;
use std::sync::Arc;

use fulgur_core::{Error, Result};
use krilla::geom::Path;
use krilla::geom::{Size, Transform};
use krilla::image::{BitsPerComponent, CustomImage, Image, ImageColorspace};
use krilla::num::NormalizedF32;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::computed::{
    ComputedBackgroundRepeat, ComputedBackgroundSize, ComputedCssPosition,
};
use raikiri_html::image_geometry::{background_image_dimensions, background_tiles};
use raikiri_html::{Fragment, NodeId, Page, PaintRect, RasterImage};
use raikiri_traits::{DecodedImage, ImagePixelSource, ImageRasterSize};
use url::Url;

#[derive(Clone, Hash)]
struct Pixels {
    width: u32,
    height: u32,
    colors: Arc<Vec<u8>>,
    alpha: Option<Arc<Vec<u8>>>,
}

impl CustomImage for Pixels {
    fn color_channel(&self) -> &[u8] {
        self.colors.as_slice()
    }
    fn alpha_channel(&self) -> Option<&[u8]> {
        self.alpha.as_ref().map(|alpha| alpha.as_slice())
    }
    fn bits_per_component(&self) -> BitsPerComponent {
        BitsPerComponent::Eight
    }
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn icc_profile(&self) -> Option<&[u8]> {
        None
    }
    fn color_space(&self) -> ImageColorspace {
        ImageColorspace::Rgb
    }
}

/// The most tiles one background layer draws.
const MAX_BACKGROUND_TILES: usize = 100_000;

/// One `url()` background image layer as computed for a box.
pub(super) struct BackgroundLayer<'s> {
    /// The computed `url()` value.
    pub url: &'s str,
    pub size: &'s ComputedBackgroundSize,
    pub position: &'s ComputedCssPosition,
    pub repeat: &'s ComputedBackgroundRepeat,
}

pub(super) struct RasterCache<'a> {
    source: Option<&'a dyn ImagePixelSource>,
    base: &'a Url,
    images: HashMap<Url, (Arc<DecodedImage>, Image)>,
}

impl<'a> RasterCache<'a> {
    pub(super) fn new(source: Option<&'a dyn ImagePixelSource>, base: &'a Url) -> Self {
        Self {
            source,
            base,
            images: HashMap::new(),
        }
    }

    fn image(&mut self, url: &Url, pixels: &Arc<DecodedImage>) -> Result<Image> {
        if let Some((cached, image)) = self.images.get(url)
            && Arc::ptr_eq(cached, pixels)
        {
            return Ok(image.clone());
        }
        // Page validates the RGBA dimensions. Split once for PDF channels;
        // repeated placements share the converted image and source pixels.
        let count = pixels.rgba.len() / 4;
        let mut colors = Vec::with_capacity(count * 3);
        let has_alpha = pixels
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] != 255);
        let mut alpha = has_alpha.then(|| Vec::with_capacity(count));
        for pixel in pixels.rgba.as_chunks::<4>().0 {
            colors.extend_from_slice(&pixel[..3]);
            if let Some(alpha) = &mut alpha {
                alpha.push(pixel[3]);
            }
        }
        let image = Image::from_custom(
            Pixels {
                width: pixels.width,
                height: pixels.height,
                colors: Arc::new(colors),
                alpha: alpha.map(Arc::new),
            },
            true,
        )
        .map_err(Error::PdfGeneration)?;
        self.images
            .insert(url.clone(), (Arc::clone(pixels), image.clone()));
        Ok(image)
    }

    pub(super) fn paint(
        &mut self,
        surface: &mut Surface<'_>,
        page: &Page<'_>,
        fragment: &Fragment<'_>,
        host_group: bool,
    ) -> Result<()> {
        let Some(source) = self.source else {
            return Ok(());
        };
        let Some(placement) = page.raster_image(fragment, source) else {
            return Ok(());
        };
        self.paint_placement(surface, page, &placement, host_group)
    }

    pub(super) fn marker(&self, page: &Page<'_>, owner: NodeId) -> Option<RasterImage> {
        page.raster_marker(owner, self.source?)
    }

    pub(super) fn paint_placement(
        &mut self,
        surface: &mut Surface<'_>,
        page: &Page<'_>,
        placement: &RasterImage,
        host_group: bool,
    ) -> Result<()> {
        let Some(path) = super::clip::clip_path(placement.clip, page.geometry().page_box) else {
            return Ok(());
        };
        let Some(size) = Size::from_wh(placement.rect.width, placement.rect.height) else {
            return Ok(());
        };
        let image = self.image(&placement.url, &placement.pixels)?;
        let opacity = (!host_group)
            .then(|| page.computed(placement.node))
            .flatten()
            .and_then(|style| NormalizedF32::new(style.opacity));
        if let Some(opacity) = opacity {
            surface.push_opacity(opacity);
        }
        surface.push_clip_path(&path, &FillRule::NonZero);
        surface.push_transform(&Transform::from_translate(
            placement.rect.x,
            placement.rect.y,
        ));
        surface.draw_image(image, size);
        surface.pop();
        surface.pop();
        if opacity.is_some() {
            surface.pop();
        }
        Ok(())
    }

    /// Draw one `url()` background layer (CSS Backgrounds 3 §3).
    ///
    /// `positioning` is the `background-origin` box that the size and
    /// position resolve against; tiles cover `painting`, the bounds of the
    /// `background-clip` area, and are clipped to `area`. Tiles use the same
    /// geometry as Raikiri's own painter. An image that cannot be fetched or
    /// decoded draws nothing, like an unavailable `<img>`.
    pub(super) fn paint_background(
        &mut self,
        surface: &mut Surface<'_>,
        layer: &BackgroundLayer<'_>,
        positioning: PaintRect,
        painting: PaintRect,
        area: (&Path, FillRule),
    ) -> Result<()> {
        let Some(source) = self.source else {
            return Ok(());
        };
        let Some(url) = self.resolve(layer.url) else {
            return Ok(());
        };
        let Some(intrinsic) = source.intrinsic_size(&url) else {
            return Ok(());
        };
        let Some((width, height)) = background_image_dimensions(
            layer.size,
            f64::from(positioning.width),
            f64::from(positioning.height),
            intrinsic,
        ) else {
            return Ok(());
        };
        let rect = |rect: PaintRect| {
            (
                f64::from(rect.x),
                f64::from(rect.y),
                f64::from(rect.width),
                f64::from(rect.height),
            )
        };
        let Some(tiles) = background_tiles(
            rect(positioning),
            rect(painting),
            (width, height),
            layer.position,
            layer.repeat,
        ) else {
            return Ok(());
        };
        // Each axis is bounded already, but tiny tiles over a large area can
        // still multiply into millions of draws; such a layer draws nothing.
        if tiles.x.len().saturating_mul(tiles.y.len()) > MAX_BACKGROUND_TILES {
            return Ok(());
        }
        let Some(size) = Size::from_wh(tiles.width as f32, tiles.height as f32) else {
            return Ok(());
        };
        let raster = ImageRasterSize {
            width: width as f32,
            height: height as f32,
        };
        let Some(pixels) = source.get_decoded_at_size(&url, raster, None) else {
            return Ok(());
        };
        let bytes = (pixels.width as usize)
            .checked_mul(pixels.height as usize)
            .and_then(|count| count.checked_mul(4));
        if pixels.width == 0 || pixels.height == 0 || bytes != Some(pixels.rgba.len()) {
            return Ok(());
        }
        let image = self.image(&url, &pixels)?;
        let (path, rule) = area;
        surface.push_clip_path(path, &rule);
        for y in &tiles.y {
            for x in &tiles.x {
                surface.push_transform(&Transform::from_translate(*x as f32, *y as f32));
                surface.draw_image(image.clone(), size);
                surface.pop();
            }
        }
        surface.pop();
        Ok(())
    }

    /// The absolute image URL of a computed `url()` value, without its
    /// fragment, which names no separate resource. `url()` and `url(#id)`
    /// name no external image, so they resolve to nothing rather than to
    /// the document itself.
    fn resolve(&self, raw: &str) -> Option<Url> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let mut url = Url::parse(raw).or_else(|_| self.base.join(raw)).ok()?;
        url.set_fragment(None);
        Some(url)
    }
}

#[cfg(test)]
impl<'a> RasterCache<'a> {
    /// A cache whose relative `url()` values resolve against `file:///`.
    pub(super) fn for_tests(source: Option<&'a dyn ImagePixelSource>) -> Self {
        static BASE: std::sync::LazyLock<Url> =
            std::sync::LazyLock::new(|| Url::parse("file:///").expect("valid URL"));
        Self::new(source, &BASE)
    }
}

#[cfg(test)]
mod tests {
    use super::RasterCache;

    #[test]
    fn local_and_empty_references_resolve_to_nothing() {
        let cache = RasterCache::for_tests(None);
        assert_eq!(cache.resolve("#frag"), None);
        assert_eq!(cache.resolve(""), None);
        assert_eq!(cache.resolve("  "), None);
        assert_eq!(
            cache.resolve("img/a.svg#x").map(|url| url.to_string()),
            Some("file:///img/a.svg".to_string())
        );
    }
}
