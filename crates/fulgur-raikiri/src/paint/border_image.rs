//! Border images (CSS Backgrounds 3 §6).
//!
//! Raikiri computes the nine parts of the image and their tiles
//! ([`border_image_geometry`]); this module draws each tile as the part's
//! rectangle of the sized image, clipped to the part's area.

use super::shape::{Edges, RoundedRect, Slice};
use super::surface::Surface;
use super::{border, gradient, raster, tiling};
use fulgur_core::Result;
use krilla::geom::{Size, Transform};
use krilla::image::Image;
use krilla::num::NormalizedF32;
use krilla::paint::{Fill, FillRule, Paint};
use raikiri_html::PaintRect;
use raikiri_html::computed::{ComputedBackgroundImage, ComputedValues, Sides};
use raikiri_html::image_geometry::{BorderImageGeometry, BorderImagePart, border_image_geometry};

/// What one tile draws: the sized image, or a gradient over its box.
enum Source {
    Image(Image),
    Gradient(Paint),
}

/// Draw the border image of the box fragment `rect`, whose broken edges
/// are `slice`. Returns whether the image replaces the border styles: true
/// for an available image even when no part is drawn (§6.1), false for no
/// image or one that cannot be fetched or decoded, so that the caller
/// draws the borders instead.
///
/// A broken edge has no border width, so number widths and outsets on it
/// count as zero and the fragment shows the image only on the edges it
/// keeps.
pub(super) fn paint(
    surface: &mut Surface<'_>,
    raster: &mut raster::RasterCache<'_>,
    rect: PaintRect,
    slice: Slice,
    style: &ComputedValues,
) -> Result<bool> {
    let image = &style.border_image;
    let widths = slice.edges(border::widths(style));
    let geometry = |natural| {
        border_image_geometry(
            (
                f64::from(rect.x),
                f64::from(rect.y),
                f64::from(rect.width),
                f64::from(rect.height),
            ),
            sides(widths),
            image,
            natural,
        )
    };
    match &image.source {
        ComputedBackgroundImage::Url(url) => {
            let Some((url, natural)) = raster.natural_size(url) else {
                return Ok(false);
            };
            let Some(geometry) = geometry(Some(natural)) else {
                return Ok(true);
            };
            let (width, height) = geometry.image_size;
            let Some(image) = raster.sized_image(&url, width as f32, height as f32)? else {
                return Ok(false);
            };
            draw(surface, &geometry, &Source::Image(image));
            Ok(true)
        }
        ComputedBackgroundImage::Gradient(gradient) => {
            let Some(geometry) = geometry(None) else {
                return Ok(true);
            };
            let (width, height) = geometry.image_size;
            let area = RoundedRect::rect(0.0, 0.0, width as f32, height as f32);
            if let Some(paint) = gradient::paint(gradient, &area, style.color) {
                draw(surface, &geometry, &Source::Gradient(paint));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn sides(edges: Edges) -> Sides<f64> {
    Sides {
        top: f64::from(edges.top),
        right: f64::from(edges.right),
        bottom: f64::from(edges.bottom),
        left: f64::from(edges.left),
    }
}

/// Draw every tile of every part of `geometry`.
fn draw(surface: &mut Surface<'_>, geometry: &BorderImageGeometry, source: &Source) {
    let (image_width, image_height) = geometry.image_size;
    let Some(image_size) = Size::from_wh(image_width as f32, image_height as f32) else {
        return;
    };
    for part in &geometry.parts {
        let (area_x, area_y, area_w, area_h) = part.area;
        // Raikiri only returns non-empty parts of a non-empty image, with a
        // bounded number of tiles each.
        let Some(area) =
            RoundedRect::rect(area_x as f32, area_y as f32, area_w as f32, area_h as f32).path()
        else {
            continue;
        };
        surface.push_clip_path(&area, &FillRule::NonZero);
        tiling::draw(
            surface,
            (&part.x, &part.y),
            (part.tile_width, part.tile_height),
            &mut |tile| draw_tile(tile, part, source, image_size),
        );
        surface.pop();
    }
}

/// Draw one tile of `part` with its top left corner at the origin.
fn draw_tile(surface: &mut Surface<'_>, part: &BorderImagePart, source: &Source, image_size: Size) {
    let (src_x, src_y, src_w, src_h) = part.source;
    let Some(source_rect) =
        RoundedRect::rect(src_x as f32, src_y as f32, src_w as f32, src_h as f32).path()
    else {
        return;
    };
    let scale_x = part.tile_width / src_w;
    let scale_y = part.tile_height / src_h;
    // Maps the source rectangle of the image onto the tile.
    surface.push_transform(&Transform::from_row(
        scale_x as f32,
        0.0,
        0.0,
        scale_y as f32,
        (-src_x * scale_x) as f32,
        (-src_y * scale_y) as f32,
    ));
    match source {
        Source::Image(image) => {
            surface.push_clip_path(&source_rect, &FillRule::NonZero);
            surface.draw_image(image.clone(), image_size);
            surface.pop();
        }
        Source::Gradient(paint) => {
            surface.set_fill(Some(Fill {
                paint: paint.clone(),
                opacity: NormalizedF32::ONE,
                rule: FillRule::NonZero,
            }));
            surface.draw_path(&source_rect);
        }
    }
    surface.pop();
}

#[cfg(test)]
mod tests;
