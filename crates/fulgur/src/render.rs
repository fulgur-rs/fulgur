use crate::config::Config;
use crate::drawables::Drawables;
use crate::error::{Error, Result};
use crate::gcpm::GcpmContext;
use crate::gcpm::counter::resolve_content_to_html;
use crate::gcpm::margin_box::{Edge, MarginBoxPosition, MarginBoxRect, compute_edge_layout};
use crate::gcpm::running::RunningElementStore;
use crate::pageable::{Canvas, Pageable};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Phase 4 PR 1 (fulgur-9t3z): geometry-driven render skeleton.
///
/// Walks `geometry` per page, dispatches each (node_id, fragment) to
/// per-type draw functions sourced from `drawables`. PR 1 emits blank
/// pages because every map in `Drawables` is empty; subsequent PRs
/// migrate one Pageable type at a time and the dispatcher grows
/// match arms.
///
/// Page settings (size, margins, landscape, GCPM `@page` overrides)
/// resolve identically to the v1 path so byte equality is achievable
/// once the draw migration completes.
#[allow(clippy::too_many_arguments)]
pub fn render_v2(
    config: &Config,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
    gcpm: &GcpmContext,
    running_store: &RunningElementStore,
    font_data: &[Arc<Vec<u8>>],
    string_set_by_node: &HashMap<usize, Vec<(String, String)>>,
    counter_ops_by_node: &BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
) -> Result<Vec<u8>> {
    let mut document = krilla::Document::new();

    let mut bookmark_collector = if config.bookmarks {
        Some(crate::pageable::BookmarkCollector::new())
    } else {
        None
    };

    let page_count = crate::pagination_layout::implied_page_count(geometry).max(1) as usize;

    // Pre-pass: register `id` anchors for `href="#..."` resolution.
    // PR 3 records paragraph ids; PR 4 adds block ids. List-item ids
    // arrive in PR 5. A node may appear in both `paragraphs` and
    // `block_styles` (shared node_id case — see `convert::replaced` /
    // `convert::inline_root`); paragraph wins so the chain mirrors the
    // priority v1 establishes via the Pageable tree walk.
    let mut dest_registry = crate::pageable::DestinationRegistry::new();
    for (&node_id, geom) in geometry {
        let Some(first_frag) = geom.fragments.first() else {
            continue;
        };
        let para_id = drawables
            .paragraphs
            .get(&node_id)
            .and_then(|p| p.id.as_ref());
        let block_id = drawables
            .block_styles
            .get(&node_id)
            .and_then(|b| b.id.as_ref());
        let table_id = drawables.tables.get(&node_id).and_then(|t| t.id.as_ref());
        let id = para_id.or(block_id).or(table_id);
        if let Some(id) = id
            && !id.is_empty()
        {
            let page_idx = first_frag.page_index as usize;
            dest_registry.set_current_page(page_idx);
            // The fragment is in body content-area-relative CSS px;
            // resolve the page-specific margin so destination y_pt is
            // page-absolute (matches v1's `collect_ids` semantics).
            let page_num = page_idx + 1;
            let (_resolved_size, resolved_margin, _resolved_landscape) =
                crate::gcpm::page_settings::resolve_page_settings(
                    &gcpm.page_settings,
                    page_num,
                    page_count,
                    config,
                );
            // `frag.x` is html-relative (already includes body's x
            // offset from the fragmenter); only y needs `body_offset_pt`
            // applied because fragments are body-content-area-relative
            // along the y axis.
            let x_pt = resolved_margin.left + crate::convert::px_to_pt(first_frag.x);
            let y_pt = resolved_margin.top
                + drawables.body_offset_pt.1
                + crate::convert::px_to_pt(first_frag.y);
            dest_registry.record(id.as_str(), x_pt, y_pt);
        }
    }

    let mut link_collector = crate::pageable::LinkCollector::new();

    // Build the GCPM margin-box renderer once. Reused across pages so
    // measure / layout / render caches survive between pages and the
    // pre-computed `string_set_states` / `counter_states` /
    // `running_states` are paid for once.
    let mut margin_box_renderer = MarginBoxRenderer::new(
        gcpm,
        running_store,
        font_data,
        geometry,
        string_set_by_node,
        counter_ops_by_node,
        page_count,
    );

    for page_idx in 0..page_count {
        let page_num = page_idx + 1;
        // Pass the full `gcpm.page_settings` (including selector
        // rules: `:first`, `:left`, `:right`) so per-page overrides
        // fire identically to the v1 GCPM path.
        let (resolved_size, resolved_margin, resolved_landscape) =
            crate::gcpm::page_settings::resolve_page_settings(
                &gcpm.page_settings,
                page_num,
                page_count,
                config,
            );
        let page_size = if resolved_landscape {
            resolved_size.landscape()
        } else {
            resolved_size
        };
        let settings = krilla::page::PageSettings::from_wh(page_size.width, page_size.height)
            .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;
        let mut page = document.start_page_with(settings);
        if let Some(c) = bookmark_collector.as_mut() {
            c.set_current_page(page_idx);
        }
        link_collector.set_current_page(page_idx);
        {
            let mut surface = page.surface();
            // Margin box pre-pass first (mirrors v1's
            // `render_to_pdf_with_gcpm` per-page ordering — header /
            // footer / corner content paint before body content). v1
            // gives margin boxes their own collector-less canvas
            // because running elements promoted into a margin box
            // shouldn't re-record bookmarks every page; we mirror that
            // here.
            {
                let mut margin_canvas = crate::pageable::Canvas {
                    surface: &mut surface,
                    bookmark_collector: None,
                    link_collector: None,
                };
                let page_content_width =
                    page_size.width - resolved_margin.left - resolved_margin.right;
                margin_box_renderer.render_page(
                    &mut margin_canvas,
                    page_idx,
                    page_num,
                    page_count,
                    page_size,
                    resolved_margin,
                    page_content_width,
                );
            }
            let mut canvas = crate::pageable::Canvas {
                surface: &mut surface,
                bookmark_collector: bookmark_collector.as_mut(),
                link_collector: Some(&mut link_collector),
            };
            // Root `<html>` + `<body>` background pre-pass. v1's
            // `BlockPageable::draw` for these elements paints
            // bg/border/shadow on EVERY page because each page's
            // sliced root pageable still calls them. v2's main
            // dispatch sees them via the fragmenter's single fragment
            // on page 0 only — multi-page docs would lose those fills
            // on continuation pages. Paint each here at its own offset
            // (`(margin.left, margin.top)` for html, plus
            // `body_offset_pt` for body) using `layout_size` from the
            // entry — mirrors v1's
            // `total_width = self.layout_size.or(cached_size)...`
            // derivation. The main dispatch loop skips both `root_id`
            // and `body_id` to avoid double-painting on page 0.
            if let Some(root_id) = drawables.root_id
                && let Some(root_block) = drawables.block_styles.get(&root_id)
            {
                paint_root_block_v2(
                    &mut canvas,
                    root_block,
                    resolved_margin.left,
                    resolved_margin.top,
                );
            }
            // body's bg pre-pass runs on continuation pages only.
            // Page 0 already paints body via the main dispatch loop
            // (the fragmenter records body's fragment on page 0, with
            // `layout_size` covering body's full height — clipped to
            // page area at PDF render time, mirroring v1 exactly).
            // Without this guard, page 0 would double-paint body's bg.
            if page_idx > 0
                && let Some(body_id) = drawables.body_id
                && let Some(body_block) = drawables.block_styles.get(&body_id)
            {
                paint_root_block_v2(
                    &mut canvas,
                    body_block,
                    resolved_margin.left + drawables.body_offset_pt.0,
                    resolved_margin.top + drawables.body_offset_pt.1,
                );
            }
            // `frag.x` is html-relative (fragmenter folds body's x
            // offset in); `frag.y` is body-content-area-relative — so
            // only y receives `body_offset_pt`.
            draw_v2_page(
                &mut canvas,
                page_idx as u32,
                resolved_margin.left,
                resolved_margin.top + drawables.body_offset_pt.1,
                geometry,
                drawables,
            );
        }
        let per_page = link_collector.take_page(page_idx);
        crate::link::emit_link_annotations(&mut page, &per_page, &dest_registry);
    }

    if let Some(c) = bookmark_collector {
        let entries = c.into_entries();
        if !entries.is_empty() {
            document.set_outline(crate::outline::build_outline(&entries));
        }
    }

    document.set_metadata(build_metadata(config));
    document
        .finish()
        .map_err(|e| Error::PdfGeneration(format!("{e:?}")))
}

/// Phase 4 v2 per-page draw dispatcher. Walks every `(node_id,
/// fragment)` pair whose fragment is on `page_index` and routes each
/// to a per-type draw function sourced from `drawables`.
///
/// Iteration is by `BTreeMap<NodeId, _>` order which is approximately
/// document order (Blitz allocates NodeIds during parse). That keeps
/// stacking order — backgrounds before foregrounds, parents before
/// children — consistent with the v1 traversal.
///
/// PR 2 covers `Drawables.images`, `.svgs`, and `.bookmark_anchors`
/// (first-fragment-only). Subsequent PRs add match arms for the
/// other maps.
fn draw_v2_page(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    page_index: u32,
    margin_left_pt: f32,
    margin_top_pt: f32,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
) {
    use crate::convert::px_to_pt;

    // Pre-compute the set of node_ids that fall under any transform's
    // `descendants` list. They are skipped in the main iteration and
    // drawn instead inside the transform's `push_transform / pop` group
    // below — mirroring v1's `TransformWrapperPageable::draw` which
    // calls `inner.draw(...)` while the surface transform is active.
    let mut transformed_descendants: std::collections::BTreeSet<usize> =
        std::collections::BTreeSet::new();
    for tx in drawables.transforms.values() {
        transformed_descendants.extend(tx.descendants.iter().copied());
    }
    // Same shape as `transformed_descendants` but keyed by an ancestor
    // block whose `style.has_overflow_clip()` is true. Strict
    // descendants paint inside the clip's `push_clip_path / pop` group;
    // skipping them here prevents the main loop from also dispatching
    // them outside the clip.
    //
    // Body is excluded from this collection: the fragmenter records
    // body with exactly one fragment at `page_index = 0`
    // (`pagination_layout.rs:380-384`), so `draw_under_clip(body)`
    // only fires on page 0. If we kept body in `clipped_descendants`,
    // every descendant would still be skipped via the
    // `clipped_descendants.contains(&node_id)` guard on page 1+ but
    // nobody would dispatch them — silently blanking all content
    // after page 1 on `<body style="overflow:hidden|auto|scroll">`
    // (PR #310 follow-up Devin). Skipping body here means body-level
    // overflow clip is not applied in v2, matching the pre-PR
    // behavior; body clipping in a paged context is unusual and the
    // pre-pass at `paint_root_block_v2` already handles body's own
    // bg / border on continuation pages.
    let mut clipped_descendants: std::collections::BTreeSet<usize> =
        std::collections::BTreeSet::new();
    for (&node_id, block) in &drawables.block_styles {
        // Exclude body AND root: both are skipped by the main
        // dispatch loop (body's only fragment lives on page 0, root
        // is never recorded in `geometry` and is painted via
        // `paint_root_block_v2`). Including either would silently
        // blank descendants on pages 1+ because `draw_under_clip`
        // never fires for them but the skip set still hides their
        // descendants from the regular per-fragment dispatch.
        // (PR #312 follow-up Devin: root exclusion symmetry.)
        if block.style.has_overflow_clip()
            && Some(node_id) != drawables.body_id
            && Some(node_id) != drawables.root_id
        {
            clipped_descendants.extend(block.clip_descendants.iter().copied());
        }
    }
    // Tables with `overflow: hidden | clip` clip their cells to the
    // padding box. Mirror the block-clip skip set so the main loop
    // doesn't dispatch cell descendants outside the table's clip
    // scope — `draw_under_clip_table` paints them inside the clip.
    // Unlike body/root, tables are always proper geometry-recorded
    // nodes so no special exclusion is needed.
    for table in drawables.tables.values() {
        if table.style.has_overflow_clip() && !table.clip_descendants.is_empty() {
            clipped_descendants.extend(table.clip_descendants.iter().copied());
        }
    }
    // Mirrors `clipped_descendants` for blocks that wrap their
    // descendants in a `draw_with_opacity` group (fractional opacity,
    // no clip — see `BlockEntry.opacity_descendants` and
    // `draw_under_opacity`). Without skipping these in the main loop
    // they'd be dispatched twice: once at full opacity here, once
    // again under the parent's opacity wrap.
    //
    // Body and root are excluded for the same reason
    // `clipped_descendants` excludes them: the fragmenter records
    // body with exactly one fragment at `page_index = 0` (so
    // `draw_under_opacity(body)` only fires on page 0), and root
    // is never recorded in `geometry` at all (it's painted via
    // the `paint_root_block_v2` pre-pass). Keeping either's
    // descendants in this set would silently blank all content on
    // pages 1+ because descendants get skipped by the guard but
    // no-one dispatches them — `html { opacity: 0.5 }` would
    // silently blank the whole document, and `body { opacity:
    // 0.5 }` would silently blank pages 1+. (PR #314 + PR #312
    // follow-up Devin Reviews.)
    let mut opacity_wrapped_descendants: std::collections::BTreeSet<usize> =
        std::collections::BTreeSet::new();
    for (&node_id, block) in &drawables.block_styles {
        if !block.opacity_descendants.is_empty()
            && Some(node_id) != drawables.body_id
            && Some(node_id) != drawables.root_id
        {
            opacity_wrapped_descendants.extend(block.opacity_descendants.iter().copied());
        }
    }

    for (&node_id, geom) in geometry {
        // Bookmark anchor: emit on the page where the node's *first*
        // fragment lands, mirroring `BookmarkMarkerWrapperPageable`'s
        // `is_first_page_for` slice semantics. Run BEFORE the
        // `transformed_descendants` skip so headings nested inside a
        // transformed ancestor (e.g. `<div style="transform:..."><h1>`)
        // still register in the PDF outline. v1 invokes
        // `BookmarkMarkerWrapperPageable::draw` recursively from inside
        // `TransformWrapperPageable::draw`, so the bookmark is recorded
        // regardless of transform membership; we mirror that by
        // unconditionally calling `record` here using the untransformed
        // y position. (v1 emits at the same untransformed y for the
        // outline destination — `collect_ids` does push the transform
        // for `/Link` rects but the bookmark itself is keyed by raw y.)
        if let Some(first_frag) = geom.fragments.first()
            && first_frag.page_index == page_index
            && let Some(anchor) = drawables.bookmark_anchors.get(&node_id)
            && let Some(c) = canvas.bookmark_collector.as_deref_mut()
        {
            let y_pt = margin_top_pt + px_to_pt(first_frag.y);
            c.record(anchor.level, anchor.label.clone(), y_pt);
        }

        if transformed_descendants.contains(&node_id) {
            // Drawn inside an ancestor transform group elsewhere in
            // this loop. Skipping prevents double-painting. Bookmark
            // anchor recording above already ran unconditionally.
            continue;
        }
        if clipped_descendants.contains(&node_id) {
            // Drawn inside an ancestor `overflow: hidden|clip` block's
            // `push_clip_path / pop` group elsewhere in this loop.
            continue;
        }
        if opacity_wrapped_descendants.contains(&node_id) {
            // Drawn inside an ancestor `draw_with_opacity` group via
            // `draw_under_opacity` elsewhere in this loop. Mirrors the
            // `clipped_descendants` skip — without it, the descendant
            // paints once at full opacity here and once under the
            // parent's opacity wrap. (fulgur-gdb9)
            continue;
        }
        // Skip the html root: its bg / border / shadow are painted
        // per-page in the pre-pass (`paint_root_block_v2`) above.
        // Body intentionally is NOT skipped here — page 0 needs the
        // main dispatch to paint body normally (so inline-root
        // children at body's node_id keep flowing through
        // `draw_block_with_inner_content`); page 1+ relies on the
        // body branch of the pre-pass.
        if Some(node_id) == drawables.root_id {
            continue;
        }

        // Per-fragment leaf draws.
        for frag in &geom.fragments {
            if frag.page_index != page_index {
                continue;
            }
            let x_pt = margin_left_pt + px_to_pt(frag.x);
            let y_pt = margin_top_pt + px_to_pt(frag.y);

            if let Some(tx) = drawables.transforms.get(&node_id) {
                draw_under_transform(
                    canvas,
                    tx,
                    node_id,
                    geom,
                    frag,
                    x_pt,
                    y_pt,
                    geometry,
                    drawables,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
                continue;
            }
            // `overflow: hidden | clip` block: bg / border / shadow
            // paint OUTSIDE the clip (matching v1's
            // `BlockPageable::draw` ordering at
            // `pageable.rs:1796-1827`), then push the clip path,
            // dispatch self's inner content + every strict descendant
            // INSIDE the clip, then pop. Same shape as
            // `draw_under_transform` but with `push_clip_path`.
            //
            // No `!clip_descendants.is_empty()` guard: shared-node_id
            // inner content (inline-root paragraph from
            // `convert::inline_root`, replaced image / svg from
            // `convert::replaced`) lands at the same `node_id` as the
            // wrapper and so produces an empty `clip_descendants`. v1
            // pushes the clip unconditionally when
            // `has_overflow_clip()` is true (`pageable.rs:1808-1826`),
            // so a `<div style="overflow:hidden;width:50px">long
            // text</div>` still needs the text clipped at the 50px
            // box even with no separate descendant NodeIds.
            // Body is intentionally excluded from `draw_under_clip`:
            // body has only a page-0 fragment so the clip would only
            // wrap page-0 content, but body's `clip_descendants`
            // include every block in the document. Descendants on
            // page 1+ are dispatched by the main loop via
            // `dispatch_fragment` (they're omitted from
            // `clipped_descendants` above). Without this skip, body's
            // page-0 clip would also re-dispatch every descendant
            // already painted by the main loop, causing a double
            // paint. See the `clipped_descendants` collection block
            // for the rest of the body-overflow rationale.
            if let Some(block) = drawables.block_styles.get(&node_id)
                && block.style.has_overflow_clip()
                && Some(node_id) != drawables.body_id
            {
                draw_under_clip(
                    canvas,
                    block,
                    node_id,
                    geom,
                    frag,
                    x_pt,
                    y_pt,
                    geometry,
                    drawables,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
                continue;
            }
            // Table with `overflow: hidden | clip`: same shape as the
            // block clip arm above. v1's `TablePageable::draw` mirrors
            // `BlockPageable::draw` and pushes a clip path around its
            // cell paint when `has_overflow_clip()` is true; v2 routes
            // through `draw_under_clip_table` which paints the outer
            // frame outside the clip and dispatches each cell descendant
            // inside.
            if let Some(table) = drawables
                .tables
                .get(&node_id)
                .filter(|t| t.style.has_overflow_clip())
            {
                draw_under_clip_table(
                    canvas,
                    table,
                    geom,
                    frag,
                    x_pt,
                    y_pt,
                    geometry,
                    drawables,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
                continue;
            }
            // Fractional-opacity block with descendants: wrap own
            // paint + descendant fragments in a single
            // `draw_with_opacity` group. Mirrors v1's
            // `BlockPageable::draw` recursion under
            // `draw_with_opacity(self.opacity, ..)`. Without this,
            // `<div opacity:0.4><svg>..</svg></div>` paints the svg
            // outside the parent's opacity wrap, dropping the parent's
            // opacity from the svg. (fulgur-gdb9)
            //
            // Body is excluded for the same reason as the clip arm
            // above: body has only a page-0 fragment so a
            // `draw_under_opacity(body)` would only wrap page-0
            // content while the `opacity_wrapped_descendants` skip
            // (collected above) excludes body explicitly so the main
            // loop dispatches body's descendants on pages 1+
            // normally. Without this exclusion `body { opacity: 0.5 }`
            // would silently blank pages 1+. (PR #314 follow-up Devin
            // Review)
            if let Some(block) = drawables
                .block_styles
                .get(&node_id)
                .filter(|b| !b.opacity_descendants.is_empty())
                .filter(|_| Some(node_id) != drawables.body_id)
            {
                draw_under_opacity(
                    canvas,
                    block,
                    node_id,
                    geom,
                    frag,
                    x_pt,
                    y_pt,
                    geometry,
                    drawables,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
                continue;
            }
            dispatch_fragment(
                canvas, node_id, geom, frag, x_pt, y_pt, drawables, page_index,
            );
        }
    }

    // Post-pass: paint multicol column rules between columns. v1's
    // `MulticolRulePageable::draw` runs this AFTER `child.draw(...)`
    // so the rule lines paint on top of the column contents. The
    // post-pass placement mirrors that ordering — every per-NodeId
    // payload is already drawn by the main loop above.
    //
    // Skip multicol containers that live inside a transform (whether
    // they ARE a transform key or are a descendant of one): in v1
    // `MulticolRulePageable::draw` runs from inside
    // `TransformWrapperPageable::draw`'s `push_transform / pop` group,
    // so the rule lines paint under the composed matrix. The transform
    // version is dispatched from `draw_under_transform`'s tail
    // (`paint_transform_scoped_multicol_rules`) so the composed
    // transform stays active. Painting them here unconditionally would
    // emit the rules twice — once in page coords (wrong) and once
    // inside the transform (correct) — and visually misalign the
    // page-coord copy. (PR #305 follow-up Devin)
    for (&container_id, entry) in &drawables.multicol_rules {
        if transformed_descendants.contains(&container_id)
            || drawables.transforms.contains_key(&container_id)
        {
            continue;
        }
        let Some(container_geom) = geometry.get(&container_id) else {
            continue;
        };
        paint_multicol_rule_for_page(
            canvas,
            entry,
            container_geom,
            margin_left_pt,
            margin_top_pt,
            page_index,
        );
    }
}

/// Per-fragment leaf-draw dispatch shared by the main loop and the
/// transform special-case. Walks `node_id`'s payload maps and emits
/// the appropriate per-type draw — exactly the same logic as before
/// the transform refactor, just hoisted into a function so
/// `draw_under_transform` can re-use it for descendants.
#[allow(clippy::too_many_arguments)]
fn dispatch_fragment(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    node_id: usize,
    geom: &crate::pagination_layout::PaginationGeometry,
    frag: &crate::pagination_layout::Fragment,
    x_pt: f32,
    y_pt: f32,
    drawables: &Drawables,
    page_index: u32,
) {
    if let Some(table) = drawables.tables.get(&node_id) {
        draw_table_v2(canvas, table, x_pt, y_pt, frag);
        return;
    }
    // True when this block / list-item / paragraph spans multiple
    // pages (the fragmenter recorded one fragment per page slice).
    // Passed down so `draw_block_inner_paint` can use the per-page
    // slice height (`frag.height`) instead of `layout_size.height`
    // — without it, every slice paints the FULL block, which
    // doubled the callout in `examples/break-inside` (fulgur-bq6i).
    let is_split = geom.is_split();
    // ListItem case: marker + body block + inline-root paragraph
    // share a single opacity group. See `draw_list_item_with_block`
    // for the v1 mirror.
    if let Some(li) = drawables.list_items.get(&node_id) {
        let block_for_li = drawables.block_styles.get(&node_id);
        let para_for_li = drawables.paragraphs.get(&node_id);
        draw_list_item_with_block(
            canvas,
            li,
            block_for_li,
            para_for_li,
            x_pt,
            y_pt,
            frag,
            &geom.fragments,
            page_index,
            is_split,
        );
        return;
    }
    // Block + inner content (paragraph / image / svg) sharing the
    // same node_id: combine into one `draw_with_opacity` group. See
    // `draw_block_with_inner_content` for the v1 mirror.
    if let Some(block) = drawables.block_styles.get(&node_id) {
        let para_for_block = drawables.paragraphs.get(&node_id);
        let img_for_block = drawables.images.get(&node_id);
        let svg_for_block = drawables.svgs.get(&node_id);
        if para_for_block.is_some() || img_for_block.is_some() || svg_for_block.is_some() {
            draw_block_with_inner_content(
                canvas,
                block,
                para_for_block,
                img_for_block,
                svg_for_block,
                x_pt,
                y_pt,
                frag,
                &geom.fragments,
                page_index,
                is_split,
            );
            return;
        }
        draw_block_v2(canvas, block, x_pt, y_pt, frag, is_split);
    }
    if let Some(img) = drawables.images.get(&node_id) {
        draw_image_v2(canvas, img, x_pt, y_pt);
        return;
    }
    if let Some(svg) = drawables.svgs.get(&node_id) {
        draw_svg_v2(canvas, svg, x_pt, y_pt);
        return;
    }
    if let Some(para) = drawables.paragraphs.get(&node_id) {
        draw_paragraph_v2(
            canvas,
            para,
            x_pt,
            y_pt,
            &geom.fragments,
            page_index,
            is_split,
        );
    }
}

/// Push the transform onto the surface + link collector, dispatch the
/// wrapper node's own payload and every descendant fragment that lands
/// on `page_index`, then pop. Mirrors v1's
/// `TransformWrapperPageable::draw`:
///
/// ```text
/// canvas.surface.push_transform(matrix);
/// inner.draw(canvas, x, y, ...);
/// canvas.surface.pop();
/// ```
///
/// The link collector also receives the transform so `/Link`
/// annotation rects are mapped into device space — same call sequence
/// v1 uses (`pageable.rs:2716-2724`).
#[allow(clippy::too_many_arguments)]
fn draw_under_transform(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    tx: &crate::drawables::TransformEntry,
    node_id: usize,
    geom: &crate::pagination_layout::PaginationGeometry,
    frag: &crate::pagination_layout::Fragment,
    x_pt: f32,
    y_pt: f32,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
) {
    use crate::convert::px_to_pt;

    // `effective_matrix` mirrors `TransformWrapperPageable::effective_matrix`:
    //   T(x + ox, y + oy) · M · T(-(x + ox), -(y + oy))
    let ox = x_pt + tx.origin.x;
    let oy = y_pt + tx.origin.y;
    use crate::pageable::Affine2D;
    let full = Affine2D::translation(ox, oy) * tx.matrix * Affine2D::translation(-ox, -oy);

    if let Some(lc) = canvas.link_collector.as_deref_mut() {
        lc.push_transform(full);
    }
    canvas.surface.push_transform(&full.to_krilla());

    // Dispatch the wrapper's own payload first (the `inner` Pageable
    // shares this `node_id`) — matches v1's `inner.draw(canvas, x, y, ...)`.
    dispatch_fragment(
        canvas, node_id, geom, frag, x_pt, y_pt, drawables, page_index,
    );

    // Then dispatch every strict descendant on this page. Each
    // descendant's fragment has its own (x, y) in untransformed local
    // coordinates — the surface transform applies on top, mirroring v1
    // where `inner.draw(...)` recurses through children with their
    // pre-transform layout.
    //
    // Descendants that have their OWN `TransformEntry` recurse into
    // `draw_under_transform` so their matrix composes with the outer
    // push (matches v1's nested `TransformWrapperPageable::draw` call
    // chain at `pageable.rs:2714-2725`). Without this recursion the
    // inner transform would be silently dropped, breaking
    // `<div style="transform:rotate"><div style="transform:scale">`
    // (PR #305 Devin).
    //
    // Pre-skip the strict descendants of any nested transform so they
    // are not dispatched twice — the nested `draw_under_transform`
    // already iterates `desc_tx.descendants` and paints them under
    // the composed matrix; iterating them again here via
    // `dispatch_fragment` would emit a SECOND draw under the outer
    // transform only (missing the inner matrix). Bug confirmed by
    // PR #305 follow-up Devin trace for
    // `<div transform:A><div transform:B><p>text</p></div></div>`.
    let nested_skip: std::collections::BTreeSet<usize> = tx
        .descendants
        .iter()
        .filter_map(|id| drawables.transforms.get(id))
        .flat_map(|inner_tx| inner_tx.descendants.iter().copied())
        .collect();
    // Symmetric pre-skip for `overflow:hidden` descendants. When a
    // clip block sits inside this transform, `draw_under_clip` (called
    // below) iterates its own `clip_descendants` to paint them inside
    // the clip; iterating those nodes again here via
    // `dispatch_fragment` would double-paint them outside the clip.
    // Mirrors the symmetric handling in `draw_under_clip`'s descendant
    // loop (PR #309 follow-up Devin).
    let mut clip_skip: std::collections::BTreeSet<usize> = tx
        .descendants
        .iter()
        .filter_map(|id| drawables.block_styles.get(id))
        .filter(|b| b.style.has_overflow_clip())
        .flat_map(|b| b.clip_descendants.iter().copied())
        .collect();
    // Tables with `overflow: hidden | clip` carry `clip_descendants`
    // too — `draw_under_clip_table` (called below) iterates them
    // inside the clip path, so the dispatch loop must skip them
    // here just like the block-clip case (fulgur-bvhw PR #320 Devin).
    clip_skip.extend(
        tx.descendants
            .iter()
            .filter_map(|id| drawables.tables.get(id))
            .filter(|t| t.style.has_overflow_clip())
            .flat_map(|t| t.clip_descendants.iter().copied()),
    );
    // Symmetric pre-skip for opacity-scoped descendants. When an
    // opacity block sits inside this transform, `draw_under_opacity`
    // (called below) iterates its own `opacity_descendants` to paint
    // them inside the opacity wrap; iterating those nodes again here
    // via `dispatch_fragment` would emit a second draw outside the
    // opacity group. Mirrors `clip_skip`. (fulgur-gdb9)
    let opacity_skip: std::collections::BTreeSet<usize> = tx
        .descendants
        .iter()
        .filter_map(|id| drawables.block_styles.get(id))
        .flat_map(|b| b.opacity_descendants.iter().copied())
        .collect();
    for &desc_id in &tx.descendants {
        if nested_skip.contains(&desc_id)
            || clip_skip.contains(&desc_id)
            || opacity_skip.contains(&desc_id)
        {
            continue;
        }
        let Some(desc_geom) = geometry.get(&desc_id) else {
            continue;
        };
        for desc_frag in &desc_geom.fragments {
            if desc_frag.page_index != page_index {
                continue;
            }
            let desc_x = margin_left_pt + px_to_pt(desc_frag.x);
            let desc_y = margin_top_pt + px_to_pt(desc_frag.y);
            if let Some(desc_tx) = drawables.transforms.get(&desc_id) {
                draw_under_transform(
                    canvas,
                    desc_tx,
                    desc_id,
                    desc_geom,
                    desc_frag,
                    desc_x,
                    desc_y,
                    geometry,
                    drawables,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
            } else if let Some(desc_block) = drawables
                .block_styles
                .get(&desc_id)
                .filter(|b| b.style.has_overflow_clip())
                .filter(|_| Some(desc_id) != drawables.body_id)
            {
                // Descendant carries `overflow:hidden|clip` — push its
                // clip path the same way the main loop does. Without
                // this, transforms wrapping a clipping block would
                // emit the inner block's bg/border via
                // `dispatch_fragment` but never push the clip, leaking
                // overflow content past the clip boundary
                // (PR #309 follow-up Devin).
                draw_under_clip(
                    canvas,
                    desc_block,
                    desc_id,
                    desc_geom,
                    desc_frag,
                    desc_x,
                    desc_y,
                    geometry,
                    drawables,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
            } else if let Some(desc_table) = drawables
                .tables
                .get(&desc_id)
                .filter(|t| t.style.has_overflow_clip())
            {
                // Table descendant with `overflow:hidden|clip` —
                // mirror the block-clip arm above so the table's clip
                // path is pushed inside the transform scope. Without
                // this, `<div style="transform:..."><table style=
                // "overflow:hidden">` paints cells under the transform
                // but loses the table boundary (fulgur-bvhw PR #320
                // Devin).
                draw_under_clip_table(
                    canvas,
                    desc_table,
                    desc_geom,
                    desc_frag,
                    desc_x,
                    desc_y,
                    geometry,
                    drawables,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
            } else if let Some(desc_block) = drawables
                .block_styles
                .get(&desc_id)
                .filter(|b| !b.opacity_descendants.is_empty())
            {
                // Opacity-scoped descendant inside this transform.
                // Without this branch, transforms wrapping an opacity
                // block would dispatch the descendant's own paint via
                // `dispatch_fragment` but skip the opacity wrap of
                // the descendant's children, dropping the descendant
                // block's opacity from its sub-children.
                // (fulgur-gdb9)
                draw_under_opacity(
                    canvas,
                    desc_block,
                    desc_id,
                    desc_geom,
                    desc_frag,
                    desc_x,
                    desc_y,
                    geometry,
                    drawables,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
            } else {
                dispatch_fragment(
                    canvas, desc_id, desc_geom, desc_frag, desc_x, desc_y, drawables, page_index,
                );
            }
        }
    }

    // Paint multicol column rules for any multicol container in this
    // transform's direct scope. Mirrors v1's
    // `MulticolRulePageable::draw` running inside
    // `TransformWrapperPageable::draw`'s `push_transform / pop` group
    // (`pageable.rs:2714-2725 → 3088`) so the rule lines render under
    // the composed matrix instead of in page coordinates.
    //
    // Direct scope = `tx.descendants` (or the transform key itself,
    // covered when `node_id` is also a multicol container) MINUS any
    // descendant that lives inside a NESTED transform — those are
    // painted by the inner `draw_under_transform` recursion to compose
    // both matrices. Without this filter, a multicol container nested
    // two transforms deep would paint its rules in the outer
    // transform's space, missing the inner matrix.
    // (PR #305 follow-up Devin)
    let nested_tx_desc: std::collections::BTreeSet<usize> = tx
        .descendants
        .iter()
        .filter_map(|id| drawables.transforms.get(id))
        .flat_map(|inner| inner.descendants.iter().copied())
        .collect();
    for (&container_id, entry) in &drawables.multicol_rules {
        let in_my_scope = container_id == node_id || tx.descendants.contains(&container_id);
        if !in_my_scope || nested_tx_desc.contains(&container_id) {
            continue;
        }
        let Some(container_geom) = geometry.get(&container_id) else {
            continue;
        };
        paint_multicol_rule_for_page(
            canvas,
            entry,
            container_geom,
            margin_left_pt,
            margin_top_pt,
            page_index,
        );
    }

    canvas.surface.pop();
    if let Some(lc) = canvas.link_collector.as_deref_mut() {
        lc.pop_transform();
    }
}

/// Push the block's overflow-clip path onto the surface, dispatch the
/// wrapper's own bg / border / shadow + every descendant fragment that
/// lands on `page_index`, then pop. Mirrors v1's `BlockPageable::draw`
/// (`pageable.rs:1796-1827`):
///
/// ```text
/// // bg/border/shadow paint OUTSIDE the clip
/// draw_with_opacity(canvas, opacity, |c| {
///     bg + border + shadow at (x, y, total_w, total_h);
///     if let Some(clip) = compute_overflow_clip_path(...) {
///         c.surface.push_clip_path(&clip, FillRule::default());
///         for child in children { child.draw(c, x + child.x, y + child.y, ..); }
///         c.surface.pop();
///     }
/// });
/// ```
///
/// In v2 the wrapper's own inner content (paragraph / image / svg
/// sharing the same `node_id`) is dispatched as part of the clipped
/// region, then strict descendants iterate inside the clip. The block
/// dispatcher's "shared node_id" combined helper
/// (`draw_block_with_inner_content`) already handles the outer
/// opacity wrap when used; here we replicate that ordering manually
/// — bg/border outside clip, inner content + descendants inside clip,
/// all wrapped in one opacity group.
#[allow(clippy::too_many_arguments)]
fn draw_under_clip(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    block: &crate::drawables::BlockEntry,
    node_id: usize,
    geom: &crate::pagination_layout::PaginationGeometry,
    frag: &crate::pagination_layout::Fragment,
    x_pt: f32,
    y_pt: f32,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
) {
    use crate::convert::px_to_pt;
    use crate::pageable::draw_with_opacity;

    let total_width = block
        .layout_size
        .map(|s| s.width)
        .unwrap_or_else(|| px_to_pt(frag.width));
    // Mirror `draw_block_inner_paint` / `draw_under_opacity`'s
    // `is_split` height fix. When this `overflow: hidden | clip`
    // block spans multiple pages (one fragment per page slice), use
    // `frag.height` so each slice paints its per-page bg / border /
    // shadow at the slice height — and pushes a clip rectangle of
    // the slice height too. Without this the bg / border overflows
    // the page bottom on earlier slices, double-paints on
    // continuation pages, AND the clip rect on continuation pages
    // covers content that should be cut off.
    // (PR #313 follow-up Devin Review — completes the PR #316 fix.)
    let is_split = geom.is_split();
    let total_height = block
        .layout_size
        .map(|s| {
            if is_split && frag.height > 0.0 {
                px_to_pt(frag.height)
            } else {
                s.height
            }
        })
        .unwrap_or_else(|| px_to_pt(frag.height));

    let para_for_block = drawables.paragraphs.get(&node_id);
    let img_for_block = drawables.images.get(&node_id);
    let svg_for_block = drawables.svgs.get(&node_id);
    let inner_inset = block.style.content_inset();

    // When this node is a list-item with overflow clip, mirror v1's
    // `ListItemPageable::draw` ordering: outer opacity uses
    // `list_item.opacity` (the body BlockPageable inside is built with
    // default opacity=1.0 in `convert::list_item::build_list_item_body`,
    // so `block.opacity` here would silently drop CSS opacity), and
    // the marker draws before `push_clip_path` (markers sit at negative
    // x outside the body box, so they must not be clipped). Without
    // this, `<li style="overflow:hidden">` loses its marker entirely
    // and any opacity set on the `<li>` is ignored. (PR #310 Devin)
    let list_item = drawables.list_items.get(&node_id);
    let opacity = list_item.map_or(block.opacity, |li| li.opacity);

    draw_with_opacity(canvas, opacity, |canvas| {
        // List-item marker paints first, OUTSIDE the clip — v1's
        // `ListItemPageable::draw` emits the marker before delegating
        // to `body.draw` (which paints bg / border / shadow). Markers
        // sit at negative x relative to (x_pt, y_pt), so they must
        // also stay outside the clip path pushed below.
        if let Some(li) = list_item
            && li.visible
        {
            draw_list_item_marker(canvas, li, x_pt, y_pt);
        }

        // bg / border / shadow outside the clip — same as
        // `draw_block_inner_paint` but inlined so the opacity wrap
        // covers the entire clipped region too.
        if block.visible {
            crate::background::draw_box_shadows(
                canvas,
                &block.style,
                x_pt,
                y_pt,
                total_width,
                total_height,
            );
            crate::background::draw_background(
                canvas,
                &block.style,
                x_pt,
                y_pt,
                total_width,
                total_height,
            );
            crate::pageable::draw_block_border(
                canvas,
                &block.style,
                x_pt,
                y_pt,
                total_width,
                total_height,
            );
        }

        // Push clip — fall through to inner content + descendants if
        // `compute_overflow_clip_path` returns `None` (style somehow
        // changed since extract decided this block clips).
        let clip_pushed = if let Some(clip_path) = crate::pageable::compute_overflow_clip_path(
            &block.style,
            x_pt,
            y_pt,
            total_width,
            total_height,
        ) {
            canvas
                .surface
                .push_clip_path(&clip_path, &krilla::paint::FillRule::default());
            true
        } else {
            false
        };

        // Inner content sharing `node_id` (inline-root paragraph,
        // replaced image / svg) paints at the content-box top-left,
        // not the border-box. Mirrors `draw_block_with_inner_content`.
        let inner_x = x_pt + inner_inset.0;
        let inner_y = y_pt + inner_inset.1;
        if let Some(p) = para_for_block {
            draw_paragraph_inner_paint(
                canvas,
                p,
                inner_x,
                inner_y,
                &geom.fragments,
                page_index,
                is_split,
            );
        }
        if let Some(i) = img_for_block {
            draw_image_inner_paint(canvas, i, inner_x, inner_y);
        }
        if let Some(s) = svg_for_block {
            draw_svg_inner_paint(canvas, s, inner_x, inner_y);
        }

        // Strict descendants — each at its own fragment's coords.
        //
        // Transform-aware dispatch: a descendant that has its own
        // `TransformEntry` must enter `draw_under_transform` so the
        // surface transform composes correctly. The main loop skips
        // these nodes via `clipped_descendants.contains(...)` BEFORE
        // it reaches the per-fragment transform check, so without the
        // recursion below v2 silently drops transforms inside an
        // `overflow: hidden` ancestor (`<div style="overflow:hidden">
        // <div style="transform:..."/></div>` — PR #310 Devin).
        //
        // Pre-skip the strict descendants of those transforms so they
        // are not dispatched twice — once via `draw_under_transform`
        // (which iterates `tx.descendants`) and once via the loop
        // body's `dispatch_fragment`.
        let transform_skip: std::collections::BTreeSet<usize> = block
            .clip_descendants
            .iter()
            .filter_map(|id| drawables.transforms.get(id))
            .flat_map(|tx| tx.descendants.iter().copied())
            .collect();
        // Symmetric pre-skip for nested `overflow:hidden` descendants.
        // The recursive `draw_under_clip` call below paints the inner
        // clip's children inside its push/pop group; iterating the
        // outer's `clip_descendants` for those same nodes here would
        // re-dispatch them outside the inner clip
        // (PR #309 follow-up Devin).
        let mut nested_clip_skip: std::collections::BTreeSet<usize> = block
            .clip_descendants
            .iter()
            .filter_map(|id| drawables.block_styles.get(id))
            .filter(|b| b.style.has_overflow_clip())
            .flat_map(|b| b.clip_descendants.iter().copied())
            .collect();
        // Tables with overflow-clip nested inside this block's clip
        // scope carry their own `clip_descendants` — the recursive
        // `draw_under_clip_table` arm below paints them inside the
        // table's clip path, so skip them here too (fulgur-bvhw PR
        // #320 Devin).
        nested_clip_skip.extend(
            block
                .clip_descendants
                .iter()
                .filter_map(|id| drawables.tables.get(id))
                .filter(|t| t.style.has_overflow_clip())
                .flat_map(|t| t.clip_descendants.iter().copied()),
        );
        // Symmetric pre-skip for opacity-scoped descendants nested
        // inside this clip. Mirrors `nested_clip_skip` — without it,
        // an opacity descendant's sub-children would be dispatched by
        // the loop AND by `draw_under_opacity` below, double-painting
        // them. (fulgur-gdb9)
        let nested_opacity_skip: std::collections::BTreeSet<usize> = block
            .clip_descendants
            .iter()
            .filter_map(|id| drawables.block_styles.get(id))
            .flat_map(|b| b.opacity_descendants.iter().copied())
            .collect();
        for &desc_id in &block.clip_descendants {
            if transform_skip.contains(&desc_id)
                || nested_clip_skip.contains(&desc_id)
                || nested_opacity_skip.contains(&desc_id)
            {
                continue;
            }
            let Some(desc_geom) = geometry.get(&desc_id) else {
                continue;
            };
            for desc_frag in &desc_geom.fragments {
                if desc_frag.page_index != page_index {
                    continue;
                }
                let desc_x = margin_left_pt + px_to_pt(desc_frag.x);
                let desc_y = margin_top_pt + px_to_pt(desc_frag.y);
                if let Some(desc_tx) = drawables.transforms.get(&desc_id) {
                    draw_under_transform(
                        canvas,
                        desc_tx,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_block) = drawables
                    .block_styles
                    .get(&desc_id)
                    .filter(|b| b.style.has_overflow_clip())
                    .filter(|_| Some(desc_id) != drawables.body_id)
                {
                    // Nested `overflow:hidden|clip` block — recurse so
                    // its own clip path is pushed. Without this, a
                    // `<div style="overflow:hidden"><div style="
                    // overflow:hidden;width:30px"><p>text</p></div>
                    // </div>` paints the inner block's bg/border via
                    // `dispatch_fragment` but never pushes the inner
                    // clip, losing the inner boundary
                    // (PR #309 follow-up Devin).
                    draw_under_clip(
                        canvas,
                        desc_block,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_table) = drawables
                    .tables
                    .get(&desc_id)
                    .filter(|t| t.style.has_overflow_clip())
                {
                    // Nested table with overflow-clip — recurse into
                    // `draw_under_clip_table` so the table boundary
                    // is pushed inside this block's clip scope.
                    // (fulgur-bvhw PR #320 Devin)
                    draw_under_clip_table(
                        canvas,
                        desc_table,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_block) = drawables
                    .block_styles
                    .get(&desc_id)
                    .filter(|b| !b.opacity_descendants.is_empty())
                {
                    // Nested opacity-scoped block — recurse so its
                    // descendants paint inside its `draw_with_opacity`
                    // wrap. (fulgur-gdb9)
                    draw_under_opacity(
                        canvas,
                        desc_block,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else {
                    dispatch_fragment(
                        canvas, desc_id, desc_geom, desc_frag, desc_x, desc_y, drawables,
                        page_index,
                    );
                }
            }
        }

        if clip_pushed {
            canvas.surface.pop();
        }
    });
}

/// Wrap the block's `dispatch_fragment` + every strict descendant in a
/// single `draw_with_opacity` group. Used for blocks that have
/// fractional opacity but no overflow clip (the clip arm,
/// `draw_under_clip`, already handles its own opacity wrap).
///
/// Mirrors v1's `BlockPageable::draw` (`pageable.rs:1770-1828`):
///
/// ```text
/// draw_with_opacity(canvas, self.opacity, |c| {
///     bg + border + shadow at (x, y, total_w, total_h);
///     for pc in self.children { pc.child.draw(c, x + pc.x, y + pc.y, ..); }
/// });
/// ```
///
/// v1 emits a single transparency-group XObject for the entire
/// subtree. v2's flat dispatch without scope tracking would emit the
/// block's own paint inside opacity but every descendant outside,
/// dropping the parent's opacity on those descendants. Mirrors
/// `draw_under_clip` minus the `push_clip_path` / `pop` calls and the
/// list-item marker arm (a list-item with opacity uses
/// `draw_list_item_with_block`, not this path, since list-item
/// markers are owned by `ListItemEntry` rather than `BlockEntry`).
#[allow(clippy::too_many_arguments)]
fn draw_under_opacity(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    block: &crate::drawables::BlockEntry,
    node_id: usize,
    geom: &crate::pagination_layout::PaginationGeometry,
    frag: &crate::pagination_layout::Fragment,
    x_pt: f32,
    y_pt: f32,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
) {
    use crate::convert::px_to_pt;
    use crate::pageable::draw_with_opacity;

    draw_with_opacity(canvas, block.opacity, |canvas| {
        // Block's own paint + shared-node_id inner content. We can
        // re-use `dispatch_fragment` because it already handles the
        // shared-node_id case via `draw_block_with_inner_content`,
        // which itself opens a `draw_with_opacity(block.opacity, ..)`
        // wrap. That nested wrap is harmless: krilla composes nested
        // opacity groups by multiplication (0.4 × 1.0 = 0.4, since the
        // inner block-with-inner-content uses the SAME opacity),
        // matching the v1 chain `draw_with_opacity(0.4) → child draws`
        // where the child happens to also be a block at full opacity.
        //
        // For pure-block-with-descendants (the case we're fixing —
        // `<div opacity:0.4><svg>..</svg></div>`), `dispatch_fragment`
        // calls `draw_block_v2` which wraps own paint in
        // `draw_with_opacity(0.4)`. The outer wrap here multiplies to
        // 0.16 — wrong! Inline the block's own paint without the inner
        // opacity wrap to avoid this.
        if drawables.list_items.contains_key(&node_id) {
            // Inside an opacity-scoped block path we never reach a
            // list-item: list-items dispatch through their own
            // `draw_list_item_with_block` which composes the marker +
            // body block + paragraph in one opacity group. If a
            // list-item carries opacity, it would not have entered
            // `draw_under_opacity` because list-item's opacity comes
            // from `ListItemEntry`, not `BlockEntry`. Defensive guard
            // only — should be unreachable.
            dispatch_fragment(
                canvas, node_id, geom, frag, x_pt, y_pt, drawables, page_index,
            );
        } else {
            // Block bg / border / shadow without the inner opacity
            // wrap. The shared-node_id (`paragraph` / `image` / `svg`
            // at the same node_id) inner content paints at the
            // content-box top-left; mirrors
            // `draw_block_with_inner_content`'s body but without its
            // own `draw_with_opacity` since the outer wrap already
            // covers it.
            let para_for_block = drawables.paragraphs.get(&node_id);
            let img_for_block = drawables.images.get(&node_id);
            let svg_for_block = drawables.svgs.get(&node_id);
            let total_width = block
                .layout_size
                .map(|s| s.width)
                .unwrap_or_else(|| px_to_pt(frag.width));
            // Mirror `draw_block_inner_paint`'s `is_split` height fix.
            // When this opacity-scoped block spans multiple pages
            // (one fragment per page slice), use `frag.height` so each
            // slice paints its per-page bg / border height instead of
            // the full layout height — without this the bg / border
            // overflows the page bottom on earlier slices and double-
            // paints on continuation pages, exactly the bug the
            // `draw_block_inner_paint` fix addresses for non-opacity
            // blocks (PR #316). (PR #314 follow-up Devin Review)
            let is_split = geom.is_split();
            let total_height = block
                .layout_size
                .map(|s| {
                    if is_split && frag.height > 0.0 {
                        px_to_pt(frag.height)
                    } else {
                        s.height
                    }
                })
                .unwrap_or_else(|| px_to_pt(frag.height));
            if block.visible {
                crate::background::draw_box_shadows(
                    canvas,
                    &block.style,
                    x_pt,
                    y_pt,
                    total_width,
                    total_height,
                );
                crate::background::draw_background(
                    canvas,
                    &block.style,
                    x_pt,
                    y_pt,
                    total_width,
                    total_height,
                );
                crate::pageable::draw_block_border(
                    canvas,
                    &block.style,
                    x_pt,
                    y_pt,
                    total_width,
                    total_height,
                );
            }
            let inner_inset = block.style.content_inset();
            let inner_x = x_pt + inner_inset.0;
            let inner_y = y_pt + inner_inset.1;
            if let Some(p) = para_for_block {
                draw_paragraph_inner_paint(
                    canvas,
                    p,
                    inner_x,
                    inner_y,
                    &geom.fragments,
                    page_index,
                    is_split,
                );
            }
            if let Some(i) = img_for_block {
                draw_image_inner_paint(canvas, i, inner_x, inner_y);
            }
            if let Some(s) = svg_for_block {
                draw_svg_inner_paint(canvas, s, inner_x, inner_y);
            }
        }

        // Descendants — same dispatch tree as `draw_under_clip` minus
        // the nested-clip recursion (an opacity-scoped block by
        // construction has `clipping == false`, so its descendants
        // can still individually have clip / transform / opacity, and
        // those need their own scope helpers).
        let transform_skip: std::collections::BTreeSet<usize> = block
            .opacity_descendants
            .iter()
            .filter_map(|id| drawables.transforms.get(id))
            .flat_map(|tx| tx.descendants.iter().copied())
            .collect();
        let mut nested_clip_skip: std::collections::BTreeSet<usize> = block
            .opacity_descendants
            .iter()
            .filter_map(|id| drawables.block_styles.get(id))
            .filter(|b| b.style.has_overflow_clip())
            .flat_map(|b| b.clip_descendants.iter().copied())
            .collect();
        // Tables with overflow-clip nested inside this opacity scope
        // recurse into `draw_under_clip_table`; pre-skip their cell
        // descendants here so they don't double-paint outside the
        // table's clip path (fulgur-bvhw PR #320 Devin).
        nested_clip_skip.extend(
            block
                .opacity_descendants
                .iter()
                .filter_map(|id| drawables.tables.get(id))
                .filter(|t| t.style.has_overflow_clip())
                .flat_map(|t| t.clip_descendants.iter().copied()),
        );
        let nested_opacity_skip: std::collections::BTreeSet<usize> = block
            .opacity_descendants
            .iter()
            .filter_map(|id| drawables.block_styles.get(id))
            .flat_map(|b| b.opacity_descendants.iter().copied())
            .collect();
        for &desc_id in &block.opacity_descendants {
            if transform_skip.contains(&desc_id)
                || nested_clip_skip.contains(&desc_id)
                || nested_opacity_skip.contains(&desc_id)
            {
                continue;
            }
            let Some(desc_geom) = geometry.get(&desc_id) else {
                continue;
            };
            for desc_frag in &desc_geom.fragments {
                if desc_frag.page_index != page_index {
                    continue;
                }
                let desc_x = margin_left_pt + px_to_pt(desc_frag.x);
                let desc_y = margin_top_pt + px_to_pt(desc_frag.y);
                if let Some(desc_tx) = drawables.transforms.get(&desc_id) {
                    draw_under_transform(
                        canvas,
                        desc_tx,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_block) = drawables
                    .block_styles
                    .get(&desc_id)
                    .filter(|b| b.style.has_overflow_clip())
                    .filter(|_| Some(desc_id) != drawables.body_id)
                {
                    draw_under_clip(
                        canvas,
                        desc_block,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_table) = drawables
                    .tables
                    .get(&desc_id)
                    .filter(|t| t.style.has_overflow_clip())
                {
                    // Table with overflow-clip nested inside this
                    // opacity scope. (fulgur-bvhw PR #320 Devin)
                    draw_under_clip_table(
                        canvas,
                        desc_table,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_block) = drawables
                    .block_styles
                    .get(&desc_id)
                    .filter(|b| !b.opacity_descendants.is_empty())
                {
                    draw_under_opacity(
                        canvas,
                        desc_block,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else {
                    dispatch_fragment(
                        canvas, desc_id, desc_geom, desc_frag, desc_x, desc_y, drawables,
                        page_index,
                    );
                }
            }
        }
    });
}

/// Paint multicol column-rule lines on `page_index` for one
/// `MulticolRuleEntry`. Partitions `entry.groups` by accumulating the
/// container's per-page heights — mirrors
/// `MulticolRulePageable::slice_for_page` + `draw` so each page only
/// emits the rule segments that fit on it.
fn paint_multicol_rule_for_page(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::MulticolRuleEntry,
    container_geom: &crate::pagination_layout::PaginationGeometry,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
) {
    use crate::convert::px_to_pt;
    use crate::pageable::stroke_line;

    let Some(stroke) = build_multicol_stroke(&entry.rule) else {
        return;
    };

    let target_pos = container_geom
        .fragments
        .iter()
        .position(|f| f.page_index == page_index);
    let Some(target_pos) = target_pos else {
        return;
    };
    let target_frag = &container_geom.fragments[target_pos];

    let consumed: f32 = px_to_pt(
        container_geom.fragments[..target_pos]
            .iter()
            .map(|f| f.height)
            .sum::<f32>(),
    );
    let cutoff = px_to_pt(target_frag.height);

    let x_base = margin_left_pt + px_to_pt(target_frag.x);
    let y_base = margin_top_pt + px_to_pt(target_frag.y);

    for group in &entry.groups {
        if group.n < 2 || group.col_heights.len() != group.n as usize {
            continue;
        }
        let group_top = group.y_offset - consumed;
        let max_h = group
            .col_heights
            .iter()
            .copied()
            .fold(0.0_f32, |acc, h| acc.max(h));
        let group_bottom = group_top + max_h;
        if group_bottom <= 0.0 || group_top >= cutoff {
            continue;
        }
        let visible_top = group_top.max(0.0);
        let y_top = y_base + visible_top;
        // Mirror `MulticolRulePageable::slice_for_page`
        // (`pageable.rs:3221-3223`): subtract the portion of each
        // column already painted on prior pages BEFORE clamping to
        // the visible strip on this page. Without this, a column rule
        // segment whose group straddles a page boundary extends past
        // the actual visible column content.
        let consumed_above = (visible_top - group_top).max(0.0);
        let visible_h = (group_bottom.min(cutoff) - visible_top).max(0.0);
        for i in 0..(group.n as usize - 1) {
            let h_left = (group.col_heights[i] - consumed_above)
                .max(0.0)
                .min(visible_h);
            let h_right = (group.col_heights[i + 1] - consumed_above)
                .max(0.0)
                .min(visible_h);
            if h_left <= 0.0 || h_right <= 0.0 {
                continue;
            }
            let rule_x = x_base
                + group.x_offset
                + (i as f32 + 1.0) * group.col_w
                + i as f32 * group.gap
                + group.gap / 2.0;
            let y_bot = y_top + h_left.min(h_right);
            stroke_line(canvas, rule_x, y_top, rule_x, y_bot, stroke.clone());
        }
    }
    canvas.surface.set_stroke(None);
}

/// Build the krilla stroke for the configured rule spec, mirroring
/// `MulticolRulePageable::build_stroke`. Returns `None` when the rule
/// is invisible (style `None` or non-positive width).
fn build_multicol_stroke(
    rule: &crate::column_css::ColumnRuleSpec,
) -> Option<krilla::paint::Stroke> {
    use crate::column_css::ColumnRuleStyle;
    use crate::pageable::{alpha_to_opacity, colored_stroke};

    if rule.width <= 0.0 || rule.style == ColumnRuleStyle::None {
        return None;
    }
    let opacity = alpha_to_opacity(rule.color[3]);
    let base = colored_stroke(&rule.color, rule.width, opacity);
    let w = rule.width;
    let stroke = match rule.style {
        ColumnRuleStyle::None => return None,
        ColumnRuleStyle::Solid => base,
        ColumnRuleStyle::Dashed => krilla::paint::Stroke {
            dash: Some(krilla::paint::StrokeDash {
                array: vec![w * 3.0, w * 2.0],
                offset: 0.0,
            }),
            ..base
        },
        ColumnRuleStyle::Dotted => krilla::paint::Stroke {
            line_cap: krilla::paint::LineCap::Round,
            dash: Some(krilla::paint::StrokeDash {
                array: vec![0.0, w * 2.0],
                offset: 0.0,
            }),
            ..base
        },
    };
    Some(stroke)
}

/// v2 image draw. Mirrors `image::ImagePageable::draw` but operates on
/// the side-channel `ImageEntry` data; the `width`/`height` are the
/// CSS-resolved size in pt that fulgur stores on the original
/// `ImagePageable`.
fn draw_image_v2(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::ImageEntry,
    x: f32,
    y: f32,
) {
    use crate::pageable::draw_with_opacity;
    draw_with_opacity(canvas, entry.opacity, |canvas| {
        draw_image_inner_paint(canvas, entry, x, y);
    });
}

/// Image paint without `draw_with_opacity` wrapper. Used by
/// `draw_block_with_inner_content` so a `<img>` whose wrapping inline-root
/// `BlockPageable` shares its node_id (`convert::replaced`) composes
/// with the block bg/border under one opacity group, mirroring v1's
/// `BlockPageable::draw` (`pageable.rs:1771`).
fn draw_image_inner_paint(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::ImageEntry,
    x: f32,
    y: f32,
) {
    if !entry.visible {
        return;
    }
    let Some(image) = decode_image_for_v2(entry) else {
        return;
    };
    let Some(size) = krilla::geom::Size::from_wh(entry.width, entry.height) else {
        return;
    };
    let transform = krilla::geom::Transform::from_translate(x, y);
    canvas.surface.push_transform(&transform);
    canvas.surface.draw_image(image, size);
    canvas.surface.pop();
}

fn decode_image_for_v2(entry: &crate::drawables::ImageEntry) -> Option<krilla::image::Image> {
    use crate::image::ImageFormat;
    use krilla::image::Image;
    let data: krilla::Data = entry.image_data.clone().into();
    let image_result = match entry.format {
        ImageFormat::Png => Image::from_png(data, true),
        ImageFormat::Jpeg => Image::from_jpeg(data, true),
        ImageFormat::Gif => Image::from_gif(data, true),
    };
    image_result.ok()
}

/// v2 SVG draw. Mirrors `svg::SvgPageable::draw`.
fn draw_svg_v2(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::SvgEntry,
    x: f32,
    y: f32,
) {
    use crate::pageable::draw_with_opacity;
    draw_with_opacity(canvas, entry.opacity, |canvas| {
        draw_svg_inner_paint(canvas, entry, x, y);
    });
}

/// SVG paint without `draw_with_opacity` wrapper. See
/// `draw_image_inner_paint` for the rationale (inline-root `<svg>`
/// shares node_id with the wrapping block).
fn draw_svg_inner_paint(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::SvgEntry,
    x: f32,
    y: f32,
) {
    use krilla_svg::{SurfaceExt, SvgSettings};
    if !entry.visible {
        return;
    }
    let Some(size) = krilla::geom::Size::from_wh(entry.width, entry.height) else {
        return;
    };
    let transform = krilla::geom::Transform::from_translate(x, y);
    canvas.surface.push_transform(&transform);
    let _ = canvas
        .surface
        .draw_svg(&entry.tree, size, SvgSettings::default());
    canvas.surface.pop();
}

/// v2 block draw. Mirrors `BlockPageable::draw`'s background / border /
/// box-shadow emission. Children paint themselves via their own
/// per-NodeId dispatch in `draw_v2_page`, so this fn does **not**
/// recurse into block children.
///
/// Overflow clip (`overflow: hidden`) is intentionally not pushed
/// here — the v1 recursive draw scope owns push/pop while the v2 flat
/// dispatch does not have a natural "end of children" point. Phase 4
/// PR 5+ will add a per-block clip scope by tracking child-exit
/// fragments. Documents that rely on `overflow: hidden` won't
/// byte-eq until then; the inline test cases avoid that property.
fn draw_block_v2(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::BlockEntry,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
    is_split: bool,
) {
    use crate::pageable::draw_with_opacity;

    draw_with_opacity(canvas, entry.opacity, |canvas| {
        draw_block_inner_paint(canvas, entry, x, y, frag, is_split);
    });
}

/// v2 root-element (`<html>`) background pre-pass. The fragmenter's
/// `geometry` table only carries body + descendants, so the standard
/// per-(node_id, fragment) dispatch never visits html. Mirror v1's
/// `BlockPageable::draw` for the html root by painting bg / border /
/// shadow at `(margin.left, margin.top)` with html's own
/// `layout_size` (which equals body's outer height including
/// collapsed margins, matching v1's `total_height` derivation).
///
/// Called once per page from `render_v2`; intentionally bypasses the
/// `body_offset_pt.y` adjustment that the main dispatch loop applies,
/// because html's bg paints at the page's margin top, not at body's
/// content origin.
fn paint_root_block_v2(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::BlockEntry,
    margin_left_pt: f32,
    margin_top_pt: f32,
) {
    use crate::pageable::draw_with_opacity;
    let Some(size) = entry.layout_size else {
        return;
    };

    draw_with_opacity(canvas, entry.opacity, |canvas| {
        if entry.visible {
            crate::background::draw_box_shadows(
                canvas,
                &entry.style,
                margin_left_pt,
                margin_top_pt,
                size.width,
                size.height,
            );
            crate::background::draw_background(
                canvas,
                &entry.style,
                margin_left_pt,
                margin_top_pt,
                size.width,
                size.height,
            );
            crate::pageable::draw_block_border(
                canvas,
                &entry.style,
                margin_left_pt,
                margin_top_pt,
                size.width,
                size.height,
            );
        }
    });
}

/// Block bg / border / shadow paint without the outer `draw_with_opacity`
/// wrap. Used by `draw_list_item_with_block` so the list-item's marker
/// and body block share a single opacity group (matches v1's
/// `ListItemPageable::draw` byte output exactly).
fn draw_block_inner_paint(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::BlockEntry,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
    is_split: bool,
) {
    let total_width = entry
        .layout_size
        .map(|s| s.width)
        .unwrap_or_else(|| crate::convert::px_to_pt(frag.width));
    // For split blocks (one fragment per page), `frag.height` reports
    // the per-page slice height. `entry.layout_size.height` always
    // carries Taffy's full block height, so painting bg / border with
    // `layout_size` would draw the FULL block on every slice — visible
    // as a callout-box overflowing page bottom on page 1 AND repeating
    // full-size on page 2 (fulgur-bq6i: `examples/break-inside`).
    //
    // Mirror v1: `BlockPageable::slice_for_page` returns a sliced
    // pageable whose `layout_size.height` already equals the slice
    // height, so v1's draw uses the slice-correct height naturally.
    // v2 has a single `BlockEntry` per node_id holding the full
    // layout, so we recover the slice-correct height from
    // `frag.height` only when the dispatcher tells us this is a
    // split fragment (`is_split = geom.fragments.len() > 1`). Using
    // a multi-fragment signal — not a `frag_h < layout_h` comparison
    // — avoids spurious flips for single-page blocks where the two
    // values may differ by 1 ULP after CSS-px → pt conversion
    // rounding.
    let total_height = entry
        .layout_size
        .map(|s| {
            if is_split && frag.height > 0.0 {
                crate::convert::px_to_pt(frag.height)
            } else {
                s.height
            }
        })
        .unwrap_or_else(|| crate::convert::px_to_pt(frag.height));

    if entry.visible {
        crate::background::draw_box_shadows(canvas, &entry.style, x, y, total_width, total_height);
        crate::background::draw_background(canvas, &entry.style, x, y, total_width, total_height);
        crate::pageable::draw_block_border(canvas, &entry.style, x, y, total_width, total_height);
    }
}

/// v2 table draw. Mirrors `TablePageable::draw`'s outer-frame
/// background / border / shadow emission. Cell paint (each `<th>` /
/// `<td>` is a `BlockPageable` with its own NodeId in geometry) lands
/// through the standard per-NodeId dispatch.
///
/// Tables with `overflow: hidden | clip` route through
/// [`draw_under_clip_table`] instead so the clip path wraps every cell
/// dispatched in the same scope. Multi-page table header repetition
/// (`<thead>` cloned on continuation pages) is deferred to a later PR.
fn draw_table_v2(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::TableEntry,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
) {
    use crate::pageable::draw_with_opacity;

    draw_with_opacity(canvas, entry.opacity, |canvas| {
        let (total_width, total_height) = table_box_size(entry, frag);
        if entry.visible {
            paint_table_outer_frame(canvas, entry, x, y, total_width, total_height);
        }
    });
}

/// Resolve the table's outer-frame width/height from the cached
/// layout. Falls back to the current Fragment height (and finally
/// `cached_height`) when `layout_size` is unset (test-only paths).
fn table_box_size(
    entry: &crate::drawables::TableEntry,
    frag: &crate::pagination_layout::Fragment,
) -> (f32, f32) {
    let total_width = entry.layout_size.map(|s| s.width).unwrap_or(entry.width);
    let total_height = entry.layout_size.map(|s| s.height).unwrap_or_else(|| {
        let from_frag = crate::convert::px_to_pt(frag.height);
        if from_frag > 0.0 {
            from_frag
        } else {
            entry.cached_height
        }
    });
    (total_width, total_height)
}

/// Paint the table's outer-frame bg / border / shadow at the current
/// (x, y, width, height). Shared between the no-clip path
/// ([`draw_table_v2`]) and the clip path ([`draw_under_clip_table`])
/// so the two emit identical PDF operators for the same input.
fn paint_table_outer_frame(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::TableEntry,
    x: f32,
    y: f32,
    total_width: f32,
    total_height: f32,
) {
    crate::background::draw_box_shadows(canvas, &entry.style, x, y, total_width, total_height);
    crate::background::draw_background(canvas, &entry.style, x, y, total_width, total_height);
    crate::pageable::draw_block_border(canvas, &entry.style, x, y, total_width, total_height);
}

/// Push a `compute_overflow_clip_path` clip around the table's outer
/// frame, dispatch each cell descendant inside the clip, then pop.
/// Mirrors [`draw_under_clip`]'s shape for blocks but specialised for
/// tables (no list-item marker, no shared-node_id inner content).
#[allow(clippy::too_many_arguments)]
fn draw_under_clip_table(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    table: &crate::drawables::TableEntry,
    geom: &crate::pagination_layout::PaginationGeometry,
    frag: &crate::pagination_layout::Fragment,
    x_pt: f32,
    y_pt: f32,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
) {
    use crate::convert::px_to_pt;
    use crate::pageable::draw_with_opacity;

    let _ = geom;
    let (total_width, total_height) = table_box_size(table, frag);

    draw_with_opacity(canvas, table.opacity, |canvas| {
        // bg / border / shadow OUTSIDE the clip, mirroring
        // `draw_under_clip` for blocks (`pageable.rs:1796-1827`).
        if table.visible {
            paint_table_outer_frame(canvas, table, x_pt, y_pt, total_width, total_height);
        }

        // Push clip — fall through to descendant dispatch even if
        // `compute_overflow_clip_path` returns `None` so the cells
        // still paint (defensive, mirrors `draw_under_clip`).
        let clip_pushed = if let Some(clip_path) = crate::pageable::compute_overflow_clip_path(
            &table.style,
            x_pt,
            y_pt,
            total_width,
            total_height,
        ) {
            canvas
                .surface
                .push_clip_path(&clip_path, &krilla::paint::FillRule::default());
            true
        } else {
            false
        };

        // Mirror `draw_under_clip`'s nested-scope skip sets so cells
        // carrying their own `transform` / `overflow:hidden` /
        // fractional opacity recurse into the proper helper rather
        // than fall through to plain `dispatch_fragment` (which would
        // silently lose the inner clip / transform / opacity wrap).
        // (PR #320 Devin Review)
        let transform_skip: std::collections::BTreeSet<usize> = table
            .clip_descendants
            .iter()
            .filter_map(|id| drawables.transforms.get(id))
            .flat_map(|tx| tx.descendants.iter().copied())
            .collect();
        let mut nested_clip_skip: std::collections::BTreeSet<usize> = table
            .clip_descendants
            .iter()
            .filter_map(|id| drawables.block_styles.get(id))
            .filter(|b| b.style.has_overflow_clip())
            .flat_map(|b| b.clip_descendants.iter().copied())
            .collect();
        nested_clip_skip.extend(
            table
                .clip_descendants
                .iter()
                .filter_map(|id| drawables.tables.get(id))
                .filter(|t| t.style.has_overflow_clip())
                .flat_map(|t| t.clip_descendants.iter().copied()),
        );
        let nested_opacity_skip: std::collections::BTreeSet<usize> = table
            .clip_descendants
            .iter()
            .filter_map(|id| drawables.block_styles.get(id))
            .flat_map(|b| b.opacity_descendants.iter().copied())
            .collect();

        for &desc_id in &table.clip_descendants {
            if transform_skip.contains(&desc_id)
                || nested_clip_skip.contains(&desc_id)
                || nested_opacity_skip.contains(&desc_id)
            {
                continue;
            }
            let Some(desc_geom) = geometry.get(&desc_id) else {
                continue;
            };
            for desc_frag in &desc_geom.fragments {
                if desc_frag.page_index != page_index {
                    continue;
                }
                let desc_x = margin_left_pt + px_to_pt(desc_frag.x);
                let desc_y = margin_top_pt + px_to_pt(desc_frag.y);
                if let Some(desc_tx) = drawables.transforms.get(&desc_id) {
                    draw_under_transform(
                        canvas,
                        desc_tx,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_block) = drawables
                    .block_styles
                    .get(&desc_id)
                    .filter(|b| b.style.has_overflow_clip())
                    .filter(|_| Some(desc_id) != drawables.body_id)
                {
                    draw_under_clip(
                        canvas,
                        desc_block,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_table) = drawables
                    .tables
                    .get(&desc_id)
                    .filter(|t| t.style.has_overflow_clip())
                {
                    draw_under_clip_table(
                        canvas,
                        desc_table,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else if let Some(desc_block) = drawables
                    .block_styles
                    .get(&desc_id)
                    .filter(|b| !b.opacity_descendants.is_empty())
                {
                    draw_under_opacity(
                        canvas,
                        desc_block,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        geometry,
                        drawables,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                } else {
                    dispatch_fragment(
                        canvas, desc_id, desc_geom, desc_frag, desc_x, desc_y, drawables,
                        page_index,
                    );
                }
            }
        }

        if clip_pushed {
            canvas.surface.pop();
        }
    });
}

/// v2 block + inner content combined draw. Mirrors v1's
/// `BlockPageable::draw` (`pageable.rs:1771`) which wraps bg/border
/// **and** the children draw inside ONE
/// `draw_with_opacity(self.opacity, ...)` group.
///
/// The shared-node_id patterns from `convert::inline_root` (block
/// wraps an inline-root paragraph) and `convert::replaced` (block
/// wraps `<img>` / `<svg>`) deliberately leave the inner draw payload
/// at `opacity: 1.0` — the wrapping block carries the real opacity.
/// Composing them all under a single `draw_with_opacity(block.opacity, ...)`
/// keeps the v1 `q .. Q` framing intact so byte-eq holds for
/// `<p style="opacity:0.5; background:red">text</p>` and friends.
#[allow(clippy::too_many_arguments)]
fn draw_block_with_inner_content(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    block: &crate::drawables::BlockEntry,
    paragraph: Option<&crate::drawables::ParagraphEntry>,
    image: Option<&crate::drawables::ImageEntry>,
    svg: Option<&crate::drawables::SvgEntry>,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
    fragments: &[crate::pagination_layout::Fragment],
    page_index: u32,
    is_split: bool,
) {
    use crate::pageable::draw_with_opacity;

    // Inner content (inline-root paragraph from `convert::inline_root`
    // or replaced image / svg from `convert::replaced`) is positioned
    // at the block's content-box top-left, not its border-box top-left.
    // v1 expresses this via `PositionedChild { x: content_inset.x, y:
    // content_inset.y, .. }` so `BlockPageable::draw` recurses into the
    // child at `(x + ix, y + iy)`. v2 has no `PositionedChild` here —
    // the inner payload shares the block's `node_id` and would
    // otherwise paint at the block's border-box origin, dropping
    // `padding + border` worth of offset for every inline-root or
    // replaced element. Mirror v1 by reading the inset from the
    // BlockStyle and adding it before recursing.
    let (ix, iy) = block.style.content_inset();
    let inner_x = x + ix;
    let inner_y = y + iy;

    draw_with_opacity(canvas, block.opacity, |canvas| {
        draw_block_inner_paint(canvas, block, x, y, frag, is_split);
        if let Some(p) = paragraph {
            draw_paragraph_inner_paint(
                canvas, p, inner_x, inner_y, fragments, page_index, is_split,
            );
        }
        if let Some(i) = image {
            draw_image_inner_paint(canvas, i, inner_x, inner_y);
        }
        if let Some(s) = svg {
            draw_svg_inner_paint(canvas, s, inner_x, inner_y);
        }
    });
}

/// v2 list-item combined draw. Mirrors v1's `ListItemPageable::draw`
/// (`pageable.rs:3336`) which wraps the marker plus everything painted
/// by `self.body.draw(...)` in a single `draw_with_opacity(self.opacity, ...)`
/// group.
///
/// The `<li>` and its body BlockPageable share the same node_id
/// (`convert/list_item.rs:81`); the body is built with `opacity: 1.0`
/// on purpose. When the body holds inline content, the inline-root
/// paragraph also lands at the same node_id (see `convert::inline_root`).
/// Painting marker + block frame + paragraph glyphs in one compositing
/// group is what keeps `<li style="opacity:..">` byte-identical with
/// v1 — separate `draw_with_opacity` calls would emit multiple `q .. Q`
/// pairs and diverge.
#[allow(clippy::too_many_arguments)]
fn draw_list_item_with_block(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    list_item: &crate::drawables::ListItemEntry,
    block: Option<&crate::drawables::BlockEntry>,
    paragraph: Option<&crate::drawables::ParagraphEntry>,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
    fragments: &[crate::pagination_layout::Fragment],
    page_index: u32,
    is_split: bool,
) {
    use crate::pageable::draw_with_opacity;

    // Same content-box offset trick as `draw_block_with_inner_content`
    // — when the body block carries `padding` / `border`, the
    // inline-root paragraph that shares the list item's node_id has to
    // paint at the body block's content-box top-left.
    let inset = block.map(|b| b.style.content_inset()).unwrap_or((0.0, 0.0));
    let inner_x = x + inset.0;
    let inner_y = y + inset.1;

    draw_with_opacity(canvas, list_item.opacity, |canvas| {
        if list_item.visible {
            draw_list_item_marker(canvas, list_item, x, y);
        }
        if let Some(b) = block {
            draw_block_inner_paint(canvas, b, x, y, frag, is_split);
        }
        if let Some(p) = paragraph {
            draw_paragraph_inner_paint(
                canvas, p, inner_x, inner_y, fragments, page_index, is_split,
            );
        }
    });
}

/// List-item marker paint without opacity wrapper or visibility gate
/// — caller (`draw_list_item_with_block`) handles both so the marker
/// and the body block share one compositing group.
fn draw_list_item_marker(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::ListItemEntry,
    x: f32,
    y: f32,
) {
    use crate::pageable::{ImageMarker, ListItemMarker};

    match &entry.marker {
        ListItemMarker::Text { lines, width } if !lines.is_empty() => {
            crate::paragraph::draw_shaped_lines(canvas, lines, x - *width, y);
        }
        ListItemMarker::Image {
            marker,
            width,
            height,
        } => {
            let marker_x = x - *width;
            let marker_y = y + (entry.marker_line_height - *height) / 2.0;
            match marker {
                ImageMarker::Raster(img) => {
                    img.draw(canvas, marker_x, marker_y, *width, *height);
                }
                ImageMarker::Svg(svg) => {
                    svg.draw(canvas, marker_x, marker_y, *width, *height);
                }
            }
        }
        _ => {}
    }
}

/// v2 paragraph draw. Mirrors `paragraph::ParagraphPageable::draw`:
/// honour `visible`, wrap with `draw_with_opacity`, then call the
/// existing `paragraph::draw_shaped_lines` which already handles glyph
/// runs / inline images / inline boxes / link rect emission /
/// decoration spans. Reusing the helper keeps the per-glyph PDF output
/// byte-identical between v1 and v2.
fn draw_paragraph_v2(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::ParagraphEntry,
    x: f32,
    y: f32,
    fragments: &[crate::pagination_layout::Fragment],
    page_index: u32,
    is_split: bool,
) {
    use crate::pageable::draw_with_opacity;
    draw_with_opacity(canvas, entry.opacity, |canvas| {
        draw_paragraph_inner_paint(canvas, entry, x, y, fragments, page_index, is_split);
    });
}

/// Paragraph paint without the outer `draw_with_opacity` wrap. Used
/// by `draw_list_item_with_block` so a list-item containing inline
/// content (the body block holds an inline-root paragraph at the same
/// node_id) can compose marker + block paint + glyph runs into a
/// single opacity group, matching v1's
/// `ListItemPageable::draw → body.draw → paragraph.draw` chain.
fn draw_paragraph_inner_paint(
    canvas: &mut crate::pageable::Canvas<'_, '_>,
    entry: &crate::drawables::ParagraphEntry,
    x: f32,
    y: f32,
    fragments: &[crate::pagination_layout::Fragment],
    page_index: u32,
    is_split: bool,
) {
    if !entry.visible {
        return;
    }
    let Some(slice) = paragraph_lines_for_page(&entry.lines, fragments, page_index, is_split)
    else {
        return;
    };
    crate::paragraph::draw_shaped_lines(canvas, &slice, x, y);
}

/// Phase 4 PR 3 follow-up (PR #302 Devin): mirror
/// `ParagraphPageable::slice_for_page` so multi-page paragraphs only
/// emit the lines belonging to the requested page.
///
/// `is_split` is the parent `PaginationGeometry::is_split()` —
/// `false` when the paragraph fits one page OR when the geometry
/// represents per-page repetition (`is_repeat=true`, e.g.
/// `position: fixed`). Either case means each fragment carries the
/// full content, so the function returns every line unmodified.
/// `true` triggers the cumulative-height slicing logic.
fn paragraph_lines_for_page(
    all_lines: &[crate::paragraph::ShapedLine],
    fragments: &[crate::pagination_layout::Fragment],
    page_index: u32,
    is_split: bool,
) -> Option<Vec<crate::paragraph::ShapedLine>> {
    let target_pos = fragments.iter().position(|f| f.page_index == page_index)?;

    if !is_split {
        return Some(all_lines.to_vec());
    }

    let target_h = crate::convert::px_to_pt(fragments[target_pos].height);
    let consumed: f32 = crate::convert::px_to_pt(
        fragments[..target_pos]
            .iter()
            .map(|f| f.height)
            .sum::<f32>(),
    );

    let eps = 0.01_f32;
    let mut line_top: f32 = 0.0;
    let mut start_idx = 0usize;
    while start_idx < all_lines.len() {
        let next_top = line_top + all_lines[start_idx].height;
        if next_top > consumed + eps {
            break;
        }
        line_top = next_top;
        start_idx += 1;
    }

    let mut end_idx = start_idx;
    let mut accum = 0.0_f32;
    while end_idx < all_lines.len() {
        let line_h = all_lines[end_idx].height;
        if accum + line_h > target_h + eps {
            break;
        }
        accum += line_h;
        end_idx += 1;
    }

    if end_idx <= start_idx {
        return None;
    }

    let sliced: Vec<crate::paragraph::ShapedLine> = all_lines[start_idx..end_idx]
        .iter()
        .cloned()
        .map(|mut line| {
            // Rebase paragraph-absolute coords (baseline + inline
            // image `computed_y`) to fragment-local. Mirror
            // `ParagraphPageable::slice_for_page` exactly.
            line.baseline -= consumed;
            for item in &mut line.items {
                if let crate::paragraph::LineItem::Image(img) = item {
                    img.computed_y -= consumed;
                }
            }
            line
        })
        .collect();
    Some(sliced)
}

/// Build krilla Metadata from Config.
fn build_metadata(config: &Config) -> krilla::metadata::Metadata {
    let mut metadata = krilla::metadata::Metadata::new();
    if let Some(ref title) = config.title {
        metadata = metadata.title(title.clone());
    }
    if !config.authors.is_empty() {
        metadata = metadata.authors(config.authors.clone());
    }
    if let Some(ref description) = config.description {
        metadata = metadata.description(description.clone());
    }
    if !config.keywords.is_empty() {
        metadata = metadata.keywords(config.keywords.clone());
    }
    if let Some(ref lang) = config.lang {
        metadata = metadata.language(lang.clone());
    }
    if let Some(ref creator) = config.creator {
        metadata = metadata.creator(creator.clone());
    }
    if let Some(ref producer) = config.producer {
        metadata = metadata.producer(producer.clone());
    }
    if let Some(ref date_str) = config.creation_date {
        if let Some(dt) = parse_datetime(date_str) {
            metadata = metadata.creation_date(dt);
        }
    }
    metadata
}

/// Parse an ISO 8601 date string into a krilla DateTime.
/// Supports: "YYYY", "YYYY-MM", "YYYY-MM-DD", "YYYY-MM-DDThh:mm:ss".
/// Returns None if any component fails to parse.
fn parse_datetime(s: &str) -> Option<krilla::metadata::DateTime> {
    let parts: Vec<&str> = s.splitn(2, 'T').collect();
    let date_tokens: Vec<&str> = parts[0].split('-').collect();
    let year: u16 = date_tokens.first()?.parse().ok()?;
    let mut dt = krilla::metadata::DateTime::new(year);
    if let Some(month_str) = date_tokens.get(1) {
        let month: u8 = month_str.parse().ok()?;
        dt = dt.month(month);
    }
    if let Some(day_str) = date_tokens.get(2) {
        let day: u8 = day_str.parse().ok()?;
        dt = dt.day(day);
    }
    if let Some(time_str) = parts.get(1) {
        // Strip trailing 'Z' for UTC
        let time_str = time_str.trim_end_matches('Z');
        let time_tokens: Vec<&str> = time_str.split(':').collect();
        if let Some(hour_str) = time_tokens.first() {
            let hour: u8 = hour_str.parse().ok()?;
            dt = dt.hour(hour);
        }
        if let Some(minute_str) = time_tokens.get(1) {
            let minute: u8 = minute_str.parse().ok()?;
            dt = dt.minute(minute);
        }
        if let Some(second_str) = time_tokens.get(2) {
            let second: u8 = second_str.parse().ok()?;
            dt = dt.second(second);
        }
    }
    Some(dt)
}

/// Cached max-content width and render Pageable for margin boxes.
/// Measure cache: (html, page_height as bits) → max-content width.
/// Render cache: (html, final_width as bits, final_height as bits) → Pageable.
type MeasureCache = HashMap<(String, u32, u32), f32>;
type RenderCache = HashMap<(String, u32, u32), Box<dyn Pageable>>;

fn width_key(w: f32) -> u32 {
    w.to_bits()
}

/// Per-page state and caches required to render `@page` margin boxes
/// (`@top-center`, `@bottom-center`, `@left-middle`, etc.). Built once
/// per render and reused across pages so measure / layout passes for
/// repeated content (e.g. a page-number footer) hit the cache.
///
/// Used by both `render_to_pdf_with_gcpm` (v1 path) and `render_v2`
/// (Phase 4 v2 path) — both call `render_page` per page.
pub(crate) struct MarginBoxRenderer<'a> {
    pub gcpm: &'a GcpmContext,
    pub running_store: &'a RunningElementStore,
    pub font_data: &'a [Arc<Vec<u8>>],
    pub margin_css: String,
    pub string_set_states: Vec<BTreeMap<String, crate::pagination_layout::StringSetPageState>>,
    pub running_states: Vec<BTreeMap<String, crate::pagination_layout::PageRunningState>>,
    pub counter_states: Vec<BTreeMap<String, i32>>,
    pub measure_cache: MeasureCache,
    pub height_cache: HashMap<(String, u32, u32), f32>,
    pub render_cache: RenderCache,
}

impl<'a> MarginBoxRenderer<'a> {
    /// Build a renderer from raw inputs. `string_set_by_node` /
    /// `counter_ops_by_node` are the per-node maps drained out of
    /// `ConvertContext` before `dom_to_pageable` consumed them.
    pub(crate) fn new(
        gcpm: &'a GcpmContext,
        running_store: &'a RunningElementStore,
        font_data: &'a [Arc<Vec<u8>>],
        pagination_geometry: &crate::pagination_layout::PaginationGeometryTable,
        string_set_by_node: &HashMap<usize, Vec<(String, String)>>,
        counter_ops_by_node: &BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
        total_pages: usize,
    ) -> Self {
        let string_set_states = if gcpm.string_set_mappings.is_empty() {
            vec![BTreeMap::new(); total_pages]
        } else {
            let by_node_btree: BTreeMap<usize, Vec<(String, String)>> = string_set_by_node
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect();
            crate::pagination_layout::collect_string_set_states(pagination_geometry, &by_node_btree)
        };
        let running_states = if gcpm.running_mappings.is_empty() {
            vec![BTreeMap::new(); total_pages]
        } else {
            crate::pagination_layout::collect_running_element_states(
                pagination_geometry,
                running_store,
            )
        };
        let counter_states =
            if gcpm.counter_mappings.is_empty() && gcpm.content_counter_mappings.is_empty() {
                vec![BTreeMap::new(); total_pages]
            } else {
                crate::pagination_layout::collect_counter_states(
                    pagination_geometry,
                    counter_ops_by_node,
                )
            };
        Self {
            gcpm,
            running_store,
            font_data,
            margin_css: strip_display_none(&gcpm.cleaned_css),
            string_set_states,
            running_states,
            counter_states,
            measure_cache: HashMap::new(),
            height_cache: HashMap::new(),
            render_cache: HashMap::new(),
        }
    }

    /// Render every margin box that applies to `page_idx` onto
    /// `canvas`. Mirrors the per-page block from
    /// `render_to_pdf_with_gcpm`'s pre-Phase-4 implementation:
    ///
    /// 1. Filter `gcpm.margin_boxes` by `@page` selector matching
    ///    (`:first` / `:left` / `:right`), preferring more-specific
    ///    selectors over the default.
    /// 2. Resolve each box's HTML content (substituting `counter()` /
    ///    `element()` / `string()` from per-page state).
    /// 3. Measure max-content width (top/bottom) or height (left/right).
    /// 4. Distribute boxes along each edge with `compute_edge_layout`.
    /// 5. Render each box at its final rect via Blitz parse + layout +
    ///    `dom_to_pageable`, then `pageable.draw(canvas, rect)`.
    ///
    /// `content_width` is the page content area width in pt — used as
    /// the available width during measure passes for top/bottom boxes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_page(
        &mut self,
        canvas: &mut Canvas<'_, '_>,
        page_idx: usize,
        page_num: usize,
        total_pages: usize,
        page_size: crate::config::PageSize,
        resolved_margin: crate::config::Margin,
        content_width: f32,
    ) {
        // Resolve effective boxes: pick the most specific matching rule
        // per position. Pseudo-class selectors (`:first`, `:left`,
        // `:right`) override the default `@page` rule.
        let mut effective_boxes: BTreeMap<MarginBoxPosition, &crate::gcpm::MarginBoxRule> =
            BTreeMap::new();
        for margin_box in &self.gcpm.margin_boxes {
            let matches = match &margin_box.page_selector {
                None => true,
                Some(sel) => match sel.as_str() {
                    ":first" => page_num == 1,
                    ":left" => page_num % 2 == 0,
                    ":right" => page_num % 2 != 0,
                    _ => true,
                },
            };
            if !matches {
                continue;
            }
            let should_replace = effective_boxes
                .get(&margin_box.position)
                .map(|existing| {
                    existing.page_selector.is_none() && margin_box.page_selector.is_some()
                })
                .unwrap_or(true);
            if should_replace {
                effective_boxes.insert(margin_box.position, margin_box);
            }
        }

        // Resolve HTML for each effective box.
        let mut resolved_htmls: BTreeMap<MarginBoxPosition, String> = BTreeMap::new();
        for (&pos, rule) in &effective_boxes {
            let content_html = resolve_content_to_html(
                &rule.content,
                self.running_store,
                &self.running_states,
                &self.string_set_states[page_idx],
                page_num,
                total_pages,
                page_idx,
                &self.counter_states[page_idx],
            );
            if !content_html.is_empty() {
                let html = if rule.declarations.is_empty() {
                    content_html
                } else {
                    format!(
                        "<div style=\"{}\">{}</div>",
                        escape_attr(&rule.declarations),
                        content_html
                    )
                };
                resolved_htmls.insert(pos, html);
            }
        }

        // Stage 1a: measure max-content width for top / bottom boxes.
        for (&pos, html) in &resolved_htmls {
            if !pos.edge().is_some_and(|e| e.is_horizontal()) {
                continue;
            }
            // Cache key includes `content_width` because `@page :first` /
            // `:left` / `:right` can override margins per page, changing
            // the available viewport width that Blitz lays the
            // `display: inline-block` measure document at. Pre-Phase-4
            // v1 had a single global `content_width` so two-tuple keys
            // were complete; the v2 port made `content_width` a
            // per-page parameter and a stale cache entry could
            // misalign margin boxes on pages with overridden margins
            // (PR #309 Devin).
            let measure_key = (
                html.clone(),
                width_key(page_size.height),
                width_key(content_width),
            );
            self.measure_cache.entry(measure_key).or_insert_with(|| {
                let measure_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\"><div style=\"display:inline-block\">{}</div></body></html>",
                    self.margin_css, html
                );
                let measure_doc = crate::blitz_adapter::parse_and_layout(
                    &measure_html,
                    crate::convert::pt_to_px(content_width),
                    crate::convert::pt_to_px(page_size.height),
                    self.font_data,
                );
                get_body_child_dimension(&measure_doc, true)
            });
        }

        // Stage 1b: measure max-content height for left / right boxes.
        for (&pos, html) in &resolved_htmls {
            let fixed_width = match pos.edge() {
                Some(Edge::Left) => resolved_margin.left,
                Some(Edge::Right) => resolved_margin.right,
                _ => continue,
            };
            // Include `page_size.height` in the key so a `@page :first`
            // (or other matched-page selector) that overrides the page
            // SIZE — not just margins — gets a fresh measurement on
            // the second page. Mirrors `measure_cache`'s key (which
            // already records `page_size.height`); without this, two
            // pages with the same fixed margin width but different
            // page heights would share a stale entry. (PR #309 Devin)
            let hc_key = (
                html.clone(),
                width_key(fixed_width),
                width_key(page_size.height),
            );
            self.height_cache.entry(hc_key).or_insert_with(|| {
                let measure_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\"><div>{}</div></body></html>",
                    self.margin_css, html
                );
                let measure_doc = crate::blitz_adapter::parse_and_layout(
                    &measure_html,
                    crate::convert::pt_to_px(fixed_width),
                    crate::convert::pt_to_px(page_size.height),
                    self.font_data,
                );
                get_body_child_dimension(&measure_doc, false)
            });
        }

        // Stage 2: distribute each edge's boxes against the page rect.
        let mut edge_defined: BTreeMap<Edge, BTreeMap<MarginBoxPosition, f32>> = BTreeMap::new();
        for (&pos, html) in &resolved_htmls {
            let edge = match pos.edge() {
                Some(e) => e,
                None => continue,
            };
            let size = if edge.is_horizontal() {
                self.measure_cache
                    .get(&(
                        html.clone(),
                        width_key(page_size.height),
                        width_key(content_width),
                    ))
                    .copied()
            } else {
                let fixed_width = if edge == Edge::Left {
                    resolved_margin.left
                } else {
                    resolved_margin.right
                };
                self.height_cache
                    .get(&(
                        html.clone(),
                        width_key(fixed_width),
                        width_key(page_size.height),
                    ))
                    .copied()
            };
            if let Some(s) = size {
                edge_defined.entry(edge).or_default().insert(pos, s);
            }
        }
        let mut all_rects: HashMap<MarginBoxPosition, MarginBoxRect> = HashMap::new();
        for (edge, defined) in &edge_defined {
            all_rects.extend(compute_edge_layout(
                *edge,
                defined,
                page_size,
                resolved_margin,
            ));
        }

        // Stage 3: render at the confirmed rect.
        for (&pos, html) in &resolved_htmls {
            let rect = all_rects
                .get(&pos)
                .copied()
                .unwrap_or_else(|| pos.bounding_rect(page_size, resolved_margin));

            let cache_key = (html.clone(), width_key(rect.width), width_key(rect.height));
            if !self.render_cache.contains_key(&cache_key) {
                let render_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\">{}</body></html>",
                    self.margin_css, html
                );
                let render_doc = crate::blitz_adapter::parse_and_layout(
                    &render_html,
                    crate::convert::pt_to_px(rect.width),
                    crate::convert::pt_to_px(rect.height),
                    self.font_data,
                );
                let dummy_store = RunningElementStore::new();
                let mut dummy_ctx = crate::convert::ConvertContext {
                    running_store: &dummy_store,
                    assets: None,
                    font_cache: HashMap::new(),
                    string_set_by_node: HashMap::new(),
                    counter_ops_by_node: HashMap::new(),
                    bookmark_by_node: HashMap::new(),
                    column_styles: crate::column_css::ColumnStyleTable::new(),
                    multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
                    pagination_geometry: crate::pagination_layout::PaginationGeometryTable::new(),
                    link_cache: Default::default(),
                    viewport_size_px: None,
                };
                let pageable = crate::convert::dom_to_pageable(&render_doc, &mut dummy_ctx);
                self.render_cache.insert(cache_key.clone(), pageable);
            }

            if let Some(pageable) = self.render_cache.get(&cache_key) {
                pageable.draw(canvas, rect.x, rect.y, rect.width, rect.height);
            }
        }
    }
}

/// Get a layout dimension of the first non-zero child of `<body>` in a Blitz document.
/// When `use_width` is true, returns max-content width; otherwise returns height.
///
/// Returned value is in PDF pt. Blitz's internal layout is in CSS px, so we
/// multiply by `PX_TO_PT` on the way out — matching the convention used at
/// the convert.rs boundary (`layout_in_pt`). This keeps the GCPM margin-box
/// measure caches in the same unit (pt) as `page_size` / `margin`, which
/// `compute_edge_layout` assumes when distributing along the edge.
fn get_body_child_dimension(doc: &blitz_html::HtmlDocument, use_width: bool) -> f32 {
    use std::ops::Deref;
    let root = doc.root_element();
    let base_doc = doc.deref();

    let px: f32 = 'outer: {
        if let Some(root_node) = base_doc.get_node(root.id) {
            for &child_id in &root_node.children {
                if let Some(child) = base_doc.get_node(child_id) {
                    if let blitz_dom::NodeData::Element(elem) = &child.data {
                        if elem.name.local.as_ref() == "body" {
                            for &body_child_id in &child.children {
                                if let Some(body_child) = base_doc.get_node(body_child_id) {
                                    let size = &body_child.final_layout.size;
                                    let v = if use_width { size.width } else { size.height };
                                    if v > 0.0 {
                                        break 'outer v;
                                    }
                                }
                            }
                            let size = &child.final_layout.size;
                            break 'outer if use_width { size.width } else { size.height };
                        }
                    }
                }
            }
        }
        0.0
    };
    crate::convert::px_to_pt(px)
}

/// Escape a string for use in an HTML attribute value.
fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Strip `display: none` declarations from CSS.
/// Used to build margin-box CSS where running elements need to be visible.
fn strip_display_none(css: &str) -> String {
    css.replace("display: none", "").replace("display:none", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- escape_attr ---

    #[test]
    fn escape_attr_no_special_chars() {
        assert_eq!(escape_attr("plain text"), "plain text");
    }

    #[test]
    fn escape_attr_ampersand() {
        assert_eq!(escape_attr("foo&bar"), "foo&amp;bar");
    }

    #[test]
    fn escape_attr_double_quote() {
        assert_eq!(escape_attr(r#"foo"bar"#), "foo&quot;bar");
    }

    #[test]
    fn escape_attr_less_than() {
        assert_eq!(escape_attr("foo<bar"), "foo&lt;bar");
    }

    #[test]
    fn escape_attr_greater_than() {
        assert_eq!(escape_attr("foo>bar"), "foo&gt;bar");
    }

    #[test]
    fn escape_attr_all_specials_combined() {
        assert_eq!(
            escape_attr(r#"<"a" & "b">"#),
            "&lt;&quot;a&quot; &amp; &quot;b&quot;&gt;"
        );
    }

    #[test]
    fn escape_attr_empty_string() {
        assert_eq!(escape_attr(""), "");
    }

    // --- strip_display_none ---

    #[test]
    fn strip_display_none_spaced_variant() {
        let css = ".x { display: none; color: red; }";
        let result = strip_display_none(css);
        assert!(
            !result.contains("display: none"),
            "should remove 'display: none'"
        );
        assert!(
            result.contains("color: red"),
            "should preserve other properties"
        );
    }

    #[test]
    fn strip_display_none_unspaced_variant() {
        let css = ".x { display:none; margin: 0; }";
        let result = strip_display_none(css);
        assert!(
            !result.contains("display:none"),
            "should remove 'display:none'"
        );
        assert!(
            result.contains("margin: 0"),
            "should preserve other properties"
        );
    }

    #[test]
    fn strip_display_none_no_match_is_noop() {
        let css = "body { color: blue; }";
        assert_eq!(strip_display_none(css), css);
    }

    #[test]
    fn strip_display_none_both_variants_in_same_string() {
        let css = "a { display: none; } b { display:none; }";
        let result = strip_display_none(css);
        assert!(!result.contains("display: none"));
        assert!(!result.contains("display:none"));
    }

    // --- width_key ---

    #[test]
    fn width_key_matches_to_bits() {
        let w = 42.5_f32;
        assert_eq!(width_key(w), w.to_bits());
    }

    #[test]
    fn width_key_distinct_values_differ() {
        assert_ne!(width_key(1.0), width_key(2.0));
    }

    #[test]
    fn width_key_zero() {
        assert_eq!(width_key(0.0_f32), 0_f32.to_bits());
    }

    // --- parse_datetime ---

    #[test]
    fn parse_datetime_valid_year_only() {
        assert!(parse_datetime("2024").is_some());
    }

    #[test]
    fn parse_datetime_valid_year_month() {
        assert!(parse_datetime("2024-06").is_some());
    }

    #[test]
    fn parse_datetime_valid_year_month_day() {
        assert!(parse_datetime("2024-06-15").is_some());
    }

    #[test]
    fn parse_datetime_valid_full_datetime() {
        assert!(parse_datetime("2024-06-15T10:30:45").is_some());
    }

    #[test]
    fn parse_datetime_valid_full_datetime_with_z() {
        assert!(parse_datetime("2024-06-15T10:30:45Z").is_some());
    }

    #[test]
    fn parse_datetime_valid_midnight() {
        assert!(parse_datetime("2024-01-01T00:00:00").is_some());
    }

    #[test]
    fn parse_datetime_valid_hour_only_in_time() {
        // only hour field present in time part → still valid
        assert!(parse_datetime("2024-01-01T12").is_some());
    }

    #[test]
    fn parse_datetime_valid_hour_minute_in_time() {
        assert!(parse_datetime("2024-01-01T12:30").is_some());
    }

    #[test]
    fn parse_datetime_invalid_empty_string() {
        assert!(parse_datetime("").is_none());
    }

    #[test]
    fn parse_datetime_invalid_non_numeric_year() {
        assert!(parse_datetime("abcd").is_none());
    }

    #[test]
    fn parse_datetime_invalid_non_numeric_month() {
        assert!(parse_datetime("2024-ab").is_none());
    }

    #[test]
    fn parse_datetime_invalid_non_numeric_day() {
        assert!(parse_datetime("2024-06-ab").is_none());
    }

    #[test]
    fn parse_datetime_invalid_non_numeric_hour() {
        assert!(parse_datetime("2024-06-15Tabc:30:45").is_none());
    }

    #[test]
    fn parse_datetime_invalid_non_numeric_minute() {
        assert!(parse_datetime("2024-06-15T10:abc:45").is_none());
    }

    #[test]
    fn parse_datetime_invalid_non_numeric_second() {
        assert!(parse_datetime("2024-06-15T10:30:abc").is_none());
    }
}
