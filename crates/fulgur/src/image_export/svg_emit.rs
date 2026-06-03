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

use crate::draw_primitives::{
    BackgroundLayer, BgImageContent, BlockStyle, GradientStopPosition, LinearGradientCorner,
    LinearGradientDirection,
};
use crate::drawables::{Drawables, ImageEntry};
use crate::image::ImageFormat as InputImageFormat;
use crate::image_export::b64;
use crate::image_export::glyph_path::glyph_to_svg_path_from_outlines;
use crate::image_export::options::ImageOptions;
use crate::pagination_layout::PaginationGeometryTable;
use crate::paragraph::{LineItem, ShapedLine};

/// One SVG sub-tree to composite onto the page during rasterization:
/// `(tree, x_pt, y_pt, w_pt, h_pt)` — the parsed `usvg::Tree` plus its
/// destination rect in pt-space.
pub type SvgComposite = (std::sync::Arc<usvg::Tree>, f32, f32, f32, f32);

/// Serialize page-0 drawables to an SVG string + the list of SvgEntry
/// sub-trees to composite at their pt rect (resvg renders sub-trees more
/// faithfully than re-nesting). Fragments are converted CSS px → pt. Paint
/// order is `BTreeMap`(NodeId) order, which approximates document order —
/// correct for flat single-composed-image layouts. Transforms, clip/opacity
/// groups, tables, list markers, and multicol are NOT dispatched in v1.
///
/// ## Deferred limitations (conscious divergences from the PDF path)
///
/// - **Replaced-element placement**: `<img>` and inline `<svg>` elements are
///   placed at the fragment border box. Padded/bordered replaced elements are
///   NOT inset to the content box (the PDF path insets by padding + border).
///   For unpadded/unbordered replaced elements the two positions coincide.
///
/// - **SVG z-order**: SVG sub-trees are composited after the whole page SVG,
///   so across different nodes an SVG paints on top of later nodes' content
///   (cross-node z-order is approximate). This is fine for flat
///   single-composed-image layouts.
pub fn page_to_svg(
    drawables: &Drawables,
    geometry: &PaginationGeometryTable,
    opts: &ImageOptions,
) -> (String, Vec<SvgComposite>) {
    let mut doc = SvgDoc::new(opts.width_px, opts.height_px, opts.background);
    let mut svg_composites = Vec::new();
    let (bx, by) = drawables.body_offset_pt;

    for (node_id, geo) in geometry {
        let Some(frag) = geo.fragments.iter().find(|f| f.page_index == 0) else {
            continue;
        };
        let x = bx + frag.x * PX_TO_PT;
        let y = by + frag.y * PX_TO_PT;
        let w = frag.width * PX_TO_PT;
        let h = frag.height * PX_TO_PT;

        if let Some(block) = drawables.block_styles.get(node_id) {
            if block.visible {
                for (i, layer) in block.style.background_layers.iter().enumerate() {
                    let id = format!("grad-{node_id}-{i}");
                    emit_background_layer(&mut doc, layer, x, y, w, h, &id);
                }
                emit_block(&mut doc, &block.style, x, y, w, h);
            }
        }
        if let Some(p) = drawables.paragraphs.get(node_id) {
            if p.visible {
                emit_paragraph(&mut doc, &p.lines, x, y);
            }
        }
        if let Some(img) = drawables.images.get(node_id) {
            if img.visible {
                emit_image(&mut doc, img, x, y, w, h);
            }
        }
        if let Some(svg) = drawables.svgs.get(node_id) {
            if svg.visible {
                svg_composites.push((svg.tree.clone(), x, y, w, h));
            }
        }
    }

    (doc.finish(), svg_composites)
}

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
            // Parse the font once per run, then reuse the outline set for
            // every glyph (avoids re-parsing the font file per glyph).
            use skrifa::MetadataProvider as _;
            let font = skrifa::FontRef::from_index(&run.font_data, run.font_index).ok();
            let outlines = font.as_ref().map(|f| f.outline_glyphs());
            let mut pen_x = ox + run.x_offset;
            let mut d = String::new();
            for g in &run.glyphs {
                let gx = pen_x + g.x_offset * run.font_size;
                let gy = baseline_y - g.y_offset * run.font_size;
                if let Some(outlines) = &outlines {
                    d.push_str(&glyph_to_svg_path_from_outlines(
                        outlines,
                        g.id,
                        run.font_size,
                        gx,
                        gy,
                    ));
                }
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

/// Emit one background layer. v1 supports `LinearGradient`; other contents are
/// skipped (follow-up). `id_suffix` is the caller-supplied gradient id (unique
/// per node + layer, e.g. `grad-{node_id}-{layer_idx}`).
///
/// The gradient line is computed in `objectBoundingBox` space (unit box, Y-down)
/// from the CSS direction, mirroring `background.rs::draw_linear_gradient` so
/// the image matches the PDF path. Corner directions are approximated by their
/// nominal 45° angle (the exact CSS corner angle depends on the box aspect
/// ratio; the unit-box approximation is close for near-square boxes).
pub fn emit_background_layer(
    doc: &mut SvgDoc,
    layer: &BackgroundLayer,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    id_suffix: &str,
) {
    let BgImageContent::LinearGradient {
        direction, stops, ..
    } = &layer.content
    else {
        return; // raster / svg / radial / conic: follow-up
    };
    let id = id_suffix;
    let (x1, y1, x2, y2) = gradient_line(*direction);
    let n = stops.len();
    let mut defs = format!(
        r#"<defs><linearGradient id="{id}" x1="{x1:.4}" y1="{y1:.4}" x2="{x2:.4}" y2="{y2:.4}" gradientUnits="objectBoundingBox">"#
    );
    for (i, s) in stops.iter().enumerate() {
        if s.is_hint {
            continue; // interpolation hint marker, not a real color stop
        }
        let offset = match s.position {
            GradientStopPosition::Fraction(f) => f,
            _ if n > 1 => i as f32 / (n - 1) as f32,
            _ => 0.0,
        };
        let [r, g, b, a] = s.rgba;
        defs.push_str(&format!(
            r#"<stop offset="{:.4}" stop-color="rgb({r},{g},{b})" stop-opacity="{:.3}"/>"#,
            offset.clamp(0.0, 1.0),
            a as f32 / 255.0
        ));
    }
    defs.push_str("</linearGradient></defs>");
    doc.push(&defs);
    doc.push(&format!(
        r#"<rect x="{}" y="{}" width="{}" height="{}" fill="url(#{id})"/>"#,
        trim(x),
        trim(y),
        trim(w),
        trim(h),
    ));
}

/// Compute a linear-gradient line `(x1, y1, x2, y2)` in `objectBoundingBox`
/// space (unit box, Y-down) from a CSS direction.
///
/// Mirrors `background.rs::draw_linear_gradient`: the CSS angle is radians,
/// `0 = "to top"`, increasing clockwise. In Y-down space the direction is
/// `(sin a, -cos a)`; centering on `(0.5, 0.5)` with half-length 0.5 gives the
/// endpoints below. Sanity checks: `a = π` (to bottom, CSS default) →
/// `(0.5, 0)→(0.5, 1)` (top→bottom, matching the old hardcoded line);
/// `a = π/2` (to right) → `(0, 0.5)→(1, 0.5)` (left→right).
///
/// `Corner` directions are approximated by their nominal 45° angle on a unit
/// (square) box; the exact CSS corner angle depends on the box aspect ratio
/// (CSS Images 3 §3.1.1) but is resolved at draw time in the PDF path.
fn gradient_line(direction: LinearGradientDirection) -> (f32, f32, f32, f32) {
    let a = match direction {
        LinearGradientDirection::Angle(a) => a,
        LinearGradientDirection::Corner(corner) => corner_angle_approx(corner),
    };
    let (s, c) = (a.sin(), a.cos());
    let x1 = 0.5 - 0.5 * s;
    let y1 = 0.5 + 0.5 * c;
    let x2 = 0.5 + 0.5 * s;
    let y2 = 0.5 - 0.5 * c;
    (x1, y1, x2, y2)
}

/// Nominal 45° angle (radians, CSS convention: `0 = "to top"`, clockwise) for
/// each corner direction, on a unit (square) box. This is the aspect-ratio-1
/// case of `background.rs::corner_to_angle_rad`.
fn corner_angle_approx(corner: LinearGradientCorner) -> f32 {
    use std::f32::consts::FRAC_PI_4;
    match corner {
        LinearGradientCorner::TopRight => FRAC_PI_4, // 45° (to top right)
        LinearGradientCorner::BottomRight => 3.0 * FRAC_PI_4, // 135° (to bottom right)
        LinearGradientCorner::BottomLeft => 5.0 * FRAC_PI_4, // 225° (to bottom left)
        LinearGradientCorner::TopLeft => 7.0 * FRAC_PI_4, // 315° (to top left)
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
        let style = BlockStyle {
            background_color: Some([10, 20, 30, 255]),
            border_color: [0, 0, 0, 255],
            border_widths: [2.0, 2.0, 2.0, 2.0],
            ..Default::default()
        };
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
    fn linear_gradient_layer_emits_gradient_def() {
        use crate::draw_primitives::{
            BackgroundLayer, BgBox, BgClip, BgImageContent, BgLengthPercentage, BgRepeat, BgSize,
            GradientStop, GradientStopPosition, LinearGradientDirection,
        };
        let layer = BackgroundLayer {
            content: BgImageContent::LinearGradient {
                direction: LinearGradientDirection::Angle(0.0),
                stops: vec![
                    GradientStop {
                        position: GradientStopPosition::Fraction(0.0),
                        rgba: [255, 0, 0, 255],
                        is_hint: false,
                    },
                    GradientStop {
                        position: GradientStopPosition::Fraction(1.0),
                        rgba: [0, 0, 255, 255],
                        is_hint: false,
                    },
                ],
                repeating: false,
            },
            intrinsic_width: 0.0,
            intrinsic_height: 0.0,
            size: BgSize::Auto,
            position_x: BgLengthPercentage::Length(0.0),
            position_y: BgLengthPercentage::Length(0.0),
            repeat_x: BgRepeat::NoRepeat,
            repeat_y: BgRepeat::NoRepeat,
            origin: BgBox::PaddingBox,
            clip: BgClip::PaddingBox,
        };
        let mut doc = SvgDoc::new(100, 100, Background::Transparent);
        emit_background_layer(&mut doc, &layer, 0.0, 0.0, 80.0, 60.0, "grad-0-0");
        let svg = doc.finish();
        assert!(svg.contains("<linearGradient"));
        assert!(svg.contains("stop-color=\"rgb(255,0,0)\""));
        assert!(svg.contains("url(#grad-0-0)"));
    }

    #[test]
    fn linear_gradient_honors_horizontal_direction() {
        use crate::draw_primitives::{
            BackgroundLayer, BgBox, BgClip, BgImageContent, BgLengthPercentage, BgRepeat, BgSize,
            GradientStop, GradientStopPosition, LinearGradientDirection,
        };
        // Angle(π/2) is "to right" → horizontal gradient: (0,0.5)→(1,0.5).
        let layer = BackgroundLayer {
            content: BgImageContent::LinearGradient {
                direction: LinearGradientDirection::Angle(std::f32::consts::FRAC_PI_2),
                stops: vec![
                    GradientStop {
                        position: GradientStopPosition::Fraction(0.0),
                        rgba: [255, 0, 0, 255],
                        is_hint: false,
                    },
                    GradientStop {
                        position: GradientStopPosition::Fraction(1.0),
                        rgba: [0, 0, 255, 255],
                        is_hint: false,
                    },
                ],
                repeating: false,
            },
            intrinsic_width: 0.0,
            intrinsic_height: 0.0,
            size: BgSize::Auto,
            position_x: BgLengthPercentage::Length(0.0),
            position_y: BgLengthPercentage::Length(0.0),
            repeat_x: BgRepeat::NoRepeat,
            repeat_y: BgRepeat::NoRepeat,
            origin: BgBox::PaddingBox,
            clip: BgClip::PaddingBox,
        };
        let mut doc = SvgDoc::new(100, 100, Background::Transparent);
        emit_background_layer(&mut doc, &layer, 0.0, 0.0, 80.0, 60.0, "grad-0-0");
        let svg = doc.finish();
        assert!(svg.contains("<linearGradient"));
        // Horizontal: x1=0, x2=1, y1=y2=0.5 — NOT the old vertical form.
        assert!(svg.contains(r#"x1="0.0000""#), "svg: {svg}");
        assert!(svg.contains(r#"x2="1.0000""#), "svg: {svg}");
        assert!(svg.contains(r#"y1="0.5000""#), "svg: {svg}");
        assert!(svg.contains(r#"y2="0.5000""#), "svg: {svg}");
        // Make sure it is not the old hardcoded vertical line.
        assert!(!svg.contains(r#"x1="0" y1="0" x2="0" y2="1""#));
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
