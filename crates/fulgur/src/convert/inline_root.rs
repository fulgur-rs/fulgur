use super::*;
use super::{list_marker, positioned, pseudo};

/// Dispatcher entry for inline-root nodes (those with `node.flags.is_inline_root()`).
/// Builds a paragraph (via `extract_paragraph`) and wraps in a block when CSS
/// styles or pseudo content require it; otherwise returns the bare paragraph.
///
/// Returns `Some(Pageable)` on a successful build; returns `None` to fall through
/// (when the node is not an inline root, or when the inline root has no text and
/// no inline pseudo images and the caller's container path should run instead).
pub(super) fn try_convert(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut super::ConvertContext<'_>,
    depth: usize,
) -> Option<Box<dyn crate::pageable::Pageable>> {
    let node = doc.get_node(node_id)?;
    let (width, height) = size_in_pt(node.final_layout.size);

    // Check if this is an inline root (contains text layout)
    if node.flags.is_inline_root() {
        let paragraph_opt = extract_paragraph(doc, node, ctx, depth);
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);
        let content_box = compute_content_box(node, &style);

        // Inline pseudo images (display: inline is the CSS default for pseudos)
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
            // Inject pseudo images BEFORE the list marker so the marker stays
            // at index 0 of the first line after both injections. CSS order
            // for list-style-position: inside is: marker → ::before → content.
            // Blitz already pushes text markers to the inline layout before
            // ::before, so when list-style-image triggers marker injection we
            // must put it at index 0 last.
            if before_inline.is_some() || after_inline.is_some() {
                pseudo::inject_inline_pseudo_images(
                    &mut paragraph.lines,
                    before_inline,
                    after_inline,
                );
                recalculate_paragraph_line_boxes(&mut paragraph.lines);
                paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
            }

            // Inject inside list-style-image as inline image at start of first line.
            // Runs AFTER pseudo image injection so the marker ends up at index 0
            // and pushes existing items (including ::before) to index 1+.
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

            // Then existing block pseudo check
            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_pseudo_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            let pagination = extract_pagination_from_column_css(ctx, node);
            if style.needs_block_wrapper()
                || has_pseudo
                || pagination != crate::pageable::Pagination::default()
            {
                let (child_x, child_y) = style.content_inset();
                // Propagate visibility to the inner paragraph — it's not a real CSS child
                // but the node's own text content, so it must respect the node's visibility.
                // Do NOT propagate opacity — the wrapping block handles it via push_opacity.
                let mut p = paragraph;
                p.visible = visible;
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(p),
                    x: child_x,
                    y: child_y,
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
                    .with_opacity(opacity)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, height);
                // Use Taffy's computed height (includes padding + border) instead of children-only height
                block.layout_size = Some(Size { width, height });
                return Some(Box::new(block));
            }
            let mut p = paragraph;
            p.opacity = opacity;
            p.visible = visible;
            return Some(Box::new(p));
        } else if before_inline.is_some() || after_inline.is_some() {
            // Synthesize a minimal paragraph for pseudo-only elements (e.g.
            // `<span class="icon"></span>` with `::before { content: url(...) }`)
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
            let mut paragraph = ParagraphPageable::new(vec![line]);
            paragraph.opacity = opacity;
            paragraph.visible = visible;

            // Check for block pseudo images too
            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_pseudo_children(doc, node, ctx, depth);
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
                    .with_opacity(opacity)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                return Some(Box::new(block));
            }
            return Some(Box::new(paragraph));
        }
        // Fall through: inline root with no text and no inline pseudo images
    }

    None
}

/// Extract `LineFontMetrics` from a `ShapedLine`'s Text items using skrifa.
/// Returns per-line accurate metrics instead of reusing a single set from the
/// first glyph run in the paragraph. Falls back to defaults if no text items.
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
            // skrifa Metrics exposes x_height but not subscript/superscript
            // offsets directly. Approximate from ascent (same ratios as CSS
            // typographic conventions).
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

/// Recalculate line boxes for all lines in a paragraph, correctly handling
/// the coordinate system difference between paragraph-absolute baselines and
/// line-local coordinates expected by `recalculate_line_box`.
///
/// `recalculate_line_box` assumes `line.baseline` is line-local (i.e. relative
/// to the line's own top edge), but Parley sets baselines as paragraph-absolute
/// offsets. For the first line these coincide, but for subsequent lines the
/// baseline is offset by the cumulative height of preceding lines. This helper
/// converts to line-local before calling `recalculate_line_box`, then converts
/// back to paragraph-absolute and promotes `computed_y` to paragraph-absolute
/// so `draw_shaped_lines` can use `y + img.computed_y` directly (matching the
/// `y + line.baseline` pattern used for text).
///
/// Font metrics are extracted per-line from the line's own Text items via
/// skrifa, so lines with different font sizes get accurate vertical-align.
pub(super) fn recalculate_paragraph_line_boxes(lines: &mut [ShapedLine]) {
    // Track original and new cumulative heights separately.
    // Parley baselines are computed against original heights, so we use
    // original_y_acc for paragraph→line-local conversion. After expansion,
    // new_y_acc tracks the updated positions for line-local→paragraph.
    let mut original_y_acc: f32 = 0.0;
    let mut new_y_acc: f32 = 0.0;
    for line in lines.iter_mut() {
        let original_height = line.height;
        let font_metrics = metrics_from_line(line);
        // Convert baseline from paragraph-absolute to line-local
        // using original cumulative heights (what Parley computed against)
        line.baseline -= original_y_acc;
        crate::paragraph::recalculate_line_box(line, &font_metrics);
        // Convert computed_y from line-local to new paragraph-absolute
        for item in &mut line.items {
            if let LineItem::Image(img) = item {
                img.computed_y += new_y_acc;
            }
        }
        // Convert baseline to new paragraph-absolute
        line.baseline += new_y_acc;
        original_y_acc += original_height;
        new_y_acc += line.height;
    }
}

/// Walk up from `start_id` to find the closest `<a href>` ancestor and build
/// a `LinkSpan` describing its target. Returns `None` if no ancestor is an
/// anchor with a non-empty `href`.
///
/// Caller should memoize results per anchor node ID so multiple glyph runs
/// descended from the same `<a>` share one `Arc<LinkSpan>` (pointer identity,
/// required for later rect-dedup in PDF emission).
pub(super) fn resolve_enclosing_anchor(
    doc: &blitz_dom::BaseDocument,
    start_id: usize,
) -> Option<(usize, LinkSpan)> {
    let mut cur = Some(start_id);
    let mut depth: usize = 0;
    while let Some(id) = cur {
        // Defense-in-depth against pathological / malformed parent chains,
        // matching the bounds applied in `debug_print_tree`,
        // `collect_positioned_children`, and `blitz_adapter::element_text`.
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

/// Recursively convert the Blitz node referenced by a Parley `InlineBox.id`
/// and return the resulting `Pageable` as the inline-box content.
///
/// `InlineBoxContent` is `Box<dyn Pageable>`, so any wrapper chain emitted
/// by `convert_node` (Transform / StringSet / CounterOp / BookmarkMarker /
/// RunningElement) survives verbatim and its side effects apply around the
/// inner Block / Paragraph when the inline-box is drawn. `MAX_DOM_DEPTH`
/// is already enforced inside `convert_node`, so a depth-exhausted node
/// returning a `SpacerPageable` flows through as a zero-height content
/// rather than dropping the inline-box.
fn convert_inline_box_node(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> crate::paragraph::InlineBoxContent {
    // This function processes an inline box emitted by Parley during
    // paragraph layout. Per CSS, `position: absolute | fixed` elements are
    // out of normal flow and should never appear in Parley's inline
    // sequence, but Blitz currently routes absolutely-positioned pseudo
    // elements (`::before` / `::after`) through Parley's inline layout
    // anyway, which would paint them at `(0, 0)` of the surrounding flow.
    //
    // Suppress that rendering path ONLY for pseudos (detected by
    // `is_pseudo_node`), because `build_absolute_pseudo_children` re-emits
    // pseudos at the CSS-correct position by walking to the containing
    // block. It does NOT handle regular (non-pseudo) absolute children —
    // those have no re-emit path, so letting them fall through to
    // `convert_node` at least preserves their content (even if they end up
    // at Parley's inline position). Suppressing non-pseudos here would
    // silently drop them, which is worse.
    if let Some(node) = doc.get_node(node_id) {
        if positioned::is_absolutely_positioned(node) && is_pseudo_node(doc, node) {
            return Box::new(SpacerPageable::new(0.0));
        }
    }
    convert_node(doc, node_id, ctx, depth + 1)
}

/// Extract a ParagraphPageable from an inline root node.
pub(super) fn extract_paragraph(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Option<ParagraphPageable> {
    use crate::paragraph::InlineBoxItem;
    let elem_data = node.element_data()?;
    let text_layout = elem_data.inline_layout_data.as_ref()?;

    let parley_layout = &text_layout.layout;
    let text = &text_layout.text;

    let mut shaped_lines = Vec::new();
    // Running total of line heights seen so far (pt). Parley reports
    // `PositionedInlineBox.y` in paragraph-relative coordinates, but
    // `InlineBoxItem.computed_y` is expected to be line-relative; subtract
    // `accumulated_line_top` when building the item.
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
                    // Parley (wired through blitz at scale=1.0) reports font
                    // size in CSS px. The Pageable tree works in PDF pt, and
                    // krilla's `draw_glyphs` also wants pt. Convert here so
                    // every downstream computation (glyph advances,
                    // decoration widths, link rects) naturally lands in pt.
                    // `glyph.advance / font_size` stays a unitless ratio.
                    let font_size_parley = run.font_size();
                    let font_size = px_to_pt(font_size_parley);

                    // Get text color from the brush (node ID) → computed styles
                    let brush = &glyph_run.style().brush;
                    let color = get_text_color(doc, brush.id);
                    let decoration = get_text_decoration(doc, brush.id);
                    let link = ctx.link_cache.lookup(doc, brush.id);

                    // Extract raw glyphs (relative offsets, not absolute positions)
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
                        // Run-level x_offset is also in parley px; convert.
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
                    // Absolute/fixed pseudos are out of normal flow and must
                    // NOT reserve inline width or contribute to line metrics.
                    // Returning a `SpacerPageable` from
                    // `convert_inline_box_node` alone is insufficient because
                    // this branch would still push an `InlineBoxItem` built
                    // from Parley's `positioned.width` / `positioned.height`,
                    // which reserves space even when the content is blank.
                    // Skip the whole `items.push` for such pseudos — the
                    // containing block's converter re-emits them at their
                    // CSS-correct position via
                    // `build_absolute_pseudo_children`.
                    if let Some(box_node) = doc.get_node(node_id) {
                        if positioned::is_absolutely_positioned(box_node)
                            && is_pseudo_node(doc, box_node)
                        {
                            continue;
                        }
                    }
                    let content = convert_inline_box_node(doc, node_id, ctx, depth);
                    let link = ctx.link_cache.lookup(doc, node_id);
                    // Parley's `PositionedInlineBox` has no baseline field
                    // (Parley 0.6), so it defaults `y` so that the box's
                    // bottom edge sits on the surrounding text baseline
                    // (`y + height = surrounding_baseline`). CSS 2.1
                    // §10.8.1 instead wants the box's *inner* last-line
                    // baseline to coincide with the surrounding baseline.
                    // Shift the box down by `height - inner_baseline_offset`
                    // to realize that. When the box has no in-flow baseline
                    // (empty, overflow clipped, flex/grid without text),
                    // fall back to Parley's default — which is the CSS
                    // bottom-edge fallback described in the same clause.
                    let height_pt = px_to_pt(positioned.height);
                    let baseline_shift =
                        crate::paragraph::inline_box_baseline_offset(content.as_ref())
                            .map(|bo| height_pt - bo)
                            .unwrap_or(0.0);
                    let computed_y = px_to_pt(positioned.y) - accumulated_line_top + baseline_shift;
                    // Propagate `visibility: hidden` from the inner pageable
                    // (set by `extract_block_style`) so the inline-box is
                    // treated as invisible at draw time — link rect emission
                    // is then also suppressed by the `!ib.visible` guard in
                    // `draw_shaped_lines`. `Pageable::is_visible()` walks
                    // wrappers for us so a `visibility: hidden` inline-block
                    // keeps that state through a transform / marker chain.
                    let visible = content.is_visible();
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

    // Propagate the inline-root `id` so headings like `<h1 id="top">` that
    // end up as plain `ParagraphPageable` (no block wrapper triggered by the
    // default style) still register with `DestinationRegistry` for
    // `href="#top"` resolution.
    Some(ParagraphPageable::new(shaped_lines).with_id(extract_block_id(node)))
}
