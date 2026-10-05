//! Gradient background images (CSS Images 3 §3) as Krilla shadings.
//!
//! The gradient box is the background positioning area. Colors are
//! interpolated in sRGB, the PDF shading's color space; the
//! `<color-interpolation-method>` of CSS Images 4 is not applied.

use super::shape::RoundedRect;
use krilla::color::rgb;
use krilla::geom::Transform;
use krilla::num::NormalizedF32;
use krilla::paint::{
    LinearGradient as KrillaLinear, Paint, RadialGradient as KrillaRadial, SpreadMethod, Stop,
    SweepGradient,
};
use raikiri_html::computed::{
    AnglePercentage, AngularColorStop, ComputedGradient, ConicGradient, CssColor, CssPosition,
    CssPositionOffset, GradientColorStop, GradientStopColor, HorizontalSide, Length,
    LinearGradient, LinearGradientDirection, RadialExtent, RadialGradient, RadialShape, RadialSize,
    VerticalSide,
};

/// A Krilla paint for `gradient` drawn in `area` (the gradient box), or
/// `None` when there is nothing to draw. `current` resolves
/// `currentcolor` stops.
pub(super) fn paint(
    gradient: &ComputedGradient,
    area: &RoundedRect,
    current: CssColor,
) -> Option<Paint> {
    if area.is_empty() {
        return None;
    }
    match gradient {
        ComputedGradient::Linear(linear) => linear_paint(linear, area, current),
        ComputedGradient::Radial(radial) => radial_paint(radial, area, current),
        ComputedGradient::Conic(conic) => conic_paint(conic, area, current),
        _ => None,
    }
}

/// CSS Images 3 §3.1.1: the gradient line passes through the center of the
/// box at the gradient angle, and its length makes the corners in the
/// direction of the line (and its opposite) reach the 100% (and 0%) stop.
fn linear_paint(gradient: &LinearGradient, area: &RoundedRect, current: CssColor) -> Option<Paint> {
    let (w, h) = (area.width, area.height);
    // Unit direction of the line in y-down space.
    let (dx, dy) = match gradient.direction {
        // 0deg points up and angles grow clockwise.
        LinearGradientDirection::Angle(angle) => {
            let radians = angle.0.to_radians();
            (radians.sin(), -radians.cos())
        }
        LinearGradientDirection::Side(side) => {
            let sx = match side.horizontal {
                Some(HorizontalSide::Left) => -1.0,
                Some(HorizontalSide::Right) => 1.0,
                _ => 0.0,
            };
            let sy = match side.vertical {
                Some(VerticalSide::Top) => -1.0,
                Some(VerticalSide::Bottom) => 1.0,
                _ => 0.0,
            };
            if sx != 0.0 && sy != 0.0 {
                // A corner: the angle that makes the 50% line join the two
                // neighboring corners, so the line is perpendicular to that
                // diagonal and points into the named quadrant.
                let length = (w * w + h * h).sqrt();
                if length <= 0.0 {
                    return None;
                }
                (sx * h / length, sy * w / length)
            } else if sx == 0.0 && sy == 0.0 {
                // No side at all; `to bottom` is the default direction.
                (0.0, 1.0)
            } else {
                (sx, sy)
            }
        }
        _ => (0.0, 1.0),
    };
    let length = (w * dx).abs() + (h * dy).abs();
    if length <= 0.0 {
        return None;
    }
    let (cx, cy) = (area.x + w / 2.0, area.y + h / 2.0);
    let start = (cx - dx * length / 2.0, cy - dy * length / 2.0);
    let positions = resolve_positions(&gradient.stops, length);
    let colors: Vec<CssColor> = gradient
        .stops
        .iter()
        .map(|stop| stop_color(stop.color, current))
        .collect();
    // The line runs from corner to corner, so one repetition of it covers
    // the box.
    let repeat = gradient.repeating.then_some((0.0, 1.0));
    let line = ColorLine::new(&positions, &colors, repeat, ALL)?;
    let at = |t: f32| (start.0 + dx * length * t, start.1 + dy * length * t);
    let (x1, y1) = at(line.start);
    let (x2, y2) = at(line.end);
    Some(
        KrillaLinear {
            x1,
            y1,
            x2,
            y2,
            transform: Transform::identity(),
            spread_method: SpreadMethod::Pad,
            stops: line.stops,
            anti_alias: false,
        }
        .into(),
    )
}

/// CSS Images 3 §3.2. PDF radial shadings are circular, so an ellipse is a
/// circle of the horizontal radius scaled vertically about the center.
fn radial_paint(gradient: &RadialGradient, area: &RoundedRect, current: CssColor) -> Option<Paint> {
    let (w, h) = (area.width, area.height);
    let (px, py) = resolve_position(gradient.position, w, h);
    let (rx, ry) = radial_size(gradient.shape, gradient.size, px, py, w, h);
    if !(rx > 0.0 && ry > 0.0) {
        // §3.2.3 degenerate ending shapes: draw the last color.
        let last = gradient.stops.last()?;
        return Some(solid(stop_color(last.color, current)));
    }
    let positions = resolve_positions(&gradient.stops, rx);
    let colors: Vec<CssColor> = gradient
        .stops
        .iter()
        .map(|stop| stop_color(stop.color, current))
        .collect();
    // Repetitions reach from the center to the farthest corner, in units of
    // the ending shape.
    let reach = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)]
        .iter()
        .map(|(x, y)| ((x - px) / rx).hypot((y - py) / ry))
        .fold(0.0, f32::max);
    let repeat = gradient.repeating.then_some((0.0, reach));
    let line = ColorLine::new(&positions, &colors, repeat, (0.0, f32::INFINITY))?;
    let (cx, cy) = (area.x + px, area.y + py);
    // Scale y by `ry / rx` about the center.
    let scale = ry / rx;
    let transform = Transform::from_row(1.0, 0.0, 0.0, scale, 0.0, cy * (1.0 - scale));
    Some(
        KrillaRadial {
            fx: cx,
            fy: cy,
            fr: (line.start * rx).max(0.0),
            cx,
            cy,
            cr: line.end * rx,
            transform,
            spread_method: SpreadMethod::Pad,
            stops: line.stops,
            anti_alias: false,
        }
        .into(),
    )
}

/// CSS Images 3 §3.2.2 ending-shape size, as `(rx, ry)`. `(px, py)` is the
/// center relative to the gradient box of `w` x `h`.
fn radial_size(
    shape: RadialShape,
    size: RadialSize,
    px: f32,
    py: f32,
    w: f32,
    h: f32,
) -> (f32, f32) {
    let (near_x, far_x) = (px.abs().min((w - px).abs()), px.abs().max((w - px).abs()));
    let (near_y, far_y) = (py.abs().min((h - py).abs()), py.abs().max((h - py).abs()));
    let circle = matches!(shape, RadialShape::Circle);
    match size {
        RadialSize::Circle(radius) => {
            let r = resolve_length(radius, 0.0);
            (r, r)
        }
        RadialSize::Ellipse(x, y) => (resolve_length(x, w), resolve_length(y, h)),
        RadialSize::Extent(extent) => match (extent, circle) {
            (RadialExtent::ClosestSide, true) => {
                let r = near_x.min(near_y);
                (r, r)
            }
            (RadialExtent::FarthestSide, true) => {
                let r = far_x.max(far_y);
                (r, r)
            }
            (RadialExtent::ClosestCorner, true) => {
                let r = near_x.hypot(near_y);
                (r, r)
            }
            (RadialExtent::ClosestSide, false) => (near_x, near_y),
            (RadialExtent::FarthestSide, false) => (far_x, far_y),
            // The ellipse keeps the aspect ratio of the matching `-side`
            // ellipse and passes through the corner, which scales both
            // radii by sqrt(2).
            (RadialExtent::ClosestCorner, false) => (
                near_x * std::f32::consts::SQRT_2,
                near_y * std::f32::consts::SQRT_2,
            ),
            (_, false) => (
                far_x * std::f32::consts::SQRT_2,
                far_y * std::f32::consts::SQRT_2,
            ),
            // `farthest-corner`, the default.
            (_, true) => {
                let r = far_x.hypot(far_y);
                (r, r)
            }
        },
        _ => {
            let r = far_x.hypot(far_y);
            (r, r)
        }
    }
}

/// CSS Images 4 §3.3 conic gradients: 0deg points up and angles grow
/// clockwise. Krilla's sweep angles start at the positive x axis and grow
/// towards the positive y axis, which in this y-down space is also
/// clockwise, so the two differ by a quarter turn.
fn conic_paint(gradient: &ConicGradient, area: &RoundedRect, current: CssColor) -> Option<Paint> {
    let (px, py) = resolve_position(gradient.position, area.width, area.height);
    let positions = resolve_angular_positions(&gradient.stops);
    let colors: Vec<CssColor> = gradient
        .stops
        .iter()
        .map(|stop| stop_color(stop.color, current))
        .collect();
    // Krilla draws every sweep with a PostScript function, which repeats
    // the stops itself; expanding them would exceed the stop count it
    // supports. A non-repeating gradient shows one turn.
    let visible = if gradient.repeating { ALL } else { (0.0, 1.0) };
    let line = ColorLine::new(&positions, &colors, None, visible)?;
    let (cx, cy) = (area.x + px, area.y + py);
    Some(
        SweepGradient {
            cx,
            cy,
            // The sweep's angles run over [0, 360) from the positive x
            // axis, so the gradient is rotated to start there instead of
            // having a domain that crosses the wrap.
            start_angle: line.start * 360.0,
            end_angle: line.end * 360.0,
            transform: Transform::from_rotate_at(gradient.angle.0 - 90.0, cx, cy),
            spread_method: if gradient.repeating {
                SpreadMethod::Repeat
            } else {
                SpreadMethod::Pad
            },
            stops: line.stops,
            anti_alias: false,
        }
        .into(),
    )
}

fn solid(color: CssColor) -> Paint {
    rgb::Color::new(color.r, color.g, color.b).into()
}

fn stop_color(color: GradientStopColor, current: CssColor) -> CssColor {
    match color {
        GradientStopColor::Resolved(color) => color,
        // `currentcolor`, and any color form this painter does not know yet.
        _ => current,
    }
}

/// A length inside a gradient. The cascade leaves only `px` and
/// percentages there; a percentage refers to `basis`.
fn resolve_length(length: Length, basis: f32) -> f32 {
    match length {
        Length::Px(px) => px,
        Length::Percent(percent) => basis * percent / 100.0,
        _ => 0.0,
    }
}

/// A `<position>` in a box of `w` x `h`, from its top-left corner.
fn resolve_position(position: CssPosition, w: f32, h: f32) -> (f32, f32) {
    let axis = |offset: CssPositionOffset, size: f32| match offset {
        CssPositionOffset::Start(length) => resolve_length(length, size),
        CssPositionOffset::End(length) => size - resolve_length(length, size),
        _ => size / 2.0,
    };
    (axis(position.horizontal, w), axis(position.vertical, h))
}

/// Stop positions as fractions of the gradient line of `length` px, before
/// fix-up (`None` where the author gave none).
fn resolve_positions(stops: &[GradientColorStop], length: f32) -> Vec<Option<f32>> {
    stops
        .iter()
        .map(|stop| {
            stop.position.map(|position| match position {
                Length::Percent(percent) => percent / 100.0,
                other => resolve_length(other, length) / length,
            })
        })
        .collect()
}

/// Conic stop positions as fractions of a full turn.
fn resolve_angular_positions(stops: &[AngularColorStop]) -> Vec<Option<f32>> {
    stops
        .iter()
        .map(|stop| {
            stop.position.map(|position| match position {
                AnglePercentage::Angle(angle) => angle.0 / 360.0,
                AnglePercentage::Percent(percent) => percent / 100.0,
                _ => 0.0,
            })
        })
        .collect()
}

/// The whole gradient line.
const ALL: (f32, f32) = (f32::NEG_INFINITY, f32::INFINITY);

/// Color stops ready for a PDF shading: offsets in [0, 1] along the
/// segment from `start` to `end`, expressed as fractions of the gradient
/// line. The shading pads: its end colors extend beyond the segment.
struct ColorLine {
    start: f32,
    end: f32,
    stops: Vec<Stop>,
}

/// Upper bound on the stops a repeating gradient expands to.
const MAX_REPEATED_STOPS: usize = 4096;

impl ColorLine {
    /// CSS Images 3 §3.4.3 stop fix-up, then a remap of the stops to the
    /// span between the first and the last one, since PDF stop offsets
    /// cannot leave [0, 1].
    ///
    /// `repeat` holds the part of the gradient line a repeating gradient
    /// must cover. Its stops are repeated across that part (§3.4.4) rather
    /// than left to a repeating shading: Krilla writes those as PostScript
    /// functions, which PDF/A forbids and some viewers draw wrongly, and has
    /// none for radial shadings.
    ///
    /// `visible` is the part of the line that can be seen; the stops are
    /// cut to it, keeping the colors the line has at its ends. A radial
    /// gradient has no negative radii, and a conic one no angle outside its
    /// turn.
    ///
    /// Translucent stops are expanded so that the shading, which
    /// interpolates color and opacity separately, reproduces interpolation
    /// in premultiplied alpha (CSS Images 4 §3.4.4, CSS Color 4 §12.3).
    fn new(
        positions: &[Option<f32>],
        colors: &[CssColor],
        repeat: Option<(f32, f32)>,
        visible: (f32, f32),
    ) -> Option<Self> {
        if colors.is_empty() || positions.len() != colors.len() {
            return None;
        }
        let mut fixed = fix_up(positions);
        let mut colors = colors.to_vec();
        if let Some((from, to)) = repeat {
            let (first, last) = (fixed[0], fixed[fixed.len() - 1]);
            let period = last - first;
            let first_repeat = ((from - first) / period).floor();
            let last_repeat = ((to - first) / period).ceil();
            let repeats = last_repeat - first_repeat;
            if period.is_nan()
                || period <= f32::EPSILON
                || !repeats.is_finite()
                || repeats * fixed.len() as f32 > MAX_REPEATED_STOPS as f32
            {
                // §3.4.4: a zero-length repeating gradient is drawn as its
                // average color; the last color stands in for it, as it
                // does for repetitions too fine to tell apart.
                return Some(Self::flat(*colors.last()?));
            }
            let mut expanded = Vec::new();
            let mut expanded_colors = Vec::new();
            let mut repeat_index = first_repeat;
            while repeat_index < last_repeat {
                for (position, color) in fixed.iter().zip(&colors) {
                    expanded.push(position + repeat_index * period);
                    expanded_colors.push(*color);
                }
                repeat_index += 1.0;
            }
            fixed = expanded;
            colors = expanded_colors;
        }
        (fixed, colors) = trim_below(&fixed, &colors, visible.0);
        (fixed, colors) = trim_above(&fixed, &colors, visible.1);
        (fixed, colors) = premultiplied(&fixed, &colors);
        let (mut start, mut end) = (fixed[0], fixed[fixed.len() - 1]);
        if end - start <= f32::EPSILON {
            // Every stop at one point: a hard edge from the first color to
            // the last, on a segment just long enough to hold it.
            start -= 1e-4;
            end += 1e-4;
        }
        let span = end - start;
        let mut offsets: Vec<f32> = fixed
            .iter()
            .map(|position| ((position - start) / span).clamp(0.0, 1.0))
            .collect();
        // Stops at one position make a hard edge; give each a distinct
        // offset so every stitched segment has a length.
        let step = (1e-3 / offsets.len() as f32).min(1e-5);
        for index in 1..offsets.len() {
            offsets[index] = offsets[index].max(offsets[index - 1] + step);
        }
        let mut limit = 1.0;
        for offset in offsets.iter_mut().rev() {
            *offset = offset.min(limit);
            limit = *offset - step;
        }
        let stops = offsets
            .iter()
            .zip(&colors)
            .map(|(offset, color)| stop(*offset, *color))
            .collect();
        Some(Self { start, end, stops })
    }

    fn flat(color: CssColor) -> Self {
        Self {
            start: 0.0,
            end: 1.0,
            stops: vec![stop(0.0, color), stop(1.0, color)],
        }
    }
}

/// The stops from `floor` on, with the color the line has at `floor` as
/// the first one.
fn trim_below(positions: &[f32], colors: &[CssColor], floor: f32) -> (Vec<f32>, Vec<CssColor>) {
    let first_kept = positions.iter().position(|position| *position >= floor);
    let Some(index) = first_kept else {
        // The whole line is before the floor: its last color remains.
        let last = colors[colors.len() - 1];
        return (vec![floor], vec![last]);
    };
    if index == 0 {
        return (positions.to_vec(), colors.to_vec());
    }
    let (p0, p1) = (positions[index - 1], positions[index]);
    let t = if p1 > p0 {
        (floor - p0) / (p1 - p0)
    } else {
        1.0
    };
    let at_floor = mix_premultiplied(colors[index - 1], colors[index], t);
    let mut kept_positions = vec![floor];
    kept_positions.extend_from_slice(&positions[index..]);
    let mut kept_colors = vec![at_floor];
    kept_colors.extend_from_slice(&colors[index..]);
    (kept_positions, kept_colors)
}

/// The stops up to `ceiling`, with the color the line has at `ceiling` as
/// the last one.
fn trim_above(positions: &[f32], colors: &[CssColor], ceiling: f32) -> (Vec<f32>, Vec<CssColor>) {
    // Trim the mirrored line from below.
    let mirrored: Vec<f32> = positions.iter().rev().map(|position| -position).collect();
    let reversed: Vec<CssColor> = colors.iter().rev().copied().collect();
    let (positions, colors) = trim_below(&mirrored, &reversed, -ceiling);
    (
        positions.iter().rev().map(|position| -position).collect(),
        colors.into_iter().rev().collect(),
    )
}

/// Steps that approximate the color curve between two stops whose
/// opacities differ and are both above zero.
const ALPHA_STEPS: u8 = 8;

/// Stops whose straight (color, opacity) interpolation matches the
/// premultiplied interpolation of the given ones. Between stops of equal
/// opacity the two agree. Next to a transparent stop the premultiplied
/// color is the other stop's color, so the transparent stop takes that
/// color, once for each side. Between two other opacities the color is not
/// linear and is sampled.
fn premultiplied(positions: &[f32], colors: &[CssColor]) -> (Vec<f32>, Vec<CssColor>) {
    if colors.iter().all(|color| color.a == colors[0].a) {
        return (positions.to_vec(), colors.to_vec());
    }
    let mut out_positions = vec![positions[0]];
    let mut out_colors = vec![mix_premultiplied(colors[0], colors[1], 0.0)];
    for index in 0..colors.len() - 1 {
        let (a, b) = (colors[index], colors[index + 1]);
        let (p0, p1) = (positions[index], positions[index + 1]);
        let steps = if a.a == b.a || a.a == 0 || b.a == 0 {
            1
        } else {
            ALPHA_STEPS
        };
        for step in 0..=steps {
            let t = f32::from(step) / f32::from(steps);
            let position = p0 + (p1 - p0) * t;
            let color = mix_premultiplied(a, b, t);
            let last = out_colors.len() - 1;
            if out_positions[last] == position && out_colors[last] == color {
                continue;
            }
            out_positions.push(position);
            out_colors.push(color);
        }
    }
    (out_positions, out_colors)
}

/// The color a fraction `t` of the way from `a` to `b`, interpolated with
/// premultiplied alpha. Where the result is transparent next to an opaque
/// end, it takes that end's color, the limit of the premultiplied color.
fn mix_premultiplied(a: CssColor, b: CssColor, t: f32) -> CssColor {
    let (alpha_a, alpha_b) = (f32::from(a.a) / 255.0, f32::from(b.a) / 255.0);
    let alpha = alpha_a + (alpha_b - alpha_a) * t;
    let channel = |ca: u8, cb: u8| {
        let (ca, cb) = (f32::from(ca), f32::from(cb));
        let value = if alpha > 0.0 {
            (ca * alpha_a * (1.0 - t) + cb * alpha_b * t) / alpha
        } else if alpha_a > 0.0 {
            ca
        } else if alpha_b > 0.0 {
            cb
        } else {
            ca + (cb - ca) * t
        };
        value.round().clamp(0.0, 255.0) as u8
    };
    CssColor {
        r: channel(a.r, b.r),
        g: channel(a.g, b.g),
        b: channel(a.b, b.b),
        a: (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    }
}

fn stop(offset: f32, color: CssColor) -> Stop {
    Stop {
        offset: NormalizedF32::new(offset.clamp(0.0, 1.0)).unwrap_or(NormalizedF32::ZERO),
        color: rgb::Color::new(color.r, color.g, color.b).into(),
        opacity: NormalizedF32::new(f32::from(color.a) / 255.0).unwrap_or(NormalizedF32::ONE),
    }
}

/// CSS Images 3 §3.4.3 "Color Stop Fixup": a missing first or last
/// position becomes 0% or 100%, a position before an earlier one moves up
/// to it, and runs of missing positions are spread evenly between their
/// neighbors.
fn fix_up(positions: &[Option<f32>]) -> Vec<f32> {
    let count = positions.len();
    let mut fixed: Vec<Option<f32>> = positions.to_vec();
    if let Some(first) = fixed.first_mut() {
        first.get_or_insert(0.0);
    }
    if count > 1 {
        fixed[count - 1].get_or_insert(1.0);
    }
    let mut highest = f32::NEG_INFINITY;
    for position in fixed.iter_mut().flatten() {
        *position = position.max(highest);
        highest = *position;
    }
    let mut index = 0;
    while index < count {
        if fixed[index].is_some() {
            index += 1;
            continue;
        }
        // `fixed[index - 1]` is set (the first entry always is); find the
        // next set entry (the last entry always is).
        let before = fixed[index - 1].unwrap_or(0.0);
        let after_index = (index..count)
            .find(|&i| fixed[i].is_some())
            .unwrap_or(count - 1);
        let after = fixed[after_index].unwrap_or(before);
        let steps = (after_index - index + 1) as f32;
        for (offset, slot) in fixed[index..after_index].iter_mut().enumerate() {
            *slot = Some(before + (after - before) * (offset as f32 + 1.0) / steps);
        }
        index = after_index;
    }
    fixed.into_iter().map(|p| p.unwrap_or(0.0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_fix_up_fills_ends_clamps_and_spaces_evenly() {
        assert_eq!(fix_up(&[None, None]), [0.0, 1.0]);
        assert_eq!(fix_up(&[None, None, None]), [0.0, 0.5, 1.0]);
        assert_eq!(fix_up(&[Some(0.5), Some(0.2), None]), [0.5, 0.5, 1.0]);
        let spread = fix_up(&[Some(0.0), None, None, Some(0.9), None]);
        for (got, want) in spread.iter().zip([0.0, 0.3, 0.6, 0.9, 1.0]) {
            assert!((got - want).abs() < 1e-6, "{spread:?}");
        }
    }

    #[test]
    fn stops_outside_the_line_move_the_shading_ends() {
        let red = CssColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let line = ColorLine::new(&[Some(-0.5), Some(1.5)], &[red, red], None, ALL).unwrap();
        assert_eq!((line.start, line.end), (-0.5, 1.5));
    }

    #[test]
    fn repeating_stops_are_expanded_over_the_covered_part() {
        let red = CssColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        // A period of a quarter repeated over [0, 1] gives four copies.
        let line =
            ColorLine::new(&[Some(0.0), Some(0.25)], &[red, red], Some((0.0, 1.0)), ALL).unwrap();
        assert_eq!(line.stops.len(), 8);
    }

    #[test]
    fn coincident_stops_get_distinct_offsets_inside_the_domain() {
        let red = CssColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let line = ColorLine::new(
            &[Some(0.0), Some(0.2), Some(0.2)],
            &[red, red, red],
            None,
            ALL,
        )
        .unwrap();
        let offsets: Vec<f32> = line.stops.iter().map(|stop| stop.offset.get()).collect();
        assert!(
            offsets.windows(2).all(|pair| pair[0] < pair[1]),
            "{offsets:?}"
        );
        assert_eq!(offsets[2], 1.0);
    }

    #[test]
    fn a_floor_keeps_the_color_the_line_has_there() {
        let black = CssColor {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let white = CssColor {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let (positions, colors) = trim_below(&[-1.0, 1.0], &[black, white], 0.0);
        assert_eq!(positions, [0.0, 1.0]);
        assert_eq!(colors[0].r, 128);
    }

    #[test]
    fn farthest_corner_ellipse_keeps_the_side_ratio() {
        let (rx, ry) = radial_size(
            RadialShape::Ellipse,
            RadialSize::Extent(RadialExtent::FarthestCorner),
            50.0,
            25.0,
            100.0,
            50.0,
        );
        assert!((rx - 50.0 * std::f32::consts::SQRT_2).abs() < 1e-3);
        assert!((ry - 25.0 * std::f32::consts::SQRT_2).abs() < 1e-3);
    }

    #[test]
    fn a_ceiling_keeps_the_color_the_line_has_there() {
        let black = CssColor {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let white = CssColor {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let (positions, colors) = trim_above(&[0.0, 2.0], &[black, white], 1.0);
        assert_eq!(positions, [0.0, 1.0]);
        assert_eq!(colors[1].r, 128);
    }

    #[test]
    fn premultiplied_mix_keeps_the_opaque_color_next_to_transparent() {
        let red = CssColor {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let clear = CssColor {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        let half = mix_premultiplied(red, clear, 0.5);
        assert_eq!((half.r, half.g, half.b, half.a), (255, 0, 0, 128));
        assert_eq!(mix_premultiplied(red, clear, 1.0).r, 255);
        assert_eq!(mix_premultiplied(clear, red, 0.0).r, 255);
        // Two transparent ends interpolate their colors directly.
        let blue_clear = CssColor { b: 255, ..clear };
        assert_eq!(mix_premultiplied(clear, blue_clear, 0.5).b, 128);
    }
}
