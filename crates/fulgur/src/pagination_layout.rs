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
/// px→pt conversion ([`crate::units::Px::in_pt`]) before reaching Krilla.
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
///
/// # Inline-root decoration (fulgur-pgbrk R1)
///
/// `content_lead_in` / `content_lead_out` are non-zero only for an
/// inline root that the fragmenter split at line boundaries. Parley's
/// line metrics are **content-box relative** (the first line's
/// `min_coord` is `0.0` even under a `150px` `padding-top`), so the
/// fragmenter has to fold the box's own decoration back in to describe
/// the border box. Following `box-decoration-break: slice`
/// (CSS Fragmentation 3 §5.4, the initial value), `content_lead_in` is
/// carried by the **first** fragment only and `content_lead_out` by the
/// **last** — which is why they live here and not on [`Fragment`].
///
/// Consumers that partition *line boxes* by accumulating `frag.height`
/// (`render::paragraph_lines_for_page`) must subtract both back out
/// first, since `ShapedLine::height` covers line boxes only.
#[derive(Clone, Debug, Default)]
pub struct PaginationGeometry {
    pub fragments: Vec<Fragment>,
    pub is_repeat: bool,
    /// `border-top + padding-top`, included in the FIRST fragment's height.
    pub content_lead_in: crate::units::Px,
    /// `padding-bottom + border-bottom`, included in the LAST fragment's height.
    pub content_lead_out: crate::units::Px,
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

/// One fragment placed past the bottom of its page's content strip
/// (fulgur-pgbrk R3).
///
/// A fragment whose bottom edge falls below `page_height_px` is laid
/// out into the page's bottom margin — over any running footer — and,
/// if it clears the paper edge as well, is clipped away by the PDF
/// MediaBox and silently lost. Both outcomes are pagination defects:
/// CSS Fragmentation §4.1 permits monolithic content to overflow, but
/// it equally permits slicing, and fulgur already slices in the
/// body-direct path. Overflow is therefore always a fulgur
/// inconsistency, never a spec requirement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FragmentOverflow {
    pub node_id: usize,
    pub page_index: u32,
    /// px by which the fragment's bottom edge exceeds the content strip.
    pub overshoot_px: f32,
    /// The same node has a further fragment on a later page, so this
    /// fragment is a *slice* whose height should have been clipped to
    /// the strip. Distinguishes a fragment-height bookkeeping bug from
    /// genuinely unbreakable content that had nowhere to go.
    pub continues_on_later_page: bool,
}

/// Tolerance for the overflow test, matching the epsilon already used
/// inline elsewhere in this module.
const OVERFLOW_EPS_PX: f32 = 0.5;

/// Tolerance for the "is this leaf oversized" gate that decides whether
/// [`slice_oversized_leaf`] runs at all — see that function's doc
/// comment ("Oversize tolerance") for the 220pt→294px Taffy rounding
/// case this absorbs. Deliberately larger than [`OVERFLOW_EPS_PX`]:
/// that constant guards genuine strip-crossing overflow detection
/// (where a tighter epsilon is correct), while this one exists purely
/// to swallow px-rounding noise before a box is judged too tall for a
/// page at all. The two gates that decide whether to call
/// `slice_oversized_leaf` (`oversized` and `spills_strip` at this
/// module's flex/grid-cell call site) must share this tolerance —
/// using `OVERFLOW_EPS_PX` for one and not the other let a box in the
/// 0.5–1.0px gap defeat the tolerance via the other gate (Codex review,
/// PR #719).
const OVERSIZE_QUANTIZATION_TOLERANCE_PX: f32 = 1.0;

/// CSS 3 Fragmentation initial value for `orphans`.
const ORPHANS_INITIAL: usize = 2;
/// CSS 3 Fragmentation initial value for `widows`.
const WIDOWS_INITIAL: usize = 2;

/// Resolve the `orphans` / `widows` in effect at `node_id`, falling back
/// to the CSS initial value `2` for each (fulgur-pgbrk R6).
///
/// Both are **inherited** properties (CSS Fragmentation 3 §4.4), but
/// `ColumnStyleTable` is a sparse side-table with no inheritance: it
/// records only the elements an author actually wrote a declaration on.
/// Rather than densify the table — every descendant of a `<body>` with
/// `widows: 3` would need an entry — inheritance is resolved here, at the
/// one place that consumes these values, by walking up the ancestor
/// chain for the nearest declaration.
///
/// The walk costs O(depth) and runs only for inline roots that actually
/// reach the line-splitting path, so it is off the hot path for every
/// document that sets neither property. It bails at
/// [`crate::MAX_DOM_DEPTH`], matching `has_forced_break_below`.
fn resolved_line_constraints(
    doc: &BaseDocument,
    node_id: usize,
    column_styles: Option<&crate::column_css::ColumnStyleTable>,
) -> (usize, usize) {
    let Some(table) = column_styles else {
        return (ORPHANS_INITIAL, WIDOWS_INITIAL);
    };
    let mut orphans: Option<u32> = None;
    let mut widows: Option<u32> = None;
    let mut cur = Some(node_id);
    let mut depth = 0usize;
    while let Some(id) = cur {
        if depth >= crate::MAX_DOM_DEPTH {
            break;
        }
        if let Some(props) = table.get(&id) {
            // Nearest declaration wins: only fill a slot still empty.
            if orphans.is_none() {
                orphans = props.orphans;
            }
            if widows.is_none() {
                widows = props.widows;
            }
            if orphans.is_some() && widows.is_some() {
                break;
            }
        }
        cur = doc.get_node(id).and_then(|n| n.parent);
        depth += 1;
    }
    (
        orphans.map_or(ORPHANS_INITIAL, |v| v as usize),
        widows.map_or(WIDOWS_INITIAL, |v| v as usize),
    )
}

/// Height of a container's fragment on the page it is leaving
/// (fulgur-pgbrk R3).
///
/// A container fragment starts at `page_start_y` and can never
/// legitimately paint below the page's content bottom, so the slice is
/// the accumulated cursor clamped to the remaining strip.
///
/// The clamp matters because `cursor_y` can legally sit past the page
/// bottom: it carries the trailing margin of the last child placed on
/// the page, and css-break-3 §5.2 truncates margins adjoining an
/// unforced break to zero. Without the clamp that margin is baked into
/// the fragment height, and `render.rs:2793` paints the container's
/// background / border / shadow with it — down through the bottom
/// margin and over any running footer.
///
/// This clamps the *container* only. A child that genuinely does not
/// fit keeps its overflowing fragment, so unbreakable-content defects
/// stay visible to [`find_overflowing_fragments`] rather than being
/// masked here.
fn parent_slice_height(cursor_y: f32, page_start_y: f32, page_height_px: f32) -> f32 {
    let strip = (page_height_px - page_start_y).max(0.0);
    (cursor_y - page_start_y).clamp(0.0, strip)
}

/// The single child-enumeration policy for every paginating walk
/// (fulgur-pgbrk walker-convergence Phase 2): prefer Blitz's
/// `layout_children` over the raw DOM `children` when it has been
/// computed and is non-empty.
///
/// When a block container has mixed block-level and inline-level
/// children, Stylo synthesizes anonymous block wrappers around the
/// inline-level siblings (CSS 2.1 §9.2.1.1). Those wrappers carry
/// their own `node_id` and Taffy layout, but they live ONLY in
/// `layout_children` — the original `children` list still points at
/// the underlying inline elements (e.g. a `<span
/// display:inline-block>`, or a body containing `<label>` followed by
/// `<fieldset>` followed by `<select><option>…</option></select>`,
/// whose inline-level siblings get wrapped in an anonymous block
/// visible only in `layout_children`).
///
/// Without this preference the walkers silently drop the inline-level
/// group's paint: extract assigns the inner paragraph's `node_id` to
/// the synthesized wrapper, but a raw-`children` walk never visits the
/// wrapper, so geometry has no fragment for that node_id and
/// `dispatch_fragment` skips the paragraph entirely (fulgur-bq6i:
/// examples/wasm-demo lost label / legend / option text content and
/// review_card_inline_block.html lost its "OK Approved" badge for this
/// exact reason; fulgur-yb27 extended the same policy to the nested
/// walk and the recursion-gate probe `subtree_requires_recursion`).
///
/// Returns an empty vec when `id` does not resolve to a node — every
/// caller treats a missing node as "no children to visit", matching
/// the previous per-site `get_node` guards.
fn layout_children_of(doc: &BaseDocument, id: usize) -> Vec<usize> {
    let Some(node) = doc.get_node(id) else {
        return Vec::new();
    };
    let layout_borrow = node.layout_children.borrow();
    if let Some(lc) = layout_borrow.as_deref()
        && !lc.is_empty()
    {
        lc.to_vec()
    } else {
        node.children.clone()
    }
}

/// The shared child skip filter for the paginating walks
/// (fulgur-pgbrk walker-convergence Phase 2): a child is not walked
/// when it
///
/// - does not resolve to a node in `doc` (dangling id), or
/// - is a pure-whitespace text node (same convention as
///   `multicol_layout::partition_children_into_segments`), or
/// - is out-of-flow positioned (`position: absolute` / `fixed` —
///   CSS 2.1 §10.6.4 / §9.6: such elements do not contribute to their
///   containing block's normal-flow height and are handled by separate
///   passes, `append_position_fixed_fragments` and the abs positioning
///   pipeline in `render`).
///
/// Callers with a *different* out-of-flow policy must NOT use this
/// helper: `record_subtree_fragments_at_offset` recurses into nested
/// absolutes (only `fixed` is skipped there), so it keeps its own
/// inline filter.
fn is_walkable_skip(doc: &BaseDocument, id: usize) -> bool {
    let Some(node) = doc.get_node(id) else {
        return true;
    };
    if let Some(text) = node.text_data()
        && text.content.chars().all(char::is_whitespace)
    {
        return true;
    }
    use ::style::properties::longhands::position::computed_value::T as Pos;
    node.primary_styles()
        .is_some_and(|s| matches!(s.get_box().clone_position(), Pos::Absolute | Pos::Fixed))
}

/// Zero-height child handling shared by the body walk
/// ([`PaginationLayoutTree::fragment_pagination_root`]) and the nested
/// walk ([`fragment_block_subtree`]) — fulgur-pgbrk
/// walker-convergence phase 3a.
///
/// Zero-height **element** nodes still enter geometry so their
/// counter / string-set / bookmark markers participate in the per-page
/// metadata walks (Phase 2.3 fix; the fragment carries `height == 0`
/// and does not advance the cursor — only the NodeId matters to the
/// collectors). `break-before` / `break-after` and the CSS Page 3 §5.3
/// implicit page-name break fire even on such elements (fulgur-p3uf
/// Phase 3.1.5a): pseudo-only divs and dimension-less images collapse
/// to `child_h == 0` but still need the directive honoured (see
/// `tests/pseudo_only_break_before.rs`). Floats stay out of the
/// page-name comparison and the `prev_used_page` update (CSS 2.1 §9.5)
/// — they do not establish class A break points.
///
/// `frame.kind` decides what "advance a page" does: a nested container
/// closes its parent fragment on the page being left through
/// `frame.parent_slice` and rebases its Taffy origin
/// (`page_taffy_origin` eagerly on break-before; deferred via
/// `origin_pending_target_y` on break-after); a body frame just
/// advances `(page, cursor_y)`. The nested walk's `suppress_page_check`
/// gate (flex / grid / atomic-inline / orthogonal containers) is
/// threaded in by the caller; the body walk passes `false`.
///
/// Returns `true` when the child is an element node and a fragment was
/// emitted — the body walk increments its emitted count; the nested
/// walk sets `emitted_anything`.
fn fragment_zero_height_child(
    cx: &FragmentationCtx<'_>,
    frame: &mut ContainerFrame,
    geometry: &mut PaginationGeometryTable,
    child: &blitz_dom::Node,
    child_id: usize,
    this_top_in_parent: f32,
    suppress_page_check: bool,
) -> bool {
    let layout = child.final_layout;
    let child_w = if layout.size.width > 0.0 {
        layout.size.width
    } else {
        frame.width
    };
    let break_props = cx
        .styles
        .and_then(|t| t.get(&child_id))
        .cloned()
        .unwrap_or_default();
    let is_float = crate::blitz_adapter::node_is_floating(child);
    let (used_start, used_end) = cx.used_page_endpoints_of(child_id);
    let page_name_changed = !suppress_page_check
        && !is_float
        && frame
            .prev_used_page
            .as_ref()
            .is_some_and(|p| *p != used_start);
    let break_before_page = matches!(
        break_props.break_before,
        Some(crate::draw_primitives::BreakBefore::Page)
    ) || page_name_changed;

    if break_before_page && frame.cursor_y > frame.page_start_y {
        let resume = resume_taffy_origin(
            frame.page_taffy_origin,
            frame.page_start_y,
            cx.page_h,
            this_top_in_parent,
        );
        if let Some(slice) = frame.parent_slice {
            slice.close_continuing(
                geometry,
                frame.row_state.as_mut(),
                frame.page,
                frame.page_start_y,
            );
        }
        frame.page += 1;
        frame.cursor_y = 0.0;
        frame.page_start_y = 0.0;
        if frame.kind == ContainerKind::Nested {
            // The zero-height breaking child IS the first on the new
            // page — rebase the origin eagerly.
            frame.page_taffy_origin = resume;
            frame.origin_pending_target_y = None;
            frame.origin_pending_anchor = None;
            frame.origin_pending_same_row = None;
        }
    }

    let mut emitted_here = false;
    if child.element_data().is_some() {
        emitted_here = true;
        geometry
            .entry(child_id)
            .or_default()
            .fragments
            .push(Fragment {
                page_index: frame.page,
                x: (frame.x_in_body + layout.location.x).as_px(),
                y: frame.cursor_y.as_px(),
                width: child_w.as_px(),
                height: 0.0_f32.as_px(),
            });
    }

    if matches!(
        break_props.break_after,
        Some(crate::draw_primitives::BreakAfter::Page)
    ) {
        if let Some(slice) = frame.parent_slice {
            slice.close_unforced(
                geometry,
                frame.row_state.as_mut(),
                frame.page,
                frame.page_start_y,
                frame.cursor_y,
            );
        }
        frame.page += 1;
        frame.cursor_y = 0.0;
        frame.page_start_y = 0.0;
        if frame.kind == ContainerKind::Nested {
            // The NEXT child is the first on the new page — defer the
            // origin rebase via the pending slot.
            frame.origin_pending_target_y = Some(frame.page_start_y);
            frame.origin_pending_anchor = None;
            frame.origin_pending_same_row = None;
        }
    }
    if !is_float {
        frame.prev_used_page = Some(used_end);
    }
    emitted_here
}

/// Multi-line inline-root handling shared by the body walk
/// ([`PaginationLayoutTree::fragment_pagination_root`]) and the nested
/// walk ([`fragment_block_subtree`]) — fulgur-pgbrk
/// walker-convergence phase 3b.
///
/// Routes a multi-line inline root through [`fragment_inline_root`]:
/// probes Parley's line metrics, applies the `break-inside: avoid`
/// suppression (relaxed when `avoid` is unfulfillable — CSS
/// Fragmentation 3 §4.4), takes the "push whole to next page"
/// class A break when the paragraph can't honour widow / orphan
/// constraints in the remaining strip, then splits at line
/// boundaries.
///
/// The two call shapes differ in exactly four parameterized ways; the
/// gates keep each difference explicit rather than silently unifying
/// it:
///
/// 1. **Child top on page.** Body measures the child at the body
///    cursor (inter-child gaps already advanced it); nested measures
///    `page_start_y + (this_top_in_parent - page_taffy_origin)`, the
///    Taffy-origin rebase that keeps flex / grid row siblings aligned
///    (fulgur-kv0r). Gate: `frame.kind`.
/// 2. **Push-whole floor.** Body's floor is fixed at 0 — a break
///    before the leading child is always a break before body (CSS
///    Fragmentation §3). Nested computes
///    `allow_leading_break && !suppress_page_check` and falls back to
///    `page_start_y` when propagation is disallowed (flex / grid /
///    atomic-inline / orthogonal containers). Gate: the
///    `suppress_page_check` argument, same convention as
///    [`fragment_zero_height_child`].
/// 3. **Nested-only parent bookkeeping (fulgur-oc51).** When the
///    paragraph crosses pages, the nested walk emits the parent's
///    fragment on every crossed page (via [`emit_parent_page_spans`])
///    and marks the row co-split (`crossed_by_recursion`), so
///    parallel flex / grid siblings restore. Body has no parent to
///    close. Gate: `frame.kind` / `frame.parent_slice` presence.
/// 4. **`row_state` max-end tracking.** Nested updates the row's
///    `max_end_page` / `max_end_cursor_y` after `break-after`;
///    body carries `row_state: None`, making the update a no-op.
///
/// Both shapes set `prev_used_page` (skipping floats, CSS 2.1 §9.5)
/// and honour `break-after: page`; nested additionally closes the
/// parent slice and defers its origin rebase via
/// `origin_pending_target_y` (kind-gated), mirroring
/// [`fragment_zero_height_child`].
///
/// Returns `Some(fragments_emitted)` when the inline split path ran
/// (`line_metrics.len() > 1`); `None` when the child has ≤ 1 line and
/// the caller must fall through to the block path. Body adds the count
/// to its emitted tally; nested sets `emitted_anything`.
fn fragment_inline_child(
    cx: &FragmentationCtx<'_>,
    frame: &mut ContainerFrame,
    geometry: &mut PaginationGeometryTable,
    child: &blitz_dom::Node,
    child_id: usize,
    this_top_in_parent: f32,
    suppress_page_check: bool,
) -> Option<usize> {
    let layout = child.final_layout;
    let child_w = if layout.size.width > 0.0 {
        layout.size.width
    } else {
        frame.width
    };
    let break_props = cx
        .styles
        .and_then(|t| t.get(&child_id))
        .cloned()
        .unwrap_or_default();
    let all_line_metrics = collect_inline_line_metrics(child);
    // fulgur-pgbrk R1: measure the BORDER box. Parley's metrics are
    // content-box relative, so `last.1 - first.0` omits the box's own
    // padding / border and under-reports it. See `inline_root_box_metrics`.
    let (lead_in, lines_h, lead_out) = inline_root_box_metrics(child, &all_line_metrics);
    let box_total_h = lead_in + lines_h + lead_out;
    let avoid_is_fulfillable = if all_line_metrics.is_empty() {
        true
    } else {
        box_total_h <= cx.page_h
    };
    let avoid_inside = matches!(
        break_props.break_inside,
        Some(crate::draw_primitives::BreakInside::Avoid)
    );
    let line_metrics = if avoid_inside && avoid_is_fulfillable {
        Vec::new()
    } else {
        all_line_metrics
    };
    if line_metrics.len() <= 1 {
        return None;
    }

    // Difference 1: child top on page (see the doc comment).
    let mut child_top_on_page = if frame.kind == ContainerKind::Nested {
        frame.page_start_y + (this_top_in_parent - frame.page_taffy_origin)
    } else {
        frame.cursor_y
    };
    // Difference 2: push-whole floor.
    let overflow_floor = if frame.allow_leading_break && !suppress_page_check {
        0.0
    } else {
        frame.page_start_y
    };
    if break_decision(child_top_on_page, box_total_h, overflow_floor, cx.page_h)
        == BreakDecision::PushToNextPage
    {
        let resume = resume_taffy_origin(
            frame.page_taffy_origin,
            frame.page_start_y,
            cx.page_h,
            this_top_in_parent,
        );
        if let Some(slice) = frame.parent_slice {
            // Nested only: close the parent on the outgoing page
            // (the `cursor > start` guard skips the fresh-strip
            // close). Body has no parent to close. The inline root
            // moves to the next page, so the parent continues here.
            if frame.cursor_y > frame.page_start_y {
                slice.close_continuing(
                    geometry,
                    frame.row_state.as_mut(),
                    frame.page,
                    frame.page_start_y,
                );
            }
        }
        frame.page += 1;
        frame.cursor_y = 0.0;
        frame.page_start_y = 0.0;
        child_top_on_page = 0.0;
        if frame.kind == ContainerKind::Nested {
            frame.page_taffy_origin = resume;
            // Any part of the container's leading decoration that the
            // outgoing page did not spend is still owed here, so the
            // inline root resumes below it rather than at the strip top.
            child_top_on_page = this_top_in_parent - resume;
        }
    }

    let (orphans, widows) = resolved_line_constraints(cx.doc, child_id, cx.styles);
    let input = InlineSplitInput {
        line_metrics: &line_metrics,
        lead_in,
        lead_out,
        orphans,
        widows,
    };
    let placement = InlinePlacement {
        id: child_id,
        x: frame.x_in_body + layout.location.x,
        width: child_w,
        cursor_y: child_top_on_page,
        page: frame.page,
    };
    let pre_page = frame.page;
    let (new_page, new_cursor, frag_count) =
        fragment_inline_root(geometry, cx.page_h, placement, &input);
    match frame.kind {
        ContainerKind::RootBody => {
            frame.page = new_page;
            frame.cursor_y = new_cursor;
        }
        ContainerKind::Nested => {
            if new_page > pre_page {
                // Difference 3: the paragraph filled every page it
                // crossed, so the parent spans those pages too
                // (fulgur-oc51 via `emit_parent_page_spans`).
                if let Some(slice) = frame.parent_slice {
                    emit_parent_page_spans(
                        geometry,
                        frame.row_state.as_mut(),
                        &slice,
                        pre_page,
                        new_page,
                        frame.page_start_y,
                        true,
                    );
                }
                frame.page = new_page;
                frame.cursor_y = new_cursor;
                frame.page_start_y = 0.0;
                frame.origin_pending_target_y = Some(frame.cursor_y);
                // Anchor the deferred rebase on THIS
                // paragraph's own Taffy-space bottom edge
                // (`this_top_in_parent + box_total_h`), not the next
                // sibling's `this_top_in_parent` — otherwise the next
                // sibling is forced flush against the paragraph's tail,
                // discarding their natural (possibly collapsed-margin)
                // gap. See `origin_pending_anchor`'s doc comment.
                frame.origin_pending_anchor = Some(this_top_in_parent + box_total_h);
                frame.origin_pending_same_row = None;
                if let Some(ref mut rs) = frame.row_state {
                    rs.crossed_by_recursion = true;
                }
            } else {
                frame.cursor_y = frame.cursor_y.max(new_cursor);
            }
        }
    }

    if matches!(
        break_props.break_after,
        Some(crate::draw_primitives::BreakAfter::Page)
    ) {
        if let Some(slice) = frame.parent_slice {
            slice.close_unforced(
                geometry,
                frame.row_state.as_mut(),
                frame.page,
                frame.page_start_y,
                frame.cursor_y,
            );
        }
        frame.page += 1;
        frame.cursor_y = 0.0;
        frame.page_start_y = 0.0;
        if frame.kind == ContainerKind::Nested {
            // The NEXT child is the first on the new page — defer the
            // origin rebase via the pending slot.
            frame.origin_pending_target_y = Some(frame.page_start_y);
            frame.origin_pending_anchor = None;
            frame.origin_pending_same_row = None;
        }
    }
    let is_float = crate::blitz_adapter::node_is_floating(child);
    if !is_float {
        let (_, used_end) = cx.used_page_endpoints_of(child_id);
        frame.prev_used_page = Some(used_end);
    }
    // Difference 4: row max-end tracking (`row_state` is None for
    // body, so this is a no-op there).
    if let Some(ref mut rs) = frame.row_state {
        if frame.page > rs.max_end_page
            || (frame.page == rs.max_end_page && frame.cursor_y > rs.max_end_cursor_y)
        {
            rs.max_end_page = frame.page;
            rs.max_end_cursor_y = frame.cursor_y;
        }
    }
    Some(frag_count)
}

/// Outcome of the shared recursion-gate helper
/// ([`fragment_recursion_child`]). The body walk turns `Placed` into
/// `emitted += 1`; the nested walk sets `emitted_anything` and — for
/// `RequestBreakBefore` — hands the break up to its own caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecursionOutcome {
    /// The gate fired and the child placed itself (possibly after one
    /// retry); `frame` holds the post-placement page / cursor.
    Placed,
    /// The child's first attempt requested a break before itself and
    /// the caller opted to propagate it (nested leading-child rule,
    /// css-break-3 §3.1.1). The geometry table is untouched (the
    /// `RequestBreakBefore` proof obligation) and `frame` is
    /// unmodified.
    RequestBreakBefore,
}

/// Recursion gate + recurse + one-shot retry shared by the body walk
/// ([`PaginationLayoutTree::fragment_pagination_root`]) and the nested
/// walk ([`fragment_block_subtree`]) — fulgur-pgbrk walker-convergence
/// phase 3c.
///
/// The gate (fulgur-g9e3.1 + fulgur-a36m + fulgur-7hf5) recurses into
/// a child whenever its subtree would split — truly oversized
/// (grandchild overflow), in-place mid-element split, or a forced
/// break / page-name change declared below — by composing the three
/// pure probes [`has_forced_break_below`],
/// [`has_page_name_change_below`] and [`subtree_requires_recursion`].
/// `subtree_requires_recursion` evaluates the walk's own
/// `break_decision` with the walk's own floor (walker-convergence
/// phase 5) and returns `false` when the grandchildren all fit the
/// available strip, so the "parent CSS height > children
/// sum" case falls through to the caller's whole-emit path. The
/// recursion enters from the child's current page-local position so an
/// in-place split produces a fragment on the current page and a tail
/// on the next (CSS Fragmentation §3).
///
/// When the recursion hands back [`SubtreeResult::RequestBreakBefore`]
/// the helper retries **at most once**: it advances a page and
/// re-enters at `cursor_y == 0`, where the callee's propagation
/// conditions (`cursor_in > 0.0`) can no longer fire.
///
/// The two call shapes differ in exactly these parameterized ways; the
/// gates keep each difference explicit rather than silently unifying
/// it (same convention as [`fragment_inline_child`]):
///
/// 1. **Multicol / `column-span: all` exception.** Both walks treat a
///    multicol container as atomic (multicol fragments itself via
///    `multicol_layout::run_pass`; fulgur-7hf5), but the body walk
///    honours the fulgur-916y exception: a multicol container with a
///    `column-span: all` direct child lays that span out as a
///    full-width block flowing between the column groups, so
///    block-flow recursion may split it across pages. Gate:
///    `frame.kind` (`RootBody`-only lookup).
/// 2. **Entry cursor.** Body enters the recursion at its current
///    cursor; nested enters at the Taffy-rebased page-local y
///    (`page_start_y + (this_top_in_parent - page_taffy_origin)`) so a
///    parallel flex / grid cell starts its recursion strip at the
///    row's y rather than below a previous cell (fulgur-u0p0). Gate:
///    `frame.kind`.
/// 3. **Child-frame depth.** Body passes its own depth (body-direct
///    children stay at depth 0); nested passes `depth + 1`. Preserved
///    as-is. Gate: `frame.kind`.
/// 4. **Leading-break propagation (nested-only).** When the first
///    attempt returns `RequestBreakBefore`, the nested walk may hand
///    the break further up instead of retrying — a break before a
///    box's first child is a break before the box, recursively
///    (css-break-3 §3.1.1). The predicate (`!emitted_anything &&
///    cursor_in > 0.0 && propagate_leading_break`) needs the nested
///    walk's *entry* cursor, which the frame no longer holds once the
///    loop advances, so the nested call site passes it precomputed as
///    `may_propagate_break`; body passes `false` (the root has no one
///    to propagate to). The child frame's `allow_leading_break` is
///    `frame.allow_leading_break && !suppress_page_check` — body's
///    frame carries `true` and the body call passes `false`, so
///    body-direct children get the literal `true` they always had.
/// 5. **Retry advance (nested-only extras).** Before re-entering, the
///    nested walk closes the parent slice on the outgoing page (when
///    `cursor_y > page_start_y`, via `frame.parent_slice`) and rebases
///    `page_taffy_origin` to the breaking child. Body has no parent
///    to close and no origin to rebase. Gates: `frame.parent_slice`
///    presence / `frame.kind`.
/// 6. **Post-placement adoption.** Body adopts the returned
///    `(page, cursor_y)` verbatim. Nested keeps the row's max bottom
///    when the recursion stayed on-page (fulgur-u0p0), emits the
///    fulgur-oc51 parent page spans for every crossed page (skipping
///    the outgoing-page fragment when nothing landed there), restarts
///    the parent's fragment at y=0 on the new page, defers the origin
///    rebase through `origin_pending_target_y` /
///    `origin_pending_same_row`, and marks the row
///    `crossed_by_recursion` so same-row cells co-split. Gate:
///    `frame.kind`.
/// 7. **`break-after: page` / used-page-name / row max-end tail.**
///    Nested closes the parent slice and defers the origin rebase;
///    body advances only. The float-skipped `prev_used_page` update
///    and the `RowState` max-end tracking are shared (no-ops for body
///    through `parent_slice: None` / `row_state: None`).
///
/// `child_id` is read from `child.id` (the document arena id — the
/// same id the call sites looked the child up by); that keeps the
/// signature at the clippy arity ceiling alongside the phase 3a / 3b
/// helpers.
///
/// Returns `None` when the gate said the subtree does not split — the
/// caller then falls through to its strip-overflow / oversized-slice /
/// whole-emit fallback. Returns `Some(Placed)` after a successful
/// placement (body: `emitted += 1`; nested: `emitted_anything =
/// true`), or `Some(RequestBreakBefore)` when the break propagates
/// (nested-only; the caller returns `SubtreeResult::RequestBreakBefore`).
fn fragment_recursion_child(
    cx: &FragmentationCtx<'_>,
    frame: &mut ContainerFrame,
    geometry: &mut PaginationGeometryTable,
    child: &blitz_dom::Node,
    this_top_in_parent: f32,
    suppress_page_check: bool,
    may_propagate_break: bool,
) -> Option<RecursionOutcome> {
    let child_id = child.id;
    let layout = child.final_layout;
    let child_h = if layout.size.height.is_finite() {
        layout.size.height
    } else {
        0.0
    };
    let child_w = if layout.size.width > 0.0 {
        layout.size.width
    } else {
        frame.width
    };

    // The gate. `subtree_requires_recursion` probes the child with the
    // walk's own enumeration (`layout_children_of` + `is_walkable_skip`)
    // and the walk's own break predicate (`break_decision`) evaluated
    // at the floor the recursion would actually use — no separate
    // floor-blind simulator anymore (walker-convergence phase 5). It
    // returns `false` when the children all fit the available strip.
    let has_splittable_children = !child.children.is_empty();
    // fulgur-7hf5: multicol containers (`column-count > 1` /
    // `column-width: <len>`) distribute children across columns; their
    // DOM children's flow does not match the visual flow
    // `subtree_requires_recursion` probes. Difference 1: the
    // fulgur-916y `column-span: all` exception is body-only.
    let is_multicol = crate::blitz_adapter::is_multicol_container(child);
    let multicol_span_all_exception = frame.kind == ContainerKind::RootBody
        && child.children.iter().any(|&id| {
            cx.doc
                .get_node(id)
                .is_some_and(crate::blitz_adapter::has_column_span_all)
        });
    // Difference 4: propagate the permission, not a bare `true` — once
    // inside a flex / grid / atomic container the whole subtree below
    // is pinned and must not hand breaks upward. For body frames this
    // is `true && !false == true`, the body's historical literal. The
    // gate evaluates its floor from the same value, so gate and walk
    // agree on the leading-child floor by construction.
    let allow_leading_break = frame.allow_leading_break && !suppress_page_check;
    let available_strip = (cx.page_h - frame.cursor_y).max(0.0);
    let needs_recursion = has_splittable_children
        && (!is_multicol || multicol_span_all_exception)
        && (has_forced_break_below(cx.doc, child_id, cx.styles, 0)
            || has_page_name_change_below(cx.doc, child_id, cx.used_page_names, 0)
            || subtree_requires_recursion(cx, child_id, available_strip, allow_leading_break));
    if !needs_recursion {
        return None;
    }

    let child_x_in_body = frame.x_in_body + layout.location.x;
    // Difference 2: entry cursor (see the doc comment).
    let entry_cursor_y = if frame.kind == ContainerKind::Nested {
        frame.page_start_y + (this_top_in_parent - frame.page_taffy_origin)
    } else {
        frame.cursor_y
    };
    // Difference 3: child-frame depth (see the doc comment).
    let child_depth = if frame.kind == ContainerKind::Nested {
        frame.depth + 1
    } else {
        frame.depth
    };
    let pre_recursion_page = frame.page;
    let pre_recursion_cursor_y = frame.cursor_y;
    let mut child_frame = ContainerFrame::child(
        child_id,
        child_x_in_body,
        child_w,
        frame.page,
        entry_cursor_y,
        allow_leading_break,
        child_depth,
    );
    let mut result = fragment_block_subtree(cx, &mut child_frame, geometry);
    if result == SubtreeResult::RequestBreakBefore {
        // Difference 4: hand the break up instead of retrying when the
        // nested leading-child rule says so.
        if may_propagate_break {
            return Some(RecursionOutcome::RequestBreakBefore);
        }
        // Difference 5: nested closes the parent on the outgoing page
        // before advancing; body has no parent to close. The retrying
        // child is placed on the next page, so the parent continues —
        // its outgoing fragment spans the full strip.
        let resume = resume_taffy_origin(
            frame.page_taffy_origin,
            frame.page_start_y,
            cx.page_h,
            this_top_in_parent,
        );
        if frame.cursor_y > frame.page_start_y {
            if let Some(slice) = frame.parent_slice {
                slice.close_continuing(
                    geometry,
                    frame.row_state.as_mut(),
                    frame.page,
                    frame.page_start_y,
                );
            }
        }
        frame.page += 1;
        frame.cursor_y = 0.0;
        frame.page_start_y = 0.0;
        if frame.kind == ContainerKind::Nested {
            // The retrying child is the first on the new page — rebase
            // the Taffy origin so it lands at `page_start_y` (= 0),
            // discarding the inter-child gap (CSS 3 Fragmentation §3),
            // except for any unspent leading decoration of the
            // container itself (see `resume_taffy_origin`).
            frame.page_taffy_origin = resume;
        }
        // Retry at most once: entered at `cursor_y == 0`, the callee's
        // `RequestBreakBefore` producers (which require `cursor_in >
        // 0.0`) cannot fire again.
        child_frame = ContainerFrame::child(
            child_id,
            child_x_in_body,
            child_w,
            frame.page,
            0.0,
            allow_leading_break,
            child_depth,
        );
        result = fragment_block_subtree(cx, &mut child_frame, geometry);
    }
    let SubtreeResult::Placed {
        page: new_page,
        cursor_y: new_cursor,
    } = result
    else {
        unreachable!("a retry entered at cursor_y == 0 always places")
    };

    // Difference 6: post-placement adoption (see the doc comment).
    match frame.kind {
        ContainerKind::RootBody => {
            frame.page = new_page;
            frame.cursor_y = new_cursor;
        }
        ContainerKind::Nested => {
            frame.page = new_page;
            // fulgur-u0p0: when the recursion stayed on the same page,
            // keep the larger of the parent's existing cursor (the row
            // max bottom from a previous parallel sibling) and the
            // recursion's returned cursor; when it crossed pages the
            // old cursor is stale.
            frame.cursor_y = if new_page == pre_recursion_page {
                frame.cursor_y.max(new_cursor)
            } else {
                new_cursor
            };
            // If the recursion crossed a boundary, the parent's
            // current-page fragment must restart at y=0 on the new
            // page.
            if frame.page != pre_recursion_page || new_cursor < frame.page_start_y {
                if frame.page > pre_recursion_page {
                    // fulgur-oc51: parent fragments for every crossed
                    // page span. Skip the outgoing-page fragment when
                    // the parent has nothing on that page (the
                    // recursing child is the parent's leading child
                    // AND the recursion propagated the break up rather
                    // than placing a slice on the outgoing page).
                    let child_placed_on_pre_page = geometry.get(&child_id).is_some_and(|g| {
                        g.fragments
                            .iter()
                            .any(|f| f.page_index == pre_recursion_page)
                    });
                    let parent_has_content_on_pre_page =
                        pre_recursion_cursor_y > frame.page_start_y || child_placed_on_pre_page;
                    if let Some(slice) = frame.parent_slice {
                        emit_parent_page_spans(
                            geometry,
                            frame.row_state.as_mut(),
                            &slice,
                            pre_recursion_page,
                            frame.page,
                            frame.page_start_y,
                            parent_has_content_on_pre_page,
                        );
                    }
                }
                frame.page_start_y = 0.0;
                frame.origin_pending_target_y = Some(frame.cursor_y);
                let row_top = this_top_in_parent;
                let row_bottom = row_top + child_h;
                let allow_same_row_rebase = cx
                    .doc
                    .get_node(frame.id)
                    .is_some_and(crate::blitz_adapter::is_flex_or_grid_container_node);
                frame.origin_pending_same_row =
                    allow_same_row_rebase.then_some((row_top, row_bottom, 0.0));
                // Non-row case anchor (see
                // `origin_pending_anchor`'s doc comment) — the recursed
                // child's own Taffy-space bottom edge, so the next
                // sibling's natural gap to it survives the rebase. Set
                // unconditionally; the consumer prefers
                // `origin_pending_same_row` when present, so this is a
                // no-op in the flex/grid row case above.
                frame.origin_pending_anchor = Some(this_top_in_parent + child_h);
                // fulgur-ysms: mark that this row had a recursion-
                // driven page cross so subsequent same-row cells know
                // to co-split.
                if let Some(ref mut rs) = frame.row_state {
                    rs.crossed_by_recursion = true;
                }
            }
        }
    }

    // Difference 7: `break-after: page` tail, float-skipped
    // used-page-name update, and row max-end tracking.
    let break_props = cx
        .styles
        .and_then(|t| t.get(&child_id))
        .cloned()
        .unwrap_or_default();
    if matches!(
        break_props.break_after,
        Some(crate::draw_primitives::BreakAfter::Page)
    ) {
        if let Some(slice) = frame.parent_slice {
            slice.close_unforced(
                geometry,
                frame.row_state.as_mut(),
                frame.page,
                frame.page_start_y,
                frame.cursor_y,
            );
        }
        frame.page += 1;
        frame.cursor_y = 0.0;
        frame.page_start_y = 0.0;
        if frame.kind == ContainerKind::Nested {
            // The NEXT child is the first on the new page — defer the
            // origin rebase via the pending slot.
            frame.origin_pending_target_y = Some(frame.page_start_y);
            frame.origin_pending_anchor = None;
            frame.origin_pending_same_row = None;
        }
    }
    if !crate::blitz_adapter::node_is_floating(child) {
        let (_, used_end) = cx.used_page_endpoints_of(child_id);
        frame.prev_used_page = Some(used_end);
    }
    if let Some(ref mut rs) = frame.row_state {
        if frame.page > rs.max_end_page
            || (frame.page == rs.max_end_page && frame.cursor_y > rs.max_end_cursor_y)
        {
            rs.max_end_page = frame.page;
            rs.max_end_cursor_y = frame.cursor_y;
        }
    }
    frame.emitted_anything = true;
    Some(RecursionOutcome::Placed)
}

/// The unchanging half of the "close the parent's fragment on the page
/// it is leaving" idiom, which appears at nine sites in
/// [`fragment_block_subtree`] (fulgur-pgbrk Risk 1).
///
/// Only the page and the two y values vary across those sites; the
/// parent's identity, x and width do not, so they are captured once per
/// call.
///
/// # Dedup policy
///
/// [`RowState::emitted_parent_pages`] exists so that N parallel flex /
/// grid cells, each independently deciding to close the parent on the
/// current page, emit only one parent fragment for it.
///
/// Eight of the nine sites consult it via [`Self::close_unforced`] —
/// every site that closes the parent because it is **leaving** a page,
/// whether the break was forced (`break-before` / `break-after: page`)
/// or unforced (strip overflow). Being about to leave a page is the
/// property the dedup keys on, and a forced break satisfies it exactly
/// as an unforced one does.
///
/// Originally only the two unforced sites deduped, which was
/// fulgur-pgbrk R8: a cell that crosses a page by recursion sets
/// `crossed_by_recursion`, which restores a same-row sibling to the
/// row-start page, and that sibling's forced break then closed the
/// parent a second time on it — with a different height, so `render.rs`
/// painted the container's decoration twice at two sizes on one page.
/// Pinned by `forced_break_does_not_close_a_grid_parent_twice_on_one_page`.
///
/// [`Self::close_forced`] survives for the **one** site that is not a
/// page departure: the function tail, which emits the parent's final
/// fragment on a page it never leaves and so has no sibling to contend
/// with. Deduping there would be wrong — a same-row cell that already
/// closed an earlier page would suppress the parent's last fragment
/// entirely.
#[derive(Clone, Copy)]
struct ParentSlice {
    id: usize,
    x_in_body: f32,
    width: f32,
    page_height_px: f32,
}

impl ParentSlice {
    /// Close the parent unconditionally. Used by the forced-break sites
    /// and by the function tail. See the dedup note on the type.
    fn close_forced(
        &self,
        geometry: &mut PaginationGeometryTable,
        page_index: u32,
        page_start_y: f32,
        cursor_y: f32,
    ) {
        geometry
            .entry(self.id)
            .or_default()
            .fragments
            .push(Fragment {
                page_index,
                x: self.x_in_body.as_px(),
                y: page_start_y.as_px(),
                width: self.width.as_px(),
                height: parent_slice_height(cursor_y, page_start_y, self.page_height_px).as_px(),
            });
    }

    /// Close the parent at most once per page across parallel flex /
    /// grid cells. Used by the two overflow-driven sites.
    fn close_unforced(
        &self,
        geometry: &mut PaginationGeometryTable,
        row_state: Option<&mut RowState>,
        page_index: u32,
        page_start_y: f32,
        cursor_y: f32,
    ) {
        let should_emit = row_state
            .map(|rs| rs.emitted_parent_pages.insert(page_index))
            .unwrap_or(true);
        if should_emit {
            self.close_forced(geometry, page_index, page_start_y, cursor_y);
        }
    }

    /// Close the parent on a page it is **leaving**, i.e. one where more
    /// of its content follows on a later page.
    ///
    /// Such a fragment spans the whole remaining strip
    /// (`page_height_px - page_start_y`), not the child cursor: the box
    /// continues past the page bottom by definition, so its background,
    /// side borders and shadow must run to the page edge rather than
    /// stopping at whichever child happened to be the last to fit. The
    /// gap between that child's bottom and the page bottom is the
    /// container's own padding-bottom, an inter-child gap, or simply
    /// space the next child could not use — never a place for the box to
    /// end.
    ///
    /// This is the same span [`emit_parent_page_spans`] already claims
    /// for the recursion / slicing crossings; routing the push and
    /// forced-break closers here is what makes the two agree. Contrast
    /// [`ParentSlice::close_forced`] / [`ParentSlice::close_unforced`],
    /// which are for the page a container **ends** on and must stay
    /// cursor-derived so a trailing margin adjoining an unforced break is
    /// not baked into the height (css-break-3 §5.2 — see
    /// [`parent_slice_height`]).
    ///
    /// Dedupes across parallel flex / grid cells exactly as
    /// [`ParentSlice::close_unforced`] does.
    fn close_continuing(
        &self,
        geometry: &mut PaginationGeometryTable,
        row_state: Option<&mut RowState>,
        page_index: u32,
        page_start_y: f32,
    ) {
        let should_emit = row_state
            .map(|rs| rs.emitted_parent_pages.insert(page_index))
            .unwrap_or(true);
        if !should_emit {
            return;
        }
        let height = (self.page_height_px - page_start_y).max(0.0);
        if height <= 0.0 {
            return;
        }
        geometry
            .entry(self.id)
            .or_default()
            .fragments
            .push(Fragment {
                page_index,
                x: self.x_in_body.as_px(),
                y: page_start_y.as_px(),
                width: self.width.as_px(),
                height: height.as_px(),
            });
    }
}

/// Outcome of fragmenting one block subtree (fulgur-pgbrk R4 / R5).
///
/// `RequestBreakBefore` is the channel by which a box hands a break
/// decision **up** to its container, which css-break-3 requires in two
/// places fulgur previously dropped it:
///
/// - §3.1.1 — "A `break-before` value on a first in-flow child box is
///   propagated to its container." The child's own break point does not
///   exist (there is no gap between a container's content edge and its
///   first child, §4.1), so the nearest legal break is the class A point
///   before the container.
/// - §4.4 rule 2 — a container with `break-inside: avoid` that does not
///   fit the current strip but would fit a fresh one must move whole
///   rather than split between its children.
///
/// **Invariant:** a call returning `RequestBreakBefore` has pushed
/// nothing into the geometry table, so the caller can advance the page
/// and re-invoke without first having to undo a partial emission. Both
/// producers below check `emitted_anything` to guarantee it.
///
/// Re-invocation terminates because both producers require `cursor_in >
/// 0.0`: after the caller advances, the subtree starts at the top of a
/// fresh page and neither condition can fire again. A break before a box
/// already at a page top is a no-op anyway (§3.1.1 collapses it), so this
/// is the spec-correct stopping rule rather than a retry counter.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SubtreeResult {
    Placed { page: u32, cursor_y: f32 },
    RequestBreakBefore,
}

/// Whether a child may break before itself on the current strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakDecision {
    /// Place the child where the cursor is.
    PlaceHere,
    /// Close the parent on this page and place the child on the next.
    PushToNextPage,
}

/// The single break decision, shared by the block strip-overflow cut, the
/// nested inline-root push-whole, the body-direct inline push-whole,
/// and the recursion-gate probe [`subtree_requires_recursion`]
/// (fulgur-pgbrk Risk 1). The gate's adoption (walker-convergence
/// phase 5) closed the divergence deferred at extraction time: the
/// probe used to be a floor-blind simulator with its own overflow
/// check, so its leading-child answer could disagree with the walk;
/// now gate and walk evaluate this same predicate on the same
/// enumeration with the same floor.
///
/// `floor` is the y below which a break is legal on this strip:
///
/// - `0.0` when an overflowing LEADING child may propagate its break up
///   to the box's own leading edge (css-break-3 §3 — a break before a
///   box's first child is also a break before the box). This is
///   fulgur-pgbrk's fix; before it the gate was `page_start_y`, which a
///   first in-flow child never exceeds, so such a child could never
///   break and was laid out past the page bottom and discarded.
/// - `page_start_y` inside a container that does not paginate its
///   children independently — flex / grid (whose items are not class A
///   break points, §4.1), atomic inline containers, orthogonal flow. See
///   `suppress_page_check`.
///
/// `child_box_h` is the **border box** height (fulgur-pgbrk R1): the
/// block path passes Taffy's `child_h`; the inline-root paths pass
/// `lead_in + lines_h + lead_out`, because Parley's line metrics are
/// content-box relative and omit the box's own padding and border.
///
/// At `child_top_on_page == floor` the answer is always `PlaceHere`: we
/// are at the top of a fresh strip with nowhere to push to, and
/// returning `PushToNextPage` there would advance pages forever.
fn break_decision(
    child_top_on_page: f32,
    child_box_h: f32,
    floor: f32,
    page_height_px: f32,
) -> BreakDecision {
    if child_top_on_page > floor && child_top_on_page + child_box_h > page_height_px {
        BreakDecision::PushToNextPage
    } else {
        BreakDecision::PlaceHere
    }
}

/// Inputs fixed for the whole fragmentation run. The [`ContainerFrame`]
/// carries the per-container mutable state; the immutable inputs (DOM,
/// style side-tables, page height) travel once here so recursive calls
/// pass references rather than re-copying arguments (see
/// `docs/plans/2026-08-18-fulgur-single-pass-fragmentation-design.md`,
/// "The walk: one fragment_container").
struct FragmentationCtx<'a> {
    doc: &'a BaseDocument,
    /// Break-style side-table (was `column_styles` on the walk
    /// signatures).
    styles: Option<&'a crate::column_css::ColumnStyleTable>,
    used_page_names: Option<&'a crate::blitz_adapter::UsedPageNameTable>,
    running: Option<&'a crate::gcpm::running::RunningElementStore>,
    page_h: f32,
}

impl FragmentationCtx<'_> {
    /// fulgur-uebl: lookup helper for the per-element start / end used
    /// page-names (CSS Page 3 §5.3). Returns `(start, end)` where each
    /// is `None` for the unnamed/auto page or `Some(name)` for a named
    /// page. When the document has no `page` declarations at all the
    /// table is absent; we return `(None, None)` so the comparison `==`
    /// always succeeds and no implicit breaks fire.
    fn used_page_endpoints_of(&self, node_id: usize) -> (Option<String>, Option<String>) {
        self.used_page_names
            .and_then(|t| t.get(&node_id).cloned())
            .unwrap_or((None, None))
    }
}

/// Whether a container is the fragmentation root (body) or a nested
/// descendant. `RootBody`'s one privilege is emitting its own
/// whole-document fragment once on page 0; after that entry push, both
/// kinds walk children by identical rules (see the design doc,
/// "Body's asymmetries become universal").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
    RootBody,
    Nested,
}

/// Per-container mutable state for the fragmentation walk. The body
/// entry (`PaginationLayoutTree::fragment_pagination_root`) builds the
/// depth-0 frame; recursive calls build a frame per child container.
/// No container assigns to another's frame — the `RequestBreakBefore`
/// proof obligation (`emitted_anything` untouched-table invariant)
/// lives on the frame.
struct ContainerFrame {
    id: usize,
    x_in_body: f32,
    width: f32,
    page: u32,
    cursor_y: f32,
    page_start_y: f32,
    page_taffy_origin: f32,
    origin_pending_target_y: Option<f32>,
    /// Taffy parent-relative y (`this_top_in_parent + box_h`) of the
    /// child whose placement produced a non-flush
    /// `origin_pending_target_y` (a recursed subtree or sliced leaf
    /// that crossed a page mid-walk, landing its tail at a page-local
    /// y other than 0). The deferred rebase must anchor on THIS
    /// point, not on the next sibling's own `this_top_in_parent` —
    /// otherwise the next sibling is forced flush against the
    /// previous one's tail, discarding their natural (possibly
    /// collapsed-margin) gap. `None` for the flush case
    /// (`origin_pending_target_y == Some(page_start_y)`), where
    /// anchoring on the next sibling's own top is correct by
    /// construction (it IS the first child on the fresh page).
    origin_pending_anchor: Option<f32>,
    origin_pending_same_row: Option<(f32, f32, f32)>,
    prev_used_page: Option<Option<String>>,
    emitted_anything: bool,
    allow_leading_break: bool,
    depth: usize,
    row_state: Option<RowState>,
    parent_slice: Option<ParentSlice>,
    kind: ContainerKind,
}

impl ContainerFrame {
    /// Build the frame for a child container about to be recursed
    /// into. `page_start_y` starts at the entry cursor (matching the
    /// previous argument wiring); `parent_slice` is filled in by
    /// `fragment_block_subtree` on entry.
    fn child(
        id: usize,
        x_in_body: f32,
        width: f32,
        page: u32,
        cursor_y: f32,
        allow_leading_break: bool,
        depth: usize,
    ) -> Self {
        ContainerFrame {
            id,
            x_in_body,
            width,
            page,
            cursor_y,
            page_start_y: cursor_y,
            page_taffy_origin: 0.0,
            origin_pending_target_y: None,
            origin_pending_anchor: None,
            origin_pending_same_row: None,
            prev_used_page: None,
            emitted_anything: false,
            allow_leading_break,
            depth,
            row_state: None,
            parent_slice: None,
            kind: ContainerKind::Nested,
        }
    }
}

/// Line-splitting inputs for [`fragment_inline_root`] / [`scan_split_points`]
/// (fulgur-pgbrk Risk 1 bundles this so the scan's argument list stays
/// under the clippy arity threshold without an `#[allow]`).
struct InlineSplitInput<'a> {
    line_metrics: &'a [(f32, f32)],
    lead_in: f32,
    lead_out: f32,
    orphans: usize,
    widows: usize,
}

/// Placement of the inline root being split (geometry-free inputs of
/// `fragment_inline_root`).
#[derive(Debug, Clone, Copy)]
struct InlinePlacement {
    id: usize,
    x: f32,
    width: f32,
    cursor_y: f32,
    page: u32,
}

/// Every fragment in `table` whose bottom edge falls below the content
/// strip, in deterministic order (fulgur-pgbrk R3).
///
/// Ordering is by node id then page index: `PaginationGeometryTable` is
/// a [`BTreeMap`] and each node's `fragments` are pushed in page order,
/// so the natural iteration order is already stable. That matters
/// because these records reach user-visible logs, and byte-stable
/// output is a project invariant.
///
/// This is the single source of truth for "did the fragmenter leave
/// content outside the page box", consumed by the production warning in
/// [`run_pass_inner`] and by test assertions.
///
/// `body_id` is excluded when supplied. Body is the one box the
/// fragmenter never fragments: its entry records the whole document
/// content height once, on page 0, as a document-level total rather
/// than a per-page placement. Every other container *is* split — a
/// wrapper spanning two pages gets one correctly clipped fragment per
/// page — so body is the sole exception, not a general carve-out for
/// containers.
pub(crate) fn find_overflowing_fragments(
    table: &PaginationGeometryTable,
    page_height_px: f32,
    body_id: Option<usize>,
) -> Vec<FragmentOverflow> {
    let mut out = Vec::new();
    for (&node_id, geom) in table {
        if body_id == Some(node_id) {
            continue;
        }
        let last_page = geom.fragments.last().map(|f| f.page_index);
        for f in &geom.fragments {
            let bottom = f.y.to_f32() + f.height.to_f32();
            if bottom > page_height_px + OVERFLOW_EPS_PX {
                out.push(FragmentOverflow {
                    node_id,
                    page_index: f.page_index,
                    overshoot_px: bottom - page_height_px,
                    continues_on_later_page: !geom.is_repeat && Some(f.page_index) != last_page,
                });
            }
        }
    }
    out
}

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
    /// is shared with `multicol_layout`, so the pagination fragmenter
    /// does not maintain its own break-style extraction. `None` means
    /// "no break properties set anywhere", which the fragmenter treats
    /// as all-`Auto`.
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
    let body_id = tree.body_id;
    let table = tree.take_geometry();
    report_fragment_overflow(&table, page_height_px, body_id);
    table
}

/// fulgur-pgbrk R3: surface any fragment the walk left outside the page
/// box.
///
/// Production builds emit one `log::warn!` per offending fragment,
/// matching the diagnostics convention used elsewhere in this crate
/// (`asset.rs`, `blitz_adapter.rs`, `column_css.rs`). The `log` facade
/// routes to whatever logger the host installed and never touches
/// fd 1, which `crates/fulgur` must not do under any circumstance
/// (CLAUDE.md); a consumer with no logger gets silence.
///
/// Test builds panic on the same condition instead, which makes the
/// invariant blanket across every caller without per-test opt-in.
/// Fixtures that legitimately trip it today are `#[ignore]`d with a
/// reference to the open gap they are blocked on — the convention the
/// `css_break3_*` block already uses — rather than allowlisted here.
fn report_fragment_overflow(
    table: &PaginationGeometryTable,
    page_height_px: f32,
    body_id: Option<usize>,
) {
    let overflows = find_overflowing_fragments(table, page_height_px, body_id);
    if overflows.is_empty() {
        return;
    }

    #[cfg(not(test))]
    for o in &overflows {
        log::warn!(
            "node {}: fragment on page {} extends {:.2}px past the {:.2}px page \
             content strip; content may be painted over the bottom margin or \
             clipped away entirely",
            o.node_id,
            o.page_index,
            o.overshoot_px,
            page_height_px,
        );
    }

    #[cfg(test)]
    panic!(
        "fulgur-pgbrk R3: {} fragment(s) placed past the {page_height_px}px page \
         content strip (content would be painted over the bottom margin or \
         clipped off the paper): {overflows:?}",
        overflows.len(),
    );
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
    /// the body has no children — both are expected for empty
    /// documents (a single empty page is still produced downstream).
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

        // Reborrow the fixed walk inputs once; recursive calls pass the
        // shared `FragmentationCtx` instead of re-copying arguments.
        let cx = FragmentationCtx {
            doc: &*self.doc,
            styles: self.column_styles,
            used_page_names: self.used_page_names.as_ref(),
            running: self.running_store,
            page_h: self.page_height_px,
        };
        // Body is the depth-0 container; its frame holds the same
        // per-container mutable state the nested walker uses (see
        // `fragment_block_subtree`). `parent_slice: None` marks "no
        // parent to close" at the root.
        let mut frame = ContainerFrame {
            id: body_id,
            x_in_body: body_x,
            width: body_w,
            page: 0,
            cursor_y: 0.0,
            page_start_y: 0.0,
            page_taffy_origin: 0.0,
            origin_pending_target_y: None,
            origin_pending_anchor: None,
            origin_pending_same_row: None,
            prev_used_page: None,
            emitted_anything: false,
            allow_leading_break: true,
            depth: 0,
            row_state: None,
            parent_slice: None,
            kind: ContainerKind::RootBody,
        };

        // fulgur-s67g Phase 2.3 (counter parity follow-up): record
        // body itself as a fragment on page 0. body's own
        // counter-reset / string-set / bookmark declarations must fire
        // only on page 0 (GCPM string-set / counters §3), so they need
        // to be recorded on the first page and only there.
        //
        // Without this entry the fragmenter's geometry table excludes body
        // entirely; `collect_counter_states` /
        // `collect_string_set_states` / `collect_bookmark_entries`
        // miss body's ops (e.g. `tests/gcpm_integration::test_counter_set`,
        // where body carries `counter-reset: chapter`). The body
        // fragment sits ahead of every body-direct-child entry in
        // NodeId order (Blitz allocates ids depth-first during parse,
        // so `body` is smaller than its descendants), so per-page
        // walks pick up body's ops before descendants — matching the
        // document tree-walk order the collectors expect.
        if matches!(frame.kind, ContainerKind::RootBody) {
            self.geometry
                .entry(frame.id)
                .or_default()
                .fragments
                .push(Fragment {
                    page_index: 0,
                    x: frame.x_in_body.as_px(),
                    y: 0.0_f32.as_px(),
                    width: frame.width.as_px(),
                    height: body_layout.size.height.as_px(),
                });
        }

        // fulgur-bq6i / fulgur-yb27: anonymous block wrappers Stylo
        // synthesizes around inline-level siblings live ONLY in
        // `layout_children` (CSS 2.1 §9.2.1.1) — the shared
        // enumeration policy is `layout_children_of`.
        let children = layout_children_of(cx.doc, frame.id);

        let mut emitted = 0usize;
        // Tracks the bottom edge of the previously emitted in-flow child
        // in body-content-box coordinates. Used to pick up inter-child
        // gaps (collapsed margins, padding) that Blitz baked into each
        // child's `final_layout.location.y` but the cursor-only walk
        // would otherwise miss — the child margin gaps must be included
        // in body's normal-flow height (CSS 2.1 §10.6.3) so per-page
        // walks match the recorded fragment tops.
        let mut prev_bottom_y_in_body: f32 = 0.0;

        for child_id in children {
            // Shared skip: dangling id / whitespace-only text /
            // out-of-flow (`position: absolute` / `fixed`) — see
            // `is_walkable_skip`.
            if is_walkable_skip(cx.doc, child_id) {
                continue;
            }
            let Some(child) = cx.doc.get_node(child_id) else {
                continue;
            };
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
            if cx
                .running
                .is_some_and(|s| s.instance_for_node(child_id).is_some())
            {
                if child.element_data().is_some() {
                    let layout = child.final_layout;
                    self.geometry
                        .entry(child_id)
                        .or_default()
                        .fragments
                        .push(Fragment {
                            page_index: frame.page,
                            x: (frame.x_in_body + layout.location.x).as_px(),
                            y: frame.cursor_y.as_px(),
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
            // false) into the normal path where `frame.cursor_y += child_h`
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
                frame.width
            };
            if child_h <= 0.0 {
                // Shared zero-height child branch (see
                // `fragment_zero_height_child`). Body frames advance
                // the page only; the page-name break comparison is
                // unrestricted here (no `suppress_page_check`).
                if fragment_zero_height_child(
                    &cx,
                    &mut frame,
                    &mut self.geometry,
                    child,
                    child_id,
                    layout.location.y,
                    false,
                ) {
                    emitted += 1;
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
            frame.cursor_y += gap;

            // fulgur-k0g0: read break-before / break-after / break-inside
            // for this child from the column-style side-table (shared with
            // multicol). Default `Auto` for nodes the table does not cover.
            let break_props = cx
                .styles
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
            let (used_start, used_end) = cx.used_page_endpoints_of(child_id);
            let page_name_changed = !is_float
                && frame
                    .prev_used_page
                    .as_ref()
                    .is_some_and(|p| *p != used_start);

            // `break-before: page` forces a page boundary before the
            // child whenever there is in-flow content already placed on
            // the current page. A leading break-before on a fresh page
            // is a no-op (CSS 3 Fragmentation §3 collapses it).
            let explicit_break_before = matches!(
                break_props.break_before,
                Some(crate::draw_primitives::BreakBefore::Page)
            );
            let page_filling_break_child =
                gap > 0.0 && child_h >= cx.page_h * 0.9 && gap + child_h <= cx.page_h + 0.5;
            if (explicit_break_before || page_name_changed) && emitted > 0 && frame.cursor_y > 0.0 {
                frame.page += 1;
                frame.cursor_y = if explicit_break_before && page_filling_break_child {
                    gap
                } else {
                    0.0
                };
            }

            // fulgur-p55h: if the child carries a Parley inline layout,
            // probe its line metrics and split at line boundaries —
            // mirrors the v1 paragraph-pageable split path (removed in
            // PR 8j-1; see git history) but inside the Taffy hook rather
            // than post-conversion.
            //
            // fulgur-k0g0: when `break-inside: avoid` is set, fall
            // through to the block path below so the paragraph emits
            // whole instead of splitting between lines.
            //
            // fulgur-pgbrk: except when honouring it is impossible.
            // `avoid` is a preference, not a guarantee — CSS
            // Fragmentation 3 §4.4 requires it to be ignored when the box
            // cannot fit in a single fragmentainer. Obeying it there does
            // not keep the paragraph whole, it just pushes the tail past
            // the page edge where it is discarded, so an unfulfillable
            // `avoid` must fall back to line-level splitting.
            //
            // Shared inline-root branch (see `fragment_inline_child`).
            // Body frames pass `false` for `suppress_page_check`; the
            // push-whole floor is the fixed 0.0 (leading-edge
            // propagation is always permitted at body level).
            if let Some(frag_count) = fragment_inline_child(
                &cx,
                &mut frame,
                &mut self.geometry,
                child,
                child_id,
                this_top_in_body,
                false,
            ) {
                emitted += frag_count;
                prev_bottom_y_in_body = this_top_in_body + child_h;
                continue;
            }

            // fulgur-g9e3.1 + fulgur-a36m + fulgur-7hf5: unified
            // recursion gate covering all three break cases (truly
            // oversized, in-place mid-element split, forced break
            // below) — see `fragment_recursion_child`. Body passes
            // `false` for `suppress_page_check` and
            // `may_propagate_break`: the root has no parent to close
            // and no one to hand a break up to, so a child's
            // `RequestBreakBefore` is always resolved by the
            // advance-and-retry inside the helper.
            if fragment_recursion_child(
                &cx,
                &mut frame,
                &mut self.geometry,
                child,
                this_top_in_body,
                false,
                false,
            )
            .is_some()
            {
                emitted += 1;
                prev_bottom_y_in_body = this_top_in_body + child_h;
                continue;
            }

            // No recursion needed — apply the existing strip-overflow
            // page advance for non-splittable / fits-fine children.
            // `break-inside: avoid` collapses to this path via
            // `fragment_inline_child`'s `avoid` suppression (it just
            // suppresses the inline split branch; remaining-strip
            // overflow handling is identical).
            if frame.cursor_y > 0.0 && frame.cursor_y + child_h > cx.page_h {
                frame.page += 1;
                frame.cursor_y = 0.0;
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
            let frag_x = frame.x_in_body + layout.location.x;

            // fulgur-sbw2: a child whose CSS-resolved height alone
            // exceeds `page_height_px` (e.g. `<div height:300vh>`)
            // must be sliced across pages. The recursion gate above
            // returns false for this shape because the child's *own*
            // children fit the strip (the gate measures descendant
            // overflow, not the parent's intrinsic height), so we
            // would otherwise emit a single oversized fragment on the
            // current page and stop. `slice_oversized_leaf` emits one
            // fragment per page strip with page-local y — see its doc
            // comment for the +1px oversize tolerance and the atomic
            // transform exclusion (fulgur-pgbrk R7 shares the helper
            // with the nested walk).
            let has_transform = child
                .primary_styles()
                .is_some_and(|s| !s.get_box().transform.0.is_empty());
            if !has_transform && child_h > cx.page_h + 1.0 {
                let (np, nc) = slice_oversized_leaf(
                    &mut self.geometry,
                    cx.doc,
                    child_id,
                    frag_x,
                    child_w,
                    child_h,
                    frame.page,
                    frame.cursor_y,
                    cx.page_h,
                    0,
                );
                frame.page = np;
                frame.cursor_y = nc;
                emitted += 1;
                prev_bottom_y_in_body = this_top_in_body + child_h;
                if !is_float {
                    frame.prev_used_page = Some(used_end.clone());
                }
                if matches!(
                    break_props.break_after,
                    Some(crate::draw_primitives::BreakAfter::Page)
                ) {
                    frame.page += 1;
                    frame.cursor_y = 0.0;
                }
                continue;
            }

            let frag = Fragment {
                page_index: frame.page,
                x: frag_x.as_px(),
                y: frame.cursor_y.as_px(),
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
            // body child is still future work. Y / width / height
            // come from the descendant's `final_layout` and are
            // mainly informational; the collectors that consume
            // this geometry today read only `page_index`.
            record_subtree_descendants(
                &mut self.geometry,
                cx.doc,
                child_id,
                frame.page,
                frame.cursor_y,
                frag_x,
                0,
            );

            frame.cursor_y += child_h;
            emitted += 1;
            prev_bottom_y_in_body = this_top_in_body + child_h;
            if !is_float {
                frame.prev_used_page = Some(used_end.clone());
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
                frame.page += 1;
                frame.cursor_y = 0.0;
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
    for child_id in layout_children_of(doc, parent_id) {
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
    // fulgur-bq6i: anonymous block wrappers live only in
    // `layout_children` — the shared enumeration policy is
    // `layout_children_of`.
    for child_id in layout_children_of(doc, parent_id) {
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

/// fulgur-pgbrk R7 reachability gate for [`slice_oversized_leaf`] at the
/// nested walk's per-child call site: true when a child is either
/// **oversized** (taller than a full page strip on its own, e.g.
/// `<div height:300vh>`) or **spills the current strip** (fits a fresh
/// strip but crosses the strip boundary from wherever it currently
/// sits — R7b, flex/grid cells and other floor-pinned children whose
/// items are not class-A break points).
///
/// Both checks share [`OVERSIZE_QUANTIZATION_TOLERANCE_PX`]
/// deliberately: at `child_page_y == 0` (a child sitting at the top of
/// a fresh strip) the two conditions collapse to the same shape, and
/// using a tighter tolerance for one than the other let a box in the
/// gap between them defeat the oversize tolerance via whichever
/// condition had the smaller slack (Codex review, PR #719) — see
/// `slice_oversized_leaf`'s "Oversize tolerance" doc section for the
/// 220pt→294px Taffy-rounding case the tolerance exists to absorb.
fn needs_leaf_slicing(child_h: f32, child_page_y: f32, page_height_px: f32) -> bool {
    let oversized = child_h > page_height_px + OVERSIZE_QUANTIZATION_TOLERANCE_PX;
    let spills_strip = child_page_y + child_h > page_height_px + OVERSIZE_QUANTIZATION_TOLERANCE_PX;
    oversized || spills_strip
}

/// fulgur-sbw2 / fulgur-pgbrk R7: emit a monolithic leaf whose
/// CSS-resolved height alone exceeds `page_height_px` (e.g.
/// `<div height:300vh>`) as one fragment per page strip, instead of a
/// single oversized fragment that hangs past the page bottom. Shared
/// by the body-direct walk (`fragment_pagination_root`) and the nested
/// walk (`fragment_block_subtree`) so monolithic content is treated
/// uniformly at every depth — css-break-3 §4.1 permits either
/// treatment of monolithic boxes, and fulgur slices everywhere.
///
/// Slice 1 lands at (`page_index`, `cursor_y`) with its height clipped
/// to the remaining strip; slices 2..N start at `y = 0` on successive
/// pages, each clipped to a full strip. `PaginationGeometry::is_split()`
/// flips to `true` automatically once `fragments.len() > 1`, and
/// render.rs picks the per-slice height accordingly. Descendants are
/// recorded against the *first* slice only; exact mid-element
/// pagination of nested content is still future work (see
/// `record_subtree_descendants` notes). `depth` is the caller's
/// recursion depth, forwarded to that recording.
///
/// Returns the updated `(page_index, cursor_y)`: following content
/// resumes directly after the last slice.
///
/// Callers must apply the two reachability gates before invoking:
///
/// - **Oversize tolerance.** Stylo / Taffy round CSS-resolved
///   `<length>` values to integer CSS pixels in some cases — a 220pt
///   spacer (= 293.333… px) is reported back as `h = 294` while
///   `page_height_px` is computed without that round-trip
///   (= 293.33334). A literal `>` then trips by ~0.67 px and the
///   spacer is wrongly sliced into two pages (gcpm_snapshot tests
///   regress: spacer + page-break pair becomes 2 + 1 instead of 1 + 1
///   per section). One CSS pixel of slack absorbs the quantization
///   without letting truly oversized content (`300vh ≈ 880 px` on a
///   293-px strip) slip through — hence `h > page_height_px + 1.0`.
/// - **Atomic transform check.** A transformed subtree paints as a
///   single atomic box (CSS Transforms §6.1) — it must never split
///   across pages because the rotation / skew / matrix is applied to a
///   single shape, not per-slice. `contain: size` is NOT excluded: a
///   `<div contain:size height:350vh>` still spans four pages visually
///   (WPT `monolithic-overflow-022-print`), it's only the descendant
///   content that's atomic.
#[allow(clippy::too_many_arguments)]
fn slice_oversized_leaf(
    geometry: &mut PaginationGeometryTable,
    doc: &BaseDocument,
    id: usize,
    x_in_body: f32,
    w: f32,
    h: f32,
    mut page_index: u32,
    cursor_y: f32,
    page_height_px: f32,
    depth: usize,
) -> (u32, f32) {
    let first_slice_h = (page_height_px - cursor_y).min(h);
    geometry.entry(id).or_default().fragments.push(Fragment {
        page_index,
        x: x_in_body.as_px(),
        y: cursor_y.as_px(),
        width: w.as_px(),
        height: first_slice_h.as_px(),
    });
    record_subtree_descendants(geometry, doc, id, page_index, cursor_y, x_in_body, depth);
    let mut remaining = h - first_slice_h;
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
    let collapse_childless = page_height_px > 0.0
        && (remaining / page_height_px).ceil() > crate::MAX_PAGES as f32
        && !subtree_has_rendered_content(doc, id, 0);
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
             {h}px (slicing would exceed the {}-page limit) \
             to a single page (fulgur-ezst)",
            crate::MAX_PAGES,
        );
        (page_index + 1, 0.0)
    } else {
        // fulgur-2m6w: cap the per-page-strip slicing at
        // `MAX_PAGES`. `h` is attacker-controlled CSS
        // (`height` / `vh`), so without this bound a few bytes of
        // HTML (`<div style="height:99999999px">`) generate ~10^5
        // fragments — and a non-finite height would never reduce
        // `remaining`, looping forever. The `page_index` ceiling is
        // load-bearing on its own (it stops the loop even when
        // `remaining` is `+inf`); content past the cap is truncated.
        while remaining > 0.0 && page_index < crate::MAX_PAGES {
            page_index += 1;
            last_slice_h = remaining.min(page_height_px);
            geometry.entry(id).or_default().fragments.push(Fragment {
                page_index,
                x: x_in_body.as_px(),
                y: 0.0_f32.as_px(),
                width: w.as_px(),
                height: last_slice_h.as_px(),
            });
            remaining -= last_slice_h;
        }
        if remaining > 0.0 {
            log::warn!(
                "pagination: block height {h}px exceeds the \
                 {}-page limit; truncating remaining content to \
                 bound rendering (fulgur-2m6w)",
                crate::MAX_PAGES,
            );
        }
        let cursor_y = if h - first_slice_h > 0.0 {
            last_slice_h
        } else {
            cursor_y + first_slice_h
        };
        (page_index, cursor_y)
    }
}

/// fulgur-7hf5 (Phase 3.1.5c) + fulgur-pgbrk walker-convergence
/// phase 5: pre-flight check for the recursion gate — true if walking
/// `node_id`'s direct children would cross a page boundary at
/// `available_strip`.
///
/// This was `would_split_block_subtree`, a cheaper-than-real
/// `fragment_block_subtree` simulator with its own overflow check —
/// and it was **floor-blind**: it accumulated a cursor from 0 in
/// available-strip space with no `overflow_floor` concept, so for a
/// LEADING child it reported "would split" where the real walk with
/// `allow_leading_break == false` (flex / grid / atomic-inline /
/// orthogonal subtrees) pins the child at `page_start_y` and places
/// it overflowing instead. That divergence (deferred during the
/// fulgur-pgbrk Risk-1 `break_decision` extraction) cost a wasted
/// recursion both paths eventually resolved identically.
///
/// The probe is now assimilated into the walk: it composes the walk's
/// OWN child enumeration ([`layout_children_of`] +
/// [`is_walkable_skip`], fulgur-yb27 — anonymous block wrappers Stylo
/// synthesizes around inline-level siblings participate, CSS 2.1
/// §9.2.1.1) with the walk's OWN break predicate ([`break_decision`])
/// evaluated at the walk's floor: `0.0` iff `allow_leading_break` (a
/// leading child's break may propagate to the container's own leading
/// edge, css-break-3 §3), else `page_start_y`. Same predicate, same
/// floor, same enumeration — the gate cannot disagree with the walk
/// about the leading-child floor again.
///
/// `available_strip` is the strip height left below the container's
/// entry cursor on the current page; the walk seeds the container
/// frame's `page_start_y` from that cursor, so the page-local entry y
/// is recovered as `page_h - available_strip` and child tops
/// accumulate from it in page space. `allow_leading_break` is the
/// would-be child frame's inherited permission
/// (`frame.allow_leading_break && !suppress_page_check` at the call
/// site); when it is false the walk's `propagate_leading_break` is
/// false too, so the floor derived here is the walk's exact floor.
/// (When the probed node ITSELF establishes a suppressed context the
/// gate stays conservative: floor `0.0` may still report a split the
/// pinned walk would place — the wasted-recursion direction, never an
/// output divergence.)
///
/// Returns `false` when the children all fit the available strip, so
/// the "parent CSS height > children sum" case falls through to the
/// caller's whole-emit path. A child taller than a full page reports
/// `true` regardless of the floor: the walk slices such a child
/// (`slice_oversized_leaf`) after placing it, which is a split.
/// Likewise (fulgur-pgbrk R7b) a child pinned at its floor — the
/// suppressed-floor containers (flex / grid / atomic-inline /
/// orthogonal) forbid the push — that spills the strip reports `true`:
/// the walk slices it in place, and when it carries descendants the
/// recursion must walk them so spill-shaped descendants fragment too
/// instead of being recorded whole past the strip.
fn subtree_requires_recursion(
    cx: &FragmentationCtx<'_>,
    node_id: usize,
    available_strip: f32,
    allow_leading_break: bool,
) -> bool {
    let page_h = cx.page_h;
    let page_start_y = (page_h - available_strip).max(0.0);
    let floor = if allow_leading_break {
        0.0
    } else {
        page_start_y
    };
    // Page-local top of the child under consideration; accumulates the
    // same inter-child gaps the walk picks up from Taffy's
    // `layout.location.y` (collapsed margins, padding — CSS 2.1
    // §10.6.3). Without the anon-wrapper enumeration a block whose tail
    // anonymous wrapper would overflow is missed by the preflight, the
    // recursion gate returns false, and the parent falls back to a
    // single oversize fragment (fulgur-yb27).
    let mut top: f32 = page_start_y;
    let mut prev_bottom: f32 = 0.0;
    for child_id in layout_children_of(cx.doc, node_id) {
        if is_walkable_skip(cx.doc, child_id) {
            continue;
        }
        let Some(child) = cx.doc.get_node(child_id) else {
            continue;
        };
        let layout = child.final_layout;
        let h = layout.size.height;
        if h <= 0.0 {
            continue;
        }
        let this_top = layout.location.y;
        let gap = (this_top - prev_bottom).max(0.0);
        top += gap;
        if break_decision(top, h, floor, page_h) == BreakDecision::PushToNextPage {
            return true;
        }
        if top <= floor && top + h > page_h + OVERFLOW_EPS_PX {
            // fulgur-pgbrk R7b: pinned at the floor and spilling the
            // strip — the walk's spill-slice fires exactly here, which
            // is a split; a spilling child with descendants needs the
            // recursion so its descendants fragment per strip too.
            // The epsilon mirrors the walk's spill predicate so gate
            // and walk stay in agreement on the boundary.
            return true;
        }
        if h > page_h {
            // Child itself oversized → the walk slices it → a split,
            // regardless of the floor (placement precedes slicing, and
            // `break_decision` is strict at the floor so the push
            // branch above can never fire for a leading oversized
            // child — no infinite page advance).
            return true;
        }
        top += h;
        prev_bottom = this_top + h;
    }
    false
}

/// fulgur-a36m (Phase 3.1.5b): true if any descendant of `node_id`
/// declares `break-before: page` or `break-after: page` in
/// `column_styles`. Walks the entire DOM subtree, bails at
/// [`crate::MAX_DOM_DEPTH`].
///
/// Detects forced page breaks anywhere below `node_id`, working on
/// Blitz nodes via the column-style side-table. Used by
/// `fragment_pagination_root` and `fragment_block_subtree` to decide
/// whether a body-direct (or nested) child needs to be entered for
/// break recursion even when it fits the current page strip whole.
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

/// fulgur-oc51: when a child crosses page boundaries *inside*
/// `fragment_block_subtree` (via recursion into its subtree, or via
/// fulgur-pgbrk R7 per-strip slicing of an oversized monolithic leaf),
/// emit the parent's fragment for every page span crossed. Without
/// this, only the trailing close at the end of the walk emits a parent
/// fragment (on the *last* page), so the parent's background / borders
/// disappear from the previous and intermediate pages.
///
/// The outgoing-page fragment spans `[page_start_y, page_height_px]`
/// (the parent's content extended to the page bottom — otherwise the
/// child would not have advanced past that page); each intermediate
/// page is a full strip. `emit_pre_page` gates the outgoing-page
/// fragment: the recursion branch suppresses it when the parent's
/// leading child handed the break up without placing anything on the
/// outgoing page (a full-strip background on an empty page would be
/// wrong); the slicing branch always passes `true` because slice 1
/// lands on the outgoing page by construction.
///
/// Emission dedupes across parallel flex / grid cells through the
/// row's `emitted_parent_pages` set (`Option<&mut RowState>` — absent
/// for sequential block flow, where each page is closed at most once).
fn emit_parent_page_spans(
    geometry: &mut PaginationGeometryTable,
    mut row_state: Option<&mut RowState>,
    parent_slice: &ParentSlice,
    pre_page: u32,
    post_page: u32,
    page_start_y: f32,
    emit_pre_page: bool,
) {
    if post_page <= pre_page {
        return;
    }
    let prev_height = (parent_slice.page_height_px - page_start_y).max(0.0);
    if emit_pre_page && prev_height > 0.0 {
        let should_emit = row_state
            .as_deref_mut()
            .map(|rs| rs.emitted_parent_pages.insert(pre_page))
            .unwrap_or(true);
        if should_emit {
            geometry
                .entry(parent_slice.id)
                .or_default()
                .fragments
                .push(Fragment {
                    page_index: pre_page,
                    x: parent_slice.x_in_body.as_px(),
                    y: page_start_y.as_px(),
                    width: parent_slice.width.as_px(),
                    height: prev_height.as_px(),
                });
        }
    }
    for p in (pre_page + 1)..post_page {
        let should_emit = row_state
            .as_deref_mut()
            .map(|rs| rs.emitted_parent_pages.insert(p))
            .unwrap_or(true);
        if should_emit {
            geometry
                .entry(parent_slice.id)
                .or_default()
                .fragments
                .push(Fragment {
                    page_index: p,
                    x: parent_slice.x_in_body.as_px(),
                    y: 0.0_f32.as_px(),
                    width: parent_slice.width.as_px(),
                    height: parent_slice.page_height_px.as_px(),
                });
        }
    }
}

fn fragment_block_subtree(
    cx: &FragmentationCtx<'_>,
    frame: &mut ContainerFrame,
    geometry: &mut PaginationGeometryTable,
) -> SubtreeResult {
    // Rebind the fixed inputs; mutable state lives on `frame`.
    let parent_id = frame.id;
    let parent_w = frame.width;
    let parent_x_in_body = frame.x_in_body;
    let page_in = frame.page;
    let cursor_in = frame.cursor_y;
    let depth = frame.depth;
    // fulgur-pgbrk: `frame.allow_leading_break` answers "may an
    // overflowing LEADING child propagate its break up to this box's own
    // leading edge (CSS Fragmentation 3 §3 — a break before a box's first
    // child is also a break before the box)?" True for block flow reached
    // from the body walk; cleared for the whole subtree below any
    // container that does not paginate its children independently —
    // flex / grid containers (whose items are not class A break points,
    // CSS Fragmentation 3 §3.2, and whose rows co-split in place per
    // fulgur-ysms), atomic inline containers, and orthogonal-flow
    // containers. Inside those, a leading child that overflows must
    // stay put and be clipped rather than dragging a box it cannot
    // actually move onto the next page.
    let allow_leading_break = frame.allow_leading_break;
    let doc = cx.doc;
    let column_styles = cx.styles;
    let used_page_names = cx.used_page_names;
    let page_height_px = cx.page_h;
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
        return SubtreeResult::Placed {
            page: page_in,
            cursor_y: cursor_in + h,
        };
    }
    let Some(parent) = doc.get_node(parent_id) else {
        return SubtreeResult::Placed {
            page: page_in,
            cursor_y: cursor_in,
        };
    };

    let mut page_index = page_in;
    let mut cursor_y = cursor_in;
    // Y on `page_index` where the parent's current-page fragment
    // starts. We close one parent fragment and start a new one each
    // time we cross a page boundary. The frame's `child()` helper
    // seeds it from the entry cursor.
    let mut page_start_y = frame.page_start_y;
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
    let mut page_taffy_origin = frame.page_taffy_origin;
    let mut origin_pending_target_y = frame.origin_pending_target_y;
    let mut origin_pending_anchor = frame.origin_pending_anchor;
    let mut origin_pending_same_row = frame.origin_pending_same_row;
    // fulgur-uebl: tracks the previous in-flow sibling's used page-name
    // for implicit forced-break detection; see `fragment_pagination_root`
    // for the rationale and the outer-Option semantics.
    let mut prev_used_page = frame.prev_used_page.clone();
    let mut row_state = frame.row_state.take();
    // fulgur-pgbrk R4 / R5: has this call pushed anything into `geometry`
    // yet? `SubtreeResult::RequestBreakBefore` promises the caller an
    // untouched table, so both producers refuse to fire once this is set.
    // Tracked explicitly rather than inferred from `cursor_y ==
    // page_start_y`, which a zero-height child leaves true after emitting.
    let mut emitted_anything = frame.emitted_anything;
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
    // fulgur-pgbrk: leading-edge break propagation is a block-flow rule.
    // `suppress_page_check` already identifies exactly the containers whose
    // children do not paginate independently, so reuse it as the gate and
    // thread the result into the recursion so the restriction covers the
    // entire subtree, not just the container's direct children.
    let propagate_leading_break = allow_leading_break && !suppress_page_check;
    // fulgur-pgbrk Risk 1: the parent's identity, x and width never
    // change across this walk, so the nine "close the parent on the page
    // it is leaving" sites capture them once here. The frame carries the
    // same slice so the unified child-visitor (design doc,
    // `fragment_container`) can read it through the frame.
    frame.parent_slice = Some(ParentSlice {
        id: parent_id,
        x_in_body: parent_x_in_body,
        width: parent_w,
        page_height_px,
    });
    let parent_slice = frame.parent_slice.expect("parent_slice set on entry");

    // fulgur-pgbrk R4 (css-break-3 §4.4 rule 2): breaking at a class A
    // point is forbidden when a common ancestor of the adjoining
    // siblings has `break-inside: avoid`. fulgur read `break-inside`
    // only on inline roots, so a block wrapper's `avoid` was never
    // consulted and the wrapper split between its children anyway.
    //
    // The box moves whole to the next page when it does not fit the
    // strip it is standing on but WOULD fit a fresh one. If it fits
    // neither, `avoid` is unfulfillable and §4.4's relaxation clause
    // applies — honouring it would only push the tail off the page,
    // so we fall through and split as before. This mirrors
    // `avoid_is_fulfillable` on the inline-root path.
    //
    // `cursor_in > 0.0` excludes a box already at a page top: there is
    // no earlier page to move to, and requesting one would bounce.
    if cursor_in > 0.0
        && propagate_leading_break
        && column_styles
            .and_then(|t| t.get(&parent_id))
            .is_some_and(|p| {
                matches!(
                    p.break_inside,
                    Some(crate::draw_primitives::BreakInside::Avoid)
                )
            })
    {
        let strip_here = (page_height_px - cursor_in).max(0.0);
        // The floor here is this frame's own: the branch above already
        // required `propagate_leading_break`, so the probe evaluates
        // `break_decision` at floor 0.0 — the same answer the old
        // floor-blind simulator gave on this path.
        let splits_here =
            subtree_requires_recursion(cx, parent_id, strip_here, propagate_leading_break);
        let splits_on_a_fresh_page =
            subtree_requires_recursion(cx, parent_id, page_height_px, propagate_leading_break);
        if splits_here && !splits_on_a_fresh_page {
            return SubtreeResult::RequestBreakBefore;
        }
    }

    // fulgur-yb27: prefer `layout_children` over raw `children` —
    // anonymous block wrappers Stylo synthesizes around inline-level
    // siblings (CSS 2.1 §9.2.1.1) carry their own Taffy layout and
    // `node_id` but live ONLY in `layout_children`. The shared
    // enumeration policy is `layout_children_of`; the shared
    // whitespace / out-of-flow skip is `is_walkable_skip`.
    //
    // Cross-page recursion correctness depends on fulgur-oc51's
    // parent-fragment push above — flipping this walk to
    // `layout_children` without that fix would lose the parent's
    // pre-recursion-page fragment in mo-006/008 (flex/grid + tall
    // monolithic + trailing inline text).
    for child_id in layout_children_of(doc, parent_id) {
        if is_walkable_skip(doc, child_id) {
            continue;
        }
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
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
                        origin_pending_anchor = None;
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
                        origin_pending_anchor = None;
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
            let anchor = origin_pending_anchor.take();
            if let Some((row_top, row_bottom, same_row_y)) = origin_pending_same_row.take()
                && this_top_in_parent < row_bottom - 0.5
            {
                target_y = same_row_y + (this_top_in_parent - row_top);
                page_taffy_origin = this_top_in_parent - (target_y - page_start_y);
            } else if let Some(anchor) = anchor {
                // Anchor the rebase on the point that
                // PRODUCED `target_y` (a recursed subtree's, or sliced
                // leaf's, own Taffy-space bottom edge —
                // `this_top_in_parent + box_h` captured at the setting
                // site) rather than THIS sibling's own
                // `this_top_in_parent`. Using this sibling's own top
                // here would force it flush against the previous
                // sibling's tail, discarding their natural (possibly
                // collapsed-margin) gap — see `origin_pending_anchor`'s
                // doc comment and the margin-collapse regression this
                // fixes.
                page_taffy_origin = anchor - (target_y - page_start_y);
            } else {
                page_taffy_origin = this_top_in_parent - (target_y - page_start_y);
            }
        }

        if child_h <= 0.0 {
            // Run the shared zero-height branch (see
            // `fragment_zero_height_child`). The helper reads /
            // writes the frame; the in-loop locals shadow its
            // fields, so flush them into the frame before the call
            // and rebind them out afterwards.
            frame.page = page_index;
            frame.cursor_y = cursor_y;
            frame.page_start_y = page_start_y;
            frame.page_taffy_origin = page_taffy_origin;
            frame.origin_pending_target_y = origin_pending_target_y;
            frame.origin_pending_anchor = origin_pending_anchor;
            frame.origin_pending_same_row = origin_pending_same_row;
            frame.prev_used_page = prev_used_page.clone();
            frame.row_state = row_state.take();
            if fragment_zero_height_child(
                cx,
                frame,
                geometry,
                child,
                child_id,
                this_top_in_parent,
                suppress_page_check,
            ) {
                emitted_anything = true;
            }
            page_index = frame.page;
            cursor_y = frame.cursor_y;
            page_start_y = frame.page_start_y;
            page_taffy_origin = frame.page_taffy_origin;
            origin_pending_target_y = frame.origin_pending_target_y;
            origin_pending_anchor = frame.origin_pending_anchor;
            origin_pending_same_row = frame.origin_pending_same_row;
            prev_used_page = frame.prev_used_page.clone();
            row_state = frame.row_state.take();
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
        // The cursor as of BEFORE this child — i.e. "has this container
        // actually placed content on the current page strip yet?".
        // `cursor_y` is about to be raised to this child's own top, which
        // for the strip's leading child is the container's
        // `border-top + padding-top` (or an uncollapsed top margin), NOT
        // placed content. Gates that mean "content is already on this
        // page" must read this, not the post-raise `cursor_y`, or a
        // container's own decoration masquerades as content.
        let content_on_this_page = cursor_y > page_start_y;
        // Update the cursor only when the child's bottom advances
        // past it. For block flow this matches cursor advancing by
        // `gap + child_h`; for grid parallel siblings the cursor
        // tracks the row's max bottom (so break-before / overflow
        // checks see the full row height).
        cursor_y = cursor_y.max(child_page_y);

        // fulgur-pgbrk R5 (css-break-3 §3.1.1): "A break-before value on
        // a first in-flow child box is propagated to its container."
        // Reaching here with nothing emitted means this IS the leading
        // in-flow child, so its own break point does not exist (§4.1: no
        // class C point without a gap) and the break belongs to the
        // container. Hand it up rather than dropping it, which is what
        // the `cursor_y > page_start_y` gate below did on its own.
        //
        // `!emitted_anything` is the whole of the leading-child test, and
        // it is also exactly `RequestBreakBefore`'s invariant (nothing
        // pushed into `geometry` yet). It deliberately does NOT also
        // require `cursor_y <= page_start_y`: line 3289 has already
        // raised `cursor_y` to `page_start_y + this_top_in_parent`, and
        // for a leading child `this_top_in_parent` IS the container's
        // `border-top + padding-top`. Gating on it therefore made any
        // container with top decoration fail to propagate its own leading
        // child's break, splitting instead and stranding a
        // decoration-sized stub on the outgoing page.
        //
        // `cursor_in > 0.0` keeps this from firing when the container
        // already starts at a page top: a break before it would be a
        // no-op, and requesting one would bounce between caller and
        // callee forever. It is also what terminates the caller's retry.
        let may_propagate_break = !emitted_anything && cursor_in > 0.0 && propagate_leading_break;
        if break_before_page && may_propagate_break {
            return SubtreeResult::RequestBreakBefore;
        }

        // Honour `break-before: page`. Leading collapse: only fires
        // when some content has already been placed on this page —
        // gated by `cursor_y > page_start_y` (mirrors body-level's
        // `cursor_y > 0.0` since body's implicit page_start is 0).
        // The breaking child lands on the next page, so the container
        // continues and claims the full strip here.
        if break_before_page && content_on_this_page {
            let resume = resume_taffy_origin(
                page_taffy_origin,
                page_start_y,
                page_height_px,
                this_top_in_parent,
            );
            parent_slice.close_continuing(geometry, row_state.as_mut(), page_index, page_start_y);
            page_index += 1;
            cursor_y = 0.0;
            page_start_y = 0.0;
            // The breaking child is the first in-flow child on the
            // new page strip. Rebase the Taffy origin so it lands at
            // `page_start_y` (= 0) — discarding the inter-child gap,
            // matching CSS 3 Fragmentation §3 (margins at forced breaks
            // truncate). Padding and border do not truncate, so any
            // unspent leading decoration still offsets it; see
            // `resume_taffy_origin`.
            page_taffy_origin = resume;
            child_page_y = this_top_in_parent - resume;
        }

        // (Strip-overflow page cut moved below the recursion gate as
        // part of fulgur-7hf5 — see the `if cursor_y > page_start_y
        // && cursor_y + child_h > page_height_px` block after the
        // gate. The gate must run from the **current** cursor so an
        // in-place split produces a `WithinChild`-shaped result on
        // the current strip, not a pre-advanced fresh page.)

        let child_x_in_body = parent_x_in_body + layout.location.x;

        // fulgur-pgbrk: split a NESTED inline root at its Parley line
        // boundaries, mirroring `fragment_pagination_root`'s body-direct
        // branch. Previously this existed only at body level: a
        // multi-line `<p>` (or a Stylo-synthesized anonymous block around
        // an inline run) nested inside a recursed subtree fell through to
        // the block path below and emitted as ONE oversized fragment, so
        // every line past the page bottom was drawn into the margin strip
        // and then off the paper, where it is discarded — silent content
        // loss, the escalation in the bug report's §1c. The line-breaking
        // machinery was correct all along, it was simply unreachable from
        // the recursion.
        //
        // `break-inside: avoid` suppresses the split, same as body level,
        // so the paragraph stays whole and takes the block path — unless
        // honouring it is impossible (CSS Fragmentation 3 §4.4: `avoid` is
        // a preference and must be ignored when the box cannot fit a
        // single fragmentainer, where obeying it would only push the tail
        // off the page and destroy it). Mirrors the body-direct branch.
        //
        // Shared inline-root branch (see `fragment_inline_child`). The
        // helper reads / writes the frame; the in-loop locals shadow its
        // fields, so flush them into the frame before the call and rebind
        // them out afterwards — same convention as the zero-height
        // branch. The nested-only parent-fragment emission (fulgur-oc51)
        // and `row_state` bookkeeping live inside the helper behind
        // `frame.kind == ContainerKind::Nested` / `frame.parent_slice`.
        frame.page = page_index;
        frame.cursor_y = cursor_y;
        frame.page_start_y = page_start_y;
        frame.page_taffy_origin = page_taffy_origin;
        frame.origin_pending_target_y = origin_pending_target_y;
        frame.origin_pending_anchor = origin_pending_anchor;
        frame.origin_pending_same_row = origin_pending_same_row;
        frame.prev_used_page = prev_used_page.clone();
        frame.row_state = row_state.take();
        let inline_split = fragment_inline_child(
            cx,
            frame,
            geometry,
            child,
            child_id,
            this_top_in_parent,
            suppress_page_check,
        );
        page_index = frame.page;
        cursor_y = frame.cursor_y;
        page_start_y = frame.page_start_y;
        page_taffy_origin = frame.page_taffy_origin;
        origin_pending_target_y = frame.origin_pending_target_y;
        origin_pending_anchor = frame.origin_pending_anchor;
        origin_pending_same_row = frame.origin_pending_same_row;
        prev_used_page = frame.prev_used_page.clone();
        row_state = frame.row_state.take();
        if inline_split.is_some() {
            emitted_anything = true;
            continue;
        }

        // fulgur-7hf5 (Phase 3.1.5c): unified recursion gate matching
        // `fragment_pagination_root`'s body-direct branch — see
        // `fragment_recursion_child`. Same flush / rebind convention
        // as the zero-height and inline helpers above: the helper
        // reads / writes the frame; the in-loop locals shadow its
        // fields, so flush them into the frame before the call and
        // rebind them out afterwards. `may_propagate_break` carries
        // the nested leading-child rule (css-break-3 §3.1.1 — a break
        // before a box's first child is a break before the box,
        // recursively): the helper hands `RequestBreakBefore` back
        // only when the child is OUR leading child and breaks may
        // still travel up. Computed once before the `break-before`
        // check above, since nothing between there and here can change
        // its inputs (the branches that set `emitted_anything` all
        // `continue`).
        frame.page = page_index;
        frame.cursor_y = cursor_y;
        frame.page_start_y = page_start_y;
        frame.page_taffy_origin = page_taffy_origin;
        frame.origin_pending_target_y = origin_pending_target_y;
        frame.origin_pending_anchor = origin_pending_anchor;
        frame.origin_pending_same_row = origin_pending_same_row;
        frame.prev_used_page = prev_used_page.clone();
        frame.row_state = row_state.take();
        let recursed = fragment_recursion_child(
            cx,
            frame,
            geometry,
            child,
            this_top_in_parent,
            suppress_page_check,
            may_propagate_break,
        );
        page_index = frame.page;
        cursor_y = frame.cursor_y;
        page_start_y = frame.page_start_y;
        page_taffy_origin = frame.page_taffy_origin;
        origin_pending_target_y = frame.origin_pending_target_y;
        origin_pending_anchor = frame.origin_pending_anchor;
        origin_pending_same_row = frame.origin_pending_same_row;
        prev_used_page = frame.prev_used_page.clone();
        row_state = frame.row_state.take();
        match recursed {
            Some(RecursionOutcome::Placed) => {
                emitted_anything = true;
                continue;
            }
            Some(RecursionOutcome::RequestBreakBefore) => {
                return SubtreeResult::RequestBreakBefore;
            }
            None => {}
        }

        // No recursion — apply the strip-overflow page cut for
        // children that don't split (non-splittable, or splittable
        // but all grandchildren fit the available strip — the
        // parent-CSS-height-vs-children-sum case stays here).
        // Use `child_page_y + child_h` (the actual placement bottom)
        // rather than `cursor_y + child_h` so a parallel sibling
        // returning to a smaller page-local y is checked correctly.
        //
        // fulgur-pgbrk: the guard is `child_page_y > 0.0`, NOT
        // `child_page_y > page_start_y`. The latter is never true for the
        // FIRST in-flow child on a strip (its rebased `child_page_y` *is*
        // `page_start_y`), so a parent that began mid-page could never
        // break before its own leading child — it laid the child out past
        // the page bottom, through the margin strip and off the paper,
        // where the content is silently discarded. Comparing against 0
        // instead propagates the break up to the parent's leading edge
        // (CSS Fragmentation 3 §3: a break before a box's first child is
        // also a break before the box), which is legal precisely because
        // the parent has not emitted anything on this page yet. At
        // `child_page_y == 0.0` we are already at the top of a fresh page
        // and there is nowhere left to push to, so the gate correctly
        // stops recursing pages and the oversized leaf overflows (the
        // inline-root branch above is what saves multi-line content in
        // that case).
        let overflow_floor = if propagate_leading_break {
            0.0
        } else {
            page_start_y
        };
        if break_decision(child_page_y, child_h, overflow_floor, page_height_px)
            == BreakDecision::PushToNextPage
        {
            // css-break-3 §3.1.1 / §4.1, the unforced twin of the
            // `break-before` propagation above: pushing our LEADING child
            // to the next page is a break at the class B point between
            // this container's content edge and that child. Taking it
            // leaves the container's `border-top + padding-top` behind on
            // the outgoing page — legal only if that decoration actually
            // fits there. When nothing has been emitted yet it does not:
            // the container has no content on this page, so the only
            // thing the stub would paint is decoration for content that
            // has moved away. Hand the break up so the caller re-places
            // the whole container on the next page (which is already what
            // a childless or inline-root container does at this shape).
            //
            // Safe with respect to `RequestBreakBefore`'s invariant:
            // `may_propagate_break` requires `!emitted_anything`, so
            // `geometry` is untouched. Terminating: the caller re-enters
            // at `cursor_in == 0.0`, which the predicate excludes.
            if may_propagate_break {
                return SubtreeResult::RequestBreakBefore;
            }
            // Only claim a fragment on the page we are leaving when the
            // parent actually placed content there. When the break is
            // propagated from the parent's leading edge (nothing emitted
            // yet, `cursor_y == page_start_y`) the parent does not appear
            // on the outgoing page at all, and a zero-height fragment
            // would additionally flip `is_split()` on and corrupt
            // downstream slicing.
            let resume = resume_taffy_origin(
                page_taffy_origin,
                page_start_y,
                page_height_px,
                this_top_in_parent,
            );
            if cursor_y > page_start_y {
                parent_slice.close_continuing(
                    geometry,
                    row_state.as_mut(),
                    page_index,
                    page_start_y,
                );
            }
            page_index += 1;
            cursor_y = 0.0;
            page_start_y = 0.0;
            // Forced to a fresh page: rebase the Taffy origin so the
            // current child lands at the top of the new strip, below
            // whatever leading decoration of this container the
            // outgoing page did not spend (see `resume_taffy_origin`).
            // Sequential siblings then continue from this point.
            page_taffy_origin = resume;
            child_page_y = this_top_in_parent - resume;
        }

        // fulgur-pgbrk R7: slice a child the walk cannot place whole,
        // uniform with the body-direct walk (fulgur-sbw2). Two shapes
        // reach this branch, both handled identically by
        // `slice_oversized_leaf` (whose doc comment carries the +1px
        // oversize tolerance and the atomic-transform exclusion — the
        // SAME gates the body-direct branch applies):
        //
        // 1. **Oversized**: a monolithic leaf whose own height exceeds
        //    the fragmentainer (childless, or whose grandchildren all
        //    fit — either way the recursion gate above said "no break
        //    points below"). css-break-3 §4.1 permits either treatment
        //    of monolithic content; fulgur slices everywhere so the
        //    geometry never lands outside the page box. A nested
        //    `overflow: hidden` box is monolithic identically.
        // 2. **Strip spill (R7b)**: a child that fits a fresh strip
        //    but crosses the current strip boundary at a floor that
        //    forbids the push — flex / grid cells, atomic-inline and
        //    orthogonal children, whose items are not class A break
        //    points (§2.1 / §4.1) and whose leading child is pinned at
        //    the row's entry cursor by the suppressed floor. Per §2.1
        //    each cell / item is a parallel fragmentation flow, and
        //    §4.1 lets a layout model add break points; since width is
        //    page-invariant the slice is exact. Each crossed strip
        //    gets one fragment clipped to it at the box's computed
        //    content offsets — the RowState co-split machinery then
        //    starts the same-row sibling at the identical cursor, so
        //    parallel cells clip in lockstep and
        //    `find_overflowing_fragments` has nothing left to catch.
        //
        // In both shapes the whole-emit fallback below is unreachable:
        // geometry that would hang past the strip is sliced instead
        // of overflowing the page box.
        let has_transform = child
            .primary_styles()
            .is_some_and(|s| !s.get_box().transform.0.is_empty());
        if !has_transform && needs_leaf_slicing(child_h, child_page_y, page_height_px) {
            emitted_anything = true;
            let pre_slice_page = page_index;
            let (np, nc) = slice_oversized_leaf(
                geometry,
                doc,
                child_id,
                child_x_in_body,
                child_w,
                child_h,
                page_index,
                child_page_y,
                page_height_px,
                depth + 1,
            );
            page_index = np;
            // fulgur-u0p0: when the slicing stayed on the same page
            // (only reachable at the `MAX_PAGES` ceiling), keep the
            // larger of the parent's existing `cursor_y` (row max
            // bottom from a previous parallel sibling) and the slicer's
            // returned cursor. When it crossed pages, the old page's
            // `cursor_y` is stale — adopt the returned one directly.
            cursor_y = if np == pre_slice_page {
                cursor_y.max(nc)
            } else {
                nc
            };
            // fulgur-oc51: the child crossed pages, so the parent's
            // fragment must exist on every crossed page — otherwise its
            // background / borders vanish from every page but the last.
            // Slice 1 lands on the outgoing page by construction, so
            // the pre-page emission is unconditional here.
            emit_parent_page_spans(
                geometry,
                row_state.as_mut(),
                &parent_slice,
                pre_slice_page,
                page_index,
                page_start_y,
                true,
            );
            page_start_y = 0.0;
            origin_pending_target_y = Some(cursor_y);
            let row_top = this_top_in_parent;
            let row_bottom = row_top + child_h;
            origin_pending_same_row = allow_same_row_rebase.then_some((row_top, row_bottom, 0.0));
            // Non-row anchor (see
            // `origin_pending_anchor`'s doc comment) — this sliced
            // leaf's own Taffy-space bottom edge, so the next sibling's
            // natural gap to it survives the rebase. Set
            // unconditionally; the consumer prefers
            // `origin_pending_same_row` when present, so this is a
            // no-op in the flex/grid row case above.
            origin_pending_anchor = Some(this_top_in_parent + child_h);
            // fulgur-ysms: mark the row crossed so subsequent same-row
            // cells co-split from the row-start state (uniform with the
            // recursion branch's handling).
            if let Some(ref mut rs) = row_state {
                rs.crossed_by_recursion = true;
            }

            // Honour `break-after: page` after the slicing (mirrors the
            // whole-emit tail below).
            if break_after_page {
                parent_slice.close_unforced(
                    geometry,
                    row_state.as_mut(),
                    page_index,
                    page_start_y,
                    cursor_y,
                );
                page_index += 1;
                cursor_y = 0.0;
                page_start_y = 0.0;
                (
                    origin_pending_target_y,
                    origin_pending_anchor,
                    origin_pending_same_row,
                ) = (Some(page_start_y), None, None);
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

        // Child fits the strip (or — before fulgur-pgbrk R7 — was an
        // atomic oversized leaf emitted whole; now only transform-atomic
        // or sub-tolerance overshoot reaches whole-emit). Emit its
        // fragment and recurse into descendants on the same page.
        emitted_anything = true;
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
            parent_slice.close_unforced(
                geometry,
                row_state.as_mut(),
                page_index,
                page_start_y,
                cursor_y,
            );
            page_index += 1;
            cursor_y = 0.0;
            page_start_y = 0.0;
            (
                origin_pending_target_y,
                origin_pending_anchor,
                origin_pending_same_row,
            ) = (Some(page_start_y), None, None);
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

    // css-break-3 §5.4 (`box-decoration-break: slice`): the container's
    // own trailing decoration belongs to its LAST fragment. `cursor_y`
    // tracks child content only — children are laid out inside the
    // padding box — so without this the box's `padding-bottom +
    // border-bottom` is dropped from every fragmented container, and
    // `render.rs` closes the border box at the last child's edge.
    //
    // Folded into `cursor_y` rather than into the emitted height alone
    // so the returned cursor describes the box's real bottom: the
    // caller places the next sibling from it (`fragment_recursion_child`
    // adopts `new_cursor` verbatim), and the non-recursed sibling path
    // it has to agree with advances by Taffy's full border-box `child_h`.
    //
    // The leading decoration needs no equivalent: children sit
    // `lead_in` below the container's own top, so the first fragment —
    // measured from `page_start_y` to the first child's bottom —
    // already contains it.
    let (lead_in, lead_out) = box_decoration(parent);
    cursor_y += lead_out;

    // Close the parent's fragment for the final page span. Always
    // emit at least one fragment so the parent is represented in
    // geometry — `collect_counter_states` and friends look up nodes
    // by id, and a missing entry would silently bypass the parity
    // gate via the early `counter_ops_by_node ⊄ geometry` check in
    // `render.rs`. Height may be 0 when every child was skipped
    // (whitespace / OOF / running) — that's intentional and
    // matches `fragment_pagination_root`'s zero-height-element path.
    parent_slice.close_forced(geometry, page_index, page_start_y, cursor_y);

    // Publish the split, for the same reason the inline path does: a
    // consumer partitioning this box's content across its fragments has
    // to know which part of the first and last fragment is decoration
    // rather than content.
    if let Some(entry) = geometry.get_mut(&parent_id) {
        entry.content_lead_in = lead_in.as_px();
        entry.content_lead_out = lead_out.as_px();
    }

    SubtreeResult::Placed {
        page: page_index,
        cursor_y,
    }
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

/// Border-box metrics for an inline root (fulgur-pgbrk R1).
///
/// Returns `(lead_in, lines_h, lead_out)`, all in CSS px:
///
/// - `lead_in` — `border-top + padding-top`, the decoration above the
///   first line box.
/// - `lines_h` — the line-box extent, `last.max_coord - first.min_coord`.
/// - `lead_out` — `padding-bottom + border-bottom`, below the last line box.
///
/// The two decoration edges must come from Taffy, not from the line
/// metrics: Parley lays an inline root out in **content-box** coordinates,
/// so a `<p>` with `border-top: 7px; padding: 150px 0 90px` reports
/// `line_metrics[0].0 == 0.0` while `final_layout` reports
/// `border.top = 7`, `padding.top = 150`, `padding.bottom = 90`. Measuring
/// the box as `last.1 - first.0` therefore under-reports it by
/// `lead_in + lead_out` — the R1 defect: the push-whole decision never
/// fires, and the tail runs off the paper.
///
/// Non-finite Taffy values are sanitized to `0.0` — same convention as
/// the child-height sanitization in `fragment_block_subtree`.
fn inline_root_box_metrics(node: &blitz_dom::Node, line_metrics: &[(f32, f32)]) -> (f32, f32, f32) {
    let (lead_in, lead_out) = box_decoration(node);
    let lines_h = match (line_metrics.first(), line_metrics.last()) {
        (Some(first), Some(last)) => (last.1 - first.0).max(0.0),
        _ => 0.0,
    };
    (lead_in, lines_h, lead_out)
}

/// Where a container's continuation resumes, in the container's own
/// Taffy coordinate space — the value to install as `page_taffy_origin`
/// after advancing a page, so that the child being placed lands at
/// `page_start_y + (child_top - origin)`.
///
/// Two things compete for the new strip's top:
///
/// - `cut` — the box-local y at which the outgoing page ended. The
///   container's outgoing fragment spans its whole remaining strip (see
///   [`ParentSlice::close_continuing`]), so everything above `cut` is
///   already painted and must not be painted again.
/// - `child_top` — the child's own box-local top.
///
/// Taking the **earlier** of the two is what makes both cases right:
///
/// - `child_top >= cut` (the ordinary split, between or inside
///   children): the child was pushed whole from below the cut, so the
///   gap collapses and it lands flush at the strip top. This is the
///   long-standing behaviour, `origin = child_top`.
/// - `child_top < cut` (the container's own `border-top + padding-top`
///   straddled the boundary): the leading decoration is only partly
///   spent, and the remainder is still owed on the continuation. The
///   child is inset by exactly what is left, `child_top - cut`, instead
///   of being slammed against the strip top and silently swallowing it.
///
/// Without the second case a container cut inside its own decoration
/// loses the unspent part from every fragment, so its border box does
/// not add up across pages.
fn resume_taffy_origin(
    page_taffy_origin: f32,
    page_start_y: f32,
    page_height_px: f32,
    child_top: f32,
) -> f32 {
    let cut = page_taffy_origin + (page_height_px - page_start_y).max(0.0);
    child_top.min(cut)
}

/// A box's own block-axis decoration, in CSS px:
/// `(border-top + padding-top, padding-bottom + border-bottom)`.
///
/// This is the part of a border box that belongs to the box itself
/// rather than to any child, and the part that
/// `box-decoration-break: slice` (CSS Fragmentation 3 §5.4, the initial
/// value) assigns to the FIRST and LAST fragment respectively. Both
/// come from Taffy rather than from any content measurement — see
/// [`inline_root_box_metrics`] for why that distinction bites on the
/// inline path.
///
/// Shared by the inline-root splitter and by
/// [`fragment_block_subtree`]'s tail, so the two paths cannot drift on
/// what a box's decoration is. Non-finite Taffy values are sanitized to
/// `0.0`, matching the child-height sanitization in
/// `fragment_block_subtree`.
fn box_decoration(node: &blitz_dom::Node) -> (f32, f32) {
    fn finite(v: f32) -> f32 {
        if v.is_finite() { v.max(0.0) } else { 0.0 }
    }
    let layout = &node.final_layout;
    (
        finite(layout.border.top) + finite(layout.padding.top),
        finite(layout.padding.bottom) + finite(layout.border.bottom),
    )
}

/// fulgur-p55h: split a multi-line inline root across page boundaries
/// at line edges, append one Fragment per page span to the geometry
/// table, and return the updated `(page_index, cursor_y, fragments_emitted)`.
///
/// Walks lines, tracks the first line of the current fragment in
/// `fragment_start_idx`, and splits when the cumulative height in
/// paragraph-local coords would push the bottom past
/// `page_height_px - paragraph_top_in_body`.
///
/// fulgur-s67g Phase 2.1 (widow / orphan): each candidate split point
/// must leave the **first** fragment with `>= ORPHANS_MIN` lines and
/// the **remainder** of the paragraph with `>= WIDOWS_MIN` lines
/// (CSS Fragmentation §4.4). When neither holds at the natural
/// overflow point, the split is skipped — subsequent lines accumulate
/// into the current fragment (overflow-tolerant) until a valid split
/// is found or the paragraph ends, in which case the paragraph emits
/// whole (oversized or pushed to a fresh page by sibling-driven
/// flow).
///
/// CSS `orphans` / `widows` properties are not parsed today, so the
/// fragmenter uses the CSS 3 Fragmentation defaults (`2` for both).
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
///
/// fulgur-pgbrk R1: `lead_in` / `lead_out` are the box's own decoration
/// (see [`inline_root_box_metrics`]). Emitted fragments describe the
/// **border box**, so `lead_in` is added to the first fragment and
/// `lead_out` to the last, per `box-decoration-break: slice`
/// (CSS Fragmentation 3 §5.4). Both are recorded on the geometry entry
/// so line-partitioning consumers can subtract them back out.
/// fulgur-pgbrk Risk 1: the split inputs travel as one
/// [`InlineSplitInput`] and the placement as one [`InlinePlacement`],
/// so the signature no longer needs a clippy arity exemption.
fn fragment_inline_root(
    geometry: &mut PaginationGeometryTable,
    page_height_px: f32,
    placement: InlinePlacement,
    input: &InlineSplitInput<'_>,
) -> (u32, f32, usize) {
    if input.line_metrics.is_empty() {
        return (placement.page, placement.cursor_y, 0);
    }

    // Pass 1: honour the orphans / widows minimums.
    let plan = scan_split_points(input, placement.cursor_y, placement.page, page_height_px);

    // css-break-3 §4.4 relaxation: "If that still does not lead to
    // sufficient break points ... the UA may break anywhere in order to
    // avoid losing content off the edge." A constrained scan that could
    // not find a legal split keeps accumulating lines into one fragment,
    // which then hangs past the fragmentainer — into the bottom margin
    // and off the paper, where the glyphs are discarded. Re-scan with
    // the restrictions dropped to 1/1 (break between any two line
    // boxes) rather than lose the content.
    //
    // Relaxing to 1 rather than 0 keeps the "a fragment holds at least
    // one line" invariant, so no empty fragment can be emitted.
    let plan = if plan
        .iter()
        .any(|f| f.y + f.height > page_height_px + OVERFLOW_EPS_PX)
    {
        let relaxed = InlineSplitInput {
            line_metrics: input.line_metrics,
            lead_in: input.lead_in,
            lead_out: input.lead_out,
            orphans: 1,
            widows: 1,
        };
        scan_split_points(&relaxed, placement.cursor_y, placement.page, page_height_px)
    } else {
        plan
    };

    let emitted = plan.len();
    let last = *plan.last().expect("scan always emits a final fragment");
    let entry = geometry.entry(placement.id).or_default();
    for f in &plan {
        entry.fragments.push(Fragment {
            page_index: f.page_index,
            x: placement.x.as_px(),
            y: f.y.as_px(),
            width: placement.width.as_px(),
            height: f.height.as_px(),
        });
    }
    entry.content_lead_in = input.lead_in.as_px();
    entry.content_lead_out = input.lead_out.as_px();

    (last.page_index, last.y + last.height, emitted)
}

/// One planned fragment of an inline root, in paragraph-local space.
#[derive(Debug, Clone, Copy, PartialEq)]
struct InlineFragmentPlan {
    page_index: u32,
    /// Top of the fragment on its own page.
    y: f32,
    /// Border-box height of this slice (see [`fragment_inline_root`]).
    height: f32,
}

/// Walk `line_metrics` and decide where the inline root splits, without
/// touching the geometry table (fulgur-pgbrk R2).
///
/// Splitting a candidate point is legal only when it leaves at least
/// `orphans` lines in the fragment being closed and at least `widows`
/// lines in the remainder (css-break-3 §4.4 rule 3). When neither holds,
/// the split is skipped and lines keep accumulating into the current
/// fragment — which is why a constrained scan can return a plan that
/// overflows the fragmentainer, and why [`fragment_inline_root`] re-runs
/// it with the constraints relaxed when that happens.
///
/// Returning a plan rather than pushing fragments directly is what makes
/// the second pass possible at all: the first pass must be discardable.
fn scan_split_points(
    input: &InlineSplitInput<'_>,
    initial_cursor_y: f32,
    initial_page_index: u32,
    page_height_px: f32,
) -> Vec<InlineFragmentPlan> {
    let line_metrics = input.line_metrics;
    let lead_in = input.lead_in;
    let lead_out = input.lead_out;
    let orphans = input.orphans;
    let widows = input.widows;
    let mut plan = Vec::new();
    if line_metrics.is_empty() {
        return plan;
    }

    let mut page_index = initial_page_index;
    let mut paragraph_top_in_body = initial_cursor_y;
    let mut fragment_start_idx: usize = 0;
    let total_lines = line_metrics.len();

    for (i, &(_line_top_local, line_bottom_local)) in line_metrics.iter().enumerate() {
        let frag_top_local = line_metrics[fragment_start_idx].0;
        // Only the first fragment carries the leading decoration
        // (`box-decoration-break: slice`), so only it starts its lines
        // `lead_in` below its own top edge.
        let frag_lead_in = if fragment_start_idx == 0 {
            lead_in
        } else {
            0.0
        };
        // The trailing decoration (`padding-bottom` / `border-bottom`)
        // is added unconditionally to whichever fragment ends up last
        // (see the `frag_lead_out` push after this loop). If line `i`
        // is the paragraph's last line, closing the fragment here makes
        // it that final fragment, so the fit check must include
        // `lead_out` too — otherwise a fragment whose lines fit exactly
        // but whose trailing decoration doesn't can sail past
        // `page_height_px` unnoticed, and the relaxed re-scan below
        // hits the identical blind spot (fulgur-pgbrk R7 follow-up,
        // Codex review PR #719).
        let frag_lead_out = if i == total_lines - 1 { lead_out } else { 0.0 };
        let projected_bottom_in_body = paragraph_top_in_body
            + frag_lead_in
            + (line_bottom_local - frag_top_local)
            + frag_lead_out;

        if projected_bottom_in_body > page_height_px && i > fragment_start_idx {
            let first_size = i - fragment_start_idx;
            let remaining_size = total_lines - i;

            // `i` is the natural split: the last line that fits. It may
            // not be a legal one.
            let split_at = if remaining_size < widows {
                // The tail would be short of `widows`, and splitting
                // LATER only makes the tail shorter — so back up to the
                // latest split that leaves exactly `widows` lines. This
                // is the only direction that can satisfy widows, and
                // lines `[start, j)` are a subset of `[start, i)`, which
                // already fits, so the earlier fragment fits too.
                let j = total_lines.saturating_sub(widows);
                if j > fragment_start_idx && j - fragment_start_idx >= orphans {
                    j
                } else {
                    // Backing up far enough would starve `orphans`.
                    // Both minimums cannot hold at once; keep
                    // accumulating and let the caller relax and re-scan.
                    continue;
                }
            } else if first_size < orphans {
                // Too few lines to leave behind. Splitting earlier makes
                // that worse and splitting later overflows, so there is
                // no legal point here.
                continue;
            } else {
                i
            };

            // Lines [fragment_start_idx, split_at) fit on the current page.
            let prev_line_bottom = line_metrics[split_at - 1].1;
            plan.push(InlineFragmentPlan {
                page_index,
                y: paragraph_top_in_body,
                height: frag_lead_in + (prev_line_bottom - frag_top_local),
            });

            page_index += 1;
            paragraph_top_in_body = 0.0;
            fragment_start_idx = split_at;
        }
    }

    // Final fragment covers lines [fragment_start_idx, end), plus the
    // box's trailing decoration (`box-decoration-break: slice`).
    let frag_top_local = line_metrics[fragment_start_idx].0;
    let last_bottom_local = line_metrics.last().expect("non-empty checked above").1;
    let frag_lead_in = if fragment_start_idx == 0 {
        lead_in
    } else {
        0.0
    };
    plan.push(InlineFragmentPlan {
        page_index,
        y: paragraph_top_in_body,
        height: frag_lead_in + (last_bottom_local - frag_top_local) + lead_out,
    });

    plan
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

        let children: Vec<usize> = layout_children_of(doc, node_id);
        for child_id in children {
            // Shared skip: dangling id / out-of-flow (handled by their
            // own passes) / whitespace-only text — see
            // `is_walkable_skip`.
            if is_walkable_skip(doc, child_id) {
                continue;
            }
            let Some(child) = doc.get_node(child_id) else {
                continue;
            };
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

    for child_id in layout_children_of(doc, fixed_root_id) {
        if is_walkable_skip(doc, child_id) {
            continue;
        }
        let Some(child) = doc.get_node(child_id) else {
            continue;
        };
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
            running_store,
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
    running_store: Option<&crate::gcpm::running::RunningElementStore>,
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
        running_store: Option<&crate::gcpm::running::RunningElementStore>,
    ) {
        if depth >= crate::MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };
        // fulgur-yb27: anonymous block wrappers live only in
        // `layout_children` — the shared enumeration policy is
        // `layout_children_of`. (The skip filter below intentionally
        // does NOT use `is_walkable_skip`: nested absolutes are
        // recursed into here, only `fixed` is skipped.)
        let children: Vec<usize> = layout_children_of(doc, node_id);
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
            // fulgur: `position: running(name)` children are rewritten to
            // `display: none` by `gcpm::parser::parse_gcpm`'s cleaned_css
            // (they are removed from normal flow and repainted only via
            // their `@page` margin box). A `display: none` node collapses
            // to a zero-size Taffy box, so the generic below (height-gated
            // on `h_for_paging > 0.0`) never records a geometry entry for
            // it — unlike `fragment_pagination_root`'s in-flow body walk,
            // which has the equivalent carve-out at the cursor level. Without
            // this check here, a running element nested inside a
            // `position: absolute` / body-direct-abs subtree never lands in
            // `geometry`, so `collect_running_element_states` can't find its
            // NodeId and the margin box it should populate renders empty
            // (fulgur `--css` + hidden-ancestor running-element bug).
            // Mirror the main walk: record a zero-size marker at the
            // running child's current subtree position and skip recursing
            // into it (it has no flow content of its own to lay out).
            if running_store.is_some_and(|s| s.instance_for_node(child_id).is_some()) {
                if child.element_data().is_some() && page_h_px > 0.0 {
                    let child_y_for_paging =
                        root_xy_for_paging.1 + offset_in_subtree.1 + child.final_layout.location.y;
                    let page_index = (child_y_for_paging / page_h_px).max(0.0).floor() as u32;
                    let page_index = page_index.min(total_pages.saturating_sub(1));
                    let stored_x =
                        root_xy_for_paging.0 + offset_in_subtree.0 + child.final_layout.location.x
                            - body_offset.0;
                    let stored_y =
                        child_y_for_paging - (page_index as f32) * page_h_px - body_offset.1;
                    geometry
                        .entry(child_id)
                        .or_default()
                        .fragments
                        .push(Fragment {
                            page_index,
                            x: stored_x.as_px(),
                            y: stored_y.as_px(),
                            width: 0.0_f32.as_px(),
                            height: 0.0_f32.as_px(),
                        });
                }
                continue;
            }
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
                            running_store,
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
                running_store,
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
        running_store,
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
        let cx = FragmentationCtx {
            doc: &doc, // &BaseDocument via deref coercion
            styles: None,
            used_page_names: None,
            running: None,
            page_h: 800.0,
        };
        let mut frame = ContainerFrame::child(
            parent_id,
            0.0,                  // parent_x_in_body
            600.0,                // parent_w
            0,                    // page_in
            0.0,                  // cursor_in
            true,                 // allow_leading_break
            crate::MAX_DOM_DEPTH, // depth → trips the guard immediately
        );
        let result = fragment_block_subtree(&cx, &mut frame, &mut geom);
        let SubtreeResult::Placed {
            page: page_out,
            cursor_y: cursor_out,
        } = result
        else {
            panic!("the depth bail always places, never requests a break")
        };

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

    /// Codex review (PR #719): a nested child at the top of a fresh
    /// strip (`child_page_y == 0`) whose height exceeds the strip by
    /// the documented 220pt→294px Taffy-rounding delta (~0.67px) sits
    /// inside `oversized`'s 1px tolerance and must NOT be sliced — even
    /// though it was previously only 0.5px (`OVERFLOW_EPS_PX`) inside
    /// `spills_strip`'s tolerance, which would wrongly trigger slicing
    /// and produce a spurious sliver page for an exact-fit box.
    #[test]
    fn needs_leaf_slicing_respects_oversize_tolerance_at_a_fresh_strip_top() {
        let page_height_px = 293.33334_f32;
        let child_h = 294.0_f32; // ~0.67px over — the documented rounding case
        assert!(
            !super::needs_leaf_slicing(child_h, 0.0, page_height_px),
            "a box inside the 1px Taffy-rounding tolerance must not be sliced, \
             even sitting at a fresh strip's top edge"
        );
    }

    /// The same tolerance must still catch a genuinely oversized child
    /// (e.g. `300vh` on a 293px strip) at a fresh strip's top.
    #[test]
    fn needs_leaf_slicing_still_catches_true_oversize() {
        let page_height_px = 293.33334_f32;
        let child_h = 880.0_f32;
        assert!(super::needs_leaf_slicing(child_h, 0.0, page_height_px));
    }

    /// A child that fits its own height but crosses the strip boundary
    /// because it sits mid-strip (`child_page_y > 0`, the R7b flex/grid
    /// case) must still be sliced.
    #[test]
    fn needs_leaf_slicing_catches_mid_strip_spill() {
        let page_height_px = 800.0_f32;
        assert!(super::needs_leaf_slicing(400.0, 500.0, page_height_px));
    }

    /// fulgur-pgbrk R7: `slice_oversized_leaf` slice arithmetic — an
    /// exact multiple of the strip height must emit one fragment per
    /// page with NO trailing zero-height sliver, and the returned
    /// cursor sits at the filled strip's bottom edge.
    #[test]
    fn slice_oversized_leaf_exact_fit_no_sliver() {
        let html = r#"<html><body><div id="p"></div></body></html>"#;
        let doc = parse(html, 600.0);
        let probe = find_by_id(&doc, "p").expect("div#p");
        let mut geom = PaginationGeometryTable::new();
        // 1600px on an 800px strip from cursor 0: two full slices.
        let (page, cursor) = super::slice_oversized_leaf(
            &mut geom, &doc, probe, 0.0, 600.0, 1600.0, 0, 0.0, 800.0, 0,
        );
        let frags = &geom.get(&probe).expect("probe").fragments;
        assert_eq!(frags.len(), 2, "exact fit slices, no sliver: {frags:?}");
        assert_eq!(
            frags.iter().map(|f| f.page_index).collect::<Vec<_>>(),
            vec![0, 1]
        );
        for f in frags {
            assert!(
                (f.height.to_f32() - 800.0).abs() < 0.01,
                "full strip: {f:?}"
            );
        }
        assert_eq!(page, 1);
        assert!(
            (cursor - 800.0).abs() < 0.01,
            "resume after a full strip, not past it: {cursor}"
        );
    }

    /// fulgur-pgbrk R7: `slice_oversized_leaf` one-past-boundary — a
    /// box one px taller than the strip emits a second one-px slice on
    /// the next page, and following content resumes after that sliver.
    #[test]
    fn slice_oversized_leaf_one_past_boundary() {
        let html = r#"<html><body><div id="p"></div></body></html>"#;
        let doc = parse(html, 600.0);
        let probe = find_by_id(&doc, "p").expect("div#p");
        let mut geom = PaginationGeometryTable::new();
        let (page, cursor) = super::slice_oversized_leaf(
            &mut geom, &doc, probe, 0.0, 600.0, 801.0, 0, 0.0, 800.0, 0,
        );
        let frags = &geom.get(&probe).expect("probe").fragments;
        assert_eq!(frags.len(), 2, "sliver slice: {frags:?}");
        assert!(
            (frags[1].height.to_f32() - 1.0).abs() < 0.01,
            "second slice is the 1px remainder: {frags:?}"
        );
        assert_eq!(page, 1);
        assert!(
            (cursor - 1.0).abs() < 0.01,
            "resume after the sliver: {cursor}"
        );
    }

    /// fulgur-2m6w: `slice_oversized_leaf` caps the per-strip slicing at
    /// `MAX_PAGES` even when the box is taller — content past the cap is
    /// truncated (a content-BEARING box, so the childless collapse below
    /// does not fire).
    #[test]
    fn slice_oversized_leaf_max_pages_cap() {
        let html = r#"<html><body><div id="p"><div id="c">content</div></div></body></html>"#;
        let doc = parse(html, 600.0);
        let probe = find_by_id(&doc, "p").expect("div#p");
        let mut geom = PaginationGeometryTable::new();
        // Clearly past `MAX_PAGES` strips even after the first slice —
        // mirrors the `height: 99999999px` fixtures used by the ezst
        // integration tests (a bare `MAX_PAGES + 1` strips would, after
        // slice 1, leave exactly `MAX_PAGES` strips of remainder and
        // the childless collapse's `ceil > MAX_PAGES` gate would not
        // fire).
        let huge = 9_999_999.0_f32;
        let (page, _) =
            super::slice_oversized_leaf(&mut geom, &doc, probe, 0.0, 600.0, huge, 0, 0.0, 800.0, 0);
        let frags = &geom.get(&probe).expect("probe").fragments;
        assert_eq!(page, crate::MAX_PAGES, "capped at MAX_PAGES");
        assert_eq!(
            frags.len() as u32,
            crate::MAX_PAGES + 1,
            "first slice plus MAX_PAGES loop slices, then truncated: {} fragments",
            frags.len()
        );
    }

    /// fulgur-ezst: `slice_oversized_leaf` collapses a CHILDLESS box
    /// whose slicing would exceed `MAX_PAGES` to its single first slice
    /// and resumes following content on the next page (cursor 0).
    #[test]
    fn slice_oversized_leaf_childless_collapse() {
        let html = r#"<html><body><div id="p"></div></body></html>"#;
        let doc = parse(html, 600.0);
        let probe = find_by_id(&doc, "p").expect("div#p");
        let mut geom = PaginationGeometryTable::new();
        let huge = 9_999_999.0_f32;
        let (page, cursor) =
            super::slice_oversized_leaf(&mut geom, &doc, probe, 0.0, 600.0, huge, 0, 0.0, 800.0, 0);
        let frags = &geom.get(&probe).expect("probe").fragments;
        assert_eq!(frags.len(), 1, "collapsed to the first slice: {frags:?}");
        assert_eq!(page, 1, "only one page consumed");
        assert!((frags[0].height.to_f32() - 800.0).abs() < 0.01);
        assert!((cursor).abs() < 0.01, "following content starts at y=0");
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

    /// Regression for the margin-collapse-after-fragmentation-break bug
    /// (see `FULGUR_MARGIN_COLLAPSE_BUG.md` at the repo root): the
    /// sibling immediately following a child whose *own* recursion
    /// crossed a page boundary lost its collapsed margin against that
    /// child entirely, landing flush against its tail instead.
    ///
    /// Shape: `section` (recursed into from body, since its content
    /// spans two pages) contains `a` (250px, fills most of page 1),
    /// `c` (a div with a single 100px child `c_inner`, `margin: 20px 0`
    /// — too tall for the 30px left on page 1, so `c` itself is
    /// recursed into and its content lands wholly on page 2), and `d`
    /// (50px, `margin-top: 20px`).
    ///
    /// `c`'s and `d`'s margins collapse to a single 20px gap (CSS 2.1
    /// §8.3.1) — neither margin is at a fragmentation break (`c`'s own
    /// leading margin was already truncated when `c_inner` was placed
    /// at page 2's top; the break is not between `c` and `d`). `d`
    /// must land at `c_inner`'s bottom (100px, page-local) + the 20px
    /// gap = 120px. Pre-fix, the deferred origin rebase anchored on
    /// `d`'s own Taffy-space top instead of `c`'s, forcing `d` flush
    /// against `c_inner` at y=100 — losing the margin.
    #[test]
    fn recursed_child_crossing_page_preserves_next_sibling_collapsed_margin() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <section style="width: 200px;">
                <div id="a" style="height: 250px; width: 200px"></div>
                <div id="c" style="width: 200px; margin-top: 20px; margin-bottom: 20px">
                  <div id="c_inner" style="height: 100px; width: 200px"></div>
                </div>
                <div id="d" style="height: 50px; width: 200px; margin-top: 20px"></div>
              </section>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 300.0);
        let d_id = find_by_id(doc.deref_mut(), "d").expect("div#d");
        let d_frag = table
            .get(&d_id)
            .expect("div#d must appear in geometry")
            .fragments
            .first()
            .expect("div#d must have a fragment");
        assert_eq!(
            d_frag.page_index, 1,
            "div#d should land on page 1 (0-indexed), following c's page-2 tail",
        );
        let d_y = d_frag.y.to_f32();
        assert!(
            (d_y - 120.0).abs() < 0.5,
            "div#d must sit 20px below c_inner's 100px-tall page-2 fragment \
             (y=120), preserving the collapsed c/d margin; got y={d_y} \
             (pre-fix: y=100, flush against c_inner with the margin dropped)",
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

    /// fulgur-i5a + fulgur-pgbrk R7: `overflow: hidden` boxes are
    /// monolithic — the clip is applied at draw time (`render.rs`
    /// push/pop_clip_path), so pagination treats an overflowing child
    /// as unsplittable. A monolithic child taller than the
    /// fragmentainer is nonetheless sliced per strip, uniform with the
    /// body-direct path (fulgur-sbw2) and the nested walk
    /// (fulgur-pgbrk R7): one fragment per page, each inside its strip.
    #[test]
    fn overflow_hidden_oversize_child_is_sliced_per_strip() {
        let html = r#"
            <html><body>
              <div id="outer" style="height: 50px; overflow: hidden">
                <div id="inner" style="height: 1200px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let outer = find_by_id(&doc, "outer").expect("div#outer");
        let inner = find_by_id(&doc, "inner").expect("div#inner");
        let table = run_pass(&mut doc, 800.0);

        // The 1200px monolithic child on an 800px strip: two slices
        // (800 + 400) on consecutive pages instead of one overflowing
        // fragment on page 0.
        let inner_frags = &table.get(&inner).expect("inner in geometry").fragments;
        assert_eq!(
            inner_frags.len(),
            2,
            "monolithic oversize child is sliced per strip; frags={inner_frags:?}"
        );
        let pages: Vec<u32> = inner_frags.iter().map(|f| f.page_index).collect();
        assert_eq!(
            pages,
            vec![0, 1],
            "consecutive pages; frags={inner_frags:?}"
        );
        let total: f32 = inner_frags.iter().map(|f| f.height.to_f32()).sum();
        assert!(
            (total - 1200.0).abs() <= 0.5,
            "the slices reconstruct the box height exactly; frags={inner_frags:?}"
        );
        for f in inner_frags {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 800.5,
                "no slice may extend past the strip; frags={inner_frags:?}"
            );
        }
        // The clipped parent still participates in geometry on every
        // page its child crossed (fulgur-oc51).
        let outer_frags = &table.get(&outer).expect("outer in geometry").fragments;
        assert!(
            outer_frags.iter().any(|f| f.page_index == 0),
            "the overflow:hidden parent keeps a page-0 fragment; frags={outer_frags:?}"
        );
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

    /// Regression for the `--css` + hidden-ancestor running-element bug
    /// (FULGUR_CSS_FLAG_RUNNING_ELEMENT_BUG.md): `gcpm::parser::parse_gcpm`
    /// rewrites `position: running(name)` to `display: none` in its
    /// `cleaned_css` output (the real DOM copy must not also paint in
    /// normal flow — only its `@page` margin-box copy should). When the
    /// running element is nested inside a `position: absolute` ancestor
    /// (the common "absolute + invisible wrapper" header/footer idiom),
    /// its now-zero-size Taffy box used to fall through
    /// `record_subtree_fragments_at_offset`'s height gate
    /// (`h_for_paging > 0.0`) with no running-element carve-out — unlike
    /// `fragment_pagination_root`'s in-flow body walk, which records a
    /// zero-height marker for running children unconditionally. The
    /// element's NodeId never reached `geometry`, so
    /// `collect_running_element_states` silently produced nothing and the
    /// margin box rendered empty.
    #[test]
    fn running_element_nested_in_absolute_subtree_lands_in_geometry_when_display_none() {
        use crate::blitz_adapter;
        use crate::gcpm::parser::parse_gcpm;
        use std::ops::DerefMut;
        use std::sync::Arc;

        let css = r#"
            @page { @top-center { content: element(top-center); } }
            .absolute { position: absolute; }
            .invisible { visibility: hidden; }
            #top-center { position: running(top-center); }
        "#;
        let gcpm = parse_gcpm(css);
        assert!(
            gcpm.cleaned_css.contains("display: none"),
            "sanity: parse_gcpm must rewrite position:running to display:none \
             in cleaned_css; got {:?}",
            gcpm.cleaned_css
        );

        let html = r#"<!DOCTYPE html>
<html><body>
  <div class="absolute invisible">
    <div id="top-center">HEADER</div>
  </div>
  <p>Body content.</p>
</body></html>"#;

        let fonts: Vec<Arc<Vec<u8>>> = Vec::new();
        let mut doc = blitz_adapter::parse(html, 600.0, &fonts);
        let pass_ctx = blitz_adapter::PassContext { font_data: &fonts };

        // Mirrors `Engine::layout_to_drawables`: cleaned_css (the
        // display:none rewrite) is injected via `InjectCssPass` — this is
        // exactly what happens for AssetBundle / `--css`-sourced CSS.
        let inject = blitz_adapter::InjectCssPass {
            css: gcpm.cleaned_css.clone(),
        };
        blitz_adapter::apply_single_pass(&inject, &mut doc, &pass_ctx);

        let running_pass = blitz_adapter::RunningElementPass::new(gcpm.running_mappings.clone());
        blitz_adapter::apply_single_pass(&running_pass, &mut doc, &pass_ctx);
        let store = running_pass.into_running_store();

        blitz_adapter::resolve(&mut doc);

        let mut geometry = PaginationGeometryTable::new();
        append_position_absolute_body_direct_fragments(
            &mut geometry,
            doc.deref_mut(),
            1,
            600.0,
            800.0,
            Some(&store),
        );

        let found = geometry
            .keys()
            .any(|&node_id| store.instance_for_node(node_id).is_some());
        assert!(
            found,
            "running element nested in a position:absolute subtree must land \
             in geometry even when display:none collapses its layout box; \
             geometry keys={:?}",
            geometry.keys().collect::<Vec<_>>()
        );

        let states = collect_running_element_states(&geometry, &store);
        let entry = states[0]
            .get("top-center")
            .expect("top-center running instance must be recorded for page 0");
        assert_eq!(entry.instance_ids.len(), 1);
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
        // "Always at least one page" convention: even an empty
        // geometry yields a single empty per-page state so downstream
        // consumers can index by page without special-casing zero.
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
        // emit exactly one fragment per fixed element (the "always
        // at least one page" convention applied to fixed
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
            None,
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

    /// fulgur-pgbrk R6: `orphans` / `widows` are inherited, but the
    /// column-style side-table records only elements the author wrote a
    /// declaration on, so the value is resolved by walking up.
    #[test]
    fn resolved_line_constraints_inherits_from_an_ancestor() {
        let html = r#"
            <html><body style="widows:4; orphans:3">
              <div><p id="probe">x</p></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let (orphans, widows) =
            super::resolved_line_constraints(doc.deref_mut(), probe, Some(&table));
        assert_eq!((orphans, widows), (3, 4));
    }

    #[test]
    fn resolved_line_constraints_prefers_the_nearest_declaration() {
        let html = r#"
            <html><body style="widows:4">
              <div style="widows:6"><p id="probe" style="orphans:5">x</p></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let (orphans, widows) =
            super::resolved_line_constraints(doc.deref_mut(), probe, Some(&table));
        assert_eq!(orphans, 5, "declared on the element itself");
        assert_eq!(widows, 6, "nearest ancestor wins over <body>");
    }

    #[test]
    fn resolved_line_constraints_falls_back_to_the_css_initial_values() {
        let html = r#"<html><body><p id="probe">x</p></body></html>"#;
        let mut doc = parse(html, 600.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
        let table = blitz_adapter::extract_column_style_table(&doc);
        assert_eq!(
            super::resolved_line_constraints(doc.deref_mut(), probe, Some(&table)),
            (2, 2)
        );
        assert_eq!(
            super::resolved_line_constraints(doc.deref_mut(), probe, None),
            (2, 2),
            "no table at all still yields the initial values"
        );
    }

    /// Codex review (PR #719): four 100px lines on a 250px strip with a
    /// 100px `lead_out` (`padding-bottom` / `border-bottom`). The lines
    /// alone fit the last fragment (200px), but the scan's overflow
    /// predicate only compared line bottoms — never `lead_out` — so it
    /// never split before the trailing line, and the unconditional
    /// `+ lead_out` push after the loop produced a 300px final fragment
    /// on a 250px strip: exactly what `find_overflowing_fragments`
    /// panics on in test builds. The fix folds `lead_out` into the fit
    /// check for the paragraph's last line, so the split lands one line
    /// earlier and every fragment (200 / 100 / 200) fits the strip.
    #[test]
    fn scan_accounts_for_lead_out_on_the_final_fragment() {
        let lines: Vec<(f32, f32)> = (0..4)
            .map(|i| (i as f32 * 100.0, (i + 1) as f32 * 100.0))
            .collect();
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 0.0,
            lead_out: 100.0,
            orphans: 1,
            widows: 1,
        };
        let plan = super::scan_split_points(&input, 0.0, 0, 250.0);
        for f in &plan {
            assert!(
                f.height <= 250.0 + 0.01,
                "fragment overflows the strip once lead_out is counted; plan={plan:?}"
            );
        }
        let last = plan.last().expect("scan always emits a final fragment");
        assert!(
            (last.height - 200.0).abs() < 0.01,
            "final fragment should be 1 line (100) + lead_out (100); plan={plan:?}"
        );
    }

    /// fulgur-pgbrk R6: the widow minimum is the one constraint that can
    /// only be satisfied by splitting EARLIER than the natural overflow
    /// point, so `scan_split_points` has to back up rather than skip.
    #[test]
    fn scan_backs_the_split_up_to_honour_a_large_widows_value() {
        // Six 100px lines, 450px strip. The natural split is after line
        // 4 (bottom 500 > 450), which leaves a 2-line tail. widows=4
        // forces the split back to line 2, leaving 4 lines in the tail.
        let lines: Vec<(f32, f32)> = (0..6)
            .map(|i| (i as f32 * 100.0, (i + 1) as f32 * 100.0))
            .collect();
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 0.0,
            lead_out: 0.0,
            orphans: 2,
            widows: 4,
        };
        let plan = super::scan_split_points(&input, 0.0, 0, 450.0);
        assert_eq!(plan.len(), 2, "plan={plan:?}");
        assert!(
            (plan[0].height - 200.0).abs() < 0.01,
            "head keeps 2 lines; plan={plan:?}"
        );
        assert!(
            (plan[1].height - 400.0).abs() < 0.01,
            "tail keeps the 4 lines widows demands; plan={plan:?}"
        );
        for f in &plan {
            assert!(f.y + f.height <= 450.5, "plan={plan:?}");
        }
    }

    #[test]
    fn scan_will_not_back_up_past_the_orphan_minimum() {
        // Same six lines, but orphans=3 and widows=5 cannot both hold:
        // leaving 5 in the tail leaves only 1 in the head. The scan
        // refuses the split rather than violating orphans, so the plan
        // overflows — which is exactly the signal `fragment_inline_root`
        // uses to relax and re-scan.
        let lines: Vec<(f32, f32)> = (0..6)
            .map(|i| (i as f32 * 100.0, (i + 1) as f32 * 100.0))
            .collect();
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 0.0,
            lead_out: 0.0,
            orphans: 3,
            widows: 5,
        };
        let plan = super::scan_split_points(&input, 0.0, 0, 450.0);
        assert_eq!(plan.len(), 1, "no legal split; plan={plan:?}");
        assert!(
            plan[0].height > 450.0,
            "the unsplit plan overflows, triggering relaxation; plan={plan:?}"
        );
    }

    /// fulgur-pgbrk R2: a 3-line paragraph whose only natural split
    /// leaves a 1-line tail violates widows = 2, so the constrained scan
    /// finds no legal split and returns a 225px plan on a 200px strip.
    ///
    /// css-break-3 §4.4's closing clause requires the restriction to be
    /// dropped rather than letting the tail escape the fragmentainer, so
    /// the relaxed re-scan splits 2/1 anyway.
    ///
    /// Until fulgur-pgbrk R2 this test asserted the opposite — one
    /// oversized fragment — and so pinned the content-loss defect.
    #[test]
    fn widow_minimum_is_relaxed_rather_than_losing_the_tail_line() {
        let mut geom = PaginationGeometryTable::new();
        // Each line 75px; cumulative bottoms at 75, 150, 225.
        // Page strip = 200, so the natural split is at line 2 (bottom
        // 225 > 200), leaving 1 line in the tail — widows violated.
        let lines = vec![(0.0, 75.0), (75.0, 150.0), (150.0, 225.0)];
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 0.0,
            lead_out: 0.0,
            orphans: 2,
            widows: 2,
        };
        let placement = InlinePlacement {
            id: 1,
            x: 0.0,
            width: 100.0,
            cursor_y: 0.0,
            page: 0,
        };
        let (new_page, new_cursor, emitted) =
            super::fragment_inline_root(&mut geom, 200.0, placement, &input);
        assert_eq!(emitted, 2, "relaxation splits 2/1 rather than overflowing");
        assert_eq!(new_page, 1);
        assert!(
            (new_cursor - 75.0).abs() < 0.01,
            "cursor is the 1-line tail on page 1, got {new_cursor}",
        );
        let frags = &geom.get(&1).unwrap().fragments;
        assert_eq!(frags.len(), 2);
        assert_eq!(frags[0].page_index, 0);
        assert!((frags[0].height.to_f32() - 150.0).abs() < 0.01);
        assert_eq!(frags[1].page_index, 1);
        assert!((frags[1].height.to_f32() - 75.0).abs() < 0.01);
        for f in frags {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 200.5,
                "no fragment escapes the strip; frags={frags:?}"
            );
        }
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
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 0.0,
            lead_out: 0.0,
            orphans: 2,
            widows: 2,
        };
        let placement = InlinePlacement {
            id: 1,
            x: 0.0,
            width: 100.0,
            cursor_y: 0.0,
            page: 0,
        };
        let (new_page, new_cursor, emitted) =
            super::fragment_inline_root(&mut geom, 200.0, placement, &input);
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

    /// fulgur-pgbrk R2: orphan violation. A 3-line paragraph on a strip
    /// that fits only one line would put 1 line in the head fragment,
    /// below orphans = 2, and no later split point satisfies both
    /// minimums either — so the constrained scan returns a 225px plan on
    /// a 100px strip.
    ///
    /// Relaxation (css-break-3 §4.4) then slices it one line per page.
    ///
    /// Until fulgur-pgbrk R2 this test asserted the opposite — one
    /// oversized fragment — and so pinned the content-loss defect.
    #[test]
    fn orphan_minimum_is_relaxed_rather_than_losing_the_tail_lines() {
        let mut geom = PaginationGeometryTable::new();
        // Lines 75px; bottoms at 75, 150, 225.
        // Page strip = 100 → natural split at line 1 (bottom 150 > 100),
        // where first_size = 1 < orphans = 2.
        let lines = vec![(0.0, 75.0), (75.0, 150.0), (150.0, 225.0)];
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 0.0,
            lead_out: 0.0,
            orphans: 2,
            widows: 2,
        };
        let placement = InlinePlacement {
            id: 1,
            x: 0.0,
            width: 100.0,
            cursor_y: 0.0,
            page: 0,
        };
        let (new_page, _new_cursor, emitted) =
            super::fragment_inline_root(&mut geom, 100.0, placement, &input);
        assert_eq!(emitted, 3, "relaxation slices one line per page");
        assert_eq!(new_page, 2);
        let frags = &geom.get(&1).unwrap().fragments;
        assert_eq!(frags.len(), 3);
        let pages: Vec<u32> = frags.iter().map(|f| f.page_index).collect();
        assert_eq!(pages, vec![0, 1, 2]);
        for f in frags {
            assert!((f.height.to_f32() - 75.0).abs() < 0.01, "frags={frags:?}");
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 100.5,
                "no fragment escapes the strip; frags={frags:?}"
            );
        }
    }

    /// fulgur-pgbrk R1: an unsplit inline root's single fragment covers
    /// the whole border box — both decoration edges included.
    #[test]
    fn inline_root_single_fragment_carries_both_decoration_edges() {
        let mut geom = PaginationGeometryTable::new();
        let lines = vec![(0.0, 75.0), (75.0, 150.0)];
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 20.0,
            lead_out: 10.0,
            orphans: 2,
            widows: 2,
        };
        let placement = InlinePlacement {
            id: 1,
            x: 0.0,
            width: 100.0,
            cursor_y: 0.0,
            page: 0,
        };
        let (_page, cursor, emitted) =
            super::fragment_inline_root(&mut geom, 500.0, placement, &input);
        assert_eq!(emitted, 1);
        let frags = &geom.get(&1).unwrap().fragments;
        assert!(
            (frags[0].height.to_f32() - 180.0).abs() < 0.01,
            "lead_in 20 + lines 150 + lead_out 10 = 180; got {:?}",
            frags[0].height
        );
        assert!(
            (cursor - 180.0).abs() < 0.01,
            "cursor must advance past the trailing decoration, got {cursor}"
        );
    }

    /// fulgur-pgbrk R1 + css-break-3 §5.4 (`box-decoration-break: slice`):
    /// across a split, the leading decoration belongs to the first
    /// fragment and the trailing decoration to the last — never both to
    /// both, and never to the middle.
    #[test]
    fn inline_root_split_slices_decoration_between_first_and_last_fragment() {
        let mut geom = PaginationGeometryTable::new();
        // Lines 75px; bottoms at 75, 150, 225, 300. Strip 200.
        // With lead_in=20 the first two lines project to 170 (fits) and
        // the third to 245 (overflows) → split after line 1.
        let lines = vec![(0.0, 75.0), (75.0, 150.0), (150.0, 225.0), (225.0, 300.0)];
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 20.0,
            lead_out: 10.0,
            orphans: 2,
            widows: 2,
        };
        let placement = InlinePlacement {
            id: 1,
            x: 0.0,
            width: 100.0,
            cursor_y: 0.0,
            page: 0,
        };
        let (page, cursor, emitted) =
            super::fragment_inline_root(&mut geom, 200.0, placement, &input);
        assert_eq!(emitted, 2);
        assert_eq!(page, 1);
        let entry = geom.get(&1).unwrap();
        let frags = &entry.fragments;
        assert!(
            (frags[0].height.to_f32() - 170.0).abs() < 0.01,
            "first fragment = lead_in 20 + 2 lines 150, no lead_out; got {:?}",
            frags[0].height
        );
        assert!(
            (frags[1].height.to_f32() - 160.0).abs() < 0.01,
            "last fragment = 2 lines 150 + lead_out 10, no lead_in; got {:?}",
            frags[1].height
        );
        assert!((cursor - 160.0).abs() < 0.01, "got {cursor}");
        // Neither fragment may leave the strip — the whole point of
        // measuring the border box.
        for f in frags {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 200.5,
                "fragment escapes the 200px strip: {f:?}"
            );
        }
        // The leads are published so `render::paragraph_lines_for_page`
        // can subtract them back out before partitioning line boxes.
        assert!((entry.content_lead_in.to_f32() - 20.0).abs() < 0.01);
        assert!((entry.content_lead_out.to_f32() - 10.0).abs() < 0.01);
    }

    /// fulgur-pgbrk R1: the whole point — a paragraph whose *decoration*
    /// is what pushes it past the strip must break, not overflow.
    ///
    /// Reproduces the downstream bug report's padded shape: a `<p>` with
    /// `padding: 150px 0` nested two `<div>`s deep, whose line boxes
    /// alone fit the remaining strip but whose border box does not.
    /// Before the fix the geometry recorded `y=100, height=160` for a
    /// 460px-tall box, the overflow check never fired, and the tail was
    /// painted off the paper and discarded.
    #[test]
    fn padded_inline_root_breaks_on_its_border_box_not_its_line_boxes() {
        let html = r#"
            <html><body style="margin:0">
              <div style="height:100px"></div>
              <div><div>
                <p id="probe" style="margin:0;padding:150px 0;line-height:20px;font-size:14px">
                  alpha bravo charlie delta echo foxtrot golf hotel india
                  juliett kilo lima mike november oscar papa quebec romeo
                  sierra tango uniform victor whiskey xray yankee zulu
                </p>
              </div></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
        let table = run_pass(doc.deref_mut(), 400.0);
        let entry = table.get(&probe).expect("probe geometry");

        assert!(
            (entry.content_lead_in.to_f32() - 150.0).abs() < 0.01,
            "padding-top must be read from Taffy — Parley's line metrics are \
             content-box relative and report 0.0 here; got {:?}",
            entry.content_lead_in
        );
        assert!(
            (entry.content_lead_out.to_f32() - 150.0).abs() < 0.01,
            "got {:?}",
            entry.content_lead_out
        );
        assert_eq!(
            entry.fragments.first().map(|f| f.page_index),
            Some(1),
            "the box is 460px tall on a 400px strip starting at y=100, so it \
             must move to a fresh page rather than overflow; frags={:?}",
            entry.fragments
        );
        // The blanket R3 guard in `run_pass_inner` is blind to this shape
        // until the fragment describes the border box, so assert it here
        // explicitly as well.
        for f in &entry.fragments {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 400.5,
                "fragment escapes the 400px strip: {f:?}"
            );
        }
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
        // Cells are not class A break points (css-break-3 §4.1): the
        // row co-splits internally — each cell emits one fragment per
        // crossed strip, clipped to it: page 0 at y=80 (20px clipped
        // at the strip bottom), page 1 at y=0 (40px remainder).
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div id="c1" style="height: 60px; width: 100px"></div>
                <div id="c2" style="height: 60px; width: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let c1 = find_by_id(doc.deref_mut(), "c1").expect("div#c1");
        let c2 = find_by_id(doc.deref_mut(), "c2").expect("div#c2");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        for (id, name) in [(c1, "c1"), (c2, "c2")] {
            assert_cell_slices(
                &geom,
                id,
                name,
                100.0,
                60.0,
                &[(0, 80.0, 20.0), (1, 0.0, 40.0)],
            );
        }
        // Parallel-row alignment: both cells' page-0 slices share the
        // same entry y.
        let y1 = geom.get(&c1).unwrap().fragments[0].y.to_f32();
        let y2 = geom.get(&c2).unwrap().fragments[0].y.to_f32();
        assert!(
            (y1 - y2).abs() < 0.5,
            "page-0 slices must share the same y (parallel row); y1={y1}, y2={y2}"
        );
    }

    #[test]
    fn flex_row_leaf_cells_cosplit_across_page_boundary() {
        // Same clipped-co-split shape as the grid variant above, on a
        // flex container.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: flex; width: 200px;">
                <div id="c1" style="height: 60px; width: 100px; flex: 0 0 100px"></div>
                <div id="c2" style="height: 60px; width: 100px; flex: 0 0 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let c1 = find_by_id(doc.deref_mut(), "c1").expect("div#c1");
        let c2 = find_by_id(doc.deref_mut(), "c2").expect("div#c2");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        for (id, name) in [(c1, "c1"), (c2, "c2")] {
            assert_cell_slices(
                &geom,
                id,
                name,
                100.0,
                60.0,
                &[(0, 80.0, 20.0), (1, 0.0, 40.0)],
            );
        }
        let y1 = geom.get(&c1).unwrap().fragments[0].y.to_f32();
        let y2 = geom.get(&c2).unwrap().fragments[0].y.to_f32();
        assert!(
            (y1 - y2).abs() < 0.5,
            "page-0 slices must share the same y (parallel row); y1={y1}, y2={y2}"
        );
    }

    #[test]
    fn grid_row_recursive_cells_cosplit_across_page_boundary() {
        // 2-col grid; each cell has 2 inner divs (40px each) = 80px.
        // spacer 70px, 100px page → grid row starts at y=70, crossing
        // the boundary (70 + 80 = 150 > 100).
        //
        // The first inner div of EACH column fragments internally:
        // page 0 at y=70 (30px clipped at the strip bottom) and page 1
        // at y=0 (10px remainder). The second inner div continues at
        // page 1 y=10 whole. Originally the sentinel pinned presence:
        // the RIGHT column's first inner div must appear on page 0
        // (pre-slicing the recursion jumped straight to page 1); now
        // it also pins the clipped slice geometry.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 70px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div style="width: 100px">
                  <div id="a1" style="height: 40px; width: 100px"></div>
                  <div id="a2" style="height: 40px; width: 100px"></div>
                </div>
                <div style="width: 100px">
                  <div id="b1" style="height: 40px; width: 100px"></div>
                  <div id="b2" style="height: 40px; width: 100px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let ids = ["a1", "a2", "b1", "b2"]
            .iter()
            .map(|s| (find_by_id(doc.deref_mut(), s).expect(s), *s))
            .collect::<Vec<_>>();
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        for (id, name) in &ids {
            let first = name.ends_with('1');
            let slices: &[(u32, f32, f32)] = if first {
                // First inner div of each column co-splits in place.
                &[(0, 70.0, 30.0), (1, 0.0, 10.0)]
            } else {
                // Second inner div continues on page 1 at y=10.
                &[(1, 10.0, 40.0)]
            };
            assert_cell_slices(&geom, *id, name, 100.0, 40.0, slices);
        }
    }

    #[test]
    fn flex_row_recursive_cells_cosplit_across_page_boundary() {
        // Same clipped-co-split shape as the grid variant above, on a
        // flex container.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 70px"></div>
              <div style="display: flex; width: 200px;">
                <div style="width: 100px; flex: 0 0 100px">
                  <div id="a1" style="height: 40px; width: 100px"></div>
                  <div id="a2" style="height: 40px; width: 100px"></div>
                </div>
                <div style="width: 100px; flex: 0 0 100px">
                  <div id="b1" style="height: 40px; width: 100px"></div>
                  <div id="b2" style="height: 40px; width: 100px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let ids = ["a1", "a2", "b1", "b2"]
            .iter()
            .map(|s| (find_by_id(doc.deref_mut(), s).expect(s), *s))
            .collect::<Vec<_>>();
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        for (id, name) in &ids {
            let first = name.ends_with('1');
            let slices: &[(u32, f32, f32)] = if first {
                &[(0, 70.0, 30.0), (1, 0.0, 10.0)]
            } else {
                &[(1, 10.0, 40.0)]
            };
            assert_cell_slices(&geom, *id, name, 100.0, 40.0, slices);
        }
    }

    // ── per-cell clip arithmetic (fulgur-pgbrk R7b) ──────────────────

    /// fulgur-pgbrk R7b: two-page row — a row entering the strip at
    /// y=80 clips each cell to the remaining 20px and carries the 40px
    /// remainder to the next page's top.
    #[test]
    fn row_cells_clip_per_strip_two_page_row() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div id="c1" style="height: 60px; width: 100px"></div>
                <div id="c2" style="height: 60px; width: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let c1 = find_by_id(doc.deref_mut(), "c1").expect("div#c1");
        let c2 = find_by_id(doc.deref_mut(), "c2").expect("div#c2");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);
        for (id, name) in [(c1, "c1"), (c2, "c2")] {
            assert_cell_slices(
                &geom,
                id,
                name,
                100.0,
                60.0,
                &[(0, 80.0, 20.0), (1, 0.0, 40.0)],
            );
        }
    }

    /// fulgur-pgbrk R7b: three-page row. A row crossing three strips
    /// necessarily has cells taller than one strip — the oversized
    /// branch and the spill branch go through the same
    /// `slice_oversized_leaf` arithmetic, and this pins it: each cell
    /// clips 20px on page 0, a full 100px strip on page 1, and the
    /// 60px remainder on page 2.
    #[test]
    fn row_cells_clip_per_strip_three_page_row() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div id="c1" style="height: 180px; width: 100px"></div>
                <div id="c2" style="height: 180px; width: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let c1 = find_by_id(doc.deref_mut(), "c1").expect("div#c1");
        let c2 = find_by_id(doc.deref_mut(), "c2").expect("div#c2");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);
        for (id, name) in [(c1, "c1"), (c2, "c2")] {
            assert_cell_slices(
                &geom,
                id,
                name,
                100.0,
                180.0,
                &[(0, 80.0, 20.0), (1, 0.0, 100.0), (2, 0.0, 60.0)],
            );
        }
    }

    /// fulgur-pgbrk R7b: exact-fit boundary — a row whose bottom edge
    /// lands exactly on the strip boundary emits no slice at all; each
    /// cell keeps its single 60px fragment.
    #[test]
    fn row_cells_clip_per_strip_exact_fit_is_not_sliced() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 40px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div id="c1" style="height: 60px; width: 100px"></div>
                <div id="c2" style="height: 60px; width: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let c1 = find_by_id(doc.deref_mut(), "c1").expect("div#c1");
        let c2 = find_by_id(doc.deref_mut(), "c2").expect("div#c2");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);
        for (id, name) in [(c1, "c1"), (c2, "c2")] {
            assert_cell_slices(&geom, id, name, 100.0, 60.0, &[(0, 40.0, 60.0)]);
        }
    }

    /// fulgur-pgbrk R7b: unequal cell heights — each cell clips its own
    /// extent: the 60px cell leaves a 40px remainder; the 90px cell
    /// leaves a 70px one. The row's size is the max across cells.
    #[test]
    fn row_cells_clip_per_strip_unequal_cell_heights() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div id="c1" style="height: 60px; width: 100px"></div>
                <div id="c2" style="height: 90px; width: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let c1 = find_by_id(doc.deref_mut(), "c1").expect("div#c1");
        let c2 = find_by_id(doc.deref_mut(), "c2").expect("div#c2");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);
        assert_cell_slices(
            &geom,
            c1,
            "c1",
            100.0,
            60.0,
            &[(0, 80.0, 20.0), (1, 0.0, 40.0)],
        );
        assert_cell_slices(
            &geom,
            c2,
            "c2",
            100.0,
            90.0,
            &[(0, 80.0, 20.0), (1, 0.0, 70.0)],
        );
    }

    // ── break-after: page in nested block subtree (lines 2263-2279) ─────────────

    /// `break-after: page` on a child inside a non-body container must close
    /// the parent's current-page fragment and advance page_index, so the
    /// sibling that follows lands on page 1.
    #[test]
    fn fragment_block_subtree_break_after_page_pushes_sibling_to_next_page() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div>
                <div style="height: 100px; break-after: page"></div>
                <div style="height: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        assert!(
            geom.values()
                .flat_map(|g| g.fragments.iter())
                .any(|f| f.page_index == 1),
            "break-after: page must push the following sibling onto page 1; geom={geom:?}"
        );
    }

    // ── zero-height body-direct children with break (lines 639-668) ─────────────

    /// A zero-height body-direct element with `break-before: page` must fire
    /// a page break when cursor_y > 0 (lines 643-644).
    #[test]
    fn zero_height_body_direct_break_before_page_fires_page_break() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 100px"></div>
              <div style="height: 0px; break-before: page"></div>
              <div style="height: 100px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        assert!(
            geom.values()
                .flat_map(|g| g.fragments.iter())
                .any(|f| f.page_index == 1),
            "break-before: page on a zero-height body-direct child must advance to page 1; \
             geom={geom:?}"
        );
    }

    /// A zero-height body-direct element with `break-after: page` must fire
    /// a page break (lines 667-668).
    #[test]
    fn zero_height_body_direct_break_after_page_fires_page_break() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 100px"></div>
              <div style="height: 0px; break-after: page"></div>
              <div style="height: 100px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        assert!(
            geom.values()
                .flat_map(|g| g.fragments.iter())
                .any(|f| f.page_index == 1),
            "break-after: page on a zero-height body-direct child must advance to page 1; \
             geom={geom:?}"
        );
    }

    // ── zero-height nested children with break (lines 1900-1953) ────────────────

    /// A zero-height element inside a block container with `break-before: page`
    /// must fire a page break and flush a parent fragment (lines 1900-1918).
    #[test]
    fn zero_height_nested_break_before_page_fires_page_break() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div>
                <div style="height: 100px"></div>
                <div style="height: 0px; break-before: page"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        assert!(
            geom.values()
                .flat_map(|g| g.fragments.iter())
                .any(|f| f.page_index == 1),
            "break-before: page on a zero-height nested child must advance to page 1; \
             geom={geom:?}"
        );
    }

    /// A zero-height element inside a block container with `break-after: page`
    /// must fire a page break and flush a parent fragment (lines 1936-1953).
    #[test]
    fn zero_height_nested_break_after_page_fires_page_break() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div>
                <div style="height: 100px"></div>
                <div style="height: 0px; break-after: page"></div>
                <div style="height: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        assert!(
            geom.values()
                .flat_map(|g| g.fragments.iter())
                .any(|f| f.page_index == 1),
            "break-after: page on a zero-height nested child must advance to page 1; \
             geom={geom:?}"
        );
    }

    // ── record_subtree_walk: position:fixed skip (line 3386) ────────────────────

    /// A `position: fixed` child inside an abs subtree must be skipped by
    /// `record_subtree_fragments_at_offset`'s walk (line 3386 `continue`).
    /// The fixed pass handles those separately; the abs walk must not emit them.
    #[test]
    fn record_subtree_walk_skips_position_fixed_child() {
        let html = r#"
            <html><body style="margin: 0">
              <div id="abs_root" style="position: absolute; top: 0; left: 0; width: 200px; height: 200px">
                <div id="fixed_child" style="position: fixed; top: 10px; left: 10px; width: 50px; height: 50px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let abs_id = find_by_id(doc.deref_mut(), "abs_root").expect("abs_root");
        let fixed_id = find_by_id(doc.deref_mut(), "fixed_child").expect("fixed_child");
        let mut geom = PaginationGeometryTable::new();
        super::record_subtree_fragments_at_offset(
            &mut geom,
            doc.deref_mut(),
            abs_id,
            (0.0, 0.0),
            (0.0, 0.0),
            800.0,
            800.0,
            1,
            false,
            &mut 0,
            None,
        );
        assert!(
            !geom.contains_key(&fixed_id),
            "position:fixed child must not be emitted by the abs subtree walk; \
             geom keys={:?}",
            geom.keys().collect::<Vec<_>>()
        );
    }

    // ── record_subtree_walk: position:absolute recursion (lines 3387-3441) ──────

    /// A `position: absolute` child nested inside an abs subtree must be
    /// recursively walked and appear in geometry (lines 3387-3441).
    #[test]
    fn record_subtree_walk_recurses_into_nested_position_absolute_child() {
        let html = r#"
            <html><body style="margin: 0">
              <div id="abs_root" style="position: absolute; top: 0; left: 0; width: 200px; height: 200px">
                <div id="nested_abs" style="position: absolute; top: 10px; left: 10px; width: 80px; height: 80px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let abs_id = find_by_id(doc.deref_mut(), "abs_root").expect("abs_root");
        let nested_id = find_by_id(doc.deref_mut(), "nested_abs").expect("nested_abs");
        let mut geom = PaginationGeometryTable::new();
        super::record_subtree_fragments_at_offset(
            &mut geom,
            doc.deref_mut(),
            abs_id,
            (0.0, 0.0),
            (0.0, 0.0),
            800.0,
            800.0,
            1,
            false,
            &mut 0,
            None,
        );
        assert!(
            geom.contains_key(&nested_id),
            "position:absolute child must be recursively walked and appear in geometry; \
             geom keys={:?}",
            geom.keys().collect::<Vec<_>>()
        );
    }

    // ── has_forced_break_below: recursive return (line 1423) ────────────────────

    /// `has_forced_break_below` must recurse into grandchildren: when a body-
    /// direct outer div contains a middle div (no direct break) that in turn
    /// contains an inner div with `break-after: page`, the recursive call at
    /// line 1422 must return `true` (triggering line 1423) so the outer div
    /// enters `fragment_block_subtree` and the break fires.
    #[test]
    fn has_forced_break_below_recursive_return_detects_grandchild_break() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 300px">
                <div style="height: 150px">
                  <div style="height: 50px; break-after: page"></div>
                  <div style="height: 50px"></div>
                </div>
                <div style="height: 100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        // The inner div has break-after: page; has_forced_break_below must
        // recurse through the middle div and detect it, causing the outer
        // subtree to be split and emitting a page-1 fragment.
        assert!(
            geom.values()
                .flat_map(|g| g.fragments.iter())
                .any(|f| f.page_index == 1),
            "has_forced_break_below must recurse into grandchild and detect \
             break-after: page (line 1423); geom={geom:?}"
        );
    }

    // ── break-after: page on a normal-height body-direct block (lines 1118-1124)

    /// A body-direct block with positive height and `break-after: page` must
    /// advance page_index and reset cursor_y even when the block does NOT
    /// overflow the page and does NOT need recursion. This exercises lines
    /// 1118-1124 in `fragment_pagination_root` (the normal-emit path), which
    /// is distinct from the zero-height path (lines 628-658) and the
    /// slice/recursion paths that both also carry a `break-after` arm.
    #[test]
    fn break_after_page_on_normal_height_body_direct_block_pushes_sibling() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 200px; break-after: page"></div>
              <div style="height: 100px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        // The second div must land on page 1 because the first carries
        // break-after: page; both divs fit in the 800px strip so overflow
        // logic is not what triggers the break.
        assert!(
            geom.values()
                .flat_map(|g| g.fragments.iter())
                .any(|f| f.page_index == 1),
            "break-after: page on a normal-height body-direct block (normal path, \
             lines 1118-1124) must push the sibling to page 1; geom={geom:?}"
        );
    }

    // ── is_float guard in body-direct normal-height path (lines 1108-1110) ──────

    /// A `float: left` body-direct child must be processed by
    /// `fragment_pagination_root` with `is_float = true`, causing the
    /// `if !is_float { prev_used_page = ... }` guard (lines 1108-1110) to
    /// take its false branch. This exercises the normal-height float path.
    ///
    /// The float carries `page: float-page` so that, without the guard,
    /// `prev_used_page` would be set to `Some("float-page")`.  The
    /// following in-flow sibling has the default (unnamed) page, so without
    /// the guard `page_name_changed` would fire and push the sibling to
    /// page 1 — causing `implied_page_count` to become 2.  The guard
    /// suppresses that forced break, keeping both elements on page 0.
    #[test]
    fn body_direct_float_child_processed_without_prev_used_page_update() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="float: left; height: 200px; width: 100px; page: float-page"></div>
              <div style="clear: left; height: 100px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        // The is_float guard prevents the float's named page from being stored
        // in prev_used_page, so the following in-flow sibling must stay on page 0.
        assert_eq!(
            super::implied_page_count(&geom),
            1,
            "is_float guard must prevent float's named page from triggering a \
             forced break before the sibling; both must remain on page 0; geom={geom:?}"
        );
        assert!(
            geom.len() >= 2,
            "float and its sibling must both appear in geometry; geom={geom:?}"
        );
    }

    // ── is_float guard in body-direct slicing path (lines 1056-1058) ────────────

    /// A `float: left` body-direct child whose height exceeds the page height
    /// must be sliced across pages (the `child_h > page_height_px + 1.0` path)
    /// with `is_float = true`. This exercises the `if !is_float` guard at
    /// lines 1056-1058 inside the slicing branch of `fragment_pagination_root`.
    ///
    /// The float carries `page: float-page` so that, without the guard,
    /// `prev_used_page` would be set to `Some("float-page")` after slicing.
    /// The following `clear: left` sibling has the default page, so removing
    /// the guard would trigger `page_name_changed` and advance `page_index`
    /// to 2, landing the sibling on page 2 instead of page 1.
    #[test]
    fn body_direct_tall_float_sliced_across_pages_is_float_guard() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="float: left; height: 1200px; width: 100px; page: float-page"></div>
              <div style="clear: left; height: 100px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        // The 1200px float must be sliced into ≥ 2 fragments (slicing path exercised).
        let max_frags = geom.values().map(|g| g.fragments.len()).max().unwrap_or(0);
        assert!(
            max_frags >= 2,
            "a 1200px float on an 800px page must be sliced into ≥ 2 fragments \
             (exercises lines 1056-1058); max_frags={max_frags}, geom={geom:?}"
        );
        // The is_float guard must prevent the float's named page from being stored
        // in prev_used_page after slicing, so the clear sibling stays on page 1
        // (not forced to page 2 by a spurious page_name_changed).
        assert_eq!(
            super::implied_page_count(&geom),
            2,
            "is_float guard must keep the clear sibling on page 1, not page 2; \
             implied_page_count must be 2 (pages 0 and 1); geom={geom:?}"
        );
    }

    // ── break-inside: avoid (lines 718-720, 732-733) ─────────────────────────

    /// A body-direct element with `break-inside: avoid` must set `avoid_inside`
    /// and take the `Vec::new()` branch (lines 718-733), suppressing the inline
    /// split path so the element is treated as an atomic block.
    #[test]
    fn break_inside_avoid_suppresses_inline_split() {
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <p style="break-inside: avoid">first paragraph</p>
              <p style="break-inside: avoid">second paragraph</p>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        assert_eq!(
            super::implied_page_count(&geom),
            1,
            "two short avoid-inside paragraphs on an 800px page must land on page 0; \
             geom={geom:?}"
        );
    }

    // ── break-after: page after recursive split (lines 876-882) ──────────────

    /// A body-direct element with `break-after: page` that triggers the
    /// recursion path (children overflow the page) must advance the page cursor
    /// *after* the subtree is processed, so the following sibling lands two
    /// pages after where the recursion started (lines 876-882).
    #[test]
    fn break_after_page_advances_after_recursive_split() {
        // 600px page; outer div (break-after: page) has two 400px children —
        // total 800px overflows the page, so `needs_recursion = true`.
        // fragment_block_subtree places inner1 on page 0, inner2 on page 1,
        // leaving page=1, cursor=400.
        // break-after fires: page=2, cursor=0.
        // Trailing 100px sibling must land on page 2.
        //
        // Discriminant vs the oversized-slice path (which would also produce
        // page=2 for #after): in the recursion path `fragment_block_subtree`
        // records inner2 on page 1; in the slice path `record_subtree_descendants`
        // records both inner children on page 0 (the first slice's page).
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="break-after: page">
                <div id="inner1" style="height: 400px"></div>
                <div id="inner2" style="height: 400px"></div>
              </div>
              <div id="after" style="height: 100px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let inner2_id = find_by_id(doc.deref_mut(), "inner2").expect("div#inner2");
        let after_id = find_by_id(doc.deref_mut(), "after").expect("div#after");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 600.0_f32.as_px(), &table);
        // Recursion-specific check: fragment_block_subtree records inner2 on
        // page 1. The slice path would record it on page 0 instead.
        let inner2_geom = geom
            .get(&inner2_id)
            .expect("div#inner2 must be in geometry");
        assert_eq!(
            inner2_geom.fragments[0].page_index, 1,
            "recursion path must place inner2 on page 1; slice path would give page 0; \
             geom={geom:?}"
        );
        let after_geom = geom.get(&after_id).expect("div#after must be in geometry");
        assert_eq!(after_geom.fragments.len(), 1);
        assert_eq!(
            after_geom.fragments[0].page_index, 2,
            "break-after: page after recursive split must push sibling to page 2; \
             geom={geom:?}"
        );
    }

    // ── break-after: page after oversized slice (lines 1059-1065) ────────────

    /// A body-direct element taller than the page (slice path) with
    /// `break-after: page` must advance the page cursor after the last slice
    /// so the following sibling lands on a fresh page (lines 1059-1065).
    #[test]
    fn break_after_page_advances_after_oversized_slice() {
        // 800px page; childless 900px div is sliced: first slice on page 0
        // (800px), second slice on page 1 (100px), cursor=100.
        // break-after fires: page=2, cursor=0.
        // Trailing 100px sibling must land on page 2.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="break-after: page; height: 900px"></div>
              <div id="after" style="height: 100px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let after_id = find_by_id(doc.deref_mut(), "after").expect("div#after");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 800.0_f32.as_px(), &table);
        let after_geom = geom.get(&after_id).expect("div#after must be in geometry");
        assert_eq!(after_geom.fragments.len(), 1);
        assert_eq!(
            after_geom.fragments[0].page_index, 2,
            "break-after: page after slice must push sibling to page 2; geom={geom:?}"
        );
    }

    // ── zero page height guard (line 412) ─────────────────────────────────────

    /// `fragment_pagination_root` must return 0 immediately and leave the
    /// geometry table empty when `page_height_px` is zero (line 412 guard).
    /// `run_pass_with_break_styles` short-circuits before calling
    /// `fragment_pagination_root`, so this test exercises the guard directly.
    #[test]
    fn zero_page_height_guard_returns_empty() {
        let mut doc = parse(
            "<html><body><div style=\"height: 100px\"></div></body></html>",
            600.0,
        );
        let mut tree = PaginationLayoutTree::new(doc.deref_mut(), 0.0);
        let emitted = tree.fragment_pagination_root();
        assert_eq!(
            emitted, 0,
            "zero page height must trigger the early-return guard"
        );
        assert!(
            tree.take_geometry().is_empty(),
            "zero page height must leave geometry table empty"
        );
    }

    // ---------------------------------------------------------------
    // fulgur-pgbrk: page-fragmentation defects reported against 0.40.0
    // (FULGUR_PAGINATION_BUG.md).
    //
    // Two independent defects, both in `fragment_block_subtree`:
    //   1. the strip-overflow cut was gated on `child_page_y >
    //      page_start_y`, which is never true for a parent's LEADING
    //      child, so a box that began mid-page could not break before
    //      its own first child;
    //   2. nested inline roots were never split at line boundaries
    //      (body-direct ones were), so a tall nested `<p>` emitted as
    //      one oversized fragment.
    //
    // Both ended the same way: content laid out past the page bottom,
    // through the margin strip, off the paper, and silently discarded.
    // ---------------------------------------------------------------

    /// Every fragment must begin inside the page strip. A fragment whose
    /// top edge is already below `page_height_px` is content that renders
    /// into the margin strip or off the paper, where it is lost.
    /// Thin wrapper over [`super::find_overflowing_fragments`] so there
    /// is one overflow predicate (fulgur-pgbrk R3).
    ///
    /// Note this is *stricter* than the original helper, which tested
    /// `y > page_h` (the fragment starts below the strip). The shared
    /// predicate tests `y + height > page_h` — a fragment that starts
    /// inside the strip but ends below it is content painted outside
    /// the page box too.
    ///
    /// `doc` is taken so body can be excluded exactly as the production
    /// check in `run_pass_inner` does — body's entry is a
    /// document-level total, not a per-page placement (see
    /// `find_overflowing_fragments`).
    fn assert_no_fragment_starts_below_page(
        table: &PaginationGeometryTable,
        doc: &BaseDocument,
        page_h: f32,
    ) {
        let escaped = super::find_overflowing_fragments(table, page_h, super::find_body_id(doc));
        assert!(
            escaped.is_empty(),
            "fragments extending below the {page_h}px page strip (content would be \
             painted over the bottom margin or clipped off the paper): {escaped:?}"
        );
    }

    /// Build a one-node table for the predicate unit tests below.
    fn table_of(entries: &[(usize, u32, f32, f32)]) -> PaginationGeometryTable {
        let mut t = PaginationGeometryTable::new();
        for &(id, page_index, y, height) in entries {
            t.entry(id).or_default().fragments.push(Fragment {
                page_index,
                x: 0.0_f32.as_px(),
                y: y.as_px(),
                width: 100.0_f32.as_px(),
                height: height.as_px(),
            });
        }
        t
    }

    /// fulgur-pgbrk R7b: pin one flex / grid cell's per-strip clipped
    /// fragments — the "per-cell internal fragmentation" the
    /// spill-slice branch of `fragment_block_subtree` performs
    /// (css-break-3 §2.1 parallel fragmentation flows). Each
    /// `(page, y, height)` entry of `slices` is one expected fragment:
    /// the first slice sits at the cell's entry cursor, every later
    /// slice at the page top, heights clipped at the strip boundary.
    /// Also asserts no fragment extends past `page_h` and that the
    /// slice heights reconstruct the cell's full extent `full_h`.
    #[track_caller]
    fn assert_cell_slices(
        geom: &PaginationGeometryTable,
        node: usize,
        name: &str,
        page_h: f32,
        full_h: f32,
        slices: &[(u32, f32, f32)],
    ) {
        let frags = &geom
            .get(&node)
            .unwrap_or_else(|| panic!("{name} must be in geometry"))
            .fragments;
        assert_eq!(
            frags.len(),
            slices.len(),
            "{name}: one fragment per crossed strip; frags={frags:?}"
        );
        let mut total = 0.0_f32;
        for (i, f) in frags.iter().enumerate() {
            let (page, y, h) = slices[i];
            total += f.height.to_f32();
            assert_eq!(
                f.page_index, page,
                "{name}: slice {i} must sit on page {page}; frags={frags:?}"
            );
            assert!(
                (f.y.to_f32() - y).abs() < 0.5,
                "{name}: slice {i} y={y} (entry cursor first, page top after); frags={frags:?}"
            );
            assert!(
                (f.height.to_f32() - h).abs() < 0.5,
                "{name}: slice {i} clips to {h}px at the strip boundary; frags={frags:?}"
            );
            assert!(
                f.y.to_f32() + f.height.to_f32() <= page_h + 0.5,
                "{name}: no fragment may extend past the {page_h}px strip; frags={frags:?}"
            );
        }
        assert!(
            (total - full_h).abs() < 0.5,
            "{name}: slice heights reconstruct the cell's {full_h}px exactly; frags={frags:?}"
        );
    }

    #[test]
    fn find_overflowing_fragments_reports_nothing_for_an_empty_table() {
        let t = PaginationGeometryTable::new();
        assert!(super::find_overflowing_fragments(&t, 400.0, None).is_empty());
    }

    #[test]
    fn find_overflowing_fragments_ignores_a_fragment_ending_exactly_at_the_strip() {
        let t = table_of(&[(1, 0, 100.0, 300.0)]);
        assert!(super::find_overflowing_fragments(&t, 400.0, None).is_empty());
    }

    #[test]
    fn find_overflowing_fragments_honours_the_half_pixel_epsilon() {
        // 0.4px over is within tolerance; 0.6px over is not.
        let under = table_of(&[(1, 0, 100.0, 300.4)]);
        assert!(super::find_overflowing_fragments(&under, 400.0, None).is_empty());
        let over = table_of(&[(1, 0, 100.0, 300.6)]);
        assert_eq!(
            super::find_overflowing_fragments(&over, 400.0, None).len(),
            1
        );
    }

    #[test]
    fn find_overflowing_fragments_catches_a_fragment_starting_below_the_strip() {
        // The case the original `assert_no_fragment_starts_below_page`
        // tested: y alone is already past the page bottom.
        let t = table_of(&[(1, 0, 500.0, 10.0)]);
        let out = super::find_overflowing_fragments(&t, 400.0, None);
        assert_eq!(out.len(), 1);
        assert!((out[0].overshoot_px - 110.0).abs() < 0.01);
    }

    #[test]
    fn find_overflowing_fragments_reports_overshoot_not_height() {
        let t = table_of(&[(1, 0, 380.0, 100.0)]);
        let out = super::find_overflowing_fragments(&t, 400.0, None);
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].overshoot_px - 80.0).abs() < 0.01,
            "overshoot is bottom - strip, not the fragment height; got {:?}",
            out[0]
        );
    }

    #[test]
    fn find_overflowing_fragments_excludes_only_the_given_body_id() {
        let t = table_of(&[(3, 0, 0.0, 900.0), (5, 0, 0.0, 900.0)]);
        let out = super::find_overflowing_fragments(&t, 400.0, Some(3));
        assert_eq!(out.len(), 1, "body is skipped, its sibling is not");
        assert_eq!(out[0].node_id, 5);
    }

    #[test]
    fn find_overflowing_fragments_is_ordered_by_node_then_page() {
        let t = table_of(&[(9, 1, 0.0, 900.0), (9, 0, 0.0, 900.0), (2, 0, 0.0, 900.0)]);
        let out = super::find_overflowing_fragments(&t, 400.0, None);
        let seen: Vec<(usize, u32)> = out.iter().map(|o| (o.node_id, o.page_index)).collect();
        assert_eq!(seen, vec![(2, 0), (9, 1), (9, 0)]);
    }

    #[test]
    fn find_overflowing_fragments_flags_a_continuing_slice() {
        // Two fragments: the page-0 one is a slice (the node continues
        // on page 1), so its overflow is a height bookkeeping bug.
        let t = table_of(&[(7, 0, 200.0, 400.0), (7, 1, 0.0, 300.0)]);
        let out = super::find_overflowing_fragments(&t, 400.0, None);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].continues_on_later_page,
            "an overflowing non-final fragment continues later; got {:?}",
            out[0]
        );
    }

    #[test]
    fn find_overflowing_fragments_does_not_flag_a_final_fragment_as_continuing() {
        let t = table_of(&[(7, 0, 0.0, 900.0)]);
        let out = super::find_overflowing_fragments(&t, 400.0, None);
        assert_eq!(out.len(), 1);
        assert!(
            !out[0].continues_on_later_page,
            "a sole fragment has nowhere to continue; got {:?}",
            out[0]
        );
    }

    #[test]
    fn parent_slice_height_clamps_a_trailing_margin_to_the_strip() {
        // css-break-3 §5.2: the margin below the last child on the page
        // is truncated at the break rather than painted past it.
        assert!((super::parent_slice_height(408.0, 0.0, 400.0) - 400.0).abs() < 0.01);
    }

    #[test]
    fn parent_slice_height_accounts_for_a_mid_page_start() {
        // A parent starting at y=200 on a 400px page can claim at most
        // the remaining 200px strip.
        assert!((super::parent_slice_height(600.0, 200.0, 400.0) - 200.0).abs() < 0.01);
    }

    #[test]
    fn parent_slice_height_passes_through_content_inside_the_strip() {
        assert!((super::parent_slice_height(300.0, 100.0, 400.0) - 200.0).abs() < 0.01);
    }

    #[test]
    fn parent_slice_height_never_goes_negative() {
        // Defensive: a backward cursor must not produce a negative
        // height, which would corrupt downstream slicing.
        assert!(super::parent_slice_height(50.0, 100.0, 400.0).abs() < 0.01);
        assert!(super::parent_slice_height(100.0, 500.0, 400.0).abs() < 0.01);
    }

    #[test]
    fn break_decision_pushes_a_child_that_overflows_below_the_floor() {
        // Child starts at 200 on a 400 strip and is 300 tall: bottom 500.
        assert_eq!(
            super::break_decision(200.0, 300.0, 0.0, 400.0),
            super::BreakDecision::PushToNextPage
        );
    }

    #[test]
    fn break_decision_places_a_child_that_fits() {
        assert_eq!(
            super::break_decision(200.0, 100.0, 0.0, 400.0),
            super::BreakDecision::PlaceHere
        );
    }

    #[test]
    fn break_decision_floor_decides_a_leading_child() {
        // A leading child sits exactly at its container's page start.
        // With leading-edge propagation permitted the floor is 0, so the
        // break is legal; with the container pinning its children the
        // floor is page_start_y and the child stays put and overflows
        // (fulgur-pgbrk).
        assert_eq!(
            super::break_decision(200.0, 300.0, 0.0, 400.0),
            super::BreakDecision::PushToNextPage
        );
        assert_eq!(
            super::break_decision(200.0, 300.0, 200.0, 400.0),
            super::BreakDecision::PlaceHere
        );
    }

    #[test]
    fn break_decision_is_strict_at_the_floor() {
        // `child_top > floor`, not `>=`.
        assert_eq!(
            super::break_decision(0.0, 500.0, 0.0, 400.0),
            super::BreakDecision::PlaceHere
        );
    }

    #[test]
    fn break_decision_places_a_child_ending_exactly_on_the_page_bottom() {
        // `child_top + h > page_height_px`, not `>=` — a child whose
        // bottom lands exactly on the boundary fits.
        assert_eq!(
            super::break_decision(100.0, 300.0, 0.0, 400.0),
            super::BreakDecision::PlaceHere
        );
    }

    #[test]
    fn break_decision_keeps_an_oversized_child_at_the_page_top() {
        // Nothing to push to. This is the gate that stops the
        // leading-child floor from becoming an infinite page advance.
        assert_eq!(
            super::break_decision(0.0, 900.0, 0.0, 400.0),
            super::BreakDecision::PlaceHere
        );
    }

    /// Walker-convergence phase 5 + fulgur-pgbrk R7b: the recursion
    /// gate is floor-aware. A leading child PINNED at the
    /// `page_start_y` floor (`allow_leading_break == false` — flex /
    /// grid / atomic-inline / orthogonal subtrees) that SPILLS the
    /// strip now reports "recurse": the walk spill-slices it in place
    /// (pre-R7b it fell back to the whole-emit path and the overflow
    /// guard caught it, so "no recursion" used to be pinned here). A
    /// pinned child that FITS still reports "no recursion".
    #[test]
    fn subtree_requires_recursion_pins_leading_child_at_page_start_floor() {
        let html = r#"
            <html><body style="margin: 0">
              <div id="spill">
                <div style="height: 300px"></div>
              </div>
              <div id="fit">
                <div style="height: 150px"></div>
              </div>
            </body></html>
        "#;
        let doc = parse(html, 600.0);
        let spill = find_by_id(&doc, "spill").expect("spill div should exist");
        let fit = find_by_id(&doc, "fit").expect("fit div should exist");
        let cx = super::FragmentationCtx {
            doc: &doc,
            styles: None,
            used_page_names: None,
            running: None,
            page_h: 400.0,
        };
        // 300px child pinned at the 200px floor spills (500 > 400.5)
        // — the walk slices it, which is a split.
        assert!(
            super::subtree_requires_recursion(&cx, spill, 200.0, false),
            "pinned child spilling the strip must trigger recursion — the walk slices it"
        );
        // 150px child pinned at the same floor fits (350 <= 400).
        assert!(
            !super::subtree_requires_recursion(&cx, fit, 200.0, false),
            "pinned child that fits the strip must not trigger recursion"
        );
    }

    /// Same geometry as the pinned case, but with leading-break
    /// propagation permitted (`allow_leading_break == true`) the floor
    /// is 0.0: the leading child's break propagates to the container's
    /// own leading edge (css-break-3 §3), so the gate must answer
    /// "recurse".
    #[test]
    fn subtree_requires_recursion_recurses_leading_child_at_zero_floor() {
        let html = r#"
            <html><body style="margin: 0">
              <div id="probe">
                <div style="height: 300px"></div>
              </div>
            </body></html>
        "#;
        let doc = parse(html, 600.0);
        let probe = find_by_id(&doc, "probe").expect("probe div should exist");
        let cx = super::FragmentationCtx {
            doc: &doc,
            styles: None,
            used_page_names: None,
            running: None,
            page_h: 400.0,
        };
        assert!(
            super::subtree_requires_recursion(&cx, probe, 200.0, true),
            "leading child overflowing below floor 0.0 must trigger \
             recursion — the walk pushes/splits it"
        );
    }

    /// An oversized (taller-than-a-page) leading child sitting exactly
    /// on the `page_start_y` floor: `break_decision` is strict at the
    /// floor, so the push branch can never fire there — no infinite
    /// page advance — yet the gate must still answer "recurse" because
    /// the walk slices the child (`slice_oversized_leaf`), which is a
    /// split. Pinning both sides keeps the gate's terminating decision
    /// from regressing to the floor-blind push interpretation.
    #[test]
    fn subtree_requires_recursion_oversized_child_at_floor_decides_slice_not_loop() {
        let html = r#"
            <html><body style="margin: 0">
              <div id="probe">
                <div style="height: 900px"></div>
              </div>
            </body></html>
        "#;
        let doc = parse(html, 600.0);
        let probe = find_by_id(&doc, "probe").expect("probe div should exist");
        let cx = super::FragmentationCtx {
            doc: &doc,
            styles: None,
            used_page_names: None,
            running: None,
            page_h: 400.0,
        };
        // The paired walk predicate at the floor: PlaceHere, strictly —
        // a child on the floor has nowhere to push to.
        assert_eq!(
            super::break_decision(200.0, 900.0, 200.0, 400.0),
            super::BreakDecision::PlaceHere
        );
        // available_strip 200 of a 400px page ⟹ page_start_y = 200,
        // so the oversized leading child sits exactly on the floor.
        assert!(
            super::subtree_requires_recursion(&cx, probe, 200.0, false),
            "oversized leading child at the floor must still recurse — \
             the walk slices it in place rather than advancing pages \
             forever"
        );
        assert!(
            super::subtree_requires_recursion(&cx, probe, 200.0, true),
            "oversized leading child below floor 0.0 must recurse — \
             pushed, then sliced on the next strip"
        );
    }

    #[test]
    fn leading_child_of_mid_page_block_breaks_instead_of_overflowing() {
        // The reported shape: a filler fills page 0 exactly, then a
        // wrapper whose FIRST child is the content. Pre-fix the inner
        // paragraph was placed at y=1448 on a 1420px page — 28px below
        // the page bottom — and everything past the paper edge vanished.
        let html = r#"
            <html><body style="margin:0">
              <div style="height:1420px"></div>
              <div>
                <div>
                  <div id="probe" style="height:200px"></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 925.0);
        let table = run_pass(&mut doc, 1420.0);
        assert_no_fragment_starts_below_page(&table, &doc, 1420.0);

        let probe = find_by_id(&doc, "probe").expect("probe div should exist");
        let frags = &table
            .get(&probe)
            .expect("probe must have geometry")
            .fragments;
        assert!(
            frags.iter().all(|f| f.page_index >= 1),
            "the leading child must be pushed to page 1, not overflowed off page 0; got {frags:?}"
        );
    }

    #[test]
    fn nested_inline_root_splits_at_line_boundaries() {
        // A multi-line paragraph nested two levels deep, starting near
        // the bottom of the strip. Pre-fix `fragment_block_subtree` had
        // no inline path at all, so this emitted ONE fragment and every
        // line past the page bottom was destroyed. It must now split
        // across pages like a body-direct paragraph does.
        // Long enough to exceed a whole 1420px strip on its own, so the
        // split is required even after the leading-edge break moves the
        // wrapper to a fresh page.
        let text = "word ".repeat(4000);
        let html = format!(
            r#"<html><body style="margin:0">
                 <div style="height:1200px"></div>
                 <div><div><p id="probe" style="margin:0">{text}</p></div></div>
               </body></html>"#
        );
        let mut doc = parse(&html, 925.0);
        let table = run_pass(&mut doc, 1420.0);
        assert_no_fragment_starts_below_page(&table, &doc, 1420.0);

        let probe = find_by_id(&doc, "probe").expect("probe paragraph should exist");
        let geom = table.get(&probe).expect("probe must have geometry");
        assert!(
            geom.is_split(),
            "a nested paragraph taller than the remaining strip must be split across \
             pages, not emitted whole; fragments={:?}",
            geom.fragments
        );
        // Every slice must fit the strip it sits on.
        for f in &geom.fragments {
            let bottom = f.y.to_f32() + f.height.to_f32();
            assert!(
                bottom <= 1420.0 + 0.5,
                "paragraph slice runs past the page bottom: {f:?}"
            );
        }
    }

    /// fulgur-pgbrk gap 1: the leading-break permission must be threaded
    /// through the RECURSION, not just applied to a container's direct
    /// children. `leading_break_is_not_propagated_out_of_a_grid_row` uses
    /// childless cells, so a literal `true` at the `depth + 1` call site
    /// would still satisfy it. Here each grid cell wraps its leading child
    /// one level deeper, so the permission has to survive two recursion
    /// hops to keep the row co-splitting in place.
    #[test]
    fn leading_break_permission_is_threaded_through_recursion() {
        // 100px page, grid row starts at y=80, leading child is 60px tall
        // (80 + 60 = 140 > 100). With the permission correctly cleared for
        // the whole subtree the row overflows in place; if it leaked back
        // to `true` the wrapper would break and the cells would jump to
        // page 1.
        let html = r#"
            <html><body style="margin: 0; padding: 0">
              <div style="height: 80px"></div>
              <div style="display: grid; grid-template-columns: 100px 100px; width: 200px;">
                <div style="width: 100px">
                  <div><div id="lead1" style="height: 60px; width: 100px"></div></div>
                </div>
                <div style="width: 100px">
                  <div><div id="lead2" style="height: 60px; width: 100px"></div></div>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let lead1 = find_by_id(doc.deref_mut(), "lead1").expect("div#lead1");
        let lead2 = find_by_id(doc.deref_mut(), "lead2").expect("div#lead2");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);

        for (id, name) in [(lead1, "lead1"), (lead2, "lead2")] {
            let frags = &geom
                .get(&id)
                .unwrap_or_else(|| panic!("{name} must be in geometry"))
                .fragments;
            assert!(
                frags.iter().any(|f| f.page_index == 0),
                "{name} must stay on page 0 and co-split in place — the leading-break \
                 permission leaked through the recursion into a grid subtree; frags={frags:?}"
            );
        }
    }

    /// fulgur-pgbrk gap 6: the gate reads `suppress_page_check`, which has
    /// three independent sources. Grid is covered above; this pins the
    /// flex and atomic-inline (`inline-block`) sources so a narrowing of
    /// the condition to grid-only is caught.
    #[test]
    fn leading_break_is_not_propagated_out_of_flex_or_inline_block() {
        for (label, container_style) in [
            ("flex", "display: flex; width: 200px"),
            ("inline-block", "display: inline-block; width: 200px"),
        ] {
            let html = format!(
                r#"<html><body style="margin: 0; padding: 0">
                     <div style="height: 80px"></div>
                     <div style="{container_style}">
                       <div style="width: 100px">
                         <div><div id="lead" style="height: 60px; width: 100px"></div></div>
                       </div>
                     </div>
                   </body></html>"#
            );
            let mut doc = parse(&html, 400.0);
            let lead = find_by_id(doc.deref_mut(), "lead").expect("div#lead");
            let table = blitz_adapter::extract_column_style_table(&doc);
            let geom =
                super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);
            let frags = &geom.get(&lead).expect("lead must be in geometry").fragments;
            assert!(
                frags.iter().any(|f| f.page_index == 0),
                "{label}: leading child must stay on page 0 (container is not a class A \
                 break point); frags={frags:?}"
            );
        }
    }

    /// fulgur-pgbrk gap 3: the new inline branch mirrors fulgur-oc51's
    /// parent bookkeeping so a wrapper's background / borders do not
    /// vanish from the pages its paragraph crosses. Without those ~60
    /// lines the wrapper would only be recorded on the LAST page.
    #[test]
    fn nested_inline_split_emits_parent_fragment_on_every_crossed_page() {
        // `wrap` is the paragraph's direct parent, so the inline branch's
        // own bookkeeping (not the recursion branch's) is what records it.
        let text = "word ".repeat(4000);
        let html = format!(
            r#"<html><body style="margin:0; padding:0">
                 <div><div id="wrap" style="background:#eee"><p style="margin:0">{text}</p></div></div>
               </body></html>"#
        );
        let mut doc = parse(&html, 925.0);
        let wrap = find_by_id(doc.deref_mut(), "wrap").expect("div#wrap");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);

        let last_page = geom
            .values()
            .flat_map(|g| g.fragments.iter())
            .map(|f| f.page_index)
            .max()
            .expect("some geometry");
        assert!(
            last_page >= 3,
            "fixture must span 4+ pages to exercise intermediate pages; last_page={last_page}"
        );

        let wrap_frags = &geom.get(&wrap).expect("wrap must be in geometry").fragments;
        for p in 0..=last_page {
            assert!(
                wrap_frags.iter().any(|f| f.page_index == p),
                "wrapper must have a fragment on every page its paragraph crosses \
                 (missing page {p}); frags={wrap_frags:?}"
            );
        }
        // Intermediate pages are full strips.
        for p in 1..last_page {
            let f = wrap_frags
                .iter()
                .find(|f| f.page_index == p)
                .expect("checked above");
            assert!(
                (f.height.to_f32() - 400.0).abs() < 0.5,
                "intermediate wrapper fragment on page {p} must span the full strip; got {f:?}"
            );
        }
    }

    /// fulgur-pgbrk gap 4: when the break is propagated from a box's
    /// leading edge the box places nothing on the page it leaves, so it
    /// must not claim a fragment there. A zero-height fragment would also
    /// flip `is_split()` on and corrupt downstream paragraph slicing.
    #[test]
    fn propagated_leading_break_claims_no_fragment_on_the_outgoing_page() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div style="height:1420px"></div>
              <div id="outer"><div id="wrap">
                <div id="probe" style="height:200px"></div>
              </div></div>
            </body></html>
        "#;
        let mut doc = parse(html, 925.0);
        let outer = find_by_id(doc.deref_mut(), "outer").expect("div#outer");
        let wrap = find_by_id(doc.deref_mut(), "wrap").expect("div#wrap");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 1420.0_f32.as_px(), &table);

        for (id, name) in [(outer, "outer"), (wrap, "wrap")] {
            let g = geom
                .get(&id)
                .unwrap_or_else(|| panic!("{name} must be in geometry"));
            assert!(
                !g.fragments.iter().any(|f| f.page_index == 0),
                "{name} places nothing on page 0 — it must not claim a fragment there \
                 (it would paint an empty box); frags={:?}",
                g.fragments
            );
            assert!(
                !g.is_split(),
                "{name} must not read as split — a stray zero-height fragment corrupts \
                 downstream slicing; frags={:?}",
                g.fragments
            );
        }
    }

    /// The inverse of the guard above: a child that genuinely STARTS on
    /// the page and then splits must still leave the parent's fragment on
    /// that page (the mo-006/008 behaviour the guard deliberately keeps).
    #[test]
    fn split_child_that_starts_on_the_page_still_emits_parent_fragment() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div style="height:200px"></div>
              <div id="outer"><div id="wrap">
                <div style="height:100px"></div>
                <div style="height:300px"></div>
              </div></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let wrap = find_by_id(doc.deref_mut(), "wrap").expect("div#wrap");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let frags = &geom.get(&wrap).expect("wrap must be in geometry").fragments;
        assert!(
            frags.iter().any(|f| f.page_index == 0),
            "wrap placed its first child on page 0, so it must keep a fragment there; \
             frags={frags:?}"
        );
    }

    /// fulgur-pgbrk gap 2: `break-inside: avoid` on a NESTED inline root.
    /// The new branch reads it to suppress the line split; the existing
    /// `break_inside_avoid_suppresses_inline_split` is body-direct and
    /// non-splitting, so it never reaches this code.
    ///
    /// Fulfillable case: the paragraph fits a whole strip, so `avoid` is
    /// honoured — it moves whole to the next page rather than splitting.
    #[test]
    fn nested_avoid_inside_moves_paragraph_whole_when_it_fits_a_page() {
        let text = "word ".repeat(400); // ~307px, fits the 400px strip
        let html = format!(
            r#"<html><body style="margin:0; padding:0">
                 <div style="height:200px"></div>
                 <div><div><p id="probe" style="margin:0; break-inside:avoid">{text}</p></div></div>
               </body></html>"#
        );
        let mut doc = parse(&html, 925.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("p#probe");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let g = geom.get(&probe).expect("probe must be in geometry");
        assert!(
            !g.is_split(),
            "break-inside: avoid must keep the paragraph whole; frags={:?}",
            g.fragments
        );
        assert_eq!(
            g.fragments[0].page_index, 1,
            "the whole paragraph moves to the next page; frags={:?}",
            g.fragments
        );
        assert!(
            g.fragments[0].y.to_f32() + g.fragments[0].height.to_f32() <= 400.5,
            "the moved paragraph must fit its strip; frags={:?}",
            g.fragments
        );
    }

    /// Unfulfillable case: the paragraph is taller than a whole strip, so
    /// no placement can honour `avoid`. CSS Fragmentation 3 §4.4 requires
    /// it to be ignored — obeying it would only push the tail off the page
    /// and destroy it, which is the very failure this work fixes.
    #[test]
    fn nested_avoid_inside_is_relaxed_when_paragraph_exceeds_a_whole_page() {
        let text = "word ".repeat(4000); // ~3072px on a 400px strip
        let html = format!(
            r#"<html><body style="margin:0; padding:0">
                 <div style="height:200px"></div>
                 <div><div><p id="probe" style="margin:0; break-inside:avoid">{text}</p></div></div>
               </body></html>"#
        );
        let mut doc = parse(&html, 925.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("p#probe");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let g = geom.get(&probe).expect("probe must be in geometry");
        assert!(
            g.is_split(),
            "an unfulfillable `avoid` must be relaxed to line-level splitting rather \
             than destroying the tail; frags={:?}",
            g.fragments
        );
        for f in &g.fragments {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 400.5,
                "every slice must fit its strip; frag={f:?}"
            );
        }
    }

    /// fulgur-pgbrk gap 5: `break-after: page` handled inside the new
    /// nested inline branch. The existing coverage is body-direct.
    #[test]
    fn nested_inline_root_honours_break_after_page() {
        let text = "word ".repeat(200);
        let html = format!(
            r#"<html><body style="margin:0; padding:0">
                 <div><div>
                   <p id="probe" style="margin:0; break-after:page">{text}</p>
                   <div id="after" style="height:50px"></div>
                 </div></div>
               </body></html>"#
        );
        let mut doc = parse(&html, 925.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("p#probe");
        let after = find_by_id(doc.deref_mut(), "after").expect("div#after");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let last_probe_page = geom
            .get(&probe)
            .expect("probe")
            .fragments
            .iter()
            .map(|f| f.page_index)
            .max()
            .expect("probe fragment");
        let after_page = geom.get(&after).expect("after").fragments[0].page_index;
        assert!(
            after_page > last_probe_page,
            "break-after: page on a nested inline root must push the following sibling \
             onto a later page; probe_last={last_probe_page} after={after_page}"
        );
    }

    /// fulgur-pgbrk gap 7: a nested multi-line paragraph inside a grid row
    /// drives `row_state` (`emitted_parent_pages`, `max_end_*`,
    /// `crossed_by_recursion`) from brand-new code. Guard against the row
    /// bookkeeping desynchronising — no duplicate parent fragments, and
    /// nothing recorded below the strip.
    #[test]
    fn nested_inline_split_inside_a_grid_row_keeps_row_state_consistent() {
        let text = "word ".repeat(600);
        let html = format!(
            r#"<html><body style="margin:0; padding:0">
                 <div style="display:grid; grid-template-columns:400px 400px; width:800px">
                   <div id="cellA" style="width:400px"><p style="margin:0">{text}</p></div>
                   <div id="cellB" style="width:400px"><p style="margin:0">{text}</p></div>
                 </div>
               </body></html>"#
        );
        let mut doc = parse(&html, 925.0);
        let cell_a = find_by_id(doc.deref_mut(), "cellA").expect("cellA");
        let cell_b = find_by_id(doc.deref_mut(), "cellB").expect("cellB");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);

        for (id, name) in [(cell_a, "cellA"), (cell_b, "cellB")] {
            let frags = &geom.get(&id).expect(name).fragments;
            let mut pages: Vec<u32> = frags.iter().map(|f| f.page_index).collect();
            pages.sort_unstable();
            let before = pages.len();
            pages.dedup();
            assert_eq!(
                before,
                pages.len(),
                "{name} must not receive duplicate fragments for the same page; frags={frags:?}"
            );
        }
        for (id, geo) in &geom {
            for f in &geo.fragments {
                assert!(
                    f.y.to_f32() <= 400.5,
                    "node {id} has a fragment starting below the strip: {f:?}"
                );
            }
        }
    }

    /// fulgur-pgbrk gap 8: the new branch carries an `if !is_float` guard
    /// on `prev_used_page`; both existing float tests are body-direct. A
    /// nested floated inline root must not corrupt the walk.
    #[test]
    fn nested_floated_inline_root_does_not_break_the_walk() {
        let text = "word ".repeat(300);
        let html = format!(
            r#"<html><body style="margin:0; padding:0">
                 <div><div>
                   <p id="probe" style="margin:0; float:left; width:400px">{text}</p>
                   <div id="after" style="height:50px"></div>
                 </div></div>
               </body></html>"#
        );
        let mut doc = parse(&html, 925.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("p#probe");
        let after = find_by_id(doc.deref_mut(), "after").expect("div#after");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        assert!(geom.contains_key(&probe), "floated probe must be recorded");
        assert!(
            geom.contains_key(&after),
            "the following sibling must survive"
        );
        for (id, geo) in &geom {
            for f in &geo.fragments {
                assert!(
                    f.y.to_f32() <= 400.5,
                    "node {id} has a fragment starting below the strip: {f:?}"
                );
            }
        }
    }

    /// fulgur-pgbrk gap 9 / R7: an oversized UNBREAKABLE leaf that is a
    /// parent's leading child at the top of a fresh page. There is
    /// nowhere to push it and no interior break point.
    ///
    /// css-break-3 §4.1 allows a UA either to overflow such a box or to
    /// slice it per fragmentainer. fulgur slices in BOTH the body-direct
    /// path (fulgur-sbw2) and the nested path (fulgur-pgbrk R7) so the
    /// two walks agree and no fragment lands outside the page box.
    ///
    /// It must also keep pinning that the `child_page_y > 0.0` floor is
    /// never "fixed" into an infinite page-advance loop: every slice
    /// after the first starts at the top of its page, and the count is
    /// bounded by the box height.
    #[test]
    fn oversized_unbreakable_leading_leaf_at_page_top_is_sliced_per_strip() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div id="outer"><div id="wrap">
                <div id="probe" style="height:900px"></div>
                <div style="height:50px"></div>
              </div></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("div#probe");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let frags = &geom.get(&probe).expect("probe").fragments;
        // 900px of content on a 400px strip: three slices (400 + 400 +
        // 100), one per fragmentainer, instead of one 900px fragment
        // hanging 500px outside the page box.
        assert_eq!(
            frags.len(),
            3,
            "a childless oversized leaf is sliced per strip; frags={frags:?}"
        );
        let pages: Vec<u32> = frags.iter().map(|f| f.page_index).collect();
        assert_eq!(pages, vec![0, 1, 2], "consecutive pages; frags={frags:?}");
        assert!(
            frags[0].y.to_f32() <= 0.5,
            "the first slice starts at the top of the strip; frags={frags:?}"
        );
        for f in &frags[1..] {
            assert!(
                f.y.to_f32() <= 0.5,
                "every later slice starts at the top of its page — the leading-child \
                 floor must not turn into an infinite page advance; frags={frags:?}"
            );
        }
        let total: f32 = frags.iter().map(|f| f.height.to_f32()).sum();
        assert!(
            (total - 900.0).abs() <= 0.5,
            "the slices reconstruct the box height exactly; frags={frags:?}"
        );
        for f in frags {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 400.5,
                "no slice may extend past the strip; frags={frags:?}"
            );
        }
    }

    #[test]
    fn leading_break_is_not_propagated_out_of_a_grid_row() {
        // Counterpart to the fix: flex / grid items are not class A
        // break points (CSS Fragmentation 3 §3.2) and their rows
        // co-split internally (fulgur-ysms, now clipped per strip by
        // fulgur-pgbrk R7b). The leading-edge break must therefore
        // stop at a grid container rather than dragging a row that
        // cannot move onto the next page. Both cells stay on page 0
        // — as clipped slices — instead of jumping to page 1.
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div style="height:80px"></div>
              <div style="display:grid; grid-template-columns:100px 100px; width:200px;">
                <div id="c1" style="height:60px; width:100px"></div>
                <div id="c2" style="height:60px; width:100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let c1 = find_by_id(doc.deref_mut(), "c1").expect("div#c1");
        let c2 = find_by_id(doc.deref_mut(), "c2").expect("div#c2");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 100.0_f32.as_px(), &table);
        for (id, name) in [(c1, "c1"), (c2, "c2")] {
            assert_cell_slices(
                &geom,
                id,
                name,
                100.0,
                60.0,
                &[(0, 80.0, 20.0), (1, 0.0, 40.0)],
            );
        }
    }

    /// fulgur-pgbrk R8: a forced break on a flex / grid cell must not
    /// close the parent on a page a sibling cell already closed.
    ///
    /// `row_state.emitted_parent_pages` exists so that N parallel
    /// flex / grid cells, each independently deciding to close the
    /// parent on the current page, emit only one parent fragment for
    /// it. Originally only the two unforced (overflow-driven) emission
    /// sites consulted it; the six forced `break-before` /
    /// `break-after: page` sites did not, and the parent came out twice
    /// on page 0 with DIFFERENT heights — `400` from the recursion's
    /// full-strip close and `60` from the forced break.
    ///
    /// That was not cosmetic: `render.rs` reads `frag.height` for
    /// background / border / box-shadow painting whenever `is_split()`,
    /// so the container's decoration was painted twice at two different
    /// sizes on one page, and every fragment-counting walk
    /// (`paragraph_lines_for_page`, `find_overflowing_fragments`) saw a
    /// phantom third fragment.
    ///
    /// The dedup is a property of "the parent is leaving this page",
    /// which is equally true of a forced and an unforced break, so all
    /// six forced sites now take [`ParentSlice::close_unforced`]. The
    /// function tail is deliberately excluded: it closes the parent's
    /// FINAL fragment, on a page the parent never leaves, so it has no
    /// sibling to contend with.
    #[test]
    fn forced_break_does_not_close_a_grid_parent_twice_on_one_page() {
        // Cell 1 crosses a page via RECURSION, which is what sets
        // `crossed_by_recursion` and therefore what makes the same-row
        // rebase at `:2033` restore `page_index` to the row start for
        // cell 2. A forced break alone does not set that flag, so two
        // cells that merely both carry `break-after: page` end up on
        // consecutive pages and never contend for one.
        //
        // Cell 1's recursion emits the parent's page-0 fragment through
        // the DEDUPED path; cell 2 is then restored to page 0 and its
        // `break-after: page` emits through a NON-deduped site.
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div id="grid" style="display:grid; grid-template-columns:100px 100px; width:200px;">
                <div style="width:100px">
                  <div style="height:250px"></div>
                  <div style="height:250px"></div>
                </div>
                <div style="height:60px; width:100px; break-after:page"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 400.0);
        let grid = find_by_id(doc.deref_mut(), "grid").expect("grid");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let frags = &geom.get(&grid).expect("grid").fragments;
        let mut seen: BTreeMap<u32, usize> = BTreeMap::new();
        for f in frags {
            *seen.entry(f.page_index).or_default() += 1;
        }
        let dupes: Vec<(u32, usize)> = seen
            .iter()
            .filter(|&(_, &n)| n > 1)
            .map(|(&p, &n)| (p, n))
            .collect();
        assert!(
            dupes.is_empty(),
            "the parent must be closed at most once per page; \
             duplicated pages={dupes:?}, frags={frags:?}"
        );
    }

    // ---------------------------------------------------------------
    // CSS Fragmentation Module Level 3 conformance map
    // (https://www.w3.org/TR/css-break-3/)
    //
    // One test per normative rule that governs page wrapping. Rules
    // fulgur does not implement yet are `#[ignore]`d with the spec
    // citation — they FAIL when run with `--ignored`, which is the
    // point: each is a pinned, runnable statement of the remaining
    // conformance gap. Remove the `#[ignore]` when implementing the
    // rule.
    //
    // Rules covered elsewhere (no duplicate here):
    // - §4.4 rule 3 (orphans/widows defaults at a feasible split) —
    //   `widow_orphan_minimum_allows_balanced_split` and neighbours.
    // - §4.4 relaxation of `break-inside: avoid` on an unfulfillable
    //   box — `nested_avoid_inside_is_relaxed_when_paragraph_exceeds_
    //   a_whole_page`.
    // - §4.1 flex/grid items are not class A break points —
    //   `leading_break_is_not_propagated_out_of_a_grid_row`.
    // - §5.4 box-decoration-break (slice vs clone at fragment edges)
    //   is a paint-level rule invisible to the geometry table; it
    //   needs a VRT fixture, not a unit test here.
    // ---------------------------------------------------------------

    /// css-break-3 §4.1 class A: an unforced break is allowed between
    /// sibling in-flow block-level boxes. The overflowing second
    /// sibling moves whole to the next page.
    #[test]
    fn css_break3_class_a_unforced_break_between_siblings() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div id="a" style="height:300px"></div>
              <div id="b" style="height:200px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 400.0);
        let a = find_by_id(&doc, "a").expect("div#a");
        let b = find_by_id(&doc, "b").expect("div#b");
        assert_eq!(table.get(&a).expect("a").fragments[0].page_index, 0);
        let bf = &table.get(&b).expect("b").fragments[0];
        assert_eq!(bf.page_index, 1, "b breaks at the class A point before it");
        assert!(bf.y.to_f32() <= 0.5, "b starts at the top of page 1");
    }

    /// css-break-3 §4.1 class C: a break point between a container's
    /// content edge and its first child exists ONLY when there is a
    /// non-zero gap. With no gap, the nearest possible break point is
    /// the class A point BEFORE the container — so an overflowing
    /// leading child must move its whole ancestor chain, never lay out
    /// past the page bottom. This is the normative basis for the
    /// fulgur-pgbrk leading-edge propagation fix.
    #[test]
    fn css_break3_no_class_c_point_without_gap_breaks_before_container() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div style="height:350px"></div>
              <div id="outer"><div id="wrap">
                <div id="probe" style="height:100px"></div>
              </div></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 400.0);
        for name in ["outer", "wrap", "probe"] {
            let id = find_by_id(&doc, name).expect(name);
            let frags = &table.get(&id).expect(name).fragments;
            assert!(
                frags.iter().all(|f| f.page_index == 1),
                "{name} must move entirely to page 1 (no class C point without a gap); \
                 frags={frags:?}"
            );
        }
        let probe = find_by_id(&doc, "probe").expect("probe");
        assert!(
            table.get(&probe).expect("probe").fragments[0].y.to_f32() <= 0.5,
            "probe lands at the top of page 1"
        );
    }

    /// css-break-3 §4.1 class B: an unforced break is allowed between
    /// line boxes inside a block container. A paragraph taller than a
    /// whole page splits at line edges and every slice stays inside
    /// its strip.
    #[test]
    fn css_break3_class_b_break_between_line_boxes() {
        let text = "word ".repeat(2000);
        let html = format!(
            r#"<html><body style="margin:0; padding:0">
                 <p id="probe" style="margin:0">{text}</p>
               </body></html>"#
        );
        let mut doc = parse(&html, 600.0);
        let table = run_pass(&mut doc, 400.0);
        let probe = find_by_id(&doc, "probe").expect("p#probe");
        let g = table.get(&probe).expect("probe");
        assert!(g.is_split(), "class B splitting must engage");
        for f in &g.fragments {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 400.5,
                "every line-box slice stays inside its strip; frag={f:?}"
            );
        }
    }

    /// css-break-3 §4.1 monolithic content: a childless fixed-height
    /// box has no interior break points; the spec permits the UA to
    /// "fragment such boxes by slicing the element's graphical
    /// representation". Body-direct, fulgur takes the slicing option
    /// (fulgur-sbw2): one fragment per page strip, each inside its
    /// strip, and following content continues after the last slice.
    /// The nested walk (fulgur-pgbrk R7) slices identically — pinned
    /// by `oversized_unbreakable_leading_leaf_at_page_top_is_sliced_
    /// per_strip`.
    #[test]
    fn css_break3_monolithic_body_direct_box_is_sliced_per_strip() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div id="mono" style="height:900px"></div>
              <div id="after" style="height:100px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 400.0);
        let mono = find_by_id(&doc, "mono").expect("mono");
        let after = find_by_id(&doc, "after").expect("after");
        let mono_frags = &table.get(&mono).expect("mono").fragments;
        assert_eq!(
            mono_frags.len(),
            3,
            "900px monolithic box on 400px strips slices into 3 fragments; \
             frags={mono_frags:?}"
        );
        for f in mono_frags {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 400.5,
                "each slice stays inside its strip; frag={f:?}"
            );
        }
        let after_frag = &table.get(&after).expect("after").fragments[0];
        assert_eq!(
            after_frag.page_index, 2,
            "the following sibling continues after the last slice; frag={after_frag:?}"
        );
    }

    /// css-break-3 §5.2: "When an unforced break occurs before or
    /// after a block-level box, any margins adjoining the break are
    /// truncated to zero." The pushed sibling's top margin must not
    /// reappear at the top of the new page.
    #[test]
    fn css_break3_s52_margin_adjoining_unforced_break_is_truncated() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div style="height:300px"></div>
              <div id="b" style="height:200px; margin-top:50px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 400.0);
        let b = find_by_id(&doc, "b").expect("div#b");
        let bf = &table.get(&b).expect("b").fragments[0];
        assert_eq!(bf.page_index, 1);
        assert!(
            bf.y.to_f32() <= 0.5,
            "the 50px top margin adjoining the unforced break must be truncated; \
             frag={bf:?}"
        );
    }

    /// css-break-3 §3.1 (child→parent break propagation): "A
    /// break-before value on a first in-flow child box is propagated
    /// to its container." A forced `break-before: page` on a nested
    /// wrapper's first child must therefore break before the WRAPPER
    /// when content already sits on the page.
    ///
    /// Implemented by fulgur-pgbrk R5. The nested walk used to suppress
    /// a leading child's break-before whenever the parent had placed
    /// nothing on the current page (the `cursor_y > page_start_y` gate)
    /// and never handed the forced value up, so the break was dropped
    /// entirely. `fragment_block_subtree` now returns
    /// `SubtreeResult::RequestBreakBefore` in that case and the caller
    /// advances a page and re-enters.
    #[test]
    fn css_break3_s31_forced_break_on_first_child_propagates_to_container() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div style="height:100px"></div>
              <div id="wrap">
                <div id="probe" style="break-before:page; height:100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let frags = &geom.get(&probe).expect("probe").fragments;
        assert!(
            frags.iter().all(|f| f.page_index >= 1),
            "break-before on a first in-flow child propagates to its container, \
             which breaks to page 1; frags={frags:?}"
        );
    }

    /// css-break-3 §4.4 rule 2: breaking at a class A point is not
    /// allowed when a common ancestor of the adjoining siblings has
    /// `break-inside: avoid`. The wrapper must instead move whole to
    /// the next page (its own class A point).
    ///
    /// fulgur-pgbrk R4, css-break-3 §4.4 relaxation: `break-inside:
    /// avoid` is a preference, not a guarantee. A wrapper taller than a
    /// whole page cannot be honoured by moving it — there is no page it
    /// fits — so obeying `avoid` would only push its tail off the paper
    /// and destroy it. The restriction is dropped and the wrapper splits.
    #[test]
    fn block_break_inside_avoid_is_relaxed_when_the_box_exceeds_a_page() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div style="height:100px"></div>
              <div id="wrap" style="break-inside:avoid">
                <div style="height:300px"></div>
                <div style="height:300px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let wrap = find_by_id(doc.deref_mut(), "wrap").expect("wrap");
        let table = blitz_adapter::extract_column_style_table(&doc);
        // 600px of content on a 400px page: no page can hold it whole.
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let g = geom.get(&wrap).expect("wrap");
        assert!(
            g.is_split(),
            "an unfulfillable avoid is relaxed rather than losing the tail; frags={:?}",
            g.fragments
        );
        // The load-bearing assertion. Splitting alone does not
        // distinguish the relaxation from a broken guard: an R4 that
        // requested a break unconditionally would push the wrapper to
        // page 1, where `cursor_in == 0` disables the request, and it
        // would split there instead — still split, still inside the
        // strip. Staying on page 0 is what proves the box was never
        // pushed, because moving it cannot help.
        assert_eq!(
            g.fragments.first().map(|f| f.page_index),
            Some(0),
            "a box that fits no page is not moved; frags={:?}",
            g.fragments
        );
        for f in &g.fragments {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 400.5,
                "no fragment escapes the strip; frags={:?}",
                g.fragments
            );
        }
    }

    /// Implemented by fulgur-pgbrk R4. fulgur used to read
    /// `break-inside` only on inline roots, so a block wrapper's `avoid`
    /// was never consulted and the wrapper split between its children —
    /// the "no-op: break-inside / page-break-inside: avoid" row in
    /// FULGUR_PAGINATION_BUG.md §1.
    ///
    /// The wrapper now hands a `SubtreeResult::RequestBreakBefore` up
    /// when it does not fit the strip it stands on but would fit a fresh
    /// page. If it fits neither, `avoid` is unfulfillable and §4.4's
    /// relaxation clause applies — see
    /// `block_break_inside_avoid_is_relaxed_when_the_box_exceeds_a_page`.
    #[test]
    fn css_break3_s44_rule2_ancestor_break_inside_avoid_forbids_class_a_break() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <div style="height:250px"></div>
              <div id="wrap" style="break-inside:avoid">
                <div id="c1" style="height:100px"></div>
                <div id="c2" style="height:100px"></div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let wrap = find_by_id(doc.deref_mut(), "wrap").expect("wrap");
        let table = blitz_adapter::extract_column_style_table(&doc);
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 400.0_f32.as_px(), &table);
        let g = geom.get(&wrap).expect("wrap");
        assert!(
            !g.is_split(),
            "break-inside:avoid on the wrapper forbids the class A break between \
             its children; frags={:?}",
            g.fragments
        );
        assert!(
            g.fragments.iter().all(|f| f.page_index == 1),
            "the wrapper moves whole to page 1 instead; frags={:?}",
            g.fragments
        );
    }

    /// css-break-3 §4.4 relaxation: "If that still does not lead to
    /// sufficient break points ... the UA may break anywhere in order
    /// to avoid losing content off the edge." When the only
    /// widows/orphans-clean split does not exist, rule 3 must be
    /// RELAXED (split anyway, or split earlier) — never resolved by
    /// letting lines escape the fragmentainer.
    ///
    /// Implemented by fulgur-pgbrk R2: the constrained scan in
    /// `scan_split_points` only ever moves a split LATER (it accumulates
    /// lines forward), so a paragraph whose natural split violates
    /// widows used to emit one oversized fragment whose tail lines
    /// landed past the page bottom — in the margin strip or off the
    /// paper. `fragment_inline_root` now re-runs the scan with the
    /// minimums dropped to 1/1 whenever the constrained plan escapes the
    /// fragmentainer.
    ///
    /// See also `widow_minimum_is_relaxed_rather_than_losing_the_tail_line`
    /// and `orphan_minimum_is_relaxed_rather_than_losing_the_tail_lines`,
    /// which pin the resulting fragment geometry rather than just the
    /// no-escape invariant.
    #[test]
    fn css_break3_s44_widow_relaxation_prevents_lines_escaping_the_strip() {
        let mut geom = PaginationGeometryTable::new();
        // Lines 75px; bottoms at 75, 150, 225. Page strip = 200. The
        // natural split (after line 2) violates widows=2; the orphan-
        // clean alternative does not exist. Relaxation requires
        // splitting anyway rather than emitting a 225px fragment on a
        // 200px strip.
        let lines = vec![(0.0, 75.0), (75.0, 150.0), (150.0, 225.0)];
        let input = InlineSplitInput {
            line_metrics: &lines,
            lead_in: 0.0,
            lead_out: 0.0,
            orphans: 2,
            widows: 2,
        };
        let placement = InlinePlacement {
            id: 1,
            x: 0.0,
            width: 100.0,
            cursor_y: 0.0,
            page: 0,
        };
        super::fragment_inline_root(&mut geom, 200.0, placement, &input);
        for f in &geom.get(&1).unwrap().fragments {
            assert!(
                f.y.to_f32() + f.height.to_f32() <= 200.5,
                "no fragment may extend past the fragmentainer once break \
                 restrictions are relaxed; frag={f:?}"
            );
        }
    }

    /// css-break-3 §4.4 rule 3: `orphans` / `widows` take author-
    /// specified values; fulgur hardcodes the initial value 2 for both
    /// and never reads the CSS properties.
    ///
    /// Implemented by fulgur-pgbrk R6. With `widows: 4`, a 6-line
    /// paragraph on a page fitting 4 lines splits 2/4 (the tail keeps 4
    /// lines) rather than 4/2. `<br>`-forced lines with a fixed
    /// `line-height` make the line count and heights font-independent.
    ///
    /// Honouring the value needs `scan_split_points` to back the split
    /// UP from the natural overflow point — the widow minimum is the one
    /// constraint that can only be satisfied by splitting earlier.
    #[test]
    fn css_break3_s44_rule3_author_widows_value_shifts_the_split() {
        let html = r#"
            <html><body style="margin:0; padding:0">
              <p id="probe" style="margin:0; line-height:100px; widows:4">
                a<br>b<br>c<br>d<br>e<br>f
              </p>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
        let table = blitz_adapter::extract_column_style_table(&doc);
        // 450px strip fits 4 of the 100px lines. Default widows=2
        // splits 4/2 (tail ≈ 200px); honouring widows:4 requires 2/4
        // (tail ≈ 400px).
        let geom = super::run_pass_with_break_styles(doc.deref_mut(), 450.0_f32.as_px(), &table);
        let g = geom.get(&probe).expect("probe");
        assert!(g.is_split(), "fixture must actually cross pages");
        let last = g.fragments.last().unwrap();
        assert!(
            last.height.to_f32() >= 350.0,
            "widows:4 must leave 4 lines (≈400px) in the tail fragment; \
             frags={:?}",
            g.fragments
        );
    }

    // ── PaginationGeometry::is_split ──────────────────────────────────────────

    #[test]
    fn is_split_empty_fragments_returns_false() {
        let geom = PaginationGeometry::default();
        assert!(!geom.is_split());
    }

    #[test]
    fn is_split_single_fragment_not_repeat_returns_false() {
        let mut geom = PaginationGeometry::default();
        geom.fragments.push(Fragment {
            page_index: 0,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
        });
        assert!(!geom.is_split());
    }

    #[test]
    fn is_split_two_fragments_not_repeat_returns_true() {
        let mut geom = PaginationGeometry::default();
        for page in [0_u32, 1] {
            geom.fragments.push(Fragment {
                page_index: page,
                x: 0.0_f32.as_px(),
                y: 0.0_f32.as_px(),
                width: 100.0_f32.as_px(),
                height: 50.0_f32.as_px(),
            });
        }
        assert!(geom.is_split());
    }

    #[test]
    fn is_split_two_fragments_is_repeat_returns_false() {
        let mut geom = PaginationGeometry {
            is_repeat: true,
            ..Default::default()
        };
        for page in [0_u32, 1] {
            geom.fragments.push(Fragment {
                page_index: page,
                x: 0.0_f32.as_px(),
                y: 0.0_f32.as_px(),
                width: 100.0_f32.as_px(),
                height: 50.0_f32.as_px(),
            });
        }
        assert!(
            !geom.is_split(),
            "is_repeat=true prevents split classification"
        );
    }

    // ── collect_counter_states ───────────────────────────────────────────────

    #[test]
    fn counter_states_empty_geometry_returns_one_empty_page() {
        let geom = PaginationGeometryTable::new();
        let ops: BTreeMap<usize, Vec<crate::gcpm::CounterOp>> = BTreeMap::new();
        let states = collect_counter_states(&geom, &ops);
        assert_eq!(states.len(), 1);
        assert!(states[0].is_empty());
    }

    #[test]
    fn counter_states_reset_op_sets_counter_value() {
        let mut geom = PaginationGeometryTable::new();
        geom.entry(10).or_default().fragments.push(Fragment {
            page_index: 0,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
        });
        let mut ops: BTreeMap<usize, Vec<crate::gcpm::CounterOp>> = BTreeMap::new();
        ops.insert(
            10,
            vec![crate::gcpm::CounterOp::Reset {
                name: "chapter".into(),
                value: 3,
            }],
        );
        let states = collect_counter_states(&geom, &ops);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].get("chapter").copied(), Some(3));
    }

    #[test]
    fn counter_states_increment_op_adds_to_counter() {
        let mut geom = PaginationGeometryTable::new();
        for (node_id, page) in [(10_usize, 0_u32), (20, 0)] {
            geom.entry(node_id).or_default().fragments.push(Fragment {
                page_index: page,
                x: 0.0_f32.as_px(),
                y: 0.0_f32.as_px(),
                width: 100.0_f32.as_px(),
                height: 50.0_f32.as_px(),
            });
        }
        let mut ops: BTreeMap<usize, Vec<crate::gcpm::CounterOp>> = BTreeMap::new();
        ops.insert(
            10,
            vec![crate::gcpm::CounterOp::Reset {
                name: "chapter".into(),
                value: 0,
            }],
        );
        ops.insert(
            20,
            vec![crate::gcpm::CounterOp::Increment {
                name: "chapter".into(),
                value: 1,
            }],
        );
        let states = collect_counter_states(&geom, &ops);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].get("chapter").copied(), Some(1));
    }

    #[test]
    fn counter_states_set_op_forces_counter_value() {
        let mut geom = PaginationGeometryTable::new();
        geom.entry(10).or_default().fragments.push(Fragment {
            page_index: 0,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
        });
        let mut ops: BTreeMap<usize, Vec<crate::gcpm::CounterOp>> = BTreeMap::new();
        ops.insert(
            10,
            vec![
                crate::gcpm::CounterOp::Reset {
                    name: "idx".into(),
                    value: 99,
                },
                crate::gcpm::CounterOp::Set {
                    name: "idx".into(),
                    value: 7,
                },
            ],
        );
        let states = collect_counter_states(&geom, &ops);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].get("idx").copied(), Some(7));
    }

    #[test]
    fn counter_states_carries_across_pages() {
        let mut geom = PaginationGeometryTable::new();
        geom.entry(10).or_default().fragments.push(Fragment {
            page_index: 0,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
        });
        geom.entry(20).or_default().fragments.push(Fragment {
            page_index: 1,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
        });
        let mut ops: BTreeMap<usize, Vec<crate::gcpm::CounterOp>> = BTreeMap::new();
        ops.insert(
            10,
            vec![crate::gcpm::CounterOp::Reset {
                name: "chapter".into(),
                value: 1,
            }],
        );
        let states = collect_counter_states(&geom, &ops);
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].get("chapter").copied(), Some(1));
        assert_eq!(states[1].get("chapter").copied(), Some(1));
    }

    #[test]
    fn counter_states_node_without_ops_is_skipped() {
        let mut geom = PaginationGeometryTable::new();
        geom.entry(99).or_default().fragments.push(Fragment {
            page_index: 0,
            x: 0.0_f32.as_px(),
            y: 0.0_f32.as_px(),
            width: 100.0_f32.as_px(),
            height: 50.0_f32.as_px(),
        });
        let ops: BTreeMap<usize, Vec<crate::gcpm::CounterOp>> = BTreeMap::new();
        let states = collect_counter_states(&geom, &ops);
        assert_eq!(states.len(), 1);
        assert!(states[0].is_empty());
    }

    /// Sum of a node's fragment heights, for the border-box conservation
    /// checks below.
    fn total_h(entry: &PaginationGeometry) -> f32 {
        entry.fragments.iter().map(|f| f.height.to_f32()).sum()
    }

    /// css-break-3 §3.1.1: a `break-before` on a box's first in-flow child
    /// is a break before the box. R5 (`cd24af40`) implemented this, but
    /// gated it on `cursor_y <= page_start_y` — and `cursor_y` has already
    /// been raised to `page_start_y + this_top_in_parent`, which for a
    /// leading child IS the container's `border-top + padding-top`. So any
    /// top decoration on the container defeated its own propagation and
    /// stranded a decoration-sized stub on the outgoing page.
    ///
    /// Pinned as a pair: the only difference between the two documents is
    /// the container's leading decoration, and both must move whole.
    #[test]
    fn leading_forced_break_propagates_through_container_top_decoration() {
        for (label, deco) in [
            ("bare", "padding:0; border:0"),
            ("padded", "padding:20px; border:5px solid #000"),
        ] {
            let html = format!(
                r#"
                <html><body style="margin:0">
                  <div style="height:200px"></div>
                  <div id="probe" style="{deco}">
                    <p style="margin:0; height:20px; break-before:page">one</p>
                    <p style="margin:0; height:20px">two</p>
                  </div>
                </body></html>
            "#
            );
            let mut doc = parse(&html, 360.0);
            let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
            let box_h = doc
                .get_node(probe)
                .expect("probe node")
                .final_layout
                .size
                .height;
            let styles = blitz_adapter::extract_column_style_table(&doc);
            let table =
                super::run_pass_with_break_styles(doc.deref_mut(), 260.0_f32.as_px(), &styles);
            let entry = table.get(&probe).expect("probe geometry");
            assert_eq!(
                entry.fragments.len(),
                1,
                "[{label}] the break belongs before the box, so it moves whole \
                 rather than stranding a {}px decoration stub; frags={:?}",
                entry
                    .fragments
                    .first()
                    .map(|f| f.height.to_f32())
                    .unwrap_or(0.0),
                entry.fragments,
            );
            assert_eq!(
                entry.fragments[0].page_index, 1,
                "[{label}] frags={:?}",
                entry.fragments
            );
            // A box that moved whole keeps its entire border box —
            // including the trailing decoration that `cursor_y` (child
            // content only) does not cover.
            assert!(
                (total_h(entry) - box_h).abs() < 0.51,
                "[{label}] expected the full {box_h}px border box; got {}px, \
                 frags={:?}",
                total_h(entry),
                entry.fragments,
            );
        }
    }

    /// The same rule for an *unforced* break. The container's leading child
    /// overflows the strip only because the container's own padding pushed
    /// it there, so the break belongs before the container (css-break-3
    /// §3 / §4.1 — a class B break is only usable if the leading
    /// decoration it leaves behind actually fits).
    #[test]
    fn leading_overflow_propagates_instead_of_stranding_decoration() {
        let html = r#"
            <html><body style="margin:0">
              <div style="height:250px"></div>
              <div id="probe" style="padding:20px; border:5px solid #000">
                <p style="margin:0; height:14px">hello</p>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 360.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
        let box_h = doc
            .get_node(probe)
            .expect("probe node")
            .final_layout
            .size
            .height;
        let table = run_pass(doc.deref_mut(), 260.0);
        let entry = table.get(&probe).expect("probe geometry");
        assert_eq!(
            entry.fragments.len(),
            1,
            "only 10px of the box's 25px leading decoration fits the strip, \
             so the class B break before its first child is unusable and the \
             box must move whole; frags={:?}",
            entry.fragments
        );
        assert!(
            (total_h(entry) - box_h).abs() < 0.51,
            "expected the full {box_h}px border box; got {}px, frags={:?}",
            total_h(entry),
            entry.fragments,
        );
    }

    /// A container cut *inside its own leading decoration*: the unspent
    /// remainder is still owed on the continuation, so the box's border
    /// box adds up across pages and its content is inset by what is
    /// left rather than slammed against the strip top.
    ///
    /// Uses a grid cell because that is where the case survives — in
    /// block flow a container whose leading decoration does not fit now
    /// moves whole instead (`propagate_leading_break`), while inside a
    /// grid propagation is forbidden and the cell must cut in place.
    /// The cell's `padding-top` is 4px and only 2px of it fit, so the
    /// paragraph resumes 2px down.
    #[test]
    fn container_cut_inside_its_leading_decoration_owes_the_remainder() {
        // Fixed-height children rather than text, so the row pitch is an
        // exact 23px (4 + 14 + 4 + 1) and does not move with font
        // metrics. Rows then start at 0, 23, ... 253; a 255px strip cuts
        // 2px into row 12's 4px padding-top.
        let cells: String = (1..=12)
            .map(|r| {
                format!(
                    "<div class=c id=\"c{r}\"><div class=k id=\"k{r}\"></div></div>\
                     <div class=c><div class=k></div></div>"
                )
            })
            .collect();
        let html = format!(
            r#"
            <html><body style="margin:0">
              <style>
                .g {{ display:grid; grid-template-columns:1fr 1fr }}
                .c {{ padding:4px; border-bottom:1px solid #000 }}
                .k {{ height:14px }}
              </style>
              <div class="g">{cells}</div>
            </body></html>
        "#
        );
        let mut doc = parse(&html, 360.0);
        let probe = find_by_id(doc.deref_mut(), "c12").expect("c12");
        let box_h = doc
            .get_node(probe)
            .expect("probe node")
            .final_layout
            .size
            .height;
        let table = run_pass(doc.deref_mut(), 255.0);
        let entry = table.get(&probe).expect("probe geometry");
        assert_eq!(
            entry.fragments.len(),
            2,
            "the last row straddles the boundary; frags={:?}",
            entry.fragments
        );
        let consumed_on_page_0 = entry.fragments[0].height.to_f32();
        assert!(
            consumed_on_page_0 < 4.0,
            "the cut must land INSIDE the 4px padding-top for this test to \
             exercise anything; got {consumed_on_page_0}px, frags={:?}",
            entry.fragments
        );
        assert!(
            (total_h(entry) - box_h).abs() < 0.51,
            "the two fragments must partition the {box_h}px border box; got \
             {}px, frags={:?}",
            total_h(entry),
            entry.fragments,
        );
        // …and the content inside resumes below the unspent padding
        // rather than flush against the strip top.
        let inner = find_by_id(doc.deref_mut(), "k12").expect("k12");
        let inner_frag = table
            .get(&inner)
            .and_then(|g| g.fragments.iter().find(|f| f.page_index == 1))
            .expect("inner child on page 1");
        let expected_inset = 4.0 - consumed_on_page_0;
        assert!(
            (inner_frag.y.to_f32() - expected_inset).abs() < 0.51,
            "content must resume {expected_inset}px down (the unspent \
             padding-top), got y={}",
            inner_frag.y.to_f32()
        );
    }

    /// A forced break on the leading child of a *grid cell*, where
    /// leading-edge propagation is forbidden (`suppress_page_check` —
    /// css-break-3 §3.2, grid items are not class A break points).
    ///
    /// Deliberate behaviour change, pinned because it is a trade between
    /// two imperfect outputs and neither had coverage. The break used to
    /// be honoured locally, at the cell's own content edge: cell 1's
    /// content went to page 2 while its parallel sibling stayed on page
    /// 1, **tearing the row in half across the page boundary** — the same
    /// class of defect as a straddling grid row whose cells fragment
    /// independently. Now the break collapses instead and the row stays
    /// intact on one page.
    ///
    /// Neither is what CSS asks for. The break should propagate out
    /// through the grid to the class A point before the grid container,
    /// moving the whole grid. That needs row-level agreement in
    /// `RowState`, which is a separate piece of work; until then, keeping
    /// the row together is the less damaging of the two available
    /// answers.
    #[test]
    fn forced_break_inside_a_grid_cell_collapses_rather_than_tearing_the_row() {
        let html = r#"
            <html><body style="margin:0">
              <div style="height:200px"></div>
              <div style="display:grid; grid-template-columns:1fr 1fr">
                <div id="a" style="padding:10px; border:2px solid #000">
                  <p style="margin:0; height:20px; break-before:page">a</p>
                </div>
                <div id="b" style="padding:10px; border:2px solid #000">
                  <p style="margin:0; height:20px">b</p>
                </div>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 360.0);
        let a = find_by_id(doc.deref_mut(), "a").expect("a");
        let b = find_by_id(doc.deref_mut(), "b").expect("b");
        let styles = blitz_adapter::extract_column_style_table(&doc);
        let table = super::run_pass_with_break_styles(doc.deref_mut(), 260.0_f32.as_px(), &styles);
        let pages_of = |id: usize| -> Vec<u32> {
            table
                .get(&id)
                .map(|e| e.fragments.iter().map(|f| f.page_index).collect())
                .unwrap_or_default()
        };
        assert_eq!(
            pages_of(a),
            pages_of(b),
            "parallel cells of one row must not land on different pages; \
             a={:?} b={:?}",
            table.get(&a).map(|e| &e.fragments),
            table.get(&b).map(|e| &e.fragments),
        );
    }

    /// A container that genuinely continues onto a later page must claim
    /// the whole remaining strip on the page it is leaving, not stop at
    /// the last child that fit. `emit_parent_page_spans` already spans the
    /// full strip for the recursion / slicing crossings; the push and
    /// forced-break closers used the child cursor instead, so a container
    /// with padding-bottom (or simply a gap below its last fitting child)
    /// had its background and side borders stop short of the page bottom.
    #[test]
    fn continuing_container_claims_the_full_strip_on_the_page_it_leaves() {
        let html = r#"
            <html><body style="margin:0">
              <div style="height:200px"></div>
              <div id="probe" style="padding:20px; border:5px solid #000">
                <p style="margin:0; height:20px">one</p>
                <p style="margin:0; height:20px">two</p>
                <p style="margin:0; height:20px">three</p>
              </div>
            </body></html>
        "#;
        let mut doc = parse(html, 360.0);
        let probe = find_by_id(doc.deref_mut(), "probe").expect("probe");
        let table = run_pass(doc.deref_mut(), 260.0);
        let entry = table.get(&probe).expect("probe geometry");
        assert!(
            entry.fragments.len() >= 2,
            "the box is 110px starting at y=200 on a 260px strip, so it splits; \
             frags={:?}",
            entry.fragments
        );
        let first = &entry.fragments[0];
        assert!(
            (first.y.to_f32() + first.height.to_f32() - 260.0).abs() < 0.51,
            "the outgoing fragment must reach the strip bottom at 260px, not \
             stop at the last child that fit; got y={} h={}, frags={:?}",
            first.y.to_f32(),
            first.height.to_f32(),
            entry.fragments,
        );
        // …and the continuation carries the box's trailing decoration:
        // two 20px children pushed here, plus 20px padding-bottom and
        // the 5px border-bottom.
        let last = entry.fragments.last().expect("last fragment");
        assert!(
            (last.height.to_f32() - 65.0).abs() < 0.51,
            "the final fragment must close the border box — 2 x 20px content \
             + 20px padding-bottom + 5px border-bottom; got {}px, frags={:?}",
            last.height.to_f32(),
            entry.fragments,
        );
    }
}
