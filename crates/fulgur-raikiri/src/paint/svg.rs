//! Inline SVG content inside the existing page paint stack.

use fulgur_core::{Error, Result};
use krilla::geom::{Rect, Size, Transform};
use krilla::num::NormalizedF32;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::{Fragment, ImageContent, Page};

pub(super) mod scene;

#[cfg(test)]
mod tests;

/// What the inline SVGs of one PDF share while they are drawn.
#[derive(Default)]
pub(super) struct SvgCache {
    /// Fonts and pictures converted for Krilla, so that a face or a picture
    /// that several SVGs draw is converted once.
    assets: scene::SceneAssets,
}

impl SvgCache {
    /// Draws the fragment's inline SVG. Pass `host_group` when the enclosing
    /// paint events composite the host opacity; otherwise the SVG keeps its
    /// root opacity.
    ///
    /// Raikiri decides whether the picture is a scene or pixels, and this
    /// draws what it is given.
    pub(super) fn paint(
        &mut self,
        surface: &mut Surface<'_>,
        page: &Page<'_>,
        fragment: &Fragment<'_>,
        host_group: bool,
    ) -> Result<()> {
        let Some(svg) = page
            .inline_svg(fragment)
            .map_err(|error| Error::Layout(format!("inline SVG: {error}")))?
        else {
            return Ok(());
        };
        let area = fragment.paint_rect();
        let size = Size::from_wh(svg.viewport.width, svg.viewport.height)
            .ok_or_else(|| Error::PdfGeneration("invalid SVG viewport".into()))?;
        // Keep the original viewport while clipping this page's fragment slice.
        let clip_path = rect_path(area.x, area.y, area.width, area.height, "fragment clip")?;
        // The picture is placed in its viewport box and clipped to it, as a
        // `preserveAspectRatio` slice needs; so are pixels, which are rounded
        // up to whole device pixels.
        let viewport = rect_path(0.0, 0.0, size.width(), size.height(), "viewport")?;
        // Neither a scene nor pixels hold the host's opacity. When the page's
        // paint events do not composite it, the SVG keeps it.
        let own_opacity = svg.host_opacity.filter(|_| !host_group);
        surface.push_clip_path(&clip_path, &FillRule::NonZero);
        surface.push_transform(&Transform::from_translate(svg.viewport.x, svg.viewport.y));
        surface.push_clip_path(&viewport, &FillRule::NonZero);
        if let Some(alpha) = own_opacity {
            surface.push_opacity(NormalizedF32::new(alpha).unwrap_or(NormalizedF32::ONE));
        }
        let result = match &svg.content {
            ImageContent::Vector(picture) => scene::paint(surface, picture, &mut self.assets),
            ImageContent::Raster(pixels) => {
                // The pixels are the CSS pixel grid at the layout's scale.
                let scale = page.svg_raster_scale();
                let drawn =
                    Size::from_wh(pixels.width as f32 / scale, pixels.height as f32 / scale)
                        .unwrap_or(size);
                self.assets.pixels(pixels, true).map(|image| {
                    surface.draw_image(image, drawn);
                })
            }
            _ => Err(Error::PdfGeneration("unknown inline SVG content".into())),
        };
        if own_opacity.is_some() {
            surface.pop();
        }
        surface.pop();
        surface.pop();
        surface.pop();
        result
    }

    #[cfg(test)]
    pub(super) fn converted_images(&self) -> usize {
        self.assets.converted_images()
    }
}

pub(super) fn rect_path(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    what: &str,
) -> Result<krilla::geom::Path> {
    let rect = Rect::from_xywh(x, y, width, height)
        .ok_or_else(|| Error::PdfGeneration(format!("invalid SVG {what}")))?;
    let mut builder = krilla::geom::PathBuilder::new();
    builder.push_rect(rect);
    builder
        .finish()
        .ok_or_else(|| Error::PdfGeneration(format!("invalid SVG {what} path")))
}
