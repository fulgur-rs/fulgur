//! Raikiri layout backend for Fulgur's development CLI.
//!
//! The input is a file path and the intended output is PDF bytes.
//! PDF drawing is not implemented yet; successful layout returns an explicit
//! drawing error. No other backend is used as a fallback.

use fulgur_core::{Error, Result};
use raikiri_html::{
    LayoutOptions, LayoutStatus, PageDefaults, RenderResources, StreamingConfig, layout,
    parse_html_with_resources,
};
use std::path::Path;

/// Read and lay out an HTML file for PDF rendering.
///
/// Returns an IO or layout error if those stages fail. After a successful
/// layout, returns a PDF generation error until PDF drawing is implemented.
/// External resources are not loaded; styles must be embedded in the HTML.
pub fn render(input: &Path) -> Result<Vec<u8>> {
    match layout_file(input, StreamingConfig::default())? {
        LayoutStatus::Completed(document) => {
            for page in document.pages() {
                let _geometry = page.geometry();
                for fragment in page.fragments() {
                    let _placement = (fragment.node(), fragment.rect(), fragment.line_range());
                }
            }
            Err(Error::PdfGeneration(
                "Raikiri PDF drawing is not implemented".into(),
            ))
        }
        LayoutStatus::Aborted => Err(Error::Layout("Raikiri layout was aborted".into())),
        _ => Err(Error::Layout("Unsupported Raikiri layout status".into())),
    }
}

fn layout_file(input: &Path, config: StreamingConfig) -> Result<LayoutStatus> {
    let html = std::fs::read(input)?;
    let resources = RenderResources::new();
    let document = parse_html_with_resources(html.as_slice(), &resources)
        .map_err(|error| Error::Layout(error.to_string()))?;
    layout(
        &document,
        PageDefaults::default(),
        config,
        LayoutOptions::new().resources(&resources),
    )
    .map_err(|error| Error::Layout(error.to_string()))
}

#[cfg(test)]
mod tests;
