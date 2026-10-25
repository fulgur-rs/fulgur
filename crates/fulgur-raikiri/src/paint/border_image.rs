//! Border images (CSS Backgrounds 3 §6).
//!
//! Raikiri computes the nine parts of the image and their tiles
//! ([`border_image_geometry`]); this module draws each tile as the part's
//! rectangle of the sized image, clipped to the part's area.

use super::shape::{Edges, RoundedRect, Slice};
use super::surface::Surface;
use super::{border, gradient, raster};
use fulgur_core::Result;
use krilla::geom::{Size, Transform};
use krilla::image::Image;
use krilla::num::NormalizedF32;
use krilla::paint::{Fill, FillRule, Paint, Pattern};
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

/// Parts with more tiles than this draw them through one tiling pattern
/// instead of one image placement per tile. A few tiles are drawn one by
/// one, which avoids the seams some viewers show between pattern cells.
const PATTERN_MIN_TILES: usize = 5;

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
        let steps = uniform_step(&part.x).zip(uniform_step(&part.y));
        match steps {
            Some((step_x, step_y)) if part.x.len() * part.y.len() >= PATTERN_MIN_TILES => {
                draw_pattern(surface, part, source, image_size, (step_x, step_y));
            }
            _ => {
                for tile_y in &part.y {
                    for tile_x in &part.x {
                        surface.push_transform(&Transform::from_translate(
                            *tile_x as f32,
                            *tile_y as f32,
                        ));
                        draw_tile(surface, part, source, image_size);
                        surface.pop();
                    }
                }
            }
        }
        surface.pop();
    }
}

/// The distance between consecutive tile edges when they are evenly
/// spaced, `Some(0.0)` for a single tile, and `None` otherwise.
fn uniform_step(edges: &[f64]) -> Option<f64> {
    let [first, second, ..] = edges else {
        return Some(0.0);
    };
    let step = second - first;
    let tolerance = step.abs() * 1e-6;
    edges
        .windows(2)
        .all(|pair| (pair[1] - pair[0] - step).abs() <= tolerance)
        .then_some(step)
}

/// Fill the tiles of `part` with one tiling pattern whose cell holds one
/// tile followed by the gap to the next (CSS Backgrounds 3 §6.6 step 3).
fn draw_pattern(
    surface: &mut Surface<'_>,
    part: &BorderImagePart,
    source: &Source,
    image_size: Size,
    (step_x, step_y): (f64, f64),
) {
    let (Some(&first_x), Some(&last_x), Some(&first_y), Some(&last_y)) =
        (part.x.first(), part.x.last(), part.y.first(), part.y.last())
    else {
        return;
    };
    // A single tile along an axis has no next tile; its cell is the tile.
    let cell_width = if step_x > 0.0 {
        step_x
    } else {
        part.tile_width
    };
    let cell_height = if step_y > 0.0 {
        step_y
    } else {
        part.tile_height
    };
    let Some(tiles) = RoundedRect::rect(
        first_x as f32,
        first_y as f32,
        (last_x - first_x + part.tile_width) as f32,
        (last_y - first_y + part.tile_height) as f32,
    )
    .path() else {
        return;
    };
    let mut builder = surface.stream_builder();
    {
        let mut cell = Surface::new(builder.surface());
        draw_tile(&mut cell, part, source, image_size);
        cell.finish();
    }
    let pattern = Pattern {
        stream: builder.finish(),
        transform: Transform::from_translate(first_x as f32, first_y as f32),
        width: cell_width as f32,
        height: cell_height as f32,
    };
    surface.set_fill(Some(Fill {
        paint: pattern.into(),
        opacity: NormalizedF32::ONE,
        rule: FillRule::NonZero,
    }));
    surface.draw_path(&tiles);
    surface.set_fill(None);
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
