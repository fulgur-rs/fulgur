use super::*;
use super::{inline_root, list_marker, positioned, pseudo};
use crate::units::F32Units;

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
        let mark = (clipping || opacity_scope).then(|| out.draw_mark());
        build_list_item_body(doc, node, style, visible, content_box, ctx, depth, out);
        record_li_clip_opacity_descendants(node_id, clipping, mark, out);
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
            let font_size_pt = styles.clone_font_size().used_size().px().as_px().in_pt();
            match styles.clone_line_height() {
                LineHeight::Normal => font_size_pt * DEFAULT_LINE_HEIGHT_RATIO,
                LineHeight::Number(num) => font_size_pt * num.0,
                LineHeight::Length(value) => value.0.px().as_px().in_pt(),
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
            let mark = (clipping || opacity_scope).then(|| out.draw_mark());
            build_list_item_body(doc, node, style, visible, content_box, ctx, depth, out);
            record_li_clip_opacity_descendants(node_id, clipping, mark, out);
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
            let fs = styles.clone_font_size().used_size().px().as_px().in_pt();
            let lh = {
                use ::style::values::computed::font::LineHeight;
                match styles.clone_line_height() {
                    LineHeight::Normal => fs * DEFAULT_LINE_HEIGHT_RATIO,
                    LineHeight::Number(num) => fs * num.0,
                    LineHeight::Length(value) => value.0.px().as_px().in_pt(),
                }
            };
            (fs, lh)
        } else {
            let fs = 12.0_f32.as_px().in_pt();
            (fs, fs * DEFAULT_LINE_HEIGHT_RATIO)
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
            // Capture a DrawMark before the child walk so
            // `inject_marker_into_first_paragraph` only considers
            // paragraphs registered for *this* list-item's subtree.
            // Without the mark, the lowest paragraph key would be the
            // lowest NodeId in the entire document (e.g. an earlier `<p>`
            // or a previous `<li>`), prepending the marker to unrelated
            // content. O(1) capture vs the old O(paragraphs_so_far)
            // BTreeSet snapshot (fulgur-un8f).
            let mark = out.draw_mark();
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
                // `inject_marker_into_first_paragraph` returns the item back on
                // failure (no target paragraph) so we can reuse it here without
                // cloning on the successful path.
                if let Err(item) = inject_marker_into_first_paragraph(out, mark, item) {
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
/// AFTER `mark` was captured.
///
/// Returns `Ok(())` on success (item consumed and prepended to the target
/// paragraph's first line). Returns `Err(item)` on failure (no paragraph
/// inserted since `mark`, or the target paragraph has no lines) — the
/// caller receives the unmodified item back and can synthesize a
/// marker-only paragraph without paying a clone on the successful path.
///
/// Uses [`crate::drawables::Drawables::min_paragraph_since`] — the lowest
/// `NodeId` inserted after `mark` — which matches v1's depth-first walk
/// for the inside-marker fallback path. Restricting to post-mark ids
/// keeps the marker scoped to the current list item's subtree (a sibling
/// list item or earlier `<p>` that registered before `mark` is
/// excluded). Byte-identical to the old find-first scan under the
/// append-only, one-insert-per-NodeId invariant on `TrackedMap` in
/// convert.
#[must_use = "returns Err(item) on failure; the handed-back item must be reused or explicitly dropped"]
fn inject_marker_into_first_paragraph(
    out: &mut crate::drawables::Drawables,
    mark: crate::drawables::DrawMark,
    item: LineItem,
) -> Result<(), LineItem> {
    let Some(target_id) = out.min_paragraph_since(mark) else {
        return Err(item);
    };
    let Some(entry) = out.paragraphs.get_mut(&target_id) else {
        return Err(item);
    };
    let Some(first_line) = entry.lines.first_mut() else {
        return Err(item);
    };
    let shift = match &item {
        LineItem::Text(run) => run
            .glyphs
            .iter()
            .map(|g| g.x_advance * run.font_size)
            .sum::<crate::units::Pt>(),
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
    Ok(())
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
                paragraph.cached_height = paragraph.lines.iter().map(|l| l.height.to_f32()).sum();
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
                height: crate::units::Pt::ZERO,
                baseline: crate::units::Pt::ZERO,
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
/// `BlockEntry` keyed by `node_id`, using the NodeIds inserted since `mark`.
/// `mark` is `Some` only when the caller decided this node has a clip or
/// opacity scope worth tracking. See [`crate::drawables::Drawables::drawn_since`].
fn record_li_clip_opacity_descendants(
    node_id: usize,
    clipping: bool,
    mark: Option<crate::drawables::DrawMark>,
    out: &mut crate::drawables::Drawables,
) {
    let Some(mark) = mark else { return };
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

#[cfg(test)]
mod tests {
    use super::{inject_marker_into_first_paragraph, record_li_clip_opacity_descendants};
    use crate::drawables::{BlockEntry, Drawables, ParagraphEntry};
    use crate::paragraph::{
        InlineBoxItem, InlineImage, LineItem, ShapedGlyph, ShapedGlyphRun, ShapedLine,
        TextDecoration, VerticalAlign,
    };
    use crate::units::F32Units;
    use std::sync::Arc;

    fn make_line(items: Vec<LineItem>) -> ShapedLine {
        ShapedLine {
            height: 12.0_f32.as_pt(),
            baseline: 9.0_f32.as_pt(),
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
            width: width.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: x_offset.as_pt(),
            computed_y: crate::units::Pt::ZERO,
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
        let mark = out.draw_mark();
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(10.0, 0.0)).is_err());
    }

    #[test]
    fn inject_returns_false_when_all_paragraphs_pre_existing() {
        let mut out = Drawables::new();
        // Insert the paragraph BEFORE capturing the mark so id 1 is pre-mark;
        // `min_paragraph_since(mark)` must then return None and injection fails.
        out.paragraphs.insert(1, make_para(vec![make_line(vec![])]));
        let mark = out.draw_mark();
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(10.0, 0.0)).is_err());
    }

    #[test]
    fn inject_returns_false_when_new_paragraph_has_no_lines() {
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs.insert(5, make_para(vec![])); // new (post-mark) but no lines
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(10.0, 0.0)).is_err());
    }

    #[test]
    fn inject_inline_box_marker_is_prepended_to_first_line() {
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 0.0)])]));
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(10.0, 0.0)).is_ok());
        let first_line = &out.paragraphs[&5].lines[0];
        assert_eq!(first_line.items.len(), 2);
        // Newly-inserted marker is at index 0.
        let LineItem::InlineBox(marker_ib) = &first_line.items[0] else {
            panic!("expected InlineBox at index 0");
        };
        assert_eq!(marker_ib.width.to_f32(), 10.0);
    }

    #[test]
    fn inject_shifts_existing_inline_box_by_marker_width() {
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        // Existing item at x_offset=5.
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 5.0)])]));
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(10.0, 0.0)).is_ok());
        // shift = marker width = 10 → existing item should now be at x_offset = 5 + 10 = 15.
        let LineItem::InlineBox(existing) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected InlineBox at index 1");
        };
        assert!(
            (existing.x_offset.to_f32() - 15.0).abs() < 0.001,
            "got {:?}",
            existing.x_offset
        );
    }

    #[test]
    fn inject_image_marker_shifts_existing_by_image_width() {
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 3.0)])]));
        let image_marker = LineItem::Image(InlineImage {
            data: Arc::new(vec![]),
            format: crate::image::ImageFormat::Png,
            width: 8.0_f32.as_pt(),
            height: 8.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: crate::units::Pt::ZERO,
            link: None,
        });
        assert!(inject_marker_into_first_paragraph(&mut out, mark, image_marker).is_ok());
        // shift = image width = 8 → existing at x_offset=3 → 3 + 8 = 11.
        let LineItem::InlineBox(ib) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected InlineBox at index 1");
        };
        assert!(
            (ib.x_offset.to_f32() - 11.0).abs() < 0.001,
            "got {:?}",
            ib.x_offset
        );
    }

    #[test]
    fn inject_text_marker_shifts_by_sum_of_advance_times_font_size() {
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 0.0)])]));
        // Two glyphs, each x_advance=0.5, font_size=12 → shift = 2 × 0.5 × 12 = 12.
        let text_marker = LineItem::Text(ShapedGlyphRun {
            font_data: Arc::new(vec![]),
            font_index: 0,
            font_size: 12.0_f32.as_pt(),
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
            text: Arc::from("• "),
            x_offset: crate::units::Pt::ZERO,
            link: None,
        });
        assert!(inject_marker_into_first_paragraph(&mut out, mark, text_marker).is_ok());
        let LineItem::InlineBox(ib) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected InlineBox at index 1");
        };
        assert!(
            (ib.x_offset.to_f32() - 12.0).abs() < 0.001,
            "got {:?}",
            ib.x_offset
        );
    }

    #[test]
    fn inject_picks_lowest_new_node_id_via_min_paragraph_since() {
        // `min_paragraph_since` returns the min of the since-mark tail regardless
        // of insertion order → node 5 is picked over node 10 even when inserted
        // second.
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs
            .insert(10, make_para(vec![make_line(vec![])]));
        out.paragraphs.insert(5, make_para(vec![make_line(vec![])]));
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(10.0, 0.0)).is_ok());
        // Marker went into node 5 (the lower key).
        assert_eq!(out.paragraphs[&5].lines[0].items.len(), 1);
        assert_eq!(out.paragraphs[&10].lines[0].items.len(), 0);
    }

    // ── record_li_clip_opacity_descendants ────────────────────────────

    #[test]
    fn record_none_mark_is_noop() {
        let mut out = Drawables::new();
        out.block_styles.insert(1, make_block_entry());
        record_li_clip_opacity_descendants(1, true, None, &mut out);
        assert!(out.block_styles[&1].clip_descendants.is_empty());
        assert!(out.block_styles[&1].opacity_descendants.is_empty());
    }

    #[test]
    fn record_clipping_true_fills_clip_descendants_and_excludes_self() {
        let mut out = Drawables::new();
        out.block_styles.insert(10, make_block_entry()); // parent (pre-mark)
        let mark = out.draw_mark();
        out.block_styles.insert(20, make_block_entry()); // child 1 (post-mark)
        out.block_styles.insert(30, make_block_entry()); // child 2 (post-mark)
        record_li_clip_opacity_descendants(10, true, Some(mark), &mut out);
        // drawn_since already yields ascending unique ids; no sort needed.
        assert_eq!(out.block_styles[&10].clip_descendants, vec![20usize, 30]);
        assert!(out.block_styles[&10].opacity_descendants.is_empty());
    }

    #[test]
    fn record_clipping_false_fills_opacity_descendants_and_excludes_self() {
        let mut out = Drawables::new();
        out.block_styles.insert(10, make_block_entry());
        let mark = out.draw_mark();
        out.block_styles.insert(11, make_block_entry());
        record_li_clip_opacity_descendants(10, false, Some(mark), &mut out);
        assert_eq!(out.block_styles[&10].opacity_descendants, vec![11usize]);
        assert!(out.block_styles[&10].clip_descendants.is_empty());
    }

    #[test]
    fn record_missing_block_entry_does_not_panic() {
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.block_styles.insert(20, make_block_entry());
        // node_id 10 has no entry in block_styles — must not panic.
        record_li_clip_opacity_descendants(10, true, Some(mark), &mut out);
    }

    #[test]
    fn record_excludes_node_id_even_when_inserted_after_mark() {
        // Both 10 and 11 are inserted after the mark, so `drawn_since` yields
        // {10, 11}. The `.filter(|&id| id != node_id)` guard must drop 10
        // (self) so clip_descendants contains only the child.
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.block_styles.insert(10, make_block_entry());
        out.block_styles.insert(11, make_block_entry());
        record_li_clip_opacity_descendants(10, true, Some(mark), &mut out);
        assert_eq!(out.block_styles[&10].clip_descendants, vec![11usize]);
        assert!(out.block_styles[&10].opacity_descendants.is_empty());
    }

    // ── inject shift loop: Text and Image existing-item arms ──────────

    fn make_text_run(x_offset: f32) -> LineItem {
        LineItem::Text(ShapedGlyphRun {
            font_data: Arc::new(vec![]),
            font_index: 0,
            font_size: 12.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![],
            text: Arc::from(""),
            x_offset: x_offset.as_pt(),
            link: None,
        })
    }

    fn make_image_item(width: f32, x_offset: f32) -> LineItem {
        LineItem::Image(InlineImage {
            data: Arc::new(vec![]),
            format: crate::image::ImageFormat::Png,
            width: width.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: x_offset.as_pt(),
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: crate::units::Pt::ZERO,
            link: None,
        })
    }

    #[test]
    fn inject_shifts_existing_text_run_by_marker_width() {
        // Existing item in the line is LineItem::Text. The shift loop's Text arm
        // (`run.x_offset += shift`) must update it when a marker is prepended.
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![make_text_run(5.0)])]));
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(10.0, 0.0)).is_ok());
        // marker InlineBox width = 10 → existing text run shifts from 5 to 15.
        let LineItem::Text(shifted) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected Text at index 1");
        };
        assert!(
            (shifted.x_offset.to_f32() - 15.0).abs() < 0.001,
            "got {:?}",
            shifted.x_offset
        );
    }

    #[test]
    fn inject_shifts_existing_inline_image_by_marker_width() {
        // Existing item in the line is LineItem::Image. The shift loop's Image arm
        // (`i.x_offset += shift`) must update it when a marker is prepended.
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs.insert(
            5,
            make_para(vec![make_line(vec![make_image_item(20.0, 3.0)])]),
        );
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(8.0, 0.0)).is_ok());
        // marker width = 8 → existing image shifts from 3 to 11.
        let LineItem::Image(shifted) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected Image at index 1");
        };
        assert!(
            (shifted.x_offset.to_f32() - 11.0).abs() < 0.001,
            "got {:?}",
            shifted.x_offset
        );
    }

    #[test]
    fn inject_shifts_all_three_item_types_in_first_line() {
        // First line has Text + Image + InlineBox. After injection all three must
        // be shifted by the marker's width; only the first line is affected.
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        let line0 = make_line(vec![
            make_text_run(1.0),
            make_image_item(15.0, 2.0),
            inline_box(5.0, 3.0),
        ]);
        let line1 = make_line(vec![inline_box(5.0, 0.0)]); // second line must not shift
        out.paragraphs.insert(5, make_para(vec![line0, line1]));
        assert!(inject_marker_into_first_paragraph(&mut out, mark, inline_box(10.0, 0.0)).is_ok());
        let items = &out.paragraphs[&5].lines[0].items;
        // Items at indices 1, 2, 3 are the original three (marker inserted at 0).
        let LineItem::Text(t) = &items[1] else {
            panic!("expected Text at 1");
        };
        assert!(
            (t.x_offset.to_f32() - 11.0).abs() < 0.001,
            "text: got {:?}",
            t.x_offset
        );
        let LineItem::Image(im) = &items[2] else {
            panic!("expected Image at 2");
        };
        assert!(
            (im.x_offset.to_f32() - 12.0).abs() < 0.001,
            "image: got {:?}",
            im.x_offset
        );
        let LineItem::InlineBox(ib) = &items[3] else {
            panic!("expected InlineBox at 3");
        };
        assert!(
            (ib.x_offset.to_f32() - 13.0).abs() < 0.001,
            "ib: got {:?}",
            ib.x_offset
        );
        // Second line must be unchanged.
        let LineItem::InlineBox(l1_ib) = &out.paragraphs[&5].lines[1].items[0] else {
            panic!("expected InlineBox in line 1");
        };
        assert!(
            (l1_ib.x_offset.to_f32() - 0.0).abs() < 0.001,
            "line 1 shifted unexpectedly: {:?}",
            l1_ib.x_offset
        );
    }

    #[test]
    fn inject_text_marker_zero_glyphs_inserts_without_shifting() {
        // A Text marker with no glyphs has advance sum = 0, so existing items
        // keep their x_offset while the marker is still prepended.
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 7.0)])]));
        let zero_glyph_marker = LineItem::Text(ShapedGlyphRun {
            font_data: Arc::new(vec![]),
            font_index: 0,
            font_size: 12.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![],
            text: Arc::from(""),
            x_offset: crate::units::Pt::ZERO,
            link: None,
        });
        assert!(inject_marker_into_first_paragraph(&mut out, mark, zero_glyph_marker).is_ok());
        // shift = 0 → existing InlineBox stays at x_offset=7.
        let LineItem::InlineBox(ib) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected InlineBox at index 1");
        };
        assert!(
            (ib.x_offset.to_f32() - 7.0).abs() < 0.001,
            "expected no shift, got {:?}",
            ib.x_offset
        );
        // Marker is still inserted at index 0.
        assert_eq!(out.paragraphs[&5].lines[0].items.len(), 2);
    }

    // ── inject shift calculation: Image and Text-with-glyphs marker arms ────

    #[test]
    fn inject_image_as_marker_uses_image_width_for_shift() {
        // When the MARKER item is LineItem::Image, shift = img.width (8 pt).
        // This exercises the `LineItem::Image(img) => img.width` arm in the
        // shift-calculation match — untested by the other inject tests which
        // all use InlineBox or zero-glyph Text as the marker.
        let mut out = Drawables::new();
        let mark = out.draw_mark();
        out.paragraphs
            .insert(5, make_para(vec![make_line(vec![inline_box(20.0, 5.0)])]));

        assert!(
            inject_marker_into_first_paragraph(&mut out, mark, make_image_item(8.0, 0.0)).is_ok()
        );

        // shift = image width 8 pt → existing InlineBox moves from 5 to 13.
        let LineItem::InlineBox(shifted) = &out.paragraphs[&5].lines[0].items[1] else {
            panic!("expected InlineBox at index 1");
        };
        assert!(
            (shifted.x_offset.to_f32() - 13.0).abs() < 0.001,
            "expected x_offset 13.0, got {:?}",
            shifted.x_offset
        );
        // Marker image is now at index 0.
        assert!(matches!(
            &out.paragraphs[&5].lines[0].items[0],
            LineItem::Image(_)
        ));
    }

    // ── smoke tests via Engine::render_html (Blitz-dependent paths) ──────────
    //
    // These exercises cover branches in `try_convert` and `build_list_item_body`
    // that cannot be reached without a live Blitz document.  They mirror the
    // pattern used in render.rs `#[cfg(test)]` smoke helpers.

    fn render_list_html(html: &str) -> Vec<u8> {
        crate::engine::Engine::builder()
            .build()
            .render(html)
            .expect("render failed")
    }

    const RED_1X1_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    // try_convert branch 1 (outside marker): opacity < 1.0 on the <li> activates
    // the `opacity_scope` snapshot path (line 62-63).
    #[test]
    fn smoke_outside_marker_with_opacity_scope() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul><li style="opacity:0.5">Faded item</li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 1 (outside marker): overflow:hidden on the <li>
    // activates the `clipping` snapshot path (line 61-63).
    #[test]
    fn smoke_outside_marker_with_overflow_clip() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul><li style="overflow:hidden;height:30px">
                <div style="height:200px">Clipped content</div>
            </li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 3 (inside marker, non-inline-root): a numeric
    // `line-height` multiplier → `LineHeight::Number` arm (line 138).
    #[test]
    fn smoke_inside_marker_line_height_number() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul style="list-style-position:inside;line-height:1.5">
                <li><p>Block child so non-inline-root</p></li>
            </ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 3 (inside marker, non-inline-root): an absolute-length
    // `line-height` → `LineHeight::Length` arm (line 139).
    #[test]
    fn smoke_inside_marker_line_height_length() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul style="list-style-position:inside;line-height:24px">
                <li><p>Block child so non-inline-root</p></li>
            </ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 3 (inside marker, non-inline-root): empty <li> with no
    // children forces the standalone marker-paragraph path (lines 172-203).
    #[test]
    fn smoke_inside_empty_li_with_marker() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul style="list-style-position:inside">
                <li></li>
                <li>Normal item</li>
            </ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker): <li> whose only child is a block
    // element.  The <li> is NOT an inline-root, so the non-inline-root walk
    // (lines 423-427) is exercised.
    #[test]
    fn smoke_outside_marker_with_block_children() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul><li><div style="background:#cef;height:20px">Block child</div></li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): a ::before pseudo whose
    // `content` is an image URL and display is NOT block-outside injects an
    // InlineImage *before* the paragraph text (lines 336-387).
    #[test]
    fn smoke_outside_marker_with_inline_before_image() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"li::before { content: url("dot.png"); width: 8px; height: 8px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul><li>Item with before image</li></ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): empty <li> with an
    // inline ::before image but no text.  `extract_paragraph` returns None so
    // the `before_inline.is_some()` branch at line 392 is taken.
    #[test]
    fn smoke_outside_marker_empty_li_with_inline_before_image() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"li::before { content: url("dot.png"); width: 8px; height: 8px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul><li></li></ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): empty <li> with no
    // text and no inline pseudo images.  Both `paragraph_opt` and
    // `before_inline`/`after_inline` are None, so the final `else` branch at
    // line 415 (fall-through to non-inline-root child walk) is exercised.
    #[test]
    fn smoke_outside_marker_empty_li_no_pseudos() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul><li></li><li>Next item</li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): a ::after pseudo whose
    // `content` is an image URL and display is NOT block-outside injects an
    // InlineImage *after* the paragraph text (lines 352-367).
    #[test]
    fn smoke_outside_marker_with_inline_after_image() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"li::after { content: url("dot.png"); width: 8px; height: 8px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul><li>Item with after image</li></ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    /// Decode NotoSans-Regular WOFF2 fixture into the TTF bytes that
    /// `AssetBundle::fonts` stores after `add_font_bytes`.
    fn load_noto_sans_ttf() -> Arc<Vec<u8>> {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/fonts/NotoSans-Regular.woff2");
        let woff2 = std::fs::read(&fixture).expect("NotoSans-Regular.woff2 missing");
        let mut bundle = crate::asset::AssetBundle::new();
        bundle.add_font_bytes(woff2).expect("WOFF2 decode failed");
        Arc::clone(&bundle.fonts[0])
    }

    // try_convert branch 3 (inside marker, non-inline-root, non-empty children):
    // when walking children produces no ParagraphEntry,
    // `inject_marker_into_first_paragraph` returns false and the marker is
    // synthesized as a standalone paragraph at the list-item level (lines 241-254).
    // Requires a bundled font so `find_marker_font` can locate the bullet glyph.
    #[test]
    fn smoke_inside_marker_block_child_no_paragraph() {
        let font_data = load_noto_sans_ttf();
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.fonts.push(font_data);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-position:inside">
                    <li><div style="height:20px;background:#eee"></div></li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 3 (inside marker, non-inline-root): empty <li> with a
    // bundled font that covers the bullet character. `find_marker_font` returns
    // Some this time, so `empty_li_marker_item = Some(LineItem::Text(...))`, and
    // the standalone marker-paragraph path (lines 177-204) is executed.
    // Without a bundled font (see `smoke_inside_empty_li_with_marker`),
    // `find_marker_font` returns None and the inner `if let Some(item)` body
    // is never entered — those lines stay at coverage count=0.
    #[test]
    fn smoke_inside_empty_li_with_bundled_font_triggers_marker_paragraph() {
        let font_data = load_noto_sans_ttf();
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.fonts.push(font_data);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-position:inside">
                    <li></li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 3 (inside marker, non-inline-root): empty <li> where the
    // marker is a list-style-image resolved as an InlineImage rather than a
    // Text run. `resolve_inside_image_marker` returns Some, so the first arm of
    // the `or_else` chain is taken and the image is used as the standalone
    // marker item. Exercises the `LineItem::Image` path via `.map(LineItem::Image)`.
    #[test]
    fn smoke_inside_empty_li_with_image_marker() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"ul { list-style-position: inside; list-style-image: url("dot.png"); }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul>
                    <li></li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 2 (fallback display:list-item): a <div> styled with
    // `display:list-item` and a bundled `list-style-image` hits the fallback
    // path (lines 76-118) that exists for non-<li> elements.
    // Default `line-height` → `LineHeight::Normal` arm (line 83).
    #[test]
    fn smoke_fallback_display_list_item_normal_line_height() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"div.li-item { display: list-item; list-style-image: url("dot.png"); }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <div class="li-item">Fallback list item</div>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 2 (fallback display:list-item): same as above but with
    // a numeric line-height multiplier → `LineHeight::Number` arm (line 84).
    #[test]
    fn smoke_fallback_display_list_item_number_line_height() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"div.li-item {
            display: list-item;
            list-style-image: url("dot.png");
            line-height: 1.5;
        }"#,
        );
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <div class="li-item">Fallback item numeric lh</div>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 2 (fallback display:list-item): absolute-length
    // line-height → `LineHeight::Length` arm (line 85).
    #[test]
    fn smoke_fallback_display_list_item_length_line_height() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"div.li-item {
            display: list-item;
            list-style-image: url("dot.png");
            line-height: 24px;
        }"#,
        );
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <div class="li-item">Fallback item length lh</div>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 2 (fallback display:list-item): a <div> styled with
    // `display:list-item` but WITHOUT `list-style-image`.  `resolve_list_marker`
    // returns `None` (no image URL in the CSS / no matching asset), so the outer
    // `if let Some(marker)` arm is not taken and the function exits the branch-2
    // block without registering a `ListItemEntry` or returning `true`.  The element
    // then falls through all three branches and `try_convert` returns `false`,
    // leaving it to the normal block-convert path.  Previously untested: the
    // branch-2 fall-through path where `resolve_list_marker` returns `None`.
    #[test]
    fn smoke_fallback_display_list_item_without_image_falls_through() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <div style="display:list-item">Fallback without image marker</div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // Smoke: a <div> with `display:list-item` AND an explicit numeric
    // `line-height` renders without crashing.  Empirically Blitz assigns
    // `list_item_data` with an outside marker layout for this combination, so
    // the outside-marker path (branch 1) fires rather than the fallback
    // (branch 2) — but the render path is still exercised end-to-end.
    #[test]
    fn smoke_display_list_item_div_with_numeric_line_height() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <div style="display:list-item;line-height:1.5">Numeric line-height</div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // Smoke: same as above but with an absolute-length `line-height`.
    #[test]
    fn smoke_display_list_item_div_with_length_line_height() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <div style="display:list-item;line-height:24px">Absolute line-height</div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // Smoke: a <div> with `display:list-item` and `list-style-image:url(dot.png)`
    // renders without crashing.  Blitz assigns `list_item_data` with an outside
    // marker layout for this combination (branch 1), so the outside-marker path
    // with an image marker is exercised.
    #[test]
    fn smoke_display_list_item_div_with_inline_image_marker() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <div style="display:list-item;list-style-image:url(dot.png)">Image marker</div>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 3 (inside marker, non-inline-root, non-empty children):
    // when `list-style-image` is set AND `list-style-position: inside`, and the
    // `<li>` has a block child (making it non-inline-root), the non-empty-children
    // path at line ~226 computes `marker_item` via
    // `resolve_inside_image_marker(...).map(LineItem::Image)`.  When successful,
    // `inject_marker_into_first_paragraph` is called with a `LineItem::Image`,
    // exercising the `LineItem::Image(img) => img.width` arm of the shift
    // calculation — distinct from the `empty_li_marker_item` path which registers
    // a standalone paragraph without calling `inject_marker_into_first_paragraph`.
    #[test]
    fn smoke_inside_nonempty_li_with_image_marker_injects_image() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"ul { list-style-position: inside; list-style-image: url("dot.png"); }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul>
                    <li><p>Block child makes li non-inline-root</p></li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): both a ::before AND a
    // ::after inline image are present simultaneously.  `pseudo::inject_inline_pseudo_images`
    // is called with both `before_inline = Some(...)` and `after_inline = Some(...)`.
    // The existing tests only have one pseudo at a time; this one exercises the
    // path where `before_inline.is_some() && after_inline.is_some()` is true inside
    // the `if let Some(mut paragraph) = paragraph_opt` arm.
    #[test]
    fn smoke_outside_marker_with_both_before_and_after_inline_images() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"li::before { content: url("dot.png"); width: 8px; height: 8px; }
               li::after  { content: url("dot.png"); width: 8px; height: 8px; }"#,
        );
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul><li>Item with both before and after images</li></ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): empty <li> with a
    // ::before pseudo whose `content` is an image URL, but NO assets are
    // provided so `build_inline_pseudo_image` returns None. The <li> is still
    // an inline root (the inline pseudo-element creates an IFC), but both
    // `paragraph_opt` and `before_inline` are None — exercising the final
    // `else` branch at lines 429-436 (fall-through to non-inline-root walk).
    #[test]
    fn smoke_outside_marker_inline_before_unresolved_url_no_assets() {
        let pdf = render_list_html(
            r#"<!doctype html><html><head>
            <style>li::before { content: url("dot.png"); width: 8px; height: 8px; }</style>
            </head><body>
            <ul><li></li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): same as above but via
    // ::after. Verifies that the else-branch at lines 429-436 is also reached
    // when `after_inline` (not `before_inline`) is the unresolved pseudo.
    #[test]
    fn smoke_outside_marker_inline_after_unresolved_url_no_assets() {
        let pdf = render_list_html(
            r#"<!doctype html><html><head>
            <style>li::after { content: url("dot.png"); width: 8px; height: 8px; }</style>
            </head><body>
            <ul><li></li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): empty <li> with both
    // ::before AND ::after inline-image pseudos whose URLs are unresolved.
    // `before_inline` and `after_inline` are both None, exercising the
    // lines 429-436 path with two simultaneous unresolved pseudo-images.
    #[test]
    fn smoke_outside_marker_both_pseudos_unresolved_url_no_assets() {
        let pdf = render_list_html(
            r#"<!doctype html><html><head>
            <style>
            li::before { content: url("before.png"); width: 6px; height: 6px; }
            li::after  { content: url("after.png");  width: 6px; height: 6px; }
            </style>
            </head><body>
            <ul><li></li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): the <li> has an
    // <a href="..."> ancestor so `attach_link_to_inline_image` (pseudo.rs)
    // finds the enclosing anchor and sets `img.link = Some(...)`.
    // Covers the `if let Some((_, span)) = resolve_enclosing_anchor(...)` body
    // in pseudo::attach_link_to_inline_image.
    #[test]
    fn smoke_outside_marker_before_image_with_anchor_ancestor() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"li::before { content: url("dot.png"); width: 8px; height: 8px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <a href="https://example.com"><ul><li>Linked item with before image</li></ul></a>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // Same as above but with a ::after inline image.  Both `before_inline`
    // and `after_inline` go through `attach_link_to_inline_image` on the
    // (outside marker, inline-root) path; verifying the after slot also
    // correctly picks up the anchor ancestor.
    #[test]
    fn smoke_outside_marker_after_image_with_anchor_ancestor() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"li::after { content: url("dot.png"); width: 8px; height: 8px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <a href="https://example.com"><ul><li>Linked item with after image</li></ul></a>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // build_list_item_body (outside marker, inline-root): ::before image with
    // an internal fragment anchor (`#target`).  The same
    // `attach_link_to_inline_image` → `resolve_enclosing_anchor` path runs,
    // this time producing a `LinkTarget::Internal` span.
    #[test]
    fn smoke_outside_marker_before_image_with_internal_anchor_ancestor() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"li::before { content: url("dot.png"); width: 8px; height: 8px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r##"<!doctype html><html><body>
                <a href="#section"><ul><li>Internal link item</li></ul></a>
                <h2 id="section">Section</h2>
                </body></html>"##,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 2 (fallback display:list-item, line_height Number arm):
    // Blitz sets `list_item_data = None` when `list-style-type: none`, which
    // routes the element through branch 2 instead of branch 1.  Combined with
    // a numeric `line-height`, the `LineHeight::Number` arm (line 85) is taken.
    // `resolve_list_marker` returns Some (list-style-image in bundle) so the
    // success path (lines 91-118) is also executed.
    #[test]
    fn smoke_branch2_list_style_none_with_image_number_line_height() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul>
                    <li style="list-style-type:none;list-style-image:url('dot.png');line-height:1.5">
                        Item without text marker, numeric line-height
                    </li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 2 (fallback display:list-item, line_height Length arm):
    // Same as above but with an absolute-length `line-height` so the
    // `LineHeight::Length` arm (line 86) is taken.
    #[test]
    fn smoke_branch2_list_style_none_with_image_length_line_height() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul>
                    <li style="list-style-type:none;list-style-image:url('dot.png');line-height:24px">
                        Item without text marker, absolute line-height
                    </li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 3 (inside marker, non-inline-root): apply
    // `list-style-position: inside` directly on the <li> element rather than
    // inheriting it from the <ul>.  Exercises the same code paths as the
    // inherited variants but gives the style resolution a direct target.
    #[test]
    fn smoke_inside_marker_style_on_li_directly_with_block_child() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul><li style="list-style-position: inside">
                <p>Block child makes li non-inline-root</p>
            </li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert branch 3 (inside marker, non-inline-root): `list-style-type:
    // none` suppresses the marker entirely, so `marker_item = None` and the
    // block_styles.insert at the end of the non-empty-children path is reached
    // without the marker injection branch being taken.
    #[test]
    fn smoke_inside_marker_list_style_none_with_block_child() {
        let pdf = render_list_html(
            r#"<!doctype html><html><body>
            <ul style="list-style-position: inside; list-style-type: none">
                <li><p>Block child, no marker</p></li>
            </ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }
}
