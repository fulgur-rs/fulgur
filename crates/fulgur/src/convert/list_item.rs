use super::*;
use super::{inline_root, list_marker, positioned, pseudo};

/// Dispatcher entry for list-item nodes. Tries three branches in this order:
///
/// 1. Outside marker (`list_item_data.position == Outside(_)`).
/// 2. `display: list-item` fallback when Blitz didn't populate `list_item_data`
///    (e.g. `list-style-type: none` + `list-style-image`).
/// 3. Inside-positioned marker on a non-inline-root `<li>` (e.g. `<li><p>...`).
///
/// Returns `Some(Pageable)` on a successful build; returns `None` to let the
/// dispatcher fall through to the next stage. Branches 2 and 3 may fall
/// through individually when marker resolution fails — the original dispatcher
/// continues to evaluate later stages, and `try_convert` preserves that.
///
/// MUST run before table/replaced/inline_root/block dispatch (see plan).
pub(super) fn try_convert(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
    depth: usize,
) -> Option<Box<dyn crate::pageable::Pageable>> {
    let node = doc.get_node(node_id)?;
    let (width, height) = size_in_pt(node.final_layout.size);

    // Check if this is a list item with an outside marker (must be before inline root check).
    //
    // Inside-positioned markers are injected into Parley's inline layout by Blitz
    // (blitz-dom/src/layout/construct.rs in `build_inline_layout`), so when the
    // `<li>` IS an inline root they fall through to the normal paragraph path below
    // and render correctly. For `list-style-image` + inside, `resolve_inside_image_marker`
    // injects the image at the start of the paragraph's first line.
    //
    // Known limitation: when `<li>` is NOT an inline root (contains only block
    // children, e.g. `<li><p>...</p></li>`) or is empty, neither Blitz nor
    // fulgur injects the marker, and the marker is not rendered. This matches
    // upstream Blitz behavior — Blitz's inline-layout injection only fires for
    // inline-root elements.
    if let Some(elem_data) = node.element_data()
        && elem_data.list_item_data.as_ref().is_some_and(|d| {
            crate::blitz_adapter::list_position_outside_layout(&d.position).is_some()
        })
    {
        let (marker_lines, marker_width, marker_line_height) =
            list_marker::extract_marker_lines(doc, node, ctx);
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);

        // Try list-style-image first; fall back to text marker if unresolved.
        let marker = list_marker::resolve_list_marker(node, marker_line_height, ctx.assets)
            .unwrap_or(ListItemMarker::Text {
                lines: marker_lines,
                width: marker_width,
            });

        // Build body WITHOUT opacity — ListItemPageable wraps everything in
        // a single opacity group. But DO propagate visibility to the body's
        // own content (paragraph/image), since those are synthetic children
        // representing the node's own content, not real CSS children.
        let content_box = compute_content_box(node, &style);
        let body = build_list_item_body(
            doc,
            node,
            style,
            visible,
            width,
            height,
            content_box,
            ctx,
            depth,
        );
        let mut item = ListItemPageable {
            marker,
            marker_line_height,
            body,
            style: BlockStyle::default(),
            width,
            height: 0.0,
            opacity,
            visible,
        };
        item.wrap(width, 10000.0);
        return Some(Box::new(item));
    }

    // Fallback: display: list-item with list-style-image but no list_item_data
    // (Blitz 0.2.4 skips list_item_data when list-style-type: none).
    //
    // The primary guard above now only matches Outside-positioned items, so we
    // must additionally require `list_item_data.is_none()` here to avoid
    // intercepting inside-positioned items that DO have list_item_data — those
    // should fall through to the inline-root path so `resolve_inside_image_marker`
    // can inject the marker inline.
    if let Some(styles) = node.primary_styles()
        && styles.get_box().display.is_list_item()
        && node
            .element_data()
            .is_none_or(|e| e.list_item_data.is_none())
    {
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);

        // Derive line_height from computed styles since there is no Parley layout.
        // Honour explicit line-height first; fall back to font-size * 1.2 for
        // `normal`, matching the same heuristic Blitz uses internally.
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
            let content_box = compute_content_box(node, &style);
            let body = build_list_item_body(
                doc,
                node,
                style,
                visible,
                width,
                height,
                content_box,
                ctx,
                depth,
            );
            let mut item = ListItemPageable {
                marker,
                marker_line_height: line_height,
                body,
                style: BlockStyle::default(),
                width,
                height: 0.0,
                opacity,
                visible,
            };
            item.wrap(width, 10000.0);
            return Some(Box::new(item));
        }
    }

    // Inside-positioned marker on non-inline-root <li> (e.g. `<li><p>text</p></li>`
    // or empty `<li>`). Blitz only injects inside markers via `build_inline_layout`,
    // which doesn't run for non-inline-root elements. We shape the marker with
    // skrifa and inject it into the first child ParagraphPageable.
    if let Some(elem_data) = node.element_data()
        && let Some(list_data) = &elem_data.list_item_data
        && crate::blitz_adapter::is_list_position_inside(&list_data.position)
        && !node.flags.is_inline_root()
    {
        let marker = &list_data.marker;
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);
        let content_box = compute_content_box(node, &style);

        // Derive font_size and line_height from computed styles.
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
        if children.is_empty() {
            // Empty <li>: create a standalone paragraph with just the marker.
            // Try image marker first (list-style-image), then text fallback.
            let marker_item: Option<LineItem> =
                list_marker::resolve_inside_image_marker(node, line_height, ctx.assets)
                    .map(LineItem::Image)
                    .or_else(|| {
                        let (fd, fi) = list_marker::find_marker_font(marker, ctx.assets, &[])?;
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
                let paragraph = ParagraphPageable::new(vec![ShapedLine {
                    height: line_height,
                    baseline: line_height / DEFAULT_LINE_HEIGHT_RATIO,
                    items: vec![item],
                }]);
                let (child_x, child_y) = style.content_inset();
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(paragraph),
                    x: child_x,
                    y: child_y,
                    out_of_flow: false,
                    is_fixed: false,
                }];
                let (positioned_children, _has_pseudo) = pseudo::wrap_with_pseudo_content(
                    doc,
                    node,
                    ctx,
                    depth,
                    content_box,
                    paragraph_children,
                );
                let needs_wrapper = style.needs_block_wrapper();
                let mut block = BlockPageable::with_positioned_children(positioned_children)
                    .with_pagination(extract_pagination_from_column_css(ctx, node))
                    .with_style(style)
                    .with_opacity(opacity)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, 10000.0);
                if needs_wrapper {
                    block.layout_size = Some(Size { width, height });
                }
                return Some(Box::new(block));
            }
            // No marker resolved — fall through to normal empty-element handling
        } else {
            // Non-empty <li> with block children: convert children, then inject
            // marker into the first ParagraphPageable found in the tree.
            // Try image marker first (list-style-image), then text fallback.
            let mut positioned_children =
                positioned::collect_positioned_children(doc, children, ctx, depth);

            let marker_item: Option<LineItem> =
                list_marker::resolve_inside_image_marker(node, line_height, ctx.assets)
                    .map(LineItem::Image)
                    .or_else(|| {
                        let (fd, fi) = list_marker::find_marker_font(
                            marker,
                            ctx.assets,
                            &positioned_children,
                        )?;
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
                if !list_marker::inject_inside_marker_item_into_children(
                    &mut positioned_children,
                    item.clone(),
                ) {
                    // No paragraph descendant found — insert a standalone marker paragraph.
                    let paragraph = ParagraphPageable::new(vec![ShapedLine {
                        height: line_height,
                        baseline: line_height / DEFAULT_LINE_HEIGHT_RATIO,
                        items: vec![item],
                    }]);
                    positioned_children.insert(
                        0,
                        PositionedChild {
                            child: Box::new(paragraph),
                            x: 0.0,
                            y: 0.0,
                            out_of_flow: false,
                            is_fixed: false,
                        },
                    );
                }
            }

            let (positioned_children, _has_pseudo) = pseudo::wrap_with_pseudo_content(
                doc,
                node,
                ctx,
                depth,
                content_box,
                positioned_children,
            );
            let has_style = style.needs_block_wrapper();
            let mut block = BlockPageable::with_positioned_children(positioned_children)
                .with_pagination(extract_pagination_from_column_css(ctx, node))
                .with_style(style)
                .with_opacity(opacity)
                .with_visible(visible)
                .with_id(extract_block_id(node));
            block.wrap(width, 10000.0);
            if has_style {
                block.layout_size = Some(Size { width, height });
            }
            return Some(Box::new(block));
        }
    }

    None
}

/// Build the body pageable for a list-item node.
///
/// Both the primary list-item path (where Blitz populates `list_item_data`)
/// and the fallback path (image-only markers with `list-style-type: none`)
/// share this logic. It handles inline pseudo images, paragraph extraction,
/// `needs_block_wrapper` + `layout_size`, synthesised paragraphs for
/// pseudo-only items, and non-inline-root block child collection.
#[allow(clippy::too_many_arguments)]
fn build_list_item_body(
    doc: &BaseDocument,
    node: &Node,
    style: BlockStyle,
    visible: bool,
    width: f32,
    height: f32,
    content_box: ContentBox,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    if node.flags.is_inline_root() {
        let paragraph_opt = inline_root::extract_paragraph(doc, node, ctx, depth);

        // Inline pseudo images for list item body
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

            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            let pagination = extract_pagination_from_column_css(ctx, node);
            let needs_wrapper = style.needs_block_wrapper()
                || has_pseudo
                || pagination != crate::pageable::Pagination::default();
            if needs_wrapper {
                let (child_x, child_y) = style.content_inset();
                let mut p = paragraph;
                p.visible = visible;
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(p),
                    x: child_x,
                    y: child_y,
                    out_of_flow: false,
                    is_fixed: false,
                }];
                let mut children = pseudo::wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    paragraph_children,
                );
                children.extend(abs_pseudos);
                let mut block = BlockPageable::with_positioned_children(children)
                    .with_pagination(pagination)
                    .with_style(style)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                Box::new(block)
            } else {
                let mut p = paragraph;
                p.visible = visible;
                Box::new(p)
            }
        } else if before_inline.is_some() || after_inline.is_some() {
            // Synthesize a minimal paragraph for pseudo-only list items
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
            let mut paragraph = ParagraphPageable::new(vec![line]);
            paragraph.visible = visible;

            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            let pagination = extract_pagination_from_column_css(ctx, node);
            if style.needs_block_wrapper()
                || has_pseudo
                || pagination != crate::pageable::Pagination::default()
            {
                let (child_x, child_y) = style.content_inset();
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(paragraph),
                    x: child_x,
                    y: child_y,
                    out_of_flow: false,
                    is_fixed: false,
                }];
                let mut children = pseudo::wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    paragraph_children,
                );
                children.extend(abs_pseudos);
                let mut block = BlockPageable::with_positioned_children(children)
                    .with_pagination(pagination)
                    .with_style(style)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                Box::new(block)
            } else {
                Box::new(paragraph)
            }
        } else {
            // Inline root with no text and no inline pseudo images —
            // fall through to the non-inline-root path below.
            let layout_children_guard_1 = node.layout_children.borrow();
            let children: &[usize] = layout_children_guard_1.as_deref().unwrap_or(&node.children);
            let positioned_children =
                positioned::collect_positioned_children(doc, children, ctx, depth);
            let (positioned_children, _has_pseudo) = pseudo::wrap_with_pseudo_content(
                doc,
                node,
                ctx,
                depth,
                content_box,
                positioned_children,
            );
            let mut block = BlockPageable::with_positioned_children(positioned_children)
                .with_pagination(extract_pagination_from_column_css(ctx, node))
                .with_style(style)
                .with_visible(visible)
                .with_id(extract_block_id(node));
            block.wrap(width, 10000.0);
            Box::new(block)
        }
    } else {
        let layout_children_guard_2 = node.layout_children.borrow();
        let children: &[usize] = layout_children_guard_2.as_deref().unwrap_or(&node.children);
        let positioned_children =
            positioned::collect_positioned_children(doc, children, ctx, depth);
        let (positioned_children, _has_pseudo) = pseudo::wrap_with_pseudo_content(
            doc,
            node,
            ctx,
            depth,
            content_box,
            positioned_children,
        );
        let mut block = BlockPageable::with_positioned_children(positioned_children)
            .with_pagination(extract_pagination_from_column_css(ctx, node))
            .with_style(style)
            .with_visible(visible)
            .with_id(extract_block_id(node));
        block.wrap(width, 10000.0);
        Box::new(block)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::find_marker_text_in_tree;
    use super::super::{ConvertContext, dom_to_pageable};
    use std::collections::HashMap;

    #[test]
    fn inside_marker_on_block_child_li() {
        let html = r#"<html><body><ul style="list-style-position:inside"><li><p>hello</p></li></ul></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            pagination_geometry: ::std::collections::BTreeMap::new(),
            link_cache: Default::default(),
            viewport_size_px: None,
        };
        let tree = dom_to_pageable(&doc, &mut ctx);
        assert!(
            find_marker_text_in_tree(&*tree, "\u{2022}"),
            "inside marker bullet should be injected into <li><p>hello</p></li>"
        );
    }

    #[test]
    fn inside_marker_on_empty_li() {
        let html =
            r#"<html><body><ul style="list-style-position:inside"><li></li></ul></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            pagination_geometry: ::std::collections::BTreeMap::new(),
            link_cache: Default::default(),
            viewport_size_px: None,
        };
        let tree = dom_to_pageable(&doc, &mut ctx);
        // Empty <li> with no AssetBundle fonts: marker may not render if no
        // system font covers the bullet. We still verify no panic occurs.
        // When a system font IS available, the marker should be present.
        let _found = find_marker_text_in_tree(&*tree, "\u{2022}");
        // Not asserting found==true because system font availability varies.
    }

    #[test]
    fn inside_marker_on_block_child_ol() {
        let html = r#"<html><body><ol style="list-style-position:inside"><li><p>hello</p></li></ol></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            pagination_geometry: ::std::collections::BTreeMap::new(),
            link_cache: Default::default(),
            viewport_size_px: None,
        };
        let tree = dom_to_pageable(&doc, &mut ctx);
        assert!(
            find_marker_text_in_tree(&*tree, "1."),
            "inside marker '1.' should be injected into <li><p>hello</p></li> in <ol>"
        );
    }
}
