//! `ParagraphRender` — renders text via the Parley→Krilla glyph bridge.

use std::sync::Arc;

use skrifa::MetadataProvider;

use crate::draw_primitives::Canvas;
use crate::image::ImageFormat;
use crate::units::F32Units;

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
///
/// `text_range` is the byte range in the original text that this glyph's
/// cluster represents. All glyphs belonging to the same cluster (e.g. a
/// ligature) share the same range — this is the correct PDF convention and
/// avoids splitting UTF-8 character boundaries.
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    pub id: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub text_range: std::ops::Range<usize>,
}

impl ShapedGlyph {
    /// Normalize a raw glyph advance/offset by `font_size` to the unit-less
    /// ratio stored in `x_advance`/`x_offset`/`y_offset` (see the fulgur
    /// convention documented at the four glyph-construction sites in
    /// `crates/fulgur/src/convert/`).
    ///
    /// Returns `0.0` in every case that would otherwise leak a non-finite
    /// value into the PDF `Tm`/`TJ` operators (issue #639):
    /// - `font_size == 0.0` (the reported repro — `font-size: 0` is a
    ///   valid CSS state used e.g. to suppress whitespace between inline
    ///   children) → the bare division would produce `NaN`.
    /// - `font_size < 0.0` or `NaN` — stray inputs from upstream bugs.
    /// - Subnormal / very small `font_size` where `v / font_size`
    ///   overflows to `±Infinity`.
    /// - Non-finite `v` (also propagated to `0.0` via the trailing
    ///   `is_finite` gate).
    #[inline]
    pub(crate) fn normalize_by_font_size(v: f32, font_size: f32) -> f32 {
        if font_size > 0.0 {
            let ratio = v / font_size;
            if ratio.is_finite() {
                return ratio;
            }
        }
        0.0
    }
}

#[cfg(test)]
mod normalize_by_font_size_tests {
    use super::ShapedGlyph;

    #[test]
    fn positive_divisor_returns_ratio() {
        assert_eq!(ShapedGlyph::normalize_by_font_size(10.0, 5.0), 2.0);
        assert_eq!(ShapedGlyph::normalize_by_font_size(0.0, 5.0), 0.0);
        assert_eq!(ShapedGlyph::normalize_by_font_size(-3.0, 6.0), -0.5);
    }

    #[test]
    fn zero_divisor_returns_zero_not_nan() {
        assert_eq!(ShapedGlyph::normalize_by_font_size(0.0, 0.0), 0.0);
        assert_eq!(ShapedGlyph::normalize_by_font_size(10.0, 0.0), 0.0);
    }

    #[test]
    fn negative_divisor_returns_zero() {
        assert_eq!(ShapedGlyph::normalize_by_font_size(10.0, -5.0), 0.0);
    }

    #[test]
    fn nan_divisor_returns_zero() {
        assert_eq!(ShapedGlyph::normalize_by_font_size(10.0, f32::NAN), 0.0);
    }

    #[test]
    fn subnormal_divisor_that_would_overflow_returns_zero() {
        let subnormal = f32::from_bits(1);
        assert!(subnormal > 0.0 && subnormal < f32::MIN_POSITIVE);
        assert!(!(1.0f32 / subnormal).is_finite());
        assert_eq!(
            ShapedGlyph::normalize_by_font_size(1.0, subnormal),
            0.0,
            "1.0 / subnormal overflows to Infinity; helper must not leak it \
             into `x_advance`",
        );
    }

    #[test]
    fn non_finite_numerator_returns_zero() {
        assert_eq!(ShapedGlyph::normalize_by_font_size(f32::NAN, 5.0), 0.0);
        assert_eq!(ShapedGlyph::normalize_by_font_size(f32::INFINITY, 5.0), 0.0);
    }
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
///
/// `text` is the paragraph's full source text (identical across every
/// `ShapedGlyphRun` produced from one paragraph). It is stored as `Arc<str>`
/// so `extract_paragraph` can bump-clone the same buffer per run rather than
/// pay a String allocation each time; it is passed by reference to krilla's
/// `draw_glyphs` and read via `as_str` in `render` — no mutation callers.
#[derive(Clone, Debug)]
pub struct ShapedGlyphRun {
    pub font_data: Arc<Vec<u8>>,
    pub font_index: u32,
    pub font_size: crate::units::Pt,
    pub color: [u8; 4], // RGBA
    pub decoration: TextDecoration,
    pub glyphs: Vec<ShapedGlyph>,
    pub text: Arc<str>,
    pub x_offset: crate::units::Pt,
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
    Length(crate::units::Pt),
    Percent(f32),
}

/// An inline image run within a shaped line.
#[derive(Clone, Debug)]
pub struct InlineImage {
    pub data: Arc<Vec<u8>>,
    pub format: ImageFormat,
    pub width: crate::units::Pt,
    pub height: crate::units::Pt,
    pub x_offset: crate::units::Pt,
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
    pub computed_y: crate::units::Pt,
    pub link: Option<Arc<LinkSpan>>,
}

/// An atomic inline box (display: inline-block / inline-flex / inline-grid /
/// inline-table) within a shaped line.
#[derive(Clone, Debug)]
pub struct InlineBoxItem {
    pub node_id: Option<usize>,
    pub width: crate::units::Pt,
    pub height: crate::units::Pt,
    pub x_offset: crate::units::Pt,
    /// Y offset from the line top in pt. `extract_paragraph` converts
    /// Parley's paragraph-relative `y` to line-relative by subtracting the
    /// accumulated line_top. Unlike `InlineImage::computed_y`, this value
    /// stays line-relative for the lifetime of the item —
    /// `recalculate_line_box` does not promote it, and `split_paragraph`
    /// does not rebase it (each fragment's own `line_top` accumulator
    /// handles vertical positioning naturally).
    pub computed_y: crate::units::Pt,
    pub link: Option<Arc<LinkSpan>>,
    pub opacity: f32,
    pub visible: bool,
}

/// A single item in a shaped line: text glyph run, inline image, or an
/// atomic inline box (display: inline-block / inline-flex / inline-grid /
/// inline-table).
#[derive(Clone, Debug)]
pub enum LineItem {
    Text(ShapedGlyphRun),
    Image(InlineImage),
    InlineBox(InlineBoxItem),
}

/// A shaped line of text.
#[derive(Clone)]
pub struct ShapedLine {
    pub height: crate::units::Pt,
    /// Absolute offset from the paragraph's top edge to this line's baseline (from Parley).
    pub baseline: crate::units::Pt,
    pub items: Vec<LineItem>,
}

/// Paragraph element that renders shaped text.
#[derive(Clone)]
pub struct ParagraphRender {
    pub lines: Vec<ShapedLine>,
    pub cached_height: f32,
    pub opacity: f32,
    pub visible: bool,
    /// HTML `id` attribute of the inline-root element this paragraph was
    /// extracted from. Used by `DestinationRegistry` to resolve `#anchor`
    /// links targeting headings (`<h1 id=..>`) and similar inline-root
    /// elements that do not carry a `BlockEntry`.
    pub id: Option<Arc<String>>,
    /// fulgur-r6we (Phase 3.2.a): DOM NodeId for `slice_for_page`
    /// geometry lookup.
    pub node_id: Option<usize>,
}

impl ParagraphRender {
    pub fn new(lines: Vec<ShapedLine>) -> Self {
        let cached_height: f32 = lines.iter().map(|l| l.height.to_f32()).sum();
        Self {
            lines,
            cached_height,
            opacity: 1.0,
            visible: true,
            id: None,
            node_id: None,
        }
    }

    /// Attach an `id` anchor to this paragraph. Chain after `new()`.
    pub fn with_id(mut self, id: Option<Arc<String>>) -> Self {
        self.id = id;
        self
    }

    pub fn with_node_id(mut self, node_id: Option<usize>) -> Self {
        self.node_id = node_id;
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
    x: crate::units::Pt,
    width: crate::units::Pt,
    decoration: TextDecoration,
    /// Use metrics from the first run in the span
    font_data: Arc<Vec<u8>>,
    font_index: u32,
    font_size: crate::units::Pt,
}

/// Collect contiguous runs with the same decoration into spans, then draw each span once.
fn draw_line_decorations(
    canvas: &mut Canvas<'_, '_>,
    items: &[LineItem],
    x: crate::units::Pt,
    baseline_y: crate::units::Pt,
) {
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
        let run_width: crate::units::Pt =
            run.glyphs.iter().map(|g| g.x_advance * run.font_size).sum();

        // Try to extend the previous span if decoration matches
        if let Some(last) = spans.last_mut() {
            let last_end = last.x + last.width;
            let gap = (run_x - last_end).abs();
            if last.decoration.same_appearance(&run.decoration) && gap < 0.5_f32.as_pt() {
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
        let metrics =
            get_decoration_metrics(&span.font_data, span.font_index, span.font_size.to_f32());

        if span.decoration.line.contains(TextDecorationLine::UNDERLINE) {
            let line_y = baseline_y + metrics.underline_offset.as_pt();
            draw_decoration_line(
                canvas,
                span.x.to_f32(),
                line_y.to_f32(),
                span.width.to_f32(),
                metrics.underline_thickness,
                span.decoration.color,
                span.decoration.style,
            );
        }
        if span.decoration.line.contains(TextDecorationLine::OVERLINE) {
            let line_y = baseline_y - metrics.overline_pos.as_pt();
            draw_decoration_line(
                canvas,
                span.x.to_f32(),
                line_y.to_f32(),
                span.width.to_f32(),
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
            let line_y = baseline_y - metrics.strikethrough_offset.as_pt();
            draw_decoration_line(
                canvas,
                span.x.to_f32(),
                line_y.to_f32(),
                span.width.to_f32(),
                metrics.strikethrough_thickness,
                span.decoration.color,
                span.decoration.style,
            );
        }
    }
}

/// PR 8g: render-side context that lets `draw_shaped_lines` dispatch
/// inline-box content (`LineItem::InlineBox`) through the v2 dispatcher.
///
/// `None` is passed by the list-item marker text render path
/// (`render::draw_list_item_marker`), because marker line streams only
/// contain Text / Image items — never `LineItem::InlineBox` — and
/// therefore have no inline-box children to dispatch.
///
/// When `Some`, the InlineBox arm computes the inline-flow position
/// `(ox, oy)` and dispatches the inline-box content directly at those
/// coordinates via `render::dispatch_fragment`. Descendants in
/// `Drawables.inline_box_subtree_descendants[content_id]` are dispatched
/// at `(ox, oy)` plus their body-relative offset from the content's own
/// fragment — a single coordinate space, no transform stack needed.
#[derive(Clone, Copy)]
pub struct InlineBoxRenderCtx<'a> {
    pub drawables: &'a crate::drawables::Drawables,
    pub geometry: &'a crate::pagination_layout::PaginationGeometryTable,
    pub page_index: u32,
    /// Page margin in PDF pt — needed by `draw_under_clip` /
    /// `draw_under_transform` / `draw_under_opacity` to compute
    /// descendant positions during the offset-transform dispatch.
    pub margin_left_pt: f32,
    pub margin_top_pt: f32,
}

/// Tracks the currently-open per-run tagged region while `draw_shaped_lines`
/// walks the line items. Each transition between link spans (or to/from
/// non-link content) closes the previous region and opens a new one.
struct RunRegionTracker {
    /// Paragraph NodeId under which `ParagraphRunItem`s are recorded.
    node_id: crate::drawables::NodeId,
    /// `Some(ptr)` while a region is open; `ptr` is `None` for non-link
    /// content or `Some(arc_ptr)` for content under an `<a>` span.
    /// `None` means no region is currently open.
    open_span_ptr: Option<Option<usize>>,
    /// Identifier returned by the matching `start_tagged` call.
    open_identifier: Option<krilla::tagging::Identifier>,
}

impl RunRegionTracker {
    fn new(node_id: crate::drawables::NodeId) -> Self {
        Self {
            node_id,
            open_span_ptr: None,
            open_identifier: None,
        }
    }

    /// Switch to the region identified by `new_span_ptr`, opening/closing
    /// `start_tagged` regions as needed and recording closed regions on
    /// `canvas.tag_collector`.
    ///
    /// When `canvas.in_marked_content` is already set (per-run tagging invoked
    /// under an outer marked-content sequence — e.g. an inline-box dispatched
    /// from a tagged block's paragraph), the open is skipped: Krilla's
    /// `start_tagged` is not nestable and would panic. The run's glyphs / images
    /// still paint, but this run does not get its own `ParagraphRunItem` entry,
    /// so any link on the run falls through to `emit_link_annotations`'s
    /// unwired-fallback path (`add_annotation`) — no `add_tagged_annotation`
    /// call with an OBJR that would fail the later tag-tree consistency check.
    fn transition_to(&mut self, canvas: &mut Canvas<'_, '_>, new_span_ptr: Option<usize>) {
        if self.open_span_ptr == Some(new_span_ptr) {
            return;
        }
        self.close(canvas);
        if canvas.in_marked_content {
            return;
        }
        use krilla::tagging::{ContentTag, SpanTag};
        let id = canvas
            .surface
            .start_tagged(ContentTag::Span(SpanTag::empty()));
        canvas.in_marked_content = true;
        self.open_span_ptr = Some(new_span_ptr);
        self.open_identifier = Some(id);
    }

    /// Close the currently-open region (if any) and record it as a
    /// `ParagraphRunItem` on the tag collector.
    fn close(&mut self, canvas: &mut Canvas<'_, '_>) {
        let Some(span_ptr) = self.open_span_ptr.take() else {
            return;
        };
        canvas.surface.end_tagged();
        canvas.in_marked_content = false;
        let id = self
            .open_identifier
            .take()
            .expect("open_identifier set whenever open_span_ptr is");
        let item = match span_ptr {
            Some(ptr) => crate::draw_primitives::ParagraphRunItem::LinkContent {
                span_ptr: ptr,
                identifier: id,
            },
            None => crate::draw_primitives::ParagraphRunItem::Content(id),
        };
        if let Some(tc) = canvas.tag_collector.as_mut() {
            tc.record_run(self.node_id, item);
        }
    }
}

/// Convert an `Option<&Arc<LinkSpan>>` to its identity pointer used as the
/// per-run region key.
fn link_span_ptr(link: Option<&Arc<LinkSpan>>) -> Option<usize> {
    link.map(|l| Arc::as_ptr(l) as usize)
}

/// Draw pre-shaped text lines at the given position.
pub fn draw_shaped_lines(
    canvas: &mut Canvas<'_, '_>,
    lines: &[ShapedLine],
    x: crate::units::Pt,
    y: crate::units::Pt,
    inline_box_ctx: Option<InlineBoxRenderCtx<'_>>,
) {
    // Track the top edge of each line within the paragraph (paragraph y=0 at
    // `y`). Lines pack tightly by `line.height`. We use the full line box for
    // link activation rects (matches WeasyPrint behavior), so tracking the
    // top via cumulative height is both simpler and more robust than trying
    // to derive it from `line.baseline` (which is an absolute baseline offset
    // and does not carry per-line ascent).
    let mut line_top = crate::units::Pt::ZERO;
    // Per-run tagging mode is active when canvas.link_run_node_id is set and
    // tagging is enabled. In this mode each glyph-run or inline-image cluster
    // bounded by `Arc<LinkSpan>` identity gets its own start_tagged/end_tagged
    // region recorded as a `ParagraphRunItem`.
    let mut run_region = match (canvas.tag_collector.is_some(), canvas.link_run_node_id) {
        (true, Some(node_id)) => Some(RunRegionTracker::new(node_id)),
        _ => None,
    };
    for line in lines {
        let line_top_abs = y + line_top;
        let baseline_y = y + line.baseline;

        for item in &line.items {
            match item {
                LineItem::Text(run) => {
                    if let Some(tracker) = run_region.as_mut() {
                        tracker.transition_to(canvas, link_span_ptr(run.link.as_ref()));
                    }
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

                    let start = krilla::geom::Point::from_xy(
                        (x + run.x_offset).to_f32(),
                        baseline_y.to_f32(),
                    );
                    canvas.surface.draw_glyphs(
                        start,
                        &krilla_glyphs,
                        font,
                        &run.text,
                        run.font_size.to_f32(),
                        false,
                    );

                    // After the glyphs are drawn, record a link rect if this
                    // run was emitted under an <a href>. Width mirrors the
                    // decoration-span computation in `draw_line_decorations`
                    // (same glyph advance accumulator); height uses the full
                    // line box so the hit area is stable across lines.
                    if let Some(link_span) = run.link.as_ref() {
                        let run_width: crate::units::Pt =
                            run.glyphs.iter().map(|g| g.x_advance * run.font_size).sum();
                        let rect = crate::draw_primitives::Rect {
                            x: x + run.x_offset,
                            y: line_top_abs,
                            width: run_width.max(crate::units::Pt::ZERO),
                            height: line.height,
                        };
                        if let Some(collector) = canvas.link_collector.as_deref_mut() {
                            collector.push_rect(link_span, rect);
                        }
                    }
                }
                LineItem::Image(img) => {
                    if let Some(tracker) = run_region.as_mut() {
                        tracker.transition_to(canvas, link_span_ptr(img.link.as_ref()));
                    }
                    if !img.visible {
                        continue;
                    }
                    crate::draw_primitives::draw_with_opacity(canvas, img.opacity, |canvas| {
                        let data: krilla::Data = Arc::clone(&img.data).into();
                        let Ok(image) = img.format.to_krilla_image(data) else {
                            return;
                        };
                        let Some(size) =
                            krilla::geom::Size::from_wh(img.width.to_f32(), img.height.to_f32())
                        else {
                            return;
                        };
                        let img_y = y + img.computed_y;
                        let transform = krilla::geom::Transform::from_translate(
                            (x + img.x_offset).to_f32(),
                            img_y.to_f32(),
                        );
                        canvas.surface.push_transform(&transform);
                        canvas.surface.draw_image(image, size);
                        canvas.surface.pop();
                    });

                    // Record a rect for this image if it sits under an <a>.
                    // Matches the image's drawn coordinates exactly:
                    // (x + x_offset, y + computed_y, width, height).
                    if let Some(link_span) = img.link.as_ref() {
                        let rect = crate::draw_primitives::Rect {
                            x: x + img.x_offset,
                            y: y + img.computed_y,
                            width: img.width.max(crate::units::Pt::ZERO),
                            height: img.height.max(crate::units::Pt::ZERO),
                        };
                        if let Some(collector) = canvas.link_collector.as_deref_mut() {
                            collector.push_rect(link_span, rect);
                        }
                    }
                }
                LineItem::InlineBox(ib) => {
                    // Close any open per-run tag region and clear
                    // `link_run_node_id` before dispatching inline-box
                    // content. `start_tagged` is non-nestable in Krilla:
                    // leaving a region open or having `link_run_node_id` set
                    // while the sub-dispatch also calls `start_tagged` panics.
                    if let Some(tracker) = run_region.as_mut() {
                        tracker.close(canvas);
                    }
                    let saved_link_run_node_id = canvas.link_run_node_id.take();
                    if !ib.visible {
                        canvas.link_run_node_id = saved_link_run_node_id;
                        continue;
                    }
                    let ox = x + ib.x_offset;
                    let oy = line_top_abs + ib.computed_y;

                    // PR 8g: dispatch via the v2 path under an offset
                    // transform. `ib.placeholder.node_id`'s geometry-recorded
                    // position (Taffy/Parley body-relative) does not include
                    // the CSS 2.1 §10.8.1 baseline_shift that `convert/
                    // inline_root.rs:493` applies at convert time, so the
                    // standard dispatcher would render the content at the
                    // wrong y. Push a translate transform equal to the
                    // difference between the inline-flow position
                    // `(ox, oy)` and the dispatcher's `(geo_x_pt, geo_y_pt)`
                    // before invoking `render::dispatch_fragment`.
                    if let Some(ctx) = inline_box_ctx
                        && let Some(content_id) = ib.node_id
                        && let Some(content_geom) = ctx.geometry.get(&content_id)
                        && let Some(content_frag) = content_geom
                            .fragments
                            .iter()
                            .find(|f| f.page_index == ctx.page_index)
                    {
                        // Dispatch the inline-box content via the standard
                        // wrapping helpers (transform / clip / opacity) so
                        // CSS `transform`, `overflow:hidden`, and fractional
                        // opacity on the inline-block are honoured. The
                        // helpers compute descendant positions from
                        // body-relative geometry as
                        // `margin + body_offset + frag.x.in_pt()`, so we
                        // dispatch at the body-relative `geo_pt` and use
                        // `push_transform(translate(off_x, off_y))` to
                        // shift the whole subtree to the inline-flow
                        // position `(ox, oy)`.
                        let geo_x_pt = ctx.margin_left_pt
                            + ctx.drawables.body_offset_pt.0.to_f32()
                            + content_frag.x.in_pt().to_f32();
                        let geo_y_pt = ctx.margin_top_pt
                            + ctx.drawables.body_offset_pt.1.to_f32()
                            + content_frag.y.in_pt().to_f32();
                        let off_x = ox.to_f32() - geo_x_pt;
                        let off_y = oy.to_f32() - geo_y_pt;
                        let transform = krilla::geom::Transform::from_translate(off_x, off_y);
                        let link_affine = crate::draw_primitives::Affine2D::translation(
                            off_x.as_pt(),
                            off_y.as_pt(),
                        );
                        crate::draw_primitives::draw_with_opacity(canvas, ib.opacity, |canvas| {
                            if let Some(lc) = canvas.link_collector.as_deref_mut() {
                                lc.push_transform(link_affine);
                            }
                            canvas.surface.push_transform(&transform);
                            crate::render::dispatch_inline_box_content(
                                canvas,
                                content_id,
                                content_geom,
                                content_frag,
                                geo_x_pt,
                                geo_y_pt,
                                ctx.drawables,
                                ctx.geometry,
                                ctx.margin_left_pt,
                                ctx.margin_top_pt,
                                ctx.page_index,
                                &ctx.drawables.inline_box_subtree_descendants,
                            );
                            canvas.surface.pop();
                            if let Some(lc) = canvas.link_collector.as_deref_mut() {
                                lc.pop_transform();
                            }
                        });
                    }

                    // Link rect built after the opacity block ends, so link
                    // hit-areas remain intact even for opacity<1.0 boxes.
                    if let Some(link_span) = ib.link.as_ref() {
                        let rect = crate::draw_primitives::Rect {
                            x: ox,
                            y: oy,
                            width: ib.width.max(crate::units::Pt::ZERO),
                            height: ib.height.max(crate::units::Pt::ZERO),
                        };
                        if let Some(collector) = canvas.link_collector.as_deref_mut() {
                            collector.push_rect(link_span, rect);
                        }
                    }
                    canvas.link_run_node_id = saved_link_run_node_id;
                }
            }
        }

        // Draw decorations after all glyphs so lines appear on top
        draw_line_decorations(canvas, &line.items, x, baseline_y);

        line_top += line.height;
    }
    // Close any open per-run tagged region at end-of-paragraph.
    if let Some(tracker) = run_region.as_mut() {
        tracker.close(canvas);
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

    let mut line_top = crate::units::Pt::ZERO;
    let mut line_bottom = original_height;

    // Phase 1: compute img_top for flow-aligned images and expand line box.
    // Store (index, img_top) for later computed_y assignment.
    let mut positions: Vec<(usize, crate::units::Pt)> = Vec::new();

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
            VerticalAlign::Middle => baseline - metrics.x_height.as_pt() / 2.0 - img.height / 2.0,
            VerticalAlign::Sub => baseline + metrics.subscript_offset.as_pt() - img.height,
            VerticalAlign::Super => baseline - metrics.superscript_offset.as_pt() - img.height,
            VerticalAlign::TextTop => baseline - metrics.ascent.as_pt(),
            VerticalAlign::TextBottom => baseline + metrics.descent.as_pt() - img.height,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageFormat;

    /// Minimal 1x1 red PNG for test images.
    pub(super) const TEST_PNG: &[u8] = &[
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
            width: width.as_pt(),
            height: height.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: va,
            opacity: 1.0,
            visible: true,
            computed_y: crate::units::Pt::ZERO,
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
            height: height.as_pt(),
            baseline: baseline.as_pt(),
            items: Vec::new(),
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    // ---------- inline-image link rect ----------

    /// Covers the inline-image link-rect arm of `draw_shaped_lines` (the
    /// `if let Some(link_span) = img.link` block): a visible `InlineImage`
    /// carrying an `<a>` link must push a `LinkCollector` rect whose
    /// coordinates equal the image's drawn box
    /// `(x + x_offset, y + computed_y, width, height)`. This path is not
    /// reachable through `render_html` today — linked inline `<img>`
    /// materialisation into a `LineItem::Image` with `link` set is still
    /// pending (see `tests/link_integration.rs`) — so we drive
    /// `draw_shaped_lines` directly with a `LinkCollector`-backed canvas.
    #[test]
    fn draw_shaped_lines_image_link_rect_matches_drawn_coords() {
        let link = Arc::new(LinkSpan {
            target: LinkTarget::External(Arc::new("https://example.com".into())),
            alt_text: None,
        });
        let mut img = make_inline_image(20.0, 10.0, VerticalAlign::Baseline);
        img.x_offset = 2.0_f32.as_pt();
        img.computed_y = 3.0_f32.as_pt();
        img.link = Some(Arc::clone(&link));

        let mut line = text_line(16.0, 12.0);
        line.items.push(LineItem::Image(img));

        let mut lc = crate::draw_primitives::LinkCollector::new();
        {
            let mut doc = krilla::Document::new();
            let settings =
                krilla::page::PageSettings::from_wh(200.0, 200.0).expect("valid page size");
            let mut page = doc.start_page_with(settings);
            let mut surface = page.surface();
            let mut canvas = Canvas {
                surface: &mut surface,
                bookmark_collector: None,
                link_collector: Some(&mut lc),
                tag_collector: None,
                link_run_node_id: None,
                in_marked_content: false,
            };
            // Draw origin (5, 7) pt; rect must land at (5+2, 7+3, 20, 10).
            draw_shaped_lines(&mut canvas, &[line], 5.0_f32.as_pt(), 7.0_f32.as_pt(), None);
        }

        let occ = lc.take_page(0);
        assert_eq!(occ.len(), 1, "exactly one link occurrence expected");
        assert_eq!(occ[0].quads.len(), 1, "one rect for the linked image");
        // Identity transform stack ⇒ the quad vertices are exactly the rect
        // corners in `transform_rect`'s bl, br, tr, tl order. rect =
        // (x + x_offset, y + computed_y, width, height) = (7, 10, 20, 10).
        let pts = occ[0].quads[0].points;
        assert_eq!(pts, [[7.0, 20.0], [27.0, 20.0], [27.0, 10.0], [7.0, 10.0]]);
    }

    /// Compare a migrated `Pt` coordinate against a raw `f32` expectation.
    fn approx_pt(a: crate::units::Pt, b: f32) -> bool {
        (a.to_f32() - b).abs() < 0.01
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
        assert!(approx_pt(line.height, 16.0), "height={:?}", line.height);
        assert!(
            approx_pt(line.baseline, 12.0),
            "baseline={:?}",
            line.baseline
        );
        if let LineItem::Image(img) = &line.items[0] {
            assert!(
                approx_pt(img.computed_y, 4.0),
                "computed_y={:?}",
                img.computed_y
            );
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
        assert!(approx_pt(line.height, 24.0), "height={:?}", line.height);
        assert!(
            approx_pt(line.baseline, 20.0),
            "baseline={:?}",
            line.baseline
        );
        if let LineItem::Image(img) = &line.items[0] {
            assert!(
                approx_pt(img.computed_y, 0.0),
                "computed_y={:?}",
                img.computed_y
            );
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
        assert!(approx_pt(line.height, 16.0), "height={:?}", line.height);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(
                approx_pt(img.computed_y, 3.0),
                "computed_y={:?}",
                img.computed_y
            );
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
                approx_pt(img.computed_y, 10.0),
                "computed_y={:?}",
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
            assert!(
                approx_pt(img.computed_y, 0.0),
                "computed_y={:?}",
                img.computed_y
            );
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
            assert!(
                approx_pt(img.computed_y, 0.0),
                "computed_y={:?}",
                img.computed_y
            );
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
            assert!(
                approx_pt(img.computed_y, 8.0),
                "computed_y={:?}",
                img.computed_y
            );
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
            assert!(
                approx_pt(img.computed_y, 0.0),
                "computed_y={:?}",
                img.computed_y
            );
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
                approx_pt(img.computed_y, line.height.to_f32() - 8.0),
                "computed_y={:?}",
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
            VerticalAlign::Length(3.0_f32.as_pt()),
        )));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(
                approx_pt(img.computed_y, 3.0),
                "computed_y={:?}",
                img.computed_y
            );
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
            assert!(
                approx_pt(img.computed_y, 2.0),
                "computed_y={:?}",
                img.computed_y
            );
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
        assert!(approx_pt(line.height, 20.0), "height={:?}", line.height);
        if let LineItem::Image(img) = &line.items[0] {
            assert!(
                approx_pt(img.computed_y, 0.0),
                "computed_y={:?}",
                img.computed_y
            );
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
        let y_acc = 16.0_f32.as_pt(); // first line height
        line2.baseline -= y_acc; // now line-local: 12.0
        let m = default_metrics();
        recalculate_line_box(&mut line2, &m);
        // Image fits within [0, 16): no expansion
        assert!(
            approx_pt(line2.height, 16.0),
            "height should stay 16, got {:?}",
            line2.height
        );
        // Convert computed_y to paragraph-absolute
        if let LineItem::Image(img) = &mut line2.items[0] {
            img.computed_y += y_acc;
            // paragraph-absolute computed_y = line-local (4.0) + y_acc (16.0) = 20.0
            assert!(
                approx_pt(img.computed_y, 20.0),
                "paragraph-absolute computed_y should be 20, got {:?}",
                img.computed_y
            );
        }
        line2.baseline += y_acc; // restore to paragraph-absolute: 28.0
        assert!(
            approx_pt(line2.baseline, 28.0),
            "baseline should be 28, got {:?}",
            line2.baseline
        );
    }

    // ---------- Id propagation ----------

    #[test]
    fn paragraph_default_has_no_id() {
        let p = ParagraphRender::new(Vec::new());
        assert!(p.id.is_none());
    }

    #[test]
    fn paragraph_with_id_stores_value() {
        let p = ParagraphRender::new(Vec::new()).with_id(Some(Arc::new("section-1".to_string())));
        assert_eq!(p.id.as_deref().map(String::as_str), Some("section-1"));
    }

    #[test]
    fn paragraph_with_node_id_stores_value() {
        let p = ParagraphRender::new(Vec::new()).with_node_id(Some(42));
        assert_eq!(p.node_id, Some(42));
    }

    #[test]
    fn paragraph_with_node_id_none_stores_none() {
        let p = ParagraphRender::new(Vec::new()).with_node_id(None);
        assert_eq!(p.node_id, None);
    }

    #[test]
    fn line_item_inline_box_variant_can_be_constructed() {
        let item = LineItem::InlineBox(InlineBoxItem {
            node_id: Some(42),
            width: 50.0_f32.as_pt(),
            height: 20.0_f32.as_pt(),
            x_offset: 10.0_f32.as_pt(),
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible: true,
        });
        match item {
            LineItem::InlineBox(ib) => {
                assert_eq!(ib.width.to_f32(), 50.0);
                assert_eq!(ib.node_id, Some(42));
            }
            _ => panic!("expected InlineBox variant"),
        }
    }

    // ---------- Debug impl coverage ----------

    /// Covers the `Debug` impl for every `LineItem` variant.
    #[test]
    fn line_item_debug_impl_covers_all_variants() {
        // Text variant — delegates to the ShapedGlyphRun derive.
        let glyph_run = ShapedGlyphRun {
            font_data: Arc::new(Vec::new()),
            font_index: 0,
            font_size: 10.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: Arc::from("hi"),
            x_offset: crate::units::Pt::ZERO,
            link: None,
        };
        let text = LineItem::Text(glyph_run);
        assert!(format!("{:?}", text).contains("Text"));

        // Image variant
        let img = LineItem::Image(make_inline_image(10.0, 10.0, VerticalAlign::Baseline));
        assert!(format!("{:?}", img).contains("Image"));

        // InlineBox variant — node_id: Some(1).
        let ib = LineItem::InlineBox(InlineBoxItem {
            node_id: Some(1),
            width: 10.0_f32.as_pt(),
            height: 5.0_f32.as_pt(),
            x_offset: 1.0_f32.as_pt(),
            computed_y: 2.0_f32.as_pt(),
            link: None,
            opacity: 1.0,
            visible: true,
        });
        let s = format!("{:?}", ib);
        assert!(s.contains("InlineBox"), "{}", s);
        assert!(s.contains("width: Pt(10.0)"), "{}", s);
    }

    // ---------- recalculate_line_box: Text item `continue` arms ----------

    /// Exercises the `LineItem::Text(_) => continue` branches in both Phase 1
    /// and Phase 2 of `recalculate_line_box`. The image-only tests never
    /// include Text items, so those arms are otherwise uncovered.
    #[test]
    fn recalculate_line_box_text_items_are_skipped() {
        let run = ShapedGlyphRun {
            font_data: Arc::new(Vec::new()),
            font_index: 0,
            font_size: 12.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: Arc::from(""),
            x_offset: crate::units::Pt::ZERO,
            link: None,
        };
        let mut line = text_line(16.0, 12.0);
        line.items.push(LineItem::Text(run));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        // Text items are skipped: height and baseline must be unchanged.
        assert!(approx_pt(line.height, 16.0), "height={:?}", line.height);
        assert!(
            approx_pt(line.baseline, 12.0),
            "baseline={:?}",
            line.baseline
        );
    }

    // ---------- recalculate_line_box: Phase-1 line_bottom expansion ----------

    /// When a non-Top/Bottom image's bottom edge (img_top + img.height) exceeds
    /// the initial line_bottom, Phase 1 must expand line_bottom. This is
    /// distinct from the top-expansion cases already tested; it exercises the
    /// `if img_top + img.height > line_bottom { line_bottom = … }` branch.
    #[test]
    fn phase1_image_overflows_bottom_expands_line_bottom() {
        // Use a large descent so that TextBottom places the image below the
        // original line_bottom (16).
        //
        // img_top = baseline + descent - img.height = 12 + 8 - 5 = 15
        // img_bottom = 15 + 5 = 20 > 16 → line_bottom = 20
        let metrics = LineFontMetrics {
            ascent: 12.0,
            descent: 8.0,
            x_height: 8.0,
            subscript_offset: 4.0,
            superscript_offset: 6.0,
        };
        let mut line = text_line(16.0, 12.0);
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            5.0,
            VerticalAlign::TextBottom,
        )));
        recalculate_line_box(&mut line, &metrics);
        // line_top stays 0 (img_top=15 > 0), shift=0
        // height = 20 - 0 = 20
        assert!(approx_pt(line.height, 20.0), "height={:?}", line.height);
        assert!(
            approx_pt(line.baseline, 12.0),
            "baseline={:?}",
            line.baseline
        );
        if let LineItem::Image(img) = &line.items[0] {
            assert!(
                approx_pt(img.computed_y, 15.0),
                "computed_y={:?}",
                img.computed_y
            );
        }
    }

    // ---------- recalculate_line_box: Phase-2 line_top expansion ----------

    /// A Bottom-aligned image whose height exceeds line_bottom causes Phase 2
    /// to expand line_top upward. This exercises the
    /// `if img_top < line_top { line_top = img_top }` branch inside Phase 2.
    #[test]
    fn bottom_image_taller_than_line_expands_line_top() {
        // img_top = line_bottom - img.height = 16 - 20 = -4  (<  line_top 0)
        // → line_top = -4, height = 16 - (-4) = 20, shift = 4
        let mut line = text_line(16.0, 12.0);
        line.items.push(LineItem::Image(make_inline_image(
            10.0,
            20.0,
            VerticalAlign::Bottom,
        )));
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        assert!(approx_pt(line.height, 20.0), "height={:?}", line.height);
        // baseline = 12 + shift(4) = 16
        assert!(
            approx_pt(line.baseline, 16.0),
            "baseline={:?}",
            line.baseline
        );
        if let LineItem::Image(img) = &line.items[0] {
            // computed_y = img_top + shift = -4 + 4 = 0
            assert!(
                approx_pt(img.computed_y, 0.0),
                "computed_y={:?}",
                img.computed_y
            );
        }
    }

    // ---------- get_decoration_metrics fallback ----------

    /// Passing empty font bytes causes `skrifa::FontRef::from_index` to fail,
    /// triggering the `else` fallback branch in `get_decoration_metrics`.
    /// All returned values are simple multiples of `font_size`.
    #[test]
    fn get_decoration_metrics_fallback_with_empty_font_data() {
        let m = get_decoration_metrics(&[], 0, 12.0);
        // fallback_thickness = 12.0 * 0.05 = 0.6
        assert!(
            approx(m.underline_offset, 12.0 * 0.075),
            "underline_offset={}",
            m.underline_offset
        );
        assert!(
            approx(m.underline_thickness, 12.0 * 0.05),
            "underline_thickness={}",
            m.underline_thickness
        );
        assert!(
            approx(m.strikethrough_offset, 12.0 * 0.3),
            "strikethrough_offset={}",
            m.strikethrough_offset
        );
        assert!(
            approx(m.strikethrough_thickness, 12.0 * 0.05),
            "strikethrough_thickness={}",
            m.strikethrough_thickness
        );
        assert!(
            approx(m.overline_pos, 12.0 * 0.7),
            "overline_pos={}",
            m.overline_pos
        );
    }

    /// Varying the font_size scales all fallback values proportionally.
    #[test]
    fn get_decoration_metrics_fallback_scales_with_font_size() {
        let m8 = get_decoration_metrics(&[], 0, 8.0);
        let m16 = get_decoration_metrics(&[], 0, 16.0);
        // Every field should double when font_size doubles.
        assert!(approx(m16.underline_offset, m8.underline_offset * 2.0));
        assert!(approx(
            m16.underline_thickness,
            m8.underline_thickness * 2.0
        ));
        assert!(approx(
            m16.strikethrough_offset,
            m8.strikethrough_offset * 2.0
        ));
        assert!(approx(m16.overline_pos, m8.overline_pos * 2.0));
    }

    // ---------- recalculate_line_box InlineBox `continue` arms (L815, L847) ----------

    /// Exercises the `LineItem::InlineBox(_) => continue` arms inside both
    /// phases of `recalculate_line_box`. The existing image-only tests never
    /// reach these branches; this mixed-item test does.
    #[test]
    fn recalculate_line_box_skips_inline_box_items() {
        let mut line = ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![
                LineItem::Image(make_inline_image(10.0, 6.0, VerticalAlign::Top)),
                LineItem::InlineBox(InlineBoxItem {
                    node_id: None,
                    width: 30.0_f32.as_pt(),
                    height: 20.0_f32.as_pt(),
                    x_offset: crate::units::Pt::ZERO,
                    computed_y: 3.0_f32.as_pt(),
                    link: None,
                    opacity: 1.0,
                    visible: true,
                }),
            ],
        };
        let m = default_metrics();
        recalculate_line_box(&mut line, &m);
        assert_eq!(line.items.len(), 2);
        match &line.items[1] {
            LineItem::InlineBox(ib) => {
                assert_eq!(ib.width.to_f32(), 30.0);
                assert!(
                    approx_pt(ib.computed_y, 3.0),
                    "computed_y={:?}",
                    ib.computed_y
                );
            }
            _ => panic!("expected InlineBox at index 1"),
        }
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
            font_size: 12.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: Arc::from(""),
            x_offset: crate::units::Pt::ZERO,
            link: None,
        };
        assert!(run.link.is_none());
    }

    // ---------- link_span_ptr ----------

    fn make_link_span(url: &str) -> Arc<LinkSpan> {
        Arc::new(LinkSpan {
            target: LinkTarget::External(Arc::new(url.into())),
            alt_text: None,
        })
    }

    #[test]
    fn link_span_ptr_none_returns_none() {
        assert_eq!(link_span_ptr(None), None);
    }

    #[test]
    fn link_span_ptr_same_arc_yields_same_value() {
        let span = make_link_span("https://example.com");
        let p1 = link_span_ptr(Some(&span));
        let p2 = link_span_ptr(Some(&span));
        assert!(p1.is_some());
        assert_eq!(p1, p2);
    }

    #[test]
    fn link_span_ptr_different_arcs_yield_different_values() {
        let a = make_link_span("https://a.example.com");
        let b = make_link_span("https://b.example.com");
        let pa = link_span_ptr(Some(&a));
        let pb = link_span_ptr(Some(&b));
        assert_ne!(
            pa, pb,
            "distinct Arc allocations must produce distinct pointers"
        );
    }

    #[test]
    fn link_span_ptr_clone_arc_yields_same_value() {
        let span = make_link_span("https://example.com");
        let clone = Arc::clone(&span);
        assert_eq!(
            link_span_ptr(Some(&span)),
            link_span_ptr(Some(&clone)),
            "Arc::clone shares the same allocation, so pointers must match",
        );
    }
}

#[cfg(test)]
mod text_decoration_tests {
    use super::*;

    // ── TextDecorationLine ───────────────────────────────────

    #[test]
    fn text_decoration_line_none_is_none() {
        assert!(TextDecorationLine::NONE.is_none());
    }

    #[test]
    fn text_decoration_line_underline_is_not_none() {
        assert!(!TextDecorationLine::UNDERLINE.is_none());
    }

    #[test]
    fn text_decoration_line_contains_self() {
        assert!(TextDecorationLine::UNDERLINE.contains(TextDecorationLine::UNDERLINE));
    }

    #[test]
    fn text_decoration_line_contains_returns_false_when_missing() {
        assert!(!TextDecorationLine::UNDERLINE.contains(TextDecorationLine::OVERLINE));
    }

    #[test]
    fn text_decoration_line_bitor_combines_flags() {
        let combined = TextDecorationLine::UNDERLINE | TextDecorationLine::OVERLINE;
        assert!(combined.contains(TextDecorationLine::UNDERLINE));
        assert!(combined.contains(TextDecorationLine::OVERLINE));
        assert!(!combined.contains(TextDecorationLine::LINE_THROUGH));
        assert!(!combined.is_none());
    }

    #[test]
    fn text_decoration_line_line_through_is_not_none() {
        assert!(!TextDecorationLine::LINE_THROUGH.is_none());
    }

    // ── TextDecoration::same_appearance ─────────────────────

    #[test]
    fn text_decoration_same_appearance_identical() {
        let d = TextDecoration {
            line: TextDecorationLine::UNDERLINE,
            style: TextDecorationStyle::Solid,
            color: [0, 0, 0, 255],
        };
        assert!(d.same_appearance(&d));
    }

    #[test]
    fn text_decoration_same_appearance_differs_on_color() {
        let a = TextDecoration {
            line: TextDecorationLine::UNDERLINE,
            style: TextDecorationStyle::Solid,
            color: [0, 0, 0, 255],
        };
        let b = TextDecoration {
            color: [255, 0, 0, 255],
            ..a
        };
        assert!(!a.same_appearance(&b));
    }

    #[test]
    fn text_decoration_same_appearance_differs_on_style() {
        let a = TextDecoration {
            line: TextDecorationLine::UNDERLINE,
            style: TextDecorationStyle::Solid,
            color: [0, 0, 0, 255],
        };
        let b = TextDecoration {
            style: TextDecorationStyle::Dashed,
            ..a
        };
        assert!(!a.same_appearance(&b));
    }

    #[test]
    fn text_decoration_same_appearance_differs_on_line() {
        let a = TextDecoration {
            line: TextDecorationLine::UNDERLINE,
            style: TextDecorationStyle::Solid,
            color: [0, 0, 0, 255],
        };
        let b = TextDecoration {
            line: TextDecorationLine::LINE_THROUGH,
            ..a
        };
        assert!(!a.same_appearance(&b));
    }

    #[test]
    fn text_decoration_default_has_no_line() {
        let d = TextDecoration::default();
        assert!(d.line.is_none());
    }
}

#[cfg(test)]
mod decoration_metrics_real_font_tests {
    use super::*;

    const NOTO_SANS_REGULAR: &[u8] =
        include_bytes!("../../../examples/.fonts/NotoSans-Regular.ttf");

    /// Passing valid TTF bytes causes `skrifa::FontRef::from_index` to succeed,
    /// executing the main branch of `get_decoration_metrics` rather than the
    /// empty-data fallback that the existing tests exercise.
    #[test]
    fn real_font_enters_skrifa_main_branch() {
        let m = get_decoration_metrics(NOTO_SANS_REGULAR, 0, 12.0);
        assert!(
            m.underline_offset > 0.0,
            "underline_offset={}",
            m.underline_offset
        );
        assert!(
            m.underline_thickness > 0.0,
            "underline_thickness={}",
            m.underline_thickness
        );
        assert!(
            m.strikethrough_offset > 0.0,
            "strikethrough_offset={}",
            m.strikethrough_offset
        );
        assert!(
            m.strikethrough_thickness > 0.0,
            "strikethrough_thickness={}",
            m.strikethrough_thickness
        );
        assert!(m.overline_pos > 0.0, "overline_pos={}", m.overline_pos);
    }

    /// The minimum underline offset floor (`font_size * 0.075`) is enforced even
    /// when the font's own OS/2 underline offset is smaller.
    #[test]
    fn real_font_underline_offset_meets_minimum() {
        let font_size = 12.0_f32;
        let m = get_decoration_metrics(NOTO_SANS_REGULAR, 0, font_size);
        assert!(
            m.underline_offset >= font_size * 0.075,
            "expected >= {:.4}, got {:.4}",
            font_size * 0.075,
            m.underline_offset,
        );
    }

    /// Both `underline_thickness` and `strikethrough_thickness` are clamped to
    /// at least `font_size * 0.02` (guards against zero-thickness OS/2 entries).
    #[test]
    fn real_font_min_thickness_is_enforced() {
        let font_size = 12.0_f32;
        let min = font_size * 0.02;
        let m = get_decoration_metrics(NOTO_SANS_REGULAR, 0, font_size);
        assert!(
            m.underline_thickness >= min,
            "underline_thickness {:.4} < min {:.4}",
            m.underline_thickness,
            min,
        );
        assert!(
            m.strikethrough_thickness >= min,
            "strikethrough_thickness {:.4} < min {:.4}",
            m.strikethrough_thickness,
            min,
        );
    }

    /// `strikethrough_offset = strikeout.offset.max(ascent * 0.35)`.
    /// For any real font both operands are positive, so the result must be > 0.
    #[test]
    fn real_font_strikethrough_offset_is_positive() {
        let m = get_decoration_metrics(NOTO_SANS_REGULAR, 0, 12.0);
        assert!(
            m.strikethrough_offset > 0.0,
            "strikethrough_offset={}",
            m.strikethrough_offset,
        );
    }

    /// All offsets and thicknesses should grow when `font_size` doubles,
    /// since they are derived from either font-metric fractions or explicit
    /// `font_size` multiples.
    #[test]
    fn real_font_metrics_grow_with_font_size() {
        let m1 = get_decoration_metrics(NOTO_SANS_REGULAR, 0, 12.0);
        let m2 = get_decoration_metrics(NOTO_SANS_REGULAR, 0, 24.0);
        assert!(
            m2.underline_offset > m1.underline_offset,
            "larger font_size should increase underline_offset"
        );
        assert!(
            m2.underline_thickness > m1.underline_thickness,
            "larger font_size should increase underline_thickness"
        );
        assert!(
            m2.strikethrough_offset > m1.strikethrough_offset,
            "larger font_size should increase strikethrough_offset"
        );
        assert!(
            m2.strikethrough_thickness > m1.strikethrough_thickness,
            "larger font_size should increase strikethrough_thickness"
        );
        assert!(
            m2.overline_pos > m1.overline_pos,
            "larger font_size should increase overline_pos"
        );
    }
}

// Exercises draw_straight_line, draw_decoration_line (all five style
// variants), draw_line_decorations (UNDERLINE / OVERLINE / LINE_THROUGH
// arms and span-merging logic), RunRegionTracker (tagged-PDF + link path),
// and the LineItem::Image / LineItem::InlineBox arms of draw_shaped_lines
// through Engine::render_html so the coverage gate sees them under --lib.
#[cfg(test)]
mod render_smoke_tests {
    fn render_html(html: &str) -> Vec<u8> {
        crate::engine::Engine::builder()
            .build()
            .render(html)
            .expect("render failed")
    }

    fn render_html_tagged(html: &str) -> Vec<u8> {
        crate::engine::Engine::builder()
            .tagged(true)
            .lang("en")
            .build()
            .render(html)
            .expect("tagged render failed")
    }

    use super::tests::TEST_PNG as RED_1X1_PNG;

    // ── draw_decoration_line: Solid (→ draw_straight_line) ───────────────

    #[test]
    fn draw_decoration_solid_underline() {
        // Exercises draw_decoration_line Solid branch → draw_straight_line.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="text-decoration: underline">underlined text</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_decoration_line: Dashed (→ draw_straight_line with dash array) ─

    #[test]
    fn draw_decoration_dashed_underline() {
        // Exercises draw_decoration_line Dashed branch.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="text-decoration: underline; text-decoration-style: dashed">dashed</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_decoration_line: Dotted (Round linecap + zero-length dash) ─────

    #[test]
    fn draw_decoration_dotted_underline() {
        // Exercises draw_decoration_line Dotted branch (round linecap).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="text-decoration: underline; text-decoration-style: dotted">dotted</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_decoration_line: Double (two draw_straight_line calls) ──────────

    #[test]
    fn draw_decoration_double_underline() {
        // Exercises draw_decoration_line Double branch (offset ± gap/2).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="text-decoration: underline; text-decoration-style: double">double</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_decoration_line: Wavy (cubic-bezier loop) ───────────────────────

    #[test]
    fn draw_decoration_wavy_underline() {
        // Exercises draw_decoration_line Wavy branch — the cubic-bezier
        // oscillation loop (wavelength ≥ 0.01 so the guard lets it through).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="text-decoration: underline; text-decoration-style: wavy">wavy</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_line_decorations: OVERLINE arm ──────────────────────────────────

    #[test]
    fn draw_line_decorations_overline_arm() {
        // Exercises the TextDecorationLine::OVERLINE branch inside
        // draw_line_decorations, computing line_y = baseline - overline_pos.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="text-decoration: overline">overlined text</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_line_decorations: LINE_THROUGH arm ───────────────────────────────

    #[test]
    fn draw_line_decorations_line_through_arm() {
        // Exercises the TextDecorationLine::LINE_THROUGH branch inside
        // draw_line_decorations, computing line_y = baseline - strikethrough_offset.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="text-decoration: line-through">struck text</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_line_decorations: all three lines combined ───────────────────────

    #[test]
    fn draw_line_decorations_all_three_combined() {
        // A single span with underline + overline + line-through exercises all
        // three draw_line_decorations arms in one pass.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p style="text-decoration: underline overline line-through">all decorations</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_line_decorations: span-merging (adjacent same-decoration runs) ──

    #[test]
    fn draw_line_decorations_span_merging_adjacent_runs() {
        // Two adjacent <span>s with identical underline decoration. The
        // gap < 0.5 branch in draw_line_decorations should extend the first
        // DecorationSpan rather than starting a new one.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p><span style="text-decoration: underline">hello</span><span
               style="text-decoration: underline"> world</span></p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── RunRegionTracker: tagged PDF with linked text ─────────────────────────

    #[test]
    fn run_region_tracker_tagged_paragraph_with_link() {
        // tagged(true) enables tag_collector; the <a href> makes
        // para_has_link_runs return true, so link_run_node_id is set.
        // Together these conditions activate RunRegionTracker::new →
        // transition_to → close inside draw_shaped_lines.
        let pdf = render_html_tagged(
            r#"<!doctype html><html><body>
            <p>Visit <a href="https://example.com">this link</a> for details.</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn run_region_tracker_tagged_paragraph_link_only() {
        // Paragraph containing only a link (no surrounding non-link text).
        // Exercises the transition from non-link to link run and back to
        // end-of-paragraph close at the paragraph boundary.
        let pdf = render_html_tagged(
            r#"<!doctype html><html><body>
            <p><a href="https://example.com">entire paragraph is a link</a></p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_shaped_lines: LineItem::Image arm (inside image marker) ─────────

    #[test]
    fn draw_shaped_lines_image_arm_inside_image_marker() {
        // list-style-position: inside + list-style-image creates a
        // LineItem::Image item prepended to the first paragraph line.
        // draw_shaped_lines hits the Image arm, rendering it via Krilla.
        let mut assets = crate::asset::AssetBundle::new();
        assets.add_image("bullet.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(assets)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <ul style="list-style-position: inside; list-style-image: url(bullet.png)">
                    <li>item with image marker</li>
                </ul>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_shaped_lines: LineItem::Image with link (image link rect path) ──

    #[test]
    fn draw_shaped_lines_image_arm_linked_image_in_paragraph() {
        // An <img> inside an <a href> inside a <p> produces a LineItem::Image
        // with img.link set, exercising the push_rect call for image links.
        let mut assets = crate::asset::AssetBundle::new();
        assets.add_image("pixel.png", RED_1X1_PNG.to_vec());
        let pdf = crate::engine::Engine::builder()
            .assets(assets)
            .build()
            .render(
                r#"<!doctype html><html><body>
                <p><a href="https://example.com"><img src="pixel.png"
                   width="20" height="20"></a> caption text</p>
                </body></html>"#,
            )
            .expect("render failed");
        assert!(pdf.starts_with(b"%PDF"));
    }

    // ── draw_shaped_lines: LineItem::InlineBox arm ───────────────────────────

    #[test]
    fn draw_shaped_lines_inline_box_arm() {
        // A display:inline-block element inside a paragraph creates a
        // LineItem::InlineBox, exercising that arm of draw_shaped_lines.
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p>before <span style="display:inline-block;width:40px;height:20px;
               background:red"></span> after</p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn draw_shaped_lines_inline_box_arm_with_link() {
        // display:inline-block wrapped in <a href> exercises the link rect
        // collection path for LineItem::InlineBox (ib.link is set).
        let pdf = render_html(
            r#"<!doctype html><html><body>
            <p><a href="https://example.com"><span
               style="display:inline-block;width:30px;height:15px;background:blue">
            </span></a></p>
            </body></html>"#,
        );
        assert!(pdf.starts_with(b"%PDF"));
    }
}

// Tests for private draw helpers that are easier to verify by calling them
// directly than by driving the full Engine pipeline.  All tests in this module
// use a minimal Krilla surface so they run fast and in-process.
#[cfg(test)]
mod draw_decoration_direct_tests {
    use super::*;
    use crate::draw_primitives::Canvas;
    use crate::image::ImageFormat;
    use crate::units::F32Units;
    use std::sync::Arc;

    // Helpers ────────────────────────────────────────────────────────────────

    /// Run `f` with a throwaway Krilla canvas (non-tagged, 200×200 pt page).
    fn with_canvas<F: FnOnce(&mut Canvas<'_, '_>)>(f: F) {
        let mut doc = krilla::Document::new();
        let settings = krilla::page::PageSettings::from_wh(200.0, 200.0).expect("valid page size");
        let mut page = doc.start_page_with(settings);
        let mut surface = page.surface();
        let mut canvas = Canvas {
            surface: &mut surface,
            bookmark_collector: None,
            link_collector: None,
            tag_collector: None,
            link_run_node_id: None,
            in_marked_content: false,
        };
        f(&mut canvas);
    }

    fn make_test_image(visible: bool) -> InlineImage {
        InlineImage {
            data: Arc::new(super::tests::TEST_PNG.to_vec()),
            format: ImageFormat::Png,
            width: 10.0_f32.as_pt(),
            height: 10.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible,
            computed_y: crate::units::Pt::ZERO,
            link: None,
        }
    }

    fn make_test_inline_box(visible: bool) -> InlineBoxItem {
        InlineBoxItem {
            node_id: None,
            width: 30.0_f32.as_pt(),
            height: 20.0_f32.as_pt(),
            x_offset: crate::units::Pt::ZERO,
            computed_y: crate::units::Pt::ZERO,
            link: None,
            opacity: 1.0,
            visible,
        }
    }

    // ── draw_decoration_line: Wavy + tiny thickness ──────────────────────────

    /// Covers lines 471-483: when the computed `half` (= `thickness * 2.0`) is
    /// less than 0.01, the wavy oscillation loop would run forever, so the guard
    /// falls back to a plain straight line.  A thickness of 0.001 gives
    /// `half = 0.002 < 0.01`.
    #[test]
    fn wavy_tiny_thickness_falls_back_to_straight_line() {
        with_canvas(|canvas| {
            draw_decoration_line(
                canvas,
                0.0,
                5.0,
                50.0,
                0.001, // thickness → half = 0.001 * 4.0 / 2.0 = 0.002 < 0.01
                [0, 0, 0, 255],
                TextDecorationStyle::Wavy,
            );
        });
    }

    /// Edge case: thickness exactly at the boundary (half == 0.01).  This should
    /// *not* trigger the guard (condition is strictly `< 0.01`) and therefore
    /// enters the normal wavy loop.
    #[test]
    fn wavy_boundary_thickness_enters_normal_loop() {
        with_canvas(|canvas| {
            // half = 0.005 * 4.0 / 2.0 = 0.01 → NOT < 0.01 → normal wavy loop
            draw_decoration_line(
                canvas,
                0.0,
                5.0,
                50.0,
                0.005,
                [0, 0, 0, 255],
                TextDecorationStyle::Wavy,
            );
        });
    }

    // ── draw_line_decorations: span-merging continue ─────────────────────────

    /// Covers line 561: when two consecutive runs share the same decoration and
    /// their gap is zero, `draw_line_decorations` extends the existing
    /// `DecorationSpan` instead of creating a new one (the `continue` branch).
    ///
    /// run1: x_offset = 0pt, glyph advance = 5.0, font_size = 10pt → width = 50pt
    /// run2: x_offset = 50pt (exactly at run1's end) → gap = 0pt < 0.5pt → extend
    #[test]
    fn draw_line_decorations_adjacent_runs_span_merging_fires() {
        let decoration = TextDecoration {
            line: TextDecorationLine::UNDERLINE,
            style: TextDecorationStyle::Solid,
            color: [0, 0, 0, 255],
        };

        let make_run = |x_offset_pt: f32| ShapedGlyphRun {
            font_data: Arc::new(Vec::new()),
            font_index: 0,
            font_size: 10.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration,
            glyphs: vec![ShapedGlyph {
                id: 0,
                x_advance: 5.0, // 5.0 * 10pt = 50pt run width
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..1,
            }],
            text: Arc::from("a"),
            x_offset: x_offset_pt.as_pt(),
            link: None,
        };

        let items = vec![
            LineItem::Text(make_run(0.0)),  // ends at 50pt
            LineItem::Text(make_run(50.0)), // starts at 50pt → gap = 0 → merges
        ];

        with_canvas(|canvas| {
            draw_line_decorations(canvas, &items, 0.0_f32.as_pt(), 12.0_f32.as_pt());
        });
    }

    // ── draw_shaped_lines: invisible image → continue (line 840) ─────────────

    /// Covers line 840: an `InlineImage` with `visible = false` is skipped by
    /// `draw_shaped_lines` without touching Krilla's draw_image path.
    #[test]
    fn draw_shaped_lines_invisible_image_skips_rendering() {
        let line = ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![LineItem::Image(make_test_image(false))],
        };
        with_canvas(|canvas| {
            draw_shaped_lines(canvas, &[line], 0.0_f32.as_pt(), 0.0_f32.as_pt(), None);
        });
    }

    // ── draw_shaped_lines: invisible InlineBox → continue (lines 888-889) ────

    /// Covers lines 888-889: an `InlineBoxItem` with `visible = false` is
    /// skipped by `draw_shaped_lines`; `canvas.link_run_node_id` is restored and
    /// the item is not dispatched.
    #[test]
    fn draw_shaped_lines_invisible_inline_box_skips_rendering() {
        let line = ShapedLine {
            height: 16.0_f32.as_pt(),
            baseline: 12.0_f32.as_pt(),
            items: vec![LineItem::InlineBox(make_test_inline_box(false))],
        };
        with_canvas(|canvas| {
            draw_shaped_lines(canvas, &[line], 0.0_f32.as_pt(), 0.0_f32.as_pt(), None);
        });
    }
}
