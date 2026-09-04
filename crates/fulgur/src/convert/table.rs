use super::*;

/// Dispatcher entry for `<table>` elements. Returns `true` when an entry
/// was inserted into `out.tables` (and any cell descendants registered).
pub(super) fn try_convert(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) -> bool {
    let Some(node) = doc.get_node(node_id) else {
        return false;
    };
    let Some(elem_data) = node.element_data() else {
        return false;
    };
    if elem_data.name.local.as_ref() != "table" {
        return false;
    }
    convert_table(doc, node, ctx, depth, out);
    true
}

fn convert_table(
    doc: &BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    let (width, height) = size_in_pt(node.final_layout.size);
    let style = extract_block_style(node, ctx.assets);
    let clipping = style.has_overflow_clip();
    let (opacity, visible) = extract_opacity_visible(node);

    out.tables.insert(
        node.id,
        crate::drawables::TableEntry {
            style,
            opacity,
            visible,
            id: extract_block_id(node),
            layout_size: Some(Size { width, height }),
            width,
            cached_height: height,
            clip_descendants: Vec::new(),
        },
    );

    let mark = clipping.then(|| out.draw_mark());

    // Walk table children to recurse cells.
    for &child_id in &node.children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        let is_thead = is_table_section(child_node, "thead");
        collect_table_cells(doc, child_id, is_thead, ctx, depth, out);
    }

    if let Some(mark) = mark {
        let descendants: Vec<usize> = out
            .drawn_since(mark)
            .into_iter()
            .filter(|&id| id != node.id)
            .collect();
        if let Some(entry) = out.tables.get_mut(&node.id) {
            entry.clip_descendants = descendants;
        }
    }
}

fn is_table_section(node: &Node, section_name: &str) -> bool {
    if let Some(elem) = node.element_data() {
        elem.name.local.as_ref() == section_name
    } else {
        false
    }
}

fn collect_table_cells(
    doc: &BaseDocument,
    node_id: usize,
    is_header: bool,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };

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

        let cw = child_node.final_layout.size.width.as_px().in_pt();
        let ch = child_node.final_layout.size.height.as_px().in_pt();

        let child_effective_is_empty = child_node
            .layout_children
            .borrow()
            .as_deref()
            .unwrap_or(&child_node.children)
            .is_empty();
        if ch == Pt::ZERO && cw == Pt::ZERO && !child_effective_is_empty {
            let child_is_header = is_header || is_table_section(child_node, "thead");
            collect_table_cells(doc, child_id, child_is_header, ctx, depth + 1, out);
            continue;
        }

        if ch == Pt::ZERO && cw == Pt::ZERO {
            continue;
        }

        // Actual cell — recurse via convert_node so its block / paragraph
        // entries land in the standard maps.
        convert_node(doc, child_id, ctx, depth + 1, out);
    }
}

#[cfg(test)]
mod tests {
    // --- comment node inside <tbody> ---
    //
    // HTML5 keeps comment nodes inside their table-section parent rather than
    // foster-parenting them. `collect_table_cells` must skip them via the
    // `NodeData::Comment` guard (table.rs:104) rather than trying to lay them
    // out, so the real cell content still reaches the drawable maps.
    #[test]
    fn table_comment_in_tbody_produces_valid_pdf() {
        let html = "<!DOCTYPE html><html><body>\
            <table>\
                <tbody>\
                    <!-- comment node exercises NodeData::Comment guard -->\
                    <tr><td>Cell</td></tr>\
                </tbody>\
            </table>\
            </body></html>";
        let pdf = crate::engine::Engine::builder()
            .build()
            .render(html)
            .expect("render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // Regression: a table entry must be created even when the only content
    // before the first real row is an HTML comment.
    #[test]
    fn table_comment_before_rows_still_registers_table_entry() {
        use crate::units::F32Units;
        use std::ops::DerefMut;

        let html = "<!DOCTYPE html><html><body>\
            <table>\
                <tbody>\
                    <!-- comment before rows -->\
                    <tr><td style=\"width:50px;height:20px\">A</td></tr>\
                </tbody>\
            </table>\
            </body></html>";

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
        let d = crate::convert::dom_to_drawables(&doc, &mut ctx);

        assert!(
            !d.tables.is_empty(),
            "table entry must be present in drawables even when tbody has a leading comment"
        );
        // Stronger assertion: the cell content must also have been converted.
        // `collect_table_cells` inserts `<td>` content via `convert_node`, which
        // produces paragraph entries for inline text. A non-empty `paragraphs`
        // map proves `collect_table_cells` walked past the comment and processed
        // the real `<tr><td>` — something `!d.tables.is_empty()` alone cannot
        // verify, since `TableEntry` is inserted before the child walk.
        assert!(
            !d.paragraphs.is_empty(),
            "cell content must be converted to paragraph drawables, \
             proving collect_table_cells walked past the comment node"
        );
    }
}
