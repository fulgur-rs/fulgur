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

use crate::draw_primitives::BlockStyle;

/// Emit a block's background fill and a uniform border rect at the given
/// pt-space rectangle. v1 handles a solid background color, a uniform border
/// (using the top width/color), and uniform border-radius (top-left rx/ry).
/// Per-side widths/colors and non-uniform radii are a follow-up.
pub fn emit_block(doc: &mut SvgDoc, style: &BlockStyle, x: f32, y: f32, w: f32, h: f32) {
    let rx = style.border_radii[0][0];
    let ry = style.border_radii[0][1];
    let radius_attr = if rx > 0.0 || ry > 0.0 {
        format!(r#" rx="{}" ry="{}""#, trim(rx), trim(ry))
    } else {
        String::new()
    };

    if let Some([r, g, b, a]) = style.background_color {
        doc.push(&format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}"{radius} fill="rgb({r},{g},{b})" fill-opacity="{:.3}"/>"#,
            trim(x),
            trim(y),
            trim(w),
            trim(h),
            a as f32 / 255.0,
            radius = radius_attr,
        ));
    }

    let bw = style.border_widths[0];
    if bw > 0.0 {
        let [r, g, b, a] = style.border_color;
        let half = bw / 2.0;
        doc.push(&format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}"{radius} fill="none" stroke="rgb({r},{g},{b})" stroke-opacity="{:.3}" stroke-width="{}"/>"#,
            trim(x + half),
            trim(y + half),
            trim((w - bw).max(0.0)),
            trim((h - bw).max(0.0)),
            a as f32 / 255.0,
            trim(bw),
            radius = radius_attr,
        ));
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
    fn block_emits_background_and_border() {
        use crate::draw_primitives::BlockStyle;
        let mut doc = SvgDoc::new(100, 100, Background::Transparent);
        let mut style = BlockStyle::default();
        style.background_color = Some([10, 20, 30, 255]);
        style.border_color = [0, 0, 0, 255];
        style.border_widths = [2.0, 2.0, 2.0, 2.0];
        emit_block(&mut doc, &style, 5.0, 6.0, 40.0, 50.0);
        let svg = doc.finish();
        assert!(svg.contains("fill=\"rgb(10,20,30)\""));
        assert!(svg.contains(r#"x="5""#));
        assert!(svg.contains("stroke="));
    }

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
