//! Primitive geometry, canvas, style, and drawing-helper types.
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::image::ImageFormat;

/// Registry of block-level anchor destinations discovered during a pre-pass
/// walk of the paginated page tree.
///
/// Maps `id` → `(page_idx, y_pt)`. Later stages (link annotation emission)
/// consult this to resolve `href="#foo"` into a `GoToXYZ` action.
///
/// # Semantics
///
/// - **First-write-wins**: duplicate IDs in a document are invalid HTML, but
///   rather than crashing we keep the first occurrence and ignore subsequent
///   ones. This matches browser behavior for `getElementById`.
/// - **BTreeMap** for deterministic iteration ordering — see CLAUDE.md.
/// - **Pre-pass**: callers must `set_current_page(idx)` before each page's
///   `collect_ids` walk.
#[derive(Debug, Default)]
pub struct DestinationRegistry {
    current_page_idx: usize,
    entries: BTreeMap<String, (usize, Pt, Pt)>,
    /// Stack of transforms applied to coordinates before storing.
    transform_stack: Vec<Affine2D>,
}

impl DestinationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the page index to attach subsequent `record` calls to.
    pub fn set_current_page(&mut self, idx: usize) {
        self.current_page_idx = idx;
    }

    /// Push a transform onto the stack; subsequent `record` calls will
    /// transform coordinates through the composed stack before storing.
    pub fn push_transform(&mut self, m: Affine2D) {
        self.transform_stack.push(m);
    }

    /// Pop the most recent transform off the stack.
    ///
    /// No-op if the stack is empty (debug builds will assert).
    pub fn pop_transform(&mut self) {
        debug_assert!(
            !self.transform_stack.is_empty(),
            "DestinationRegistry::pop_transform called on empty stack"
        );
        self.transform_stack.pop();
    }

    /// Compose all stacked transforms into a single matrix.
    fn current_transform(&self) -> Affine2D {
        self.transform_stack
            .iter()
            .copied()
            .fold(Affine2D::IDENTITY, |acc, m| acc * m)
    }

    /// Record an anchor destination. First-write-wins: later duplicates are ignored.
    pub fn record(&mut self, id: &str, x: Pt, y: Pt) {
        let (tx, ty) = self.current_transform().transform_point(x, y);
        self.entries
            .entry(id.to_string())
            .or_insert((self.current_page_idx, tx, ty));
    }

    /// Look up a recorded anchor. Returns `(page_idx, x, y)`.
    pub fn get(&self, id: &str) -> Option<(usize, Pt, Pt)> {
        self.entries.get(id).copied()
    }
}

/// Point unit (1/72 inch)
pub type Pt = f32;

#[derive(Debug, Clone, Copy)]
pub struct Size {
    pub width: Pt,
    pub height: Pt,
}

/// 2×3 affine transformation matrix used for CSS `transform`.
///
/// Stored in column-vector convention:
///
/// ```text
/// | a  c  e |     | x |     | a*x + c*y + e |
/// | b  d  f |  *  | y |  =  | b*x + d*y + f |
/// | 0  0  1 |     | 1 |     |       1       |
/// ```
///
/// This matches `krilla::geom::Transform::from_row(a, b, c, d, e, f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2D {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine2D {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// ε tolerance for identity detection (absorbs trig float noise).
    const IDENTITY_EPS: f32 = 1e-5;

    pub fn is_identity(&self) -> bool {
        (self.a - 1.0).abs() < Self::IDENTITY_EPS
            && self.b.abs() < Self::IDENTITY_EPS
            && self.c.abs() < Self::IDENTITY_EPS
            && (self.d - 1.0).abs() < Self::IDENTITY_EPS
            && self.e.abs() < Self::IDENTITY_EPS
            && self.f.abs() < Self::IDENTITY_EPS
    }

    pub fn translation(tx: f32, ty: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn rotation(theta_rad: f32) -> Self {
        let (s, c) = theta_rad.sin_cos();
        Self {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: 0.0,
            f: 0.0,
        }
    }

    /// 2D skew. `ax_rad` is the x-axis skew angle, `ay_rad` is the y-axis skew.
    pub fn skew(ax_rad: f32, ay_rad: f32) -> Self {
        Self {
            a: 1.0,
            b: ay_rad.tan(),
            c: ax_rad.tan(),
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn to_krilla(&self) -> krilla::geom::Transform {
        krilla::geom::Transform::from_row(self.a, self.b, self.c, self.d, self.e, self.f)
    }

    /// Apply this affine transform to a 2D point.
    pub fn transform_point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Transform a `Rect` into a `Quad` by applying this matrix to each corner.
    ///
    /// The four corners of the input rect (in Y-down page coordinates) are
    /// transformed individually, preserving the krilla quad-point order:
    /// bottom-left → bottom-right → top-right → top-left.
    pub fn transform_rect(&self, r: &Rect) -> Quad {
        let x0 = r.x;
        let y0 = r.y;
        let x1 = r.x + r.width;
        let y1 = r.y + r.height;
        let bl = self.transform_point(x0, y1);
        let br = self.transform_point(x1, y1);
        let tr = self.transform_point(x1, y0);
        let tl = self.transform_point(x0, y0);
        Quad {
            points: [[bl.0, bl.1], [br.0, br.1], [tr.0, tr.1], [tl.0, tl.1]],
        }
    }
}

/// Matrix product `self * rhs`. Applied to a point `p`, this yields
/// `(self * rhs) * p = self * (rhs * p)`, i.e. `rhs` acts first.
impl std::ops::Mul for Affine2D {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self {
        Self {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }
}

/// A 2D point in user-space coordinates (Pt).
///
/// Used for both absolute draw positions and box-local offsets such as
/// `transform-origin`; the interpretation depends on the call site.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point2 {
    pub x: Pt,
    pub y: Pt,
}

impl Point2 {
    pub const fn new(x: Pt, y: Pt) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakBefore {
    Auto,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakAfter {
    Auto,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakInside {
    Auto,
    Avoid,
}

/// Axis-aligned rectangle used to describe PDF link activation areas.
///
/// Coordinates are in PDF points in the Krilla surface coordinate space
/// (origin at top-left, y growing downward) — i.e. the same space the
/// `draw()` methods use when talking to `Surface`. `x`, `y` mark the
/// top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Four-point quadrilateral for transformed link areas.
///
/// Point order follows krilla convention:
/// bottom-left → bottom-right → top-right → top-left (Y-down coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub points: [[f32; 2]; 4],
}

impl Quad {
    /// Returns `true` when the quad has collapsed to zero (or near-zero) area,
    /// e.g. after a `scaleX(0)` transform. Uses the cross product of two edge
    /// vectors originating from the bottom-left corner.
    pub fn is_degenerate(&self) -> bool {
        let ax = self.points[1][0] - self.points[0][0];
        let ay = self.points[1][1] - self.points[0][1];
        let bx = self.points[3][0] - self.points[0][0];
        let by = self.points[3][1] - self.points[0][1];
        (ax * by - ay * bx).abs() <= f32::EPSILON
    }

    /// Convert to krilla's `Quadrilateral` for PDF annotation emission.
    pub fn to_krilla(&self) -> krilla::geom::Quadrilateral {
        krilla::geom::Quadrilateral([
            krilla::geom::Point::from_xy(self.points[0][0], self.points[0][1]),
            krilla::geom::Point::from_xy(self.points[1][0], self.points[1][1]),
            krilla::geom::Point::from_xy(self.points[2][0], self.points[2][1]),
            krilla::geom::Point::from_xy(self.points[3][0], self.points[3][1]),
        ])
    }
}

/// One clickable link area captured by `LinkCollector` during draw.
///
/// A single `<a>` element may produce multiple quads when its glyphs wrap
/// across lines (or when nested inlines split shaping into multiple runs);
/// in that case `quads` holds one entry per fragment and `target`/`alt_text`
/// are the shared anchor metadata.
#[derive(Debug, Clone)]
pub struct LinkOccurrence {
    pub page_idx: usize,
    pub target: crate::paragraph::LinkTarget,
    pub alt_text: Option<String>,
    pub quads: Vec<Quad>,
    /// `Arc::as_ptr(link_span) as usize` — correlates with `TagCollector` run entries.
    pub span_ptr: usize,
}

/// Per-page collector of link activation rects, grouped by `<a>` identity.
///
/// Identity is the pointer value of `Arc<LinkSpan>`. `convert.rs` guarantees
/// that every glyph run / inline image extracted from the same `<a>`
/// shares the *same* `Arc<LinkSpan>` clone, so runs that were split by
/// `<em>`/`<strong>` inside an anchor merge into a single `LinkOccurrence`
/// with multiple rects. Distinct `<a href="...">` elements — even pointing
/// at the same URL — land in separate occurrences.
///
/// Occurrences are bucketed by `page_idx` so emission is O(L) per page
/// instead of O(P×L). Within a bucket, ordering is insertion order (the
/// draw order for that page), which is deterministic — what matters for
/// reproducible PDFs. The internal `HashMap` dedup index (keyed by
/// `(page_idx, Arc pointer)`) is never iterated for output, so it does
/// not violate CLAUDE.md's BTreeMap-for-output rule.
#[derive(Debug, Default)]
pub struct LinkCollector {
    current_page_idx: usize,
    /// Dedup index: `(page_idx, Arc pointer)` → index into the per-page
    /// Vec in `pages`. Stale entries for already-taken pages are harmless.
    index: std::collections::HashMap<(usize, usize), usize>,
    /// Occurrences grouped by `page_idx`. `BTreeMap` for deterministic
    /// iteration (CLAUDE.md rule) and cheap page-keyed removal.
    pages: BTreeMap<usize, Vec<LinkOccurrence>>,
    /// Stack of transforms applied to rects before storing as quads.
    transform_stack: Vec<Affine2D>,
}

impl LinkCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_current_page(&mut self, idx: usize) {
        self.current_page_idx = idx;
    }

    /// Push a transform onto the stack; subsequent `push_rect` calls will
    /// transform the rect through the composed stack before storing.
    pub fn push_transform(&mut self, m: Affine2D) {
        self.transform_stack.push(m);
    }

    /// Pop the most recent transform off the stack.
    ///
    /// No-op if the stack is empty (debug builds will assert).
    pub fn pop_transform(&mut self) {
        debug_assert!(
            !self.transform_stack.is_empty(),
            "LinkCollector::pop_transform called on empty stack"
        );
        self.transform_stack.pop();
    }

    /// Compose all stacked transforms into a single matrix.
    fn current_transform(&self) -> Affine2D {
        self.transform_stack
            .iter()
            .copied()
            .fold(Affine2D::IDENTITY, |acc, m| acc * m)
    }

    /// Record a rect for the given `<a>`. Rects pointing at the same
    /// `Arc<LinkSpan>` *on the same page* are merged into a single
    /// `LinkOccurrence`; on different pages they produce separate
    /// occurrences.
    pub fn push_rect(&mut self, link: &std::sync::Arc<crate::paragraph::LinkSpan>, rect: Rect) {
        // Skip degenerate rects (non-positive width or height) to match the
        // filtering the old `rect_to_quad` helper performed via
        // `KRect::from_xywh`, which rejects them.
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return;
        }
        let quad = self.current_transform().transform_rect(&rect);
        // Also reject quads that collapsed to zero area after transform
        // (e.g. scaleX(0)). Cross product of two edge vectors gives
        // twice the signed area of the parallelogram.
        if quad.is_degenerate() {
            return;
        }
        let page_idx = self.current_page_idx;
        let key = (page_idx, std::sync::Arc::as_ptr(link) as usize);
        let bucket = self.pages.entry(page_idx).or_default();
        if let Some(&i) = self.index.get(&key) {
            // Defensive check: if the index is stale (e.g. the page was
            // already drained via `take_page`), `i` may be out of range.
            if let Some(occ) = bucket.get_mut(i) {
                occ.quads.push(quad);
                return;
            }
        }
        let i = bucket.len();
        self.index.insert(key, i);
        bucket.push(LinkOccurrence {
            page_idx,
            target: link.target.clone(),
            alt_text: link.alt_text.clone(),
            quads: vec![quad],
            span_ptr: std::sync::Arc::as_ptr(link) as usize,
        });
    }

    /// Remove and return all occurrences recorded for `page_idx`.
    ///
    /// Pages are emitted in order during rendering, so after calling
    /// `take_page(n)` no further `push_rect` calls should target page `n`.
    /// Returns an empty `Vec` if the page had no link occurrences.
    pub fn take_page(&mut self, page_idx: usize) -> Vec<LinkOccurrence> {
        self.pages.remove(&page_idx).unwrap_or_default()
    }

    /// Consume the collector and return every occurrence across all pages,
    /// flattened in page-index order. Retained for testing.
    pub fn into_occurrences(self) -> Vec<LinkOccurrence> {
        self.pages.into_values().flatten().collect()
    }

    /// Return every occurrence across all pages as an owned Vec, in
    /// page-index order. Retained for testing — production callers should
    /// prefer `take_page` for O(L) per-page emission.
    pub fn occurrences(&self) -> Vec<LinkOccurrence> {
        self.pages.values().flatten().cloned().collect()
    }
}

/// Per-run tagged item for paragraphs drawn in per-run tagging mode.
///
/// Collected by `draw_shaped_lines` when `Canvas::link_run_node_id` is set.
/// `build_struct_tree` assembles these into `P` children, grouping consecutive
/// `LinkContent` items with the same `span_ptr` into `Link` TagGroups.
#[derive(Debug)]
pub enum ParagraphRunItem {
    /// Non-link content identifier (child of P/H/LBody directly).
    Content(krilla::tagging::Identifier),
    /// Link content identifier grouped by the `Arc<LinkSpan>` pointer.
    LinkContent {
        span_ptr: usize,
        identifier: krilla::tagging::Identifier,
    },
}

/// One entry recorded by [`TagCollector::record`] for whole-paragraph tagging:
/// `(node_id, pdf_tag, content_identifier, heading_title)`.
pub type TagEntry = (
    crate::drawables::NodeId,
    crate::tagging::PdfTag,
    krilla::tagging::Identifier,
    Option<String>,
);

/// Per-render accumulator for tagged-content identifiers.
///
/// `entries` stores one tuple per `surface.start_tagged` call made for
/// whole-paragraph tagging. `run_entries` stores per-run identifiers for
/// paragraphs that opened multiple regions (one per `<a>` link span). After
/// all pages are rendered, `render_v2` consumes both via [`Self::into_parts`]
/// and builds a `krilla::tagging::TagTree`.
pub struct TagCollector {
    entries: Vec<TagEntry>,
    run_entries: BTreeMap<crate::drawables::NodeId, Vec<ParagraphRunItem>>,
}

impl TagCollector {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            run_entries: BTreeMap::new(),
        }
    }

    pub fn record(
        &mut self,
        node_id: crate::drawables::NodeId,
        tag: crate::tagging::PdfTag,
        id: krilla::tagging::Identifier,
        heading_title: Option<String>,
    ) {
        self.entries.push((node_id, tag, id, heading_title));
    }

    /// Consume `self` and return the whole-paragraph entries and per-run
    /// entries as separate owned values. Both fields are needed independently
    /// by `build_struct_tree`; returning them together avoids the partial-move
    /// error that `tc.run_entries` followed by a `tc`-consuming call would
    /// cause.
    pub fn into_parts(
        self,
    ) -> (
        Vec<TagEntry>,
        BTreeMap<crate::drawables::NodeId, Vec<ParagraphRunItem>>,
    ) {
        (self.entries, self.run_entries)
    }

    /// Record a per-run item under `node_id` (paragraph NodeId).
    pub fn record_run(&mut self, node_id: crate::drawables::NodeId, item: ParagraphRunItem) {
        self.run_entries.entry(node_id).or_default().push(item);
    }

    /// Collect every `LinkContent::span_ptr` recorded across all paragraphs.
    /// `emit_link_annotations` uses this set to decide whether a given link
    /// occurrence is wired into the struct tree (and thus eligible for
    /// `add_tagged_annotation`).
    pub fn wired_link_span_ptrs(&self) -> std::collections::BTreeSet<usize> {
        self.run_entries
            .values()
            .flatten()
            .filter_map(|item| match item {
                ParagraphRunItem::LinkContent { span_ptr, .. } => Some(*span_ptr),
                ParagraphRunItem::Content(_) => None,
            })
            .collect()
    }
}

impl Default for TagCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrapper around Krilla Surface for drawing commands.
/// This decouples Pageable types from Krilla's concrete Surface type.
pub struct Canvas<'a, 'b> {
    pub surface: &'a mut krilla::surface::Surface<'b>,
    pub bookmark_collector: Option<&'a mut BookmarkCollector>,
    pub link_collector: Option<&'a mut LinkCollector>,
    pub tag_collector: Option<&'a mut TagCollector>,
    /// When `Some(node_id)` (a `drawables::NodeId`), `draw_shaped_lines`
    /// operates in per-run tagging mode: each glyph-run cluster bounded by
    /// `Arc<LinkSpan>` identity gets its own `start_tagged/end_tagged` region
    /// recorded as a `ParagraphRunItem` under that paragraph's NodeId.
    pub link_run_node_id: Option<crate::drawables::NodeId>,
}

/// Run a draw closure wrapped in opacity guards.
/// Skips drawing entirely if fully transparent (opacity == 0).
/// Wraps in a Krilla transparency group if partially transparent.
///
/// **Does NOT check visibility.** CSS `visibility: hidden` only hides
/// the element's own content (background, border, text) but children
/// with `visibility: visible` must still render. Container draw()
/// methods handle visibility themselves.
pub fn draw_with_opacity(
    canvas: &mut Canvas<'_, '_>,
    opacity: f32,
    f: impl FnOnce(&mut Canvas<'_, '_>),
) {
    if opacity == 0.0 {
        return;
    }
    let needs_opacity = opacity < 1.0;
    if needs_opacity {
        let nf =
            krilla::num::NormalizedF32::new(opacity).unwrap_or(krilla::num::NormalizedF32::ONE);
        canvas.surface.push_opacity(nf);
    }
    f(canvas);
    if needs_opacity {
        canvas.surface.pop();
    }
}

// ─── BlockStyle ──────────────────────────────────────────

/// A resolved single box-shadow value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoxShadow {
    /// Horizontal offset in points.
    pub offset_x: f32,
    /// Vertical offset in points.
    pub offset_y: f32,
    /// Blur radius in points. Currently unused for rendering (v0.4.5 draws blur=0).
    pub blur: f32,
    /// Spread radius in points. Negative values shrink the shadow.
    pub spread: f32,
    /// Shadow color as RGBA.
    pub color: [u8; 4],
    /// Whether this is an inset shadow. Currently unsupported (skipped at draw time).
    pub inset: bool,
}

/// Visual style for a block element.
#[derive(Clone, Debug, Default)]
pub struct BlockStyle {
    /// Background color as RGBA
    pub background_color: Option<[u8; 4]>,
    /// Background image layers (first = top-most, rendered in reverse order).
    pub background_layers: Vec<BackgroundLayer>,
    /// Border color as RGBA
    pub border_color: [u8; 4],
    /// Border widths: top, right, bottom, left
    pub border_widths: [f32; 4],
    /// Padding: top, right, bottom, left
    pub padding: [f32; 4],
    /// Border radii: [top-left, top-right, bottom-right, bottom-left] × [rx, ry]
    pub border_radii: [[f32; 2]; 4],
    /// Border styles: top, right, bottom, left
    pub border_styles: [BorderStyleValue; 4],
    /// `overflow-x` value
    pub overflow_x: Overflow,
    /// `overflow-y` value
    pub overflow_y: Overflow,
    /// Box shadows in CSS declaration order (first = top-most in paint stack).
    pub box_shadows: Vec<BoxShadow>,
}

/// CSS border-style values supported by fulgur.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderStyleValue {
    /// No border drawn
    None,
    /// Solid line (default when border-width > 0)
    #[default]
    Solid,
    /// Dashed line
    Dashed,
    /// Dotted line
    Dotted,
    /// Two parallel lines
    Double,
    /// 3D grooved effect
    Groove,
    /// 3D ridged effect
    Ridge,
    /// 3D inset effect
    Inset,
    /// 3D outset effect
    Outset,
}

/// CSS `overflow-x` / `overflow-y` value.
///
/// PDF は静的メディアなので、CSS の `hidden`/`clip`/`scroll`/`auto` はすべて
/// 「padding-box でクリップ」という同一の動作に統合する。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    /// `visible` — クリップしない (デフォルト)
    #[default]
    Visible,
    /// `hidden` / `clip` / `scroll` / `auto` — padding-box でクリップする
    Clip,
}

// ─── Background types ────────────────────────────────────

/// A length or percentage value for background positioning/sizing.
#[derive(Clone, Debug)]
pub enum BgLengthPercentage {
    /// Absolute length in points.
    Length(f32),
    /// Percentage (0.0–1.0).
    Percentage(f32),
}

/// CSS `background-size` value.
#[derive(Clone, Debug)]
pub enum BgSize {
    /// `auto` — use intrinsic image size.
    Auto,
    /// `cover` — scale to fill, may crop.
    Cover,
    /// `contain` — scale to fit, may letterbox.
    Contain,
    /// Explicit `<width> <height>`. `None` means `auto` for that axis.
    Explicit(Option<BgLengthPercentage>, Option<BgLengthPercentage>),
}

/// CSS `background-repeat` single-axis keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgRepeat {
    Repeat,
    NoRepeat,
    Space,
    Round,
}

/// CSS box model reference for `background-origin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgBox {
    BorderBox,
    PaddingBox,
    ContentBox,
}

/// CSS `background-clip` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgClip {
    BorderBox,
    PaddingBox,
    ContentBox,
    Text,
}

/// CSS gradient color stop の位置。
///
/// - `Fraction` は `%` 由来の比率値 (convert 時に `pct.0` として保持)。
///   convert は値域チェックを行わないため範囲外も入りうる。最終的な
///   範囲検証 (`[0, 1]` 外なら Layer drop) は draw 時の
///   `background::resolve_gradient_stops` が担う (CSS Images §3.5.1)。
/// - `LengthPx` は `<length>` 形式で記述された値 (例 `50px`)。draw 時に
///   gradient line 長さで割って fraction 化する。
/// - `Auto` は CSS auto。draw 時に CSS Images §3.5.1 fixup で前後の fixed
///   stop から補間される。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientStopPosition {
    Auto,
    Fraction(f32),
    LengthPx(f32),
}

/// A single color stop in a CSS gradient. Position は `GradientStopPosition`
/// で保持され、draw 時に gradient line 長さで fraction に解決される。
///
/// `is_hint=true` のときは CSS interpolation hint marker。`rgba` は無効値で
/// あり読まない契約。draw 段の `resolve_gradient_stops` が `expand_interpolation_hints`
/// を介して隣接 stop の色から N 個の中間 stop に展開する (CSS Images 3 §3.5.3)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub position: GradientStopPosition,
    pub rgba: [u8; 4],
    pub is_hint: bool,
}

/// CSS `to <h> <v>` corner direction. The four enumerated variants exhaust
/// the valid CSS combinations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearGradientCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Direction of a CSS `linear-gradient(...)` line.
///
/// Explicit angles (`30deg`) and the four cardinal `to <side>` keywords
/// resolve to a fixed angle at convert time. Corner keywords (`to top right`)
/// produce an angle that depends on the gradient box's aspect ratio per CSS
/// Images 3 §3.1.1, so they are resolved at draw time when the box
/// dimensions are known.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinearGradientDirection {
    /// CSS angle in radians: 0 = "to top", increasing clockwise.
    Angle(f32),
    Corner(LinearGradientCorner),
}

/// CSS `radial-gradient(<shape>?, ...)` の shape 部分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadialGradientShape {
    Circle,
    Ellipse,
}

/// CSS `radial-gradient(... <extent>, ...)` keyword。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadialExtent {
    ClosestSide,
    FarthestSide,
    ClosestCorner,
    FarthestCorner,
}

/// CSS `radial-gradient(<shape>? <size>?, ...)` の size 部分。
///
/// extent keyword は draw 時に gradient box から半径を計算する。
/// 明示半径も length-percentage を含むため draw 時に解決する。
#[derive(Clone, Debug)]
pub enum RadialGradientSize {
    Extent(RadialExtent),
    /// circle の場合は rx == ry とする。ellipse は独立。
    Explicit {
        rx: BgLengthPercentage,
        ry: BgLengthPercentage,
    },
}

/// Content payload for a background-image layer.
#[derive(Clone, Debug)]
pub enum BgImageContent {
    /// Raster image (PNG/JPEG/GIF) — rendered via krilla Image API.
    Raster {
        data: Arc<Vec<u8>>,
        format: ImageFormat,
    },
    /// SVG vector image — rendered via krilla-svg draw_svg.
    Svg { tree: Arc<usvg::Tree> },
    /// CSS `linear-gradient(...)` / `repeating-linear-gradient(...)`.
    /// `repeating=true` の場合、stops は draw 時に CSS Images 3 §3.6 の
    /// 周期展開 (period = last_pos - first_pos) を経て [0, 1] に正規化される。
    LinearGradient {
        direction: LinearGradientDirection,
        stops: Vec<GradientStop>,
        repeating: bool,
    },
    /// CSS `radial-gradient(...)` / `repeating-radial-gradient(...)`.
    /// position は origin rect 内の中心。`repeating` の意味は LinearGradient と同じ。
    RadialGradient {
        shape: RadialGradientShape,
        size: RadialGradientSize,
        position_x: BgLengthPercentage,
        position_y: BgLengthPercentage,
        stops: Vec<GradientStop>,
        repeating: bool,
    },
    /// CSS `conic-gradient(...)` / `repeating-conic-gradient(...)`.
    ///
    /// stops の position は convert 時に **生 fraction** として保持する
    /// (`<percentage>` はそのまま、`<angle>` は `angle / 2π`)。`[0, 1]` を
    /// 跨ぐ値 (例: `-30deg → -0.083`, `120% → 1.2`) もそのまま許容し、
    /// 最終的な範囲ハンドリングと repeating の周期展開は draw 時に行う。
    /// draw 経路は PostScript shading を使わず path wedge 分解で発行する
    /// ため PDF/A-1, A-2 適合 (`background.rs::draw_conic_gradient`)。
    ConicGradient {
        /// CSS `from <angle>` を radians で保持 (規約: 0=top, CW)。
        from_angle: f32,
        position_x: BgLengthPercentage,
        position_y: BgLengthPercentage,
        stops: Vec<GradientStop>,
        repeating: bool,
    },
}

/// A single CSS background image layer with all associated properties.
#[derive(Clone, Debug)]
pub struct BackgroundLayer {
    pub content: BgImageContent,
    pub intrinsic_width: f32,
    pub intrinsic_height: f32,
    pub size: BgSize,
    pub position_x: BgLengthPercentage,
    pub position_y: BgLengthPercentage,
    pub repeat_x: BgRepeat,
    pub repeat_y: BgRepeat,
    pub origin: BgBox,
    pub clip: BgClip,
}

impl BlockStyle {
    /// Whether any border radius is non-zero.
    pub fn has_radius(&self) -> bool {
        self.border_radii.iter().any(|r| r[0] > 0.0 || r[1] > 0.0)
    }

    /// Whether this style has any visual properties (background, border, or padding).
    pub fn has_visual_style(&self) -> bool {
        self.background_color.is_some()
            || !self.background_layers.is_empty()
            || self.border_widths.iter().any(|&w| w > 0.0)
            || self.padding.iter().any(|&p| p > 0.0)
            || !self.box_shadows.is_empty()
    }

    /// Returns (left_inset, top_inset) for content positioning inside border+padding.
    pub fn content_inset(&self) -> (f32, f32) {
        (
            self.border_widths[3] + self.padding[3],
            self.border_widths[0] + self.padding[0],
        )
    }

    /// Whether any axis has overflow clipping enabled.
    pub fn has_overflow_clip(&self) -> bool {
        self.overflow_x == Overflow::Clip || self.overflow_y == Overflow::Clip
    }

    /// Whether a node with this style requires its own draw entry.
    ///
    /// True when the node has any visual effect that must be rendered on its
    /// own surface — backgrounds/borders/padding (`has_visual_style`), a
    /// non-zero `border-radius` (`has_radius`), or overflow clipping
    /// (`has_overflow_clip`, which uses the node's box as the clip region).
    pub fn needs_block_wrapper(&self) -> bool {
        self.has_visual_style() || self.has_radius() || self.has_overflow_clip()
    }
}

// ─── BookmarkEntry / BookmarkCollector ──────────────────

/// One record captured by `BookmarkCollector` during draw.
#[derive(Debug, Clone, PartialEq)]
pub struct BookmarkEntry {
    pub page_idx: usize,
    pub y_pt: Pt,
    pub level: u8,
    pub label: String,
}

/// Shared, mutable collector threaded through `Canvas` during page
/// rendering. `render.rs` sets `current_page_idx` before drawing each page;
/// bookmark draw logic pushes an entry for each `<h1>`–`<h6>` marker.
#[derive(Debug, Default)]
pub struct BookmarkCollector {
    current_page_idx: usize,
    entries: Vec<BookmarkEntry>,
}

impl BookmarkCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_current_page(&mut self, idx: usize) {
        self.current_page_idx = idx;
    }

    pub fn record(&mut self, level: u8, label: String, y_pt: Pt) {
        self.entries.push(BookmarkEntry {
            page_idx: self.current_page_idx,
            y_pt,
            level,
            label,
        });
    }

    pub fn into_entries(self) -> Vec<BookmarkEntry> {
        self.entries
    }
}

// ─── Drawing helper functions ────────────────────────────

/// Build a rounded rectangle path using cubic Bézier approximation.
pub fn build_rounded_rect_path(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: &[[f32; 2]; 4],
) -> Option<krilla::geom::Path> {
    let mut pb = krilla::geom::PathBuilder::new();
    append_rounded_rect_subpath(&mut pb, x, y, w, h, radii);
    pb.finish()
}

/// Append a rounded rectangle as a subpath to an existing `PathBuilder`.
///
/// Useful for composing compound paths (e.g., ring shapes for box-shadow clipping).
/// The subpath is self-closing; the caller can continue adding subpaths after this returns.
pub(crate) fn append_rounded_rect_subpath(
    pb: &mut krilla::geom::PathBuilder,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radii: &[[f32; 2]; 4],
) {
    // Bézier approximation constant for quarter circle
    const KAPPA: f32 = 0.552_284_8;

    // CSS spec: if adjacent radii sum exceeds an edge, scale all radii proportionally.
    // Compute the minimum scale factor across all four edges.
    let scale = |a: f32, b: f32, edge: f32| -> f32 {
        let sum = a + b;
        if sum > edge && sum > 0.0 {
            edge / sum
        } else {
            1.0
        }
    };
    let f = scale(radii[0][0], radii[1][0], w) // top edge (rx)
        .min(scale(radii[1][1], radii[2][1], h)) // right edge (ry)
        .min(scale(radii[2][0], radii[3][0], w)) // bottom edge (rx)
        .min(scale(radii[3][1], radii[0][1], h)); // left edge (ry)

    let r: [[f32; 2]; 4] = [
        [radii[0][0] * f, radii[0][1] * f],
        [radii[1][0] * f, radii[1][1] * f],
        [radii[2][0] * f, radii[2][1] * f],
        [radii[3][0] * f, radii[3][1] * f],
    ];

    // Start at top-left corner (after radius)
    pb.move_to(x + r[0][0], y);

    // Top edge → top-right corner
    pb.line_to(x + w - r[1][0], y);
    if r[1][0] > 0.0 || r[1][1] > 0.0 {
        pb.cubic_to(
            x + w - r[1][0] * (1.0 - KAPPA),
            y,
            x + w,
            y + r[1][1] * (1.0 - KAPPA),
            x + w,
            y + r[1][1],
        );
    }

    // Right edge → bottom-right corner
    pb.line_to(x + w, y + h - r[2][1]);
    if r[2][0] > 0.0 || r[2][1] > 0.0 {
        pb.cubic_to(
            x + w,
            y + h - r[2][1] * (1.0 - KAPPA),
            x + w - r[2][0] * (1.0 - KAPPA),
            y + h,
            x + w - r[2][0],
            y + h,
        );
    }

    // Bottom edge → bottom-left corner
    pb.line_to(x + r[3][0], y + h);
    if r[3][0] > 0.0 || r[3][1] > 0.0 {
        pb.cubic_to(
            x + r[3][0] * (1.0 - KAPPA),
            y + h,
            x,
            y + h - r[3][1] * (1.0 - KAPPA),
            x,
            y + h - r[3][1],
        );
    }

    // Left edge → top-left corner
    pb.line_to(x, y + r[0][1]);
    if r[0][0] > 0.0 || r[0][1] > 0.0 {
        pb.cubic_to(
            x,
            y + r[0][1] * (1.0 - KAPPA),
            x + r[0][0] * (1.0 - KAPPA),
            y,
            x + r[0][0],
            y,
        );
    }

    pb.close();
}

/// Build a clip path for `overflow` based on the padding box.
///
/// - Returns `None` when both axes are `visible`, or when the padding box
///   collapses to zero/negative size.
/// - Axis-independent: a non-clipped axis uses a virtually unlimited range
///   (`±1e6`) so only the clipped axis is effectively bounded.
/// - `border-radius` is honored **only** when both axes are clipped. With
///   single-axis clipping, a plain rectangle is used (simplification).
pub(crate) fn compute_overflow_clip_path(
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Option<krilla::geom::Path> {
    if style.overflow_x == Overflow::Visible && style.overflow_y == Overflow::Visible {
        return None;
    }

    // padding-box = border-box inset by border widths (top, right, bottom, left)
    let bw = &style.border_widths;
    let pb_x = x + bw[3];
    let pb_y = y + bw[0];
    let pb_w = w - bw[1] - bw[3];
    let pb_h = h - bw[0] - bw[2];

    // Non-clipped axes extend to effectively unlimited range so only the
    // clipped axis is actually bounded. We intentionally do NOT bail out on
    // `pb_w <= 0 || pb_h <= 0` here: a collapsed non-clipped axis is fine
    // because it will be expanded to `±INFINITE` below. Only if a *clipped*
    // axis has zero/negative size should we skip the clip (the final
    // `cw <= 0 || ch <= 0` check below handles that).
    const INFINITE: f32 = 1.0e6;
    let (cx, cw) = if style.overflow_x == Overflow::Clip {
        (pb_x, pb_w)
    } else {
        (pb_x - INFINITE, pb_w + 2.0 * INFINITE)
    };
    let (cy, ch) = if style.overflow_y == Overflow::Clip {
        (pb_y, pb_h)
    } else {
        (pb_y - INFINITE, pb_h + 2.0 * INFINITE)
    };

    if cw <= 0.0 || ch <= 0.0 {
        return None;
    }

    let both_axes = style.overflow_x == Overflow::Clip && style.overflow_y == Overflow::Clip;
    let has_radius = style.has_radius();

    if both_axes && has_radius {
        let inner_radii = compute_padding_box_inner_radii(&style.border_radii, bw);
        build_rounded_rect_path(cx, cy, cw, ch, &inner_radii)
    } else {
        build_overflow_rect_path(cx, cy, cw, ch)
    }
}

/// Axis-aligned rectangle path for overflow clipping.
///
/// `background.rs` has a private equivalent (`build_rect_path`); we keep a
/// local copy here rather than making that one `pub(crate)` because overflow
/// clipping is conceptually independent of background drawing.
fn build_overflow_rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<krilla::geom::Path> {
    let mut pb = krilla::geom::PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x + w, y);
    pb.line_to(x + w, y + h);
    pb.line_to(x, y + h);
    pb.close();
    pb.finish()
}

/// Convert border-box (outer) radii to padding-box (inner) radii.
///
/// CSS spec (`border-radius` interaction with `overflow`):
/// `inner_r = max(0, outer_r - border_width_on_that_side)`.
///
/// * `outer` layout: `[top-left, top-right, bottom-right, bottom-left] × [rx, ry]`
/// * `borders` layout: `[top, right, bottom, left]`
fn compute_padding_box_inner_radii(outer: &[[f32; 2]; 4], borders: &[f32; 4]) -> [[f32; 2]; 4] {
    let [bt, br, bb, bl] = *borders;
    [
        [(outer[0][0] - bl).max(0.0), (outer[0][1] - bt).max(0.0)], // top-left
        [(outer[1][0] - br).max(0.0), (outer[1][1] - bt).max(0.0)], // top-right
        [(outer[2][0] - br).max(0.0), (outer[2][1] - bb).max(0.0)], // bottom-right
        [(outer[3][0] - bl).max(0.0), (outer[3][1] - bb).max(0.0)], // bottom-left
    ]
}

/// Lighten an RGBA color by a factor (0.0–1.0). Higher factor = lighter.
fn lighten_color(c: &[u8; 4], factor: f32) -> [u8; 4] {
    [
        (c[0] as f32 + (255.0 - c[0] as f32) * factor) as u8,
        (c[1] as f32 + (255.0 - c[1] as f32) * factor) as u8,
        (c[2] as f32 + (255.0 - c[2] as f32) * factor) as u8,
        c[3],
    ]
}

/// Darken an RGBA color by a factor (0.0–1.0). Higher factor = darker.
fn darken_color(c: &[u8; 4], factor: f32) -> [u8; 4] {
    [
        (c[0] as f32 * (1.0 - factor)) as u8,
        (c[1] as f32 * (1.0 - factor)) as u8,
        (c[2] as f32 * (1.0 - factor)) as u8,
        c[3],
    ]
}

/// For 3D border styles, determine the light and dark colors for a given side.
/// Returns (outer_color, inner_color) for groove/ridge, or just the single color for inset/outset.
/// `is_top_or_left`: true for top/left sides, false for bottom/right sides.
fn border_3d_colors(
    base: &[u8; 4],
    style: BorderStyleValue,
    is_top_or_left: bool,
) -> ([u8; 4], Option<[u8; 4]>) {
    let light = lighten_color(base, 0.5);
    let dark = darken_color(base, 0.5);
    match style {
        BorderStyleValue::Groove => {
            if is_top_or_left {
                (dark, Some(light))
            } else {
                (light, Some(dark))
            }
        }
        BorderStyleValue::Ridge => {
            if is_top_or_left {
                (light, Some(dark))
            } else {
                (dark, Some(light))
            }
        }
        BorderStyleValue::Inset => {
            if is_top_or_left {
                (dark, None)
            } else {
                (light, None)
            }
        }
        BorderStyleValue::Outset => {
            if is_top_or_left {
                (light, None)
            } else {
                (dark, None)
            }
        }
        _ => (*base, None),
    }
}

/// Apply border-style dash settings to a stroke.
fn apply_border_style(
    stroke: krilla::paint::Stroke,
    style: BorderStyleValue,
    width: f32,
) -> Option<krilla::paint::Stroke> {
    match style {
        BorderStyleValue::None => None,
        BorderStyleValue::Solid => Some(stroke),
        BorderStyleValue::Dashed => {
            let dash_len = width * 3.0;
            Some(krilla::paint::Stroke {
                dash: Some(krilla::paint::StrokeDash {
                    array: vec![dash_len, dash_len],
                    offset: 0.0,
                }),
                ..stroke
            })
        }
        BorderStyleValue::Dotted => Some(krilla::paint::Stroke {
            line_cap: krilla::paint::LineCap::Round,
            dash: Some(krilla::paint::StrokeDash {
                array: vec![0.0, width * 2.0],
                offset: 0.0,
            }),
            ..stroke
        }),
        // NOTE: `Double` here is the solid-stroke fallback used by
        // draw_border_line / draw_block_border when width < 3 (CSS Backgrounds L3).
        // Returning `None` for Double would silently break that fallback.
        BorderStyleValue::Double
        | BorderStyleValue::Groove
        | BorderStyleValue::Ridge
        | BorderStyleValue::Inset
        | BorderStyleValue::Outset => Some(stroke), // handled specially at call site
    }
}

/// Helper to draw a simple line segment with a given stroke.
pub(crate) fn stroke_line(
    canvas: &mut Canvas<'_, '_>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    stroke: krilla::paint::Stroke,
) {
    canvas.surface.set_stroke(Some(stroke));
    let mut pb = krilla::geom::PathBuilder::new();
    pb.move_to(x1, y1);
    pb.line_to(x2, y2);
    if let Some(path) = pb.finish() {
        canvas.surface.draw_path(&path);
    }
}

/// Returns `None` for non-positive width or height (krilla rejects degenerate rects).
fn build_rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<krilla::geom::Path> {
    let rect = krilla::geom::Rect::from_xywh(x, y, w, h)?;
    let mut pb = krilla::geom::PathBuilder::new();
    pb.push_rect(rect);
    pb.finish()
}

/// Stroke the rectangle inset on all sides by `inset`.
/// One `draw_path` call emits a single closed subpath (m+3l+h in krilla;
/// future `re`), replacing 4 abutting `stroke_line` calls.
fn stroke_inset_rect(
    canvas: &mut Canvas<'_, '_>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    inset: f32,
    stroke: krilla::paint::Stroke,
) {
    let path = build_rect_path(
        x + inset,
        y + inset,
        (w - inset * 2.0).max(0.0),
        (h - inset * 2.0).max(0.0),
    );
    if let Some(path) = path {
        canvas.surface.set_stroke(Some(stroke));
        canvas.surface.draw_path(&path);
    }
}

pub(crate) fn alpha_to_opacity(alpha: u8) -> krilla::num::NormalizedF32 {
    krilla::num::NormalizedF32::new(alpha as f32 / 255.0).unwrap_or(krilla::num::NormalizedF32::ONE)
}

/// Create a stroke with a specific color and width, inheriting opacity from base.
pub(crate) fn colored_stroke(
    color: &[u8; 4],
    width: f32,
    opacity: krilla::num::NormalizedF32,
) -> krilla::paint::Stroke {
    krilla::paint::Stroke {
        paint: krilla::color::rgb::Color::new(color[0], color[1], color[2]).into(),
        width,
        opacity,
        ..Default::default()
    }
}

/// Draw a single border line with style, handling double and 3D effects.
/// `base_color` is the original RGBA border color (needed for 3D color computation).
/// `is_top_or_left` determines the light/dark color assignment for 3D styles.
/// `outward_sign` is +1.0 if the computed normal (-dy,dx) points outward, -1.0 if inward.
#[allow(clippy::too_many_arguments)]
fn draw_border_line(
    canvas: &mut Canvas<'_, '_>,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    width: f32,
    style: BorderStyleValue,
    base_color: &[u8; 4],
    opacity: krilla::num::NormalizedF32,
    is_top_or_left: bool,
    outward_sign: f32,
) {
    if width <= 0.0 || style == BorderStyleValue::None {
        return;
    }

    match style {
        // CSS Backgrounds L3: border-width < 3 の double は solid として描画。
        BorderStyleValue::Double if width >= 3.0 => {
            let gap = width / 3.0;
            let dx = x2 - x1;
            let dy = y2 - y1;
            let len = (dx * dx + dy * dy).sqrt();
            if len == 0.0 {
                return;
            }
            let nx = -dy / len * gap;
            let ny = dx / len * gap;
            let thin = colored_stroke(base_color, width / 3.0, opacity);
            stroke_line(canvas, x1 + nx, y1 + ny, x2 + nx, y2 + ny, thin.clone());
            stroke_line(canvas, x1 - nx, y1 - ny, x2 - nx, y2 - ny, thin);
        }
        BorderStyleValue::Groove | BorderStyleValue::Ridge => {
            let (outer_color, inner_color) = border_3d_colors(base_color, style, is_top_or_left);
            let inner_color = inner_color.unwrap_or(outer_color);
            let dx = x2 - x1;
            let dy = y2 - y1;
            let len = (dx * dx + dy * dy).sqrt();
            if len == 0.0 {
                return;
            }
            let half = width / 4.0;
            let nx = -dy / len * half;
            let ny = dx / len * half;
            let half_w = width / 2.0;
            let inward_sign = -outward_sign;
            stroke_line(
                canvas,
                x1 + nx * outward_sign,
                y1 + ny * outward_sign,
                x2 + nx * outward_sign,
                y2 + ny * outward_sign,
                colored_stroke(&outer_color, half_w, opacity),
            );
            stroke_line(
                canvas,
                x1 + nx * inward_sign,
                y1 + ny * inward_sign,
                x2 + nx * inward_sign,
                y2 + ny * inward_sign,
                colored_stroke(&inner_color, half_w, opacity),
            );
        }
        BorderStyleValue::Inset | BorderStyleValue::Outset => {
            let (color, _) = border_3d_colors(base_color, style, is_top_or_left);
            stroke_line(
                canvas,
                x1,
                y1,
                x2,
                y2,
                colored_stroke(&color, width, opacity),
            );
        }
        _ => {
            let base = colored_stroke(base_color, width, opacity);
            if let Some(styled) = apply_border_style(base, style, width) {
                stroke_line(canvas, x1, y1, x2, y2, styled);
            }
        }
    }
}

pub(crate) fn draw_block_border(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let [bt, br, bb, bl] = style.border_widths;
    let [st, sr, sb, sl] = style.border_styles;
    if !(bt > 0.0 || br > 0.0 || bb > 0.0 || bl > 0.0) {
        return;
    }
    let bc = &style.border_color;

    let uniform_width = bt == br && br == bb && bb == bl;
    let uniform_style = st == sr && sr == sb && sb == sl;
    if style.has_radius() && uniform_width && uniform_style && st != BorderStyleValue::None {
        let inset = bt / 2.0;
        let inset_radii = style
            .border_radii
            .map(|[rx, ry]| [(rx - inset).max(0.0), (ry - inset).max(0.0)]);
        if let Some(path) = build_rounded_rect_path(
            x + inset,
            y + inset,
            w - inset * 2.0,
            h - inset * 2.0,
            &inset_radii,
        ) {
            let base = krilla::paint::Stroke {
                paint: krilla::color::rgb::Color::new(bc[0], bc[1], bc[2]).into(),
                width: bt,
                opacity: alpha_to_opacity(bc[3]),
                ..Default::default()
            };
            if let Some(styled) = apply_border_style(base, st, bt) {
                canvas.surface.set_fill(None);
                canvas.surface.set_stroke(Some(styled));
                canvas.surface.draw_path(&path);
                canvas.surface.set_stroke(None);
            }
        }
    } else if !style.has_radius()
        && uniform_width
        && uniform_style
        && matches!(st, BorderStyleValue::Solid | BorderStyleValue::Double)
    {
        let opacity = alpha_to_opacity(bc[3]);
        canvas.surface.set_fill(None);

        // CSS Backgrounds L3: border-width < 3 の double は solid として描画。
        if st == BorderStyleValue::Double && bt >= 3.0 {
            // Double = 3 equal bands (border/gap/border): thin_w = bt/3.
            // Stroke centerlines: outer at bt/6, inner at bt*5/6.
            let thin_w = bt / 3.0;
            let stroke_thin = colored_stroke(bc, thin_w, opacity);
            stroke_inset_rect(canvas, x, y, w, h, thin_w / 2.0, stroke_thin.clone());
            stroke_inset_rect(canvas, x, y, w, h, bt - thin_w / 2.0, stroke_thin);
        } else {
            let base = colored_stroke(bc, bt, opacity);
            if let Some(styled) = apply_border_style(base, st, bt) {
                stroke_inset_rect(canvas, x, y, w, h, bt / 2.0, styled);
            }
        }
        canvas.surface.set_stroke(None);
    } else {
        let opacity = alpha_to_opacity(bc[3]);
        canvas.surface.set_fill(None);

        // top: normal=(0,+half) points down=inward, so outward_sign=-1
        draw_border_line(
            canvas,
            x,
            y + bt / 2.0,
            x + w,
            y + bt / 2.0,
            bt,
            st,
            bc,
            opacity,
            true,
            -1.0,
        );
        // bottom (top_or_left = false)
        draw_border_line(
            canvas,
            x,
            y + h - bb / 2.0,
            x + w,
            y + h - bb / 2.0,
            bb,
            sb,
            bc,
            opacity,
            false,
            1.0, // bottom: normal=(0,+half) points down=outward
        );
        // left: normal=(-half,0) points left=outward, so outward_sign=+1
        draw_border_line(
            canvas,
            x + bl / 2.0,
            y,
            x + bl / 2.0,
            y + h,
            bl,
            sl,
            bc,
            opacity,
            true,
            1.0,
        );
        // right: normal=(-half,0) points left=inward, so outward_sign=-1
        draw_border_line(
            canvas,
            x + w - br / 2.0,
            y,
            x + w - br / 2.0,
            y + h,
            br,
            sr,
            bc,
            opacity,
            false,
            -1.0, // right: outward_sign=-1
        );

        canvas.surface.set_stroke(None);
    }
}

// ─── clamp_marker_size ───────────────────────────────────

/// Clamp an intrinsic image size to a line-height limit while preserving
/// the aspect ratio. Used to size list-style-image markers so they match
/// the surrounding text's line-height.
///
/// Returns `(width, height)` in pt. If the intrinsic height is zero, both
/// return values are zero (avoids division by zero for malformed images).
pub(crate) fn clamp_marker_size(
    intrinsic_width: Pt,
    intrinsic_height: Pt,
    line_height: Pt,
) -> (Pt, Pt) {
    if intrinsic_height <= 0.0 {
        return (0.0, 0.0);
    }
    if intrinsic_height <= line_height {
        (intrinsic_width, intrinsic_height)
    } else {
        let scale = line_height / intrinsic_height;
        (intrinsic_width * scale, line_height)
    }
}

#[cfg(test)]
mod dp_unit_tests {
    use super::*;
    use std::sync::Arc;

    // ── DestinationRegistry transform stack ─────────────────

    #[test]
    fn destination_registry_push_pop_transform_affects_record() {
        let mut reg = DestinationRegistry::new();
        reg.set_current_page(3);
        reg.push_transform(Affine2D::translation(10.0, 20.0));
        reg.record("anchor", 5.0, 7.0);
        let (page, x, y) = reg.get("anchor").expect("recorded");
        assert_eq!(page, 3);
        assert!((x - 15.0).abs() < 1e-4);
        assert!((y - 27.0).abs() < 1e-4);
        reg.pop_transform();
        // After pop, subsequent records use identity.
        reg.record("anchor2", 1.0, 2.0);
        let (_, x2, y2) = reg.get("anchor2").expect("recorded");
        assert!((x2 - 1.0).abs() < 1e-4);
        assert!((y2 - 2.0).abs() < 1e-4);
    }

    // ── LinkCollector ──────────────────────────────────────

    fn make_test_link() -> Arc<crate::paragraph::LinkSpan> {
        Arc::new(crate::paragraph::LinkSpan {
            target: crate::paragraph::LinkTarget::External(Arc::new(
                "https://example.com".to_string(),
            )),
            alt_text: None,
        })
    }

    #[test]
    fn link_collector_into_occurrences_and_occurrences() {
        let mut collector = LinkCollector::new();
        let link = make_test_link();
        collector.set_current_page(0);
        collector.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
        );

        let occ_borrowed = collector.occurrences();
        assert_eq!(occ_borrowed.len(), 1);
        assert_eq!(occ_borrowed[0].page_idx, 0);
        assert_eq!(occ_borrowed[0].quads.len(), 1);

        let occ_owned = collector.into_occurrences();
        assert_eq!(occ_owned.len(), 1);
    }

    #[test]
    fn link_collector_push_rect_skips_zero_width_rect() {
        let mut collector = LinkCollector::new();
        let link = make_test_link();
        collector.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 10.0,
            },
        );
        collector.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 0.0,
            },
        );
        assert!(collector.occurrences().is_empty());
    }

    #[test]
    fn link_collector_push_rect_skips_degenerate_quad_after_transform() {
        let mut collector = LinkCollector::new();
        let link = make_test_link();
        // scaleX(0) collapses width to zero → degenerate quad.
        collector.push_transform(Affine2D::scale(0.0, 1.0));
        collector.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
        );
        collector.pop_transform();
        assert!(collector.occurrences().is_empty());
    }

    // ── compute_overflow_clip_path branches ─────────────────

    #[test]
    fn compute_overflow_clip_visible_visible_returns_none() {
        let style = BlockStyle {
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            ..Default::default()
        };
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_none());
    }

    #[test]
    fn compute_overflow_clip_x_clip_y_visible_returns_path() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Visible,
            border_widths: [1.0, 1.0, 1.0, 1.0],
            ..Default::default()
        };
        // y axis non-clip → expanded to ±INFINITE (line 988).
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_some());
    }

    #[test]
    fn compute_overflow_clip_both_axes_with_radius_uses_rounded_path() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Clip,
            border_widths: [2.0, 2.0, 2.0, 2.0],
            border_radii: [[8.0, 8.0]; 4],
            ..Default::default()
        };
        // both axes clipped + has_radius → rounded path branch (lines 999-1000).
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_some());
    }

    #[test]
    fn compute_overflow_clip_zero_size_axis_returns_none() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Clip,
            border_widths: [50.0, 50.0, 50.0, 50.0],
            ..Default::default()
        };
        // border-widths exceed the box → cw / ch ≤ 0 → returns None.
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 80.0, 80.0).is_none());
    }

    // ── compute_padding_box_inner_radii (private) ───────────

    #[test]
    fn padding_box_inner_radii_subtracts_borders_with_floor_zero() {
        let outer = [[10.0, 12.0], [8.0, 6.0], [4.0, 4.0], [20.0, 14.0]];
        let borders = [3.0, 5.0, 2.0, 7.0]; // top, right, bottom, left
        let inner = compute_padding_box_inner_radii(&outer, &borders);
        // top-left: outer[0] - [bl, bt] = [10-7=3, 12-3=9]
        assert!((inner[0][0] - 3.0).abs() < 1e-5);
        assert!((inner[0][1] - 9.0).abs() < 1e-5);
        // top-right: outer[1] - [br, bt] = [8-5=3, 6-3=3]
        assert!((inner[1][0] - 3.0).abs() < 1e-5);
        assert!((inner[1][1] - 3.0).abs() < 1e-5);
        // bottom-right: outer[2] - [br, bb] = [4-5=-1→0, 4-2=2]
        assert!(inner[2][0].abs() < 1e-5);
        assert!((inner[2][1] - 2.0).abs() < 1e-5);
        // bottom-left: outer[3] - [bl, bb] = [20-7=13, 14-2=12]
        assert!((inner[3][0] - 13.0).abs() < 1e-5);
        assert!((inner[3][1] - 12.0).abs() < 1e-5);
    }

    // ── lighten_color / darken_color (private) ──────────────

    #[test]
    fn lighten_color_blends_toward_white_alpha_preserved() {
        let lightened = lighten_color(&[100, 200, 50, 128], 0.5);
        // blend_to_white(c, 0.5) = c + (255-c)*0.5
        assert_eq!(lightened, [(100 + (255 - 100) / 2) as u8, 227, 152, 128]);
    }

    #[test]
    fn darken_color_blends_toward_black_alpha_preserved() {
        let darkened = darken_color(&[100, 200, 50, 200], 0.5);
        assert_eq!(darkened, [50, 100, 25, 200]);
    }

    // ── border_3d_colors (private) — all 4 styles, both sides ──

    #[test]
    fn border_3d_colors_groove_top_left_dark_outer_light_inner() {
        let base = [128, 128, 128, 255];
        let (outer, inner) = border_3d_colors(&base, BorderStyleValue::Groove, true);
        let dark = darken_color(&base, 0.5);
        let light = lighten_color(&base, 0.5);
        assert_eq!(outer, dark);
        assert_eq!(inner, Some(light));
    }

    #[test]
    fn border_3d_colors_groove_bottom_right_light_outer_dark_inner() {
        let base = [128, 128, 128, 255];
        let (outer, inner) = border_3d_colors(&base, BorderStyleValue::Groove, false);
        let dark = darken_color(&base, 0.5);
        let light = lighten_color(&base, 0.5);
        assert_eq!(outer, light);
        assert_eq!(inner, Some(dark));
    }

    #[test]
    fn border_3d_colors_ridge_inverts_groove() {
        let base = [128, 128, 128, 255];
        let (outer_tl, inner_tl) = border_3d_colors(&base, BorderStyleValue::Ridge, true);
        let dark = darken_color(&base, 0.5);
        let light = lighten_color(&base, 0.5);
        assert_eq!(outer_tl, light);
        assert_eq!(inner_tl, Some(dark));

        let (outer_br, inner_br) = border_3d_colors(&base, BorderStyleValue::Ridge, false);
        assert_eq!(outer_br, dark);
        assert_eq!(inner_br, Some(light));
    }

    #[test]
    fn border_3d_colors_inset_top_left_dark_no_inner() {
        let base = [128, 128, 128, 255];
        let (outer, inner) = border_3d_colors(&base, BorderStyleValue::Inset, true);
        assert_eq!(outer, darken_color(&base, 0.5));
        assert!(inner.is_none());
    }

    #[test]
    fn border_3d_colors_inset_bottom_right_light_no_inner() {
        let base = [128, 128, 128, 255];
        let (outer, inner) = border_3d_colors(&base, BorderStyleValue::Inset, false);
        assert_eq!(outer, lighten_color(&base, 0.5));
        assert!(inner.is_none());
    }

    #[test]
    fn border_3d_colors_outset_top_left_light_no_inner() {
        let base = [128, 128, 128, 255];
        let (outer, inner) = border_3d_colors(&base, BorderStyleValue::Outset, true);
        assert_eq!(outer, lighten_color(&base, 0.5));
        assert!(inner.is_none());
    }

    #[test]
    fn border_3d_colors_outset_bottom_right_dark_no_inner() {
        let base = [128, 128, 128, 255];
        let (outer, inner) = border_3d_colors(&base, BorderStyleValue::Outset, false);
        assert_eq!(outer, darken_color(&base, 0.5));
        assert!(inner.is_none());
    }

    #[test]
    fn border_3d_colors_solid_returns_base_no_inner() {
        let base = [50, 60, 70, 200];
        let (outer, inner) = border_3d_colors(&base, BorderStyleValue::Solid, true);
        assert_eq!(outer, base);
        assert!(inner.is_none());
    }

    // ── clamp_marker_size edge cases ─────────────────────────

    #[test]
    fn clamp_marker_size_zero_height_returns_zero_zero() {
        let (w, h) = clamp_marker_size(20.0, 0.0, 12.0);
        assert_eq!(w, 0.0);
        assert_eq!(h, 0.0);
    }

    #[test]
    fn clamp_marker_size_negative_height_returns_zero_zero() {
        let (w, h) = clamp_marker_size(20.0, -5.0, 12.0);
        assert_eq!(w, 0.0);
        assert_eq!(h, 0.0);
    }

    #[test]
    fn clamp_marker_size_within_line_height_passes_through() {
        let (w, h) = clamp_marker_size(20.0, 10.0, 12.0);
        assert_eq!(w, 20.0);
        assert_eq!(h, 10.0);
    }

    #[test]
    fn clamp_marker_size_oversized_scales_down_preserving_aspect() {
        // intrinsic 40×20, line_height 10 → scale = 0.5 → (20, 10).
        let (w, h) = clamp_marker_size(40.0, 20.0, 10.0);
        assert!((w - 20.0).abs() < 1e-5);
        assert!((h - 10.0).abs() < 1e-5);
    }

    // ── Affine2D math ───────────────────────────────────────

    #[test]
    fn affine2d_identity_is_recognized() {
        assert!(Affine2D::IDENTITY.is_identity());
    }

    #[test]
    fn affine2d_translation_is_not_identity() {
        assert!(!Affine2D::translation(1.0, 0.0).is_identity());
    }

    #[test]
    fn affine2d_scale_is_not_identity() {
        assert!(!Affine2D::scale(2.0, 1.0).is_identity());
    }

    #[test]
    fn affine2d_rotation_90_maps_x_to_y() {
        use std::f32::consts::FRAC_PI_2;
        let r = Affine2D::rotation(FRAC_PI_2);
        let (x, y) = r.transform_point(1.0, 0.0);
        assert!((x - 0.0).abs() < 1e-5, "x={x}");
        assert!((y - 1.0).abs() < 1e-5, "y={y}");
    }

    #[test]
    fn affine2d_rotation_is_not_identity() {
        use std::f32::consts::FRAC_PI_4;
        assert!(!Affine2D::rotation(FRAC_PI_4).is_identity());
    }

    #[test]
    fn affine2d_skew_x_shears_y_axis() {
        use std::f32::consts::FRAC_PI_4;
        // skew(π/4, 0) → b=tan(0)=0, c=tan(π/4)=1
        let s = Affine2D::skew(FRAC_PI_4, 0.0);
        let (x, y) = s.transform_point(0.0, 1.0);
        assert!((x - 1.0).abs() < 1e-5, "x={x}");
        assert!((y - 1.0).abs() < 1e-5, "y={y}");
    }

    #[test]
    fn affine2d_mul_two_translations_add() {
        let t1 = Affine2D::translation(3.0, 4.0);
        let t2 = Affine2D::translation(1.0, -2.0);
        let composed = t1 * t2;
        assert!((composed.e - 4.0).abs() < 1e-5);
        assert!((composed.f - 2.0).abs() < 1e-5);
    }

    #[test]
    fn affine2d_mul_scale_then_translate() {
        let s = Affine2D::scale(2.0, 3.0);
        let t = Affine2D::translation(10.0, 5.0);
        // (s * t) * p = s * (t * p): translate first, then scale
        let composed = s * t;
        let (x, y) = composed.transform_point(1.0, 1.0);
        // translate: (1+10, 1+5) = (11, 6); scale: (22, 18)
        assert!((x - 22.0).abs() < 1e-5, "x={x}");
        assert!((y - 18.0).abs() < 1e-5, "y={y}");
    }

    #[test]
    fn affine2d_transform_rect_identity_is_noop() {
        let r = Rect {
            x: 2.0,
            y: 3.0,
            width: 4.0,
            height: 5.0,
        };
        let q = Affine2D::IDENTITY.transform_rect(&r);
        // bottom-left, bottom-right, top-right, top-left in Y-down coords
        assert!((q.points[0][0] - 2.0).abs() < 1e-5); // bl.x
        assert!((q.points[0][1] - 8.0).abs() < 1e-5); // bl.y = y+h
        assert!((q.points[1][0] - 6.0).abs() < 1e-5); // br.x = x+w
        assert!((q.points[1][1] - 8.0).abs() < 1e-5); // br.y = y+h
        assert!((q.points[2][0] - 6.0).abs() < 1e-5); // tr.x
        assert!((q.points[2][1] - 3.0).abs() < 1e-5); // tr.y = y
        assert!((q.points[3][0] - 2.0).abs() < 1e-5); // tl.x
        assert!((q.points[3][1] - 3.0).abs() < 1e-5); // tl.y = y
    }

    #[test]
    fn affine2d_transform_rect_translation_shifts_all_corners() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 5.0,
        };
        let t = Affine2D::translation(2.0, 3.0);
        let q = t.transform_rect(&r);
        for pt in &q.points {
            assert!(pt[0] >= 2.0 - 1e-5 && pt[0] <= 12.0 + 1e-5, "x={}", pt[0]);
            assert!(pt[1] >= 3.0 - 1e-5 && pt[1] <= 8.0 + 1e-5, "y={}", pt[1]);
        }
    }

    // ── Quad ────────────────────────────────────────────────

    #[test]
    fn quad_non_degenerate_has_positive_area() {
        let q = Quad {
            points: [[0.0, 5.0], [5.0, 5.0], [5.0, 0.0], [0.0, 0.0]],
        };
        assert!(!q.is_degenerate());
    }

    #[test]
    fn quad_collapsed_to_line_is_degenerate() {
        // All points on the same horizontal line → zero area.
        let q = Quad {
            points: [[0.0, 0.0], [5.0, 0.0], [5.0, 0.0], [0.0, 0.0]],
        };
        assert!(q.is_degenerate());
    }

    #[test]
    fn quad_single_point_is_degenerate() {
        let q = Quad {
            points: [[3.0, 3.0]; 4],
        };
        assert!(q.is_degenerate());
    }

    // ── BlockStyle predicates ────────────────────────────────

    #[test]
    fn block_style_has_radius_false_for_default() {
        let s = BlockStyle::default();
        assert!(!s.has_radius());
    }

    #[test]
    fn block_style_has_radius_true_when_any_nonzero() {
        let mut s = BlockStyle::default();
        s.border_radii[2] = [0.0, 5.0];
        assert!(s.has_radius());
    }

    #[test]
    fn block_style_has_visual_style_background_color() {
        let s = BlockStyle {
            background_color: Some([255, 0, 0, 255]),
            ..Default::default()
        };
        assert!(s.has_visual_style());
    }

    #[test]
    fn block_style_has_visual_style_border_width() {
        let s = BlockStyle {
            border_widths: [0.0, 1.0, 0.0, 0.0],
            ..Default::default()
        };
        assert!(s.has_visual_style());
    }

    #[test]
    fn block_style_has_visual_style_padding() {
        let s = BlockStyle {
            padding: [0.0, 0.0, 0.0, 3.0],
            ..Default::default()
        };
        assert!(s.has_visual_style());
    }

    #[test]
    fn block_style_has_visual_style_box_shadow() {
        let s = BlockStyle {
            box_shadows: vec![BoxShadow::default()],
            ..Default::default()
        };
        assert!(s.has_visual_style());
    }

    #[test]
    fn block_style_has_visual_style_false_for_default() {
        assert!(!BlockStyle::default().has_visual_style());
    }

    #[test]
    fn block_style_content_inset_sums_left_border_and_left_padding() {
        let s = BlockStyle {
            border_widths: [5.0, 0.0, 0.0, 3.0], // top, right, bottom, left
            padding: [7.0, 0.0, 0.0, 2.0],       // top, right, bottom, left
            ..Default::default()
        };
        let (left, top) = s.content_inset();
        assert!((left - 5.0).abs() < 1e-5, "left={left}"); // bl(3)+pl(2)=5
        assert!((top - 12.0).abs() < 1e-5, "top={top}"); // bt(5)+pt(7)=12
    }

    #[test]
    fn block_style_has_overflow_clip_x_only() {
        let s = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Visible,
            ..Default::default()
        };
        assert!(s.has_overflow_clip());
    }

    #[test]
    fn block_style_has_overflow_clip_y_only() {
        let s = BlockStyle {
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Clip,
            ..Default::default()
        };
        assert!(s.has_overflow_clip());
    }

    #[test]
    fn block_style_has_overflow_clip_false_for_visible() {
        let s = BlockStyle::default(); // both Visible
        assert!(!s.has_overflow_clip());
    }

    #[test]
    fn block_style_needs_block_wrapper_from_radius_alone() {
        let s = BlockStyle {
            border_radii: [[5.0, 5.0]; 4],
            ..Default::default()
        };
        assert!(s.needs_block_wrapper());
    }

    #[test]
    fn block_style_needs_block_wrapper_from_overflow_clip_alone() {
        let s = BlockStyle {
            overflow_x: Overflow::Clip,
            ..Default::default()
        };
        assert!(s.needs_block_wrapper());
    }

    #[test]
    fn block_style_needs_block_wrapper_false_for_default() {
        assert!(!BlockStyle::default().needs_block_wrapper());
    }

    // ── BookmarkCollector ────────────────────────────────────

    #[test]
    fn bookmark_collector_records_on_correct_page() {
        let mut bc = BookmarkCollector::new();
        bc.set_current_page(2);
        bc.record(1, "Chapter One".to_string(), 100.0);
        bc.set_current_page(5);
        bc.record(2, "Section 5.1".to_string(), 42.0);

        let entries = bc.into_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].page_idx, 2);
        assert_eq!(entries[0].level, 1);
        assert_eq!(entries[0].label, "Chapter One");
        assert!((entries[0].y_pt - 100.0).abs() < 1e-5);
        assert_eq!(entries[1].page_idx, 5);
        assert_eq!(entries[1].level, 2);
        assert_eq!(entries[1].label, "Section 5.1");
    }

    #[test]
    fn bookmark_collector_empty_by_default() {
        let bc = BookmarkCollector::new();
        assert!(bc.into_entries().is_empty());
    }

    // ── alpha_to_opacity ─────────────────────────────────────

    #[test]
    fn alpha_to_opacity_255_is_one() {
        let v = alpha_to_opacity(255);
        assert!((v.get() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn alpha_to_opacity_0_is_zero() {
        let v = alpha_to_opacity(0);
        assert!(v.get() < 1e-3);
    }

    #[test]
    fn alpha_to_opacity_128_is_near_half() {
        let v = alpha_to_opacity(128);
        assert!((v.get() - 128.0 / 255.0).abs() < 1e-3);
    }

    // ── LinkCollector::take_page ─────────────────────────────

    #[test]
    fn link_collector_take_page_removes_and_returns_entries() {
        let mut collector = LinkCollector::new();
        let link = make_test_link();
        collector.set_current_page(1);
        collector.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 5.0,
            },
        );
        collector.set_current_page(2);
        collector.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 5.0,
            },
        );

        let page1 = collector.take_page(1);
        assert_eq!(page1.len(), 1);
        assert_eq!(page1[0].page_idx, 1);

        // Page 1 entries are gone; page 2 still present.
        assert!(collector.take_page(1).is_empty());
        let page2 = collector.take_page(2);
        assert_eq!(page2.len(), 1);
        assert_eq!(page2[0].page_idx, 2);
    }

    #[test]
    fn link_collector_take_missing_page_returns_empty() {
        let mut collector = LinkCollector::new();
        assert!(collector.take_page(99).is_empty());
    }

    // ── LinkCollector: same-link multiple pages → separate occurrences ──

    #[test]
    fn link_collector_same_link_different_pages_produces_separate_occurrences() {
        let mut collector = LinkCollector::new();
        let link = make_test_link();
        collector.set_current_page(0);
        collector.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 5.0,
            },
        );
        collector.set_current_page(1);
        collector.push_rect(
            &link,
            Rect {
                x: 5.0,
                y: 0.0,
                width: 10.0,
                height: 5.0,
            },
        );

        let all = collector.into_occurrences();
        assert_eq!(all.len(), 2, "expected one occurrence per page");
    }

    // ── DestinationRegistry: first-write-wins ────────────────

    #[test]
    fn destination_registry_first_write_wins_for_duplicate_ids() {
        let mut reg = DestinationRegistry::new();
        reg.set_current_page(0);
        reg.record("anchor", 10.0, 20.0);
        reg.set_current_page(1);
        reg.record("anchor", 99.0, 99.0); // duplicate — should be ignored
        let (page, x, y) = reg.get("anchor").expect("recorded");
        assert_eq!(page, 0, "first write should win");
        assert!((x - 10.0).abs() < 1e-5);
        assert!((y - 20.0).abs() < 1e-5);
    }

    // ── compute_overflow_clip_path: y-only clip branch ───────

    #[test]
    fn compute_overflow_clip_y_clip_x_visible_returns_path() {
        let style = BlockStyle {
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Clip,
            border_widths: [1.0, 1.0, 1.0, 1.0],
            ..Default::default()
        };
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_some());
    }

    // ── compute_overflow_clip_path: both-axes, no-radius branch ──

    #[test]
    fn compute_overflow_clip_both_axes_no_radius_returns_path() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Clip,
            border_widths: [1.0, 1.0, 1.0, 1.0],
            ..Default::default()
        };
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_some());
    }
}

#[cfg(test)]
pub(crate) mod run_tag_tests {
    use super::*;
    use krilla::tagging::{ContentTag, Identifier, SpanTag};

    pub(crate) fn make_identifier() -> Identifier {
        let mut doc = krilla::Document::new();
        let settings = krilla::page::PageSettings::from_wh(100.0, 100.0).expect("valid page size");
        let mut page = doc.start_page_with(settings);
        let mut surface = page.surface();
        let id = surface.start_tagged(ContentTag::Span(SpanTag::empty()));
        surface.end_tagged();
        id
    }

    #[test]
    fn record_run_content_round_trips() {
        let mut tc = TagCollector::new();
        tc.record_run(42, ParagraphRunItem::Content(make_identifier()));
        let runs = tc.run_entries.get(&42).expect("run entry recorded");
        assert_eq!(runs.len(), 1);
        assert!(matches!(runs[0], ParagraphRunItem::Content(_)));
    }

    #[test]
    fn record_run_link_content_round_trips() {
        let mut tc = TagCollector::new();
        tc.record_run(
            7,
            ParagraphRunItem::LinkContent {
                span_ptr: 0xdeadbeef,
                identifier: make_identifier(),
            },
        );
        let runs = tc.run_entries.get(&7).expect("run entry recorded");
        assert_eq!(runs.len(), 1);
        assert!(matches!(
            runs[0],
            ParagraphRunItem::LinkContent {
                span_ptr: 0xdeadbeef,
                ..
            }
        ));
    }

    #[test]
    fn run_entries_empty_by_default() {
        let tc = TagCollector::new();
        assert!(tc.run_entries.is_empty());
    }

    #[test]
    fn wired_link_span_ptrs_collects_link_content_only() {
        let mut tc = TagCollector::new();
        tc.record_run(1, ParagraphRunItem::Content(make_identifier()));
        tc.record_run(
            1,
            ParagraphRunItem::LinkContent {
                span_ptr: 0x1234,
                identifier: make_identifier(),
            },
        );
        tc.record_run(
            2,
            ParagraphRunItem::LinkContent {
                span_ptr: 0xabcd,
                identifier: make_identifier(),
            },
        );
        let wired = tc.wired_link_span_ptrs();
        assert!(wired.contains(&0x1234));
        assert!(wired.contains(&0xabcd));
        assert_eq!(wired.len(), 2);
    }
}

/// Float-tolerance helpers shared across the in-crate transform test
/// modules (`affine_tests`, `transform_wrapper_tests`, and the
/// `transform_tests` module in `blitz_adapter.rs`).
#[cfg(test)]
pub(crate) mod matrix_test_util {
    pub(crate) const EPS: f32 = 1e-5;

    pub(crate) fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }
}
