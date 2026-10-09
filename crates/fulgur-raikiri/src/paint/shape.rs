//! Box geometry: border-box rectangles with rounded corners, inset to the
//! padding and content edges, and the PDF paths that outline them.

use krilla::geom::{Path, PathBuilder};
use raikiri_html::PaintRect;
use raikiri_html::computed::ComputedBorderRadius;

/// Lengths for the four sides of a box, in px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub(super) fn scale(self, factor: f32) -> Self {
        Self {
            top: self.top * factor,
            right: self.right * factor,
            bottom: self.bottom * factor,
            left: self.left * factor,
        }
    }

    pub(super) fn add(self, other: Self) -> Self {
        Self {
            top: self.top + other.top,
            right: self.right + other.right,
            bottom: self.bottom + other.bottom,
            left: self.left + other.left,
        }
    }
}

/// Which physical edges are breaks between page or inline fragments rather
/// than edges of the box. With the initial `box-decoration-break:
/// slice` (CSS Fragmentation 3 §5.4), a fragment is a slice of one
/// unbroken box: a broken edge has no border, padding or corner rounding.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Slice {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl Slice {
    /// The broken edges of `fragment`: every edge but the start of the
    /// first fragment and the end of the last one.
    pub(super) fn of(fragment: &raikiri_html::Fragment<'_>) -> Self {
        Self {
            top: !fragment.is_first_fragment().unwrap_or(true),
            bottom: !fragment.is_last_fragment().unwrap_or(true),
            ..Self::default()
        }
    }

    /// The physical inline edges missing from one generated line piece.
    pub(super) fn generated(piece: &raikiri_html::GeneratedBox<'_>) -> Self {
        let (left, right) =
            if piece.style.direction == raikiri_html::computed::ComputedDirection::Rtl {
                (!piece.has_end_edge, !piece.has_start_edge)
            } else {
                (!piece.has_start_edge, !piece.has_end_edge)
            };
        Self {
            left,
            right,
            ..Self::default()
        }
    }

    /// `edges` with the broken sides set to zero.
    pub(super) fn edges(self, mut edges: Edges) -> Edges {
        if self.top {
            edges.top = 0.0;
        }
        if self.bottom {
            edges.bottom = 0.0;
        }
        if self.left {
            edges.left = 0.0;
        }
        if self.right {
            edges.right = 0.0;
        }
        edges
    }
}

/// A rectangle whose corners are quarter ellipses. `radii` holds
/// `[horizontal, vertical]` radii for the top-left, top-right, bottom-right
/// and bottom-left corners, in that order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RoundedRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub radii: [[f32; 2]; 4],
}

#[derive(Clone, Copy)]
struct CornerPath {
    start: [f32; 2],
    curve: Option<[[f32; 2]; 3]>,
}

impl RoundedRect {
    /// The border box of a fragment with its used corner radii.
    ///
    /// Resolve both axes and the common overlap scale through Raikiri's
    /// used-value API (CSS Backgrounds 3 §§4.1, 4.5).
    pub(super) fn border_box(rect: PaintRect, radius: &ComputedBorderRadius) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width.max(0.0),
            height: rect.height.max(0.0),
            radii: radius.used(rect.width, rect.height),
        }
    }

    /// This shape with square corners along the broken edges of `slice`.
    pub(super) fn sliced(mut self, slice: Slice) -> Self {
        if slice.top {
            self.radii[0] = [0.0; 2];
            self.radii[1] = [0.0; 2];
        }
        if slice.bottom {
            self.radii[2] = [0.0; 2];
            self.radii[3] = [0.0; 2];
        }
        if slice.left {
            self.radii[0] = [0.0; 2];
            self.radii[3] = [0.0; 2];
        }
        if slice.right {
            self.radii[1] = [0.0; 2];
            self.radii[2] = [0.0; 2];
        }
        self
    }

    /// A plain rectangle.
    pub(super) fn rect(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width: width.max(0.0),
            height: height.max(0.0),
            radii: [[0.0; 2]; 4],
        }
    }

    pub(super) fn is_rounded(&self) -> bool {
        self.radii.iter().flatten().any(|radius| *radius > 0.0)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// This shape moved inward by `edges`, as the padding edge is from the
    /// border edge. CSS Backgrounds 3 §4.2: the inner radius of a corner is
    /// the outer radius minus the adjacent border width, floored at zero.
    /// Insets larger than the box collapse it to an empty one. Opposite
    /// edges crop inner curves when drawing, without rescaling their radii.
    pub(super) fn inset(&self, edges: Edges) -> Self {
        let r = self.radii;
        let horizontal = edges.left + edges.right;
        let vertical = edges.top + edges.bottom;
        // Keep the inner box inside the outer one when the insets overlap,
        // in proportion to each side's inset.
        let x_scale = if horizontal > self.width && horizontal > 0.0 {
            self.width / horizontal
        } else {
            1.0
        };
        let y_scale = if vertical > self.height && vertical > 0.0 {
            self.height / vertical
        } else {
            1.0
        };
        let (left, right) = (edges.left * x_scale, edges.right * x_scale);
        let (top, bottom) = (edges.top * y_scale, edges.bottom * y_scale);
        Self {
            x: self.x + left,
            y: self.y + top,
            width: (self.width - left - right).max(0.0),
            height: (self.height - top - bottom).max(0.0),
            radii: [
                [(r[0][0] - left).max(0.0), (r[0][1] - top).max(0.0)],
                [(r[1][0] - right).max(0.0), (r[1][1] - top).max(0.0)],
                [(r[2][0] - right).max(0.0), (r[2][1] - bottom).max(0.0)],
                [(r[3][0] - left).max(0.0), (r[3][1] - bottom).max(0.0)],
            ]
            .map(|corner| {
                if corner[0] == 0.0 || corner[1] == 0.0 {
                    [0.0; 2]
                } else {
                    corner
                }
            }),
        }
    }

    /// Append one clockwise subpath, starting at the top-left curve's end.
    /// Inner ellipses retain their radii and are cropped at opposite edges
    /// (CSS Backgrounds 3 §4.2). An empty intersection appends nothing.
    pub(super) fn append_to(&self, builder: &mut PathBuilder) {
        if self.is_empty() {
            return;
        }
        // Validate every corner before mutating a border ring's builder.
        let Some(corners) = self.corner_paths() else {
            return;
        };
        if self.intersecting_corners() {
            let points = self.common_outline(&corners);
            if points.len() < 3 {
                return;
            }
            builder.move_to(points[0][0] as f32, points[0][1] as f32);
            for point in &points[1..] {
                builder.line_to(point[0] as f32, point[1] as f32);
            }
            builder.close();
            return;
        }
        let start = corners[0]
            .curve
            .map_or(corners[0].start, |points| points[2]);
        builder.move_to(start[0], start[1]);
        for index in [1, 2, 3, 0] {
            let corner = corners[index];
            builder.line_to(corner.start[0], corner.start[1]);
            if let Some([a, b, end]) = corner.curve {
                builder.cubic_to(a[0], a[1], b[0], b[1], end[0], end[1]);
            }
        }
        builder.close();
    }

    fn intersecting_corners(&self) -> bool {
        let r = self.radii.map(|corner| corner.map(f64::from));
        let (width, height) = (f64::from(self.width), f64::from(self.height));
        let cropped = r
            .iter()
            .any(|corner| corner[0] > width || corner[1] > height);
        cropped
            && [(0, 2), (1, 3)].into_iter().any(|(a, b)| {
                r[a].iter().chain(&r[b]).all(|radius| *radius > 0.0)
                    && r[a][0] + r[b][0] > width
                    && r[a][1] + r[b][1] > height
            })
    }

    /// Intersect the rectangle with every convex corner constraint. Flatten
    /// only when cropped diagonal arcs can cross; individual corners keep
    /// their cubic paths. The clipping chords bound a single common outline.
    fn common_outline(&self, corners: &[CornerPath; 4]) -> Vec<[f64; 2]> {
        let (x0, y0) = (f64::from(self.x), f64::from(self.y));
        let (x1, y1) = (x0 + f64::from(self.width), y0 + f64::from(self.height));
        let mut polygon = vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        for corner in corners {
            if let Some([a, b, end]) = corner.curve {
                let curve = [corner.start, a, b, end].map(|p| p.map(f64::from));
                let mut points = vec![curve[0]];
                flatten_corner(curve, 0, &mut points);
                for edge in points.windows(2) {
                    polygon = clip_convex_polygon(polygon, edge[0], edge[1]);
                    if polygon.len() < 3 {
                        return Vec::new();
                    }
                }
            }
        }
        polygon
    }

    fn corner_paths(&self) -> Option<[CornerPath; 4]> {
        let (x0, y0) = (f64::from(self.x), f64::from(self.y));
        let (x1, y1) = (x0 + f64::from(self.width), y0 + f64::from(self.height));
        let square = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        let mut paths = square.map(|point| CornerPath {
            start: point.map(|v| v as f32),
            curve: None,
        });
        for (index, [rx, ry]) in self.radii.map(|r| r.map(f64::from)).into_iter().enumerate() {
            if rx <= 0.0 || ry <= 0.0 {
                continue;
            }
            let (cx, cy) = match index {
                0 => (x0 + rx, y0 + ry),
                1 => (x1 - rx, y0 + ry),
                2 => (x1 - rx, y1 - ry),
                _ => (x0 + rx, y1 - ry),
            };
            let (lower, upper) = match index {
                0 => ((cy - y1) / ry, (cx - x1) / rx),
                1 => ((x0 - cx) / rx, (cy - y1) / ry),
                2 => ((y0 - cy) / ry, (x0 - cx) / rx),
                _ => ((cx - x1) / rx, (y0 - cy) / ry),
            };
            let start = lower.clamp(0.0, 1.0).asin();
            let end = upper.clamp(0.0, 1.0).acos();
            if start >= end {
                return None;
            }
            let point_and_tangent = |angle: f64| {
                let (sin, cos) = angle.sin_cos();
                match index {
                    0 => ([cx - rx * cos, cy - ry * sin], [rx * sin, -ry * cos]),
                    1 => ([cx + rx * sin, cy - ry * cos], [rx * cos, ry * sin]),
                    2 => ([cx + rx * cos, cy + ry * sin], [-rx * sin, ry * cos]),
                    _ => ([cx - rx * sin, cy + ry * cos], [-rx * cos, -ry * sin]),
                }
            };
            let (mut from, from_tangent) = point_and_tangent(start);
            let (mut to, to_tangent) = point_and_tangent(end);
            // Place intersections exactly on their clipping edge.
            if lower > 0.0 {
                match index {
                    0 => from[1] = y1,
                    1 => from[0] = x0,
                    2 => from[1] = y0,
                    _ => from[0] = x1,
                }
            }
            if upper > 0.0 {
                match index {
                    0 => to[0] = x1,
                    1 => to[1] = y1,
                    2 => to[0] = x0,
                    _ => to[1] = y0,
                }
            }
            // Tangent controls approximate this arc (at most a quarter turn).
            let alpha = (4.0 / 3.0) * ((end - start) / 4.0).tan();
            let a = std::array::from_fn(|axis| from[axis] + alpha * from_tangent[axis]);
            let b = std::array::from_fn(|axis| to[axis] - alpha * to_tangent[axis]);
            paths[index] = CornerPath {
                start: from.map(|v| v as f32),
                curve: Some([a, b, to].map(|point| point.map(|v| v as f32))),
            };
        }
        Some(paths)
    }

    /// The outline of this shape, or `None` when it is empty.
    pub(super) fn path(&self) -> Option<Path> {
        let mut builder = PathBuilder::new();
        self.append_to(&mut builder);
        builder.finish()
    }
}

/// Subdivide a convex cubic until its control hull is within 0.05px of
/// its chord. Depth is bounded to 10 (1024 chords) for extreme coordinates.
fn flatten_corner(curve: [[f64; 2]; 4], depth: u8, points: &mut Vec<[f64; 2]>) {
    let [from, a, b, to] = curve;
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let length = dx.hypot(dy);
    let distance = |point: [f64; 2]| {
        let (px, py) = (point[0] - from[0], point[1] - from[1]);
        if length == 0.0 {
            px.hypot(py)
        } else {
            (dx * py - dy * px).abs() / length
        }
    };
    if depth == 10 || distance(a).max(distance(b)) <= 0.05 {
        points.push(to);
        return;
    }
    let midpoint = |a: [f64; 2], b: [f64; 2]| [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
    let ab = midpoint(from, a);
    let bc = midpoint(a, b);
    let cd = midpoint(b, to);
    let abc = midpoint(ab, bc);
    let bcd = midpoint(bc, cd);
    let center = midpoint(abc, bcd);
    flatten_corner([from, ab, abc, center], depth + 1, points);
    flatten_corner([center, bcd, cd, to], depth + 1, points);
}

/// Keep the half-plane to the clockwise contour's interior side of `a`→`b`.
fn clip_convex_polygon(polygon: Vec<[f64; 2]>, a: [f64; 2], b: [f64; 2]) -> Vec<[f64; 2]> {
    let Some(&last) = polygon.last() else {
        return polygon;
    };
    let distance =
        |point: [f64; 2]| (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0]);
    let mut out = Vec::with_capacity(polygon.len() + 1);
    let mut previous = last;
    let mut previous_distance = distance(previous);
    for current in polygon {
        let current_distance = distance(current);
        if (previous_distance >= 0.0) != (current_distance >= 0.0) {
            let t = previous_distance / (previous_distance - current_distance);
            out.push(std::array::from_fn(|axis| {
                previous[axis] + t * (current[axis] - previous[axis])
            }));
        }
        if current_distance >= 0.0 {
            out.push(current);
        }
        previous = current;
        previous_distance = current_distance;
    }
    out
}

/// The area between `outer` and `inner`, to fill with the even-odd rule.
/// `None` when `outer` is empty; the whole of `outer` when `inner` is.
pub(super) fn ring(outer: &RoundedRect, inner: &RoundedRect) -> Option<Path> {
    let mut builder = PathBuilder::new();
    outer.append_to(&mut builder);
    if !outer.is_empty() {
        inner.append_to(&mut builder);
    }
    builder.finish()
}

/// A closed polygon through `points`.
pub(super) fn polygon(points: &[(f32, f32)]) -> Option<Path> {
    let (first, rest) = points.split_first()?;
    let mut builder = PathBuilder::new();
    builder.move_to(first.0, first.1);
    for point in rest {
        builder.line_to(point.0, point.1);
    }
    builder.close();
    builder.finish()
}

#[cfg(test)]
mod tests;
