use super::*;
use super::{list_marker, positioned, pseudo};
use crate::paragraph::{InlineBoxItem, ParagraphRender};
use crate::units::F32Units;
use std::sync::Arc;

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

    // Snapshot must be taken BEFORE `extract_paragraph` because the latter
    // recurses into inline-box children (registering their drawable
    // entries into `out`); placing the snapshot after would miss the
    // non-inline-box nodes (abs-positioned / pseudo children registered by
    // `register_pseudo_content`) that belong in the
    // `clip_descendants`/`opacity_descendants` diff.
    //
    // The `id != node_id` filter drops the inline-root's own id from the
    // descendant list. The diff still *contains* inline-box subtree
    // members, because this is a plain before/after set difference — but
    // those must NOT be painted from the descendant list: they are owned
    // by paragraph-time `LineItem::InlineBox` dispatch. `render.rs`
    // filters them out against `inline_box_subtree_skip` in all three
    // descendant walks (the top-level dispatch loop, `draw_under_clip`,
    // and `draw_under_opacity`). Dropping that filter double-paints the
    // subtree, and the duplicate compounds exponentially with nesting.
    let style = extract_block_style(node, ctx.assets);
    let (opacity, visible) = extract_opacity_visible(node);
    let needs_block_pre = style.needs_block_wrapper()
        || pseudo::node_has_block_pseudo_image(doc, node)
        || pseudo::node_has_absolute_pseudo(doc, node);
    let clipping_pre = needs_block_pre && style.has_overflow_clip();
    let opacity_scope_pre = needs_block_pre && !clipping_pre && opacity < 1.0;
    let pre_mark = (clipping_pre || opacity_scope_pre).then(|| out.draw_mark());

    let paragraph_opt = extract_paragraph(doc, node, ctx, depth, out);
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
            paragraph.cached_height = paragraph.lines.iter().map(|l| l.height.to_f32()).sum();
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
                paragraph.cached_height = paragraph.lines.iter().map(|l| l.height.to_f32()).sum();
            }
        }

        // Block / abs pseudo wrapping decision (mirrors `needs_block_pre`
        // computed up top so the snapshot side matches).
        let needs_block = needs_block_pre;
        let clipping = clipping_pre;
        let _opacity_scope = opacity_scope_pre;

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
            if let Some(mark) = pre_mark {
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
        return true;
    } else if before_inline.is_some() || after_inline.is_some() {
        // Synthesize a minimal paragraph for pseudo-only elements.
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
        let font_metrics = metrics_from_line(&line);
        crate::paragraph::recalculate_line_box(&mut line, &font_metrics);
        let lines = vec![line];

        let needs_block = needs_block_pre;
        let clipping = clipping_pre;
        let _opacity_scope = opacity_scope_pre;

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
            if let Some(mark) = pre_mark {
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
                skrifa::instance::Size::new(run.font_size.to_f32()),
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
    let mut original_y_acc = crate::units::Pt::ZERO;
    let mut new_y_acc = crate::units::Pt::ZERO;
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

/// CSS 2.1 §10.8.1: return the offset from an inline-block's top edge to
/// the baseline used for `vertical-align: baseline` (the baseline of the
/// *last* line box inside). Returns `None` when no in-flow baseline is
/// available, in which case the caller falls back to the bottom margin
/// edge (zero `baseline_shift`).
///
/// Drawables-aware baseline lookup. Inline-box content is represented by an
/// `InlineBoxPlaceholder` carrying only `node_id`, so there is
/// no trait tree to walk. Read the baseline from `out.paragraphs[node_id]`
/// (the inline-root case) or recurse into the node's Taffy children
/// (flex / grid / ordinary block) to find the last in-flow descendant that
/// contributes a baseline.
///
/// Returns `None` when:
/// - the inline-block has `overflow: clip|hidden|scroll|auto` (the spec
///   fallback),
/// - no descendant contributes a CSS line baseline (a leaf `<img>` /
///   `<svg>` / `<canvas>` inline-box).
pub(super) fn inline_box_baseline_offset_from_drawables(
    doc: &BaseDocument,
    out: &crate::drawables::Drawables,
    node_id: usize,
) -> Option<crate::units::Pt> {
    if let Some(block) = out.block_styles.get(&node_id)
        && block.style.has_overflow_clip()
    {
        return None;
    }
    pageable_last_baseline_from_drawables(doc, out, node_id, 0)
}

/// Recursive worker for `inline_box_baseline_offset_from_drawables`.
/// Walks the block's child list in REVERSE, deriving children from
/// `node.layout_children` / `node.children` (Taffy DOM). `top_inset` of
/// each container adds its own `border-top + padding-top`; child layout
/// `location.y` adds the child's offset within the container; the
/// recursive call returns the inner baseline relative to the child's top
/// edge.
fn pageable_last_baseline_from_drawables(
    doc: &BaseDocument,
    out: &crate::drawables::Drawables,
    node_id: usize,
    depth: usize,
) -> Option<crate::units::Pt> {
    if depth >= MAX_DOM_DEPTH {
        return None;
    }
    // 1) If this node has a paragraph entry (inline-root), use the last
    //    line's baseline + the node's top_inset (border + padding).
    if let Some(para) = out.paragraphs.get(&node_id) {
        let top_inset = out
            .block_styles
            .get(&node_id)
            .map(|b| b.style.border_widths[0] + b.style.padding[0])
            .unwrap_or(crate::units::Pt::ZERO);
        if let Some(line) = para.lines.last() {
            return Some(top_inset + line.baseline);
        }
    }
    // 2) Otherwise walk DOM children in REVERSE. Use Blitz's
    //    `layout_children` when available so anonymous block wrappers
    //    around inline-level siblings are visited correctly.
    let node = doc.get_node(node_id)?;
    let layout_children_borrow = node.layout_children.borrow();
    // An explicit `Some([])` from Blitz means "no in-flow children" and is
    // authoritative — fall back to `node.children` only when Blitz has not
    // populated `layout_children` at all. Otherwise an inline-block whose
    // only descendants are absolutely-positioned would walk those out-of-flow
    // nodes here and report a bogus baseline.
    let walk_children: &[usize] = layout_children_borrow.as_deref().unwrap_or(&node.children);
    for &child_id in walk_children.iter().rev() {
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
        if let Some(inner) = pageable_last_baseline_from_drawables(doc, out, child_id, depth + 1) {
            // Child y inside this container, in PDF pt. The child
            // recursively returns its inner baseline relative to its
            // own top edge; the container's own `top_inset` is folded
            // in by branch (1) above.
            return Some(child.final_layout.location.y.as_px().in_pt() + inner);
        }
    }
    None
}

/// Recursively convert the Blitz node referenced by a Parley `InlineBox.id`.
///
/// Returns `Some(node_id)` for normal inline boxes so that
/// `paragraph::draw_shaped_lines` can look up the content's geometry /
/// drawables entry and dispatch it through
/// `render::dispatch_inline_box_content`. Returns `None` for
/// absolutely-positioned pseudos — those are re-emitted by
/// `walk_absolute_pseudo_children` at the CSS-correct position and must
/// not be dispatched via the inline-box path.
///
/// The side-effect call to `convert_node` registers the inline-box subtree
/// into `out` so the v2 dispatcher can find it.
fn convert_inline_box_node(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) -> Option<usize> {
    // Suppress the rendering path for absolutely-positioned pseudos that
    // Blitz routes through Parley's inline layout — they are re-emitted by
    // `walk_absolute_pseudo_children` at the CSS-correct position. Letting
    // them register here would double-paint via the inline-box dispatch.
    // Returning `None` causes `paragraph::draw_shaped_lines` to skip the
    // inline-box dispatch for this item.
    if let Some(node) = doc.get_node(node_id) {
        if positioned::is_absolutely_positioned(node) && is_pseudo_node(doc, node) {
            return None;
        }
    }
    convert_node(doc, node_id, ctx, depth + 1, out);
    Some(node_id)
}

/// Extract a `ParagraphRender` from an inline root node. The caller
/// (`try_convert` above, or `list_item::build_list_item_body`) consumes
/// the returned paragraph and inserts a `ParagraphEntry` into `out`. We
/// keep returning `Option<ParagraphRender>` instead of writing into `out`
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
) -> Option<ParagraphRender> {
    let elem_data = node.element_data()?;
    let text_layout = elem_data.inline_layout_data.as_ref()?;

    let parley_layout = &text_layout.layout;
    // Materialize the paragraph text as `Arc<str>` once so each `ShapedGlyphRun`
    // stores a cheap Arc bump instead of a full String clone. All runs of a
    // paragraph share the same buffer; Blitz owns the original `String` and
    // drops it with the DOM, so we can't borrow.
    let text: Arc<str> = Arc::from(text_layout.text.as_str());

    let mut shaped_lines = Vec::new();
    let mut accumulated_line_top = crate::units::Pt::ZERO;

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        let mut items = Vec::new();
        // Track cumulative glyph offset within a Run across consecutive GlyphRuns
        // that share the same parent Run. Reset when the Run changes.
        let mut prev_run_key = usize::MAX;
        let mut run_glyph_offset = 0usize;

        for item in line.items() {
            match item {
                parley::PositionedLayoutItem::GlyphRun(glyph_run) => {
                    let run = glyph_run.run();
                    let font_ref = run.font();
                    let font_index = font_ref.index;
                    let font_arc = ctx.get_or_insert_font(font_ref);
                    let font_size_parley = run.font_size();
                    let font_size = font_size_parley.as_px().in_pt();

                    let brush = &glyph_run.style().brush;
                    let color = get_text_color(doc, brush.id);
                    let decoration = get_text_decoration(doc, brush.id);
                    let link = ctx.link_cache.lookup(doc, brush.id);

                    // Advance or reset the per-Run offset counter.
                    let run_key = run.cluster_range().start;
                    if run_key != prev_run_key {
                        prev_run_key = run_key;
                        run_glyph_offset = 0;
                    }
                    let glyph_start = run_glyph_offset;

                    // Build (text_range, Glyph) pairs scoped to this GlyphRun.
                    // `glyph_run.glyphs()` = run.visual_clusters().flat_map(.glyphs())
                    //   .skip(glyph_start).take(glyph_count).
                    // We replicate the same window on the annotated cluster sequence
                    // and advance the offset counter by the number of glyphs consumed.
                    let mut annotated = run
                        .visual_clusters()
                        .flat_map(|cluster| {
                            let r = cluster.text_range();
                            cluster.glyphs().map(move |g| (r.clone(), g))
                        })
                        .skip(glyph_start);

                    let mut glyphs = Vec::new();
                    for g in glyph_run.glyphs() {
                        let (text_range, _) = annotated.next().unwrap_or_else(|| {
                            panic!(
                                "annotated cluster iterator exhausted before glyph_run.glyphs(); \
                                 run cluster_range={:?}, glyph_start={glyph_start}",
                                run.cluster_range()
                            )
                        });
                        run_glyph_offset += 1;
                        glyphs.push(ShapedGlyph {
                            id: g.id,
                            x_advance: ShapedGlyph::normalize_by_font_size(
                                g.advance,
                                font_size_parley,
                            ),
                            x_offset: ShapedGlyph::normalize_by_font_size(g.x, font_size_parley),
                            y_offset: ShapedGlyph::normalize_by_font_size(g.y, font_size_parley),
                            text_range,
                        });
                    }

                    if !glyphs.is_empty() {
                        let run_text = Arc::clone(&text);
                        let run_x_offset = glyph_run.offset().as_px().in_pt();
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
                    // Mark before recursing so we can compute the inline-box
                    // descendant set for the v2 dispatcher's skip table.
                    let mark = out.draw_mark();
                    let content = convert_inline_box_node(doc, node_id, ctx, depth, out);
                    // Record the descendants the paragraph render path
                    // owns under its offset transform. Filter against
                    // already-recorded skip entries so nested inline-boxes
                    // don't double-register.
                    let descendants: Vec<crate::drawables::NodeId> = out
                        .drawn_since(mark)
                        .into_iter()
                        .filter(|id| *id != node_id)
                        .filter(|id| !out.inline_box_subtree_skip.contains(id))
                        .collect();
                    out.inline_box_subtree_skip.insert(node_id);
                    out.inline_box_subtree_skip
                        .extend(descendants.iter().copied());
                    out.inline_box_subtree_descendants
                        .insert(node_id, descendants);

                    let link = ctx.link_cache.lookup(doc, node_id);
                    let height = positioned.height.as_px().in_pt();
                    // Read baseline from `out` (Drawables). The Drawables-aware
                    // lookup queries `out.paragraphs[node_id]` (and
                    // `block_styles[node_id]` for top-inset) directly.
                    let baseline_shift =
                        inline_box_baseline_offset_from_drawables(doc, out, node_id)
                            .map(|bo| height - bo)
                            .unwrap_or(crate::units::Pt::ZERO);
                    let computed_y =
                        positioned.y.as_px().in_pt() - accumulated_line_top + baseline_shift;
                    let visible = doc
                        .get_node(node_id)
                        .map(super::style::extract_opacity_visible)
                        .map(|(_, v)| v)
                        .unwrap_or(true);
                    items.push(LineItem::InlineBox(InlineBoxItem {
                        node_id: content,
                        width: positioned.width.as_px().in_pt(),
                        height,
                        x_offset: positioned.x.as_px().in_pt(),
                        computed_y,
                        link,
                        opacity: 1.0,
                        visible,
                    }));
                }
            }
        }

        let line_height = metrics.line_height.as_px().in_pt();
        shaped_lines.push(ShapedLine {
            height: line_height,
            baseline: metrics.baseline.as_px().in_pt(),
            items,
        });
        accumulated_line_top += line_height;
    }

    if shaped_lines.is_empty() {
        return None;
    }

    Some(ParagraphRender::new(shaped_lines).with_id(extract_block_id(node)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blitz_adapter::BaseDocument;
    use crate::image::ImageFormat;
    use crate::paragraph::{
        InlineBoxItem, InlineImage, LineItem, LinkTarget, ShapedGlyphRun, ShapedLine,
        TextDecoration, VerticalAlign,
    };
    use crate::units::F32Units;
    use blitz_html::HtmlDocument;
    use std::ops::Deref;
    use std::sync::Arc;

    // ── Helpers ────────────────────────────────────────────────────────────

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    /// Compare a migrated `Pt` coordinate against a raw `f32` expectation.
    fn approx_pt(a: crate::units::Pt, b: f32) -> bool {
        (a.to_f32() - b).abs() < 0.01
    }

    /// A line with no items (text-only placeholder) and a paragraph-relative
    /// baseline. `baseline` here is the offset from the paragraph top, matching
    /// the convention used by `extract_paragraph` (which stores
    /// `parley_metrics.baseline.as_px().in_pt()` — a paragraph-relative value).
    fn text_line(height: f32, baseline: f32) -> ShapedLine {
        ShapedLine {
            height: height.as_pt(),
            baseline: baseline.as_pt(),
            items: Vec::new(),
        }
    }

    fn make_text_run(font_data: Vec<u8>) -> LineItem {
        LineItem::Text(ShapedGlyphRun {
            font_data: Arc::new(font_data),
            font_index: 0,
            font_size: 12.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: Arc::from(""),
            x_offset: crate::units::Pt::ZERO,
            link: None,
        })
    }

    fn make_image(width: f32, height: f32, va: VerticalAlign) -> LineItem {
        LineItem::Image(InlineImage {
            data: Arc::new(vec![]),
            format: ImageFormat::Png,
            width: width.as_pt(),
            height: height.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: va,
            opacity: 1.0,
            visible: true,
            computed_y: crate::units::Pt::ZERO,
            link: None,
        })
    }

    fn make_inline_box() -> LineItem {
        LineItem::InlineBox(InlineBoxItem {
            node_id: None,
            width: 10.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible: true,
        })
    }

    /// Collect `computed_y` from every `Image` item in `items`.
    /// Called with text-only lines (covering `_ => None`) and image lines
    /// (covering the `Image` arm) so both arms are always exercised.
    fn image_ys(items: &[LineItem]) -> Vec<f32> {
        items
            .iter()
            .filter_map(|item| match item {
                LineItem::Image(img) => Some(img.computed_y.to_f32()),
                _ => None,
            })
            .collect()
    }

    // Expected fallback values from the `default` literal in `metrics_from_line`.
    const DEF_ASCENT: f32 = 12.0;
    const DEF_DESCENT: f32 = 4.0;
    const DEF_X_HEIGHT: f32 = 8.0;
    const DEF_SUBSCRIPT: f32 = 4.0;
    const DEF_SUPERSCRIPT: f32 = 6.0;

    // ── metrics_from_line ──────────────────────────────────────────────────

    #[test]
    fn metrics_from_line_empty_line_returns_defaults() {
        let line = text_line(16.0, 12.0);
        let m = metrics_from_line(&line);
        assert!(approx(m.ascent, DEF_ASCENT), "ascent={}", m.ascent);
        assert!(approx(m.descent, DEF_DESCENT), "descent={}", m.descent);
        assert!(approx(m.x_height, DEF_X_HEIGHT), "x_height={}", m.x_height);
        assert!(
            approx(m.subscript_offset, DEF_SUBSCRIPT),
            "subscript={}",
            m.subscript_offset
        );
        assert!(
            approx(m.superscript_offset, DEF_SUPERSCRIPT),
            "superscript={}",
            m.superscript_offset
        );
    }

    /// `LineItem::Image` arms hit the `continue` branch — the function skips
    /// all image items and falls through to the default return.
    #[test]
    fn metrics_from_line_image_only_returns_defaults() {
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(make_image(10.0, 8.0, VerticalAlign::Baseline));
        let m = metrics_from_line(&line);
        assert!(approx(m.ascent, DEF_ASCENT), "ascent={}", m.ascent);
    }

    /// `LineItem::InlineBox` arms hit the `continue` branch — same fallback.
    #[test]
    fn metrics_from_line_inline_box_only_returns_defaults() {
        let mut line = text_line(16.0, 12.0);
        line.items.push(make_inline_box());
        let m = metrics_from_line(&line);
        assert!(approx(m.ascent, DEF_ASCENT), "ascent={}", m.ascent);
    }

    /// Empty / invalid font bytes cause `skrifa::FontRef::from_index` to fail.
    /// The `if let Ok(...)` guard is not entered, so the loop continues and the
    /// function returns defaults after exhausting all items.
    #[test]
    fn metrics_from_line_invalid_font_bytes_returns_defaults() {
        let mut line = text_line(16.0, 12.0);
        line.items.push(make_text_run(vec![]));
        let m = metrics_from_line(&line);
        assert!(approx(m.ascent, DEF_ASCENT), "ascent={}", m.ascent);
        assert!(approx(m.descent, DEF_DESCENT), "descent={}", m.descent);
    }

    /// Mixed line: one image (skipped), one text with bad font (falls through),
    /// one inline-box (skipped) — all paths return defaults.
    #[test]
    fn metrics_from_line_mixed_non_text_items_return_defaults() {
        let mut line = text_line(16.0, 12.0);
        line.items.push(make_image(5.0, 5.0, VerticalAlign::Middle));
        line.items.push(make_text_run(vec![0, 1, 2, 3]));
        line.items.push(make_inline_box());
        let m = metrics_from_line(&line);
        assert!(approx(m.ascent, DEF_ASCENT), "ascent={}", m.ascent);
        assert!(
            approx(m.subscript_offset, DEF_SUBSCRIPT),
            "subscript={}",
            m.subscript_offset
        );
    }

    // ── recalculate_paragraph_line_boxes ──────────────────────────────────

    #[test]
    fn recalculate_paragraph_line_boxes_empty_slice_is_noop() {
        let mut lines: Vec<ShapedLine> = Vec::new();
        recalculate_paragraph_line_boxes(&mut lines);
        assert!(lines.is_empty());
    }

    /// A text-only line has no images, so `recalculate_line_box` is a no-op
    /// for both height and baseline.  For the first line `original_y_acc` and
    /// `new_y_acc` are both 0 — the normalization/de-normalization cancels out
    /// and the stored values are unchanged.
    #[test]
    fn recalculate_paragraph_line_boxes_text_only_single_line_unchanged() {
        let mut lines = vec![{
            let mut l = text_line(16.0, 12.0);
            l.items.push(make_text_run(vec![]));
            l
        }];
        recalculate_paragraph_line_boxes(&mut lines);
        let h = lines[0].height;
        assert!(approx_pt(h, 16.0), "height={h:?}");
        assert!(
            approx_pt(lines[0].baseline, 12.0),
            "baseline={:?}",
            lines[0].baseline
        );
    }

    /// Two text-only lines: for each line `new_y_acc == original_y_acc`
    /// (no expansion), so `baseline -= original_y_acc` and
    /// `baseline += new_y_acc` cancel out — both baselines are unchanged.
    #[test]
    fn recalculate_paragraph_line_boxes_two_text_lines_baselines_unchanged() {
        // Paragraph-relative baselines: line 0 baseline=12, line 1 baseline=26
        // (line 0 is 16pt tall, line 1 has 10pt line-relative baseline → 16+10=26).
        let mut lines = vec![
            {
                let mut l = text_line(16.0, 12.0);
                l.items.push(make_text_run(vec![]));
                l
            },
            {
                let mut l = text_line(14.0, 26.0);
                l.items.push(make_text_run(vec![]));
                l
            },
        ];
        recalculate_paragraph_line_boxes(&mut lines);
        assert!(
            approx_pt(lines[0].height, 16.0),
            "line0 height={:?}",
            lines[0].height
        );
        assert!(
            approx_pt(lines[0].baseline, 12.0),
            "line0 baseline={:?}",
            lines[0].baseline
        );
        assert!(
            approx_pt(lines[1].height, 14.0),
            "line1 height={:?}",
            lines[1].height
        );
        assert!(
            approx_pt(lines[1].baseline, 26.0),
            "line1 baseline={:?}",
            lines[1].baseline
        );
    }

    /// A Baseline-aligned image that fits inside the first line's box causes no
    /// height expansion. `recalculate_line_box` sets `img.computed_y` to
    /// `img_top` (baseline − image_height).  For the first line `new_y_acc==0`,
    /// so the final paragraph-relative `computed_y` equals the line-relative
    /// `img_top`.
    ///
    /// Line: height=16, baseline=12.  img height=8 → img_top = 12−8 = 4.
    #[test]
    fn recalculate_paragraph_line_boxes_baseline_image_in_first_line_sets_computed_y() {
        let mut lines = vec![{
            let mut l = text_line(16.0, 12.0);
            l.items.push(make_image(10.0, 8.0, VerticalAlign::Baseline));
            l
        }];
        recalculate_paragraph_line_boxes(&mut lines);
        let h = lines[0].height;
        assert!(approx_pt(h, 16.0), "height={h:?}");
        assert!(
            approx_pt(lines[0].baseline, 12.0),
            "baseline={:?}",
            lines[0].baseline
        );
        if let LineItem::Image(img) = &lines[0].items[0] {
            assert!(
                approx_pt(img.computed_y, 4.0),
                "computed_y={:?}",
                img.computed_y
            );
        } else {
            panic!("expected Image at index 0");
        }
    }

    /// An image in the SECOND line receives `new_y_acc` (the height of the
    /// first line) added to its computed_y, making the result paragraph-relative.
    ///
    /// Line 0: height=10, baseline=8, text-only  → new_y_acc becomes 10.
    /// Line 1: height=16, paragraph-relative baseline=18 (line-relative 8),
    ///         image (Baseline, height=2) → line-relative img_top = 8−2 = 6.
    ///         After `img.computed_y += new_y_acc(10)` → paragraph-relative = 16.
    #[test]
    fn recalculate_paragraph_line_boxes_image_in_second_line_gets_paragraph_offset() {
        let line1_para_baseline = 10.0 + 8.0; // accumulated height(10) + line-relative baseline(8)
        let mut lines = vec![
            {
                // Line 0: text-only, height=10, paragraph-relative baseline=8.
                let mut l = text_line(10.0, 8.0);
                l.items.push(make_text_run(vec![]));
                l
            },
            {
                // Line 1: one small image (height=2, Baseline). The image fits
                // within the line box after normalization so no height expansion
                // occurs: line height stays 16.
                let mut l = text_line(16.0, line1_para_baseline);
                l.items.push(make_image(5.0, 2.0, VerticalAlign::Baseline));
                l
            },
        ];
        recalculate_paragraph_line_boxes(&mut lines);

        // Line 0 must be unchanged.
        assert!(
            approx_pt(lines[0].height, 10.0),
            "line0 height={:?}",
            lines[0].height
        );

        // For line 1: normalize baseline → 18-10=8; img_top=8-2=6; no expansion;
        // img.computed_y = 6 → += new_y_acc(10) → 16.
        if let LineItem::Image(img) = &lines[1].items[0] {
            assert!(
                approx_pt(img.computed_y, 16.0),
                "computed_y={:?}",
                img.computed_y
            );
        } else {
            panic!("expected Image in line 1 at index 0");
        }
    }

    // ── metrics_from_line: happy path with real font bytes ─────────────────

    /// Exercises the `Ok(font_ref)` branch of `metrics_from_line`: when the
    /// text run carries valid TTF bytes, skrifa parses the font and returns
    /// real metrics rather than the hard-coded fallback values.
    ///
    /// We only assert that the values are positive and that ascent is NOT
    /// the fallback (12.0), which would indicate the font branch was entered.
    #[test]
    fn metrics_from_line_real_font_returns_font_metrics() {
        const NOTO_SANS: &[u8] = include_bytes!("../../../../examples/.fonts/NotoSans-Regular.ttf");

        let mut line = text_line(16.0, 12.0);
        line.items.push(make_text_run(NOTO_SANS.to_vec()));
        let m = metrics_from_line(&line);

        // The real font branch was taken, so values differ from the fallback.
        assert!(
            m.ascent != DEF_ASCENT,
            "expected real font ascent, got fallback 12.0"
        );
        assert!(m.ascent > 0.0, "ascent={}", m.ascent);
        assert!(m.descent > 0.0, "descent={}", m.descent);
        assert!(m.x_height > 0.0, "x_height={}", m.x_height);
        // Derived fields follow ascent proportionally.
        let expected_sub = m.ascent * 0.3;
        let expected_sup = m.ascent * 0.4;
        assert!(approx(m.subscript_offset, expected_sub));
        assert!(approx(m.superscript_offset, expected_sup));
    }

    /// When multiple items precede the first valid-font text run, the function
    /// must skip non-text items and continue to find the first parseable font.
    #[test]
    fn metrics_from_line_real_font_skips_non_text_items_before_it() {
        const NOTO_SANS: &[u8] = include_bytes!("../../../../examples/.fonts/NotoSans-Regular.ttf");

        let mut line = text_line(16.0, 12.0);
        // image and inline-box come first, then a valid-font text run.
        line.items
            .push(make_image(5.0, 5.0, VerticalAlign::Baseline));
        line.items.push(make_inline_box());
        line.items.push(make_text_run(NOTO_SANS.to_vec()));
        let m = metrics_from_line(&line);

        assert!(m.ascent != DEF_ASCENT, "expected real font, got default");
        assert!(m.ascent > 0.0, "ascent={}", m.ascent);
    }

    // ── recalculate_paragraph_line_boxes: divergent new_y_acc ─────────────

    /// When a tall Baseline image causes line 0 to expand (height 16 → 24),
    /// `new_y_acc` and `original_y_acc` diverge after line 0:
    ///   original_y_acc = 16 (original height)
    ///   new_y_acc      = 24 (expanded height)
    ///
    /// Line 1's baseline must be adjusted by `new_y_acc` (24), not
    /// `original_y_acc` (16). Without this, subsequent text would land at
    /// the wrong vertical position inside the expanded paragraph box.
    ///
    /// Setup (all values in PDF pt, default font metrics: ascent=12, descent=4):
    ///   Line 0: height=16, para-relative baseline=12
    ///           Baseline image height=20 → img_top = 12-20 = -8 < line_top(0)
    ///           After recalculate_line_box: line_top=-8, height=24, baseline=20
    ///   Line 1: height=12, para-relative baseline=24 (original line 0 height + 8)
    ///           No images.
    ///           Normalize:   baseline -= original_y_acc(16) → 24-16 = 8
    ///           Recalculate: no change (text-only)
    ///           De-normalize: baseline += new_y_acc(24)    → 8+24  = 32
    #[test]
    fn recalculate_paragraph_line_boxes_expanding_line_shifts_subsequent_baseline() {
        let mut lines = vec![
            {
                // Line 0: tall Baseline image forces expansion.
                let mut l = text_line(16.0, 12.0);
                l.items.push(make_image(5.0, 20.0, VerticalAlign::Baseline));
                l
            },
            {
                // Line 1: text-only. Para-relative baseline = original line-0
                // height (16) + line-local baseline (8) = 24.
                let mut l = text_line(12.0, 24.0);
                l.items.push(make_text_run(vec![]));
                l
            },
        ];
        recalculate_paragraph_line_boxes(&mut lines);

        // Line 0 must expand.
        let (h0, b0) = (lines[0].height, lines[0].baseline);
        assert!(approx_pt(h0, 24.0));
        assert!(approx_pt(b0, 20.0));

        // Line 1 height unchanged; baseline adjusted by new_y_acc=24 not 16.
        let (h1, b1) = (lines[1].height, lines[1].baseline);
        assert!(approx_pt(h1, 12.0));
        assert!(approx_pt(b1, 32.0));
        // Text-only line has no images; this call covers the `_ => None` arm of image_ys.
        assert!(image_ys(&lines[1].items).is_empty());
    }

    /// Companion to the above: an image in line 1 must receive `new_y_acc=24`
    /// (the expanded first-line height) as its paragraph-offset, not the
    /// original 16. This exercises `img.computed_y += new_y_acc` when
    /// `new_y_acc != original_y_acc`.
    ///
    /// Line 1: height=12, baseline=24 (para-relative), image height=4 (Baseline).
    ///   Normalize baseline: 24-16=8
    ///   img_top = 8-4=4, img_bottom=8 → within [0,12), no expansion.
    ///   img.computed_y = 4, then += new_y_acc(24) → 28.
    #[test]
    fn recalculate_paragraph_line_boxes_image_in_second_line_uses_expanded_new_y_acc() {
        let mut lines = vec![
            {
                let mut l = text_line(16.0, 12.0);
                l.items.push(make_image(5.0, 20.0, VerticalAlign::Baseline));
                l
            },
            {
                // Line 1 has a small Baseline image (height=4).
                let mut l = text_line(12.0, 24.0);
                l.items.push(make_image(5.0, 4.0, VerticalAlign::Baseline));
                l
            },
        ];
        recalculate_paragraph_line_boxes(&mut lines);

        // Line 0: verify expansion occurred so the test is meaningful.
        let h0 = lines[0].height;
        assert!(approx_pt(h0, 24.0));

        // Line 1 image: computed_y = line-local img_top(4) + new_y_acc(24) = 28.
        // image_ys covers the LineItem::Image arm; the _ => None arm is covered in
        // recalculate_paragraph_line_boxes_expanding_line_shifts_subsequent_baseline.
        assert!(approx(image_ys(&lines[1].items)[0], 28.0));
    }

    // ── Helpers for Blitz-backed tests ────────────────────────────────────────

    fn parse_doc(html: &str) -> HtmlDocument {
        crate::blitz_adapter::parse_and_layout(
            html,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        )
    }

    fn find_first_by_tag(doc: &BaseDocument, start_id: usize, tag: &str) -> Option<usize> {
        let node = doc.get_node(start_id)?;
        if node
            .element_data()
            .is_some_and(|e| e.name.local.as_ref() == tag)
        {
            return Some(start_id);
        }
        for &c in &node.children {
            if let Some(found) = find_first_by_tag(doc, c, tag) {
                return Some(found);
            }
        }
        None
    }

    fn find_tag(doc: &HtmlDocument, tag: &str) -> usize {
        let root = doc.root_element();
        find_first_by_tag(doc.deref(), root.id, tag)
            .unwrap_or_else(|| panic!("<{tag}> not found in document"))
    }

    fn make_paragraph_entry(baselines: &[f32]) -> crate::drawables::ParagraphEntry {
        crate::drawables::ParagraphEntry {
            lines: baselines
                .iter()
                .map(|&b| ShapedLine {
                    height: 16.0_f32.as_pt(),
                    baseline: b.as_pt(),
                    items: vec![],
                })
                .collect(),
            opacity: 1.0,
            visible: true,
            id: None,
        }
    }

    fn make_block_entry_plain() -> crate::drawables::BlockEntry {
        crate::drawables::BlockEntry {
            style: crate::draw_primitives::BlockStyle::default(),
            opacity: 1.0,
            visible: true,
            id: None,
            layout_size: None,
            clip_descendants: vec![],
            opacity_descendants: vec![],
        }
    }

    // ── resolve_enclosing_anchor ──────────────────────────────────────────────

    #[test]
    fn resolve_enclosing_anchor_returns_none_when_no_anchor_ancestor() {
        let doc = parse_doc("<html><body><div><span>text</span></div></body></html>");
        let span_id = find_tag(&doc, "span");
        assert!(
            super::resolve_enclosing_anchor(doc.deref(), span_id).is_none(),
            "no <a> ancestor must return None"
        );
    }

    #[test]
    fn resolve_enclosing_anchor_external_href_returns_external_target() {
        let doc = parse_doc(
            r#"<html><body><a href="https://example.com"><span>link</span></a></body></html>"#,
        );
        let span_id = find_tag(&doc, "span");
        let result = super::resolve_enclosing_anchor(doc.deref(), span_id);
        assert!(result.is_some(), "external href must produce Some");
        let (_anchor_id, link_span) = result.unwrap();
        match &link_span.target {
            LinkTarget::External(url) => {
                assert_eq!(url.as_str(), "https://example.com");
            }
            other => panic!("expected External, got {:?}", other),
        }
    }

    #[test]
    fn resolve_enclosing_anchor_internal_href_returns_internal_target() {
        let doc =
            parse_doc(r##"<html><body><a href="#section"><span>link</span></a></body></html>"##);
        let span_id = find_tag(&doc, "span");
        let result = super::resolve_enclosing_anchor(doc.deref(), span_id);
        assert!(result.is_some(), "fragment href must produce Some");
        match &result.unwrap().1.target {
            LinkTarget::Internal(frag) => {
                assert_eq!(frag.as_str(), "section");
            }
            other => panic!("expected Internal, got {:?}", other),
        }
    }

    #[test]
    fn resolve_enclosing_anchor_empty_href_returns_none() {
        let doc = parse_doc(r#"<html><body><a href=""><span>link</span></a></body></html>"#);
        let span_id = find_tag(&doc, "span");
        assert!(
            super::resolve_enclosing_anchor(doc.deref(), span_id).is_none(),
            "empty href must return None"
        );
    }

    #[test]
    fn resolve_enclosing_anchor_returns_none_when_a_has_no_href() {
        let doc = parse_doc(r#"<html><body><a><span>no href</span></a></body></html>"#);
        let span_id = find_tag(&doc, "span");
        assert!(
            super::resolve_enclosing_anchor(doc.deref(), span_id).is_none(),
            "<a> with no href attr → None"
        );
    }

    #[test]
    fn resolve_enclosing_anchor_includes_alt_text_when_anchor_has_text() {
        let doc =
            parse_doc(r#"<html><body><a href="https://example.com">Click here</a></body></html>"#);
        let a_id = find_tag(&doc, "a");
        let result = super::resolve_enclosing_anchor(doc.deref(), a_id);
        let (_, span) = result.expect("anchor finds itself");
        assert!(
            span.alt_text.is_some(),
            "anchor with text content → alt_text set"
        );
    }

    #[test]
    fn resolve_enclosing_anchor_walks_multiple_levels_to_find_anchor() {
        // <span> is nested three levels deep inside a valid <a>. The function
        // must walk up span → p → div → a to find the enclosing anchor.
        // (Nested <a> elements are invalid HTML5 and would be parsed as
        // siblings, so we use a real multi-element nesting instead.)
        let doc = parse_doc(
            r#"<html><body><a href="https://example.com"><div><p><span>deep</span></p></div></a></body></html>"#,
        );
        let span_id = find_tag(&doc, "span");
        let result = super::resolve_enclosing_anchor(doc.deref(), span_id);
        let (_, span) = result.expect("anchor found via multi-level walk");
        match &span.target {
            LinkTarget::External(url) => {
                assert_eq!(url.as_str(), "https://example.com");
            }
            other => panic!("expected External, got {other:?}"),
        }
    }

    #[test]
    fn resolve_enclosing_anchor_returns_anchor_node_id_not_span() {
        let doc =
            parse_doc(r#"<html><body><a href="https://x.com"><span>x</span></a></body></html>"#);
        let span_id = find_tag(&doc, "span");
        let anchor_id = find_tag(&doc, "a");
        let (returned_id, _) = super::resolve_enclosing_anchor(doc.deref(), span_id).unwrap();
        assert_eq!(
            returned_id, anchor_id,
            "returned id must be the <a> node's id, not the span's"
        );
    }

    // ── inline_box_baseline_offset_from_drawables ─────────────────────────────

    #[test]
    fn inline_box_baseline_overflow_clip_x_returns_none() {
        let doc = parse_doc("<html><body><div>x</div></body></html>");
        let mut out = crate::drawables::Drawables::new();
        let node_id = 9999;
        let mut entry = make_block_entry_plain();
        entry.style = crate::draw_primitives::BlockStyle {
            overflow_x: crate::draw_primitives::Overflow::Clip,
            ..Default::default()
        };
        out.block_styles.insert(node_id, entry);
        let result = super::inline_box_baseline_offset_from_drawables(doc.deref(), &out, node_id);
        assert!(
            result.is_none(),
            "overflow_x=Clip must short-circuit to None"
        );
    }

    #[test]
    fn inline_box_baseline_no_overflow_clip_delegates_to_pageable_last() {
        // No block entry → overflow-clip guard skipped → pageable_last called.
        // A paragraph entry for node_id means branch 1 fires and returns the baseline.
        let doc = parse_doc("<html><body><div>x</div></body></html>");
        let mut out = crate::drawables::Drawables::new();
        let node_id = 9998;
        out.paragraphs
            .insert(node_id, make_paragraph_entry(&[12.0]));
        let result = super::inline_box_baseline_offset_from_drawables(doc.deref(), &out, node_id);
        assert_eq!(
            result,
            Some(12.0_f32.as_pt()),
            "no overflow clip + paragraph entry → Some(baseline)"
        );
    }

    #[test]
    fn inline_box_baseline_overflow_visible_block_does_not_short_circuit() {
        // Block entry with default (visible) overflow must NOT trigger the guard.
        // With a paragraph also present the function should return Some.
        let doc = parse_doc("<html><body><div>x</div></body></html>");
        let mut out = crate::drawables::Drawables::new();
        let node_id = 9997;
        out.block_styles.insert(node_id, make_block_entry_plain()); // overflow Visible
        out.paragraphs.insert(node_id, make_paragraph_entry(&[9.0]));
        let result = super::inline_box_baseline_offset_from_drawables(doc.deref(), &out, node_id);
        assert_eq!(
            result,
            Some(9.0_f32.as_pt()),
            "visible overflow must not short-circuit"
        );
    }

    // ── pageable_last_baseline_from_drawables ──────────────────────────────────

    #[test]
    fn pageable_last_baseline_returns_none_at_max_depth() {
        let doc = parse_doc("<html><body><div>x</div></body></html>");
        let out = crate::drawables::Drawables::new();
        let result = super::pageable_last_baseline_from_drawables(
            doc.deref(),
            &out,
            0,
            crate::MAX_DOM_DEPTH,
        );
        assert!(
            result.is_none(),
            "at MAX_DOM_DEPTH must return None immediately"
        );
    }

    #[test]
    fn pageable_last_baseline_uses_last_line_of_multi_line_paragraph() {
        // Multi-line paragraph: the function must return the LAST line's baseline,
        // not the first.
        let doc = parse_doc("<html><body><div>x</div></body></html>");
        let mut out = crate::drawables::Drawables::new();
        let node_id = 9997;
        out.paragraphs
            .insert(node_id, make_paragraph_entry(&[12.0, 26.0]));
        let result = super::pageable_last_baseline_from_drawables(doc.deref(), &out, node_id, 0);
        assert_eq!(
            result,
            Some(26.0_f32.as_pt()),
            "must return last line baseline (26.0), not first (12.0)"
        );
    }

    #[test]
    fn pageable_last_baseline_adds_border_and_padding_top_inset() {
        // Block entry with border-top=4pt and padding-top=2pt adds top_inset=6pt
        // to the paragraph baseline.
        let doc = parse_doc("<html><body><div>x</div></body></html>");
        let mut out = crate::drawables::Drawables::new();
        let node_id = 9996;
        out.paragraphs
            .insert(node_id, make_paragraph_entry(&[12.0]));
        let mut entry = make_block_entry_plain();
        entry.style.border_widths[0] = 4.0_f32.as_pt(); // top border
        entry.style.padding[0] = 2.0_f32.as_pt(); // top padding
        out.block_styles.insert(node_id, entry);
        let result = super::pageable_last_baseline_from_drawables(doc.deref(), &out, node_id, 0);
        // top_inset = 4 + 2 = 6; baseline = 12 → Some(18).
        assert!(
            result.is_some_and(|v| (v.to_f32() - 18.0).abs() < 0.001),
            "expected Some(18.0), got {:?}",
            result
        );
    }

    #[test]
    fn pageable_last_baseline_empty_paragraph_lines_falls_through_to_dom_walk() {
        // A paragraph entry with no lines causes `lines.last()` to return None,
        // so branch 1 is not taken. The function then tries the DOM walk, but
        // node_id 9995 does not exist in the Blitz doc, so `doc.get_node` fails
        // and the function returns None.
        let doc = parse_doc("<html><body><div>x</div></body></html>");
        let mut out = crate::drawables::Drawables::new();
        let node_id = 9995;
        out.paragraphs.insert(node_id, make_paragraph_entry(&[])); // no lines
        let result = super::pageable_last_baseline_from_drawables(doc.deref(), &out, node_id, 0);
        assert!(
            result.is_none(),
            "empty lines + non-existent node must return None"
        );
    }

    #[test]
    fn pageable_last_baseline_walks_dom_children_in_reverse_to_find_paragraph() {
        // Parse a doc with section→div(text). Insert a ParagraphEntry only for
        // the div. Calling on section (no paragraph entry) must walk children in
        // reverse, find the div's ParagraphEntry, and return its baseline plus
        // the div's layout y-offset.
        let doc = parse_doc("<html><body><section><div>text content</div></section></body></html>");
        let section_id = find_tag(&doc, "section");
        let div_id = find_tag(&doc, "div");
        let mut out = crate::drawables::Drawables::new();
        out.paragraphs.insert(div_id, make_paragraph_entry(&[12.0]));
        let result = super::pageable_last_baseline_from_drawables(doc.deref(), &out, section_id, 0);
        // section has no paragraph entry → DOM walk finds div → Some(y_offset + 12.0).
        // y_offset >= 0, so result >= 12.0.
        assert!(
            result.is_some(),
            "reverse DOM walk must find child's paragraph entry"
        );
        assert!(
            result.unwrap().to_f32() >= 12.0,
            "baseline must be at least the child paragraph's baseline"
        );
    }

    // ── smoke tests for try_convert code paths ────────────────────────────────

    // Minimal 1×1 red PNG — shared by all smoke tests that need an image asset.
    const RED_1X1_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn make_engine_with_dot_png() -> crate::engine::Engine {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        crate::engine::Engine::builder().assets(bundle).build()
    }

    fn pdf_has_image(pdf: &[u8]) -> bool {
        pdf.windows(b"/Subtype /Image".len())
            .any(|w| w == b"/Subtype /Image")
            | pdf
                .windows(b"/Subtype/Image".len())
                .any(|w| w == b"/Subtype/Image")
    }

    #[test]
    fn smoke_try_convert_inline_root_with_overflow_clip() {
        // A <p> with overflow:hidden is an inline root. needs_block_wrapper() is
        // true (has_overflow_clip), so needs_block_pre=true and clipping_pre=true.
        // This exercises the pre_snapshot + clip_descendants tracking path.
        let pdf = crate::engine::Engine::builder()
            .build()
            .render(
                r#"<!doctype html><html><body>
                <p style="overflow:hidden;height:30px;background:#abc">Clipped text</p>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn smoke_try_convert_inline_root_with_opacity_scope() {
        // A <p> with background (visual style) and opacity < 1.0 activates
        // needs_block_pre=true and opacity_scope_pre=true in try_convert.
        // The pre_snapshot + opacity_descendants path is exercised.
        let pdf = crate::engine::Engine::builder()
            .build()
            .render(
                r#"<!doctype html><html><body>
                <p style="opacity:0.5;background:#def">Faded paragraph</p>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert: before_inline map closure (lines 63-65) +
    // inject into existing paragraph (lines 82-84).
    // A <p> with text AND a ::before pseudo-image triggers both:
    // - build_inline_pseudo_image returns Some → the .map() closure runs
    // - paragraph_opt is Some (text present) → inject_inline_pseudo_images runs.
    #[test]
    fn smoke_before_inline_image_on_paragraph_with_text() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"p::before { content: url("dot.png"); width: 8px; height: 8px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(r#"<!doctype html><html><body><p>Hello world</p></body></html>"#)
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — ::before pseudo-image injection branch may have been skipped"
        );
    }

    // try_convert: after_inline map closure (lines 74-76) +
    // inject into existing paragraph (lines 82-84).
    #[test]
    fn smoke_after_inline_image_on_paragraph_with_text() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"p::after { content: url("dot.png"); width: 8px; height: 8px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(r#"<!doctype html><html><body><p>Hello world</p></body></html>"#)
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — ::after pseudo-image injection branch may have been skipped"
        );
    }

    // try_convert: both before and after inline images, with text present —
    // exercises both map closures and the combined inject path.
    #[test]
    fn smoke_before_and_after_inline_images_on_paragraph_with_text() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(
            r#"p::before { content: url("dot.png"); width: 6px; height: 6px; }
               p::after  { content: url("dot.png"); width: 6px; height: 6px; }"#,
        );
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(r#"<!doctype html><html><body><p>Both sides</p></body></html>"#)
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — ::before/::after combined injection branch may have been skipped"
        );
    }

    // try_convert: pseudo-only inline root — element with ::before image but no
    // text content. paragraph_opt is None; before_inline is Some, so the
    // "synthesize minimal paragraph" branch (lines 162-222) is taken if the
    // element is flagged as an inline root by Blitz.
    #[test]
    fn smoke_pseudo_only_before_image_empty_inline_root() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle
            .add_css(r#"p.marker::before { content: url("dot.png"); width: 10px; height: 10px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <p class="marker"></p>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — pseudo-only paragraph synthesis branch may have been skipped"
        );
    }

    // try_convert: inside list-style-image on an inline-root <li> (lines 88-107).
    // The <li> directly contains text (making it an inline root). With
    // list-style-position: inside and list-style-image: url(bullet.png),
    // resolve_inside_image_marker returns Some and the shift loop runs.
    #[test]
    fn smoke_inside_list_image_marker_on_inline_root_li() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("bullet.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-position: inside;
                           list-style-image: url('bullet.png')">
                    <li>Item with inside image marker</li>
                    <li>Second item</li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — inside list-style-image marker shift branch may have been skipped"
        );
    }

    // try_convert: inside list-style-image where the <li> also has a ::before
    // inline pseudo image — both marker shift and before_inline inject run.
    #[test]
    fn smoke_inside_list_image_marker_with_before_pseudo_on_inline_root_li() {
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("bullet.png", RED_1X1_PNG.to_vec());
        bundle.add_image("dot.png", RED_1X1_PNG.to_vec());
        bundle.add_css(r#"li::before { content: url("dot.png"); width: 6px; height: 6px; }"#);
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-position: inside;
                           list-style-image: url('bullet.png')">
                    <li>Marker + before pseudo</li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — inside marker + ::before combined injection branch may have been skipped"
        );
    }

    // inline_box_baseline_from_drawables: the `make_engine_with_dot_png` helper
    // is used here to ensure the helper compiles and its asset bundle is wired.
    #[test]
    fn smoke_make_engine_with_dot_png_helper_compiles() {
        let pdf = make_engine_with_dot_png()
            .render(r#"<!doctype html><html><body><p>ok</p></body></html>"#)
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // try_convert: pseudo-only (before_inline=Some, paragraph_opt=None) where
    // needs_block_pre=true via background (has_visual_style), but clipping_pre=false
    // and opacity_scope_pre=false (opacity=1.0). Exercises the `if needs_block {}`
    // block insertion at lines 191-203 WITHOUT a pre_mark.
    #[test]
    fn smoke_pseudo_only_with_background_covers_needs_block_no_pre_mark() {
        let pdf = make_engine_with_dot_png()
            .render(
                r#"<!doctype html><html><head>
                <style>
                  p.marker::before { content: url("dot.png"); width: 10px; height: 10px; }
                </style>
                </head><body>
                <p class="marker" style="background: #eee; padding: 4px;"></p>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — pseudo-only needs_block (no pre_mark) path may have been skipped"
        );
    }

    // try_convert: pseudo-only where needs_block_pre=true and clipping_pre=true
    // (overflow:hidden). Exercises the pre_mark + clip_descendants path at
    // lines 204-215 inside the needs_block block.
    #[test]
    fn smoke_pseudo_only_with_overflow_hidden_covers_clip_descendants() {
        let pdf = make_engine_with_dot_png()
            .render(
                r#"<!doctype html><html><head>
                <style>
                  p.marker::before { content: url("dot.png"); width: 10px; height: 10px; }
                </style>
                </head><body>
                <p class="marker" style="overflow: hidden; height: 20px;"></p>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — pseudo-only clip_descendants path may have been skipped"
        );
    }

    // try_convert: pseudo-only where needs_block_pre=true via background and
    // opacity_scope_pre=true (opacity<1.0, no overflow:hidden). Exercises the
    // pre_mark + opacity_descendants path at lines 204-215 inside needs_block.
    #[test]
    fn smoke_pseudo_only_with_opacity_covers_opacity_descendants() {
        let pdf = make_engine_with_dot_png()
            .render(
                r#"<!doctype html><html><head>
                <style>
                  p.marker::before { content: url("dot.png"); width: 10px; height: 10px; }
                </style>
                </head><body>
                <p class="marker" style="background: #eee; opacity: 0.5;"></p>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(
            pdf_has_image(&pdf),
            "image XObject missing — pseudo-only opacity_descendants path may have been skipped"
        );
    }

    // pageable_last_baseline_from_drawables: exercises line 393 (closing `}` of
    // the `if let Some(inner) = …` arm when the recursive call returns None).
    // DOM order inside <section>: [div(text), aside(empty)]. Reversed: [aside, div].
    // The aside yields None from the recursive call (no paragraph, no children),
    // so the if-let condition is false and its closing `}` is reached. The div
    // then yields Some and the function returns.
    #[test]
    fn pageable_last_baseline_walk_past_empty_sibling_to_find_baseline() {
        let doc = parse_doc(
            "<html><body><section><div>text</div><aside></aside></section></body></html>",
        );
        let section_id = find_tag(&doc, "section");
        let div_id = find_tag(&doc, "div");
        let mut out = crate::drawables::Drawables::new();
        out.paragraphs.insert(div_id, make_paragraph_entry(&[12.0]));
        let result = super::pageable_last_baseline_from_drawables(doc.deref(), &out, section_id, 0);
        assert!(
            result.is_some(),
            "must find div paragraph baseline via DOM walk past empty aside sibling"
        );
        assert!(
            result.unwrap().to_f32() >= 12.0,
            "baseline must be at least the child paragraph's baseline"
        );
    }
}
