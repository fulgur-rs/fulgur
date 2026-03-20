use std::collections::HashMap;

use crate::config::{Margin, PageSize};

/// Which edge of the page a set of margin boxes belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Parse a CSS at-keyword name (without the `@`) into a `MarginBoxPosition`.
    ///
    /// Accepts names like `"top-center"`, `"bottom-left-corner"`, etc.
    pub fn from_at_keyword(name: &str) -> Option<Self> {
        match name {
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

/// Map an edge to its (left, center, right) non-corner positions.
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
        // Left/Right don't use this helper in compute_edge_layout,
        // but we provide a mapping for completeness.
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

/// Distribute available width among up to 3 positions (left, center, right).
/// Returns the computed width for each defined position.
fn distribute_widths(
    left_max: Option<f32>,
    center_max: Option<f32>,
    right_max: Option<f32>,
    available: f32,
) -> (Option<f32>, Option<f32>, Option<f32>) {
    let defined_count =
        left_max.is_some() as u8 + center_max.is_some() as u8 + right_max.is_some() as u8;

    if defined_count == 0 {
        return (None, None, None);
    }

    // 1 position defined: gets full available width
    if defined_count == 1 {
        return (
            left_max.map(|_| available),
            center_max.map(|_| available),
            right_max.map(|_| available),
        );
    }

    if let Some(c_max) = center_max {
        // Center defined (with or without L/R)
        let ac_max = left_max.unwrap_or(0.0) + right_max.unwrap_or(0.0);
        let (c_width, ac_width) = flex_distribute(c_max, ac_max, available);
        let half_ac = ac_width / 2.0;
        (
            left_max.map(|_| half_ac),
            Some(c_width),
            right_max.map(|_| half_ac),
        )
    } else {
        // C not defined, L + R
        let l_max = left_max.unwrap_or(0.0);
        let r_max = right_max.unwrap_or(0.0);
        let (l_width, r_width) = flex_distribute(l_max, r_max, available);
        (Some(l_width), None, Some(r_width))
    }
}

/// Compute the rects for all defined margin boxes on a given edge,
/// using CSS Paged Media flex-based width distribution.
/// `defined` maps non-corner positions to their max-content width.
/// Corner rects are NOT included — compute those separately with `bounding_rect`.
pub fn compute_edge_layout(
    edge: Edge,
    defined: &HashMap<MarginBoxPosition, f32>,
    page_size: PageSize,
    margin: Margin,
) -> HashMap<MarginBoxPosition, MarginBoxRect> {
    let mut result = HashMap::new();

    match edge {
        Edge::Top | Edge::Bottom => {
            let content_width = page_size.width - margin.left - margin.right;
            let (y, height) = if edge == Edge::Top {
                (0.0, margin.top)
            } else {
                (page_size.height - margin.bottom, margin.bottom)
            };

            let (left_pos, center_pos, right_pos) = edge_positions(edge);
            let left_max = defined.get(&left_pos).copied();
            let center_max = defined.get(&center_pos).copied();
            let right_max = defined.get(&right_pos).copied();

            let (l_w, c_w, r_w) = distribute_widths(left_max, center_max, right_max, content_width);

            let mut x = margin.left;
            if let Some(w) = l_w {
                result.insert(
                    left_pos,
                    MarginBoxRect {
                        x,
                        y,
                        width: w,
                        height,
                    },
                );
                x += w;
            }
            if let Some(w) = c_w {
                result.insert(
                    center_pos,
                    MarginBoxRect {
                        x,
                        y,
                        width: w,
                        height,
                    },
                );
                x += w;
            }
            if let Some(w) = r_w {
                result.insert(
                    right_pos,
                    MarginBoxRect {
                        x,
                        y,
                        width: w,
                        height,
                    },
                );
            }
        }
        Edge::Left | Edge::Right => {
            // Phase 3: just return bounding_rect for each defined position
            for &pos in defined.keys() {
                result.insert(pos, pos.bounding_rect(page_size, margin));
            }
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

    // --- distribute_widths tests ---

    #[test]
    fn test_distribute_center_only() {
        let (l, c, r) = distribute_widths(None, Some(100.0), None, 600.0);
        assert!(l.is_none());
        assert!((c.unwrap() - 600.0).abs() < 0.01);
        assert!(r.is_none());
    }

    #[test]
    fn test_distribute_left_right() {
        let (l, c, r) = distribute_widths(Some(100.0), None, Some(200.0), 600.0);
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
        let (l, c, r) = distribute_widths(Some(50.0), Some(200.0), Some(50.0), 600.0);
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

        let mut defined = HashMap::new();
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

        let mut defined = HashMap::new();
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

        let mut defined = HashMap::new();
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
}
