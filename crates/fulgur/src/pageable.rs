use std::sync::Arc;

use crate::drawables::ListItemMarker;
use crate::gcpm::CounterOp;

pub use crate::draw_primitives::*;

/// Core pagination-aware layout trait.
pub trait Pageable: Send + Sync {
    /// Emit drawing commands.
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt);

    /// Clone this pageable into a boxed trait object.
    fn clone_box(&self) -> Box<dyn Pageable>;

    /// Measured height from last wrap() call.
    /// Downcast support for tests.
    fn as_any(&self) -> &dyn std::any::Any;

    /// Whether this pageable should be drawn. Defaults to `true`.
    ///
    /// Concrete types that track a `visibility: hidden` style (Block,
    /// Paragraph, ListItem, Table) override this to return their internal
    /// `visible` flag. Wrappers delegate to their inner pageable so the
    /// visibility of an inline-box host element propagates through
    /// `TransformWrapperPageable` / marker wrappers to the caller that
    /// needs to gate rendering (see `InlineBoxItem.visible`).
    /// The DOM `usize` NodeId this Pageable represents, if any. Used by
    /// `extract_drawables_from_pageable` to key per-node draw payloads in
    /// the `Drawables` side-channel. Wrappers delegate to their inner
    /// Pageable's `node_id` (markers do not have their own NodeId —
    /// they piggyback on the host element's geometry entry).
    ///
    /// Default `None` for impls that have no DOM correspondence
    /// (synthetic Pageables built by tests, etc.). Concrete plain types
    /// (`BlockPageable`, `SpacerPageable`) override to return their
    /// stored `node_id`; wrappers delegate to their inner Pageable.
    fn node_id(&self) -> Option<usize> {
        None
    }
}

impl Clone for Box<dyn Pageable> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

// ─── PositionedChild ─────────────────────────────────────

/// A child element with its Taffy-computed position.
///
/// `out_of_flow` (CSS 2.1 §10.6.4) marks `position: absolute|fixed`
/// children. They are excluded from `BlockPageable::wrap`'s height fold
/// (their height does not contribute to the parent's flow height) and
/// keep their CB-relative `y` (negative values allowed) so a tall abs
/// element naturally slices across the pages where its CB overlaps via
/// the fragmenter's geometry partition.
///
/// `is_fixed` is the additional sub-classification for `position: fixed`
/// (fulgur-jkl5). Both `position: absolute` and `position: fixed` are
/// `out_of_flow`, but they differ in **how the second-half y shift is
/// applied during pagination**:
///
/// - `position: absolute` is anchored to its abs-CB (typically body or
///   the nearest positioned ancestor). When body splits across pages,
///   the abs CB slides with body, so the child's y is shifted by
///   `split_y` to follow it. A tall abs element with `top: 0` naturally
///   becomes negative-y on the second page and Krilla clips the
///   already-painted top half — producing the correct slice.
/// - `position: fixed` is anchored to the **viewport / page area**, not
///   to body. It must appear at the same on-page coordinates on every
///   page. The second-half shift therefore has to be **suppressed** so
///   `y` stays at its viewport-relative value. Without this fix, a
///   fixed element placed at `top: 10px` ends up at
///   `10 - page_height` (i.e. ~-760pt for A4) on page 2 and is clipped
///   off the top of every page after the first.
#[derive(Clone)]
pub struct PositionedChild {
    pub child: Box<dyn Pageable>,
    pub x: Pt,
    pub y: Pt,
    /// Phase 4 PR 8f: child's measured height in PDF pt, captured at
    /// convert time from Taffy. Replaces the runtime
    /// `pc.height` trait dispatch that previously sourced this
    /// value via `Pageable::height`. Set to `0.0` by default so legacy
    /// tests that build `PositionedChild` directly without sizing
    /// information remain valid; production constructors always
    /// populate it from `size_in_pt(node.final_layout.size)`.
    pub height: Pt,
    pub out_of_flow: bool,
    /// fulgur-jkl5: subset of `out_of_flow` where the child is
    /// `position: fixed`. The element appears on every page at the same
    /// viewport-relative coordinates. Always implies `out_of_flow`.
    /// Defaults to `false`; constructors `in_flow` / `out_of_flow` keep
    /// it `false`, callers that need the fixed distinction use
    /// [`Self::fixed`] or set the field explicitly.
    pub is_fixed: bool,
}

impl PositionedChild {
    /// Construct an in-flow child (default for normal block layout).
    pub fn in_flow(child: Box<dyn Pageable>, x: Pt, y: Pt) -> Self {
        Self {
            child,
            x,
            y,
            height: 0.0,
            out_of_flow: false,
            is_fixed: false,
        }
    }

    /// Construct an out-of-flow child (`position: absolute`). The
    /// caller must have already resolved `(x, y)` against the appropriate
    /// containing block (CSS 2.1 §10.3.7 / §10.6.4) and expressed it
    /// relative to the parent's border-box.
    pub fn out_of_flow(child: Box<dyn Pageable>, x: Pt, y: Pt) -> Self {
        Self {
            child,
            x,
            y,
            height: 0.0,
            out_of_flow: true,
            is_fixed: false,
        }
    }

    /// Construct a viewport-anchored child (`position: fixed`).
    /// Differs from [`Self::out_of_flow`] only in `is_fixed: true` —
    /// the y coordinate is preserved verbatim across page splits so
    /// the element appears at the same on-page position on every page
    /// (Chrome-compatible repetition for paged media — fulgur-jkl5).
    pub fn fixed(child: Box<dyn Pageable>, x: Pt, y: Pt) -> Self {
        Self {
            child,
            x,
            y,
            height: 0.0,
            out_of_flow: true,
            is_fixed: true,
        }
    }
}

// ─── BlockPageable ───────────────────────────────────────

/// A block container that positions children using Taffy layout coordinates.
/// Handles margin/border/padding/background and page splitting.
#[derive(Clone)]
pub struct BlockPageable {
    pub children: Vec<PositionedChild>,
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
    /// fulgur-r6we (Phase 3.2.a): DOM `usize` NodeId. Lets
    /// `slice_for_page` look up the matching `Fragment` in
    /// `PaginationGeometryTable`, which is keyed by NodeId. Default
    /// `None` for legacy / test-only constructions; convert.rs sets it
    /// for production-built BlockPageables. Split fragments share the
    /// originating node's id (both halves represent the same DOM
    /// element on different pages).
    pub node_id: Option<usize>,
}

impl BlockPageable {
    pub fn new(children: Vec<Box<dyn Pageable>>) -> Self {
        // Legacy test-only constructor: stacks children vertically with zero
        // heights. After Phase 4 PR 8f's `Pageable::height` removal, the
        // y-advance per child can no longer source the height through trait
        // dispatch — production builds always use
        // `with_positioned_children` with explicit heights from Taffy, and
        // tests using this helper do not assert on per-child y positions.
        let positioned: Vec<PositionedChild> = children
            .into_iter()
            .map(|child| PositionedChild {
                child,
                x: 0.0,
                y: 0.0,
                height: 0.0,
                out_of_flow: false,
                is_fixed: false,
            })
            .collect();
        Self {
            children: positioned,
            cached_size: None,
            layout_size: None,
            style: BlockStyle::default(),
            opacity: 1.0,
            visible: true,
            id: None,
            node_id: None,
        }
    }

    pub fn with_positioned_children(children: Vec<PositionedChild>) -> Self {
        Self {
            children,
            cached_size: None,
            layout_size: None,
            style: BlockStyle::default(),
            opacity: 1.0,
            visible: true,
            id: None,
            node_id: None,
        }
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

    /// fulgur-r6we (Phase 3.2.a): set the DOM NodeId. Used by
    /// `slice_for_page` to look up the block's `Fragment` in
    /// `PaginationGeometryTable`. Plumbed from `convert::convert_node`
    /// — see `crates/fulgur/src/convert/`.
    pub fn with_node_id(mut self, node_id: Option<usize>) -> Self {
        self.node_id = node_id;
        self
    }
}

impl Pageable for BlockPageable {
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
                    .draw(canvas, x + pc.x, y + pc.y, avail_width, pc.height);
            }

            if clip_pushed {
                canvas.surface.pop();
            }
        });
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.node_id
    }
}

// ─── SpacerPageable ──────────────────────────────────────

/// Fixed-height vertical space. Cannot be split.
#[derive(Clone)]
pub struct SpacerPageable {
    pub height: Pt,
    /// fulgur-r6we (Phase 3.2.a): DOM NodeId for `slice_for_page`
    /// geometry lookup. See `BlockPageable::node_id`.
    pub node_id: Option<usize>,
}

impl SpacerPageable {
    pub fn new(height: Pt) -> Self {
        Self {
            height,
            node_id: None,
        }
    }

    pub fn with_node_id(mut self, node_id: Option<usize>) -> Self {
        self.node_id = node_id;
        self
    }
}

impl Pageable for SpacerPageable {
    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {
        // Spacers are invisible
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.node_id
    }
}

// ─── BookmarkMarkerPageable ──────────────────────────────

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
    fn draw(&self, canvas: &mut Canvas<'_, '_>, _x: Pt, y: Pt, _aw: Pt, _ah: Pt) {
        self.record_if_collecting(y, canvas.bookmark_collector.as_deref_mut());
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
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
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, aw: Pt, ah: Pt) {
        self.marker.draw(canvas, x, y, aw, ah);
        self.child.draw(canvas, x, y, aw, ah);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.child.node_id()
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
    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {}

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
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
    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {}

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
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
    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {}

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
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
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        self.child.draw(canvas, x, y, avail_width, avail_height);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.child.node_id()
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

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.inner.node_id()
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
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        self.child.draw(canvas, x, y, avail_width, avail_height);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.child.node_id()
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
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        self.child.draw(canvas, x, y, avail_width, avail_height);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.child.node_id()
    }
}

// ─── MulticolRulePageable ─────────────────────────────────

/// Wraps a multicol container child with the resolved `column-rule` spec and
/// the per-`ColumnGroup` geometry recorded by the Taffy multicol hook, so
/// `draw()` can paint vertical rules between adjacent non-empty columns
/// without re-running layout.
///
/// All `Pageable` methods delegate to `self.child` **except** `draw()` and
/// `split_boxed()`:
///
/// - `draw()` calls `child.draw()` first (columns paint their own
///   contents), then walks `self.groups` and strokes a vertical line in the
///   centre of each gutter whose adjacent columns are both non-empty. The
///   rule's vertical extent is clamped to the **shorter** of the two
///   adjacent columns' filled heights — this matches the spec intent that
///   a rule never hangs past the end of a shorter column.
/// - `split_boxed()` forwards to the child and redistributes `self.groups`
///   across the two halves based on the first half's consumed height.
///
/// # Geometry partitioning on split
///
/// After the child splits, the wrapper re-wraps the first fragment to
/// obtain a `cutoff` height (the amount of the container's content box
/// consumed on the first page). Groups are then placed as follows:
///
/// - Groups with `y_offset + max(col_heights) <= cutoff` stay on the first
///   half unchanged.
/// - Groups with `y_offset >= cutoff` move to the second half with
///   `y_offset -= cutoff`.
/// - Groups straddling the boundary are split across halves: the first
///   half keeps `col_heights` clamped to `cutoff - y_offset`, and the
///   continuation half carries `col_heights - remaining` at `y_offset =
///   0`. Cross-page pagination of a `ColumnGroup`'s *content* is still
///   approximate (fulgur-6q5 / fulgur-wfd), but the rule geometry paints
///   on both pages.
///
/// Task 5 wires this wrapper into `convert.rs` so multicol containers carry
/// their parsed rule spec + per-`ColumnGroup` geometry into the draw pass.
#[derive(Clone)]
pub struct MulticolRulePageable {
    pub child: Box<dyn Pageable>,
    pub rule: crate::column_css::ColumnRuleSpec,
    pub groups: Vec<crate::multicol_layout::ColumnGroupGeometry>,
}

impl MulticolRulePageable {
    pub fn new(
        child: Box<dyn Pageable>,
        rule: crate::column_css::ColumnRuleSpec,
        groups: Vec<crate::multicol_layout::ColumnGroupGeometry>,
    ) -> Self {
        Self {
            child,
            rule,
            groups,
        }
    }

    /// Build the krilla stroke for the configured rule spec, including the
    /// dash pattern prescribed for `Dashed` / `Dotted`. Returns `None` when
    /// `draw()` must skip rule painting entirely (style `None` or
    /// non-positive width).
    fn build_stroke(&self) -> Option<krilla::paint::Stroke> {
        use crate::column_css::ColumnRuleStyle;
        if self.rule.width <= 0.0 || self.rule.style == ColumnRuleStyle::None {
            return None;
        }
        let opacity = alpha_to_opacity(self.rule.color[3]);
        let base = colored_stroke(&self.rule.color, self.rule.width, opacity);
        let w = self.rule.width;
        let stroke = match self.rule.style {
            ColumnRuleStyle::None => return None,
            ColumnRuleStyle::Solid => base,
            // `[width * 3.0, width * 2.0]` per fulgur-v7a Task 4 spec
            // (intentionally distinct from `border-style: dashed`, which
            // uses `[width*3, width*3]`).
            ColumnRuleStyle::Dashed => krilla::paint::Stroke {
                dash: Some(krilla::paint::StrokeDash {
                    array: vec![w * 3.0, w * 2.0],
                    offset: 0.0,
                }),
                ..base
            },
            // `line_cap: Round` + `[0, w*2]` — zero-length dashes
            // rendered as round caps become actual dots, matching the
            // dotted-border treatment used elsewhere in fulgur. Without
            // the round cap this would paint as square dash segments.
            ColumnRuleStyle::Dotted => krilla::paint::Stroke {
                line_cap: krilla::paint::LineCap::Round,
                dash: Some(krilla::paint::StrokeDash {
                    array: vec![0.0, w * 2.0],
                    offset: 0.0,
                }),
                ..base
            },
        };
        Some(stroke)
    }
}

impl Pageable for MulticolRulePageable {
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        // Columns paint their own contents first.
        self.child.draw(canvas, x, y, avail_width, avail_height);

        let Some(stroke) = self.build_stroke() else {
            return;
        };

        for group in &self.groups {
            if group.n < 2 || group.col_heights.len() != group.n as usize {
                continue;
            }
            let y_top = y + group.y_offset;
            for i in 0..(group.n as usize - 1) {
                let h_left = group.col_heights[i];
                let h_right = group.col_heights[i + 1];
                if h_left <= 0.0 || h_right <= 0.0 {
                    continue;
                }
                // Gap centre between column i and column i+1. `x_offset`
                // shifts from the container's border-box left into its
                // content-box left (padding-left + border-left), matching
                // the frame `y_top` already works in.
                //   rule_x = x + x_offset + (i+1) * col_w + i * gap + gap/2
                let rule_x = x
                    + group.x_offset
                    + (i as f32 + 1.0) * group.col_w
                    + i as f32 * group.gap
                    + group.gap / 2.0;
                let y_bot = y_top + h_left.min(h_right);
                stroke_line(canvas, rule_x, y_top, rule_x, y_bot, stroke.clone());
            }
        }
        canvas.surface.set_stroke(None);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.child.node_id()
    }
}

// ─── ListItemPageable ───────────────────────────────────

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
    /// fulgur-3vwx (Phase 3.2.b): DOM NodeId for `slice_for_page`
    /// geometry lookup. See `BlockPageable::node_id`. The `<li>`
    /// element's NodeId, plumbed through `convert::list_item`.
    pub node_id: Option<usize>,
}

impl Pageable for ListItemPageable {
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
            if self.visible {
                match &self.marker {
                    ListItemMarker::Text { lines, width } if !lines.is_empty() => {
                        let marker_x = x - width;
                        crate::paragraph::draw_shaped_lines(canvas, lines, marker_x, y, None);
                    }
                    ListItemMarker::Image { .. } => {
                        // v1 dead-code path: image markers carry `ImageEntry` /
                        // `SvgEntry` (drawables components, no draw method by
                        // design — see ECS separation in render.rs systems).
                        // The v2 path draws them via `render::draw_list_item_marker`
                        // → `draw_image_v2` / `draw_svg_v2`. v1 entry points were
                        // removed in PR 8a; this arm is kept only for compilation
                        // and will be deleted with pageable.rs in PR 8j.
                    }
                    _ => {}
                }
            }
            self.body.draw(canvas, x, y, avail_width, avail_height);
        });
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.node_id
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
    /// fulgur-3vwx (Phase 3.2.b): DOM NodeId for `slice_for_page`
    /// geometry lookup. See `BlockPageable::node_id`.
    pub node_id: Option<usize>,
}

impl Pageable for TablePageable {
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
                    .draw(canvas, x + pc.x, y + pc.y, total_width, pc.height);
            }

            if clip_pushed {
                canvas.surface.pop();
            }
        });
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn node_id(&self) -> Option<usize> {
        self.node_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_string_set_pageable_fields() {
        let p = StringSetPageable::new("title".to_string(), "Chapter 1".to_string());
        assert_eq!(p.name, "title");
        assert_eq!(p.value, "Chapter 1");
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
    use crate::image::ImageFormat;

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

#[cfg(test)]
mod affine_tests {
    use crate::draw_primitives::matrix_test_util::{approx, matrix_approx};
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

    #[test]
    fn skew_x_shears_point_along_x_axis() {
        use std::f32::consts::FRAC_PI_4;
        // skew(ax=π/4, ay=0): x' = x + tan(π/4)·y = x + y, y' = y
        let m = Affine2D::skew(FRAC_PI_4, 0.0);
        let (x, y) = m.transform_point(1.0, 1.0);
        assert!((x - 2.0).abs() < 1e-5, "x should equal x+y=2, got {x}");
        assert!((y - 1.0).abs() < 1e-5, "y should be unchanged=1, got {y}");
    }

    #[test]
    fn skew_y_shears_point_along_y_axis() {
        use std::f32::consts::FRAC_PI_4;
        // skew(ax=0, ay=π/4): x' = x, y' = tan(π/4)·x + y = x + y
        let m = Affine2D::skew(0.0, FRAC_PI_4);
        let (x, y) = m.transform_point(1.0, 1.0);
        assert!((x - 1.0).abs() < 1e-5, "x should be unchanged=1, got {x}");
        assert!((y - 2.0).abs() < 1e-5, "y should equal x+y=2, got {y}");
    }

    #[test]
    fn skew_zero_angles_equals_identity() {
        let m = Affine2D::skew(0.0, 0.0);
        assert!(m.is_identity(), "skew(0,0) must equal identity");
    }
}

#[cfg(test)]
mod transform_wrapper_tests {
    use crate::draw_primitives::matrix_test_util::approx;
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    #[derive(Clone)]
    struct StubPageable;

    impl Pageable for StubPageable {
        fn draw(&self, _: &mut Canvas<'_, '_>, _: Pt, _: Pt, _: Pt, _: Pt) {}
        fn clone_box(&self) -> Box<dyn Pageable> {
            Box::new(self.clone())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn make_wrapper(matrix: Affine2D, origin: Point2) -> TransformWrapperPageable {
        TransformWrapperPageable::new(Box::new(StubPageable), matrix, origin)
    }

    #[test]
    fn translate_only_matrix() {
        let w = make_wrapper(Affine2D::translation(10.0, 20.0), Point2::new(0.0, 0.0));
        let m = w.effective_matrix(0.0, 0.0);
        assert!(approx(m.e, 10.0));
        assert!(approx(m.f, 20.0));
        assert!(approx(m.a, 1.0));
        assert!(approx(m.d, 1.0));
    }

    #[test]
    fn rotate_90_maps_unit_vector_at_origin_zero() {
        let w = make_wrapper(Affine2D::rotation(FRAC_PI_2), Point2::new(0.0, 0.0));
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
        let w = make_wrapper(Affine2D::rotation(FRAC_PI_2), Point2::new(50.0, 50.0));
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
        let w = make_wrapper(Affine2D::rotation(FRAC_PI_2), Point2::new(50.0, 50.0));
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
    fn bookmark_marker_has_no_geometry_payload() {
        let m = BookmarkMarkerPageable::new(1, "Chapter 1".to_string());
        assert_eq!(m.level, 1);
        assert_eq!(m.label, "Chapter 1");
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

#[cfg(test)]
mod multicol_rule_tests {
    use super::*;
    use crate::column_css::{ColumnRuleSpec, ColumnRuleStyle};
    use crate::multicol_layout::ColumnGroupGeometry;

    fn make_spacer(h: Pt) -> Box<dyn Pageable> {
        let s = SpacerPageable::new(h);
        Box::new(s)
    }

    fn make_rule_spec(style: ColumnRuleStyle) -> ColumnRuleSpec {
        ColumnRuleSpec {
            width: 1.0,
            style,
            color: [0, 0, 0, 255],
        }
    }

    fn make_group(y_offset: f32, col_heights: Vec<f32>) -> ColumnGroupGeometry {
        let n = col_heights.len() as u32;
        ColumnGroupGeometry {
            x_offset: 0.0,
            y_offset,
            col_w: 80.0,
            gap: 20.0,
            n,
            col_heights,
        }
    }

    #[test]
    fn multicol_rule_wrapper_skips_rule_when_style_none() {
        // With style=None, build_stroke() must return None so draw() is a
        // pass-through. We can't easily count stroke calls without a mock
        // Canvas; the contract is exercised indirectly via build_stroke.
        let block = BlockPageable::new(vec![make_spacer(100.0)]);
        let wrapped = MulticolRulePageable::new(
            Box::new(block),
            make_rule_spec(ColumnRuleStyle::None),
            vec![make_group(0.0, vec![50.0, 50.0])],
        );
        assert!(wrapped.build_stroke().is_none());
    }

    #[test]
    fn multicol_rule_wrapper_skips_rule_when_width_zero() {
        // `width <= 0.0` is the other early-exit branch.
        let block = BlockPageable::new(vec![make_spacer(100.0)]);
        let mut spec = make_rule_spec(ColumnRuleStyle::Solid);
        spec.width = 0.0;
        let wrapped = MulticolRulePageable::new(
            Box::new(block),
            spec,
            vec![make_group(0.0, vec![50.0, 50.0])],
        );
        assert!(wrapped.build_stroke().is_none());
    }
}

#[cfg(test)]
mod link_collector_tests {
    use super::*;
    use crate::paragraph::{LinkSpan, LinkTarget};
    use std::sync::Arc;

    fn make_link(url: &str) -> Arc<LinkSpan> {
        Arc::new(LinkSpan {
            target: LinkTarget::External(Arc::new(url.to_string())),
            alt_text: None,
        })
    }

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn push_rect_zero_width_is_discarded() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        let link = make_link("https://example.com");
        lc.push_rect(&link, r(0.0, 0.0, 0.0, 10.0));
        assert!(
            lc.into_occurrences().is_empty(),
            "zero-width rect must be ignored"
        );
    }

    #[test]
    fn push_rect_zero_height_is_discarded() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        let link = make_link("https://example.com");
        lc.push_rect(&link, r(0.0, 0.0, 10.0, 0.0));
        assert!(
            lc.into_occurrences().is_empty(),
            "zero-height rect must be ignored"
        );
    }

    #[test]
    fn push_rect_negative_dimension_is_discarded() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        let link = make_link("https://example.com");
        lc.push_rect(&link, r(0.0, 0.0, -5.0, 10.0));
        assert!(
            lc.into_occurrences().is_empty(),
            "negative-width rect must be ignored"
        );
    }

    #[test]
    fn same_link_same_page_merges_into_one_occurrence() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        let link = make_link("https://example.com");
        // Simulates a multi-line anchor: two rects for the same Arc<LinkSpan>
        lc.push_rect(&link, r(0.0, 0.0, 50.0, 12.0));
        lc.push_rect(&link, r(0.0, 14.0, 30.0, 12.0));
        let occs = lc.into_occurrences();
        assert_eq!(
            occs.len(),
            1,
            "same link same page must produce one occurrence"
        );
        assert_eq!(
            occs[0].quads.len(),
            2,
            "both rects must be retained as separate quads"
        );
    }

    #[test]
    fn same_link_different_pages_produce_separate_occurrences() {
        let mut lc = LinkCollector::new();
        let link = make_link("https://example.com");
        lc.set_current_page(0);
        lc.push_rect(&link, r(0.0, 0.0, 10.0, 10.0));
        lc.set_current_page(1);
        lc.push_rect(&link, r(0.0, 0.0, 10.0, 10.0));
        let occs = lc.into_occurrences();
        assert_eq!(
            occs.len(),
            2,
            "same link on different pages must produce two occurrences"
        );
        assert_eq!(occs[0].page_idx, 0);
        assert_eq!(occs[1].page_idx, 1);
    }

    #[test]
    fn take_page_returns_and_removes_its_page() {
        let mut lc = LinkCollector::new();
        let link_a = make_link("https://a.com");
        let link_b = make_link("https://b.com");
        lc.set_current_page(0);
        lc.push_rect(&link_a, r(0.0, 0.0, 10.0, 10.0));
        lc.set_current_page(1);
        lc.push_rect(&link_b, r(0.0, 0.0, 10.0, 10.0));

        let page0 = lc.take_page(0);
        assert_eq!(page0.len(), 1);
        assert_eq!(page0[0].page_idx, 0);

        // Page 0 gone; only page 1 remains.
        let remaining = lc.into_occurrences();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].page_idx, 1);
    }

    #[test]
    fn take_page_on_missing_page_returns_empty() {
        let mut lc = LinkCollector::new();
        let result = lc.take_page(99);
        assert!(
            result.is_empty(),
            "take_page on absent page must return empty vec"
        );
    }

    #[test]
    fn occurrences_is_non_consuming_snapshot() {
        let mut lc = LinkCollector::new();
        lc.set_current_page(0);
        let link = make_link("https://example.com");
        lc.push_rect(&link, r(0.0, 0.0, 10.0, 10.0));

        let snap1 = lc.occurrences();
        let snap2 = lc.occurrences();
        assert_eq!(snap1.len(), 1);
        assert_eq!(snap2.len(), 1, "second call must see the same data");

        // Collector must still be usable after snapshots.
        lc.set_current_page(1);
        lc.push_rect(&link, r(0.0, 0.0, 5.0, 5.0));
        assert_eq!(
            lc.occurrences().len(),
            2,
            "new page occurrence visible after snapshots"
        );
    }
}
