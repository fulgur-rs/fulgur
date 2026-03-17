use crate::config::Config;
use crate::error::{Error, Result};
use crate::pageable::{Canvas, Pageable};
use crate::paginate::paginate;

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
