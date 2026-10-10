//! Page-margin boxes (CSS Paged Media 3 §4.2), laid out by Raikiri for each
//! page: the painter only draws the resolved boxes.

use super::raster::{BackgroundLayer, RasterCache};
use super::shape::{Edges, RoundedRect};
use super::{FontCache, fill, paint_glyph_run};
use fulgur_core::Result;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::computed::ComputedVisualBox;
use raikiri_html::{MarginBox, Page, PaintInsets};

/// Draw the margin boxes of `page` in Raikiri's order, below the page body.
pub(super) fn paint(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    fonts: &mut FontCache,
    raster: &mut RasterCache<'_>,
) -> Result<()> {
    for margin_box in page.margin_boxes() {
        paint_box(surface, &margin_box, fonts, raster)?;
    }
    Ok(())
}

/// The background color and image, the solid borders, then the text clipped
/// to the border box.
fn paint_box(
    surface: &mut Surface<'_>,
    margin_box: &MarginBox,
    fonts: &mut FontCache,
    raster: &mut RasterCache<'_>,
) -> Result<()> {
    let rect = margin_box.rect;
    let outer = RoundedRect::rect(rect.x, rect.y, rect.width, rect.height);
    let Some(border_box) = outer.path() else {
        return Ok(());
    };
    if let Some(color) = margin_box.background_color {
        surface.set_fill(Some(fill(color)));
        surface.draw_path(&border_box);
    }
    if let Some(image) = &margin_box.background_image {
        let borders = edges(margin_box.border_widths());
        let padding = edges(margin_box.padding);
        let visual_box = |visual: ComputedVisualBox| match visual {
            ComputedVisualBox::PaddingBox => Some(outer.inset(borders)),
            ComputedVisualBox::ContentBox => Some(outer.inset(borders.add(padding))),
            ComputedVisualBox::BorderBox => Some(outer),
            // Margin boxes have no glyph or border-area clip shapes here;
            // Raikiri's own painter skips the image in these cases too.
            _ => None,
        };
        if let (Some(positioning), Some(painting)) =
            (visual_box(image.origin), visual_box(image.clip))
            && let Some(area) = painting.path()
        {
            let layer = BackgroundLayer {
                url: &image.url,
                size: &image.size,
                position: &image.position,
                repeat: &image.repeat,
            };
            raster.paint_background(
                surface,
                &layer,
                positioning.bounds(),
                painting.bounds(),
                (&area, FillRule::NonZero),
            )?;
        }
    }
    for (side, border) in margin_box.borders.iter().enumerate() {
        let Some(border) = border else { continue };
        let width = border.width;
        // Each side covers the full length of its edge; corners overlap.
        let strip = match side {
            0 => RoundedRect::rect(rect.x, rect.y, rect.width, width),
            1 => RoundedRect::rect(rect.x + rect.width - width, rect.y, width, rect.height),
            2 => RoundedRect::rect(rect.x, rect.y + rect.height - width, rect.width, width),
            _ => RoundedRect::rect(rect.x, rect.y, width, rect.height),
        };
        if let Some(path) = strip.path() {
            surface.set_fill(Some(fill(border.color)));
            surface.draw_path(&path);
        }
    }
    let runs = margin_box.text_runs();
    if runs.is_empty() {
        return Ok(());
    }
    surface.push_clip_path(&border_box, &FillRule::NonZero);
    for run in &runs {
        paint_glyph_run(surface, run, fonts);
    }
    surface.pop();
    Ok(())
}

fn edges(insets: PaintInsets) -> Edges {
    Edges {
        top: insets.top,
        right: insets.right,
        bottom: insets.bottom,
        left: insets.left,
    }
}
