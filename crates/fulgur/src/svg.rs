//! SvgPageable — renders inline <svg> elements to PDF as vector graphics
//! via krilla-svg's SurfaceExt::draw_svg.

use std::sync::Arc;

use usvg::Tree;

use crate::pageable::{Canvas, Pageable, Pagination, Pt, Size};

/// An inline `<svg>` element rendered as vector graphics.
#[derive(Clone)]
pub struct SvgPageable {
    /// Parsed SVG tree, shared via Arc for cheap cloning during pagination.
    pub tree: Arc<Tree>,
    /// Display width in PDF points — CSS-resolved by Blitz/Taffy, NOT the
    /// SVG's intrinsic `viewBox` size. krilla-svg scales the tree to this
    /// box on draw, so callers must pass the layout box, not `tree.size()`.
    pub width: f32,
    /// Display height in PDF points — see `width`.
    pub height: f32,
    pub opacity: f32,
    pub visible: bool,
}

impl SvgPageable {
    pub fn new(tree: Arc<Tree>, width: f32, height: f32) -> Self {
        Self {
            tree,
            width,
            height,
            opacity: 1.0,
            visible: true,
        }
    }
}

impl Pageable for SvgPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        // SVGs are atomic — cannot be split across pages
        None
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, _avail_width: Pt, _avail_height: Pt) {
        use crate::pageable::draw_with_opacity;
        use krilla_svg::{SurfaceExt, SvgSettings};

        if !self.visible {
            return;
        }
        draw_with_opacity(canvas, self.opacity, |canvas| {
            let Some(size) = krilla::geom::Size::from_wh(self.width, self.height) else {
                return;
            };
            let transform = krilla::geom::Transform::from_translate(x, y);
            canvas.surface.push_transform(&transform);
            // draw_svg returns Option<()>; None means the tree was malformed.
            // We silently skip rather than panic, matching ImagePageable's behavior
            // when krilla::image::Image::from_* returns Err.
            let _ = canvas
                .surface
                .draw_svg(&self.tree, size, SvgSettings::default());
            canvas.surface.pop();
        });
    }

    fn pagination(&self) -> Pagination {
        Pagination::default()
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.height
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid SVG: 100x50 red rectangle
    const MINIMAL_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="red"/></svg>"#;

    fn parse_tree() -> Arc<Tree> {
        let opts = usvg::Options::default();
        let tree = Tree::from_str(MINIMAL_SVG, &opts).expect("parse minimal svg");
        Arc::new(tree)
    }

    #[test]
    fn test_wrap_returns_configured_size() {
        let mut svg = SvgPageable::new(parse_tree(), 120.0, 60.0);
        let size = svg.wrap(1000.0, 1000.0);
        assert_eq!(size.width, 120.0);
        assert_eq!(size.height, 60.0);
    }

    #[test]
    fn test_split_returns_none() {
        let svg = SvgPageable::new(parse_tree(), 100.0, 50.0);
        assert!(svg.split(1000.0, 1000.0).is_none());
    }

    #[test]
    fn test_height_returns_configured_height() {
        let svg = SvgPageable::new(parse_tree(), 100.0, 50.0);
        assert_eq!(svg.height(), 50.0);
    }

    #[test]
    fn test_clone_box_shares_tree_via_arc() {
        let original = SvgPageable::new(parse_tree(), 100.0, 50.0);
        let original_ptr = Arc::as_ptr(&original.tree);
        let cloned = original.clone();
        let cloned_ptr = Arc::as_ptr(&cloned.tree);
        assert_eq!(
            original_ptr, cloned_ptr,
            "clone must share the underlying usvg::Tree via Arc"
        );
    }

    #[test]
    fn test_default_opacity_and_visible() {
        let svg = SvgPageable::new(parse_tree(), 100.0, 50.0);
        assert_eq!(svg.opacity, 1.0);
        assert!(svg.visible);
    }
}
