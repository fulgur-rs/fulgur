//! ParagraphPageable — renders text via the Parley→Krilla glyph bridge.

use std::sync::Arc;

use skrifa::MetadataProvider;

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
#[derive(Clone, Debug, Default)]
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
#[derive(Clone)]
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

/// A shaped line of text.
#[derive(Clone)]
pub struct ShapedLine {
    pub height: f32,
    /// Absolute offset from the paragraph's top edge to this line's baseline (from Parley).
    pub baseline: f32,
    pub glyph_runs: Vec<ShapedGlyphRun>,
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
fn draw_line_decorations(
    canvas: &mut Canvas<'_, '_>,
    runs: &[ShapedGlyphRun],
    x: Pt,
    baseline_y: Pt,
) {
    let mut spans: Vec<DecorationSpan> = Vec::new();

    for run in runs {
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
            decoration: TextDecoration {
                line: run.decoration.line,
                style: run.decoration.style,
                color: run.decoration.color,
            },
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

        for run in &line.glyph_runs {
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
                paint: krilla::color::rgb::Color::new(run.color[0], run.color[1], run.color[2])
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

        // Draw decorations after all glyphs so lines appear on top
        draw_line_decorations(canvas, &line.glyph_runs, x, baseline_y);
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
