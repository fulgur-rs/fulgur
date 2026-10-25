//! Box shadows (CSS Backgrounds 3 §7.1).
//!
//! An outer shadow is the border box grown by the spread distance and moved
//! by the offset. It is painted below the background and shows only outside
//! the border box. An inner (`inset`) shadow is painted above the background
//! and below the border, inside the padding box, where the padding box shrunk
//! by the spread distance and moved by the offset does not cover. The
//! shadows of one box are painted last declared first, so the first one is
//! on top.
//!
//! A sharp shadow is a filled path. PDF has no blur, so a blurred shadow is
//! rasterized the way blurred text shadows are.

use super::fill;
use super::shadow::{self, Budget, RASTER_SCALE, ShadowParams, SkiaPath};
use super::shape::{RoundedRect, Slice, ring};
use super::surface::Surface;
use super::text_clip::PathSink;
use krilla::geom::PathBuilder;
use krilla::paint::{Fill, FillRule};
use raikiri_html::computed::{ComputedBoxShadowItem, ComputedValues, CssColor, TextShadowColor};

/// Paint the outer shadows of the box whose border box is `border_box`.
pub(super) fn paint_outer(
    surface: &mut Surface<'_>,
    border_box: &RoundedRect,
    slice: Slice,
    style: &ComputedValues,
    budget: &mut Budget,
) {
    // An empty border box is not skipped: its spread shadow can have area.
    for item in style.box_shadow.iter().rev().filter(|item| !item.inset) {
        let Some(shadow) = Used::of(item, style.color) else {
            continue;
        };
        let shape = extended(border_box, slice, shadow.reach())
            .spread(shadow.spread)
            .moved(shadow.offset);
        if shape.is_empty() {
            continue;
        }
        // The shadow shows outside the border box only. At a broken edge
        // the fragment is a slice of the unbroken box's shadow, so the
        // shadow is cut there.
        let margin = shadow.extent() + 1.0;
        let reach = union(border_box.bounds(), shape.bounds());
        let mut visible = RoundedRect::rect(
            reach.x - margin,
            reach.y - margin,
            reach.width + 2.0 * margin,
            reach.height + 2.0 * margin,
        );
        cut(&mut visible, border_box, slice);
        // `visible` holds the shadow, so the ring is never empty.
        if let Some(clip) = ring(&visible, border_box) {
            surface.push_clip_path(&clip, &FillRule::EvenOdd);
            draw(
                surface,
                &shadow,
                FillRule::NonZero,
                |sink| shape.append_to(sink),
                budget,
            );
            surface.pop();
        }
    }
}

/// Paint the inner shadows of the box whose padding box is `padding_box`.
pub(super) fn paint_inset(
    surface: &mut Surface<'_>,
    padding_box: &RoundedRect,
    slice: Slice,
    style: &ComputedValues,
    budget: &mut Budget,
) {
    if !style.box_shadow.iter().any(|item| item.inset) {
        return;
    }
    let Some(clip) = padding_box.path() else {
        return;
    };
    for item in style.box_shadow.iter().rev().filter(|item| item.inset) {
        let Some(shadow) = Used::of(item, style.color) else {
            continue;
        };
        // The shadow covers the padding box, and the blur extent past it so
        // that the blur fades in from beyond the padding edge, except for
        // the hole. A broken edge has no shadow, as if the hole went on
        // across the break.
        let margin = shadow.extent() + 1.0;
        let bounds = padding_box.bounds();
        let area = RoundedRect::rect(
            bounds.x - margin,
            bounds.y - margin,
            bounds.width + 2.0 * margin,
            bounds.height + 2.0 * margin,
        );
        let hole = cropped(
            extended(padding_box, slice, shadow.reach())
                .spread(-shadow.spread)
                .moved(shadow.offset),
            &area,
        );
        surface.push_clip_path(&clip, &FillRule::NonZero);
        draw(
            surface,
            &shadow,
            FillRule::EvenOdd,
            |sink| {
                area.append_to(sink);
                hole.append_to(sink);
            },
            budget,
        );
        surface.pop();
    }
}

/// The used values of one shadow, in CSS px.
#[derive(Clone, Copy)]
struct Used {
    offset: (f32, f32),
    blur: f32,
    spread: f32,
    color: CssColor,
}

impl Used {
    /// `item` with `currentcolor` resolved to `current`, or `None` when it
    /// draws nothing: a transparent color, or a length that overflowed.
    fn of(item: &ComputedBoxShadowItem, current: CssColor) -> Option<Self> {
        let color = match item.color {
            TextShadowColor::Resolved(color) => color,
            _ => current,
        };
        let used = Self {
            offset: (item.offset_x.px(), item.offset_y.px()),
            blur: item.blur_radius.px().max(0.0),
            spread: item.spread_radius.px(),
            color,
        };
        let finite = [used.offset.0, used.offset.1, used.blur, used.spread]
            .iter()
            .all(|value| value.is_finite());
        (color.a != 0 && finite).then_some(used)
    }

    /// How far the blur reaches past the shape: three standard deviations,
    /// where the Gaussian has fallen below one percent.
    fn extent(&self) -> f32 {
        self.blur * 1.5
    }

    /// A distance past which a broken edge's shadow cannot reach back into
    /// the fragment.
    fn reach(&self) -> f32 {
        self.extent() + self.spread.abs() + self.offset.0.abs() + self.offset.1.abs() + 1.0
    }
}

/// Fill the path `append` writes with `shadow`'s color, blurred when it has
/// a blur radius.
fn draw(
    surface: &mut Surface<'_>,
    shadow: &Used,
    rule: FillRule,
    append: impl Fn(&mut dyn PathSink),
    budget: &mut Budget,
) {
    if shadow.blur > 0.0 {
        let mut path = SkiaPath(tiny_skia::PathBuilder::new());
        append(&mut path);
        let rule = match rule {
            FillRule::EvenOdd => tiny_skia::FillRule::EvenOdd,
            FillRule::NonZero => tiny_skia::FillRule::Winding,
        };
        let params = ShadowParams {
            offset: (0.0, 0.0),
            blur_radius: shadow.blur,
            color: shadow.color,
            scale: raster_scale(shadow.blur),
        };
        if let Some(image) = path
            .0
            .finish()
            .and_then(|path| shadow::blurred_raster(&path, rule, params, budget))
        {
            shadow::draw(surface, image);
        }
    } else {
        let mut path = PathBuilder::new();
        append(&mut path);
        if let Some(path) = path.finish() {
            surface.set_fill(Some(Fill {
                rule,
                ..fill(shadow.color)
            }));
            surface.draw_path(&path);
        }
    }
}

/// Raster pixels per CSS px for a shadow blurred by `blur`. A blurred edge
/// changes by at most 0.4 / σ of its alpha per CSS px, so a raster pixel of
/// σ / 6 keeps each step under 7% while a wide blur needs fewer pixels.
fn raster_scale(blur: f32) -> f32 {
    let sigma = blur / 2.0;
    (6.0 / sigma).clamp(1.0, RASTER_SCALE)
}

/// `shape` with its broken edges moved outward by `by`.
fn extended(shape: &RoundedRect, slice: Slice, by: f32) -> RoundedRect {
    let mut shape = *shape;
    if slice.top {
        shape.y -= by;
        shape.height += by;
    }
    if slice.bottom {
        shape.height += by;
    }
    if slice.left {
        shape.x -= by;
        shape.width += by;
    }
    if slice.right {
        shape.width += by;
    }
    shape
}

/// Move the broken edges of `area` onto those of `fragment`.
fn cut(area: &mut RoundedRect, fragment: &RoundedRect, slice: Slice) {
    let right = area.x + area.width;
    let bottom = area.y + area.height;
    if slice.top {
        area.y = fragment.y;
    }
    if slice.bottom {
        area.height = fragment.y + fragment.height - area.y;
    } else {
        area.height = bottom - area.y;
    }
    if slice.left {
        area.x = fragment.x;
    }
    if slice.right {
        area.width = fragment.x + fragment.width - area.x;
    } else {
        area.width = right - area.x;
    }
}

/// `shape` cut to `area`, with square corners on the cut edges. An empty
/// shape when they do not overlap.
fn cropped(shape: RoundedRect, area: &RoundedRect) -> RoundedRect {
    let left = shape.x.max(area.x);
    let top = shape.y.max(area.y);
    let right = (shape.x + shape.width).min(area.x + area.width);
    let bottom = (shape.y + shape.height).min(area.y + area.height);
    let mut out = RoundedRect {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
        radii: shape.radii,
    };
    let cut = [
        top > shape.y,
        right < shape.x + shape.width,
        bottom < shape.y + shape.height,
        left > shape.x,
    ];
    // Corners in order top-left, top-right, bottom-right, bottom-left, and
    // the two edges each one joins.
    for (corner, (a, b)) in [(0, 3), (0, 1), (1, 2), (2, 3)].into_iter().enumerate() {
        if cut[a] || cut[b] {
            out.radii[corner] = [0.0; 2];
        }
    }
    out
}

/// The smallest rectangle holding `a` and `b`.
fn union(a: raikiri_html::PaintRect, b: raikiri_html::PaintRect) -> raikiri_html::PaintRect {
    let left = a.x.min(b.x);
    let top = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    raikiri_html::PaintRect::new(left, top, right - left, bottom - top)
}

impl RoundedRect {
    /// This shape grown by `by` on every side (shrunk when negative), with
    /// the corner radii grown with it (CSS Backgrounds 3 §7.1.1). A radius
    /// smaller than the growth grows by less, so that a sharp corner stays
    /// sharp and the change from round to sharp is continuous.
    fn spread(&self, by: f32) -> Self {
        let width = self.width + 2.0 * by;
        let height = self.height + 2.0 * by;
        if !(width > 0.0 && height > 0.0) {
            return Self::rect(self.x, self.y, 0.0, 0.0);
        }
        let radius = |r: f32| {
            if r <= 0.0 {
                0.0
            } else if by > 0.0 {
                let ratio = r / by;
                let proportion = if ratio < 1.0 {
                    1.0 + (ratio - 1.0).powi(3)
                } else {
                    1.0
                };
                r + by * proportion
            } else {
                (r + by).max(0.0)
            }
        };
        Self {
            x: self.x - by,
            y: self.y - by,
            width,
            height,
            radii: self.radii.map(|corner| {
                let corner = corner.map(radius);
                if corner[0] == 0.0 || corner[1] == 0.0 {
                    [0.0; 2]
                } else {
                    corner
                }
            }),
        }
    }

    /// This shape moved by `offset`.
    fn moved(mut self, offset: (f32, f32)) -> Self {
        self.x += offset.0;
        self.y += offset.1;
        self
    }
}

#[cfg(test)]
mod tests;
