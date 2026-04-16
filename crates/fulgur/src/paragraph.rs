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

/// Target for a clickable link in PDF output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    External(Arc<String>),
    Internal(Arc<String>),
}

/// Link association attached to a glyph run or inline image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub target: LinkTarget,
    pub alt_text: Option<String>,
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
    pub link: Option<Arc<LinkSpan>>,
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
    pub link: Option<Arc<LinkSpan>>,
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
    /// HTML `id` attribute of the inline-root element this paragraph was
    /// extracted from. Used by `DestinationRegistry` to resolve `#anchor`
    /// links targeting headings (`<h1 id=..>`) and similar inline-root
    /// elements that do not gain a `BlockPageable` wrapper.
    pub id: Option<Arc<String>>,
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
            id: None,
        }
    }

    /// Attach an `id` anchor to this paragraph. Chain after `new()`.
    pub fn with_id(mut self, id: Option<Arc<String>>) -> Self {
        self.id = id;
        self
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
    // Track the top edge of each line within the paragraph (paragraph y=0 at
    // `y`). Lines pack tightly by `line.height`. We use the full line box for
    // link activation rects (matches WeasyPrint behavior), so tracking the
    // top via cumulative height is both simpler and more robust than trying
    // to derive it from `line.baseline` (which is an absolute baseline offset
    // and does not carry per-line ascent).
    let mut line_top: f32 = 0.0;
    for line in lines {
        let line_top_abs = y + line_top;
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

                    // After the glyphs are drawn, record a link rect if this
                    // run was emitted under an <a href>. Width mirrors the
                    // decoration-span computation in `draw_line_decorations`
                    // (same glyph advance accumulator); height uses the full
                    // line box so the hit area is stable across lines.
                    if let Some(link_span) = run.link.as_ref() {
                        let run_width: f32 =
                            run.glyphs.iter().map(|g| g.x_advance * run.font_size).sum();
                        let rect = crate::pageable::Rect {
                            x: x + run.x_offset,
                            y: line_top_abs,
                            width: run_width.max(0.0),
                            height: line.height,
                        };
                        if let Some(collector) = canvas.link_collector.as_deref_mut() {
                            collector.push_rect(link_span, rect);
                        }
                    }
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
                        let Some(size) = krilla::geom::Size::from_wh(img.width, img.height) else {
                            return;
                        };
                        let img_y = y + img.computed_y;
                        let transform =
                            krilla::geom::Transform::from_translate(x + img.x_offset, img_y);
                        canvas.surface.push_transform(&transform);
                        canvas.surface.draw_image(image, size);
                        canvas.surface.pop();
                    });

                    // Record a rect for this image if it sits under an <a>.
                    // Matches the image's drawn coordinates exactly:
                    // (x + x_offset, y + computed_y, width, height).
                    if let Some(link_span) = img.link.as_ref() {
                        let rect = crate::pageable::Rect {
                            x: x + img.x_offset,
                            y: y + img.computed_y,
                            width: img.width.max(0.0),
                            height: img.height.max(0.0),
                        };
                        if let Some(collector) = canvas.link_collector.as_deref_mut() {
                            collector.push_rect(link_span, rect);
                        }
                    }
                }
            }
        }

        // Draw decorations after all glyphs so lines appear on top
        draw_line_decorations(canvas, &line.items, x, baseline_y);

        line_top += line.height;
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
            VerticalAlign::Middle => baseline - metrics.x_height / 2.0 - img.height / 2.0,
            VerticalAlign::Sub => baseline + metrics.subscript_offset - img.height,
            VerticalAlign::Super => baseline - metrics.superscript_offset - img.height,
            VerticalAlign::TextTop => baseline - metrics.ascent,
            VerticalAlign::TextBottom => baseline + metrics.descent - img.height,
            VerticalAlign::Length(v) => baseline - v - img.height,
            VerticalAlign::Percent(p) => baseline - (original_height * p) - img.height,
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
        first.id = self.id.clone();

        // Rebase second fragment: baseline is absolute from paragraph top,
        // so subtract the consumed height to make it relative to the new fragment.
        let second_lines: Vec<ShapedLine> = self.lines[split_at..]
            .iter()
            .cloned()
            .map(|mut line| {
                line.baseline -= consumed;
                // Rebase inline image positions for the new fragment:
                // computed_y is paragraph-absolute (like baseline), so it
                // must also be shifted by the consumed height.
                for item in &mut line.items {
                    if let LineItem::Image(img) = item {
                        img.computed_y -= consumed;
                    }
                }
                line
            })
            .collect();
        let mut second = ParagraphPageable::new(second_lines);
        second.opacity = self.opacity;
        second.visible = self.visible;
        // Both fragments inherit the id. `DestinationRegistry::record` is
        // first-write-wins, so the second fragment's entry is a no-op when
        // it lands on a later page — we just carry the id so ordering quirks
        // don't drop anchors.
        second.id = self.id.clone();

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

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        _avail_width: Pt,
        _avail_height: Pt,
        registry: &mut crate::pageable::DestinationRegistry,
    ) {
        if let Some(id) = &self.id
            && !id.is_empty()
        {
            registry.record(id, x, y);
        }
        // No child recursion: paragraph lines are glyph data, not Pageables.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageFormat;

    /// Minimal 1x1 red PNG for test images.
    const TEST_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
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
            link: None,
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            8.0,
            VerticalAlign::Baseline,
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            20.0,
            VerticalAlign::Baseline,
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            10.0,
            VerticalAlign::Middle,
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            6.0,
            VerticalAlign::Sub,
        )));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(
                approx(img.computed_y, 10.0),
                "computed_y={}",
                img.computed_y
            );
        }
    }

    // ---------- Super ----------

    #[test]
    fn super_alignment() {
        // img_top = baseline - superscript_offset - img.height = 12 - 6 - 6 = 0
        let mut line = text_line(16.0, 12.0);
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            6.0,
            VerticalAlign::Super,
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            8.0,
            VerticalAlign::TextTop,
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            8.0,
            VerticalAlign::TextBottom,
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            8.0,
            VerticalAlign::Top,
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            8.0,
            VerticalAlign::Bottom,
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            6.0,
            VerticalAlign::Length(3.0),
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            6.0,
            VerticalAlign::Percent(0.25),
        )));
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
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            20.0,
            VerticalAlign::Top,
        )));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        assert!(approx(line.height, 20.0), "height={}", line.height);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(approx(img.computed_y, 0.0), "computed_y={}", img.computed_y);
        }
    }

    // ---------- Multi-line regression: coordinate system ----------

    #[test]
    fn multiline_second_line_image_no_inflation() {
        // Regression: recalculate_line_box assumes baseline is line-local.
        // For a second line with paragraph-absolute baseline=28, calling
        // recalculate_line_box directly would compute img_top = 28 - 8 = 20,
        // which is way outside [0, 16) and would incorrectly inflate height.
        //
        // The correct approach (used by recalculate_paragraph_line_boxes in
        // convert.rs) is to convert baseline to line-local first.
        let mut line2 = text_line(16.0, 28.0); // paragraph-absolute baseline
        line2.items.push(LineItem::Image(make_inline_image(
            10.0,
            8.0,
            VerticalAlign::Baseline,
        )));

        // Simulate the caller's coordinate conversion:
        let y_acc = 16.0; // first line height
        line2.baseline -= y_acc; // now line-local: 12.0
        let m = default_metrics();
        recalculate_line_box(&mut line2, &m);
        // Image fits within [0, 16): no expansion
        assert!(
            approx(line2.height, 16.0),
            "height should stay 16, got {}",
            line2.height
        );
        // Convert computed_y to paragraph-absolute
        if let LineItem::Image(img) = &mut line2.items[0] {
            img.computed_y += y_acc;
            // paragraph-absolute computed_y = line-local (4.0) + y_acc (16.0) = 20.0
            assert!(
                approx(img.computed_y, 20.0),
                "paragraph-absolute computed_y should be 20, got {}",
                img.computed_y
            );
        }
        line2.baseline += y_acc; // restore to paragraph-absolute: 28.0
        assert!(
            approx(line2.baseline, 28.0),
            "baseline should be 28, got {}",
            line2.baseline
        );
    }

    // ---------- Split with inline image ----------

    #[test]
    fn split_rebases_inline_image_computed_y() {
        // Four lines so orphans/widows (default=2) allow splitting at line 2.
        // Split after lines 1+2 (consumed=32), leaving lines 3+4 in second fragment.
        // Line4 has an image with paragraph-absolute computed_y.
        let line1 = text_line(16.0, 12.0);
        let line2 = text_line(16.0, 28.0);
        let line3 = text_line(16.0, 44.0);
        let mut line4 = text_line(16.0, 60.0);
        let mut img = make_inline_image(10.0, 8.0, VerticalAlign::Baseline);
        img.computed_y = 52.0; // paragraph-absolute (baseline 60 - img height 8)
        line4.items.push(LineItem::Image(img));

        let para = ParagraphPageable::new(vec![line1, line2, line3, line4]);

        // Split after line2: avail_height = 32 fits lines 1+2 exactly
        let (_first, second) = para.split(100.0, 32.0).expect("should split");

        // Second fragment has 2 lines (line3, line4), rebased by consumed=32
        let second_para = second.as_any().downcast_ref::<ParagraphPageable>().unwrap();
        assert_eq!(second_para.lines.len(), 2);
        let line = &second_para.lines[1]; // line4 in second fragment
        // baseline was 60, consumed=32 → rebased to 28
        assert!(
            approx(line.baseline, 28.0),
            "rebased baseline should be 28, got {}",
            line.baseline
        );
        if let LineItem::Image(img) = &line.items[0] {
            // computed_y was 52, consumed=32 → rebased to 20
            assert!(
                approx(img.computed_y, 20.0),
                "rebased computed_y should be 20, got {}",
                img.computed_y
            );
        } else {
            panic!("expected image item");
        }
    }

    // ---------- Id propagation ----------

    #[test]
    fn paragraph_default_has_no_id() {
        let p = ParagraphPageable::new(Vec::new());
        assert!(p.id.is_none());
    }

    #[test]
    fn paragraph_with_id_stores_value() {
        let p = ParagraphPageable::new(Vec::new()).with_id(Some(Arc::new("section-1".to_string())));
        assert_eq!(p.id.as_deref().map(String::as_str), Some("section-1"));
    }

    #[test]
    fn collect_ids_records_paragraph_id() {
        use crate::pageable::DestinationRegistry;
        let p = ParagraphPageable::new(Vec::new()).with_id(Some(Arc::new("anchor".to_string())));
        let mut reg = DestinationRegistry::default();
        reg.set_current_page(3);
        p.collect_ids(10.0, 42.0, 400.0, 600.0, &mut reg);
        assert_eq!(reg.get("anchor"), Some((3, 10.0, 42.0)));
    }

    #[test]
    fn collect_ids_is_noop_without_id() {
        use crate::pageable::DestinationRegistry;
        let p = ParagraphPageable::new(Vec::new());
        let mut reg = DestinationRegistry::default();
        p.collect_ids(0.0, 0.0, 400.0, 600.0, &mut reg);
        assert!(reg.get("anything").is_none());
    }

    #[test]
    fn split_propagates_id_to_both_fragments() {
        // Four lines so orphans/widows (default=2) allow splitting at line 2.
        let line1 = text_line(16.0, 12.0);
        let line2 = text_line(16.0, 28.0);
        let line3 = text_line(16.0, 44.0);
        let line4 = text_line(16.0, 60.0);
        let para = ParagraphPageable::new(vec![line1, line2, line3, line4])
            .with_id(Some(Arc::new("heading".to_string())));

        let (first, second) = para.split(100.0, 32.0).expect("should split");
        let first_para = first.as_any().downcast_ref::<ParagraphPageable>().unwrap();
        let second_para = second.as_any().downcast_ref::<ParagraphPageable>().unwrap();
        assert_eq!(
            first_para.id.as_deref().map(String::as_str),
            Some("heading")
        );
        assert_eq!(
            second_para.id.as_deref().map(String::as_str),
            Some("heading")
        );
    }
}

#[cfg(test)]
mod link_span_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn link_target_equality_is_by_value() {
        let a = LinkTarget::External(Arc::new("https://example.com".into()));
        let b = LinkTarget::External(Arc::new("https://example.com".into()));
        assert_eq!(a, b);
        let c = LinkTarget::Internal(Arc::new("section".into()));
        assert_ne!(a, c);
    }

    #[test]
    fn shaped_glyph_run_default_has_no_link() {
        let run = ShapedGlyphRun {
            font_data: Arc::new(Vec::new()),
            font_index: 0,
            font_size: 12.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: String::new(),
            x_offset: 0.0,
            link: None,
        };
        assert!(run.link.is_none());
    }
}

#[cfg(test)]
mod link_collect_tests {
    use super::*;
    use crate::pageable::{Canvas, LinkCollector};
    use std::collections::HashMap;

    fn render_pages_into_collector(html: &str, collector: &mut LinkCollector) -> usize {
        use crate::convert::{self, ConvertContext};
        use crate::gcpm::running::RunningElementStore;

        let doc = crate::blitz_adapter::parse_and_layout(html, 400.0, 600.0, &[]);
        let dummy_store = RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &dummy_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
        };
        let pageable = convert::dom_to_pageable(&doc, &mut ctx);
        let pages = crate::paginate::paginate(pageable, 400.0, 600.0);
        let page_count = pages.len();

        let mut krilla_doc = krilla::Document::new();
        for (idx, p) in pages.iter().enumerate() {
            let settings =
                krilla::page::PageSettings::from_wh(400.0, 600.0).expect("valid page dimensions");
            let mut page = krilla_doc.start_page_with(settings);
            let mut surface = page.surface();
            collector.set_current_page(idx);
            {
                let mut canvas = Canvas {
                    surface: &mut surface,
                    bookmark_collector: None,
                    link_collector: Some(collector),
                };
                p.draw(&mut canvas, 0.0, 0.0, 400.0, 600.0);
            }
        }
        // Drop the document — we only care about the collector side effect.
        let _ = krilla_doc.finish();
        page_count
    }

    #[test]
    fn draw_pushes_link_rect_for_glyph_run_inside_anchor() {
        let html = r#"<html><body><p><a href="https://x.test">hello world</a></p></body></html>"#;
        let mut collector = LinkCollector::new();
        render_pages_into_collector(html, &mut collector);

        let occs = collector.into_occurrences();
        assert!(!occs.is_empty(), "expected at least one link occurrence");
        let first = &occs[0];
        assert_eq!(first.page_idx, 0, "link should land on page 0");
        assert!(
            matches!(&first.target, LinkTarget::External(u) if u.as_str() == "https://x.test"),
            "unexpected target: {:?}",
            first.target,
        );
        assert!(!first.quads.is_empty());
        // Width: BR.x - BL.x; Height: BL.y - TL.y (axis-aligned, no transform)
        let q = &first.quads[0];
        let width = q.points[1][0] - q.points[0][0];
        let height = q.points[0][1] - q.points[3][1];
        assert!(width > 0.0, "expected positive quad width, got {}", width,);
        assert!(
            height > 0.0,
            "expected positive quad height, got {}",
            height,
        );
    }

    #[test]
    fn draw_merges_multiple_glyph_runs_under_same_anchor_into_one_occurrence() {
        // <em> inside <a> forces shaping to split into multiple glyph runs,
        // but convert.rs clones the same Arc<LinkSpan> onto every run — so
        // LinkCollector's Arc-pointer dedup should merge them into one
        // occurrence with multiple rects.
        let html =
            r#"<html><body><p><a href="https://x.test"><em>foo</em>bar</a></p></body></html>"#;
        let mut collector = LinkCollector::new();
        render_pages_into_collector(html, &mut collector);

        let occs = collector.into_occurrences();
        assert_eq!(
            occs.len(),
            1,
            "expected one occurrence even with multiple glyph runs, got {:?}",
            occs.iter().map(|o| &o.target).collect::<Vec<_>>(),
        );
        assert!(
            occs[0].quads.len() >= 2,
            "expected at least two quads merged under one occurrence, got {}",
            occs[0].quads.len(),
        );
    }

    #[test]
    fn distinct_anchors_with_same_href_stay_separate() {
        // Two separate <a> elements pointing at the same URL must produce
        // two occurrences — Arc identity is what distinguishes them.
        let html = r#"<html><body><p><a href="https://x.test">one</a> <a href="https://x.test">two</a></p></body></html>"#;
        let mut collector = LinkCollector::new();
        render_pages_into_collector(html, &mut collector);

        let occs = collector.into_occurrences();
        assert_eq!(
            occs.len(),
            2,
            "expected two occurrences for two distinct <a> elements",
        );
    }
}
