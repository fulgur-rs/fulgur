use super::*;

/// Dispatcher entry for `<table>` elements. Returns `Some(Pageable)` if the
/// node is an HTML `<table>` element; returns `None` otherwise so the caller
/// falls through to the next dispatch stage.
///
/// Note: signature includes `depth` (deviating from the plan template) because
/// `convert_table` recurses into child rows/cells and must propagate the
/// `MAX_DOM_DEPTH` budget.
pub(super) fn try_convert(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
    depth: usize,
) -> Option<Box<dyn crate::pageable::Pageable>> {
    let node = doc.get_node(node_id)?;
    let elem_data = node.element_data()?;
    if elem_data.name.local.as_ref() == "table" {
        return Some(convert_table(doc, node, ctx, depth));
    }
    None
}

/// Convert a table element into a TablePageable with header/body cell groups.
fn convert_table(
    doc: &BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    let (width, height) = size_in_pt(node.final_layout.size);
    let style = extract_block_style(node, ctx.assets);

    let mut header_cells: Vec<PositionedChild> = Vec::new();
    let mut body_cells: Vec<PositionedChild> = Vec::new();

    // Walk table children to separate thead from tbody
    for &child_id in &node.children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        let is_thead = is_table_section(child_node, "thead");

        collect_table_cells(
            doc,
            child_id,
            is_thead,
            &mut header_cells,
            &mut body_cells,
            ctx,
            depth,
        );
    }

    // Calculate header height from header cells
    let header_height = header_cells
        .iter()
        .fold(0.0f32, |max_h, pc| max_h.max(pc.y + pc.height));

    let (opacity, visible) = extract_opacity_visible(node);
    let table = TablePageable {
        header_cells,
        body_cells,
        header_height,
        style,
        layout_size: Some(Size { width, height }),
        width,
        cached_height: height,
        opacity,
        visible,
        id: extract_block_id(node),
        node_id: Some(node.id),
    };
    Box::new(table)
}

/// Check if a node is a specific table section element.
fn is_table_section(node: &Node, section_name: &str) -> bool {
    if let Some(elem) = node.element_data() {
        elem.name.local.as_ref() == section_name
    } else {
        false
    }
}

/// Recursively collect table cells (td/th) from a table subtree.
fn collect_table_cells(
    doc: &BaseDocument,
    node_id: usize,
    is_header: bool,
    header_cells: &mut Vec<PositionedChild>,
    body_cells: &mut Vec<PositionedChild>,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };

    // Drain counter ops on the current section/row node itself so that
    // counter-reset / counter-increment / counter-set declared on
    // <thead>/<tbody>/<tr> reach `collect_counter_states()` for margin boxes.
    // Without this, ops on these intermediate nodes stay in
    // `ctx.counter_ops_by_node` forever and never propagate.
    {
        let (x, y, _, _) = layout_in_pt(&node.final_layout);
        let out: &mut Vec<PositionedChild> = if is_header { header_cells } else { body_cells };
        emit_counter_op_markers(node_id, x, y, ctx, out);
        emit_orphan_bookmark_marker(node_id, x, y, ctx, out);
    }

    let layout_children_guard = node.layout_children.borrow();
    let effective_children = layout_children_guard.as_deref().unwrap_or(&node.children);
    for &child_id in effective_children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        if matches!(&child_node.data, NodeData::Comment) {
            continue;
        }
        if is_non_visual_element(child_node) {
            continue;
        }

        let (cx, cy, cw, ch) = layout_in_pt(&child_node.final_layout);

        // Zero-size container (tr, thead, tbody) — recurse into children
        let child_effective_is_empty = child_node
            .layout_children
            .borrow()
            .as_deref()
            .unwrap_or(&child_node.children)
            .is_empty();
        if ch == 0.0 && cw == 0.0 && !child_effective_is_empty {
            let child_is_header = is_header || is_table_section(child_node, "thead");
            collect_table_cells(
                doc,
                child_id,
                child_is_header,
                header_cells,
                body_cells,
                ctx,
                depth + 1,
            );
            continue;
        }

        // Skip zero-size leaves
        if ch == 0.0 && cw == 0.0 {
            continue;
        }

        // Actual cell (td/th) — convert and add to appropriate group
        let cell_pageable = convert_node(doc, child_id, ctx, depth + 1);
        let positioned = PositionedChild {
            child: cell_pageable,
            x: cx,
            y: cy,
            height: ch,
            out_of_flow: false,
            is_fixed: false,
        };

        if is_header {
            header_cells.push(positioned);
        } else {
            body_cells.push(positioned);
        }
    }
}
