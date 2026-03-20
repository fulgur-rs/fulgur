use crate::config::{Margin, PageSize};

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
}
