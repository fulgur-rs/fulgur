use super::*;
use super::{list_marker, positioned, pseudo};
use crate::pageable::SpacerPageable;
use crate::paragraph::{InlineBoxItem, ParagraphPageable};

/// Dispatcher entry for inline-root nodes (those with `node.flags.is_inline_root()`).
///
/// Builds a `ParagraphEntry` and inserts it into `out.paragraphs`. When the
/// node has visual style or pseudo content, also inserts a `BlockEntry` so
/// the dispatcher paints background / border / opacity around the paragraph.
///
/// Returns `true` when at least one entry was registered for this node.
/// Returns `false` to fall through (when the node is not an inline root,
/// or when an inline root has no text and no inline pseudo images).
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
    if !node.flags.is_inline_root() {
        return false;
    }
    let (width, height) = size_in_pt(node.final_layout.size);

    let paragraph_opt = extract_paragraph(doc, node, ctx, depth, out);
    let style = extract_block_style(node, ctx.assets);
    let (opacity, visible) = extract_opacity_visible(node);
    let content_box = compute_content_box(node, &style);

    // Inline pseudo images.
    let before_inline = node
        .before
        .and_then(|id| doc.get_node(id))
        .filter(|p| !pseudo::is_block_pseudo(p))
        .and_then(|p| {
            pseudo::build_inline_pseudo_image(p, content_box.width, content_box.height, ctx.assets)
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
            pseudo::build_inline_pseudo_image(p, content_box.width, content_box.height, ctx.assets)
        })
        .map(|mut img| {
            pseudo::attach_link_to_inline_image(&mut img, doc, node.id);
            img
        });

    if let Some(mut paragraph) = paragraph_opt {
        // Inject pseudo images BEFORE the list marker so the marker stays
        // at index 0 of the first line after both injections.
        if before_inline.is_some() || after_inline.is_some() {
            pseudo::inject_inline_pseudo_images(&mut paragraph.lines, before_inline, after_inline);
            recalculate_paragraph_line_boxes(&mut paragraph.lines);
            paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
        }

        // Inside list-style-image marker injection.
        if !paragraph.lines.is_empty() {
            let first_line_height = paragraph.lines[0].height;
            if let Some(inline_img) =
                list_marker::resolve_inside_image_marker(node, first_line_height, ctx.assets)
            {
                let shift = inline_img.width;
                for item in &mut paragraph.lines[0].items {
                    match item {
                        LineItem::Text(run) => run.x_offset += shift,
                        LineItem::Image(i) => i.x_offset += shift,
                        LineItem::InlineBox(ib) => ib.x_offset += shift,
                    }
                }
                paragraph.lines[0]
                    .items
                    .insert(0, LineItem::Image(inline_img));
                recalculate_paragraph_line_boxes(&mut paragraph.lines);
                paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
            }
        }

        // Block / abs pseudos around the paragraph.
        let block_pseudo_present = pseudo::node_has_block_pseudo_image(doc, node)
            || pseudo::node_has_absolute_pseudo(doc, node);
        let needs_block = style.needs_block_wrapper() || block_pseudo_present;

        // Always insert the paragraph entry keyed by the inline-root id.
        out.paragraphs.insert(
            node_id,
            crate::drawables::ParagraphEntry {
                lines: paragraph.lines,
                opacity: if needs_block { 1.0 } else { opacity },
                visible,
                id: extract_block_id(node),
            },
        );
        if needs_block {
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
            // Register pseudo content (block-pseudo images + abs children).
            pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);
        }
        return true;
    } else if before_inline.is_some() || after_inline.is_some() {
        // Synthesize a minimal paragraph for pseudo-only elements.
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
        let font_metrics = metrics_from_line(&line);
        crate::paragraph::recalculate_line_box(&mut line, &font_metrics);
        let lines = vec![line];

        let block_pseudo_present = pseudo::node_has_block_pseudo_image(doc, node)
            || pseudo::node_has_absolute_pseudo(doc, node);
        let needs_block = style.needs_block_wrapper() || block_pseudo_present;

        out.paragraphs.insert(
            node_id,
            crate::drawables::ParagraphEntry {
                lines,
                opacity: if needs_block { 1.0 } else { opacity },
                visible,
                id: extract_block_id(node),
            },
        );
        if needs_block {
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
            pseudo::register_pseudo_content(doc, node, ctx, depth, content_box, out);
        }
        return true;
    }

    // Inline root with no text and no inline pseudo images — fall through.
    false
}

/// Extract `LineFontMetrics` from a `ShapedLine`'s Text items using skrifa.
pub(super) fn metrics_from_line(line: &ShapedLine) -> LineFontMetrics {
    let default = LineFontMetrics {
        ascent: 12.0,
        descent: 4.0,
        x_height: 8.0,
        subscript_offset: 4.0,
        superscript_offset: 6.0,
    };
    for item in &line.items {
        let run = match item {
            LineItem::Text(r) => r,
            LineItem::Image(_) => continue,
            LineItem::InlineBox(_) => continue,
        };
        if let Ok(font_ref) = skrifa::FontRef::from_index(&run.font_data, run.font_index) {
            let metrics = font_ref.metrics(
                skrifa::instance::Size::new(run.font_size),
                skrifa::instance::LocationRef::default(),
            );
            return LineFontMetrics {
                ascent: metrics.ascent,
                descent: metrics.descent.abs(),
                x_height: metrics.x_height.unwrap_or(metrics.ascent * 0.5),
                subscript_offset: metrics.ascent * 0.3,
                superscript_offset: metrics.ascent * 0.4,
            };
        }
    }
    default
}

/// Recalculate line boxes for all lines in a paragraph.
pub(super) fn recalculate_paragraph_line_boxes(lines: &mut [ShapedLine]) {
    let mut original_y_acc: f32 = 0.0;
    let mut new_y_acc: f32 = 0.0;
    for line in lines.iter_mut() {
        let original_height = line.height;
        let font_metrics = metrics_from_line(line);
        line.baseline -= original_y_acc;
        crate::paragraph::recalculate_line_box(line, &font_metrics);
        for item in &mut line.items {
            if let LineItem::Image(img) = item {
                img.computed_y += new_y_acc;
            }
        }
        line.baseline += new_y_acc;
        original_y_acc += original_height;
        new_y_acc += line.height;
    }
}

/// Walk up from `start_id` to find the closest `<a href>` ancestor and
/// build a `LinkSpan`.
pub(super) fn resolve_enclosing_anchor(
    doc: &BaseDocument,
    start_id: usize,
) -> Option<(usize, LinkSpan)> {
    let mut cur = Some(start_id);
    let mut depth: usize = 0;
    while let Some(id) = cur {
        if depth >= MAX_DOM_DEPTH {
            return None;
        }
        let node = doc.get_node(id)?;
        if let NodeData::Element(el) = &node.data {
            if el.name.local.as_ref() == "a" {
                let href = crate::blitz_adapter::get_attr(el, "href")?.trim();
                if href.is_empty() {
                    return None;
                }
                let target = if let Some(frag) = href.strip_prefix('#') {
                    LinkTarget::Internal(Arc::new(frag.to_string()))
                } else {
                    LinkTarget::External(Arc::new(href.to_string()))
                };
                let alt = crate::blitz_adapter::element_text(doc, id);
                let alt_text = if alt.is_empty() { None } else { Some(alt) };
                return Some((id, LinkSpan { target, alt_text }));
            }
        }
        cur = node.parent;
        depth += 1;
    }
    None
}

/// Recursively convert the Blitz node referenced by a Parley `InlineBox.id`.
///
/// Returns a placeholder `Pageable` content (a zero-height `SpacerPageable`)
/// for the `LineItem::InlineBox.content` slot — that field still has the v1
/// `Box<dyn Pageable>` shape, but PR 8g routes inline-box rendering through
/// the v2 dispatcher (`dispatch_inline_box_content` keyed by content node
/// id) so the placeholder content is never actually drawn through the v1
/// `Pageable::draw` path. The side-effect call to `convert_node` registers
/// the inline-box subtree into `out` so the v2 dispatcher can find it.
fn convert_inline_box_node(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) -> crate::paragraph::InlineBoxContent {
    // Suppress the rendering path for absolutely-positioned pseudos that
    // Blitz routes through Parley's inline layout — they are re-emitted by
    // `walk_absolute_pseudo_children` at the CSS-correct position. Letting
    // them register here would double-paint via the inline-box dispatch.
    if let Some(node) = doc.get_node(node_id) {
        if positioned::is_absolutely_positioned(node) && is_pseudo_node(doc, node) {
            return Box::new(SpacerPageable::new(0.0));
        }
    }
    convert_node(doc, node_id, ctx, depth + 1, out);
    Box::new(SpacerPageable::new(0.0))
}

/// Extract a `ParagraphPageable` from an inline root node. The caller
/// (`try_convert` above, or `list_item::build_list_item_body`) consumes
/// the returned paragraph and inserts a `ParagraphEntry` into `out`. We
/// keep returning `Option<ParagraphPageable>` instead of writing into `out`
/// here so callers can inject pseudo images / list markers BEFORE
/// committing the entry — the pre-PR-8i interface in that respect.
///
/// The `out` parameter still flows through because inline-box recursion
/// registers its subtree directly into `out` via `convert_node`. After the
/// recursion completes we record `inline_box_subtree_skip` /
/// `inline_box_subtree_descendants` so the v2 dispatcher knows to defer
/// dispatch to the paragraph render path.
pub(super) fn extract_paragraph(
    doc: &BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) -> Option<ParagraphPageable> {
    let elem_data = node.element_data()?;
    let text_layout = elem_data.inline_layout_data.as_ref()?;

    let parley_layout = &text_layout.layout;
    let text = &text_layout.text;

    let mut shaped_lines = Vec::new();
    let mut accumulated_line_top: f32 = 0.0;

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        let mut items = Vec::new();

        for item in line.items() {
            match item {
                parley::PositionedLayoutItem::GlyphRun(glyph_run) => {
                    let run = glyph_run.run();
                    let font_ref = run.font();
                    let font_index = font_ref.index;
                    let font_arc = ctx.get_or_insert_font(font_ref);
                    let font_size_parley = run.font_size();
                    let font_size = px_to_pt(font_size_parley);

                    let brush = &glyph_run.style().brush;
                    let color = get_text_color(doc, brush.id);
                    let decoration = get_text_decoration(doc, brush.id);
                    let link = ctx.link_cache.lookup(doc, brush.id);

                    let text_len = text.len();
                    let mut glyphs = Vec::new();
                    for g in glyph_run.glyphs() {
                        glyphs.push(ShapedGlyph {
                            id: g.id,
                            x_advance: g.advance / font_size_parley,
                            x_offset: g.x / font_size_parley,
                            y_offset: g.y / font_size_parley,
                            text_range: 0..text_len,
                        });
                    }

                    if !glyphs.is_empty() {
                        let run_text = text.clone();
                        let run_x_offset = px_to_pt(glyph_run.offset());
                        items.push(LineItem::Text(ShapedGlyphRun {
                            font_data: font_arc,
                            font_index,
                            font_size,
                            color,
                            decoration,
                            glyphs,
                            text: run_text,
                            x_offset: run_x_offset,
                            link,
                        }));
                    }
                }
                parley::PositionedLayoutItem::InlineBox(positioned) => {
                    let node_id = positioned.id as usize;
                    if let Some(box_node) = doc.get_node(node_id) {
                        if positioned::is_absolutely_positioned(box_node)
                            && is_pseudo_node(doc, box_node)
                        {
                            continue;
                        }
                    }
                    // Snapshot before recursing so we can compute the
                    // inline-box descendant set for the v2 dispatcher's
                    // skip table.
                    let before = collect_drawables_node_ids(out);
                    let content = convert_inline_box_node(doc, node_id, ctx, depth, out);
                    let after = collect_drawables_node_ids(out);
                    // Record the descendants the paragraph render path
                    // owns under its offset transform. Filter against
                    // already-recorded skip entries so nested inline-boxes
                    // don't double-register.
                    let descendants: Vec<crate::drawables::NodeId> = after
                        .difference(&before)
                        .copied()
                        .filter(|id| *id != node_id)
                        .filter(|id| !out.inline_box_subtree_skip.contains(id))
                        .collect();
                    out.inline_box_subtree_skip.insert(node_id);
                    out.inline_box_subtree_skip
                        .extend(descendants.iter().copied());
                    out.inline_box_subtree_descendants
                        .insert(node_id, descendants);

                    let link = ctx.link_cache.lookup(doc, node_id);
                    let height_pt = px_to_pt(positioned.height);
                    let baseline_shift =
                        crate::paragraph::inline_box_baseline_offset(content.as_ref())
                            .map(|bo| height_pt - bo)
                            .unwrap_or(0.0);
                    let computed_y = px_to_pt(positioned.y) - accumulated_line_top + baseline_shift;
                    let visible = doc
                        .get_node(node_id)
                        .map(super::style::extract_opacity_visible)
                        .map(|(_, v)| v)
                        .unwrap_or(true);
                    items.push(LineItem::InlineBox(InlineBoxItem {
                        content,
                        width: px_to_pt(positioned.width),
                        height: height_pt,
                        x_offset: px_to_pt(positioned.x),
                        computed_y,
                        link,
                        opacity: 1.0,
                        visible,
                    }));
                }
            }
        }

        let line_height_pt = px_to_pt(metrics.line_height);
        shaped_lines.push(ShapedLine {
            height: line_height_pt,
            baseline: px_to_pt(metrics.baseline),
            items,
        });
        accumulated_line_top += line_height_pt;
    }

    if shaped_lines.is_empty() {
        return None;
    }

    Some(ParagraphPageable::new(shaped_lines).with_id(extract_block_id(node)))
}
