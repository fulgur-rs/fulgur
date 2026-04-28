//! Spike: Taffy-hooked block-only paginator (fulgur-4cbc).
//!
//! Sibling of [`crate::multicol_layout`]. The multicol module proves the
//! `LayoutPartialTree` wrapper pattern works for routing one CSS feature
//! through fulgur-owned layout while leaving the rest to `BaseDocument`.
//! This module is a feasibility evaluation of the same idiom for page
//! fragmentation.
//!
//! # Status: spike (no production wiring)
//!
//! `run_pass` invokes `taffy::compute_root_layout(&mut wrapper, body_id, ...)`
//! and the wrapper's `compute_child_layout` intercepts the body to
//! dispatch into [`compute_pagination_layout`]. That function walks the
//! body's direct block children (using `final_layout` already populated
//! by Blitz's first-pass `resolve()`) and records what fragments would
//! be produced if the page were sliced at `page_height_px`.
//!
//! Concretely it is still a **measurement walk wrapped in a real Taffy
//! callback** — it does not yet re-issue per-strip layout requests with
//! constrained `available_space`. The point of routing through
//! `compute_root_layout` now is structural: it forces every trait method
//! on the wrapper to be exercised at runtime so the next iteration —
//! constraining `available_space.height = page_strip` and feeding each
//! child a remaining-strip-height — can swap the body of
//! `compute_pagination_layout` without changing the public surface.
//!
//! # Scope (block-only)
//!
//! - `<body>`'s direct block children only. Anything nested inside those
//!   children is reused as-is from `final_layout`.
//! - No `break-before` / `break-after` / `break-inside`. No widow/orphan.
//! - No out-of-flow handling (`position: fixed` is owned by
//!   `blitz_adapter::relayout_position_fixed`; floats / abs are not
//!   considered here).
//! - No table-row / flex-item / multicol-internal break.
//! - Inline (Parley) break is out of scope; a paragraph that overflows
//!   the page is recorded as a single oversized fragment for now.

// Spike scaffolding: every public item is exercised only by the
// in-module `#[cfg(test)] mod tests` until follow-up work wires this
// into the engine pipeline. `#[allow(dead_code)]` keeps the warning
// surface clean during the spike — remove it once `engine.rs` calls
// `run_pass` for production rendering or the comparison harness.
#![allow(dead_code)]

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
/// For the block-only spike the vector is normally length 1 (the node
/// fits on one page). A node taller than the page produces multiple
/// fragments — but in the current measurement-only implementation we
/// emit it as a single oversized fragment on the page where its top
/// edge lands, because we have no inline / break point information yet.
#[derive(Clone, Debug, Default)]
pub struct PaginationGeometry {
    pub fragments: Vec<Fragment>,
}

/// Side-table mapping DOM `usize` NodeIds to their pagination geometry.
///
/// `BTreeMap` for the same determinism reason as
/// [`crate::multicol_layout::MulticolGeometryTable`]: PDF byte order
/// downstream depends on iteration order.
pub type PaginationGeometryTable = BTreeMap<usize, PaginationGeometry>;

/// Taffy tree wrapper that — once the spike grows beyond measurement — will
/// intercept the pagination root through `compute_child_layout` and route
/// it through fulgur's own page-stripping layout.
///
/// `page_height_px` is the height of the page content area (after the
/// engine has subtracted page-margin / `@page` margins). The wrapper
/// borrows the `BaseDocument` for one pass and is discarded; the
/// `geometry` it accumulates is drained via [`Self::take_geometry`] for
/// downstream comparison or convert wiring.
pub struct PaginationLayoutTree<'a> {
    pub(crate) doc: &'a mut BaseDocument,
    pub(crate) page_height_px: f32,
    pub(crate) geometry: PaginationGeometryTable,
    /// Cached id of the `<body>` element, if any. Used as the
    /// fragmentation root for the block-only spike. `None` means the
    /// document had no body and the pass becomes a no-op.
    pub(crate) body_id: Option<usize>,
    /// Available-space mode for `drive_taffy_root_layout`. Set via
    /// [`run_pass_constrained`] (fulgur-ik6o spike); the default
    /// [`run_pass`] keeps `MaxContent` so the post-walk reads natural
    /// child heights.
    pub(crate) strip_mode: StripMode,
    /// fulgur-k0g0: `break-before` / `break-after` / `break-inside`
    /// per node, harvested by
    /// [`crate::blitz_adapter::extract_column_style_table`]. The table
    /// is shared with `multicol_layout` (Pageable's
    /// `extract_pagination_from_column_css` reads the same fields), so
    /// the pagination spike does not maintain its own break-style
    /// extraction. `None` means "no break properties set anywhere",
    /// which the fragmenter treats as all-`Auto`.
    pub(crate) column_styles: Option<&'a crate::column_css::ColumnStyleTable>,
}

/// `available_space.height` mode passed to `taffy::compute_root_layout`
/// when driving the body's layout.
///
/// The `MaxContent` variant matches the original spike: body is laid
/// out at its natural height, and the spike post-walks `final_layout`
/// to fragment children. `Definite` is the fulgur-ik6o experiment —
/// constrain Taffy to one page strip and see what happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripMode {
    /// Pass `AvailableSpace::MaxContent` so children retain natural
    /// heights and the spike's post-walk does the fragmentation.
    MaxContent,
    /// Pass `AvailableSpace::Definite(page_height_px)` and observe
    /// whether Taffy/Blitz produces a strip-constrained layout we
    /// can fragment differently.
    Definite,
}

/// One-shot entry: run the block-only fragmenter for `doc` against a
/// `page_height_px` page strip and return the resulting geometry table.
///
/// Intended to be called **after** `blitz_adapter::resolve()` (and after
/// `multicol_layout::run_pass` when multicol is in play) so that
/// `final_layout` reflects the post-layout positions the spike walks.
///
/// Drives the wrapper through `taffy::compute_root_layout(&mut tree,
/// body_id, ...)` so [`PaginationLayoutTree::compute_child_layout`]
/// fires for body and dispatches into [`compute_pagination_layout`].
/// The body's existing layout is restored after the call so we do not
/// disturb downstream consumers of `final_layout` (`convert::dom_to_pageable`
/// reads body's location to position children — `compute_root_layout`
/// would otherwise rewrite it to (0, 0)).
///
/// Callers should treat the returned table as observational only — it is
/// not wired into the existing `Pageable` / `paginate` path.
pub fn run_pass(doc: &mut BaseDocument, page_height_px: f32) -> PaginationGeometryTable {
    run_pass_with_mode(doc, page_height_px, StripMode::MaxContent, None)
}

/// fulgur-ik6o variant: drive `compute_root_layout` with
/// `AvailableSpace::Definite(page_height_px)` instead of `MaxContent`.
///
/// Observational entry — used by the comparison harness to record per-
/// fixture differences between the two modes. Same caveat as
/// [`run_pass`]: not wired into production.
pub fn run_pass_constrained(
    doc: &mut BaseDocument,
    page_height_px: f32,
) -> PaginationGeometryTable {
    run_pass_with_mode(doc, page_height_px, StripMode::Definite, None)
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
    run_pass_with_mode(
        doc,
        page_height_px,
        StripMode::MaxContent,
        Some(column_styles),
    )
}

fn run_pass_with_mode<'a>(
    doc: &'a mut BaseDocument,
    page_height_px: f32,
    strip_mode: StripMode,
    column_styles: Option<&'a crate::column_css::ColumnStyleTable>,
) -> PaginationGeometryTable {
    let mut tree = PaginationLayoutTree::new(doc, page_height_px);
    tree.strip_mode = strip_mode;
    tree.column_styles = column_styles;
    if tree.body_id.is_some() && page_height_px > 0.0 {
        tree.drive_taffy_root_layout();
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
            strip_mode: StripMode::MaxContent,
            column_styles: None,
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
    /// The available space we hand Taffy is the body's *existing* layout
    /// width and an unbounded height (`AvailableSpace::MaxContent`). We
    /// pass MaxContent rather than `page_height_px` because the current
    /// fragmenter still relies on the children's natural `final_layout`
    /// heights — restricting `available_space.height` here would let
    /// Taffy clip or shrink children, breaking the measurement walk.
    /// Constraining the strip is the next iteration's job, alongside a
    /// child-by-child `compute_child_layout` re-invocation pattern.
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

        let avail_height = match self.strip_mode {
            StripMode::MaxContent => AvailableSpace::MaxContent,
            StripMode::Definite => AvailableSpace::Definite(self.page_height_px.max(1.0)),
        };
        // Constrained mode invalidates the cache so Taffy actually re-
        // runs body's layout against the new available_space rather
        // than returning the cached MaxContent result. Unconstrained
        // mode is happy to read whatever Blitz already cached.
        if matches!(self.strip_mode, StripMode::Definite) {
            self.cache_clear(nid);
        }
        let avail = Size {
            width: AvailableSpace::Definite(prior_unrounded.size.width.max(1.0)),
            height: avail_height,
        };
        taffy::compute_root_layout(self, nid, avail);

        // Restore the body's location so downstream readers (convert,
        // paginate) see the same coordinates Blitz's first pass set.
        if let Some(node) = self.doc.get_node_mut(body_id) {
            node.unrounded_layout.location = prior_unrounded.location;
            node.final_layout.location = prior_final.location;
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

        let children = self
            .doc
            .get_node(body_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();

        let mut page_index: u32 = 0;
        let mut cursor_y: f32 = 0.0;
        let mut emitted = 0usize;

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
            let layout = child.final_layout;
            let child_h = layout.size.height;
            let child_w = if layout.size.width > 0.0 {
                layout.size.width
            } else {
                body_w
            };
            if child_h <= 0.0 {
                continue;
            }

            // fulgur-k0g0: read break-before / break-after / break-inside
            // for this child from the column-style side-table (shared with
            // multicol). Default `Auto` for nodes the table does not cover.
            let break_props = self
                .column_styles
                .and_then(|t| t.get(&child_id))
                .copied()
                .unwrap_or_default();

            // `break-before: page` forces a page boundary before the
            // child whenever there is in-flow content already placed on
            // the current page. A leading break-before on a fresh page
            // is a no-op (CSS 3 Fragmentation §3 collapses it).
            if matches!(
                break_props.break_before,
                Some(crate::pageable::BreakBefore::Page)
            ) && cursor_y > 0.0
            {
                page_index += 1;
                cursor_y = 0.0;
            }

            let avoid_inside = matches!(
                break_props.break_inside,
                Some(crate::pageable::BreakInside::Avoid)
            );

            // fulgur-p55h: if the child carries a Parley inline layout,
            // probe its line metrics and split at line boundaries —
            // mirrors `paragraph::ParagraphPageable::split` (line 945)
            // but inside the Taffy hook rather than post-conversion.
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
                if matches!(
                    break_props.break_after,
                    Some(crate::pageable::BreakAfter::Page)
                ) {
                    page_index += 1;
                    cursor_y = 0.0;
                }
                continue;
            }

            // Block fallback: child overflows the current page strip →
            // advance to the next page first. A child taller than a
            // full page is still emitted whole on its starting page.
            // (`break-inside: avoid` already collapses to this path
            // via `avoid_inside` above — it just suppresses the inline
            // split branch; the remaining-strip overflow handling is
            // identical.)
            if cursor_y > 0.0 && cursor_y + child_h > self.page_height_px {
                page_index += 1;
                cursor_y = 0.0;
            }

            let frag = Fragment {
                page_index,
                x: body_x,
                y: cursor_y,
                width: child_w,
                height: child_h,
            };
            self.geometry
                .entry(child_id)
                .or_default()
                .fragments
                .push(frag);

            cursor_y += child_h;
            emitted += 1;

            // `break-after: page` forces a page boundary after the
            // child. A trailing break on the last in-flow child does
            // emit an empty trailing page in CSS, but the spike's
            // observable signal (page_count) treats this as "advance
            // cursor"; the next iteration's emit-or-skip handles
            // whether the page is materialised.
            if matches!(
                break_props.break_after,
                Some(crate::pageable::BreakAfter::Page)
            ) {
                page_index += 1;
                cursor_y = 0.0;
            }
        }

        emitted
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

/// fulgur-p55h: split a multi-line inline root across page boundaries
/// at line edges, append one Fragment per page span to the geometry
/// table, and return the updated `(page_index, cursor_y, fragments_emitted)`.
///
/// Mirrors `paragraph::ParagraphPageable::split` (paragraph.rs:945+):
/// walk lines, track the first line of the current fragment in
/// `fragment_start_idx`, and split when the cumulative height in
/// paragraph-local coords would push the bottom past
/// `page_height_px - paragraph_top_in_body`.
///
/// Output:
///
/// - On a single-page paragraph (no overflow), one Fragment is appended
///   covering all lines. `cursor_y` advances by the paragraph's natural
///   height.
/// - On a multi-page paragraph, one Fragment per page is appended. The
///   final `cursor_y` is the height consumed on the last page (lines
///   ending on a partial page leave room for a following sibling).
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
    if line_metrics.is_empty() {
        return (initial_page_index, initial_cursor_y, 0);
    }

    let mut page_index = initial_page_index;
    let mut paragraph_top_in_body = initial_cursor_y;
    let mut fragment_start_idx: usize = 0;
    let mut emitted = 0usize;

    for (i, &(_line_top_local, line_bottom_local)) in line_metrics.iter().enumerate() {
        let frag_top_local = line_metrics[fragment_start_idx].0;
        let projected_bottom_in_body = paragraph_top_in_body + (line_bottom_local - frag_top_local);

        if projected_bottom_in_body > page_height_px && i > fragment_start_idx {
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

/// fulgur-6tco: walk the geometry table page-by-page to thread
/// `string-set` state across pages, mirroring
/// [`crate::paginate::collect_string_set_states`].
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
/// emit the marker. This matches Pageable's invariant that
/// `StringSetWrapperPageable.markers` "always travel with the content
/// they describe" (`paginate.rs:91-96`) — markers attach to the
/// first split fragment.
///
/// Source-order assumption: `geometry` is a `BTreeMap<usize, ..>` so
/// iteration is by ascending NodeId. For body's direct children that
/// matches DOM source order, since Blitz allocates ids sequentially
/// during parse. Nested string-set declarations (markers attached to
/// a `<span>` inside a `<p>`) are not in the spike's geometry table
/// today and so are silently dropped — same scope limitation as
/// `fragment_pagination_root` itself.
pub fn collect_string_set_states(
    geometry: &PaginationGeometryTable,
    string_set_by_node: &BTreeMap<usize, Vec<(String, String)>>,
) -> Vec<BTreeMap<String, crate::paginate::StringSetPageState>> {
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

    let mut result: Vec<BTreeMap<String, crate::paginate::StringSetPageState>> =
        Vec::with_capacity(nodes_per_page.len());
    let mut carry: BTreeMap<String, String> = BTreeMap::new();

    for nodes in &nodes_per_page {
        let mut page_state: BTreeMap<String, crate::paginate::StringSetPageState> = BTreeMap::new();
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

/// fulgur-jkl5: enumerate `position: fixed` elements and emit one
/// fragment per page so downstream rendering can repeat them on every
/// page (Chrome-compatible behaviour for paged media — see WPT
/// fixedpos-* family).
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
/// the inherited (often wrong) abs-position layout. The spike branch
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
/// `blitz_adapter::relayout_position_fixed` (branch
/// `feat/fixedpos-viewport-cb`). Visits raw `node.children` rather
/// than `layout_children` because the latter may be invalidated by
/// the time this runs, and pseudo-elements (`::before` / `::after`)
/// live in `node.before` / `node.after` outside the children vec.
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
/// Convention matches `compare_with_pageable::spike_page_count`:
/// returns `max(page_index) + 1` if the table has any fragments, else
/// `1` (Pageable's "always at least one page" guarantee). Exposed as
/// a helper so callers that need to thread `total_pages` into
/// [`append_position_fixed_fragments`] can do so without re-computing.
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

/// Custom layout dispatch for the body (the spike's fragmentation root).
///
/// Mirrors the structure of [`crate::multicol_layout::compute_multicol_layout`]:
/// the wrapper's `compute_child_layout` fires for body, delegates the
/// real layout to `BaseDocument` (so children's `final_layout` is
/// populated correctly), then post-walks body's direct children and
/// records fragments in the geometry side-table.
///
/// In the next iteration this is where per-strip available_space
/// constraint and child-by-child re-layout will live. For the current
/// spike it's a thin shim that proves the dispatch path works.
fn compute_pagination_layout(
    tree: &mut PaginationLayoutTree<'_>,
    body_id: NodeId,
    inputs: taffy::tree::LayoutInput,
) -> taffy::LayoutOutput {
    // Delegate the actual layout work to BaseDocument so children get
    // their normal natural sizes. The output is body's full natural
    // height — that height is what `convert::dom_to_pageable` already
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

    /// Parse helper for the spike's tests.
    ///
    /// We deliberately don't accept a viewport height: `blitz_adapter::parse`
    /// uses a hardcoded viewport_h internally, and the spike's strip slicing
    /// is driven by the `page_height_px` argument to `run_pass` rather than
    /// by the viewport. The fixtures pass viewport_w only.
    fn parse(html: &str, viewport_w: f32) -> blitz_html::HtmlDocument {
        let fonts: Vec<Arc<Vec<u8>>> = Vec::new();
        let mut doc = blitz_adapter::parse(html, viewport_w, &fonts);
        blitz_adapter::resolve(&mut doc);
        doc
    }

    #[test]
    fn empty_document_emits_no_fragments() {
        let mut doc = parse("<html><body></body></html>", 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert!(
            table.is_empty(),
            "no children → no fragments, got {table:?}"
        );
    }

    #[test]
    fn html_only_input_still_paginates_synthesized_body() {
        // html5ever synthesizes `<body>` for any HTML input, so
        // `find_body_id` always succeeds in the parse pipeline. The
        // synthesized body contains no children, so the pass emits no
        // fragments — which is the behaviour Pageable produces for the
        // same input.
        let mut doc = parse("<html></html>", 600.0);
        let tree = PaginationLayoutTree::new(&mut doc, 800.0);
        assert!(tree.body_id.is_some(), "html5ever should synthesize a body");
        let table = run_pass(&mut doc, 800.0);
        assert!(table.is_empty(), "empty body → no fragments, got {table:?}");
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
        assert_eq!(table.len(), 3, "expected 3 entries, got {}", table.len());
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
        assert_eq!(table.len(), 2);
        let pages: Vec<u32> = table.values().map(|g| g.fragments[0].page_index).collect();
        assert_eq!(
            pages,
            vec![0, 1],
            "first child page 0, second child page 1, got {pages:?}"
        );
    }

    /// fulgur-6tco: synthesize a geometry table + string_set_by_node
    /// map and verify `collect_string_set_states` produces the same
    /// per-page state shape Pageable's `paginate::collect_string_set_states`
    /// produces for an equivalent Pageable tree.
    #[test]
    fn string_set_state_carries_across_pages() {
        use crate::paginate::StringSetPageState;

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
        use crate::paginate::StringSetPageState;

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

    #[test]
    fn taller_than_page_block_emits_single_oversize_fragment() {
        // 1000px block on a 800px page. Block-only spike emits it whole
        // on the page where its top lands, with the full height — true
        // split is the next iteration's job.
        let html = r#"
            <html><body>
              <div style="height: 1000px"></div>
            </body></html>
        "#;
        let mut doc = parse(html, 600.0);
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(table.len(), 1);
        let geom = table.values().next().unwrap();
        assert_eq!(geom.fragments.len(), 1);
        assert_eq!(geom.fragments[0].page_index, 0);
        assert!(
            (geom.fragments[0].height - 1000.0).abs() < 1.0,
            "expected ~1000, got {}",
            geom.fragments[0].height
        );
    }
}

/// Comparison harness: drive the same HTML through `paginate::paginate(...)`
/// and `pagination_layout::run_pass(...)` and tabulate the per-fixture
/// page count agreement. The harness is observational — its purpose is to
/// surface where the two paths agree (so the spike can claim it covers
/// the simple-block case) and where they diverge (so the next iteration
/// of the spike has a concrete target list).
///
/// Lives in the same file as the unit tests so it can use `pub(crate)`
/// helpers like `crate::convert::pt_to_px` and `crate::paginate::paginate`
/// directly without touching the public surface.
#[cfg(test)]
mod compare_with_pageable {
    use crate::convert::pt_to_px;
    use crate::paginate::paginate;
    use crate::{Engine, PageSize};
    use std::ops::DerefMut;

    /// Compute the spike's page count from a geometry table.
    ///
    /// Thin wrapper over [`super::implied_page_count`] so the comparison
    /// harness and the production-facing helper can never drift apart on
    /// the "empty → 1" convention.
    fn spike_page_count(table: &super::PaginationGeometryTable) -> u32 {
        super::implied_page_count(table)
    }

    /// Run the engine's testing helper to build a `Pageable` for `html`,
    /// paginate it at the engine's content size, and return the page
    /// count. Uses A4 portrait with default margins so the test is stable
    /// across machines.
    fn pageable_page_count(html: &str) -> usize {
        let engine = Engine::builder().page_size(PageSize::A4).build();
        let pageable = engine.build_pageable_for_testing_no_gcpm(html);
        let cfg = engine.config();
        let pages = paginate(pageable, cfg.content_width(), cfg.content_height());
        pages.len()
    }

    /// Run the spike against the same HTML the Pageable side rendered.
    /// Re-parses (deterministic) so we get a fresh `BaseDocument` and
    /// can mutate it without unsafe shenanigans.
    fn spike_page_count_for(html: &str) -> u32 {
        spike_page_count_with_mode(html, super::StripMode::MaxContent)
    }

    /// fulgur-ik6o probe: re-run the spike with
    /// `AvailableSpace::Definite(page_height_px)` instead of
    /// `MaxContent`. Returns the spike's page count under that mode
    /// for comparison.
    fn spike_page_count_constrained(html: &str) -> u32 {
        spike_page_count_with_mode(html, super::StripMode::Definite)
    }

    fn spike_page_count_with_mode(html: &str, mode: super::StripMode) -> u32 {
        use crate::blitz_adapter;
        let engine = Engine::builder().page_size(PageSize::A4).build();
        let cfg = engine.config();
        let (mut doc, _gcpm) = blitz_adapter::parse_html_with_local_resources(
            html,
            pt_to_px(cfg.content_width()),
            pt_to_px(cfg.page_height()) as u32,
            &[],
            None,
        );
        blitz_adapter::resolve(&mut doc);
        let column_styles = blitz_adapter::extract_column_style_table(&doc);
        let _multicol = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);
        // fulgur-k0g0: thread the column-style side-table so the
        // fragmenter sees break-before / break-after / break-inside
        // for fixtures that set them. The constrained-mode probe
        // (fulgur-ik6o) does not yet honour break rules — that's
        // intentional, the probe exists to compare available_space
        // modes only.
        let table = match mode {
            super::StripMode::MaxContent => super::run_pass_with_break_styles(
                doc.deref_mut(),
                pt_to_px(cfg.content_height()),
                &column_styles,
            ),
            super::StripMode::Definite => {
                super::run_pass_constrained(doc.deref_mut(), pt_to_px(cfg.content_height()))
            }
        };
        spike_page_count(&table)
    }

    /// Each fixture: (label, html, agreement expected?).
    ///
    /// `agreement_expected = false` means we already know Pageable and
    /// the block-only spike will diverge for this case (e.g. inline text
    /// that wraps across pages). The harness still runs both sides and
    /// records the disagreement so the next iteration knows what to fix.
    fn fixtures() -> Vec<(&'static str, &'static str, bool)> {
        vec![
            (
                "empty body → 1 page on both sides",
                "<html><body></body></html>",
                true,
            ),
            (
                "three short blocks fit one page",
                r#"<html><body>
                    <div style="height: 100px"></div>
                    <div style="height: 100px"></div>
                    <div style="height: 100px"></div>
                </body></html>"#,
                true,
            ),
            (
                "two blocks split across two pages",
                // Each block is 600px = 450pt, so two stack to 900pt
                // which exceeds A4 portrait content height (~770pt).
                r#"<html><body>
                    <div style="height: 600px"></div>
                    <div style="height: 600px"></div>
                </body></html>"#,
                true,
            ),
            (
                "five blocks span three pages",
                // A4 portrait content height ≈ 1027 CSS px (770pt).
                // 5 × 400px = 2000px stacks as: 400 → 800 → break (1200 >
                // 1027), then 400 → 800 → break, then 400 — three pages
                // on both sides. The harness only checks page-count
                // agreement, not the predicted breakdown.
                r#"<html><body>
                    <div style="height: 400px"></div>
                    <div style="height: 400px"></div>
                    <div style="height: 400px"></div>
                    <div style="height: 400px"></div>
                    <div style="height: 400px"></div>
                </body></html>"#,
                true,
            ),
            // ── vh / percentage observation fixtures ──────────────────
            //
            // These probe the cases the block-only spike is *expected* to
            // disagree with Pageable on. Marking them
            // `expected_agreement = false` documents the divergence; once
            // the spike grows mid-element splitting, flip these to `true`
            // and use the test as a regression gate.
            (
                "single 100vh div (taller than content area)",
                // A4: viewport height = page height (842pt = 1122 CSS
                // px), but content height = 770pt = 1027 CSS px. A 100vh
                // box is 1122px, ~95px taller than one page strip.
                // Surprise: both report 1 page. Pageable's
                // `BlockPageable::split` returns `Err(unsplit)` for a
                // body whose only child is a single oversized empty
                // box (no break point inside, refusing to emit an empty
                // page first), and the spike's `cursor_y > 0` guard does
                // the same on its side. Convergent fallback rather than
                // shared correctness — flagged here to remember.
                r#"<html><body>
                    <div style="height: 100vh"></div>
                </body></html>"#,
                true,
            ),
            (
                "two 50vh divs sum to 100vh (overflows content area)",
                // 2 × 50vh = 1122 CSS px (same total as 100vh). Spike:
                // first 50vh = 561 px (cursor 0 → 561, no break since
                // cursor was 0). Second 50vh would push cursor to 1122
                // > 1027 → break, second block on page 1. Total 2 pages.
                // Pageable should produce 2 pages too because the second
                // block doesn't fit. Expected: agreement at 2 pages.
                r#"<html><body>
                    <div style="height: 50vh"></div>
                    <div style="height: 50vh"></div>
                </body></html>"#,
                true,
            ),
            (
                "one 1028px div (just over content area)",
                // 1px taller than A4 content (~1027 CSS px). Same
                // convergent fallback as the 100vh case — both sides
                // report 1 page because neither will split a single
                // oversized empty first child (no inner break point on
                // an empty `<div>`, no useful split). Recorded to
                // confirm the 1px-over case behaves the same as the
                // 95px-over `100vh` case.
                r#"<html><body>
                    <div style="height: 1028px"></div>
                </body></html>"#,
                true,
            ),
            (
                "nested 100% height with no parent height resolves to zero",
                // CSS 2.1 §10.5: percentage height resolves to `auto`
                // when the containing block has no explicit height. Both
                // sides should produce a single empty page (the divs
                // collapse). The spike's measurement walk reads
                // final_layout, so it sees the same zero-height boxes
                // Pageable converts. Expected: both report 1 page.
                r#"<html><body>
                    <div style="height: 100%">
                        <div style="height: 100%"></div>
                    </div>
                </body></html>"#,
                true,
            ),
            (
                "long paragraph wraps into multiple pages",
                // fulgur-p55h: the spike now probes Parley's line
                // metrics (`Layout::lines()` → `LineMetrics`) and
                // splits inline roots at line boundaries via
                // `fragment_inline_root`. This fixture flipped from
                // `expected_agreement = false` to `true` once the
                // inline-aware path landed — leaving it as a
                // regression gate for future changes.
                // 50px font-size + line-height 1.5 → ~75 px per line.
                // Lorem ipsum block wraps into ~70 lines at A4 content
                // width → ~5250 px total, comfortably overflowing 2+
                // pages. Pageable's `ParagraphPageable::split` (line
                // boundaries) should split it across pages. The spike's
                // `fragment_pagination_root` sees one block child with
                // a 5000+ px height and emits it whole → 1 page.
                r#"<html><body><p style="font-size: 50px; line-height: 1.5">
                    Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed
                    do eiusmod tempor incididunt ut labore et dolore magna
                    aliqua. Ut enim ad minim veniam, quis nostrud exercitation
                    ullamco laboris nisi ut aliquip ex ea commodo consequat.
                    Duis aute irure dolor in reprehenderit in voluptate velit
                    esse cillum dolore eu fugiat nulla pariatur. Excepteur sint
                    occaecat cupidatat non proident, sunt in culpa qui officia
                    deserunt mollit anim id est laborum.
                </p></body></html>"#,
                true,
            ),
            (
                "small lead block then oversized block forces page break",
                // 100px small + 1100px tall block. Spike: cursor 0 → 100,
                // then 100+1100=1200 > 1027 → break, oversized block
                // emits whole on page 1. Total 2 pages. Pageable behaves
                // similarly: small fits page 0, oversized doesn't fit
                // remaining 927px so it pushes to page 1. Expected
                // agreement at 2 pages.
                r#"<html><body>
                    <div style="height: 100px"></div>
                    <div style="height: 1100px"></div>
                </body></html>"#,
                true,
            ),
            // ── fulgur-k0g0: break-before / break-after / break-inside ─
            (
                "break-before: page forces page boundary",
                // Two 100px blocks, second has break-before: page. Spike:
                // first block on page 0, second forced onto page 1 even
                // though both could fit one page. Pageable does the same
                // via paginate.rs split-on-break-before. Expected: 2.
                r#"<html><head><style>
                    .b { break-before: page; }
                </style></head><body>
                    <div style="height: 100px"></div>
                    <div class="b" style="height: 100px"></div>
                </body></html>"#,
                true,
            ),
            (
                "break-after: page forces page boundary",
                // First block has break-after: page → second pushed to
                // page 1. Same observable effect as break-before on the
                // second block; this fixture exercises the
                // post-emission branch in fragment_pagination_root.
                r#"<html><head><style>
                    .a { break-after: page; }
                </style></head><body>
                    <div class="a" style="height: 100px"></div>
                    <div style="height: 100px"></div>
                </body></html>"#,
                true,
            ),
            (
                "break-inside: avoid keeps tall paragraph whole",
                // Intentional divergence: the spike's
                // `fragment_pagination_root` checks `break-inside:
                // avoid` *before* entering the inline-split branch and
                // emits the paragraph as a single oversized block →
                // 1 page. Pageable's `ParagraphPageable::split`
                // (paragraph.rs:945) does NOT check `break_inside`, so
                // it splits at line boundaries regardless → 2 pages.
                // The spike behaviour is correct per CSS Fragmentation
                // §3.3; Pageable has a latent bug here. Tracking the
                // Pageable fix is out of scope for this spike — file
                // separately if it matters.
                r#"<html><body><p style="font-size: 50px; line-height: 1.5; break-inside: avoid">
                    Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed
                    do eiusmod tempor incididunt ut labore et dolore magna
                    aliqua. Ut enim ad minim veniam, quis nostrud exercitation
                    ullamco laboris nisi ut aliquip ex ea commodo consequat.
                    Duis aute irure dolor in reprehenderit in voluptate velit
                    esse cillum dolore eu fugiat nulla pariatur. Excepteur sint
                    occaecat cupidatat non proident, sunt in culpa qui officia
                    deserunt mollit anim id est laborum.
                </p></body></html>"#,
                false,
            ),
        ]
    }

    #[test]
    fn page_count_agreement_table() {
        let fixtures = fixtures();
        let mut disagreements: Vec<String> = Vec::new();

        for (label, html, expected_agreement) in &fixtures {
            let pageable_pages = pageable_page_count(html) as u32;
            let spike_pages = spike_page_count_for(html);
            let agree = pageable_pages == spike_pages;

            eprintln!(
                "[{:>1}] {label:<55} pageable={pageable_pages} spike={spike_pages}",
                if agree { "✓" } else { "✗" },
            );

            if agree != *expected_agreement {
                disagreements.push(format!(
                    "{label}: pageable={pageable_pages} spike={spike_pages} expected_agreement={expected_agreement}",
                ));
            }
        }

        assert!(
            disagreements.is_empty(),
            "Pageable vs spike disagreement (or unexpected agreement) for:\n  - {}",
            disagreements.join("\n  - "),
        );
    }

    /// fulgur-ik6o probe: tabulate page counts under both
    /// `StripMode::MaxContent` and `StripMode::Definite` for every
    /// fixture, printed via `eprintln!` for human inspection. The test
    /// itself only asserts that the constrained-mode pass does not
    /// panic, so this serves as observation without locking in expected
    /// outcomes — those go into the design doc once we know what
    /// `AvailableSpace::Definite(page_height_px)` actually does.
    #[test]
    fn strip_mode_observation_table() {
        let fixtures = fixtures();
        eprintln!(
            "┌ fixture ──────────────────────────────────────────── pageable max_content definite"
        );
        for (label, html, _) in &fixtures {
            let pageable_pages = pageable_page_count(html) as u32;
            let max_content = spike_page_count_for(html);
            let definite = spike_page_count_constrained(html);
            eprintln!("│ {label:<55} {pageable_pages:>9} {max_content:>11} {definite:>9}",);
        }
        eprintln!(
            "└────────────────────────────────────────────────────────────────────────────────"
        );
    }
}
