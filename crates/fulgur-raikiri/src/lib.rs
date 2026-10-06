//! Raikiri layout backend for Fulgur's development CLI.
//!
//! The input is a file path and the output is PDF bytes. Drawing covers page
//! geometry, box backgrounds and borders, and text. No other backend is used
//! as a fallback.

use fulgur_core::{AssetBundle, Config, Error, Result};
use raikiri_html::{
    LayoutConfig, LayoutOptions, LayoutStatus, PageDefaults, RenderResources, layout,
    parse_html_with_resources,
};
use std::path::Path;

/// Rendering resources for the Raikiri development backend.
///
/// Fonts supplied by the bundle are registered in order. Image bundles are
/// rejected because the current consumer paint API cannot draw their content.
#[derive(Clone, Copy)]
pub struct RenderOptions<'a> {
    /// Optional user stylesheets and bundled fonts.
    pub assets: Option<&'a AssetBundle>,
    /// Whether host-dependent system font fallback is enabled.
    pub system_fonts: bool,
}

impl Default for RenderOptions<'_> {
    fn default() -> Self {
        Self {
            assets: None,
            system_fonts: true,
        }
    }
}

/// Read, lay out, and paint an HTML file using default rendering resources.
///
/// Page settings act as CSS defaults unless marked explicit in the config.
/// Local resources are limited to the input file's directory.
///
/// # Errors
/// Returns an error for invalid config, input IO, layout, or PDF generation.
pub fn render(input: &Path, config: &Config) -> Result<Vec<u8>> {
    render_with_options(input, config, &RenderOptions::default())
}

/// Render with additional user stylesheets and bundled fonts.
///
/// The same resources remain alive throughout parsing, layout, and painting.
///
/// # Errors
/// Returns an asset error for invalid fonts, image bundles, or disabling system
/// fonts without a bundled font; other errors match [`render`].
pub fn render_with_options(
    input: &Path,
    config: &Config,
    options: &RenderOptions<'_>,
) -> Result<Vec<u8>> {
    config.validate()?;
    metadata::build(config)?;
    with_layout(
        input,
        config,
        options,
        LayoutConfig::default(),
        |status, _resources| draw(status, config),
    )
}

/// Draw a layout result as PDF bytes.
fn draw(status: LayoutStatus, config: &Config) -> Result<Vec<u8>> {
    // `LayoutStatus` is non-exhaustive; anything but a completed layout,
    // including an abort, leaves no pages to draw.
    let LayoutStatus::Completed(document) = status else {
        return Err(Error::Layout(
            "Raikiri layout was aborted or did not complete".into(),
        ));
    };
    paint::paint_document(&document, config, None)
}

#[cfg(test)]
fn layout_file(input: &Path, config: &Config, layout_config: LayoutConfig) -> Result<LayoutStatus> {
    with_layout(
        input,
        config,
        &RenderOptions::default(),
        layout_config,
        |status, _resources| Ok(status),
    )
}

fn with_layout<T>(
    input: &Path,
    config: &Config,
    options: &RenderOptions<'_>,
    layout_config: LayoutConfig,
    consume: impl FnOnce(LayoutStatus, &RenderResources<'_>) -> Result<T>,
) -> Result<T> {
    let fonts = assets::fonts(options)?;
    let html = std::fs::read(input)?;
    let files = files::BaseDirectoryProvider::for_input(input)?;
    let mut resources = RenderResources::new()
        .stylesheet(page_stylesheet(config))
        .network_provider(&files)
        .base_url(files.document_url(input)?);
    if let Some(bundle) = options.assets {
        for css in &bundle.css {
            resources = resources.stylesheet(css.clone());
        }
    }
    if let Some(fonts) = fonts {
        resources = resources.fonts(fonts);
    }
    let document = parse_html_with_resources(html.as_slice(), &resources)
        .map_err(|error| Error::Layout(error.to_string()))?;
    let status = layout(
        &document,
        PageDefaults::default(),
        layout_config,
        LayoutOptions::new().resources(&resources),
    )
    .map_err(|error| Error::Layout(error.to_string()))?;
    consume(status, &resources)
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

mod assets;

mod metadata;
