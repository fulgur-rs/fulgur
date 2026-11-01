//! Repeated tiles of an image, drawn one by one or through one tiling
//! pattern.

use super::shape::RoundedRect;
use super::surface::Surface;
use krilla::geom::Transform;
use krilla::num::NormalizedF32;
use krilla::paint::{Fill, FillRule, Pattern};

/// Grids with more tiles than this are drawn through one tiling pattern
/// instead of one placement per tile. A few tiles are drawn one by one,
/// which avoids the seams some viewers show between pattern cells.
const PATTERN_MIN_TILES: usize = 5;

/// The most tiles of an unevenly spaced grid drawn one by one; a larger
/// grid draws nothing.
const MAX_SINGLE_TILES: usize = 100_000;

/// Draw a tile of `width` x `height` at every position of the grid
/// `(xs, ys)`, where each position is the top left corner of a tile.
/// `draw_tile` draws one tile with its top left corner at the origin.
pub(super) fn draw(
    surface: &mut Surface<'_>,
    (xs, ys): (&[f64], &[f64]),
    (width, height): (f64, f64),
    draw_tile: &mut dyn FnMut(&mut Surface<'_>),
) {
    let count = xs.len().saturating_mul(ys.len());
    match uniform_step(xs).zip(uniform_step(ys)) {
        Some(steps) if count >= PATTERN_MIN_TILES => {
            draw_pattern(surface, (xs, ys), (width, height), steps, draw_tile);
        }
        _ if count <= MAX_SINGLE_TILES => {
            for y in ys {
                for x in xs {
                    surface.push_transform(&Transform::from_translate(*x as f32, *y as f32));
                    draw_tile(surface);
                    surface.pop();
                }
            }
        }
        _ => {}
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

/// Fill the grid with one tiling pattern whose cell holds one tile followed
/// by the gap to the next, as `space` spacing leaves (CSS Backgrounds 3
/// §2.4, §6.6).
fn draw_pattern(
    surface: &mut Surface<'_>,
    (xs, ys): (&[f64], &[f64]),
    (width, height): (f64, f64),
    (step_x, step_y): (f64, f64),
    draw_tile: &mut dyn FnMut(&mut Surface<'_>),
) {
    let (Some(&first_x), Some(&last_x), Some(&first_y), Some(&last_y)) =
        (xs.first(), xs.last(), ys.first(), ys.last())
    else {
        return;
    };
    // A single tile along an axis has no next tile; its cell is the tile.
    let cell_width = if step_x > 0.0 { step_x } else { width };
    let cell_height = if step_y > 0.0 { step_y } else { height };
    let Some(tiles) = RoundedRect::rect(
        first_x as f32,
        first_y as f32,
        (last_x - first_x + width) as f32,
        (last_y - first_y + height) as f32,
    )
    .path() else {
        return;
    };
    let mut builder = surface.stream_builder();
    {
        let mut cell = Surface::new(builder.surface());
        draw_tile(&mut cell);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_steps_need_evenly_spaced_edges() {
        assert_eq!(uniform_step(&[5.0]), Some(0.0));
        assert_eq!(uniform_step(&[0.0, 2.0, 4.0]), Some(2.0));
        assert_eq!(uniform_step(&[0.0, 2.0, 5.0]), None);
    }
}
