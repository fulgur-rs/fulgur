//! Page-margin boxes (CSS Paged Media 3 §4.2), laid out by Raikiri for each
//! page: the painter only draws the resolved boxes.

use super::raster::BackgroundLayer;
use super::shape::{Edges, RoundedRect};
use super::{FontCache, fill, paint_body, paint_glyph_run, paints_glyphs, raster, svg};
use crate::tagging::{Tags, Target, margin_box_artifact};
use fulgur_core::{Error, Result};
use krilla::geom::Transform;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::computed::ComputedVisualBox;
use raikiri_html::{MarginBox, Page, PaintInsets, PaintRect, PlacedRunningElement};

/// Draw `margin_boxes` in Raikiri's order, below the page body. Margin boxes
/// hold running headers and footers, which tagged output marks as pagination
/// artifacts.
///
/// The running elements the boxes show come from `page`; without a page the
/// boxes draw their text instead, untagged.
///
/// Returns the running elements drawn, with the border box each is clipped
/// to, so their links can be placed on the page.
pub(super) fn paint<'a>(
    surface: &mut Surface<'_>,
    page: Option<&Page<'a>>,
    margin_boxes: &[MarginBox],
    fonts: &mut FontCache,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
    tags: &mut Tags,
) -> Result<Vec<(PlacedRunningElement<'a>, PaintRect)>> {
    let mut drawn = Vec::new();
    for margin_box in margin_boxes {
        let Some(page) = page else {
            paint_box(surface, margin_box, None, fonts, svg, raster)?;
            continue;
        };
        let running = page
            .margin_box_running_element(margin_box)
            .map_err(|error| Error::Layout(error.to_string()))?;
        let artifact = Target::Artifact(margin_box_artifact(margin_box.slot));
        tags.mark(surface, page, artifact, |surface| {
            paint_box(surface, margin_box, running, fonts, svg, raster)
        })?;
        if let Some(running) = running {
            drawn.push((running, margin_box.rect));
        }
    }
    Ok(drawn)
}

/// The background color and image, the solid borders, then the content
/// clipped to the border box: the running element the box shows (CSS GCPM 3
/// §1.2.2), drawn like a page body, or else the box text.
fn paint_box(
    surface: &mut Surface<'_>,
    margin_box: &MarginBox,
    running: Option<PlacedRunningElement<'_>>,
    fonts: &mut FontCache,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
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
    if let Some(running) = running {
        surface.push_clip_path(&border_box, &FillRule::NonZero);
        surface.push_transform(&Transform::from_translate(
            running.origin.0,
            running.origin.1,
        ));
        // The whole box is one artifact, and marked content does not nest,
        // so the running element is drawn without tags of its own.
        let painted = paint_body(
            surface,
            &running.layout.page(),
            fonts,
            svg,
            raster,
            &mut Tags::disabled(),
        );
        surface.pop();
        surface.pop();
        return painted;
    }
    let runs = margin_box.text_runs();
    if runs.is_empty() {
        return Ok(());
    }
    surface.push_clip_path(&border_box, &FillRule::NonZero);
    for run in runs.iter().filter(|run| paints_glyphs(run)) {
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
