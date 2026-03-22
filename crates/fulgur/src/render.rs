use crate::config::Config;
use crate::error::{Error, Result};
use crate::gcpm::GcpmContext;
use crate::gcpm::counter::resolve_content_to_html;
use crate::gcpm::margin_box::{Edge, MarginBoxPosition, compute_edge_layout};
use crate::gcpm::running::RunningElementStore;
use crate::pageable::{Canvas, Pageable};
use crate::paginate::paginate;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Render a Pageable tree to PDF bytes.
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

    for page_content in &pages {
        let settings = krilla::page::PageSettings::from_wh(page_size.width, page_size.height)
            .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;

        let mut page = document.start_page_with(settings);
        let mut surface = page.surface();

        // Pass margin offsets as x/y origin to draw
        let mut canvas = Canvas {
            surface: &mut surface,
        };
        page_content.draw(
            &mut canvas,
            config.margin.left,
            config.margin.top,
            content_width,
            content_height,
        );
        // Surface::finish is handled by Drop
    }

    document.set_metadata(build_metadata(config));

    let pdf_bytes = document
        .finish()
        .map_err(|e| Error::PdfGeneration(format!("{e:?}")))?;
    Ok(pdf_bytes)
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
/// Measure cache: html → max-content width (measured once at content_width).
/// Render cache: (html, final_width as bits) → Pageable (laid out at confirmed width).
type MeasureCache = HashMap<String, f32>;
type RenderCache = HashMap<(String, u32), Box<dyn Pageable>>;

fn width_key(w: f32) -> u32 {
    w.to_bits()
}

/// Get the width of the first non-zero-width child of `<body>` in a Blitz document.
/// This represents the max-content width of the margin box content.
fn get_body_child_width(doc: &blitz_html::HtmlDocument) -> f32 {
    use std::ops::Deref;
    let root = doc.root_element();
    let base_doc = doc.deref();
    // Walk: html → body → first child with size
    if let Some(root_node) = base_doc.get_node(root.id) {
        for &child_id in &root_node.children {
            if let Some(child) = base_doc.get_node(child_id) {
                if let blitz_dom::NodeData::Element(elem) = &child.data {
                    if elem.name.local.as_ref() == "body" {
                        // Get first child of body with non-zero width
                        for &body_child_id in &child.children {
                            if let Some(body_child) = base_doc.get_node(body_child_id) {
                                let w = body_child.final_layout.size.width;
                                if w > 0.0 {
                                    return w;
                                }
                            }
                        }
                        return child.final_layout.size.width;
                    }
                }
            }
        }
    }
    0.0
}

/// Render a Pageable tree to PDF bytes with GCPM margin box support.
///
/// Uses a 2-pass approach:
/// - Pass 1: paginate the body content to determine page count
/// - Pass 2: render each page, resolving margin box content (counters, running elements)
///   and laying them out via Blitz before drawing
pub fn render_to_pdf_with_gcpm(
    root: Box<dyn Pageable>,
    config: &Config,
    gcpm: &GcpmContext,
    running_store: &RunningElementStore,
    font_data: &[Arc<Vec<u8>>],
) -> Result<Vec<u8>> {
    let content_width = config.content_width();
    let content_height = config.content_height();

    // Pass 1: paginate body content
    let pages = paginate(root, content_width, content_height);
    let total_pages = pages.len();

    let page_size = if config.landscape {
        config.page_size.landscape()
    } else {
        config.page_size
    };

    let running_pairs = running_store.to_pairs();

    // Build margin-box CSS: strip display:none rules that the parser
    // injected for running elements (they need to be visible in margin boxes).
    let margin_css = strip_display_none(&gcpm.cleaned_css);

    // Caches: measure (html → max-content width), render (html+width → Pageable)
    let mut measure_cache: MeasureCache = HashMap::new();
    let mut render_cache: RenderCache = HashMap::new();

    let mut document = krilla::Document::new();

    // Pass 2: render each page with margin boxes
    for (page_idx, page_content) in pages.iter().enumerate() {
        let page_num = page_idx + 1;

        let settings = krilla::page::PageSettings::from_wh(page_size.width, page_size.height)
            .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;
        let mut page = document.start_page_with(settings);
        let mut surface = page.surface();
        let mut canvas = Canvas {
            surface: &mut surface,
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
            let content_html =
                resolve_content_to_html(&rule.content, &running_pairs, page_num, total_pages);
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

        // Stage 1: Measure max-content width for each unique HTML.
        // Uses inline-block wrapper so Blitz computes shrink-to-fit width.
        for html in resolved_htmls.values() {
            if !measure_cache.contains_key(html) {
                let measure_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\"><div style=\"display:inline-block\">{}</div></body></html>",
                    margin_css, html
                );
                let measure_doc = crate::blitz_adapter::parse_and_layout(
                    &measure_html,
                    content_width,
                    page_size.height,
                    font_data,
                );
                let max_content_width = get_body_child_width(&measure_doc);
                measure_cache.insert(html.clone(), max_content_width);
            }
        }

        // Stage 2: Group by edge and compute layout
        let mut top_defined: BTreeMap<MarginBoxPosition, f32> = BTreeMap::new();
        let mut bottom_defined: BTreeMap<MarginBoxPosition, f32> = BTreeMap::new();

        for (&pos, html) in &resolved_htmls {
            if let Some(&mcw) = measure_cache.get(html) {
                match pos {
                    MarginBoxPosition::TopLeft
                    | MarginBoxPosition::TopCenter
                    | MarginBoxPosition::TopRight => {
                        top_defined.insert(pos, mcw);
                    }
                    MarginBoxPosition::BottomLeft
                    | MarginBoxPosition::BottomCenter
                    | MarginBoxPosition::BottomRight => {
                        bottom_defined.insert(pos, mcw);
                    }
                    _ => {} // corners and left/right edges handled separately
                }
            }
        }

        let top_rects = compute_edge_layout(Edge::Top, &top_defined, page_size, config.margin);
        let bottom_rects =
            compute_edge_layout(Edge::Bottom, &bottom_defined, page_size, config.margin);

        // Stage 3: Render at confirmed width and draw.
        // Pageable is created (or fetched from cache) at the final rect width.
        for (&pos, html) in &resolved_htmls {
            let rect = if let Some(r) = top_rects.get(&pos) {
                *r
            } else if let Some(r) = bottom_rects.get(&pos) {
                *r
            } else {
                pos.bounding_rect(page_size, config.margin)
            };

            let cache_key = (html.clone(), width_key(rect.width));
            if !render_cache.contains_key(&cache_key) {
                let render_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\">{}</body></html>",
                    margin_css, html
                );
                let render_doc = crate::blitz_adapter::parse_and_layout(
                    &render_html,
                    rect.width,
                    rect.height,
                    font_data,
                );
                let mut dummy_store = RunningElementStore::new();
                let pageable = crate::convert::dom_to_pageable(&render_doc, None, &mut dummy_store);
                render_cache.insert(cache_key.clone(), pageable);
            }

            if let Some(pageable) = render_cache.get(&cache_key) {
                pageable.draw(&mut canvas, rect.x, rect.y, rect.width, rect.height);
            }
        }

        // Draw body content
        page_content.draw(
            &mut canvas,
            config.margin.left,
            config.margin.top,
            content_width,
            content_height,
        );
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
