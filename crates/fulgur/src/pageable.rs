use std::collections::BTreeMap;
use std::sync::Arc;

use crate::gcpm::CounterOp;
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

#[derive(Debug, Clone, Copy)]
pub struct Pagination {
    pub break_before: BreakBefore,
    pub break_after: BreakAfter,
    pub break_inside: BreakInside,
    pub orphans: usize,
    pub widows: usize,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            break_before: BreakBefore::Auto,
            break_after: BreakAfter::Auto,
            break_inside: BreakInside::Auto,
            orphans: 2,
            widows: 2,
        }
    }
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

/// Wrapper around Krilla Surface for drawing commands.
/// This decouples Pageable types from Krilla's concrete Surface type.
pub struct Canvas<'a, 'b> {
    pub surface: &'a mut krilla::surface::Surface<'b>,
    pub bookmark_collector: Option<&'a mut BookmarkCollector>,
    pub link_collector: Option<&'a mut LinkCollector>,
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

/// Core pagination-aware layout trait.
pub trait Pageable: Send + Sync {
    /// Measure size within available area.
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size;

    /// Split at page boundary. Returns None if element fits entirely
    /// or cannot be split.
    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)>;

    /// Split consuming the boxed value (avoids cloning children).
    /// Returns `Ok((first, second))` on success, or `Err(self)` when no split is
    /// possible, giving the caller back ownership.
    fn split_boxed(self: Box<Self>, avail_width: Pt, avail_height: Pt) -> SplitResult {
        match self.split(avail_width, avail_height) {
            Some(pair) => Ok(pair),
            None => Err(self.clone_box()),
        }
    }

    /// Emit drawing commands.
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt);

    /// CSS pagination properties for this element.
    fn pagination(&self) -> Pagination {
        Pagination::default()
    }

    /// Clone this pageable into a boxed trait object.
    fn clone_box(&self) -> Box<dyn Pageable>;

    /// Measured height from last wrap() call.
    fn height(&self) -> Pt;

    /// Downcast support for tests.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Walk this Pageable and record any block-level `id` anchors into
    /// `registry`, in page-local coordinates.
    ///
    /// `(x, y)` is the top-left of this element in the current page's
    /// content area. Containers must recurse into their children using the
    /// same positional arithmetic that `draw()` uses so anchor positions
    /// match the rendered output.
    ///
    /// Default: no-op. Overridden on block-like Pageables (`BlockPageable`)
    /// and containers that hold children (pagination wrappers, `ListItemPageable`,
    /// `TablePageable`, etc.).
    fn collect_ids(
        &self,
        _x: Pt,
        _y: Pt,
        _avail_width: Pt,
        _avail_height: Pt,
        _registry: &mut DestinationRegistry,
    ) {
    }
}

impl Clone for Box<dyn Pageable> {
    fn clone(&self) -> Self {
        self.clone_box()
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

    /// Whether a node with this style must be wrapped in a `BlockPageable`.
    ///
    /// Wrapping is required when the node has any visual effect that must be
    /// rendered on its own surface — backgrounds/borders/padding
    /// (`has_visual_style`), a non-zero `border-radius` (`has_radius`), or
    /// overflow clipping (`has_overflow_clip`, which uses the node's box as
    /// the clip region).
    pub fn needs_block_wrapper(&self) -> bool {
        self.has_visual_style() || self.has_radius() || self.has_overflow_clip()
    }
}

// ─── PositionedChild ─────────────────────────────────────

/// A child element with its Taffy-computed position.
#[derive(Clone)]
pub struct PositionedChild {
    pub child: Box<dyn Pageable>,
    pub x: Pt,
    pub y: Pt,
}

pub type SplitPair = (Box<dyn Pageable>, Box<dyn Pageable>);
pub type SplitResult = Result<SplitPair, Box<dyn Pageable>>;

/// Result of scanning a `BlockPageable`'s children to decide where to split.
enum SplitDecision {
    /// Element fits or cannot be split.
    NoSplit,
    /// Split between children; all children before `usize` go to the first
    /// fragment, the rest go to the second.
    AtIndex(usize),
    /// Split *within* the child at `usize`; the contained pair is the child's
    /// first and second fragments.
    WithinChild(usize, SplitPair),
}

// ─── BlockPageable ───────────────────────────────────────

/// A block container that positions children using Taffy layout coordinates.
/// Handles margin/border/padding/background and page splitting.
#[derive(Clone)]
pub struct BlockPageable {
    pub children: Vec<PositionedChild>,
    pub pagination: Pagination,
    pub cached_size: Option<Size>,
    /// Taffy-computed layout size (preserved across wrap() calls for drawing).
    pub layout_size: Option<Size>,
    pub style: BlockStyle,
    pub opacity: f32,
    pub visible: bool,
    /// HTML `id` attribute (trimmed, non-empty). Used as an anchor target
    /// for internal `href="#..."` links. `Arc<String>` so split fragments
    /// can share without cloning the string.
    pub id: Option<Arc<String>>,
}

impl BlockPageable {
    pub fn new(children: Vec<Box<dyn Pageable>>) -> Self {
        // Legacy constructor: stack children vertically
        let mut y = 0.0;
        let positioned: Vec<PositionedChild> = children
            .into_iter()
            .map(|child| {
                let child_y = y;
                y += child.height();
                PositionedChild {
                    child,
                    x: 0.0,
                    y: child_y,
                }
            })
            .collect();
        Self {
            children: positioned,
            pagination: Pagination::default(),
            cached_size: None,
            layout_size: None,
            style: BlockStyle::default(),
            opacity: 1.0,
            visible: true,
            id: None,
        }
    }

    pub fn with_positioned_children(children: Vec<PositionedChild>) -> Self {
        Self {
            children,
            pagination: Pagination::default(),
            cached_size: None,
            layout_size: None,
            style: BlockStyle::default(),
            opacity: 1.0,
            visible: true,
            id: None,
        }
    }

    pub fn with_pagination(mut self, pagination: Pagination) -> Self {
        self.pagination = pagination;
        self
    }

    pub fn with_style(mut self, style: BlockStyle) -> Self {
        self.style = style;
        self
    }

    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn with_id(mut self, id: Option<Arc<String>>) -> Self {
        self.id = id;
        self
    }

    /// Determine where (if at all) this block should be split at the given
    /// available height.  Both `split()` and `split_boxed()` call this to
    /// avoid duplicating the scanning logic.
    fn find_split_point(&self, avail_height: Pt) -> SplitDecision {
        if self.pagination.break_inside == BreakInside::Avoid {
            return SplitDecision::NoSplit;
        }

        let has_forced_break = self.children.iter().enumerate().any(|(i, pc)| {
            (pc.child.pagination().break_before == BreakBefore::Page && i > 0)
                || (pc.child.pagination().break_after == BreakAfter::Page
                    && i < self.children.len() - 1)
        });

        let total_height = self.cached_size.map(|s| s.height).unwrap_or(0.0);
        if total_height <= avail_height && !has_forced_break {
            return SplitDecision::NoSplit;
        }

        for (i, pc) in self.children.iter().enumerate() {
            if pc.child.pagination().break_before == BreakBefore::Page && i > 0 && pc.y > 0.0 {
                return SplitDecision::AtIndex(i);
            }

            if pc.y + pc.child.height() > avail_height {
                let child_avail = avail_height - pc.y;
                // TODO: Use split_boxed for child sub-split once the child can be extracted from Vec before iteration
                if let Some(parts) = (child_avail > 0.0)
                    .then(|| pc.child.split(0.0, child_avail))
                    .flatten()
                {
                    return SplitDecision::WithinChild(i, parts);
                } else if i == 0 && self.children.len() == 1 {
                    return SplitDecision::NoSplit;
                } else {
                    return SplitDecision::AtIndex(i.max(1));
                }
            }

            if pc.child.pagination().break_after == BreakAfter::Page {
                return SplitDecision::AtIndex(i + 1);
            }
        }

        SplitDecision::NoSplit
    }
}

/// Build a rounded rectangle path using cubic Bézier curves for corners.
/// radii: [top-left, top-right, bottom-right, bottom-left] × [rx, ry]
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

/// Clone a slice of PositionedChild, optionally shifting y coordinates.
/// When `y_offset` is 0.0, children are cloned as-is.
/// A negative `y_offset` shifts children upward (subtracts from y).
fn clone_children(children: &[PositionedChild], y_offset: f32) -> Vec<PositionedChild> {
    children
        .iter()
        .map(|pc| PositionedChild {
            child: pc.child.clone_box(),
            x: pc.x,
            y: pc.y - y_offset,
        })
        .collect()
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
        BorderStyleValue::Double
        | BorderStyleValue::Groove
        | BorderStyleValue::Ridge
        | BorderStyleValue::Inset
        | BorderStyleValue::Outset => Some(stroke), // handled specially at call site
    }
}

/// Helper to draw a simple line segment with a given stroke.
fn stroke_line(
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

/// Build a krilla `Path` for the axis-aligned rectangle at (x,y) with
/// size (w,h). Returns `None` if `w <= 0` or `h <= 0` (krilla rejects
/// degenerate rects).
fn build_rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<krilla::geom::Path> {
    let rect = krilla::geom::Rect::from_xywh(x, y, w, h)?;
    let mut pb = krilla::geom::PathBuilder::new();
    pb.push_rect(rect);
    pb.finish()
}

/// Stroke a rectangle as a single closed subpath.
/// krilla 0.7's `PathBuilder::push_rect` decomposes to `m + 3l + h` rather
/// than emitting the PDF `re` operator, but the content-stream saving is
/// still real: one `draw_path` call replaces 4 abutting `stroke_line` calls,
/// collapsing 4 × (m + l + S) into 1 × (m + 3l + h + S).
fn stroke_rect(
    canvas: &mut Canvas<'_, '_>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    stroke: krilla::paint::Stroke,
) {
    if let Some(path) = build_rect_path(x, y, w, h) {
        canvas.surface.set_stroke(Some(stroke));
        canvas.surface.draw_path(&path);
    }
}

/// Create a stroke with a specific color and width, inheriting opacity from base.
fn colored_stroke(
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
        BorderStyleValue::Double => {
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

fn draw_block_border(
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
                opacity: krilla::num::NormalizedF32::new(bc[3] as f32 / 255.0)
                    .unwrap_or(krilla::num::NormalizedF32::ONE),
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
        && matches!(st, BorderStyleValue::Solid)
    {
        let opacity = krilla::num::NormalizedF32::new(bc[3] as f32 / 255.0)
            .unwrap_or(krilla::num::NormalizedF32::ONE);
        let inset = bt / 2.0;
        let base = colored_stroke(bc, bt, opacity);
        if let Some(styled) = apply_border_style(base, st, bt) {
            canvas.surface.set_fill(None);
            stroke_rect(
                canvas,
                x + inset,
                y + inset,
                (w - inset * 2.0).max(0.0),
                (h - inset * 2.0).max(0.0),
                styled,
            );
            canvas.surface.set_stroke(None);
        }
    } else {
        let opacity = krilla::num::NormalizedF32::new(bc[3] as f32 / 255.0)
            .unwrap_or(krilla::num::NormalizedF32::ONE);
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

impl Pageable for BlockPageable {
    fn wrap(&mut self, avail_width: Pt, _avail_height: Pt) -> Size {
        // Ensure children have been wrapped (split() creates unwrapped children)
        for pc in &mut self.children {
            if pc.child.height() == 0.0 {
                pc.child.wrap(avail_width, 10000.0);
            }
        }
        // Use max of children's (y + height) for total height
        let total_height = self.children.iter_mut().fold(0.0f32, |max_h, pc| {
            let child_h = pc.child.height();
            max_h.max(pc.y + child_h)
        });
        let size = Size {
            width: avail_width,
            height: total_height,
        };
        self.cached_size = Some(size);
        size
    }

    fn split(
        &self,
        _avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        match self.find_split_point(avail_height) {
            SplitDecision::NoSplit => None,

            SplitDecision::WithinChild(idx, (first_part, second_part)) => {
                let pc = &self.children[idx];
                let mut first = clone_children(&self.children[..idx], 0.0);
                first.push(PositionedChild {
                    child: first_part,
                    x: pc.x,
                    y: pc.y,
                });

                let mut second = vec![PositionedChild {
                    child: second_part,
                    x: pc.x,
                    y: 0.0,
                }];
                second.extend(clone_children(&self.children[idx + 1..], pc.y));

                Some((
                    Box::new(
                        BlockPageable::with_positioned_children(first)
                            .with_pagination(self.pagination)
                            .with_style(self.style.clone())
                            .with_opacity(self.opacity)
                            .with_visible(self.visible)
                            .with_id(self.id.clone()),
                    ),
                    Box::new(
                        BlockPageable::with_positioned_children(second)
                            .with_pagination(self.pagination)
                            .with_style(self.style.clone())
                            .with_opacity(self.opacity)
                            .with_visible(self.visible)
                            .with_id(self.id.clone()),
                    ),
                ))
            }

            SplitDecision::AtIndex(split_index) => {
                if split_index == 0 || split_index >= self.children.len() {
                    return None;
                }

                let split_y = self.children[split_index].y;
                let first = clone_children(&self.children[..split_index], 0.0);
                let second = clone_children(&self.children[split_index..], split_y);

                Some((
                    Box::new(
                        BlockPageable::with_positioned_children(first)
                            .with_pagination(self.pagination)
                            .with_style(self.style.clone())
                            .with_opacity(self.opacity)
                            .with_visible(self.visible)
                            .with_id(self.id.clone()),
                    ),
                    Box::new(
                        BlockPageable::with_positioned_children(second)
                            .with_pagination(self.pagination)
                            .with_style(self.style.clone())
                            .with_opacity(self.opacity)
                            .with_visible(self.visible)
                            .with_id(self.id.clone()),
                    ),
                ))
            }
        }
    }

    fn split_boxed(self: Box<Self>, _avail_width: Pt, avail_height: Pt) -> SplitResult {
        match self.find_split_point(avail_height) {
            SplitDecision::NoSplit => Err(self),

            SplitDecision::WithinChild(idx, (first_part, second_part)) => {
                let mut me = *self;
                let split_child_x = me.children[idx].x;
                let split_child_y = me.children[idx].y;

                let mut tail = me.children.split_off(idx + 1);
                let _split_child = me.children.pop().unwrap(); // remove the split child
                let mut first = me.children; // children[..idx]

                first.push(PositionedChild {
                    child: first_part,
                    x: split_child_x,
                    y: split_child_y,
                });

                // Shift tail y coordinates
                for pc in &mut tail {
                    pc.y -= split_child_y;
                }

                let mut second = vec![PositionedChild {
                    child: second_part,
                    x: split_child_x,
                    y: 0.0,
                }];
                second.append(&mut tail);

                Ok((
                    Box::new(
                        BlockPageable::with_positioned_children(first)
                            .with_pagination(me.pagination)
                            .with_style(me.style.clone())
                            .with_opacity(me.opacity)
                            .with_visible(me.visible)
                            .with_id(me.id.clone()),
                    ),
                    Box::new(
                        BlockPageable::with_positioned_children(second)
                            .with_pagination(me.pagination)
                            .with_style(me.style)
                            .with_opacity(me.opacity)
                            .with_visible(me.visible)
                            .with_id(me.id),
                    ),
                ))
            }

            SplitDecision::AtIndex(split_index) => {
                let mut me = *self;

                if split_index == 0 || split_index >= me.children.len() {
                    return Err(Box::new(me));
                }

                let split_y = me.children[split_index].y;
                let mut second_children = me.children.split_off(split_index);
                let first_children = me.children;

                // Shift y coordinates on second fragment
                for pc in &mut second_children {
                    pc.y -= split_y;
                }

                Ok((
                    Box::new(
                        BlockPageable::with_positioned_children(first_children)
                            .with_pagination(me.pagination)
                            .with_style(me.style.clone())
                            .with_opacity(me.opacity)
                            .with_visible(me.visible)
                            .with_id(me.id.clone()),
                    ),
                    Box::new(
                        BlockPageable::with_positioned_children(second_children)
                            .with_pagination(me.pagination)
                            .with_style(me.style)
                            .with_opacity(me.opacity)
                            .with_visible(me.visible)
                            .with_id(me.id),
                    ),
                ))
            }
        }
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
            // Prefer layout_size (Taffy-computed, stable) over cached_size (may be children-only)
            let total_width = self
                .layout_size
                .or(self.cached_size)
                .map(|s| s.width)
                .unwrap_or(avail_width);
            let total_height = self
                .layout_size
                .or(self.cached_size)
                .map(|s| s.height)
                .unwrap_or(avail_height);

            // visibility: hidden skips own rendering but children still draw
            if self.visible {
                crate::background::draw_box_shadows(
                    canvas,
                    &self.style,
                    x,
                    y,
                    total_width,
                    total_height,
                );
                crate::background::draw_background(
                    canvas,
                    &self.style,
                    x,
                    y,
                    total_width,
                    total_height,
                );
                draw_block_border(canvas, &self.style, x, y, total_width, total_height);
            }

            // overflow clipping: clip children to the padding box.
            // Background and borders are intentionally drawn outside the clip
            // so borders render correctly at the block's edge.
            let clip_pushed = if let Some(clip_path) =
                compute_overflow_clip_path(&self.style, x, y, total_width, total_height)
            {
                canvas
                    .surface
                    .push_clip_path(&clip_path, &krilla::paint::FillRule::default());
                true
            } else {
                false
            };

            for pc in &self.children {
                pc.child
                    .draw(canvas, x + pc.x, y + pc.y, avail_width, pc.child.height());
            }

            if clip_pushed {
                canvas.surface.pop();
            }
        });
    }

    fn pagination(&self) -> Pagination {
        self.pagination
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.layout_size
            .or(self.cached_size)
            .map(|s| s.height)
            .unwrap_or(0.0)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        _avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        // Record this block's own id at its top-left in page-local coords.
        if let Some(id) = &self.id
            && !id.is_empty()
        {
            registry.record(id, x, y);
        }
        // Recurse into children using the same positional arithmetic
        // `draw()` uses (see the loop at the end of `draw`). We do NOT
        // need to replicate clipping / opacity / background paths —
        // anchor registration only cares about block-top coordinates.
        for pc in &self.children {
            pc.child
                .collect_ids(x + pc.x, y + pc.y, avail_width, pc.child.height(), registry);
        }
    }
}

// ─── SpacerPageable ──────────────────────────────────────

/// Fixed-height vertical space. Cannot be split.
#[derive(Clone)]
pub struct SpacerPageable {
    pub height: Pt,
}

impl SpacerPageable {
    pub fn new(height: Pt) -> Self {
        Self { height }
    }
}

impl Pageable for SpacerPageable {
    fn wrap(&mut self, avail_width: Pt, _avail_height: Pt) -> Size {
        Size {
            width: avail_width,
            height: self.height,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        None
    }

    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {
        // Spacers are invisible
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.height
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ─── BookmarkMarkerPageable ──────────────────────────────

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
/// `BookmarkMarkerPageable::draw` pushes an entry for each marker it sees.
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

/// Zero-size marker for a bookmark entry, for PDF outline generation.
/// Attached to the source block so the marker travels with the first
/// fragment on page splits (see `BookmarkMarkerWrapperPageable`).
#[derive(Clone)]
pub struct BookmarkMarkerPageable {
    pub level: u8,
    pub label: String,
}

impl BookmarkMarkerPageable {
    pub fn new(level: u8, label: String) -> Self {
        Self { level, label }
    }

    /// Helper used by both `draw` and unit tests — records into the collector
    /// if one is present.
    pub fn record_if_collecting(&self, y: Pt, collector: Option<&mut BookmarkCollector>) {
        if let Some(c) = collector {
            c.record(self.level, self.label.clone(), y);
        }
    }
}

impl Pageable for BookmarkMarkerPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        Size {
            width: 0.0,
            height: 0.0,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        None
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, _x: Pt, y: Pt, _aw: Pt, _ah: Pt) {
        self.record_if_collecting(y, canvas.bookmark_collector.as_deref_mut());
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        0.0
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ─── BookmarkMarkerWrapperPageable ──────────────────────────

/// Wraps a Pageable with a `BookmarkMarkerPageable`, keeping the marker
/// attached to the first fragment on `split()` so outline anchors land on
/// the page where the bookmark source visually starts.
#[derive(Clone)]
pub struct BookmarkMarkerWrapperPageable {
    pub marker: BookmarkMarkerPageable,
    pub child: Box<dyn Pageable>,
}

impl BookmarkMarkerWrapperPageable {
    pub fn new(marker: BookmarkMarkerPageable, child: Box<dyn Pageable>) -> Self {
        Self { marker, child }
    }
}

impl Pageable for BookmarkMarkerWrapperPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        self.child.wrap(avail_width, avail_height)
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (first, second) = self.child.split(avail_width, avail_height)?;
        let first_wrapped = BookmarkMarkerWrapperPageable {
            marker: self.marker.clone(),
            child: first,
        };
        // Second fragment does NOT carry the marker — the heading started on
        // the previous page.
        Some((Box::new(first_wrapped), second))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, aw: Pt, ah: Pt) {
        self.marker.draw(canvas, x, y, aw, ah);
        self.child.draw(canvas, x, y, aw, ah);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.child.height()
    }

    fn pagination(&self) -> Pagination {
        self.child.pagination()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        self.child
            .collect_ids(x, y, avail_width, avail_height, registry);
    }
}

// ─── StringSetPageable ──────────────────────────────────

/// Zero-size marker for named string values.
/// Inserted into the Pageable tree to track string-set positions during pagination.
#[derive(Clone)]
pub struct StringSetPageable {
    pub name: String,
    pub value: String,
}

impl StringSetPageable {
    pub fn new(name: String, value: String) -> Self {
        Self { name, value }
    }
}

impl Pageable for StringSetPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        Size {
            width: 0.0,
            height: 0.0,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        None
    }

    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {}

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        0.0
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ─── RunningElementMarkerPageable ────────────────────────

/// Zero-size marker for a running element instance.
///
/// Inserted into the Pageable tree at the source position where
/// `position: running(name)` was declared, so that pagination can track
/// which running element instances fall on which page. The actual HTML of
/// the running element lives in `RunningElementStore`, keyed by
/// `instance_id`.
///
/// Parallels `StringSetPageable` but carries an `instance_id` instead of
/// a value — running elements are full DOM subtrees that can be large, so
/// the marker stays zero-cost and the HTML is looked up by id at render
/// time via `resolve_element_policy`.
///
/// During convert, markers are attached to the following real child via
/// `RunningElementWrapperPageable` so that when the child moves to the
/// next page due to an unsplittable overflow, the marker travels with it.
#[derive(Clone)]
pub struct RunningElementMarkerPageable {
    pub name: String,
    pub instance_id: usize,
}

impl RunningElementMarkerPageable {
    pub fn new(name: String, instance_id: usize) -> Self {
        Self { name, instance_id }
    }
}

impl Pageable for RunningElementMarkerPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        Size {
            width: 0.0,
            height: 0.0,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        None
    }

    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {}

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        0.0
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ─── CounterOpMarkerPageable ──────────────────────────────

/// Zero-size marker that carries counter operations through pagination.
///
/// Inserted into the Pageable tree so that `collect_counter_states` can replay
/// counter-reset / counter-increment / counter-set in document order and build
/// per-page counter snapshots.
#[derive(Debug, Clone)]
pub struct CounterOpMarkerPageable {
    pub ops: Vec<CounterOp>,
}

impl CounterOpMarkerPageable {
    pub fn new(ops: Vec<CounterOp>) -> Self {
        Self { ops }
    }
}

impl Pageable for CounterOpMarkerPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        Size {
            width: 0.0,
            height: 0.0,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        None
    }

    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {}

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        0.0
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ─── CounterOpWrapperPageable ─────────────────────────────

/// Wraps a Pageable together with `CounterOp` operations that must stay
/// attached to it during pagination.
///
/// Without this wrapper, a plain `BlockPageable` containing
/// `[CounterOpMarkerPageable, child]` could split such that the marker is
/// left on the previous page while the real child is moved to the next page
/// (when the child is unsplittable and larger than the available space).
/// `collect_counter_states` would then attribute the counter operation to
/// the wrong page.
///
/// The wrapper delegates `split()` to the inner child: if the child splits,
/// markers travel with the first fragment; if the child cannot split, the
/// wrapper is atomic and the whole thing moves to the next page together.
#[derive(Clone)]
pub struct CounterOpWrapperPageable {
    pub ops: Vec<CounterOp>,
    pub child: Box<dyn Pageable>,
}

impl CounterOpWrapperPageable {
    pub fn new(ops: Vec<CounterOp>, child: Box<dyn Pageable>) -> Self {
        Self { ops, child }
    }
}

impl Pageable for CounterOpWrapperPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        self.child.wrap(avail_width, avail_height)
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (first, second) = self.child.split(avail_width, avail_height)?;
        let first_wrapped = CounterOpWrapperPageable {
            ops: self.ops.clone(),
            child: first,
        };
        Some((Box::new(first_wrapped), second))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        self.child.draw(canvas, x, y, avail_width, avail_height);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.child.height()
    }

    fn pagination(&self) -> Pagination {
        self.child.pagination()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        self.child
            .collect_ids(x, y, avail_width, avail_height, registry);
    }
}

// ─── TransformWrapperPageable ──────────────────────────────

/// Wraps a Pageable in a CSS `transform`. The matrix is pre-resolved
/// at convert time (percentages / keywords already turned into px).
///
/// The wrapper is **atomic**: `split()` always returns `None`, forcing
/// the whole subtree onto a single page. A transformed element that
/// spans a page break would be geometrically meaningless (half of a
/// rotated title on each page), so we follow PrinceXML / WeasyPrint
/// behavior and never split through a transform.
///
/// `origin` is the `transform-origin` resolved to px, measured from the
/// element's border-box top-left corner.
#[derive(Clone)]
pub struct TransformWrapperPageable {
    pub inner: Box<dyn Pageable>,
    pub matrix: Affine2D,
    pub origin: Point2,
}

impl TransformWrapperPageable {
    pub fn new(inner: Box<dyn Pageable>, matrix: Affine2D, origin: Point2) -> Self {
        Self {
            inner,
            matrix,
            origin,
        }
    }

    /// Compute the full matrix that will be pushed onto the Krilla surface
    /// when this wrapper is drawn at `(draw_x, draw_y)`.
    ///
    /// The transform-origin is translated into the draw coordinate system,
    /// then the composition `T(ox, oy) · M · T(-ox, -oy)` is built so that
    /// rotation/scale happen around the chosen origin point.
    ///
    /// Exposed (hidden from docs) so integration tests can verify
    /// geometric correctness without constructing a Krilla surface.
    #[doc(hidden)]
    pub fn effective_matrix(&self, draw_x: Pt, draw_y: Pt) -> Affine2D {
        let ox = draw_x + self.origin.x;
        let oy = draw_y + self.origin.y;
        Affine2D::translation(ox, oy) * self.matrix * Affine2D::translation(-ox, -oy)
    }
}

impl Pageable for TransformWrapperPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        self.inner.wrap(avail_width, avail_height)
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        None
    }

    /// The wrapper is atomic, so the boxed split path can move ownership
    /// straight back to the caller instead of falling through the default
    /// implementation, which would clone the entire subtree.
    fn split_boxed(self: Box<Self>, _avail_width: Pt, _avail_height: Pt) -> SplitResult {
        Err(self)
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        let full = self.effective_matrix(x, y);
        if let Some(lc) = canvas.link_collector.as_deref_mut() {
            lc.push_transform(full);
        }
        canvas.surface.push_transform(&full.to_krilla());
        self.inner.draw(canvas, x, y, avail_width, avail_height);
        canvas.surface.pop();
        if let Some(lc) = canvas.link_collector.as_deref_mut() {
            lc.pop_transform();
        }
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.inner.height()
    }

    fn pagination(&self) -> Pagination {
        self.inner.pagination()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        let full = self.effective_matrix(x, y);
        registry.push_transform(full);
        self.inner
            .collect_ids(x, y, avail_width, avail_height, registry);
        registry.pop_transform();
    }
}

// ─── StringSetWrapperPageable ──────────────────────────────

/// Wraps a Pageable together with `StringSetPageable` markers that must stay
/// attached to it during pagination.
///
/// Without this wrapper, a plain `BlockPageable` containing `[markers..., child]`
/// could split such that the markers are left on the previous page while the
/// real child is moved to the next page (when the child is unsplittable and
/// larger than the available space). `collect_string_set_states` would then
/// resolve `string()` one page too early.
///
/// The wrapper delegates `split()` to the inner child: if the child splits,
/// markers travel with the first fragment; if the child cannot split, the
/// wrapper is atomic and the whole thing moves to the next page together.
#[derive(Clone)]
pub struct StringSetWrapperPageable {
    pub markers: Vec<StringSetPageable>,
    pub child: Box<dyn Pageable>,
}

impl StringSetWrapperPageable {
    pub fn new(markers: Vec<StringSetPageable>, child: Box<dyn Pageable>) -> Self {
        Self { markers, child }
    }
}

impl Pageable for StringSetWrapperPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        self.child.wrap(avail_width, avail_height)
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (first, second) = self.child.split(avail_width, avail_height)?;
        let first_wrapped = StringSetWrapperPageable {
            markers: self.markers.clone(),
            child: first,
        };
        Some((Box::new(first_wrapped), second))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        self.child.draw(canvas, x, y, avail_width, avail_height);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.child.height()
    }

    fn pagination(&self) -> Pagination {
        self.child.pagination()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        self.child
            .collect_ids(x, y, avail_width, avail_height, registry);
    }
}

// ─── RunningElementWrapperPageable ──────────────────────────

/// Wraps a Pageable together with `RunningElementMarkerPageable` markers that
/// must stay attached to it during pagination.
///
/// Running elements are rewritten to `display: none`, so their markers have
/// no layout of their own. Without this wrapper, a plain marker emitted as
/// a sibling could be stranded on the previous page when the following
/// unsplittable child overflows — the marker's zero-size position would
/// land before the split point while the content conceptually belonging
/// with it is pushed to the next page. Chapter heading + large figure is
/// the canonical case.
///
/// The wrapper delegates `split()` to the inner child: if the child splits,
/// markers travel with the first fragment; if the child cannot split, the
/// wrapper is atomic and the whole thing moves to the next page together.
#[derive(Clone)]
pub struct RunningElementWrapperPageable {
    pub markers: Vec<RunningElementMarkerPageable>,
    pub child: Box<dyn Pageable>,
}

impl RunningElementWrapperPageable {
    pub fn new(markers: Vec<RunningElementMarkerPageable>, child: Box<dyn Pageable>) -> Self {
        Self { markers, child }
    }
}

impl Pageable for RunningElementWrapperPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        self.child.wrap(avail_width, avail_height)
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (first, second) = self.child.split(avail_width, avail_height)?;
        let first_wrapped = RunningElementWrapperPageable {
            markers: self.markers.clone(),
            child: first,
        };
        Some((Box::new(first_wrapped), second))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        self.child.draw(canvas, x, y, avail_width, avail_height);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.child.height()
    }

    fn pagination(&self) -> Pagination {
        self.child.pagination()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        self.child
            .collect_ids(x, y, avail_width, avail_height, registry);
    }
}

// ─── ListItemPageable ───────────────────────────────────

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

/// Image marker contents — either a raster image or a parsed SVG tree.
#[derive(Clone)]
pub enum ImageMarker {
    Raster(crate::image::ImagePageable),
    Svg(crate::svg::SvgPageable),
}

/// Marker attached to a `ListItemPageable`.
///
/// Exactly one variant holds valid content per list item, enforced by the
/// type system. `None` is used for the second fragment after a page-break
/// split (the marker only appears on the first fragment).
#[derive(Clone)]
pub enum ListItemMarker {
    /// Text marker with shaped glyph runs extracted from Blitz/Parley.
    Text {
        lines: Vec<crate::paragraph::ShapedLine>,
        width: Pt,
    },
    /// Image marker (list-style-image: url(...)) — raster or SVG.
    Image {
        marker: ImageMarker,
        /// Display width after clamp (pt).
        width: Pt,
        /// Display height after clamp (pt).
        height: Pt,
    },
    /// No marker — split trailing fragment or list-style-type: none.
    None,
}

/// A list item with an outside-positioned marker.
#[derive(Clone)]
pub struct ListItemPageable {
    /// Marker (text, image, or none).
    pub marker: ListItemMarker,
    /// Line-height of the first shaped line — used to vertically center
    /// image markers. Zero for `ListItemMarker::None`.
    pub marker_line_height: Pt,
    /// The list item's body content.
    pub body: Box<dyn Pageable>,
    /// Visual style (background, borders, padding).
    pub style: BlockStyle,
    /// Taffy-computed width.
    pub width: Pt,
    /// Cached height from wrap().
    pub height: Pt,
    /// CSS opacity (0.0–1.0), applied to both marker and body.
    pub opacity: f32,
    /// CSS visibility (false = hidden).
    pub visible: bool,
}

impl Pageable for ListItemPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        let body_size = self.body.wrap(avail_width, avail_height);
        self.height = body_size.height;
        Size {
            width: avail_width,
            height: self.height,
        }
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (top_body, bottom_body) = self.body.split(avail_width, avail_height)?;
        Some((
            Box::new(ListItemPageable {
                marker: self.marker.clone(),
                marker_line_height: self.marker_line_height,
                body: top_body,
                style: self.style.clone(),
                width: self.width,
                height: 0.0,
                opacity: self.opacity,
                visible: self.visible,
            }),
            Box::new(ListItemPageable {
                marker: ListItemMarker::None,
                marker_line_height: 0.0,
                body: bottom_body,
                style: self.style.clone(),
                width: self.width,
                height: 0.0,
                opacity: self.opacity,
                visible: self.visible,
            }),
        ))
    }

    fn split_boxed(self: Box<Self>, avail_width: Pt, avail_height: Pt) -> SplitResult {
        let me = *self;
        let (top_body, bottom_body) = match me.body.split_boxed(avail_width, avail_height) {
            Ok(pair) => pair,
            Err(body) => {
                return Err(Box::new(ListItemPageable { body, ..me }));
            }
        };
        Ok((
            Box::new(ListItemPageable {
                marker: me.marker,
                marker_line_height: me.marker_line_height,
                body: top_body,
                style: me.style.clone(),
                width: me.width,
                height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
            }),
            Box::new(ListItemPageable {
                marker: ListItemMarker::None,
                marker_line_height: 0.0,
                body: bottom_body,
                style: me.style,
                width: me.width,
                height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
            }),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
            if self.visible {
                match &self.marker {
                    ListItemMarker::Text { lines, width } if !lines.is_empty() => {
                        let marker_x = x - width;
                        crate::paragraph::draw_shaped_lines(canvas, lines, marker_x, y);
                    }
                    ListItemMarker::Image {
                        marker,
                        width,
                        height,
                    } => {
                        let marker_x = x - *width;
                        let marker_y = y + (self.marker_line_height - *height) / 2.0;
                        match marker {
                            ImageMarker::Raster(img) => {
                                img.draw(canvas, marker_x, marker_y, *width, *height);
                            }
                            ImageMarker::Svg(svg) => {
                                svg.draw(canvas, marker_x, marker_y, *width, *height);
                            }
                        }
                    }
                    _ => {}
                }
            }
            self.body.draw(canvas, x, y, avail_width, avail_height);
        });
    }

    fn pagination(&self) -> Pagination {
        self.body.pagination()
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.height
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn collect_ids(
        &self,
        x: Pt,
        y: Pt,
        avail_width: Pt,
        avail_height: Pt,
        registry: &mut DestinationRegistry,
    ) {
        // `draw` calls `self.body.draw(canvas, x, y, ...)` (markers drawn
        // at negative x); the body is the positional root for any nested
        // block ids, so walk it at the same (x, y).
        self.body
            .collect_ids(x, y, avail_width, avail_height, registry);
    }
}

// ─── TablePageable ─────────────────────────────────────

/// A table with repeating header on page breaks.
#[derive(Clone)]
pub struct TablePageable {
    /// Cells belonging to thead (repeated on each page)
    pub header_cells: Vec<PositionedChild>,
    /// Cells belonging to tbody (split across pages)
    pub body_cells: Vec<PositionedChild>,
    /// Height of the header row(s)
    pub header_height: Pt,
    /// Visual style (background, borders, border-radii)
    pub style: BlockStyle,
    /// Taffy-computed layout size
    pub layout_size: Option<Size>,
    /// Table width (preserved across splits)
    pub width: Pt,
    /// Cached height from wrap()
    pub cached_height: Pt,
    pub opacity: f32,
    pub visible: bool,
    /// HTML `id` attribute (trimmed, non-empty). Used as an anchor target
    /// for internal `href="#..."` links. `Arc<String>` so split fragments
    /// can share without cloning the string. Mirrors `BlockPageable::id`.
    pub id: Option<Arc<String>>,
}

impl Pageable for TablePageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        if let Some(ls) = self.layout_size {
            self.cached_height = ls.height;
            return ls;
        }
        let max_h = self
            .header_cells
            .iter()
            .chain(self.body_cells.iter())
            .fold(0.0f32, |acc, pc| acc.max(pc.y + pc.child.height()));
        self.cached_height = max_h;
        Size {
            width: self.width,
            height: max_h,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        // Find the first body cell that overflows the available height
        let overflow_index = self
            .body_cells
            .iter()
            .position(|pc| pc.y + pc.child.height() > avail_height);

        let overflow_index = match overflow_index {
            Some(0) | None => return None,
            Some(i) => i,
        };

        // Snap to the start of the row containing the overflow cell.
        // Cells in the same row share the same y coordinate.
        let overflow_y = self.body_cells[overflow_index].y;
        let split_index = self.body_cells[..overflow_index]
            .iter()
            .rposition(|pc| pc.y < overflow_y)
            .map(|i| i + 1)
            .unwrap_or(0);

        if split_index == 0 {
            return None;
        }

        let split_y = self.body_cells[split_index].y;

        let first_header = clone_children(&self.header_cells, 0.0);
        let first_body = clone_children(&self.body_cells[..split_index], 0.0);

        let second_header = clone_children(&self.header_cells, 0.0);
        let second_body: Vec<PositionedChild> = self.body_cells[split_index..]
            .iter()
            .map(|pc| PositionedChild {
                child: pc.child.clone_box(),
                x: pc.x,
                y: self.header_height + (pc.y - split_y),
            })
            .collect();

        Some((
            Box::new(TablePageable {
                header_cells: first_header,
                body_cells: first_body,
                header_height: self.header_height,
                style: self.style.clone(),
                layout_size: None,
                width: self.width,
                cached_height: 0.0,
                opacity: self.opacity,
                visible: self.visible,
                id: self.id.clone(),
            }),
            Box::new(TablePageable {
                header_cells: second_header,
                body_cells: second_body,
                header_height: self.header_height,
                style: self.style.clone(),
                layout_size: None,
                width: self.width,
                cached_height: 0.0,
                opacity: self.opacity,
                visible: self.visible,
                id: self.id.clone(),
            }),
        ))
    }

    fn split_boxed(self: Box<Self>, _avail_width: Pt, avail_height: Pt) -> SplitResult {
        // Find the first body cell that overflows the available height
        let overflow_index = self
            .body_cells
            .iter()
            .position(|pc| pc.y + pc.child.height() > avail_height);

        let overflow_index = match overflow_index {
            Some(0) | None => return Err(self),
            Some(i) => i,
        };

        // Snap to the start of the row containing the overflow cell.
        let overflow_y = self.body_cells[overflow_index].y;
        let split_index = self.body_cells[..overflow_index]
            .iter()
            .rposition(|pc| pc.y < overflow_y)
            .map(|i| i + 1)
            .unwrap_or(0);

        if split_index == 0 {
            return Err(self);
        }

        let split_y = self.body_cells[split_index].y;
        let mut me = *self;

        let mut second_body = me.body_cells.split_off(split_index);
        let first_body = me.body_cells;

        // Shift y coordinates on second fragment body cells
        let header_height = me.header_height;
        for pc in &mut second_body {
            pc.y = header_height + (pc.y - split_y);
        }

        // Headers: clone for first, move for second
        let first_header = clone_children(&me.header_cells, 0.0);

        Ok((
            Box::new(TablePageable {
                header_cells: first_header,
                body_cells: first_body,
                header_height,
                style: me.style.clone(),
                layout_size: None,
                width: me.width,
                cached_height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
                id: me.id.clone(),
            }),
            Box::new(TablePageable {
                header_cells: me.header_cells,
                body_cells: second_body,
                header_height,
                style: me.style,
                layout_size: None,
                width: me.width,
                cached_height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
                id: me.id,
            }),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, _avail_width: Pt, _avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
            let total_width = self.width;
            let total_height = self
                .layout_size
                .map(|s| s.height)
                .unwrap_or(self.cached_height);

            if self.visible {
                crate::background::draw_box_shadows(
                    canvas,
                    &self.style,
                    x,
                    y,
                    total_width,
                    total_height,
                );
                crate::background::draw_background(
                    canvas,
                    &self.style,
                    x,
                    y,
                    total_width,
                    total_height,
                );
                draw_block_border(canvas, &self.style, x, y, total_width, total_height);
            }

            // overflow clipping: clip header + body cells to the padding box.
            // Background and borders are drawn outside the clip so the
            // table's border renders at its full border-box edge.
            let clip_pushed = if let Some(clip_path) =
                compute_overflow_clip_path(&self.style, x, y, total_width, total_height)
            {
                canvas
                    .surface
                    .push_clip_path(&clip_path, &krilla::paint::FillRule::default());
                true
            } else {
                false
            };

            for pc in self.header_cells.iter().chain(self.body_cells.iter()) {
                pc.child
                    .draw(canvas, x + pc.x, y + pc.y, total_width, pc.child.height());
            }

            if clip_pushed {
                canvas.surface.pop();
            }
        });
    }

    fn pagination(&self) -> Pagination {
        Pagination::default()
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.layout_size
            .map(|s| s.height)
            .unwrap_or(self.cached_height)
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
        registry: &mut DestinationRegistry,
    ) {
        // Record this table's own id at its top-left (mirrors BlockPageable).
        if let Some(id) = &self.id
            && !id.is_empty()
        {
            registry.record(id, x, y);
        }
        // Mirror TablePageable::draw child iteration — header + body cells.
        let total_width = self.width;
        for pc in self.header_cells.iter().chain(self.body_cells.iter()) {
            pc.child
                .collect_ids(x + pc.x, y + pc.y, total_width, pc.child.height(), registry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spacer(h: Pt) -> Box<dyn Pageable> {
        let mut s = SpacerPageable::new(h);
        s.wrap(100.0, 1000.0);
        Box::new(s)
    }

    #[test]
    fn test_block_fits_on_one_page() {
        let mut block = BlockPageable::new(vec![make_spacer(100.0), make_spacer(100.0)]);
        block.wrap(200.0, 300.0);
        assert!(block.split(200.0, 300.0).is_none());
    }

    #[test]
    fn test_block_splits_across_pages() {
        let mut block = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        block.wrap(200.0, 1000.0);
        let result = block.split(200.0, 250.0);
        assert!(result.is_some());
        let (first, second) = result.unwrap();
        let mut first = first;
        let mut second = second;
        let s1 = first.wrap(200.0, 250.0);
        let s2 = second.wrap(200.0, 1000.0);
        assert!((s1.height - 200.0).abs() < 0.01);
        assert!((s2.height - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_break_before_page() {
        let breaking = BlockPageable::new(vec![make_spacer(50.0)]).with_pagination(Pagination {
            break_before: BreakBefore::Page,
            ..Pagination::default()
        });
        let mut breaking = breaking;
        breaking.wrap(200.0, 1000.0);

        let mut block = BlockPageable::new(vec![
            make_spacer(50.0),
            make_spacer(50.0),
            Box::new(breaking),
        ]);
        block.wrap(200.0, 1000.0);

        // Even though everything fits in 1000pt, break-before should force split
        let result = block.split(200.0, 1000.0);
        assert!(result.is_some());
    }

    #[test]
    fn test_break_inside_avoid() {
        let block = BlockPageable::new(vec![make_spacer(200.0)]).with_pagination(Pagination {
            break_inside: BreakInside::Avoid,
            ..Pagination::default()
        });
        let mut block = block;
        block.wrap(200.0, 1000.0);
        // Even if it doesn't fit, split returns None
        assert!(block.split(200.0, 100.0).is_none());
    }

    #[test]
    fn test_list_item_delegates_to_body() {
        let body = make_spacer(100.0);
        let mut item = ListItemPageable {
            marker: ListItemMarker::Text {
                lines: Vec::new(),
                width: 20.0,
            },
            marker_line_height: 0.0,
            body,
            style: BlockStyle::default(),
            width: 200.0,
            height: 100.0,
            opacity: 1.0,
            visible: true,
        };
        let size = item.wrap(200.0, 1000.0);
        assert!((size.height - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_list_item_split_keeps_marker_on_first_part() {
        let mut body = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        body.wrap(200.0, 1000.0);
        let mut item = ListItemPageable {
            marker: ListItemMarker::Text {
                lines: Vec::new(),
                width: 20.0,
            },
            marker_line_height: 14.0,
            body: Box::new(body),
            style: BlockStyle::default(),
            width: 200.0,
            height: 300.0,
            opacity: 1.0,
            visible: true,
        };
        item.wrap(200.0, 1000.0);
        let result = item.split(200.0, 250.0);
        assert!(result.is_some());
        let (first, second) = result.unwrap();
        // First part keeps the text marker
        let first_item = first.as_any().downcast_ref::<ListItemPageable>().unwrap();
        match &first_item.marker {
            ListItemMarker::Text { width, .. } => assert!((*width - 20.0).abs() < 0.01),
            _ => panic!("expected Text marker on first fragment"),
        }
        // Second part has no marker
        let second_item = second.as_any().downcast_ref::<ListItemPageable>().unwrap();
        assert!(matches!(second_item.marker, ListItemMarker::None));
    }

    #[test]
    fn test_list_item_image_marker_split_keeps_on_first_part() {
        use crate::image::{ImageFormat, ImagePageable};
        use std::sync::Arc;

        let mut body = BlockPageable::new(vec![
            make_spacer(100.0),
            make_spacer(100.0),
            make_spacer(100.0),
        ]);
        body.wrap(200.0, 1000.0);

        // Dummy PNG bytes are not actually decoded — we only exercise
        // clone/split logic, not rendering.
        let img = ImagePageable::new(Arc::new(vec![0u8; 4]), ImageFormat::Png, 12.0, 12.0);

        let mut item = ListItemPageable {
            marker: ListItemMarker::Image {
                marker: ImageMarker::Raster(img),
                width: 12.0,
                height: 12.0,
            },
            marker_line_height: 14.0,
            body: Box::new(body),
            style: BlockStyle::default(),
            width: 200.0,
            height: 300.0,
            opacity: 1.0,
            visible: true,
        };
        item.wrap(200.0, 1000.0);
        let result = item.split(200.0, 250.0);
        assert!(result.is_some());
        let (first, second) = result.unwrap();

        let first_item = first.as_any().downcast_ref::<ListItemPageable>().unwrap();
        assert!(matches!(first_item.marker, ListItemMarker::Image { .. }));
        assert!((first_item.marker_line_height - 14.0).abs() < 0.01);

        let second_item = second.as_any().downcast_ref::<ListItemPageable>().unwrap();
        assert!(matches!(second_item.marker, ListItemMarker::None));
        assert_eq!(second_item.marker_line_height, 0.0);
    }

    #[test]
    fn test_clamp_marker_size_below_line_height() {
        // 16x16 px image (= 12x12 pt) with line-height 24 pt → stays intrinsic
        let (w, h) = clamp_marker_size(12.0, 12.0, 24.0);
        assert!((w - 12.0).abs() < 0.01);
        assert!((h - 12.0).abs() < 0.01);
    }

    #[test]
    fn test_clamp_marker_size_equal_line_height() {
        let (w, h) = clamp_marker_size(24.0, 24.0, 24.0);
        assert!((w - 24.0).abs() < 0.01);
        assert!((h - 24.0).abs() < 0.01);
    }

    #[test]
    fn test_clamp_marker_size_above_line_height_preserves_aspect() {
        // 64x48 pt with line-height 12 pt: scale = 12/48 = 0.25 → (16, 12)
        let (w, h) = clamp_marker_size(64.0, 48.0, 12.0);
        assert!((w - 16.0).abs() < 0.01);
        assert!((h - 12.0).abs() < 0.01);
    }

    #[test]
    fn test_clamp_marker_size_zero_intrinsic_height_returns_zero() {
        let (w, h) = clamp_marker_size(10.0, 0.0, 12.0);
        assert_eq!(w, 0.0);
        assert_eq!(h, 0.0);
    }

    #[test]
    fn test_table_split_boxed_repeats_headers_and_rebases_y() {
        // Header row at y=0, height=30
        let header = vec![PositionedChild {
            child: make_spacer(30.0),
            x: 0.0,
            y: 0.0,
        }];

        // Three body rows at y=30, y=80, y=130 (each 50pt tall)
        let body = vec![
            PositionedChild {
                child: make_spacer(50.0),
                x: 0.0,
                y: 30.0,
            },
            PositionedChild {
                child: make_spacer(50.0),
                x: 0.0,
                y: 80.0,
            },
            PositionedChild {
                child: make_spacer(50.0),
                x: 0.0,
                y: 130.0,
            },
        ];

        let mut table = TablePageable {
            header_cells: header,
            body_cells: body,
            header_height: 30.0,
            style: BlockStyle::default(),
            layout_size: None,
            width: 200.0,
            cached_height: 0.0,
            opacity: 1.0,
            visible: true,
            id: None,
        };
        table.wrap(200.0, 1000.0);

        // Available height = 120pt → header(30) + first body row(50) fits,
        // second body row at y=80 with height 50 overflows (80+50=130 > 120).
        let concrete: Box<TablePageable> = Box::new(table);

        let result = concrete.split_boxed(200.0, 120.0);
        assert!(result.is_ok(), "split_boxed should return Ok");

        let (first, second) = match result {
            Ok(pair) => pair,
            Err(_) => panic!("split_boxed returned Err"),
        };
        let first_table = first.as_any().downcast_ref::<TablePageable>().unwrap();
        let second_table = second.as_any().downcast_ref::<TablePageable>().unwrap();

        // Both fragments have headers
        assert_eq!(first_table.header_cells.len(), 1);
        assert_eq!(second_table.header_cells.len(), 1);

        // First fragment: 1 body row
        assert_eq!(first_table.body_cells.len(), 1);
        assert!((first_table.body_cells[0].y - 30.0).abs() < 0.01);

        // Second fragment: 2 body rows, y rebased to header_height
        assert_eq!(second_table.body_cells.len(), 2);
        // First body cell: header_height + (80 - 80) = 30
        assert!(
            (second_table.body_cells[0].y - 30.0).abs() < 0.01,
            "expected y=30.0, got {}",
            second_table.body_cells[0].y
        );
        // Second body cell: header_height + (130 - 80) = 80
        assert!(
            (second_table.body_cells[1].y - 80.0).abs() < 0.01,
            "expected y=80.0, got {}",
            second_table.body_cells[1].y
        );
    }

    #[test]
    fn test_string_set_pageable_zero_size() {
        let mut p = StringSetPageable::new("title".to_string(), "Chapter 1".to_string());
        let size = p.wrap(100.0, 100.0);
        assert_eq!(size.width, 0.0);
        assert_eq!(size.height, 0.0);
        assert_eq!(p.height(), 0.0);
    }

    #[test]
    fn test_string_set_pageable_no_split() {
        let p = StringSetPageable::new("title".to_string(), "Chapter 1".to_string());
        assert!(p.split(100.0, 100.0).is_none());
    }

    #[test]
    fn test_string_set_pageable_fields() {
        let p = StringSetPageable::new("title".to_string(), "Chapter 1".to_string());
        assert_eq!(p.name, "title");
        assert_eq!(p.value, "Chapter 1");
    }

    #[test]
    fn test_running_element_marker_is_zero_size_noop() {
        let mut m = RunningElementMarkerPageable::new("header".to_string(), 42);
        let size = m.wrap(100.0, 100.0);
        assert_eq!(size.width, 0.0);
        assert_eq!(size.height, 0.0);
        assert_eq!(m.height(), 0.0);
        assert_eq!(m.name, "header");
        assert_eq!(m.instance_id, 42);
        assert!(m.split(100.0, 100.0).is_none());
    }

    // ─── DestinationRegistry tests ───────────────────────────

    #[test]
    fn destination_registry_collects_block_ids_from_paginated_pages() {
        use crate::convert::{self, ConvertContext};
        use crate::gcpm::running::RunningElementStore;
        use std::collections::HashMap;

        // Use `<div>` wrappers (container nodes, always BlockPageable) so
        // the test does not depend on whether headings gain a block wrapper
        // via default CSS — `<h1>` is an inline root that only becomes a
        // BlockPageable when `needs_block_wrapper()` triggers on its style.
        let html = r##"<html><body>
            <div id="top" style="border:1px solid red">Top</div>
            <div style="height:2000px"></div>
            <div id="next" style="border:1px solid red">Next</div>
        </body></html>"##;
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
        let mut registry = DestinationRegistry::default();
        for (idx, p) in pages.iter().enumerate() {
            registry.set_current_page(idx);
            p.collect_ids(0.0, 0.0, 400.0, 600.0, &mut registry);
        }
        assert!(registry.get("top").is_some(), "missing top");
        assert!(registry.get("next").is_some(), "missing next");
        // `top` and `next` should be on different pages (or different y) since
        // the spacer between them is 2000px tall.
        assert_ne!(registry.get("top"), registry.get("next"));
    }

    #[test]
    fn destination_registry_ignores_blocks_without_id() {
        // A BlockPageable with `id: None` should not register anything.
        let mut block = BlockPageable::with_positioned_children(vec![]);
        block.wrap(100.0, 100.0);
        let mut registry = DestinationRegistry::default();
        registry.set_current_page(0);
        block.collect_ids(0.0, 0.0, 100.0, 100.0, &mut registry);
        assert!(registry.get("anything").is_none());
    }

    #[test]
    fn destination_registry_captures_paragraph_id_for_headings() {
        // Headings like `<h1 id="top">` are inline roots that typically
        // become `ParagraphPageable`, not `BlockPageable`. Ensure their ids
        // are still captured so `href="#top"` links resolve.
        use crate::convert::{self, ConvertContext};
        use crate::gcpm::running::RunningElementStore;
        use std::collections::HashMap;

        let html = r##"<html><body>
            <h1 id="top">Top</h1>
            <div style="height:2000px"></div>
            <h2 id="next">Next</h2>
        </body></html>"##;
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
        let mut registry = DestinationRegistry::default();
        for (idx, p) in pages.iter().enumerate() {
            registry.set_current_page(idx);
            p.collect_ids(0.0, 0.0, 400.0, 600.0, &mut registry);
        }
        assert!(
            registry.get("top").is_some(),
            "expected <h1 id=top> to be captured"
        );
        assert!(
            registry.get("next").is_some(),
            "expected <h2 id=next> to be captured"
        );
        assert_ne!(
            registry.get("top"),
            registry.get("next"),
            "headings separated by 2000px spacer should not share location"
        );
    }

    #[test]
    fn destination_registry_captures_table_id() {
        // Regression: `<table id=...>` previously skipped BlockPageable
        // wrapping, so TablePageable never registered its own id.
        use crate::convert::{self, ConvertContext};
        use crate::gcpm::running::RunningElementStore;
        use std::collections::HashMap;

        let html = r##"<html><body>
            <table id="data"><tr><td>x</td></tr></table>
        </body></html>"##;
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
        let mut registry = DestinationRegistry::default();
        for (idx, p) in pages.iter().enumerate() {
            registry.set_current_page(idx);
            p.collect_ids(0.0, 0.0, 400.0, 600.0, &mut registry);
        }
        assert!(
            registry.get("data").is_some(),
            "table id was not captured (entries: {:?})",
            registry
        );
    }

    #[test]
    fn destination_registry_first_write_wins_for_duplicate_ids() {
        let mut reg = DestinationRegistry::default();
        reg.set_current_page(0);
        reg.record("dup", 0.0, 10.0);
        reg.set_current_page(2);
        reg.record("dup", 0.0, 99.0);
        assert_eq!(reg.get("dup"), Some((0, 0.0, 10.0)));
    }
}

#[cfg(test)]
mod background_tests {
    use super::*;

    #[test]
    fn test_background_layer_defaults() {
        let style = BlockStyle::default();
        assert!(style.background_layers.is_empty());
    }

    #[test]
    fn test_has_visual_style_with_background_layer() {
        let mut style = BlockStyle::default();
        assert!(!style.has_visual_style());
        style.background_layers.push(BackgroundLayer {
            content: BgImageContent::Raster {
                data: Arc::new(vec![]),
                format: ImageFormat::Png,
            },
            intrinsic_width: 100.0,
            intrinsic_height: 100.0,
            size: BgSize::Auto,
            position_x: BgLengthPercentage::Percentage(0.0),
            position_y: BgLengthPercentage::Percentage(0.0),
            repeat_x: BgRepeat::Repeat,
            repeat_y: BgRepeat::Repeat,
            origin: BgBox::PaddingBox,
            clip: BgClip::BorderBox,
        });
        assert!(style.has_visual_style());
    }

    #[test]
    fn has_visual_style_with_only_box_shadow() {
        let style = BlockStyle {
            box_shadows: vec![BoxShadow {
                offset_x: 2.0,
                offset_y: 2.0,
                blur: 0.0,
                spread: 0.0,
                color: [0, 0, 0, 255],
                inset: false,
            }],
            ..Default::default()
        };
        assert!(style.has_visual_style());
    }

    /// Pin BoxShadow default values to guard against accidental derive changes.
    #[test]
    fn box_shadow_default_values() {
        let d = BoxShadow::default();
        assert_eq!(d.offset_x, 0.0);
        assert_eq!(d.offset_y, 0.0);
        assert_eq!(d.blur, 0.0);
        assert_eq!(d.spread, 0.0);
        assert_eq!(d.color, [0, 0, 0, 0]);
        assert!(!d.inset);
    }
}

#[cfg(test)]
mod overflow_tests {
    use super::*;

    #[test]
    fn test_overflow_default_is_visible() {
        let style = BlockStyle::default();
        assert_eq!(style.overflow_x, Overflow::Visible);
        assert_eq!(style.overflow_y, Overflow::Visible);
    }

    #[test]
    fn test_overflow_clip_flag() {
        let mut style = BlockStyle {
            overflow_x: Overflow::Clip,
            ..Default::default()
        };
        assert!(style.has_overflow_clip());
        style.overflow_x = Overflow::Visible;
        style.overflow_y = Overflow::Clip;
        assert!(style.has_overflow_clip());
        style.overflow_y = Overflow::Visible;
        assert!(!style.has_overflow_clip());
    }

    #[test]
    fn test_clip_path_visible_returns_none() {
        let style = BlockStyle::default();
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_none());
    }

    #[test]
    fn test_clip_path_both_axes_rect() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Clip,
            ..Default::default()
        };
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(path.is_some(), "both-axes clip should produce a path");
    }

    #[test]
    fn test_clip_path_axis_x_only() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            // overflow_y stays Visible
            ..Default::default()
        };
        let path = compute_overflow_clip_path(&style, 10.0, 20.0, 100.0, 50.0);
        assert!(path.is_some(), "x-only clip should produce a path");
        // NOTE: krilla::geom::Path does not expose a bounds() accessor in
        // 0.7, so we cannot assert on the rect dimensions directly. The
        // axis-independent widening is covered indirectly via the
        // implementation's branching on `Overflow::Clip`.
    }

    #[test]
    fn test_clip_path_axis_y_only() {
        let style = BlockStyle {
            overflow_y: Overflow::Clip,
            // overflow_x stays Visible
            ..Default::default()
        };
        let path = compute_overflow_clip_path(&style, 10.0, 20.0, 100.0, 50.0);
        assert!(path.is_some(), "y-only clip should produce a path");
    }

    #[test]
    fn test_clip_path_with_border_inset() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Clip,
            border_widths: [2.0, 3.0, 4.0, 5.0], // top, right, bottom, left
            ..Default::default()
        };
        // border-box 100x100 at origin 0,0 → padding-box is (5, 2) to (97, 96)
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(path.is_some(), "should produce a clip path");
    }

    #[test]
    fn test_clip_path_rounded_both_axes() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Clip,
            border_radii: [[10.0, 10.0]; 4],
            ..Default::default()
        };
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(path.is_some(), "rounded clip should produce a path");
    }

    #[test]
    fn test_clip_path_zero_padding_box_returns_none() {
        // If border eats the entire box, padding-box has zero or negative size
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            overflow_y: Overflow::Clip,
            border_widths: [50.0, 50.0, 50.0, 50.0], // 100 total on each axis
            ..Default::default()
        };
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(path.is_none(), "zero padding-box should return None");
    }

    #[test]
    fn test_clip_path_axis_x_only_survives_zero_height() {
        // `overflow-x: hidden; overflow-y: visible` with a collapsed height
        // (e.g. borders eating all the vertical space) must still produce a
        // clip path: the non-clipped axis is expanded to ±INFINITE so zero
        // `pb_h` is harmless.
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            // overflow_y stays Visible
            border_widths: [50.0, 0.0, 50.0, 0.0], // top+bottom = 100, same as h
            ..Default::default()
        };
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(
            path.is_some(),
            "x-only clip should survive a collapsed padding-box height"
        );
    }

    #[test]
    fn test_clip_path_axis_y_only_survives_zero_width() {
        // Symmetric: `overflow-y: hidden; overflow-x: visible` with collapsed
        // width must still produce a clip path.
        let style = BlockStyle {
            overflow_y: Overflow::Clip,
            // overflow_x stays Visible
            border_widths: [0.0, 50.0, 0.0, 50.0], // left+right = 100, same as w
            ..Default::default()
        };
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(
            path.is_some(),
            "y-only clip should survive a collapsed padding-box width"
        );
    }

    #[test]
    fn test_clip_path_axis_x_only_returns_none_on_zero_clipped_axis() {
        // If the *clipped* axis has zero size, no meaningful clip is
        // possible and the helper should return None.
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            // overflow_y stays Visible
            border_widths: [0.0, 50.0, 0.0, 50.0], // width collapses to 0
            ..Default::default()
        };
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(
            path.is_none(),
            "x-only clip with zero pb_w should return None"
        );
    }

    #[test]
    fn test_block_draw_has_no_clip_by_default() {
        // Default BlockStyle has both axes Visible, so has_overflow_clip is false.
        let style = BlockStyle::default();
        assert!(!style.has_overflow_clip());
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_none());
    }

    #[test]
    fn test_block_draw_has_clip_when_configured() {
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            ..Default::default()
        };
        assert!(style.has_overflow_clip());
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_some());
    }

    #[test]
    fn test_needs_block_wrapper_for_overflow_only() {
        // A bare overflow:hidden style (no background, border, padding,
        // radius) must still require a BlockPageable wrapper.
        let style = BlockStyle {
            overflow_x: Overflow::Clip,
            ..Default::default()
        };
        assert!(!style.has_visual_style());
        assert!(!style.has_radius());
        assert!(style.has_overflow_clip());
        assert!(style.needs_block_wrapper());
    }

    #[test]
    fn test_needs_block_wrapper_default_is_false() {
        let style = BlockStyle::default();
        assert!(!style.needs_block_wrapper());
    }
}

/// Float-tolerance helpers shared across the in-crate transform test
/// modules (`affine_tests`, `transform_wrapper_tests`, and the
/// `transform_tests` module in `blitz_adapter.rs`).
#[cfg(test)]
pub(crate) mod matrix_test_util {
    use super::Affine2D;

    pub(crate) const EPS: f32 = 1e-5;

    pub(crate) fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    pub(crate) fn matrix_approx(a: &Affine2D, b: &Affine2D) -> bool {
        approx(a.a, b.a)
            && approx(a.b, b.b)
            && approx(a.c, b.c)
            && approx(a.d, b.d)
            && approx(a.e, b.e)
            && approx(a.f, b.f)
    }
}

#[cfg(test)]
mod affine_tests {
    use super::matrix_test_util::{approx, matrix_approx};
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[test]
    fn identity_is_identity() {
        assert!(Affine2D::IDENTITY.is_identity());
        let m = Affine2D::translation(3.0, 4.0);
        assert!(matrix_approx(&(m * Affine2D::IDENTITY), &m));
        assert!(matrix_approx(&(Affine2D::IDENTITY * m), &m));
    }

    #[test]
    fn rotation_90_maps_unit_vector() {
        let r = Affine2D::rotation(FRAC_PI_2);
        let x = r.a * 1.0 + r.c * 0.0 + r.e;
        let y = r.b * 1.0 + r.d * 0.0 + r.f;
        assert!(approx(x, 0.0), "x expected 0.0, got {x}");
        assert!(approx(y, 1.0), "y expected 1.0, got {y}");
    }

    #[test]
    fn translation_times_rotation_is_non_commutative() {
        let t = Affine2D::translation(10.0, 0.0);
        let r = Affine2D::rotation(FRAC_PI_2);
        assert!(
            !matrix_approx(&(t * r), &(r * t)),
            "expected non-commutative result"
        );
    }

    #[test]
    fn is_identity_tolerates_epsilon() {
        let almost = Affine2D {
            a: 1.0 + 1e-7,
            b: 1e-7,
            c: -1e-7,
            d: 1.0 - 1e-7,
            e: 1e-7,
            f: -1e-7,
        };
        assert!(almost.is_identity());
    }

    #[test]
    fn scale_matrix_has_correct_diagonal() {
        let s = Affine2D::scale(2.0, 3.0);
        assert!(approx(s.a, 2.0));
        assert!(approx(s.d, 3.0));
        assert!(approx(s.b, 0.0));
        assert!(approx(s.c, 0.0));
        assert!(approx(s.e, 0.0));
        assert!(approx(s.f, 0.0));
    }

    #[test]
    fn transform_point_identity() {
        let m = Affine2D::IDENTITY;
        let (x, y) = m.transform_point(10.0, 20.0);
        assert!((x - 10.0).abs() < 1e-5);
        assert!((y - 20.0).abs() < 1e-5);
    }

    #[test]
    fn transform_point_translation() {
        let m = Affine2D::translation(5.0, -3.0);
        let (x, y) = m.transform_point(10.0, 20.0);
        assert!((x - 15.0).abs() < 1e-5);
        assert!((y - 17.0).abs() < 1e-5);
    }

    #[test]
    fn transform_point_rotation_90() {
        let m = Affine2D::rotation(std::f32::consts::FRAC_PI_2);
        let (x, y) = m.transform_point(1.0, 0.0);
        assert!((x - 0.0).abs() < 1e-4);
        assert!((y - 1.0).abs() < 1e-4);
    }

    #[test]
    fn transform_rect_identity_produces_axis_aligned_quad() {
        let r = Rect {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        };
        let q = Affine2D::IDENTITY.transform_rect(&r);
        let expected = [[10.0, 60.0], [40.0, 60.0], [40.0, 20.0], [10.0, 20.0]];
        for (i, (got, exp)) in q.points.iter().zip(expected.iter()).enumerate() {
            assert!((got[0] - exp[0]).abs() < 1e-5, "quad[{i}].x");
            assert!((got[1] - exp[1]).abs() < 1e-5, "quad[{i}].y");
        }
    }

    #[test]
    fn transform_rect_translate() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let q = Affine2D::translation(100.0, 200.0).transform_rect(&r);
        assert!((q.points[0][0] - 100.0).abs() < 1e-5);
        assert!((q.points[0][1] - 210.0).abs() < 1e-5);
    }

    #[test]
    fn quad_is_degenerate_after_scale_zero() {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let q = Affine2D::scale(0.0, 1.0).transform_rect(&r);
        assert!(
            q.is_degenerate(),
            "scaleX(0) should produce degenerate quad"
        );

        let q2 = Affine2D::scale(1.0, 0.0).transform_rect(&r);
        assert!(
            q2.is_degenerate(),
            "scaleY(0) should produce degenerate quad"
        );
    }

    #[test]
    fn quad_is_not_degenerate_for_normal_transform() {
        let r = Rect {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        };
        let q = Affine2D::rotation(std::f32::consts::FRAC_PI_4).transform_rect(&r);
        assert!(!q.is_degenerate(), "rotated rect should not be degenerate");
    }
}

#[cfg(test)]
mod transform_wrapper_tests {
    use super::matrix_test_util::approx;
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[derive(Clone)]
    struct StubPageable {
        w: Pt,
        h: Pt,
    }

    impl Pageable for StubPageable {
        fn wrap(&mut self, _: Pt, _: Pt) -> Size {
            Size {
                width: self.w,
                height: self.h,
            }
        }
        fn split(&self, _: Pt, _: Pt) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
            None
        }
        fn draw(&self, _: &mut Canvas<'_, '_>, _: Pt, _: Pt, _: Pt, _: Pt) {}
        fn clone_box(&self) -> Box<dyn Pageable> {
            Box::new(self.clone())
        }
        fn height(&self) -> Pt {
            self.h
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn wrap(matrix: Affine2D, origin: Point2) -> TransformWrapperPageable {
        TransformWrapperPageable::new(
            Box::new(StubPageable { w: 100.0, h: 100.0 }),
            matrix,
            origin,
        )
    }

    #[test]
    fn translate_only_matrix() {
        let w = wrap(Affine2D::translation(10.0, 20.0), Point2::new(0.0, 0.0));
        let m = w.effective_matrix(0.0, 0.0);
        assert!(approx(m.e, 10.0));
        assert!(approx(m.f, 20.0));
        assert!(approx(m.a, 1.0));
        assert!(approx(m.d, 1.0));
    }

    #[test]
    fn rotate_90_maps_unit_vector_at_origin_zero() {
        let w = wrap(Affine2D::rotation(FRAC_PI_2), Point2::new(0.0, 0.0));
        let m = w.effective_matrix(0.0, 0.0);
        let x = m.a * 1.0 + m.c * 0.0 + m.e;
        let y = m.b * 1.0 + m.d * 0.0 + m.f;
        assert!(approx(x, 0.0), "x expected 0.0, got {x}");
        assert!(approx(y, 1.0), "y expected 1.0, got {y}");
    }

    #[test]
    fn rotate_with_center_origin_fixes_center() {
        // A 100×100 box rotated 90° around its center must leave the center
        // point fixed — verified through the composed matrix rather than
        // any intermediate step.
        let w = wrap(Affine2D::rotation(FRAC_PI_2), Point2::new(50.0, 50.0));
        let m = w.effective_matrix(0.0, 0.0);
        let x = m.a * 50.0 + m.c * 50.0 + m.e;
        let y = m.b * 50.0 + m.d * 50.0 + m.f;
        assert!(approx(x, 50.0), "origin x should be fixed, got {x}");
        assert!(approx(y, 50.0), "origin y should be fixed, got {y}");
    }

    #[test]
    fn rotate_with_center_origin_fixes_absolute_center_at_nonzero_draw_position() {
        // Same property at a non-zero draw position. Catches regressions
        // where effective_matrix() drops the (draw_x, draw_y) addition: the
        // absolute fixed point in canvas coordinates must be (10+50, 20+50)
        // = (60, 70).
        let w = wrap(Affine2D::rotation(FRAC_PI_2), Point2::new(50.0, 50.0));
        let m = w.effective_matrix(10.0, 20.0);
        let x = m.a * 60.0 + m.c * 70.0 + m.e;
        let y = m.b * 60.0 + m.d * 70.0 + m.f;
        assert!(
            approx(x, 60.0),
            "absolute origin x should be fixed, got {x}"
        );
        assert!(
            approx(y, 70.0),
            "absolute origin y should be fixed, got {y}"
        );
    }

    #[test]
    fn split_is_always_none() {
        let w = wrap(Affine2D::rotation(FRAC_PI_2), Point2::new(0.0, 0.0));
        assert!(w.split(1000.0, 1000.0).is_none());
    }

    #[test]
    fn wrap_delegates_to_inner_size() {
        let mut w = wrap(Affine2D::rotation(FRAC_PI_2), Point2::new(0.0, 0.0));
        let size = w.wrap(1000.0, 1000.0);
        assert!(approx(size.width, 100.0));
        assert!(approx(size.height, 100.0));
    }

    #[test]
    fn bookmark_marker_is_zero_sized_and_draws_nothing() {
        let m = BookmarkMarkerPageable::new(1, "Chapter 1".to_string());
        let size = {
            let mut c = m.clone();
            c.wrap(100.0, 100.0)
        };
        assert_eq!(size.width, 0.0);
        assert_eq!(size.height, 0.0);
        assert_eq!(m.height(), 0.0);
        assert_eq!(m.level, 1);
        assert_eq!(m.label, "Chapter 1");
    }

    #[test]
    fn bookmark_wrapper_keeps_marker_with_first_fragment() {
        // Build a splittable block: two 500pt spacers stacked so the
        // boundary at y=500 is inside the available 500pt window.
        let mut top = SpacerPageable::new(500.0);
        top.wrap(500.0, 1000.0);
        let mut bot = SpacerPageable::new(500.0);
        bot.wrap(500.0, 1000.0);
        let mut block = BlockPageable::with_positioned_children(vec![
            PositionedChild {
                child: Box::new(top),
                x: 0.0,
                y: 0.0,
            },
            PositionedChild {
                child: Box::new(bot),
                x: 0.0,
                y: 500.0,
            },
        ]);
        block.wrap(500.0, 1000.0);

        let child: Box<dyn Pageable> = Box::new(block);
        let marker = BookmarkMarkerPageable::new(1, "Title".into());
        let wrapper = BookmarkMarkerWrapperPageable::new(marker, child);

        // Split at 500pt.
        let split = wrapper.split(500.0, 500.0);
        let (first, _second) = split.expect("tall child must split");

        // First must contain the BookmarkMarkerPageable.
        let any = first.as_any();
        let w = any
            .downcast_ref::<BookmarkMarkerWrapperPageable>()
            .expect("first fragment wraps marker");
        assert_eq!(w.marker.label, "Title");
    }

    #[test]
    fn bookmark_wrapper_forwards_pagination() {
        let block = BlockPageable::with_positioned_children(vec![]).with_pagination(Pagination {
            break_before: BreakBefore::Page,
            ..Pagination::default()
        });
        let wrapper = BookmarkMarkerWrapperPageable::new(
            BookmarkMarkerPageable::new(1, "T".into()),
            Box::new(block),
        );
        assert_eq!(wrapper.pagination().break_before, BreakBefore::Page);
    }

    #[test]
    fn bookmark_collector_records_entry_on_draw() {
        use crate::pageable::BookmarkCollector;
        let mut collector = BookmarkCollector::new();
        collector.set_current_page(2);

        let marker = BookmarkMarkerPageable::new(2, "Section".to_string());

        // Build a krilla surface stand-in. Since we can't easily construct a real
        // Surface in unit tests, only verify the collector path: the marker
        // records to the collector via a helper, not via Canvas plumbing directly.
        //
        // Therefore: expose a `BookmarkMarkerPageable::record_if_collecting(y, collector)`
        // helper that the test calls directly.
        marker.record_if_collecting(42.0, Some(&mut collector));

        let entries = collector.into_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page_idx, 2);
        assert_eq!(entries[0].y_pt, 42.0);
        assert_eq!(entries[0].level, 2);
        assert_eq!(entries[0].label, "Section");
    }
}

#[cfg(test)]
mod link_collector_transform_tests {
    use super::*;
    use crate::paragraph::{LinkSpan, LinkTarget};
    use std::sync::Arc;

    fn make_link() -> Arc<LinkSpan> {
        Arc::new(LinkSpan {
            target: LinkTarget::External(Arc::new("https://test.example".to_string())),
            alt_text: None,
        })
    }

    #[test]
    fn push_rect_without_transform_stores_axis_aligned_quad() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        let link = make_link();
        lc.push_rect(
            &link,
            Rect {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 10.0,
            },
        );
        let occs = lc.into_occurrences();
        assert_eq!(occs.len(), 1);
        assert_eq!(occs[0].quads.len(), 1);
        let q = &occs[0].quads[0];
        // Bottom-left of rect at (10, 20, w=30, h=10): (10, 30)
        assert!((q.points[0][0] - 10.0).abs() < 1e-5, "bl.x");
        assert!((q.points[0][1] - 30.0).abs() < 1e-5, "bl.y");
    }

    #[test]
    fn push_rect_with_translation_shifts_quad() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        lc.push_transform(Affine2D::translation(100.0, 200.0));
        let link = make_link();
        lc.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
        );
        lc.pop_transform();
        let occs = lc.into_occurrences();
        let q = &occs[0].quads[0];
        // BL: (0,10) + translate(100,200) = (100, 210)
        assert!((q.points[0][0] - 100.0).abs() < 1e-5);
        assert!((q.points[0][1] - 210.0).abs() < 1e-5);
    }

    #[test]
    fn nested_transforms_compose() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        lc.push_transform(Affine2D::translation(10.0, 0.0));
        lc.push_transform(Affine2D::translation(0.0, 20.0));
        let link = make_link();
        lc.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 5.0,
                height: 5.0,
            },
        );
        lc.pop_transform();
        lc.pop_transform();
        let occs = lc.into_occurrences();
        let q = &occs[0].quads[0];
        // BL: (0,5) + translate(10,20) = (10, 25)
        assert!((q.points[0][0] - 10.0).abs() < 1e-5);
        assert!((q.points[0][1] - 25.0).abs() < 1e-5);
    }

    #[test]
    fn rotation_produces_non_axis_aligned_quad() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        lc.push_transform(Affine2D::rotation(std::f32::consts::FRAC_PI_2));
        let link = make_link();
        lc.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 5.0,
            },
        );
        lc.pop_transform();
        let occs = lc.into_occurrences();
        let q = &occs[0].quads[0];
        // After 90° rotation: (x,y) → (-y, x)
        // BL (0,5) → (-5, 0)
        assert!((q.points[0][0] - (-5.0)).abs() < 1e-4, "bl.x after rot90");
        assert!((q.points[0][1] - 0.0).abs() < 1e-4, "bl.y after rot90");
    }

    #[test]
    fn empty_transform_stack_is_identity() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        let link = make_link();
        lc.push_rect(
            &link,
            Rect {
                x: 5.0,
                y: 10.0,
                width: 20.0,
                height: 15.0,
            },
        );
        let occs = lc.into_occurrences();
        let q = &occs[0].quads[0];
        // TL corner untransformed: (5, 10)
        assert!((q.points[3][0] - 5.0).abs() < 1e-5, "tl.x identity");
        assert!((q.points[3][1] - 10.0).abs() < 1e-5, "tl.y identity");
    }

    #[test]
    fn scale_zero_x_produces_no_occurrence() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        lc.push_transform(Affine2D::scale(0.0, 1.0));
        let link = make_link();
        lc.push_rect(
            &link,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
        );
        lc.pop_transform();
        let occs = lc.into_occurrences();
        assert!(
            occs.is_empty(),
            "scaleX(0) should produce no link occurrence"
        );
    }
}

#[cfg(test)]
mod dest_registry_transform_tests {
    use super::*;

    #[test]
    fn record_without_transform_stores_original_coords() {
        let mut reg = DestinationRegistry::new();
        reg.set_current_page(0);
        reg.record("sec", 10.0, 50.0);
        let (page, x, y) = reg.get("sec").unwrap();
        assert_eq!(page, 0);
        assert!((x - 10.0).abs() < 1e-5);
        assert!((y - 50.0).abs() < 1e-5);
    }

    #[test]
    fn record_with_translation_shifts_coords() {
        let mut reg = DestinationRegistry::new();
        reg.set_current_page(0);
        reg.push_transform(Affine2D::translation(100.0, 200.0));
        reg.record("sec", 10.0, 50.0);
        reg.pop_transform();
        let (page, x, y) = reg.get("sec").unwrap();
        assert_eq!(page, 0);
        assert!((x - 110.0).abs() < 1e-5);
        assert!((y - 250.0).abs() < 1e-5);
    }

    #[test]
    fn nested_transforms_compose_in_registry() {
        let mut reg = DestinationRegistry::new();
        reg.set_current_page(0);
        reg.push_transform(Affine2D::translation(10.0, 0.0));
        reg.push_transform(Affine2D::translation(0.0, 20.0));
        reg.record("nested", 5.0, 5.0);
        reg.pop_transform();
        reg.pop_transform();
        let (_, x, y) = reg.get("nested").unwrap();
        assert!((x - 15.0).abs() < 1e-5);
        assert!((y - 25.0).abs() < 1e-5);
    }

    #[test]
    fn first_write_wins_even_with_transform() {
        let mut reg = DestinationRegistry::new();
        reg.set_current_page(0);
        reg.record("dup", 0.0, 10.0);
        reg.push_transform(Affine2D::translation(100.0, 100.0));
        reg.record("dup", 0.0, 10.0);
        reg.pop_transform();
        let (_, x, y) = reg.get("dup").unwrap();
        assert!((x - 0.0).abs() < 1e-5, "first write should win");
        assert!((y - 10.0).abs() < 1e-5, "first write should win");
    }
}
