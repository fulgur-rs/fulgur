//! Cached raster pixels inside the existing page paint stack.

use std::collections::HashMap;
use std::sync::Arc;

use fulgur_core::{Error, Result};
use krilla::geom::{Size, Transform};
use krilla::image::{BitsPerComponent, CustomImage, Image, ImageColorspace};
use krilla::num::NormalizedF32;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::{Fragment, NodeId, Page, RasterImage};
use raikiri_traits::{DecodedImage, ImagePixelSource};
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

pub(super) struct RasterCache<'a> {
    source: Option<&'a dyn ImagePixelSource>,
    images: HashMap<Url, (Arc<DecodedImage>, Image)>,
}

impl<'a> RasterCache<'a> {
    pub(super) fn new(source: Option<&'a dyn ImagePixelSource>) -> Self {
        Self {
            source,
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
}
