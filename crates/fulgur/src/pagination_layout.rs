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

use crate::units::F32Units;
use blitz_dom::BaseDocument;
use std::collections::{BTreeMap, BTreeSet};
use taffy::{
    AvailableSpace, CacheTree, LayoutPartialTree, NodeId, RoundTree, Size, TraversePartialTree,
    TraverseTree,
};

/// One placement slot recorded per (source node × page).
///
/// `x`, `y`, `width`, `height` are type-enforced CSS pixels
/// ([`crate::units::Px`]) — Taffy's native unit — and `y` is measured from
/// the page's content-box top. The convert / draw layer is responsible for
/// `px_to_pt` conversion ([`crate::units::Px::in_pt`]) before reaching
/// Krilla.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    pub page_index: u32,
    pub x: crate::units::Px,
    pub y: crate::units::Px,
    pub width: crate::units::Px,
    pub height: crate::units::Px,
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
    page_height_px: crate::units::Px,
    column_styles: &'a crate::column_css::ColumnStyleTable,
) -> PaginationGeometryTable {
    run_pass_inner(doc, page_height_px.to_f32(), Some(column_styles), None)
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
        // fulgur-uebl: production `run_pass_inner` populates
        // `used_page_names` once `column_styles` is available; the
        // test-only Taffy parity path still needs the same table or
        // any future fixture using `page:` would silently skip the
        // implicit-break logic. Lazy-fill so call sites that want the
        // baseline behaviour can still leave both `None`.
        if self.used_page_names.is_none() {
            self.used_page_names = self
                .column_styles
                .map(|cs| crate::blitz_adapter::compute_used_page_names(self.doc, cs));
        }
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
                x: body_x.as_px(),
                y: 0.0_f32.as_px(),
                width: body_w.as_px(),
                height: body_layout.size.height.as_px(),
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
                            x: (body_x + layout.location.x).as_px(),
                            y: cursor_y.as_px(),
                            width: 0.0_f32.as_px(),
                            height: 0.0_f32.as_px(),
                        });
                    emitted += 1;
                }
                continue;
            }
            let layout = child.final_layout;
            // fulgur-2m6w: defend against a non-finite Taffy height
            // (`+inf` / `NaN`). A `NaN` height slips past the
            // `child_h > page_height_px + 1.0` slice gate (NaN compares
            // false) into the normal path where `cursor_y += child_h`
            // would poison every later page advance; `+inf` would corrupt
            // `prev_bottom_y_in_body`. Treat either as zero height — the
            // node still enters geometry via the `child_h <= 0.0` branch
            // so its counter / bookmark markers survive.
            let child_h = if layout.size.height.is_finite() {
                layout.size.height
            } else {
                0.0
            };
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
                            x: (body_x + layout.location.x).as_px(),
                            y: cursor_y.as_px(),
                            width: child_w.as_px(),
                            height: 0.0_f32.as_px(),
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
            let explicit_break_before = matches!(
                break_props.break_before,
                Some(crate::draw_primitives::BreakBefore::Page)
            );
            let page_filling_break_child = gap > 0.0
                && child_h >= self.page_height_px * 0.9
                && gap + child_h <= self.page_height_px + 0.5;
            if (explicit_break_before || page_name_changed) && emitted > 0 && cursor_y > 0.0 {
                page_index += 1;
                cursor_y = if explicit_break_before && page_filling_break_child {
                    gap
                } else {
                    0.0
                };
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
            let frag_x = body_x + layout.location.x;

            // fulgur-sbw2: a child whose CSS-resolved height alone
            // exceeds `page_height_px` (e.g. `<div height:300vh>`)
            // must be sliced across pages. The recursion gate above
            // returns false for this shape because the child's *own*
            // children fit the strip (the gate measures descendant
            // overflow, not the parent's intrinsic height), so we
            // would otherwise emit a single oversized fragment on the
            // current page and stop. Emit one fragment per page strip
            // with page-local y — `PaginationGeometry::is_split()`
            // flips to `true` automatically once `fragments.len() > 1`,
            // and render.rs picks the per-slice height accordingly.
            // Descendants are recorded against the *first* slice only;
            // exact mid-element pagination of nested content is still
            // future work (see `record_subtree_descendants` notes).
            //
            // Tolerance: Stylo / Taffy round CSS-resolved `<length>`
            // values to integer CSS pixels in some cases — a 220pt
            // spacer (= 293.333… px) is reported back as `child_h =
            // 294` while `page_height_px` is computed without that
            // round-trip (= 293.33334). The literal `>` then trips
            // by ~0.67 px and the spacer is wrongly sliced into two
            // pages (gcpm_snapshot tests regress: spacer + page-break
            // pair becomes 2 + 1 instead of 1 + 1 per section). One
            // CSS pixel of slack absorbs the quantization without
            // letting truly oversized content (`300vh ≈ 880 px on a
            // 293-px strip`) slip through.
            //
            // Atomic children opt out: a transformed subtree paints
            // as a single atomic box (CSS Transforms §6.1) — it must
            // never split across pages because the rotation /
            // skew / matrix is applied to a single shape, not
            // per-slice. `contain: size` is NOT excluded: a
            // `<div contain:size height:350vh>` still spans four
            // pages visually (WPT `monolithic-overflow-022-print`),
            // it's only the descendant content that's atomic.
            let has_transform = child
                .primary_styles()
                .is_some_and(|s| !s.get_box().transform.0.is_empty());
            if !has_transform && child_h > self.page_height_px + 1.0 {
                let first_slice_h = (self.page_height_px - cursor_y).min(child_h);
                self.geometry
                    .entry(child_id)
                    .or_default()
                    .fragments
                    .push(Fragment {
                        page_index,
                        x: frag_x.as_px(),
                        y: cursor_y.as_px(),
                        width: child_w.as_px(),
                        height: first_slice_h.as_px(),
                    });
                record_subtree_descendants(
                    &mut self.geometry,
                    self.doc,
                    child_id,
                    page_index,
                    cursor_y,
                    frag_x,
                    0,
                );
                let mut remaining = child_h - first_slice_h;
                let mut last_slice_h = first_slice_h;
                // fulgur-ezst: a CHILDLESS block whose slicing would exceed
                // the cap is a pathological amplifier — `<div
                // style="height:99999999px">` is a web-only spacer/overflow
                // idiom that prints nothing but blank pages. Collapse it to
                // its single first slice: emit only that slice AND bound the
                // space it occupies (resume following content on the next
                // page) so the document does not balloon. Background / border
                // presence does NOT gate this — nobody authors a
                // >MAX_PAGES-tall filled band on purpose. A content-bearing
                // block, or a childless band that fits within the cap, is not
                // collapsed and takes the truncate-and-warn path below
                // unchanged.
                //
                // fulgur-c8re (security): a replaced element (`<img>` /
                // `<svg>`) is NOT special-cased out of this collapse —
                // painting or not. It only reaches this branch when it is
                // taller than `MAX_PAGES` pages (~10M px), a range no
                // legitimate single image occupies, so clipping it to one page
                // loses nothing real — even a *resolved* image, because a 1×1
                // bitmap stretched with `height:99999999px` is the same
                // amplifier as an unresolved `src`. Gating on tag name alone
                // (the removed `is_replaced_content`) let such a node amplify a
                // few bytes of HTML into ~`MAX_PAGES` blank pages (a validated
                // high-severity DoS).
                let collapse_childless = self.page_height_px > 0.0
                    && (remaining / self.page_height_px).ceil() > crate::MAX_PAGES as f32
                    && !subtree_has_rendered_content(self.doc, child_id, 0);
                if collapse_childless {
                    // fulgur-c8re (Codex P1 on PR #575): bound the OCCUPIED
                    // SPACE, not just the fragment pushes. `implied_page_count`
                    // reads the max fragment index across ALL nodes, so if the
                    // slice loop advanced `page_index` to `MAX_PAGES` a trailing
                    // in-flow sibling (`<div huge></div><p>after</p>`) would be
                    // stranded on a deep page and re-inflate the PDF to
                    // ~`MAX_PAGES` blank pages. The first slice already emitted
                    // above fills the current strip, so resume following content
                    // on the next page.
                    log::warn!(
                        "pagination: collapsed a childless block of height \
                         {child_h}px (slicing would exceed the {}-page limit) \
                         to a single page (fulgur-ezst)",
                        crate::MAX_PAGES,
                    );
                    page_index += 1;
                    cursor_y = 0.0;
                } else {
                    // fulgur-2m6w: cap the per-page-strip slicing at
                    // `MAX_PAGES`. `child_h` is attacker-controlled CSS
                    // (`height` / `vh`), so without this bound a few bytes of
                    // HTML (`<div style="height:99999999px">`) generate ~10^5
                    // fragments — and a non-finite height would never reduce
                    // `remaining`, looping forever. The `page_index` ceiling is
                    // load-bearing on its own (it stops the loop even when
                    // `remaining` is `+inf`); content past the cap is truncated.
                    while remaining > 0.0 && page_index < crate::MAX_PAGES {
                        page_index += 1;
                        last_slice_h = remaining.min(self.page_height_px);
                        self.geometry
                            .entry(child_id)
                            .or_default()
                            .fragments
                            .push(Fragment {
                                page_index,
                                x: frag_x.as_px(),
                                y: 0.0_f32.as_px(),
                                width: child_w.as_px(),
                                height: last_slice_h.as_px(),
                            });
                        remaining -= last_slice_h;
                    }
                    if remaining > 0.0 {
                        log::warn!(
                            "pagination: block height {child_h}px exceeds the \
                             {}-page limit; truncating remaining content to \
                             bound rendering (fulgur-2m6w)",
                            crate::MAX_PAGES,
                        );
                    }
                    cursor_y = if child_h - first_slice_h > 0.0 {
                        last_slice_h
                    } else {
                        cursor_y + first_slice_h
                    };
                }
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

            let frag = Fragment {
                page_index,
                x: frag_x.as_px(),
                y: cursor_y.as_px(),
                width: child_w.as_px(),
                height: child_h.as_px(),
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
                frag_x,
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

/// fulgur-ezst: true if `parent_id`'s subtree renders any VISIBLE box (a
/// descendant with positive area — both width and height > 0). Used to
/// classify a pathologically tall block as "childless" (collapsible): the
/// collapse is safe only when nothing visible would be lost.
///
/// Walks children like `record_subtree_descendants` (the `layout_children`
/// preference, short-circuiting on the first hit), but the emptiness test
/// differs deliberately: a descendant with zero AREA (EITHER dimension
/// <= 0) paints nothing, so it does not count as content — instead we
/// recurse into it, because with `overflow: visible` a zero-height /
/// zero-width box can still host visible descendants that overflow it.
/// (This is why the test is `||`, not `&&`: an empty block child lays out
/// width>0 / height==0, and `&&` would wrongly count it as content and
/// disable the collapse — a trivial DoS bypass,
/// `<div style="height:99999999px"><div></div></div>`; Codex P2 on PR #553.)
/// `record_subtree_descendants` still *records* such zero-area boxes (their
/// node ids may carry counters / ids), which is a separate question from
/// "does this paint anything". Conservative on depth: a subtree deeper than
/// `MAX_DOM_DEPTH` reads as no content, matching `record_subtree_descendants`
/// (which also stops recording there, so such content renders blank anyway).
fn subtree_has_rendered_content(doc: &BaseDocument, parent_id: usize, depth: usize) -> bool {
    if depth >= crate::MAX_DOM_DEPTH {
        return false;
    }
    let Some(parent) = doc.get_node(parent_id) else {
        return false;
    };
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
        // `visibility: hidden` / `collapse` occupies layout space but paints
        // nothing (conversion skips its paint), so it does not count as
        // content — a `<div style="visibility:hidden">` must not defeat the
        // collapse (Codex P2 on PR #553).
        let invisible = {
            use style::properties::longhands::visibility::computed_value::T as Visibility;
            child
                .primary_styles()
                .is_some_and(|s| s.clone_visibility() != Visibility::Visible)
        };
        if invisible || layout.size.height <= 0.0 || layout.size.width <= 0.0 {
            // An invisible or zero-area box paints nothing itself, but may
            // host visible descendants — overflowing it (overflow: visible)
            // or flipping `visibility` back to `visible` — so recurse.
            if subtree_has_rendered_content(doc, child_id, depth + 1) {
                return true;
            }
            continue;
        }
        return true;
    }
    false
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
                x: child_x.as_px(),
                y: child_y.as_px(),
                width: w.as_px(),
                height: h.as_px(),
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
    // fulgur-yb27: walk `layout_children` so anonymous block wrappers
    // Stylo synthesizes around inline-level siblings (CSS 2.1
    // §9.2.1.1) participate in the cumulative-overflow simulation.
    // Mirrors the `fragment_block_subtree` walker switch above —
    // without this, a block whose tail anon block wrapper would
    // overflow is missed by the preflight, the recursion gate
    // returns false, and the parent falls back to a single
    // oversize fragment.
    let layout_children_borrow = parent.layout_children.borrow();
    let walk_children: Vec<usize> = layout_children_borrow
        .as_deref()
        .filter(|v| !v.is_empty())
        .map(|v| v.to_vec())
        .unwrap_or_else(|| parent.children.clone());
    drop(layout_children_borrow);
    for &child_id in &walk_children {
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
        // Floats are out of normal flow (CSS 2.1 §9.5) — match
        // `fragment_pagination_root` / `fragment_block_subtree` which
        // skip them from `prev_used_page` comparisons. Without this,
        // a float-only page-name change would force recursion through
        // a subtree the real comparison would treat as unchanged.
        if crate::blitz_adapter::node_is_floating(child) {
            continue;
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
/// Row-level state for grid/flex parallel-sibling co-split (fulgur-ysms).
///
/// Saved once at the first cell of each row; subsequent cells in the same
/// row restore from `start_*` so their recursion begins at the same
/// (page, cursor) as the first cell did. After all cells in the row are
/// processed the outer cursor advances to `max_end_*`.
struct RowState {
    start_page: u32,
    start_cursor_y: f32,
    start_page_start_y: f32,
    start_page_taffy_origin: f32,
    max_end_page: u32,
    max_end_cursor_y: f32,
    /// Taffy `location.y` of the first cell in this row (reserved for future use).
    _row_top: f32,
    /// Max `location.y + height` seen across cells in this row.
    row_bottom: f32,
    /// Pages for which a parent fragment has already been emitted this row,
    /// to avoid duplicate entries when multiple cells cross the same boundary.
    emitted_parent_pages: BTreeSet<u32>,
    /// True when at least one cell in this row has crossed a page boundary via
    /// recursion. Only in this case do subsequent same-row cells need the full
    /// state-restore (co-split); non-recursion page advances are already handled
    /// by the existing `origin_pending_same_row` mechanism.
    crossed_by_recursion: bool,
}

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
                x: parent_x_in_body.as_px(),
                y: cursor_in.as_px(),
                width: parent_w.as_px(),
                height: h.as_px(),
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
    let mut page_taffy_origin: f32 = 0.0;
    let mut origin_pending_target_y: Option<f32> = None;
    let mut origin_pending_same_row: Option<(f32, f32, f32)> = None;
    // fulgur-uebl: tracks the previous in-flow sibling's used page-name
    // for implicit forced-break detection; see `fragment_pagination_root`
    // for the rationale and the outer-Option semantics.
    let mut prev_used_page: Option<Option<String>> = None;
    let mut row_state: Option<RowState> = None;
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
    let allow_same_row_rebase = crate::blitz_adapter::is_flex_or_grid_container_node(parent);
    let suppress_page_check = allow_same_row_rebase
        || crate::blitz_adapter::is_atomic_inline_container_node(parent)
        || parent_is_orthogonal;

    // fulgur-yb27: prefer `layout_children` over raw `children` —
    // same rationale as `record_subtree_descendants` and
    // `fragment_pagination_root`. When a block container has mixed
    // block-level and inline-level children, Stylo synthesizes
    // anonymous block wrappers around the inline-level siblings (CSS
    // 2.1 §9.2.1.1). Those wrappers carry their own Taffy layout and
    // `node_id`, but they live ONLY in `layout_children`. Walking
    // raw `children` would re-visit the underlying inline runs
    // without their Taffy layout (so they'd land at the parent's
    // origin instead of after their predecessor block) and skip the
    // per-anon-block break decision the body-level walker honours
    // since fulgur-bq6i.
    //
    // Cross-page recursion correctness depends on fulgur-oc51's
    // parent-fragment push above — flipping this walk to
    // `layout_children` without that fix would lose the parent's
    // pre-recursion-page fragment in mo-006/008 (flex/grid + tall
    // monolithic + trailing inline text).
    let layout_children_borrow = parent.layout_children.borrow();
    let walk_children: Vec<usize> = layout_children_borrow
        .as_deref()
        .filter(|v| !v.is_empty())
        .map(|v| v.to_vec())
        .unwrap_or_else(|| parent.children.clone());
    drop(layout_children_borrow);
    for &child_id in &walk_children {
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
        // fulgur-2m6w: same non-finite guard as `fragment_pagination_root`.
        // A nested child with a non-finite Taffy height (`+inf` / `NaN`, or
        // an `f32::MAX` height that overflows to `+inf` after one
        // `cursor_y += child_h`) would otherwise poison `cursor_y` /
        // `page_start_y` for the parent and every following sibling. Treat
        // it as zero height so it falls into the `child_h <= 0.0` branch.
        let child_h = if layout.size.height.is_finite() {
            layout.size.height
        } else {
            0.0
        };
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

        // fulgur-ysms: row-level co-split for flex/grid containers.
        // Must run BEFORE origin_pending_target_y is consumed so that
        // restoring page_taffy_origin is consistent.
        if allow_same_row_rebase {
            let same_row = row_state
                .as_ref()
                .map(|rs| this_top_in_parent < rs.row_bottom - 0.5);
            match same_row {
                Some(true) => {
                    let rs = row_state.as_ref().unwrap();
                    if rs.crossed_by_recursion {
                        // Co-split: a previous cell in this row crossed a page
                        // boundary via recursion. Restore to the row-start state
                        // so this cell's recursion begins at the same (page,
                        // cursor) — both cells will independently fragment and
                        // their results are merged via max_end_*.
                        //
                        // Also clear origin_pending_* set by the previous cell's
                        // recursion: the restored page_taffy_origin is the one
                        // that puts THIS cell at the correct row-start y, and the
                        // previous cell's pending origin would incorrectly shift
                        // page_taffy_origin again.
                        page_index = rs.start_page;
                        cursor_y = rs.start_cursor_y;
                        page_start_y = rs.start_page_start_y;
                        page_taffy_origin = rs.start_page_taffy_origin;
                        origin_pending_target_y = None;
                        origin_pending_same_row = None;
                    }
                    // If crossed_by_recursion is false (e.g., a previous cell
                    // crossed via the non-recursion strip-overflow path), the
                    // existing origin_pending_same_row mechanism already handles
                    // correct placement — no additional restore is needed.
                }
                Some(false) => {
                    // New row: advance outer cursor to the max end reached
                    // across all cells in the previous row.
                    let rs = row_state.take().unwrap();
                    page_index = rs.max_end_page;
                    cursor_y = rs.max_end_cursor_y;
                    if rs.max_end_page > rs.start_page {
                        page_start_y = 0.0;
                        origin_pending_target_y = Some(rs.max_end_cursor_y);
                        origin_pending_same_row = None;
                    } else {
                        page_start_y = rs.start_page_start_y;
                    }
                }
                None => {}
            }
            if row_state.is_none() {
                // First cell in a new row: snapshot current state.
                row_state = Some(RowState {
                    start_page: page_index,
                    start_cursor_y: cursor_y,
                    start_page_start_y: page_start_y,
                    start_page_taffy_origin: page_taffy_origin,
                    max_end_page: page_index,
                    max_end_cursor_y: cursor_y,
                    _row_top: this_top_in_parent,
                    row_bottom: this_top_in_parent + child_h,
                    emitted_parent_pages: BTreeSet::new(),
                    crossed_by_recursion: false,
                });
            } else if let Some(ref mut rs) = row_state {
                rs.row_bottom = rs.row_bottom.max(this_top_in_parent + child_h);
            }
        }

        if let Some(mut target_y) = origin_pending_target_y.take() {
            if let Some((row_top, row_bottom, same_row_y)) = origin_pending_same_row.take()
                && this_top_in_parent < row_bottom - 0.5
            {
                target_y = same_row_y + (this_top_in_parent - row_top);
            }
            page_taffy_origin = this_top_in_parent - (target_y - page_start_y);
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
                        x: parent_x_in_body.as_px(),
                        y: page_start_y.as_px(),
                        width: parent_w.as_px(),
                        height: (cursor_y - page_start_y).as_px(),
                    });
                page_index += 1;
                cursor_y = 0.0;
                page_start_y = 0.0;
                // Zero-height break-before: this child IS the first
                // on the new page — apply origin rebase eagerly.
                page_taffy_origin = this_top_in_parent;
                (origin_pending_target_y, origin_pending_same_row) = (None, None);
            }
            if child.element_data().is_some() {
                geometry
                    .entry(child_id)
                    .or_default()
                    .fragments
                    .push(Fragment {
                        page_index,
                        x: (parent_x_in_body + layout.location.x).as_px(),
                        y: cursor_y.as_px(),
                        width: child_w.as_px(),
                        height: 0.0_f32.as_px(),
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
                        x: parent_x_in_body.as_px(),
                        y: page_start_y.as_px(),
                        width: parent_w.as_px(),
                        height: (cursor_y - page_start_y).as_px(),
                    });
                page_index += 1;
                cursor_y = 0.0;
                page_start_y = 0.0;
                // Zero-height break-after: NEXT child is the first
                // on the new page — defer origin rebase.
                (origin_pending_target_y, origin_pending_same_row) = (Some(page_start_y), None);
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
                    x: parent_x_in_body.as_px(),
                    y: page_start_y.as_px(),
                    width: parent_w.as_px(),
                    height: (cursor_y - page_start_y).as_px(),
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
            let pre_recursion_cursor_y = cursor_y;
            // fulgur-u0p0: enter recursion from `child_page_y` (the rebased
            // page-local y of THIS child) instead of the parent's running
            // `cursor_y`. For block flow, `child_page_y == cursor_y` after
            // the cursor_y update above, so this is a no-op. For grid / flex
            // parallel siblings (same Taffy `location.y` as a previous
            // sibling on this page), `cursor_y` still holds the previous
            // sibling's bottom — passing it would stack the parallel cell
            // below the previous one instead of beside it. Using
            // `child_page_y` keeps each cell's recursion strip aligned to
            // the row's y on the current page.
            let (np, nc) = fragment_block_subtree(
                geometry,
                doc,
                column_styles,
                used_page_names,
                child_id,
                child_w,
                child_x_in_body,
                page_index,
                child_page_y,
                page_height_px,
                depth + 1,
            );
            page_index = np;
            // fulgur-u0p0: when the recursion stayed on the same page,
            // keep the larger of the parent's existing `cursor_y` (row max
            // bottom from a previous parallel sibling) and the recursion's
            // returned `nc`. When the recursion crossed pages, the previous
            // page's `cursor_y` is stale — adopt `nc` directly.
            cursor_y = if np == pre_recursion_page {
                cursor_y.max(nc)
            } else {
                nc
            };
            // If the recursion crossed a boundary, the parent's
            // current-page fragment must restart at y=0 on the new
            // page. Defensive `nc < page_start_y` guards against
            // backward cursor returns (impossible in normal flow).
            if page_index != pre_recursion_page || nc < page_start_y {
                // fulgur-oc51: emit parent fragments for every
                // page span the recursion crossed. Without this,
                // only the trailing close at the end of this
                // function emits a parent fragment (on the *last*
                // page), so the parent's background / borders
                // disappear from the previous and intermediate
                // pages. Pre-recursion overflow close (no-
                // recursion branch, line ~1713) covers the
                // analogous case for non-recursive children, but
                // there is no equivalent push when the page
                // advance happens *inside* the recursion.
                //
                // The parent's content extended to the page
                // bottom on every previous page (otherwise the
                // recursion would not have advanced past that
                // page), so the previous-page fragment spans
                // `[page_start_y, page_height_px]` and any
                // intermediate page is a full strip.
                if page_index > pre_recursion_page {
                    // Match the no-recursion strip-overflow shape
                    // (line ~1713): the parent fragment's height is
                    // its accumulated logical content on the
                    // previous page, treated as if the splitting
                    // child had been laid out in full (without
                    // fragmentation). The renderer clips the
                    // overflow visually. Using the visible page
                    // strip (`page_height_px - page_start_y`)
                    // instead would stop short of the page's
                    // margin-area paint that adjacent passing tests
                    // (mo-006/008) expect.
                    let logical_height = (pre_recursion_cursor_y + child_h - page_start_y).max(0.0);
                    let prev_height = logical_height.max((page_height_px - page_start_y).max(0.0));
                    if prev_height > 0.0 {
                        let should_emit = row_state
                            .as_mut()
                            .map(|rs| rs.emitted_parent_pages.insert(pre_recursion_page))
                            .unwrap_or(true);
                        if should_emit {
                            geometry
                                .entry(parent_id)
                                .or_default()
                                .fragments
                                .push(Fragment {
                                    page_index: pre_recursion_page,
                                    x: parent_x_in_body.as_px(),
                                    y: page_start_y.as_px(),
                                    width: parent_w.as_px(),
                                    height: prev_height.as_px(),
                                });
                        }
                    }
                    for p in (pre_recursion_page + 1)..page_index {
                        let should_emit = row_state
                            .as_mut()
                            .map(|rs| rs.emitted_parent_pages.insert(p))
                            .unwrap_or(true);
                        if should_emit {
                            geometry
                                .entry(parent_id)
                                .or_default()
                                .fragments
                                .push(Fragment {
                                    page_index: p,
                                    x: parent_x_in_body.as_px(),
                                    y: 0.0_f32.as_px(),
                                    width: parent_w.as_px(),
                                    height: page_height_px.as_px(),
                                });
                        }
                    }
                }
                page_start_y = 0.0;
                origin_pending_target_y = Some(cursor_y);
                let row_top = this_top_in_parent;
                let row_bottom = row_top + child_h;
                origin_pending_same_row =
                    allow_same_row_rebase.then_some((row_top, row_bottom, 0.0));
                // fulgur-ysms: mark that this row had a recursion-driven page
                // cross so subsequent same-row cells know to co-split.
                if let Some(ref mut rs) = row_state {
                    rs.crossed_by_recursion = true;
                }
            }

            // Honour `break-after: page` after recursion.
            if break_after_page {
                geometry
                    .entry(parent_id)
                    .or_default()
                    .fragments
                    .push(Fragment {
                        page_index,
                        x: parent_x_in_body.as_px(),
                        y: page_start_y.as_px(),
                        width: parent_w.as_px(),
                        height: (cursor_y - page_start_y).as_px(),
                    });
                page_index += 1;
                cursor_y = 0.0;
                page_start_y = 0.0;
                (origin_pending_target_y, origin_pending_same_row) = (Some(page_start_y), None);
            }
            if !is_float {
                prev_used_page = Some(used_end.clone());
            }
            if let Some(ref mut rs) = row_state {
                if page_index > rs.max_end_page
                    || (page_index == rs.max_end_page && cursor_y > rs.max_end_cursor_y)
                {
                    rs.max_end_page = page_index;
                    rs.max_end_cursor_y = cursor_y;
                }
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
            let should_emit = row_state
                .as_mut()
                .map(|rs| rs.emitted_parent_pages.insert(page_index))
                .unwrap_or(true);
            if should_emit {
                geometry
                    .entry(parent_id)
                    .or_default()
                    .fragments
                    .push(Fragment {
                        page_index,
                        x: parent_x_in_body.as_px(),
                        y: page_start_y.as_px(),
                        width: parent_w.as_px(),
                        height: (cursor_y - page_start_y).as_px(),
                    });
            }
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
                x: child_x_in_body.as_px(),
                y: child_page_y.as_px(),
                width: child_w.as_px(),
                height: child_h.as_px(),
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
                    x: parent_x_in_body.as_px(),
                    y: page_start_y.as_px(),
                    width: parent_w.as_px(),
                    height: (cursor_y - page_start_y).as_px(),
                });
            page_index += 1;
            cursor_y = 0.0;
            page_start_y = 0.0;
            (origin_pending_target_y, origin_pending_same_row) = (Some(page_start_y), None);
        }
        if !is_float {
            prev_used_page = Some(used_end.clone());
        }
        if let Some(ref mut rs) = row_state {
            if page_index > rs.max_end_page
                || (page_index == rs.max_end_page && cursor_y > rs.max_end_cursor_y)
            {
                rs.max_end_page = page_index;
                rs.max_end_cursor_y = cursor_y;
            }
        }
    }

    // fulgur-ysms: finalize any open row — advance to the max end state
    // reached across all parallel sibling cells.
    if let Some(rs) = row_state.take() {
        page_index = rs.max_end_page;
        cursor_y = rs.max_end_cursor_y;
        if rs.max_end_page > rs.start_page {
            page_start_y = 0.0;
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
            x: parent_x_in_body.as_px(),
            y: page_start_y.as_px(),
            width: parent_w.as_px(),
            height: (cursor_y - page_start_y).as_px(),
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
                x: paragraph_x.as_px(),
                y: paragraph_top_in_body.as_px(),
                width: width.as_px(),
                height: frag_h.as_px(),
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
        x: paragraph_x.as_px(),
        y: paragraph_top_in_body.as_px(),
        width: width.as_px(),
        height: frag_h.as_px(),
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
    viewport_w_px: f32,
    viewport_h_px: f32,
) {
    use ::style::properties::longhands::position::computed_value::T as Pos;

    let pages = total_pages.max(1);
    let body_offset_xy = body_origin_in_px(doc);
    let mut fixed_ids: Vec<usize> = Vec::new();
    let root_id = doc.root_element().id;
    walk_for_position_fixed(doc, root_id, &mut fixed_ids, 0);

    // Aggregate budget across all fixed roots and their descendants: each
    // node emits one fragment per page, so the retained total is
    // O((roots + descendants) × pages). `pages` is bounded by MAX_PAGES, but
    // the node axis is not — cap the total to keep the pass bounded on
    // untrusted input (`crate::MAX_SUBTREE_PAGE_FRAGMENTS`).
    let mut emitted: usize = 0;

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
        // fulgur-a8m5: Taffy's `compute_root_layout` (used by
        // `relayout_position_fixed`) does not resolve `bottom` / `right`
        // insets when the absolute element is the root of a layout
        // subtree — it places the element at (0, 0) regardless. The
        // CSS 2.1 §9.4 viewport CB needs explicit inset resolution
        // here, otherwise WPT fixedpos-001/002/008 render `bottom: 0`
        // fixed elements at the top of every page.
        let (resolved_x, resolved_y) =
            resolve_viewport_cb_location(node, w, h, viewport_w_px, viewport_h_px)
                .unwrap_or((layout.location.x, layout.location.y));
        // Render adds `body_offset_pt.y` to every fragment's y to
        // account for the html→body offset (collapsed margins from
        // in-flow body-direct children, etc.). Fixed elements are
        // viewport-anchored, not body-anchored, so subtract that
        // offset here so the dispatch path produces a viewport-
        // relative y in PDF coordinates. Without this compensation,
        // documents that mix in-flow content with `position: fixed`
        // (WPT fixedpos-008) shift the fixed text by the in-flow
        // div's margin-top.
        let (x, y) = (resolved_x - body_offset_xy.0, resolved_y - body_offset_xy.1);

        let entry = geometry.entry(id).or_default();
        // Replace any prior placements (e.g. if the fixed element was
        // also walked by `fragment_pagination_root` and emitted as a
        // single fragment). Per-page repetition is the canonical
        // representation for fixed content.
        entry.fragments.clear();
        entry.is_repeat = true;
        for page_index in 0..pages {
            if emitted >= crate::MAX_SUBTREE_PAGE_FRAGMENTS {
                break;
            }
            entry.fragments.push(Fragment {
                page_index,
                x: x.as_px(),
                y: y.as_px(),
                width: w.as_px(),
                height: h.as_px(),
            });
            emitted += 1;
        }

        // fulgur-4m16: emit per-page repeated fragments for every
        // in-flow descendant of the fixed root. v2 dispatch is
        // geometry-driven and reads fragments per `node_id`, so a
        // fixed root with a sized block-element child (e.g. WPT
        // fixedpos-009 `<div style="position:fixed; bottom:0; right:0">
        // <div class="pencil" style="width:36px; height:36px;
        // background:black; mask-image:..."></div></div>`) needs an
        // entry for the pencil child or v2 never reaches it. The
        // existing root-only fragment carries inline text rendering
        // (fixedpos-001 / 008 ref pattern) but not block descendants.
        // Always walk the subtree (even once the budget is spent): the walk
        // clears every descendant's entry, which is required because this
        // fixed pass runs a second time after the absolute pass and must not
        // leave stale fragments from the first pass behind (Codex review). The
        // per-node push is itself bounded by the `emitted` budget.
        record_fixed_subtree_descendants(geometry, doc, id, (x, y), pages, &mut emitted);
    }

    // Don't allocate empty entries for nodes without fragments.
    geometry.retain(|_, geom| !geom.fragments.is_empty());
}

/// fulgur-4m16: walk every in-flow descendant of a `position: fixed`
/// root and emit one fragment per page (`is_repeat = true`) at the
/// descendant's offset within the fixed subtree, anchored to the
/// root's already-resolved viewport-CB position.
///
/// Mirrors [`record_subtree_fragments_at_offset`] (used by
/// `append_position_absolute_body_direct_fragments`) except:
///   - fragments repeat on every page instead of landing on a single
///     y-derived page,
///   - the caller passes the root's body-relative stored (x, y) so we
///     don't re-resolve viewport-CB here (the root's `final_layout`
///     can lack end-side inset resolution — see fulgur-a8m5),
///   - body-offset compensation already happened on the caller's `(x, y)`,
///     so descendants just add their subtree offset on top.
///
/// Skips out-of-flow descendants (handled by their own pass) and
/// whitespace-only text nodes (matches fragmenter behavior).
fn record_fixed_subtree_descendants(
    geometry: &mut PaginationGeometryTable,
    doc: &BaseDocument,
    fixed_root_id: usize,
    root_stored_xy: (f32, f32),
    pages: u32,
    emitted: &mut usize,
) {
    use ::style::properties::longhands::position::computed_value::T as Pos;

    #[allow(clippy::too_many_arguments)]
    fn walk(
        geometry: &mut PaginationGeometryTable,
        doc: &BaseDocument,
        node_id: usize,
        offset_in_subtree: (f32, f32),
        root_stored_xy: (f32, f32),
        pages: u32,
        depth: usize,
        emitted: &mut usize,
    ) {
        if depth >= crate::MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };
        let stored_x = root_stored_xy.0 + offset_in_subtree.0;
        let stored_y = root_stored_xy.1 + offset_in_subtree.1;
        let w = node.final_layout.size.width;
        let h = node.final_layout.size.height;

        let entry = geometry.entry(node_id).or_default();
        entry.fragments.clear();
        entry.is_repeat = true;
        // Per-page repeat fragments, bounded by the aggregate `emitted` budget
        // (the hard cap on O(descendants × pages)). Use `break`, not `return`,
        // and do not skip zero-area nodes: the walk must visit and clear every
        // descendant (the fixed pass runs twice — before and after the absolute
        // pass — so a later node can hold stale fragments from the first pass),
        // and a zero-size wrapper's fragment can still scope a visible child via
        // transform / opacity dispatch, which reads the wrapper's geometry
        // (Codex review).
        for page_index in 0..pages.max(1) {
            if *emitted >= crate::MAX_SUBTREE_PAGE_FRAGMENTS {
                break;
            }
            entry.fragments.push(Fragment {
                page_index,
                x: stored_x.as_px(),
                y: stored_y.as_px(),
                width: w.as_px(),
                height: h.as_px(),
            });
            *emitted += 1;
        }

        let children: Vec<usize> = {
            let layout_borrow = node.layout_children.borrow();
            if let Some(lc) = layout_borrow.as_deref()
                && !lc.is_empty()
            {
                lc.to_vec()
            } else {
                node.children.clone()
            }
        };
        for child_id in children {
            let Some(child) = doc.get_node(child_id) else {
                continue;
            };
            let is_oof = child.primary_styles().is_some_and(|s| {
                matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed)
            });
            if is_oof {
                continue;
            }
            if let Some(text) = child.text_data()
                && text.content.chars().all(char::is_whitespace)
            {
                continue;
            }
            let child_offset = (
                offset_in_subtree.0 + child.final_layout.location.x,
                offset_in_subtree.1 + child.final_layout.location.y,
            );
            walk(
                geometry,
                doc,
                child_id,
                child_offset,
                root_stored_xy,
                pages,
                depth + 1,
                emitted,
            );
        }
    }

    let Some(root) = doc.get_node(fixed_root_id) else {
        return;
    };
    let children: Vec<usize> = {
        let layout_borrow = root.layout_children.borrow();
        if let Some(lc) = layout_borrow.as_deref()
            && !lc.is_empty()
        {
            lc.to_vec()
        } else {
            root.children.clone()
        }
    };
    for child_id in children {
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
        let is_oof = child
            .primary_styles()
            .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed));
        if is_oof {
            continue;
        }
        if let Some(text) = child.text_data()
            && text.content.chars().all(char::is_whitespace)
        {
            continue;
        }
        let child_offset = (child.final_layout.location.x, child.final_layout.location.y);
        walk(
            geometry,
            doc,
            child_id,
            child_offset,
            root_stored_xy,
            pages,
            1,
            emitted,
        );
    }
}

/// fulgur-a8m5: emit a Fragment for every body-direct
/// `position: absolute` element whose effective containing block falls
/// back to the viewport (when body's box collapses to zero because all
/// of its children are out-of-flow — see CSS 2.1 §10.1.5 and the
/// matching `viewport_size_px` body-zero fallback in
/// `convert::positioned::resolve_cb_for_absolute`).
///
/// `fragment_pagination_root` skips out-of-flow children unconditionally,
/// so without this pass `<body><div style="position:absolute; bottom:0">…</div></body>`
/// never reaches `pagination_geometry` and the v2 dispatch loop drops
/// the element entirely (WPT `fixedpos-00{1,2,8}` ref-side breakage).
///
/// Each visited in-flow node emits fragments for every page intersected
/// by its resolved y range; off-page elements (e.g. `bottom: -100vh` in
/// a single-page document) are dropped because no page can paint them.
pub fn append_position_absolute_body_direct_fragments(
    geometry: &mut PaginationGeometryTable,
    doc: &BaseDocument,
    total_pages: u32,
    viewport_w_px: f32,
    viewport_h_px: f32,
    running_store: Option<&crate::gcpm::running::RunningElementStore>,
) {
    use ::style::properties::longhands::position::computed_value::T as Pos;

    let pages = total_pages.max(1);
    let body_id = match find_body_id(doc) {
        Some(id) => id,
        None => return,
    };
    let body = match doc.get_node(body_id) {
        Some(n) => n,
        None => return,
    };
    // Per CSS 2.1 §10.1.5, the containing block for `position: absolute`
    // children of `<body>` (a static-position element) falls through to
    // the initial containing block (the viewport) regardless of body's
    // own size. The fragmenter unconditionally skips out-of-flow
    // children (`fragment_pagination_root` `continue` for `Pos::Absolute`),
    // so this pass runs for every body-direct abs — not just when body
    // collapses to zero.
    let body_offset_xy = body_origin_in_px(doc);

    // Aggregate budget across all body-direct absolute subtrees: each node is
    // recorded once per intersected page, so many page-spanning absolutes are
    // O(nodes × pages). Cap the retained total (shared with the fixed pass via
    // `crate::MAX_SUBTREE_PAGE_FRAGMENTS`).
    let mut emitted: usize = 0;

    let body_children = body.children.clone();
    let body_has_in_flow_content = body_children.iter().any(|&child_id| {
        let Some(child) = doc.get_node(child_id) else {
            return false;
        };
        if let Some(text) = child.text_data()
            && text.content.chars().all(char::is_whitespace)
        {
            return false;
        }
        if running_store.is_some_and(|s| s.instance_for_node(child_id).is_some()) {
            return false;
        }
        !is_out_of_flow_positioned(child) && !crate::blitz_adapter::node_is_floating(child)
    });
    for child_id in body_children {
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
        let is_abs_only = child
            .primary_styles()
            .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Absolute));
        if !is_abs_only {
            continue;
        }
        let layout = child.final_layout;
        let (w, h) = (layout.size.width, layout.size.height);
        let (resolved_x, resolved_y) =
            resolve_viewport_cb_location(child, w, h, viewport_w_px, viewport_h_px)
                .unwrap_or((layout.location.x, layout.location.y));
        let page_stride_px = if uses_bottom_without_top(child) {
            viewport_h_px.round()
        } else {
            viewport_h_px
        };

        // Walk the subtree and emit fragments for every page each
        // in-flow node intersects (block, paragraph, anonymous wrapper).
        // Without this the dispatch loop drops descendants whose node_id
        // is not the root abs id (anonymous block wrappers around mixed
        // text/element content — fixedpos-002 / fixedpos-008 ref-side
        // pattern). Out-of-flow descendants are skipped because they
        // are handled by their own pass. The render path adds
        // `body_offset_pt` to every emitted fragment's y, so we pass
        // the body offset down and let the walker subtract it from the
        // stored y while keeping page assignment based on the un-
        // compensated viewport-anchored y.
        record_subtree_fragments_at_offset(
            geometry,
            doc,
            child_id,
            (resolved_x, resolved_y),
            body_offset_xy,
            viewport_h_px,
            page_stride_px,
            pages,
            !body_has_in_flow_content,
            &mut emitted,
        );
    }

    geometry.retain(|_, geom| !geom.fragments.is_empty());
}

/// Walk a body-direct out-of-flow subtree and emit Fragments on every
/// intersected page for each in-flow node (the subtree root + every
/// block / paragraph / anonymous wrapper inside it). Each fragment's
/// body-relative location = the subtree root's resolved viewport-CB
/// location plus the node's accumulated `final_layout.location` offset
/// from that root.
///
/// Skips out-of-flow descendants (their own pass handles them) and
/// whitespace-only text nodes (mirrors `fragment_pagination_root`).
#[allow(clippy::too_many_arguments)]
fn record_subtree_fragments_at_offset(
    geometry: &mut PaginationGeometryTable,
    doc: &BaseDocument,
    subtree_root_id: usize,
    root_xy_for_paging: (f32, f32),
    body_offset: (f32, f32),
    page_h_px: f32,
    page_stride_px: f32,
    total_pages: u32,
    may_extend_pages: bool,
    emitted: &mut usize,
) {
    #[allow(clippy::too_many_arguments)]
    fn walk(
        geometry: &mut PaginationGeometryTable,
        doc: &BaseDocument,
        node_id: usize,
        offset_in_subtree: (f32, f32),
        root_xy_for_paging: (f32, f32),
        body_offset: (f32, f32),
        page_h_px: f32,
        page_stride_px: f32,
        total_pages: u32,
        may_extend_pages: bool,
        // fulgur-xa9q: true once we are at or below an overflow-clipping
        // ancestor (`clips_overflow`: any non-`visible` overflow). Inside such
        // a subtree the "START beyond budget extends" exception is suppressed
        // for descendants, so clipped overflow cannot generate pages
        // (page-background-003 cat box, which is `overflow:clip`). `contain:
        // size` alone is NOT a clip and does not set this boundary.
        containment_boundary: bool,
        // Subtree-offset and border-box size of the nearest positioned
        // ancestor of `node_id` — the containing block that `node_id`'s
        // out-of-flow children resolve their explicit insets against (CSS
        // 2.1 §10.1.4). The body-direct abs at the subtree root is itself
        // positioned, so it overrides these on the first frame.
        cb_anchor: (f32, f32),
        cb_size: (f32, f32),
        depth: usize,
        emitted: &mut usize,
    ) {
        if depth >= crate::MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };
        // Prefer `layout_children` so anonymous block wrappers Stylo
        // synthesizes around mixed inline/block content are visited
        // (CSS 2.1 §9.2.1.1 — see `fragment_pagination_root` for the
        // same idiom).
        let children: Vec<usize> = {
            let layout_borrow = node.layout_children.borrow();
            if let Some(lc) = layout_borrow.as_deref()
                && !lc.is_empty()
            {
                lc.to_vec()
            } else {
                node.children.clone()
            }
        };
        // The containing block for THIS node's out-of-flow children is
        // `node` itself when `node` is positioned (non-static), otherwise the
        // inherited nearest-positioned ancestor (`cb_anchor`/`cb_size`). A
        // `position: static` element does not establish a CB, so an abs child
        // beneath it anchors to the same ancestor its static parent does.
        //
        // The CB is the positioned ancestor's *padding box* (CSS 2.1
        // §10.1.4): the anchor backs in by its top/left border and the size
        // drops both borders. `offset_in_subtree` is `node`'s border-box
        // origin, so add the border to reach the padding edge.
        let node_positioned = {
            use ::style::properties::longhands::position::computed_value::T as Pos;
            node.primary_styles()
                .is_some_and(|s| !matches!(s.get_box().clone_position(), Pos::Static))
        };
        let (child_cb_anchor, child_cb_size) = if node_positioned {
            let border = node.final_layout.border;
            (
                (
                    offset_in_subtree.0 + border.left,
                    offset_in_subtree.1 + border.top,
                ),
                (
                    node.final_layout.size.width - border.left - border.right,
                    node.final_layout.size.height - border.top - border.bottom,
                ),
            )
        } else {
            (cb_anchor, cb_size)
        };
        // Page assignment is based on the un-compensated viewport-CB
        // resolved position (the actual paint location). Storage is
        // body-relative because the dispatch path adds `body_offset_pt`
        // back at draw time.
        let final_y_for_paging = root_xy_for_paging.1 + offset_in_subtree.1;
        let stored_x = root_xy_for_paging.0 + offset_in_subtree.0 - body_offset.0;
        let w = node.final_layout.size.width;
        let h = node.final_layout.size.height;
        let is_size_contained = has_contain_size(node);
        let monolithic_adjust: f32 = children
            .iter()
            .filter_map(|child_id| doc.get_node(*child_id))
            .filter(|child| !is_out_of_flow_positioned(child) && has_contain_size(child))
            .map(|child| (child.final_layout.size.height - page_h_px).max(0.0))
            .sum();
        let h_for_paging = (h - monolithic_adjust).max(0.0);
        let mut descendant_total_pages = total_pages;

        if final_y_for_paging.is_finite()
            && h.is_finite()
            && h_for_paging.is_finite()
            && page_h_px > 0.0
            && h_for_paging > 0.0
        {
            // Stylo computes `Nvh` against the viewport snapshot taken
            // at parse time, which can differ from `page_h_px` by a
            // sub-px amount (the @page resolution uses the resolved
            // content area; Stylo's computed `100vh` rounds elsewhere).
            // Without tolerance, `top: 100vh` on a 1-page render
            // becomes `final_y = 971.0` against `page_h_px = 971.34`,
            // which floor()s to page 0 and renders the off-page text
            // on page 1 (WPT fixedpos-008 ref-side). Snap final_y
            // toward integer multiples of page_h before paging.
            let start_ratio = final_y_for_paging / page_h_px;
            let start_round = start_ratio.round();
            // fulgur-xa9q: the Stylo `Nvh` sub-px residual vs `page_h_px` scales
            // with the `vh` MULTIPLE summed into `final_y_for_paging` — Stylo's
            // per-`vh`-unit rounding error is multiplied by the vh count, so a
            // `top:400vh` lands ~1.35px short of `4*page_h` and a `top:500vh`
            // further, regardless of nesting depth. A flat `1e-3` tolerance
            // misses this and mis-pages the element onto the page boundary so
            // its line splits and clips (fixedpos-005 "fifth", fixedpos-008
            // page 6). Scale the tolerance by the rounded page (≈ the vh
            // multiple), capped at 1% of the page (~half a line) so extreme
            // page counts cannot grow the window unbounded. NOTE: depth-scaling
            // was tried and regresses fixedpos-008 — the residual tracks the vh
            // multiple, not nesting depth (Codex review on PR #498).
            let snap_tol = (1e-3 * start_round.abs().max(1.0)).min(0.01);
            let start_is_snapped = (start_ratio - start_round).abs() < snap_tol;
            let snapped_start_ratio = if start_is_snapped {
                start_round
            } else {
                start_ratio
            };
            let bottom_y_for_paging = final_y_for_paging + h_for_paging;
            let bottom_ratio = bottom_y_for_paging / page_h_px;
            let mut last_page_f =
                if bottom_y_for_paging.is_infinite() && bottom_y_for_paging.is_sign_positive() {
                    total_pages.saturating_sub(1) as f32
                } else if (bottom_ratio - bottom_ratio.round()).abs() < 1e-6 {
                    bottom_ratio.round() - 1.0
                } else {
                    bottom_ratio.floor()
                };
            let first_page_f = snapped_start_ratio.floor().max(0.0);
            if is_size_contained {
                last_page_f = first_page_f;
            }
            // fulgur-xa9q: the start snap can advance `first_page_f` onto the
            // next page while `last_page_f` still floors from the UNSNAPPED
            // bottom; for a box shorter than the corrected residual that would
            // make `first_page_f > last_page_f` and drop the fragment entirely
            // (Codex review). A box must appear on at least its (snapped) start
            // page, so never let the last page precede the first.
            last_page_f = last_page_f.max(first_page_f);
            // fulgur-xa9q: an abs whose START is at/beyond the existing in-flow
            // page budget extends the page count even with in-flow content
            // present (Chrome-compatible) — UNLESS it sits inside a containment
            // /clip boundary, where overflow is invisible and must not paginate.
            // A tall abs anchored WITHIN the budget stays clamped (it has an
            // in-budget page to clip onto) so short-flow layouts do not grow.
            let node_may_extend =
                may_extend_pages || (first_page_f >= total_pages as f32 && !containment_boundary);
            if first_page_f.is_finite()
                && last_page_f.is_finite()
                && first_page_f <= last_page_f
                && (node_may_extend || first_page_f < total_pages as f32)
            {
                // fulgur-2m6w: clamp the emitted page index to `MAX_PAGES`,
                // but ONLY on the page-EXTENSION path. `first_page_f` /
                // `last_page_f` derive from the abs element's viewport-CB
                // resolved Y, which is attacker-controlled CSS
                // (`position:absolute; top:99999999px`). When the element
                // extends the page count (`node_may_extend`) an unclamped
                // value lands at page ~10^5, `descendant_total_pages`
                // propagates that to `implied_page_count`, and `render_v2`
                // allocates + renders every intervening page — the same
                // small-input DoS the body-direct slice cap blocks. This
                // `walk` runs for the abs root AND nested abs descendants,
                // so one clamp bounds both.
                //
                // The NON-extending branch is already bounded by
                // `total_pages` (the in-flow page count, itself capped /
                // input-proportional), so it must stay UNCLAMPED: clamping
                // `first_page` while `last_page` keeps `min(.., total_pages-1)`
                // would emit a spurious fragment run from `MAX_PAGES` through
                // the real (in-budget) page when `total_pages > MAX_PAGES`
                // from many ordinary in-flow pages (Codex review on PR #501).
                let (first_page, last_page) = if node_may_extend {
                    (
                        (first_page_f as u32).min(crate::MAX_PAGES),
                        (last_page_f as u32).min(crate::MAX_PAGES),
                    )
                } else {
                    (
                        first_page_f as u32,
                        (last_page_f as u32).min(total_pages.saturating_sub(1)),
                    )
                };
                // fulgur-xa9q: page ASSIGNMENT snaps the start ratio to the page
                // grid (`first_page_f` via `snapped_start_ratio`), but the stored
                // Y must use the SAME snapped origin. Otherwise a `top: 100vh`
                // abs lands ~1px off the in-flow fragmenter's exact page-top
                // placement (the Stylo `100vh` vs `page_h_px` sub-px residual),
                // producing the fixedpos-005/006/008 page-2 pixel diff once the
                // page count matches. Snap only the paging origin, and only for
                // top-anchored subtrees (`page_stride_px == page_h_px`); the
                // bottom-only repeat idiom uses a rounded stride and must keep
                // its raw origin.
                let paging_origin_y = if page_stride_px == page_h_px && start_is_snapped {
                    snapped_start_ratio * page_h_px
                } else {
                    final_y_for_paging
                };
                let entry = geometry.entry(node_id).or_default();
                entry.fragments.clear();
                entry.is_repeat = false;
                for page_index in first_page..=last_page {
                    // Aggregate budget: each node emits one fragment per
                    // intersected page, so a subtree of many page-spanning
                    // absolutes is O(nodes × pages). Stop past the cap
                    // (`crate::MAX_SUBTREE_PAGE_FRAGMENTS`) — shared with the
                    // fixed pass. Use `break`, not `return`: the walk must keep
                    // visiting (and clearing) the rest of the subtree so no
                    // stale fragments survive from an earlier pass (Codex
                    // review).
                    if *emitted >= crate::MAX_SUBTREE_PAGE_FRAGMENTS {
                        break;
                    }
                    let is_monolithic_continuation =
                        monolithic_adjust > 0.0 && page_index > first_page;
                    let stored_y = if is_monolithic_continuation {
                        -body_offset.1
                    } else {
                        paging_origin_y - (page_index as f32) * page_stride_px - body_offset.1
                    };
                    let stored_h = if is_monolithic_continuation {
                        let consumed = (page_index - first_page) as f32 * page_h_px;
                        (h_for_paging - consumed).clamp(0.0, page_h_px)
                    } else {
                        h
                    };
                    entry.fragments.push(Fragment {
                        page_index,
                        x: stored_x.as_px(),
                        y: stored_y.as_px(),
                        width: w.as_px(),
                        height: stored_h.as_px(),
                    });
                    *emitted += 1;
                }
                descendant_total_pages = descendant_total_pages.max(last_page.saturating_add(1));
            }
        }

        let mut monolithic_y_adjust = 0.0;
        for child_id in children {
            let Some(child) = doc.get_node(child_id) else {
                continue;
            };
            // Out-of-flow descendants:
            //   - `position: fixed` is a repeat element handled by
            //     `append_position_fixed_fragments`; skip it here so it
            //     does not contribute to extent (it would otherwise be
            //     double-counted).
            //   - `position: absolute` (fulgur-puml #1): a nested abs's
            //     height/offset must still drive the page count. Resolve
            //     its location against ITS containing block (the nearest
            //     positioned ancestor = the current `node`'s box) via
            //     `resolve_viewport_cb_location`, fold that CB-relative
            //     (x, y) into the accumulated `offset_in_subtree`, and
            //     recurse. `root_xy_for_paging` stays the body-direct
            //     abs root anchor; viewport-relative (`vh`) insets are
            //     CB-independent, so the CB dims only affect percentage
            //     insets.
            {
                use ::style::properties::longhands::position::computed_value::T as Pos;
                match child.primary_styles().map(|s| s.get_box().clone_position()) {
                    // Fixed is handled by its own pass; skip here.
                    Some(Pos::Fixed) => continue,
                    Some(Pos::Absolute) => {
                        let (child_w, child_h) = (
                            child.final_layout.size.width,
                            child.final_layout.size.height,
                        );
                        // Resolve against the nearest positioned ancestor's
                        // padding box (`child_cb_*`), NOT the immediate DOM
                        // parent — a `position: static` parent does not
                        // establish a CB (CSS 2.1 §10.1.4).
                        let (rel_x, rel_y) = resolve_viewport_cb_location(
                            child,
                            child_w,
                            child_h,
                            child_cb_size.0,
                            child_cb_size.1,
                        )
                        .unwrap_or((child.final_layout.location.x, child.final_layout.location.y));
                        // Per-axis anchor: an explicit inset is relative to the
                        // CB origin (`child_cb_anchor`); an `auto` inset keeps
                        // the static position, i.e. the flow offset relative to
                        // the immediate parent (`offset_in_subtree`). For a
                        // directly-nested abs the immediate parent IS the
                        // positioned ancestor, so both bases coincide and this
                        // is a no-op; they diverge only across a static
                        // intermediate.
                        let (explicit_x, explicit_y) = explicit_inset_axes(child);
                        let base_x = if explicit_x {
                            child_cb_anchor.0
                        } else {
                            offset_in_subtree.0
                        };
                        let base_y = if explicit_y {
                            child_cb_anchor.1
                        } else {
                            offset_in_subtree.1
                        };
                        let nested_offset = (base_x + rel_x, base_y + rel_y);
                        walk(
                            geometry,
                            doc,
                            child_id,
                            nested_offset,
                            root_xy_for_paging,
                            body_offset,
                            page_h_px,
                            page_stride_px,
                            descendant_total_pages,
                            may_extend_pages,
                            containment_boundary || clips_overflow(node),
                            child_cb_anchor,
                            child_cb_size,
                            depth + 1,
                            emitted,
                        );
                        continue;
                    }
                    // In-flow (or unstyled): fall through to the normal path below.
                    _ => {}
                }
            }
            // Skip whitespace-only text (matches fragmenter).
            if let Some(text) = child.text_data()
                && text.content.chars().all(char::is_whitespace)
            {
                continue;
            }
            let child_offset = (
                offset_in_subtree.0 + child.final_layout.location.x,
                offset_in_subtree.1 + child.final_layout.location.y - monolithic_y_adjust,
            );
            walk(
                geometry,
                doc,
                child_id,
                child_offset,
                root_xy_for_paging,
                body_offset,
                page_h_px,
                page_stride_px,
                descendant_total_pages,
                may_extend_pages,
                containment_boundary || clips_overflow(node),
                child_cb_anchor,
                child_cb_size,
                depth + 1,
                emitted,
            );
            if has_contain_size(child) {
                monolithic_y_adjust += (child.final_layout.size.height - page_h_px).max(0.0);
            }
        }
    }

    walk(
        geometry,
        doc,
        subtree_root_id,
        (0.0, 0.0),
        root_xy_for_paging,
        body_offset,
        page_h_px,
        page_stride_px,
        total_pages,
        may_extend_pages,
        // fulgur-xa9q: the subtree root starts outside any containment boundary.
        false,
        // Initial CB: the subtree root is the body-direct abs (positioned),
        // so it overrides these on the first frame — seed with the root's own
        // anchor/size for correctness if that ever changes.
        (0.0, 0.0),
        (0.0, 0.0),
        0,
        emitted,
    );
}

/// Which axes of an out-of-flow element carry an explicit (length /
/// percentage) inset, as `(x, y)`. An explicit inset is resolved against the
/// containing block; an `auto` inset falls back to the static (flow) position.
/// Mirrors the per-axis idiom in [`resolve_viewport_cb_location`].
fn explicit_inset_axes(node: &blitz_dom::Node) -> (bool, bool) {
    use ::style::values::generics::position::GenericInset;

    fn is_length_percentage(inset: &::style::values::computed::position::Inset) -> bool {
        matches!(inset, GenericInset::LengthPercentage(_))
    }

    let Some(styles) = node.primary_styles() else {
        return (false, false);
    };
    let pos = styles.get_position();
    (
        is_length_percentage(&pos.left) || is_length_percentage(&pos.right),
        is_length_percentage(&pos.top) || is_length_percentage(&pos.bottom),
    )
}

fn is_out_of_flow_positioned(node: &blitz_dom::Node) -> bool {
    use ::style::properties::longhands::position::computed_value::T as Pos;

    node.primary_styles()
        .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed))
}

fn has_contain_size(node: &blitz_dom::Node) -> bool {
    node.primary_styles().is_some_and(|s| {
        s.get_box()
            .clone_contain()
            .contains(::style::values::computed::box_::Contain::SIZE)
    })
}

/// Whether `node` establishes a clip context that hides its overflowing
/// descendants from painting — used as the page-extension boundary
/// (fulgur-xa9q): a descendant whose paint overflow is clipped here cannot
/// generate pages even when it lands past the in-flow budget.
///
/// Two conditions, both required:
///   1. `overflow` is not `visible` on some axis (`hidden`/`clip`/`scroll`/
///      `auto` all clip; mirrors `convert::style::overflow`).
///   2. the box actually establishes a clip context — a block-level box or an
///      atomic inline / BFC root. A non-replaced `display: inline` box ignores
///      `overflow` and the renderer pushes no clip scope for it, so an inline
///      wrapper like `<span style="overflow:hidden">` must NOT act as a
///      boundary (Codex review on PR #498).
///
/// NOTE: `contain: size` is deliberately NOT treated as a clip — it sizes the
/// box without its contents but leaves overflow visible, so a
/// `contain:size; overflow:visible` box must still let descendants extend.
fn clips_overflow(node: &blitz_dom::Node) -> bool {
    use ::style::values::computed::Overflow as Ov;
    use ::style::values::specified::box_::{DisplayInside, DisplayOutside};
    node.primary_styles().is_some_and(|s| {
        let clips = s.clone_overflow_x() != Ov::Visible || s.clone_overflow_y() != Ov::Visible;
        if !clips {
            return false;
        }
        let display = s.clone_display();
        display.outside() == DisplayOutside::Block || display.inside() != DisplayInside::Flow
    })
}

/// CSS-px (x, y) of `<body>`'s top-left in its containing block (html).
/// Mirrors `convert::extract_body_offset_pt` but stays in CSS px so
/// pagination_layout doesn't need to round-trip through pt. The render
/// path adds `drawables.body_offset_pt` to every fragment's y when
/// dispatching, so viewport-anchored fragments must subtract this
/// offset to keep the dispatched y page-relative.
fn body_origin_in_px(doc: &BaseDocument) -> (f32, f32) {
    let Some(body_id) = find_body_id(doc) else {
        return (0.0, 0.0);
    };
    let Some(body) = doc.get_node(body_id) else {
        return (0.0, 0.0);
    };
    (body.final_layout.location.x, body.final_layout.location.y)
}

/// Resolve a viewport-CB-anchored absolute/fixed element's CSS px
/// (x, y) using its computed `top` / `left` / `right` / `bottom`
/// insets. Mirrors CSS 2.1 §10.3.7 / §10.6.4 over-constrained
/// resolution: start-side (top / left) wins when both sides are set,
/// end-side (bottom / right) only fires when the start-side is `auto`.
///
/// Returns `None` when no inset is set on either axis (caller should
/// keep Taffy's `final_layout.location` as the static-position
/// fallback).
fn resolve_viewport_cb_location(
    node: &blitz_dom::Node,
    el_w_px: f32,
    el_h_px: f32,
    cb_w_px: f32,
    cb_h_px: f32,
) -> Option<(f32, f32)> {
    use ::style::values::computed::Length;
    use ::style::values::generics::position::GenericInset;

    fn resolve(inset: &::style::values::computed::position::Inset, basis_px: f32) -> Option<f32> {
        match inset {
            GenericInset::LengthPercentage(lp) => Some(lp.resolve(Length::new(basis_px)).px()),
            _ => None,
        }
    }

    let styles = node.primary_styles()?;
    let pos = styles.get_position();
    let left = resolve(&pos.left, cb_w_px);
    let top = resolve(&pos.top, cb_h_px);
    let right = resolve(&pos.right, cb_w_px);
    let bottom = resolve(&pos.bottom, cb_h_px);
    if left.is_none() && top.is_none() && right.is_none() && bottom.is_none() {
        return None;
    }
    // Per-axis resolution: when both insets on an axis are `auto`, fall
    // back to Taffy's static position (`final_layout.location`). Caller
    // unwraps the returned tuple against that fallback, so we surface
    // only the axes that have an explicit inset.
    // End-side (right / bottom) anchoring positions the element's *margin
    // box* edge against the CB edge, so the border-box origin must back off
    // by the used end-side margin (CSS 2.1 §10.3.7 / §10.6.4). Taffy resolves
    // these into `final_layout.margin`; without subtracting them an abs with
    // `bottom:0; margin-bottom:2em` collapses onto `bottom:0`. This is a
    // distinct end-side-margin bug from nested-abs pagination — see the
    // `abs_bottom_margin_offsets_above_sibling` regression test. NOTE: this
    // helper resolves `position: fixed` elements too (all three callers), so
    // the margin term corrects fixed end-anchoring as well as abs. The margin
    // is subtracted unrounded between the two rounded terms; fixedpos-004's
    // pixel-exact reftest is the validator for this rounding choice.
    let margin = node.final_layout.margin;
    let x = if let Some(l) = left {
        l
    } else if let Some(r) = right {
        // To mirror Taffy's internal end-side layout, collapse viewport
        // edge before subtracting element width.
        (cb_w_px - r).round() - margin.right - el_w_px.round()
    } else {
        node.final_layout.location.x
    };
    let y = if let Some(t) = top {
        t
    } else if let Some(b) = bottom {
        // Same order as the in-tree flow for `bottom: 0` style anchors:
        // first round viewport location, then subtract rounded element
        // height. This keeps fixed/abs reference paths aligned to one
        // px boundary when cb height / element size are sub-pixel.
        (cb_h_px - b).round() - margin.bottom - el_h_px.round()
    } else {
        node.final_layout.location.y
    };
    Some((x, y))
}

fn uses_bottom_without_top(node: &blitz_dom::Node) -> bool {
    use ::style::values::generics::position::GenericInset;

    fn is_length_percentage(inset: &::style::values::computed::position::Inset) -> bool {
        matches!(inset, GenericInset::LengthPercentage(_))
    }

    let Some(styles) = node.primary_styles() else {
        return false;
    };
    let pos = styles.get_position();
    !is_length_percentage(&pos.top) && is_length_percentage(&pos.bottom)
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
            .filter(|g| {
                g.fragments
                    .iter()
                    .any(|f| (f.height.to_f32() - 30.0).abs() < 0.5)
            })
            .map(|g| g.fragments[0].page_index)
            .collect();
        assert_eq!(h2_pages.len(), 2, "expected 2 h2 entries, got {h2_pages:?}");
        // Pages of the h2s should match those of their containing divs:
        // first div on page 0 → first h2 on page 0; second div on page
        // 1 → second h2 on page 1.
        assert_eq!(h2_pages, vec![0, 1]);
    }

    /// fulgur-2map.5: directly exercise `fragment_block_subtree`'s
    /// `depth >= MAX_DOM_DEPTH` guard (pagination_layout.rs ~1539-1557).
    ///
    /// The guard is the FIRST statement of the function, so calling it
    /// with `depth = crate::MAX_DOM_DEPTH` trips the bail immediately —
    /// no deep HTML and no big-stack thread required (the prior
    /// render_smoke coverage needed a 600-deep `<div>` chain plus a
    /// 256 MB stack just to recurse far enough to reach this arm). This
    /// also tracks the constant itself: if `MAX_DOM_DEPTH` ever changes,
    /// the test still enters the guard at exactly the limit.
    #[test]
    fn fragment_block_subtree_at_depth_limit_bails_with_whole_fragment() {
        let doc = parse(
            r#"<html><body style="margin:0"><div id="d" style="height:50px"></div></body></html>"#,
            600.0,
        );
        let parent_id = find_by_id(&doc, "d").expect("div#d should exist");
        // The bail copies this node's laid-out height into the fragment.
        let node_h = doc
            .get_node(parent_id)
            .expect("div node")
            .final_layout
            .size
            .height;
        assert!(
            (node_h - 50.0).abs() < 1.0,
            "div should lay out ~50px tall, got {node_h}"
        );

        let mut geom = PaginationGeometryTable::new();
        let (page_out, cursor_out) = fragment_block_subtree(
            &mut geom,
            &doc, // &BaseDocument via deref coercion
            None, // column_styles
            None, // used_page_names
            parent_id,
            600.0,                // parent_w
            0.0,                  // parent_x_in_body
            0,                    // page_in
            0.0,                  // cursor_in
            800.0,                // page_height_px
            crate::MAX_DOM_DEPTH, // depth → trips the guard immediately
        );

        // The bail pushes exactly ONE whole fragment for parent_id at its
        // entry coordinates, then returns (page_in, cursor_in + height).
        let entry = geom
            .get(&parent_id)
            .expect("bail must emit a geometry entry for parent_id");
        // Hot-path-bind `.len()` so the assert message doesn't introduce a
        // failure-only region that codecov marks uncovered (see the P1c
        // assert-arg artifact note in CLAUDE.md / units migration memory).
        let n_frags = entry.fragments.len();
        assert_eq!(
            n_frags, 1,
            "bail emits a single whole fragment, got {n_frags}"
        );
        let frag = &entry.fragments[0];
        assert_eq!(frag.page_index, 0, "fragment stays on the entry page");
        let fx = frag.x.to_f32();
        assert_eq!(fx, 0.0, "fragment x == parent_x_in_body, got {fx}");
        let fy = frag.y.to_f32();
        assert_eq!(fy, 0.0, "fragment y == cursor_in, got {fy}");
        let fw = frag.width.to_f32();
        assert_eq!(fw, 600.0, "fragment width == parent_w, got {fw}");
        let fh = frag.height.to_f32();
        assert!(
            (fh - node_h).abs() < 1.0,
            "fragment height == node layout height: {fh} vs {node_h}"
        );

        assert_eq!(page_out, 0, "bail returns the entry page unchanged");
        assert!(
            (cursor_out - node_h).abs() < 1.0,
            "cursor advances by the node height: {cursor_out} vs {node_h}"
        );
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

    /// Find the first element node carrying `id="<id>"`.
    fn find_by_id(doc: &blitz_dom::BaseDocument, id: &str) -> Option<usize> {
        fn walk(doc: &blitz_dom::BaseDocument, node_id: usize, target: &str) -> Option<usize> {
            let node = doc.get_node(node_id)?;
            if let Some(ed) = node.element_data()
                && let Some(attr_id) = ed.attrs().iter().find(|a| a.name.local.as_ref() == "id")
                && attr_id.value.as_str() == target
            {
                return Some(node_id);
            }
            for &child in &node.children {
                if let Some(found) = walk(doc, child, target) {
                    return Some(found);
                }
            }
            None
        }
        walk(doc, doc.root_element().id, id)
    }

    /// fulgur-ezst: a tiny input with a pathologically tall CSS height on a
    /// CHILDLESS block prints only blank pages, so instead of slicing it to
    /// the `MAX_PAGES` cap (~10k blank pages) the fragmenter collapses it to
    /// a single page. `height: 99999999px` would otherwise slice into
    /// ~125 000 fragments — the small-input DoS.
    #[test]
    fn pathological_childless_tall_block_collapses() {
        let html = r#"<html><body><div style="height: 99999999px"></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(
            implied_page_count(&table),
            1,
            "childless pathological height must collapse to a single page",
        );
    }

    /// fulgur-ezst: Stylo/Taffy clamps an absurd `<length>` to `f32::MAX`
    /// (≈3.4e38), the worst-case finite input. A childless block that tall
    /// also collapses to one page (the ceiling-bounded slice loop still runs
    /// its counter math but pushes no fragment).
    #[test]
    fn f32_max_childless_height_collapses() {
        let html = r#"<html><body><div style="height: 1e39px"></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(
            implied_page_count(&table),
            1,
            "childless f32::MAX height must collapse to a single page",
        );
    }

    /// fulgur-ezst: background presence does not gate the collapse — a
    /// childless filled band this tall is still a pathological amplifier.
    #[test]
    fn childless_tall_block_with_background_collapses() {
        let html =
            r#"<html><body><div style="height: 99999999px; background: red"></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(implied_page_count(&table), 1);
    }

    /// fulgur-ezst: a tall block WITH rendered content is not childless, so
    /// it is NOT collapsed — it still clamps to ~MAX_PAGES (truncate-and-
    /// warn). Also guards routing: a huge parent with a small child must
    /// reach the slice loop, not the recursion branch (the recursion gate
    /// measures descendant overflow, not the parent's intrinsic height).
    #[test]
    fn content_bearing_tall_block_is_page_capped() {
        let html = r#"<html><body><div style="height: 99999999px"><p>x</p></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        let pages = implied_page_count(&table);
        assert!(
            pages > 1_000,
            "content-bearing block must take the cap path, not collapse; got {pages}",
        );
        assert!(
            pages <= crate::MAX_PAGES + 1,
            "content-bearing block must stay clamped to ~MAX_PAGES ({}); got {pages}",
            crate::MAX_PAGES,
        );
    }

    /// fulgur-ezst: a childless band that fits WITHIN the cap renders its
    /// full page count — the collapse must not over-fire on ordinary
    /// multi-page spacers. `height: 4000px` on an 800px strip = 5 pages.
    #[test]
    fn childless_subcap_band_renders_full() {
        let html = r#"<html><body><div style="height: 4000px"></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(
            implied_page_count(&table),
            5,
            "a sub-cap childless band must render fully, not collapse",
        );
    }

    /// fulgur-ezst (Codex P2 on PR #553): an empty *block* child lays out
    /// with positive width but zero height, so a `height <= 0 && width <= 0`
    /// emptiness test would wrongly count it as content and disable the
    /// collapse — a trivial DoS bypass. `subtree_has_rendered_content` treats
    /// any zero-AREA descendant as blank (recursing for overflow), so a tall
    /// block whose only child is an empty block still collapses to one page.
    #[test]
    fn childless_tall_block_with_empty_block_child_collapses() {
        let html = r#"<html><body><div style="height: 99999999px"><div></div></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(
            implied_page_count(&table),
            1,
            "an empty (zero-area) block child must not defeat the collapse",
        );
    }

    /// fulgur-ezst: the emptiness recursion must still find VISIBLE content
    /// nested under a zero-area wrapper — a zero-height (overflow:visible)
    /// wrapper holding a real `<p>` is content, so the tall block is NOT
    /// childless and stays on the cap path. Guards the `||` fix against
    /// over-collapsing (dropping visibly-overflowing descendants).
    #[test]
    fn tall_block_with_content_under_zero_height_wrapper_is_not_collapsed() {
        let html = r#"<html><body><div style="height: 99999999px"><div style="height: 0"><p>x</p></div></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        let pages = implied_page_count(&table);
        assert!(
            pages > 1_000,
            "visible content under a zero-height wrapper must block the collapse; got {pages}",
        );
        assert!(
            pages <= crate::MAX_PAGES + 1,
            "still clamped to ~MAX_PAGES ({}); got {pages}",
            crate::MAX_PAGES,
        );
    }

    /// fulgur-ezst: the defensive guards of `subtree_has_rendered_content`
    /// read as "no content" (false) without panicking — a nonexistent node
    /// id (the `get_node` None guard) and a call already at the recursion
    /// ceiling (the `MAX_DOM_DEPTH` guard).
    #[test]
    fn subtree_has_rendered_content_guards() {
        use std::ops::DerefMut;
        let html = r#"<html><body><div id="x">hi</div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let x = find_by_id(doc.deref_mut(), "x").expect("div#x");
        assert!(!subtree_has_rendered_content(
            doc.deref_mut(),
            usize::MAX,
            0
        ));
        assert!(!subtree_has_rendered_content(
            doc.deref_mut(),
            x,
            crate::MAX_DOM_DEPTH
        ));
    }

    /// fulgur-c8re (security): a pathologically tall CHILDLESS replaced
    /// element (`<img>` / `<svg>`) that paints nothing must collapse like any
    /// blank spacer. "Paints nothing" covers the common offline-first case of
    /// an unresolved `src` (no matching `AssetBundle` entry), a
    /// `visibility:hidden` image, an undecodable format, and an empty `<svg>`.
    ///
    /// The predecessor `is_replaced_content` exception (Codex P2 on PR #553 /
    /// fulgur-ezst) gated the collapse on tag name alone, so such a
    /// non-painting node disabled the collapse and amplified a few bytes of
    /// HTML into ~`MAX_PAGES` blank pages (a validated high-severity DoS). It
    /// was removed: a replaced element only reaches this branch when it is
    /// taller than `MAX_PAGES` pages (~10M px), a range no legitimate single
    /// image occupies, so clipping it to one page loses no real content.
    #[test]
    fn replaced_tall_childless_block_collapses() {
        for tag in [
            // Unresolved `src`: no `AssetBundle` on this test path, so the
            // image paints nothing — yet it is still a pathological amplifier.
            r#"<img src="missing.png" style="display:block;width:10px;height:99999999px">"#,
            // `visibility:hidden`: occupies layout, paints nothing.
            r#"<img src="x.png" style="display:block;visibility:hidden;width:10px;height:99999999px">"#,
            // Empty `<svg>`: no drawable content.
            r#"<svg style="display:block;width:10px;height:99999999px"></svg>"#,
        ] {
            let html = format!(r#"<html><body>{tag}</body></html>"#);
            let mut doc = parse(&html, 600.0);
            let table = run_pass(&mut doc, 800.0);
            assert_eq!(
                implied_page_count(&table),
                1,
                "a non-painting tall replaced element must collapse, not \
                 amplify into blank pages: {tag}",
            );
        }
    }

    /// fulgur-ezst (Codex P2 on PR #553): a `visibility:hidden` descendant
    /// occupies layout space but paints nothing, so it must not defeat the
    /// collapse — a tall block whose only child is invisible still collapses.
    #[test]
    fn invisible_only_child_collapses() {
        let html = r#"<html><body><div style="height:99999999px"><div style="visibility:hidden;height:1px"></div></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(
            implied_page_count(&table),
            1,
            "a visibility:hidden-only descendant must not defeat the collapse",
        );
    }

    /// fulgur-ezst: the visibility skip must still recurse — a
    /// `visibility:visible` descendant under a `visibility:hidden` wrapper is
    /// real content, so the tall block is not childless and stays capped.
    #[test]
    fn visible_child_under_hidden_wrapper_blocks_collapse() {
        let html = r#"<html><body><div style="height:99999999px"><div style="visibility:hidden"><p style="visibility:visible">x</p></div></div></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        let pages = implied_page_count(&table);
        assert!(
            pages > 1_000,
            "visible content under a hidden wrapper must block the collapse; got {pages}",
        );
    }

    /// fulgur-c8re (security, Codex P1 on PR #575): the childless collapse must
    /// bound the SPACE the pathological element occupies, not just skip its
    /// per-page fragment pushes. The predecessor behaviour left the slice loop
    /// advancing `page_index` all the way to `MAX_PAGES`, so a following in-flow
    /// sibling was stranded on a deep page and `implied_page_count` — which
    /// reads the max fragment index across ALL nodes — re-inflated the document
    /// to ~`MAX_PAGES` blank pages (`<div huge></div><p>after</p>`), defeating
    /// the collapse. The element now consumes only its single first slice and
    /// following content reflows onto the next page. Asserts: the tall div
    /// contributes exactly one fragment (collapse fired) AND the `<p>` sibling
    /// lands on a bounded page, so the whole document stays a handful of pages.
    #[test]
    fn childless_collapse_reflows_following_sibling() {
        use std::ops::DerefMut;
        let html = r#"<html><body><div id="tall" style="height: 99999999px"></div><p id="after">after</p></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        let tall_id = find_by_id(doc.deref_mut(), "tall").expect("div#tall");
        let after_id = find_by_id(doc.deref_mut(), "after").expect("p#after");
        let tall_frags = table
            .get(&tall_id)
            .expect("div#tall must appear in geometry")
            .fragments
            .len();
        assert_eq!(
            tall_frags, 1,
            "collapsed childless div must contribute exactly one fragment, got {tall_frags}",
        );
        let after_page = table
            .get(&after_id)
            .expect("p#after must appear in geometry")
            .fragments[0]
            .page_index;
        assert!(
            after_page <= 1,
            "collapsed spacer must reflow its trailing sibling onto a bounded page \
             (<= 1), not strand it ~MAX_PAGES deep; got {after_page}",
        );
        assert!(
            implied_page_count(&table) <= 2,
            "a childless spacer followed by content must stay a handful of pages; got {}",
            implied_page_count(&table),
        );
    }

    /// fulgur-c8re (security, Codex P1 on PR #575): the replaced-element
    /// collapse must bound trailing content too. A non-painting tall image
    /// (`<img src="missing.png" height:99999999px>`) followed by a `<p>` must
    /// not amplify into ~`MAX_PAGES` blank pages — the same trailing-sibling
    /// vector as `childless_collapse_reflows_following_sibling`, but exercising
    /// the replaced-element path that the `is_replaced_content` removal opened.
    #[test]
    fn replaced_tall_collapse_bounds_trailing_sibling() {
        let html = r#"<html><body><img src="missing.png" style="display:block;width:10px;height:99999999px"><p>after</p></body></html>"#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert!(
            implied_page_count(&table) <= 2,
            "a non-painting tall replaced element followed by content must \
             collapse without amplifying; got {} pages",
            implied_page_count(&table),
        );
    }

    /// fulgur-2m6w: a non-finite Taffy height (`+inf` / `NaN`) is treated
    /// as zero so it can never reach the slicing loop (where `+inf` would
    /// make `remaining -= last_slice_h` loop forever) nor poison the
    /// `cursor_y` advance. CSS clamps to `f32::MAX` and cannot produce a
    /// non-finite height, so inject one directly into the resolved layout.
    /// The node still enters geometry (via the `child_h <= 0.0` branch) and
    /// the document stays single-page.
    #[test]
    fn non_finite_height_treated_as_zero() {
        use std::ops::DerefMut;
        for bad in [f32::INFINITY, f32::NAN] {
            let html = r#"<html><body><div id="x">hi</div></body></html>"#;
            let mut doc = parse(html, 600.0);
            let id = find_by_id(doc.deref_mut(), "x").expect("div#x");
            doc.deref_mut()
                .get_node_mut(id)
                .expect("div#x")
                .final_layout
                .size
                .height = bad;
            let table = run_pass(&mut doc, 800.0);
            assert!(
                implied_page_count(&table) == 1,
                "non-finite height {bad} must not paginate; got {} pages",
                implied_page_count(&table),
            );
            assert!(
                table.contains_key(&id),
                "non-finite-height node must still be recorded in geometry",
            );
        }
    }

    /// fulgur-i5a: `overflow: hidden` is a pure visual effect that must
    /// not affect pagination. A 50px-tall parent containing a 1200px-tall
    /// child stays single-page even though the child would otherwise
    /// extend past the 800px page strip. The clip is applied at draw
    /// time (`render.rs` push/pop_clip_path), and pagination follows the
    /// parent's layout box, not the child's overflowing intrinsic height.
    #[test]
    fn overflow_hidden_does_not_split_oversize_child() {
        let html = r#"
            <html><body>
              <div style="height: 50px; overflow: hidden">
                <div style="height: 1200px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);

        // body + outer (50px clip) + inner (1200px overflowing child)
        // = 3 entries; every entry has a single fragment on page 0.
        assert_eq!(table.len(), 3, "expected body + outer + inner = 3 entries");
        for (id, geom) in &table {
            assert_eq!(
                geom.fragments.len(),
                1,
                "node {id} should not split under overflow:hidden parent"
            );
            assert_eq!(geom.fragments[0].page_index, 0);
        }
    }

    /// Phase 4 prerequisite repro: confirm `string-set` carry semantic
    /// across page break.
    #[test]
    fn string_set_carry_across_page_break() {
        use crate::blitz_adapter;
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
        let geometry =
            run_pass_with_break_styles(doc.deref_mut(), 720.0_f32.as_pt().in_px(), &column_styles);

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
            800.0_f32.as_pt().in_px().to_f32(),
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
            fragments[0].height.to_f32(),
            0.0,
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
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
        });
        geom.entry(20).or_default().fragments.push(Fragment {
            page_index: 0,
            x: 0.0_f32.as_px(),
            y: 50.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
        });
        geom.entry(30).or_default().fragments.push(Fragment {
            page_index: 1,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
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
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 800.0_f32.as_px(),
        });
        geom.entry(42).or_default().fragments.push(Fragment {
            page_index: 1,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 200.0_f32.as_px(),
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

        super::append_position_fixed_fragments(
            &mut geom,
            doc.deref_mut(),
            pages_before,
            600.0,
            800.0,
        );

        // The fixed div should now appear in `geom` with one fragment
        // per page. We don't know its NodeId statically, so locate it
        // by the per-fragment width = 100.0.
        let fixed_entries: Vec<_> = geom
            .iter()
            .filter(|(_, g)| {
                g.fragments.iter().any(|f| {
                    (f.width.to_f32() - 100.0).abs() < 0.5 && (f.height.to_f32() - 50.0).abs() < 0.5
                })
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
        super::append_position_fixed_fragments(&mut geom, doc.deref_mut(), 0, 600.0, 800.0);
        assert_eq!(geom.len(), 1);
        let (_, g) = geom.iter().next().unwrap();
        assert_eq!(g.fragments.len(), 1);
        assert_eq!(g.fragments[0].page_index, 0);
    }

    /// fulgur-a8m5: `append_position_fixed_fragments` must resolve
    /// `bottom: 0` against the viewport CB. Taffy's `compute_root_layout`
    /// (used by `relayout_position_fixed`) does not honour end-side
    /// insets when the absolute element IS the layout-tree root, so
    /// `final_layout.location.y` stays at 0 even for `bottom: 0`. v2's
    /// dispatch reads `pagination_geometry` directly, so without inset
    /// resolution here, WPT fixedpos-001 / fixedpos-002 / fixedpos-008
    /// render their `bottom: 0` fixed text at the top of every page
    /// instead of the bottom.
    #[test]
    fn position_fixed_bottom_zero_resolves_against_viewport() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: fixed; bottom: 0; height: 30px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        crate::blitz_adapter::relayout_position_fixed(&mut doc, 600.0, 800.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_fixed_fragments(&mut geom, doc.deref_mut(), 1, 600.0, 800.0);

        // Locate the fixed div fragment by its 30px height.
        let entries: Vec<_> = geom
            .iter()
            .filter(|(_, g)| {
                g.fragments
                    .iter()
                    .any(|f| (f.height.to_f32() - 30.0).abs() < 0.5)
            })
            .collect();
        assert_eq!(entries.len(), 1, "exactly one fixed entry");
        let (_, g) = entries[0];
        assert_eq!(g.fragments.len(), 1);
        let frag = &g.fragments[0];
        // viewport_h_px=800, height=30 → bottom edge sits at 800 → top at 770.
        // body has zero height (no in-flow content), so body_offset_xy=(0,0).
        let frag_y = frag.y.to_f32();
        assert!(
            (frag_y - 770.0).abs() < 1.0,
            "bottom:0 fixed should resolve to y=770 (viewport_h - height); got {frag_y}",
        );
    }

    /// fulgur-orcx: `position: fixed` uses `bottom` anchoring against
    /// viewport-anchored CB. When both values are sub-pixel, resolving
    /// via the exact computed formula and rounding order used by Taffy
    /// keeps fixed root and abs-in-relative reference paths aligned.
    /// Without this, fixed-path can land 0.5px lower and fail
    /// WPT fixedpos-009-print by halo edge.
    #[test]
    fn position_fixed_bottom_zero_rounds_like_taffy_with_fractional_viewport_height() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: fixed; right: 0; bottom: 0; width: 36px; height: 36px">
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        crate::blitz_adapter::relayout_position_fixed(&mut doc, 600.0, 800.6);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_fixed_fragments(&mut geom, doc.deref_mut(), 1, 600.0, 800.6);

        let entries: Vec<_> = geom
            .iter()
            .filter(|(_, g)| {
                g.fragments
                    .iter()
                    .any(|f| (f.width.to_f32() - 36.0).abs() < 0.5)
            })
            .collect();
        assert_eq!(entries.len(), 1, "expected one fixed entry");
        let frag = entries[0].1.fragments.first().unwrap();
        assert_eq!(frag.page_index, 0, "expected fixed fragment only on page 0");
        // With Taffy-like rounding: round(cb_h - b) - round(h) => round(800.6)-36 = 765.
        let frag_y = frag.y.to_f32();
        assert!(
            (frag_y - 765.0).abs() < 0.25,
            "fractional fixed bottom anchor should resolve to y≈765; got {frag_y}",
        );
    }

    #[test]
    fn position_absolute_body_direct_bottom_viewport_page_stride_keeps_page_local_y_stable() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: absolute; bottom: -971px; height: 19px">x</div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            3,
            600.0,
            971.338_87,
            None,
        );

        let frag = geom
            .values()
            .flat_map(|g| &g.fragments)
            .find(|f| f.page_index == 1 && (f.height.to_f32() - 19.0).abs() < 0.5)
            .expect("absolute bottom:-viewport fragment should land on page 1");
        let frag_y = frag.y.to_f32();
        assert!(
            (frag_y - 952.0).abs() < 0.01,
            "absolute ref fragment should keep the same page-local bottom anchor as fixed; got {frag_y}",
        );
    }

    /// fulgur-a8m5: body's collapsed-margin offset (e.g. an in-flow
    /// child with `margin-top:4em`) appears in
    /// `drawables.body_offset_pt`, which the v2 dispatch path adds to
    /// every fragment's y. Viewport-anchored fixed elements must
    /// subtract that offset at storage time so the dispatched y lands
    /// at the page-relative position the CSS asks for. Locks the math
    /// PDF y = margin_top_pt + body_offset_pt + (frag.y_pt) — for
    /// `top:0` we want PDF y = margin_top_pt, so frag.y_px must equal
    /// `-body_offset_y_px`.
    #[test]
    fn position_fixed_top_zero_compensates_for_body_offset() {
        // The in-flow div pushes body's content area down by ~4em.
        let html = r#"
            <html><body>
              <div style="margin-top:4em">x</div>
              <div style="position: fixed; top: 0; height: 30px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        crate::blitz_adapter::relayout_position_fixed(&mut doc, 600.0, 800.0);
        let body_y_px = super::body_origin_in_px(doc.deref_mut()).1;
        // Sanity: body_offset must be non-zero, otherwise the test is
        // not exercising compensation at all.
        assert!(
            body_y_px > 0.5,
            "test assumes body has a non-zero offset; got {body_y_px}"
        );
        let mut geom = PaginationGeometryTable::new();
        super::append_position_fixed_fragments(&mut geom, doc.deref_mut(), 1, 600.0, 800.0);

        let entries: Vec<_> = geom
            .iter()
            .filter(|(_, g)| {
                g.fragments
                    .iter()
                    .any(|f| (f.height.to_f32() - 30.0).abs() < 0.5)
            })
            .collect();
        assert_eq!(entries.len(), 1, "exactly one fixed entry");
        let frag = entries[0].1.fragments.first().unwrap();
        // top:0 → resolved_y=0 → stored_y = 0 - body_y_px = -body_y_px.
        let frag_y = frag.y.to_f32();
        assert!(
            (frag_y - (-body_y_px)).abs() < 0.5,
            "top:0 fixed frag.y must be -body_offset (={}); got {}",
            -body_y_px,
            frag_y
        );
    }

    /// fulgur-4m16: when a `position: fixed` root has a sized
    /// block-element child, the child must also receive per-page
    /// repeated fragments. v2 dispatch reads
    /// `pagination_geometry[node_id]` and never recurses into a fixed
    /// root's subtree, so without per-descendant fragments the child
    /// is never drawn (WPT fixedpos-009: a `<div class="pencil"
    /// style="width:36px; height:36px">` inside the fixed root never
    /// renders, leaving every page blank where the pencil should be).
    #[test]
    fn position_fixed_emits_fragments_for_block_descendants() {
        // Use shrink-to-fit-friendly width / height on the fixed root
        // so Taffy gives it a definite size; otherwise an unsized fixed
        // root inherits available_space (600 wide), which obscures the
        // root-vs-child position relationship the test pins. The bug
        // being tested (descendants missing from geometry) is
        // independent of root sizing.
        let html = r#"
            <html><body style="margin:0">
              <div style="position: fixed; bottom: 0; width: 36px; height: 36px">
                <div style="width: 36px; height: 36px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        crate::blitz_adapter::relayout_position_fixed(&mut doc, 600.0, 800.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_fixed_fragments(&mut geom, doc.deref_mut(), 2, 600.0, 800.0);

        // Both root and its 36×36 child must appear in `geom`. The
        // root's fragments come from the existing inset-resolution
        // path; the child's fragments come from the new descendants
        // walker (fulgur-4m16). Without that walker, only one entry
        // (the root) shows up.
        let entries: Vec<_> = geom
            .iter()
            .filter(|(_, g)| {
                g.fragments.iter().any(|f| {
                    (f.width.to_f32() - 36.0).abs() < 0.5 && (f.height.to_f32() - 36.0).abs() < 0.5
                })
            })
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "expected 2 entries (fixed root + child), got {} — fulgur-4m16: \
             without record_fixed_subtree_descendants the child entry is missing",
            entries.len(),
        );

        // Both must be `is_repeat = true` with one fragment per page.
        for (_, g) in &entries {
            assert!(g.is_repeat, "entry must be is_repeat=true");
            assert_eq!(g.fragments.len(), 2, "entry: one fragment per page");
            let pages_seen: Vec<u32> = g.fragments.iter().map(|f| f.page_index).collect();
            assert_eq!(pages_seen, vec![0u32, 1u32]);
        }

        // Both entries' first fragments must agree on (x, y) because
        // the child's `final_layout.location` inside the root is
        // (0, 0) (no padding/margin/border on the root). A divergence
        // here would mean the descendants walker is computing offsets
        // wrong.
        let f0 = &entries[0].1.fragments[0];
        let f1 = &entries[1].1.fragments[0];
        let f0x = f0.x.to_f32();
        let f0y = f0.y.to_f32();
        let f1x = f1.x.to_f32();
        let f1y = f1.y.to_f32();
        assert!(
            (f0x - f1x).abs() < 0.5 && (f0y - f1y).abs() < 0.5,
            "root and child must share (x, y); got root=({f0x},{f0y}) child=({f1x},{f1y})",
        );

        // Pin the y coordinate: bottom:0 with height=36 in an 800px
        // viewport places the box top at y=764. body is empty so
        // body_offset_xy=(0,0).
        assert!(
            (f0y - 764.0).abs() < 1.0,
            "bottom:0 fixed (h=36) must resolve to y=764 (viewport_h - h); got {f0y}",
        );
    }

    /// fulgur-a8m5: body-direct `position: absolute` with `bottom: 0`
    /// should land at the bottom of page 0 with its descendant Paragraph
    /// fragments at the same position. The fragmenter unconditionally
    /// skips out-of-flow children, so this pass is the only thing that
    /// puts these abs body-direct nodes into `pagination_geometry` for
    /// v2 dispatch (WPT fixedpos-001 ref, fixedpos-008 ref).
    #[test]
    fn position_absolute_body_direct_bottom_zero_lands_on_page_zero() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: absolute; bottom: 0; height: 30px">x</div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            1,
            600.0,
            800.0,
            None,
        );

        // Locate the abs div by height=30.
        let mut found = None;
        for g in geom.values() {
            for f in &g.fragments {
                if (f.height.to_f32() - 30.0).abs() < 0.5 {
                    found = Some(f.clone());
                }
            }
        }
        let frag = found.expect("abs body-direct fragment for bottom:0 must be emitted");
        assert_eq!(frag.page_index, 0);
        // Same math as the fixed case: body has zero height, so
        // body_offset compensation is a no-op and viewport-CB
        // resolution gives y = 800 - 30 = 770.
        let frag_y = frag.y.to_f32();
        assert!(
            (frag_y - 770.0).abs() < 1.0,
            "abs body-direct bottom:0 should land at y=770; got {frag_y}",
        );
    }

    /// fulgur-z4zc: a body-direct `position:absolute` subtree whose
    /// viewport-CB y range crosses page boundaries must emit geometry on
    /// every intersected page. The main fragmenter skips absolute OOF
    /// children, so `append_position_absolute_body_direct_fragments` is
    /// responsible for recording these page-local placements.
    #[test]
    fn position_absolute_body_direct_overflow_emits_fragments_on_each_page() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: absolute; top: 0; width: 100px; height: 1800px">
                <div style="width: 50px; height: 1800px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            1,
            600.0,
            800.0,
            None,
        );

        let mut tall_entries: Vec<Vec<u32>> = geom
            .values()
            .filter(|g| {
                g.fragments
                    .iter()
                    .any(|f| (f.height.to_f32() - 1800.0).abs() < 0.5)
            })
            .map(|g| {
                let mut pages: Vec<u32> = g.fragments.iter().map(|f| f.page_index).collect();
                pages.sort_unstable();
                pages
            })
            .collect();
        tall_entries.sort();

        assert_eq!(
            tall_entries,
            vec![vec![0, 1, 2], vec![0, 1, 2]],
            "expected absolute root and in-flow child to emit fragments on pages 0, 1, and 2; got {tall_entries:?}"
        );
    }

    /// fulgur-2m6w: a body-direct `position:absolute` node positioned far
    /// beyond the page budget (`top: 99999999px`) must not extend the page
    /// count without bound. The abs path emits one fragment per intersected
    /// page and `descendant_total_pages` feeds `implied_page_count`, so an
    /// unclamped page index from a tiny box makes `render_v2` allocate and
    /// render ~10^5 pages — the same small-input DoS the body-direct slice
    /// cap blocks, reached via the absolute-positioning path (Codex review
    /// on PR #501). Without the clamp this input lands at page ~125000;
    /// page indices are clamped to `MAX_PAGES`.
    #[test]
    fn position_absolute_far_offset_is_page_capped() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: absolute; top: 99999999px; width: 10px; height: 10px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            1,
            600.0,
            800.0,
            None,
        );
        // The fragment must still be emitted (not silently dropped) — just
        // pinned to the cap page rather than page ~125000.
        let max_page = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .map(|f| f.page_index)
            .max()
            .expect("abs fragment must be emitted");
        assert!(
            max_page <= crate::MAX_PAGES,
            "abs page index must be clamped to MAX_PAGES ({}), got {max_page}",
            crate::MAX_PAGES,
        );
    }

    /// fulgur-2m6w (Codex review on PR #501): the `MAX_PAGES` clamp must
    /// apply ONLY to the page-extension path. When `total_pages` legitimately
    /// exceeds `MAX_PAGES` (many ordinary in-flow pages — input-proportional,
    /// not amplified), a NON-extending absolute element starting at an
    /// in-budget page above the cap must emit a single fragment at its real
    /// page, not a spurious run from `MAX_PAGES` through the real page.
    #[test]
    fn position_absolute_in_budget_start_above_cap_not_clamped() {
        let page_h = 800.0_f32;
        let start_page = crate::MAX_PAGES + 20_000; // > MAX_PAGES
        let total_pages = start_page + 5; // in-budget
        let top = page_h * start_page as f32;
        // In-flow `<p>` makes `body_has_in_flow_content` true, so
        // `may_extend_pages` is false → the non-extending branch.
        let html = format!(
            r#"<html><body style="margin:0">
                 <p>in-flow</p>
                 <div style="position:absolute; top:{top}px; width:10px; height:10px"></div>
               </body></html>"#
        );
        let mut doc = parse(&html, 600.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            total_pages,
            600.0,
            page_h,
            None,
        );
        // Isolate the 10px-wide abs node's fragments.
        let abs_pages: Vec<u32> = geom
            .values()
            .filter(|g| {
                g.fragments
                    .iter()
                    .any(|f| (f.width.to_f32() - 10.0).abs() < 0.5)
            })
            .flat_map(|g| g.fragments.iter().map(|f| f.page_index))
            .collect();
        assert_eq!(
            abs_pages,
            vec![start_page],
            "non-extending in-budget abs must emit only at its real page \
             (no spurious clamp run), got {abs_pages:?}",
        );
    }

    #[test]
    fn position_absolute_body_direct_expanded_pages_reach_later_text() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: absolute; top: 0; width: 100%; background: yellow">
                <div style="contain: size; width: 50px; height: 1800px"></div>
                This text should land after the contained child.
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            1,
            600.0,
            800.0,
            None,
        );

        let has_later_text_fragment = geom.values().any(|g| {
            g.fragments.iter().any(|f| {
                f.page_index == 1
                    && f.height > crate::units::Px::ZERO
                    && f.height < 100.0_f32.as_px()
            })
        });

        assert!(
            has_later_text_fragment,
            "expected later text/anonymous fragment on page 1; got {geom:?}"
        );
    }

    #[test]
    fn position_fixed_repeats_on_pages_added_by_body_direct_absolute() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: fixed; top: 0; width: 10px; height: 20px"></div>
              <div style="position: absolute; top: 0; width: 100px; height: 1200px"></div>
            </body></html>
        "#;
        let engine = crate::Engine::builder().build();
        let (_, geom) = engine.build_drawables_and_geometry_for_testing_no_gcpm(html);

        let fixed_pages = geom
            .values()
            .find(|g| {
                g.is_repeat
                    && g.fragments.iter().any(|f| {
                        (f.width.to_f32() - 10.0).abs() < 0.5
                            && (f.height.to_f32() - 20.0).abs() < 0.5
                    })
            })
            .map(|g| {
                let mut pages: Vec<u32> = g.fragments.iter().map(|f| f.page_index).collect();
                pages.sort_unstable();
                pages
            });

        assert_eq!(fixed_pages, Some(vec![0, 1]));
    }

    #[test]
    fn fixed_subtree_fragment_total_is_bounded() {
        // Security regression: a `position: fixed` root and every in-flow
        // descendant each emit one fragment per page → O(nodes × pages).
        // The aggregate MAX_SUBTREE_PAGE_FRAGMENTS budget caps the retained
        // total regardless of how many descendants the subtree has.
        let mut kids = String::new();
        for _ in 0..20_000 {
            kids.push_str(r#"<div style="height:1px">x</div>"#);
        }
        let html = format!(
            r#"<html><body style="margin:0">
              <div style="position: fixed; top:0; width:50px; height:50px">{kids}</div>
              <div style="height:100000px"></div>
            </body></html>"#
        );
        let engine = crate::Engine::builder().build();
        let (_, geom) = engine.build_drawables_and_geometry_for_testing_no_gcpm(&html);
        let total: usize = geom
            .values()
            .filter(|g| g.is_repeat)
            .map(|g| g.fragments.len())
            .sum();
        assert!(
            total <= crate::MAX_SUBTREE_PAGE_FRAGMENTS,
            "fixed fragment total must be bounded, got {total}"
        );
    }

    #[test]
    fn absolute_subtree_fragment_total_is_bounded() {
        // Security regression (same class as fixed-subtree fragments): the
        // body-direct absolute pass records, for every node in each absolute
        // subtree, one fragment per page the node intersects
        // (`first_page..=last_page`, bounded per-node by MAX_PAGES). Many
        // page-spanning absolute elements amplify to O(nodes × pages). The
        // aggregate MAX_SUBTREE_PAGE_FRAGMENTS budget caps the retained total.
        let mut sibs = String::new();
        for _ in 0..15_000 {
            sibs.push_str(
                r#"<div style="position:absolute; top:0; width:10px; height:200000px"></div>"#,
            );
        }
        let html = format!(r#"<html><body style="margin:0">{sibs}</body></html>"#);
        let engine = crate::Engine::builder().build();
        let (_, geom) = engine.build_drawables_and_geometry_for_testing_no_gcpm(&html);
        let total: usize = geom.values().map(|g| g.fragments.len()).sum();
        // The absolute pass is capped at MAX_SUBTREE_PAGE_FRAGMENTS; the sum
        // also counts incidental non-absolute fragments (root/body/in-flow),
        // which are themselves bounded by MAX_PAGES (a single in-flow flow).
        // Uncapped this scenario emits ~3M fragments, so the budget clearly
        // fired.
        assert!(
            total <= crate::MAX_SUBTREE_PAGE_FRAGMENTS + crate::MAX_PAGES as usize,
            "absolute fragment total must be bounded, got {total}"
        );
    }

    #[test]
    fn position_absolute_body_direct_tiny_overflow_reaches_next_page() {
        let html = r#"
            <html><body style="margin:0">
              <div id="tiny" style="position: absolute; top: 0; width: 100px; height: 801px"></div>
              <div id="exact" style="position: absolute; top: 0; width: 100px; height: 800px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        // Reuses the module-level `find_by_id` helper (see the
        // pagination-cap tests) instead of redefining it here.
        let tiny_id = find_by_id(doc.deref_mut(), "tiny").expect("tiny abs node");
        doc.deref_mut()
            .get_node_mut(tiny_id)
            .expect("tiny abs node")
            .final_layout
            .size
            .height = 800.1;
        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            2,
            600.0,
            800.0,
            None,
        );

        let mut tiny_overflow_pages = None;
        let mut exact_boundary_pages = None;
        for g in geom.values() {
            if g.fragments
                .iter()
                .any(|f| (f.height.to_f32() - 800.1).abs() < 0.05)
            {
                let mut pages: Vec<u32> = g.fragments.iter().map(|f| f.page_index).collect();
                pages.sort_unstable();
                tiny_overflow_pages = Some(pages);
            }
            if g.fragments
                .iter()
                .any(|f| (f.height.to_f32() - 800.0).abs() < 0.05)
            {
                let mut pages: Vec<u32> = g.fragments.iter().map(|f| f.page_index).collect();
                pages.sort_unstable();
                exact_boundary_pages = Some(pages);
            }
        }

        assert_eq!(tiny_overflow_pages, Some(vec![0, 1]));
        assert_eq!(exact_boundary_pages, Some(vec![0]));
    }

    #[test]
    fn position_absolute_body_direct_height_overflow_extends_to_last_page() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: absolute; top: 0; width: 100px; height: 1px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let abs_id = find_node_by_local_name(&doc, "div").expect("abs div");
        doc.deref_mut()
            .get_node_mut(abs_id)
            .expect("abs div")
            .final_layout
            .size
            .height = f32::MAX;
        let mut geom = PaginationGeometryTable::new();
        super::record_subtree_fragments_at_offset(
            &mut geom,
            doc.deref_mut(),
            abs_id,
            (0.0, f32::MAX),
            (0.0, 0.0),
            f32::MAX,
            f32::MAX,
            3,
            true,
            &mut 0,
        );

        let pages: Vec<u32> = geom
            .get(&abs_id)
            .expect("abs div geometry")
            .fragments
            .iter()
            .map(|f| f.page_index)
            .collect();
        assert_eq!(pages, vec![1, 2]);
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
            super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table)
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
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);

        // Filter to fragments whose width is exactly the cell width
        // (100) — that's only the two cards. Grid container has
        // width 200 (two columns), html / body have viewport width.
        let card_y: Vec<f32> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                f.page_index == 0
                    && (f.height.to_f32() - 100.0).abs() < 0.5
                    && (f.width.to_f32() - 100.0).abs() < 0.5
            })
            .map(|f| f.y.to_f32())
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

    #[test]
    fn fragment_block_subtree_following_block_continues_after_split_child_tail() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 220px;">
                <div style="height: 100px; width: 200px"></div>
                <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                </div>
                <h2 style="height: 30px; width: 200px; margin: 0">after grid</h2>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 250.0_f32.as_px(), &table);

        let mut candidates: Vec<_> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| f.page_index == 1 && (f.height.to_f32() - 30.0).abs() < 0.5)
            .collect();
        candidates.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());
        let h2 = candidates
            .first()
            .expect("expected the trailing h2 to land on page 1");
        let h2_y = h2.y.to_f32();
        assert!(
            h2_y >= 100.0,
            "block sibling after split grid must continue after the grid tail; got y={h2_y} (pre-fix: y=0 overlaps the tail)",
        );
    }

    /// fulgur-2m6w: a nested child with a non-finite Taffy height must not
    /// poison `cursor_y` inside the recursive splitter. The grid pattern
    /// routes the section through `fragment_block_subtree` (250px content vs
    /// 150px strip); a `+inf` height injected into one cell is sanitized to
    /// zero so the recursion neither panics nor produces a non-finite /
    /// unbounded page count, and the node is still recorded in geometry.
    #[test]
    fn fragment_block_subtree_non_finite_child_height_is_sanitized() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 220px;">
                <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                  <div id="bad" style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                </div>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let bad = find_by_id(doc.deref_mut(), "bad").expect("div#bad");
        doc.deref_mut()
            .get_node_mut(bad)
            .expect("div#bad")
            .final_layout
            .size
            .height = f32::INFINITY;
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 150.0_f32.as_px(), &table);
        // Load-bearing assertion: without the guard the injected `+inf`
        // reaches a Fragment height and poisons `cursor_y`, so emitted
        // fragments carry non-finite `y` / `height`. The guard zeroes it,
        // keeping every emitted coordinate finite.
        for (id, g) in geom.iter() {
            for f in &g.fragments {
                let fy = f.y.to_f32();
                let fh = f.height.to_f32();
                assert!(
                    fy.is_finite() && fh.is_finite(),
                    "node {id}: non-finite fragment y={fy} height={fh} leaked through \
                     fragment_block_subtree",
                );
            }
        }
        assert!(
            geom.contains_key(&bad),
            "nested non-finite-height node must still be recorded",
        );
    }

    #[test]
    fn fragment_block_subtree_following_block_continues_after_split_flex_tail() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 220px;">
                <div style="height: 100px; width: 200px"></div>
                <div style="display: flex; flex-wrap: wrap; width: 200px;">
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                </div>
                <h2 style="height: 30px; width: 200px; margin: 0">after flex</h2>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 250.0_f32.as_px(), &table);

        let mut candidates: Vec<_> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| f.page_index == 1 && (f.height.to_f32() - 30.0).abs() < 0.5)
            .collect();
        candidates.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());
        let h2 = candidates
            .first()
            .expect("expected the trailing h2 to land on page 1");
        let h2_y = h2.y.to_f32();
        assert!(
            h2_y >= 100.0,
            "block sibling after split flex must continue after the flex tail; got y={h2_y} (pre-fix: y=0 overlaps the tail)",
        );
    }

    #[test]
    fn fragment_block_subtree_grid_later_row_parallel_siblings_share_page_y() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 220px;">
                <div style="height: 100px; width: 200px"></div>
                <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                </div>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 250.0_f32.as_px(), &table);

        let mut cells: Vec<_> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                (f.height.to_f32() - 100.0).abs() < 0.5 && (f.width.to_f32() - 100.0).abs() < 0.5
            })
            .map(|f| (f.page_index, f.x.to_f32(), f.y.to_f32()))
            .collect();
        cells.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let page_one_cells: Vec<_> = cells.iter().filter(|(p, _, _)| *p == 1).collect();
        assert_eq!(
            page_one_cells.len(),
            2,
            "expected the second grid row's two cells on page 1, got {cells:?}"
        );
        assert!(
            (page_one_cells[0].2 - page_one_cells[1].2).abs() < 0.5,
            "parallel cells in the same later grid row must share y; got {cells:?}"
        );
    }

    #[test]
    fn fragment_block_subtree_flex_later_row_parallel_siblings_share_page_y() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 220px;">
                <div style="height: 100px; width: 200px"></div>
                <div style="display: flex; flex-wrap: wrap; width: 200px;">
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                </div>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 250.0_f32.as_px(), &table);

        let mut cells: Vec<_> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                (f.height.to_f32() - 100.0).abs() < 0.5 && (f.width.to_f32() - 100.0).abs() < 0.5
            })
            .map(|f| (f.page_index, f.x.to_f32(), f.y.to_f32()))
            .collect();
        cells.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let page_one_cells: Vec<_> = cells.iter().filter(|(p, _, _)| *p == 1).collect();
        assert_eq!(
            page_one_cells.len(),
            2,
            "expected the second flex row's two cells on page 1, got {cells:?}"
        );
        assert!(
            (page_one_cells[0].2 - page_one_cells[1].2).abs() < 0.5,
            "parallel cells in the same later flex row must share y; got {cells:?}"
        );
    }

    #[test]
    fn fragment_block_subtree_grid_later_row_preserves_parallel_sibling_offset() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 220px;">
                <div style="height: 100px; width: 200px"></div>
                <div style="display: grid; grid-template-columns: 100px 100px; align-items: start; width: 200px;">
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 80px; width: 100px; margin-top: 20px"></div>
                  <div style="height: 100px; width: 100px"></div>
                </div>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 250.0_f32.as_px(), &table);

        let mut page_one_cells: Vec<_> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| f.page_index == 1 && (f.width.to_f32() - 100.0).abs() < 0.5)
            .map(|f| (f.height.to_f32(), f.x.to_f32(), f.y.to_f32()))
            .collect();
        page_one_cells.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let unshifted = page_one_cells
            .iter()
            .find(|(h, _, _)| (*h - 100.0).abs() < 0.5)
            .expect("expected unshifted second-row grid cell on page 1");
        assert!(
            (unshifted.2 + 20.0).abs() < 0.5,
            "same-row sibling must preserve its cross-axis offset relative to the split row; got {page_one_cells:?}"
        );
    }

    #[test]
    fn fragment_block_subtree_flex_later_row_preserves_parallel_sibling_offset() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 220px;">
                <div style="height: 100px; width: 200px"></div>
                <div style="display: flex; flex-wrap: wrap; align-items: flex-start; width: 200px;">
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 100px; width: 100px"></div>
                  <div style="height: 80px; width: 100px; margin-top: 20px"></div>
                  <div style="height: 100px; width: 100px"></div>
                </div>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 250.0_f32.as_px(), &table);

        let mut page_one_cells: Vec<_> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| f.page_index == 1 && (f.width.to_f32() - 100.0).abs() < 0.5)
            .map(|f| (f.height.to_f32(), f.x.to_f32(), f.y.to_f32()))
            .collect();
        page_one_cells.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let unshifted = page_one_cells
            .iter()
            .find(|(h, _, _)| (*h - 100.0).abs() < 0.5)
            .expect("expected unshifted second-row flex item on page 1");
        assert!(
            (unshifted.2 + 20.0).abs() < 0.5,
            "same-row sibling must preserve its cross-axis offset relative to the split row; got {page_one_cells:?}"
        );
    }

    #[test]
    fn fragment_block_subtree_grid_row_recursive_cells_share_page_y() {
        // 2 cell の grid row で、両 cell が inner content を持ち
        // recursion 経路を通る (`needs_recursion=true`) case。両 cell
        // が page 内に収まるサイズなので page boundary は跨がない。
        //
        // pre-fix: 右 cell の recursion が parent の `cursor_y`
        // (= 左 cell の bottom = 200) を引きずり、右 column の inner
        // div が y=200 (block flow stacking) に積まれる。
        //
        // post-fix: 両 cell とも row top y=0 から開始し、各 inner div
        // が y=0..50, y=50..100 に並ぶ。
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div style="width: 100px">
                  <div style="height: 50px; width: 100px"></div>
                  <div style="height: 50px; width: 100px"></div>
                </div>
                <div style="width: 100px">
                  <div style="height: 50px; width: 100px"></div>
                  <div style="height: 50px; width: 100px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);

        // 100×50 の inner div fragment 4 個を集める。
        let mut inner: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                (f.width.to_f32() - 100.0).abs() < 0.5 && (f.height.to_f32() - 50.0).abs() < 0.5
            })
            .map(|f| (f.page_index, f.x.to_f32(), f.y.to_f32()))
            .collect();
        inner.sort_by(|a, b| a.partial_cmp(b).unwrap());

        assert_eq!(
            inner.len(),
            4,
            "expected 4 inner divs (2 per column × 2 columns), got inner={inner:?}"
        );

        // すべて page 0 に収まる
        assert!(
            inner.iter().all(|(p, _, _)| *p == 0),
            "all 4 inner divs must be on page 0, inner={inner:?}"
        );

        // 左 column (x=0): y=0 と y=50 の 2 個
        let left_ys: Vec<f32> = inner
            .iter()
            .filter(|(_, x, _)| x.abs() < 0.5)
            .map(|(_, _, y)| *y)
            .collect();
        let approx_eq = |a: &[f32], b: &[f32]| -> bool {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.5)
        };
        assert!(
            approx_eq(&left_ys, &[0.0, 50.0]),
            "left column y, inner={inner:?}"
        );

        // 右 column (x=100): y=0 と y=50 の 2 個 (block flow になっていれば
        // y=100, y=150 になる)
        let right_ys: Vec<f32> = inner
            .iter()
            .filter(|(_, x, _)| (x - 100.0).abs() < 0.5)
            .map(|(_, _, y)| *y)
            .collect();
        assert!(
            approx_eq(&right_ys, &[0.0, 50.0]),
            "right column y must match left column (parallel siblings, not stacked), inner={inner:?}"
        );
    }

    #[test]
    fn fragment_block_subtree_flex_row_recursive_cells_share_page_y() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="display: flex; width: 200px;">
                <div style="width: 100px; flex: 0 0 100px">
                  <div style="height: 50px; width: 100px"></div>
                  <div style="height: 50px; width: 100px"></div>
                </div>
                <div style="width: 100px; flex: 0 0 100px">
                  <div style="height: 50px; width: 100px"></div>
                  <div style="height: 50px; width: 100px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);

        let mut inner: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                (f.width.to_f32() - 100.0).abs() < 0.5 && (f.height.to_f32() - 50.0).abs() < 0.5
            })
            .map(|f| (f.page_index, f.x.to_f32(), f.y.to_f32()))
            .collect();
        inner.sort_by(|a, b| a.partial_cmp(b).unwrap());

        assert_eq!(
            inner.len(),
            4,
            "expected 4 inner divs (2 per column × 2 columns), got inner={inner:?}"
        );
        assert!(
            inner.iter().all(|(p, _, _)| *p == 0),
            "all 4 inner divs must be on page 0, inner={inner:?}"
        );

        let left_ys: Vec<f32> = inner
            .iter()
            .filter(|(_, x, _)| x.abs() < 0.5)
            .map(|(_, _, y)| *y)
            .collect();
        let right_ys: Vec<f32> = inner
            .iter()
            .filter(|(_, x, _)| (x - 100.0).abs() < 0.5)
            .map(|(_, _, y)| *y)
            .collect();
        let approx_eq = |a: &[f32], b: &[f32]| -> bool {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.5)
        };
        assert!(
            approx_eq(&left_ys, &[0.0, 50.0]),
            "left column y, inner={inner:?}"
        );
        assert!(
            approx_eq(&right_ys, &[0.0, 50.0]),
            "right column y must match left column (parallel siblings, not stacked), inner={inner:?}"
        );
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
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
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
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);

        // Find every fragment with height ≈ 100 on page 1; B is the
        // only such fragment (outer's page-1 fragment height is the
        // total parent strip, which equals 100 after the fix because
        // only B sits on page 1; outer's page-0 fragment carries
        // A + gap = 120; A is on page 0).
        let b_on_page1: Vec<&Fragment> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| f.page_index == 1 && (f.height.to_f32() - 100.0).abs() < 0.5)
            .collect();
        assert!(
            !b_on_page1.is_empty(),
            "expected B fragment on page 1, geom={geom:?}"
        );
        for f in &b_on_page1 {
            let fy = f.y.to_f32();
            assert!(
                fy.abs() < 0.5,
                "B should land at y=0 on the new page (forced break discards \
                 the inter-child gap), but got y={fy} (gap leaked through \
                 break-before — see Devin Review on PR #285). frag={f:?}",
            );
        }
    }

    #[test]
    fn body_level_break_before_preserves_own_top_margin_on_new_page() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 40px; margin: 0"></div>
              <div style="height: 90px; margin-top: 10px; break-before: page"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        let second_on_page1: Vec<&Fragment> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| f.page_index == 1 && (f.height.to_f32() - 90.0).abs() < 0.5)
            .collect();
        assert_eq!(
            second_on_page1.len(),
            1,
            "expected only the second child on page 1, geom={geom:?}"
        );
        assert!(
            (second_on_page1[0].y.to_f32() - 10.0).abs() < 0.5,
            "body-level break-before should keep the element's own top margin on the new page; geom={geom:?}"
        );
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
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 1.0_f32.as_px(),
            height: 1.0_f32.as_px(),
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
        assert!((frags[0].height.to_f32() - 150.0).abs() < 0.01);
        // Second fragment: lines 2-3 (height = 150 in para-local).
        assert!((frags[1].height.to_f32() - 150.0).abs() < 0.01);
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
        assert!((frags[0].height.to_f32() - 225.0).abs() < 0.01);
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
            .find(|g| (g.fragments[0].height.to_f32() - 1000.0).abs() < 1.0)
            .expect("oversized child fragment");
        assert_eq!(oversize.fragments.len(), 1);
        assert_eq!(oversize.fragments[0].page_index, 0);
    }

    /// fulgur-yb27: `fragment_block_subtree` must walk `layout_children`
    /// so anonymous block wrappers Stylo synthesizes around inline-
    /// level siblings (CSS 2.1 §9.2.1.1) are visited. Without this,
    /// a tail inline string after a tall block sibling never
    /// fragments to the next page — the block consumes page 1, the
    /// trailing inline run is treated as a zero-height text node
    /// (`final_layout` defaults), and pagination terminates with a
    /// single fragment.
    #[test]
    fn fragment_block_subtree_walks_layout_children_for_anon_block_synthesis() {
        // Outer wrapper > [tall block sibling, trailing inline text].
        // Stylo wraps the trailing text in an anon block whose Taffy
        // location.y is past the page boundary.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="width: 200px;">
                <div style="height: 300px; width: 50px; background:hotpink;"></div>
                trailing tail
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 200.0_f32.as_px(), &table);
        // Page count must be ≥ 2 — without yb27 the trailing inline
        // text never reaches a new page (single fragment, page 0 only).
        let max_page = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .map(|f| f.page_index)
            .max()
            .unwrap_or(0);
        assert!(
            max_page >= 1,
            "yb27: anon block from mixed inline/block siblings must \
             paginate to a new page; max_page={max_page}",
        );
    }

    /// fulgur-oc51: when `fragment_block_subtree`'s recursion advances
    /// `page_index`, the parent's pre-recursion-page span must be
    /// recorded as a fragment. Without this, a tall nested subtree
    /// that crosses pages inside the recursion lifts the parent's
    /// only fragment to the *last* page (line 1799 close), leaving
    /// page 1 with no parent paint at all (background/borders gone).
    #[test]
    fn fragment_block_subtree_emits_parent_fragment_when_recursion_crosses_page() {
        // Two-deep nesting: outer (with background) > inner > [tall
        // child, trailing inline]. Inner's recursion will cross the
        // page boundary; outer (the <section>) must still get a
        // fragment on page 0.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 200px;">
                <div style="width: 200px;">
                  <div style="height: 300px; width: 50px; background:hotpink;"></div>
                  tail
                </div>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        // Locate the <section> node id explicitly so the assertion
        // targets the specific block that should keep a page-0
        // fragment — without this, a wide body fragment on page 0
        // would satisfy a generic "any wide page-0 fragment" check
        // even when the section itself has no page-0 entry.
        let section_id =
            find_node_by_local_name(&doc, "section").expect("fixture must contain a <section>");
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 200.0_f32.as_px(), &table);
        let section_geom = geom.get(&section_id).unwrap_or_else(|| {
            panic!("oc51: <section> (node_id={section_id}) missing from geometry; geom={geom:?}")
        });
        let has_page_0 = section_geom.fragments.iter().any(|f| f.page_index == 0);
        assert!(
            has_page_0,
            "oc51: <section> (node_id={section_id}) must keep a page-0 \
             fragment when its nested recursion crosses a page boundary; \
             section_fragments={:?}",
            section_geom.fragments,
        );
        // Must also reach page 1+ — otherwise the test isn't actually
        // exercising the recursion-cross-page path.
        let max_page = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .map(|f| f.page_index)
            .max()
            .unwrap_or(0);
        assert!(
            max_page >= 1,
            "oc51: fixture must paginate to ≥ 2 pages to exercise the \
             recursion-cross-page path; max_page={max_page}",
        );
    }

    /// Locate the first node in the document whose element local name
    /// matches `tag`. Used by tests that need a node_id reference for
    /// a specific HTML element without depending on Stylo's internal
    /// node numbering.
    fn find_node_by_local_name(doc: &blitz_html::HtmlDocument, tag: &str) -> Option<usize> {
        use std::ops::Deref;
        let base: &blitz_dom::BaseDocument = doc.deref();
        let root_id = base.root_element().id;
        fn walk(base: &blitz_dom::BaseDocument, id: usize, tag: &str) -> Option<usize> {
            let node = base.get_node(id)?;
            if let Some(elem) = node.element_data()
                && elem.name.local.as_ref() == tag
            {
                return Some(id);
            }
            for &child_id in &node.children {
                if let Some(found) = walk(base, child_id, tag) {
                    return Some(found);
                }
            }
            None
        }
        walk(base, root_id, tag)
    }

    /// fulgur-puml: `explicit_inset_axes` must classify each axis as
    /// explicit (length/percentage inset present) vs auto, since the nested-
    /// abs base selection depends on it. A silent regression here would
    /// mis-anchor abs descendants across a static intermediate.
    #[test]
    fn explicit_inset_axes_classifies_per_axis() {
        use std::ops::Deref;
        // (fragment, (x_explicit, y_explicit))
        let cases: &[(&str, (bool, bool))] = &[
            ("<div style=\"position:absolute\"></div>", (false, false)),
            (
                "<div style=\"position:absolute; top:10px\"></div>",
                (false, true),
            ),
            (
                "<div style=\"position:absolute; right:0\"></div>",
                (true, false),
            ),
            (
                "<div style=\"position:absolute; left:0; right:0\"></div>",
                (true, false),
            ),
            (
                "<div style=\"position:absolute; top:0; left:0\"></div>",
                (true, true),
            ),
        ];
        for (frag, expected) in cases {
            let html = format!("<html><body>{frag}</body></html>");
            let doc = parse(&html, 600.0);
            let id = find_node_by_local_name(&doc, "div").expect("div present");
            let node = doc.deref().get_node(id).expect("node present");
            assert_eq!(explicit_inset_axes(node), *expected, "fragment: {frag}");
        }
    }

    /// coderabbit: body containing only a `position: running()` element
    /// plus a tall `position: absolute` div must treat the body as
    /// having no in-flow content so `may_extend_pages = true`.
    /// Without the running_store guard, the running child increments
    /// `body_has_in_flow_content` and truncates the abs subtree.
    #[test]
    fn position_absolute_body_direct_running_only_body_extends_pages() {
        use crate::blitz_adapter;
        use crate::gcpm::parser::parse_gcpm;
        use std::sync::Arc;

        let css = ".header { position: running(pageHeader); }";
        let html = r#"<!DOCTYPE html>
<html><head></head>
<body style="margin:0">
<div class="header">Doc Header</div>
<div style="position: absolute; top: 0; width: 100px; height: 1800px">x</div>
</body></html>"#;

        let gcpm = parse_gcpm(css);
        let fonts: Vec<Arc<Vec<u8>>> = Vec::new();
        let mut doc = blitz_adapter::parse(html, 600.0, &fonts);
        let pass = blitz_adapter::RunningElementPass::new(gcpm.running_mappings.clone());
        let pass_ctx = blitz_adapter::PassContext { font_data: &fonts };
        blitz_adapter::apply_single_pass(&pass, &mut doc, &pass_ctx);
        let store = pass.into_running_store();
        blitz_adapter::resolve(&mut doc);

        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            1,
            600.0,
            800.0,
            Some(&store),
        );

        let max_page = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| (f.height.to_f32() - 1800.0).abs() < 0.5)
            .map(|f| f.page_index)
            .max();
        assert!(
            max_page.is_some_and(|p| p >= 2),
            "tall abs div in running-only body should extend to page 2; max_page={max_page:?}"
        );
    }

    /// coderabbit: abs subtree starting beyond the current page budget
    /// must still emit fragments when `may_extend_pages` is true.
    /// Regression for the condition `first_page_f < total_pages` that
    /// blocked fragment emission even when the absolute pass is
    /// responsible for extending the page count.
    #[test]
    fn position_absolute_body_direct_beyond_page_budget_extends_pages() {
        let html = r#"
            <html><body style="margin:0">
              <div style="position: absolute; top: 1600px; width: 100px; height: 100px">x</div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let mut geom = PaginationGeometryTable::new();
        super::append_position_absolute_body_direct_fragments(
            &mut geom,
            doc.deref_mut(),
            1,
            600.0,
            800.0,
            None,
        );
        let max_page = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .map(|f| f.page_index)
            .max();
        assert!(
            max_page.is_some_and(|p| p >= 2),
            "abs div at top:1600px with 800px pages should land on page 2; max_page={max_page:?}"
        );
    }

    #[test]
    fn grid_row_leaf_cells_cosplit_across_page_boundary() {
        // 2-col grid, each leaf cell 60px tall.
        // spacer 80px pushes grid to y=80 on a 100px page.
        // pre-fix: cell 2 pushed to page 1 at y=0 (whole cell).
        // post-fix: both cells split — page 0 y=80..100 (20px),
        //           page 1 y=0..40 (40px).
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div style="height: 60px; width: 100px"></div>
                <div style="height: 60px; width: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        // collect all 60px-tall, 100px-wide fragments (the two leaf cells)
        let mut frags: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                (f.height.to_f32() - 60.0).abs() < 0.5 && (f.width.to_f32() - 100.0).abs() < 0.5
            })
            .map(|f| (f.page_index, f.x.to_f32(), f.y.to_f32()))
            .collect();
        frags.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let on_page0: Vec<_> = frags.iter().filter(|(p, _, _)| *p == 0).collect();
        assert_eq!(
            on_page0.len(),
            2,
            "both leaf cells must have a fragment on page 0 (co-split); frags={frags:?}"
        );
        let ys: Vec<f32> = on_page0.iter().map(|(_, _, y)| *y).collect();
        assert!(
            (ys[0] - ys[1]).abs() < 0.5,
            "page-0 fragments must share the same y (parallel row); ys={ys:?}"
        );
    }

    #[test]
    fn flex_row_leaf_cells_cosplit_across_page_boundary() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: flex; width: 200px;">
                <div style="height: 60px; width: 100px; flex: 0 0 100px"></div>
                <div style="height: 60px; width: 100px; flex: 0 0 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        let mut frags: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                (f.height.to_f32() - 60.0).abs() < 0.5 && (f.width.to_f32() - 100.0).abs() < 0.5
            })
            .map(|f| (f.page_index, f.x.to_f32(), f.y.to_f32()))
            .collect();
        frags.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let on_page0: Vec<_> = frags.iter().filter(|(p, _, _)| *p == 0).collect();
        assert_eq!(
            on_page0.len(),
            2,
            "both leaf cells must have a fragment on page 0 (co-split); frags={frags:?}"
        );
        let ys: Vec<f32> = on_page0.iter().map(|(_, _, y)| *y).collect();
        assert!(
            (ys[0] - ys[1]).abs() < 0.5,
            "page-0 fragments must share the same y; ys={ys:?}"
        );
    }

    #[test]
    fn grid_row_recursive_cells_cosplit_across_page_boundary() {
        // 2-col grid, each cell has 2 inner divs (40px each) = 80px total.
        // spacer 70px, page_height 100px → grid starts at y=70.
        // Row bottom = 70 + 80 = 150 > 100 → crosses page boundary.
        //
        // pre-fix: cell 2 recursion starts at page 1 cursor=0 (because
        //   cell 1's recursion advanced page_index to 1) → both inner
        //   divs of cell 2 land entirely on page 1.
        // post-fix: both cells start recursion at page 0 cursor=70 →
        //   first inner div (40px) splits at page boundary, landing
        //   partly on page 0 (y=70..100) and partly on page 1 (y=0..10).
        //   Specifically: both columns' first inner divs must appear on page 0.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 70px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div style="width: 100px">
                  <div style="height: 40px; width: 100px"></div>
                  <div style="height: 40px; width: 100px"></div>
                </div>
                <div style="width: 100px">
                  <div style="height: 40px; width: 100px"></div>
                  <div style="height: 40px; width: 100px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        // inner divs: 40px tall, 100px wide
        let mut inner: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                (f.height.to_f32() - 40.0).abs() < 0.5 && (f.width.to_f32() - 100.0).abs() < 0.5
            })
            .map(|f| (f.page_index, f.x.to_f32(), f.y.to_f32()))
            .collect();
        inner.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // Each column has 2 inner divs (40px each).
        // After the fix, both columns' FIRST inner div must appear on page 0
        // (at y≈70, since the grid row starts there).
        // Pre-fix: only cell 1's first inner div is on page 0; cell 2's
        // recursion starts from page 1 so its inner divs are at y=0,40 on page 1.
        let _first_divs_page0: Vec<_> = inner
            .iter()
            .filter(|(p, _, y)| *p == 0 && *y > 60.0)
            .collect();
        // The critical assertion: the RIGHT column's first inner div must also
        // appear on page 0 (at x=100, y≈70). Pre-fix: cell 2's recursion starts
        // from page 1, so its inner divs land at x=100 only on page 1.
        let right_col_page0: Vec<_> = inner
            .iter()
            .filter(|(p, x, y)| *p == 0 && (x - 100.0).abs() < 0.5 && *y > 60.0)
            .collect();
        assert!(
            !right_col_page0.is_empty(),
            "right column's first inner div must appear on page 0 at x=100 (pre-fix: only on page 1); inner={inner:?}"
        );
    }

    #[test]
    fn flex_row_recursive_cells_cosplit_across_page_boundary() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 70px"></div>
              <div style="display: flex; width: 200px;">
                <div style="width: 100px; flex: 0 0 100px">
                  <div style="height: 40px; width: 100px"></div>
                  <div style="height: 40px; width: 100px"></div>
                </div>
                <div style="width: 100px; flex: 0 0 100px">
                  <div style="height: 40px; width: 100px"></div>
                  <div style="height: 40px; width: 100px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        let mut inner: Vec<(u32, f32, f32)> = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .filter(|f| {
                (f.height.to_f32() - 40.0).abs() < 0.5 && (f.width.to_f32() - 100.0).abs() < 0.5
            })
            .map(|f| (f.page_index, f.x.to_f32(), f.y.to_f32()))
            .collect();
        inner.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let right_col_page0: Vec<_> = inner
            .iter()
            .filter(|(p, x, y)| *p == 0 && (x - 100.0).abs() < 0.5 && *y > 60.0)
            .collect();
        assert!(
            !right_col_page0.is_empty(),
            "right column's first inner div must appear on page 0 at x=100 (pre-fix: only on page 1); inner={inner:?}"
        );
    }
}
