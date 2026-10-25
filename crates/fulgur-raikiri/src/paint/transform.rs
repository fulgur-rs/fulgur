//! Paint transforms (CSS Transforms 1): Raikiri lists the elements that its
//! painter moves from their layout location, and the text runs carry the
//! same moves.

use super::surface::Surface;
use krilla::geom::Transform;
use raikiri_html::{PaintRect, PaintTransform};

/// The Krilla transform for `transform`.
pub(super) fn to_krilla(transform: PaintTransform) -> Transform {
    Transform::from_row(
        transform.a,
        transform.b,
        transform.c,
        transform.d,
        transform.e,
        transform.f,
    )
}

/// Draw with `draw` through `transform`, if any.
pub(super) fn with<R>(
    surface: &mut Surface<'_>,
    transform: Option<PaintTransform>,
    draw: impl FnOnce(&mut Surface<'_>) -> R,
) -> R {
    let Some(transform) = transform else {
        return draw(surface);
    };
    surface.push_transform(&to_krilla(transform));
    let result = draw(surface);
    surface.pop();
    result
}

/// The inverse of `transform`, `None` when it is singular.
pub(super) fn inverse(transform: PaintTransform) -> Option<PaintTransform> {
    let PaintTransform {
        a, b, c, d, e, f, ..
    } = transform;
    let det = a * d - b * c;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let inverse = PaintTransform::new(
        d / det,
        -b / det,
        -c / det,
        a / det,
        (c * f - d * e) / det,
        (b * e - a * f) / det,
    );
    [
        inverse.a, inverse.b, inverse.c, inverse.d, inverse.e, inverse.f,
    ]
    .iter()
    .all(|value| value.is_finite())
    .then_some(inverse)
}

/// The transform from the coordinates of `run_transform` to those of
/// `space`: `None` when they are the same.
pub(super) fn relative(
    space: PaintTransform,
    run_transform: Option<PaintTransform>,
) -> Option<PaintTransform> {
    let target = run_transform.unwrap_or(PaintTransform::IDENTITY);
    if target == space {
        return None;
    }
    // A singular space draws nothing, so any placement will do.
    Some(inverse(space).map_or(target, |inverse| inverse * target))
}

/// The box around `rect` once moved by `transform`.
pub(super) fn bounds(transform: PaintTransform, rect: PaintRect) -> PaintRect {
    let corners = [
        (rect.x, rect.y),
        (rect.x + rect.width, rect.y),
        (rect.x, rect.y + rect.height),
        (rect.x + rect.width, rect.y + rect.height),
    ]
    .map(|point| transform.apply(point));
    let (mut x0, mut y0) = corners[0];
    let (mut x1, mut y1) = corners[0];
    for (x, y) in corners {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    PaintRect::new(x0, y0, x1 - x0, y1 - y0)
}

#[cfg(test)]
mod tests;
