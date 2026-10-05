//! Box geometry: border-box rectangles with rounded corners, inset to the
//! padding and content edges, and the PDF paths that outline them.

use krilla::geom::{Path, PathBuilder};
use raikiri_html::PaintRect;
use raikiri_html::computed::{ComputedBorderRadius, ComputedLengthPercentage};

/// Cubic Bézier control-point distance for a quarter ellipse, as a fraction
/// of the radius.
const KAPPA: f32 = 0.552_284_8;

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

/// Which block-direction edges of a box fragment are fragmentation breaks
/// rather than edges of the box. With the initial `box-decoration-break:
/// slice` (CSS Fragmentation 3 §5.4), a fragment is a slice of one
/// unbroken box: a broken edge has no border, padding or corner rounding.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Slice {
    pub top: bool,
    pub bottom: bool,
}

impl Slice {
    /// The broken edges of `fragment`: every edge but the start of the
    /// first fragment and the end of the last one.
    pub(super) fn of(fragment: &raikiri_html::Fragment<'_>) -> Self {
        Self {
            top: !fragment.is_first_fragment().unwrap_or(true),
            bottom: !fragment.is_last_fragment().unwrap_or(true),
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

impl RoundedRect {
    /// The border box of a fragment with its used corner radii.
    ///
    /// CSS Backgrounds 3 §5.1: a percentage radius refers to the border box,
    /// its width for the horizontal radius and its height for the vertical
    /// one. The computed value has one value per corner, so both radii come
    /// from it. §5.5: when the radii of two adjacent corners do not fit
    /// along a side, every radius is scaled by the same factor.
    pub(super) fn border_box(rect: PaintRect, radius: &ComputedBorderRadius) -> Self {
        let corner = |value: ComputedLengthPercentage| match value {
            ComputedLengthPercentage::Px(px) => [px.max(0.0), px.max(0.0)],
            ComputedLengthPercentage::Percent(percent) => [
                (rect.width * percent / 100.0).max(0.0),
                (rect.height * percent / 100.0).max(0.0),
            ],
        };
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width.max(0.0),
            height: rect.height.max(0.0),
            radii: [
                corner(radius.top_left),
                corner(radius.top_right),
                corner(radius.bottom_right),
                corner(radius.bottom_left),
            ],
        }
        .clamped()
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

    /// Make every corner with a zero radius square (CSS Backgrounds 3
    /// §5.1: "If either length is zero, the corner is square, not
    /// rounded"), then scale every radius by `f = min(Li / Si)` over the
    /// four sides, where `Li` is the side length and `Si` the sum of the two
    /// radii along it (§5.5), when `f < 1`.
    fn clamped(mut self) -> Self {
        for corner in &mut self.radii {
            if corner[0] <= 0.0 || corner[1] <= 0.0 {
                *corner = [0.0; 2];
            }
        }
        let r = self.radii;
        let fit = |length: f32, sum: f32| {
            if sum > length && sum > 0.0 {
                length / sum
            } else {
                1.0
            }
        };
        let factor = fit(self.width, r[0][0] + r[1][0])
            .min(fit(self.height, r[1][1] + r[2][1]))
            .min(fit(self.width, r[2][0] + r[3][0]))
            .min(fit(self.height, r[3][1] + r[0][1]));
        if factor < 1.0 {
            for corner in &mut self.radii {
                corner[0] *= factor;
                corner[1] *= factor;
            }
        }
        self
    }

    pub(super) fn is_rounded(&self) -> bool {
        self.radii.iter().flatten().any(|radius| *radius > 0.0)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// This shape moved inward by `edges`, as the padding edge is from the
    /// border edge. CSS Backgrounds 3 §5.2: the inner radius of a corner is
    /// the outer radius minus the adjacent border width, floored at zero.
    /// Insets larger than the box collapse it to an empty one.
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
            ],
        }
        .clamped()
    }

    /// Append this shape as one closed subpath, clockwise from the end of
    /// the top-left corner. Nothing is appended for an empty shape.
    pub(super) fn append_to(&self, builder: &mut PathBuilder) {
        if self.is_empty() {
            return;
        }
        let (x0, y0) = (self.x, self.y);
        let (x1, y1) = (self.x + self.width, self.y + self.height);
        let [tl, tr, br, bl] = self.radii;
        let k = 1.0 - KAPPA;
        builder.move_to(x0 + tl[0], y0);
        builder.line_to(x1 - tr[0], y0);
        if tr[0] > 0.0 && tr[1] > 0.0 {
            builder.cubic_to(x1 - tr[0] * k, y0, x1, y0 + tr[1] * k, x1, y0 + tr[1]);
        }
        builder.line_to(x1, y1 - br[1]);
        if br[0] > 0.0 && br[1] > 0.0 {
            builder.cubic_to(x1, y1 - br[1] * k, x1 - br[0] * k, y1, x1 - br[0], y1);
        }
        builder.line_to(x0 + bl[0], y1);
        if bl[0] > 0.0 && bl[1] > 0.0 {
            builder.cubic_to(x0 + bl[0] * k, y1, x0, y1 - bl[1] * k, x0, y1 - bl[1]);
        }
        builder.line_to(x0, y0 + tl[1]);
        if tl[0] > 0.0 && tl[1] > 0.0 {
            builder.cubic_to(x0, y0 + tl[1] * k, x0 + tl[0] * k, y0, x0 + tl[0], y0);
        }
        builder.close();
    }

    /// The outline of this shape, or `None` when it is empty.
    pub(super) fn path(&self) -> Option<Path> {
        let mut builder = PathBuilder::new();
        self.append_to(&mut builder);
        builder.finish()
    }
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
mod tests {
    use super::*;

    fn radius(px: f32) -> ComputedBorderRadius {
        ComputedBorderRadius::all(raikiri_html::computed::ComputedLength(px))
    }

    #[test]
    fn overlapping_radii_scale_by_the_smallest_side_ratio() {
        // 100 x 40 with 30px corners: the vertical sides need 60px but have
        // 40, so f = 40 / 60 and every radius becomes 20px.
        let shape = RoundedRect::border_box(PaintRect::new(0.0, 0.0, 100.0, 40.0), &radius(30.0));
        for corner in shape.radii {
            assert!((corner[0] - 20.0).abs() < 1e-4, "{corner:?}");
            assert!((corner[1] - 20.0).abs() < 1e-4, "{corner:?}");
        }
    }

    #[test]
    fn percentage_radius_refers_to_each_axis_of_the_border_box() {
        let all = ComputedLengthPercentage::Percent(50.0);
        let shape = RoundedRect::border_box(
            PaintRect::new(0.0, 0.0, 80.0, 40.0),
            &ComputedBorderRadius::corners(all, all, all, all),
        );
        assert_eq!(shape.radii[0], [40.0, 20.0]);
    }

    #[test]
    fn inner_radius_subtracts_the_adjacent_border_width() {
        let outer = RoundedRect::border_box(PaintRect::new(0.0, 0.0, 100.0, 100.0), &radius(10.0));
        let inner = outer.inset(Edges {
            top: 4.0,
            right: 12.0,
            bottom: 4.0,
            left: 2.0,
        });
        assert_eq!(
            (inner.x, inner.y, inner.width, inner.height),
            (2.0, 4.0, 86.0, 92.0)
        );
        assert_eq!(inner.radii[0], [8.0, 6.0]);
        // The right border (12px) exceeds the 10px radius: one inner radius
        // is zero, so the corner is square (§5.1), not [0, 6].
        assert_eq!(inner.radii[1], [0.0, 0.0]);
    }

    #[test]
    fn insets_larger_than_the_box_collapse_it_in_proportion() {
        let outer = RoundedRect::rect(0.0, 0.0, 10.0, 6.0);
        // 15px of horizontal and 12px of vertical inset in a 10 x 6 box:
        // each side keeps its share, and the inner box is empty.
        let inner = outer.inset(Edges {
            top: 4.0,
            right: 10.0,
            bottom: 8.0,
            left: 5.0,
        });
        assert!((inner.x - 10.0 / 3.0).abs() < 1e-5, "{inner:?}");
        assert_eq!((inner.y, inner.width, inner.height), (2.0, 0.0, 0.0));
        assert!(inner.is_empty());
        assert!(inner.path().is_none());
        // The ring of an empty inner box is the whole outer box.
        assert!(ring(&outer, &inner).is_some());
    }
}
