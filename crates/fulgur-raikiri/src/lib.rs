//! Raikiri layout backend for Fulgur's development CLI.
//!
//! The input is a file path or an HTML string and the output is PDF bytes. Drawing covers page
//! geometry, box backgrounds and borders, text, and resolved images. No other backend is used
//! as a fallback.

use fulgur_core::{AssetBundle, Config, Error, Result};
use raikiri_html::{
    LayoutConfig, LayoutOptions, LayoutStatus, PageDefaults, RenderResources, layout,
    parse_html_with_resources,
};
use std::borrow::Cow;
use std::path::Path;

/// Rendering resources for the Raikiri development backend.
///
/// Fonts supplied by the bundle are registered in order. Each bundle image
/// is a file at the URL its name resolves to against the base directory (the
/// input file's directory); documents reach it with ordinary URL resolution,
/// ahead of the files in that directory.
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
/// Returns an asset error for invalid fonts or disabling system fonts without
/// a bundled font; other errors match [`render`].
pub fn render_with_options(
    input: &Path,
    config: &Config,
    options: &RenderOptions<'_>,
) -> Result<Vec<u8>> {
    render_source(Source::File(input), config, options)
}

/// Lay out and paint an HTML string using default rendering resources.
///
/// Relative URLs resolve against `base_dir`, and local resources are limited
/// to that directory, as for a file inside it. Without a base directory the
/// document behaves like `about:blank`: no resource is read and only
/// same-document fragment links resolve.
///
/// # Errors
/// Returns an error for invalid config, an unreadable base directory, layout,
/// or PDF generation.
pub fn render_html(html: &str, base_dir: Option<&Path>, config: &Config) -> Result<Vec<u8>> {
    render_html_with_options(html, base_dir, config, &RenderOptions::default())
}

/// Render an HTML string with additional user stylesheets and bundled fonts.
///
/// # Errors
/// Returns an asset error as [`render_with_options`] does; other errors match
/// [`render_html`].
pub fn render_html_with_options(
    html: &str,
    base_dir: Option<&Path>,
    config: &Config,
    options: &RenderOptions<'_>,
) -> Result<Vec<u8>> {
    render_source(Source::Html { html, base_dir }, config, options)
}

fn render_source(
    source: Source<'_>,
    config: &Config,
    options: &RenderOptions<'_>,
) -> Result<Vec<u8>> {
    config.validate()?;
    metadata::build(config)?;
    with_layout(
        source,
        config,
        options,
        LayoutConfig::default(),
        |status, resources, collector, document_url| {
            draw(status, resources, config, collector, document_url, options)
        },
    )
}

/// Where the HTML comes from and which directory its resources come from.
#[derive(Clone, Copy)]
enum Source<'a> {
    /// A file, read whole; resources come from its directory.
    File(&'a Path),
    /// HTML already in memory, with an optional base directory.
    Html {
        html: &'a str,
        base_dir: Option<&'a Path>,
    },
}

impl<'a> Source<'a> {
    /// The HTML bytes, the file provider, and the document URL.
    fn load(self) -> Result<(Cow<'a, [u8]>, files::BaseDirectoryProvider, url::Url)> {
        match self {
            Self::File(input) => {
                let html = std::fs::read(input)?;
                let files = files::BaseDirectoryProvider::for_input(input)?;
                let document_url = files.document_url(input)?;
                Ok((Cow::Owned(html), files, document_url))
            }
            Self::Html { html, base_dir } => {
                let files = match base_dir {
                    Some(dir) => files::BaseDirectoryProvider::for_directory(dir)?,
                    None => files::BaseDirectoryProvider::none(),
                };
                let document_url = files.directory_url()?;
                Ok((Cow::Borrowed(html.as_bytes()), files, document_url))
            }
        }
    }
}

/// Draw a layout result as PDF bytes.
fn draw(
    status: LayoutStatus,
    resources: &RenderResources<'_>,
    config: &Config,
    collector: &bookmarks::BookmarkCollector,
    document_url: &url::Url,
    options: &RenderOptions<'_>,
) -> Result<Vec<u8>> {
    // `LayoutStatus` is non-exhaustive; anything but a completed layout,
    // including an abort, leaves no pages to draw.
    let LayoutStatus::Completed(document) = status else {
        return Err(Error::Layout(
            "Raikiri layout was aborted or did not complete".into(),
        ));
    };
    let outline = config
        .bookmarks
        .then(|| bookmarks::outline(&document, collector));
    paint::paint_document(&document, resources, config, outline, document_url, options)
}

#[cfg(test)]
fn layout_file(input: &Path, config: &Config, layout_config: LayoutConfig) -> Result<LayoutStatus> {
    with_layout(
        Source::File(input),
        config,
        &RenderOptions::default(),
        layout_config,
        |status, _resources, _collector, _document_url| Ok(status),
    )
}

fn with_layout<T>(
    source: Source<'_>,
    config: &Config,
    options: &RenderOptions<'_>,
    layout_config: LayoutConfig,
    consume: impl FnOnce(
        LayoutStatus,
        &RenderResources<'_>,
        &bookmarks::BookmarkCollector,
        &url::Url,
    ) -> Result<T>,
) -> Result<T> {
    let fonts = assets::fonts(options)?;
    let (html, files, document_url) = source.load()?;
    let network = bundle::Sandbox::new(options.assets, files);
    let images = raikiri_net::ImageResolver::new(network.clone());
    let resolver = images::OptionalImages(&images);
    let mut resources = RenderResources::new()
        .replaced_resolver(&resolver)
        .image_pixel_source(&images)
        .stylesheet(page_stylesheet(config))
        .network_provider(&network)
        .base_url(document_url.clone());
    if config.bookmarks {
        resources = resources.stylesheet(bookmarks::heading_stylesheet());
    }
    if let Some(bundle) = options.assets {
        for css in &bundle.css {
            resources = resources.stylesheet(css.clone());
        }
    }
    if let Some(fonts) = fonts {
        resources = resources.fonts(fonts);
    }
    let document = parse_html_with_resources(html.as_ref(), &resources)
        .map_err(|error| Error::Layout(error.to_string()))?;
    let registrations = if config.bookmarks {
        bookmarks::registrations()
    } else {
        Vec::new()
    };
    let mut collector = bookmarks::BookmarkCollector::default();
    let mut layout_options = LayoutOptions::new().resources(&resources);
    if config.bookmarks {
        layout_options = layout_options.consumer_properties(&registrations, &mut collector);
    }
    let status = layout(
        &document,
        PageDefaults::default(),
        layout_config,
        layout_options,
    )
    .map_err(|error| Error::Layout(error.to_string()))?;
    consume(status, &resources, &collector, &document_url)
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

mod bundle;
mod files;
mod images;
mod paint;

mod assets;

mod metadata;

mod bookmarks;
