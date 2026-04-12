//! ParagraphPageable — renders text via the Parley→Krilla glyph bridge.

use std::sync::Arc;

use skrifa::MetadataProvider;

use crate::image::ImageFormat;
use crate::pageable::{Canvas, Pageable, Pagination, Pt, Size};

/// Which decoration lines to draw (bitflags).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextDecorationLine(u8);

impl TextDecorationLine {
    pub const NONE: Self = Self(0);
    pub const UNDERLINE: Self = Self(1 << 0);
    pub const OVERLINE: Self = Self(1 << 1);
    pub const LINE_THROUGH: Self = Self(1 << 2);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for TextDecorationLine {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Visual style of the decoration line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextDecorationStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
    Double,
    Wavy,
}

/// All text-decoration info for a glyph run.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextDecoration {
    pub line: TextDecorationLine,
    pub style: TextDecorationStyle,
    pub color: [u8; 4],
}

impl TextDecoration {
    /// Check if two decorations have the same visual appearance.
    fn same_appearance(&self, other: &TextDecoration) -> bool {
        self.line == other.line && self.style == other.style && self.color == other.color
    }
}

/// A pre-extracted glyph for rendering via Krilla.
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    pub id: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub text_range: std::ops::Range<usize>,
}

/// A pre-extracted glyph run (single font + style).
#[derive(Clone, Debug)]
pub struct ShapedGlyphRun {
    pub font_data: Arc<Vec<u8>>,
    pub font_index: u32,
    pub font_size: f32,
    pub color: [u8; 4], // RGBA
    pub decoration: TextDecoration,
    pub glyphs: Vec<ShapedGlyph>,
    pub text: String,
    pub x_offset: f32,
}

/// Vertical alignment for inline replaced elements (images).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum VerticalAlign {
    #[default]
    Baseline,
    Middle,
    Top,
    Bottom,
    Sub,
    Super,
    TextTop,
    TextBottom,
    Length(f32),
    Percent(f32),
}

/// An inline image run within a shaped line.
#[derive(Clone, Debug)]
pub struct InlineImage {
    pub data: Arc<Vec<u8>>,
    pub format: ImageFormat,
    pub width: f32,
    pub height: f32,
    pub x_offset: f32,
    pub vertical_align: VerticalAlign,
    pub opacity: f32,
    pub visible: bool,
    /// Y position relative to line top, computed by recalculate_line_box.
    pub computed_y: f32,
}

/// A single item in a shaped line: either a text glyph run or an inline image.
#[derive(Clone, Debug)]
pub enum LineItem {
    Text(ShapedGlyphRun),
    Image(InlineImage),
}

/// A shaped line of text.
#[derive(Clone)]
pub struct ShapedLine {
    pub height: f32,
    /// Absolute offset from the paragraph's top edge to this line's baseline (from Parley).
    pub baseline: f32,
    pub items: Vec<LineItem>,
}

/// Paragraph element that renders shaped text.
#[derive(Clone)]
pub struct ParagraphPageable {
    pub lines: Vec<ShapedLine>,
    pub pagination: Pagination,
    pub cached_height: f32,
    pub opacity: f32,
    pub visible: bool,
}

impl ParagraphPageable {
    pub fn new(lines: Vec<ShapedLine>) -> Self {
        let cached_height: f32 = lines.iter().map(|l| l.height).sum();
        Self {
            lines,
            pagination: Pagination::default(),
            cached_height,
            opacity: 1.0,
            visible: true,
        }
    }
}

/// Font metrics for decoration line positioning.
struct DecorationMetrics {
    underline_offset: f32,
    underline_thickness: f32,
    strikethrough_offset: f32,
    strikethrough_thickness: f32,
    /// Position for overline (cap_height or approximation)
    overline_pos: f32,
}

fn get_decoration_metrics(font_data: &[u8], font_index: u32, font_size: f32) -> DecorationMetrics {
    let fallback_thickness = font_size * 0.05;

    if let Ok(font_ref) = skrifa::FontRef::from_index(font_data, font_index) {
        let metrics = font_ref.metrics(
            skrifa::instance::Size::new(font_size),
            skrifa::instance::LocationRef::default(),
        );
        let underline = metrics.underline.unwrap_or(skrifa::metrics::Decoration {
            offset: -font_size * 0.1,
            thickness: fallback_thickness,
        });
        let strikeout = metrics.strikeout.unwrap_or(skrifa::metrics::Decoration {
            offset: font_size * 0.3,
            thickness: fallback_thickness,
        });
        // skrifa underline.offset is very small for some fonts (e.g. -0.23 for 12pt).
        // Use a minimum offset based on font size to ensure underline is visually distinct
        // but not too far from the baseline.
        let min_underline_offset = font_size * 0.075;
        let underline_offset = (-underline.offset).max(min_underline_offset);

        // strikethrough should be at ~40% of ascent (x-height center area).
        // Some fonts report it too low; clamp to reasonable range.
        let strikethrough_offset = strikeout.offset.max(metrics.ascent * 0.35);

        // overline: use cap_height if available, otherwise 90% of ascent
        let overline_pos = metrics.cap_height.unwrap_or(metrics.ascent * 0.9);

        // Guard against zero thickness (some fonts report 0 in OS/2 table)
        let min_thickness = font_size * 0.02;

        DecorationMetrics {
            underline_offset,
            underline_thickness: underline.thickness.max(min_thickness),
            strikethrough_offset,
            strikethrough_thickness: strikeout.thickness.max(min_thickness),
            overline_pos,
        }
    } else {
        DecorationMetrics {
            underline_offset: font_size * 0.075,
            underline_thickness: fallback_thickness,
            strikethrough_offset: font_size * 0.3,
            strikethrough_thickness: fallback_thickness,
            overline_pos: font_size * 0.7,
        }
    }
}

/// Draw a straight line with the given stroke (shared by Solid, Dashed, Dotted).
fn draw_straight_line(
    canvas: &mut Canvas<'_, '_>,
    x: f32,
    y: f32,
    width: f32,
    stroke: krilla::paint::Stroke,
) {
    canvas.surface.set_fill(None);
    canvas.surface.set_stroke(Some(stroke));
    let mut pb = krilla::geom::PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x + width, y);
    if let Some(path) = pb.finish() {
        canvas.surface.draw_path(&path);
    }
}

fn draw_decoration_line(
    canvas: &mut Canvas<'_, '_>,
    x: f32,
    y: f32,
    width: f32,
    thickness: f32,
    color: [u8; 4],
    style: TextDecorationStyle,
) {
    let paint: krilla::paint::Paint =
        krilla::color::rgb::Color::new(color[0], color[1], color[2]).into();
    let opacity = krilla::num::NormalizedF32::new(color[3] as f32 / 255.0)
        .unwrap_or(krilla::num::NormalizedF32::ONE);

    match style {
        TextDecorationStyle::Solid => {
            draw_straight_line(
                canvas,
                x,
                y,
                width,
                krilla::paint::Stroke {
                    paint,
                    width: thickness,
                    opacity,
                    ..Default::default()
                },
            );
        }
        TextDecorationStyle::Dashed => {
            let dash_len = thickness * 3.0;
            draw_straight_line(
                canvas,
                x,
                y,
                width,
                krilla::paint::Stroke {
                    paint,
                    width: thickness,
                    opacity,
                    dash: Some(krilla::paint::StrokeDash {
                        array: vec![dash_len, dash_len],
                        offset: 0.0,
                    }),
                    ..Default::default()
                },
            );
        }
        TextDecorationStyle::Dotted => {
            let dot_spacing = thickness * 2.0;
            draw_straight_line(
                canvas,
                x,
                y,
                width,
                krilla::paint::Stroke {
                    paint,
                    width: thickness,
                    opacity,
                    line_cap: krilla::paint::LineCap::Round,
                    dash: Some(krilla::paint::StrokeDash {
                        array: vec![0.0, dot_spacing],
                        offset: 0.0,
                    }),
                    ..Default::default()
                },
            );
        }
        TextDecorationStyle::Double => {
            let gap = thickness * 1.5;
            let stroke = krilla::paint::Stroke {
                paint,
                width: thickness,
                opacity,
                ..Default::default()
            };
            draw_straight_line(canvas, x, y - gap / 2.0, width, stroke.clone());
            draw_straight_line(canvas, x, y + gap / 2.0, width, stroke);
        }
        TextDecorationStyle::Wavy => {
            let amplitude = thickness * 1.5;
            let wavelength = thickness * 4.0;
            let half = wavelength / 2.0;

            // Guard against zero/tiny wavelength to prevent infinite loop
            if half < 0.01 {
                draw_straight_line(
                    canvas,
                    x,
                    y,
                    width,
                    krilla::paint::Stroke {
                        paint,
                        width: thickness,
                        opacity,
                        ..Default::default()
                    },
                );
            } else {
                let stroke = krilla::paint::Stroke {
                    paint,
                    width: thickness,
                    opacity,
                    ..Default::default()
                };
                canvas.surface.set_fill(None);
                canvas.surface.set_stroke(Some(stroke));
                let mut pb = krilla::geom::PathBuilder::new();
                pb.move_to(x, y);
                let mut cx = x;
                let mut up = true;
                while cx < x + width {
                    let end_x = (cx + half).min(x + width);
                    let segment = end_x - cx;
                    let dy = if up { -amplitude } else { amplitude };
                    pb.cubic_to(
                        cx + segment * 0.33,
                        y + dy,
                        cx + segment * 0.67,
                        y + dy,
                        end_x,
                        y,
                    );
                    cx = end_x;
                    up = !up;
                }
                if let Some(path) = pb.finish() {
                    canvas.surface.draw_path(&path);
                }
            }
        }
    }
    canvas.surface.set_stroke(None);
}

/// A contiguous span of runs sharing the same decoration attributes.
struct DecorationSpan {
    x: f32,
    width: f32,
    decoration: TextDecoration,
    /// Use metrics from the first run in the span
    font_data: Arc<Vec<u8>>,
    font_index: u32,
    font_size: f32,
}

/// Collect contiguous runs with the same decoration into spans, then draw each span once.
fn draw_line_decorations(canvas: &mut Canvas<'_, '_>, items: &[LineItem], x: Pt, baseline_y: Pt) {
    let mut spans: Vec<DecorationSpan> = Vec::new();

    for item in items {
        let run = match item {
            LineItem::Text(run) => run,
            LineItem::Image(_) => continue,
        };
        if run.decoration.line.is_none() {
            continue;
        }

        let run_x = x + run.x_offset;
        let run_width: f32 = run.glyphs.iter().map(|g| g.x_advance * run.font_size).sum();

        // Try to extend the previous span if decoration matches
        if let Some(last) = spans.last_mut() {
            let last_end = last.x + last.width;
            let gap = (run_x - last_end).abs();
            if last.decoration.same_appearance(&run.decoration) && gap < 0.5 {
                last.width = (run_x + run_width) - last.x;
                continue;
            }
        }

        spans.push(DecorationSpan {
            x: run_x,
            width: run_width,
            decoration: run.decoration,
            font_data: Arc::clone(&run.font_data),
            font_index: run.font_index,
            font_size: run.font_size,
        });
    }

    for span in &spans {
        let metrics = get_decoration_metrics(&span.font_data, span.font_index, span.font_size);

        if span.decoration.line.contains(TextDecorationLine::UNDERLINE) {
            let line_y = baseline_y + metrics.underline_offset;
            draw_decoration_line(
                canvas,
                span.x,
                line_y,
                span.width,
                metrics.underline_thickness,
                span.decoration.color,
                span.decoration.style,
            );
        }
        if span.decoration.line.contains(TextDecorationLine::OVERLINE) {
            let line_y = baseline_y - metrics.overline_pos;
            draw_decoration_line(
                canvas,
                span.x,
                line_y,
                span.width,
                metrics.underline_thickness,
                span.decoration.color,
                span.decoration.style,
            );
        }
        if span
            .decoration
            .line
            .contains(TextDecorationLine::LINE_THROUGH)
        {
            let line_y = baseline_y - metrics.strikethrough_offset;
            draw_decoration_line(
                canvas,
                span.x,
                line_y,
                span.width,
                metrics.strikethrough_thickness,
                span.decoration.color,
                span.decoration.style,
            );
        }
    }
}

/// Draw pre-shaped text lines at the given position.
pub fn draw_shaped_lines(canvas: &mut Canvas<'_, '_>, lines: &[ShapedLine], x: Pt, y: Pt) {
    for line in lines {
        let baseline_y = y + line.baseline;

        for item in &line.items {
            match item {
                LineItem::Text(run) => {
                    // Create Krilla font from cached data
                    let data: krilla::Data = Arc::clone(&run.font_data).into();
                    let Some(font) = krilla::text::Font::new(data, run.font_index) else {
                        continue;
                    };

                    // Convert shaped glyphs to Krilla glyphs
                    // Values are already normalized (/ font_size) in convert.rs
                    let krilla_glyphs: Vec<krilla::text::KrillaGlyph> = run
                        .glyphs
                        .iter()
                        .map(|g| krilla::text::KrillaGlyph {
                            glyph_id: krilla::text::GlyphId::new(g.id),
                            text_range: g.text_range.clone(),
                            x_advance: g.x_advance,
                            x_offset: g.x_offset,
                            y_offset: g.y_offset,
                            y_advance: 0.0,
                            location: None,
                        })
                        .collect();

                    if krilla_glyphs.is_empty() {
                        continue;
                    }

                    // Set text color
                    let fill = krilla::paint::Fill {
                        paint: krilla::color::rgb::Color::new(
                            run.color[0],
                            run.color[1],
                            run.color[2],
                        )
                        .into(),
                        opacity: krilla::num::NormalizedF32::new(run.color[3] as f32 / 255.0)
                            .unwrap_or(krilla::num::NormalizedF32::ONE),
                        rule: Default::default(),
                    };
                    canvas.surface.set_fill(Some(fill));

                    let start = krilla::geom::Point::from_xy(x + run.x_offset, baseline_y);
                    canvas.surface.draw_glyphs(
                        start,
                        &krilla_glyphs,
                        font,
                        &run.text,
                        run.font_size,
                        false,
                    );
                }
                LineItem::Image(img) => {
                    if !img.visible {
                        continue;
                    }
                    crate::pageable::draw_with_opacity(canvas, img.opacity, |canvas| {
                        let data: krilla::Data = Arc::clone(&img.data).into();
                        let Ok(image) = img.format.to_krilla_image(data) else {
                            return;
                        };
                        let Some(size) =
                            krilla::geom::Size::from_wh(img.width, img.height)
                        else {
                            return;
                        };
                        let img_y = y + img.computed_y;
                        let transform = krilla::geom::Transform::from_translate(
                            x + img.x_offset,
                            img_y,
                        );
                        canvas.surface.push_transform(&transform);
                        canvas.surface.draw_image(image, size);
                        canvas.surface.pop();
                    });
                }
            }
        }

        // Draw decorations after all glyphs so lines appear on top
        draw_line_decorations(canvas, &line.items, x, baseline_y);
    }
}

/// Font metrics used by `recalculate_line_box` to position inline images
/// relative to the text baseline.
#[derive(Clone, Debug)]
pub struct LineFontMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub x_height: f32,
    pub subscript_offset: f32,
    pub superscript_offset: f32,
}

/// Recalculate the line box height and baseline after inline images have been
/// injected. Each image's `computed_y` (relative to the new line top) is set
/// here; `draw_shaped_lines` uses it directly.
///
/// The algorithm:
///
/// 1. Start with the existing line box `[0, height)` from text metrics.
/// 2. For each image (except `Top`/`Bottom`), compute `img_top` relative to
///    the original coordinate system (0 = line top before expansion).
/// 3. Expand `line_top` / `line_bottom` if the image overflows.
/// 4. Handle `Top` and `Bottom` images (they align to the final edges).
/// 5. Update `line.height`, `line.baseline`, and each image's `computed_y`.
pub fn recalculate_line_box(line: &mut ShapedLine, metrics: &LineFontMetrics) {
    let original_height = line.height;
    let baseline = line.baseline;

    let mut line_top: f32 = 0.0;
    let mut line_bottom: f32 = original_height;

    // Phase 1: compute img_top for flow-aligned images and expand line box.
    // Store (index, img_top) for later computed_y assignment.
    let mut positions: Vec<(usize, f32)> = Vec::new();

    for (idx, item) in line.items.iter().enumerate() {
        let img = match item {
            LineItem::Image(img) => img,
            LineItem::Text(_) => continue,
        };

        let img_top = match img.vertical_align {
            VerticalAlign::Top | VerticalAlign::Bottom => {
                // Deferred to phase 2
                continue;
            }
            VerticalAlign::Baseline => baseline - img.height,
            VerticalAlign::Middle => {
                baseline - metrics.x_height / 2.0 - img.height / 2.0
            }
            VerticalAlign::Sub => {
                baseline + metrics.subscript_offset - img.height
            }
            VerticalAlign::Super => {
                baseline - metrics.superscript_offset - img.height
            }
            VerticalAlign::TextTop => baseline - metrics.ascent,
            VerticalAlign::TextBottom => {
                baseline + metrics.descent - img.height
            }
            VerticalAlign::Length(v) => baseline - v - img.height,
            VerticalAlign::Percent(p) => {
                baseline - (original_height * p) - img.height
            }
        };

        if img_top < line_top {
            line_top = img_top;
        }
        if img_top + img.height > line_bottom {
            line_bottom = img_top + img.height;
        }
        positions.push((idx, img_top));
    }

    // Phase 2: Top / Bottom images use the (possibly expanded) line box.
    for (idx, item) in line.items.iter().enumerate() {
        let img = match item {
            LineItem::Image(img) => img,
            LineItem::Text(_) => continue,
        };
        let img_top = match img.vertical_align {
            VerticalAlign::Top => line_top,
            VerticalAlign::Bottom => line_bottom - img.height,
            _ => continue,
        };
        if img_top < line_top {
            line_top = img_top;
        }
        if img_top + img.height > line_bottom {
            line_bottom = img_top + img.height;
        }
        positions.push((idx, img_top));
    }

    // Phase 3: Apply — shift everything so line_top becomes 0.
    let shift = -line_top;
    line.height = line_bottom - line_top;
    line.baseline = baseline + shift;

    for (idx, img_top) in positions {
        if let LineItem::Image(img) = &mut line.items[idx] {
            img.computed_y = img_top + shift;
        }
    }
}

impl Pageable for ParagraphPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        self.cached_height = self.lines.iter().map(|l| l.height).sum();
        Size {
            width: _avail_width,
            height: self.cached_height,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        if self.lines.len() <= 1 {
            return None;
        }

        let orphans = self.pagination.orphans;
        let widows = self.pagination.widows;

        // Find the split point
        let mut consumed: f32 = 0.0;
        let mut split_at = 0;
        for (i, line) in self.lines.iter().enumerate() {
            if consumed + line.height > avail_height {
                split_at = i;
                break;
            }
            consumed += line.height;
            split_at = i + 1;
        }

        if split_at == 0 || split_at >= self.lines.len() {
            return None;
        }

        // Enforce orphans/widows
        if split_at < orphans {
            return None;
        }
        if self.lines.len() - split_at < widows {
            let adjusted = self.lines.len().saturating_sub(widows);
            if adjusted < orphans || adjusted == 0 {
                return None;
            }
            // split_at = adjusted; -- would break orphan rule
        }

        let mut first = ParagraphPageable::new(self.lines[..split_at].to_vec());
        first.opacity = self.opacity;
        first.visible = self.visible;

        // Rebase second fragment: baseline is absolute from paragraph top,
        // so subtract the consumed height to make it relative to the new fragment.
        let second_lines: Vec<ShapedLine> = self.lines[split_at..]
            .iter()
            .cloned()
            .map(|mut line| {
                line.baseline -= consumed;
                line
            })
            .collect();
        let mut second = ParagraphPageable::new(second_lines);
        second.opacity = self.opacity;
        second.visible = self.visible;

        Some((Box::new(first), Box::new(second)))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, _avail_width: Pt, _avail_height: Pt) {
        if !self.visible {
            return;
        }
        crate::pageable::draw_with_opacity(canvas, self.opacity, |canvas| {
            draw_shaped_lines(canvas, &self.lines, x, y);
        });
    }

    fn pagination(&self) -> Pagination {
        self.pagination
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.cached_height
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageFormat;

    /// Minimal 1x1 red PNG for test images.
    const TEST_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
        0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78,
        0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92,
        0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn make_inline_image(width: f32, height: f32, va: VerticalAlign) -> InlineImage {
        InlineImage {
            data: Arc::new(TEST_PNG.to_vec()),
            format: ImageFormat::Png,
            width,
            height,
            x_offset: 0.0,
            vertical_align: va,
            opacity: 1.0,
            visible: true,
            computed_y: 0.0,
        }
    }

    fn default_metrics() -> LineFontMetrics {
        LineFontMetrics {
            ascent: 12.0,
            descent: 4.0,
            x_height: 8.0,
            subscript_offset: 4.0,
            superscript_offset: 6.0,
        }
    }

    /// A text-only line: height=16, baseline=12.
    fn text_line(height: f32, baseline: f32) -> ShapedLine {
        ShapedLine {
            height,
            baseline,
            items: Vec::new(),
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    // ---------- Baseline ----------

    #[test]
    fn baseline_image_within_line_no_expansion() {
        // 8px image at baseline: img_top = 12 - 8 = 4, img_bottom = 12.
        // Line is [0, 16) so no expansion needed.
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 8.0, VerticalAlign::Baseline)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        assert!(approx(line.height, 16.0), "height={}", line.height);
        assert!(approx(line.baseline, 12.0), "baseline={}", line.baseline);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 4.0), "computed_y={}", img.computed_y);
        }
    }

    #[test]
    fn baseline_image_taller_expands_line() {
        // 20px image at baseline: img_top = 12 - 20 = -8, img_bottom = 12.
        // line_top shifts to -8 → new height = 24, baseline = 20.
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 20.0, VerticalAlign::Baseline)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        assert!(approx(line.height, 24.0), "height={}", line.height);
        assert!(approx(line.baseline, 20.0), "baseline={}", line.baseline);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 0.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- Middle ----------

    #[test]
    fn middle_alignment() {
        // img_top = baseline - x_height/2 - img.height/2 = 12 - 4 - 5 = 3
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 10.0, VerticalAlign::Middle)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        assert!(approx(line.height, 16.0), "height={}", line.height);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 3.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- Sub ----------

    #[test]
    fn sub_alignment() {
        // img_top = baseline + subscript_offset - img.height = 12 + 4 - 6 = 10
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 6.0, VerticalAlign::Sub)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 10.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- Super ----------

    #[test]
    fn super_alignment() {
        // img_top = baseline - superscript_offset - img.height = 12 - 6 - 6 = 0
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 6.0, VerticalAlign::Super)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 0.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- TextTop ----------

    #[test]
    fn text_top_alignment() {
        // img_top = baseline - ascent = 12 - 12 = 0
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 8.0, VerticalAlign::TextTop)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 0.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- TextBottom ----------

    #[test]
    fn text_bottom_alignment() {
        // img_top = baseline + descent - img.height = 12 + 4 - 8 = 8
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 8.0, VerticalAlign::TextBottom)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 8.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- Top ----------

    #[test]
    fn top_alignment_uses_line_top() {
        // Top image aligns to the top of the line box.
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 8.0, VerticalAlign::Top)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 0.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- Bottom ----------

    #[test]
    fn bottom_alignment_uses_line_bottom() {
        // Bottom image aligns to the bottom of the line box.
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 8.0, VerticalAlign::Bottom)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(
                approx(img.computed_y, line.height - 8.0),
                "computed_y={}",
                img.computed_y,
            );
        }
    }

    // ---------- Length ----------

    #[test]
    fn length_offset() {
        // img_top = baseline - v - img.height = 12 - 3.0 - 6 = 3
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 6.0, VerticalAlign::Length(3.0))));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 3.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- Percent ----------

    #[test]
    fn percent_offset() {
        // img_top = baseline - (height * p) - img.height = 12 - (16 * 0.25) - 6 = 2
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 6.0, VerticalAlign::Percent(0.25))));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 2.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- Top image expands downward ----------

    #[test]
    fn top_image_taller_than_line_expands() {
        // 20px Top image on a 16px line → line grows to 20.
        let mut line = text_line(16.0, 12.0);
        line.items
            .push(LineItem::Image(make_inline_image(10.0, 20.0, VerticalAlign::Top)));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        assert!(approx(line.height, 20.0), "height={}", line.height);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 0.0), "computed_y={}", img.computed_y);
        }
    }
}
