//! Raikiri layout backend for Fulgur's development CLI.
//!
//! The input is a file path and the output is PDF bytes. Drawing covers page
//! geometry, box backgrounds and borders, and text. No other backend is used
//! as a fallback.

use fulgur_core::{Config, Error, Result};
use raikiri_html::{
    LayoutConfig, LayoutOptions, LayoutStatus, PageDefaults, RenderResources, layout,
    parse_html_with_resources,
};
use std::path::Path;

/// Read and lay out an HTML file for PDF rendering.
///
/// `config` supplies the page size and margins (see [`page_stylesheet`]).
/// Stylesheets, `@import`s, and other resources the document references are
/// read from the local filesystem, limited to the input file's directory.
///
/// Returns a configuration error for page geometry that
/// [`Config::validate`] rejects, an IO or layout error if those stages fail,
/// or a PDF generation error if the PDF cannot be written.
pub fn render(input: &Path, config: &Config) -> Result<Vec<u8>> {
    config.validate()?;
    draw(layout_file(input, config, LayoutConfig::default())?)
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

fn layout_file(input: &Path, config: &Config, layout_config: LayoutConfig) -> Result<LayoutStatus> {
    let html = std::fs::read(input)?;
    let files = files::BaseDirectoryProvider::for_input(input)?;
    let resources = RenderResources::new()
        .stylesheet(page_stylesheet(config))
        .network_provider(&files)
        .base_url(files.document_url(input)?);
    let document = parse_html_with_resources(html.as_slice(), &resources)
        .map_err(|error| Error::Layout(error.to_string()))?;
    layout(
        &document,
        PageDefaults::default(),
        layout_config,
        LayoutOptions::new().resources(&resources),
    )
    .map_err(|error| Error::Layout(error.to_string()))
}

/// The page size and margins of `config` as a user-origin `@page` rule.
///
/// User declarations lose to the document's own `@page` rules, which matches
/// Fulgur's defaults: CSS `size` and `margin` win unless the caller set them
/// explicitly. A field marked in [`Config::overrides`] is declared
/// `!important`, and important user declarations beat every author
/// declaration (CSS Cascade 4 §6.2).
///
/// Two cases still differ from the Blitz backend: a landscape-only override
/// keeps the document's `@page size` as declared, orientation included,
/// because CSS cannot override the orientation of a size it does not name;
/// and `@page { size: auto }` resolves to A4 rather than the configured size.
fn page_stylesheet(config: &Config) -> String {
    let overrides = config.overrides;
    let size = if config.landscape {
        config.page_size.landscape()
    } else {
        config.page_size
    };
    let size_priority = important(overrides.page_size);
    let margin = config.margin;
    let margin_priority = important(overrides.margin);
    format!(
        "@page {{ size: {}pt {}pt{size_priority}; margin: {}pt {}pt {}pt {}pt{margin_priority}; }}",
        size.width, size.height, margin.top, margin.right, margin.bottom, margin.left,
    )
}

fn important(set: bool) -> &'static str {
    if set { " !important" } else { "" }
}

#[cfg(test)]
mod tests;

mod files;
mod paint;
