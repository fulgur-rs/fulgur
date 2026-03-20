use crate::config::Config;
use crate::error::{Error, Result};
use crate::gcpm::GcpmContext;
use crate::gcpm::counter::resolve_content_to_html;
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

    // Layout cache: resolved_html -> pageable tree
    let mut layout_cache: HashMap<String, Box<dyn Pageable>> = HashMap::new();

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
        use crate::gcpm::margin_box::MarginBoxPosition;
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

        // Draw effective margin boxes
        for margin_box in effective_boxes.values() {
            let resolved_html =
                resolve_content_to_html(&margin_box.content, &running_pairs, page_num, total_pages);

            // Empty content means suppress this margin box
            if resolved_html.is_empty() {
                continue;
            }

            let rect = margin_box.position.bounding_rect(page_size, config.margin);

            // Populate cache if needed
            if !layout_cache.contains_key(&resolved_html) {
                let margin_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\">{}</body></html>",
                    margin_css, resolved_html
                );
                let margin_doc = crate::blitz_adapter::parse_and_layout(
                    &margin_html,
                    rect.width,
                    rect.height,
                    font_data,
                );
                let mut dummy_store = RunningElementStore::new();
                let pageable = crate::convert::dom_to_pageable(&margin_doc, None, &mut dummy_store);
                layout_cache.insert(resolved_html.clone(), pageable);
            }

            if let Some(margin_pageable) = layout_cache.get(&resolved_html) {
                margin_pageable.draw(&mut canvas, rect.x, rect.y, rect.width, rect.height);
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
