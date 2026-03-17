//! Thin adapter over Blitz APIs. All Blitz-specific code is isolated here
//! so that upstream API changes only require changes in this module.

use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};
use parley::FontContext;
use std::sync::Arc;

/// Parse HTML and return a fully resolved document (styles + layout computed).
///
/// We pass the content width as the viewport width so Taffy wraps text
/// at the right column. The viewport height is set very large so that
/// Taffy lays out the full document without clipping — our own pagination
/// algorithm handles page breaks.
pub fn parse_and_layout(
    html: &str,
    viewport_width: f32,
    _viewport_height: f32,
    font_data: &[Arc<Vec<u8>>],
) -> HtmlDocument {
    let viewport = Viewport::new(
        viewport_width as u32,
        10000, // Large height — let Taffy lay out everything, we paginate later
        1.0,
        ColorScheme::Light,
    );

    // Build FontContext with bundled fonts
    let font_ctx = if font_data.is_empty() {
        None
    } else {
        let mut ctx = FontContext::new();
        for data in font_data {
            let blob: parley::fontique::Blob<u8> = (**data).clone().into();
            ctx.collection.register_fonts(blob, None);
        }
        Some(ctx)
    };

    let config = DocumentConfig {
        viewport: Some(viewport),
        font_ctx,
        ..DocumentConfig::default()
    };

    let mut doc = HtmlDocument::from_html(html, config);

    // Resolve styles (Stylo) and layout (Taffy)
    doc.resolve(0.0);

    doc
}
