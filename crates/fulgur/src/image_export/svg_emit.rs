//! Serialize page-0 `Drawables` into an SVG document string.
//!
//! Coordinates are authored in PDF pt (the same space `render_v2` draws in).
//! Text is emitted as glyph-outline paths so resvg never re-shapes and the
//! result is host-font-independent.

use crate::image_export::options::Background;

const PX_TO_PT: f32 = 0.75;

/// Builder accumulating SVG body content between an opening `<svg>` (sized to
/// the logical canvas, viewBox in pt) and the closing tag.
pub struct SvgDoc {
    width_pt: f32,
    height_pt: f32,
    body: String,
}

impl SvgDoc {
    /// Start a document for a `width_px × height_px` logical canvas.
    pub fn new(width_px: u32, height_px: u32, background: Background) -> Self {
        let width_pt = width_px as f32 * PX_TO_PT;
        let height_pt = height_px as f32 * PX_TO_PT;
        let mut body = String::new();
        if let Background::Solid([r, g, b, a]) = background {
            body.push_str(&format!(
                r#"<rect x="0" y="0" width="{width_pt}" height="{height_pt}" fill="rgb({r},{g},{b})" fill-opacity="{:.3}"/>"#,
                a as f32 / 255.0
            ));
        }
        Self {
            width_pt,
            height_pt,
            body,
        }
    }

    /// Append raw SVG markup to the document body.
    pub fn push(&mut self, markup: &str) {
        self.body.push_str(markup);
    }

    /// Close the document and return the full SVG string.
    pub fn finish(self) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">{body}</svg>"#,
            w = trim(self.width_pt),
            h = trim(self.height_pt),
            body = self.body
        )
    }
}

/// Format a float without a trailing `.0` so `472.5` and `900` both read
/// cleanly in the viewBox.
fn trim(v: f32) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_export::options::Background;

    #[test]
    fn skeleton_has_viewbox_and_size() {
        // 1200x630 logical px → viewBox in pt (px * 0.75).
        let svg = SvgDoc::new(1200, 630, Background::Transparent).finish();
        assert!(svg.contains(r#"viewBox="0 0 900 472.5""#));
        assert!(svg.contains("</svg>"));
        // transparent → no opaque background rect
        assert!(!svg.contains("<rect"));
    }

    #[test]
    fn solid_background_emits_rect() {
        let svg = SvgDoc::new(10, 10, Background::Solid([255, 0, 0, 255])).finish();
        assert!(svg.contains("<rect"));
        assert!(svg.contains("fill=\"rgb(255,0,0)\""));
    }
}
