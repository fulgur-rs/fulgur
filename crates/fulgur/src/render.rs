use crate::config::Config;
use crate::error::{Error, Result};
use crate::gcpm::GcpmContext;
use crate::gcpm::counter::resolve_content_to_html;
use crate::gcpm::margin_box::{Edge, MarginBoxPosition, compute_edge_layout};
use crate::gcpm::running::RunningElementStore;
use crate::pageable::{Canvas, Pageable};
use crate::paginate::paginate;
use std::collections::HashMap;
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

    // Set metadata
    let mut metadata = krilla::metadata::Metadata::new();
    if let Some(ref title) = config.title {
        metadata = metadata.title(title.clone());
    }
    if let Some(ref author) = config.author {
        metadata = metadata.authors(vec![author.clone()]);
    }

    document.set_metadata(metadata);

    let pdf_bytes = document
        .finish()
        .map_err(|e| Error::PdfGeneration(format!("{e:?}")))?;
    Ok(pdf_bytes)
}

/// A cached margin box layout: the pageable tree and its max-content width.
struct MarginBoxLayout {
    pageable: Box<dyn Pageable>,
    max_content_width: f32,
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

    // Layout cache: resolved_html -> MarginBoxLayout
    let mut layout_cache: HashMap<String, MarginBoxLayout> = HashMap::new();

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
        let mut effective_boxes: HashMap<MarginBoxPosition, &crate::gcpm::MarginBoxRule> =
            HashMap::new();
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

        // Collect resolved HTML for each effective box
        let mut resolved_htmls: HashMap<MarginBoxPosition, String> = HashMap::new();
        for (&pos, rule) in &effective_boxes {
            let html =
                resolve_content_to_html(&rule.content, &running_pairs, page_num, total_pages);
            if !html.is_empty() {
                resolved_htmls.insert(pos, html);
            }
        }

        // Stage 1: Layout all at content_width, populate cache
        for html in resolved_htmls.values() {
            if !layout_cache.contains_key(html) {
                let margin_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\">{}</body></html>",
                    margin_css, html
                );
                let margin_doc = crate::blitz_adapter::parse_and_layout(
                    &margin_html,
                    content_width,
                    page_size.height,
                    font_data,
                );
                let max_content_width = get_body_child_width(&margin_doc);
                let mut dummy_store = RunningElementStore::new();
                let pageable = crate::convert::dom_to_pageable(&margin_doc, None, &mut dummy_store);
                layout_cache.insert(
                    html.clone(),
                    MarginBoxLayout {
                        pageable,
                        max_content_width,
                    },
                );
            }
        }

        // Stage 2: Group by edge and compute layout
        let mut top_defined: HashMap<MarginBoxPosition, f32> = HashMap::new();
        let mut bottom_defined: HashMap<MarginBoxPosition, f32> = HashMap::new();

        for (&pos, html) in &resolved_htmls {
            if let Some(layout) = layout_cache.get(html) {
                match pos {
                    MarginBoxPosition::TopLeft
                    | MarginBoxPosition::TopCenter
                    | MarginBoxPosition::TopRight => {
                        top_defined.insert(pos, layout.max_content_width);
                    }
                    MarginBoxPosition::BottomLeft
                    | MarginBoxPosition::BottomCenter
                    | MarginBoxPosition::BottomRight => {
                        bottom_defined.insert(pos, layout.max_content_width);
                    }
                    _ => {} // corners and left/right edges handled separately
                }
            }
        }

        let top_rects = compute_edge_layout(Edge::Top, &top_defined, page_size, config.margin);
        let bottom_rects =
            compute_edge_layout(Edge::Bottom, &bottom_defined, page_size, config.margin);

        // Stage 3: Draw
        for (&pos, html) in &resolved_htmls {
            let rect = if let Some(r) = top_rects.get(&pos) {
                *r
            } else if let Some(r) = bottom_rects.get(&pos) {
                *r
            } else {
                // Corner or left/right edge: use bounding_rect
                pos.bounding_rect(page_size, config.margin)
            };

            if let Some(layout) = layout_cache.get(html) {
                layout
                    .pageable
                    .draw(&mut canvas, rect.x, rect.y, rect.width, rect.height);
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

    // Set metadata (same as render_to_pdf)
    let mut metadata = krilla::metadata::Metadata::new();
    if let Some(ref title) = config.title {
        metadata = metadata.title(title.clone());
    }
    if let Some(ref author) = config.author {
        metadata = metadata.authors(vec![author.clone()]);
    }
    document.set_metadata(metadata);

    let pdf_bytes = document
        .finish()
        .map_err(|e| Error::PdfGeneration(format!("{e:?}")))?;
    Ok(pdf_bytes)
}

/// Strip `display: none` declarations from CSS.
/// Used to build margin-box CSS where running elements need to be visible.
fn strip_display_none(css: &str) -> String {
    css.replace("display: none", "").replace("display:none", "")
}
