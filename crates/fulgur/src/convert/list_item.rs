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
        let clipping = style.has_overflow_clip();
        let opacity_scope = !clipping && opacity < 1.0;
        let snapshot = (clipping || opacity_scope).then(|| collect_drawables_node_ids(out));
        build_list_item_body(doc, node, style, visible, content_box, ctx, depth, out);
        record_li_clip_opacity_descendants(node_id, clipping, snapshot, out);
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
            let clipping = style.has_overflow_clip();
            let opacity_scope = !clipping && opacity < 1.0;
            let snapshot = (clipping || opacity_scope).then(|| collect_drawables_node_ids(out));
            build_list_item_body(doc, node, style, visible, content_box, ctx, depth, out);
            record_li_clip_opacity_descendants(node_id, clipping, snapshot, out);
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
            //
            // Snapshot existing paragraph ids before the child walk so
            // `inject_marker_into_first_paragraph` only considers
            // paragraphs registered for *this* list-item's subtree.
            // Without the snapshot, `out.paragraphs.iter_mut().next()`
            // picks the lowest NodeId in the entire document (e.g. an
            // earlier `<p>` or a previous `<li>`), prepending the
            // marker to unrelated content.
            let pre_paragraph_ids: std::collections::BTreeSet<usize> =
                out.paragraphs.keys().copied().collect();
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
                if !inject_marker_into_first_paragraph(out, &pre_paragraph_ids, item.clone()) {
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

/// Inject `item` at the start of the first paragraph entry registered
/// AFTER `pre_existing_ids` was captured, returning `true` on success.
/// The first-paragraph search is iteration-order over
/// `BTreeMap<NodeId, ...>` so ordering is deterministic — matches v1's
/// depth-first walk for the inside-marker fallback path. Restricting to
/// post-snapshot ids keeps the marker scoped to the current list
/// item's subtree (a sibling list item or earlier `<p>` that registered
/// before the snapshot is excluded).
fn inject_marker_into_first_paragraph(
    out: &mut crate::drawables::Drawables,
    pre_existing_ids: &std::collections::BTreeSet<usize>,
    item: LineItem,
) -> bool {
    let Some((_, entry)) = out
        .paragraphs
        .iter_mut()
        .find(|(id, _)| !pre_existing_ids.contains(id))
    else {
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

/// Fill in `clip_descendants` / `opacity_descendants` on an already-inserted
/// `BlockEntry` keyed by `node_id`, using the before/after snapshot diff.
/// `snapshot` is `Some` only when the caller decided this node has a clip
/// or opacity scope worth tracking.
fn record_li_clip_opacity_descendants(
    node_id: usize,
    clipping: bool,
    snapshot: Option<std::collections::BTreeSet<usize>>,
    out: &mut crate::drawables::Drawables,
) {
    let Some(before) = snapshot else { return };
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

#[cfg(test)]
mod tests {
    use super::{inject_marker_into_first_paragraph, record_li_clip_opacity_descendants};
    use crate::drawables::{BlockEntry, Drawables, ParagraphEntry};
    use crate::paragraph::{
        InlineBoxItem, InlineImage, LineItem, ShapedGlyph, ShapedGlyphRun, ShapedLine,
        TextDecoration, VerticalAlign,
    };
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn make_line(items: Vec<LineItem>) -> ShapedLine {
        ShapedLine {
            height: 12.0,
            baseline: 9.0,
            items,
        }
    }

    fn make_para(lines: Vec<ShapedLine>) -> ParagraphEntry {
        ParagraphEntry {
            lines,
            opacity: 1.0,
            visible: true,
            id: None,
        }
    }

    fn inline_box(width: f32, x_offset: f32) -> LineItem {
        LineItem::InlineBox(InlineBoxItem {
            node_id: None,
            width,
            height: 10.0,
            x_offset,
            computed_y: 0.0,
            link: None,
            opacity: 1.0,
            visible: true,
        })
    }

    fn make_block_entry() -> BlockEntry {
        BlockEntry {
            style: crate::draw_primitives::BlockStyle::default(),
            opacity: 1.0,
            visible: true,
            id: None,
            layout_size: None,
            clip_descendants: vec![],
            opacity_descendants: vec![],
        }
    }

    // ── inject_marker_into_first_paragraph ────────────────────────────

    #[test]
    fn inject_returns_false_when_paragraphs_is_empty() {
        let mut out = Drawables::new();
        let pre = BTreeSet::new();
        assert!(!inject_marker_into_first_paragraph(
            &mut out,
            &pre,
            inline_box(10.0, 0.0)
        ));
    }

    #[test]
    fn inject_returns_false_when_all_paragraphs_pre_existing() {
        let mut out = Drawables::new();
        out.paragraphs.insert(1, make_para(vec![make_line(vec![])]));
        let pre: BTreeSet<usize> = [1].into();
        assert!(!inject_marker_into_first_paragraph(
            &mut out,
            &pre,
            inline_box(10.0, 0.0)
        ));
    }

    #[test]
    fn inject_returns_false_when_new_paragraph_has_no_lines() {
        let mut out = Drawables::new();
        out.paragraphs.insert(5, make_para(vec![])); // new but no lines
        let pre: BTreeSet<usize> = BTreeSet::new();
        assert!(!inject_marker_into_first_paragraph(
            &mut out,
            &pre,
            inline_box(10.0, 0.0)
        ));
    }

    #[test]
    fn inject_inline_box_marker_is_prepended_to_first_line() {
        let mut out = Drawables::new();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 0.0)])]));
        let pre: BTreeSet<usize> = BTreeSet::new();
        assert!(inject_marker_into_first_paragraph(
            &mut out,
            &pre,
            inline_box(10.0, 0.0)
        ));
        let first_line = &out.paragraphs[&5].lines[0];
        assert_eq!(first_line.items.len(), 2);
        // Newly-inserted marker is at index 0.
        let LineItem::InlineBox(marker_ib) = &first_line.items[0] else {
            panic!("expected InlineBox at index 0");
        };
        assert_eq!(marker_ib.width, 10.0);
    }

    #[test]
    fn inject_shifts_existing_inline_box_by_marker_width() {
        let mut out = Drawables::new();
        // Existing item at x_offset=5.
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 5.0)])]));
        let pre: BTreeSet<usize> = BTreeSet::new();
        inject_marker_into_first_paragraph(&mut out, &pre, inline_box(10.0, 0.0));
        // shift = marker width = 10 → existing item should now be at x_offset = 5 + 10 = 15.
        let LineItem::InlineBox(existing) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected InlineBox at index 1");
        };
        assert!(
            (existing.x_offset - 15.0).abs() < 0.001,
            "got {}",
            existing.x_offset
        );
    }

    #[test]
    fn inject_image_marker_shifts_existing_by_image_width() {
        let mut out = Drawables::new();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 3.0)])]));
        let pre: BTreeSet<usize> = BTreeSet::new();
        let image_marker = LineItem::Image(InlineImage {
            data: Arc::new(vec![]),
            format: crate::image::ImageFormat::Png,
            width: 8.0,
            height: 8.0,
            x_offset: 0.0,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: 0.0,
            link: None,
        });
        assert!(inject_marker_into_first_paragraph(
            &mut out,
            &pre,
            image_marker
        ));
        // shift = image width = 8 → existing at x_offset=3 → 3 + 8 = 11.
        let LineItem::InlineBox(ib) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected InlineBox at index 1");
        };
        assert!((ib.x_offset - 11.0).abs() < 0.001, "got {}", ib.x_offset);
    }

    #[test]
    fn inject_text_marker_shifts_by_sum_of_advance_times_font_size() {
        let mut out = Drawables::new();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 0.0)])]));
        let pre: BTreeSet<usize> = BTreeSet::new();
        // Two glyphs, each x_advance=0.5, font_size=12 → shift = 2 × 0.5 × 12 = 12.
        let text_marker = LineItem::Text(ShapedGlyphRun {
            font_data: Arc::new(vec![]),
            font_index: 0,
            font_size: 12.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![
                ShapedGlyph {
                    id: 0,
                    x_advance: 0.5,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    text_range: 0..1,
                },
                ShapedGlyph {
                    id: 0,
                    x_advance: 0.5,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    text_range: 1..2,
                },
            ],
            text: "• ".to_string(),
            x_offset: 0.0,
            link: None,
        });
        inject_marker_into_first_paragraph(&mut out, &pre, text_marker);
        let LineItem::InlineBox(ib) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected InlineBox at index 1");
        };
        assert!((ib.x_offset - 12.0).abs() < 0.001, "got {}", ib.x_offset);
    }

    #[test]
    fn inject_picks_lowest_new_node_id_via_btreemap_ordering() {
        // BTreeMap iterates in ascending key order → node 5 is picked before node 10.
        let mut out = Drawables::new();
        out.paragraphs
            .insert(10, make_para(vec![make_line(vec![])]));
        out.paragraphs.insert(5, make_para(vec![make_line(vec![])]));
        let pre: BTreeSet<usize> = BTreeSet::new();
        assert!(inject_marker_into_first_paragraph(
            &mut out,
            &pre,
            inline_box(10.0, 0.0)
        ));
        // Marker went into node 5 (the lower key).
        assert_eq!(out.paragraphs[&5].lines[0].items.len(), 1);
        assert_eq!(out.paragraphs[&10].lines[0].items.len(), 0);
    }

    // ── record_li_clip_opacity_descendants ────────────────────────────

    #[test]
    fn record_none_snapshot_is_noop() {
        let mut out = Drawables::new();
        out.block_styles.insert(1, make_block_entry());
        record_li_clip_opacity_descendants(1, true, None, &mut out);
        assert!(out.block_styles[&1].clip_descendants.is_empty());
        assert!(out.block_styles[&1].opacity_descendants.is_empty());
    }

    #[test]
    fn record_clipping_true_fills_clip_descendants_and_excludes_self() {
        let mut out = Drawables::new();
        out.block_styles.insert(10, make_block_entry()); // parent
        out.block_styles.insert(20, make_block_entry()); // child 1
        out.block_styles.insert(30, make_block_entry()); // child 2
        // Snapshot captured before the children were walked — only node 10 existed then.
        let pre: BTreeSet<usize> = [10].into();
        record_li_clip_opacity_descendants(10, true, Some(pre), &mut out);
        let mut got = out.block_styles[&10].clip_descendants.clone();
        got.sort_unstable();
        assert_eq!(got, vec![20usize, 30]);
        assert!(out.block_styles[&10].opacity_descendants.is_empty());
    }

    #[test]
    fn record_clipping_false_fills_opacity_descendants_and_excludes_self() {
        let mut out = Drawables::new();
        out.block_styles.insert(10, make_block_entry());
        out.block_styles.insert(11, make_block_entry());
        let pre: BTreeSet<usize> = [10].into();
        record_li_clip_opacity_descendants(10, false, Some(pre), &mut out);
        assert_eq!(out.block_styles[&10].opacity_descendants, vec![11usize]);
        assert!(out.block_styles[&10].clip_descendants.is_empty());
    }

    #[test]
    fn record_missing_block_entry_does_not_panic() {
        let mut out = Drawables::new();
        out.block_styles.insert(20, make_block_entry());
        let pre: BTreeSet<usize> = BTreeSet::new();
        // node_id 10 has no entry in block_styles — must not panic.
        record_li_clip_opacity_descendants(10, true, Some(pre), &mut out);
    }

    #[test]
    fn record_excludes_node_id_even_when_snapshot_does_not_contain_it() {
        // pre is empty, so after − before = {10, 11}. The `.filter(|&id| id != node_id)`
        // guard must drop 10 so clip_descendants contains only the child.
        let mut out = Drawables::new();
        out.block_styles.insert(10, make_block_entry());
        out.block_styles.insert(11, make_block_entry());
        let pre: BTreeSet<usize> = BTreeSet::new();
        record_li_clip_opacity_descendants(10, true, Some(pre), &mut out);
        assert_eq!(out.block_styles[&10].clip_descendants, vec![11usize]);
        assert!(out.block_styles[&10].opacity_descendants.is_empty());
    }
}
