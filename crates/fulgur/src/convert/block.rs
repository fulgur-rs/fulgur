use super::*;
use super::{positioned, pseudo};

/// Catch-all dispatch for nodes not matched by list_item / table / replaced /
/// inline_root. Always returns a `Box<dyn Pageable>`:
///
/// - Childless nodes: `SpacerPageable` (or a `BlockPageable` if pseudos / column-pagination
///   require a block wrapper).
/// - Container nodes: `BlockPageable` wrapping `positioned::collect_positioned_children`'s
///   output (with `pseudo::wrap_with_pseudo_content` applied).
///
/// Always reachable — the dispatcher invokes this last after every `try_convert` returns
/// `None`. Consequently this fn does NOT return `Option`.
pub(super) fn convert(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
    depth: usize,
) -> Box<dyn crate::pageable::Pageable> {
    let node = doc.get_node(node_id).unwrap();
    let (width, height) = size_in_pt(node.final_layout.size);

    let layout_children_guard = node.layout_children.borrow();
    let children: &[usize] = layout_children_guard.as_deref().unwrap_or(&node.children);

    if children.is_empty() {
        let style = extract_block_style(node, ctx.assets);
        let content_box = compute_content_box(node, &style);
        // Check for pseudo images even on childless elements — e.g.
        // `<div class="icon"></div>` with `.icon::before { content: url(...) }`
        // should emit the image. Without this the pseudo is silently dropped.
        let (positioned_children, has_pseudo) =
            pseudo::wrap_with_pseudo_content(doc, node, ctx, depth, content_box, Vec::new());
        if style.needs_block_wrapper() || has_pseudo {
            let (opacity, visible) = extract_opacity_visible(node);
            let mut block = BlockPageable::with_positioned_children(positioned_children)
                .with_style(style)
                .with_opacity(opacity)
                .with_visible(visible)
                .with_id(extract_block_id(node))
                .with_node_id(Some(node_id));
            block.wrap(width, height);
            block.layout_size = Some(Size { width, height });
            return Box::new(block);
        }
        // Plain leaf node — create a spacer with the computed height
        let mut spacer = SpacerPageable::new(height).with_node_id(Some(node_id));
        spacer.wrap(width, height);
        return Box::new(spacer);
    }

    // Container node — collect children with Taffy-computed positions
    let positioned_children = positioned::collect_positioned_children(doc, children, ctx, depth);
    let style = extract_block_style(node, ctx.assets);
    let content_box = compute_content_box(node, &style);
    let (positioned_children, _has_pseudo) =
        pseudo::wrap_with_pseudo_content(doc, node, ctx, depth, content_box, positioned_children);

    let has_style = style.needs_block_wrapper();
    let (opacity, visible) = extract_opacity_visible(node);
    let mut block = BlockPageable::with_positioned_children(positioned_children)
        .with_style(style)
        .with_opacity(opacity)
        .with_visible(visible)
        .with_id(extract_block_id(node))
        .with_node_id(Some(node_id));
    block.wrap(width, 10000.0);
    if has_style {
        block.layout_size = Some(Size { width, height });
    }
    Box::new(block)
}
