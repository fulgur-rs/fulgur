//! Thin adapter over Blitz APIs. All Blitz-specific code is isolated here
//! so that upstream API changes only require changes in this module.

use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};

/// Parse HTML and return a fully resolved document (styles + layout computed).
pub fn parse_and_layout(
    html: &str,
    viewport_width: f32,
    viewport_height: f32,
) -> HtmlDocument {
    let viewport = Viewport::new(
        viewport_width as u32,
        viewport_height as u32,
        1.0,
        ColorScheme::Light,
    );

    let config = DocumentConfig {
        viewport: Some(viewport),
        ..DocumentConfig::default()
    };

    let mut doc = HtmlDocument::from_html(html, config);

    // Resolve styles (Stylo) and layout (Taffy)
    doc.resolve(0.0);

    doc
}
