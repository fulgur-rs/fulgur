use super::*;
use super::{positioned, pseudo};

/// Catch-all dispatch for nodes not matched by list_item / table / replaced /
/// inline_root. Inserts a `BlockEntry` into `out.block_styles` (and walks
/// children recursively into `out`) — a leaf with no visual style and no
/// pseudo content registers nothing, matching the v1 zero-height spacer
/// behaviour where the dispatcher had no per-NodeId payload to record.
pub(super) fn convert(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    let (width, height) = size_in_pt(node.final_layout.size);

    let layout_children_guard = node.layout_children.borrow();
    let children: &[usize] = layout_children_guard.as_deref().unwrap_or(&node.children);

    if children.is_empty() {
        let style = extract_block_style(node, ctx.assets);
        let content_box = compute_content_box(node, &style);
        // Check for pseudo images even on childless elements — e.g.
        // `<div class="icon"></div>` with `.icon::before { content: url(...) }`.
        let has_pseudo = pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);
        if style.needs_block_wrapper() || has_pseudo {
            insert_block_entry(node, style, width, height, out);
        }
        // Plain leaf with neither style nor pseudo: nothing to record.
        return;
    }

    // Container node — walk children, snapshot before/after to compute
    // clip / opacity descendants.
    let style = extract_block_style(node, ctx.assets);
    let content_box = compute_content_box(node, &style);
    let clipping = style.has_overflow_clip();
    let (opacity, _visible) = extract_opacity_visible(node);
    // Track an opacity scope only when the block has fractional opacity
    // AND does NOT also clip — clip's `draw_under_clip` already wraps
    // descendants in `draw_with_opacity`, so the dual case is covered
    // there and recording it again would double-track.
    let opacity_scope = !clipping && opacity < 1.0;
    let mark = (clipping || opacity_scope).then(|| out.draw_mark());

    // Always insert the block entry — the dispatcher silently no-ops
    // when neither paint nor clip nor opacity applies, so leaving an
    // unstyled entry costs nothing and keeps abs/fixed descendants able
    // to look up their CB at render time.
    insert_block_entry(node, style, width, height, out);

    // Walk in-flow children.
    positioned::walk_children_into_drawables(doc, children, ctx, depth, out);
    // Walk pseudo + absolutely-positioned descendants.
    pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);

    if let Some(mark) = mark {
        let descendants: Vec<usize> = out
            .drawn_since(mark)
            .into_iter()
            .filter(|&id| id != node_id)
            .collect();
        if let Some(entry) = out.block_styles.get_mut(&node_id) {
            if clipping {
                entry.clip_descendants = descendants;
            } else {
                entry.opacity_descendants = descendants;
            }
        }
    }
}

/// Insert a `BlockEntry` keyed by `node.id`. Idempotent in the sense that
/// callers may re-insert; the most recent value wins. Callers should pass
/// the canonical `style` derived from the node.
fn insert_block_entry(
    node: &Node,
    style: BlockStyle,
    width: crate::units::Pt,
    height: crate::units::Pt,
    out: &mut crate::drawables::Drawables,
) {
    let (opacity, visible) = extract_opacity_visible(node);
    out.block_styles.insert(
        node.id,
        crate::drawables::BlockEntry {
            style,
            opacity,
            visible,
            id: extract_block_id(node),
            layout_size: Some(Size { width, height }),
            clip_descendants: Vec::new(),
            opacity_descendants: Vec::new(),
        },
    );
}

#[cfg(test)]
mod block_tests {
    use crate::units::F32Units;
    use std::ops::DerefMut;

    fn build_drawables(html: &str) -> crate::drawables::Drawables {
        let mut doc = crate::blitz_adapter::parse_and_layout(
            html,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            true,
        );
        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc, &[]);
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);
        let pagination_geometry = crate::pagination_layout::run_pass(doc.deref_mut(), 842.0);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = crate::convert::ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: Default::default(),
            string_set_by_node: Default::default(),
            counter_ops_by_node: Default::default(),
            bookmark_by_node: Default::default(),
            column_styles,
            multicol_geometry,
            pagination_geometry,
            link_cache: Default::default(),
            viewport_size_px: Some((595.0, 842.0)),
        };
        crate::convert::dom_to_drawables(&doc, &mut ctx)
    }

    // Covers the opacity_descendants path (block.rs line ~71):
    // a block with fractional opacity and no overflow:hidden must populate
    // `opacity_descendants` with its descendant node IDs.
    #[test]
    fn block_with_fractional_opacity_no_clip_populates_opacity_descendants() {
        let html = "<!DOCTYPE html><html><body>\
            <div style=\"opacity: 0.5; width: 200px;\">\
                <p style=\"width: 100px; height: 20px;\">text</p>\
            </div>\
            </body></html>";
        let d = build_drawables(html);
        let has_opacity_descendants = d
            .block_styles
            .values()
            .any(|e| !e.opacity_descendants.is_empty());
        assert!(
            has_opacity_descendants,
            "a block with opacity<1 and no overflow:hidden must have non-empty opacity_descendants"
        );
    }

    // Complementary: when both opacity<1 AND overflow:hidden, the clip path
    // takes precedence — clip_descendants is populated, opacity_descendants
    // stays empty for that entry.
    #[test]
    fn block_with_clip_and_opacity_uses_clip_path_not_opacity_path() {
        let html = "<!DOCTYPE html><html><body>\
            <div style=\"opacity: 0.5; overflow: hidden; width: 200px;\">\
                <p style=\"width: 100px; height: 20px;\">text</p>\
            </div>\
            </body></html>";
        let d = build_drawables(html);
        let has_clip_descendants = d
            .block_styles
            .values()
            .any(|e| !e.clip_descendants.is_empty());
        assert!(
            has_clip_descendants,
            "a block with overflow:hidden must have non-empty clip_descendants"
        );
        // The entry that clips must NOT also record opacity_descendants.
        let clipping_entry_has_opacity = d
            .block_styles
            .values()
            .any(|e| !e.clip_descendants.is_empty() && !e.opacity_descendants.is_empty());
        assert!(
            !clipping_entry_has_opacity,
            "an entry that clips must not also populate opacity_descendants"
        );
    }
}
