use crate::config::Config;
use crate::error::{Error, Result};
use crate::gcpm::counter::resolve_content_to_html;
use crate::gcpm::running::RunningElementStore;
use crate::gcpm::GcpmContext;
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

        // Draw margin boxes first (behind body content)
        for margin_box in &gcpm.margin_boxes {
            // Filter by page selector
            if let Some(ref sel) = margin_box.page_selector {
                match sel.as_str() {
                    ":first" if page_num != 1 => continue,
                    ":left" if page_num % 2 != 0 => continue,
                    ":right" if page_num % 2 == 0 => continue,
                    _ => {}
                }
            }
            let resolved_html = resolve_content_to_html(
                &margin_box.content,
                &running_pairs,
                page_num,
                total_pages,
            );

            if resolved_html.is_empty() {
                continue;
            }

            let rect = margin_box.position.bounding_rect(page_size, config.margin);

            // Populate cache if needed
            if !layout_cache.contains_key(&resolved_html) {
                let margin_html = format!(
                    "<html><body style=\"margin:0;padding:0;\">{}</body></html>",
                    resolved_html
                );
                let margin_doc = crate::blitz_adapter::parse_and_layout(
                    &margin_html,
                    rect.width,
                    rect.height,
                    font_data,
                );
                let mut dummy_store = RunningElementStore::new();
                let pageable =
                    crate::convert::dom_to_pageable(&margin_doc, None, &mut dummy_store);
                layout_cache.insert(resolved_html.clone(), pageable);
            }

            if let Some(margin_pageable) = layout_cache.get(&resolved_html) {
                margin_pageable.draw(
                    &mut canvas,
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                );
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
