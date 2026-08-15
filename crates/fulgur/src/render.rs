use crate::config::Config;
use crate::draw_primitives::Canvas;
use crate::drawables::Drawables;
use crate::error::{Error, Result};
use crate::gcpm::GcpmContext;
use crate::gcpm::counter::resolve_content_to_html_with_anchor;
use crate::gcpm::margin_box::{Edge, MarginBoxPosition, MarginBoxRect, compute_edge_layout};
use crate::gcpm::running::RunningElementStore;
use crate::gcpm::target_ref::AnchorMap;
use crate::units::F32Units;
use krilla::SerializeSettings;
use krilla::configure::{Configuration, Validator};
use krilla::tagging::{Identifier, Node, TagGroup, TagTree};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Geometry-driven page render.
///
/// Walks `geometry` per page and dispatches each (node_id, fragment)
/// to per-type draw functions sourced from `drawables`. Page settings
/// (size, margins, landscape, GCPM `@page` overrides) resolve once per
/// page.
#[allow(clippy::too_many_arguments)]
pub fn render_v2(
    config: &Config,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
    gcpm: &GcpmContext,
    running_store: &RunningElementStore,
    font_data: &[Arc<Vec<u8>>],
    system_fonts: bool,
    string_set_by_node: &HashMap<usize, Vec<(String, String)>>,
    counter_ops_by_node: &BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
    html_title: Option<String>,
    serialize_settings: SerializeSettings,
    anchor_map: Option<&AnchorMap>,
    implicit_href_by_page: &BTreeMap<usize, String>,
) -> Result<Vec<u8>> {
    let mut document = if config.effective_tagging() {
        let configuration = if config.pdf_ua {
            Configuration::new_with_validator(Validator::UA1)
        } else {
            Configuration::new()
        };
        krilla::Document::new_with(SerializeSettings {
            enable_tagging: true,
            configuration,
            ..serialize_settings
        })
    } else {
        krilla::Document::new_with(serialize_settings)
    };

    let mut bookmark_collector = if config.effective_bookmarks() {
        Some(crate::draw_primitives::BookmarkCollector::new())
    } else {
        None
    };

    let mut tag_collector = if config.effective_tagging() {
        Some(crate::draw_primitives::TagCollector::new())
    } else {
        None
    };

    let page_count = crate::pagination_layout::implied_page_count(geometry).max(1) as usize;

    // Pre-pass: register `id` anchors for `href="#..."` resolution.
    // A node may appear in both `paragraphs` and `block_styles`
    // (shared node_id case — see `convert::replaced` /
    // `convert::inline_root`); paragraph wins so the shared-id case
    // resolves to the inline-root's anchor.
    let mut dest_registry = crate::draw_primitives::DestinationRegistry::new();
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
                    drawables.root_dir_rtl,
                );
            // `frag.x` is html-relative (already includes body's x
            // offset from the fragmenter); only y needs `body_offset_pt`
            // applied, and only on page 0 (continuation pages are already
            // page-content-area-relative after the fragmenter resets cursor_y).
            let body_y_off = if page_idx == 0 {
                drawables.body_offset_pt.1
            } else {
                crate::units::Pt::ZERO
            };
            let x_pt = resolved_margin.left.as_pt() + first_frag.x.in_pt();
            let y_pt = resolved_margin.top.as_pt() + body_y_off + first_frag.y.in_pt();
            dest_registry.record(id.as_str(), x_pt, y_pt);
        }
    }

    // Body content and margin boxes get separate LinkCollectors. Body
    // occurrences are clipped to the content area before emission so a
    // body link positioned into a page-margin strip (via CSS positioning
    // or overflow) cannot leave a click target under a later-painted
    // margin box — the paint order would otherwise let the visible text
    // and the clickable URI disagree. Margin-box occurrences are emitted
    // unclipped so `@page @bottom-*` anchors remain clickable.
    let mut body_link_collector = crate::draw_primitives::LinkCollector::new();
    let mut margin_link_collector = crate::draw_primitives::LinkCollector::new();

    let mut link_annot_ids: BTreeMap<usize, Vec<krilla::tagging::Identifier>> = BTreeMap::new();

    // Build the GCPM margin-box renderer once. Reused across pages so
    // measure / layout / render caches survive between pages and the
    // pre-computed `string_set_states` / `counter_states` /
    // `running_states` are paid for once.
    let mut margin_box_renderer = MarginBoxRenderer::new(
        gcpm,
        running_store,
        font_data,
        system_fonts,
        geometry,
        string_set_by_node,
        counter_ops_by_node,
        page_count,
        implicit_href_by_page,
    );

    // Page-independent skip sets. These reference only `drawables.*`
    // (transforms / block_styles / tables) and never change across
    // pages, so building them inside `draw_v2_page` once per page made
    // the per-page work O(N_blocks + N_tables + N_transforms). See
    // fulgur-v1cm.
    let (transformed_descendants, clipped_descendants, opacity_wrapped_descendants) =
        build_page_skip_sets(drawables);

    // Per-page dispatch buckets: which `node_id`s have at least one
    // fragment on this page index. The previous shape — walking the
    // entire `geometry` BTreeMap once per page — was the dominant
    // O(P²) cost in document-grade documents (N tables × P
    // page-sections), since `geometry` grows with N which itself
    // scales with P. Bucketing collapses that to O(N) build + O(F)
    // dispatch where F is the total fragment count. (fulgur-v1cm)
    //
    // Iteration order inside each bucket follows `BTreeMap<NodeId, _>`
    // which is approximately document order, preserving v1 stacking
    // (parents before children, backgrounds before foregrounds). This
    // is the same invariant the original loop relied on. Each node is
    // inserted at most once per page even when it has multiple
    // fragments on that page — `dispatch_fragment` and the per-fragment
    // arms below already iterate `geom.fragments` themselves and skip
    // mismatched `frag.page_index`, so a single dispatch entry per
    // (node, page) is sufficient.
    let mut per_page_node_ids: Vec<Vec<usize>> = vec![Vec::new(); page_count];
    for (&node_id, geom) in geometry {
        let mut seen_pages: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for frag in &geom.fragments {
            let p = frag.page_index;
            if (p as usize) < page_count && seen_pages.insert(p) {
                per_page_node_ids[p as usize].push(node_id);
            }
        }
    }

    for (page_idx, page_node_ids) in per_page_node_ids.iter().enumerate() {
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
                drawables.root_dir_rtl,
            );
        let page_size = if resolved_landscape {
            resolved_size.landscape()
        } else {
            resolved_size
        };
        let settings = krilla::page::PageSettings::from_wh(page_size.width, page_size.height)
            .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;
        let content_area = resolved_content_area(page_size, resolved_margin);
        let mut page = document.start_page_with(settings);
        if let Some(c) = bookmark_collector.as_mut() {
            c.set_current_page(page_idx);
        }
        body_link_collector.set_current_page(page_idx);
        margin_link_collector.set_current_page(page_idx);
        let page_has_margin_box_paint;
        {
            let mut surface = page.surface();
            {
                let mut canvas = crate::draw_primitives::Canvas {
                    surface: &mut surface,
                    bookmark_collector: bookmark_collector.as_mut(),
                    link_collector: Some(&mut body_link_collector),
                    tag_collector: tag_collector.as_mut(),
                    link_run_node_id: None,
                    in_marked_content: false,
                };
                // Root `<html>` + `<body>` background pre-pass. Root
                // `<html>` / `<body>` backgrounds repeat per page
                // (CSS Paged Media §5.4), but the fragmenter only
                // records a single fragment for each on page 0, so a
                // pure per-fragment dispatch would lose their fills on
                // continuation pages. Paint each here at its own offset
                // (`(margin.left, margin.top)` for html, plus
                // `body_offset_pt` for body) using `layout_size` from
                // the entry. The main dispatch loop skips both
                // `root_id` and `body_id` to avoid double-painting on
                // page 0.
                if let Some(root_id) = drawables.root_id
                    && let Some(root_block) = drawables.block_styles.get(&root_id)
                {
                    paint_root_block_v2(
                        &mut canvas,
                        root_block,
                        resolved_margin.left,
                        resolved_margin.top,
                        None,
                    );
                }
                // Body bg pre-pass: runs on ALL pages. body's
                // `layout_size.height` is the full document height (page-0
                // layout), which overshoots the content area whenever
                // margin-bottom > 0. Painting here with `content_area_h`
                // prevents that overshoot on every page (fulgur-ossm).
                //
                // Continuation pages (page_idx > 0) have no body fragment in
                // the geometry table (fragmenter only records body on page 0),
                // so the pre-pass is their sole source of body background.
                //
                // Page 0: body HAS a geometry fragment, so the main dispatch
                // also visits it. `draw_v2_page` therefore skips body's block
                // bg for body_id (only rendering inline content) to avoid
                // double-painting (see the body_id check in `draw_v2_page`).
                if let Some(body_id) = drawables.body_id
                    && let Some(body_block) = drawables.block_styles.get(&body_id)
                {
                    let content_area_h = content_area.height.to_f32();
                    let body_bg_y = if page_idx == 0 {
                        resolved_margin.top + drawables.body_offset_pt.1.to_f32()
                    } else {
                        resolved_margin.top
                    };
                    paint_root_block_v2(
                        &mut canvas,
                        body_block,
                        resolved_margin.left + drawables.body_offset_pt.0.to_f32(),
                        body_bg_y,
                        Some(content_area_h),
                    );
                }
                // `frag.x` is html-relative (fragmenter folds body's x
                // offset in); `frag.y` is body-content-area-relative.
                // On page 0 body_offset_pt.1 translates body-content-area
                // to html-content-area; on continuation pages the fragmenter
                // resets cursor_y=0 per page so fragments are already
                // page-content-area-relative and the offset must not apply.
                let body_top_pt = if page_idx == 0 {
                    resolved_margin.top + drawables.body_offset_pt.1.to_f32()
                } else {
                    resolved_margin.top
                };
                draw_v2_page(
                    &mut canvas,
                    page_idx as u32,
                    resolved_margin.left,
                    body_top_pt,
                    geometry,
                    drawables,
                    &transformed_descendants,
                    &clipped_descendants,
                    &opacity_wrapped_descendants,
                    page_node_ids,
                );
            }
            // Paint margin boxes after body content so page headers /
            // footers are not hidden by page-filling body backgrounds.
            // Keep bookmarks disabled for repeated running elements.
            // Margin-box links go into their own collector so their
            // occurrences do not get clipped to the body content area
            // (they live in the margin strips by design).
            let mut margin_canvas = crate::draw_primitives::Canvas {
                surface: &mut surface,
                bookmark_collector: None,
                link_collector: Some(&mut margin_link_collector),
                tag_collector: None,
                link_run_node_id: None,
                in_marked_content: false,
            };
            let page_content_width = content_area.width.to_f32();
            let any_margin_box_painted = margin_box_renderer.render_page(
                &mut margin_canvas,
                page_idx,
                page_num,
                page_count,
                page_size,
                resolved_margin,
                page_content_width,
                anchor_map,
            );
            page_has_margin_box_paint = any_margin_box_painted;
        }
        // Body occurrences are clipped to the content area rect ONLY
        // when at least one `@page` margin box actually painted on
        // this page — that's the only scenario where a later-painted
        // margin box could visually cover a body link and leave the
        // click target out of sync with the visible text. When no
        // margin box painted, nothing overlays the body layer, and
        // clipping would drop visible body-drawn page furniture
        // (`position: fixed; bottom: 0` page numbers, `margin-top`
        // overflow into the strip, etc.) making it unclickable
        // without any security benefit. `content_area` is the same
        // rect the body background clamp and the margin-box renderer
        // used earlier this iteration — see `resolved_content_area`.
        let raw_body_page = body_link_collector.take_page(page_idx);
        let body_per_page = if page_has_margin_box_paint {
            crate::link::clip_body_occurrences_to_content_area(raw_body_page, &content_area)
        } else {
            raw_body_page
        };
        let margin_per_page = margin_link_collector.take_page(page_idx);
        // Only span_ptrs that are wired into the struct tree via
        // ParagraphRunItem::LinkContent entries should use add_tagged_annotation;
        // others fall back to add_annotation so that Krilla's invariant
        // (every tagged annotation appears in the tag tree) is not violated for
        // link types not yet wired (e.g. InlineBox links).
        let wired_ptrs = tag_collector.as_ref().map(|tc| tc.wired_link_span_ptrs());
        for (ptr, id) in crate::link::emit_link_annotations(
            &mut page,
            &body_per_page,
            &dest_registry,
            wired_ptrs.as_ref(),
        ) {
            link_annot_ids.entry(ptr).or_default().push(id);
        }
        for (ptr, id) in crate::link::emit_link_annotations(
            &mut page,
            &margin_per_page,
            &dest_registry,
            wired_ptrs.as_ref(),
        ) {
            link_annot_ids.entry(ptr).or_default().push(id);
        }
    }

    if let Some(tc) = tag_collector {
        let mut tree = TagTree::new().with_lang(config.lang.clone());
        build_struct_tree(tc, drawables, &link_annot_ids, &mut tree);
        document.set_tag_tree(tree);
    }

    if let Some(c) = bookmark_collector {
        let entries = c.into_entries();
        if !entries.is_empty() {
            document.set_outline(crate::outline::build_outline(&entries));
        }
    }

    document.set_metadata(build_metadata(config, html_title.as_deref()));
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
/// Build the three page-independent skip sets used by `draw_v2_page`.
///
/// Content-area rect for a page: the region inside `resolved_margin`,
/// where body content is laid out. Single source of truth for the three
/// downstream consumers — body background clamp height, GCPM margin-box
/// renderer width, and body-link annotation clip rect — so a future
/// change to how `resolved_margin` subtracts from `page_size` shifts
/// all three together instead of leaving one behind.
fn resolved_content_area(
    page_size: crate::config::PageSize,
    resolved_margin: crate::config::Margin,
) -> crate::draw_primitives::Rect {
    crate::draw_primitives::Rect {
        x: resolved_margin.left.as_pt(),
        y: resolved_margin.top.as_pt(),
        width: (page_size.width - resolved_margin.left - resolved_margin.right).as_pt(),
        height: (page_size.height - resolved_margin.top - resolved_margin.bottom).as_pt(),
    }
}

/// - `transformed_descendants`: every node listed in some
///   `TransformEntry::descendants` — drawn inside that transform's
///   `push_transform / pop` group, so the main per-fragment loop must
///   skip them to avoid double-painting outside the transform.
/// - `clipped_descendants`: every node listed in the
///   `clip_descendants` of an `overflow:hidden|clip` block or table
///   (excluding body and root, see `render_v2` rationale).
/// - `opacity_wrapped_descendants`: every node listed in
///   `opacity_descendants` of a fractional-opacity block (excluding
///   body and root).
///
/// All three depend only on `drawables.*` and are page-independent.
/// `render_v2` builds them once and passes references into every
/// `draw_v2_page` call. (fulgur-v1cm)
fn build_page_skip_sets(
    drawables: &Drawables,
) -> (
    std::collections::BTreeSet<usize>,
    std::collections::BTreeSet<usize>,
    std::collections::BTreeSet<usize>,
) {
    let transformed_descendants: std::collections::BTreeSet<usize> = drawables
        .transforms
        .values()
        .flat_map(|tx| tx.descendants.iter().copied())
        .collect();

    let mut clipped_descendants: std::collections::BTreeSet<usize> =
        std::collections::BTreeSet::new();
    for (&node_id, block) in drawables.block_styles.iter() {
        // Exclude body and root: body's only fragment lives on
        // page 0 (so `draw_under_clip(body)` only fires there) and
        // root is never recorded in `geometry`. Including either
        // would silently blank descendants on pages 1+ via the
        // `clipped_descendants.contains(&node_id)` guard.
        if block.style.has_overflow_clip()
            && Some(node_id) != drawables.body_id
            && Some(node_id) != drawables.root_id
        {
            clipped_descendants.extend(block.clip_descendants.iter().copied());
        }
    }
    for table in drawables.tables.values() {
        if table.style.has_overflow_clip() && !table.clip_descendants.is_empty() {
            clipped_descendants.extend(table.clip_descendants.iter().copied());
        }
    }

    let mut opacity_wrapped_descendants: std::collections::BTreeSet<usize> =
        std::collections::BTreeSet::new();
    for (&node_id, block) in drawables.block_styles.iter() {
        if !block.opacity_descendants.is_empty()
            && Some(node_id) != drawables.body_id
            && Some(node_id) != drawables.root_id
        {
            opacity_wrapped_descendants.extend(block.opacity_descendants.iter().copied());
        }
    }

    (
        transformed_descendants,
        clipped_descendants,
        opacity_wrapped_descendants,
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_v2_page(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    page_index: u32,
    margin_left_pt: f32,
    margin_top_pt: f32,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
    transformed_descendants: &std::collections::BTreeSet<usize>,
    clipped_descendants: &std::collections::BTreeSet<usize>,
    opacity_wrapped_descendants: &std::collections::BTreeSet<usize>,
    page_node_ids: &[usize],
) {
    // Skip sets and per-page dispatch list are built once in
    // `render_v2`. Walking only `page_node_ids` (instead of the full
    // `geometry` map per page) is what fixes the O(P²) regression
    // documented in fulgur-v1cm.

    for &node_id in page_node_ids {
        // SAFETY: `per_page_node_ids` is populated from `geometry`'s keys
        // in `render_v2`, so every `node_id` here is guaranteed to be in
        // `geometry`. Indexing keeps the access on a single line so line
        // coverage isn't dragged down by an else-arm that never fires.
        let geom = &geometry[&node_id];
        // Bookmark anchor: emit on the page where the node's *first*
        // fragment lands. Run BEFORE the `transformed_descendants`
        // skip so headings nested inside a transformed ancestor (e.g.
        // `<div style="transform:..."><h1>`) still register in the PDF
        // outline — record unconditionally using the untransformed y
        // position so the outline destination is stable. (`/Link`
        // annotation rects still receive the transform via the link
        // collector; only the bookmark destination is keyed by raw y.)
        if let Some(first_frag) = geom.fragments.first()
            && first_frag.page_index == page_index
            && let Some(anchor) = drawables.bookmark_anchors.get(&node_id)
            && let Some(c) = canvas.bookmark_collector.as_deref_mut()
        {
            let y_pt = margin_top_pt.as_pt() + first_frag.y.in_pt();
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
        if drawables.inline_box_subtree_skip.contains(&node_id) {
            // PR 8g: belongs to inline-box content (or its descendants)
            // dispatched explicitly by `paragraph::draw_shaped_lines`
            // under an offset transform. Skipping here avoids
            // double-rendering at the body-relative geometry position
            // (which doesn't include the CSS 2.1 §10.8.1 baseline_shift
            // that fulgur applies at convert time, see
            // `convert/inline_root.rs:493`).
            continue;
        }
        // Skip the html root: its bg / border / shadow are painted
        // per-page in the pre-pass (`paint_root_block_v2`) above.
        if Some(node_id) == drawables.root_id {
            continue;
        }

        // Body block bg is painted by the per-page pre-pass (paint_root_block_v2)
        // with content_area_h clamping. Body is never a paragraph/image/svg root,
        // so its inline children render under their own node IDs via the loop below.
        // Skip here to avoid a double-paint of the block background.
        if Some(node_id) == drawables.body_id && drawables.block_styles.contains_key(&node_id) {
            continue;
        }

        // Per-fragment leaf draws.
        for frag in &geom.fragments {
            if frag.page_index != page_index {
                continue;
            }
            let x_pt = margin_left_pt + frag.x.in_pt().to_f32();
            let y_pt = margin_top_pt + frag.y.in_pt().to_f32();

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
            // `overflow: hidden | clip` block: paint bg / border /
            // shadow OUTSIDE the clip, then push the clip path,
            // dispatch self's inner content + every strict descendant
            // INSIDE the clip, then pop (CSS 2.1 §11.1.1 painting
            // order for overflow clipping). Same shape as
            // `draw_under_transform` but with `push_clip_path`.
            //
            // No `!clip_descendants.is_empty()` guard: shared-node_id
            // inner content (inline-root paragraph from
            // `convert::inline_root`, replaced image / svg from
            // `convert::replaced`) lands at the same `node_id` as the
            // wrapper and so produces an empty `clip_descendants`. The
            // clip must still be pushed unconditionally when
            // `has_overflow_clip()` is true so a
            // `<div style="overflow:hidden;width:50px">long text</div>`
            // still gets the text clipped at the 50px box even with
            // no separate descendant NodeIds.
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
            // Table with `overflow: hidden | clip`: same painting
            // order as the block clip arm above (CSS 2.1 §11.1.1).
            // Route through `draw_under_clip_table`, which paints the
            // outer frame outside the clip and dispatches each cell
            // descendant inside.
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
            // `draw_with_opacity` group so descendants paint inside the
            // parent's opacity transparency group (CSS opacity §2).
            // Without this, `<div opacity:0.4><svg>..</svg></div>`
            // paints the svg outside the parent's opacity wrap,
            // dropping the parent's opacity from the svg. (fulgur-gdb9)
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
                canvas,
                node_id,
                geom,
                frag,
                x_pt,
                y_pt,
                drawables,
                geometry,
                margin_left_pt,
                margin_top_pt,
                page_index,
            );
        }
    }

    // Post-pass: paint multicol column rules between columns. Column
    // rules paint AFTER column content so rule lines sit on top of
    // the column contents (CSS Multi-column §4.5). Every per-NodeId
    // payload is already drawn by the main loop above.
    //
    // Skip multicol containers that live inside a transform (whether
    // they ARE a transform key or are a descendant of one): when a
    // multicol container carries a transform, the rules must paint
    // under the composed matrix (CSS Transforms §11 stacking-context
    // rules), so the transform version is dispatched from
    // `draw_under_transform`'s tail while the composed transform is
    // still active. Painting them here unconditionally would emit the
    // rules twice — once in page coords (wrong) and once inside the
    // transform (correct) — and visually misalign the
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
/// PR 8g: dispatch inline-box content (and its descendants) under the
/// caller's `push_transform(translate(off_x, off_y))`. Mirrors the main
/// loop's transform / clip / opacity routing so CSS `transform`,
/// `overflow:hidden`, and fractional opacity on the inline-block are
/// honoured.
///
/// Called by `paragraph::draw_shaped_lines` for `LineItem::InlineBox`.
/// `(x_pt, y_pt)` is the body-relative dispatch position
/// (`margin + body_offset + content_frag.x.in_pt()`); the active
/// translate transform shifts the entire subtree to the inline-flow
/// position computed by the paragraph render path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_inline_box_content(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    content_id: usize,
    content_geom: &crate::pagination_layout::PaginationGeometry,
    content_frag: &crate::pagination_layout::Fragment,
    x_pt: f32,
    y_pt: f32,
    drawables: &Drawables,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
    inline_box_subtree_descendants: &std::collections::BTreeMap<usize, Vec<usize>>,
) {
    if let Some(tx) = drawables.transforms.get(&content_id) {
        draw_under_transform(
            canvas,
            tx,
            content_id,
            content_geom,
            content_frag,
            x_pt,
            y_pt,
            geometry,
            drawables,
            margin_left_pt,
            margin_top_pt,
            page_index,
        );
        return;
    }
    if let Some(block) = drawables.block_styles.get(&content_id)
        && block.style.has_overflow_clip()
    {
        draw_under_clip(
            canvas,
            block,
            content_id,
            content_geom,
            content_frag,
            x_pt,
            y_pt,
            geometry,
            drawables,
            margin_left_pt,
            margin_top_pt,
            page_index,
        );
        return;
    }
    if let Some(block) = drawables.block_styles.get(&content_id)
        && !block.opacity_descendants.is_empty()
    {
        draw_under_opacity(
            canvas,
            block,
            content_id,
            content_geom,
            content_frag,
            x_pt,
            y_pt,
            geometry,
            drawables,
            margin_left_pt,
            margin_top_pt,
            page_index,
        );
        return;
    }
    // No wrapper effect on the inline-box content itself: dispatch at
    // body-relative `(x_pt, y_pt)` and walk its strict descendants
    // (`inline_box_subtree_descendants[content_id]`) at the same body-
    // relative frame. The caller's translate transform shifts everything.
    dispatch_fragment(
        canvas,
        content_id,
        content_geom,
        content_frag,
        x_pt,
        y_pt,
        drawables,
        geometry,
        margin_left_pt,
        margin_top_pt,
        page_index,
    );
    if let Some(descs) = inline_box_subtree_descendants.get(&content_id) {
        for &desc_id in descs {
            let Some(desc_geom) = geometry.get(&desc_id) else {
                continue;
            };
            let Some(desc_frag) = desc_geom
                .fragments
                .iter()
                .find(|f| f.page_index == page_index)
            else {
                continue;
            };
            let desc_x_pt =
                margin_left_pt + drawables.body_offset_pt.0.to_f32() + desc_frag.x.in_pt().to_f32();
            let desc_y_pt =
                margin_top_pt + drawables.body_offset_pt.1.to_f32() + desc_frag.y.in_pt().to_f32();
            dispatch_fragment(
                canvas,
                desc_id,
                desc_geom,
                desc_frag,
                desc_x_pt,
                desc_y_pt,
                drawables,
                geometry,
                margin_left_pt,
                margin_top_pt,
                page_index,
            );
        }
    }
}

/// Returns `true` if any line item in `entry` is a text run or inline image
/// with a non-`None` link. Used to decide whether to activate per-run tagging
/// mode in `dispatch_fragment` rather than the normal paragraph-level tagging.
fn para_has_link_runs(entry: &crate::drawables::ParagraphEntry) -> bool {
    entry.lines.iter().any(|line| {
        line.items.iter().any(|item| match item {
            crate::paragraph::LineItem::Text(run) => run.link.is_some(),
            crate::paragraph::LineItem::Image(img) => img.link.is_some(),
            crate::paragraph::LineItem::InlineBox(_) => false,
        })
    })
}

/// The struct-tree node id that per-run link tags for `node_id` should nest
/// under, or `None` when the node has no home in the tag tree — in which case
/// per-run tagging MUST be skipped. Otherwise `RunRegionTracker` records a
/// `ParagraphRunItem::LinkContent` that marks the span "wired", so
/// `emit_link_annotations` uses `add_tagged_annotation`, but `build_struct_tree`
/// only walks ids present in `drawables.semantics`; the annotation's OBJR never
/// enters the StructTree and Krilla panics during serialization
/// ("annotation identifier ... doesn't appear in the tag tree").
///
/// A semantic `<li>` remaps to its synthetic LBody id. Any other node must
/// itself be present in `drawables.semantics`. A CSS list item or inline root
/// whose element has no `SemanticEntry` — `<a>`, a custom element, `<body>`,
/// etc., for which `classify_element` returns `None` — yields `None` here and
/// keeps its link annotation untagged.
fn run_tag_target(
    drawables: &Drawables,
    node_id: crate::drawables::NodeId,
) -> Option<crate::drawables::NodeId> {
    drawables.li_lbody_ids.get(&node_id).copied().or_else(|| {
        drawables
            .semantics
            .contains_key(&node_id)
            .then_some(node_id)
    })
}

/// Collect the plain-text title from a paragraph's shaped lines.
///
/// Returns the concatenated text of all `Text` run items across all lines.
/// Used to populate the `/T` (Title) attribute on heading tags required by
/// PDF/UA-1.
fn extract_heading_title(para: &crate::drawables::ParagraphEntry) -> String {
    para.lines
        .iter()
        .flat_map(|line| line.items.iter())
        .filter_map(|item| match item {
            crate::paragraph::LineItem::Text(run) => Some(run.text.as_ref()),
            _ => None,
        })
        .collect()
}

/// Compute the `/T` (Title) attribute value for a heading tag, honouring
/// `ParagraphEntry.visible`.
///
/// Returns `None` when the paragraph is invisible (CSS `visibility: hidden`
/// or `visibility: collapse` — stored as `visible = false`) so that the
/// heading's text does not leak into the tag tree while the paint path
/// suppresses it (`draw_paragraph_inner_paint` returns early for
/// `!entry.visible`). Also returns `None` for a visible-but-empty title,
/// which keeps `Tag::Hn` from emitting a zero-length `/T`.
///
/// Called from both title-extraction sinks — `try_start_tagged` (the normal
/// paragraph tagging path) and the `build_struct_tree` per-run backfill
/// (heading whose content is per-run tagged because of an inner link) — so
/// the same `visible` gate applies to every path that can produce a
/// heading `/T` attribute.
fn heading_title_of(para: &crate::drawables::ParagraphEntry) -> Option<String> {
    if !para.visible {
        return None;
    }
    let title = extract_heading_title(para);
    (!title.is_empty()).then_some(title)
}

/// Start a Krilla tagged content sequence for a paragraph-bearing node when
/// tagging is enabled and the node has a P / H / Span semantic entry.
///
/// Returns `Some((tag, id))` on success so that `finish_tagged` can close it.
/// Returns `None` when tagging is disabled, the node has no recognised
/// semantic entry, the node carries no paragraph content (pure containers
/// must not call `start_tagged` — it is not nestable and would panic on a
/// second call before `end_tagged`), OR the canvas is already inside an
/// outer marked-content sequence (`canvas.in_marked_content == true`). The
/// last case fires when a tagged inline-root block dispatches an inline-box
/// whose content would itself want a P/Span/LBody tag; nested opens would
/// panic in Krilla (`can't start marked content twice`), so the inner tag is
/// dropped and its sub-tree renders as a graceful, untagged extension of the
/// outer sequence. Link annotations under the skipped tag still land in the
/// PDF via `emit_link_annotations`'s unwired-fallback path (`add_annotation`).
fn try_start_tagged(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    node_id: usize,
    drawables: &Drawables,
) -> Option<(
    usize, // record_id (TagCollector.record() に渡す NodeId)
    crate::tagging::PdfTag,
    krilla::tagging::Identifier,
    Option<String>,
)> {
    canvas.tag_collector.as_ref()?;
    if canvas.in_marked_content {
        return None;
    }
    let semantic = drawables.semantics.get(&node_id)?;
    let result = match &semantic.tag {
        crate::tagging::PdfTag::P | crate::tagging::PdfTag::Span => {
            use krilla::tagging::{ContentTag, SpanTag};
            let id = canvas
                .surface
                .start_tagged(ContentTag::Span(SpanTag::empty()));
            Some((node_id, semantic.tag.clone(), id, None))
        }
        crate::tagging::PdfTag::H { .. } => {
            let heading_title = drawables
                .paragraphs
                .get(&node_id)
                .and_then(heading_title_of);
            use krilla::tagging::{ContentTag, SpanTag};
            let id = canvas
                .surface
                .start_tagged(ContentTag::Span(SpanTag::empty()));
            Some((node_id, semantic.tag.clone(), id, heading_title))
        }
        crate::tagging::PdfTag::Li => {
            // inline-root li: コンテンツを lbody_id で記録（LBody 配下に収める）
            let &lbody_id = drawables.li_lbody_ids.get(&node_id)?;
            use krilla::tagging::{ContentTag, SpanTag};
            let id = canvas
                .surface
                .start_tagged(ContentTag::Span(SpanTag::empty()));
            Some((lbody_id, crate::tagging::PdfTag::LBody, id, None))
        }
        _ => None,
    };
    if result.is_some() {
        canvas.in_marked_content = true;
    }
    result
}

/// Close a tagged content sequence opened by `try_start_tagged` and record
/// the resulting `Identifier` in the `TagCollector` for StructTree assembly.
///
/// No-op when `tag_info` is `None` (tagging disabled or not applicable).
///
/// # Invariant
/// `tag_info` must only hold values produced by `try_start_tagged` on the
/// same `canvas`. `try_start_tagged` returns `None` when `tag_collector` is
/// absent, so if `tag_info` is `Some`, `canvas.tag_collector` is guaranteed
/// to be `Some` as well.
fn finish_tagged(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    tag_info: Option<(
        usize, // record_id
        crate::tagging::PdfTag,
        krilla::tagging::Identifier,
        Option<String>,
    )>,
) {
    if let Some((record_id, tag, id, heading_title)) = tag_info {
        canvas.surface.end_tagged();
        canvas.in_marked_content = false;
        canvas
            .tag_collector
            .as_mut()
            .expect("tag_collector is Some when tag_info is Some")
            .record(record_id, tag, id, heading_title);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_fragment(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    node_id: usize,
    geom: &crate::pagination_layout::PaginationGeometry,
    frag: &crate::pagination_layout::Fragment,
    x_pt: f32,
    y_pt: f32,
    drawables: &Drawables,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
) {
    if let Some(table) = drawables.tables.get(&node_id) {
        draw_table_v2(canvas, table, geom, x_pt, y_pt, frag);
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
            node_id,
            li,
            block_for_li,
            para_for_li,
            x_pt,
            y_pt,
            frag,
            &geom.fragments,
            page_index,
            is_split,
            drawables,
            geometry,
            margin_left_pt,
            margin_top_pt,
        );
        return;
    }
    // fulgur-6q5 Task 8: when a multicol container splits a paragraph
    // across columns, `convert_multicol_paragraph_slices` records a
    // per-source-NodeId override in `drawables.paragraph_slices`. The
    // standard `paragraphs[node_id]` path would render every line at
    // the source's body-relative position (column 0 only); the override
    // suppresses that path and paints one slice per non-empty column at
    // the slice origin (computed from the multicol container's
    // border-box top-left + per-column line frame).
    let has_paragraph_slices = drawables.paragraph_slices.contains_key(&node_id);
    // Block + inner content (paragraph / image / svg) sharing the
    // same node_id: combine into one `draw_with_opacity` group. See
    // `draw_block_with_inner_content` for the v1 mirror.
    if let Some(block) = drawables.block_styles.get(&node_id) {
        // Suppress the inline-paragraph branch of
        // `draw_block_with_inner_content` when paragraph_slices owns
        // this NodeId (Case A: container is itself the inline root, so
        // it carries both `block_styles` and `paragraphs` entries — we
        // still need the block's bg / border / shadow but the lines
        // come from the slice override painted afterward).
        let para_for_block = if has_paragraph_slices {
            None
        } else {
            drawables.paragraphs.get(&node_id)
        };
        let img_for_block = drawables.images.get(&node_id);
        let svg_for_block = drawables.svgs.get(&node_id);
        if para_for_block.is_some() || img_for_block.is_some() || svg_for_block.is_some() {
            let run_tag_node_id = run_tag_target(drawables, node_id);
            let use_run_tagging = para_for_block
                .map(|p| canvas.tag_collector.is_some() && para_has_link_runs(p))
                .unwrap_or(false)
                && run_tag_node_id.is_some();
            let tag_info = if use_run_tagging {
                canvas.link_run_node_id = run_tag_node_id;
                None
            } else if para_for_block.is_some() {
                try_start_tagged(canvas, node_id, drawables)
            } else {
                None
            };
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
                drawables,
                geometry,
                margin_left_pt,
                margin_top_pt,
            );
            if has_paragraph_slices {
                paint_multicol_paragraph_slices(
                    canvas,
                    drawables,
                    geometry,
                    node_id,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
            }
            finish_tagged(canvas, tag_info);
            if use_run_tagging {
                canvas.link_run_node_id = None;
            }
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
    if has_paragraph_slices {
        paint_multicol_paragraph_slices(
            canvas,
            drawables,
            geometry,
            node_id,
            margin_left_pt,
            margin_top_pt,
            page_index,
        );
        return;
    }
    if let Some(para) = drawables.paragraphs.get(&node_id) {
        let run_tag_node_id = run_tag_target(drawables, node_id);
        let use_run_tagging =
            canvas.tag_collector.is_some() && para_has_link_runs(para) && run_tag_node_id.is_some();
        let tag_info = if use_run_tagging {
            canvas.link_run_node_id = run_tag_node_id;
            None
        } else {
            try_start_tagged(canvas, node_id, drawables)
        };
        draw_paragraph_v2(
            canvas,
            para,
            x_pt,
            y_pt,
            &geom.fragments,
            page_index,
            is_split,
            drawables,
            geometry,
            margin_left_pt,
            margin_top_pt,
        );
        finish_tagged(canvas, tag_info);
        if use_run_tagging {
            canvas.link_run_node_id = None;
        }
    }
}

/// fulgur-6q5 Task 8: paint a paragraph that `multicol_layout`
/// distributed across columns. The standard `draw_paragraph_v2` path
/// renders every line at the source's body-relative position (column 0
/// only); this override paints one slice per non-empty column at the
/// slice origin recorded by `convert_multicol_paragraph_slices`.
///
/// The slice's `origin_pt` is **multicol container border-box-relative**
/// in PDF pt (per the `ParagraphSlice` doc on `drawables.rs`). Resolve
/// the container's body-relative page position via
/// `geometry[container_node_id]` — same shape as the main loop's
/// `(margin_left_pt + frag.x.in_pt(), margin_top_pt + frag.y.in_pt())`
/// — and add the slice origin. Honour the source paragraph's `visible`
/// and `opacity` exactly like `draw_paragraph_v2`. `lines` are
/// pre-rebased (Task 7) so each slice's first line has
/// `baseline = ascent` from the slice top — no further rebase here.
///
/// **Case A opacity limitation (fulgur-6q5 follow-up):** when the
/// multicol container itself has `opacity:N` (Case A:
/// `<div opacity:0.5; column-count:2>text</div>`),
/// `paragraphs[node_id].opacity` is forced to 1.0 by `extract_paragraph`
/// (because `needs_block` is true), so this helper applies no alpha to
/// the slice text. The container's block layer already paints at
/// `block.opacity`, but slices are emitted **outside** that
/// `draw_with_opacity` group — so text loses block opacity. This is a
/// strict improvement over pre-Task-8 (which kept all text in col 0).
/// Revisit if Task 10 VRT or downstream usage exposes a regression;
/// the fix likely requires invoking the slice paint inside the block's
/// opacity group, not at the dispatch level.
///
/// **Multi-page container handling (fulgur-6q5 Fix 4):** when the
/// multicol container straddles a page boundary, the container has
/// multiple fragments and `slice.origin_pt` is measured from the
/// **container border-box top** (i.e. the start of the FIRST
/// fragment), not from the current page's fragment. The helper
/// detects the multi-fragment case (`container_geom.fragments.len() >
/// 1`), computes `consumed = sum of prior fragments' heights`,
/// rebases each slice's y into the current fragment's frame, and
/// paints only the slices that fit *wholly* within the visible strip
/// on this page. Slices that straddle a page boundary are NOT
/// painted on either page — mid-line page splitting of multicol
/// slices is out of scope (fulgur-6q5 follow-up).
///
/// The single-fragment fast path (the common case) preserves the
/// pre-Fix-4 behaviour: paint every slice at `container_origin +
/// origin_pt`, no rebase, no visibility filter. This is required
/// because the per-fragment `height` excludes the container's own
/// padding, so a per-slice visibility filter would false-positive on
/// padded single-page containers — slice `origin_pt.y + size_pt.1`
/// legitimately exceeds `target_frag.height` (in pt) when the
/// container has padding (see VRT
/// `multicol-inline-root-split-case-a`).
fn paint_multicol_paragraph_slices(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    drawables: &Drawables,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    source_node_id: usize,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
) {
    use crate::draw_primitives::draw_with_opacity;
    let Some(slices_entry) = drawables.paragraph_slices.get(&source_node_id) else {
        return;
    };
    let para = drawables.paragraphs.get(&source_node_id);
    if let Some(p) = para
        && !p.visible
    {
        return;
    }
    let opacity = para.map(|p| p.opacity).unwrap_or(1.0);
    // Resolve the container's body-relative position on this page from
    // the geometry table — same convention as `dispatch_fragment`'s
    // caller. Skip when the container has no fragment on `page_index`
    // (e.g. earlier-page-only containers shouldn't repaint here).
    let Some(container_geom) = geometry.get(&slices_entry.container_node_id) else {
        return;
    };
    let Some(target_pos) = container_geom
        .fragments
        .iter()
        .position(|f| f.page_index == page_index)
    else {
        return;
    };
    let target_frag = &container_geom.fragments[target_pos];

    // fulgur-6q5 Fix 4: mirror `paint_multicol_rule_for_page`'s
    // partitioning. `consumed` = sum of prior fragments' heights, so
    // a slice's effective top in the current fragment frame is
    // `slice.origin_pt.1 - consumed`. `cutoff` is the visible strip's
    // height on this page.
    let consumed: f32 = container_geom.fragments[..target_pos]
        .iter()
        .map(|f| f.height)
        .sum::<crate::units::Px>()
        .in_pt()
        .to_f32();
    let cutoff = target_frag.height.in_pt().to_f32();

    let container_x_pt = margin_left_pt.as_pt() + target_frag.x.in_pt();
    let container_y_pt = margin_top_pt.as_pt() + target_frag.y.in_pt();

    // Single-fragment containers (the common case) keep the original
    // behaviour: paint every slice at `container_origin + origin_pt`,
    // unconditionally. The container's fragment height is reported in
    // a "visible strip" frame that excludes the container's own
    // padding, so a per-slice `slice_bottom <= cutoff` filter would
    // false-positive on padded single-page containers (e.g. VRT
    // `multicol-inline-root-split-case-a`: padding 40px, cutoff
    // ≈ 88pt, but valid slices reach origin_pt.1 + size_pt.1 ≈ 105pt).
    //
    // `is_split()` (false when `is_repeat=true`) is the right gate, but
    // NOT because multicol can only ever have one fragment per page. It
    // used to say `position: fixed` was the sole producer of
    // `is_repeat=true`, so `is_split() == fragments.len() > 1` here; a
    // repeated table header can hold a multicol container too, which
    // makes that premise false (fulgur-naj7.12 measured `is_repeat=true`
    // with one fragment per page for `#mc` inside a `<th>`).
    //
    // The predicate is still correct under the real rule: `is_repeat`
    // fragments are *copies* of the whole container, not slices of it,
    // so each page must draw the full column content and partitioning
    // would be wrong. The spike confirmed the rendered output — the
    // header's paragraphs appear complete on every page
    // (`tests/naj7_spike_is_repeat.rs`).
    let needs_partition = container_geom.is_split();
    let run_tag_node_id = run_tag_target(drawables, source_node_id);
    let use_run_tagging = canvas.tag_collector.is_some()
        && drawables
            .paragraphs
            .get(&source_node_id)
            .is_some_and(para_has_link_runs)
        && run_tag_node_id.is_some();
    if use_run_tagging {
        canvas.link_run_node_id = run_tag_node_id;
    }
    draw_with_opacity(canvas, opacity, |canvas| {
        for slice in &slices_entry.slices {
            let slice_top = slice.origin_pt.1.to_f32() - consumed;
            let slice_bottom = slice_top + slice.size_pt.1.to_f32();
            if needs_partition {
                // Multi-fragment container: rebase + visibility filter.
                // Skip slices that don't intersect this page's visible
                // range. Also skip slices that straddle the page
                // boundary — mid-line page splitting of multicol
                // slices is out of scope (fulgur-6q5 follow-up).
                // Skipping prevents off-page replay; the trade-off is
                // a slice that crosses a fragment break disappears on
                // every page it would have crossed. Revisit once
                // split-aware rendering of multicol slices lands.
                let above = slice_bottom <= 0.0;
                let below = slice_top >= cutoff;
                let straddles = slice_top < 0.0 || slice_bottom > cutoff;
                if above || below || straddles {
                    continue;
                }
            }
            let abs_x = container_x_pt + slice.origin_pt.0;
            let abs_y = container_y_pt + slice_top.as_pt();
            crate::paragraph::draw_shaped_lines(canvas, &slice.lines, abs_x, abs_y, None);
        }
    });
    if use_run_tagging {
        canvas.link_run_node_id = None;
    }
}

/// Collect the inline-box subtrees that paragraphs drawn *inside a scope*
/// already paint, so the scope's own descendant walk does not paint them a
/// second time.
///
/// `scope_ids` must yield the scope owner plus every id in the walk's
/// descendant list: any paragraph in that subtree is drawn inside the
/// scope, so whatever inline boxes it carries are its to paint.
///
/// This is deliberately narrower than `Drawables::inline_box_subtree_skip`.
/// That set is global, which is right for the top-level dispatch loop —
/// every member is painted by *some* paragraph — but wrong inside a scope.
/// A scoped inline-block's own subtree lands in the global set only because
/// the inline-block is an inline box in its *parent's* paragraph; when the
/// inline-block has no paragraph of its own, as in
///
/// ```html
/// <div style="display:inline-block;overflow:hidden">
///   <div style="background:red"></div>
/// </div>
/// ```
///
/// the scope walk is the only thing that would paint that child. Skipping
/// it there drops the content outright — the regression PR #677 shipped by
/// reaching for the global set here (fulgur-49xw).
fn paragraph_owned_inline_boxes(
    scope_ids: impl IntoIterator<Item = usize>,
    drawables: &Drawables,
) -> std::collections::BTreeSet<usize> {
    let mut skip = std::collections::BTreeSet::new();
    for id in scope_ids {
        let Some(para) = drawables.paragraphs.get(&id) else {
            continue;
        };
        for item in para.lines.iter().flat_map(|line| line.items.iter()) {
            let crate::paragraph::LineItem::InlineBox(ib) = item else {
                continue;
            };
            let Some(ib_id) = ib.node_id else {
                continue;
            };
            // `paragraph::draw_shaped_lines` hands the box to
            // `dispatch_inline_box_content`, which paints the box itself
            // and walks `inline_box_subtree_descendants[ib_id]`.
            skip.insert(ib_id);
            if let Some(descs) = drawables.inline_box_subtree_descendants.get(&ib_id) {
                skip.extend(descs.iter().copied());
            }
        }
    }
    skip
}

/// Push the transform onto the surface + link collector, dispatch the
/// wrapper node's own payload and every descendant fragment that lands
/// on `page_index`, then pop:
///
/// ```text
/// canvas.surface.push_transform(matrix);
/// inner.draw(canvas, x, y, ...);
/// canvas.surface.pop();
/// ```
///
/// The link collector also receives the transform so `/Link`
/// annotation rects are mapped into device space.
#[allow(clippy::too_many_arguments)]
fn draw_under_transform(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
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
    // Apply the transform around the box's transform-origin
    // (CSS Transforms §12): the effective matrix is
    //   T(x + ox, y + oy) · M · T(-(x + ox), -(y + oy))
    let ox = x_pt + tx.origin.x.to_f32();
    let oy = y_pt + tx.origin.y.to_f32();
    use crate::draw_primitives::Affine2D;
    let full = Affine2D::translation(ox.as_pt(), oy.as_pt())
        * tx.matrix
        * Affine2D::translation((-ox).as_pt(), (-oy).as_pt());

    if let Some(lc) = canvas.link_collector.as_deref_mut() {
        lc.push_transform(full);
    }
    canvas.surface.push_transform(&full.to_krilla());

    // Dispatch the wrapper's own payload first (the same NodeId
    // carries both the transform and its inner content), before
    // recursing into descendants.
    dispatch_fragment(
        canvas,
        node_id,
        geom,
        frag,
        x_pt,
        y_pt,
        drawables,
        geometry,
        margin_left_pt,
        margin_top_pt,
        page_index,
    );

    // Then dispatch every strict descendant on this page. Each
    // descendant's fragment carries its own (x, y) in untransformed
    // local coordinates — the surface transform applies on top so the
    // descendants paint under their ancestor's matrix.
    //
    // Descendants that have their OWN `TransformEntry` recurse into
    // `draw_under_transform` so their matrix composes with the outer
    // push (nested transforms compose right-to-left; each level
    // pushes its own matrix — CSS Transforms §11). Without this
    // recursion the inner transform would be silently dropped,
    // breaking `<div style="transform:rotate"><div style="transform:scale">`
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
    // Inline boxes carried by paragraphs inside this transform are painted
    // by those paragraphs' `LineItem::InlineBox` arms. Re-dispatching them
    // here paints them twice — and because the duplicate re-enters
    // `draw_under_transform` for a nested transformed inline-block, the
    // cost doubles per nesting level rather than staying constant.
    let paragraph_ib_skip = paragraph_owned_inline_boxes(
        std::iter::once(node_id).chain(tx.descendants.iter().copied()),
        drawables,
    );
    for &desc_id in &tx.descendants {
        if nested_skip.contains(&desc_id)
            || clip_skip.contains(&desc_id)
            || opacity_skip.contains(&desc_id)
            || paragraph_ib_skip.contains(&desc_id)
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
            let desc_x = margin_left_pt + desc_frag.x.in_pt().to_f32();
            let desc_y = margin_top_pt + desc_frag.y.in_pt().to_f32();
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
                    canvas,
                    desc_id,
                    desc_geom,
                    desc_frag,
                    desc_x,
                    desc_y,
                    drawables,
                    geometry,
                    margin_left_pt,
                    margin_top_pt,
                    page_index,
                );
            }
        }
    }

    // Paint multicol column rules for any multicol container in this
    // transform's direct scope. When the multicol container carries
    // a transform, the column rules must paint under the composed
    // matrix (CSS Transforms §11 stacking-context rules), so this
    // post-pass runs inside the `push_transform / pop` group instead
    // of in page coordinates.
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
/// lands on `page_index`, then pop. Painting order per
/// CSS 2.1 §11.1.1 for overflow clipping:
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
/// The wrapper's own inner content (paragraph / image / svg sharing
/// the same `node_id`) is dispatched as part of the clipped region,
/// then strict descendants iterate inside the clip. The block
/// dispatcher's "shared node_id" combined helper
/// (`draw_block_with_inner_content`) already handles the outer
/// opacity wrap when used; here we replicate that ordering manually
/// — bg/border outside clip, inner content + descendants inside clip,
/// all wrapped in one opacity group.
#[allow(clippy::too_many_arguments)]
fn draw_under_clip(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
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
    use crate::draw_primitives::draw_with_opacity;

    let total_width = block
        .layout_size
        .map(|s| s.width.to_f32())
        .unwrap_or_else(|| frag.width.in_pt().to_f32());
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
            if is_split && frag.height > crate::units::Px::ZERO {
                frag.height.in_pt().to_f32()
            } else {
                s.height.to_f32()
            }
        })
        .unwrap_or_else(|| frag.height.in_pt().to_f32());

    let para_for_block = drawables.paragraphs.get(&node_id);
    let img_for_block = drawables.images.get(&node_id);
    let svg_for_block = drawables.svgs.get(&node_id);
    let inner_inset = block.style.content_inset();

    // When this node is a list-item with overflow clip, honour the
    // list-item painting order: outer opacity uses `list_item.opacity`
    // (the body block inside is built with default opacity=1.0 in
    // `convert::list_item::build_list_item_body`, so `block.opacity`
    // here would silently drop CSS opacity), and the marker draws
    // before `push_clip_path` (markers sit at negative x outside the
    // body box, so they must not be clipped). Without this,
    // `<li style="overflow:hidden">` loses its marker entirely and
    // any opacity set on the `<li>` is ignored. (PR #310 Devin)
    let list_item = drawables.list_items.get(&node_id);
    let opacity = list_item.map_or(block.opacity, |li| li.opacity);

    draw_with_opacity(canvas, opacity, |canvas| {
        // List-item marker paints first, OUTSIDE the clip — the
        // marker is emitted before the body's bg / border / shadow.
        // Markers sit at negative x relative to (x_pt, y_pt), so they
        // must also stay outside the clip path pushed below.
        if let Some(li) = list_item
            && li.visible
        {
            draw_list_item_marker_tagged(canvas, li, node_id, drawables, x_pt, y_pt);
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
            crate::draw_primitives::draw_block_border(
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
        let clip_pushed = if let Some(clip_path) =
            crate::draw_primitives::compute_overflow_clip_path(
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
            let run_tag_node_id = run_tag_target(drawables, node_id);
            let use_run_tagging = canvas.tag_collector.is_some()
                && para_has_link_runs(p)
                && run_tag_node_id.is_some();
            let tag_info = if use_run_tagging {
                canvas.link_run_node_id = run_tag_node_id;
                None
            } else {
                try_start_tagged(canvas, node_id, drawables)
            };
            draw_paragraph_inner_paint(
                canvas,
                p,
                inner_x,
                inner_y,
                &geom.fragments,
                page_index,
                is_split,
                drawables,
                geometry,
                margin_left_pt,
                margin_top_pt,
            );
            finish_tagged(canvas, tag_info);
            if use_run_tagging {
                canvas.link_run_node_id = None;
            }
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
        // Inline boxes carried by paragraphs inside this clip are painted
        // by those paragraphs' `LineItem::InlineBox` arms. Re-dispatching
        // them here paints them twice — and because the duplicate re-enters
        // `draw_under_clip` for a nested clipped inline-block, the cost
        // doubles per nesting level rather than staying constant.
        let paragraph_ib_skip = paragraph_owned_inline_boxes(
            std::iter::once(node_id).chain(block.clip_descendants.iter().copied()),
            drawables,
        );
        for &desc_id in &block.clip_descendants {
            if transform_skip.contains(&desc_id)
                || nested_clip_skip.contains(&desc_id)
                || nested_opacity_skip.contains(&desc_id)
                || paragraph_ib_skip.contains(&desc_id)
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
                let desc_x = margin_left_pt + desc_frag.x.in_pt().to_f32();
                let desc_y = margin_top_pt + desc_frag.y.in_pt().to_f32();
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
                        canvas,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        drawables,
                        geometry,
                        margin_left_pt,
                        margin_top_pt,
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
/// The whole subtree must live in a single opacity transparency group
/// (CSS opacity §2):
///
/// ```text
/// draw_with_opacity(canvas, self.opacity, |c| {
///     bg + border + shadow at (x, y, total_w, total_h);
///     for pc in self.children { pc.child.draw(c, x + pc.x, y + pc.y, ..); }
/// });
/// ```
///
/// A single transparency-group XObject covers the whole subtree; a
/// flat dispatch without scope tracking would emit the block's own
/// paint inside opacity but every descendant outside, dropping the
/// parent's opacity on those descendants. Mirrors `draw_under_clip`
/// minus the `push_clip_path` / `pop` calls and the list-item marker
/// arm (a list-item with opacity uses `draw_list_item_with_block`,
/// not this path, since list-item markers are owned by `ListItemEntry`
/// rather than `BlockEntry`).
#[allow(clippy::too_many_arguments)]
fn draw_under_opacity(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
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
    use crate::draw_primitives::draw_with_opacity;

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
                canvas,
                node_id,
                geom,
                frag,
                x_pt,
                y_pt,
                drawables,
                geometry,
                margin_left_pt,
                margin_top_pt,
                page_index,
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
                .map(|s| s.width.to_f32())
                .unwrap_or_else(|| frag.width.in_pt().to_f32());
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
                    if is_split && frag.height > crate::units::Px::ZERO {
                        frag.height.in_pt().to_f32()
                    } else {
                        s.height.to_f32()
                    }
                })
                .unwrap_or_else(|| frag.height.in_pt().to_f32());
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
                crate::draw_primitives::draw_block_border(
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
                let run_tag_node_id = run_tag_target(drawables, node_id);
                let use_run_tagging = canvas.tag_collector.is_some()
                    && para_has_link_runs(p)
                    && run_tag_node_id.is_some();
                let tag_info = if use_run_tagging {
                    canvas.link_run_node_id = run_tag_node_id;
                    None
                } else {
                    try_start_tagged(canvas, node_id, drawables)
                };
                draw_paragraph_inner_paint(
                    canvas,
                    p,
                    inner_x,
                    inner_y,
                    &geom.fragments,
                    page_index,
                    is_split,
                    drawables,
                    geometry,
                    margin_left_pt,
                    margin_top_pt,
                );
                finish_tagged(canvas, tag_info);
                if use_run_tagging {
                    canvas.link_run_node_id = None;
                }
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
        // Same narrowing as `draw_under_clip`: paragraphs inside this
        // opacity group paint their own inline boxes, so the walk must not
        // paint them again. Unlike the clip and transform paths this
        // duplicate does not compound — `draw_under_opacity` has no
        // self-recursive arm — but it is still a double paint.
        let paragraph_ib_skip = paragraph_owned_inline_boxes(
            std::iter::once(node_id).chain(block.opacity_descendants.iter().copied()),
            drawables,
        );
        for &desc_id in &block.opacity_descendants {
            if transform_skip.contains(&desc_id)
                || nested_clip_skip.contains(&desc_id)
                || nested_opacity_skip.contains(&desc_id)
                || paragraph_ib_skip.contains(&desc_id)
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
                let desc_x = margin_left_pt + desc_frag.x.in_pt().to_f32();
                let desc_y = margin_top_pt + desc_frag.y.in_pt().to_f32();
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
                        canvas,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        drawables,
                        geometry,
                        margin_left_pt,
                        margin_top_pt,
                        page_index,
                    );
                }
            }
        }
    });
}

/// Paint multicol column-rule lines on `page_index` for one
/// `MulticolRuleEntry`. Partitions `entry.groups` by accumulating the
/// container's per-page heights so each page only emits the rule
/// segments that fit on it.
fn paint_multicol_rule_for_page(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::MulticolRuleEntry,
    container_geom: &crate::pagination_layout::PaginationGeometry,
    margin_left_pt: f32,
    margin_top_pt: f32,
    page_index: u32,
) {
    use crate::draw_primitives::stroke_line;
    use crate::units::{F32Units, Pt};

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

    let consumed = container_geom.fragments[..target_pos]
        .iter()
        .map(|f| f.height)
        .sum::<crate::units::Px>()
        .in_pt();
    let cutoff = target_frag.height.in_pt();

    let x_base = margin_left_pt.as_pt() + target_frag.x.in_pt();
    let y_base = margin_top_pt.as_pt() + target_frag.y.in_pt();

    for group in &entry.groups {
        if group.n < 2 || group.col_heights.len() != group.n as usize {
            continue;
        }
        let group_top = group.y_offset - consumed;
        let max_h = group.col_heights.iter().copied().fold(Pt::ZERO, Pt::max);
        let group_bottom = group_top + max_h;
        if group_bottom <= Pt::ZERO || group_top >= cutoff {
            continue;
        }
        let visible_top = group_top.max(Pt::ZERO);
        let y_top = y_base + visible_top;
        // Subtract the portion of each column already painted on
        // prior pages BEFORE clamping to the visible strip on this
        // page. Without this, a column rule segment whose group
        // straddles a page boundary extends past the actual visible
        // column content.
        let consumed_above = (visible_top - group_top).max(Pt::ZERO);
        let visible_h = (group_bottom.min(cutoff) - visible_top).max(Pt::ZERO);
        for i in 0..(group.n as usize - 1) {
            let h_left = (group.col_heights[i] - consumed_above)
                .max(Pt::ZERO)
                .min(visible_h);
            let h_right = (group.col_heights[i + 1] - consumed_above)
                .max(Pt::ZERO)
                .min(visible_h);
            if h_left <= Pt::ZERO || h_right <= Pt::ZERO {
                continue;
            }
            let rule_x = x_base
                + group.x_offset
                + (i as f32 + 1.0) * group.col_w
                + i as f32 * group.gap
                + group.gap / 2.0;
            let y_bot = y_top + h_left.min(h_right);
            stroke_line(
                canvas,
                rule_x.to_f32(),
                y_top.to_f32(),
                rule_x.to_f32(),
                y_bot.to_f32(),
                stroke.clone(),
            );
        }
    }
    canvas.surface.set_stroke(None);
}

/// Build the krilla stroke for the configured column-rule spec.
/// Returns `None` when the rule is invisible (style `None` or
/// non-positive width).
fn build_multicol_stroke(
    rule: &crate::column_css::ColumnRuleSpec,
) -> Option<krilla::paint::Stroke> {
    use crate::column_css::ColumnRuleStyle;
    use crate::draw_primitives::{alpha_to_opacity, colored_stroke};

    if rule.width <= crate::units::Pt::ZERO || rule.style == ColumnRuleStyle::None {
        return None;
    }
    let opacity = alpha_to_opacity(rule.color[3]);
    let base = colored_stroke(&rule.color, rule.width.to_f32(), opacity);
    let w = rule.width.to_f32();
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

/// v2 image draw. Mirrors `image::ImageRender::draw` but operates on
/// the side-channel `ImageEntry` data; the `width`/`height` are the
/// CSS-resolved size in pt that fulgur stores on the original
/// `ImageRender`.
fn draw_image_v2(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::ImageEntry,
    x: f32,
    y: f32,
) {
    use crate::draw_primitives::draw_with_opacity;
    draw_with_opacity(canvas, entry.opacity, |canvas| {
        draw_image_inner_paint(canvas, entry, x, y);
    });
}

/// Image paint without `draw_with_opacity` wrapper. Used by
/// `draw_block_with_inner_content` so a `<img>` whose wrapping
/// inline-root block shares its node_id (`convert::replaced`)
/// composes with the block bg/border under one opacity group
/// (CSS opacity §2 — single transparency group for the combined
/// paint).
fn draw_image_inner_paint(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
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
    let Some(size) = krilla::geom::Size::from_wh(entry.width.to_f32(), entry.height.to_f32())
    else {
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

/// v2 SVG draw. Mirrors `svg::SvgRender::draw`.
fn draw_svg_v2(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::SvgEntry,
    x: f32,
    y: f32,
) {
    use crate::draw_primitives::draw_with_opacity;
    draw_with_opacity(canvas, entry.opacity, |canvas| {
        draw_svg_inner_paint(canvas, entry, x, y);
    });
}

/// SVG paint without `draw_with_opacity` wrapper. See
/// `draw_image_inner_paint` for the rationale (inline-root `<svg>`
/// shares node_id with the wrapping block).
fn draw_svg_inner_paint(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::SvgEntry,
    x: f32,
    y: f32,
) {
    use krilla_svg::{SurfaceExt, SvgSettings};
    if !entry.visible {
        return;
    }
    let Some(size) = krilla::geom::Size::from_wh(entry.width.to_f32(), entry.height.to_f32())
    else {
        return;
    };
    let transform = krilla::geom::Transform::from_translate(x, y);
    canvas.surface.push_transform(&transform);
    let _ = canvas
        .surface
        .draw_svg(&entry.tree, size, SvgSettings::default());
    canvas.surface.pop();
}

/// Block draw: emits background / border / box-shadow for the
/// wrapper. Children paint themselves via their own per-NodeId
/// dispatch in `draw_v2_page`, so this fn does **not** recurse into
/// block children.
///
/// Overflow clip (`overflow: hidden`) is intentionally not pushed
/// here — the clip scope is owned by `draw_under_clip`, which paints
/// bg/border/shadow outside the clip and pushes the clip path around
/// the descendant dispatch (see that function for the full painting
/// order).
fn draw_block_v2(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::BlockEntry,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
    is_split: bool,
) {
    use crate::draw_primitives::draw_with_opacity;

    draw_with_opacity(canvas, entry.opacity, |canvas| {
        draw_block_inner_paint(canvas, entry, x, y, frag, is_split);
    });
}

/// Root-element (`<html>`) background pre-pass. The fragmenter's
/// `geometry` table only carries body + descendants, so the standard
/// per-(node_id, fragment) dispatch never visits html. Paint bg /
/// border / shadow at `(margin.left, margin.top)` using html's own
/// `layout_size` (which equals body's outer height including
/// collapsed margins). Root `<html>` / `<body>` backgrounds repeat
/// per page (CSS Paged Media §5.4), so this runs once per page.
///
/// Called from `render_v2`; intentionally bypasses the
/// `body_offset_pt.y` adjustment that the main dispatch loop applies,
/// because html's bg paints at the page's margin top, not at body's
/// content origin.
fn paint_root_block_v2(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::BlockEntry,
    margin_left_pt: f32,
    margin_top_pt: f32,
    height_override: Option<f32>,
) {
    use crate::draw_primitives::draw_with_opacity;
    let Some(size) = entry.layout_size else {
        return;
    };

    let h = height_override.unwrap_or(size.height.to_f32());
    let w = size.width.to_f32();

    draw_with_opacity(canvas, entry.opacity, |canvas| {
        if entry.visible {
            crate::background::draw_box_shadows(
                canvas,
                &entry.style,
                margin_left_pt,
                margin_top_pt,
                w,
                h,
            );
            crate::background::draw_background(
                canvas,
                &entry.style,
                margin_left_pt,
                margin_top_pt,
                w,
                h,
            );
            crate::draw_primitives::draw_block_border(
                canvas,
                &entry.style,
                margin_left_pt,
                margin_top_pt,
                w,
                h,
            );
        }
    });
}

/// Block bg / border / shadow paint without the outer `draw_with_opacity`
/// wrap. Used by `draw_list_item_with_block` so the list-item's
/// marker and body block share a single opacity transparency group
/// (CSS opacity §2).
fn draw_block_inner_paint(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::BlockEntry,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
    is_split: bool,
) {
    let total_width = entry
        .layout_size
        .map(|s| s.width.to_f32())
        .unwrap_or_else(|| frag.width.in_pt().to_f32());
    // For split blocks (one fragment per page), `frag.height` reports
    // the per-page slice height. `entry.layout_size.height` always
    // carries Taffy's full block height, so painting bg / border with
    // `layout_size` would draw the FULL block on every slice — visible
    // as a callout-box overflowing page bottom on page 1 AND repeating
    // full-size on page 2 (fulgur-bq6i: `examples/break-inside`).
    //
    // Recover the slice-correct height from `frag.height` when the
    // dispatcher signals this is a split fragment
    // (`is_split = geom.fragments.len() > 1`). Using the multi-
    // fragment signal — not a `frag_h < layout_h` comparison —
    // avoids spurious flips for single-page blocks where the two
    // values may differ by 1 ULP after CSS-px → pt conversion
    // rounding.
    let total_height = entry
        .layout_size
        .map(|s| {
            if is_split && frag.height > crate::units::Px::ZERO {
                frag.height.in_pt().to_f32()
            } else {
                s.height.to_f32()
            }
        })
        .unwrap_or_else(|| frag.height.in_pt().to_f32());

    if entry.visible {
        crate::background::draw_box_shadows(canvas, &entry.style, x, y, total_width, total_height);
        crate::background::draw_background(canvas, &entry.style, x, y, total_width, total_height);
        crate::draw_primitives::draw_block_border(
            canvas,
            &entry.style,
            x,
            y,
            total_width,
            total_height,
        );
    }
}

/// Table draw: emits the outer-frame background / border / shadow.
/// Cell paint (each `<th>` / `<td>` is a block with its own NodeId in
/// geometry) lands through the standard per-NodeId dispatch.
///
/// Tables with `overflow: hidden | clip` route through
/// [`draw_under_clip_table`] instead so the clip path wraps every
/// cell dispatched in the same scope. Split tables size their frame
/// from the current fragment in both paths.
fn draw_table_v2(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::TableEntry,
    geom: &crate::pagination_layout::PaginationGeometry,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
) {
    use crate::draw_primitives::draw_with_opacity;

    // A split table can record a zero-height fragment — nested tables
    // whose header does not fit the remaining outer strip do this on
    // page 0. `table_box_size` then falls back to the full layout box
    // (the unsplit zero-height path still needs `cached_height`), so
    // painting here would leak a full-size frame as a sliver.
    if geom.is_split() && frag.height <= crate::units::Px::ZERO {
        return;
    }

    draw_with_opacity(canvas, entry.opacity, |canvas| {
        let (total_width, total_height) = table_box_size(entry, geom, frag);
        if entry.visible {
            paint_table_outer_frame(canvas, entry, x, y, total_width, total_height);
        }
    });
}

/// Resolve the table's outer-frame width/height from split geometry
/// or the cached layout. Falls back to the current Fragment height
/// (and finally `cached_height`) when `layout_size` is unset.
fn table_box_size(
    entry: &crate::drawables::TableEntry,
    geom: &crate::pagination_layout::PaginationGeometry,
    frag: &crate::pagination_layout::Fragment,
) -> (f32, f32) {
    let total_width = entry
        .layout_size
        .map(|s| s.width.to_f32())
        .unwrap_or(entry.width.to_f32());
    let fragment_height = frag.height.in_pt().to_f32();
    let total_height = if geom.is_split() && fragment_height > 0.0 {
        fragment_height
    } else {
        entry
            .layout_size
            .map(|s| s.height.to_f32())
            .unwrap_or_else(|| {
                let from_frag = fragment_height;
                if from_frag > 0.0 {
                    from_frag
                } else {
                    entry.cached_height.to_f32()
                }
            })
    };
    (total_width, total_height)
}

/// Paint the table's outer-frame bg / border / shadow at the current
/// (x, y, width, height). Shared between the no-clip path
/// ([`draw_table_v2`]) and the clip path ([`draw_under_clip_table`])
/// so the two emit identical PDF operators for the same input.
fn paint_table_outer_frame(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::TableEntry,
    x: f32,
    y: f32,
    total_width: f32,
    total_height: f32,
) {
    crate::background::draw_box_shadows(canvas, &entry.style, x, y, total_width, total_height);
    crate::background::draw_background(canvas, &entry.style, x, y, total_width, total_height);
    crate::draw_primitives::draw_block_border(
        canvas,
        &entry.style,
        x,
        y,
        total_width,
        total_height,
    );
}

/// Push a `compute_overflow_clip_path` clip around the table's current
/// fragment frame, dispatch each cell descendant inside the clip, then pop.
/// Mirrors [`draw_under_clip`]'s shape for blocks but specialised for
/// tables (no list-item marker, no shared-node_id inner content).
#[allow(clippy::too_many_arguments)]
fn draw_under_clip_table(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
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
    use crate::draw_primitives::draw_with_opacity;

    // A 0-tall split slice must not paint or clip at `layout_size` /
    // `cached_height` — same reason as `draw_table_v2`. Unlike that
    // no-clip path we must NOT return here: `build_page_skip_sets`
    // (see `render.rs:463`) routes every cell of an `overflow: hidden`
    // table through this function, so an early return would drop the
    // whole page's cells — repeated header included. Suppress the
    // outer frame and the clip, then dispatch descendants as usual.
    let zero_height_slice = geom.is_split() && frag.height <= crate::units::Px::ZERO;

    let (total_width, total_height) = table_box_size(table, geom, frag);

    draw_with_opacity(canvas, table.opacity, |canvas| {
        // bg / border / shadow OUTSIDE the clip, mirroring
        // `draw_under_clip` for blocks (`pageable.rs:1796-1827`).
        if table.visible && !zero_height_slice {
            paint_table_outer_frame(canvas, table, x_pt, y_pt, total_width, total_height);
        }

        // Push clip — fall through to descendant dispatch even if
        // `compute_overflow_clip_path` returns `None` so the cells
        // still paint (defensive, mirrors `draw_under_clip`).
        // A zero-height slice clips to nothing, so skip the push
        // entirely rather than erase the descendants we just kept.
        let clip_pushed = if zero_height_slice {
            false
        } else if let Some(clip_path) = crate::draw_primitives::compute_overflow_clip_path(
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

        // A table has no paragraph of its own, so the skip set comes purely
        // from the cell paragraphs inside `clip_descendants` — that is
        // where the duplicate paint of a cell's inline box originates.
        let paragraph_ib_skip =
            paragraph_owned_inline_boxes(table.clip_descendants.iter().copied(), drawables);
        for &desc_id in &table.clip_descendants {
            if transform_skip.contains(&desc_id)
                || nested_clip_skip.contains(&desc_id)
                || nested_opacity_skip.contains(&desc_id)
                || paragraph_ib_skip.contains(&desc_id)
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
                let desc_x = margin_left_pt + desc_frag.x.in_pt().to_f32();
                let desc_y = margin_top_pt + desc_frag.y.in_pt().to_f32();
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
                        canvas,
                        desc_id,
                        desc_geom,
                        desc_frag,
                        desc_x,
                        desc_y,
                        drawables,
                        geometry,
                        margin_left_pt,
                        margin_top_pt,
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

/// Block + inner content combined draw. Wraps bg/border **and** the
/// inner content in ONE `draw_with_opacity(self.opacity, ...)` group
/// (CSS opacity §2 — a single transparency group covers the whole
/// composed paint).
///
/// The shared-node_id patterns from `convert::inline_root` (block
/// wraps an inline-root paragraph) and `convert::replaced` (block
/// wraps `<img>` / `<svg>`) deliberately leave the inner draw payload
/// at `opacity: 1.0` — the wrapping block carries the real opacity.
/// Composing them all under a single
/// `draw_with_opacity(block.opacity, ...)` keeps the `q .. Q` framing
/// intact for `<p style="opacity:0.5; background:red">text</p>` and
/// friends.
#[allow(clippy::too_many_arguments)]
fn draw_block_with_inner_content(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
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
    drawables: &Drawables,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    margin_left_pt: f32,
    margin_top_pt: f32,
) {
    use crate::draw_primitives::draw_with_opacity;

    // Inner content (inline-root paragraph from `convert::inline_root`
    // or replaced image / svg from `convert::replaced`) is positioned
    // at the block's content-box top-left, not its border-box top-left.
    // Because the inner payload shares the block's `node_id`, it
    // would otherwise paint at the block's border-box origin,
    // dropping `padding + border` worth of offset for every
    // inline-root or replaced element. Read the inset from the
    // BlockStyle and add it before recursing.
    let (ix, iy) = block.style.content_inset();
    let inner_x = x + ix;
    let inner_y = y + iy;

    draw_with_opacity(canvas, block.opacity, |canvas| {
        draw_block_inner_paint(canvas, block, x, y, frag, is_split);
        if let Some(p) = paragraph {
            draw_paragraph_inner_paint(
                canvas,
                p,
                inner_x,
                inner_y,
                fragments,
                page_index,
                is_split,
                drawables,
                geometry,
                margin_left_pt,
                margin_top_pt,
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

/// List-item combined draw. Wraps the marker plus everything painted
/// by the body block in a single
/// `draw_with_opacity(self.opacity, ...)` group so
/// `<li style="opacity:..">` composites as one transparency group
/// (CSS opacity §2).
///
/// The `<li>` and its body block share the same node_id
/// (`convert/list_item.rs:81`); the body is built with `opacity: 1.0`
/// on purpose. When the body holds inline content, the inline-root
/// paragraph also lands at the same node_id (see
/// `convert::inline_root`). Painting marker + block frame + paragraph
/// glyphs in one compositing group avoids emitting multiple
/// `q .. Q` pairs that would break the single-group semantics.
#[allow(clippy::too_many_arguments)]
fn draw_list_item_with_block(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    node_id: usize,
    list_item: &crate::drawables::ListItemEntry,
    block: Option<&crate::drawables::BlockEntry>,
    paragraph: Option<&crate::drawables::ParagraphEntry>,
    x: f32,
    y: f32,
    frag: &crate::pagination_layout::Fragment,
    fragments: &[crate::pagination_layout::Fragment],
    page_index: u32,
    is_split: bool,
    drawables: &Drawables,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    margin_left_pt: f32,
    margin_top_pt: f32,
) {
    use crate::draw_primitives::draw_with_opacity;

    // Same content-box offset trick as `draw_block_with_inner_content`
    // — when the body block carries `padding` / `border`, the
    // inline-root paragraph that shares the list item's node_id has to
    // paint at the body block's content-box top-left.
    let inset = block.map(|b| b.style.content_inset()).unwrap_or((0.0, 0.0));
    let inner_x = x + inset.0;
    let inner_y = y + inset.1;

    draw_with_opacity(canvas, list_item.opacity, |canvas| {
        if list_item.visible {
            draw_list_item_marker_tagged(canvas, list_item, node_id, drawables, x, y);
        }
        if let Some(b) = block {
            draw_block_inner_paint(canvas, b, x, y, frag, is_split);
        }
        if let Some(p) = paragraph {
            let run_tag_node_id = run_tag_target(drawables, node_id);
            let use_run_tagging = canvas.tag_collector.is_some()
                && para_has_link_runs(p)
                && run_tag_node_id.is_some();
            let tag_info = if use_run_tagging {
                canvas.link_run_node_id = run_tag_node_id;
                None
            } else {
                try_start_tagged(canvas, node_id, drawables)
            };
            draw_paragraph_inner_paint(
                canvas,
                p,
                inner_x,
                inner_y,
                fragments,
                page_index,
                is_split,
                drawables,
                geometry,
                margin_left_pt,
                margin_top_pt,
            );
            finish_tagged(canvas, tag_info);
            if use_run_tagging {
                canvas.link_run_node_id = None;
            }
        }
    });
}

/// List-item marker paint without opacity wrapper or visibility gate
/// — caller (`draw_list_item_with_block`) handles both so the marker
/// and the body block share one compositing group.
fn draw_list_item_marker(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::ListItemEntry,
    x: f32,
    y: f32,
) {
    use crate::drawables::{ImageMarker, ListItemMarker};

    match &entry.marker {
        ListItemMarker::Text { lines, width } if !lines.is_empty() => {
            crate::paragraph::draw_shaped_lines(canvas, lines, x.as_pt() - *width, y.as_pt(), None);
        }
        ListItemMarker::Image {
            marker,
            width,
            height,
        } => {
            let marker_x = x - width.to_f32();
            let marker_y = (y.as_pt() + (entry.marker_line_height - *height) / 2.0).to_f32();
            match marker {
                ImageMarker::Raster(img) => draw_image_v2(canvas, img, marker_x, marker_y),
                ImageMarker::Svg(svg) => draw_svg_v2(canvas, svg, marker_x, marker_y),
            }
        }
        _ => {}
    }
}

/// List-item marker を描画し、タグ付きモードでは Lbl 構造要素でラップする。
///
/// When `canvas.in_marked_content` is already set (a nested tagged marker
/// inside an outer marked-content sequence — e.g. a `<ul><li>...</li></ul>`
/// dispatched from a tagged block's inline-box), the Lbl tag is skipped so
/// Krilla's non-nestable `start_tagged` does not panic. The marker glyphs
/// / image still paint; only the structural Lbl association is dropped for
/// the nested case (graceful degradation).
fn draw_list_item_marker_tagged(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    li: &crate::drawables::ListItemEntry,
    node_id: usize,
    drawables: &Drawables,
    x: f32,
    y: f32,
) {
    let lbl_id = canvas
        .tag_collector
        .as_ref()
        .and_then(|_| drawables.li_lbl_ids.get(&node_id).copied());
    let can_open_marker_tag = lbl_id.is_some() && !canvas.in_marked_content;
    let marker_tag_id = can_open_marker_tag.then(|| {
        let id = canvas
            .surface
            .start_tagged(krilla::tagging::ContentTag::Span(
                krilla::tagging::SpanTag::empty(),
            ));
        canvas.in_marked_content = true;
        id
    });

    draw_list_item_marker(canvas, li, x, y);

    if let (Some(lid), Some(id)) = (lbl_id, marker_tag_id) {
        canvas.surface.end_tagged();
        canvas.in_marked_content = false;
        canvas
            .tag_collector
            .as_mut()
            .expect("tag_collector is Some because marker_tag_id was issued from it")
            .record(lid, crate::tagging::PdfTag::Lbl, id, None);
    }
}

/// v2 paragraph draw. Mirrors `paragraph::ParagraphRender::draw`:
/// honour `visible`, wrap with `draw_with_opacity`, then call the
/// existing `paragraph::draw_shaped_lines` which already handles glyph
/// runs / inline images / inline boxes / link rect emission /
/// decoration spans. Reusing the helper keeps the per-glyph PDF output
/// byte-identical between v1 and v2.
#[allow(clippy::too_many_arguments)]
fn draw_paragraph_v2(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::ParagraphEntry,
    x: f32,
    y: f32,
    fragments: &[crate::pagination_layout::Fragment],
    page_index: u32,
    is_split: bool,
    drawables: &Drawables,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    margin_left_pt: f32,
    margin_top_pt: f32,
) {
    use crate::draw_primitives::draw_with_opacity;
    draw_with_opacity(canvas, entry.opacity, |canvas| {
        draw_paragraph_inner_paint(
            canvas,
            entry,
            x,
            y,
            fragments,
            page_index,
            is_split,
            drawables,
            geometry,
            margin_left_pt,
            margin_top_pt,
        );
    });
}

/// Paragraph paint without the outer `draw_with_opacity` wrap. Used
/// by `draw_list_item_with_block` so a list-item containing inline
/// content (the body block holds an inline-root paragraph at the
/// same node_id) can compose marker + block paint + glyph runs into
/// a single opacity transparency group (CSS opacity §2).
#[allow(clippy::too_many_arguments)]
fn draw_paragraph_inner_paint(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    entry: &crate::drawables::ParagraphEntry,
    x: f32,
    y: f32,
    fragments: &[crate::pagination_layout::Fragment],
    page_index: u32,
    is_split: bool,
    drawables: &Drawables,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    margin_left_pt: f32,
    margin_top_pt: f32,
) {
    if !entry.visible {
        return;
    }
    let Some(slice) = paragraph_lines_for_page(&entry.lines, fragments, page_index, is_split)
    else {
        return;
    };
    // PR 8g: build an InlineBoxRenderCtx so `draw_shaped_lines` can
    // dispatch any `LineItem::InlineBox` content through
    // `dispatch_inline_box_content` under a translate transform that
    // moves the body-relative geometry dispatch position to the
    // inline-flow position computed from `ib.x_offset` and
    // `ib.computed_y` (the latter folds in the CSS 2.1 §10.8.1
    // baseline_shift correction from `convert/inline_root.rs:493`).
    let inline_box_ctx = Some(crate::paragraph::InlineBoxRenderCtx {
        drawables,
        geometry,
        page_index,
        margin_left_pt,
        margin_top_pt,
    });
    crate::paragraph::draw_shaped_lines(canvas, &slice, x.as_pt(), y.as_pt(), inline_box_ctx);
}

/// Phase 4 PR 3 follow-up (PR #302 Devin): mirror
/// `ParagraphRender::slice_for_page` so multi-page paragraphs only
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

    let target_h = fragments[target_pos].height.in_pt();
    let consumed: crate::units::Pt = fragments[..target_pos]
        .iter()
        .map(|f| f.height)
        .sum::<crate::units::Px>()
        .in_pt();

    let eps = 0.01_f32.as_pt();
    let mut line_top = crate::units::Pt::ZERO;
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
    let mut accum = crate::units::Pt::ZERO;
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
            // `ParagraphRender::slice_for_page` exactly.
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
fn build_metadata(config: &Config, html_title: Option<&str>) -> krilla::metadata::Metadata {
    let mut metadata = krilla::metadata::Metadata::new();
    let effective_title = config.title.as_deref().or(html_title);
    if let Some(title) = effective_title {
        metadata = metadata.title(title.to_string());
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

/// Cached max-content width and rendered `Drawables` for margin boxes.
/// Measure cache: (html, page_height as bits) → max-content width.
/// Render cache: (html, final_width as bits, final_height as bits) →
/// (`Drawables`, `PaginationGeometryTable`).
type MeasureCache = HashMap<(String, u32, u32), crate::units::Pt>;
type RenderCache = HashMap<
    (String, u32, u32),
    (
        crate::drawables::Drawables,
        crate::pagination_layout::PaginationGeometryTable,
    ),
>;

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
    pub system_fonts: bool,
    pub margin_css: String,
    pub string_set_states: Vec<BTreeMap<String, crate::pagination_layout::StringSetPageState>>,
    pub running_states: Vec<BTreeMap<String, crate::pagination_layout::PageRunningState>>,
    pub counter_states: Vec<BTreeMap<String, i32>>,
    pub measure_cache: MeasureCache,
    pub height_cache: HashMap<(String, u32, u32), crate::units::Pt>,
    pub render_cache: RenderCache,
    /// fulgur-qgy7: per-page implicit `href` for
    /// `target-*(attr(href), ...)` evaluated inside `@page` margin
    /// boxes. Built once per render pass by `engine::Engine::layout_to_drawables`.
    pub implicit_href_by_page: &'a BTreeMap<usize, String>,
}

impl<'a> MarginBoxRenderer<'a> {
    /// Build a renderer from raw inputs. `string_set_by_node` /
    /// `counter_ops_by_node` are the per-node maps drained out of
    /// `ConvertContext` before `dom_to_drawables` consumed them.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        gcpm: &'a GcpmContext,
        running_store: &'a RunningElementStore,
        font_data: &'a [Arc<Vec<u8>>],
        system_fonts: bool,
        pagination_geometry: &crate::pagination_layout::PaginationGeometryTable,
        string_set_by_node: &HashMap<usize, Vec<(String, String)>>,
        counter_ops_by_node: &BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
        total_pages: usize,
        implicit_href_by_page: &'a BTreeMap<usize, String>,
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
            system_fonts,
            margin_css: strip_display_none(&gcpm.cleaned_css),
            string_set_states,
            running_states,
            counter_states,
            measure_cache: HashMap::new(),
            height_cache: HashMap::new(),
            render_cache: HashMap::new(),
            implicit_href_by_page,
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
    ///    `dom_to_drawables`, then dispatched through the v2 paint path.
    ///
    /// `content_width` is the page content area width in pt — used as
    /// the available width during measure passes for top/bottom boxes.
    ///
    /// `anchor_map` is the pass-2 cross-reference table; when `Some`,
    /// `target-counter()` / `target-counters()` / `target-text()` inside
    /// margin-box `content` resolve via the supplied map. When `None`
    /// (pass 1, or single-pass renders without target refs), those
    /// resolvers return empty strings — see
    /// `gcpm::counter::resolve_content_to_html_with_anchor`.
    ///
    /// fulgur-qgy7: margin-box `target-*(attr(href), ...)` resolves
    /// against the per-page implicit `href` stored on
    /// `implicit_href_by_page`. The map is populated by
    /// `engine::build_implicit_href_map` from the first
    /// `<a href="#...">` whose first fragment lands on each page.
    /// Pages with no such anchor look up `None` and the resolver
    /// returns an empty string.
    /// Returns `true` if any effective margin box resolved to non-empty
    /// content on this page (regardless of edge). Callers use this to
    /// decide whether body-link annotations that landed in a page-
    /// margin strip must be clipped: on a page with zero margin boxes
    /// nothing paints on top of body content in the strips, so
    /// preserving body annotations there keeps `position: fixed;
    /// bottom: 0`-style body-drawn page furniture clickable.
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
        anchor_map: Option<&AnchorMap>,
    ) -> bool {
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
                    ":left" => page_num.is_multiple_of(2),
                    ":right" => !page_num.is_multiple_of(2),
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

        // Resolve HTML for each effective box. Margin-box content goes
        // through `_with_anchor` so `target-*` resolves when
        // `anchor_map` is present (pass 2).
        let implicit_href = self
            .implicit_href_by_page
            .get(&page_idx)
            .map(String::as_str);
        let mut resolved_htmls: BTreeMap<MarginBoxPosition, String> = BTreeMap::new();
        for (&pos, rule) in &effective_boxes {
            let content_html = resolve_content_to_html_with_anchor(
                &rule.content,
                self.running_store,
                &self.running_states,
                &self.string_set_states[page_idx],
                page_num,
                total_pages,
                page_idx,
                &self.counter_states[page_idx],
                anchor_map,
                implicit_href,
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
                    content_width.as_pt().in_px(),
                    page_size.height.as_pt().in_px(),
                    self.font_data,
                    self.system_fonts,
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
                    fixed_width.as_pt().in_px(),
                    page_size.height.as_pt().in_px(),
                    self.font_data,
                    self.system_fonts,
                );
                get_body_child_dimension(&measure_doc, false)
            });
        }

        // Stage 2: distribute each edge's boxes against the page rect.
        let mut edge_defined: BTreeMap<Edge, BTreeMap<MarginBoxPosition, crate::units::Pt>> =
            BTreeMap::new();
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

            let cache_key = (
                html.clone(),
                width_key(rect.width.to_f32()),
                width_key(rect.height.to_f32()),
            );
            if !self.render_cache.contains_key(&cache_key) {
                let render_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\">{}</body></html>",
                    self.margin_css, html
                );
                let mut render_doc = crate::blitz_adapter::parse_and_layout(
                    &render_html,
                    rect.width.in_px(),
                    rect.height.in_px(),
                    self.font_data,
                    self.system_fonts,
                );
                let empty_column_styles = crate::column_css::ColumnStyleTable::new();
                let geometry = crate::pagination_layout::run_pass_with_break_styles(
                    &mut render_doc,
                    rect.height.in_px(),
                    &empty_column_styles,
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
                    pagination_geometry: geometry,
                    link_cache: Default::default(),
                    viewport_size_px: None,
                };
                let drawables = crate::convert::dom_to_drawables(&render_doc, &mut dummy_ctx);
                let geometry = dummy_ctx.pagination_geometry;
                self.render_cache
                    .insert(cache_key.clone(), (drawables, geometry));
            }

            if let Some((drawables, geometry)) = self.render_cache.get(&cache_key) {
                if let Some(root_id) = drawables.root_id {
                    if let Some(root_block) = drawables.block_styles.get(&root_id) {
                        paint_root_block_v2(
                            canvas,
                            root_block,
                            rect.x.to_f32(),
                            rect.y.to_f32(),
                            None,
                        );
                    }
                }
                // `body_offset_pt` is (0, 0) here because the wrapper HTML fixes
                // `body { margin: 0; padding: 0; }` via an inline style, which
                // takes higher specificity than `self.margin_css`. No adjustment
                // needed unlike `render_v2`.
                let (txd, cd, owd) = build_page_skip_sets(drawables);
                // Margin-box content is always single-page (page 0).
                let page_nodes: Vec<usize> = geometry
                    .iter()
                    .filter_map(|(&id, g)| {
                        g.fragments.iter().any(|f| f.page_index == 0).then_some(id)
                    })
                    .collect();
                draw_v2_page(
                    canvas,
                    0,
                    rect.x.to_f32(),
                    rect.y.to_f32(),
                    geometry,
                    drawables,
                    &txd,
                    &cd,
                    &owd,
                    &page_nodes,
                );
            }
        }
        // Body-link clip gate: `true` iff at least one effective margin
        // box on this page produced non-empty content, i.e. something
        // was actually painted on top of the body layer. When `false`,
        // the caller must NOT clip body annotations that landed in a
        // margin strip — nothing covers them, so dropping the click
        // target would leave visible body-drawn page furniture
        // unclickable.
        !resolved_htmls.is_empty()
    }
}

/// Get a layout dimension of the first non-zero child of `<body>` in a Blitz document.
/// When `use_width` is true, returns max-content width; otherwise returns height.
///
/// Returns a [`crate::units::Pt`]. Blitz's internal layout is in CSS px, so the
/// raw Taffy size is tagged `Px` and converted with `Px::in_pt` on the way out —
/// matching the convention used at the convert.rs boundary (`layout_in_pt`). This
/// keeps the GCPM margin-box measure caches in the same unit (pt) as `page_size` /
/// `margin`, which `compute_edge_layout` assumes when distributing along the edge.
fn get_body_child_dimension(doc: &blitz_html::HtmlDocument, use_width: bool) -> crate::units::Pt {
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
    px.as_px().in_pt()
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

/// Build a hierarchical [`TagTree`] from [`TagCollector`] entries and
/// the `semantics` map in [`Drawables`].
///
/// The flat approach used before fulgur-izp.5 created one top-level
/// [`TagGroup`] per tagged NodeId. This function instead uses
/// [`crate::tagging::SemanticEntry::parent`] to nest groups, so that
/// `<section><h1>…</h1></section>` produces a Div group containing an
/// Hn group rather than two sibling groups.
fn build_struct_tree(
    tc: crate::draw_primitives::TagCollector,
    drawables: &Drawables,
    link_annot_ids: &BTreeMap<usize, Vec<Identifier>>,
    tree: &mut TagTree,
) {
    let mut identifiers: BTreeMap<crate::drawables::NodeId, Vec<Identifier>> = BTreeMap::new();
    let mut heading_titles: BTreeMap<crate::drawables::NodeId, String> = BTreeMap::new();
    // `into_parts` returns `entries` and `run_entries` together so neither
    // field needs to be borrowed before the other is moved.
    let (tc_entries, run_entries) = tc.into_parts();
    for (node_id, _tag, id, heading_title) in tc_entries {
        identifiers.entry(node_id).or_default().push(id);
        if let Some(title) = heading_title {
            heading_titles.entry(node_id).or_insert(title);
        }
    }
    // Nodes that use per-run tagging (run_entries) bypass try_start_tagged and
    // therefore never record a heading_title through tc_entries. Backfill their
    // titles here so <h1><a href>…</a></h1> still gets the /T attribute.
    // `heading_title_of` filters out invisible headings so the tag tree does
    // not leak `visibility: hidden` content that the paint path suppressed.
    for &node_id in run_entries.keys() {
        if heading_titles.contains_key(&node_id) {
            continue;
        }
        if let Some(entry) = drawables.semantics.get(&node_id) {
            if matches!(entry.tag, crate::tagging::PdfTag::H { .. }) {
                if let Some(title) = drawables
                    .paragraphs
                    .get(&node_id)
                    .and_then(heading_title_of)
                {
                    heading_titles.insert(node_id, title);
                }
            }
        }
    }

    let mut children_map: BTreeMap<crate::drawables::NodeId, Vec<crate::drawables::NodeId>> =
        BTreeMap::new();
    for (&node_id, entry) in &drawables.semantics {
        if let Some(parent_id) = entry.parent {
            children_map.entry(parent_id).or_default().push(node_id);
        }
    }

    let roots: Vec<crate::drawables::NodeId> = drawables
        .semantics
        .iter()
        .filter(|(_, e)| e.parent.is_none())
        .map(|(&id, _)| id)
        .collect();

    for root_id in roots {
        let group = build_tag_group(
            root_id,
            drawables,
            &identifiers,
            &heading_titles,
            &children_map,
            &run_entries,
            link_annot_ids,
        );
        tree.push(Node::Group(group));
    }
}

fn build_tag_group(
    node_id: crate::drawables::NodeId,
    drawables: &Drawables,
    identifiers: &BTreeMap<crate::drawables::NodeId, Vec<Identifier>>,
    heading_titles: &BTreeMap<crate::drawables::NodeId, String>,
    children_map: &BTreeMap<crate::drawables::NodeId, Vec<crate::drawables::NodeId>>,
    run_entries: &BTreeMap<crate::drawables::NodeId, Vec<crate::draw_primitives::ParagraphRunItem>>,
    link_annot_ids: &BTreeMap<usize, Vec<Identifier>>,
) -> TagGroup {
    let entry = &drawables.semantics[&node_id]; // invariant: node_id always derived from drawables.semantics
    let title = heading_titles.get(&node_id).cloned();
    let mut group = TagGroup::new(crate::tagging::pdf_tag_to_krilla_tag(
        &entry.tag,
        title,
        entry.alt_text.clone(),
    ));

    // Paragraphs with per-run link tagging use run_entries instead of the
    // single identifiers path.
    if let Some(run_items) = run_entries.get(&node_id) {
        let mut i = 0;
        while i < run_items.len() {
            use crate::draw_primitives::ParagraphRunItem;
            match &run_items[i] {
                ParagraphRunItem::Content(id) => {
                    group.push(Node::Leaf(*id));
                    i += 1;
                }
                ParagraphRunItem::LinkContent { span_ptr, .. } => {
                    let ptr = *span_ptr;
                    let mut link_group = TagGroup::new(crate::tagging::pdf_tag_to_krilla_tag(
                        &crate::tagging::PdfTag::Link,
                        None,
                        None,
                    ));
                    // Collect all consecutive items with the same span_ptr.
                    while let Some(ParagraphRunItem::LinkContent {
                        span_ptr: p,
                        identifier: id,
                    }) = run_items.get(i)
                        && *p == ptr
                    {
                        link_group.push(Node::Leaf(*id));
                        i += 1;
                    }
                    // Annotation identifiers (OBJR) follow content identifiers.
                    // A cross-page link produces one identifier per page.
                    if let Some(annot_ids) = link_annot_ids.get(&ptr) {
                        for &annot_id in annot_ids {
                            link_group.push(Node::Leaf(annot_id));
                        }
                    }
                    group.push(Node::Group(link_group));
                }
            }
        }
    } else if let Some(ids) = identifiers.get(&node_id) {
        for &id in ids {
            group.push(Node::Leaf(id));
        }
    }

    if let Some(children) = children_map.get(&node_id) {
        for &child_id in children {
            let child = build_tag_group(
                child_id,
                drawables,
                identifiers,
                heading_titles,
                children_map,
                run_entries,
                link_annot_ids,
            );
            group.push(Node::Group(child));
        }
    }

    group
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

    // --- build_tag_group: LinkContent branch ---

    use crate::draw_primitives::run_tag_tests::make_identifier;

    /// Build a minimal [`Drawables`] with a single semantic paragraph node.
    fn drawables_with_para(node_id: crate::drawables::NodeId) -> Drawables {
        let mut d = Drawables::new();
        d.semantics.insert(
            node_id,
            crate::tagging::SemanticEntry {
                tag: crate::tagging::PdfTag::P,
                parent: None,
                alt_text: None,
            },
        );
        d
    }

    #[test]
    fn build_tag_group_link_content_creates_nested_link_group() {
        // Paragraph node_id = 1 has one non-link run and one link run (ptr=99).
        let node_id: crate::drawables::NodeId = 1;
        let content_id = make_identifier();
        let link_id = make_identifier();
        let annot_id = make_identifier();

        let mut run_entries: BTreeMap<
            crate::drawables::NodeId,
            Vec<crate::draw_primitives::ParagraphRunItem>,
        > = BTreeMap::new();
        run_entries.insert(
            node_id,
            vec![
                crate::draw_primitives::ParagraphRunItem::Content(content_id),
                crate::draw_primitives::ParagraphRunItem::LinkContent {
                    span_ptr: 99,
                    identifier: link_id,
                },
            ],
        );

        let mut link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        link_annot_ids.entry(99).or_default().push(annot_id);

        let drawables = drawables_with_para(node_id);
        let identifiers: BTreeMap<crate::drawables::NodeId, Vec<Identifier>> = BTreeMap::new();
        let heading_titles: BTreeMap<crate::drawables::NodeId, String> = BTreeMap::new();
        let children_map: BTreeMap<crate::drawables::NodeId, Vec<crate::drawables::NodeId>> =
            BTreeMap::new();

        let group = build_tag_group(
            node_id,
            &drawables,
            &identifiers,
            &heading_titles,
            &children_map,
            &run_entries,
            &link_annot_ids,
        );

        // The group should have two children: one Leaf (Content) and one Group (Link).
        assert_eq!(group.children.len(), 2, "expected Leaf + Group(Link)");
        // Second child should be a Group (the Link TagGroup).
        assert!(
            matches!(group.children[1], Node::Group(_)),
            "second child should be a Link Group"
        );
    }

    #[test]
    fn build_tag_group_link_content_no_annot_id_still_builds_group() {
        // Same as above but no annotation identifier in link_annot_ids.
        let node_id: crate::drawables::NodeId = 2;
        let link_id = make_identifier();

        let mut run_entries: BTreeMap<
            crate::drawables::NodeId,
            Vec<crate::draw_primitives::ParagraphRunItem>,
        > = BTreeMap::new();
        run_entries.insert(
            node_id,
            vec![crate::draw_primitives::ParagraphRunItem::LinkContent {
                span_ptr: 77,
                identifier: link_id,
            }],
        );

        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new(); // empty — no OBJR

        let drawables = drawables_with_para(node_id);
        let identifiers: BTreeMap<crate::drawables::NodeId, Vec<Identifier>> = BTreeMap::new();
        let heading_titles: BTreeMap<crate::drawables::NodeId, String> = BTreeMap::new();
        let children_map: BTreeMap<crate::drawables::NodeId, Vec<crate::drawables::NodeId>> =
            BTreeMap::new();

        let group = build_tag_group(
            node_id,
            &drawables,
            &identifiers,
            &heading_titles,
            &children_map,
            &run_entries,
            &link_annot_ids,
        );

        assert_eq!(
            group.children.len(),
            1,
            "link without OBJR should still produce one Link Group"
        );
        assert!(
            matches!(group.children[0], Node::Group(_)),
            "child should be a Link Group"
        );
    }

    // --- para_has_link_runs ---

    fn make_glyph_run(
        text: &str,
        link: Option<std::sync::Arc<crate::paragraph::LinkSpan>>,
    ) -> crate::paragraph::ShapedGlyphRun {
        crate::paragraph::ShapedGlyphRun {
            font_data: std::sync::Arc::new(vec![]),
            font_index: 0,
            font_size: 12.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: crate::paragraph::TextDecoration::default(),
            glyphs: vec![],
            text: std::sync::Arc::from(text),
            x_offset: crate::units::Pt::ZERO,
            link,
        }
    }

    fn make_shaped_line(items: Vec<crate::paragraph::LineItem>) -> crate::paragraph::ShapedLine {
        crate::paragraph::ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items,
        }
    }

    fn make_para(lines: Vec<crate::paragraph::ShapedLine>) -> crate::drawables::ParagraphEntry {
        crate::drawables::ParagraphEntry {
            lines,
            opacity: 1.0,
            visible: true,
            id: None,
        }
    }

    fn make_hidden_para(
        lines: Vec<crate::paragraph::ShapedLine>,
    ) -> crate::drawables::ParagraphEntry {
        crate::drawables::ParagraphEntry {
            lines,
            opacity: 1.0,
            visible: false,
            id: None,
        }
    }

    #[test]
    fn para_has_link_runs_empty_paragraph_returns_false() {
        let para = make_para(vec![]);
        assert!(!para_has_link_runs(&para));
    }

    #[test]
    fn para_has_link_runs_text_without_link_returns_false() {
        let run = make_glyph_run("hello", None);
        let line = make_shaped_line(vec![crate::paragraph::LineItem::Text(run)]);
        let para = make_para(vec![line]);
        assert!(!para_has_link_runs(&para));
    }

    #[test]
    fn para_has_link_runs_text_with_link_returns_true() {
        let span = std::sync::Arc::new(crate::paragraph::LinkSpan {
            target: crate::paragraph::LinkTarget::External(std::sync::Arc::new(
                "https://example.com".to_string(),
            )),
            alt_text: None,
        });
        let run = make_glyph_run("click me", Some(span));
        let line = make_shaped_line(vec![crate::paragraph::LineItem::Text(run)]);
        let para = make_para(vec![line]);
        assert!(para_has_link_runs(&para));
    }

    #[test]
    fn para_has_link_runs_inline_box_returns_false() {
        let item = crate::paragraph::InlineBoxItem {
            node_id: None,
            width: 10.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        let line = make_shaped_line(vec![crate::paragraph::LineItem::InlineBox(item)]);
        let para = make_para(vec![line]);
        assert!(!para_has_link_runs(&para));
    }

    /// Build a paragraph carrying one inline box with the given node id.
    fn para_with_inline_box(node_id: Option<usize>) -> crate::drawables::ParagraphEntry {
        let item = crate::paragraph::InlineBoxItem {
            node_id,
            width: 10.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        make_para(vec![make_shaped_line(vec![
            crate::paragraph::LineItem::InlineBox(item),
        ])])
    }

    #[test]
    fn paragraph_owned_inline_boxes_expands_the_scope_owner_paragraph() {
        let mut d = Drawables::new();
        // Scope owner 1 owns a paragraph carrying inline box 2, whose
        // subtree is {3, 4}.
        d.paragraphs.insert(1, para_with_inline_box(Some(2)));
        d.inline_box_subtree_descendants.insert(2, vec![3, 4]);

        let skip = paragraph_owned_inline_boxes([1, 3, 4], &d);
        assert_eq!(
            skip,
            [2, 3, 4]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            "the box and its recorded subtree are painted by the paragraph",
        );
    }

    #[test]
    fn paragraph_owned_inline_boxes_expands_descendant_paragraphs() {
        let mut d = Drawables::new();
        // The scope owner (1) has no paragraph; a descendant (5) does. A
        // table is exactly this shape — the duplicate originates in a cell.
        d.paragraphs.insert(5, para_with_inline_box(Some(6)));
        d.inline_box_subtree_descendants.insert(6, vec![7]);

        let skip = paragraph_owned_inline_boxes([1, 5, 6, 7], &d);
        assert_eq!(
            skip,
            [6, 7]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
        );
    }

    #[test]
    fn paragraph_owned_inline_boxes_ignores_ids_without_a_paragraph() {
        let mut d = Drawables::new();
        // 9 is a plain block drawable, not a paragraph — nothing to skip.
        // This is the block-child case: the id is in the scope's descendant
        // list but no paragraph paints it, so the walk must still dispatch
        // it. Skipping it here is what PR #677 got wrong (fulgur-49xw).
        d.inline_box_subtree_descendants.insert(8, vec![9]);

        let skip = paragraph_owned_inline_boxes([8, 9], &d);
        assert!(
            skip.is_empty(),
            "no paragraph in scope means nothing is paragraph-owned, got {skip:?}",
        );
    }

    #[test]
    fn paragraph_owned_inline_boxes_skips_boxes_without_a_node_id() {
        let mut d = Drawables::new();
        d.paragraphs.insert(1, para_with_inline_box(None));

        let skip = paragraph_owned_inline_boxes([1], &d);
        assert!(
            skip.is_empty(),
            "an inline box with no node id has nothing to skip"
        );
    }

    #[test]
    fn paragraph_owned_inline_boxes_handles_a_box_with_no_recorded_subtree() {
        let mut d = Drawables::new();
        // `inline_box_subtree_descendants` has no entry for 2 — a leaf
        // inline box. The box itself is still paragraph-owned.
        d.paragraphs.insert(1, para_with_inline_box(Some(2)));

        let skip = paragraph_owned_inline_boxes([1], &d);
        assert_eq!(
            skip,
            [2].into_iter().collect::<std::collections::BTreeSet<_>>(),
        );
    }

    #[test]
    fn para_has_link_runs_image_with_link_returns_true() {
        let span = std::sync::Arc::new(crate::paragraph::LinkSpan {
            target: crate::paragraph::LinkTarget::Internal(std::sync::Arc::new(
                "section".to_string(),
            )),
            alt_text: None,
        });
        let img = crate::paragraph::InlineImage {
            data: std::sync::Arc::new(vec![]),
            format: crate::image::ImageFormat::Png,
            width: 20.0_f32.as_pt(),
            height: 20.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: crate::paragraph::VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: crate::units::Pt::ZERO,
            link: Some(span),
        };
        let line = make_shaped_line(vec![crate::paragraph::LineItem::Image(img)]);
        let para = make_para(vec![line]);
        assert!(para_has_link_runs(&para));
    }

    #[test]
    fn para_has_link_runs_image_without_link_returns_false() {
        let img = crate::paragraph::InlineImage {
            data: std::sync::Arc::new(vec![]),
            format: crate::image::ImageFormat::Png,
            width: 10.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: crate::paragraph::VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: crate::units::Pt::ZERO,
            link: None,
        };
        let line = make_shaped_line(vec![crate::paragraph::LineItem::Image(img)]);
        let para = make_para(vec![line]);
        assert!(!para_has_link_runs(&para));
    }

    #[test]
    fn para_has_link_runs_link_in_second_line_returns_true() {
        let plain_line = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "first", None,
        ))]);
        let span = std::sync::Arc::new(crate::paragraph::LinkSpan {
            target: crate::paragraph::LinkTarget::External(std::sync::Arc::new(
                "https://example.com".to_string(),
            )),
            alt_text: None,
        });
        let link_line = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "second",
            Some(span),
        ))]);
        let para = make_para(vec![plain_line, link_line]);
        assert!(para_has_link_runs(&para));
    }

    // --- run_tag_target ---

    #[test]
    fn run_tag_target_li_lbody_ids_hit_returns_lbody_id() {
        // When li_lbody_ids contains node_id, the synthetic LBody id is returned
        // (which differs from the input node_id).
        let mut d = Drawables::new();
        let li_id: crate::drawables::NodeId = 10;
        let lbody_id: crate::drawables::NodeId = 9999;
        d.li_lbody_ids.insert(li_id, lbody_id);
        assert_eq!(run_tag_target(&d, li_id), Some(lbody_id));
    }

    #[test]
    fn run_tag_target_li_lbody_ids_returns_mapped_not_input_id() {
        // The returned id is the LBody synthetic id, NOT the li input id.
        let mut d = Drawables::new();
        let li_id: crate::drawables::NodeId = 20;
        let lbody_id: crate::drawables::NodeId = 8888;
        d.li_lbody_ids.insert(li_id, lbody_id);
        let result = run_tag_target(&d, li_id);
        assert_ne!(result, Some(li_id));
        assert_eq!(result, Some(lbody_id));
    }

    #[test]
    fn run_tag_target_semantics_hit_returns_node_id() {
        // When only semantics contains node_id, returns Some(node_id) unchanged.
        let node_id: crate::drawables::NodeId = 5;
        let d = drawables_with_para(node_id);
        assert_eq!(run_tag_target(&d, node_id), Some(node_id));
    }

    #[test]
    fn run_tag_target_neither_map_returns_none() {
        // A node absent from both li_lbody_ids and semantics yields None.
        let d = Drawables::new();
        assert_eq!(run_tag_target(&d, 42), None);
    }

    #[test]
    fn run_tag_target_li_lbody_ids_takes_priority_over_semantics() {
        // When node_id is in both maps, li_lbody_ids wins and lbody_id is returned.
        let mut d = Drawables::new();
        let node_id: crate::drawables::NodeId = 7;
        let lbody_id: crate::drawables::NodeId = 7777;
        d.li_lbody_ids.insert(node_id, lbody_id);
        d.semantics.insert(
            node_id,
            crate::tagging::SemanticEntry {
                tag: crate::tagging::PdfTag::Li,
                parent: None,
                alt_text: None,
            },
        );
        assert_eq!(run_tag_target(&d, node_id), Some(lbody_id));
    }

    // --- extract_heading_title ---

    #[test]
    fn extract_heading_title_empty_paragraph_returns_empty() {
        let para = make_para(vec![]);
        assert_eq!(extract_heading_title(&para), "");
    }

    #[test]
    fn extract_heading_title_single_text_run() {
        let line = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "Hello", None,
        ))]);
        let para = make_para(vec![line]);
        assert_eq!(extract_heading_title(&para), "Hello");
    }

    #[test]
    fn extract_heading_title_multiple_runs_across_lines() {
        let line1 = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "Foo", None,
        ))]);
        let line2 = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "Bar", None,
        ))]);
        let para = make_para(vec![line1, line2]);
        assert_eq!(extract_heading_title(&para), "FooBar");
    }

    #[test]
    fn extract_heading_title_skips_image_and_inline_box() {
        let img = crate::paragraph::InlineImage {
            data: std::sync::Arc::new(vec![]),
            format: crate::image::ImageFormat::Png,
            width: 10.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: crate::paragraph::VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: crate::units::Pt::ZERO,
            link: None,
        };
        let inline_box = crate::paragraph::InlineBoxItem {
            node_id: None,
            width: 10.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        let line = make_shaped_line(vec![
            crate::paragraph::LineItem::Image(img),
            crate::paragraph::LineItem::Text(make_glyph_run("text", None)),
            crate::paragraph::LineItem::InlineBox(inline_box),
        ]);
        let para = make_para(vec![line]);
        assert_eq!(extract_heading_title(&para), "text");
    }

    // --- heading_title_of ---
    //
    // `heading_title_of` gates `extract_heading_title` on
    // `ParagraphEntry.visible` and drops zero-length titles. It is the
    // single choke point shared by `try_start_tagged` (H arm) and
    // `build_struct_tree`'s per-run backfill, so a `None` here means the
    // heading `/T` attribute is never emitted for that paragraph.

    #[test]
    fn heading_title_of_visible_paragraph_with_text_returns_title() {
        // Sanity: the visible-with-text path still forwards a title.
        let line = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "Chapter", None,
        ))]);
        let para = make_para(vec![line]);
        assert_eq!(heading_title_of(&para), Some("Chapter".to_string()));
    }

    #[test]
    fn heading_title_of_visible_empty_paragraph_returns_none() {
        // An empty paragraph would have produced a zero-length `/T`
        // before; the helper drops that in a single place so both call
        // sites see identical behaviour.
        let para = make_para(vec![]);
        assert_eq!(heading_title_of(&para), None);
    }

    #[test]
    fn heading_title_of_hidden_paragraph_returns_none() {
        // Core security regression guard: a `visibility: hidden` /
        // `collapse` paragraph must not surface its text through the tag
        // tree even though `extract_heading_title` would happily collect
        // it. Without this gate the text would end up in `Tag::Hn`'s
        // `/T` attribute for both the normal and per-run backfill sinks
        // while the paint path skips the same paragraph, leaking hidden
        // content to any tool that reads the struct tree.
        let line = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "hidden secret",
            None,
        ))]);
        let para = make_hidden_para(vec![line]);
        assert_eq!(heading_title_of(&para), None);
    }

    #[test]
    fn heading_title_of_hidden_empty_paragraph_returns_none() {
        // The visibility gate short-circuits before the text collection,
        // so an empty-and-hidden paragraph is `None` too — same result
        // as the visible-empty case, exercised via the other branch.
        let para = make_hidden_para(vec![]);
        assert_eq!(heading_title_of(&para), None);
    }

    // --- paragraph_lines_for_page ---

    fn make_fragment(page_index: u32, height: f32) -> crate::pagination_layout::Fragment {
        crate::pagination_layout::Fragment {
            page_index,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: height.as_px(),
        }
    }

    fn make_line(height_pt: f32, baseline_pt: f32) -> crate::paragraph::ShapedLine {
        crate::paragraph::ShapedLine {
            height: height_pt.as_pt(),
            baseline: baseline_pt.as_pt(),
            items: vec![],
        }
    }

    #[test]
    fn paragraph_lines_for_page_no_matching_fragment_returns_none() {
        let lines = vec![make_line(16.0, 12.0)];
        let fragments = vec![make_fragment(0, 16.0)];
        // Ask for page 1, which has no fragment.
        let result = paragraph_lines_for_page(&lines, &fragments, 1, false);
        assert!(result.is_none());
    }

    #[test]
    fn paragraph_lines_for_page_not_split_returns_all_lines() {
        // 64px × 0.75 ≈ 48pt (3 × 16pt lines). Non-split path returns
        // all lines unchanged — baseline values are preserved as-is.
        let lines = vec![
            make_line(16.0, 12.0),
            make_line(16.0, 28.0),
            make_line(16.0, 44.0),
        ];
        let fragments = vec![make_fragment(0, 64.0)];
        let result = paragraph_lines_for_page(&lines, &fragments, 0, false);
        assert!(result.is_some());
        let sliced = result.unwrap();
        assert_eq!(sliced.len(), 3);
        assert!(
            (sliced[0].baseline.to_f32() - 12.0).abs() < 0.01,
            "baseline unchanged on non-split"
        );
    }

    #[test]
    fn paragraph_lines_for_page_split_first_page_returns_first_lines() {
        // Lines are 12pt each. Fragment heights are in CSS px;
        // 16px × PX_TO_PT (0.75) = 12pt, so 16px per line.
        // Page 0: consumed = 0, baseline unchanged.
        let lines = vec![make_line(12.0, 9.0), make_line(12.0, 21.0)];
        let fragments = vec![
            make_fragment(0, 16.0), // 16px = 12pt → first line
            make_fragment(1, 16.0),
        ];
        let result = paragraph_lines_for_page(&lines, &fragments, 0, true);
        assert!(result.is_some());
        let sliced = result.unwrap();
        assert_eq!(sliced.len(), 1, "page 0 should contain exactly one line");
        assert!(
            (sliced[0].baseline.to_f32() - 9.0).abs() < 0.01,
            "page 0 baseline unchanged (consumed=0)"
        );
    }

    #[test]
    fn paragraph_lines_for_page_split_second_page_returns_remaining_lines() {
        // Page 1: consumed = 12pt (one line). The function subtracts consumed
        // from baseline, so paragraph-absolute 21pt → fragment-local 9pt.
        let lines = vec![make_line(12.0, 9.0), make_line(12.0, 21.0)];
        let fragments = vec![
            make_fragment(0, 16.0), // 16px = 12pt → first line
            make_fragment(1, 16.0),
        ];
        let result = paragraph_lines_for_page(&lines, &fragments, 1, true);
        assert!(result.is_some());
        let sliced = result.unwrap();
        assert_eq!(sliced.len(), 1, "page 1 should contain exactly one line");
        assert!(
            (sliced[0].baseline.to_f32() - 9.0).abs() < 0.01,
            "baseline rebased: 21pt - 12pt consumed = 9pt"
        );
    }

    #[test]
    fn paragraph_lines_for_page_split_empty_range_returns_none() {
        // Fragment height is 0px → no lines fit.
        let lines = vec![make_line(12.0, 9.0)];
        let fragments = vec![
            make_fragment(0, 100.0), // page 0 gets the line
            make_fragment(1, 0.0),   // page 1 has zero height
        ];
        let result = paragraph_lines_for_page(&lines, &fragments, 1, true);
        assert!(result.is_none());
    }

    // --- build_page_skip_sets ---

    fn block_entry_with_overflow_clip(descendants: Vec<usize>) -> crate::drawables::BlockEntry {
        let style = crate::draw_primitives::BlockStyle {
            overflow_x: crate::draw_primitives::Overflow::Clip,
            ..Default::default()
        };
        crate::drawables::BlockEntry {
            style,
            opacity: 1.0,
            visible: true,
            id: None,
            layout_size: None,
            clip_descendants: descendants,
            opacity_descendants: vec![],
        }
    }

    fn block_entry_with_opacity_descendants(
        descendants: Vec<usize>,
    ) -> crate::drawables::BlockEntry {
        crate::drawables::BlockEntry {
            style: crate::draw_primitives::BlockStyle::default(),
            opacity: 0.5,
            visible: true,
            id: None,
            layout_size: None,
            clip_descendants: vec![],
            opacity_descendants: descendants,
        }
    }

    #[test]
    fn build_page_skip_sets_empty_drawables_returns_empty_sets() {
        let d = Drawables::new();
        let (tx, clip, opacity) = build_page_skip_sets(&d);
        assert!(tx.is_empty());
        assert!(clip.is_empty());
        assert!(opacity.is_empty());
    }

    #[test]
    fn build_page_skip_sets_transform_descendants_collected() {
        let mut d = Drawables::new();
        d.transforms.insert(
            10,
            crate::drawables::TransformEntry {
                matrix: crate::draw_primitives::Affine2D::translation(
                    0.0_f32.as_pt(),
                    0.0_f32.as_pt(),
                ),
                origin: crate::draw_primitives::Point2::new(
                    crate::units::Pt::ZERO,
                    crate::units::Pt::ZERO,
                ),
                descendants: vec![11, 12],
            },
        );
        let (tx, clip, opacity) = build_page_skip_sets(&d);
        assert!(tx.contains(&11));
        assert!(tx.contains(&12));
        assert!(!tx.contains(&10), "wrapper itself is not a descendant");
        assert!(clip.is_empty());
        assert!(opacity.is_empty());
    }

    #[test]
    fn build_page_skip_sets_overflow_clip_block_collects_descendants() {
        let mut d = Drawables::new();
        let node_id: usize = 20;
        d.block_styles
            .insert(node_id, block_entry_with_overflow_clip(vec![21, 22]));
        let (_, clip, _) = build_page_skip_sets(&d);
        assert!(clip.contains(&21));
        assert!(clip.contains(&22));
    }

    #[test]
    fn build_page_skip_sets_body_excluded_from_clip_descendants() {
        let mut d = Drawables::new();
        let body_id: usize = 5;
        d.body_id = Some(body_id);
        d.block_styles
            .insert(body_id, block_entry_with_overflow_clip(vec![6, 7]));
        let (_, clip, _) = build_page_skip_sets(&d);
        assert!(!clip.contains(&6), "body clip descendants must be excluded");
        assert!(!clip.contains(&7));
    }

    #[test]
    fn build_page_skip_sets_opacity_descendants_collected() {
        let mut d = Drawables::new();
        let node_id: usize = 30;
        d.block_styles
            .insert(node_id, block_entry_with_opacity_descendants(vec![31, 32]));
        let (_, _, opacity) = build_page_skip_sets(&d);
        assert!(opacity.contains(&31));
        assert!(opacity.contains(&32));
    }

    #[test]
    fn build_page_skip_sets_body_excluded_from_opacity_descendants() {
        let mut d = Drawables::new();
        let body_id: usize = 3;
        d.body_id = Some(body_id);
        d.block_styles
            .insert(body_id, block_entry_with_opacity_descendants(vec![4, 5]));
        let (_, _, opacity) = build_page_skip_sets(&d);
        assert!(
            !opacity.contains(&4),
            "body opacity descendants must be excluded"
        );
        assert!(!opacity.contains(&5));
    }

    #[test]
    fn build_page_skip_sets_table_overflow_clip_collects_descendants() {
        let mut d = Drawables::new();
        let style = crate::draw_primitives::BlockStyle {
            overflow_x: crate::draw_primitives::Overflow::Clip,
            ..Default::default()
        };
        d.tables.insert(
            40,
            crate::drawables::TableEntry {
                style,
                opacity: 1.0,
                visible: true,
                id: None,
                layout_size: None,
                width: 200.0_f32.as_pt(),
                cached_height: 100.0_f32.as_pt(),
                clip_descendants: vec![41, 42],
            },
        );
        let (_, clip, _) = build_page_skip_sets(&d);
        assert!(clip.contains(&41));
        assert!(clip.contains(&42));
    }

    #[test]
    fn build_page_skip_sets_root_excluded_from_clip_descendants() {
        let mut d = Drawables::new();
        let root_id: usize = 2;
        d.root_id = Some(root_id);
        d.block_styles
            .insert(root_id, block_entry_with_overflow_clip(vec![3, 4]));
        let (_, clip, _) = build_page_skip_sets(&d);
        assert!(!clip.contains(&3), "root clip descendants must be excluded");
        assert!(!clip.contains(&4));
    }

    #[test]
    fn build_page_skip_sets_root_excluded_from_opacity_descendants() {
        let mut d = Drawables::new();
        let root_id: usize = 7;
        d.root_id = Some(root_id);
        d.block_styles
            .insert(root_id, block_entry_with_opacity_descendants(vec![8, 9]));
        let (_, _, opacity) = build_page_skip_sets(&d);
        assert!(
            !opacity.contains(&8),
            "root opacity descendants must be excluded"
        );
        assert!(!opacity.contains(&9));
    }

    // --- table_box_size ---

    fn make_table_entry_for_size(
        layout_size: Option<crate::draw_primitives::Size>,
        width: f32,
        cached_height: f32,
    ) -> crate::drawables::TableEntry {
        use crate::units::F32Units;
        crate::drawables::TableEntry {
            style: crate::draw_primitives::BlockStyle::default(),
            opacity: 1.0,
            visible: true,
            id: None,
            layout_size,
            width: width.as_pt(),
            cached_height: cached_height.as_pt(),
            clip_descendants: vec![],
        }
    }

    fn make_frag_with_height(height: f32) -> crate::pagination_layout::Fragment {
        crate::pagination_layout::Fragment {
            page_index: 0,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 0.0_f32.as_px(),
            height: height.as_px(),
        }
    }

    #[test]
    fn table_box_size_with_layout_size_uses_layout_dimensions() {
        // When layout_size is Some, both width and height come from it,
        // ignoring entry.width, entry.cached_height, and frag.height.
        use crate::units::F32Units;
        let sz = crate::draw_primitives::Size {
            width: 120.0_f32.as_pt(),
            height: 80.0_f32.as_pt(),
        };
        let entry = make_table_entry_for_size(Some(sz), 200.0, 50.0);
        let frag = make_frag_with_height(99.0);
        let geom = crate::pagination_layout::PaginationGeometry {
            fragments: vec![frag.clone()],
            is_repeat: false,
        };
        let (w, h) = table_box_size(&entry, &geom, &frag);
        assert!((w - 120.0).abs() < 0.001, "width from layout_size");
        assert!((h - 80.0).abs() < 0.001, "height from layout_size");
    }

    #[test]
    fn table_box_size_split_table_uses_fragment_height() {
        use crate::units::F32Units;
        let sz = crate::draw_primitives::Size {
            width: 120.0_f32.as_pt(),
            height: 80.0_f32.as_pt(),
        };
        let entry = make_table_entry_for_size(Some(sz), 200.0, 50.0);
        let frag = make_frag_with_height(40.0);
        let mut continuation = frag.clone();
        continuation.page_index = 1;
        let geom = crate::pagination_layout::PaginationGeometry {
            fragments: vec![frag.clone(), continuation],
            is_repeat: false,
        };
        let (_, h) = table_box_size(&entry, &geom, &frag);
        assert!((h - 30.0).abs() < 0.01, "height = frag.height.in_pt()");
    }

    #[test]
    fn table_box_size_no_layout_size_nonzero_frag_uses_in_pt_height() {
        // frag.height = 40 CSS px → 40 × 0.75 = 30 PDF pt (via Px::in_pt)
        let entry = make_table_entry_for_size(None, 150.0, 99.0);
        let frag = make_frag_with_height(40.0);
        let geom = crate::pagination_layout::PaginationGeometry {
            fragments: vec![frag.clone()],
            is_repeat: false,
        };
        let (w, h) = table_box_size(&entry, &geom, &frag);
        assert!((w - 150.0).abs() < 0.001, "width falls back to entry.width");
        assert!((h - 30.0).abs() < 0.01, "height = frag.height.in_pt()");
    }

    #[test]
    fn table_box_size_split_zero_height_falls_back_to_layout_size() {
        // The paint guard in `draw_table_v2` / `draw_under_clip_table`
        // skips this slice. `table_box_size` itself must keep the
        // layout_size fallback so unsplit callers are unchanged.
        use crate::units::F32Units;
        let sz = crate::draw_primitives::Size {
            width: 120.0_f32.as_pt(),
            height: 80.0_f32.as_pt(),
        };
        let entry = make_table_entry_for_size(Some(sz), 200.0, 50.0);
        let frag = make_frag_with_height(0.0);
        let mut continuation = make_frag_with_height(40.0);
        continuation.page_index = 1;
        let geom = crate::pagination_layout::PaginationGeometry {
            fragments: vec![frag.clone(), continuation],
            is_repeat: false,
        };
        let (_, h) = table_box_size(&entry, &geom, &frag);
        assert!(
            (h - 80.0).abs() < 0.001,
            "split zero-height still reports layout_size; paint must skip"
        );
    }

    #[test]
    fn table_box_size_no_layout_size_zero_frag_falls_back_to_cached_height() {
        // When frag.height is 0, Px(0).in_pt() = Pt(0.0) which fails the `> 0.0`
        // guard, so cached_height is used instead.
        let entry = make_table_entry_for_size(None, 150.0, 55.0);
        let frag = make_frag_with_height(0.0);
        let geom = crate::pagination_layout::PaginationGeometry {
            fragments: vec![frag.clone()],
            is_repeat: false,
        };
        let (w, h) = table_box_size(&entry, &geom, &frag);
        assert!((w - 150.0).abs() < 0.001, "width falls back to entry.width");
        assert!(
            (h - 55.0).abs() < 0.001,
            "height falls back to cached_height when frag.height is zero"
        );
    }

    // --- build_struct_tree ---

    fn make_p_semantic_entry(parent: Option<usize>) -> crate::tagging::SemanticEntry {
        crate::tagging::SemanticEntry {
            tag: crate::tagging::PdfTag::P,
            parent,
            alt_text: None,
        }
    }

    fn make_h1_semantic_entry(parent: Option<usize>) -> crate::tagging::SemanticEntry {
        crate::tagging::SemanticEntry {
            tag: crate::tagging::PdfTag::H { level: 1 },
            parent,
            alt_text: None,
        }
    }

    #[test]
    fn build_struct_tree_empty_inputs_yields_empty_tree() {
        let tc = crate::draw_primitives::TagCollector::new();
        let d = Drawables::new();
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert!(
            tree.children.is_empty(),
            "empty inputs should produce no tree nodes"
        );
    }

    #[test]
    fn build_struct_tree_single_p_node_produces_one_root_group() {
        let node_id: usize = 1;
        let id = make_identifier();
        let mut tc = crate::draw_primitives::TagCollector::new();
        tc.record(node_id, crate::tagging::PdfTag::P, id, None);
        let mut d = Drawables::new();
        d.semantics.insert(node_id, make_p_semantic_entry(None));
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert_eq!(tree.children.len(), 1, "one semantic root → one Group");
        assert!(matches!(tree.children[0], Node::Group(_)));
    }

    #[test]
    fn build_struct_tree_tc_entry_heading_title_is_forwarded() {
        // An H2 node recorded via tc.record with an explicit heading_title should
        // result in a single root Group in the tree (title is opaque to test code,
        // so we only verify the structure).
        let node_id: usize = 10;
        let id = make_identifier();
        let mut tc = crate::draw_primitives::TagCollector::new();
        tc.record(
            node_id,
            crate::tagging::PdfTag::H { level: 2 },
            id,
            Some("Section title".to_string()),
        );
        let mut d = Drawables::new();
        d.semantics.insert(node_id, make_h1_semantic_entry(None));
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert_eq!(tree.children.len(), 1, "one root Group");
        assert!(matches!(tree.children[0], Node::Group(_)));
    }

    #[test]
    fn build_struct_tree_run_entry_h_tag_backfills_title_from_paragraph() {
        // Node uses per-run tagging (tc.record_run, not tc.record), so
        // heading_titles starts empty. The backfill loop in build_struct_tree
        // should detect the H tag + non-empty paragraph text and insert the title.
        let node_id: usize = 20;
        let id = make_identifier();
        let mut tc = crate::draw_primitives::TagCollector::new();
        tc.record_run(
            node_id,
            crate::draw_primitives::ParagraphRunItem::Content(id),
        );
        let mut d = Drawables::new();
        d.semantics.insert(node_id, make_h1_semantic_entry(None));
        let text_line = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "Intro", None,
        ))]);
        d.paragraphs.insert(node_id, make_para(vec![text_line]));
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert_eq!(tree.children.len(), 1, "one root Group from run_entries");
        assert!(matches!(tree.children[0], Node::Group(_)));
    }

    #[test]
    fn build_struct_tree_child_semantic_is_nested_under_parent() {
        // parent_id has no parent (root); child_id has parent = parent_id.
        // After build_struct_tree only parent_id appears at the tree root,
        // with child_id nested one level deeper.
        let parent_id: usize = 1;
        let child_id: usize = 2;
        let parent_tc_id = make_identifier();
        let child_tc_id = make_identifier();
        let mut tc = crate::draw_primitives::TagCollector::new();
        tc.record(parent_id, crate::tagging::PdfTag::P, parent_tc_id, None);
        tc.record(child_id, crate::tagging::PdfTag::Span, child_tc_id, None);
        let mut d = Drawables::new();
        d.semantics.insert(parent_id, make_p_semantic_entry(None));
        d.semantics.insert(
            child_id,
            crate::tagging::SemanticEntry {
                tag: crate::tagging::PdfTag::Span,
                parent: Some(parent_id),
                alt_text: None,
            },
        );
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert_eq!(tree.children.len(), 1, "only parent_id at root level");
        // parent group: Leaf(parent_tc_id) + Group(child). Use a matches! guard
        // to inspect the inner structure without a let-else panic arm that would
        // be unreachable in a passing test and flagged by coverage tools.
        assert!(
            matches!(
                &tree.children[0],
                Node::Group(g)
                    if g.children.len() == 2 && matches!(g.children[1], Node::Group(_))
            ),
            "root Group should contain Leaf + nested child Group"
        );
    }

    // --- build_metadata ---
    //
    // krilla::metadata::Metadata fields are pub(crate), so we use Debug formatting
    // to assert field values where meaningful behavior needs verification.

    #[test]
    fn build_metadata_default_config_does_not_panic() {
        // Config::default() has producer=Some("fulgur") — exercises the producer
        // branch. All other optional fields are None / empty, so their branches
        // are skipped without panicking.
        let config = Config::default();
        let meta = build_metadata(&config, None);
        let debug = format!("{meta:?}");
        assert!(
            debug.contains("\"fulgur\""),
            "default producer should appear in metadata"
        );
    }

    #[test]
    fn build_metadata_config_title_overrides_html_title() {
        // config.title is Some → effective_title = Some("Config Title"), html_title ignored.
        let config = Config {
            title: Some("Config Title".to_string()),
            ..Default::default()
        };
        let meta = build_metadata(&config, Some("HTML Title"));
        let debug = format!("{meta:?}");
        assert!(
            debug.contains("\"Config Title\""),
            "config.title should take priority"
        );
        assert!(
            !debug.contains("\"HTML Title\""),
            "html_title should be ignored when config.title is set"
        );
    }

    #[test]
    fn build_metadata_html_title_fallback_when_config_title_none() {
        // config.title is None → effective_title = html_title via .or().
        let config = Config::default();
        let meta = build_metadata(&config, Some("HTML Title"));
        let debug = format!("{meta:?}");
        assert!(
            debug.contains("\"HTML Title\""),
            "html_title should be used as fallback when config.title is None"
        );
    }

    #[test]
    fn build_metadata_with_authors() {
        let config = Config {
            authors: vec!["Alice".to_string(), "Bob".to_string()],
            ..Default::default()
        };
        let _ = build_metadata(&config, None);
    }

    #[test]
    fn build_metadata_with_description() {
        let config = Config {
            description: Some("A test document.".to_string()),
            ..Default::default()
        };
        let _ = build_metadata(&config, None);
    }

    #[test]
    fn build_metadata_with_keywords() {
        let config = Config {
            keywords: vec!["rust".to_string(), "pdf".to_string()],
            ..Default::default()
        };
        let _ = build_metadata(&config, None);
    }

    #[test]
    fn build_metadata_with_lang() {
        let config = Config {
            lang: Some("ja".to_string()),
            ..Default::default()
        };
        let _ = build_metadata(&config, None);
    }

    #[test]
    fn build_metadata_with_creator() {
        let config = Config {
            creator: Some("FulgurTest".to_string()),
            ..Default::default()
        };
        let _ = build_metadata(&config, None);
    }

    #[test]
    fn build_metadata_with_valid_creation_date() {
        // Exercises the `if let Some(dt) = parse_datetime(date_str)` true branch.
        // Debug output shows `creation_date: Some(...)` when the date is valid.
        let config = Config {
            creation_date: Some("2026-05-12T10:30:00".to_string()),
            ..Default::default()
        };
        let meta = build_metadata(&config, None);
        let debug = format!("{meta:?}");
        assert!(
            debug.contains("creation_date: Some"),
            "valid ISO-8601 date should be parsed and set: {debug}"
        );
    }

    #[test]
    fn build_metadata_with_invalid_creation_date_does_not_set_date() {
        // parse_datetime returns None → the inner `if let Some(dt)` arm is skipped,
        // leaving creation_date unset on the Metadata struct.
        let config = Config {
            creation_date: Some("not-a-date".to_string()),
            ..Default::default()
        };
        let meta = build_metadata(&config, None);
        let debug = format!("{meta:?}");
        assert!(
            debug.contains("creation_date: None"),
            "invalid date should leave creation_date as None: {debug}"
        );
    }

    #[test]
    fn build_metadata_all_optional_fields_set() {
        let config = Config {
            title: Some("Full Test Title".to_string()),
            authors: vec!["Author A".to_string()],
            description: Some("Full description.".to_string()),
            keywords: vec!["key1".to_string(), "key2".to_string()],
            lang: Some("en".to_string()),
            creator: Some("Creator App".to_string()),
            producer: Some("Fulgur PDF".to_string()),
            creation_date: Some("2026-01-01".to_string()),
            ..Default::default()
        };
        let _ = build_metadata(&config, None);
    }

    // --- paragraph_lines_for_page: inline-image computed_y rebasing ---

    #[test]
    fn paragraph_lines_for_page_split_rebases_inline_image_computed_y() {
        // Two 12pt lines (each corresponds to a 16px CSS-px fragment).
        // 16px × PX_TO_PT (0.75) = 12pt.
        // Line 1 contains an Image whose computed_y = 15.0 is paragraph-absolute.
        // On page 1: consumed = Px(16).in_pt() = 12pt.
        // After rebasing: img.computed_y = 15.0 − 12.0 = 3.0.
        let img = crate::paragraph::InlineImage {
            data: Arc::new(vec![]),
            format: crate::image::ImageFormat::Png,
            width: 10.0_f32.as_pt(),
            height: 8.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: crate::paragraph::VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: 15.0_f32.as_pt(),
            link: None,
        };
        let line0 = make_line(12.0, 9.0); // page 0: no items
        let line1 = crate::paragraph::ShapedLine {
            height: 12.0_f32.as_pt(),
            baseline: 21.0_f32.as_pt(),
            items: vec![crate::paragraph::LineItem::Image(img)],
        };
        let fragments = vec![
            make_fragment(0, 16.0), // 16px → 12pt
            make_fragment(1, 16.0),
        ];
        let result = paragraph_lines_for_page(&[line0, line1], &fragments, 1, true);
        assert!(result.is_some(), "page 1 should yield Some");
        let sliced = result.unwrap();
        assert_eq!(sliced.len(), 1, "one line on page 1");
        if let crate::paragraph::LineItem::Image(img) = &sliced[0].items[0] {
            assert!(
                (img.computed_y.to_f32() - 3.0).abs() < 0.01,
                "expected computed_y=3.0 after rebasing, got {:?}",
                img.computed_y,
            );
        } else {
            panic!("expected Image item in sliced line");
        }
    }

    // --- build_struct_tree: H run_entry with missing / empty paragraph ---

    #[test]
    fn build_struct_tree_run_entry_h_no_paragraph_no_backfill() {
        // H node uses per-run tagging but has NO paragraph in drawables.
        // The backfill loop's `if let Some(para)` arm yields None → silent skip.
        let node_id: usize = 40;
        let id = make_identifier();
        let mut tc = crate::draw_primitives::TagCollector::new();
        tc.record_run(
            node_id,
            crate::draw_primitives::ParagraphRunItem::Content(id),
        );
        let mut d = Drawables::new();
        d.semantics.insert(node_id, make_h1_semantic_entry(None));
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert_eq!(tree.children.len(), 1);
        assert!(matches!(tree.children[0], Node::Group(_)));
    }

    #[test]
    fn build_struct_tree_run_entry_h_empty_title_no_backfill() {
        // H node uses per-run tagging; paragraph exists but contains only an
        // InlineBox (no Text runs). extract_heading_title returns "" →
        // `if !title.is_empty()` is false → heading_titles entry is NOT inserted.
        let node_id: usize = 41;
        let id = make_identifier();
        let mut tc = crate::draw_primitives::TagCollector::new();
        tc.record_run(
            node_id,
            crate::draw_primitives::ParagraphRunItem::Content(id),
        );
        let mut d = Drawables::new();
        d.semantics.insert(node_id, make_h1_semantic_entry(None));
        let ib = crate::paragraph::InlineBoxItem {
            node_id: None,
            width: 50.0_f32.as_pt(),
            height: 20.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible: true,
        };
        let line = crate::paragraph::ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![crate::paragraph::LineItem::InlineBox(ib)],
        };
        d.paragraphs.insert(node_id, make_para(vec![line]));
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert_eq!(tree.children.len(), 1);
        assert!(matches!(tree.children[0], Node::Group(_)));
    }

    // --- build_page_skip_sets: table overflow_clip with empty descendants ---

    #[test]
    fn build_page_skip_sets_table_overflow_clip_empty_descendants_not_added() {
        // A table with has_overflow_clip() = true but clip_descendants empty must
        // NOT extend clipped_descendants — the `&& !table.clip_descendants.is_empty()`
        // guard short-circuits before `extend`.
        let mut d = Drawables::new();
        let style = crate::draw_primitives::BlockStyle {
            overflow_x: crate::draw_primitives::Overflow::Clip,
            ..Default::default()
        };
        d.tables.insert(
            50,
            crate::drawables::TableEntry {
                style,
                opacity: 1.0,
                visible: true,
                id: None,
                layout_size: None,
                width: 100.0_f32.as_pt(),
                cached_height: 50.0_f32.as_pt(),
                clip_descendants: vec![],
            },
        );
        let (_, clip, _) = build_page_skip_sets(&d);
        assert!(clip.is_empty(), "empty clip_descendants → nothing added");
    }

    // --- build_multicol_stroke ---

    fn rule_spec(
        width: f32,
        style: crate::column_css::ColumnRuleStyle,
    ) -> crate::column_css::ColumnRuleSpec {
        use crate::units::F32Units;
        crate::column_css::ColumnRuleSpec {
            width: width.as_pt(),
            style,
            color: [0, 0, 0, 255],
        }
    }

    #[test]
    fn build_multicol_stroke_none_style_returns_none() {
        let rule = rule_spec(2.0, crate::column_css::ColumnRuleStyle::None);
        assert!(build_multicol_stroke(&rule).is_none());
    }

    #[test]
    fn build_multicol_stroke_zero_width_returns_none() {
        let rule = rule_spec(0.0, crate::column_css::ColumnRuleStyle::Solid);
        assert!(build_multicol_stroke(&rule).is_none());
    }

    #[test]
    fn build_multicol_stroke_negative_width_returns_none() {
        let rule = rule_spec(-1.0, crate::column_css::ColumnRuleStyle::Solid);
        assert!(build_multicol_stroke(&rule).is_none());
    }

    #[test]
    fn build_multicol_stroke_solid_returns_some_without_dash() {
        let rule = rule_spec(3.0, crate::column_css::ColumnRuleStyle::Solid);
        let stroke = build_multicol_stroke(&rule).expect("solid rule should return Some");
        assert!(stroke.dash.is_none(), "solid rule should have no dash");
        assert_eq!(
            stroke.line_cap,
            krilla::paint::LineCap::Butt,
            "solid default cap is Butt"
        );
        assert!(
            (stroke.width - 3.0).abs() < 0.001,
            "width should match rule.width"
        );
    }

    #[test]
    fn build_multicol_stroke_dashed_has_expected_dash_array() {
        let w = 4.0_f32;
        let rule = rule_spec(w, crate::column_css::ColumnRuleStyle::Dashed);
        let stroke = build_multicol_stroke(&rule).expect("dashed rule should return Some");
        let dash = stroke.dash.expect("dashed rule should have a dash");
        assert_eq!(dash.array, vec![w * 3.0, w * 2.0], "dash array is [3w, 2w]");
        assert!((dash.offset - 0.0).abs() < 0.001, "dash offset should be 0");
    }

    #[test]
    fn build_multicol_stroke_dotted_has_round_cap_and_expected_dash_array() {
        let w = 2.0_f32;
        let rule = rule_spec(w, crate::column_css::ColumnRuleStyle::Dotted);
        let stroke = build_multicol_stroke(&rule).expect("dotted rule should return Some");
        assert_eq!(
            stroke.line_cap,
            krilla::paint::LineCap::Round,
            "dotted cap is Round"
        );
        let dash = stroke.dash.expect("dotted rule should have a dash");
        assert_eq!(dash.array, vec![0.0, w * 2.0], "dash array is [0, 2w]");
        assert!((dash.offset - 0.0).abs() < 0.001, "dash offset should be 0");
    }

    // --- build_struct_tree: missing branches ---

    #[test]
    fn build_struct_tree_run_entry_p_tag_skips_heading_backfill() {
        // A node in run_entries with PdfTag::P (not H).
        // The backfill loop's `if matches!(entry.tag, PdfTag::H { .. })` is false → skip.
        // The tree should still build correctly via build_tag_group's run_entries branch.
        let node_id: usize = 50;
        let id = make_identifier();
        let mut tc = crate::draw_primitives::TagCollector::new();
        tc.record_run(
            node_id,
            crate::draw_primitives::ParagraphRunItem::Content(id),
        );
        let mut d = Drawables::new();
        d.semantics.insert(node_id, make_p_semantic_entry(None));
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert_eq!(tree.children.len(), 1, "one root Group");
        assert!(matches!(tree.children[0], Node::Group(_)));
    }

    #[test]
    fn build_struct_tree_run_entry_h_already_in_heading_titles_continues() {
        // A node appears in both tc_entries (populating heading_titles) AND run_entries.
        // The backfill loop's `if heading_titles.contains_key(&node_id) { continue; }` fires.
        let node_id: usize = 60;
        let tc_id = make_identifier();
        let run_id = make_identifier();
        let mut tc = crate::draw_primitives::TagCollector::new();
        tc.record(
            node_id,
            crate::tagging::PdfTag::H { level: 1 },
            tc_id,
            Some("Pre-existing title".to_string()),
        );
        tc.record_run(
            node_id,
            crate::draw_primitives::ParagraphRunItem::Content(run_id),
        );
        let mut d = Drawables::new();
        d.semantics.insert(node_id, make_h1_semantic_entry(None));
        let text_line = make_shaped_line(vec![crate::paragraph::LineItem::Text(make_glyph_run(
            "Different title",
            None,
        ))]);
        d.paragraphs.insert(node_id, make_para(vec![text_line]));
        let link_annot_ids: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut tree = TagTree::new();
        build_struct_tree(tc, &d, &link_annot_ids, &mut tree);
        assert_eq!(tree.children.len(), 1, "one root Group");
        assert!(matches!(tree.children[0], Node::Group(_)));
    }

    // --- decode_image_for_v2: Jpeg and Gif format branches ---

    fn make_image_entry(
        format: crate::image::ImageFormat,
        data: Vec<u8>,
    ) -> crate::drawables::ImageEntry {
        crate::drawables::ImageEntry {
            image_data: Arc::new(data),
            format,
            width: 10.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            opacity: 1.0,
            visible: true,
        }
    }

    #[test]
    fn decode_image_for_v2_jpeg_invalid_data_returns_none() {
        let entry = make_image_entry(crate::image::ImageFormat::Jpeg, vec![0xFF, 0xD8, 0xFF]);
        let result = decode_image_for_v2(&entry);
        assert!(result.is_none(), "invalid JPEG bytes should return None");
    }

    #[test]
    fn decode_image_for_v2_gif_invalid_data_returns_none() {
        let entry = make_image_entry(crate::image::ImageFormat::Gif, b"GIF89a".to_vec());
        let result = decode_image_for_v2(&entry);
        assert!(result.is_none(), "invalid GIF bytes should return None");
    }

    // --- MarginBoxRenderer::new: non-empty GCPM mapping branches ---

    fn empty_geometry() -> crate::pagination_layout::PaginationGeometryTable {
        BTreeMap::new()
    }

    #[test]
    fn margin_box_renderer_new_with_string_set_mappings_takes_else_branch() {
        // Non-empty string_set_mappings forces the else branch at
        // `collect_string_set_states(pagination_geometry, &by_node_btree)`.
        let gcpm = crate::gcpm::GcpmContext {
            string_set_mappings: vec![crate::gcpm::StringSetMapping {
                parsed: crate::gcpm::ParsedSelector::Class("hd".to_string()),
                name: "chapter".to_string(),
                values: vec![crate::gcpm::StringSetValue::ContentText],
            }],
            ..Default::default()
        };
        let store = RunningElementStore::default();
        let geom = empty_geometry();
        let implicit: BTreeMap<usize, String> = BTreeMap::new();
        let _mbr = MarginBoxRenderer::new(
            &gcpm,
            &store,
            &[],
            false,
            &geom,
            &HashMap::new(),
            &BTreeMap::new(),
            1,
            &implicit,
        );
    }

    #[test]
    fn margin_box_renderer_new_with_running_mappings_takes_else_branch() {
        // Non-empty running_mappings forces the else branch at
        // `collect_running_element_states(pagination_geometry, running_store)`.
        let gcpm = crate::gcpm::GcpmContext {
            running_mappings: vec![crate::gcpm::RunningMapping {
                parsed: crate::gcpm::ParsedSelector::Class("hd".to_string()),
                running_name: "header".to_string(),
            }],
            ..Default::default()
        };
        let store = RunningElementStore::default();
        let geom = empty_geometry();
        let implicit: BTreeMap<usize, String> = BTreeMap::new();
        let _mbr = MarginBoxRenderer::new(
            &gcpm,
            &store,
            &[],
            false,
            &geom,
            &HashMap::new(),
            &BTreeMap::new(),
            1,
            &implicit,
        );
    }

    #[test]
    fn margin_box_renderer_new_with_counter_mappings_takes_else_branch() {
        // Non-empty counter_mappings forces the else branch at
        // `collect_counter_states(pagination_geometry, counter_ops_by_node)`.
        let gcpm = crate::gcpm::GcpmContext {
            counter_mappings: vec![crate::gcpm::CounterMapping {
                parsed: crate::gcpm::ParsedSelector::Tag("h1".to_string()),
                ops: vec![crate::gcpm::CounterOp::Reset {
                    name: "chapter".to_string(),
                    value: 0,
                }],
            }],
            ..Default::default()
        };
        let store = RunningElementStore::default();
        let geom = empty_geometry();
        let implicit: BTreeMap<usize, String> = BTreeMap::new();
        let _mbr = MarginBoxRenderer::new(
            &gcpm,
            &store,
            &[],
            false,
            &geom,
            &HashMap::new(),
            &BTreeMap::new(),
            1,
            &implicit,
        );
    }

    // ── Inline smoke tests: exercise rendering paths for --lib coverage ──────
    //
    // Integration smoke tests in crates/fulgur/tests/render_smoke.rs are
    // excluded from `cargo llvm-cov --lib` measurement. These inline tests
    // exercise the same rendering functions (draw_block_v2, draw_paragraph_v2,
    // draw_image_v2, draw_svg_v2, draw_table_v2, draw_under_transform/clip/
    // opacity, try_start_tagged, paint_multicol_paragraph_slices, …) through
    // Engine::render_html so the coverage gate sees them.

    fn render_html(html: &str) -> Vec<u8> {
        crate::engine::Engine::builder()
            .build()
            .render(html)
            .expect("render failed")
    }

    // Valid 1×1 red PNG for image tests (same fixture used in render_smoke.rs).
    const RED_1X1_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    // --- draw_block_v2 / draw_block_inner_paint / draw_paragraph_v2 ---

    #[test]
    fn render_smoke_block_with_background_and_border() {
        // Exercises draw_block_v2 → draw_block_inner_paint (bg + border paint).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="width:100px;height:60px;background:#cef;
                        border:2px solid #44a;padding:8px;">
              content
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_paragraph_with_text() {
        // Exercises draw_paragraph_v2 → draw_paragraph_inner_paint →
        // paragraph_lines_for_page (non-split) → draw_shaped_lines text arm.
        let pdf = render_html(r#"<!doctype html><html><body><p>Hello, world.</p></body></html>"#);
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_block_with_inline_text_content() {
        // Exercises draw_block_with_inner_content (block + inline paragraph)
        // and dispatch_fragment's paragraph-without-block path.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="background:#fee;border:1px solid #c88;padding:4px;">
              Styled inline text block.
            </p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_list_item_with_block / draw_list_item_marker ---

    #[test]
    fn render_smoke_unordered_list_with_disc_markers() {
        // Exercises draw_list_item_with_block + draw_list_item_marker
        // (outside/disc default marker path).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <ul><li>Alpha</li><li>Beta</li><li>Gamma</li></ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_ordered_list_with_decimal_markers() {
        // Exercises draw_list_item_marker's counter text path.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <ol><li>One</li><li>Two</li><li>Three</li></ol>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_list_style_inside() {
        // list-style-position: inside routes through a different marker
        // placement path in draw_list_item_marker.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <ul style="list-style-position:inside">
              <li>Item A</li><li>Item B</li>
            </ul>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_image_v2 / draw_image_inner_paint / decode_image_for_v2 ---

    #[test]
    fn render_smoke_raster_image_in_flow() {
        // Exercises draw_image_v2 → draw_image_inner_paint → decode_image_for_v2
        // PNG branch. The image is registered via AssetBundle.
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("img.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <img src="img.png" style="width:64px;height:64px;">
                </body></html>"#,
            )
            .expect("render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_svg_v2 / draw_svg_inner_paint ---

    #[test]
    fn render_smoke_svg_inline_in_flow() {
        // Exercises draw_svg_v2 → draw_svg_inner_paint.
        let pdf = render_html(
            r##"<!doctype html><html><body>
            <svg xmlns="http://www.w3.org/2000/svg" width="60" height="60">
              <rect x="5" y="5" width="50" height="50" fill="#3af"/>
            </svg>
            </body></html>"##,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_table_v2 / paint_table_outer_frame ---

    #[test]
    fn render_smoke_table_with_borders() {
        // Exercises draw_table_v2 → table_box_size + paint_table_outer_frame.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <table border="1" style="border-collapse:collapse">
              <tr><th>A</th><th>B</th></tr>
              <tr><td>1</td><td>2</td></tr>
            </table>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_table_overflow_clip() {
        // Exercises draw_under_clip_table when a table has overflow:hidden
        // and descendant cells need clipping.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <table style="width:200px;overflow:hidden;background:#eef">
              <tr>
                <td style="background:#cef;padding:4px">Cell 1</td>
                <td style="background:#fce;padding:4px">Cell 2</td>
              </tr>
            </table>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_transform ---

    #[test]
    fn render_smoke_transform_rotate() {
        // Exercises draw_under_transform (the `transform: rotate(…)` path).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="width:80px;height:40px;background:#cef;transform:rotate(15deg)">
              rotated
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_clip ---

    #[test]
    fn render_smoke_overflow_hidden_clips_child() {
        // Exercises draw_under_clip with a child that overflows its container.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="width:80px;height:40px;overflow:hidden;background:#cef">
              <div style="width:200px;height:20px;background:#f99">clipped</div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_opacity ---

    #[test]
    fn render_smoke_opacity_on_block_with_child_svg() {
        // Exercises draw_under_opacity: a block with fractional opacity wrapping
        // an SVG child forces the opacity_descendants path.
        let pdf = render_html(
            r##"<!doctype html><html><body>
            <div style="opacity:0.5">
              <svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
                <circle cx="20" cy="20" r="15" fill="#e74"/>
              </svg>
            </div>
            </body></html>"##,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- try_start_tagged / finish_tagged ---

    #[test]
    fn render_smoke_tagged_pdf_headings_and_paragraphs() {
        // Exercises try_start_tagged + finish_tagged for H and P entries.
        let pdf = crate::engine::Engine::builder()
            .tagged(true)
            .lang("en")
            .build()
            .render(
                r#"<!doctype html><html><body>
                <h1>Heading One</h1>
                <p>A paragraph of text.</p>
                <h2>Heading Two</h2>
                <p>Another paragraph.</p>
                </body></html>"#,
            )
            .expect("tagged render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- paint_multicol_paragraph_slices ---

    #[test]
    fn render_smoke_multicol_with_paragraph_text() {
        // Exercises paint_multicol_paragraph_slices: a multicol container whose
        // columns each hold inline text. Without this, the slice-distribution
        // logic for paragraph spans across multicol columns is untouched.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="column-count:2;column-gap:20px;width:400px">
              <p>Column text alpha beta gamma delta epsilon zeta eta theta.</p>
              <p>Second paragraph iota kappa lambda mu nu xi omicron pi rho.</p>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- render_page (MarginBoxRenderer) / draw_v2_page ---

    #[test]
    fn render_smoke_page_margin_box_bottom_center() {
        // Exercises draw_v2_page → MarginBoxRenderer::render_page for an
        // @bottom-center margin box — the most common GCPM use case.
        let pdf = render_html(
            r#"<!doctype html><html><head><style>
            @page {
              size: A4; margin: 20mm;
              @bottom-center { content: "Page footer"; }
            }
            </style></head><body><p>Body content.</p></body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_page_with_body_background_multi_page() {
        // Exercises paint_root_block_v2 (html + body pre-passes on every page)
        // and draw_v2_page across multiple pages.
        let pdf = render_html(
            r#"<!doctype html><html><head><style>
            html,body{margin:0;background:#fafafa}
            .tall{height:900px;background:#cef}
            </style></head><body>
            <div class="tall"></div><div class="tall"></div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- dispatch_fragment: visibility skip path ---

    #[test]
    fn render_smoke_visibility_hidden_element_skipped() {
        // Exercises the `!block.visible` early-return in dispatch_fragment
        // (and draw_block_v2). The element should not paint but render must succeed.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="visibility:hidden;width:100px;height:50px;background:red"></div>
            <div style="width:100px;height:50px;background:#cef">visible</div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- decode_image_for_v2: valid PNG path (non-trivial decode) ---

    #[test]
    fn render_smoke_png_image_decode_success() {
        // Exercises decode_image_for_v2's Png branch with valid data, ensuring
        // ImageFormat::to_krilla_image returns Ok and draw_image_inner_paint fires.
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("red.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body style="margin:0">
                <div style="background:url(red.png);width:80px;height:80px"></div>
                </body></html>"#,
            )
            .expect("render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- bookmarks path in render_v2 ---

    #[test]
    fn render_smoke_bookmarks_from_headings() {
        // Exercises the bookmark_collector path in render_v2:
        // bookmark_collector.set_current_page(page_idx) and the BookmarkPass
        // in engine.rs (UA CSS bookmark_mappings → effective_bookmarks).
        let pdf = crate::engine::Engine::builder()
            .bookmarks(true)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <h1>Chapter One</h1>
                <p>Body content for chapter one.</p>
                <h2>Section 1.1</h2>
                <p>Content for section 1.1.</p>
                <h2>Section 1.2</h2>
                <p>Content for section 1.2.</p>
                </body></html>"#,
            )
            .expect("render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- PDF/UA mode (Validator::UA1) ---

    #[test]
    fn render_smoke_pdf_ua_mode() {
        // Exercises the Validator::UA1 configuration branch in render_v2:
        // Configuration::new_with_validator(Validator::UA1) when config.pdf_ua = true.
        // pdf_ua implies both enable_tagging and effective_bookmarks.
        // PDF/UA-1 requires a document title (NoDocumentTitle validation).
        let pdf = crate::engine::Engine::builder()
            .pdf_ua(true)
            .lang("en")
            .title("PDF/UA Test Document")
            .build()
            .render(
                r#"<!doctype html><html><body>
                <h1>Title</h1>
                <p>A paragraph of body text.</p>
                </body></html>"#,
            )
            .expect("render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- try_start_tagged: PdfTag::Li branch + draw_list_item_marker_tagged ---

    #[test]
    fn render_smoke_tagged_unordered_list() {
        // Exercises try_start_tagged's PdfTag::Li branch (lines ~924-931),
        // draw_list_item_marker_tagged with lbl_id set, and the li_lbody_ids
        // path in draw_list_item_with_block. Requires tagged mode to activate
        // the tagging collector that populates li_lbl_ids / li_lbody_ids.
        let pdf = crate::engine::Engine::builder()
            .tagged(true)
            .lang("en")
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul>
                  <li>First item</li>
                  <li>Second item</li>
                  <li>Third item</li>
                </ul>
                </body></html>"#,
            )
            .expect("render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- use_run_tagging = true path (tagged paragraph containing a hyperlink) ---

    #[test]
    fn render_smoke_tagged_paragraph_with_hyperlink() {
        // When tagging is enabled and a paragraph contains an <a href>, the
        // paragraph's glyph runs include LinkSpan entries → para_has_link_runs
        // returns true → use_run_tagging = true in dispatch_fragment.
        // Exercises: link_run_node_id assignment, per-run TagCollector recording.
        let pdf = crate::engine::Engine::builder()
            .tagged(true)
            .lang("en")
            .build()
            .render(
                r#"<!doctype html><html><body>
                <p>Visit <a href="https://example.com">our website</a> for more info.</p>
                <p>Another <a href="https://other.example.com">link here</a> too.</p>
                </body></html>"#,
            )
            .expect("render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- link annotation path (non-tagged, link_collector emission) ---

    #[test]
    fn render_smoke_paragraph_with_external_link() {
        // In non-tagged mode, <a href="https://..."> produces a link annotation
        // via link_collector. Exercises draw_shaped_lines link annotation path
        // and dest_registry external link emission.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p>See <a href="https://example.com">example.com</a> for details.</p>
            <p>Also see <a href="https://other.org">other.org</a>.</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- dest_registry.record: internal anchor id on a paragraph node ---

    #[test]
    fn render_smoke_internal_anchor_link() {
        // An element with an `id` attribute is registered in dest_registry
        // (render_v2 pre-pass, lines ~93-121). An <a href="#target"> produces
        // an internal link annotation pointing to that destination.
        let pdf = render_html(
            r##"<!doctype html><html><body>
            <p id="intro">Introduction paragraph.</p>
            <p>Go to <a href="#intro">the intro</a>.</p>
            </body></html>"##,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- nested overflow:hidden blocks (draw_under_clip recursive call) ---

    #[test]
    fn render_smoke_nested_overflow_clip_blocks() {
        // A block with overflow:hidden containing another block with overflow:hidden
        // triggers the nested-clip recursion in draw_under_clip (lines ~1866-1893):
        // the outer clip's descendant loop dispatches inner clip blocks via a
        // recursive draw_under_clip call so each has its own push_clip_path.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="width:120px;height:80px;overflow:hidden;background:#def">
              <div style="width:90px;height:60px;overflow:hidden;background:#fed">
                <p style="width:200px">Clipped text in nested overflow hidden blocks.</p>
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- image inside overflow:hidden (draw_under_clip img_for_block path) ---

    #[test]
    fn render_smoke_image_inside_overflow_clip() {
        // A block sharing node_id with an image, inside an overflow:hidden container,
        // triggers draw_under_clip's img_for_block path (lines ~1770-1772):
        // draw_image_inner_paint is called at the content-box-offset coordinates
        // inside the pushed clip path.
        let mut bundle = crate::asset::AssetBundle::default();
        bundle.add_image("red.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(bundle)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <div style="width:60px;height:40px;overflow:hidden">
                  <img src="red.png" style="width:100px;height:100px">
                </div>
                </body></html>"#,
            )
            .expect("render");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- build_metadata: producer=None branch ---

    #[test]
    fn build_metadata_producer_none_skips_producer_field() {
        // Config::default() sets producer=Some("fulgur"). Override with None to
        // exercise the `if let Some(ref producer)` false branch in build_metadata.
        let config = crate::config::Config {
            producer: None,
            ..Default::default()
        };
        let meta = build_metadata(&config, None);
        let debug = format!("{meta:?}");
        assert!(
            debug.contains("producer: None"),
            "producer:None should leave the metadata producer field unset: {debug}"
        );
    }

    // --- multi-page document with tall content (is_split paths) ---

    #[test]
    fn render_smoke_tall_content_multi_page_split() {
        // A block taller than one page causes the fragmenter to record multiple
        // fragments per node, setting is_split=true. Exercises draw_block_inner_paint
        // and draw_under_opacity's is_split height-fix branches (fulgur-bq6i).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="height:1200px;background:#f0f4ff;border:2px solid #89c">
              Tall block spanning multiple A4 pages.
            </div>
            <p>Second page paragraph.</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- opacity block inside overflow:hidden (draw_under_clip → draw_under_opacity) ---

    #[test]
    fn render_smoke_opacity_block_inside_overflow_clip() {
        // A block with overflow:hidden whose clip_descendants include a node with
        // fractional opacity (non-empty opacity_descendants) triggers the
        // draw_under_clip → draw_under_opacity recursion (lines ~1916-1937).
        let pdf = render_html(
            r##"<!doctype html><html><body>
            <div style="width:150px;height:100px;overflow:hidden;background:#eef">
              <div style="opacity:0.5">
                <svg xmlns="http://www.w3.org/2000/svg" width="80" height="60">
                  <rect x="5" y="5" width="70" height="50" fill="#37f"/>
                </svg>
              </div>
            </div>
            </body></html>"##,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_opacity: block container with block-level children ---

    #[test]
    fn render_smoke_opacity_block_with_block_child_div() {
        // Triggers draw_under_opacity. The outer div has opacity:0.5 and contains
        // a block-level child div (not inline text), so block::convert processes
        // the outer div with opacity_scope=true and populates opacity_descendants.
        // draw_v2_page then calls draw_under_opacity for the outer div.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="opacity:0.5">
              <div style="width:100px;height:50px;background:#c34;"></div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_opacity_block_with_block_child_para_and_div() {
        // draw_under_opacity with multiple block children including a paragraph.
        // Exercises the para_for_block arm inside draw_under_opacity's else branch.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="opacity:0.4">
              <div style="width:80px;height:20px;background:#eef;"></div>
              <p>Paragraph inside opacity block.</p>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- paint_multicol_rule_for_page: column-rule CSS ---

    #[test]
    fn render_smoke_multicol_column_rule_solid() {
        // Triggers paint_multicol_rule_for_page and build_multicol_stroke's Solid
        // branch. column-rule requires build_multicol_stroke to return Some and
        // paint_multicol_rule_for_page to actually draw lines between columns.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="column-count:2;column-gap:20px;column-rule:2px solid #444;width:400px">
              <p>First column alpha beta gamma delta epsilon zeta eta theta.</p>
              <p>Second column iota kappa lambda mu nu xi omicron pi rho sigma.</p>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_multicol_column_rule_dashed() {
        // build_multicol_stroke's Dashed branch — sets dash array on stroke.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="column-count:2;column-gap:16px;column-rule:3px dashed #999;width:360px">
              <p>Dashed rule left column text alpha beta gamma.</p>
              <p>Dashed rule right column text delta epsilon zeta.</p>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_multicol_column_rule_dotted() {
        // build_multicol_stroke's Dotted branch — sets Round line cap + dash array.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="column-count:2;column-gap:16px;column-rule:4px dotted #555;width:360px">
              <p>Dotted rule left column text.</p>
              <p>Dotted rule right column text.</p>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_transform: nested / clip / opacity descendants ---

    #[test]
    fn render_smoke_nested_transforms() {
        // draw_under_transform's nested-transform branch (lines ~1436-1450):
        // outer transform dispatches an inner transform via recursive
        // draw_under_transform so both matrices compose.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="transform:rotate(5deg);width:100px;height:60px;background:#cef">
              <div style="transform:scale(0.8);background:#efc;width:80px;height:40px">
                nested transforms
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_overflow_clip_inside_transform() {
        // draw_under_transform's overflow-clip descendant branch (lines ~1451-1477):
        // an overflow:hidden block nested inside a transformed ancestor triggers
        // draw_under_clip from within draw_under_transform's descendant loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="transform:rotate(3deg);width:120px;height:80px;background:#def">
              <div style="overflow:hidden;width:90px;height:60px;background:#fed">
                <p style="width:200px">Overflow clipped inside transform.</p>
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_opacity_block_inside_transform() {
        // draw_under_transform's opacity-scoped descendant branch (lines ~1503-1528):
        // a block with opacity < 1 and block children, nested inside a transformed
        // ancestor, triggers draw_under_opacity from draw_under_transform's
        // descendant loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="transform:rotate(5deg);width:140px;height:100px;background:#eef">
              <div style="opacity:0.5">
                <div style="width:80px;height:50px;background:#c34;"></div>
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- paint_multicol_paragraph_slices: long paragraph spanning columns ---

    #[test]
    fn render_smoke_multicol_paragraph_spanning_columns() {
        // A single long paragraph inside a narrow multicol container forces
        // the paragraph to span multiple columns, triggering
        // convert_multicol_paragraph_slices → paragraph_slices map →
        // paint_multicol_paragraph_slices in dispatch_fragment.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="column-count:3;column-gap:10px;width:300px;font-size:12px">
              <p>Alpha beta gamma delta epsilon zeta eta theta iota kappa lambda
                 mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega.
                 Lorem ipsum dolor sit amet consectetur adipiscing elit sed do
                 eiusmod tempor incididunt ut labore et dolore magna aliqua.</p>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- dispatch_inline_box_content: transform on inline-block ---

    #[test]
    fn render_smoke_inline_block_with_transform_dispatches_under_transform() {
        // An inline-block element with `transform` inside a paragraph creates a
        // `LineItem::InlineBox` in the shaped line. `dispatch_inline_box_content`
        // must route it through `draw_under_transform` (the first branch,
        // `drawables.transforms.get(&content_id).is_some()`).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p>Before
              <span style="display:inline-block;transform:rotate(15deg);
                           background:#cef;width:40px;height:20px;
                           line-height:20px;text-align:center">R</span>
            after</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- dispatch_inline_box_content: overflow:hidden clip on inline-block ---

    #[test]
    fn render_smoke_inline_block_with_overflow_hidden_dispatches_under_clip() {
        // An inline-block with `overflow:hidden` containing a wider child exercises
        // `dispatch_inline_box_content`'s clip branch (`block.style.has_overflow_clip()`).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p>Text
              <span style="display:inline-block;overflow:hidden;
                           width:40px;height:20px;background:#fce">
                <span style="display:inline-block;width:120px;height:20px;
                             background:#e9f">wide child</span>
              </span>
            more text</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- dispatch_inline_box_content: opacity scope on inline-block ---

    #[test]
    fn render_smoke_inline_block_with_opacity_and_svg_child_dispatches_under_opacity() {
        // An inline-block with fractional `opacity` and an SVG child triggers
        // `opacity_descendants` population during convert, then
        // `dispatch_inline_box_content` routes through `draw_under_opacity`
        // (the third branch, `!block.opacity_descendants.is_empty()`).
        let pdf = render_html(
            r##"<!doctype html><html><body>
            <p>Text
              <span style="display:inline-block;opacity:0.5;width:24px;height:24px">
                <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24">
                  <circle cx="12" cy="12" r="10" fill="#3af"/>
                </svg>
              </span>
            more</p>
            </body></html>"##,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- dest_registry: body_y_off = 0.0 branch on page 2+ ---

    #[test]
    fn render_smoke_id_anchor_on_second_page_sets_destination() {
        // A node with `id` whose first fragment is on page 2+ exercises the
        // `body_y_off = 0.0` branch in `render_v2`'s dest-registry loop
        // (`page_idx != 0` → else 0.0).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="height:1200pt">spacer to push to page 2</div>
            <p id="page2-target">This paragraph is on page 2.</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_v2_page: multicol rule skipped for transformed descendant ---

    #[test]
    fn render_smoke_multicol_rule_inside_transform_skips_outer_paint() {
        // A multicol container nested inside a `transform` element ends up in
        // `transformed_descendants`. The column-rule loop in `draw_v2_page` must
        // skip it (the `transformed_descendants.contains(&container_id)` guard at
        // the top of the loop) and instead paint the rule from inside
        // `draw_under_transform`.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="transform:rotate(1deg)">
              <div style="column-count:2;column-rule:2px solid #369;
                          column-gap:20px;width:400px;font-size:12px">
                <p>Column A text here for rule test.</p>
                <p>Column B text here for rule test.</p>
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- paint_multicol_paragraph_slices from dispatch_fragment (Case A) ---

    #[test]
    fn render_smoke_multicol_self_inline_root_bare_text() {
        // Exercises paint_multicol_paragraph_slices called from within
        // dispatch_fragment when the multicol container is itself the inline
        // root (Case A: bare text directly in the container, no <p> wrapper).
        // In this case the container has both a block_styles entry and a
        // paragraph_slices entry; dispatch_fragment must paint the block bg
        // first and then call paint_multicol_paragraph_slices for the lines.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="column-count:2;column-gap:10px;width:400px;font-size:14px">
              alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu
              nu xi omicron pi rho sigma tau upsilon phi chi psi omega
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_clip: transform / table+clip / opacity branches ---

    #[test]
    fn render_smoke_transform_inside_overflow_clip() {
        // Exercises draw_under_clip's descendant loop: when a transformed
        // child lives inside an overflow:hidden container, draw_under_clip must
        // call draw_under_transform for that descendant rather than
        // dispatch_fragment directly.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="width:120px;height:80px;overflow:hidden;background:#cef">
              <div style="transform:rotate(8deg);width:80px;height:40px;background:#fce">
                rotated inside clip
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_table_overflow_clip_inside_overflow_clip() {
        // Exercises draw_under_clip's table-with-overflow-clip branch: a table
        // with overflow:hidden nested inside an overflow:hidden block forces
        // draw_under_clip_table to be called from within draw_under_clip's
        // descendant loop so the table's own clip boundary is pushed correctly.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="width:200px;height:120px;overflow:hidden;background:#eef">
              <table style="width:300px;overflow:hidden;border:1px solid #aaa">
                <tr>
                  <td style="padding:4px;background:#cef">wide cell overflowing</td>
                  <td style="padding:4px;background:#fce">second cell</td>
                </tr>
              </table>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_transform: table+clip branch ---

    #[test]
    fn render_smoke_table_overflow_clip_inside_transform() {
        // Exercises draw_under_transform's table-with-overflow-clip branch: a
        // table with overflow:hidden nested inside a transformed element forces
        // draw_under_clip_table to be called from within draw_under_transform's
        // descendant loop so the table's clip boundary is pushed under the
        // transform.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="transform:translate(10px,5px)">
              <table style="width:200px;overflow:hidden;border:1px solid #aaa">
                <tr>
                  <td style="padding:6px;background:#cef">cell inside transform</td>
                </tr>
              </table>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_opacity: transform / clip / table+clip / nested-opacity branches ---

    #[test]
    fn render_smoke_transform_inside_opacity_scope() {
        // Exercises draw_under_opacity's transform-descendant branch: a
        // transformed child nested inside an opacity-scoped block forces
        // draw_under_transform to be called from within draw_under_opacity's
        // descendant loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="opacity:0.7">
              <div style="transform:rotate(10deg);width:80px;height:40px;background:#fce">
                rotated inside opacity
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_overflow_clip_inside_opacity_scope() {
        // Exercises draw_under_opacity's overflow-clip descendant branch: an
        // overflow:hidden child nested inside an opacity-scoped block forces
        // draw_under_clip to be called from within draw_under_opacity's
        // descendant loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="opacity:0.7">
              <div style="overflow:hidden;width:100px;height:60px;background:#cef">
                <div style="width:200px;height:20px;background:#f99">clipped</div>
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_table_overflow_clip_inside_opacity_scope() {
        // Exercises draw_under_opacity's table-with-overflow-clip descendant
        // branch: a table with overflow:hidden nested inside an opacity-scoped
        // block forces draw_under_clip_table to be called from within
        // draw_under_opacity's descendant loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="opacity:0.65">
              <table style="width:200px;overflow:hidden;border:1px solid #aaa">
                <tr>
                  <td style="padding:4px;background:#cef">opacity+table+clip</td>
                </tr>
              </table>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_nested_opacity_inside_opacity_scope() {
        // Exercises draw_under_opacity's nested-opacity descendant branch: an
        // opacity-scoped child nested inside another opacity-scoped block forces
        // a recursive draw_under_opacity call from within the outer
        // draw_under_opacity's descendant loop.
        let pdf = render_html(
            r##"<!doctype html><html><body>
            <div style="opacity:0.8">
              <div style="opacity:0.5">
                <div style="background:#c9f;width:60px;height:40px">double-faded</div>
              </div>
            </div>
            </body></html>"##,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_clip: nested opacity branch (lines 1971-1988) ---

    #[test]
    fn render_smoke_opacity_inside_overflow_clip() {
        // draw_under_clip (lines 1971-1988): when iterating clip_descendants,
        // a descendant that is an opacity-scoped block triggers draw_under_opacity
        // from within draw_under_clip's descendant loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <div style="overflow:hidden;width:200px;height:100px">
              <div style="opacity:0.5">
                <div style="width:150px;height:60px;background:#cef">inner child</div>
              </div>
            </div>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- draw_under_clip_table: transform / clip-block / nested-table / opacity branches ---

    #[test]
    fn render_smoke_transform_inside_clip_table() {
        // draw_under_clip_table (lines 2868-2881, 2856-2857): a table with
        // overflow:hidden whose cell contains a transformed element triggers
        // draw_under_transform from within draw_under_clip_table's
        // clip_descendants loop. The transformed element's own children are
        // added to transform_skip and hit the continue at line 2856.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <table style="overflow:hidden;border:1px solid #aaa;width:200px">
              <tr>
                <td style="padding:4px">
                  <div style="transform:rotate(5deg);background:#cef;width:80px;height:40px">
                    rotated text inside table clip
                  </div>
                </td>
              </tr>
            </table>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_overflow_clip_block_inside_clip_table() {
        // draw_under_clip_table (lines 2887-2901): a table with overflow:hidden
        // containing a block with overflow:hidden triggers draw_under_clip from
        // within draw_under_clip_table's clip_descendants loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <table style="overflow:hidden;border:1px solid #aaa;width:200px">
              <tr>
                <td style="padding:4px">
                  <div style="overflow:hidden;width:120px;height:50px;background:#cef">
                    <div style="width:300px;height:30px;background:#f99">overflowing child</div>
                  </div>
                </td>
              </tr>
            </table>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_table_inside_clip_table() {
        // draw_under_clip_table (lines 2906-2919): a table with overflow:hidden
        // that contains another table with overflow:hidden triggers a recursive
        // draw_under_clip_table call from within the outer
        // draw_under_clip_table's clip_descendants loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <table style="overflow:hidden;border:1px solid #aaa;width:220px">
              <tr>
                <td style="padding:4px">
                  <table style="overflow:hidden;border:1px solid #c99;width:180px">
                    <tr><td style="padding:4px;background:#cef">nested table cell</td></tr>
                  </table>
                </td>
              </tr>
            </table>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_opacity_inside_clip_table() {
        // draw_under_clip_table (lines 2924-2938): a table with overflow:hidden
        // containing an opacity-scoped block triggers draw_under_opacity from
        // within draw_under_clip_table's clip_descendants loop.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <table style="overflow:hidden;border:1px solid #aaa;width:200px">
              <tr>
                <td style="padding:4px">
                  <div style="opacity:0.5">
                    <div style="background:#cef;width:80px;height:40px">faded cell content</div>
                  </div>
                </td>
              </tr>
            </table>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- MarginBoxRenderer::render_page: @page :first / :left / :right selectors ---

    #[test]
    fn render_smoke_page_first_selector_margin_box() {
        // MarginBoxRenderer::render_page (lines 3565-3578): the @page :first
        // pseudo-class selector must match on page 1 and override the generic
        // @page rule's margin-box content. On subsequent pages :first does not
        // match (line 3573 `continue`). The `should_replace` logic (line 3578)
        // is also exercised when the :first rule supersedes the generic rule.
        let pdf = render_html(
            r#"<!doctype html><html><head><style>
            @page { size: A4; margin: 20mm; @top-center { content: "All pages"; } }
            @page :first { @top-center { content: "First page only"; } }
            </style></head><body>
            <p>First page content.</p>
            <div style="height:900px"></div>
            <p>Second page content.</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn render_smoke_page_left_right_selectors() {
        // MarginBoxRenderer::render_page (lines 3565-3569, 3573): :left and
        // :right pseudo-class selectors are evaluated per page_num parity.
        // On a 3-page document both branches fire and pages where a selector
        // does not match hit the `continue` at line 3573.
        let pdf = render_html(
            r#"<!doctype html><html><head><style>
            @page { size: A4; margin: 20mm; }
            @page :left  { @top-center { content: "Left page";  } }
            @page :right { @top-center { content: "Right page"; } }
            </style></head><body>
            <p>Page one.</p>
            <div style="height:900px"></div>
            <p>Page two.</p>
            <div style="height:900px"></div>
            <p>Page three.</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // --- MarginBoxRenderer stage 1b + stage 2: @left-middle / @right-middle boxes ---

    #[test]
    fn render_smoke_page_left_right_margin_boxes() {
        // MarginBoxRenderer::render_page stage 1b (lines 3659-3688): left/right
        // margin boxes require a separate height-measurement pass. Stage 2 size
        // lookup for vertical edges (lines 3708-3719) uses those heights to
        // place the boxes. Stage 1a's `continue` for non-horizontal edges
        // (line 3624) is also hit.
        let pdf = render_html(
            r#"<!doctype html><html><head><style>
            @page {
              size: A4; margin: 20mm 40mm;
              @left-middle  { content: "Left margin";  }
              @right-middle { content: "Right margin"; }
            }
            </style></head><body>
            <p>Content with left and right margin boxes.</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }
}
