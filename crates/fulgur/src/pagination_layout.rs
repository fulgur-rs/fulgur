//! Taffy-hooked block-level paginator (fulgur-4cbc).
//!
//! Sibling of [`crate::multicol_layout`]. The multicol module proves the
//! `LayoutPartialTree` wrapper pattern works for routing one CSS feature
//! through fulgur-owned layout while leaving the rest to `BaseDocument`;
//! this module applies the same idiom to page fragmentation.
//!
//! # Status: production-wired, observational consumer
//!
//! [`run_pass_with_break_styles`] is invoked once per render from
//! `engine.rs` after `multicol_layout::run_pass`. The production path
//! skips `taffy::compute_root_layout` and calls
//! [`fragment_pagination_root`] directly: it walks the body's direct
//! block children's existing `final_layout` — descending into Parley
//! line metrics for inline roots — and records the would-be page
//! geometry in a `PaginationGeometryTable`. Re-driving Taffy on body
//! re-stores every descendant's layout fields and introduces sub-pixel
//! floating-point drift that breaks `examples_determinism`'s byte-wise
//! PDF comparison; see [`PaginationLayoutTree::drive_taffy_root_layout`]
//! for the full root cause.
//!
//! The wrapper's `LayoutPartialTree` / `RoundTree` / `CacheTree` /
//! `TraversePartialTree` impls (which dispatch body's layout into
//! [`compute_pagination_layout`] via `taffy::compute_root_layout`) are
//! kept compile-time live as scaffolding for a future per-strip
//! constrained variant; the `taffy_driven_dispatch_matches_direct_walk`
//! test exercises them at runtime and asserts geometry parity with the
//! production direct walk.
//!
//! Today the engine drops the returned table (`let _pagination_geometry
//! = …`). Follow-up work will capture the table on `ConvertContext`
//! and wire downstream consumers (counter / string-set replacement,
//! per-page repetition redesign, …).
//!
//! # Coverage
//!
//! The wrapper is currently exercised against the body subtree only.
//! Anything nested inside body's direct children continues to use
//! Blitz's normal layout dispatch, and the fragmenter post-walks
//! `final_layout` rather than re-issuing per-strip
//! `compute_child_layout` calls. The fulgur-ik6o probe established
//! that constraining `available_space.height` does not change Taffy's
//! block-layout output — see
//! `docs/plans/2026-04-28-pagination-layout-spike.md`.
//!
//! # Features wired today
//!
//! - Block-level fragmentation against `page_height_px`
//!   ([`fragment_pagination_root`]).
//! - Inline-aware split at Parley line boundaries
//!   ([`fragment_inline_root`], reads `inline_layout_data` populated by
//!   `resolve()`).
//! - `break-before` / `break-after` / `break-inside: avoid` from the
//!   shared [`crate::column_css::ColumnStyleTable`] side-table.
//!
//! # Production extension points
//!
//! [`collect_string_set_states`] and [`implied_page_count`] are `pub`
//! for use by `render_v2` and friends. [`append_position_fixed_fragments`]
//! is wired into `engine.rs` so v2's geometry-driven dispatch can
//! repeat `position: fixed` elements on every page (`is_repeat=true`
//! on the resulting `PaginationGeometry`).

use blitz_dom::BaseDocument;
use std::collections::BTreeMap;
use taffy::{
    AvailableSpace, CacheTree, LayoutPartialTree, NodeId, RoundTree, Size, TraversePartialTree,
    TraverseTree,
};

/// One placement slot recorded per (source node × page).
///
/// `x`, `y`, `width`, `height` are in CSS pixels — Taffy's native unit —
/// and `y` is measured from the page's content-box top. The convert /
/// draw layer is responsible for `px_to_pt` conversion before reaching
/// Krilla.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub page_index: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Per-source-node geometry: every page on which the node has a placement.
///
/// For the block-only fragmenter the vector is normally length 1 (the node
/// fits on one page). A node taller than the page produces multiple
/// fragments — but in the current measurement-only implementation we
/// emit it as a single oversized fragment on the page where its top
/// edge lands, because we have no inline / break point information yet.
///
/// # Repeat vs. split semantics
///
/// `is_repeat = false` (default): the vector represents a *split* —
/// each fragment is one slice of the same content, so consumers
/// accumulate `frag.height` to recover where to slice paragraph lines
/// or block content.
///
/// `is_repeat = true`: the vector represents *per-page repetition* —
/// every fragment carries the full content (`width` / `height` ==
/// the full element size). Consumers must NOT slice; each fragment
/// is a complete redraw at the same coordinates. Used by
/// [`append_position_fixed_fragments`] for `position: fixed` elements
/// that repeat on every page.
#[derive(Clone, Debug, Default)]
pub struct PaginationGeometry {
    pub fragments: Vec<Fragment>,
    pub is_repeat: bool,
}

impl PaginationGeometry {
    /// Whether this node's content was *split* across multiple pages —
    /// i.e. each fragment is a slice of the same content. Returns
    /// `false` when the geometry represents per-page repetition
    /// (`is_repeat == true`), because in that case every fragment
    /// carries the full content and slicers must NOT subdivide it.
    pub fn is_split(&self) -> bool {
        !self.is_repeat && self.fragments.len() > 1
    }
}

/// Side-table mapping DOM `usize` NodeIds to their pagination geometry.
///
/// `BTreeMap` for the same determinism reason as
/// [`crate::multicol_layout::MulticolGeometryTable`]: PDF byte order
/// downstream depends on iteration order.
pub type PaginationGeometryTable = BTreeMap<usize, PaginationGeometry>;

/// Taffy tree wrapper that intercepts the pagination root through
/// `compute_child_layout` and routes it through fulgur's own
/// page-stripping logic.
///
/// `page_height_px` is the height of the page content area (after the
/// engine has subtracted page-margin / `@page` margins). The wrapper
/// borrows the `BaseDocument` for one pass and is discarded; the
/// `geometry` it accumulates is drained via [`Self::take_geometry`] so
/// callers can either thread it into `ConvertContext` or drop it for
/// observational use.
pub struct PaginationLayoutTree<'a> {
    pub(crate) doc: &'a mut BaseDocument,
    pub(crate) page_height_px: f32,
    pub(crate) geometry: PaginationGeometryTable,
    /// Cached id of the `<body>` element, if any. Used as the
    /// fragmentation root for the block-only fragmenter. `None` means the
    /// document had no body and the pass becomes a no-op.
    pub(crate) body_id: Option<usize>,
    /// fulgur-k0g0: `break-before` / `break-after` / `break-inside`
    /// per node, harvested by
    /// [`crate::blitz_adapter::extract_column_style_table`]. The table
    /// is shared with `multicol_layout` (Pageable's
    /// `extract_pagination_from_column_css` reads the same fields), so
    /// the pagination fragmenter does not maintain its own break-style
    /// extraction. `None` means "no break properties set anywhere",
    /// which the fragmenter treats as all-`Auto`.
    pub(crate) column_styles: Option<&'a crate::column_css::ColumnStyleTable>,
    /// fulgur-s67g Phase 2.2: `position: running()` element instances
    /// registered by [`crate::blitz_adapter::RunningElementPass`].
    /// `fragment_pagination_root` consults this store to skip running-
    /// named children — they are placed into `@page` margin boxes per
    /// page, not into the body's flow, so they must not contribute to
    /// the body cursor or page-fragment geometry. `None` (the default
    /// for unit-test entry points) means "no running mappings"; the
    /// fragmenter treats every body child as in-flow.
    pub(crate) running_store: Option<&'a crate::gcpm::running::RunningElementStore>,
    /// fulgur-uebl: per-element used page-name (CSS Page 3 §5.3),
    /// resolved from the same author-facing `page` declarations the
    /// `column_styles` table carries. The fragmenter consults this when
    /// a child is iterated: if its used page-name differs from the
    /// previously-placed sibling's, an implicit forced page break is
    /// induced before the child. `None` means the document has no
    /// `page` declarations and the fragmenter skips the comparison
    /// entirely.
    pub(crate) used_page_names: Option<crate::blitz_adapter::UsedPageNameTable>,
}

/// One-shot entry: run the block-level fragmenter for `doc` against a
/// `page_height_px` page strip and return the resulting geometry table.
///
/// Intended to be called **after** `blitz_adapter::resolve()` (and after
/// `multicol_layout::run_pass` when multicol is in play) so that
/// `final_layout` reflects the post-layout positions the fragmenter
/// walks.
///
/// Calls [`fragment_pagination_root`] directly to walk body's
/// children's existing `final_layout` (populated by
/// `blitz_adapter::resolve` and `multicol_layout::run_pass`) and
/// record per-node fragments. Same direct-walk model as the production
/// entry point — see the module docs for why we skip
/// `taffy::compute_root_layout` here. The Taffy-dispatch path is
/// preserved as test-only via
/// [`PaginationLayoutTree::drive_taffy_root_layout`].
///
/// Test-only convenience for fixtures that don't need break-style
/// awareness. Production callers use [`run_pass_with_break_styles`]
/// so `break-before` / `break-after` / `break-inside` from the shared
/// `ColumnStyleTable` are honoured.
#[cfg(test)]
pub fn run_pass(doc: &mut BaseDocument, page_height_px: f32) -> PaginationGeometryTable {
    run_pass_inner(doc, page_height_px, None, None)
}

/// fulgur-k0g0 variant: thread the document's `break-before` /
/// `break-after` / `break-inside` side-table (harvested by
/// [`crate::blitz_adapter::extract_column_style_table`]) into the
/// fragmenter. `break-before: page` and `break-after: page` force
/// page boundaries; `break-inside: avoid` defers a child that does not
/// fit the remaining strip rather than splitting it.
pub fn run_pass_with_break_styles<'a>(
    doc: &'a mut BaseDocument,
    page_height_px: f32,
    column_styles: &'a crate::column_css::ColumnStyleTable,
) -> PaginationGeometryTable {
    run_pass_inner(doc, page_height_px, Some(column_styles), None)
}

/// fulgur-s67g Phase 2.2 variant: extends
/// [`run_pass_with_break_styles`] with awareness of `position:
/// running()` element instances. Running children are skipped during
/// the body walk so they do not contribute to body cursor or page
/// fragments — per-page placement is handled by
/// [`collect_running_element_states`].
pub fn run_pass_with_break_and_running<'a>(
    doc: &'a mut BaseDocument,
    page_height_px: f32,
    column_styles: &'a crate::column_css::ColumnStyleTable,
    running_store: &'a crate::gcpm::running::RunningElementStore,
) -> PaginationGeometryTable {
    run_pass_inner(
        doc,
        page_height_px,
        Some(column_styles),
        Some(running_store),
    )
}

fn run_pass_inner<'a>(
    doc: &'a mut BaseDocument,
    page_height_px: f32,
    column_styles: Option<&'a crate::column_css::ColumnStyleTable>,
    running_store: Option<&'a crate::gcpm::running::RunningElementStore>,
) -> PaginationGeometryTable {
    // fulgur-uebl: pre-compute the used page-name table when column
    // styles are available. The walk takes one DOM pass and produces a
    // `BTreeMap` keyed by node id, matching the determinism convention
    // used by the rest of the side-tables.
    let used_page_names =
        column_styles.map(|cs| crate::blitz_adapter::compute_used_page_names(doc, cs));
    let mut tree = PaginationLayoutTree::new(doc, page_height_px);
    tree.column_styles = column_styles;
    tree.running_store = running_store;
    tree.used_page_names = used_page_names;
    if tree.body_id.is_some() && page_height_px > 0.0 {
        // Read body's children's existing `final_layout` (populated by
        // Blitz's `resolve()` and `multicol_layout::run_pass`) and
        // produce the page-fragment geometry without re-driving Taffy.
        //
        // We deliberately *skip* `drive_taffy_root_layout` (which runs
        // `taffy::compute_root_layout` through the wrapper) on the
        // production path: re-issuing layout for body forces every
        // descendant's `compute_child_layout` to re-execute, and even
        // with cache hits the round-trip introduces sub-pixel
        // floating-point drift that breaks
        // `examples_determinism`'s byte-wise comparison against
        // committed PDFs. The wrapper's `LayoutPartialTree` /
        // `RoundTree` / `CacheTree` / `TraversePartialTree` impls
        // remain in place for tests that *do* exercise the full Taffy
        // dispatch (`drive_taffy_root_layout`) and as scaffolding for
        // a future per-strip-constrained variant where re-driving
        // layout is what actually does the pagination work.
        tree.fragment_pagination_root();
    }
    tree.take_geometry()
}

impl<'a> PaginationLayoutTree<'a> {
    pub fn new(doc: &'a mut BaseDocument, page_height_px: f32) -> Self {
        let body_id = find_body_id(doc);
        Self {
            doc,
            page_height_px,
            geometry: BTreeMap::new(),
            body_id,
            column_styles: None,
            running_store: None,
            used_page_names: None,
        }
    }

    /// fulgur-uebl: lookup helper for the per-element start / end used
    /// page-names (CSS Page 3 §5.3). Returns `(start, end)` where each
    /// is `None` for the unnamed/auto page or `Some(name)` for a named
    /// page. When the document has no `page` declarations at all the
    /// table is absent; we return `(None, None)` so the comparison `==`
    /// always succeeds and no implicit breaks fire.
    fn used_page_endpoints_of(&self, node_id: usize) -> (Option<String>, Option<String>) {
        self.used_page_names
            .as_ref()
            .and_then(|t| t.get(&node_id).cloned())
            .unwrap_or((None, None))
    }

    /// Drain the accumulated per-node geometry table.
    ///
    /// Mirrors [`crate::multicol_layout::FulgurLayoutTree::take_geometry`]:
    /// uses `mem::take` so a second call returns an empty table rather than
    /// double-counting.
    pub fn take_geometry(&mut self) -> PaginationGeometryTable {
        std::mem::take(&mut self.geometry)
    }

    /// Drive `taffy::compute_root_layout(&mut self, body_id, ...)` so the
    /// wrapper's `compute_child_layout` fires on body and dispatches into
    /// [`compute_pagination_layout`].
    ///
    /// **Test-only.** Production callers (`run_pass_with_break_styles`)
    /// reach geometry via `fragment_pagination_root` directly because
    /// re-driving Taffy on body re-stores every descendant's layout
    /// fields (even on cache hits) and introduces sub-pixel
    /// floating-point drift that breaks `examples_determinism`'s
    /// byte-wise PDF comparison against committed goldens. This entry
    /// is preserved so the wrapper's `LayoutPartialTree` / `RoundTree`
    /// / `CacheTree` / `TraversePartialTree` impls keep one runtime
    /// exerciser and a future per-strip-constrained variant has a
    /// drop-in seam.
    #[cfg(test)]
    ///
    /// The available space we hand Taffy is the body's *existing* layout
    /// width and an unbounded height (`AvailableSpace::MaxContent`). We
    /// pass MaxContent rather than `page_height_px` because the
    /// fragmenter relies on the children's natural `final_layout`
    /// heights — restricting `available_space.height` here would let
    /// Taffy clip or shrink children, breaking the measurement walk.
    /// (The fulgur-ik6o spike experimented with `Definite` and
    /// established that Taffy's block layout does not consult
    /// `available_space.height` for mid-element splitting; see
    /// `docs/plans/2026-04-28-pagination-layout-spike.md`.)
    ///
    /// `compute_root_layout` resets the layout's `location` to `(0, 0)`
    /// because it treats the node as a Taffy root. Body is *not* a real
    /// root in the document tree (html is its parent), so we save and
    /// restore body's location across the call — same approach as
    /// [`crate::multicol_layout::FulgurLayoutTree::layout_multicol_subtrees`].
    fn drive_taffy_root_layout(&mut self) {
        let Some(body_id) = self.body_id else {
            return;
        };
        let nid = NodeId::from(body_id);
        let prior_unrounded = self.doc.get_unrounded_layout(nid);
        let prior_final = self
            .doc
            .get_node(body_id)
            .map(|n| n.final_layout)
            .unwrap_or_default();

        let avail = Size {
            width: AvailableSpace::Definite(prior_unrounded.size.width.max(1.0)),
            height: AvailableSpace::MaxContent,
        };
        taffy::compute_root_layout(self, nid, avail);

        // Restore body's full layout so downstream readers (convert,
        // paginate) see byte-identical state to Blitz's first pass —
        // examples_determinism would otherwise pick up sub-pixel
        // float-rep differences when `compute_root_layout` re-stores
        // the same logical values via `set_unrounded_layout` /
        // `set_final_layout`.
        if let Some(node) = self.doc.get_node_mut(body_id) {
            node.unrounded_layout = prior_unrounded;
            node.final_layout = prior_final;
        }
    }

    /// Walk the body's direct block children and record fragments.
    ///
    /// Called from `compute_pagination_layout` after Taffy dispatches
    /// body's layout through the wrapper. Returns the number of
    /// fragments emitted. `0` means either the document has no body or
    /// the body has no children — both are expected for empty documents
    /// and the convert-side comparison should treat them as equivalent
    /// to `Pageable` producing a single empty page.
    ///
    /// Algorithm (block-only, measurement-only):
    ///
    /// 1. Look up body's `final_layout` to fix the available width and
    ///    the body-relative y origin.
    /// 2. For each direct child whose `final_layout` is non-zero:
    ///    a. Compute the child's bottom edge relative to body content.
    ///    b. If `cursor_y + child_h <= page_height_px` the child fits on
    ///    the current page; emit one fragment with `page_index` set.
    ///    c. Otherwise advance `page_index`, reset `cursor_y` to 0, then
    ///    place the child on the new page. A child taller than the
    ///    page is emitted whole (oversized fragment) — true split
    ///    requires inline / break point support that is out of scope.
    /// 3. Record `Vec<Fragment>` per source node id.
    pub fn fragment_pagination_root(&mut self) -> usize {
        let Some(body_id) = self.body_id else {
            return 0;
        };
        if self.page_height_px <= 0.0 {
            return 0;
        }

        let body_layout = self
            .doc
            .get_node(body_id)
            .map(|n| n.final_layout)
            .unwrap_or_default();
        let body_w = body_layout.size.width;
        let body_x = body_layout.location.x;

        // fulgur-s67g Phase 2.3 (counter parity follow-up): record
        // body itself as a fragment on page 0. Pageable wraps `body`
        // with `CounterOpWrapperPageable` /
        // `StringSetWrapperPageable` /
        // `BookmarkMarkerWrapperPageable` whenever those features are
        // declared on `body`, and the wrapper's `split` keeps the
        // marker on the **first** half — so body's own counter-reset
        // / string-set / bookmark ops fire only on page 0
        // (`pageable.rs:2829-2840` etc.).
        //
        // Without this entry the fragmenter's geometry table excludes body
        // entirely; `collect_counter_states` /
        // `collect_string_set_states` / `collect_bookmark_entries`
        // miss body's ops and the parity gates fire on documents like
        // `tests/gcpm_integration::test_counter_set` (body has
        // `counter-reset: chapter` declared on it). The body fragment
        // sits ahead of every body-direct-child entry in NodeId order
        // (Blitz allocates ids depth-first during parse, so `body` is
        // smaller than its descendants), so per-page walks pick up
        // body's ops first, matching Pageable's tree-walk order.
        self.geometry
            .entry(body_id)
            .or_default()
            .fragments
            .push(Fragment {
                page_index: 0,
                x: body_x,
                y: 0.0,
                width: body_w,
                height: body_layout.size.height,
            });

        // Prefer body's `layout_children` — same rationale as
        // `record_subtree_descendants`. When a block container has
        // mixed block-level and inline-level children, Stylo
        // synthesizes anonymous block wrappers around the inline-
        // level siblings (CSS 2.1 §9.2.1.1). Those wrappers carry
        // their own `node_id` and Taffy layout, but they live ONLY
        // in `layout_children` — `children` still points at the
        // underlying inline elements (e.g. a body containing
        // `<label>` followed by `<fieldset>` followed by
        // `<select><option>...</option></select>` produces an
        // anonymous block wrapping the `<select>` siblings, visible
        // only in `layout_children`).
        //
        // Without this preference v2 silently drops the inline-level
        // group's paint: extract assigns the inner paragraph's
        // `node_id` to the synthesized wrapper, but the body iteration
        // walks raw `children` and never visits the wrapper, so
        // geometry has no fragment for that node_id and
        // `dispatch_fragment` skips the paragraph entirely
        // (fulgur-bq6i: examples/wasm-demo lost label / legend / option
        // text content for this exact reason).
        let children = self
            .doc
            .get_node(body_id)
            .map(|n| {
                let layout_borrow = n.layout_children.borrow();
                if let Some(lc) = layout_borrow.as_deref()
                    && !lc.is_empty()
                {
                    lc.to_vec()
                } else {
                    n.children.clone()
                }
            })
            .unwrap_or_default();

        let mut page_index: u32 = 0;
        let mut cursor_y: f32 = 0.0;
        let mut emitted = 0usize;
        // Tracks the bottom edge of the previously emitted in-flow child
        // in body-content-box coordinates. Used to pick up inter-child
        // gaps (collapsed margins, padding) that Blitz baked into each
        // child's `final_layout.location.y` but the cursor-only walk
        // would otherwise miss. Pageable accumulates `pc.y + child_h`
        // from `final_layout.location.y` during convert, so margin gaps
        // are present in the Pageable side; the fragmenter must match.
        let mut prev_bottom_y_in_body: f32 = 0.0;
        // fulgur-uebl: tracks the used page-name of the previously
        // placed in-flow sibling (`Some(Some(name))` named, `Some(None)`
        // auto/unnamed, `None` no previous). When the next sibling's
        // used page-name differs, we induce a forced break before it
        // (CSS Page 3 §5.3, "Using Named Pages").
        let mut prev_used_page: Option<Option<String>> = None;

        for child_id in children {
            let Some(child) = self.doc.get_node(child_id) else {
                continue;
            };
            // Skip pure-whitespace text nodes — same convention as
            // multicol_layout's `partition_children_into_segments`.
            if let Some(text) = child.text_data()
                && text.content.chars().all(char::is_whitespace)
            {
                continue;
            }
            // CSS 2.1 §10.6.4: out-of-flow elements (`position: absolute`
            // / `position: fixed`) do not contribute to their containing
            // block's normal-flow height. Pageable routes them through
            // `PositionedChild { out_of_flow: true }` and they never
            // advance pagination cursors; the fragmenter must match or the
            // fulgur-cj6u Phase 1.2 parity assertion fires on documents
            // with abs/fixed body-direct children.
            {
                use ::style::properties::longhands::position::computed_value::T as Pos;
                let is_out_of_flow = child.primary_styles().is_some_and(|s| {
                    matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed)
                });
                if is_out_of_flow {
                    continue;
                }
            }
            // fulgur-s67g Phase 2.2: skip `position: running()` named
            // children from the body cursor. They are removed from
            // body flow and placed into `@page` margin boxes per page;
            // including them in the cursor would over-count height.
            //
            // Phase 3.4 follow-up (PR #296 Devin): record a zero-height
            // fragment at the cursor position before skipping so the
            // running node enters `geometry` keyed by its NodeId. The
            // fragment carries `height = 0` (cursor does not advance)
            // but `page_index` is the page on which the running
            // element's source position lands — exactly what
            // `collect_running_element_states` needs to map running
            // instances to their per-page state.
            if self
                .running_store
                .is_some_and(|s| s.instance_for_node(child_id).is_some())
            {
                if child.element_data().is_some() {
                    let layout = child.final_layout;
                    self.geometry
                        .entry(child_id)
                        .or_default()
                        .fragments
                        .push(Fragment {
                            page_index,
                            x: body_x + layout.location.x,
                            y: cursor_y,
                            width: 0.0,
                            height: 0.0,
                        });
                    emitted += 1;
                }
                continue;
            }
            let layout = child.final_layout;
            let child_h = layout.size.height;
            let child_w = if layout.size.width > 0.0 {
                layout.size.width
            } else {
                body_w
            };
            if child_h <= 0.0 {
                // Phase 2.3 fix: zero-height **element** nodes still
                // need to enter geometry so their counter /
                // string-set / bookmark markers participate in the
                // parity walks. Pageable emits these via
                // `convert::collect_positioned_children`'s dedicated
                // `emit_*_markers` helpers when a 0×0 child is
                // encountered (e.g.
                // `<div class="reset" style="..."></div>` carrying a
                // `counter-set` declaration). The fragment carries
                // height 0 so it does not advance the cursor — only
                // the NodeId matters for the per-page metadata walks.
                // Whitespace-only text nodes are already filtered
                // above; running and abs/fixed elements are filtered
                // before this branch.
                //
                // fulgur-p3uf (Phase 3.1.5a): honour `break-before`
                // / `break-after` on zero-height element nodes too.
                // Bare `<img>` with no explicit dimensions and
                // pseudo-only `<div>` (rendering only `::before`
                // content) both arrive here with `child_h == 0` after
                // Blitz's intrinsic-size collapse, and Pageable
                // honours their `break-before: page` directive
                // (`tests/pseudo_only_break_before.rs::bare_img_honours_break_before_page`
                // and `pseudo_only_inline_root_honours_break_before_page`).
                // The pre-3.1.5a fragmenter `continue`'d before
                // reading break properties at all, so the gate
                // `forced_break_skipped` masked the divergence.
                let zero_break_props = self
                    .column_styles
                    .and_then(|t| t.get(&child_id))
                    .cloned()
                    .unwrap_or_default();
                // fulgur-uebl: floats are out of normal flow for the
                // sibling page-name comparison (CSS Page 3 / CSS 2.1
                // §9.5). Skip the comparison and the `prev_used_page`
                // update entirely for floated zero-height children;
                // they do not establish class A break points and should
                // not influence break decisions on adjacent in-flow
                // boxes.
                let zero_is_float = crate::blitz_adapter::node_is_floating(child);
                let (zero_used_start, zero_used_end) = self.used_page_endpoints_of(child_id);
                let zero_page_name_changed = !zero_is_float
                    && prev_used_page
                        .as_ref()
                        .is_some_and(|p| *p != zero_used_start);
                let zero_force_break = matches!(
                    zero_break_props.break_before,
                    Some(crate::draw_primitives::BreakBefore::Page)
                ) || zero_page_name_changed;
                if zero_force_break && cursor_y > 0.0 {
                    page_index += 1;
                    cursor_y = 0.0;
                }
                if child.element_data().is_some() {
                    self.geometry
                        .entry(child_id)
                        .or_default()
                        .fragments
                        .push(Fragment {
                            page_index,
                            x: body_x + layout.location.x,
                            y: cursor_y,
                            width: child_w,
                            height: 0.0,
                        });
                    emitted += 1;
                }
                if !zero_is_float {
                    prev_used_page = Some(zero_used_end);
                }
                if matches!(
                    zero_break_props.break_after,
                    Some(crate::draw_primitives::BreakAfter::Page)
                ) {
                    page_index += 1;
                    cursor_y = 0.0;
                }
                continue;
            }

            // Pick up the inter-child gap in body coordinates (collapsed
            // top/bottom margins, body padding before the first child)
            // before any break / overflow logic so the cursor reflects
            // Blitz's flow positions. `max(0.0)` guards against negative
            // gaps from sibling overlap (rare with default UA styles).
            let this_top_in_body = layout.location.y;
            let gap = (this_top_in_body - prev_bottom_y_in_body).max(0.0);
            cursor_y += gap;

            // fulgur-k0g0: read break-before / break-after / break-inside
            // for this child from the column-style side-table (shared with
            // multicol). Default `Auto` for nodes the table does not cover.
            let break_props = self
                .column_styles
                .and_then(|t| t.get(&child_id))
                .cloned()
                .unwrap_or_default();

            // fulgur-uebl: page-name change between adjacent siblings
            // induces an implicit forced break (CSS Page 3 §5.3).
            // Treated identically to an authored `break-before: page`
            // so the existing leading-break-on-fresh-page collapse
            // applies (CSS 3 Fragmentation §3). Compare the previous
            // sibling's `end` against this child's `start` — that's how
            // a page-name change buried inside a subtree (e.g.
            // `propagated-008`) surfaces to the body-level walk.
            //
            // Floats are out of normal flow (CSS 2.1 §9.5) and do not
            // establish class A break points, so they're skipped from
            // both the comparison and the `prev_used_page` update.
            let is_float = crate::blitz_adapter::node_is_floating(child);
            let (used_start, used_end) = self.used_page_endpoints_of(child_id);
            let page_name_changed =
                !is_float && prev_used_page.as_ref().is_some_and(|p| *p != used_start);

            // `break-before: page` forces a page boundary before the
            // child whenever there is in-flow content already placed on
            // the current page. A leading break-before on a fresh page
            // is a no-op (CSS 3 Fragmentation §3 collapses it).
            if (matches!(
                break_props.break_before,
                Some(crate::draw_primitives::BreakBefore::Page)
            ) || page_name_changed)
                && cursor_y > 0.0
            {
                page_index += 1;
                cursor_y = 0.0;
            }

            let avoid_inside = matches!(
                break_props.break_inside,
                Some(crate::draw_primitives::BreakInside::Avoid)
            );

            // fulgur-p55h: if the child carries a Parley inline layout,
            // probe its line metrics and split at line boundaries —
            // mirrors the v1 paragraph-pageable split path (removed in
            // PR 8j-1; see git history) but inside the Taffy hook rather
            // than post-conversion.
            //
            // fulgur-k0g0: when `break-inside: avoid` is set, fall
            // through to the block path below so the paragraph emits
            // whole instead of splitting between lines.
            let line_metrics = if avoid_inside {
                Vec::new()
            } else {
                collect_inline_line_metrics(child)
            };
            if line_metrics.len() > 1 {
                // fulgur-s67g Phase 2.2: if the paragraph cannot fit
                // the remaining space on the current page strip but
                // would fit (or at least start fresh) on a new page,
                // advance the page boundary before calling
                // `fragment_inline_root`. Mirrors Pageable's
                // `BlockPageable::split` falling back to `AtIndex`
                // (split before the child) when the v1 paragraph-pageable
                // split path (removed in PR 8j-1) could not honour
                // widow / orphan and returned `None`.
                let para_total_h = line_metrics
                    .last()
                    .map(|l| l.1 - line_metrics[0].0)
                    .unwrap_or(child_h);
                if cursor_y > 0.0 && cursor_y + para_total_h > self.page_height_px {
                    page_index += 1;
                    cursor_y = 0.0;
                }
                let para_x = layout.location.x;
                let (new_page_index, new_cursor_y, frag_count) = fragment_inline_root(
                    &mut self.geometry,
                    child_id,
                    body_x + para_x,
                    child_w,
                    cursor_y,
                    page_index,
                    self.page_height_px,
                    &line_metrics,
                );
                page_index = new_page_index;
                cursor_y = new_cursor_y;
                emitted += frag_count;
                prev_bottom_y_in_body = this_top_in_body + child_h;
                if !is_float {
                    prev_used_page = Some(used_end.clone());
                }
                if matches!(
                    break_props.break_after,
                    Some(crate::draw_primitives::BreakAfter::Page)
                ) {
                    page_index += 1;
                    cursor_y = 0.0;
                }
                continue;
            }

            // fulgur-g9e3.1 + fulgur-a36m + fulgur-7hf5: unified
            // recursion gate covering all three break cases —
            //   - truly oversized (`child_h > page_h_px`) caught
            //     here when `would_split_block_subtree` finds the
            //     overflowing grandchild,
            //   - in-place mid-element split (`cursor_y + child_h >
            //     page_h_px` with `child_h <= page_h_px`),
            //   - forced break declared anywhere in the subtree.
            //
            // The recursion enters from the **current** cursor (not
            // a pre-advanced 0), so an in-place split with `cursor_y
            // > 0` produces a `WithinChild`-shaped result on the
            // current page and a tail on the next, matching
            // Pageable's behaviour.
            //
            // `would_split_block_subtree` is a cheap simulator that
            // walks the DOM children once with the same gap / OOF /
            // whitespace skips `fragment_block_subtree` uses — it
            // returns `false` when the children all fit in the
            // available strip, so the in-place case where the
            // parent's CSS height exceeds children's sum (e.g. a
            // 600px div with one 30px h2) falls through to the
            // existing whole-emit path and avoids the children-sum
            // parent-height bug.
            //
            // `break-inside: avoid` + truly-oversized → still falls
            // through to splitting (Pageable's `total_height >
            // page_height` override at `pageable.rs:1165`).
            let child_node = self.doc.get_node(child_id);
            let has_splittable_children = child_node.is_some_and(|n| !n.children.is_empty());
            // fulgur-7hf5: multicol containers (`column-count > 1` /
            // `column-width: <len>`) distribute children across
            // columns; their DOM children's flow does not match the
            // visual flow `would_split_block_subtree` simulates. Skip
            // recursion for them — Pageable's `ColumnGroupPageable`
            // handles their split internally and emits whole when the
            // multicol box itself fits the strip.
            //
            // fulgur-916y: multicol containers with a `column-span:
            // all` direct child get an exception — the span subtree
            // is laid out by Taffy as a full-width block flowing
            // between the column groups, so block-flow recursion via
            // `fragment_block_subtree` can split it across pages
            // when it overflows. Containers without span:all stay
            // atomic.
            let is_multicol = child_node.is_some_and(crate::blitz_adapter::is_multicol_container);
            let multicol_has_span_all = is_multicol
                && child_node.is_some_and(|n| {
                    n.children.iter().any(|&id| {
                        self.doc
                            .get_node(id)
                            .is_some_and(crate::blitz_adapter::has_column_span_all)
                    })
                });
            let available_strip = (self.page_height_px - cursor_y).max(0.0);
            let needs_recursion = has_splittable_children
                && (!is_multicol || multicol_has_span_all)
                && (has_forced_break_below(self.doc, child_id, self.column_styles, 0)
                    || has_page_name_change_below(
                        self.doc,
                        child_id,
                        self.used_page_names.as_ref(),
                        0,
                    )
                    || would_split_block_subtree(
                        self.doc,
                        child_id,
                        available_strip,
                        self.page_height_px,
                        0,
                    ));
            if needs_recursion {
                let child_x_in_body = body_x + layout.location.x;
                let (new_page, new_cursor) = fragment_block_subtree(
                    &mut self.geometry,
                    self.doc,
                    self.column_styles,
                    self.used_page_names.as_ref(),
                    child_id,
                    child_w,
                    child_x_in_body,
                    page_index,
                    cursor_y,
                    self.page_height_px,
                    0,
                );
                page_index = new_page;
                cursor_y = new_cursor;
                emitted += 1;
                prev_bottom_y_in_body = this_top_in_body + child_h;
                if !is_float {
                    prev_used_page = Some(used_end.clone());
                }
                if matches!(
                    break_props.break_after,
                    Some(crate::draw_primitives::BreakAfter::Page)
                ) {
                    page_index += 1;
                    cursor_y = 0.0;
                }
                continue;
            }

            // No recursion needed — apply the existing strip-overflow
            // page advance for non-splittable / fits-fine children.
            // `break-inside: avoid` collapses to this path via
            // `avoid_inside` above (it just suppresses the inline
            // split branch; remaining-strip overflow handling is
            // identical).
            if cursor_y > 0.0 && cursor_y + child_h > self.page_height_px {
                page_index += 1;
                cursor_y = 0.0;
            }

            // Phase 4 PR 5 fix: include `layout.location.x` so the
            // child's left margin / padding offset within body is
            // captured. Pre-Phase-4 the fragmenter only fed
            // `slice_for_page` which doesn't read `frag.x`, so
            // `body_x` alone happened to be enough; the new
            // geometry-driven render path now consults `frag.x` for
            // every Block / Image / Paragraph and reverts to v2
            // drawing at x=0 without this. Matches the descendant
            // fragment shape on the line below.
            let frag = Fragment {
                page_index,
                x: body_x + layout.location.x,
                y: cursor_y,
                width: child_w,
                height: child_h,
            };
            self.geometry
                .entry(child_id)
                .or_default()
                .fragments
                .push(frag);

            // fulgur-s67g Phase 2.5: descend into the child's subtree
            // and record per-node fragments for every visible
            // descendant. The collect_*_states walks expect coverage of
            // nested DOM elements so bookmark / counter / string-set
            // markers attached e.g. to an `h2` inside a wrapper `<div>`
            // appear in geometry too.
            //
            // The descendant fragments live on the same page as
            // their ancestor — exact mid-element split inside a
            // body child is still future work (Phase 3 / Pageable
            // replacement). Y / width / height come from the
            // descendant's `final_layout` and are mainly
            // informational; the parity gates that consume this
            // geometry today read only `page_index`.
            record_subtree_descendants(
                &mut self.geometry,
                self.doc,
                child_id,
                page_index,
                cursor_y,
                body_x + layout.location.x,
                0,
            );

            cursor_y += child_h;
            emitted += 1;
            prev_bottom_y_in_body = this_top_in_body + child_h;
            if !is_float {
                prev_used_page = Some(used_end.clone());
            }

            // `break-after: page` forces a page boundary after the
            // child. A trailing break on the last in-flow child does
            // emit an empty trailing page in CSS, but the fragmenter's
            // observable signal (page_count) treats this as "advance
            // cursor"; the next iteration's emit-or-skip handles
            // whether the page is materialised.
            if matches!(
                break_props.break_after,
                Some(crate::draw_primitives::BreakAfter::Page)
            ) {
                page_index += 1;
                cursor_y = 0.0;
            }
        }

        emitted
    }
}

/// fulgur-s67g Phase 2.5: recursively record fragments for every
/// visible descendant of a body-direct child, attaching them to the
/// same `page_index` as the ancestor.
///
/// `parent_page_y` is the parent's body-relative y position on the
/// current page strip; `parent_x_in_body` is the parent's x position
/// (already pre-resolved against `body_x`). For each descendant, the
/// recorded fragment uses absolute body-relative coordinates
/// computed by adding the descendant's `final_layout.location` to
/// the parent's frame.
///
/// Skips zero-size descendants and bails at
/// [`crate::MAX_DOM_DEPTH`] to keep recursion bounded against
/// adversarial input.
///
/// Mid-element split inside a body child (a deeply nested element
/// crossing the page boundary that the parent itself did not split
/// at) is **not** modelled here — descendants land on the same page
/// as their ancestor. Closing this "block-level only" gap requires the
/// full per-strip layout pass that future fragmenter work will
/// introduce.
fn record_subtree_descendants(
    geometry: &mut PaginationGeometryTable,
    doc: &BaseDocument,
    parent_id: usize,
    page_index: u32,
    parent_page_y: f32,
    parent_x_in_body: f32,
    depth: usize,
) {
    if depth >= crate::MAX_DOM_DEPTH {
        return;
    }
    let Some(parent) = doc.get_node(parent_id) else {
        return;
    };
    // Prefer Blitz's `layout_children` over the raw DOM `children` when
    // it's been computed: when a block container has mixed
    // block-level and inline-level children, Stylo synthesizes
    // anonymous block wrappers around inline-level siblings (CSS 2.1
    // §9.2.1.1). Those wrappers are real `Node` instances with their
    // own `node_id` and Taffy layout, but they live ONLY in
    // `layout_children` — the original `children` list still points
    // at the underlying inline elements (e.g. a `<span
    // display:inline-block>`).
    //
    // Without this preference v2 silently drops the inline-level
    // span: extract assigns the inner paragraph's `node_id` to the
    // anonymous wrapper (because Blitz's `is_inline_root()` flag sits
    // on the wrapper), but the fragmenter — walking `children` —
    // never visits the wrapper, so geometry has no fragment for that
    // node_id and `dispatch_fragment` skips the paragraph entirely.
    // (fulgur-bq6i: review_card_inline_block.html lost its
    // "OK Approved" rounded badge for this exact reason.)
    let layout_children_borrow = parent.layout_children.borrow();
    let walk_children: &[usize] = layout_children_borrow
        .as_deref()
        .filter(|v| !v.is_empty())
        .unwrap_or(&parent.children);
    for &child_id in walk_children {
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
        let layout = child.final_layout;
        let h = layout.size.height;
        let w = layout.size.width;
        // Phase 4 PR 5: zero-size containers (`<tbody>`, `<tr>`,
        // anonymous boxes) carry no paint payload but DO host visible
        // descendants (e.g. table cells) that v2 needs in geometry.
        // Skipping them entirely leaves cells out of `geometry`; the
        // dispatcher then never finds the cell node_ids and v2 emits
        // a blank table. Recurse without recording when h/w are
        // both zero so the descendant cells still register.
        if h <= 0.0 && w <= 0.0 {
            record_subtree_descendants(
                geometry,
                doc,
                child_id,
                page_index,
                parent_page_y + layout.location.y,
                parent_x_in_body + layout.location.x,
                depth + 1,
            );
            continue;
        }
        let child_x = parent_x_in_body + layout.location.x;
        let child_y = parent_page_y + layout.location.y;
        geometry
            .entry(child_id)
            .or_default()
            .fragments
            .push(Fragment {
                page_index,
                x: child_x,
                y: child_y,
                width: w,
                height: h,
            });
        record_subtree_descendants(
            geometry,
            doc,
            child_id,
            page_index,
            child_y,
            child_x,
            depth + 1,
        );
    }
}

/// fulgur-7hf5 (Phase 3.1.5c): pre-flight check for the recursion
/// gate — true if walking `parent_id`'s direct children would cross
/// a page boundary at `available_h`.
///
/// Cheaper-than-real `fragment_block_subtree` simulator: same gap /
/// OOF / whitespace skips, but no fragment emission. Returns `true`
/// on the first overflow detected. Lets the caller decide "should I
/// recurse here?" without paying the cost of recursion when recursion
/// would not actually split — distinguishing "recurse and split"
/// (within-child) from "push child whole / emit whole" (at-index or
/// no split).
///
/// `available_h` is the strip height left below the parent's entry
/// cursor on the current page. `page_h_px` lets the simulator detect
/// grandchildren taller than a full page (which would themselves
/// recurse and therefore split, regardless of `available_h`).
fn would_split_block_subtree(
    doc: &BaseDocument,
    parent_id: usize,
    available_h: f32,
    page_h_px: f32,
    depth: usize,
) -> bool {
    if depth >= crate::MAX_DOM_DEPTH {
        return false;
    }
    let Some(parent) = doc.get_node(parent_id) else {
        return false;
    };
    let mut cursor: f32 = 0.0;
    let mut prev_bottom: f32 = 0.0;
    for &child_id in &parent.children {
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
        if let Some(text) = child.text_data()
            && text.content.chars().all(char::is_whitespace)
        {
            continue;
        }
        {
            use ::style::properties::longhands::position::computed_value::T as Pos;
            let is_oof = child.primary_styles().is_some_and(|s| {
                matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed)
            });
            if is_oof {
                continue;
            }
        }
        let layout = child.final_layout;
        let h = layout.size.height;
        if h <= 0.0 {
            continue;
        }
        let this_top = layout.location.y;
        let gap = (this_top - prev_bottom).max(0.0);
        cursor += gap;
        if cursor + h > available_h {
            return true;
        }
        if h > page_h_px {
            // Grandchild itself oversized → would recurse → would
            // split, regardless of `available_h` budget.
            return true;
        }
        cursor += h;
        prev_bottom = this_top + h;
    }
    false
}

/// fulgur-a36m (Phase 3.1.5b): true if any descendant of `node_id`
/// declares `break-before: page` or `break-after: page` in
/// `column_styles`. Walks the entire DOM subtree, bails at
/// [`crate::MAX_DOM_DEPTH`].
///
/// Mirrors `BlockPageable::has_forced_break_below()` from `pageable.rs`,
/// but works on Blitz nodes via the column-style side-table rather
/// than the converted Pageable tree. Used by `fragment_pagination_root`
/// and `fragment_block_subtree` to decide whether a body-direct (or
/// nested) child needs to be entered for break recursion even when it
/// fits the current page strip whole.
fn has_forced_break_below(
    doc: &BaseDocument,
    node_id: usize,
    column_styles: Option<&crate::column_css::ColumnStyleTable>,
    depth: usize,
) -> bool {
    if depth >= crate::MAX_DOM_DEPTH {
        return false;
    }
    let Some(node) = doc.get_node(node_id) else {
        return false;
    };
    for &child_id in &node.children {
        if let Some(props) = column_styles.and_then(|t| t.get(&child_id))
            && (matches!(
                props.break_before,
                Some(crate::draw_primitives::BreakBefore::Page)
            ) || matches!(
                props.break_after,
                Some(crate::draw_primitives::BreakAfter::Page)
            ))
        {
            return true;
        }
        if has_forced_break_below(doc, child_id, column_styles, depth + 1) {
            return true;
        }
    }
    false
}

/// fulgur-uebl: true if any sibling pair inside `node_id`'s subtree
/// has different used page-names. Used as a recursion gate so that
/// `fragment_block_subtree` is entered for subtrees that fit the page
/// strip but contain implicit page-name forced breaks (CSS Page 3
/// §5.3). Walking the whole subtree is acceptable here — the
/// `column_styles` / `used_page_names` tables are sparse, and the bail
/// at [`crate::MAX_DOM_DEPTH`] matches `has_forced_break_below`.
fn has_page_name_change_below(
    doc: &BaseDocument,
    node_id: usize,
    used_page_names: Option<&crate::blitz_adapter::UsedPageNameTable>,
    depth: usize,
) -> bool {
    if depth >= crate::MAX_DOM_DEPTH {
        return false;
    }
    let Some(table) = used_page_names else {
        return false;
    };
    let Some(node) = doc.get_node(node_id) else {
        return false;
    };
    // Atomic inline containers (`inline-block`, `inline-flex`, etc.)
    // are fully opaque: their internal block flow does not paginate
    // independently from the parent line box. Skip the entire subtree
    // so the recursion gate doesn't fire on them.
    if crate::blitz_adapter::is_atomic_inline_container_node(node) {
        return false;
    }
    // Orthogonal-flow nodes (writing-mode different from their own
    // parent) are also atomic from the outer flow's perspective (CSS
    // Writing Modes 4 §9). Even when called directly with the
    // orthogonal node as the target, treat its subtree as opaque so
    // the recursion gate doesn't trigger a `fragment_block_subtree`
    // entry that would interact with Taffy's orthogonal-flow sizing
    // and produce layout drift not present in the whole-emit baseline.
    if let Some(gp_id) = node.parent
        && let Some(gp) = doc.get_node(gp_id)
        && crate::blitz_adapter::is_orthogonal_to_parent(gp, node)
    {
        return false;
    }
    // Flex / grid containers suppress sibling comparison among their
    // direct items (CSS Page 3 / CSS Fragmentation 3 — flex / grid
    // items are not class A break points). But page-name forced breaks
    // inside an item's own BFC still apply, so we must keep recursing
    // into each item — only the direct-children comparison is gated.
    let suppress_direct_compare = crate::blitz_adapter::is_flex_or_grid_container_node(node);
    let mut prev_used: Option<Option<String>> = None;
    for &child_id in &node.children {
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
        // Skip whitespace-only text and out-of-flow children — same
        // filters as the fragmenter loop, so the predicate matches
        // exactly what `fragment_block_subtree` would compare.
        if let Some(text) = child.text_data()
            && text.content.chars().all(char::is_whitespace)
        {
            continue;
        }
        if child.element_data().is_none() {
            continue;
        }
        // Orthogonal-to-this-node child: fully atomic from this node's
        // perspective (CSS Writing Modes 4 §9). Skip the entire
        // subtree — no comparison, no recursion.
        if crate::blitz_adapter::is_orthogonal_to_parent(node, child) {
            continue;
        }
        {
            use ::style::properties::longhands::position::computed_value::T as Pos;
            let is_out_of_flow = child.primary_styles().is_some_and(|s| {
                matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed)
            });
            if is_out_of_flow {
                continue;
            }
        }
        let (child_start, child_end) = table.get(&child_id).cloned().unwrap_or((None, None));
        if !suppress_direct_compare && prev_used.as_ref().is_some_and(|p| *p != child_start) {
            return true;
        }
        if !suppress_direct_compare {
            prev_used = Some(child_end);
        }
        // Always recurse: even when direct sibling comparison is
        // suppressed (flex / grid container), descendants in their
        // own BFC may still trigger an internal page-name break.
        if has_page_name_change_below(doc, child_id, used_page_names, depth + 1) {
            return true;
        }
    }
    false
}

/// fulgur-g9e3.1: split a block element across pages by walking its DOM
/// children and emitting per-page fragments for both the block itself
/// and its children.
///
/// For each in-flow child, if it does not fit in the remaining strip,
/// advance the page boundary and continue placing on a fresh strip.
/// Children with their own DOM children that are taller than a full page
/// recurse so the split walks all the way down to where overflow actually
/// resolves.
///
/// Per-page parent fragments capture the height consumed by children on
/// each page (`cursor - page_start_y`). The downstream collectors
/// (`collect_string_set_states` / `collect_counter_states` /
/// `collect_bookmark_entries`) consume the per-page snapshots produced
/// here.
///
/// fulgur-a36m (Phase 3.1.5b): also honours `break-before: page` /
/// `break-after: page` on direct children, and recurses into children
/// whose subtrees declare a forced break (`has_forced_break_below`)
/// so deeper nested breaks land on the right page.
///
/// In-place mid-element split (`cursor_y + child_h > page_h` with
/// `child_h <= page_h` and a CSS-set parent height that diverges from
/// children sum) still falls back to the pre-3.1 push-to-next-page
/// behaviour — that's `fulgur-7hf5` (Phase 3.1.5c).
///
/// Skips OOF / whitespace-text children, same convention as
/// `fragment_pagination_root`. Bails at [`crate::MAX_DOM_DEPTH`] —
/// any nodes below that depth go unrecorded (matches
/// `record_subtree_descendants`).
///
/// ## Known gaps deferred to `fulgur-a9qf` (Phase 3.1.5)
///
/// `fragment_block_subtree` does **not** mirror `fragment_pagination_root`
/// in three respects. None of these surface in the current test corpus
/// (`cargo test -p fulgur` 1111 / 0); each is tracked as a regression
/// scope-add on `fulgur-a9qf` (notes §5a / §5b / §5c) so they close
/// alongside in-place mid-element split:
///
/// - **Nested `position: running()` markers are not skipped here.** The
///   helper has no access to `running_store`, so a running marker that
///   sits inside an oversized subtree is treated as in-flow and
///   over-advances `cursor_y`. Body-level filtering is intact; only the
///   recursion path is affected.
/// - **Nested inline roots are not split at line edges.** When a tall
///   `<p>` (multi-line inline root) lives inside an oversized ancestor,
///   the recursion falls back to DOM-child block split rather than
///   calling `collect_inline_line_metrics` / `fragment_inline_root` like
///   the body-level walker does.
/// - **Multi-page recursive traversal does not emit per-page parent
///   fragments for intermediate pages.** When the recursive call
///   advances more than one page, only the first and last page get a
///   parent-`parent_id` fragment via the pre-recursion overflow close
///   and the trailing close at the end of this function. Counter /
///   string-set / bookmark ops attached to `parent_id` itself would
///   then miss the intermediate pages — the existing tests attach ops
///   to leaf children, so this stays masked until 3.1.5.
///
/// Returns `(final_page_index, final_cursor_y)`: the page and y where
/// the parent's last child finished. The caller resumes its outer
/// cursor from these values.
#[allow(clippy::too_many_arguments)]
fn fragment_block_subtree(
    geometry: &mut PaginationGeometryTable,
    doc: &BaseDocument,
    column_styles: Option<&crate::column_css::ColumnStyleTable>,
    used_page_names: Option<&crate::blitz_adapter::UsedPageNameTable>,
    parent_id: usize,
    parent_w: f32,
    parent_x_in_body: f32,
    page_in: u32,
    cursor_in: f32,
    page_height_px: f32,
    depth: usize,
) -> (u32, f32) {
    if depth >= crate::MAX_DOM_DEPTH {
        // Bailed: emit a single whole-fragment for the parent at its
        // entry coordinates so geometry still has an entry for it.
        let h = doc
            .get_node(parent_id)
            .map(|n| n.final_layout.size.height)
            .unwrap_or(0.0);
        geometry
            .entry(parent_id)
            .or_default()
            .fragments
            .push(Fragment {
                page_index: page_in,
                x: parent_x_in_body,
                y: cursor_in,
                width: parent_w,
                height: h,
            });
        return (page_in, cursor_in + h);
    }
    let Some(parent) = doc.get_node(parent_id) else {
        return (page_in, cursor_in);
    };

    let mut page_index = page_in;
    let mut cursor_y = cursor_in;
    // Y on `page_index` where the parent's current-page fragment
    // starts. We close one parent fragment and start a new one each
    // time we cross a page boundary.
    let mut page_start_y = cursor_in;
    // fulgur-kv0r: parent-relative y of the first in-flow child on
    // the current page strip. Taffy's `layout.location.y` is in the
    // parent's full coordinate system (same value across page
    // splits); each child's page-local y becomes
    // `page_start_y + (this_top_in_parent - page_taffy_origin)`,
    // which gives:
    // - block siblings: sequential placement (same as cursor-advance)
    // - grid / flex parallel siblings: same y (Taffy reports same
    //   `location.y` for cards in the same row, so the offset
    //   collapses to the row's first y).
    //
    // `origin_pending` defers the rebase: after a page advance
    // (forced break, strip overflow, recursion crossing) we don't
    // immediately know whether the next child is sequential or
    // parallel, so we set the origin lazily to the next-arriving
    // child's `this_top_in_parent` — that child lands at
    // `page_start_y` regardless of what its absolute Taffy y is.
    let mut page_taffy_origin: f32 = 0.0;
    let mut origin_pending: bool = false;
    // fulgur-uebl: tracks the previous in-flow sibling's used page-name
    // for implicit forced-break detection; see `fragment_pagination_root`
    // for the rationale and the outer-Option semantics.
    let mut prev_used_page: Option<Option<String>> = None;
    // fulgur-uebl: flex / grid containers establish a flex/grid
    // formatting context where children are not class A break points
    // (CSS Fragmentation 3 §3.2). The `page` property doesn't apply to
    // flex / grid items, so we suppress the implicit-forced-break
    // comparison among them. Atomic inline containers (`inline-block`,
    // `inline-flex`, `inline-grid`) are similarly opaque from a
    // pagination perspective — their internal block flow does not
    // paginate independently, so sibling comparison among their
    // children would just produce spurious breaks. Orthogonal-flow
    // containers (writing-mode different from their own parent) are
    // also treated atomically per CSS Writing Modes 4 §9. Inner
    // block-level descendants in their own BFC still get the
    // comparison via deeper recursion.
    let parent_is_orthogonal = parent
        .parent
        .and_then(|gp_id| doc.get_node(gp_id))
        .is_some_and(|gp| crate::blitz_adapter::is_orthogonal_to_parent(gp, parent));
    let suppress_page_check = crate::blitz_adapter::is_flex_or_grid_container_node(parent)
        || crate::blitz_adapter::is_atomic_inline_container_node(parent)
        || parent_is_orthogonal;

    for &child_id in &parent.children {
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
        // Whitespace-only text — same skip as `fragment_pagination_root`.
        if let Some(text) = child.text_data()
            && text.content.chars().all(char::is_whitespace)
        {
            continue;
        }
        // CSS 2.1 §10.6.4: out-of-flow children do not contribute to
        // their containing block's normal-flow height.
        {
            use ::style::properties::longhands::position::computed_value::T as Pos;
            let is_out_of_flow = child.primary_styles().is_some_and(|s| {
                matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed)
            });
            if is_out_of_flow {
                continue;
            }
        }
        let layout = child.final_layout;
        let child_h = layout.size.height;
        let child_w = if layout.size.width > 0.0 {
            layout.size.width
        } else {
            parent_w
        };

        // fulgur-a36m: read break-* props for this child once. Both
        // the zero-height and non-zero paths honour them.
        let break_props = column_styles
            .and_then(|t| t.get(&child_id))
            .cloned()
            .unwrap_or_default();
        // fulgur-uebl: detect page-name change against the previous
        // in-flow sibling and treat it as an implicit forced break.
        // Compare prev's `end` against this child's `start`. Floats are
        // out of normal flow (CSS 2.1 §9.5) and skipped here too.
        let is_float = crate::blitz_adapter::node_is_floating(child);
        let (used_start, used_end) = used_page_names
            .and_then(|t| t.get(&child_id).cloned())
            .unwrap_or((None, None));
        let page_name_changed = !suppress_page_check
            && !is_float
            && prev_used_page.as_ref().is_some_and(|p| *p != used_start);
        let break_before_page = matches!(
            break_props.break_before,
            Some(crate::draw_primitives::BreakBefore::Page)
        ) || page_name_changed;
        let break_after_page = matches!(
            break_props.break_after,
            Some(crate::draw_primitives::BreakAfter::Page)
        );

        // Compute Taffy parent-relative top early — both the zero-
        // height path below and the non-zero path further down use
        // it (and break-before / break-after rebases the
        // `page_taffy_origin` against it on page advance).
        let this_top_in_parent = layout.location.y;
        // Apply deferred origin rebase: the previous child's
        // page advance left `origin_pending` set, so this child
        // becomes the "first child on the new page strip".
        if origin_pending {
            page_taffy_origin = this_top_in_parent;
            origin_pending = false;
        }

        if child_h <= 0.0 {
            // Phase 2.3 fix: zero-height **element** nodes still
            // need to enter geometry so their counter / string-set
            // / bookmark markers participate in the parity walks.
            //
            // Zero-height children skip the inter-child gap (matching
            // `fragment_pagination_root`'s zero-height branch where
            // `continue` happens before the gap calc), so break-before
            // can fire here without first folding gap into cursor_y.
            if break_before_page && cursor_y > page_start_y {
                geometry
                    .entry(parent_id)
                    .or_default()
                    .fragments
                    .push(Fragment {
                        page_index,
                        x: parent_x_in_body,
                        y: page_start_y,
                        width: parent_w,
                        height: cursor_y - page_start_y,
                    });
                page_index += 1;
                cursor_y = 0.0;
                page_start_y = 0.0;
                // Zero-height break-before: this child IS the first
                // on the new page — apply origin rebase eagerly.
                page_taffy_origin = this_top_in_parent;
                origin_pending = false;
            }
            if child.element_data().is_some() {
                geometry
                    .entry(child_id)
                    .or_default()
                    .fragments
                    .push(Fragment {
                        page_index,
                        x: parent_x_in_body + layout.location.x,
                        y: cursor_y,
                        width: child_w,
                        height: 0.0,
                    });
            }
            // Honour `break-after: page` for zero-height elements
            // too — same fulgur-p3uf (Phase 3.1.5a) fix as
            // `fragment_pagination_root`'s zero-height branch.
            if break_after_page {
                geometry
                    .entry(parent_id)
                    .or_default()
                    .fragments
                    .push(Fragment {
                        page_index,
                        x: parent_x_in_body,
                        y: page_start_y,
                        width: parent_w,
                        height: cursor_y - page_start_y,
                    });
                page_index += 1;
                cursor_y = 0.0;
                page_start_y = 0.0;
                // Zero-height break-after: NEXT child is the first
                // on the new page — defer origin rebase.
                origin_pending = true;
            }
            if !is_float {
                prev_used_page = Some(used_end.clone());
            }
            continue;
        }

        // fulgur-kv0r: place the child at its Taffy-reported parent-
        // relative y, offset by the parent's start on the current
        // page (`page_start_y`) and rebased against
        // `page_taffy_origin` so the first child on each page strip
        // lands at `page_start_y` regardless of its absolute parent
        // y. For grid / flex parallel siblings (same `location.y`),
        // this places them at the same page-local y; for sequential
        // block flow, it matches Taffy's stacked positions exactly.
        let mut child_page_y = page_start_y + (this_top_in_parent - page_taffy_origin);
        // Update the cursor only when the child's bottom advances
        // past it. For block flow this matches cursor advancing by
        // `gap + child_h`; for grid parallel siblings the cursor
        // tracks the row's max bottom (so break-before / overflow
        // checks see the full row height).
        cursor_y = cursor_y.max(child_page_y);

        // Honour `break-before: page`. Leading collapse: only fires
        // when some content has already been placed on this page —
        // gated by `cursor_y > page_start_y` (mirrors body-level's
        // `cursor_y > 0.0` since body's implicit page_start is 0).
        if break_before_page && cursor_y > page_start_y {
            geometry
                .entry(parent_id)
                .or_default()
                .fragments
                .push(Fragment {
                    page_index,
                    x: parent_x_in_body,
                    y: page_start_y,
                    width: parent_w,
                    height: cursor_y - page_start_y,
                });
            page_index += 1;
            cursor_y = 0.0;
            page_start_y = 0.0;
            // The breaking child is the first in-flow child on the
            // new page strip. Rebase the Taffy origin to its
            // `this_top_in_parent` so it lands at `page_start_y` (= 0)
            // — discarding the inter-child gap, matching CSS 3
            // Fragmentation §3 (margins at forced breaks truncate).
            page_taffy_origin = this_top_in_parent;
            child_page_y = 0.0;
        }

        // (Strip-overflow page cut moved below the recursion gate as
        // part of fulgur-7hf5 — see the `if cursor_y > page_start_y
        // && cursor_y + child_h > page_height_px` block after the
        // gate. The gate must run from the **current** cursor so an
        // in-place split produces a `WithinChild`-shaped result on
        // the current strip, not a pre-advanced fresh page.)

        let child_x_in_body = parent_x_in_body + layout.location.x;

        // fulgur-7hf5 (Phase 3.1.5c): unified recursion gate matching
        // `fragment_pagination_root`'s body-direct branch — recurse
        // whenever the child's subtree would split (in-place,
        // truly-oversized, or forced-break-below). The recursion
        // enters from the current cursor so an in-place split
        // produces a `WithinChild`-shaped result on the current page
        // strip and a tail on the next.
        //
        // `would_split_block_subtree` returns `false` when all the
        // child's grandchildren fit the available strip — protects
        // against the "parent CSS height > children sum" case where
        // recursion would emit a parent fragment shorter than
        // expected.
        let available_strip = (page_height_px - cursor_y).max(0.0);
        // fulgur-7hf5: see body-direct branch — multicol containers
        // are atomic from the fragmenter's perspective.
        let is_multicol = crate::blitz_adapter::is_multicol_container(child);
        let needs_recursion = !child.children.is_empty()
            && !is_multicol
            && (has_forced_break_below(doc, child_id, column_styles, 0)
                || has_page_name_change_below(doc, child_id, used_page_names, 0)
                || would_split_block_subtree(doc, child_id, available_strip, page_height_px, 0));
        if needs_recursion {
            let pre_recursion_page = page_index;
            let (np, nc) = fragment_block_subtree(
                geometry,
                doc,
                column_styles,
                used_page_names,
                child_id,
                child_w,
                child_x_in_body,
                page_index,
                cursor_y,
                page_height_px,
                depth + 1,
            );
            page_index = np;
            cursor_y = nc;
            // If the recursion crossed a boundary, the parent's
            // current-page fragment must restart at y=0 on the new
            // page. Defensive `nc < page_start_y` guards against
            // backward cursor returns (impossible in normal flow).
            if page_index != pre_recursion_page || nc < page_start_y {
                page_start_y = 0.0;
                // Recursion crossed pages: NEXT sibling is the first
                // child on the new page — defer origin rebase to its
                // arrival (could be sequential block or grid / flex
                // parallel, same handling either way).
                origin_pending = true;
            }

            // Honour `break-after: page` after recursion.
            if break_after_page {
                geometry
                    .entry(parent_id)
                    .or_default()
                    .fragments
                    .push(Fragment {
                        page_index,
                        x: parent_x_in_body,
                        y: page_start_y,
                        width: parent_w,
                        height: cursor_y - page_start_y,
                    });
                page_index += 1;
                cursor_y = 0.0;
                page_start_y = 0.0;
                // Break-after: NEXT child starts the new page —
                // defer origin rebase to its arrival.
                origin_pending = true;
            }
            if !is_float {
                prev_used_page = Some(used_end.clone());
            }
            continue;
        }

        // No recursion — apply the strip-overflow page cut for
        // children that don't split (non-splittable, or splittable
        // but all grandchildren fit the available strip — the
        // parent-CSS-height-vs-children-sum case stays here).
        // Use `child_page_y + child_h` (the actual placement bottom)
        // rather than `cursor_y + child_h` so a parallel sibling
        // returning to a smaller page-local y is checked correctly.
        if child_page_y > page_start_y && child_page_y + child_h > page_height_px {
            geometry
                .entry(parent_id)
                .or_default()
                .fragments
                .push(Fragment {
                    page_index,
                    x: parent_x_in_body,
                    y: page_start_y,
                    width: parent_w,
                    height: cursor_y - page_start_y,
                });
            page_index += 1;
            cursor_y = 0.0;
            page_start_y = 0.0;
            // Forced to a fresh page: rebase the Taffy origin so the
            // current child lands at page_start_y (= 0) on the new
            // page. Sequential siblings then continue from this point.
            page_taffy_origin = this_top_in_parent;
            child_page_y = 0.0;
        }

        // Child fits the strip (or is an atomic oversized leaf that
        // simply overflows below the page bottom — Pageable's same
        // fallback at `pageable.rs:1252`, `in_flow_count == 1 →
        // NoSplit`). Emit its fragment and recurse into descendants
        // on the same page.
        geometry
            .entry(child_id)
            .or_default()
            .fragments
            .push(Fragment {
                page_index,
                x: child_x_in_body,
                y: child_page_y,
                width: child_w,
                height: child_h,
            });
        record_subtree_descendants(
            geometry,
            doc,
            child_id,
            page_index,
            child_page_y,
            child_x_in_body,
            depth + 1,
        );
        // Track the lowest point reached on this page so the
        // overflow / break-before checks above see the full row's
        // bottom for grid / flex parents (parallel siblings update
        // `cursor_y` to `max(cursor_y, child_page_y + child_h)` —
        // the per-row max bottom).
        cursor_y = cursor_y.max(child_page_y + child_h);

        // Honour `break-after: page` after the child fragment lands
        // (and the descendant walk records same-page entries).
        if break_after_page {
            geometry
                .entry(parent_id)
                .or_default()
                .fragments
                .push(Fragment {
                    page_index,
                    x: parent_x_in_body,
                    y: page_start_y,
                    width: parent_w,
                    height: cursor_y - page_start_y,
                });
            page_index += 1;
            cursor_y = 0.0;
            page_start_y = 0.0;
            // Break-after: NEXT child starts the new page — defer
            // origin rebase to its arrival.
            origin_pending = true;
        }
        if !is_float {
            prev_used_page = Some(used_end.clone());
        }
    }

    // Close the parent's fragment for the final page span. Always
    // emit at least one fragment so the parent is represented in
    // geometry — `collect_counter_states` and friends look up nodes
    // by id, and a missing entry would silently bypass the parity
    // gate via the early `counter_ops_by_node ⊄ geometry` check in
    // `render.rs`. Height may be 0 when every child was skipped
    // (whitespace / OOF / running) — that's intentional and
    // matches `fragment_pagination_root`'s zero-height-element path.
    geometry
        .entry(parent_id)
        .or_default()
        .fragments
        .push(Fragment {
            page_index,
            x: parent_x_in_body,
            y: page_start_y,
            width: parent_w,
            height: cursor_y - page_start_y,
        });

    (page_index, cursor_y)
}

/// fulgur-p55h: read per-line `(min_coord, max_coord)` pairs from a
/// node's Parley `inline_layout_data`, if any.
///
/// `min_coord` is the line's top-most Y in the paragraph's local
/// coordinate system; `max_coord` is its bottom-most Y. Both are in
/// CSS pixels and accumulate top-to-bottom across the line vector.
/// Returns an empty vec for non-inline-root nodes (block / text /
/// element with no inline children) so callers can branch on
/// `metrics.len() > 1` to decide between line-aware and block paths.
fn collect_inline_line_metrics(node: &blitz_dom::Node) -> Vec<(f32, f32)> {
    let Some(elem) = node.element_data() else {
        return Vec::new();
    };
    let Some(text_layout) = elem.inline_layout_data.as_deref() else {
        return Vec::new();
    };
    text_layout
        .layout
        .lines()
        .map(|line| {
            let m = line.metrics();
            (m.min_coord, m.max_coord)
        })
        .collect()
}

/// fulgur-p55h: split a multi-line inline root across page boundaries
/// at line edges, append one Fragment per page span to the geometry
/// table, and return the updated `(page_index, cursor_y, fragments_emitted)`.
///
/// Mirrors the v1 paragraph-pageable split path (removed in PR 8j-1;
/// see git history): walk lines, track the first line of the current
/// fragment in `fragment_start_idx`, and split when the cumulative
/// height in paragraph-local coords would push the bottom past
/// `page_height_px - paragraph_top_in_body`.
///
/// fulgur-s67g Phase 2.1 (widow / orphan): each candidate split point
/// must leave the **first** fragment with `>= ORPHANS_MIN` lines and
/// the **remainder** of the paragraph with `>= WIDOWS_MIN` lines.
/// When neither holds at the natural overflow point, the split is
/// skipped — subsequent lines accumulate into the current fragment
/// (overflow-tolerant) until a valid split is found or the paragraph
/// ends. This matches the v1 Pageable side: the paragraph-pageable
/// split path (removed in PR 8j-1; see git history) returned `None`
/// when widows/orphans could not be honoured, which the outer
/// `BlockPageable::split` resolved by emitting the whole paragraph at
/// the current position (oversized or pushed to a fresh page by
/// sibling-driven flow).
///
/// Pageable hard-codes `orphans = widows = 2` via `Pagination::default()`
/// (`pageable.rs:268-275`). CSS `orphans` / `widows` properties are
/// not parsed today, so the fragmenter uses the same constants.
///
/// Output:
///
/// - On a single-page paragraph (no overflow), one Fragment is appended
///   covering all lines. `cursor_y` advances by the paragraph's natural
///   height.
/// - On a multi-page paragraph, one Fragment per page is appended. The
///   final `cursor_y` is the height consumed on the last page (lines
///   ending on a partial page leave room for a following sibling).
/// - On a paragraph with too few lines to honour orphans+widows
///   simultaneously (`< ORPHANS_MIN + WIDOWS_MIN` lines total), no
///   split is taken — the paragraph emits as one fragment, possibly
///   oversized.
///
/// Edge case: if the very first line on a fresh page is taller than
/// the page strip, the line is emitted as an oversized fragment (no
/// further mid-line split) — same fallback as the block branch.
#[allow(clippy::too_many_arguments)]
fn fragment_inline_root(
    geometry: &mut PaginationGeometryTable,
    child_id: usize,
    paragraph_x: f32,
    width: f32,
    initial_cursor_y: f32,
    initial_page_index: u32,
    page_height_px: f32,
    line_metrics: &[(f32, f32)],
) -> (u32, f32, usize) {
    /// CSS 3 Fragmentation default for `orphans`. Matches Pageable's
    /// `Pagination::default()`.
    const ORPHANS_MIN: usize = 2;
    /// CSS 3 Fragmentation default for `widows`.
    const WIDOWS_MIN: usize = 2;

    if line_metrics.is_empty() {
        return (initial_page_index, initial_cursor_y, 0);
    }

    let mut page_index = initial_page_index;
    let mut paragraph_top_in_body = initial_cursor_y;
    let mut fragment_start_idx: usize = 0;
    let mut emitted = 0usize;
    let total_lines = line_metrics.len();

    for (i, &(_line_top_local, line_bottom_local)) in line_metrics.iter().enumerate() {
        let frag_top_local = line_metrics[fragment_start_idx].0;
        let projected_bottom_in_body = paragraph_top_in_body + (line_bottom_local - frag_top_local);

        if projected_bottom_in_body > page_height_px && i > fragment_start_idx {
            // fulgur-s67g Phase 2.1: honour widow / orphan minimums.
            let first_size = i - fragment_start_idx;
            let remaining_size = total_lines - i;
            if first_size < ORPHANS_MIN || remaining_size < WIDOWS_MIN {
                // Cannot split here without violating widow/orphan.
                // Skip — keep accumulating into the current fragment.
                // If no future split point honours both constraints,
                // the loop falls through to "emit final fragment"
                // below and the paragraph emits as a single oversized
                // fragment — matching Pageable's `split → None` →
                // outer-emits-whole fallback.
                continue;
            }

            // Lines [fragment_start_idx, i) fit on the current page.
            // Emit them as one fragment, advance to the next page, and
            // start the next fragment at line i.
            let prev_line_bottom = line_metrics[i - 1].1;
            let frag_h = prev_line_bottom - frag_top_local;
            let frag = Fragment {
                page_index,
                x: paragraph_x,
                y: paragraph_top_in_body,
                width,
                height: frag_h,
            };
            geometry.entry(child_id).or_default().fragments.push(frag);
            emitted += 1;

            page_index += 1;
            paragraph_top_in_body = 0.0;
            fragment_start_idx = i;
        }
    }

    // Final fragment covers lines [fragment_start_idx, end).
    let frag_top_local = line_metrics[fragment_start_idx].0;
    let last_bottom_local = line_metrics.last().expect("non-empty checked above").1;
    let frag_h = last_bottom_local - frag_top_local;
    let frag = Fragment {
        page_index,
        x: paragraph_x,
        y: paragraph_top_in_body,
        width,
        height: frag_h,
    };
    geometry.entry(child_id).or_default().fragments.push(frag);
    emitted += 1;

    let cursor_y = paragraph_top_in_body + frag_h;
    (page_index, cursor_y, emitted)
}

/// Per-page state for a named string emitted by `string-set:`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringSetPageState {
    /// Value at start of page (carried from previous page's `last`).
    pub start: Option<String>,
    /// First value set on this page.
    pub first: Option<String>,
    /// Last value set on this page.
    pub last: Option<String>,
}

/// Per-page state for running element instances of a given name.
#[derive(Debug, Clone, Default)]
pub struct PageRunningState {
    /// Instance IDs of running elements whose source position falls on
    /// this page, in source order.
    pub instance_ids: Vec<usize>,
}

/// fulgur-6tco: walk the geometry table page-by-page to thread
/// `string-set` state across pages.
///
/// For each page index 0..max_page:
///
/// 1. Initialise per-name `start` from the previous page's `last`
///    (the carry).
/// 2. For each node id whose **first** fragment lands on this page,
///    apply its `(name, value)` markers in NodeId order — records
///    `first` (only set once per page per name) and updates `last`
///    plus the carry for subsequent pages.
///
/// Markers fire only on a node's first appearance: when an inline
/// root spans two pages, its second-page fragment does **not** re-
/// emit the marker.
///
/// Source-order assumption: `geometry` is a `BTreeMap<usize, ..>` so
/// iteration is by ascending NodeId. For body's direct children that
/// matches DOM source order, since Blitz allocates ids sequentially
/// during parse. Nested string-set declarations (markers attached to
/// a `<span>` inside a `<p>`) are not in the fragmenter's geometry
/// table today and so are silently dropped — same scope limitation as
/// `fragment_pagination_root` itself.
pub fn collect_string_set_states(
    geometry: &PaginationGeometryTable,
    string_set_by_node: &BTreeMap<usize, Vec<(String, String)>>,
) -> Vec<BTreeMap<String, StringSetPageState>> {
    let max_page = geometry
        .values()
        .flat_map(|g| g.fragments.iter())
        .map(|f| f.page_index)
        .max()
        .map(|m| m + 1)
        .unwrap_or(1);

    // For each page, the list of nodes whose first fragment lands
    // there, in NodeId (≈ source) order.
    let mut nodes_per_page: Vec<Vec<usize>> = vec![Vec::new(); max_page as usize];
    for (&node_id, geom) in geometry {
        if let Some(first_frag) = geom.fragments.first()
            && (first_frag.page_index as usize) < nodes_per_page.len()
        {
            nodes_per_page[first_frag.page_index as usize].push(node_id);
        }
    }

    let mut result: Vec<BTreeMap<String, StringSetPageState>> =
        Vec::with_capacity(nodes_per_page.len());
    let mut carry: BTreeMap<String, String> = BTreeMap::new();

    for nodes in &nodes_per_page {
        let mut page_state: BTreeMap<String, StringSetPageState> = BTreeMap::new();
        for (name, value) in &carry {
            page_state.entry(name.clone()).or_default().start = Some(value.clone());
        }
        for node_id in nodes {
            let Some(entries) = string_set_by_node.get(node_id) else {
                continue;
            };
            for (name, value) in entries {
                let state = page_state.entry(name.clone()).or_default();
                if state.first.is_none() {
                    state.first = Some(value.clone());
                }
                state.last = Some(value.clone());
                carry.insert(name.clone(), value.clone());
            }
        }
        result.push(page_state);
    }

    result
}

/// Walk the geometry table page-by-page and emit the running element
/// instances whose first fragment lands on each page.
///
/// Each `instance_id` is adopted only once — on the page where its
/// node's first fragment lands. This matches the source-order policy
/// the margin-box renderer uses with `resolve_element_policy` to pick
/// the right instance for `first` / `last` / `first-except`.
pub fn collect_running_element_states(
    geometry: &PaginationGeometryTable,
    running_store: &crate::gcpm::running::RunningElementStore,
) -> Vec<BTreeMap<String, PageRunningState>> {
    let max_page = geometry
        .values()
        .flat_map(|g| g.fragments.iter())
        .map(|f| f.page_index)
        .max()
        .map(|m| m + 1)
        .unwrap_or(1);

    let mut result: Vec<BTreeMap<String, PageRunningState>> =
        vec![BTreeMap::new(); max_page as usize];

    for (&node_id, geom) in geometry {
        let Some(first_frag) = geom.fragments.first() else {
            continue;
        };
        let page_idx = first_frag.page_index as usize;
        if page_idx >= result.len() {
            continue;
        }
        let Some(instance_id) = running_store.instance_for_node(node_id) else {
            continue;
        };
        let Some(name) = running_store.name_of(instance_id) else {
            continue;
        };
        result[page_idx]
            .entry(name.to_string())
            .or_default()
            .instance_ids
            .push(instance_id);
    }

    result
}

/// fulgur-s67g Phase 2.3: walk the geometry table page-by-page and
/// replay counter operations in document order, returning the
/// cumulative counter snapshot at the end of each page.
///
/// Same source-order assumption as
/// [`collect_string_set_states`]: the per-node counter ops are
/// applied in the order they appear in the body's children list,
/// approximated by `BTreeMap<NodeId, _>` iteration. Nested counter
/// declarations on descendants of body's direct children are not in
/// the fragmenter's geometry today and are silently dropped — same
/// scope limitation as `fragment_pagination_root` itself.
pub fn collect_counter_states(
    geometry: &PaginationGeometryTable,
    counter_ops_by_node: &BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
) -> Vec<BTreeMap<String, i32>> {
    use crate::gcpm::CounterOp;
    use crate::gcpm::counter::CounterState;

    let max_page = geometry
        .values()
        .flat_map(|g| g.fragments.iter())
        .map(|f| f.page_index)
        .max()
        .map(|m| m + 1)
        .unwrap_or(1);

    // For each page, the list of nodes whose first fragment lands
    // there, in NodeId (≈ source) order.
    let mut nodes_per_page: Vec<Vec<usize>> = vec![Vec::new(); max_page as usize];
    for (&node_id, geom) in geometry {
        if let Some(first_frag) = geom.fragments.first()
            && (first_frag.page_index as usize) < nodes_per_page.len()
        {
            nodes_per_page[first_frag.page_index as usize].push(node_id);
        }
    }

    let mut state = CounterState::new();
    let mut result: Vec<BTreeMap<String, i32>> = Vec::with_capacity(nodes_per_page.len());

    for nodes in &nodes_per_page {
        for node_id in nodes {
            let Some(ops) = counter_ops_by_node.get(node_id) else {
                continue;
            };
            for op in ops {
                match op {
                    CounterOp::Reset { name, value } => state.reset(name, *value),
                    CounterOp::Increment { name, value } => state.increment(name, *value),
                    CounterOp::Set { name, value } => state.set(name, *value),
                }
            }
        }
        result.push(state.snapshot());
    }

    result
}

/// fulgur-jkl5: enumerate `position: fixed` elements and emit one
/// fragment per page so downstream rendering can repeat them on every
/// page (Chrome-compatible behaviour for paged media — see WPT
/// fixedpos-* family).
///
/// fulgur-rpvu: wired into the v2 production path. v2's
/// `dispatch_fragment` loop iterates `Fragment`s per (node_id, page),
/// so emitting one Fragment per page for each `position: fixed`
/// element produces the expected per-page repetition naturally. The
/// resulting `PaginationGeometry.is_repeat` is set to `true` so
/// consumers know each fragment carries the *full* content rather
/// than a slice (paragraph-line / block-height slicing must be
/// suppressed for repeat fragments). Both fixed-element paths
/// (v1 and v2) produce equivalent output.
///
/// `total_pages` is the document's resolved page count, typically
/// computed from `PaginationGeometryTable`'s max `page_index + 1` after
/// `run_pass*` has run. `0` is normalised to `1` so even an empty
/// document gets a valid fragment for any fixed element on it.
///
/// The fragment's `(x, y, width, height)` come from each fixed
/// element's existing `final_layout` — same coordinate frame the
/// non-paginated convert path already uses. **This function relies on
/// `blitz_adapter::relayout_position_fixed` (added in fulgur-tbxs,
/// branch `feat/fixedpos-viewport-cb`) having run beforehand** so
/// that `final_layout` reflects viewport-CB resolution rather than
/// the inherited (often wrong) abs-position layout. The fragmenter branch
/// does not yet include `relayout_position_fixed`; once both land on
/// `main` this function picks up the corrected positions automatically.
///
/// The emitted fragments are appended to `geometry` (typically the
/// table returned by `run_pass`) so a single side-table carries both
/// the body-fragmentation geometry and the fixed-element repetition.
/// Convert-side consumers (`convert::positioned.rs`) iterate the
/// node's `Vec<Fragment>` to place one copy of the element per page.
pub fn append_position_fixed_fragments(
    geometry: &mut PaginationGeometryTable,
    doc: &BaseDocument,
    total_pages: u32,
) {
    use ::style::properties::longhands::position::computed_value::T as Pos;

    let pages = total_pages.max(1);
    let mut fixed_ids: Vec<usize> = Vec::new();
    let root_id = doc.root_element().id;
    walk_for_position_fixed(doc, root_id, &mut fixed_ids, 0);

    for id in fixed_ids {
        let Some(node) = doc.get_node(id) else {
            continue;
        };
        // Re-check style here even though `walk_for_position_fixed`
        // already filtered — guards against nodes whose style was
        // mutated between the walk and this read (defensive only,
        // single-threaded code path).
        let is_fixed = node
            .primary_styles()
            .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Fixed));
        if !is_fixed {
            continue;
        }
        let layout = node.final_layout;
        let (w, h) = (layout.size.width, layout.size.height);
        let (x, y) = (layout.location.x, layout.location.y);

        let entry = geometry.entry(id).or_default();
        // Replace any prior placements (e.g. if the fixed element was
        // also walked by `fragment_pagination_root` and emitted as a
        // single fragment). Per-page repetition is the canonical
        // representation for fixed content.
        entry.fragments.clear();
        entry.is_repeat = true;
        for page_index in 0..pages {
            entry.fragments.push(Fragment {
                page_index,
                x,
                y,
                width: w,
                height: h,
            });
        }
    }

    // Don't allocate empty entries for nodes without fragments.
    geometry.retain(|_, geom| !geom.fragments.is_empty());
}

/// Recursive walker that collects every node id whose computed
/// `position` is `fixed`. Mirrors the helper of the same shape in
/// `blitz_adapter::relayout_position_fixed`. Visits raw `node.children`
/// rather than `layout_children` because the latter may be invalidated
/// by the time this runs, and pseudo-elements (`::before` / `::after`)
/// live in `node.before` / `node.after` outside the children vec.
///
/// Used by [`append_position_fixed_fragments`].
fn walk_for_position_fixed(doc: &BaseDocument, node_id: usize, out: &mut Vec<usize>, depth: usize) {
    use ::style::properties::longhands::position::computed_value::T as Pos;

    if depth >= crate::MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    let is_fixed = node
        .primary_styles()
        .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Fixed));
    if is_fixed {
        out.push(node_id);
    }
    for &child_id in &node.children {
        walk_for_position_fixed(doc, child_id, out, depth + 1);
    }
    // Pseudo-elements: a `::before { position: fixed }` would
    // otherwise be missed by the children-only walk. The `before` /
    // `after` slots live directly on `Node`, not on `ElementData`.
    if let Some(pseudo_id) = node.before {
        walk_for_position_fixed(doc, pseudo_id, out, depth + 1);
    }
    if let Some(pseudo_id) = node.after {
        walk_for_position_fixed(doc, pseudo_id, out, depth + 1);
    }
}

/// fulgur-jkl5: total page count implied by a geometry table.
///
/// Returns `max(page_index) + 1` if the table has any fragments, else
/// `1` (at least one page is always implied).
///
/// Used by fulgur-cj6u Phase 1.2 as the fragmenter-side input to a
/// `paginate(...).len() == implied_page_count(&geometry)` parity
/// assertion in `render_to_pdf_with_gcpm`. Drift between Pageable's
/// split decisions and the fragmenter is the regression
/// signal Phase 2 work needs to chase.
pub fn implied_page_count(geometry: &PaginationGeometryTable) -> u32 {
    geometry
        .values()
        .flat_map(|g| g.fragments.iter())
        .map(|f| f.page_index)
        .max()
        .map(|m| m + 1)
        .unwrap_or(1)
}

/// Locate the `<body>` element id by walking the html root's children.
///
/// Prefers the first child whose tag name is `body`. Falls back to
/// `None` when the document has no `<body>` (e.g. fragments parsed
/// outside a normal document context). Spec-pure HTML5 parsing always
/// synthesizes a `<body>`, but tests and library callers can pass
/// arbitrary fragments so we do not rely on its presence.
fn find_body_id(doc: &BaseDocument) -> Option<usize> {
    let root_id = doc.root_element().id;
    let root = doc.get_node(root_id)?;
    for child_id in &root.children {
        let Some(child) = doc.get_node(*child_id) else {
            continue;
        };
        if let Some(elem) = child.element_data()
            && elem.name.local.as_ref() == "body"
        {
            return Some(*child_id);
        }
    }
    None
}

// ── Trait delegation to BaseDocument (mirrors multicol_layout) ────────────
//
// These trait impls are not exercised by the current measurement-only
// `fragment_pagination_root` walk — they are scaffolding for the next
// iteration that will call `taffy::compute_root_layout(self, body_id, ...)`
// to drive the fragmenter through Taffy's normal dispatch. Keeping the
// shapes here so the upgrade is a localized change.

impl TraversePartialTree for PaginationLayoutTree<'_> {
    type ChildIter<'a>
        = <BaseDocument as TraversePartialTree>::ChildIter<'a>
    where
        Self: 'a;

    fn child_ids(&self, node_id: NodeId) -> Self::ChildIter<'_> {
        self.doc.child_ids(node_id)
    }

    fn child_count(&self, node_id: NodeId) -> usize {
        self.doc.child_count(node_id)
    }

    fn get_child_id(&self, node_id: NodeId, index: usize) -> NodeId {
        self.doc.get_child_id(node_id, index)
    }
}

impl TraverseTree for PaginationLayoutTree<'_> {}

impl CacheTree for PaginationLayoutTree<'_> {
    fn cache_get(
        &self,
        node_id: NodeId,
        known_dimensions: Size<Option<f32>>,
        available_space: Size<AvailableSpace>,
        run_mode: taffy::RunMode,
    ) -> Option<taffy::LayoutOutput> {
        self.doc
            .cache_get(node_id, known_dimensions, available_space, run_mode)
    }

    fn cache_store(
        &mut self,
        node_id: NodeId,
        known_dimensions: Size<Option<f32>>,
        available_space: Size<AvailableSpace>,
        run_mode: taffy::RunMode,
        layout_output: taffy::LayoutOutput,
    ) {
        self.doc.cache_store(
            node_id,
            known_dimensions,
            available_space,
            run_mode,
            layout_output,
        );
    }

    fn cache_clear(&mut self, node_id: NodeId) {
        self.doc.cache_clear(node_id);
    }
}

impl LayoutPartialTree for PaginationLayoutTree<'_> {
    type CoreContainerStyle<'a>
        = &'a taffy::Style<style::Atom>
    where
        Self: 'a;

    type CustomIdent = style::Atom;

    fn get_core_container_style(&self, node_id: NodeId) -> Self::CoreContainerStyle<'_> {
        self.doc.get_core_container_style(node_id)
    }

    fn set_unrounded_layout(&mut self, node_id: NodeId, layout: &taffy::Layout) {
        self.doc.set_unrounded_layout(node_id, layout);
    }

    fn resolve_calc_value(&self, calc_ptr: *const (), parent_size: f32) -> f32 {
        self.doc.resolve_calc_value(calc_ptr, parent_size)
    }

    fn compute_child_layout(
        &mut self,
        node_id: NodeId,
        inputs: taffy::tree::LayoutInput,
    ) -> taffy::LayoutOutput {
        if Some(usize::from(node_id)) == self.body_id {
            return compute_pagination_layout(self, node_id, inputs);
        }
        // Everything else delegates to BaseDocument's normal dispatch.
        self.doc.compute_child_layout(node_id, inputs)
    }
}

impl RoundTree for PaginationLayoutTree<'_> {
    fn get_unrounded_layout(&self, node_id: NodeId) -> taffy::Layout {
        self.doc.get_unrounded_layout(node_id)
    }

    fn set_final_layout(&mut self, node_id: NodeId, layout: &taffy::Layout) {
        self.doc.set_final_layout(node_id, layout);
    }
}

/// Custom layout dispatch for the body (the fragmenter's fragmentation root).
///
/// Mirrors the structure of [`crate::multicol_layout::compute_multicol_layout`]:
/// the wrapper's `compute_child_layout` fires for body, delegates the
/// real layout to `BaseDocument` (so children's `final_layout` is
/// populated correctly), then post-walks body's direct children and
/// records fragments in the geometry side-table.
///
/// In the next iteration this is where per-strip available_space
/// constraint and child-by-child re-layout will live. For the current
/// fragmenter it's a thin shim that proves the dispatch path works.
fn compute_pagination_layout(
    tree: &mut PaginationLayoutTree<'_>,
    body_id: NodeId,
    inputs: taffy::tree::LayoutInput,
) -> taffy::LayoutOutput {
    // Delegate the actual layout work to BaseDocument so children get
    // their normal natural sizes. The output is body's full natural
    // height — that height is what `convert::dom_to_drawables` already
    // expects to read from `final_layout`.
    let output = tree.doc.compute_child_layout(body_id, inputs);

    // Now post-walk to populate the geometry side-table. We can't reuse
    // `fragment_pagination_root` directly because it returns a fragment
    // count; the dispatch path doesn't need that, so we inline the same
    // walk and discard the count.
    let _emitted = tree.fragment_pagination_root();

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blitz_adapter;
    use std::ops::DerefMut;
    use std::sync::Arc;

    /// Parse helper for the fragmenter's tests.
    ///
    /// We deliberately don't accept a viewport height: `blitz_adapter::parse`
    /// uses a hardcoded viewport_h internally, and the fragmenter's strip slicing
    /// is driven by the `page_height_px` argument to `run_pass` rather than
    /// by the viewport. The fixtures pass viewport_w only.
    fn parse(html: &str, viewport_w: f32) -> blitz_html::HtmlDocument {
        let fonts: Vec<Arc<Vec<u8>>> = Vec::new();
        let mut doc = blitz_adapter::parse(html, viewport_w, &fonts);
        blitz_adapter::resolve(&mut doc);
        doc
    }

    #[test]
    fn empty_document_emits_only_body_fragment() {
        let mut doc = parse("<html><body></body></html>", 600.0);
        let table = run_pass(&mut doc, 800.0);
        // Phase 2.3 fix: body itself is now recorded so its own
        // counter / string-set / bookmark ops are visible to the
        // parity walks. Empty body → just the body fragment.
        assert_eq!(table.len(), 1, "expected only body fragment, got {table:?}");
    }

    #[test]
    fn html_only_input_still_paginates_synthesized_body() {
        // html5ever synthesizes `<body>` for any HTML input, so
        // `find_body_id` always succeeds in the parse pipeline. The
        // synthesized body has no children — the geometry table
        // still contains the body fragment itself (Phase 2.3 fix)
        // but no child entries.
        let mut doc = parse("<html></html>", 600.0);
        let tree = PaginationLayoutTree::new(&mut doc, 800.0);
        assert!(tree.body_id.is_some(), "html5ever should synthesize a body");
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(table.len(), 1, "expected only body fragment, got {table:?}");
    }

    /// fulgur-s67g Phase 2.5: nested descendants must be recorded
    /// in the geometry table on the same page as their ancestor, so
    /// bookmark / counter / string-set markers attached to deeply
    /// nested DOM elements participate in parity assertions.
    #[test]
    fn nested_descendants_inherit_parent_page() {
        let html = r#"
            <html><body>
              <div style="height: 600px">
                <h2 style="height: 30px">Section 1</h2>
              </div>
              <div style="height: 600px">
                <h2 style="height: 30px">Section 2</h2>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);

        // Two outer divs split across two pages (600 + 600 > 800),
        // each carrying a nested h2. Geometry should contain both
        // outer divs AND both inner h2s — four entries total — with
        // the h2 sharing its parent's page_index.
        assert!(
            table.len() >= 4,
            "expected at least 4 entries (2 divs + 2 h2s), got {}",
            table.len(),
        );
        let h2_pages: Vec<u32> = table
            .values()
            .filter(|g| g.fragments.iter().any(|f| (f.height - 30.0).abs() < 0.5))
            .map(|g| g.fragments[0].page_index)
            .collect();
        assert_eq!(h2_pages.len(), 2, "expected 2 h2 entries, got {h2_pages:?}");
        // Pages of the h2s should match those of their containing divs:
        // first div on page 0 → first h2 on page 0; second div on page
        // 1 → second h2 on page 1.
        assert_eq!(h2_pages, vec![0, 1]);
    }

    #[test]
    fn three_short_blocks_fit_one_page() {
        // Each block is 200px tall; page is 800px → all three fit on
        // page 0.
        let html = r#"
            <html><body>
              <div style="height: 200px"></div>
              <div style="height: 200px"></div>
              <div style="height: 200px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        // Phase 2.3 fix: body itself is recorded too, so total = 4
        // (body + 3 child divs). All on page 0.
        assert_eq!(table.len(), 4, "expected 4 entries, got {}", table.len());
        for (id, geom) in &table {
            assert_eq!(
                geom.fragments.len(),
                1,
                "node {id} should have a single fragment"
            );
            assert_eq!(geom.fragments[0].page_index, 0);
        }
    }

    #[test]
    fn oversize_block_run_breaks_to_next_page() {
        // Block 1 is 600px, block 2 is 400px. Page strip is 800px.
        // Block 1 fits on page 0 (cursor 0 → 600). Block 2 starts at
        // y=600, would end at y=1000 > 800 → break to page 1.
        let html = r#"
            <html><body>
              <div style="height: 600px"></div>
              <div style="height: 400px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        // Phase 2.3 fix: body + 2 children = 3 entries.
        // Body fragment is page 0; children are 0, 1.
        assert_eq!(table.len(), 3);
        let pages: Vec<u32> = table.values().map(|g| g.fragments[0].page_index).collect();
        assert_eq!(
            pages,
            vec![0, 0, 1],
            "body page 0, first child page 0, second child page 1, got {pages:?}"
        );
    }

    /// Phase 4 prerequisite repro: confirm `string-set` carry semantic
    /// across page break.
    #[test]
    fn string_set_carry_across_page_break() {
        use crate::blitz_adapter;
        use crate::convert::pt_to_px;
        use crate::gcpm::parser::parse_gcpm;
        use std::ops::DerefMut;
        use std::sync::Arc;

        let html = r#"<!DOCTYPE html>
<html>
<head><style>
@page { size: A4; margin: 60pt; }
h2 { string-set: chapter-title content(text); }
.box { padding: 60pt 0; }
</style></head>
<body>
<h2>Introduction</h2>
<div class="box">f1</div>
<div class="box">f2</div>
<div class="box">f3</div>
<div class="box">f4</div>
<div class="box">f5</div>
<div class="box">f6</div>
<div class="box">f7</div>
<div class="box">f8</div>
<h2 style="page-break-before:always">Background</h2>
</body></html>"#;
        let css = "h2 { string-set: chapter-title content(text); }";
        let gcpm = parse_gcpm(css);
        let fonts: Vec<Arc<Vec<u8>>> = Vec::new();
        let mut doc = blitz_adapter::parse(html, 600.0, &fonts);
        let pass = blitz_adapter::StringSetPass::new(gcpm.string_set_mappings.clone());
        let pass_ctx = blitz_adapter::PassContext { font_data: &fonts };
        blitz_adapter::apply_single_pass(&pass, &mut doc, &pass_ctx);
        let store = pass.into_store();
        blitz_adapter::resolve(&mut doc);
        let column_styles = blitz_adapter::extract_column_style_table(&doc);
        let geometry = run_pass_with_break_styles(doc.deref_mut(), pt_to_px(720.0), &column_styles);

        let mut by_node: std::collections::BTreeMap<usize, Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        for entry in store.entries() {
            by_node
                .entry(entry.node_id)
                .or_default()
                .push((entry.name.clone(), entry.value.clone()));
        }
        let states = collect_string_set_states(&geometry, &by_node);
        assert!(
            states.len() >= 2,
            "must span at least 2 pages, got {}",
            states.len()
        );
        let p0 = states[0]
            .get("chapter-title")
            .expect("page 0 must have chapter-title state");
        assert_eq!(p0.first.as_deref(), Some("Introduction"), "page 0 first");
        let p1 = states[1]
            .get("chapter-title")
            .expect("page 1 must have chapter-title state (carry)");
        assert_eq!(
            p1.start.as_deref(),
            Some("Introduction"),
            "page 1 start (carry from page 0 last)"
        );
    }

    /// Phase 3.4 follow-up (PR #296 Devin): regression for the
    /// fragmenter's running-element handling. `fragment_pagination_root`
    /// must record a zero-height fragment for every
    /// `position: running()` element so the running NodeId appears in
    /// geometry; without this, the downstream collect walk returns
    /// all-empty maps and `content: element(name)` in margin boxes
    /// silently produces nothing. Drive the engine pipeline through
    /// `Engine::render_html` and inspect the geometry table built by
    /// the same fragmenter pass.
    #[test]
    fn running_element_node_lands_in_geometry_with_zero_height() {
        use crate::blitz_adapter;
        use crate::convert::pt_to_px;
        use crate::gcpm::parser::parse_gcpm;
        use std::ops::DerefMut;
        use std::sync::Arc;

        let css = ".header { position: running(pageHeader); }";
        let html = r#"<!DOCTYPE html>
<html><head><style>.header { position: running(pageHeader); }</style></head>
<body>
<div class="header">Doc Header</div>
<p>Body.</p>
</body></html>"#;

        let gcpm = parse_gcpm(css);
        let fonts: Vec<Arc<Vec<u8>>> = Vec::new();
        let mut doc = blitz_adapter::parse(html, 600.0, &fonts);
        let pass = blitz_adapter::RunningElementPass::new(gcpm.running_mappings.clone());
        let pass_ctx = blitz_adapter::PassContext { font_data: &fonts };
        blitz_adapter::apply_single_pass(&pass, &mut doc, &pass_ctx);
        let store = pass.into_running_store();
        blitz_adapter::resolve(&mut doc);
        let column_styles = blitz_adapter::extract_column_style_table(&doc);

        let geometry = run_pass_with_break_and_running(
            doc.deref_mut(),
            pt_to_px(800.0),
            &column_styles,
            &store,
        );

        // The running element's NodeId must exist in geometry on page 0
        // with a zero-height fragment.
        let mut found_running_node = None;
        for (&node_id, geom) in &geometry {
            if store.instance_for_node(node_id).is_some() {
                found_running_node = Some((node_id, geom.fragments.clone()));
                break;
            }
        }
        let (node_id, fragments) =
            found_running_node.expect("running element NodeId must appear in geometry table");
        assert_eq!(fragments.len(), 1, "single zero-height fragment");
        assert_eq!(fragments[0].page_index, 0);
        assert_eq!(
            fragments[0].height, 0.0,
            "running fragment must not advance the cursor"
        );

        // collect_running_element_states must surface the instance.
        let states = collect_running_element_states(&geometry, &store);
        let entry = states[0]
            .get("pageHeader")
            .expect("pageHeader entry must appear in page 0 state");
        assert_eq!(
            entry.instance_ids,
            vec![store.instance_for_node(node_id).unwrap()]
        );
    }

    /// fulgur-6tco: synthesize a geometry table + string_set_by_node
    /// map and verify `collect_string_set_states` produces the expected
    /// per-page `(start, first, last)` shape.
    #[test]
    fn string_set_state_carries_across_pages() {
        use super::StringSetPageState;

        // Three nodes: A on page 0, B on page 0, C on page 1.
        // A sets header="a", B sets header="b" (so first/last on page 0
        // differ), C sets nothing — page 1 inherits "b" via carry.
        let mut geom = PaginationGeometryTable::new();
        geom.entry(10).or_default().fragments.push(Fragment {
            page_index: 0,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        });
        geom.entry(20).or_default().fragments.push(Fragment {
            page_index: 0,
            x: 0.0,
            y: 50.0,
            width: 100.0,
            height: 50.0,
        });
        geom.entry(30).or_default().fragments.push(Fragment {
            page_index: 1,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        });

        let mut markers: BTreeMap<usize, Vec<(String, String)>> = BTreeMap::new();
        markers.insert(10, vec![("header".into(), "a".into())]);
        markers.insert(20, vec![("header".into(), "b".into())]);

        let states = super::collect_string_set_states(&geom, &markers);
        assert_eq!(states.len(), 2);

        // Page 0: no carry (first page), first set by A, last updated by B.
        let p0 = &states[0]["header"];
        assert_eq!(
            *p0,
            StringSetPageState {
                start: None,
                first: Some("a".into()),
                last: Some("b".into()),
            }
        );
        // Page 1: carry from p0.last ("b"). C sets nothing → first/last stay None.
        let p1 = &states[1]["header"];
        assert_eq!(
            *p1,
            StringSetPageState {
                start: Some("b".into()),
                first: None,
                last: None,
            }
        );
    }

    #[test]
    fn string_set_first_appearance_only_for_split_paragraph() {
        // A node spans two pages (inline-aware split). Markers fire
        // only on the first appearance.
        use super::StringSetPageState;

        let mut geom = PaginationGeometryTable::new();
        geom.entry(42).or_default().fragments.push(Fragment {
            page_index: 0,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 800.0,
        });
        geom.entry(42).or_default().fragments.push(Fragment {
            page_index: 1,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 200.0,
        });

        let mut markers: BTreeMap<usize, Vec<(String, String)>> = BTreeMap::new();
        markers.insert(42, vec![("title".into(), "intro".into())]);

        let states = super::collect_string_set_states(&geom, &markers);
        assert_eq!(states.len(), 2);
        assert_eq!(
            states[0]["title"],
            StringSetPageState {
                start: None,
                first: Some("intro".into()),
                last: Some("intro".into()),
            }
        );
        assert_eq!(
            states[1]["title"],
            StringSetPageState {
                start: Some("intro".into()),
                first: None,
                last: None,
            }
        );
    }

    #[test]
    fn string_set_states_empty_geometry_returns_one_empty_page() {
        // Mirrors Pageable's "always at least one page" convention.
        let geom = PaginationGeometryTable::new();
        let markers = BTreeMap::new();
        let states = super::collect_string_set_states(&geom, &markers);
        assert_eq!(states.len(), 1);
        assert!(states[0].is_empty());
    }

    /// fulgur-jkl5: `position: fixed` element should emit one
    /// fragment per page so downstream rendering can repeat it.
    #[test]
    fn position_fixed_repeats_per_page() {
        let html = r#"
            <html><body>
              <div style="height: 600px"></div>
              <div style="height: 600px"></div>
              <div style="position: fixed; top: 10px; left: 20px;
                          width: 100px; height: 50px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);

        let mut geom = super::run_pass(doc.deref_mut(), 800.0);
        let pages_before = super::implied_page_count(&geom);
        assert!(
            pages_before >= 2,
            "two 600px blocks on 800px page should split → {pages_before} pages",
        );

        super::append_position_fixed_fragments(&mut geom, doc.deref_mut(), pages_before);

        // The fixed div should now appear in `geom` with one fragment
        // per page. We don't know its NodeId statically, so locate it
        // by the per-fragment width = 100.0.
        let fixed_entries: Vec<_> = geom
            .iter()
            .filter(|(_, g)| {
                g.fragments
                    .iter()
                    .any(|f| (f.width - 100.0).abs() < 0.5 && (f.height - 50.0).abs() < 0.5)
            })
            .collect();
        assert_eq!(
            fixed_entries.len(),
            1,
            "exactly one fixed element entry expected, got {}",
            fixed_entries.len()
        );
        let (_, fixed_geom) = fixed_entries[0];
        assert_eq!(
            fixed_geom.fragments.len() as u32,
            pages_before,
            "fixed element should have one fragment per page",
        );
        let pages_seen: Vec<u32> = fixed_geom.fragments.iter().map(|f| f.page_index).collect();
        assert_eq!(pages_seen, (0..pages_before).collect::<Vec<_>>());
    }

    #[test]
    fn position_fixed_with_no_pages_normalises_to_one_page() {
        // append_position_fixed_fragments(geom, doc, 0) should still
        // emit exactly one fragment per fixed element (the Pageable
        // "always at least one page" convention applied to fixed
        // repetition).
        let html = r#"
            <html><body>
              <div style="position: fixed; top: 0; left: 0;
                          width: 50px; height: 30px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_fixed_fragments(&mut geom, doc.deref_mut(), 0);
        assert_eq!(geom.len(), 1);
        let (_, g) = geom.iter().next().unwrap();
        assert_eq!(g.fragments.len(), 1);
        assert_eq!(g.fragments[0].page_index, 0);
    }

    /// Exercises `PaginationLayoutTree`'s `LayoutPartialTree` /
    /// `RoundTree` / `CacheTree` / `TraversePartialTree` impls
    /// at runtime by routing body's layout through
    /// `taffy::compute_root_layout`. Production reaches geometry via
    /// `fragment_pagination_root` directly (see the docstring on
    /// `drive_taffy_root_layout` for why), so this test is the only
    /// runtime user of those trait impls — without it, `cargo build`
    /// would still type-check the impls but no code path would actually
    /// invoke them. Asserts the geometry the Taffy-driven path produces
    /// matches the direct walk used in production.
    ///
    /// Both sides feed the same `ColumnStyleTable` so the parity check
    /// covers the break-style-aware code path that production wires
    /// through `run_pass_with_break_styles`. The fixture sets
    /// `break-before: page` on the middle child so the geometry differs
    /// from the style-unaware case (without the table all three blocks
    /// pack onto page 0; with it, the middle block opens page 1).
    #[test]
    fn taffy_driven_dispatch_matches_direct_walk() {
        let html = r#"
            <html><body>
              <div style="height: 200px"></div>
              <div style="break-before: page; height: 200px"></div>
              <div style="height: 200px"></div>
            </body></html>
        "#;

        let direct_geom = {
            let mut doc = parse(html, 600.0);
            let table = blitz_adapter::extract_column_style_table(&doc);
            super::run_pass_with_break_styles(doc.deref_mut(), 800.0, &table)
        };

        let taffy_geom = {
            let mut doc = parse(html, 600.0);
            let table = blitz_adapter::extract_column_style_table(&doc);
            let mut tree = PaginationLayoutTree::new(doc.deref_mut(), 800.0);
            tree.column_styles = Some(&table);
            tree.drive_taffy_root_layout();
            tree.take_geometry()
        };

        // Sanity: the break-* branch actually fired — page_index 1
        // appears at least once in the direct geometry.
        assert!(
            direct_geom
                .values()
                .flat_map(|g| g.fragments.iter())
                .any(|f| f.page_index == 1),
            "expected break-before: page to push a child onto page 1, got {direct_geom:?}"
        );

        assert_eq!(direct_geom.len(), taffy_geom.len());
        for (id, direct) in &direct_geom {
            let taffy = taffy_geom.get(id).expect("same node id in both passes");
            assert_eq!(direct.fragments, taffy.fragments, "node {id}");
        }
    }

    /// fulgur-kv0r: parallel siblings in a grid / flex parent should
    /// share the same page-local y when they share Taffy's
    /// `layout.location.y`. Pre-fix, `fragment_block_subtree`
    /// advanced `cursor_y` after each child via `cursor_y += child_h`,
    /// so card 2 (Taffy y=0) was recorded at y=200 (= card 1's
    /// height) in geometry. Post-fix the loop reads
    /// `child_page_y = page_start_y + (this_top_in_parent - page_taffy_origin)`
    /// directly from Taffy, and updates `cursor_y` only as a row's
    /// max bottom for break / overflow checks.
    #[test]
    fn fragment_block_subtree_grid_parallel_siblings_share_page_y() {
        // Two cells in a 2-column grid row, each 100px tall and
        // 100px wide so the grid container distinguishes them by x.
        // Pre-fix: card 2 placed at y=100 (cursor-advanced after
        // card 1). Post-fix: card 2 placed at y=0 (Taffy `location.y`).
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div style="height: 100px; width: 100px"></div>
                <div style="height: 100px; width: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0, &table);

        // Filter to fragments whose width is exactly the cell width
        // (100) — that's only the two cards. Grid container has
        // width 200 (two columns), html / body have viewport width.
        let card_y: Vec<f32> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                f.page_index == 0 && (f.height - 100.0).abs() < 0.5 && (f.width - 100.0).abs() < 0.5
            })
            .map(|f| f.y)
            .collect();
        assert_eq!(
            card_y.len(),
            2,
            "expected two grid cells (100×100) on page 0, got {card_y:?}"
        );
        for y in &card_y {
            assert!(
                y.abs() < 0.5,
                "grid parallel siblings must share y=0, got {y} (pre-fix: card 2 at y=100 due \
                 to cursor-advance)",
            );
        }
    }

    /// fulgur-916y: a multicol container with a `column-span: all`
    /// child whose subtree exceeds one page must split across pages
    /// in the partition path. Pre-fix, the multicol gate
    /// (`!is_multicol`) blocked recursion, so the whole multicol
    /// container ended up as a single fragment regardless of
    /// overflow — fragmenter reported 1 page. Post-fix, the gate
    /// admits multicol containers that have a span:all child, so
    /// `fragment_block_subtree` recurses into the span subtree and
    /// splits it across pages via the regular block-flow logic.
    ///
    /// Pins `implied_page_count(geometry) >= 2` for the
    /// `multicol_span_all` integration fixture's HTML rendered with
    /// the fragmenter's strip height set small enough that the
    /// span:all section overflows page 0.
    #[test]
    fn fragment_pagination_root_recurses_into_multicol_with_span_all() {
        let mut long = String::new();
        for _ in 0..40 {
            long.push_str(
                "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                 Sed do eiusmod tempor incididunt ut labore et dolore magna \
                 aliqua. Ut enim ad minim veniam, quis nostrud exercitation.</p>",
            );
        }
        let html = format!(
            r#"<!doctype html><html><head><style>
                body {{ margin: 10pt; font-size: 10pt; }}
                .mc {{ column-count: 2; column-gap: 10pt; }}
                .span {{ column-span: all; }}
            </style></head><body>
              <div class="mc">
                <p>before column content.</p>
                <section class="span">{long}</section>
                <p>after column content.</p>
              </div>
            </body></html>"#,
            long = long
        );

        // 600 viewport, 400 page strip (small enough to overflow).
        let mut doc = parse(&html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0, &table);
        let pages = super::implied_page_count(&geom);
        assert!(
            pages >= 2,
            "expected multicol with span:all overflow to split into >=2 pages, got {pages}",
        );
    }

    /// Devin Review on PR #285 (fulgur-a36m Phase 3.1.5b):
    /// `fragment_block_subtree` had `break-before: page` firing BEFORE
    /// the inter-child gap was folded into `cursor_y`, so the gap was
    /// re-applied AFTER the break-before reset — placing the child at
    /// `y=gap` on the new page instead of `y=0`. The body-level
    /// `fragment_pagination_root` had the correct ordering. This test
    /// pins B's y-coordinate on the new page and would catch the
    /// pre-fix value (gap≈20, was 26.6 in CSS px after Stylo's pt→px).
    ///
    /// Setup: outer wrapper triggers recursion via
    /// `has_forced_break_below`. Inside, A (h=100) at y=0 and B
    /// (h=100) at y=120 with `break-before: page`. The `margin-top:
    /// 20px` on B creates a 20px gap that the bug would leak through.
    #[test]
    fn fragment_block_subtree_break_before_after_gap_places_child_at_y_zero() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div id="outer" style="margin: 0; padding: 0">
                <div id="a" style="height: 100px; margin: 0"></div>
                <div id="b" style="margin-top: 20px; break-before: page; height: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0, &table);

        // Find every fragment with height ≈ 100 on page 1; B is the
        // only such fragment (outer's page-1 fragment height is the
        // total parent strip, which equals 100 after the fix because
        // only B sits on page 1; outer's page-0 fragment carries
        // A + gap = 120; A is on page 0).
        let b_on_page1: Vec<&Fragment> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| f.page_index == 1 && (f.height - 100.0).abs() < 0.5)
            .collect();
        assert!(
            !b_on_page1.is_empty(),
            "expected B fragment on page 1, geom={geom:?}"
        );
        for f in &b_on_page1 {
            assert!(
                f.y.abs() < 0.5,
                "B should land at y=0 on the new page (forced break discards \
                 the inter-child gap), but got y={} (gap leaked through \
                 break-before — see Devin Review on PR #285). frag={f:?}",
                f.y,
            );
        }
    }

    #[test]
    fn implied_page_count_is_one_for_empty_geometry() {
        let geom = PaginationGeometryTable::new();
        assert_eq!(super::implied_page_count(&geom), 1);
    }

    #[test]
    fn implied_page_count_uses_max_index_plus_one() {
        let mut geom = PaginationGeometryTable::new();
        geom.entry(1).or_default().fragments.push(Fragment {
            page_index: 2,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        assert_eq!(super::implied_page_count(&geom), 3);
    }

    /// fulgur-s67g Phase 2.1: a 3-line paragraph that overflows the
    /// page strip after line 2 cannot split between line 2 and the
    /// final line — the second fragment would have only 1 line, below
    /// the widows = 2 minimum. Spike emits the paragraph whole.
    #[test]
    fn widow_minimum_blocks_single_line_tail_fragment() {
        let mut geom = PaginationGeometryTable::new();
        // Each line 75px; cumulative bottoms at 75, 150, 225.
        // Page strip = 200, so naturally we'd split at line 2 (bottom
        // 225 > 200), leaving 1 line in the tail — widow violated.
        let lines = vec![(0.0, 75.0), (75.0, 150.0), (150.0, 225.0)];
        let (new_page, new_cursor, emitted) = super::fragment_inline_root(
            &mut geom, /*child_id=*/ 1, /*paragraph_x=*/ 0.0, /*width=*/ 100.0,
            /*initial_cursor_y=*/ 0.0, /*initial_page_index=*/ 0,
            /*page_height_px=*/ 200.0, &lines,
        );
        assert_eq!(emitted, 1, "widow violation → single oversized fragment");
        assert_eq!(new_page, 0);
        assert!(
            (new_cursor - 225.0).abs() < 0.01,
            "cursor advances by full paragraph height, got {new_cursor}",
        );
        let frags = &geom.get(&1).unwrap().fragments;
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].page_index, 0);
    }

    /// fulgur-s67g Phase 2.1: a 4-line paragraph splittable at line 2
    /// (first 2 lines on page 0, last 2 on page 1) honours both
    /// orphans = 2 and widows = 2.
    #[test]
    fn widow_orphan_minimum_allows_balanced_split() {
        let mut geom = PaginationGeometryTable::new();
        // Each line 75px; bottoms at 75, 150, 225, 300.
        // Page strip = 200 → natural split at line 2 (bottom 225 > 200).
        // first_size = 2 ≥ orphans, remaining_size = 2 ≥ widows. Split OK.
        let lines = vec![(0.0, 75.0), (75.0, 150.0), (150.0, 225.0), (225.0, 300.0)];
        let (new_page, new_cursor, emitted) =
            super::fragment_inline_root(&mut geom, 1, 0.0, 100.0, 0.0, 0, 200.0, &lines);
        assert_eq!(emitted, 2, "valid split → 2 fragments");
        assert_eq!(new_page, 1);
        let frags = &geom.get(&1).unwrap().fragments;
        assert_eq!(frags.len(), 2);
        assert_eq!(frags[0].page_index, 0);
        assert_eq!(frags[1].page_index, 1);
        // First fragment: lines 0-1 (height = 150).
        assert!((frags[0].height - 150.0).abs() < 0.01);
        // Second fragment: lines 2-3 (height = 150 in para-local).
        assert!((frags[1].height - 150.0).abs() < 0.01);
        // cursor_y on page 1 = paragraph_top_in_body (0.0) + 150 = 150.
        assert!((new_cursor - 150.0).abs() < 0.01);
    }

    /// fulgur-s67g Phase 2.1: orphan violation. A 3-line paragraph
    /// with overflow at line 1 (only line 0 fits) would put just 1
    /// line in the first fragment — below orphans = 2. No split; emit
    /// whole.
    #[test]
    fn orphan_minimum_blocks_single_line_head_fragment() {
        let mut geom = PaginationGeometryTable::new();
        // Lines 75px; bottoms at 75, 150, 225.
        // Page strip = 100 → natural split at line 1 (bottom 150 > 100).
        // first_size = 1 < orphans=2. Don't split.
        let lines = vec![(0.0, 75.0), (75.0, 150.0), (150.0, 225.0)];
        let (new_page, _new_cursor, emitted) =
            super::fragment_inline_root(&mut geom, 1, 0.0, 100.0, 0.0, 0, 100.0, &lines);
        assert_eq!(emitted, 1, "orphan violation → single oversized fragment");
        assert_eq!(new_page, 0);
        let frags = &geom.get(&1).unwrap().fragments;
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].page_index, 0);
        assert!((frags[0].height - 225.0).abs() < 0.01);
    }

    #[test]
    fn taller_than_page_block_emits_single_oversize_fragment() {
        // 1000px block on a 800px page. Block-only fragmenter emits it whole
        // on the page where its top lands, with the full height — true
        // split is the next iteration's job.
        let html = r#"
            <html><body>
              <div style="height: 1000px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        // Phase 2.3 fix: body + 1 oversized child = 2 entries.
        assert_eq!(table.len(), 2);
        // The oversized child is the entry whose height ≈ 1000.
        let oversize = table
            .values()
            .find(|g| (g.fragments[0].height - 1000.0).abs() < 1.0)
            .expect("oversized child fragment");
        assert_eq!(oversize.fragments.len(), 1);
        assert_eq!(oversize.fragments[0].page_index, 0);
    }
}
