//! Raikiri layout backend for Fulgur's development CLI.
//!
//! The input is a file path and the output is PDF bytes. Drawing covers page
//! geometry, box backgrounds and borders, and text. No other backend is used
//! as a fallback.

use fulgur_core::{Error, Result};
use raikiri_html::{
    LayoutConfig, LayoutOptions, LayoutStatus, PageDefaults, RenderResources, layout,
    parse_html_with_resources,
};
use std::path::Path;

/// Read and lay out an HTML file for PDF rendering.
///
/// Returns an IO or layout error if those stages fail, or a PDF generation
/// error if the PDF cannot be written. External resources are not loaded;
/// styles must be embedded in the HTML.
pub fn render(input: &Path) -> Result<Vec<u8>> {
    draw(layout_file(input, LayoutConfig::default())?)
}

/// Draw a layout result as PDF bytes.
fn draw(status: LayoutStatus) -> Result<Vec<u8>> {
    // `LayoutStatus` is non-exhaustive; anything but a completed layout,
    // including an abort, leaves no pages to draw.
    let LayoutStatus::Completed(document) = status else {
        return Err(Error::Layout(
            "Raikiri layout was aborted or did not complete".into(),
        ));
    };
    paint::paint_document(&document)
}

fn layout_file(input: &Path, config: LayoutConfig) -> Result<LayoutStatus> {
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

mod paint;
