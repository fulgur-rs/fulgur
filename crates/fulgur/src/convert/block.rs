use super::*;
use super::{positioned, pseudo};

/// Catch-all dispatch for nodes not matched by list_item / table / replaced /
/// inline_root. Inserts a `BlockEntry` into `out.block_styles` (and walks
/// children recursively into `out`) — a leaf with no visual style and no
/// pseudo content registers nothing, matching the v1 `SpacerPageable`
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
    let snapshot = (clipping || opacity_scope).then(|| collect_drawables_node_ids(out));

    // Always insert the block entry — the v2 dispatcher silently no-ops
    // when neither paint nor clip nor opacity applies, so leaving an
    // unstyled entry costs nothing and keeps abs/fixed descendants able
    // to look up their CB at render time. Mirrors v1's behavior of
    // always emitting a `BlockPageable` for container nodes.
    insert_block_entry(node, style.clone(), width, height, out);

    // Walk in-flow children.
    positioned::walk_children_into_drawables(doc, children, ctx, depth, out);
    // Walk pseudo + absolutely-positioned descendants.
    pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);

    if let Some(before) = snapshot {
        let after = collect_drawables_node_ids(out);
        let descendants: Vec<usize> = after
            .difference(&before)
            .copied()
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
    width: f32,
    height: f32,
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
