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
    doc: &BaseDocument,
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
            let abs_pseudos = positioned::build_absolute_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            if style.needs_block_wrapper() || has_pseudo {
                let (child_x, child_y) = style.content_inset();
                // Propagate visibility to the inner paragraph — it's not a real CSS child
                // but the node's own text content, so it must respect the node's visibility.
                // Do NOT propagate opacity — the wrapping block handles it via push_opacity.
                // `node_id` is set so the inner paragraph's `slice_for_page`
                // can locate its own fragment (fulgur-frmj). The wrapping
                // `BlockPageable` shares the same `node_id` and detects the
                // shared case in `slice_for_page` to keep `pc.y` as the
                // content inset (padding + border) instead of collapsing to
                // `child_frag.y - self_frag.y == 0`.
                let mut p = paragraph;
                p.visible = visible;
                p.node_id = Some(node_id);
                let p_h = p.cached_height;
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(p),
                    x: child_x,
                    y: child_y,
                    height: p_h,
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
                    .with_style(style)
                    .with_opacity(opacity)
                    .with_visible(visible)
                    .with_id(extract_block_id(node))
                    .with_node_id(Some(node_id));
                // Use Taffy's computed height (includes padding + border) instead of children-only height
                block.layout_size = Some(Size { width, height });
                return Some(Box::new(block));
            }
            let mut p = paragraph;
            p.opacity = opacity;
            p.visible = visible;
            p.node_id = Some(node_id);
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
            paragraph.visible = visible;
            // `node_id` is always set on the inner paragraph so its
            // `slice_for_page` can locate its fragment. When wrapped in
            // a `BlockPageable` below, the block carries the same
            // `node_id`; `BlockPageable::slice_for_page` detects the
            // shared case and uses `pc.y` (content inset) instead of
            // collapsing to `child_frag.y - self_frag.y == 0`
            // (fulgur-frmj).
            paragraph.node_id = Some(node_id);

            // Check for block pseudo images too
            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            if style.needs_block_wrapper() || has_pseudo {
                let (child_x, child_y) = style.content_inset();
                let p_h = paragraph.cached_height;
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(paragraph),
                    x: child_x,
                    y: child_y,
                    height: p_h,
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
                    .with_style(style)
                    .with_opacity(opacity)
                    .with_visible(visible)
                    .with_id(extract_block_id(node))
                    .with_node_id(Some(node_id));
                block.layout_size = Some(Size { width, height });
                return Some(Box::new(block));
            }
            paragraph.opacity = opacity;
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
    doc: &BaseDocument,
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
    doc: &BaseDocument,
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
    doc: &BaseDocument,
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
    //
    // `node_id` is intentionally NOT set here. Callers set it on the
    // returned paragraph (whether it ends up as the outermost Pageable
    // or wrapped in a `BlockPageable`); the wrapping path also sets
    // `node_id` on the block so `BlockPageable::slice_for_page` can
    // detect the shared-node case and use `pc.y` (content inset)
    // instead of collapsing to `child_frag.y - self_frag.y == 0`
    // (fulgur-frmj).
    Some(ParagraphPageable::new(shaped_lines).with_id(extract_block_id(node)))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{find_tag, make_ctx};
    use crate::engine::Engine;
    use crate::pageable::{BlockPageable, Pageable, PositionedChild};
    use crate::paragraph::{LineItem, LinkTarget, ParagraphPageable};
    use std::ops::Deref;
    use std::sync::Arc;

    // ---- paragraph link tests ----

    #[test]
    fn paragraph_attaches_external_link_to_glyph_run_inside_anchor() {
        let html =
            r#"<html><body><p>Go to <a href="https://example.com">example</a>.</p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = super::extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        let mut found_external = false;
        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    if let Some(ls) = &run.link {
                        if let LinkTarget::External(u) = &ls.target {
                            if u.as_str() == "https://example.com" {
                                found_external = true;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            found_external,
            "expected at least one glyph run under <a> to carry an External link"
        );
    }

    #[test]
    fn paragraph_attaches_internal_link_for_fragment_href() {
        let html = r##"<html><body><p>See <a href="#intro">intro</a></p></body></html>"##;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = super::extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        let mut found = false;
        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    if let Some(ls) = &run.link {
                        if let LinkTarget::Internal(frag) = &ls.target {
                            if frag.as_str() == "intro" {
                                found = true;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            found,
            "expected fragment link to produce LinkTarget::Internal(\"intro\")"
        );
    }

    #[test]
    fn paragraph_shares_arc_linkspan_across_glyph_runs_under_same_anchor() {
        // <em> forces two separate glyph runs (different style) under one <a>.
        let html =
            r#"<html><body><p><a href="https://x.test"><em>foo</em> bar</a></p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = super::extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        let mut links: Vec<Arc<crate::paragraph::LinkSpan>> = Vec::new();
        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    if let Some(ls) = &run.link {
                        links.push(Arc::clone(ls));
                    }
                }
            }
        }
        assert!(
            links.len() >= 2,
            "expected at least two linked glyph runs (got {})",
            links.len()
        );
        let first = &links[0];
        for other in &links[1..] {
            assert!(
                Arc::ptr_eq(first, other),
                "all glyph runs inside the same <a> must share one Arc<LinkSpan>"
            );
        }
    }

    #[test]
    fn paragraph_leaves_link_none_for_anchor_without_href() {
        let html = r#"<html><body><p>Text <a>no href</a> here.</p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = super::extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    assert!(
                        run.link.is_none(),
                        "glyph runs under <a> without href must have link: None"
                    );
                }
            }
        }
    }

    #[test]
    fn paragraph_leaves_link_none_for_anchor_with_empty_href() {
        let html = r#"<html><body><p><a href="">empty</a></p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = super::extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    assert!(
                        run.link.is_none(),
                        "glyph runs under <a href=\"\"> must have link: None"
                    );
                }
            }
        }
    }

    #[test]
    fn paragraph_linkspan_alt_text_uses_anchor_text_content() {
        let html = r#"<html><body><p><a href="https://x.test">hello world</a></p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = super::extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        let mut alt: Option<String> = None;
        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    if let Some(ls) = &run.link {
                        alt = ls.alt_text.clone();
                    }
                }
            }
        }
        assert_eq!(alt.as_deref(), Some("hello world"));
    }

    // ---- inline-block extraction tests ----

    fn find_paragraph(root: &dyn Pageable) -> Option<&ParagraphPageable> {
        if let Some(p) = root.as_any().downcast_ref::<ParagraphPageable>() {
            return Some(p);
        }
        if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
            for PositionedChild { child, .. } in &block.children {
                if let Some(p) = find_paragraph(child.as_ref()) {
                    return Some(p);
                }
            }
        }
        None
    }

    fn build_tree(html: &str) -> Box<dyn Pageable> {
        Engine::builder()
            .build()
            .build_pageable_for_testing_no_gcpm(html)
    }

    #[test]
    fn inline_block_becomes_line_item_inline_box() {
        let html = r#"<!DOCTYPE html><html><body><p>before <span style="display:inline-block;width:40px;height:20px;background:red"></span> after</p></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");

        let found = para
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .find(|it| matches!(it, LineItem::InlineBox(_)));
        assert!(
            found.is_some(),
            "inline-block should appear as LineItem::InlineBox"
        );

        // Value assertions: the extracted InlineBox must carry the CSS
        // sizes (40px × 20px → 30pt × 15pt), be visible at full opacity,
        // and sit at a non-zero x offset because it comes after "before ".
        let ib = match found.unwrap() {
            LineItem::InlineBox(ib) => ib,
            _ => unreachable!(),
        };
        let expected_w = super::super::px_to_pt(40.0);
        let expected_h = super::super::px_to_pt(20.0);
        assert!(
            (ib.width - expected_w).abs() < 0.5,
            "width: expected ~{expected_w}pt, got {}pt",
            ib.width
        );
        assert!(
            (ib.height - expected_h).abs() < 0.5,
            "height: expected ~{expected_h}pt, got {}pt",
            ib.height
        );
        assert_eq!(ib.opacity, 1.0, "opacity should default to 1.0");
        assert!(ib.visible, "InlineBox should be visible by default");
        assert!(
            ib.x_offset > 0.0,
            "x_offset should be non-zero (text precedes the inline-block), got {}",
            ib.x_offset
        );
    }

    #[test]
    fn inline_block_with_block_child_has_block_content() {
        // Note: `<p>` cannot contain `<div>` in HTML5 (auto-closes). Use a
        // `<div>` inline root so the parser keeps the block-child shape.
        let html = r#"<!DOCTYPE html><html><body><div>text <span style="display:inline-block;width:40px;height:20px"><div>inner</div></span> more</div></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");

        // Locate the line containing the InlineBox and the box itself,
        // so we can also assert the line-relative `computed_y` invariant.
        let (line, ib) = para
            .lines
            .iter()
            .find_map(|l| {
                l.items.iter().find_map(|it| match it {
                    LineItem::InlineBox(ib) => Some((l, ib)),
                    _ => None,
                })
            })
            .expect("InlineBox expected");
        assert!(
            ib.content
                .as_any()
                .downcast_ref::<BlockPageable>()
                .is_some(),
            "inline-block content should surface as BlockPageable"
        );

        // `computed_y` is line-relative. It may be negative for
        // baseline-aligned inline-blocks: an empty inline-block has its
        // baseline at its bottom edge (CSS 2.1 §10.8), so a box taller
        // than the line's ascent legitimately extends above line-top.
        // The invariant we can assert without rejecting that case is
        // that the box overlaps the line box — bottom below line-top,
        // top above line-bottom. That still catches "paragraph-relative
        // leak" on a multi-line paragraph (y would push the box out of
        // the first line entirely) and unconverted Parley values.
        assert!(
            ib.computed_y + ib.height > 0.0 && ib.computed_y < line.height,
            "computed_y should place the box overlapping the line, got y={} h={} line.height={}",
            ib.computed_y,
            ib.height,
            line.height
        );
    }

    #[test]
    fn inline_block_with_transform_preserves_wrapper() {
        // Addresses the CodeRabbit "wrapper semantics drop" finding that
        // prompted the `Box<dyn Pageable>` refactor of `InlineBoxContent`:
        // an inline-block with a CSS `transform` is wrapped by `convert_node`
        // in `TransformWrapperPageable`, and now that wrapper survives at
        // the top of `ib.content` (previously it was peeled and the
        // transform effect lost).
        let html = r#"<!DOCTYPE html><html><body><div>text <span style="display:inline-block;transform:rotate(2deg);width:40px;height:20px;background:red">x</span> more</div></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");
        let ib = para
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .find_map(|it| match it {
                LineItem::InlineBox(ib) => Some(ib),
                _ => None,
            })
            .expect("inline-block should appear as LineItem::InlineBox");
        assert!(
            ib.content
                .as_any()
                .downcast_ref::<crate::pageable::TransformWrapperPageable>()
                .is_some(),
            "transform should survive as TransformWrapperPageable at the top \
             of the inline-box content"
        );
    }

    // ---- metrics_from_line tests ----

    /// Default fallback metrics returned by `metrics_from_line` when no
    /// Text item with parseable font data is present. Mirrors the literal
    /// values in the helper (line ~211).
    const FALLBACK_METRICS: (f32, f32, f32, f32, f32) = (12.0, 4.0, 8.0, 4.0, 6.0);

    #[test]
    fn metrics_from_line_falls_back_when_line_has_no_text_items() {
        use super::super::tests::sample_png_arc;
        use crate::image::ImageFormat;
        use crate::paragraph::{InlineImage, LineItem, ShapedLine, VerticalAlign};

        // Line with only an Image item — the helper's loop continues over
        // non-Text variants and falls through to the hardcoded defaults.
        let line = ShapedLine {
            height: 20.0,
            baseline: 16.0,
            items: vec![LineItem::Image(InlineImage {
                data: sample_png_arc(),
                format: ImageFormat::Png,
                width: 8.0,
                height: 8.0,
                x_offset: 0.0,
                vertical_align: VerticalAlign::Baseline,
                opacity: 1.0,
                visible: true,
                computed_y: 0.0,
                link: None,
            })],
        };
        let m = super::metrics_from_line(&line);
        let (a, d, x, sub, sup) = FALLBACK_METRICS;
        assert!((m.ascent - a).abs() < 0.01, "ascent: {}", m.ascent);
        assert!((m.descent - d).abs() < 0.01, "descent: {}", m.descent);
        assert!((m.x_height - x).abs() < 0.01, "x_height: {}", m.x_height);
        assert!(
            (m.subscript_offset - sub).abs() < 0.01,
            "subscript_offset: {}",
            m.subscript_offset,
        );
        assert!(
            (m.superscript_offset - sup).abs() < 0.01,
            "superscript_offset: {}",
            m.superscript_offset,
        );
    }

    #[test]
    fn metrics_from_line_returns_fallback_when_font_data_is_invalid() {
        use crate::paragraph::{LineItem, ShapedGlyph, ShapedGlyphRun, ShapedLine, TextDecoration};

        // Garbage bytes — `skrifa::FontRef::from_index` will return Err and
        // the helper falls through the loop to the default metrics.
        let bad_font = Arc::new(vec![0u8; 16]);
        let run = ShapedGlyphRun {
            font_data: bad_font,
            font_index: 0,
            font_size: 12.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![ShapedGlyph {
                id: 0,
                x_advance: 1.0,
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..1,
            }],
            text: "A".to_string(),
            x_offset: 0.0,
            link: None,
        };
        let line = ShapedLine {
            height: 20.0,
            baseline: 16.0,
            items: vec![LineItem::Text(run)],
        };
        let m = super::metrics_from_line(&line);
        let (a, d, x, sub, sup) = FALLBACK_METRICS;
        assert!(
            (m.ascent - a).abs() < 0.01,
            "garbage font_data must hit fallback ascent, got {}",
            m.ascent,
        );
        assert!((m.descent - d).abs() < 0.01);
        assert!((m.x_height - x).abs() < 0.01);
        assert!((m.subscript_offset - sub).abs() < 0.01);
        assert!((m.superscript_offset - sup).abs() < 0.01);
    }

    #[test]
    fn metrics_from_line_picks_first_text_item_in_mixed_line() {
        use super::super::tests::sample_png_arc;
        use crate::image::ImageFormat;
        use crate::paragraph::{InlineImage, LineItem, ShapedLine, VerticalAlign};

        // Round-trip a real document through `extract_paragraph` to obtain a
        // genuine font_data that skrifa can parse. Then build a fresh line
        // whose items are: Image, the recovered Text run, InlineBox-skipped
        // (we only need Image then Text to prove "first Text wins").
        let html = r#"<html><body><p>hello</p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = super::extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        // Pull a real ShapedGlyphRun out of the rendered paragraph.
        let real_run = para
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .find_map(|it| match it {
                LineItem::Text(r) => Some(r.clone()),
                _ => None,
            })
            .expect("at least one Text item in <p>hello</p>");

        // Construct a fresh line with: an Image first (must be skipped),
        // then the real Text run (must be selected).
        let img = InlineImage {
            data: sample_png_arc(),
            format: ImageFormat::Png,
            width: 8.0,
            height: 8.0,
            x_offset: 0.0,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: 0.0,
            link: None,
        };
        let line = ShapedLine {
            height: 20.0,
            baseline: 16.0,
            items: vec![LineItem::Image(img), LineItem::Text(real_run)],
        };

        let m = super::metrics_from_line(&line);
        // Anything other than the literal fallback proves we picked up the
        // real font (skrifa-derived metrics for a typical text font are not
        // 12/4/8/4/6 by coincidence).
        let (a, d, x, sub, sup) = FALLBACK_METRICS;
        let is_fallback = (m.ascent - a).abs() < 0.01
            && (m.descent - d).abs() < 0.01
            && (m.x_height - x).abs() < 0.01
            && (m.subscript_offset - sub).abs() < 0.01
            && (m.superscript_offset - sup).abs() < 0.01;
        assert!(
            !is_fallback,
            "expected real-font metrics for a parseable Text item, got fallback: \
             ascent={}, descent={}, x_height={}, sub={}, sup={}",
            m.ascent, m.descent, m.x_height, m.subscript_offset, m.superscript_offset,
        );
        // Sanity: skrifa derives subscript_offset = ascent * 0.3 and
        // superscript_offset = ascent * 0.4 (see helper body).
        assert!(
            (m.subscript_offset - m.ascent * 0.3).abs() < 0.01,
            "subscript_offset should be ascent*0.3, got sub={} ascent={}",
            m.subscript_offset,
            m.ascent,
        );
        assert!(
            (m.superscript_offset - m.ascent * 0.4).abs() < 0.01,
            "superscript_offset should be ascent*0.4, got sup={} ascent={}",
            m.superscript_offset,
            m.ascent,
        );
    }

    // ---- recalculate_paragraph_line_boxes tests ----

    #[test]
    fn recalculate_paragraph_line_boxes_empty_slice_is_noop() {
        // Just verify the helper accepts an empty slice without panicking.
        let mut lines: Vec<crate::paragraph::ShapedLine> = Vec::new();
        super::recalculate_paragraph_line_boxes(&mut lines);
        assert!(lines.is_empty());
    }

    #[test]
    fn recalculate_paragraph_line_boxes_rebases_multiple_lines() {
        use crate::paragraph::ShapedLine;
        // Three lines, each height 20.0 with paragraph-absolute baselines
        // 16.0 / 36.0 / 56.0 (i.e. line N's baseline = 16.0 + 20*N).
        // With items: vec![] the helper falls back to default metrics, and
        // `recalculate_line_box` over an empty `items` is the identity on
        // (height, baseline). So after the helper round-trips through
        // line-local and back, every baseline must be **the same** as the
        // input — the function only converts coordinate system, doesn't
        // mutate the layout when there are no images.
        let mut lines = vec![
            ShapedLine {
                height: 20.0,
                baseline: 16.0,
                items: vec![],
            },
            ShapedLine {
                height: 20.0,
                baseline: 36.0,
                items: vec![],
            },
            ShapedLine {
                height: 20.0,
                baseline: 56.0,
                items: vec![],
            },
        ];
        super::recalculate_paragraph_line_boxes(&mut lines);
        assert!((lines[0].baseline - 16.0).abs() < 0.01, "line 0 baseline");
        assert!((lines[1].baseline - 36.0).abs() < 0.01, "line 1 baseline");
        assert!((lines[2].baseline - 56.0).abs() < 0.01, "line 2 baseline");
        // Heights are unchanged when there are no inline images.
        for (i, l) in lines.iter().enumerate() {
            assert!(
                (l.height - 20.0).abs() < 0.01,
                "line {} height changed: {}",
                i,
                l.height,
            );
        }
    }

    #[test]
    fn recalculate_paragraph_line_boxes_handles_image_computed_y_rebase() {
        use super::super::tests::sample_png_arc;
        use crate::image::ImageFormat;
        use crate::paragraph::{InlineImage, LineItem, ShapedLine, VerticalAlign};

        // Build two identical lines, each carrying a single inline image.
        // Both lines have height 20.0 and baseline 16.0 (line-local), and
        // the image is small enough that `recalculate_line_box` does NOT
        // expand the line box. With identical layouts both lines compute
        // the SAME line-local img_top in `recalculate_line_box`. The helper
        // then promotes line 1's `computed_y` by `new_y_acc` (= line 0's
        // post-expansion height = 20.0) so it lands paragraph-absolute.
        //
        // Key invariant: `lines[1].computed_y - lines[0].computed_y` must
        // equal `lines[0].height` after the helper runs. That's the
        // line-local → paragraph-absolute rebase contract.
        fn make_image() -> InlineImage {
            InlineImage {
                data: sample_png_arc(),
                format: ImageFormat::Png,
                width: 8.0,
                height: 8.0,
                x_offset: 0.0,
                vertical_align: VerticalAlign::Baseline,
                opacity: 1.0,
                visible: true,
                computed_y: 0.0,
                link: None,
            }
        }
        let mut lines = vec![
            ShapedLine {
                height: 20.0,
                baseline: 16.0,
                items: vec![LineItem::Image(make_image())],
            },
            // Line 1 baseline is paragraph-absolute (= 16.0 + 20.0).
            ShapedLine {
                height: 20.0,
                baseline: 36.0,
                items: vec![LineItem::Image(make_image())],
            },
        ];
        super::recalculate_paragraph_line_boxes(&mut lines);

        let cy0 = match &lines[0].items[0] {
            LineItem::Image(img) => img.computed_y,
            _ => panic!("line 0 image"),
        };
        let cy1 = match &lines[1].items[0] {
            LineItem::Image(img) => img.computed_y,
            _ => panic!("line 1 image"),
        };
        // Expected gap is line 0's expanded height; for an 8pt baseline
        // image and 16/4/8 fallback metrics the line box is unchanged
        // (img_top = 16 - 8 = 8 lies inside [0, 20]), so height stays 20.
        let line0_height_after = lines[0].height;
        assert!(
            (line0_height_after - 20.0).abs() < 0.01,
            "line 0 height should not expand for an 8pt baseline image, got {}",
            line0_height_after,
        );
        let gap = cy1 - cy0;
        assert!(
            (gap - line0_height_after).abs() < 0.01,
            "line 1 image should be rebased by line 0's height (expected gap ~{}, got {})",
            line0_height_after,
            gap,
        );
        // Sanity: line 0's image is line-local, so its computed_y must lie
        // within [0, line.height]. (For Baseline align: cy0 = baseline - h
        // = 16 - 8 = 8, then plus shift=0.)
        assert!(
            cy0 >= 0.0 && cy0 <= lines[0].height,
            "line 0 computed_y should be line-local in [0, {}], got {}",
            lines[0].height,
            cy0,
        );
    }

    // ---- resolve_enclosing_anchor tests ----

    #[test]
    fn resolve_enclosing_anchor_returns_none_when_no_ancestor_anchor() {
        let html = r#"<html><body><p>plain text</p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let p_id = find_tag(&doc, "p").expect("p exists");
        // Walk to <p>'s first child (the text node) to be sure no ancestor
        // chain hits an <a>.
        let text_id = doc
            .get_node(p_id)
            .and_then(|n| n.children.first().copied())
            .expect("text child of <p>");

        let result = super::resolve_enclosing_anchor(doc.deref(), text_id);
        assert!(
            result.is_none(),
            "no <a> ancestor should yield None, got {:?}",
            result.map(|(id, _)| id),
        );
    }

    #[test]
    fn resolve_enclosing_anchor_returns_external_for_https_href() {
        let html = r#"<html><body><p>see <a href="https://example.com">here</a></p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let a_id = find_tag(&doc, "a").expect("a exists");
        // Start the walk from the text node *inside* the anchor so the helper
        // has to walk up at least one level to find the enclosing <a>.
        let inner_text_id = doc
            .get_node(a_id)
            .and_then(|n| n.children.first().copied())
            .expect("text child of <a>");

        let (anchor_id, span) =
            super::resolve_enclosing_anchor(doc.deref(), inner_text_id).expect("anchor resolved");
        assert_eq!(anchor_id, a_id, "returned id should be the <a> element");
        match &span.target {
            LinkTarget::External(arc) => {
                assert_eq!(arc.as_str(), "https://example.com");
            }
            other => panic!("expected External link target, got {:?}", other),
        }
        assert_eq!(span.alt_text.as_deref(), Some("here"));
    }

    #[test]
    fn resolve_enclosing_anchor_returns_internal_for_fragment_href() {
        let html = r##"<html><body><p>see <a href="#sec1">section</a></p></body></html>"##;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let a_id = find_tag(&doc, "a").expect("a exists");
        let inner_text_id = doc
            .get_node(a_id)
            .and_then(|n| n.children.first().copied())
            .expect("text child of <a>");

        let (anchor_id, span) = super::resolve_enclosing_anchor(doc.deref(), inner_text_id)
            .expect("internal anchor resolved");
        assert_eq!(anchor_id, a_id);
        match &span.target {
            LinkTarget::Internal(arc) => {
                // The leading '#' must be stripped per the helper's contract.
                assert_eq!(arc.as_str(), "sec1");
            }
            other => panic!("expected Internal link target, got {:?}", other),
        }
        assert_eq!(span.alt_text.as_deref(), Some("section"));
    }

    #[test]
    fn resolve_enclosing_anchor_returns_none_for_empty_href() {
        let html = r#"<html><body><p><a href="">x</a></p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let a_id = find_tag(&doc, "a").expect("a exists");
        let inner_text_id = doc
            .get_node(a_id)
            .and_then(|n| n.children.first().copied())
            .expect("text child of <a>");

        let result = super::resolve_enclosing_anchor(doc.deref(), inner_text_id);
        assert!(
            result.is_none(),
            "empty href should yield None, got {:?}",
            result.map(|(id, _)| id),
        );
    }

    #[test]
    fn inline_block_baseline_aligns_with_surrounding_text() {
        // An inline-block with text "boxed" inside, surrounded by "before" /
        // "after" text. Per CSS 2.1 §10.8.1, the inline-block's baseline is
        // the baseline of its last inner line, which should coincide with
        // the baseline of the surrounding text line.
        let html = r#"<!DOCTYPE html><html><body><div>before <span style="display:inline-block;padding:6px 10px;border:2px solid #333;background:#def">boxed</span> after</div></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");

        // Locate the inline-box and the line it sits on.
        let (ib, line) = para
            .lines
            .iter()
            .find_map(|l| {
                l.items.iter().find_map(|it| match it {
                    LineItem::InlineBox(ib) => Some((ib, l)),
                    _ => None,
                })
            })
            .expect("InlineBox expected");

        // Compute the inner baseline of the inline-box (offset from its
        // top edge).
        let inner_baseline = crate::paragraph::inline_box_baseline_offset(ib.content.as_ref())
            .expect("inline-box with visible text should have an inner baseline");

        // Fixture places the inline-box on the first (and only) line, so
        // `line_top = 0`. `ib.computed_y` is line-relative, and
        // `line.baseline` is paragraph-relative; with `line_top = 0` they
        // share the same origin, so we can compare directly.
        let line_top = 0.0_f32;
        let box_inner_baseline_abs = line_top + ib.computed_y + inner_baseline;
        let expected = line.baseline;
        let delta = (box_inner_baseline_abs - expected).abs();
        assert!(
            delta < 0.5,
            "inline-block inner baseline {} should align with surrounding line baseline {} (delta={})",
            box_inner_baseline_abs,
            expected,
            delta
        );
    }
}
