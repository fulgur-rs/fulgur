//! Border painting (CSS Backgrounds 3 §3 and §4.4).
//!
//! Each side is painted in its own region: the part of the border area
//! between the lines that join the outer corners of the border box to the
//! inner corners of the padding box. Corner curves come from the shared
//! outer and inner rounded rectangles, so every style follows them.

use super::fill;
use super::shape::{Edges, RoundedRect, polygon, ring};
use krilla::color::rgb;
use krilla::geom::{Path, PathBuilder};
use krilla::num::NormalizedF32;
use krilla::paint::{FillRule, LineCap, Stroke, StrokeDash};
use krilla::surface::Surface;
use raikiri_html::computed::{BorderColor, BorderStyle, ComputedValues, CssColor};

/// The used width, style and color of one border side.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Side {
    width: f32,
    style: BorderStyle,
    color: CssColor,
}

impl Side {
    fn is_visible(&self) -> bool {
        self.width > 0.0
            && self.color.a > 0
            && !matches!(self.style, BorderStyle::None | BorderStyle::Hidden)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Position {
    Top,
    Right,
    Bottom,
    Left,
}

impl Position {
    fn is_top_or_left(self) -> bool {
        matches!(self, Position::Top | Position::Left)
    }
}

/// The used border widths of `style`: zero for `none` and `hidden`.
pub(super) fn widths(style: &ComputedValues) -> Edges {
    let width = |border: &raikiri_html::computed::ComputedBorder| {
        if matches!(border.style(), BorderStyle::None | BorderStyle::Hidden) {
            0.0
        } else {
            border.width().px().max(0.0)
        }
    };
    Edges {
        top: width(&style.border.top),
        right: width(&style.border.right),
        bottom: width(&style.border.bottom),
        left: width(&style.border.left),
    }
}

/// Draw the four borders of a box whose border box is `outer`.
pub(super) fn paint_borders(
    surface: &mut Surface<'_>,
    outer: &RoundedRect,
    style: &ComputedValues,
) {
    let widths = widths(style);
    let side = |border: &raikiri_html::computed::ComputedBorder, width: f32| Side {
        width,
        style: border.style(),
        color: resolve_border_color(border.color, style.color),
    };
    let sides = [
        (Position::Top, side(&style.border.top, widths.top)),
        (Position::Right, side(&style.border.right, widths.right)),
        (Position::Bottom, side(&style.border.bottom, widths.bottom)),
        (Position::Left, side(&style.border.left, widths.left)),
    ];
    if sides.iter().all(|(_, side)| !side.is_visible()) || outer.is_empty() {
        return;
    }
    let inner = outer.inset(widths);

    // Four identical sides whose style does not shade per side are drawn as
    // one shape, which leaves no seams at the corner joins.
    let first = sides[0].1;
    if sides.iter().all(|(_, side)| *side == first) {
        match first.style {
            BorderStyle::Solid => return fill_ring(surface, outer, &inner, first.color),
            BorderStyle::Double => return paint_double(surface, outer, widths, first.color),
            BorderStyle::Dashed | BorderStyle::Dotted if outer.is_rounded() => {
                return stroke_rounded(surface, outer, widths, first);
            }
            _ => {}
        }
    }

    for (position, side) in sides {
        if !side.is_visible() {
            continue;
        }
        let Some(region) = side_region(outer, &inner, position) else {
            continue;
        };
        surface.push_clip_path(&region, &FillRule::NonZero);
        paint_side(surface, outer, &inner, widths, position, side);
        surface.pop();
    }
}

fn resolve_border_color(color: BorderColor, current: CssColor) -> CssColor {
    match color {
        BorderColor::Resolved(color) => color,
        // `currentcolor`, and any color form this painter does not know yet.
        _ => current,
    }
}

/// The quadrilateral that owns one side: its outer edge plus the lines from
/// the outer corners to the inner corners.
fn side_region(outer: &RoundedRect, inner: &RoundedRect, position: Position) -> Option<Path> {
    let (ox0, oy0) = (outer.x, outer.y);
    let (ox1, oy1) = (outer.x + outer.width, outer.y + outer.height);
    let (ix0, iy0) = (inner.x, inner.y);
    let (ix1, iy1) = (inner.x + inner.width, inner.y + inner.height);
    let points = match position {
        Position::Top => [(ox0, oy0), (ox1, oy0), (ix1, iy0), (ix0, iy0)],
        Position::Right => [(ox1, oy0), (ox1, oy1), (ix1, iy1), (ix1, iy0)],
        Position::Bottom => [(ox1, oy1), (ox0, oy1), (ix0, iy1), (ix1, iy1)],
        Position::Left => [(ox0, oy1), (ox0, oy0), (ix0, iy0), (ix0, iy1)],
    };
    polygon(&points)
}

/// Paint one side inside its region (already clipped).
fn paint_side(
    surface: &mut Surface<'_>,
    outer: &RoundedRect,
    inner: &RoundedRect,
    widths: Edges,
    position: Position,
    side: Side,
) {
    match side.style {
        BorderStyle::Double => paint_double(surface, outer, widths, side.color),
        // CSS Backgrounds 3 §3.2 leaves the colors of the 3D styles to the
        // UA; the lit and shaded halves follow common browser practice.
        BorderStyle::Groove | BorderStyle::Ridge => {
            let middle = outer.inset(widths.scale(0.5));
            let (light, dark) = (lighten(side.color), darken(side.color));
            // Groove looks carved in: the outer half of the top and left
            // sides is shaded. Ridge is the reverse.
            let outer_dark = (side.style == BorderStyle::Groove) == position.is_top_or_left();
            let (outer_color, inner_color) = if outer_dark {
                (dark, light)
            } else {
                (light, dark)
            };
            fill_ring(surface, outer, &middle, outer_color);
            fill_ring(surface, &middle, inner, inner_color);
        }
        BorderStyle::Inset | BorderStyle::Outset => {
            let shaded = (side.style == BorderStyle::Inset) == position.is_top_or_left();
            let color = if shaded {
                darken(side.color)
            } else {
                lighten(side.color)
            };
            fill_ring(surface, outer, inner, color);
        }
        BorderStyle::Dashed | BorderStyle::Dotted => {
            if outer.is_rounded() {
                // The dashes follow the corner curves; the ring keeps the
                // strokes of a wider neighbor out of this side's area.
                let Some(area) = ring(outer, inner) else {
                    return;
                };
                surface.push_clip_path(&area, &FillRule::EvenOdd);
                stroke_rounded(surface, outer, widths, side);
                surface.pop();
            } else {
                stroke_straight(surface, outer, widths, position, side);
            }
        }
        // `solid`, and any style this painter does not know yet.
        _ => fill_ring(surface, outer, inner, side.color),
    }
}

fn fill_ring(surface: &mut Surface<'_>, outer: &RoundedRect, inner: &RoundedRect, color: CssColor) {
    if color.a == 0 {
        return;
    }
    if let Some(path) = ring(outer, inner) {
        surface.set_fill(Some(krilla::paint::Fill {
            rule: FillRule::EvenOdd,
            ..fill(color)
        }));
        surface.draw_path(&path);
    }
}

/// `double`: two lines with a gap between them, the three taking a third of
/// the border width each. Below 3px there is no room for a visible gap, so
/// the side is drawn solid, as browsers do.
fn paint_double(surface: &mut Surface<'_>, outer: &RoundedRect, widths: Edges, color: CssColor) {
    let inner = outer.inset(widths);
    let max_width = widths
        .top
        .max(widths.right)
        .max(widths.bottom)
        .max(widths.left);
    if max_width < 3.0 {
        return fill_ring(surface, outer, &inner, color);
    }
    let first = outer.inset(widths.scale(1.0 / 3.0));
    let second = outer.inset(widths.scale(2.0 / 3.0));
    fill_ring(surface, outer, &first, color);
    fill_ring(surface, &second, &inner, color);
}

/// Dash pattern for a stroke of `width`. CSS Backgrounds 3 §3.2 leaves the
/// dash and gap lengths to the UA: dashes are three widths long with equal
/// gaps, dots are round with one width between them.
fn dash_stroke(side: Side, dash: f32, gap: f32) -> Stroke {
    let dotted = side.style == BorderStyle::Dotted;
    Stroke {
        paint: rgb::Color::new(side.color.r, side.color.g, side.color.b).into(),
        width: side.width,
        opacity: NormalizedF32::new(f32::from(side.color.a) / 255.0).unwrap_or(NormalizedF32::ONE),
        line_cap: if dotted {
            LineCap::Round
        } else {
            LineCap::Butt
        },
        dash: Some(StrokeDash {
            array: vec![dash, gap],
            offset: 0.0,
        }),
        ..Default::default()
    }
}

/// Stroke the center line of the border area of a rounded box. Used both
/// for four identical sides and, clipped, for one side.
fn stroke_rounded(surface: &mut Surface<'_>, outer: &RoundedRect, widths: Edges, side: Side) {
    let center = outer.inset(widths.scale(0.5));
    let Some(path) = center.path() else {
        return;
    };
    // Fit a whole number of dashes or dots around the closed outline, so
    // the pattern does not collide where it starts and ends.
    let w = side.width;
    let perimeter = perimeter(&center);
    let stroke = if side.style == BorderStyle::Dotted {
        let count = (perimeter / (2.0 * w)).round().max(1.0);
        dash_stroke(side, 0.0, perimeter / count)
    } else {
        let count = (perimeter / (6.0 * w)).round().max(1.0);
        let period = perimeter / count;
        dash_stroke(side, period / 2.0, period / 2.0)
    };
    surface.set_fill(None);
    surface.set_stroke(Some(stroke));
    surface.draw_path(&path);
    surface.set_stroke(None);
}

/// Length of the outline of `shape`, with Ramanujan's approximation for the
/// quarter ellipses.
fn perimeter(shape: &RoundedRect) -> f32 {
    let mut length = 2.0 * (shape.width + shape.height);
    for [rx, ry] in shape.radii {
        if rx > 0.0 && ry > 0.0 {
            let h = ((rx - ry) / (rx + ry)).powi(2);
            let ellipse = std::f32::consts::PI
                * (rx + ry)
                * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt()));
            length += ellipse / 4.0 - rx - ry;
        }
    }
    length
}

/// Stroke one straight side, spacing the dashes or dots so that the side
/// starts and ends with one.
fn stroke_straight(
    surface: &mut Surface<'_>,
    outer: &RoundedRect,
    widths: Edges,
    position: Position,
    side: Side,
) {
    let w = side.width;
    let (x0, y0) = (outer.x, outer.y);
    let (x1, y1) = (outer.x + outer.width, outer.y + outer.height);
    // The side's center line from one corner to the other, along the side.
    // Dots start at the center of each corner square; dashes at the edge.
    let dotted = side.style == BorderStyle::Dotted;
    let ((ax, ay), (bx, by)) = match position {
        Position::Top => {
            let y = y0 + w / 2.0;
            if dotted {
                ((x0 + widths.left / 2.0, y), (x1 - widths.right / 2.0, y))
            } else {
                ((x0, y), (x1, y))
            }
        }
        Position::Bottom => {
            let y = y1 - w / 2.0;
            if dotted {
                ((x0 + widths.left / 2.0, y), (x1 - widths.right / 2.0, y))
            } else {
                ((x0, y), (x1, y))
            }
        }
        Position::Left => {
            let x = x0 + w / 2.0;
            if dotted {
                ((x, y0 + widths.top / 2.0), (x, y1 - widths.bottom / 2.0))
            } else {
                ((x, y0), (x, y1))
            }
        }
        Position::Right => {
            let x = x1 - w / 2.0;
            if dotted {
                ((x, y0 + widths.top / 2.0), (x, y1 - widths.bottom / 2.0))
            } else {
                ((x, y0), (x, y1))
            }
        }
    };
    let length = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
    let (mut bx, mut by) = (bx, by);
    let stroke = if dotted {
        // `n` dots with equal spacing, one at each end. The line goes on
        // for half a space so that rounding cannot drop the last dot.
        let count = (length / (2.0 * w)).round().max(1.0);
        let spacing = length / count;
        if length > 0.0 {
            bx += (bx - ax) / length * spacing / 2.0;
            by += (by - ay) / length * spacing / 2.0;
        }
        dash_stroke(side, 0.0, spacing)
    } else {
        // `n` dashes and `n - 1` gaps; the gaps absorb the remainder.
        let dash = 3.0 * w;
        let count = ((length + dash) / (2.0 * dash)).round().max(1.0);
        if count < 2.0 {
            dash_stroke(side, length, 0.0)
        } else {
            dash_stroke(side, dash, (length - count * dash) / (count - 1.0))
        }
    };
    let mut builder = PathBuilder::new();
    builder.move_to(ax, ay);
    builder.line_to(bx, by);
    if let Some(path) = builder.finish() {
        surface.set_fill(None);
        surface.set_stroke(Some(stroke));
        surface.draw_path(&path);
        surface.set_stroke(None);
    }
}

fn lighten(color: CssColor) -> CssColor {
    let mix = |c: u8| c + ((255 - c) / 2);
    CssColor {
        r: mix(color.r),
        g: mix(color.g),
        b: mix(color.b),
        ..color
    }
}

fn darken(color: CssColor) -> CssColor {
    let mix = |c: u8| c / 2;
    CssColor {
        r: mix(color.r),
        g: mix(color.g),
        b: mix(color.b),
        ..color
    }
}
