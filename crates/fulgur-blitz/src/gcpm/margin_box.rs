use std::collections::{BTreeMap, HashMap};

use crate::config::{Margin, PageSize};
use crate::units::{F32Units, Pt};

/// Which edge of the page a set of margin boxes belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub fn is_horizontal(self) -> bool {
        matches!(self, Edge::Top | Edge::Bottom)
    }
}

/// Rectangle describing a margin box's position and size in page coordinates (points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarginBoxRect {
    pub x: Pt,
    pub y: Pt,
    pub width: Pt,
    pub height: Pt,
}

/// The 16 CSS page margin box positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MarginBoxPosition {
    TopLeftCorner,
    TopLeft,
    TopCenter,
    TopRight,
    TopRightCorner,
    LeftTop,
    LeftMiddle,
    LeftBottom,
    RightTop,
    RightMiddle,
    RightBottom,
    BottomLeftCorner,
    BottomLeft,
    BottomCenter,
    BottomRight,
    BottomRightCorner,
}

impl MarginBoxPosition {
    /// Which edge this position belongs to, or `None` for corner boxes.
    pub fn edge(&self) -> Option<Edge> {
        match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => Some(Edge::Top),
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight => Some(Edge::Bottom),
            Self::LeftTop | Self::LeftMiddle | Self::LeftBottom => Some(Edge::Left),
            Self::RightTop | Self::RightMiddle | Self::RightBottom => Some(Edge::Right),
            _ => None,
        }
    }

    /// Parse a CSS at-keyword name (without the `@`) into a `MarginBoxPosition`.
    ///
    /// Accepts names like `"top-center"`, `"bottom-left-corner"`, etc.
    pub fn from_at_keyword(name: &str) -> Option<Self> {
        // CSS at-rule names are ASCII case-insensitive.
        // cssparser does not lowercase at-rule names before passing them here.
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "top-left-corner" => Some(Self::TopLeftCorner),
            "top-left" => Some(Self::TopLeft),
            "top-center" => Some(Self::TopCenter),
            "top-right" => Some(Self::TopRight),
            "top-right-corner" => Some(Self::TopRightCorner),
            "left-top" => Some(Self::LeftTop),
            "left-middle" => Some(Self::LeftMiddle),
            "left-bottom" => Some(Self::LeftBottom),
            "right-top" => Some(Self::RightTop),
            "right-middle" => Some(Self::RightMiddle),
            "right-bottom" => Some(Self::RightBottom),
            "bottom-left-corner" => Some(Self::BottomLeftCorner),
            "bottom-left" => Some(Self::BottomLeft),
            "bottom-center" => Some(Self::BottomCenter),
            "bottom-right" => Some(Self::BottomRight),
            "bottom-right-corner" => Some(Self::BottomRightCorner),
            _ => None,
        }
    }

    /// Compute the bounding rectangle for this margin box position.
    ///
    /// Coordinates are in page-space points with origin at top-left of page.
    pub fn bounding_rect(&self, page_size: PageSize, margin: Margin) -> MarginBoxRect {
        let content_width = page_size.width.as_pt() - margin.left.as_pt() - margin.right.as_pt();
        let content_height = page_size.height.as_pt() - margin.top.as_pt() - margin.bottom.as_pt();
        let third_w = content_width / 3.0;
        let third_h = content_height / 3.0;

        match self {
            // --- Top edge corners ---
            Self::TopLeftCorner => MarginBoxRect {
                x: Pt::ZERO,
                y: Pt::ZERO,
                width: margin.left.as_pt(),
                height: margin.top.as_pt(),
            },
            Self::TopRightCorner => MarginBoxRect {
                x: page_size.width.as_pt() - margin.right.as_pt(),
                y: Pt::ZERO,
                width: margin.right.as_pt(),
                height: margin.top.as_pt(),
            },

            // --- Top edge positions ---
            Self::TopLeft => MarginBoxRect {
                x: margin.left.as_pt(),
                y: Pt::ZERO,
                width: third_w,
                height: margin.top.as_pt(),
            },
            Self::TopCenter => MarginBoxRect {
                x: margin.left.as_pt(),
                y: Pt::ZERO,
                width: content_width,
                height: margin.top.as_pt(),
            },
            Self::TopRight => MarginBoxRect {
                x: margin.left.as_pt() + 2.0 * third_w,
                y: Pt::ZERO,
                width: third_w,
                height: margin.top.as_pt(),
            },

            // --- Bottom edge corners ---
            Self::BottomLeftCorner => MarginBoxRect {
                x: Pt::ZERO,
                y: page_size.height.as_pt() - margin.bottom.as_pt(),
                width: margin.left.as_pt(),
                height: margin.bottom.as_pt(),
            },
            Self::BottomRightCorner => MarginBoxRect {
                x: page_size.width.as_pt() - margin.right.as_pt(),
                y: page_size.height.as_pt() - margin.bottom.as_pt(),
                width: margin.right.as_pt(),
                height: margin.bottom.as_pt(),
            },

            // --- Bottom edge positions ---
            Self::BottomLeft => MarginBoxRect {
                x: margin.left.as_pt(),
                y: page_size.height.as_pt() - margin.bottom.as_pt(),
                width: third_w,
                height: margin.bottom.as_pt(),
            },
            Self::BottomCenter => MarginBoxRect {
                x: margin.left.as_pt(),
                y: page_size.height.as_pt() - margin.bottom.as_pt(),
                width: content_width,
                height: margin.bottom.as_pt(),
            },
            Self::BottomRight => MarginBoxRect {
                x: margin.left.as_pt() + 2.0 * third_w,
                y: page_size.height.as_pt() - margin.bottom.as_pt(),
                width: third_w,
                height: margin.bottom.as_pt(),
            },

            // --- Left edge positions ---
            Self::LeftTop => MarginBoxRect {
                x: Pt::ZERO,
                y: margin.top.as_pt(),
                width: margin.left.as_pt(),
                height: third_h,
            },
            Self::LeftMiddle => MarginBoxRect {
                x: Pt::ZERO,
                y: margin.top.as_pt() + third_h,
                width: margin.left.as_pt(),
                height: third_h,
            },
            Self::LeftBottom => MarginBoxRect {
                x: Pt::ZERO,
                y: margin.top.as_pt() + 2.0 * third_h,
                width: margin.left.as_pt(),
                height: third_h,
            },

            // --- Right edge positions ---
            Self::RightTop => MarginBoxRect {
                x: page_size.width.as_pt() - margin.right.as_pt(),
                y: margin.top.as_pt(),
                width: margin.right.as_pt(),
                height: third_h,
            },
            Self::RightMiddle => MarginBoxRect {
                x: page_size.width.as_pt() - margin.right.as_pt(),
                y: margin.top.as_pt() + third_h,
                width: margin.right.as_pt(),
                height: third_h,
            },
            Self::RightBottom => MarginBoxRect {
                x: page_size.width.as_pt() - margin.right.as_pt(),
                y: margin.top.as_pt() + 2.0 * third_h,
                width: margin.right.as_pt(),
                height: third_h,
            },
        }
    }
}

/// Map an edge to its (first, center, last) non-corner positions.
fn edge_positions(edge: Edge) -> (MarginBoxPosition, MarginBoxPosition, MarginBoxPosition) {
    match edge {
        Edge::Top => (
            MarginBoxPosition::TopLeft,
            MarginBoxPosition::TopCenter,
            MarginBoxPosition::TopRight,
        ),
        Edge::Bottom => (
            MarginBoxPosition::BottomLeft,
            MarginBoxPosition::BottomCenter,
            MarginBoxPosition::BottomRight,
        ),
        Edge::Left => (
            MarginBoxPosition::LeftTop,
            MarginBoxPosition::LeftMiddle,
            MarginBoxPosition::LeftBottom,
        ),
        Edge::Right => (
            MarginBoxPosition::RightTop,
            MarginBoxPosition::RightMiddle,
            MarginBoxPosition::RightBottom,
        ),
    }
}

/// Distribute available space between two items based on their max-content widths.
fn flex_distribute(a_max: Pt, b_max: Pt, available: Pt) -> (Pt, Pt) {
    let total = a_max + b_max;
    if total == Pt::ZERO {
        return (available / 2.0, available / 2.0);
    }
    let a_factor = a_max / total;
    if total <= available {
        let flex_space = available - total;
        let a = a_max + flex_space * a_factor;
        let b = b_max + flex_space * (1.0 - a_factor);
        (a, b)
    } else {
        let a = available * a_factor;
        let b = available * (1.0 - a_factor);
        (a, b)
    }
}

/// Distribute available space among up to 3 positions (first, center, last).
/// Returns the computed size for each defined position.
fn distribute_sizes(
    first_max: Option<Pt>,
    center_max: Option<Pt>,
    last_max: Option<Pt>,
    available: Pt,
) -> (Option<Pt>, Option<Pt>, Option<Pt>) {
    let defined_count =
        first_max.is_some() as u8 + center_max.is_some() as u8 + last_max.is_some() as u8;

    if defined_count == 0 {
        return (None, None, None);
    }

    // 1 position defined: gets full available space
    if defined_count == 1 {
        return (
            first_max.map(|_| available),
            center_max.map(|_| available),
            last_max.map(|_| available),
        );
    }

    if let Some(c_max) = center_max {
        // Center defined (with or without first/last)
        let fl_max = first_max.unwrap_or(Pt::ZERO) + last_max.unwrap_or(Pt::ZERO);
        let (c_size, fl_size) = flex_distribute(c_max, fl_max, available);
        let half_fl = fl_size / 2.0;
        (
            first_max.map(|_| half_fl),
            Some(c_size),
            last_max.map(|_| half_fl),
        )
    } else {
        // Center not defined, first + last
        let f_max = first_max.unwrap_or(Pt::ZERO);
        let l_max = last_max.unwrap_or(Pt::ZERO);
        let (f_size, l_size) = flex_distribute(f_max, l_max, available);
        (Some(f_size), None, Some(l_size))
    }
}

/// Compute the rects for all defined margin boxes on a given edge,
/// using CSS Paged Media flex-based width distribution.
/// `defined` maps non-corner positions to their max-content width.
/// Corner rects are NOT included — compute those separately with `bounding_rect`.
pub fn compute_edge_layout(
    edge: Edge,
    defined: &BTreeMap<MarginBoxPosition, Pt>,
    page_size: PageSize,
    margin: Margin,
) -> HashMap<MarginBoxPosition, MarginBoxRect> {
    let mut result = HashMap::new();

    let (first_pos, center_pos, last_pos) = edge_positions(edge);
    let first_max = defined.get(&first_pos).copied();
    let center_max = defined.get(&center_pos).copied();
    let last_max = defined.get(&last_pos).copied();

    // Primary axis: width for T/B, height for L/R
    // fixed_origin: start offset on primary axis (margin.left for T/B, margin.top for L/R)
    // cross_origin: position on cross axis (y for T/B, x for L/R)
    // cross_extent: size on cross axis (margin height for T/B, margin width for L/R)
    let (available, fixed_origin, cross_origin, cross_extent) = match edge {
        Edge::Top => (
            page_size.width.as_pt() - margin.left.as_pt() - margin.right.as_pt(),
            margin.left.as_pt(),
            Pt::ZERO,
            margin.top.as_pt(),
        ),
        Edge::Bottom => (
            page_size.width.as_pt() - margin.left.as_pt() - margin.right.as_pt(),
            margin.left.as_pt(),
            page_size.height.as_pt() - margin.bottom.as_pt(),
            margin.bottom.as_pt(),
        ),
        Edge::Left => (
            page_size.height.as_pt() - margin.top.as_pt() - margin.bottom.as_pt(),
            margin.top.as_pt(),
            Pt::ZERO,
            margin.left.as_pt(),
        ),
        Edge::Right => (
            page_size.height.as_pt() - margin.top.as_pt() - margin.bottom.as_pt(),
            margin.top.as_pt(),
            page_size.width.as_pt() - margin.right.as_pt(),
            margin.right.as_pt(),
        ),
    };

    let (f_size, c_size, l_size) = distribute_sizes(first_max, center_max, last_max, available);

    let is_horizontal = matches!(edge, Edge::Top | Edge::Bottom);

    // Build rect from primary-axis offset and size
    let make_rect = |offset: Pt, size: Pt| -> MarginBoxRect {
        if is_horizontal {
            MarginBoxRect {
                x: offset,
                y: cross_origin,
                width: size,
                height: cross_extent,
            }
        } else {
            MarginBoxRect {
                x: cross_origin,
                y: offset,
                width: cross_extent,
                height: size,
            }
        }
    };

    if let Some(cs) = c_size {
        // Center-based layout: first_slot | center | last_slot
        let first_slot = f_size.unwrap_or_else(|| l_size.unwrap_or(Pt::ZERO));
        let o_first = fixed_origin;
        let o_center = o_first + first_slot;
        let o_last = o_center + cs;

        if let Some(s) = f_size {
            result.insert(first_pos, make_rect(o_first, s));
        }
        result.insert(center_pos, make_rect(o_center, cs));
        if let Some(s) = l_size {
            result.insert(last_pos, make_rect(o_last, s));
        }
    } else {
        // No center: sequential layout
        let mut offset = fixed_origin;
        if let Some(s) = f_size {
            result.insert(first_pos, make_rect(offset, s));
            offset += s;
        }
        if let Some(s) = l_size {
            result.insert(last_pos, make_rect(offset, s));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tag an `f32` literal as `Pt` for the size/measure inputs the
    /// distribution helpers now take. Keeps the test arithmetic readable
    /// after the `units::Pt` migration.
    fn pt(v: f32) -> Pt {
        v.as_pt()
    }

    #[test]
    fn test_from_at_keyword_valid() {
        assert_eq!(
            MarginBoxPosition::from_at_keyword("top-center"),
            Some(MarginBoxPosition::TopCenter)
        );
        assert_eq!(
            MarginBoxPosition::from_at_keyword("bottom-left-corner"),
            Some(MarginBoxPosition::BottomLeftCorner)
        );
        assert_eq!(
            MarginBoxPosition::from_at_keyword("right-middle"),
            Some(MarginBoxPosition::RightMiddle)
        );
        assert_eq!(
            MarginBoxPosition::from_at_keyword("top-left"),
            Some(MarginBoxPosition::TopLeft)
        );
    }

    #[test]
    fn test_from_at_keyword_invalid() {
        assert_eq!(MarginBoxPosition::from_at_keyword("center"), None);
        assert_eq!(MarginBoxPosition::from_at_keyword(""), None);
        assert_eq!(MarginBoxPosition::from_at_keyword("top-middle"), None);
    }

    #[test]
    fn test_bounding_rect_top_center() {
        let page = PageSize::A4; // 595.28 x 841.89
        let margin = Margin::uniform(72.0); // 1 inch all around
        let rect = MarginBoxPosition::TopCenter.bounding_rect(page, margin);

        let content_width = page.width - margin.left - margin.right;
        assert!((rect.x.to_f32() - margin.left).abs() < 0.01);
        assert!((rect.y.to_f32() - 0.0).abs() < 0.01);
        assert!((rect.width.to_f32() - content_width).abs() < 0.01);
        assert!((rect.height.to_f32() - margin.top).abs() < 0.01);
    }

    #[test]
    fn test_bounding_rect_bottom_center() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let rect = MarginBoxPosition::BottomCenter.bounding_rect(page, margin);

        let content_width = page.width - margin.left - margin.right;
        assert!((rect.x.to_f32() - margin.left).abs() < 0.01);
        assert!((rect.y.to_f32() - (page.height - margin.bottom)).abs() < 0.01);
        assert!((rect.width.to_f32() - content_width).abs() < 0.01);
        assert!((rect.height.to_f32() - margin.bottom).abs() < 0.01);
    }

    #[test]
    fn test_bounding_rect_top_left_corner() {
        let page = PageSize::A4;
        let margin = Margin {
            top: 50.0,
            right: 40.0,
            bottom: 60.0,
            left: 70.0,
        };
        let rect = MarginBoxPosition::TopLeftCorner.bounding_rect(page, margin);

        assert!((rect.x.to_f32() - 0.0).abs() < 0.01);
        assert!((rect.y.to_f32() - 0.0).abs() < 0.01);
        assert!((rect.width.to_f32() - 70.0).abs() < 0.01);
        assert!((rect.height.to_f32() - 50.0).abs() < 0.01);
    }

    // --- flex_distribute tests ---

    #[test]
    fn test_flex_distribute_both_fit() {
        // a=100, b=200, available=600 → proportional: a=200, b=400
        let (a, b) = flex_distribute(pt(100.0), pt(200.0), pt(600.0));
        assert!((a.to_f32() - 200.0).abs() < 0.01);
        assert!((b.to_f32() - 400.0).abs() < 0.01);
    }

    #[test]
    fn test_flex_distribute_overflow() {
        // a=300, b=600, available=450 → proportional shrink: a=150, b=300
        let (a, b) = flex_distribute(pt(300.0), pt(600.0), pt(450.0));
        assert!((a.to_f32() - 150.0).abs() < 0.01);
        assert!((b.to_f32() - 300.0).abs() < 0.01);
    }

    #[test]
    fn test_flex_distribute_zero() {
        let (a, b) = flex_distribute(pt(0.0), pt(0.0), pt(300.0));
        assert!((a.to_f32() - 150.0).abs() < 0.01);
        assert!((b.to_f32() - 150.0).abs() < 0.01);
    }

    // --- distribute_sizes tests ---

    #[test]
    fn test_distribute_center_only() {
        let (l, c, r) = distribute_sizes(None, Some(pt(100.0)), None, pt(600.0));
        assert!(l.is_none());
        assert!((c.unwrap().to_f32() - 600.0).abs() < 0.01);
        assert!(r.is_none());
    }

    #[test]
    fn test_distribute_left_right() {
        let (l, c, r) = distribute_sizes(Some(pt(100.0)), None, Some(pt(200.0)), pt(600.0));
        assert!(c.is_none());
        // flex_distribute(100, 200, 600) → (200, 400)
        assert!((l.unwrap().to_f32() - 200.0).abs() < 0.01);
        assert!((r.unwrap().to_f32() - 400.0).abs() < 0.01);
    }

    #[test]
    fn test_distribute_all_three() {
        // center=200, left=50, right=50, available=600
        // ac_max = 50+50 = 100
        // flex_distribute(200, 100, 600) → total=300, flex_space=300
        //   c_factor = 200/300 = 2/3, c = 200 + 300*2/3 = 400
        //   ac = 100 + 300*1/3 = 200, half_ac = 100
        let (l, c, r) =
            distribute_sizes(Some(pt(50.0)), Some(pt(200.0)), Some(pt(50.0)), pt(600.0));
        assert!((c.unwrap().to_f32() - 400.0).abs() < 0.01);
        assert!((l.unwrap().to_f32() - 100.0).abs() < 0.01);
        assert!((r.unwrap().to_f32() - 100.0).abs() < 0.01);
    }

    // --- compute_edge_layout tests ---

    #[test]
    fn test_compute_edge_layout_top_center_only() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_width = page.width - margin.left - margin.right;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::TopCenter, pt(100.0));

        let result = compute_edge_layout(Edge::Top, &defined, page, margin);
        assert_eq!(result.len(), 1);
        let rect = result[&MarginBoxPosition::TopCenter];
        assert!((rect.x.to_f32() - margin.left).abs() < 0.01);
        assert!((rect.width.to_f32() - content_width).abs() < 0.01);
        assert!((rect.y.to_f32() - 0.0).abs() < 0.01);
        assert!((rect.height.to_f32() - margin.top).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_top_left_right() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_width = page.width - margin.left - margin.right;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::TopLeft, pt(100.0));
        defined.insert(MarginBoxPosition::TopRight, pt(200.0));

        let result = compute_edge_layout(Edge::Top, &defined, page, margin);
        assert_eq!(result.len(), 2);

        let left_rect = result[&MarginBoxPosition::TopLeft];
        let right_rect = result[&MarginBoxPosition::TopRight];

        // Widths sum to content_width
        assert!(
            (left_rect.width.to_f32() + right_rect.width.to_f32() - content_width).abs() < 0.01
        );
        // No overlap: right starts where left ends
        assert!(
            (right_rect.x.to_f32() - (left_rect.x.to_f32() + left_rect.width.to_f32())).abs()
                < 0.01
        );
        // Left starts at margin.left
        assert!((left_rect.x.to_f32() - margin.left).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_top_all_three() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_width = page.width - margin.left - margin.right;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::TopLeft, pt(50.0));
        defined.insert(MarginBoxPosition::TopCenter, pt(200.0));
        defined.insert(MarginBoxPosition::TopRight, pt(50.0));

        let result = compute_edge_layout(Edge::Top, &defined, page, margin);
        assert_eq!(result.len(), 3);

        let l = result[&MarginBoxPosition::TopLeft];
        let c = result[&MarginBoxPosition::TopCenter];
        let r = result[&MarginBoxPosition::TopRight];

        // Widths sum to content_width
        assert!(
            (l.width.to_f32() + c.width.to_f32() + r.width.to_f32() - content_width).abs() < 0.01
        );
        // Correct x positions: left starts at margin.left
        assert!((l.x.to_f32() - margin.left).abs() < 0.01);
        // Center starts after left
        assert!((c.x.to_f32() - (l.x.to_f32() + l.width.to_f32())).abs() < 0.01);
        // Right starts after center
        assert!((r.x.to_f32() - (c.x.to_f32() + c.width.to_f32())).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_left_all_three() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_height = page.height - margin.top - margin.bottom;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::LeftTop, pt(50.0));
        defined.insert(MarginBoxPosition::LeftMiddle, pt(200.0));
        defined.insert(MarginBoxPosition::LeftBottom, pt(50.0));

        let result = compute_edge_layout(Edge::Left, &defined, page, margin);
        assert_eq!(result.len(), 3);

        let t = result[&MarginBoxPosition::LeftTop];
        let m = result[&MarginBoxPosition::LeftMiddle];
        let b = result[&MarginBoxPosition::LeftBottom];

        // Heights sum to content_height
        assert!(
            (t.height.to_f32() + m.height.to_f32() + b.height.to_f32() - content_height).abs()
                < 0.01
        );
        // All have x=0, width=margin.left
        assert!((t.x.to_f32() - 0.0).abs() < 0.01);
        assert!((t.width.to_f32() - margin.left).abs() < 0.01);
        // Correct y positions: top starts at margin.top
        assert!((t.y.to_f32() - margin.top).abs() < 0.01);
        // Middle starts after top
        assert!((m.y.to_f32() - (t.y.to_f32() + t.height.to_f32())).abs() < 0.01);
        // Bottom starts after middle
        assert!((b.y.to_f32() - (m.y.to_f32() + m.height.to_f32())).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_right_top_bottom() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_height = page.height - margin.top - margin.bottom;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::RightTop, pt(100.0));
        defined.insert(MarginBoxPosition::RightBottom, pt(200.0));

        let result = compute_edge_layout(Edge::Right, &defined, page, margin);
        assert_eq!(result.len(), 2);

        let t = result[&MarginBoxPosition::RightTop];
        let b = result[&MarginBoxPosition::RightBottom];

        // Heights sum to content_height
        assert!((t.height.to_f32() + b.height.to_f32() - content_height).abs() < 0.01);
        // x = page_width - margin.right, width = margin.right
        assert!((t.x.to_f32() - (page.width - margin.right)).abs() < 0.01);
        assert!((t.width.to_f32() - margin.right).abs() < 0.01);
        // Top starts at margin.top
        assert!((t.y.to_f32() - margin.top).abs() < 0.01);
        // Bottom starts where top ends
        assert!((b.y.to_f32() - (t.y.to_f32() + t.height.to_f32())).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_left_middle_only() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_height = page.height - margin.top - margin.bottom;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::LeftMiddle, pt(100.0));

        let result = compute_edge_layout(Edge::Left, &defined, page, margin);
        assert_eq!(result.len(), 1);

        let m = result[&MarginBoxPosition::LeftMiddle];
        // Single slot gets full height
        assert!((m.height.to_f32() - content_height).abs() < 0.01);
        assert!((m.x.to_f32() - 0.0).abs() < 0.01);
        assert!((m.y.to_f32() - margin.top).abs() < 0.01);
        assert!((m.width.to_f32() - margin.left).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_center_right_no_left() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_width = page.width - margin.left - margin.right;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::TopCenter, pt(200.0));
        defined.insert(MarginBoxPosition::TopRight, pt(50.0));

        let result = compute_edge_layout(Edge::Top, &defined, page, margin);
        assert_eq!(result.len(), 2);

        let c = result[&MarginBoxPosition::TopCenter];
        let r = result[&MarginBoxPosition::TopRight];

        // Widths sum to content_width (center + right + left_slot)
        // Center should NOT start at margin.left — it should be offset
        // by the right slot width to stay centered.
        assert!(c.x.to_f32() > margin.left);
        // Right starts after center
        assert!((r.x.to_f32() - (c.x.to_f32() + c.width.to_f32())).abs() < 0.01);
        // Right ends at content edge
        assert!((r.x.to_f32() + r.width.to_f32() - (margin.left + content_width)).abs() < 0.01);
    }

    // --- Edge::is_horizontal ---

    #[test]
    fn edge_is_horizontal_top_and_bottom_are_horizontal() {
        assert!(Edge::Top.is_horizontal());
        assert!(Edge::Bottom.is_horizontal());
    }

    #[test]
    fn edge_is_horizontal_left_and_right_are_not_horizontal() {
        assert!(!Edge::Left.is_horizontal());
        assert!(!Edge::Right.is_horizontal());
    }

    // --- MarginBoxPosition::edge ---

    #[test]
    fn edge_top_positions_belong_to_top_edge() {
        assert_eq!(MarginBoxPosition::TopLeft.edge(), Some(Edge::Top));
        assert_eq!(MarginBoxPosition::TopCenter.edge(), Some(Edge::Top));
        assert_eq!(MarginBoxPosition::TopRight.edge(), Some(Edge::Top));
    }

    #[test]
    fn edge_bottom_positions_belong_to_bottom_edge() {
        assert_eq!(MarginBoxPosition::BottomLeft.edge(), Some(Edge::Bottom));
        assert_eq!(MarginBoxPosition::BottomCenter.edge(), Some(Edge::Bottom));
        assert_eq!(MarginBoxPosition::BottomRight.edge(), Some(Edge::Bottom));
    }

    #[test]
    fn edge_left_positions_belong_to_left_edge() {
        assert_eq!(MarginBoxPosition::LeftTop.edge(), Some(Edge::Left));
        assert_eq!(MarginBoxPosition::LeftMiddle.edge(), Some(Edge::Left));
        assert_eq!(MarginBoxPosition::LeftBottom.edge(), Some(Edge::Left));
    }

    #[test]
    fn edge_right_positions_belong_to_right_edge() {
        assert_eq!(MarginBoxPosition::RightTop.edge(), Some(Edge::Right));
        assert_eq!(MarginBoxPosition::RightMiddle.edge(), Some(Edge::Right));
        assert_eq!(MarginBoxPosition::RightBottom.edge(), Some(Edge::Right));
    }

    #[test]
    fn edge_corner_positions_return_none() {
        assert_eq!(MarginBoxPosition::TopLeftCorner.edge(), None);
        assert_eq!(MarginBoxPosition::TopRightCorner.edge(), None);
        assert_eq!(MarginBoxPosition::BottomLeftCorner.edge(), None);
        assert_eq!(MarginBoxPosition::BottomRightCorner.edge(), None);
    }

    // --- from_at_keyword: remaining keywords not yet covered ---

    #[test]
    fn from_at_keyword_all_valid_keywords() {
        let cases: &[(&str, MarginBoxPosition)] = &[
            ("top-left-corner", MarginBoxPosition::TopLeftCorner),
            ("top-left", MarginBoxPosition::TopLeft),
            ("top-center", MarginBoxPosition::TopCenter),
            ("top-right", MarginBoxPosition::TopRight),
            ("top-right-corner", MarginBoxPosition::TopRightCorner),
            ("left-top", MarginBoxPosition::LeftTop),
            ("left-middle", MarginBoxPosition::LeftMiddle),
            ("left-bottom", MarginBoxPosition::LeftBottom),
            ("right-top", MarginBoxPosition::RightTop),
            ("right-middle", MarginBoxPosition::RightMiddle),
            ("right-bottom", MarginBoxPosition::RightBottom),
            ("bottom-left-corner", MarginBoxPosition::BottomLeftCorner),
            ("bottom-left", MarginBoxPosition::BottomLeft),
            ("bottom-center", MarginBoxPosition::BottomCenter),
            ("bottom-right", MarginBoxPosition::BottomRight),
            ("bottom-right-corner", MarginBoxPosition::BottomRightCorner),
        ];
        for (keyword, expected) in cases {
            assert_eq!(
                MarginBoxPosition::from_at_keyword(keyword),
                Some(*expected),
                "keyword={keyword}"
            );
        }
    }

    #[test]
    fn from_at_keyword_case_insensitive() {
        assert_eq!(
            MarginBoxPosition::from_at_keyword("TOP-LEFT"),
            Some(MarginBoxPosition::TopLeft)
        );
        assert_eq!(
            MarginBoxPosition::from_at_keyword("Bottom-Right-Corner"),
            Some(MarginBoxPosition::BottomRightCorner)
        );
    }

    // --- bounding_rect: all remaining positions ---

    fn page() -> PageSize {
        PageSize::A4 // 595.28 × 841.89 pt
    }

    fn margin() -> Margin {
        Margin {
            top: 50.0,
            right: 40.0,
            bottom: 60.0,
            left: 70.0,
        }
    }

    /// Compare a `Pt` rect field against an expected `f32` (page-space pt).
    fn approx(a: Pt, b: f32) -> bool {
        (a.to_f32() - b).abs() < 0.01
    }

    #[test]
    fn bounding_rect_top_right_corner() {
        let r = MarginBoxPosition::TopRightCorner.bounding_rect(page(), margin());
        assert!(approx(r.x, page().width - margin().right));
        assert!(approx(r.y, 0.0));
        assert!(approx(r.width, margin().right));
        assert!(approx(r.height, margin().top));
    }

    #[test]
    fn bounding_rect_top_left() {
        let r = MarginBoxPosition::TopLeft.bounding_rect(page(), margin());
        let content_w = page().width - margin().left - margin().right;
        assert!(approx(r.x, margin().left));
        assert!(approx(r.y, 0.0));
        assert!(approx(r.width, content_w / 3.0));
        assert!(approx(r.height, margin().top));
    }

    #[test]
    fn bounding_rect_top_right() {
        let r = MarginBoxPosition::TopRight.bounding_rect(page(), margin());
        let content_w = page().width - margin().left - margin().right;
        let third = content_w / 3.0;
        assert!(approx(r.x, margin().left + 2.0 * third));
        assert!(approx(r.y, 0.0));
        assert!(approx(r.width, third));
        assert!(approx(r.height, margin().top));
    }

    #[test]
    fn bounding_rect_bottom_left_corner() {
        let r = MarginBoxPosition::BottomLeftCorner.bounding_rect(page(), margin());
        assert!(approx(r.x, 0.0));
        assert!(approx(r.y, page().height - margin().bottom));
        assert!(approx(r.width, margin().left));
        assert!(approx(r.height, margin().bottom));
    }

    #[test]
    fn bounding_rect_bottom_right_corner() {
        let r = MarginBoxPosition::BottomRightCorner.bounding_rect(page(), margin());
        assert!(approx(r.x, page().width - margin().right));
        assert!(approx(r.y, page().height - margin().bottom));
        assert!(approx(r.width, margin().right));
        assert!(approx(r.height, margin().bottom));
    }

    #[test]
    fn bounding_rect_bottom_left() {
        let r = MarginBoxPosition::BottomLeft.bounding_rect(page(), margin());
        let content_w = page().width - margin().left - margin().right;
        assert!(approx(r.x, margin().left));
        assert!(approx(r.y, page().height - margin().bottom));
        assert!(approx(r.width, content_w / 3.0));
        assert!(approx(r.height, margin().bottom));
    }

    #[test]
    fn bounding_rect_bottom_right() {
        let r = MarginBoxPosition::BottomRight.bounding_rect(page(), margin());
        let content_w = page().width - margin().left - margin().right;
        let third = content_w / 3.0;
        assert!(approx(r.x, margin().left + 2.0 * third));
        assert!(approx(r.y, page().height - margin().bottom));
        assert!(approx(r.width, third));
        assert!(approx(r.height, margin().bottom));
    }

    #[test]
    fn bounding_rect_left_top() {
        let r = MarginBoxPosition::LeftTop.bounding_rect(page(), margin());
        let content_h = page().height - margin().top - margin().bottom;
        assert!(approx(r.x, 0.0));
        assert!(approx(r.y, margin().top));
        assert!(approx(r.width, margin().left));
        assert!(approx(r.height, content_h / 3.0));
    }

    #[test]
    fn bounding_rect_left_middle() {
        let r = MarginBoxPosition::LeftMiddle.bounding_rect(page(), margin());
        let content_h = page().height - margin().top - margin().bottom;
        let third = content_h / 3.0;
        assert!(approx(r.x, 0.0));
        assert!(approx(r.y, margin().top + third));
        assert!(approx(r.width, margin().left));
        assert!(approx(r.height, third));
    }

    #[test]
    fn bounding_rect_left_bottom() {
        let r = MarginBoxPosition::LeftBottom.bounding_rect(page(), margin());
        let content_h = page().height - margin().top - margin().bottom;
        let third = content_h / 3.0;
        assert!(approx(r.x, 0.0));
        assert!(approx(r.y, margin().top + 2.0 * third));
        assert!(approx(r.width, margin().left));
        assert!(approx(r.height, third));
    }

    #[test]
    fn bounding_rect_right_top() {
        let r = MarginBoxPosition::RightTop.bounding_rect(page(), margin());
        let content_h = page().height - margin().top - margin().bottom;
        assert!(approx(r.x, page().width - margin().right));
        assert!(approx(r.y, margin().top));
        assert!(approx(r.width, margin().right));
        assert!(approx(r.height, content_h / 3.0));
    }

    #[test]
    fn bounding_rect_right_middle() {
        let r = MarginBoxPosition::RightMiddle.bounding_rect(page(), margin());
        let content_h = page().height - margin().top - margin().bottom;
        let third = content_h / 3.0;
        assert!(approx(r.x, page().width - margin().right));
        assert!(approx(r.y, margin().top + third));
        assert!(approx(r.width, margin().right));
        assert!(approx(r.height, third));
    }

    #[test]
    fn bounding_rect_right_bottom() {
        let r = MarginBoxPosition::RightBottom.bounding_rect(page(), margin());
        let content_h = page().height - margin().top - margin().bottom;
        let third = content_h / 3.0;
        assert!(approx(r.x, page().width - margin().right));
        assert!(approx(r.y, margin().top + 2.0 * third));
        assert!(approx(r.width, margin().right));
        assert!(approx(r.height, third));
    }

    // --- distribute_sizes: first-only and last-only ---

    #[test]
    fn test_distribute_first_only() {
        let (f, c, l) = distribute_sizes(Some(pt(80.0)), None, None, pt(400.0));
        assert!(
            approx(f.unwrap(), 400.0),
            "first-only should get full space"
        );
        assert!(c.is_none());
        assert!(l.is_none());
    }

    #[test]
    fn test_distribute_last_only() {
        let (f, c, l) = distribute_sizes(None, None, Some(pt(80.0)), pt(400.0));
        assert!(f.is_none());
        assert!(c.is_none());
        assert!(approx(l.unwrap(), 400.0), "last-only should get full space");
    }

    #[test]
    fn test_distribute_none_all() {
        let (f, c, l) = distribute_sizes(None, None, None, pt(400.0));
        assert!(f.is_none());
        assert!(c.is_none());
        assert!(l.is_none());
    }

    // --- compute_edge_layout: bottom edge and last-only ---

    #[test]
    fn test_compute_edge_layout_bottom_center_only() {
        let p = PageSize::A4;
        let m = Margin::uniform(72.0);
        let content_width = p.width - m.left - m.right;
        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::BottomCenter, pt(100.0));
        let result = compute_edge_layout(Edge::Bottom, &defined, p, m);
        assert_eq!(result.len(), 1);
        let rect = result[&MarginBoxPosition::BottomCenter];
        assert!(approx(rect.y, p.height - m.bottom));
        assert!(approx(rect.width, content_width));
        assert!(approx(rect.height, m.bottom));
    }

    #[test]
    fn test_compute_edge_layout_right_middle_only() {
        let p = PageSize::A4;
        let m = Margin::uniform(72.0);
        let content_height = p.height - m.top - m.bottom;
        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::RightMiddle, pt(50.0));
        let result = compute_edge_layout(Edge::Right, &defined, p, m);
        assert_eq!(result.len(), 1);
        let rect = result[&MarginBoxPosition::RightMiddle];
        assert!(approx(rect.x, p.width - m.right));
        assert!(approx(rect.width, m.right));
        assert!(approx(rect.height, content_height));
        assert!(approx(rect.y, m.top));
    }

    #[test]
    fn test_compute_edge_layout_no_defined_returns_empty() {
        let p = PageSize::A4;
        let m = Margin::uniform(72.0);
        let defined = BTreeMap::new();
        let result = compute_edge_layout(Edge::Top, &defined, p, m);
        assert!(result.is_empty());
    }

    #[test]
    fn test_compute_edge_layout_bottom_left_only() {
        // No center, only first (BottomLeft) — exercises the `offset += s`
        // in the sequential (no-center) branch of compute_edge_layout.
        let p = PageSize::A4;
        let m = Margin::uniform(72.0);
        let content_width = p.width - m.left - m.right;
        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::BottomLeft, pt(120.0));
        let result = compute_edge_layout(Edge::Bottom, &defined, p, m);
        assert_eq!(result.len(), 1);
        let rect = result[&MarginBoxPosition::BottomLeft];
        // First-only: gets full available width; starts at margin.left
        assert!(approx(rect.width, content_width));
        assert!(approx(rect.x, m.left));
    }
}
