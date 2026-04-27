//! opacity + visibility extraction.
//!
//! Free function — called directly by block / list_item / inline_root /
//! pseudo, not from extract_block_style.

use blitz_dom::Node;

/// Extract CSS opacity and visibility from computed styles.
/// Returns `(opacity, visible)` with defaults `(1.0, true)`.
///
/// `pub(in crate::convert)` rather than `pub(super)`: callers in
/// `convert/{block,list_item,inline_root,pseudo}.rs` resolve this symbol via
/// `style/mod.rs`'s `pub(super) use` re-export. A `pub(super)` definition
/// (visible only to `style/mod.rs`) makes the re-export non-transparent and
/// trips E0603 at the `convert/mod.rs` import site. Restricting to the
/// `convert` subtree keeps unrelated modules (`paragraph`, `render`, ...) from
/// reaching the helper directly.
pub(in crate::convert) fn extract_opacity_visible(node: &Node) -> (f32, bool) {
    use style::properties::longhands::visibility::computed_value::T as Visibility;
    node.primary_styles()
        .map(|s| {
            let opacity = s.clone_opacity();
            let v = s.clone_visibility();
            let visible = v != Visibility::Hidden && v != Visibility::Collapse;
            (opacity, visible)
        })
        .unwrap_or((1.0, true))
}
