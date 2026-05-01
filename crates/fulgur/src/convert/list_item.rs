use super::*;
use super::{inline_root, list_marker, positioned, pseudo};

/// Dispatcher entry for list-item nodes.
///
/// Returns `true` when the node was registered as a list item (with
/// matching `BlockEntry` for the body, and `ListItemEntry` for the
/// marker). Returns `false` to fall through to the next dispatch stage.
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
    let (width, height) = size_in_pt(node.final_layout.size);

    // Outside marker (must run before inline-root check).
    if let Some(elem_data) = node.element_data()
        && elem_data.list_item_data.as_ref().is_some_and(|d| {
            crate::blitz_adapter::list_position_outside_layout(&d.position).is_some()
        })
    {
        let (marker_lines, marker_width, marker_line_height) =
            list_marker::extract_marker_lines(doc, node, ctx);
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);

        let marker = list_marker::resolve_list_marker(node, marker_line_height, ctx.assets)
            .unwrap_or(crate::drawables::ListItemMarker::Text {
                lines: marker_lines,
                width: marker_width,
            });

        out.list_items.insert(
            node_id,
            crate::drawables::ListItemEntry {
                marker,
                marker_line_height,
                opacity,
                visible,
            },
        );
        // Body block carries the node's style + layout for paint dispatch.
        out.block_styles.insert(
            node_id,
            crate::drawables::BlockEntry {
                style: style.clone(),
                opacity,
                visible,
                id: extract_block_id(node),
                layout_size: Some(Size { width, height }),
                clip_descendants: Vec::new(),
                opacity_descendants: Vec::new(),
            },
        );
        let content_box = compute_content_box(node, &style);
        build_list_item_body(doc, node, style, visible, content_box, ctx, depth, out);
        return true;
    }

    // Fallback: display: list-item with list-style-image but no list_item_data.
    if let Some(styles) = node.primary_styles()
        && styles.get_box().display.is_list_item()
        && node
            .element_data()
            .is_none_or(|e| e.list_item_data.is_none())
    {
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);

        let line_height = {
            use ::style::values::computed::font::LineHeight;
            let font_size_pt = px_to_pt(styles.clone_font_size().used_size().px());
            match styles.clone_line_height() {
                LineHeight::Normal => font_size_pt * DEFAULT_LINE_HEIGHT_RATIO,
                LineHeight::Number(num) => font_size_pt * num.0,
                LineHeight::Length(value) => px_to_pt(value.0.px()),
            }
        };

        if let Some(marker) = list_marker::resolve_list_marker(node, line_height, ctx.assets) {
            out.list_items.insert(
                node_id,
                crate::drawables::ListItemEntry {
                    marker,
                    marker_line_height: line_height,
                    opacity,
                    visible,
                },
            );
            out.block_styles.insert(
                node_id,
                crate::drawables::BlockEntry {
                    style: style.clone(),
                    opacity,
                    visible,
                    id: extract_block_id(node),
                    layout_size: Some(Size { width, height }),
                    clip_descendants: Vec::new(),
                    opacity_descendants: Vec::new(),
                },
            );
            let content_box = compute_content_box(node, &style);
            build_list_item_body(doc, node, style, visible, content_box, ctx, depth, out);
            return true;
        }
    }

    // Inside-positioned marker on non-inline-root <li>.
    if let Some(elem_data) = node.element_data()
        && let Some(list_data) = &elem_data.list_item_data
        && crate::blitz_adapter::is_list_position_inside(&list_data.position)
        && !node.flags.is_inline_root()
    {
        let marker = &list_data.marker;
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);
        let content_box = compute_content_box(node, &style);

        let (font_size_pt, line_height) = if let Some(styles) = node.primary_styles() {
            let fs = px_to_pt(styles.clone_font_size().used_size().px());
            let lh = {
                use ::style::values::computed::font::LineHeight;
                match styles.clone_line_height() {
                    LineHeight::Normal => fs * DEFAULT_LINE_HEIGHT_RATIO,
                    LineHeight::Number(num) => fs * num.0,
                    LineHeight::Length(value) => px_to_pt(value.0.px()),
                }
            };
            (fs, lh)
        } else {
            (px_to_pt(12.0), px_to_pt(12.0) * DEFAULT_LINE_HEIGHT_RATIO)
        };

        let color = get_text_color(doc, node_id);

        let layout_children_guard_inside = node.layout_children.borrow();
        let children: &[usize] = layout_children_guard_inside
            .as_deref()
            .unwrap_or(&node.children);

        // For the empty-li case we need the marker BEFORE walking; the
        // non-empty case computes its marker after the child walk so the
        // font lookup can fall back to a child paragraph's font.
        let empty_li_marker_item: Option<LineItem> =
            list_marker::resolve_inside_image_marker(node, line_height, ctx.assets)
                .map(LineItem::Image)
                .or_else(|| {
                    let (fd, fi) = list_marker::find_marker_font(marker, ctx.assets, out)?;
                    let run = list_marker::shape_marker_with_skrifa(
                        marker,
                        &fd,
                        fi,
                        font_size_pt,
                        color,
                    )?;
                    Some(LineItem::Text(run))
                });

        if children.is_empty() {
            // Empty <li>: standalone paragraph with just the marker.
            if let Some(item) = empty_li_marker_item {
                let lines = vec![ShapedLine {
                    height: line_height,
                    baseline: line_height / DEFAULT_LINE_HEIGHT_RATIO,
                    items: vec![item],
                }];
                out.paragraphs.insert(
                    node_id,
                    crate::drawables::ParagraphEntry {
                        lines,
                        opacity: 1.0,
                        visible,
                        id: extract_block_id(node),
                    },
                );
                out.block_styles.insert(
                    node_id,
                    crate::drawables::BlockEntry {
                        style: style.clone(),
                        opacity,
                        visible,
                        id: extract_block_id(node),
                        layout_size: Some(Size { width, height }),
                        clip_descendants: Vec::new(),
                        opacity_descendants: Vec::new(),
                    },
                );
                pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);
                return true;
            }
            // No marker — fall through to normal empty-element handling.
        } else {
            // Non-empty <li>: walk children first so `find_marker_font`
            // can fall back to a child paragraph's font. Then inject the
            // marker into a descendant paragraph if any. Without a
            // paragraph descendant we synthesize a marker-only paragraph
            // at the li level.
            positioned::walk_children_into_drawables(doc, children, ctx, depth, out);
            pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);

            let marker_item: Option<LineItem> =
                list_marker::resolve_inside_image_marker(node, line_height, ctx.assets)
                    .map(LineItem::Image)
                    .or_else(|| {
                        let (fd, fi) = list_marker::find_marker_font(marker, ctx.assets, out)?;
                        let run = list_marker::shape_marker_with_skrifa(
                            marker,
                            &fd,
                            fi,
                            font_size_pt,
                            color,
                        )?;
                        Some(LineItem::Text(run))
                    });

            if let Some(item) = marker_item {
                if !inject_marker_into_first_paragraph(out, item.clone()) {
                    let lines = vec![ShapedLine {
                        height: line_height,
                        baseline: line_height / DEFAULT_LINE_HEIGHT_RATIO,
                        items: vec![item],
                    }];
                    out.paragraphs.insert(
                        node_id,
                        crate::drawables::ParagraphEntry {
                            lines,
                            opacity: 1.0,
                            visible,
                            id: extract_block_id(node),
                        },
                    );
                }
            }
            out.block_styles.insert(
                node_id,
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
            return true;
        }
    }

    false
}

/// Inject `item` at the start of the first paragraph entry currently in
/// `out.paragraphs`, returning `true` on success. The first-paragraph
/// search is iteration-order over `BTreeMap<NodeId, ...>` so ordering is
/// deterministic — matches v1's depth-first walk for the inside-marker
/// fallback path.
fn inject_marker_into_first_paragraph(
    out: &mut crate::drawables::Drawables,
    item: LineItem,
) -> bool {
    let Some((_, entry)) = out.paragraphs.iter_mut().next() else {
        return false;
    };
    let Some(first_line) = entry.lines.first_mut() else {
        return false;
    };
    let shift = match &item {
        LineItem::Text(run) => run
            .glyphs
            .iter()
            .map(|g| g.x_advance * run.font_size)
            .sum::<f32>(),
        LineItem::Image(img) => img.width,
        LineItem::InlineBox(ib) => ib.width,
    };
    for existing in &mut first_line.items {
        match existing {
            LineItem::Text(run) => run.x_offset += shift,
            LineItem::Image(i) => i.x_offset += shift,
            LineItem::InlineBox(ib) => ib.x_offset += shift,
        }
    }
    first_line.items.insert(0, item);
    true
}

/// Build the body for a list-item node (outside marker / fallback path).
/// Walks the body content into `out`, applying the same paragraph /
/// pseudo-image logic the inline-root path uses.
#[allow(clippy::too_many_arguments)]
fn build_list_item_body(
    doc: &BaseDocument,
    node: &Node,
    style: BlockStyle,
    visible: bool,
    content_box: ContentBox,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    if node.flags.is_inline_root() {
        let paragraph_opt = inline_root::extract_paragraph(doc, node, ctx, depth, out);

        let before_inline = node
            .before
            .and_then(|id| doc.get_node(id))
            .filter(|p| !pseudo::is_block_pseudo(p))
            .and_then(|p| {
                pseudo::build_inline_pseudo_image(
                    p,
                    content_box.width,
                    content_box.height,
                    ctx.assets,
                )
            })
            .map(|mut img| {
                pseudo::attach_link_to_inline_image(&mut img, doc, node.id);
                img
            });
        let after_inline = node
            .after
            .and_then(|id| doc.get_node(id))
            .filter(|p| !pseudo::is_block_pseudo(p))
            .and_then(|p| {
                pseudo::build_inline_pseudo_image(
                    p,
                    content_box.width,
                    content_box.height,
                    ctx.assets,
                )
            })
            .map(|mut img| {
                pseudo::attach_link_to_inline_image(&mut img, doc, node.id);
                img
            });

        if let Some(mut paragraph) = paragraph_opt {
            if before_inline.is_some() || after_inline.is_some() {
                pseudo::inject_inline_pseudo_images(
                    &mut paragraph.lines,
                    before_inline,
                    after_inline,
                );
                inline_root::recalculate_paragraph_line_boxes(&mut paragraph.lines);
                paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
            }
            out.paragraphs.insert(
                node.id,
                crate::drawables::ParagraphEntry {
                    lines: paragraph.lines,
                    opacity: 1.0,
                    visible,
                    id: extract_block_id(node),
                },
            );
            // Always register pseudo content for non-inline-root path
            // consistency; it is a no-op when there is none.
            let _ = style; // style is reused by the caller's BlockEntry.
            pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);
        } else if before_inline.is_some() || after_inline.is_some() {
            let mut line = ShapedLine {
                height: 0.0,
                baseline: 0.0,
                items: vec![],
            };
            pseudo::inject_inline_pseudo_images(
                std::slice::from_mut(&mut line),
                before_inline,
                after_inline,
            );
            let font_metrics = inline_root::metrics_from_line(&line);
            crate::paragraph::recalculate_line_box(&mut line, &font_metrics);
            out.paragraphs.insert(
                node.id,
                crate::drawables::ParagraphEntry {
                    lines: vec![line],
                    opacity: 1.0,
                    visible,
                    id: extract_block_id(node),
                },
            );
            pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);
        } else {
            // Inline root with no text and no inline pseudo images — fall
            // through to non-inline-root walk.
            let layout_children_guard_1 = node.layout_children.borrow();
            let children: &[usize] = layout_children_guard_1.as_deref().unwrap_or(&node.children);
            positioned::walk_children_into_drawables(doc, children, ctx, depth, out);
            pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);
        }
    } else {
        let layout_children_guard_2 = node.layout_children.borrow();
        let children: &[usize] = layout_children_guard_2.as_deref().unwrap_or(&node.children);
        positioned::walk_children_into_drawables(doc, children, ctx, depth, out);
        pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);
    }
}
