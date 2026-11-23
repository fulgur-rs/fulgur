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
/// Variation axis tags and values in user space (`wght` 700, `wdth` 75).
pub type FontVariations = Vec<([u8; 4], f32)>;

/// The byte range of text that a cluster's glyphs stand for.
///
/// Parley gives all of a ligature's glyphs to its first cluster and none to
/// the clusters it continues over, so the first cluster's own range covers
/// only the first letter: the `ffi` glyph of `office` would carry the text
/// `f`, and the word would extract from the PDF as `ofce`. The range runs on
/// over the continuation clusters, so the glyph carries the whole `ffi`.
pub(crate) fn cluster_glyph_text_range<B: parley::Brush>(
    cluster: &parley::Cluster<'_, B>,
) -> std::ops::Range<usize> {
    let mut range = cluster.text_range();
    if cluster.is_ligature_start() {
        let mut next = cluster.next_logical();
        while let Some(continuation) = next.filter(|c| c.is_ligature_continuation()) {
            range.end = range.end.max(continuation.text_range().end);
            next = continuation.next_logical();
        }
    }
    range
}

/// The `tab-size` of a paragraph: how far apart its tab stops are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TabSize {
    /// A number of space advances of the tab's font.
    Spaces(f32),
    Length(crate::units::Pt),
}

/// What a line's tab stops depend on, from Parley's line metrics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TabLine {
    /// The line has no right-to-left text.
    pub left_to_right: bool,
    /// Where the line box ends, in pt from the start of the paragraph's box:
    /// before a float beside it, at the box's edge otherwise.
    pub end: f32,
    /// The width of the white space that hangs past the end (`pre-wrap`).
    pub hanging: f32,
}

/// What sets the tab stops of a paragraph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TabStops {
    pub size: TabSize,
    /// `letter-spacing` plus `word-spacing`, in pt: CSS counts them in the
    /// space that a `tab-size` number multiplies.
    pub spacing: f32,
    /// Whether the lines start at the left edge of the box (`text-align`
    /// start or left on left-to-right text): the stops are measured from
    /// there, and a centred or right-aligned line would no longer be.
    pub start_aligned: bool,
}

/// The space glyph of a run's font and its advance in em.
fn space_glyph(run: &ShapedGlyphRun) -> Option<(u32, f32)> {
    use skrifa::MetadataProvider;
    let font = skrifa::FontRef::from_index(&run.font_data, run.font_index).ok()?;
    let id = font.charmap().map(' ')?;
    let location = font.axes().location(
        run.variations
            .iter()
            .map(|(tag, value)| (skrifa::Tag::new(tag), *value)),
    );
    let advance = font
        .glyph_metrics(skrifa::instance::Size::new(1.0), &location)
        .advance_width(id)?;
    Some((id.to_u32(), advance))
}

fn is_tab(text: &str, glyph: &ShapedGlyph) -> bool {
    text.get(glyph.text_range.clone()) == Some("\t")
}

fn line_has_tab(items: &[LineItem]) -> bool {
    items.iter().any(|item| match item {
        LineItem::Text(run) => run.glyphs.iter().any(|g| is_tab(&run.text, g)),
        _ => false,
    })
}

/// Where a line's content ends, in pt from the start of the box.
fn line_end(items: &[LineItem]) -> f32 {
    items
        .iter()
        .map(|item| match item {
            LineItem::Text(run) => {
                run.x_offset.to_f32()
                    + run.glyphs.iter().map(|g| g.x_advance).sum::<f32>() * run.font_size.to_f32()
            }
            LineItem::Image(image) => (image.x_offset + image.width).to_f32(),
            LineItem::InlineBox(inline_box) => (inline_box.x_offset + inline_box.width).to_f32(),
        })
        .fold(0.0, f32::max)
}

/// Takes the tabs of a line out of its glyphs, leaving their width, and
/// with `expand` makes each as wide as the gap to the next tab stop, moving
/// what follows along.
fn tab_line(items: &mut [LineItem], stops: &TabStops, expand: bool) {
    let mut shift = 0.0_f32;
    for item in items.iter_mut() {
        let run = match item {
            LineItem::Text(run) => run,
            LineItem::Image(image) => {
                image.x_offset += shift.as_pt();
                continue;
            }
            LineItem::InlineBox(inline_box) => {
                inline_box.x_offset += shift.as_pt();
                continue;
            }
        };
        run.x_offset += shift.as_pt();
        if !run.glyphs.iter().any(|g| is_tab(&run.text, g)) {
            continue;
        }
        let font_size = run.font_size.to_f32();
        let space_id = space_glyph(run);
        let space = space_id.map_or(0.25, |(_, advance)| advance) * font_size + stops.spacing;
        let interval = match stops.size {
            TabSize::Spaces(n) => n * space,
            TabSize::Length(length) => length.to_f32(),
        };
        let mut pen = run.x_offset.to_f32();
        // The width of the tabs before the first glyph kept, and the first
        // of those tabs.
        let mut lead = 0.0_f32;
        let mut first_tab = None;
        let mut kept: Vec<ShapedGlyph> = Vec::with_capacity(run.glyphs.len());
        for glyph in std::mem::take(&mut run.glyphs) {
            let advance = glyph.x_advance * font_size;
            if !is_tab(&run.text, &glyph) {
                pen += advance;
                kept.push(glyph);
                continue;
            }
            let width = if !expand || font_size <= 0.0 {
                advance
            } else if interval > 0.0 {
                let mut stop = ((pen / interval).floor() + 1.0) * interval;
                if stop - pen < space * 0.5 {
                    stop += interval;
                }
                stop - pen
            } else {
                0.0
            };
            shift += width - advance;
            pen += width;
            match kept.last_mut() {
                Some(previous) if font_size > 0.0 => previous.x_advance += width / font_size,
                Some(_) => {}
                None => {
                    lead += width;
                    first_tab.get_or_insert(glyph);
                }
            }
        }
        // Tabs before the first glyph stay in the run, so its link area and
        // its underline cover them: the first glyph moves along by their
        // width. A run of tabs alone keeps one space glyph for them when it
        // has a link or a decoration to show.
        if lead > 0.0 && font_size > 0.0 {
            if let Some(first) = kept.first_mut() {
                first.x_offset += lead / font_size;
                first.x_advance += lead / font_size;
            } else if let (Some(tab), Some((id, _))) = (first_tab, space_id)
                && (run.link.is_some() || !run.decoration.line.is_none())
            {
                kept.push(ShapedGlyph {
                    id,
                    x_advance: lead / font_size,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    ..tab
                });
            } else {
                run.x_offset += lead.as_pt();
            }
        }
        run.glyphs = kept;
    }
}

/// Lays out the preserved tabs (`white-space: pre`, `pre-wrap`) of a
/// paragraph. Parley shapes a tab like any other character, which most
/// fonts draw with `.notdef`, a box that PDF/UA and PDF/A refuse, as wide
/// as that glyph. A tab draws nothing here; it is the gap it leaves. On a
/// left-to-right line that starts at the left of its box it reaches the
/// next tab stop, as CSS Text 3 §4.2 asks: the stops are `tab-size` apart
/// from the start of the box, and a gap shorter than half a space goes on
/// to the stop after. `tab_lines` has, by line, what Parley measured.
///
/// Parley broke and measured the lines with each tab as wide as its glyph.
/// Where the stops would carry a line past the end of its line box (a
/// table cell or an inline-block sized to its text, a `pre-wrap` line
/// broken at the edge, a float beside it), the paragraph keeps those widths
/// rather than run into what is next to it.
pub(crate) fn apply_tab_stops(lines: &mut [ShapedLine], tab_lines: &[TabLine], stops: TabStops) {
    if !lines.iter().any(|line| line_has_tab(&line.items)) {
        return;
    }
    let expands = |index: usize| {
        stops.start_aligned && tab_lines.get(index).is_some_and(|line| line.left_to_right)
    };
    let fits = lines.iter().enumerate().all(|(index, line)| {
        let Some(metrics) = tab_lines.get(index) else {
            return true;
        };
        if !expands(index) || !line_has_tab(&line.items) {
            return true;
        }
        let limit = metrics.end + 0.01;
        let before = line_end(&line.items) - metrics.hanging;
        let mut items = line.items.clone();
        tab_line(&mut items, &stops, true);
        before > limit || line_end(&items) - metrics.hanging <= limit
    });
    for (index, line) in lines.iter_mut().enumerate() {
        if line_has_tab(&line.items) {
            tab_line(&mut line.items, &stops, fits && expands(index));
        }
    }
}

/// The variation axis values fontique chose for a parley run: when a font
/// is variable, a requested `font-weight`, `font-stretch` or `font-style` is
/// matched by setting its axes rather than by picking another file.
pub(crate) fn run_variations<B: parley::Brush>(run: &parley::Run<'_, B>) -> FontVariations {
    run.synthesis()
        .variation_settings()
        .iter()
        .map(|(tag, value)| (tag.to_be_bytes(), *value))
        .collect()
}

/// The bytes of a font file, shared without copying. Fonts found by Parley
/// keep the [`parley::FontData`] blob they were loaded into (a CJK collection
/// is tens of megabytes), and `@font-face` fonts keep their `Arc<Vec<u8>>`.
#[derive(Clone)]
pub struct FontBytes(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl FontBytes {
    /// Shares the blob Parley loaded the font into.
    pub fn from_blob(blob: &parley::fontique::Blob<u8>) -> Self {
        Self(blob.clone().into_raw_parts().0)
    }
}

impl std::ops::Deref for FontBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

impl AsRef<[u8]> for FontBytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl std::fmt::Debug for FontBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FontBytes({} bytes)", self.len())
    }
}

impl From<Vec<u8>> for FontBytes {
    fn from(data: Vec<u8>) -> Self {
        Self(Arc::new(data))
    }
}

impl From<Arc<Vec<u8>>> for FontBytes {
    fn from(data: Arc<Vec<u8>>) -> Self {
        Self(data)
    }
}

impl From<FontBytes> for krilla::Data {
    fn from(data: FontBytes) -> Self {
        data.0.into()
    }
}

#[cfg(test)]
mod font_bytes_tests {
    use super::FontBytes;

    #[test]
    fn from_blob_shares_the_font_data() {
        let blob = parley::fontique::Blob::from(vec![1u8, 2, 3]);
        let bytes = FontBytes::from_blob(&blob);
        assert_eq!(&*bytes, &[1, 2, 3]);
        assert!(std::ptr::eq(bytes.as_ptr(), blob.data().as_ptr()));
    }
}

#[derive(Clone, Debug)]
pub struct ShapedGlyphRun {
    pub font_data: FontBytes,
    pub font_index: u32,
    /// Variation axis values (e.g. `wght` = 700) that select the instance
    /// of a variable font. Empty for static fonts.
    pub variations: FontVariations,
    pub font_size: crate::units::Pt,
    pub color: [u8; 4], // RGBA
    pub decoration: TextDecoration,
    pub glyphs: Vec<ShapedGlyph>,
    pub text: Arc<str>,
    pub x_offset: crate::units::Pt,
    pub link: Option<Arc<LinkSpan>>,
    /// The language of the run's text when it is not the paragraph's
    /// (`<span lang="de">` in an English paragraph). A tagged PDF gives
    /// the run a marked-content sequence of its own with that `/Lang`.
    pub lang: Option<Arc<str>>,
    /// Whether `text` is the paragraph's own text, laid out by Parley, so the
    /// glyphs' text ranges give the run's place in reading order. A list
    /// marker put at the start of the first line has text of its own.
    pub from_paragraph_text: bool,
    /// The background colors of the inline elements the run is in (`<span>`,
    /// `<mark>`), outermost first, up to the inline root (whose background is
    /// its block's). Each is drawn behind the run's glyphs over the font's
    /// ascent and descent, as Chromium paints an inline box's content area.
    pub inline_backgrounds: Arc<[[u8; 4]]>,
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
    /// The index in the paragraph's text the box sits at, for reading order
    /// (see [`reading_order`]); `None` when it is not known.
    pub text_index: Option<usize>,
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

/// One inline element's background on a line: contiguous runs at the same
/// nesting depth with the same color, over the tallest ascent and descent.
struct BackgroundSpan {
    depth: usize,
    color: [u8; 4],
    x: f32,
    end: f32,
    top: f32,
    bottom: f32,
}

/// Draw the backgrounds of the inline elements on a line (`<span>`,
/// `<mark>`) behind its glyphs, outermost first. Each covers its runs'
/// advance and their fonts' ascent and descent, the content area Chromium
/// paints for an inline box. Adjacent runs of one element are merged, so no
/// seam shows between them.
fn draw_line_backgrounds(
    canvas: &mut Canvas<'_, '_>,
    items: &[LineItem],
    x: crate::units::Pt,
    baseline_y: crate::units::Pt,
) {
    let mut spans: Vec<BackgroundSpan> = Vec::new();
    for item in items {
        let LineItem::Text(run) = item else {
            continue;
        };
        if run.inline_backgrounds.is_empty() {
            continue;
        }
        let font_size = run.font_size.to_f32();
        let (ascent, descent) = skrifa::FontRef::from_index(run.font_data.as_ref(), run.font_index)
            .map(|font| {
                let metrics = font.metrics(
                    skrifa::instance::Size::new(font_size),
                    skrifa::instance::LocationRef::default(),
                );
                (metrics.ascent, -metrics.descent)
            })
            .unwrap_or((font_size * 0.8, font_size * 0.2));
        let run_x = (x + run.x_offset).to_f32();
        let run_end = run_x
            + run
                .glyphs
                .iter()
                .map(|g| g.x_advance * font_size)
                .sum::<f32>();
        let top = baseline_y.to_f32() - ascent;
        let bottom = baseline_y.to_f32() + descent;
        for (depth, &color) in run.inline_backgrounds.iter().enumerate() {
            let merged = spans.iter_mut().rev().find(|span| {
                span.depth == depth && span.color == color && (run_x - span.end).abs() < 0.5
            });
            match merged {
                Some(span) => {
                    span.end = run_end;
                    span.top = span.top.min(top);
                    span.bottom = span.bottom.max(bottom);
                }
                None => spans.push(BackgroundSpan {
                    depth,
                    color,
                    x: run_x,
                    end: run_end,
                    top,
                    bottom,
                }),
            }
        }
    }
    if spans.is_empty() {
        return;
    }
    spans.sort_by_key(|span| span.depth);
    canvas.as_artifact(|canvas| {
        for span in &spans {
            let Some(rect) = krilla::geom::Rect::from_ltrb(span.x, span.top, span.end, span.bottom)
            else {
                continue;
            };
            let [r, g, b, a] = span.color;
            canvas.surface.set_fill(Some(krilla::paint::Fill {
                paint: krilla::color::rgb::Color::new(r, g, b).into(),
                opacity: krilla::num::NormalizedF32::new(a as f32 / 255.0)
                    .unwrap_or(krilla::num::NormalizedF32::ONE),
                rule: Default::default(),
            }));
            canvas.surface.set_stroke(None);
            let mut path = krilla::geom::PathBuilder::new();
            path.push_rect(rect);
            if let Some(path) = path.finish() {
                canvas.surface.draw_path(&path);
            }
        }
    });
}

/// A contiguous span of runs sharing the same decoration attributes.
struct DecorationSpan {
    x: crate::units::Pt,
    width: crate::units::Pt,
    decoration: TextDecoration,
    /// Use metrics from the first run in the span
    font_data: FontBytes,
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
            font_data: run.font_data.clone(),
            font_index: run.font_index,
            font_size: run.font_size,
        });
    }

    if spans.is_empty() {
        return;
    }
    // Decorations are drawn after the line's items, so on a line that ends
    // in an inline box no run's sequence is open any more: they are
    // decoration, an artifact. Inside an open sequence (a run's, or the
    // paragraph's) they are drawn as part of it.
    canvas.as_artifact(|canvas| draw_decoration_spans(canvas, &spans, baseline_y));
}

fn draw_decoration_spans(
    canvas: &mut Canvas<'_, '_>,
    spans: &[DecorationSpan],
    baseline_y: crate::units::Pt,
) {
    for span in spans {
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

/// What a per-run tagged region is for: the link its content is under
/// (`Arc` identity pointer, `None` for non-link content) and the language
/// of its text when that is not the paragraph's.
#[derive(Clone, Debug, PartialEq)]
struct RegionKey {
    link: Option<usize>,
    lang: Option<Arc<str>>,
}

/// The places of a line's items in reading order, when that is not the
/// order they are drawn in: Parley gives a line's items in visual order,
/// left to right, which on a line with right-to-left text reads its runs
/// backwards (`שלוש שתיים אחת` for `אחת שתיים שלוש`). A text run's place is
/// the first byte of its text, an inline box's the text index it sits at,
/// before the text there. An image, a run with text of its own or a box
/// whose index is unknown (list markers) keeps the place of the item drawn
/// before it. `None` when
/// the items are already in reading order.
pub(crate) fn reading_order(items: &[LineItem]) -> Option<Vec<usize>> {
    let mut keys = Vec::with_capacity(items.len());
    let mut previous = (0, 0);
    let mut in_order = true;
    for item in items {
        let key = match item {
            LineItem::Text(run) if run.from_paragraph_text => run
                .glyphs
                .iter()
                .map(|g| g.text_range.start)
                .min()
                .map(|start| (start, 1)),
            LineItem::InlineBox(inline_box) => inline_box.text_index.map(|index| (index, 0)),
            LineItem::Text(_) | LineItem::Image(_) => None,
        }
        .unwrap_or(previous);
        in_order &= key >= previous;
        previous = key;
        keys.push(key);
    }
    if in_order {
        return None;
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&index| keys[index]);
    Some(order)
}

/// A record of a line in reading order that waits for the end of the line.
enum Deferred {
    Run(crate::draw_primitives::ParagraphRunItem),
    InlineBox {
        content_id: crate::drawables::NodeId,
        link: Option<usize>,
    },
}

/// A line whose items are drawn in another order than they are read.
struct ReorderedLine {
    /// The reading-order place of each item, by item index.
    places: Vec<usize>,
    /// The place of the item being drawn.
    place: usize,
    deferred: Vec<(usize, Deferred)>,
}

/// Tracks the currently-open per-run tagged region while `draw_shaped_lines`
/// walks the line items. Each transition between link spans (or to/from
/// non-link content, or to text in another language) closes the previous
/// region and opens a new one.
///
/// A line whose items are not drawn in reading order (right-to-left text)
/// gives each item a region of its own and records them, and its inline
/// boxes, in reading order when the line ends. The content stream keeps the
/// visual order, as Chromium writes it, so the page and text extraction by
/// content order stay as they are; the structure tree reads the line in
/// order.
struct RunRegionTracker {
    /// Paragraph NodeId under which `ParagraphRunItem`s are recorded.
    node_id: crate::drawables::NodeId,
    /// The key of the open region; `None` when no region is open.
    open_key: Option<RegionKey>,
    /// Identifier returned by the matching `start_tagged` call.
    open_identifier: Option<krilla::tagging::Identifier>,
    /// Set while drawing a line that is not in reading order.
    reordered: Option<ReorderedLine>,
}

impl RunRegionTracker {
    fn new(node_id: crate::drawables::NodeId) -> Self {
        Self {
            node_id,
            open_key: None,
            open_identifier: None,
            reordered: None,
        }
    }

    /// Start a line: when its items are not in reading order, each gets a
    /// region of its own, recorded in reading order by [`Self::end_line`].
    fn begin_line(&mut self, canvas: &mut Canvas<'_, '_>, items: &[LineItem]) {
        let Some(order) = reading_order(items) else {
            return;
        };
        self.close(canvas);
        let mut places = vec![0; items.len()];
        for (place, &index) in order.iter().enumerate() {
            places[index] = place;
        }
        self.reordered = Some(ReorderedLine {
            places,
            place: 0,
            deferred: Vec::new(),
        });
    }

    /// Start drawing item `index` of the line.
    fn begin_item(&mut self, canvas: &mut Canvas<'_, '_>, index: usize) {
        if self.reordered.is_none() {
            return;
        }
        self.close(canvas);
        if let Some(line) = self.reordered.as_mut() {
            line.place = line.places.get(index).copied().unwrap_or(index);
        }
    }

    /// Note an inline box drawn between the runs of the paragraph.
    fn note_inline_box(
        &mut self,
        canvas: &mut Canvas<'_, '_>,
        content_id: crate::drawables::NodeId,
        link: Option<usize>,
    ) {
        if let Some(line) = self.reordered.as_mut() {
            line.deferred
                .push((line.place, Deferred::InlineBox { content_id, link }));
        } else if let Some(tc) = canvas.tag_collector.as_mut() {
            tc.note_linked_inline_box(self.node_id, content_id, link);
        }
    }

    /// End a line: a line not in reading order closes its last region and
    /// records its regions and inline boxes in reading order.
    fn end_line(&mut self, canvas: &mut Canvas<'_, '_>) {
        if self.reordered.is_none() {
            return;
        }
        self.close(canvas);
        let Some(mut line) = self.reordered.take() else {
            return;
        };
        line.deferred.sort_by_key(|(place, _)| *place);
        let Some(tc) = canvas.tag_collector.as_mut() else {
            return;
        };
        for (_, record) in line.deferred {
            match record {
                Deferred::Run(item) => tc.record_run(self.node_id, item),
                Deferred::InlineBox { content_id, link } => {
                    tc.note_linked_inline_box(self.node_id, content_id, link)
                }
            }
        }
    }

    /// Switch to the region identified by `new_key`, opening/closing
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
    fn transition_to(&mut self, canvas: &mut Canvas<'_, '_>, new_key: RegionKey) {
        if self.open_key.as_ref() == Some(&new_key) {
            return;
        }
        self.close(canvas);
        if canvas.in_marked_content {
            // Inside a group tagged as one sequence: the link's text is
            // part of it, and the link gets a `Link` element there.
            if let (Some(ptr), Some(tc)) = (new_key.link, canvas.tag_collector.as_mut()) {
                tc.note_group_link(ptr);
            }
            return;
        }
        use krilla::tagging::{ContentTag, SpanTag};
        let id = canvas.surface.start_tagged(ContentTag::Span(
            SpanTag::empty().with_lang(new_key.lang.as_deref()),
        ));
        canvas.in_marked_content = true;
        self.open_key = Some(new_key);
        self.open_identifier = Some(id);
    }

    /// Close the currently-open region (if any) and record it as a
    /// `ParagraphRunItem` on the tag collector, or keep it for the end of a
    /// line not in reading order.
    fn close(&mut self, canvas: &mut Canvas<'_, '_>) {
        let Some(key) = self.open_key.take() else {
            return;
        };
        canvas.surface.end_tagged();
        canvas.in_marked_content = false;
        let id = self
            .open_identifier
            .take()
            .expect("open_identifier set whenever open_key is");
        let item = match key.link {
            Some(ptr) => crate::draw_primitives::ParagraphRunItem::LinkContent {
                span_ptr: ptr,
                identifier: id,
            },
            None => crate::draw_primitives::ParagraphRunItem::Content(id),
        };
        if let Some(line) = self.reordered.as_mut() {
            line.deferred.push((line.place, Deferred::Run(item)));
        } else if let Some(tc) = canvas.tag_collector.as_mut() {
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

        // Inline backgrounds go beneath the line's glyphs.
        draw_line_backgrounds(canvas, &line.items, x, baseline_y);
        if let Some(tracker) = run_region.as_mut() {
            tracker.begin_line(canvas, &line.items);
        }
        for (index, item) in line.items.iter().enumerate() {
            if let Some(tracker) = run_region.as_mut() {
                tracker.begin_item(canvas, index);
            }
            match item {
                LineItem::Text(run) => {
                    if let Some(tracker) = run_region.as_mut() {
                        tracker.transition_to(
                            canvas,
                            RegionKey {
                                link: link_span_ptr(run.link.as_ref()),
                                lang: run.lang.clone(),
                            },
                        );
                    }
                    // Create Krilla font from cached data
                    let data: krilla::Data = run.font_data.clone().into();
                    let coords: Vec<(krilla::text::Tag, f32)> = run
                        .variations
                        .iter()
                        .map(|(tag, value)| (krilla::text::Tag::new(tag), *value))
                        .collect();
                    let Some(font) =
                        krilla::text::Font::new_variable(data, run.font_index, &coords)
                    else {
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
                        tracker.transition_to(
                            canvas,
                            RegionKey {
                                link: link_span_ptr(img.link.as_ref()),
                                lang: None,
                            },
                        );
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
                        // The box's own element (a `Figure`, a `Span`) goes
                        // here in the paragraph's reading order, between
                        // the runs around it.
                        if let Some(content_id) = ib.node_id {
                            tracker.note_inline_box(
                                canvas,
                                content_id,
                                link_span_ptr(ib.link.as_ref()),
                            );
                        }
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
                        // A fractional opacity makes the box one group, and
                        // marked content opened inside a group is lost: tag
                        // the whole box as its content's, before the group.
                        let group_tag = crate::render::start_opacity_group_tagged(
                            canvas,
                            content_id,
                            ctx.drawables,
                            ib.opacity,
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
                        crate::render::finish_group_tagged(canvas, group_tag);
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
        if let Some(tracker) = run_region.as_mut() {
            tracker.end_line(canvas);
        }

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
            text_index: None,
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
            font_data: crate::paragraph::FontBytes::from(Vec::new()),
            font_index: 0,
            variations: Vec::new(),
            font_size: 10.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: Arc::from("hi"),
            x_offset: crate::units::Pt::ZERO,
            link: None,
            lang: None,
            from_paragraph_text: false,
            inline_backgrounds: Default::default(),
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
            text_index: None,
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
            font_data: crate::paragraph::FontBytes::from(Vec::new()),
            font_index: 0,
            variations: Vec::new(),
            font_size: 12.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: Arc::from(""),
            x_offset: crate::units::Pt::ZERO,
            link: None,
            lang: None,
            from_paragraph_text: false,
            inline_backgrounds: Default::default(),
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
                    text_index: None,
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
            font_data: crate::paragraph::FontBytes::from(Vec::new()),
            font_index: 0,
            variations: Vec::new(),
            font_size: 12.0_f32.as_pt(),
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: Vec::new(),
            text: Arc::from(""),
            x_offset: crate::units::Pt::ZERO,
            link: None,
            lang: None,
            from_paragraph_text: false,
            inline_backgrounds: Default::default(),
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

    fn noto_sans_regular() -> std::sync::Arc<Vec<u8>> {
        crate::test_fonts::noto_sans_regular_ttf()
    }

    /// Passing valid TTF bytes causes `skrifa::FontRef::from_index` to succeed,
    /// executing the main branch of `get_decoration_metrics` rather than the
    /// empty-data fallback that the existing tests exercise.
    #[test]
    fn real_font_enters_skrifa_main_branch() {
        let m = get_decoration_metrics(&noto_sans_regular(), 0, 12.0);
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
        let m = get_decoration_metrics(&noto_sans_regular(), 0, font_size);
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
        let m = get_decoration_metrics(&noto_sans_regular(), 0, font_size);
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
        let m = get_decoration_metrics(&noto_sans_regular(), 0, 12.0);
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
        let m1 = get_decoration_metrics(&noto_sans_regular(), 0, 12.0);
        let m2 = get_decoration_metrics(&noto_sans_regular(), 0, 24.0);
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
    /// Returns the serialized PDF bytes so callers can assert the document
    /// is non-empty, confirming the canvas pipeline completed successfully.
    fn with_canvas<F: FnOnce(&mut Canvas<'_, '_>)>(f: F) -> Vec<u8> {
        let mut doc = krilla::Document::new();
        let settings = krilla::page::PageSettings::from_wh(200.0, 200.0).expect("valid page size");
        {
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
        doc.finish().expect("document serialization must succeed")
    }

    /// Construct a minimal 10×10 pt `InlineImage` using the 1×1 red PNG fixture.
    /// Pass `visible: false` to exercise the invisible-image skip path.
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

    /// Construct a minimal 30×20 pt `InlineBoxItem`.
    /// Pass `visible: false` to exercise the invisible-inline-box skip path.
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
            text_index: None,
        }
    }

    // ── draw_decoration_line: Wavy + tiny thickness ──────────────────────────

    /// Covers lines 471-483: when the computed `half` (= `thickness * 2.0`) is
    /// less than 0.01, the wavy oscillation loop would run forever, so the guard
    /// falls back to a plain straight line.  A thickness of 0.001 gives
    /// `half = 0.002 < 0.01`.
    #[test]
    fn wavy_tiny_thickness_falls_back_to_straight_line() {
        let pdf = with_canvas(|canvas| {
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
        assert!(!pdf.is_empty());
    }

    /// Edge case: thickness exactly at the boundary (half == 0.01).  This should
    /// *not* trigger the guard (condition is strictly `< 0.01`) and therefore
    /// enters the normal wavy loop.
    #[test]
    fn wavy_boundary_thickness_enters_normal_loop() {
        let pdf = with_canvas(|canvas| {
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
        assert!(!pdf.is_empty());
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
            font_data: crate::paragraph::FontBytes::from(Vec::new()),
            font_index: 0,
            variations: Vec::new(),
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
            lang: None,
            from_paragraph_text: false,
            inline_backgrounds: Default::default(),
        };

        let items = vec![
            LineItem::Text(make_run(0.0)),  // ends at 50pt
            LineItem::Text(make_run(50.0)), // starts at 50pt → gap = 0 → merges
        ];

        let pdf = with_canvas(|canvas| {
            draw_line_decorations(canvas, &items, 0.0_f32.as_pt(), 12.0_f32.as_pt());
        });
        assert!(!pdf.is_empty());
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
        let pdf = with_canvas(|canvas| {
            draw_shaped_lines(canvas, &[line], 0.0_f32.as_pt(), 0.0_f32.as_pt(), None);
        });
        assert!(!pdf.is_empty());
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
        let pdf = with_canvas(|canvas| {
            draw_shaped_lines(canvas, &[line], 0.0_f32.as_pt(), 0.0_f32.as_pt(), None);
        });
        assert!(!pdf.is_empty());
    }
}
