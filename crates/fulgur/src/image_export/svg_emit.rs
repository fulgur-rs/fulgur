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
use crate::drawables::ImageEntry;
use crate::image::ImageFormat as InputImageFormat;
use crate::image_export::b64;
use crate::image_export::glyph_path::glyph_to_svg_path;
use crate::paragraph::{LineItem, ShapedLine};

/// Emit a paragraph's shaped lines as filled glyph-outline paths. `(ox, oy)`
/// is the paragraph's top-left in pt. Each line's glyphs are placed on a
/// baseline at `oy + line.baseline`; `run.x_offset` and per-glyph
/// `x_advance`/`x_offset` advance the pen.
///
/// `x_advance`, `x_offset`, and `y_offset` in `ShapedGlyph` are EM-fractions
/// (normalized by dividing by `font_size` in `convert.rs`), so they are
/// multiplied by `run.font_size` to obtain pt values.
///
/// Inline images and inline boxes are skipped in v1.
pub fn emit_paragraph(doc: &mut SvgDoc, lines: &[ShapedLine], ox: f32, oy: f32) {
    for line in lines {
        let baseline_y = oy + line.baseline;
        for item in &line.items {
            let LineItem::Text(run) = item else {
                continue; // inline images / boxes: follow-up task
            };
            let mut pen_x = ox + run.x_offset;
            let mut d = String::new();
            for g in &run.glyphs {
                let gx = pen_x + g.x_offset * run.font_size;
                let gy = baseline_y - g.y_offset * run.font_size;
                d.push_str(&glyph_to_svg_path(
                    &run.font_data,
                    run.font_index,
                    g.id,
                    run.font_size,
                    gx,
                    gy,
                ));
                pen_x += g.x_advance * run.font_size;
            }
            if !d.is_empty() {
                let [r, gc, b, a] = run.color;
                doc.push(&format!(
                    r#"<path d="{d}" fill="rgb({r},{gc},{b})" fill-opacity="{:.3}"/>"#,
                    a as f32 / 255.0
                ));
            }
        }
    }
}

/// Emit a raster image as an SVG `<image>` with a base64 data URI at the
/// given pt rect. `preserveAspectRatio="none"` makes the bitmap fill the
/// layout rect, matching how the PDF path scales `<img>` to its box.
pub fn emit_image(doc: &mut SvgDoc, entry: &ImageEntry, x: f32, y: f32, w: f32, h: f32) {
    let mime = match entry.format {
        InputImageFormat::Png => "image/png",
        InputImageFormat::Jpeg => "image/jpeg",
        InputImageFormat::Gif => "image/gif",
    };
    let data = b64::encode(&entry.image_data);
    doc.push(&format!(
        r#"<image x="{}" y="{}" width="{}" height="{}" preserveAspectRatio="none" opacity="{:.3}" href="data:{mime};base64,{data}"/>"#,
        trim(x),
        trim(y),
        trim(w),
        trim(h),
        entry.opacity,
    ));
}

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

    #[test]
    fn image_emits_data_uri() {
        use crate::drawables::ImageEntry;
        use std::sync::Arc;
        let entry = ImageEntry {
            image_data: Arc::new(vec![0x89, b'P', b'N', b'G']),
            format: crate::image::ImageFormat::Png,
            width: 10.0,
            height: 10.0,
            opacity: 1.0,
            visible: true,
        };
        let mut doc = SvgDoc::new(100, 100, Background::Transparent);
        emit_image(&mut doc, &entry, 4.0, 5.0, 20.0, 30.0);
        let svg = doc.finish();
        assert!(svg.contains("<image"));
        assert!(svg.contains("data:image/png;base64,"));
        assert!(svg.contains(r#"x="4""#));
    }

    #[test]
    fn paragraph_emits_filled_text_path() {
        use crate::paragraph::{LineItem, ShapedGlyph, ShapedGlyphRun, ShapedLine, TextDecoration};
        use std::sync::Arc;

        let font = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/.fonts/NotoSans-Regular.ttf"
        ))
        .unwrap();
        // resolve 'A' glyph id robustly
        let a_id = {
            use skrifa::MetadataProvider;
            let f = skrifa::FontRef::from_index(&font, 0).unwrap();
            f.charmap().map('A').unwrap().to_u32()
        };
        let run = ShapedGlyphRun {
            font_data: Arc::new(font),
            font_index: 0,
            font_size: 16.0,
            color: [200, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![ShapedGlyph {
                id: a_id,
                x_advance: 10.0,
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..1,
            }],
            text: "A".into(),
            x_offset: 0.0,
            link: None,
        };
        let line = ShapedLine {
            height: 20.0,
            baseline: 14.0,
            items: vec![LineItem::Text(run)],
        };
        let mut doc = SvgDoc::new(100, 100, Background::Transparent);
        emit_paragraph(&mut doc, &[line], 8.0, 8.0);
        let svg = doc.finish();
        assert!(svg.contains("<path"));
        assert!(svg.contains("fill=\"rgb(200,0,0)\""));
    }
}
