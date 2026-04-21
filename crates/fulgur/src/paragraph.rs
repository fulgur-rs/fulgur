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
    /// Y position of this image's top edge. On initial construction this
    /// value is temporarily line-relative, but `recalculate_line_box`
    /// promotes it to paragraph-absolute by adding the line's top offset.
    /// During pagination, `split_paragraph` rebases it by subtracting the
    /// consumed height so the next fragment starts at its own paragraph
    /// origin. Contrast with `InlineBoxItem::computed_y`, which stays
    /// line-relative for its entire lifetime.
    pub computed_y: f32,
    pub link: Option<Arc<LinkSpan>>,
}

/// Content of an atomic inline box. Alias for `Box<dyn Pageable>` so wrapper
/// Pageables (Transform / StringSet / CounterOp / BookmarkMarker /
/// RunningElement) survive at the inline-box root and their side effects —
/// CSS transform, named-string capture, counter ops, outline markers,
/// running-element rehosting — apply when the inline-box is rendered.
///
/// `Box<dyn Pageable>` implements `Clone` via `clone_box()` (see
/// `impl Clone for Box<dyn Pageable>` in `pageable.rs`), so `LineItem`
/// still derives `Clone` without an explicit `dyn_clone` crate.
pub type InlineBoxContent = Box<dyn crate::pageable::Pageable>;

/// Returns the offset from `content`'s top edge to the baseline used by CSS
/// for `vertical-align: baseline`. Returns `None` when no in-flow baseline is
/// available, in which case the caller should fall back to the bottom margin
/// edge (CSS 2.1 §10.8.1).
///
/// CSS 2.1 §10.8.1 specifies that for an inline-block with `overflow: visible`
/// and in-flow text, the baseline of the box is the baseline of the last
/// line box inside. If the box has `overflow != visible`, or has no in-flow
/// line boxes, the baseline is the bottom margin edge.
pub(crate) fn inline_box_baseline_offset(content: &dyn crate::pageable::Pageable) -> Option<f32> {
    // CSS fallback: when the outermost clippable block has overflow clip
    // set, the inline-box baseline is the bottom margin edge. Returning
    // `None` here signals that to the caller (convert.rs), which defaults
    // to zero shift. Wrapper layers are transparent for this check — we
    // only peek through them to reach the actual Block / Paragraph.
    if has_outer_overflow_clip(content) {
        return None;
    }
    pageable_last_baseline(content)
}

/// Peek through wrapper pageables to the outermost Block/Paragraph and ask
/// whether it has `overflow: clip` (or hidden/scroll/auto). Wrappers
/// themselves never clip — they only carry markers / transforms.
fn has_outer_overflow_clip(p: &dyn crate::pageable::Pageable) -> bool {
    let any = p.as_any();
    if let Some(b) = any.downcast_ref::<crate::pageable::BlockPageable>() {
        return b.style.has_overflow_clip();
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::TransformWrapperPageable>() {
        return has_outer_overflow_clip(w.inner.as_ref());
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::BookmarkMarkerWrapperPageable>() {
        return has_outer_overflow_clip(w.child.as_ref());
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::CounterOpWrapperPageable>() {
        return has_outer_overflow_clip(w.child.as_ref());
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::StringSetWrapperPageable>() {
        return has_outer_overflow_clip(w.child.as_ref());
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::RunningElementWrapperPageable>() {
        return has_outer_overflow_clip(w.child.as_ref());
    }
    // Paragraph or any other concrete pageable: no overflow style → no clip.
    false
}

/// Recursively find the offset from `p`'s top edge to the last in-flow
/// baseline inside. Walks through wrapper pageables transparently and
/// descends into `BlockPageable`'s children. Returns `None` when nothing
/// paragraph-like is reachable (e.g. a pure image / spacer inline-box).
///
/// Note: `TransformWrapperPageable` is walked without reversing its
/// matrix. For the small rotate/skew transforms common on inline-blocks,
/// the visual delta is minor; a fully matrix-aware baseline would have
/// to project the inner baseline back through `self.matrix`, which is
/// outside this refactor's scope.
///
/// Exposed at `pub(crate)` so unit tests can assert the walk directly.
pub(crate) fn pageable_last_baseline(p: &dyn crate::pageable::Pageable) -> Option<f32> {
    let any = p.as_any();
    if let Some(para) = any.downcast_ref::<ParagraphPageable>() {
        return para.lines.last().map(|l| l.baseline);
    }
    if let Some(block) = any.downcast_ref::<crate::pageable::BlockPageable>() {
        for pc in block.children.iter().rev() {
            if let Some(inner_bo) = pageable_last_baseline(pc.child.as_ref()) {
                return Some(pc.y + inner_bo);
            }
        }
        return None;
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::TransformWrapperPageable>() {
        return pageable_last_baseline(w.inner.as_ref());
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::BookmarkMarkerWrapperPageable>() {
        return pageable_last_baseline(w.child.as_ref());
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::CounterOpWrapperPageable>() {
        return pageable_last_baseline(w.child.as_ref());
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::StringSetWrapperPageable>() {
        return pageable_last_baseline(w.child.as_ref());
    }
    if let Some(w) = any.downcast_ref::<crate::pageable::RunningElementWrapperPageable>() {
        return pageable_last_baseline(w.child.as_ref());
    }
    None
}

/// An atomic inline box (display: inline-block / inline-flex / inline-grid /
/// inline-table) within a shaped line.
#[derive(Clone)]
pub struct InlineBoxItem {
    pub content: InlineBoxContent,
    pub width: f32,
    pub height: f32,
    pub x_offset: f32,
    /// Y offset from the line top in pt. `extract_paragraph` converts
    /// Parley's paragraph-relative `y` to line-relative by subtracting the
    /// accumulated line_top. Unlike `InlineImage::computed_y`, this value
    /// stays line-relative for the lifetime of the item —
    /// `recalculate_line_box` does not promote it, and `split_paragraph`
    /// does not rebase it (each fragment's own `line_top` accumulator
    /// handles vertical positioning naturally).
    pub computed_y: f32,
    pub link: Option<Arc<LinkSpan>>,
    pub opacity: f32,
    pub visible: bool,
}

/// A single item in a shaped line: text glyph run, inline image, or an
/// atomic inline box (display: inline-block / inline-flex / inline-grid /
/// inline-table).
#[derive(Clone)]
pub enum LineItem {
    Text(ShapedGlyphRun),
    Image(InlineImage),
    InlineBox(InlineBoxItem),
}

// Manual `Debug` for `LineItem`: `BlockPageable` / `ParagraphPageable`
// (reachable via `InlineBox.content`, now a `Box<dyn Pageable>`) do not
// themselves implement `Debug`, so the derive can't traverse them. Delegate
// to the existing Text/Image impls and print a compact variant-only form
// for InlineBox, labeling the inner pageable by type via `Any` introspection.
impl std::fmt::Debug for LineItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineItem::Text(t) => f.debug_tuple("Text").field(t).finish(),
            LineItem::Image(i) => f.debug_tuple("Image").field(i).finish(),
            LineItem::InlineBox(ib) => f
                .debug_struct("InlineBox")
                .field("width", &ib.width)
                .field("height", &ib.height)
                .field("x_offset", &ib.x_offset)
                .field("computed_y", &ib.computed_y)
                .field("opacity", &ib.opacity)
                .field("visible", &ib.visible)
                .field("link", &ib.link.is_some())
                .field("content", &inline_box_content_label(ib.content.as_ref()))
                .finish(),
        }
    }
}

/// Best-effort label for the pageable type hosted inside an
/// `InlineBoxItem.content`. Used only by the manual `Debug` impl so
/// `{:?}` output keeps the pre-refactor `Block(..)` / `Paragraph(..)`
/// wording (tests assert on those substrings) while still covering the
/// wrapper pageables that now flow through.
fn inline_box_content_label(p: &dyn crate::pageable::Pageable) -> &'static str {
    let any = p.as_any();
    if any.is::<crate::pageable::BlockPageable>() {
        "Block(..)"
    } else if any.is::<ParagraphPageable>() {
        "Paragraph(..)"
    } else if any.is::<crate::pageable::TransformWrapperPageable>() {
        "Transform(..)"
    } else if any.is::<crate::pageable::BookmarkMarkerWrapperPageable>() {
        "BookmarkMarker(..)"
    } else if any.is::<crate::pageable::CounterOpWrapperPageable>() {
        "CounterOp(..)"
    } else if any.is::<crate::pageable::StringSetWrapperPageable>() {
        "StringSet(..)"
    } else if any.is::<crate::pageable::RunningElementWrapperPageable>() {
        "RunningElement(..)"
    } else {
        "Other(..)"
    }
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
            LineItem::InlineBox(_) => continue,
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
                LineItem::InlineBox(ib) => {
                    if !ib.visible {
                        continue;
                    }
                    let ox = x + ib.x_offset;
                    let oy = line_top_abs + ib.computed_y;
                    crate::pageable::draw_with_opacity(canvas, ib.opacity, |canvas| {
                        // Pass (ox, oy) as absolute draw coordinates instead
                        // of pushing a krilla transform + drawing at (0, 0).
                        // Nested link rects are tracked in logical coords via
                        // `Canvas::link_collector` (pageable-space, not krilla
                        // surface-space), so a krilla transform would shift
                        // the visuals but leave link hit-areas at the origin.
                        // `ib.content` is `Box<dyn Pageable>`, so dispatch
                        // straight to the trait — any wrapper chain
                        // (Transform / StringSet / Counter / Bookmark /
                        // RunningElement) is preserved and applies its
                        // side effects around the inner Block / Paragraph.
                        ib.content.draw(canvas, ox, oy, ib.width, ib.height);
                    });

                    // Link rect built after the opacity block ends, so link
                    // hit-areas remain intact even for opacity<1.0 boxes.
                    if let Some(link_span) = ib.link.as_ref() {
                        let rect = crate::pageable::Rect {
                            x: ox,
                            y: oy,
                            width: ib.width.max(0.0),
                            height: ib.height.max(0.0),
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
            LineItem::InlineBox(_) => continue,
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
            LineItem::InlineBox(_) => continue,
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
                // InlineBoxItem.computed_y is line-relative — no per-item
                // rebase needed; the new fragment's line_top accumulator
                // handles it at draw time.
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

    fn is_visible(&self) -> bool {
        self.visible
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
        // Recurse into inline-box content so nested `id`s (e.g. an
        // `<a id="target">` inside `<span style="display:inline-block">`)
        // still reach the destination registry for `href="#target"`
        // resolution. The coordinate arithmetic mirrors `draw_shaped_lines`
        // (see paragraph.rs:614): `line_top` starts at 0, advances by
        // `line.height` after each line, so `oy = y + line_top + computed_y`
        // matches the draw-path's `line_top_abs + computed_y`.
        let mut line_top: f32 = 0.0;
        for line in &self.lines {
            for item in &line.items {
                if let LineItem::InlineBox(ib) = item {
                    let child_x = x + ib.x_offset;
                    let child_y = y + line_top + ib.computed_y;
                    ib.content
                        .collect_ids(child_x, child_y, ib.width, ib.height, registry);
                }
            }
            line_top += line.height;
        }
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

    #[test]
    fn line_item_inline_box_variant_can_be_constructed() {
        use crate::pageable::BlockPageable;
        // BlockPageable::new takes Vec<Box<dyn Pageable>>; an empty vec is
        // enough for the construction test.
        let block = BlockPageable::new(Vec::new());
        let item = LineItem::InlineBox(InlineBoxItem {
            content: Box::new(block) as InlineBoxContent,
            width: 50.0,
            height: 20.0,
            x_offset: 10.0,
            computed_y: 0.0,
            link: None,
            opacity: 1.0,
            visible: true,
        });
        match item {
            LineItem::InlineBox(ib) => {
                assert_eq!(ib.width, 50.0);
                assert_eq!(ib.height, 20.0);
                assert!(
                    ib.content
                        .as_any()
                        .downcast_ref::<BlockPageable>()
                        .is_some()
                );
            }
            _ => panic!("expected InlineBox variant"),
        }
    }

    // ---------- Manual Debug impl coverage (L230-252) ----------

    /// Covers the manual `Debug` impl for every `LineItem` variant, including
    /// the `InlineBox` struct fields and the inner `Block(..)` / `Paragraph(..)`
    /// content branches. The derive can't traverse BlockPageable/Paragraph-
    /// Pageable because they don't implement Debug, so this path is all
    /// custom.
    #[test]
    fn line_item_debug_impl_covers_all_variants() {
        use crate::pageable::BlockPageable;

        // Text variant — delegates to the ShapedGlyphRun derive.
        let glyph_run = ShapedGlyphRun {
            font_data: Arc::new(Vec::new()),
            font_index: 0,
            font_size: 10.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: String::from("hi"),
            x_offset: 0.0,
            link: None,
        };
        let text = LineItem::Text(glyph_run);
        let s = format!("{:?}", text);
        assert!(s.contains("Text"), "{}", s);

        // Image variant — delegates to InlineImage derive.
        let img = LineItem::Image(make_inline_image(10.0, 10.0, VerticalAlign::Baseline));
        let s = format!("{:?}", img);
        assert!(s.contains("Image"), "{}", s);

        // InlineBox with Block content.
        let block = BlockPageable::new(Vec::new());
        let ib_block = LineItem::InlineBox(InlineBoxItem {
            content: Box::new(block) as InlineBoxContent,
            width: 10.0,
            height: 5.0,
            x_offset: 1.0,
            computed_y: 2.0,
            link: None,
            opacity: 1.0,
            visible: true,
        });
        let s = format!("{:?}", ib_block);
        assert!(s.contains("InlineBox"), "{}", s);
        assert!(s.contains("width: 10.0"), "{}", s);
        assert!(s.contains("Block(..)"), "{}", s);
        assert!(s.contains("link: false"), "{}", s);

        // InlineBox with Paragraph content (exercises the Paragraph(..) arm
        // inside the manual Debug's content label lookup — otherwise never
        // hit).
        let para = ParagraphPageable::new(Vec::new());
        let ib_para = LineItem::InlineBox(InlineBoxItem {
            content: Box::new(para) as InlineBoxContent,
            width: 0.0,
            height: 0.0,
            x_offset: 0.0,
            computed_y: 0.0,
            link: Some(Arc::new(LinkSpan {
                target: LinkTarget::External(Arc::new("https://x".into())),
                alt_text: None,
            })),
            opacity: 0.5,
            visible: false,
        });
        let s = format!("{:?}", ib_para);
        assert!(s.contains("Paragraph(..)"), "{}", s);
        assert!(s.contains("visible: false"), "{}", s);
        assert!(s.contains("link: true"), "{}", s);
    }

    // ---------- recalculate_line_box InlineBox `continue` arms (L815, L847) ----------

    /// Exercises the `LineItem::InlineBox(_) => continue` arms inside both
    /// phases of `recalculate_line_box`. The existing image-only tests never
    /// reach these branches; this mixed-item test does.
    #[test]
    fn recalculate_line_box_skips_inline_box_items() {
        use crate::pageable::BlockPageable;
        let block = BlockPageable::new(Vec::new());
        let mut line = ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![
                // Image with Top alignment forces phase 2 to iterate too.
                LineItem::Image(make_inline_image(10.0, 6.0, VerticalAlign::Top)),
                LineItem::InlineBox(InlineBoxItem {
                    content: Box::new(block) as InlineBoxContent,
                    width: 30.0,
                    height: 20.0,
                    x_offset: 0.0,
                    computed_y: 3.0,
                    link: None,
                    opacity: 1.0,
                    visible: true,
                }),
            ],
        };
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        // InlineBox must still be present unmodified (skipped by both phases).
        assert_eq!(line.items.len(), 2);
        match &line.items[1] {
            LineItem::InlineBox(ib) => {
                assert_eq!(ib.width, 30.0);
                // computed_y is line-relative and not touched by
                // recalculate_line_box — must match the input verbatim.
                assert!(approx(ib.computed_y, 3.0), "computed_y={}", ib.computed_y);
            }
            _ => panic!("expected InlineBox at index 1"),
        }
    }

    // ---------- collect_ids recursion into inline-box Paragraph content ----------

    /// Covers the `ib.content.collect_ids(..)` path in
    /// `ParagraphPageable::collect_ids` when the inline-box hosts a
    /// `ParagraphPageable`. Existing integration tests use Block content
    /// exclusively, so this branch stays otherwise untested.
    #[test]
    fn collect_ids_recurses_into_inline_box_paragraph_content() {
        use crate::pageable::DestinationRegistry;
        let inner =
            ParagraphPageable::new(Vec::new()).with_id(Some(Arc::new("inner-id".to_string())));
        let outer_line = ShapedLine {
            height: 20.0,
            baseline: 15.0,
            items: vec![LineItem::InlineBox(InlineBoxItem {
                content: Box::new(inner) as InlineBoxContent,
                width: 30.0,
                height: 20.0,
                x_offset: 5.0,
                computed_y: 0.0,
                link: None,
                opacity: 1.0,
                visible: true,
            })],
        };
        let outer = ParagraphPageable::new(vec![outer_line]);
        let mut reg = DestinationRegistry::default();
        reg.set_current_page(2);
        outer.collect_ids(10.0, 20.0, 400.0, 600.0, &mut reg);
        // Recorded at (x + ib.x_offset, y + line_top + computed_y) =
        // (10+5, 20+0+0) = (15, 20) on page 2.
        assert_eq!(reg.get("inner-id"), Some((2, 15.0, 20.0)));
    }

    // ---------- pageable_last_baseline walks wrappers ----------

    /// Verifies that `pageable_last_baseline` walks through wrapper
    /// pageables (e.g. `BookmarkMarkerWrapperPageable`) to reach the inner
    /// `ParagraphPageable`'s last-line baseline. This is the core baseline
    /// guarantee that the `Box<dyn Pageable>` refactor enables: prior to
    /// this, the helper only accepted `BlockPageable` / `ParagraphPageable`
    /// directly and silently dropped the baseline when a wrapper got in
    /// the way.
    #[test]
    fn pageable_last_baseline_walks_through_wrappers() {
        use crate::pageable::{BookmarkMarkerPageable, BookmarkMarkerWrapperPageable};

        // ParagraphPageable with a single line of baseline = 10pt.
        let para = ParagraphPageable::new(vec![ShapedLine {
            height: 14.0,
            baseline: 10.0,
            items: Vec::new(),
        }]);

        // Wrap in a bookmark marker (zero-size wrapper).
        let marker = BookmarkMarkerPageable::new(1, "H".to_string());
        let wrapped: Box<dyn crate::pageable::Pageable> =
            Box::new(BookmarkMarkerWrapperPageable::new(marker, Box::new(para)));

        assert_eq!(
            super::pageable_last_baseline(wrapped.as_ref()),
            Some(10.0),
            "pageable_last_baseline must walk through the wrapper to reach the inner paragraph's baseline"
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
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
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

    /// Covers `draw_shaped_lines`' InlineBox arm link-rect emission
    /// (paragraph.rs L755-765). An `<a>` whose child is an inline-block must
    /// produce a link rect sized from the inline-box's own dimensions (not
    /// from any inner glyph run). Before inline-box rendering support this
    /// branch was dead code; this fixture now exercises it.
    #[test]
    fn draw_pushes_link_rect_for_inline_box_inside_anchor() {
        let html = r#"<html><body><div><a href="https://ib.test"><span style="display:inline-block;width:40px;height:20px;background:red">x</span></a></div></body></html>"#;
        let mut collector = LinkCollector::new();
        render_pages_into_collector(html, &mut collector);

        let occs = collector.into_occurrences();
        let ib_occ = occs
            .iter()
            .find(
                |o| matches!(&o.target, crate::paragraph::LinkTarget::External(u) if u.as_str() == "https://ib.test"),
            )
            .expect("expected link occurrence for inline-box anchor");
        assert!(
            !ib_occ.quads.is_empty(),
            "expected at least one quad for the inline-box link rect",
        );
    }
}
