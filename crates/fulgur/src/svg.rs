//! SvgPageable — renders inline <svg> elements to PDF as vector graphics
//! via krilla-svg's SurfaceExt::draw_svg.

use std::sync::Arc;

use usvg::Tree;

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
    /// fulgur-3vwx (Phase 3.2.b): DOM NodeId for `slice_for_page`
    /// geometry lookup. See `BlockPageable::node_id`.
    pub node_id: Option<usize>,
}

impl SvgPageable {
    pub fn new(tree: Arc<Tree>, width: f32, height: f32) -> Self {
        Self {
            tree,
            width,
            height,
            opacity: 1.0,
            visible: true,
            node_id: None,
        }
    }

    pub fn with_node_id(mut self, node_id: Option<usize>) -> Self {
        self.node_id = node_id;
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
    fn test_height_returns_configured_height() {
        let svg = SvgPageable::new(parse_tree(), 100.0, 50.0);
        assert_eq!(svg.height, 50.0);
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
