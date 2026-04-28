use crate::config::Config;
use crate::error::{Error, Result};
use crate::gcpm::GcpmContext;
use crate::gcpm::counter::resolve_content_to_html;
use crate::gcpm::margin_box::{Edge, MarginBoxPosition, MarginBoxRect, compute_edge_layout};
use crate::gcpm::running::RunningElementStore;
use crate::pageable::{Canvas, Pageable};
use crate::paginate::paginate;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Render a Pageable tree to PDF bytes.
///
/// Public entry that does not run the fulgur-cj6u Phase 1.2
/// page-count parity assertion: the caller built the Pageable
/// directly without going through `Engine::render_html`, so no
/// `pagination_layout::PaginationGeometryTable` is available. The
/// parity gate lives in [`render_to_pdf_with_gcpm`] which is the
/// engine-internal entry threaded with the spike's geometry.
pub fn render_to_pdf(root: Box<dyn Pageable>, config: &Config) -> Result<Vec<u8>> {
    let content_width = config.content_width();
    let content_height = config.content_height();

    // Paginate
    let pages = paginate(root, content_width, content_height);

    // Create PDF document
    let mut document = krilla::Document::new();

    let page_size = if config.landscape {
        config.page_size.landscape()
    } else {
        config.page_size
    };

    let mut collector = if config.bookmarks {
        Some(crate::pageable::BookmarkCollector::new())
    } else {
        None
    };

    // Pre-pass: collect block-level anchor destinations so `href="#id"`
    // links can resolve to `(page_idx, y)` during annotation emission.
    let mut dest_registry = crate::pageable::DestinationRegistry::new();
    for (idx, p) in pages.iter().enumerate() {
        dest_registry.set_current_page(idx);
        p.collect_ids(
            config.margin.left,
            config.margin.top,
            content_width,
            content_height,
            &mut dest_registry,
        );
    }

    let mut link_collector = crate::pageable::LinkCollector::new();

    for (page_idx, page_content) in pages.iter().enumerate() {
        let settings = krilla::page::PageSettings::from_wh(page_size.width, page_size.height)
            .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;

        let mut page = document.start_page_with(settings);

        if let Some(c) = collector.as_mut() {
            c.set_current_page(page_idx);
        }
        link_collector.set_current_page(page_idx);

        // Scope the surface borrow so we can mutate `page` (add_annotation)
        // afterwards: `page.surface()` returns a `Surface<'_>` that exclusively
        // borrows `page` until dropped.
        {
            let mut surface = page.surface();
            let mut canvas = Canvas {
                surface: &mut surface,
                bookmark_collector: collector.as_mut(),
                link_collector: Some(&mut link_collector),
            };
            page_content.draw(
                &mut canvas,
                config.margin.left,
                config.margin.top,
                content_width,
                content_height,
            );
            // Surface drops here, releasing the borrow on `page`.
        }

        // Emit link annotations for this page now that `page` is exclusively
        // ours again. `take_page` drains just this page's occurrences in
        // O(L_page) instead of scanning the entire occurrence list.
        let per_page = link_collector.take_page(page_idx);
        crate::link::emit_link_annotations(&mut page, &per_page, &dest_registry);
    }

    if let Some(c) = collector {
        let entries = c.into_entries();
        if !entries.is_empty() {
            document.set_outline(crate::outline::build_outline(&entries));
        }
    }

    document.set_metadata(build_metadata(config));

    let pdf_bytes = document
        .finish()
        .map_err(|e| Error::PdfGeneration(format!("{e:?}")))?;
    Ok(pdf_bytes)
}

/// fulgur-cj6u Phase 1.2: in debug builds, assert that
/// `pagination_layout::implied_page_count(geometry)` matches the
/// Pageable-driven page count from `paginate(...)`. Drift between
/// the two is the regression signal Phase 2 work needs to chase
/// (widow/orphan, running element / margin-box, counter, table-row
/// break, …).
///
/// Skipped when `geometry` is empty — the spike pass was either not
/// run or the document had no body / no in-flow children, in which
/// case Pageable's "always at least one page" convention diverges
/// from the spike's "no fragments" output and the assertion is
/// uninformative. Empty bodies still go through Pageable's single-
/// empty-page fallback.
///
/// Release builds compile this to a no-op via `cfg!(debug_assertions)`.
fn assert_pageable_spike_parity(
    pages: &[Box<dyn Pageable>],
    geometry: &crate::pagination_layout::PaginationGeometryTable,
) {
    if !cfg!(debug_assertions) || geometry.is_empty() {
        return;
    }
    let pageable_count = pages.len() as u32;
    let spike_count = crate::pagination_layout::implied_page_count(geometry);
    // fulgur-s67g Phase 2.6: when Pageable splits an oversized element
    // across pages (mid-element split, e.g. `.huge { break-inside: avoid }`
    // taller than `@page`), Pageable emits more pages than the spike's
    // strip-based fragmenter currently models. That gap is Phase 3 work
    // (per-strip layout pass / `fulgur-g9e3`); until then, skip parity
    // when Pageable > spike. The reverse direction is still a regression.
    if pageable_count > spike_count {
        return;
    }
    debug_assert_eq!(
        pageable_count, spike_count,
        "page count parity drift: paginate={pageable_count} spike={spike_count}",
    );
}

/// Detect mid-element split: Pageable produces more pages than spike's
/// `implied_page_count`. See `assert_pageable_spike_parity` for the
/// rationale — this is Phase 3 territory and the dependent parity
/// assertions can't be meaningfully compared either.
fn mid_element_split_skipped(
    pageable_pages: usize,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
) -> bool {
    let spike_count = crate::pagination_layout::implied_page_count(geometry) as usize;
    pageable_pages > spike_count
}

/// fulgur-cj6u Phase 1.3: in debug builds, assert that the spike's
/// `pagination_layout::collect_string_set_states` produces the same
/// per-page `(start, first, last)` shape Pageable's tree walk does.
/// Same skip semantics as `assert_pageable_spike_parity`: empty
/// geometry → spike pass not run; release builds compile to a no-op.
///
/// `string_set_by_node` is the engine-side `HashMap`; the spike
/// wants a `BTreeMap` (deterministic iteration), so we materialise
/// one once for the comparison. The conversion is debug-only via
/// `cfg!(debug_assertions)`.
fn assert_string_set_states_parity(
    pageable_states: &[BTreeMap<String, crate::paginate::StringSetPageState>],
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    string_set_by_node: &HashMap<usize, Vec<(String, String)>>,
) {
    if !cfg!(debug_assertions) || geometry.is_empty() {
        return;
    }
    if mid_element_split_skipped(pageable_states.len(), geometry) {
        return;
    }
    let by_node_btree: BTreeMap<usize, Vec<(String, String)>> = string_set_by_node
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let spike_states =
        crate::pagination_layout::collect_string_set_states(geometry, &by_node_btree);
    debug_assert_eq!(
        pageable_states.len(),
        spike_states.len(),
        "string-set state vec length drift: pageable={} spike={}",
        pageable_states.len(),
        spike_states.len(),
    );
    for (idx, (pg, sp)) in pageable_states.iter().zip(spike_states.iter()).enumerate() {
        debug_assert_eq!(
            pg, sp,
            "string-set state drift on page {idx}:\n  pageable = {pg:#?}\n  spike    = {sp:#?}",
        );
    }
}

/// fulgur-s67g Phase 2.3: in debug builds, assert that the spike's
/// `pagination_layout::collect_counter_states` produces the same
/// per-page counter snapshot Pageable's tree walk does. Same skip
/// semantics as the other parity helpers: empty geometry → spike pass
/// not run; release builds compile to a no-op.
fn assert_counter_states_parity(
    pageable_states: &[BTreeMap<String, i32>],
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    counter_ops_by_node: &BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
) {
    if !cfg!(debug_assertions) || geometry.is_empty() {
        return;
    }
    // `fragment_pagination_root` skips zero-height body children
    // (e.g. `<div class="reset"></div>` carrying `counter-set: ..`),
    // so a counter-op node can be absent from `geometry` while
    // Pageable still applies its op during the tree walk. Skip the
    // assertion in that case — the spike's view is intentionally
    // incomplete here, same scope limitation as the function's
    // docstring already calls out for nested declarations.
    if counter_ops_by_node
        .keys()
        .any(|id| !geometry.contains_key(id))
    {
        return;
    }
    if mid_element_split_skipped(pageable_states.len(), geometry) {
        return;
    }
    let spike_states =
        crate::pagination_layout::collect_counter_states(geometry, counter_ops_by_node);
    debug_assert_eq!(
        pageable_states.len(),
        spike_states.len(),
        "counter state vec length drift: pageable={} spike={}",
        pageable_states.len(),
        spike_states.len(),
    );
    for (idx, (pg, sp)) in pageable_states.iter().zip(spike_states.iter()).enumerate() {
        debug_assert_eq!(
            pg, sp,
            "counter state drift on page {idx}:\n  pageable = {pg:#?}\n  spike    = {sp:#?}",
        );
    }
}

/// fulgur-s67g Phase 2.4: in debug builds, assert that the spike's
/// `pagination_layout::collect_bookmark_entries` produces the same
/// `(page_idx, level, label)` triples Pageable's collector emits at
/// draw time. `y_pt` is intentionally not compared — see the
/// docstring on `collect_bookmark_entries` for the rationale.
fn assert_bookmark_entries_parity(
    pageable_entries: &[crate::pageable::BookmarkEntry],
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    bookmark_by_node: &BTreeMap<usize, crate::blitz_adapter::BookmarkInfo>,
    total_pages: usize,
) {
    if !cfg!(debug_assertions) || geometry.is_empty() {
        return;
    }
    if mid_element_split_skipped(total_pages, geometry) {
        return;
    }
    let pageable_triples: Vec<crate::pagination_layout::BookmarkPageEntry> = pageable_entries
        .iter()
        .map(|e| crate::pagination_layout::BookmarkPageEntry {
            page_idx: e.page_idx,
            level: e.level,
            label: e.label.clone(),
        })
        .collect();
    let mut spike_triples =
        crate::pagination_layout::collect_bookmark_entries(geometry, bookmark_by_node);
    // Sort both by (page_idx, label) so iteration order from BTreeMap
    // (NodeId-ordered) matches Pageable's draw-order recording when
    // the source order happens to disagree on tie-breaks.
    let mut sorted_pageable = pageable_triples.clone();
    sorted_pageable.sort_by(|a, b| {
        a.page_idx
            .cmp(&b.page_idx)
            .then(a.level.cmp(&b.level))
            .then(a.label.cmp(&b.label))
    });
    spike_triples.sort_by(|a, b| {
        a.page_idx
            .cmp(&b.page_idx)
            .then(a.level.cmp(&b.level))
            .then(a.label.cmp(&b.label))
    });
    debug_assert_eq!(
        sorted_pageable, spike_triples,
        "bookmark entries drift:\n  pageable = {sorted_pageable:#?}\n  spike    = {spike_triples:#?}",
    );
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
type MeasureCache = HashMap<(String, u32), f32>;
type RenderCache = HashMap<(String, u32, u32), Box<dyn Pageable>>;

fn width_key(w: f32) -> u32 {
    w.to_bits()
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

/// Render a Pageable tree to PDF bytes with GCPM margin box support.
///
/// Uses a 2-pass approach:
/// - Pass 1: paginate the body content to determine page count
/// - Pass 2: render each page, resolving margin box content (counters, running elements)
///   and laying them out via Blitz before drawing
#[allow(clippy::too_many_arguments)]
pub fn render_to_pdf_with_gcpm(
    root: Box<dyn Pageable>,
    config: &Config,
    gcpm: &GcpmContext,
    running_store: &RunningElementStore,
    font_data: &[Arc<Vec<u8>>],
    pagination_geometry: &crate::pagination_layout::PaginationGeometryTable,
    string_set_by_node: &HashMap<usize, Vec<(String, String)>>,
    counter_ops_by_node: &BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
    bookmark_by_node: &BTreeMap<usize, crate::blitz_adapter::BookmarkInfo>,
) -> Result<Vec<u8>> {
    // Resolve the default (no-selector) CSS @page margin for initial pagination.
    // :first/:left/:right overrides are applied per-page during rendering below.
    let default_page_rules: Vec<_> = gcpm
        .page_settings
        .iter()
        .filter(|r| r.page_selector.is_none())
        .cloned()
        .collect();
    let (init_size, init_margin, init_landscape) =
        crate::gcpm::page_settings::resolve_page_settings(&default_page_rules, 1, 0, config);
    let init_size = if init_landscape {
        init_size.landscape()
    } else {
        init_size
    };
    let content_width = init_size.width - init_margin.left - init_margin.right;
    let content_height = init_size.height - init_margin.top - init_margin.bottom;

    // Pass 1: paginate body content
    let pages = paginate(root, content_width, content_height);
    // fulgur-cj6u Phase 1.2 / fulgur-s67g Phase 2.6: cross-check the
    // spike fragmenter agrees with Pageable on the page count.
    // Phase 2.6 (`@page` size / margin resolution) makes the engine
    // pre-resolve page-1 settings before driving the spike, so the
    // strip height the spike sees matches `content_height` here by
    // construction — no skip needed for `@page`-modified docs.
    // (Phase 2.2 already ungated the running-elements skip.)
    assert_pageable_spike_parity(&pages, pagination_geometry);
    let total_pages = pages.len();
    let string_set_states = if gcpm.string_set_mappings.is_empty() {
        vec![BTreeMap::new(); pages.len()]
    } else {
        crate::paginate::collect_string_set_states(&pages)
    };
    // fulgur-cj6u Phase 1.3: parity-check the spike's geometry-table-
    // driven `collect_string_set_states` against Pageable's tree walk.
    // No activation gate beyond "string-set actually used" — Phase 2.2
    // / 2.6 ungated the running-elements and `@page` skips.
    if !gcpm.string_set_mappings.is_empty() {
        assert_string_set_states_parity(
            &string_set_states,
            pagination_geometry,
            string_set_by_node,
        );
    }
    let running_states = if gcpm.running_mappings.is_empty() {
        vec![BTreeMap::new(); pages.len()]
    } else {
        crate::paginate::collect_running_element_states(&pages)
    };
    let counter_states =
        if gcpm.counter_mappings.is_empty() && gcpm.content_counter_mappings.is_empty() {
            vec![BTreeMap::new(); pages.len()]
        } else {
            crate::paginate::collect_counter_states(&pages)
        };
    // fulgur-s67g Phase 2.3: parity-check the spike's geometry-table-
    // driven `collect_counter_states` against Pageable's tree walk.
    // No activation gate beyond "counter actually used".
    if !gcpm.counter_mappings.is_empty() || !gcpm.content_counter_mappings.is_empty() {
        assert_counter_states_parity(&counter_states, pagination_geometry, counter_ops_by_node);
    }

    // Build margin-box CSS: strip display:none rules that the parser
    // injected for running elements (they need to be visible in margin boxes).
    let margin_css = strip_display_none(&gcpm.cleaned_css);

    // Caches: measure (html → max-content width), height ((html, layout_width) → max-content height),
    // render (html+width → Pageable)
    let mut measure_cache: MeasureCache = HashMap::new();
    let mut height_cache: HashMap<(String, u32), f32> = HashMap::new();
    let mut render_cache: RenderCache = HashMap::new();

    let mut document = krilla::Document::new();

    let mut collector = if config.bookmarks {
        Some(crate::pageable::BookmarkCollector::new())
    } else {
        None
    };

    // Pre-pass: collect block-level anchor destinations. Under GCPM,
    // `@page :first` / `@page :left` / etc. can override the size or
    // margins of individual pages, so we must replay the same
    // `resolve_page_settings` logic the render loop uses below — using the
    // global `config.margin` here would produce stale destination
    // coordinates on pages whose size or margins differ from the default.
    let mut dest_registry = crate::pageable::DestinationRegistry::new();
    for (idx, p) in pages.iter().enumerate() {
        let page_num = idx + 1;
        let (resolved_size, resolved_margin, resolved_landscape) =
            crate::gcpm::page_settings::resolve_page_settings(
                &gcpm.page_settings,
                page_num,
                total_pages,
                config,
            );
        let page_size = if resolved_landscape {
            resolved_size.landscape()
        } else {
            resolved_size
        };
        let page_content_width = page_size.width - resolved_margin.left - resolved_margin.right;
        let page_content_height = page_size.height - resolved_margin.top - resolved_margin.bottom;

        dest_registry.set_current_page(idx);
        p.collect_ids(
            resolved_margin.left,
            resolved_margin.top,
            page_content_width,
            page_content_height,
            &mut dest_registry,
        );
    }

    let mut link_collector = crate::pageable::LinkCollector::new();

    // Pass 2: render each page with margin boxes
    for (page_idx, page_content) in pages.iter().enumerate() {
        let page_num = page_idx + 1;

        // Resolve per-page size, margin, and landscape from @page rules + CLI overrides
        let (resolved_size, resolved_margin, resolved_landscape) =
            crate::gcpm::page_settings::resolve_page_settings(
                &gcpm.page_settings,
                page_num,
                total_pages,
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

        if let Some(c) = collector.as_mut() {
            c.set_current_page(page_idx);
        }
        link_collector.set_current_page(page_idx);

        let mut surface = page.surface();

        // Margin boxes use a Canvas with no bookmark collector — running
        // elements promoted into margin boxes may contain h1-h6, but their
        // bookmark entry must come from the source position in the body,
        // not from each margin-box repetition. The body Canvas (created
        // after this scope) carries the collector instead.
        //
        // Margin-box links are out of scope for this task — only the body
        // canvas wires the link collector below. Clickable `<a>` inside
        // header/footer content is a follow-up.
        let mut canvas = Canvas {
            surface: &mut surface,
            bookmark_collector: None,
            link_collector: None,
        };

        // Resolve margin boxes: for each position, pick the most specific
        // matching rule. Pseudo-class selectors (:first, :left, :right) override
        // the default @page rule for the same position.
        let mut effective_boxes: BTreeMap<MarginBoxPosition, &crate::gcpm::MarginBoxRule> =
            BTreeMap::new();
        for margin_box in &gcpm.margin_boxes {
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
            // More specific selector (Some) overrides less specific (None)
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

        // Collect resolved HTML for each effective box, wrapping in a div
        // with the margin box's own declarations (font-size, color, margin, etc.)
        let mut resolved_htmls: BTreeMap<MarginBoxPosition, String> = BTreeMap::new();
        for (&pos, rule) in &effective_boxes {
            let content_html = resolve_content_to_html(
                &rule.content,
                running_store,
                &running_states,
                &string_set_states[page_idx],
                page_num,
                total_pages,
                page_idx,
                &counter_states[page_idx],
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

        // Stage 1a: Measure max-content width for top/bottom boxes.
        // Uses inline-block wrapper so Blitz computes shrink-to-fit width.
        for (&pos, html) in &resolved_htmls {
            if !pos.edge().is_some_and(|e| e.is_horizontal()) {
                continue;
            }
            let measure_key = (html.clone(), width_key(page_size.height));
            measure_cache.entry(measure_key).or_insert_with(|| {
                let measure_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\"><div style=\"display:inline-block\">{}</div></body></html>",
                    margin_css, html
                );
                let measure_doc = crate::blitz_adapter::parse_and_layout(
                    &measure_html,
                    crate::convert::pt_to_px(content_width),
                    crate::convert::pt_to_px(page_size.height),
                    font_data,
                );
                get_body_child_dimension(&measure_doc, true)
            });
        }

        // Stage 1b: Measure max-content height for left/right boxes.
        // Layout at fixed margin width, then read the resulting height.
        for (&pos, html) in &resolved_htmls {
            let fixed_width = match pos.edge() {
                Some(Edge::Left) => resolved_margin.left,
                Some(Edge::Right) => resolved_margin.right,
                _ => continue,
            };
            let hc_key = (html.clone(), width_key(fixed_width));
            height_cache.entry(hc_key).or_insert_with(|| {
                let measure_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\"><div>{}</div></body></html>",
                    margin_css, html
                );
                let measure_doc = crate::blitz_adapter::parse_and_layout(
                    &measure_html,
                    crate::convert::pt_to_px(fixed_width),
                    crate::convert::pt_to_px(page_size.height),
                    font_data,
                );
                get_body_child_dimension(&measure_doc, false)
            });
        }

        // Stage 2: Group by edge and compute layout
        let mut edge_defined: BTreeMap<Edge, BTreeMap<MarginBoxPosition, f32>> = BTreeMap::new();

        for (&pos, html) in &resolved_htmls {
            let edge = match pos.edge() {
                Some(e) => e,
                None => continue, // corners
            };
            let size = if edge.is_horizontal() {
                measure_cache
                    .get(&(html.clone(), width_key(page_size.height)))
                    .copied()
            } else {
                let fixed_width = if edge == Edge::Left {
                    resolved_margin.left
                } else {
                    resolved_margin.right
                };
                height_cache
                    .get(&(html.clone(), width_key(fixed_width)))
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

        // Stage 3: Render at confirmed width and draw.
        // Pageable is created (or fetched from cache) at the final rect width.
        for (&pos, html) in &resolved_htmls {
            let rect = all_rects
                .get(&pos)
                .copied()
                .unwrap_or_else(|| pos.bounding_rect(page_size, resolved_margin));

            let cache_key = (html.clone(), width_key(rect.width), width_key(rect.height));
            if !render_cache.contains_key(&cache_key) {
                let render_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\">{}</body></html>",
                    margin_css, html
                );
                let render_doc = crate::blitz_adapter::parse_and_layout(
                    &render_html,
                    crate::convert::pt_to_px(rect.width),
                    crate::convert::pt_to_px(rect.height),
                    font_data,
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
                render_cache.insert(cache_key.clone(), pageable);
            }

            if let Some(pageable) = render_cache.get(&cache_key) {
                pageable.draw(&mut canvas, rect.x, rect.y, rect.width, rect.height);
            }
        }

        // Draw body content with resolved per-page margin. Reuse `canvas`
        // by overwriting it so the previous (collector-less) Canvas's
        // borrow on `surface` is released, then reborrow with the bookmark
        // and link collectors so bookmark markers can record their
        // (page_idx, y) for the PDF outline and `<a>` rects for link
        // annotations.
        canvas = Canvas {
            surface: &mut surface,
            bookmark_collector: collector.as_mut(),
            link_collector: Some(&mut link_collector),
        };
        let page_content_width = page_size.width - resolved_margin.left - resolved_margin.right;
        let page_content_height = page_size.height - resolved_margin.top - resolved_margin.bottom;
        page_content.draw(
            &mut canvas,
            resolved_margin.left,
            resolved_margin.top,
            page_content_width,
            page_content_height,
        );
        // Release the surface borrow before mutating `page` to add
        // annotations. `Surface` has a `Drop` impl that flushes the content
        // stream and releases its borrow on `page`. `Canvas` is a no-drop
        // wrapper around `&mut surface`, so dropping `surface` is enough.
        drop(surface);

        let per_page = link_collector.take_page(page_idx);
        crate::link::emit_link_annotations(&mut page, &per_page, &dest_registry);
    }

    if let Some(c) = collector {
        let entries = c.into_entries();
        // fulgur-s67g Phase 2.4: parity-check the spike's
        // geometry-driven bookmark walk against Pageable's draw-time
        // collector output. Compares only `(page_idx, level, label)`
        // — the spike does not work in PDF-pt frames so y_pt parity
        // is deferred to Phase 4 (convert / render rewrite).
        // Phase 2.6 ungated the `@page`-content-height skip.
        if !entries.is_empty() {
            assert_bookmark_entries_parity(
                &entries,
                pagination_geometry,
                bookmark_by_node,
                total_pages,
            );
        }
        if !entries.is_empty() {
            document.set_outline(crate::outline::build_outline(&entries));
        }
    }

    document.set_metadata(build_metadata(config));

    let pdf_bytes = document
        .finish()
        .map_err(|e| Error::PdfGeneration(format!("{e:?}")))?;
    Ok(pdf_bytes)
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
    use crate::config::Config;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn simple_root() -> Box<dyn Pageable> {
        use crate::pageable::{BlockPageable, SpacerPageable};
        Box::new(BlockPageable::new(vec![Box::new(SpacerPageable::new(
            100.0,
        ))]))
    }

    fn assert_pdf_header(pdf: &[u8]) {
        assert!(pdf.starts_with(b"%PDF"));
    }

    fn pdf_info_field(pdf: &[u8], key: &[u8]) -> Option<String> {
        use std::io::Cursor;
        let doc = lopdf::Document::load_from(Cursor::new(pdf)).ok()?;
        let info_id = doc.trailer.get(b"Info").ok()?.as_reference().ok()?;
        let info = match doc.get_object(info_id) {
            Ok(lopdf::Object::Dictionary(d)) => d.clone(),
            _ => return None,
        };
        let bytes = info.get(key).ok()?.as_str().ok()?;
        Some(
            if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
                let chars: Vec<u16> = bytes[2..]
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&chars).to_owned()
            } else {
                bytes.iter().map(|&b| b as char).collect()
            },
        )
    }

    fn pdf_page1_size(pdf: &[u8]) -> (f32, f32) {
        use std::io::Cursor;
        let doc = lopdf::Document::load_from(Cursor::new(pdf)).expect("valid PDF");
        let pages = doc.get_pages();
        let &page_id = pages.get(&1).expect("page 1 exists");
        let page_dict = match doc.get_object(page_id).expect("page object") {
            lopdf::Object::Dictionary(d) => d.clone(),
            _ => panic!("page is not a dictionary"),
        };
        let arr = page_dict
            .get(b"MediaBox")
            .expect("MediaBox")
            .as_array()
            .expect("MediaBox is array")
            .clone();
        let to_f32 = |o: &lopdf::Object| match o {
            lopdf::Object::Integer(i) => *i as f32,
            lopdf::Object::Real(f) => *f,
            _ => 0.0,
        };
        (
            to_f32(&arr[2]) - to_f32(&arr[0]),
            to_f32(&arr[3]) - to_f32(&arr[1]),
        )
    }

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

    // ── render_to_pdf ─────────────────────────────────────────────────────────

    #[test]
    fn render_to_pdf_produces_valid_pdf() {
        let pdf = render_to_pdf(simple_root(), &Config::default()).unwrap();
        assert_pdf_header(&pdf);
    }

    #[test]
    fn render_to_pdf_landscape_page() {
        let config = Config::builder().landscape(true).build();
        let pdf = render_to_pdf(simple_root(), &config).unwrap();
        assert_pdf_header(&pdf);
    }

    #[test]
    fn render_to_pdf_bookmarks_enabled() {
        let config = Config::builder().bookmarks(true).build();
        let pdf = render_to_pdf(simple_root(), &config).unwrap();
        assert_pdf_header(&pdf);
    }

    #[test]
    fn render_to_pdf_all_metadata_fields() {
        let config = Config::builder()
            .title("My Title")
            .author("Alice")
            .description("A description")
            .keywords(["rust", "pdf"])
            .lang("en-US")
            .creator("my-creator")
            .producer("my-producer")
            .creation_date("2024-06-15T10:30:45Z")
            .build();
        let pdf = render_to_pdf(simple_root(), &config).unwrap();
        assert_pdf_header(&pdf);
        assert_eq!(pdf_info_field(&pdf, b"Title").as_deref(), Some("My Title"));
        assert_eq!(pdf_info_field(&pdf, b"Author").as_deref(), Some("Alice"));
        assert_eq!(
            pdf_info_field(&pdf, b"Producer").as_deref(),
            Some("my-producer")
        );
        assert_eq!(
            pdf_info_field(&pdf, b"Creator").as_deref(),
            Some("my-creator")
        );
        assert!(pdf_info_field(&pdf, b"CreationDate").is_some());
    }

    #[test]
    fn render_to_pdf_creation_date_parse_failure_is_ignored() {
        let config = Config::builder().creation_date("not-a-date").build();
        let pdf = render_to_pdf(simple_root(), &config).unwrap();
        assert_pdf_header(&pdf);
    }

    // ── render_to_pdf_with_gcpm ───────────────────────────────────────────────

    #[test]
    fn render_to_pdf_with_gcpm_empty_context() {
        use crate::gcpm::GcpmContext;
        use crate::gcpm::running::RunningElementStore;
        let pdf = render_to_pdf_with_gcpm(
            simple_root(),
            &Config::default(),
            &GcpmContext::default(),
            &RunningElementStore::new(),
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_pdf_header(&pdf);
    }

    #[test]
    fn render_to_pdf_with_gcpm_landscape() {
        use crate::gcpm::GcpmContext;
        use crate::gcpm::running::RunningElementStore;
        let config = Config::builder().landscape(true).build();
        let pdf = render_to_pdf_with_gcpm(
            simple_root(),
            &config,
            &GcpmContext::default(),
            &RunningElementStore::new(),
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_pdf_header(&pdf);
    }

    #[test]
    fn render_to_pdf_with_gcpm_bookmarks_enabled() {
        use crate::gcpm::GcpmContext;
        use crate::gcpm::running::RunningElementStore;
        let config = Config::builder().bookmarks(true).build();
        let pdf = render_to_pdf_with_gcpm(
            simple_root(),
            &config,
            &GcpmContext::default(),
            &RunningElementStore::new(),
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_pdf_header(&pdf);
    }

    #[test]
    fn render_to_pdf_with_gcpm_page_settings_keyword() {
        use crate::gcpm::running::RunningElementStore;
        use crate::gcpm::{GcpmContext, PageSettingsRule, PageSizeDecl};
        let gcpm = GcpmContext {
            page_settings: vec![PageSettingsRule {
                page_selector: None,
                size: Some(PageSizeDecl::Keyword("A4".into())),
                margin: None,
            }],
            ..GcpmContext::default()
        };
        let pdf = render_to_pdf_with_gcpm(
            simple_root(),
            &Config::default(),
            &gcpm,
            &RunningElementStore::new(),
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_pdf_header(&pdf);
        let (w, h) = pdf_page1_size(&pdf);
        assert!(
            (w - 595.28).abs() < 1.0,
            "expected A4 width ≈ 595.28 pt, got {w}"
        );
        assert!(
            (h - 841.89).abs() < 1.0,
            "expected A4 height ≈ 841.89 pt, got {h}"
        );
    }

    #[test]
    fn render_to_pdf_with_gcpm_all_metadata_fields() {
        use crate::gcpm::GcpmContext;
        use crate::gcpm::running::RunningElementStore;
        let config = Config::builder()
            .title("GCPM Title")
            .author("Author")
            .description("description")
            .keywords(["kw1"])
            .lang("fr")
            .creator("creator")
            .producer("producer")
            .creation_date("2024-01-01")
            .build();
        let pdf = render_to_pdf_with_gcpm(
            simple_root(),
            &config,
            &GcpmContext::default(),
            &RunningElementStore::new(),
            &[],
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
        assert_pdf_header(&pdf);
        assert_eq!(
            pdf_info_field(&pdf, b"Title").as_deref(),
            Some("GCPM Title")
        );
        assert_eq!(pdf_info_field(&pdf, b"Author").as_deref(), Some("Author"));
        assert_eq!(
            pdf_info_field(&pdf, b"Producer").as_deref(),
            Some("producer")
        );
        assert_eq!(pdf_info_field(&pdf, b"Creator").as_deref(), Some("creator"));
        assert!(pdf_info_field(&pdf, b"CreationDate").is_some());
    }
}
