use std::collections::{BTreeMap, HashMap};

use crate::config::{Margin, PageSize};

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
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
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
        let content_width = page_size.width - margin.left - margin.right;
        let content_height = page_size.height - margin.top - margin.bottom;
        let third_w = content_width / 3.0;
        let third_h = content_height / 3.0;

        match self {
            // --- Top edge corners ---
            Self::TopLeftCorner => MarginBoxRect {
                x: 0.0,
                y: 0.0,
                width: margin.left,
                height: margin.top,
            },
            Self::TopRightCorner => MarginBoxRect {
                x: page_size.width - margin.right,
                y: 0.0,
                width: margin.right,
                height: margin.top,
            },

            // --- Top edge positions ---
            Self::TopLeft => MarginBoxRect {
                x: margin.left,
                y: 0.0,
                width: third_w,
                height: margin.top,
            },
            Self::TopCenter => MarginBoxRect {
                x: margin.left,
                y: 0.0,
                width: content_width,
                height: margin.top,
            },
            Self::TopRight => MarginBoxRect {
                x: margin.left + 2.0 * third_w,
                y: 0.0,
                width: third_w,
                height: margin.top,
            },

            // --- Bottom edge corners ---
            Self::BottomLeftCorner => MarginBoxRect {
                x: 0.0,
                y: page_size.height - margin.bottom,
                width: margin.left,
                height: margin.bottom,
            },
            Self::BottomRightCorner => MarginBoxRect {
                x: page_size.width - margin.right,
                y: page_size.height - margin.bottom,
                width: margin.right,
                height: margin.bottom,
            },

            // --- Bottom edge positions ---
            Self::BottomLeft => MarginBoxRect {
                x: margin.left,
                y: page_size.height - margin.bottom,
                width: third_w,
                height: margin.bottom,
            },
            Self::BottomCenter => MarginBoxRect {
                x: margin.left,
                y: page_size.height - margin.bottom,
                width: content_width,
                height: margin.bottom,
            },
            Self::BottomRight => MarginBoxRect {
                x: margin.left + 2.0 * third_w,
                y: page_size.height - margin.bottom,
                width: third_w,
                height: margin.bottom,
            },

            // --- Left edge positions ---
            Self::LeftTop => MarginBoxRect {
                x: 0.0,
                y: margin.top,
                width: margin.left,
                height: third_h,
            },
            Self::LeftMiddle => MarginBoxRect {
                x: 0.0,
                y: margin.top + third_h,
                width: margin.left,
                height: third_h,
            },
            Self::LeftBottom => MarginBoxRect {
                x: 0.0,
                y: margin.top + 2.0 * third_h,
                width: margin.left,
                height: third_h,
            },

            // --- Right edge positions ---
            Self::RightTop => MarginBoxRect {
                x: page_size.width - margin.right,
                y: margin.top,
                width: margin.right,
                height: third_h,
            },
            Self::RightMiddle => MarginBoxRect {
                x: page_size.width - margin.right,
                y: margin.top + third_h,
                width: margin.right,
                height: third_h,
            },
            Self::RightBottom => MarginBoxRect {
                x: page_size.width - margin.right,
                y: margin.top + 2.0 * third_h,
                width: margin.right,
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
fn flex_distribute(a_max: f32, b_max: f32, available: f32) -> (f32, f32) {
    let total = a_max + b_max;
    if total == 0.0 {
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
    first_max: Option<f32>,
    center_max: Option<f32>,
    last_max: Option<f32>,
    available: f32,
) -> (Option<f32>, Option<f32>, Option<f32>) {
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
        let fl_max = first_max.unwrap_or(0.0) + last_max.unwrap_or(0.0);
        let (c_size, fl_size) = flex_distribute(c_max, fl_max, available);
        let half_fl = fl_size / 2.0;
        (
            first_max.map(|_| half_fl),
            Some(c_size),
            last_max.map(|_| half_fl),
        )
    } else {
        // Center not defined, first + last
        let f_max = first_max.unwrap_or(0.0);
        let l_max = last_max.unwrap_or(0.0);
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
    defined: &BTreeMap<MarginBoxPosition, f32>,
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
            page_size.width - margin.left - margin.right,
            margin.left,
            0.0,
            margin.top,
        ),
        Edge::Bottom => (
            page_size.width - margin.left - margin.right,
            margin.left,
            page_size.height - margin.bottom,
            margin.bottom,
        ),
        Edge::Left => (
            page_size.height - margin.top - margin.bottom,
            margin.top,
            0.0,
            margin.left,
        ),
        Edge::Right => (
            page_size.height - margin.top - margin.bottom,
            margin.top,
            page_size.width - margin.right,
            margin.right,
        ),
    };

    let (f_size, c_size, l_size) = distribute_sizes(first_max, center_max, last_max, available);

    let is_horizontal = matches!(edge, Edge::Top | Edge::Bottom);

    // Build rect from primary-axis offset and size
    let make_rect = |offset: f32, size: f32| -> MarginBoxRect {
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
        let first_slot = f_size.unwrap_or_else(|| l_size.unwrap_or(0.0));
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
        assert!((rect.x - margin.left).abs() < 0.01);
        assert!((rect.y - 0.0).abs() < 0.01);
        assert!((rect.width - content_width).abs() < 0.01);
        assert!((rect.height - margin.top).abs() < 0.01);
    }

    #[test]
    fn test_bounding_rect_bottom_center() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let rect = MarginBoxPosition::BottomCenter.bounding_rect(page, margin);

        let content_width = page.width - margin.left - margin.right;
        assert!((rect.x - margin.left).abs() < 0.01);
        assert!((rect.y - (page.height - margin.bottom)).abs() < 0.01);
        assert!((rect.width - content_width).abs() < 0.01);
        assert!((rect.height - margin.bottom).abs() < 0.01);
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

        assert!((rect.x - 0.0).abs() < 0.01);
        assert!((rect.y - 0.0).abs() < 0.01);
        assert!((rect.width - 70.0).abs() < 0.01);
        assert!((rect.height - 50.0).abs() < 0.01);
    }

    // --- flex_distribute tests ---

    #[test]
    fn test_flex_distribute_both_fit() {
        // a=100, b=200, available=600 → proportional: a=200, b=400
        let (a, b) = flex_distribute(100.0, 200.0, 600.0);
        assert!((a - 200.0).abs() < 0.01);
        assert!((b - 400.0).abs() < 0.01);
    }

    #[test]
    fn test_flex_distribute_overflow() {
        // a=300, b=600, available=450 → proportional shrink: a=150, b=300
        let (a, b) = flex_distribute(300.0, 600.0, 450.0);
        assert!((a - 150.0).abs() < 0.01);
        assert!((b - 300.0).abs() < 0.01);
    }

    #[test]
    fn test_flex_distribute_zero() {
        let (a, b) = flex_distribute(0.0, 0.0, 300.0);
        assert!((a - 150.0).abs() < 0.01);
        assert!((b - 150.0).abs() < 0.01);
    }

    // --- distribute_sizes tests ---

    #[test]
    fn test_distribute_center_only() {
        let (l, c, r) = distribute_sizes(None, Some(100.0), None, 600.0);
        assert!(l.is_none());
        assert!((c.unwrap() - 600.0).abs() < 0.01);
        assert!(r.is_none());
    }

    #[test]
    fn test_distribute_left_right() {
        let (l, c, r) = distribute_sizes(Some(100.0), None, Some(200.0), 600.0);
        assert!(c.is_none());
        // flex_distribute(100, 200, 600) → (200, 400)
        assert!((l.unwrap() - 200.0).abs() < 0.01);
        assert!((r.unwrap() - 400.0).abs() < 0.01);
    }

    #[test]
    fn test_distribute_all_three() {
        // center=200, left=50, right=50, available=600
        // ac_max = 50+50 = 100
        // flex_distribute(200, 100, 600) → total=300, flex_space=300
        //   c_factor = 200/300 = 2/3, c = 200 + 300*2/3 = 400
        //   ac = 100 + 300*1/3 = 200, half_ac = 100
        let (l, c, r) = distribute_sizes(Some(50.0), Some(200.0), Some(50.0), 600.0);
        assert!((c.unwrap() - 400.0).abs() < 0.01);
        assert!((l.unwrap() - 100.0).abs() < 0.01);
        assert!((r.unwrap() - 100.0).abs() < 0.01);
    }

    // --- compute_edge_layout tests ---

    #[test]
    fn test_compute_edge_layout_top_center_only() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_width = page.width - margin.left - margin.right;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::TopCenter, 100.0);

        let result = compute_edge_layout(Edge::Top, &defined, page, margin);
        assert_eq!(result.len(), 1);
        let rect = result[&MarginBoxPosition::TopCenter];
        assert!((rect.x - margin.left).abs() < 0.01);
        assert!((rect.width - content_width).abs() < 0.01);
        assert!((rect.y - 0.0).abs() < 0.01);
        assert!((rect.height - margin.top).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_top_left_right() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_width = page.width - margin.left - margin.right;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::TopLeft, 100.0);
        defined.insert(MarginBoxPosition::TopRight, 200.0);

        let result = compute_edge_layout(Edge::Top, &defined, page, margin);
        assert_eq!(result.len(), 2);

        let left_rect = result[&MarginBoxPosition::TopLeft];
        let right_rect = result[&MarginBoxPosition::TopRight];

        // Widths sum to content_width
        assert!((left_rect.width + right_rect.width - content_width).abs() < 0.01);
        // No overlap: right starts where left ends
        assert!((right_rect.x - (left_rect.x + left_rect.width)).abs() < 0.01);
        // Left starts at margin.left
        assert!((left_rect.x - margin.left).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_top_all_three() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_width = page.width - margin.left - margin.right;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::TopLeft, 50.0);
        defined.insert(MarginBoxPosition::TopCenter, 200.0);
        defined.insert(MarginBoxPosition::TopRight, 50.0);

        let result = compute_edge_layout(Edge::Top, &defined, page, margin);
        assert_eq!(result.len(), 3);

        let l = result[&MarginBoxPosition::TopLeft];
        let c = result[&MarginBoxPosition::TopCenter];
        let r = result[&MarginBoxPosition::TopRight];

        // Widths sum to content_width
        assert!((l.width + c.width + r.width - content_width).abs() < 0.01);
        // Correct x positions: left starts at margin.left
        assert!((l.x - margin.left).abs() < 0.01);
        // Center starts after left
        assert!((c.x - (l.x + l.width)).abs() < 0.01);
        // Right starts after center
        assert!((r.x - (c.x + c.width)).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_left_all_three() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_height = page.height - margin.top - margin.bottom;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::LeftTop, 50.0);
        defined.insert(MarginBoxPosition::LeftMiddle, 200.0);
        defined.insert(MarginBoxPosition::LeftBottom, 50.0);

        let result = compute_edge_layout(Edge::Left, &defined, page, margin);
        assert_eq!(result.len(), 3);

        let t = result[&MarginBoxPosition::LeftTop];
        let m = result[&MarginBoxPosition::LeftMiddle];
        let b = result[&MarginBoxPosition::LeftBottom];

        // Heights sum to content_height
        assert!((t.height + m.height + b.height - content_height).abs() < 0.01);
        // All have x=0, width=margin.left
        assert!((t.x - 0.0).abs() < 0.01);
        assert!((t.width - margin.left).abs() < 0.01);
        // Correct y positions: top starts at margin.top
        assert!((t.y - margin.top).abs() < 0.01);
        // Middle starts after top
        assert!((m.y - (t.y + t.height)).abs() < 0.01);
        // Bottom starts after middle
        assert!((b.y - (m.y + m.height)).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_right_top_bottom() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_height = page.height - margin.top - margin.bottom;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::RightTop, 100.0);
        defined.insert(MarginBoxPosition::RightBottom, 200.0);

        let result = compute_edge_layout(Edge::Right, &defined, page, margin);
        assert_eq!(result.len(), 2);

        let t = result[&MarginBoxPosition::RightTop];
        let b = result[&MarginBoxPosition::RightBottom];

        // Heights sum to content_height
        assert!((t.height + b.height - content_height).abs() < 0.01);
        // x = page_width - margin.right, width = margin.right
        assert!((t.x - (page.width - margin.right)).abs() < 0.01);
        assert!((t.width - margin.right).abs() < 0.01);
        // Top starts at margin.top
        assert!((t.y - margin.top).abs() < 0.01);
        // Bottom starts where top ends
        assert!((b.y - (t.y + t.height)).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_left_middle_only() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_height = page.height - margin.top - margin.bottom;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::LeftMiddle, 100.0);

        let result = compute_edge_layout(Edge::Left, &defined, page, margin);
        assert_eq!(result.len(), 1);

        let m = result[&MarginBoxPosition::LeftMiddle];
        // Single slot gets full height
        assert!((m.height - content_height).abs() < 0.01);
        assert!((m.x - 0.0).abs() < 0.01);
        assert!((m.y - margin.top).abs() < 0.01);
        assert!((m.width - margin.left).abs() < 0.01);
    }

    #[test]
    fn test_compute_edge_layout_center_right_no_left() {
        let page = PageSize::A4;
        let margin = Margin::uniform(72.0);
        let content_width = page.width - margin.left - margin.right;

        let mut defined = BTreeMap::new();
        defined.insert(MarginBoxPosition::TopCenter, 200.0);
        defined.insert(MarginBoxPosition::TopRight, 50.0);

        let result = compute_edge_layout(Edge::Top, &defined, page, margin);
        assert_eq!(result.len(), 2);

        let c = result[&MarginBoxPosition::TopCenter];
        let r = result[&MarginBoxPosition::TopRight];

        // Widths sum to content_width (center + right + left_slot)
        // Center should NOT start at margin.left — it should be offset
        // by the right slot width to stay centered.
        assert!(c.x > margin.left);
        // Right starts after center
        assert!((r.x - (c.x + c.width)).abs() < 0.01);
        // Right ends at content edge
        assert!((r.x + r.width - (margin.left + content_width)).abs() < 0.01);
    }
}
