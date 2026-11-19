//! Taffy custom layout hook for CSS Multi-column Layout.
//!
//! [`FulgurLayoutTree`] wraps a [`blitz_dom::BaseDocument`] as a Taffy
//! `LayoutPartialTree`, intercepts multicol containers, and routes them
//! through [`compute_multicol_layout`]. Direct children are partitioned
//! by `column-span: all` into alternating `ColumnGroup` / `SpanAll`
//! segments: columnar segments run through `layout_column_group`
//! (balance distribution at `col_w`), and `SpanAll` segments occupy the
//! full container width and stack vertically between column groups.
//!
//! Everything else delegates to `BaseDocument`'s built-in dispatch. The
//! pattern follows blitz's own
//! [`blitz_dom::BaseDocument::compute_inline_layout`], where Parley is
//! wired into Taffy via `compute_leaf_layout`; multicol uses the same
//! mechanism one layer up.

use crate::blitz_adapter::NodeLayoutExt;
use crate::units::F32Units;
use blitz_dom::{BaseDocument, LayoutPassState};
use std::collections::BTreeMap;
use taffy::{
    AvailableSpace, CacheTree, CollapsibleMarginSet, CoreStyle, LayoutContainingBlock,
    LayoutPartialTree, Line, NodeId, Point, RequestedAxis, ResolveOrZero, RoundTree, RunMode, Size,
    SizingMode, TraversePartialTree, TraverseTree,
};

/// Per-column slice of an inline-root paragraph distributed across
/// columns by `layout_column_group`.
///
/// `line_range.is_empty()` is the **placeholder marker**: the entry was
/// padded into `ParagraphSplitEntry::column_slices` (self-inline-root
/// containers keep one entry per column), and the column receives no
/// content from this paragraph. In that case `origin` and `size` are
/// both zero (`Default` values) — consumers MUST check
/// `line_range.is_empty()` before reading them. For non-empty slices,
/// `origin` is in the segment-relative content-box frame (same frame
/// as `ColumnGroupGeometry::col_heights`); absolute coordinates are
/// `(group.x_offset + origin.x, group.y_offset + origin.y)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColumnLineSlice {
    /// Half-open parley line index range. Indices reference the parley
    /// layout reachable through `source_node_id`'s `inline_layout_data`.
    /// Empty range = placeholder slot; see the type-level doc.
    pub line_range: std::ops::Range<usize>,
    /// Slice top-left in CSS pixels, relative to the column group
    /// segment (matches `ColumnGroupGeometry::col_heights`'s frame).
    /// Zero for placeholder entries — gate on `line_range.is_empty()`.
    pub origin: taffy::Point<crate::units::Px>,
    /// Slice size — `col_w × Σ line_height(line_range)`. CSS pixels.
    /// Zero for placeholder entries.
    pub size: taffy::Size<crate::units::Px>,
}

/// Plan for one paragraph distributed across `ColumnGroupGeometry`'s
/// columns. From `layout_column_group`, `column_slices` holds one slice
/// per column the paragraph reaches, in line order (columns of later rows,
/// when the column set breaks across pages, included). From
/// `layout_self_inline_root_container` it holds one entry per column,
/// with empty `line_range` for unused columns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParagraphSplitEntry {
    /// DOM `usize` NodeId whose `inline_layout_data` was sliced. In Case A
    /// this equals the multicol container's own NodeId; in Case B it's
    /// the inline-root child's NodeId.
    pub source_node_id: usize,
    pub column_slices: Vec<ColumnLineSlice>,
}

/// Per-`ColumnGroup` geometry recorded by the Taffy multicol hook.
///
/// `layout_column_group` builds one of these every time it balances a run of
/// columnar children. Consumers use the geometry to paint `column-rule`
/// lines between adjacent non-empty columns without re-running layout: the
/// rule at gutter `i ↔ i+1` starts at `y_offset` and extends
/// `min(col_heights[i], col_heights[i+1])` downward.
///
/// Conventions:
///
/// - `y_offset` is measured from the multicol container's content box top,
///   not from the page or the viewport.
/// - `col_heights` always has length `n`; an entry is `0.0` when the column
///   contains no placements.
/// - `col_w` and `gap` are in CSS pixels (Taffy's native unit), matching the
///   rest of the multicol hook.
/// - `x_offset` / `y_offset` are relative to the multicol container's
///   **border-box** top-left. They already include the container's own
///   padding + border.
#[derive(Clone, Debug, Default)]
pub struct ColumnGroupGeometry {
    /// Horizontal offset from the container's border-box left to column 0's
    /// left edge. Equals the container's (padding-left + border-left).
    pub x_offset: crate::units::Px,
    /// Vertical offset from the container's border-box top to this group's
    /// top. Includes the container's (padding-top + border-top); subsequent
    /// groups accumulate prior segment heights on top of that.
    pub y_offset: crate::units::Px,
    /// Width of a single column, in CSS pixels.
    pub col_w: crate::units::Px,
    /// Horizontal gap between adjacent columns, in CSS pixels.
    pub gap: crate::units::Px,
    /// Number of columns this group balances across.
    pub n: u32,
    /// Per-column filled height. `col_heights[i]` is the bottom-most
    /// placement's `(location.y + size.height)` minus `y_offset` for column
    /// `i`, clamped to `>= 0.0`. An entry of `0.0` means column `i` received
    /// no placements. Always has length == `n`.
    pub col_heights: Vec<crate::units::Px>,
    /// Inline-root paragraphs distributed across the columns of this
    /// group. Empty when no child paragraph needed splitting.
    pub paragraph_splits: Vec<ParagraphSplitEntry>,
    /// Boxes with a border, padding or background that break across the
    /// columns of this group, in node order.
    pub box_splits: Vec<BoxSplitEntry>,
}

/// A box with a border, padding or background broken across columns. It is
/// painted as one box `height` tall, sliced: each fragment shows the part
/// of the box from `offset` down, clipped to the fragment's rectangle
/// (`box-decoration-break: slice`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoxSplitEntry {
    pub source_node_id: usize,
    /// Sum of the fragments' heights.
    pub height: crate::units::Px,
    pub fragments: Vec<BoxFragment>,
}

/// One column's part of a [`BoxSplitEntry`]. `origin` is in the same frame
/// as [`ColumnLineSlice::origin`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoxFragment {
    pub origin: taffy::Point<crate::units::Px>,
    pub size: taffy::Size<crate::units::Px>,
    /// Height of the box above this fragment (in earlier columns).
    pub offset: crate::units::Px,
}

/// Full per-container multicol geometry record.
///
/// One `ColumnGroupGeometry` per `Segment::ColumnGroup` in source order.
/// `SpanAll` segments do not contribute geometry because they span the full
/// container width — there are no inter-column gutters to draw a rule in.
#[derive(Clone, Debug, Default)]
pub struct MulticolGeometry {
    pub groups: Vec<ColumnGroupGeometry>,
}

/// Side-table mapping multicol container `usize` NodeIds to their geometry.
///
/// Keyed by raw `usize` (same convention as
/// [`crate::column_css::ColumnStyleTable`]) so convert-side lookups can use
/// the DOM node id directly. `BTreeMap`, not `HashMap`, because iteration
/// order may feed into PDF byte order downstream.
pub type MulticolGeometryTable = BTreeMap<usize, MulticolGeometry>;

/// One top-level slice of a multicol container's flattened child list.
///
/// Children are partitioned by `column-span: all` **on direct children
/// only** — nested descendants with `column-span: all` deep inside a
/// non-span child are ignored (CSS Multi-column Level 1).
#[derive(Debug)]
pub(crate) enum Segment {
    /// Children that participate in the balanced column grid.
    ColumnGroup(Vec<NodeId>),
    /// A single child that spans the full container width.
    SpanAll(NodeId),
}

/// Walk the direct children of `container_id`, grouping runs of non-span
/// children into `Segment::ColumnGroup` and emitting `Segment::SpanAll`
/// for each direct child carrying `column-span: all`.
pub(crate) fn partition_children_into_segments(
    doc: &BaseDocument,
    container_id: usize,
) -> Vec<Segment> {
    // Layout children, when Blitz built them: text and inline content
    // mixed with blocks sits in anonymous block wrappers there, which have
    // the styles and layout data the column layout reads. The bare text
    // nodes in the DOM list have neither.
    let children: Vec<usize> = doc
        .get_node(crate::blitz_adapter::bid(container_id))
        .map(|n| {
            let layout_children = n.layout_children.borrow();
            crate::blitz_adapter::uids(
                layout_children
                    .as_deref()
                    .filter(|v| !v.is_empty())
                    .unwrap_or(&n.children),
            )
        })
        .unwrap_or_default();

    let mut out: Vec<Segment> = Vec::new();
    let mut group: Vec<NodeId> = Vec::new();
    for child_id in children {
        let Some(child_node) = doc.get_node(crate::blitz_adapter::bid(child_id)) else {
            continue;
        };
        // Skip text nodes that are entirely whitespace — they're HTML
        // pretty-printing noise with no layout content. Non-whitespace
        // text nodes (real inline content between block children) stay in
        // the ColumnGroup so they are not silently dropped.
        if let Some(text) = child_node.text_data()
            && text.content.chars().all(char::is_whitespace)
        {
            continue;
        }
        // `column-span: all` only applies to element children; non-elements
        // (text content, comments) flow through as ordinary members of the
        // current ColumnGroup so their layout is preserved.
        let is_span = child_node.element_data().is_some()
            && crate::blitz_adapter::has_column_span_all(child_node);
        if is_span {
            if !group.is_empty() {
                out.push(Segment::ColumnGroup(std::mem::take(&mut group)));
            }
            out.push(Segment::SpanAll(NodeId::from(child_id)));
        } else {
            group.push(NodeId::from(child_id));
        }
    }
    if !group.is_empty() {
        out.push(Segment::ColumnGroup(group));
    }
    out
}

/// Taffy tree wrapper around a `BaseDocument` that intercepts multicol
/// containers and routes them through fulgur's own layout.
///
/// `column_styles` carries the Phase A `column-*` side-table harvested by
/// [`crate::blitz_adapter::extract_column_style_table`] — the multicol
/// layout branch will read `column-fill` from it to switch between balanced
/// and greedy ("auto") column filling. It is borrowed (rather than owned)
/// because the tree lives for a single layout pass; the engine keeps the
/// owning `ColumnStyleTable` alive across both this pass and the subsequent
/// convert pass.
pub struct FulgurLayoutTree<'a> {
    /// Blitz's layout pass over the document. Derefs to `BaseDocument`, and
    /// carries Blitz's Taffy tree impls that this wrapper delegates to.
    pub(crate) doc: LayoutPassState<'a>,
    // Read by `compute_multicol_layout` to resolve `column-fill` before
    // delegating to `layout_column_group`. Populated by the engine via
    // `extract_column_style_table` before the multicol pass runs.
    pub(crate) column_styles: &'a crate::column_css::ColumnStyleTable,
    /// Per-container geometry populated by `compute_multicol_layout` as it
    /// balances each `ColumnGroup` segment. Owned (not borrowed) because the
    /// table is produced during this layout pass and must outlive the tree —
    /// callers drain it via [`FulgurLayoutTree::take_geometry`] and thread
    /// it into the convert pipeline. `BTreeMap` keeps iteration order
    /// deterministic across runs.
    pub(crate) geometry: MulticolGeometryTable,
    /// Containers that break across pages, and where they start: their
    /// column sets are filled page by page (see [`PagedStart`]).
    pub(crate) paged: BTreeMap<usize, PagedStart>,
}

/// Where a multicol container that breaks across pages starts. Its column
/// sets then fill each page's columns to the page end, and only the last
/// page's columns are balanced (CSS Multi-column 1 §8.1, `column-fill`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PagedStart {
    /// Page content height, in CSS px.
    pub page_h: f32,
    /// Distance from the top of the page's content area to the
    /// container's border-box top, in CSS px.
    pub top_in_page: f32,
}

/// The column rows of a column set that breaks across pages: the first
/// row ends `first_h` below the set's top, every later one is `page_h`
/// tall.
#[derive(Clone, Copy, Debug)]
struct Paging {
    first_h: f32,
    page_h: f32,
}

/// One-shot entry used by the render pipeline after `blitz_adapter::resolve`.
/// Runs the multicol Taffy hook on every multicol subtree in the document,
/// then returns the geometry table so downstream passes (convert → draw)
/// can paint `column-rule` lines without re-walking layout.
///
/// `column_styles` is the Phase A side-table harvested by
/// [`crate::blitz_adapter::extract_column_style_table`]. Callers that do not
/// need `column-fill` / `column-rule` resolution can pass an empty table
/// (e.g. test helpers — see the `FulgurLayoutTree::new` call sites in the
/// unit-test modules below) and simply drop the returned geometry.
pub fn run_pass(
    doc: &mut BaseDocument,
    column_styles: &crate::column_css::ColumnStyleTable,
) -> MulticolGeometryTable {
    let mut tree = FulgurLayoutTree::new(doc, column_styles);
    tree.layout_multicol_subtrees();
    tree.take_geometry()
}

/// Lay out again the multicol containers in `paged`, filling their columns
/// page by page from the given start. Other containers keep their layout.
/// Returns their new geometry.
pub fn run_paged_pass(
    doc: &mut BaseDocument,
    column_styles: &crate::column_css::ColumnStyleTable,
    paged: &BTreeMap<usize, PagedStart>,
) -> MulticolGeometryTable {
    let mut tree = FulgurLayoutTree::new(doc, column_styles);
    tree.paged = paged.clone();
    tree.layout_multicol_subtrees_in(Some(paged));
    tree.take_geometry()
}

impl<'a> FulgurLayoutTree<'a> {
    pub fn new(
        doc: &'a mut BaseDocument,
        column_styles: &'a crate::column_css::ColumnStyleTable,
    ) -> Self {
        Self {
            doc: LayoutPassState::new(doc),
            column_styles,
            geometry: BTreeMap::new(),
            paged: BTreeMap::new(),
        }
    }

    /// Drain the accumulated per-container geometry table.
    ///
    /// Uses `mem::take` so a second call on the same tree returns an empty
    /// table rather than double-counting (the tree keeps a `BTreeMap::new()`
    /// in place). Callers normally invoke this once after
    /// [`FulgurLayoutTree::layout_multicol_subtrees`] has finished.
    pub fn take_geometry(&mut self) -> MulticolGeometryTable {
        std::mem::take(&mut self.geometry)
    }

    /// Re-run Taffy layout for each multicol container in the tree.
    ///
    /// Intended to be called after blitz's `resolve()` has produced an
    /// initial (block-shaped) layout. We walk the tree to find every
    /// multicol container and, for each one:
    ///
    /// 1. Invoke [`taffy::compute_root_layout`] on the container's subtree
    ///    through our wrapper. That makes the multicol node the Taffy root
    ///    for its own layout pass, so our `compute_child_layout` sees it
    ///    first and dispatches to [`compute_multicol_layout`].
    /// 2. Compare the new container height against the blitz-assigned one
    ///    and propagate the delta up the ancestor chain so siblings
    ///    positioned after the multicol move with it.
    ///
    /// Inside-out order: nested multicol resolves before its outer
    /// container, so the outer pass sees post-inner sizes.
    ///
    /// Returns the number of multicol subtrees laid out.
    pub fn layout_multicol_subtrees(&mut self) -> usize {
        self.layout_multicol_subtrees_in(None)
    }

    /// [`Self::layout_multicol_subtrees`] restricted to the containers
    /// keyed in `only`, when given.
    fn layout_multicol_subtrees_in(&mut self, only: Option<&BTreeMap<usize, PagedStart>>) -> usize {
        let mut multicol_ids = collect_multicol_node_ids(&self.doc);
        if let Some(only) = only {
            multicol_ids.retain(|id| only.contains_key(id));
        }
        for id in multicol_ids.iter().rev() {
            let node_id = NodeId::from(*id);
            let prior_layout = self.doc.get_unrounded_layout(node_id);
            let prior_final = self
                .doc
                .get_node(crate::blitz_adapter::bid(*id))
                .map(|n| *n.final_layout_or_zero())
                .unwrap_or_default();
            let prior = prior_layout.size;
            let available_space = taffy::Size {
                width: AvailableSpace::Definite(prior.width),
                height: AvailableSpace::Definite(prior.height.max(1.0)),
            };
            clear_subtree_cache(&mut self.doc, *id);
            taffy::compute_root_layout(self, node_id, available_space);
            taffy::round_layout(self, node_id);

            // `compute_root_layout` resets the subtree root's `location`
            // to (0, 0) because it treats the node as a Taffy root. The
            // multicol is NOT a root in the full document tree; it sits
            // at the position blitz originally placed it. Restore that
            // position in both unrounded and final layouts.
            if let Some(node) = self.doc.get_node_mut(crate::blitz_adapter::bid(*id)) {
                if let Some(ld) = node.try_layout_data_mut() {
                    ld.unrounded_layout.location = prior_layout.location;
                    ld.final_layout.location = prior_final.location;
                }
            }

            let new_h = self.doc.get_unrounded_layout(node_id).size.height;
            let delta = new_h - prior.height;
            if delta.abs() > 0.01 {
                propagate_height_delta(&mut self.doc, *id, delta);
            }
        }
        multicol_ids.len()
    }

    fn is_multicol(&self, node_id: NodeId) -> bool {
        self.doc
            .get_node(crate::blitz_adapter::bid(usize::from(node_id)))
            .is_some_and(crate::blitz_adapter::is_multicol_container)
    }
}

// ── Trait delegation to Blitz's LayoutPassState ──────────────────────────

impl<'d> TraversePartialTree for FulgurLayoutTree<'d> {
    type ChildIter<'a>
        = <LayoutPassState<'d> as TraversePartialTree>::ChildIter<'a>
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

impl TraverseTree for FulgurLayoutTree<'_> {}

impl CacheTree for FulgurLayoutTree<'_> {
    fn cache_get(
        &self,
        node_id: NodeId,
        input: &taffy::LayoutInput,
    ) -> Option<taffy::LayoutOutput> {
        self.doc.cache_get(node_id, input)
    }

    fn cache_store(
        &mut self,
        node_id: NodeId,
        input: &taffy::LayoutInput,
        layout_output: taffy::LayoutOutput,
    ) {
        self.doc.cache_store(node_id, input, layout_output);
    }

    fn cache_clear(&mut self, node_id: NodeId) {
        self.doc.cache_clear(node_id);
    }
}

impl<'d> LayoutPartialTree for FulgurLayoutTree<'d> {
    type CoreContainerStyle<'a>
        = <LayoutPassState<'d> as LayoutPartialTree>::CoreContainerStyle<'a>
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
        if self.is_multicol(node_id) {
            return compute_multicol_layout(self, node_id, inputs);
        }
        // Delegate to blitz for everything else. Recursion inside blitz stays
        // within its own dispatch — nested multicol under an inline-root /
        // table / replaced subtree is not intercepted by this scaffold.
        self.doc.compute_child_layout(node_id, inputs)
    }
}

impl<'d> LayoutContainingBlock for FulgurLayoutTree<'d> {
    type OofItemStyle<'a>
        = <LayoutPassState<'d> as LayoutContainingBlock>::OofItemStyle<'a>
    where
        Self: 'a;

    fn get_oof_item_style(&self, node_id: NodeId) -> Self::OofItemStyle<'_> {
        self.doc.get_oof_item_style(node_id)
    }

    fn clear_hoisted_children(&mut self, node_id: NodeId) {
        self.doc.clear_hoisted_children(node_id);
    }

    fn add_hoisted_children(&mut self, node_id: NodeId, hoisted: &[NodeId]) {
        self.doc.add_hoisted_children(node_id, hoisted);
    }

    fn get_detailed_layout_info(&self, node_id: NodeId) -> &taffy::DetailedLayoutInfo<style::Atom> {
        self.doc.get_detailed_layout_info(node_id)
    }
}

impl RoundTree for FulgurLayoutTree<'_> {
    fn get_unrounded_layout(&self, node_id: NodeId) -> taffy::Layout {
        self.doc.get_unrounded_layout(node_id)
    }

    fn set_final_layout(&mut self, node_id: NodeId, layout: &taffy::Layout) {
        self.doc.set_final_layout(node_id, layout);
    }

    fn is_out_of_flow(&self, node_id: NodeId) -> bool {
        self.doc.is_out_of_flow(node_id)
    }

    fn hoisted_child_count(&self, node_id: NodeId) -> usize {
        self.doc.hoisted_child_count(node_id)
    }

    fn get_hoisted_child_id(&self, node_id: NodeId, index: usize) -> NodeId {
        self.doc.get_hoisted_child_id(node_id, index)
    }
}

/// Resolve the CSS `column-count` / `column-width` pair into a concrete
/// `(used_count, used_column_width)` for the given content width.
///
/// Spec reference: <https://drafts.csswg.org/css-multicol/#cw>.
pub fn resolve_column_layout(
    content_w: f32,
    count: Option<u32>,
    width: Option<f32>,
    gap: f32,
) -> (u32, f32) {
    // How many columns of width `w` (with `gap` between them) fit in `content_w`?
    let fits_count = |w: f32| -> u32 {
        let denom = w + gap;
        if denom > 0.0 {
            (((content_w + gap) / denom).floor() as u32).max(1)
        } else {
            1
        }
    };
    // Clamp to `[1, MAX_COLUMN_COUNT]` at this single choke point. The used
    // count `n` multiplies retained layout metadata downstream (`col_heights`,
    // per-split `column_slices`, `slice_lines_by_budget`), all of which read
    // this returned `n`; bounding it here bounds every sink at once and blocks
    // the unbounded-`column-count` memory-exhaustion DoS. Applies to both the
    // explicit-count and width-derived paths (every branch routes through
    // `capped`).
    let capped = |n: u32| -> (u32, f32) {
        let n = n.clamp(1, crate::MAX_COLUMN_COUNT);
        let col_w = ((content_w - gap * (n as f32 - 1.0)) / n as f32).max(0.0);
        (n, col_w)
    };

    let width = width.filter(|&w| w > 0.0);
    match (count, width) {
        (Some(n), None) => capped(n),
        (None, Some(w)) => capped(fits_count(w)),
        (Some(n), Some(w)) => capped(n.min(fits_count(w))),
        (None, None) => (1, content_w.max(0.0)),
    }
}

/// Main multicol layout entry.
///
/// Pipeline:
///
/// 1. Read `column-count` / `column-width` / `column-gap` from the node.
/// 2. Derive `(n, col_w)` from the container width.
/// 3. Partition direct element children into `ColumnGroup` / `SpanAll`
///    segments via `partition_children_into_segments` (nested
///    `column-span: all` inside a non-span child is ignored per CSS
///    Multi-column Level 1).
/// 4. For each segment, stacking vertically:
///    - `ColumnGroup`: delegate to `layout_column_group` at `col_w`
///      with greedy balance (auto fallback when content overflows
///      `avail_h * n`).
///    - `SpanAll`: lay out the child at `container_w` as a single block.
/// 5. Write each child's placement back via
///    [`LayoutPartialTree::set_unrounded_layout`] with the per-segment
///    width (`col_w` for column members, `container_w` for `SpanAll`).
/// 6. Return the container's total size (`container_w × cursor_y`).
pub fn compute_multicol_layout(
    tree: &mut FulgurLayoutTree<'_>,
    node_id: NodeId,
    inputs: taffy::tree::LayoutInput,
) -> taffy::LayoutOutput {
    // 1. MulticolProps
    let Some(props) = tree
        .doc
        .get_node(crate::blitz_adapter::bid(usize::from(node_id)))
        .and_then(crate::blitz_adapter::extract_multicol_props)
    else {
        // Dispatcher already matched, but be defensive.
        return tree.doc.compute_child_layout(node_id, inputs);
    };

    // 2. Container dimensions. `known_dimensions.width` from
    //    `compute_root_layout` is the **border-box** width — it already
    //    includes the multicol's own padding + border on both sides. We
    //    treat that as the node's outer width, read the prior resolved
    //    padding/border (written by blitz before the hook ran), and strip
    //    them off to get the content area the columns actually span.
    let container_w = inputs
        .known_dimensions
        .width
        .or(match inputs.available_space.width {
            AvailableSpace::Definite(w) => Some(w),
            _ => None,
        })
        .unwrap_or(0.0);

    // Resolve padding + border from the node's current style (not from
    // `unrounded_layout`, which reflects whatever width the *previous*
    // layout pass chose). This matters when the same multicol runs more
    // than once — e.g. a nested multicol whose container width differs
    // between its outer and inner Taffy passes — so that `%` / `calc()`
    // insets re-resolve against the basis in use right now.
    //
    // Basis is `inputs.parent_size.width`, matching Taffy's own
    // `compute_block_layout` (CSS resolves padding/border percentages
    // against the containing block's content width, not the element's
    // own width).
    let (pad, bdr) = {
        let style = tree.get_core_container_style(node_id);
        let raw_pad = style.padding();
        let raw_bdr = style.border();
        let basis = inputs.parent_size.width;
        let pad = raw_pad.resolve_or_zero(basis, |val, b| tree.resolve_calc_value(val, b));
        let bdr = raw_bdr.resolve_or_zero(basis, |val, b| tree.resolve_calc_value(val, b));
        (pad, bdr)
    };
    let inset_left = pad.left + bdr.left;
    let inset_right = pad.right + bdr.right;
    let inset_top = pad.top + bdr.top;
    let inset_bottom = pad.bottom + bdr.bottom;
    let content_w = (container_w - inset_left - inset_right).max(0.0);

    let gap = props.column_gap.max(0.0);
    let (n, col_w) = resolve_column_layout(content_w, props.column_count, props.column_width, gap);

    // 3. Measure every child at col_w via Taffy. We force
    //    `PerformLayout` here regardless of `inputs.run_mode`: even when
    //    our parent is merely sizing us (`ComputeSize`), we need real
    //    child heights to run the balance distribution — without a
    //    completed layout the per-column budget can't be decided.
    let child_inputs = taffy::tree::LayoutInput {
        run_mode: RunMode::PerformLayout,
        sizing_mode: SizingMode::InherentSize,
        axis: RequestedAxis::Both,
        known_dimensions: Size {
            width: Some(col_w),
            height: None,
        },
        known_dimensions_are_definite: Size {
            width: true,
            height: true,
        },
        parent_size: Size {
            width: Some(col_w),
            height: inputs.parent_size.height,
        },
        available_space: Size {
            width: AvailableSpace::Definite(col_w),
            height: AvailableSpace::MaxContent,
        },
        vertical_margins_are_collapsible: Line::FALSE,
    };

    // `column-span: all` children are laid out at the container's full
    // content width (border-box minus the multicol's own padding + border)
    // rather than per-column width. Otherwise identical in shape to
    // `child_inputs` so the rest of blitz's dispatch treats it the same way.
    let span_child_inputs = taffy::tree::LayoutInput {
        run_mode: RunMode::PerformLayout,
        sizing_mode: SizingMode::InherentSize,
        axis: RequestedAxis::Both,
        known_dimensions: Size {
            width: Some(content_w),
            height: None,
        },
        known_dimensions_are_definite: Size {
            width: true,
            height: true,
        },
        parent_size: Size {
            width: Some(content_w),
            height: inputs.parent_size.height,
        },
        available_space: Size {
            width: AvailableSpace::Definite(content_w),
            height: AvailableSpace::MaxContent,
        },
        vertical_margins_are_collapsible: Line::FALSE,
    };

    // 4. column-fill: balance (computed inside layout_column_group).
    //    Subtract the vertical insets from the definite height so the
    //    balance budget matches the content area — the multicol's own
    //    padding + border are added back to `cursor_y` below when we
    //    return the container's outer (border-box) height. Without this
    //    subtraction, a definite-height multicol with non-zero padding
    //    would double-count its vertical insets and grow too tall.
    let paged = tree.paged.get(&usize::from(node_id)).copied();
    // A container broken across pages takes the height its pages give it;
    // the definite height here is only the previous pass's result.
    let avail_h = match inputs.available_space.height {
        AvailableSpace::Definite(h) if paged.is_none() => (h - inset_top - inset_bottom).max(0.0),
        _ => f32::INFINITY,
    };

    // Resolve the container's `column-fill` once per layout. The Phase A
    // side-table (see `column_css::ColumnStyleTable`) holds the parsed
    // value; absent entries default to `Balance` per the CSS initial
    // value. `ColumnFill::Auto` switches the per-group budget from the
    // balance search to a greedy sequential fill (see
    // `layout_column_group`).
    let fill = tree
        .column_styles
        .get(&usize::from(node_id))
        .and_then(|p| p.fill)
        .unwrap_or_default();

    // 4b. Case A — self-inline-root container.
    //
    //     When the multicol container itself is `is_inline_root()` (its
    //     direct children are all inline / text), Blitz shaped the
    //     paragraph against the **container's own** content width and never
    //     calls `compute_child_layout` on the container itself. The
    //     segment-walking path below would descend into the bare text-node
    //     child and produce no meaningful column distribution, leaving
    //     trailing columns empty.
    //
    //     Detect this case and dispatch to `layout_self_inline_root_container`,
    //     which clones the container's parley layout, re-breaks it at
    //     `col_w`, and fills `paragraph_splits` directly off the rebroken
    //     line heights. See `docs/plans/2026-05-03-multicol-inline-root-split.md`.
    let is_self_inline_root = tree
        .doc
        .get_node(crate::blitz_adapter::bid(usize::from(node_id)))
        .map(|n| {
            n.flags.is_inline_root()
                && n.element_data()
                    .and_then(|e| e.inline_layout_data.as_ref())
                    .is_some()
        })
        .unwrap_or(false);
    if is_self_inline_root {
        return layout_self_inline_root_container(
            tree,
            node_id,
            container_w,
            col_w,
            gap,
            n,
            avail_h,
            fill,
            inset_left,
            inset_top,
            inset_bottom,
        );
    }

    // 5. Walk the segments produced by `partition_children_into_segments`
    //    and dispatch each to the appropriate layout strategy:
    //
    //    * `ColumnGroup` → `layout_column_group` at `col_w`, offset by
    //      the current cursor.
    //    * `SpanAll` → measure the child at `container_w` and place it
    //      at `(0, cursor_y)`.
    //
    //    Each segment's vertical extent accumulates into `cursor_y`. Track
    //    widths (`col_w` / `container_w`) drive the segment's placement
    //    math but are NOT forced onto the child's stored layout — we keep
    //    the measured `size.width` so replaced / shrink-to-fit / explicitly
    //    sized children are not stretched to the full track.
    let segments = partition_children_into_segments(&tree.doc, usize::from(node_id));

    // A column group with floats is measured as part of one block, so that
    // text wraps around the floats: Blitz lays the whole container out as
    // a plain block one column wide (it does not do columns). That also
    // lays out the spanners at the column width, so their cached layouts
    // are dropped and they are laid out again below at the full width.
    let float_groups: Vec<bool> = segments
        .iter()
        .map(|s| matches!(s, Segment::ColumnGroup(c) if group_has_floats(tree, c)))
        .collect();
    if float_groups.iter().any(|&f| f) {
        let shared = SharedMeasure {
            inset_left,
            inset_right,
            parent_size: inputs.parent_size,
        };
        tree.cache_clear(node_id);
        tree.doc
            .compute_child_layout(node_id, shared.inputs_at(col_w));
        tree.cache_clear(node_id);
        for segment in &segments {
            if let Segment::SpanAll(child) = segment {
                clear_layout_caches(tree, *child);
            }
        }
    }

    let mut cursor_y: f32 = 0.0;
    let mut placements: Vec<(NodeId, Point<f32>, Size<f32>)> = Vec::new();
    let mut group_geometries: Vec<ColumnGroupGeometry> = Vec::new();
    for (segment, has_floats) in segments.into_iter().zip(float_groups) {
        match segment {
            Segment::ColumnGroup(children) => {
                // The first row of columns ends at the end of the page the
                // set starts on.
                let paging = paged.filter(|p| p.page_h > 0.0).map(|p| {
                    let top = p.top_in_page + inset_top + cursor_y;
                    let used = top.rem_euclid(p.page_h);
                    Paging {
                        first_h: p.page_h - used,
                        page_h: p.page_h,
                    }
                });
                let (group_placements, geometries, seg_h) = layout_column_group(
                    tree,
                    col_w,
                    gap,
                    n,
                    avail_h,
                    fill,
                    &children,
                    cursor_y,
                    child_inputs,
                    paging,
                    has_floats.then_some(inset_left),
                );
                placements.extend(group_placements);
                group_geometries.extend(geometries);
                cursor_y += seg_h;
            }
            Segment::SpanAll(child_id) => {
                // A spanner keeps its own vertical margins; they separate it
                // from the column sets before and after it.
                let (margin_top, margin_bottom) = vertical_margins(tree, child_id, content_w);
                let output = tree.compute_child_layout(child_id, span_child_inputs);
                cursor_y += margin_top.max(0.0);
                let location = Point {
                    x: 0.0,
                    y: cursor_y,
                };
                placements.push((child_id, location, output.size));
                cursor_y += output.size.height + margin_bottom.max(0.0);
            }
        }
    }

    // Offset every placement by the multicol's own padding + border start.
    // Up to this point `(x, y)` are relative to the content box origin;
    // Taffy stores child `location` relative to the parent's border-box
    // top-left (see `compute_block_layout` in taffy — `resolved_content_box_inset.left`
    // is added to the child's location), so we shift into that frame here.
    for (_, location, _) in placements.iter_mut() {
        location.x += inset_left;
        location.y += inset_top;
    }

    // Shift each recorded group geometry into the same border-box frame so
    // downstream column-rule rendering (which receives `x, y` at the
    // container's border-box origin) can use `group.x_offset + col_x_math`
    // and `group.y_offset` directly without reapplying the container's padding.
    for group in group_geometries.iter_mut() {
        group.x_offset = inset_left.as_px();
        group.y_offset += inset_top.as_px();
    }

    // Stash the per-container geometry for downstream consumers. We
    // intentionally record the entry even when the container produced no
    // column groups (all `SpanAll`, or entirely empty) — the presence of the
    // key lets convert-side code distinguish "layout hook ran but found
    // nothing balanceable" from "hook never ran".
    tree.geometry.insert(
        usize::from(node_id),
        MulticolGeometry {
            groups: group_geometries,
        },
    );

    // 6. Write child positions back into Taffy's storage. Only location
    //    and size change between the nested `compute_child_layout` call
    //    and the multicol-assigned slot — padding, border, margin,
    //    scrollbar, content_size, and order all stay at the values the
    //    child's own layout pass already decided. Copy the existing
    //    Layout wholesale and overwrite just the two fields that move,
    //    so we don't accidentally collapse the child's content box into
    //    its border box (which happens if `content_size` is set to the
    //    outer `size`).
    for (child_id, location, size) in &placements {
        let mut layout = tree.doc.get_unrounded_layout(*child_id);
        layout.location = *location;
        layout.size = *size;
        tree.set_unrounded_layout(*child_id, &layout);
    }

    // Container height = stacked segment content + padding + border on
    // both axes. `container_w` already includes horizontal padding/border
    // (it came in as the border-box width).
    let container_h = (cursor_y + inset_top + inset_bottom).max(0.0);

    // 7. Container size = width × stacked segment height.
    taffy::LayoutOutput {
        size: Size {
            width: container_w,
            height: container_h,
        },
        scrollable_overflow_rect: taffy::ScrollableOverflow::new(
            taffy::Rect {
                left: 0.0,
                right: container_w,
                top: 0.0,
                bottom: container_h,
            },
            Point { x: false, y: false },
        ),
        baselines: taffy::Baselines::NONE,
        top_margin: CollapsibleMarginSet::ZERO,
        bottom_margin: CollapsibleMarginSet::ZERO,
        margins_can_collapse_through: false,
        // TODO(blitz-0.3 port): out-of-flow candidates bubbled up from
        // the column children are dropped here.
        oof_candidates: taffy::OofCandidates::NONE,
        oof_positioning_area: None,
    }
}

/// Case A handler — multicol container that is itself inline-root.
///
/// The container has only inline / text content, so Blitz shaped its
/// own parley layout against `content_w` and never calls
/// `compute_child_layout` on the container. Re-breaking the layout at
/// `col_w` requires a local clone (the canonical `inline_layout_data`
/// stays untouched so the convert pass still has a copy to re-clone in
/// Task 7).
///
/// Output: a single `ColumnGroupGeometry` whose `paragraph_splits` carry
/// the per-column line ranges for the container's own NodeId. No
/// placements are written back to Taffy — `compute_root_layout` populates
/// the container's outer slot from the returned `LayoutOutput`, and the
/// container is its own paragraph (no child elements to position).
#[allow(clippy::too_many_arguments)]
fn layout_self_inline_root_container(
    tree: &mut FulgurLayoutTree<'_>,
    node_id: NodeId,
    container_w: f32,
    col_w: f32,
    gap: f32,
    n: u32,
    avail_h: f32,
    fill: crate::column_css::ColumnFill,
    inset_left: f32,
    inset_top: f32,
    inset_bottom: f32,
) -> taffy::LayoutOutput {
    // 1. Clone the container's parley layout. Drop the doc borrow before
    //    mutating `tree.geometry` below.
    let cloned_layout = {
        let node = tree
            .doc
            .get_node(crate::blitz_adapter::bid(usize::from(node_id)))
            .expect("Case A predicate guarantees the node exists");
        let elem = node
            .element_data()
            .expect("Case A predicate guarantees element_data");
        let text_layout = elem
            .inline_layout_data
            .as_ref()
            .expect("Case A predicate guarantees inline_layout_data");
        text_layout.layout.clone()
    };

    // 2. Re-break at `col_w` and re-align. Alignment is left-as-default
    //    (`Alignment::Start`) — this matches the layout pass's role: the
    //    rebroken clone is consumed only to read line counts and per-line
    //    heights here, so horizontal alignment within the rebroken column
    //    has no effect on the recorded `paragraph_splits`. Task 7 will
    //    re-clone the same source layout, re-break with the same `col_w`,
    //    then apply the container's own `text-align` for rendering — line
    //    break positions are deterministic given identical `max_advance`,
    //    so Task 7's indices match the ones we record here.
    let mut rebroken = cloned_layout;
    rebroken.set_alignment(
        parley::Alignment::default(),
        parley::AlignmentOptions::default(),
    );
    rebroken.break_all_lines(Some(col_w));

    // fulgur-6q5 Fix 2: when the rebroken layout contains inline-box
    // items (inline images, inline-blocks, replaced atomic inlines),
    // `convert_multicol_paragraph_slices`'s `GlyphRun`-only materialiser
    // would silently drop them. Avoid emitting a `ParagraphSplitEntry`
    // in that case — instead record a degenerate single-column group
    // whose height matches the original (un-rebroken) parley layout, so
    // the container renders its text at full container width through
    // the standard inline-root path. This is layout-imperfect (the text
    // doesn't actually flow into the second column) but content-safe
    // (no glyph or image is dropped).
    if !parley_layout_is_glyph_run_only(&rebroken) {
        let original_h = {
            let node = tree
                .doc
                .get_node(crate::blitz_adapter::bid(usize::from(node_id)))
                .expect("Case A predicate guarantees the node exists");
            let elem = node
                .element_data()
                .expect("Case A predicate guarantees element_data");
            let text_layout = elem
                .inline_layout_data
                .as_ref()
                .expect("Case A predicate guarantees inline_layout_data");
            crate::blitz_adapter::parley_line_heights(&text_layout.layout)
                .iter()
                .sum::<f32>()
        };
        // Single column-group covering column 0; `paragraph_splits`
        // empty so the convert pass skips this container.
        let mut col_heights: Vec<f32> = vec![0.0_f32; n as usize];
        if let Some(first) = col_heights.first_mut() {
            *first = original_h;
        }
        let geometry = ColumnGroupGeometry {
            x_offset: inset_left.as_px(),
            y_offset: inset_top.as_px(),
            col_w: col_w.as_px(),
            gap: gap.as_px(),
            n,
            col_heights: col_heights
                .iter()
                .copied()
                .map(crate::units::F32Units::as_px)
                .collect(),
            paragraph_splits: Vec::new(),
            box_splits: Vec::new(),
        };
        tree.geometry.insert(
            usize::from(node_id),
            MulticolGeometry {
                groups: vec![geometry],
            },
        );
        let container_h = (original_h + inset_top + inset_bottom).max(0.0);
        return taffy::LayoutOutput {
            size: Size {
                width: container_w,
                height: container_h,
            },
            scrollable_overflow_rect: taffy::ScrollableOverflow::new(
                taffy::Rect {
                    left: 0.0,
                    right: container_w,
                    top: 0.0,
                    bottom: container_h,
                },
                Point { x: false, y: false },
            ),
            baselines: taffy::Baselines::NONE,
            top_margin: CollapsibleMarginSet::ZERO,
            bottom_margin: CollapsibleMarginSet::ZERO,
            margins_can_collapse_through: false,
            // TODO(blitz-0.3 port): out-of-flow candidates bubbled up from
            // the column children are dropped here.
            oof_candidates: taffy::OofCandidates::NONE,
            oof_positioning_area: None,
        };
    }

    // 3. Per-line heights from the rebroken clone.
    let line_heights = crate::blitz_adapter::parley_line_heights(&rebroken);
    let total_h: f32 = line_heights.iter().sum();

    // 4. Per-column budget. For Case A there is exactly one paragraph and
    //    one column group, so balance and auto resolve trivially:
    //    - `auto`  → `avail_h` (greedy fill, trailing columns empty when
    //      content fits in one).
    //    - `balance` → `ceil(total_h / n)` (single-paragraph one-shot
    //      ideal split). Clamped to `avail_h` only when finite, so the
    //      common in-test path with `avail_h = +inf` keeps the ideal
    //      split untouched. Multi-paragraph balance search is unnecessary
    //      because there is exactly one source.
    let budget = match fill {
        crate::column_css::ColumnFill::Auto => avail_h,
        crate::column_css::ColumnFill::Balance => {
            if n == 0 || total_h <= 0.0 {
                0.0
            } else {
                let ideal = (total_h / n as f32).ceil();
                if avail_h.is_finite() {
                    ideal.min(avail_h)
                } else {
                    ideal
                }
            }
        }
    };

    // 5. Slice the line heights across the `n` columns.
    let slices = slice_lines_by_budget(&line_heights, budget, n);

    // 6. Build per-column `ColumnLineSlice` entries. Empty slices keep
    //    `Default` placeholder values per the type contract.
    let mut col_heights: Vec<f32> = vec![0.0_f32; n as usize];
    let mut column_slices: Vec<ColumnLineSlice> = Vec::with_capacity(n as usize);
    for (col_idx, (line_range, slice_h)) in slices.iter().enumerate() {
        if line_range.is_empty() {
            column_slices.push(ColumnLineSlice::default());
            continue;
        }
        let col_x = col_idx as f32 * (col_w + gap);
        col_heights[col_idx] = *slice_h;
        column_slices.push(ColumnLineSlice {
            line_range: line_range.clone(),
            // Slice origin in the segment-relative content-box frame —
            // `compute_multicol_layout` would normally shift this into the
            // border-box frame in the post-loop fixup; here we apply the
            // shift ourselves on the geometry below before returning.
            origin: Point {
                x: col_x.as_px(),
                y: 0.0_f32.as_px(),
            },
            size: Size {
                width: col_w.as_px(),
                height: (*slice_h).as_px(),
            },
        });
    }
    debug_assert_eq!(column_slices.len(), n as usize);

    let paragraph_splits = vec![ParagraphSplitEntry {
        source_node_id: usize::from(node_id),
        column_slices,
    }];

    // 7. Container outer height = tallest column + vertical insets. Width
    //    is already the border-box width on input. Don't write
    //    `set_unrounded_layout` for the container itself — the caller
    //    (`compute_root_layout` via `layout_multicol_subtrees`) populates
    //    the container's outer slot from the returned `LayoutOutput`.
    //    Compute `max_col_h` from the local before moving `col_heights`
    //    into the geometry struct, so we don't have to re-fetch through
    //    `tree.geometry` after insertion.
    let max_col_h = col_heights.iter().copied().fold(0.0_f32, f32::max);

    // 8. Geometry — already in the container's *border-box* frame because
    //    we set `x_offset` / `y_offset` to the resolved insets directly
    //    (mirrors the post-loop shift that the segment path applies in
    //    `compute_multicol_layout`).
    let geometry = ColumnGroupGeometry {
        x_offset: inset_left.as_px(),
        y_offset: inset_top.as_px(),
        col_w: col_w.as_px(),
        gap: gap.as_px(),
        n,
        col_heights: col_heights
            .iter()
            .copied()
            .map(crate::units::F32Units::as_px)
            .collect(),
        paragraph_splits,
        box_splits: Vec::new(),
    };

    // 9. Stash the per-container geometry under the container's own
    //    NodeId, the same key Case B uses. Task 7 looks up by the multicol
    //    container's NodeId; for Case A the container *is* the source, so
    //    `paragraph_splits[0].source_node_id == node_id`.
    tree.geometry.insert(
        usize::from(node_id),
        MulticolGeometry {
            groups: vec![geometry],
        },
    );

    let container_h = (max_col_h + inset_top + inset_bottom).max(0.0);

    taffy::LayoutOutput {
        size: Size {
            width: container_w,
            height: container_h,
        },
        scrollable_overflow_rect: taffy::ScrollableOverflow::new(
            taffy::Rect {
                left: 0.0,
                right: container_w,
                top: 0.0,
                bottom: container_h,
            },
            Point { x: false, y: false },
        ),
        baselines: taffy::Baselines::NONE,
        top_margin: CollapsibleMarginSet::ZERO,
        bottom_margin: CollapsibleMarginSet::ZERO,
        margins_can_collapse_through: false,
        // TODO(blitz-0.3 port): out-of-flow candidates bubbled up from
        // the column children are dropped here.
        oof_candidates: taffy::OofCandidates::NONE,
        oof_positioning_area: None,
    }
}

/// Collapsed vertical margins between two adjoining blocks (CSS 2.2 §8.3.1):
/// the largest positive margin plus the most negative one.
#[derive(Clone, Copy, Debug, Default)]
struct CollapsedMargin {
    positive: f32,
    negative: f32,
}

impl CollapsedMargin {
    fn add(&mut self, margin: f32) {
        if margin > 0.0 {
            self.positive = self.positive.max(margin);
        } else {
            self.negative = self.negative.min(margin);
        }
    }

    fn value(self) -> f32 {
        self.positive + self.negative
    }
}

/// One indivisible piece of a column group's content, in flow order.
#[derive(Clone, Debug)]
struct FlowUnit {
    kind: FlowUnitKind,
    /// Height of the piece itself.
    height: f32,
    /// Collapsed margin between the previous piece and this one. Dropped
    /// when the piece starts a column after a break (margins adjoining an
    /// unforced break are truncated, CSS Fragmentation 3 §5.2).
    gap_before: f32,
    /// Space above the piece that moves with it: the top border and
    /// padding of the boxes the piece opens, and the margins inside them.
    lead: f32,
    /// Space below the piece that moves with it: the margins, bottom
    /// padding and border of the boxes the piece closes.
    trail: f32,
    /// No column break before this piece: it flows beside a float that
    /// starts above it, and the float goes to its anchor's column.
    keep_with_previous: bool,
    /// Room a float anchored above needs below this piece's extent, in
    /// the same column. It does not push the next piece down.
    overhang: f32,
}

impl FlowUnit {
    /// Height the unit takes in a column.
    fn extent(&self) -> f32 {
        self.lead + self.height + self.trail
    }
}

#[derive(Clone, Debug)]
enum FlowUnitKind {
    /// A box placed whole (`index` into `ColumnFlow::blocks`).
    Block(usize),
    /// One line of a paragraph that may break between lines (`index` into
    /// `ColumnFlow::paragraphs`).
    Line(usize),
}

/// A box placed whole, or a paragraph that breaks between lines. `x` is
/// its left edge relative to the column.
#[derive(Clone, Debug)]
struct FlowBlock {
    id: NodeId,
    size: Size<f32>,
    x: f32,
    parent: Option<usize>,
}

/// A block whose children flow through the columns themselves.
#[derive(Clone, Debug)]
struct FlowWrapper {
    id: NodeId,
    parent: Option<usize>,
    /// Left edge relative to the column.
    x: f32,
    units: std::ops::Range<usize>,
    /// Set when the box paints something (border, padding, background).
    /// Without it the box is invisible and is lent a box covering every
    /// column it touches.
    frame: Option<BoxFrame>,
}

/// The visible box of a [`FlowWrapper`] that breaks across columns.
#[derive(Clone, Copy, Debug)]
struct BoxFrame {
    width: f32,
    /// Distance from the top of the box's first unit (the start of its
    /// `lead`) to the box's top border edge.
    top_offset: f32,
    /// Distance from the end of the box's last unit's content to the
    /// box's bottom border edge.
    bottom_offset: f32,
}

/// A float, carried along with the unit it sits beside.
#[derive(Clone, Debug)]
struct FlowFloat {
    block: FlowBlock,
    frame: u32,
    /// Top margin edge, top border edge and bottom margin edge, in its
    /// frame's natural coordinates (the single-column layout).
    margin_top: f32,
    top: f32,
    margin_bottom: f32,
    /// Unit the float moves with, and its top border edge relative to
    /// that unit's content top. Set by [`ColumnFlow::anchor_floats`].
    anchor: usize,
    dy: f32,
}

/// Where a unit's content sits in the single-column layout that measured
/// it. Positions compare only within one frame: a frame is a run of
/// content laid out together (the whole group when it was laid out as one
/// block, otherwise each top-level child).
#[derive(Clone, Copy, Debug)]
struct NaturalPos {
    frame: u32,
    top: f32,
    /// The unit starts a box with `clear`.
    clears: bool,
}

/// A column group flattened into flow units.
#[derive(Default)]
struct ColumnFlow {
    units: Vec<FlowUnit>,
    /// One per unit.
    natural: Vec<NaturalPos>,
    blocks: Vec<FlowBlock>,
    paragraphs: Vec<FlowBlock>,
    wrappers: Vec<FlowWrapper>,
    floats: Vec<FlowFloat>,
    /// Margin after the last unit (inside the multicol, a BFC root, it
    /// still takes space).
    trailing_margin: f32,
    /// A box with `clear` was opened and its first unit is still to come.
    pending_clear: bool,
}

/// What has been seen since the last unit: margins that may still
/// collapse, and the edges of boxes opened since.
#[derive(Default)]
struct PendingSpace {
    margin: CollapsedMargin,
    /// The collapsed margin before the first box opened since the last
    /// unit; the box's top edge stops it from collapsing further.
    gap: Option<f32>,
    /// Box edges and inner margins after `gap`.
    lead: f32,
}

/// Deepest wrapper nesting that is flattened into the columns. Deeper
/// wrappers are placed whole.
const MAX_FLOW_WRAPPER_DEPTH: usize = 32;

/// How a block takes part in column flow.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FlowBox {
    /// Paints nothing and has no insets: it can be cut between columns,
    /// or lent a bigger invisible box, without any visible difference.
    Plain,
    /// Has a border, padding or background, with a top and a bottom edge
    /// (`top`, `bottom`: border plus padding). Its children flow through
    /// the columns and its box is painted in slices, one per column
    /// (`box-decoration-break: slice`).
    Framed { top: f32, bottom: f32 },
}

/// Classify an in-flow `display: block` box that is statically positioned
/// and has no transform, opacity, clipping, shadow or definite height.
/// Anything else (and boxes with an edge on one side only) is placed whole.
fn flow_box(tree: &FulgurLayoutTree<'_>, node_id: NodeId, col_w: f32) -> Option<FlowBox> {
    use ::style::properties::longhands::position::computed_value::T as Pos;
    use ::style::values::specified::box_::{DisplayInside, DisplayOutside};
    use blitz_dom::CopyStyleGetters;

    let node = tree
        .doc
        .get_node(crate::blitz_adapter::bid(usize::from(node_id)))?;
    if node.element_data().is_none()
        || crate::blitz_adapter::is_multicol_container(node)
        || crate::blitz_adapter::has_column_span_all(node)
    {
        return None;
    }
    let styles = node.primary_styles()?;
    let display = styles.clone_display();
    let flowing = display.outside() == DisplayOutside::Block
        && display.inside() == DisplayInside::Flow
        && !display.is_list_item()
        && matches!(styles.get_box().slow_clone_position(), Pos::Static)
        && styles.get_box().transform.0.is_empty()
        && styles.clone_opacity() >= 1.0
        && styles.clone_overflow_x() == ::style::values::computed::Overflow::Visible
        && styles.clone_overflow_y() == ::style::values::computed::Overflow::Visible
        && styles.slow_clone_box_shadow().0.is_empty();
    if !flowing {
        return None;
    }
    let style = tree.get_core_container_style(node_id);
    if !style.size().height.is_auto() {
        return None;
    }
    let calc = |val, basis| tree.resolve_calc_value(val, basis);
    let padding = style.padding().resolve_or_zero(Some(col_w), calc);
    let border = style.border().resolve_or_zero(Some(col_w), calc);
    let insets_zero = [padding, border]
        .iter()
        .flat_map(|r| [r.top, r.bottom, r.left, r.right])
        .all(|v| v == 0.0);
    let background_transparent = styles
        .slow_clone_background_color()
        .resolve_to_absolute(&styles.clone_color())
        .alpha
        <= 0.0;
    let no_background_image = styles
        .slow_clone_background_image()
        .0
        .iter()
        .all(|i| matches!(i, ::style::values::computed::image::Image::None));
    if insets_zero && background_transparent && no_background_image {
        return Some(FlowBox::Plain);
    }
    // A min or max height would have to be shared out between the slices.
    if !style.min_size().height.is_auto() || !style.max_size().height.is_auto() {
        return None;
    }
    let top = border.top + padding.top;
    let bottom = border.bottom + padding.bottom;
    // With no edge on one side, margins collapse through that side, which
    // the flow does not model.
    (top > 0.0 && bottom > 0.0).then_some(FlowBox::Framed { top, bottom })
}

/// Plain in-flow block: see [`FlowBox::Plain`].
fn is_plain_flow_block(tree: &FulgurLayoutTree<'_>, node_id: NodeId, col_w: f32) -> bool {
    flow_box(tree, node_id, col_w) == Some(FlowBox::Plain)
}

/// Margins of `node_id`, resolved against the column width.
fn margins(tree: &FulgurLayoutTree<'_>, node_id: NodeId, col_w: f32) -> taffy::Rect<f32> {
    let style = tree.get_core_container_style(node_id);
    style.margin().resolve_or_zero(Some(col_w), |val, basis| {
        tree.resolve_calc_value(val, basis)
    })
}

/// Top and bottom margins of `node_id`, resolved against the column width.
fn vertical_margins(tree: &FulgurLayoutTree<'_>, node_id: NodeId, col_w: f32) -> (f32, f32) {
    let margin = margins(tree, node_id, col_w);
    (margin.top, margin.bottom)
}

/// Per-line heights of `node_id` when it is a paragraph that may break
/// between lines inside the columns: a plain block that is an inline root
/// with text only (inline boxes cannot be re-drawn per slice yet), whose
/// lines make up its whole height.
fn splittable_line_heights(
    tree: &FulgurLayoutTree<'_>,
    node_id: NodeId,
    size: Size<f32>,
    col_w: f32,
) -> Option<Vec<f32>> {
    let node = tree
        .doc
        .get_node(crate::blitz_adapter::bid(usize::from(node_id)))?;
    if !node.flags.is_inline_root() {
        return None;
    }
    let layout = &node.element_data()?.inline_layout_data.as_ref()?.layout;
    if !parley_layout_is_glyph_run_only(layout) {
        return None;
    }
    if !is_plain_flow_block(tree, node_id, col_w) {
        return None;
    }
    let heights = crate::blitz_adapter::parley_line_heights(layout);
    let total: f32 = heights.iter().sum();
    (heights.len() > 1 && (total - size.height).abs() <= 0.5).then_some(heights)
}

/// `node_id` is floated.
fn is_floated(tree: &FulgurLayoutTree<'_>, node_id: NodeId) -> bool {
    use ::style::properties::longhands::float::computed_value::T as Float;
    tree.doc
        .get_node(crate::blitz_adapter::bid(usize::from(node_id)))
        .and_then(|n| n.primary_styles())
        .is_some_and(|s| !matches!(s.get_box().slow_clone_float(), Float::None))
}

/// A column group whose layout needs floats: a floated child next to an
/// in-flow one.
fn group_has_floats(tree: &FulgurLayoutTree<'_>, children: &[NodeId]) -> bool {
    children.iter().any(|&c| is_floated(tree, c)) && children.iter().any(|&c| !is_floated(tree, c))
}

/// Drop the cached layouts of `node_id` and its descendants, so that the
/// next layout recomputes them (and their text) at the size asked for.
fn clear_layout_caches(tree: &mut FulgurLayoutTree<'_>, node_id: NodeId) {
    let children: Vec<NodeId> = tree.child_ids(node_id).collect();
    tree.cache_clear(node_id);
    for child in children {
        clear_layout_caches(tree, child);
    }
}

/// `node_id` clears floats.
fn clears_floats(tree: &FulgurLayoutTree<'_>, node_id: NodeId) -> bool {
    use ::style::properties::longhands::clear::computed_value::T as Clear;
    tree.doc
        .get_node(crate::blitz_adapter::bid(usize::from(node_id)))
        .and_then(|n| n.primary_styles())
        .is_some_and(|s| !matches!(s.get_box().slow_clone_clear(), Clear::None))
}

/// Layout children of a wrapper candidate, or `None` when one of them is
/// out of flow (positioned against the wrapper, which would move) or none
/// is in flow. Floats are kept: they move with the content beside them.
fn flow_children(tree: &FulgurLayoutTree<'_>, node_id: NodeId) -> Option<Vec<NodeId>> {
    let children: Vec<NodeId> = tree.child_ids(node_id).collect();
    if children.iter().any(|&child| tree.is_out_of_flow(child)) {
        return None;
    }
    children
        .iter()
        .any(|&child| !is_floated(tree, child))
        .then_some(children)
}

/// A child to flatten.
#[derive(Clone, Copy, Debug)]
struct FlowChild {
    id: NodeId,
    /// Border-box size.
    size: Size<f32>,
    /// Left edge relative to the column.
    x: f32,
    /// Top border edge in its frame's natural coordinates.
    y: f32,
    frame: u32,
}

impl ColumnFlow {
    /// Flatten `children` (already laid out at `col_w`) into flow units.
    /// Nested sizes and offsets are read from the layout their parent's
    /// pass stored.
    ///
    /// Floats move with the unit their top sits beside, and the units
    /// beside them stay in that column. Top-level floats are only taken as
    /// floats when the group was laid out as one block
    /// (`top_level_floats`); otherwise they are placed as blocks.
    fn collect(
        tree: &FulgurLayoutTree<'_>,
        children: &[FlowChild],
        col_w: f32,
        top_level_floats: bool,
    ) -> Self {
        let mut flow = ColumnFlow::default();
        let mut pending = PendingSpace::default();
        flow.add_children(
            tree,
            children,
            None,
            col_w,
            0,
            top_level_floats,
            &mut pending,
        );
        flow.trailing_margin = pending.margin.value().max(0.0);
        if !flow.floats.is_empty() {
            flow.add_clearance();
            flow.anchor_floats();
        }
        flow
    }

    /// Append a unit, taking the space pending before it. `natural` is
    /// its content top in its frame.
    fn push_unit(
        &mut self,
        kind: FlowUnitKind,
        height: f32,
        pending: &mut PendingSpace,
        frame: u32,
        natural: f32,
    ) {
        let (gap_before, lead) = match pending.gap.take() {
            Some(gap) => (gap, pending.lead + pending.margin.value()),
            None => (pending.margin.value(), 0.0),
        };
        *pending = PendingSpace::default();
        self.units.push(FlowUnit {
            kind,
            height,
            gap_before,
            lead,
            trail: 0.0,
            keep_with_previous: false,
            overhang: 0.0,
        });
        self.natural.push(NaturalPos {
            frame,
            top: natural,
            clears: std::mem::take(&mut self.pending_clear),
        });
    }

    /// Give each unit that clears floats the space its single-column
    /// layout gave it: the margin model alone would put it beside the
    /// float. The space sits in the gap before the unit, so a column
    /// break before it drops it, as the float stays behind.
    fn add_clearance(&mut self) {
        let mut reference: std::collections::BTreeMap<u32, (f32, f32)> = Default::default();
        let mut y = 0.0_f32;
        for (unit, natural) in self.units.iter_mut().zip(&self.natural) {
            let mut top = y + unit.gap_before;
            let content = top + unit.lead;
            match reference.get(&natural.frame) {
                Some(&(natural0, content0)) if natural.clears => {
                    let expected = content0 + (natural.top - natural0);
                    if expected > content + 0.5 {
                        unit.gap_before += expected - content;
                        top += expected - content;
                    }
                }
                Some(_) => {}
                None => {
                    reference.insert(natural.frame, (natural.top, content));
                }
            }
            y = top + unit.extent();
        }
    }

    /// Tie each float to the first unit of its frame at or below its top
    /// (or the frame's last unit), keep the units beside it with that
    /// unit, and reserve the room it needs below the last of them.
    fn anchor_floats(&mut self) {
        const SNAP: f32 = 0.5;
        for float in &mut self.floats {
            let in_frame: Vec<usize> = (0..self.units.len())
                .filter(|&i| self.natural[i].frame == float.frame)
                .collect();
            let Some(&last_in_frame) = in_frame.last() else {
                continue;
            };
            let anchor = in_frame
                .iter()
                .copied()
                .find(|&i| self.natural[i].top >= float.margin_top - SNAP)
                .unwrap_or(last_in_frame);
            float.anchor = anchor;
            float.dy = float.top - self.natural[anchor].top;
            let mut last = anchor;
            for &i in in_frame.iter().filter(|&&i| i > anchor) {
                if self.natural[i].top >= float.margin_bottom - SNAP {
                    break;
                }
                self.units[i].keep_with_previous = true;
                last = i;
            }
            let last_bottom = self.natural[last].top + self.units[last].height;
            let unit = &mut self.units[last];
            unit.overhang = unit.overhang.max(float.margin_bottom - last_bottom);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_children(
        &mut self,
        tree: &FulgurLayoutTree<'_>,
        children: &[FlowChild],
        parent: Option<usize>,
        col_w: f32,
        depth: usize,
        floats: bool,
        pending: &mut PendingSpace,
    ) {
        for &FlowChild {
            id,
            size,
            x,
            y,
            frame: natural_frame,
        } in children
        {
            let (margin_top, margin_bottom) = vertical_margins(tree, id, col_w);
            if floats && is_floated(tree, id) {
                // Out of flow: no unit and no margin collapsing.
                self.floats.push(FlowFloat {
                    block: FlowBlock {
                        id,
                        size,
                        x,
                        parent,
                    },
                    frame: natural_frame,
                    margin_top: y - margin_top,
                    top: y,
                    margin_bottom: y + size.height + margin_bottom,
                    anchor: 0,
                    dy: 0.0,
                });
                continue;
            }
            if clears_floats(tree, id) {
                self.pending_clear = true;
            }
            pending.margin.add(margin_top);

            let is_inline_root = tree
                .doc
                .get_node(crate::blitz_adapter::bid(usize::from(id)))
                .is_some_and(|n| n.flags.is_inline_root());
            let wrapper = (depth < MAX_FLOW_WRAPPER_DEPTH && !is_inline_root)
                .then(|| flow_box(tree, id, col_w))
                .flatten()
                .and_then(|kind| flow_children(tree, id).map(|c| (kind, c)));

            if let Some((kind, grandchildren)) = wrapper {
                let index = self.wrappers.len();
                let first = self.units.len();
                let frame = match kind {
                    // Margins collapse through a wrapper without padding or
                    // border, so its own margins join its children's.
                    FlowBox::Plain => None,
                    // The top edge ends the collapsing: the margins before
                    // it become the gap, the edge and what follows lead.
                    FlowBox::Framed { top, .. } => {
                        let top_offset = match pending.gap {
                            None => {
                                pending.gap = Some(pending.margin.value());
                                0.0
                            }
                            Some(_) => {
                                pending.lead += pending.margin.value();
                                pending.lead
                            }
                        };
                        pending.margin = CollapsedMargin::default();
                        pending.lead += top;
                        Some(BoxFrame {
                            width: size.width,
                            top_offset,
                            bottom_offset: 0.0,
                        })
                    }
                };
                self.wrappers.push(FlowWrapper {
                    id,
                    parent,
                    x,
                    units: first..first,
                    frame,
                });
                let nested: Vec<FlowChild> = grandchildren
                    .iter()
                    .map(|&c| {
                        let layout = tree.get_unrounded_layout(c);
                        FlowChild {
                            id: c,
                            size: layout.size,
                            x: x + layout.location.x,
                            y: y + layout.location.y,
                            frame: natural_frame,
                        }
                    })
                    .collect();
                self.add_children(tree, &nested, Some(index), col_w, depth + 1, true, pending);
                self.wrappers[index].units.end = self.units.len();
                if let FlowBox::Framed { bottom, .. } = kind
                    && let Some(last) = self.units[first..].last_mut()
                {
                    // The bottom edge keeps the last child's margin inside.
                    last.trail += pending.margin.value() + bottom;
                    pending.margin = CollapsedMargin::default();
                    if let Some(frame) = self.wrappers[index].frame.as_mut() {
                        frame.bottom_offset = last.trail;
                    }
                }
            } else if let Some(lines) = splittable_line_heights(tree, id, size, col_w) {
                let paragraph = self.paragraphs.len();
                self.paragraphs.push(FlowBlock {
                    id,
                    size,
                    x,
                    parent,
                });
                let mut line_top = y;
                for height in lines {
                    self.push_unit(
                        FlowUnitKind::Line(paragraph),
                        height,
                        pending,
                        natural_frame,
                        line_top,
                    );
                    line_top += height;
                }
            } else {
                let block = self.blocks.len();
                self.blocks.push(FlowBlock {
                    id,
                    size,
                    x,
                    parent,
                });
                self.push_unit(
                    FlowUnitKind::Block(block),
                    size.height,
                    pending,
                    natural_frame,
                    y,
                );
            }
            pending.margin.add(margin_bottom);
        }
    }
}

/// Where each unit lands: `(column, top)` with `top` relative to the
/// column group and measured at the start of the unit's `lead`. Fills
/// columns top to bottom, breaking before a unit that would end below
/// `budget`. A column always takes at least one unit. With `strict`,
/// running out of columns fails; otherwise the last column takes the rest.
fn place_units(units: &[FlowUnit], n: u32, budget: f32, strict: bool) -> Option<Vec<(u32, f32)>> {
    const EPSILON: f32 = 0.01;
    let mut out = Vec::with_capacity(units.len());
    let mut col: u32 = 0;
    let mut y: f32 = 0.0;
    let mut col_start = 0;
    let mut index = 0;
    while index < units.len() {
        let unit = &units[index];
        let column_has_content = index > col_start;
        // The first column keeps its leading margin; later columns start
        // after a break and drop it.
        let gap = if column_has_content || col == 0 {
            unit.gap_before
        } else {
            0.0
        };
        if column_has_content && y + gap + unit.extent() + unit.overhang > budget + EPSILON {
            if col + 1 < n {
                let restart = break_point(units, col_start, index);
                out.truncate(restart);
                col += 1;
                y = 0.0;
                col_start = restart;
                index = restart;
                continue;
            } else if strict {
                return None;
            }
        }
        let top = y + gap;
        out.push((col, top));
        y = top + unit.extent();
        index += 1;
    }
    Some(out)
}

/// Where a column that starts at `col_start` breaks when the unit at
/// `index` does not fit: before the run of units kept together with it
/// (beside a float), or at `index` when that run fills the column.
fn break_point(units: &[FlowUnit], col_start: usize, index: usize) -> usize {
    let mut start = index;
    while start > col_start && units[start].keep_with_previous {
        start -= 1;
    }
    if start == col_start { index } else { start }
}

/// The smallest column height that fits `units` into `n` columns.
fn balanced_budget(units: &[FlowUnit], n: u32) -> f32 {
    let Some(single) = place_units(units, 1, f32::INFINITY, true) else {
        return 0.0;
    };
    let total = units
        .iter()
        .zip(&single)
        .map(|(u, (_, top))| top + u.extent() + u.overhang)
        .fold(0.0_f32, f32::max);
    if total <= 0.0 || n <= 1 {
        return total;
    }
    // Margins may fall at a break and be dropped, so only the units' own
    // extents bound the column height from below.
    let tallest = units.iter().map(FlowUnit::extent).fold(0.0_f32, f32::max);
    let content: f32 = units.iter().map(FlowUnit::extent).sum();
    let mut low = (content / n as f32).max(tallest);
    let mut high = total;
    if place_units(units, n, low, true).is_some() {
        return low;
    }
    // `low` does not fit and `high` (everything in one column) does.
    for _ in 0..40 {
        if high - low <= 0.05 {
            break;
        }
        let mid = (low + high) / 2.0;
        if place_units(units, n, mid, true).is_some() {
            high = mid;
        } else {
            low = mid;
        }
    }
    high
}

/// Consecutive units in the same column: `(column, first, last)` unit
/// indices, inclusive.
fn column_runs(tops: &[(u32, f32)], units: std::ops::Range<usize>) -> Vec<(u32, usize, usize)> {
    let mut runs: Vec<(u32, usize, usize)> = Vec::new();
    for index in units {
        let col = tops[index].0;
        match runs.last_mut() {
            Some(run) if run.0 == col => run.2 = index,
            _ => runs.push((col, index, index)),
        }
    }
    runs
}

/// Fill one row of `n` columns, `budget` tall, with the leading units of
/// `units`, stopping at the first unit that no longer fits. Returns the
/// `(column, top)` of each placed unit. A unit taller than the row is
/// placed alone in a column, unless `skip_oversized` (the row is the rest
/// of a page that already has content), in which case that column stays
/// empty.
fn fill_row(units: &[FlowUnit], n: u32, budget: f32, skip_oversized: bool) -> Vec<(u32, f32)> {
    const EPSILON: f32 = 0.01;
    let mut out = Vec::new();
    let mut col: u32 = 0;
    let mut y: f32 = 0.0;
    let mut col_start = 0;
    let mut index = 0;
    while index < units.len() {
        let unit = &units[index];
        let column_has_content = index > col_start;
        let gap = if column_has_content {
            unit.gap_before
        } else {
            0.0
        };
        if y + gap + unit.extent() + unit.overhang > budget + EPSILON
            && (column_has_content || skip_oversized)
        {
            let restart = if column_has_content {
                break_point(units, col_start, index)
            } else {
                index
            };
            out.truncate(restart);
            index = restart;
            if col + 1 < n {
                col += 1;
                y = 0.0;
                col_start = restart;
                continue;
            }
            break;
        }
        out.push((col, y + gap));
        y += gap + unit.extent();
        index += 1;
    }
    out
}

/// A row of columns in a column set: one per page when the set breaks
/// across pages, otherwise just one.
#[derive(Clone, Copy, Debug)]
struct ColumnRow {
    /// Top, relative to the column set.
    top: f32,
    /// Column height the units were placed against.
    budget: f32,
}

/// Place `units` into rows of `n` columns. Returns each unit's
/// `(slot, top)`, where `slot / n` is the row and `slot % n` the column
/// and `top` is relative to the column set, and the rows.
fn place_rows(
    units: &[FlowUnit],
    n: u32,
    fill: crate::column_css::ColumnFill,
    avail_h: f32,
    paging: Option<Paging>,
) -> (Vec<(u32, f32)>, Vec<ColumnRow>) {
    const EPSILON: f32 = 0.01;
    let last_row_budget = |units: &[FlowUnit], room: f32| match fill {
        crate::column_css::ColumnFill::Auto => room,
        crate::column_css::ColumnFill::Balance => balanced_budget(units, n).min(room),
    };
    let Some(paging) = paging else {
        let budget = last_row_budget(units, avail_h);
        let tops = place_units(units, n, budget, false).unwrap_or_default();
        return (tops, vec![ColumnRow { top: 0.0, budget }]);
    };

    let mut tops: Vec<(u32, f32)> = Vec::with_capacity(units.len());
    let mut rows: Vec<ColumnRow> = Vec::new();
    let mut start = 0;
    let mut row_top = 0.0_f32;
    let mut room = paging.first_h;
    while start < units.len() {
        let row = rows.len() as u32;
        let first_row = rows.is_empty();
        // Units after a page break drop the margin before them.
        let mut rest = units[start..].to_vec();
        if !first_row {
            rest[0].gap_before = 0.0;
        }
        let out_of_pages = rows.len() >= crate::MAX_PAGES as usize;
        // The rest fits this page: it is the last row, balanced.
        let budget = last_row_budget(&rest, room);
        if out_of_pages || place_units(&rest, n, budget, true).is_some() {
            let placed = place_units(&rest, n, budget, false).unwrap_or_default();
            tops.extend(placed.into_iter().map(|(c, y)| (row * n + c, row_top + y)));
            rows.push(ColumnRow {
                top: row_top,
                budget,
            });
            break;
        }
        let mut placed = fill_row(&rest, n, room, first_row && room + EPSILON < paging.page_h);
        if placed.is_empty() && !first_row {
            // Never stall: a full page takes at least one unit.
            placed.push((0, 0.0));
        }
        start += placed.len();
        tops.extend(placed.into_iter().map(|(c, y)| (row * n + c, row_top + y)));
        rows.push(ColumnRow {
            top: row_top,
            budget: room,
        });
        row_top += room;
        room = paging.page_h;
    }
    (tops, rows)
}

/// The multicol container's insets and basis, for laying it out as one
/// block a column wide.
#[derive(Clone, Copy, Debug)]
struct SharedMeasure {
    /// Left and right padding plus border.
    inset_left: f32,
    inset_right: f32,
    parent_size: Size<Option<f32>>,
}

impl SharedMeasure {
    /// Inputs that lay the container out as a block whose content box is
    /// one column wide.
    fn inputs_at(&self, col_w: f32) -> taffy::tree::LayoutInput {
        let width = col_w + self.inset_left + self.inset_right;
        taffy::tree::LayoutInput {
            run_mode: RunMode::PerformLayout,
            sizing_mode: SizingMode::InherentSize,
            axis: RequestedAxis::Both,
            known_dimensions: Size {
                width: Some(width),
                height: None,
            },
            known_dimensions_are_definite: Size {
                width: true,
                height: true,
            },
            parent_size: self.parent_size,
            available_space: Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::MaxContent,
            },
            vertical_margins_are_collapsible: Line::FALSE,
        }
    }
}

/// Place `children` into `n` columns of `col_w`, starting at `y_offset`.
/// `avail_h` caps the column height (for `column-fill: auto`, it is the
/// column height). With `paging`, the columns are filled page by page and
/// only the last page's columns are balanced.
///
/// The children are flattened into flow units first: paragraphs break
/// between lines, blocks without painting let their own children flow
/// through the columns, blocks with a border, padding or background do
/// too and are painted in one slice per column, and everything else is
/// placed whole. Vertical margins collapse as in normal flow and are
/// dropped at the top of a column after a break. `column-fill: balance`
/// then searches for the smallest column height that holds everything.
///
/// Returns `(placements, geometries, height)`:
///
/// - `placements` holds the `(child, location, size)` of the direct
///   children, in the content-box frame. `compute_multicol_layout` shifts
///   them by the container's insets and writes them to Taffy. Nested
///   boxes (inside wrappers) are written here, relative to their wrapper.
/// - `geometries` has one entry per row of columns (one per page when the
///   set breaks across pages), each with the column width, gap, count,
///   y-offset and per-column filled height. The first also carries the
///   paragraphs and boxes split across columns, in its own frame.
/// - `height` is the column set's height.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn layout_column_group(
    tree: &mut FulgurLayoutTree<'_>,
    col_w: f32,
    gap: f32,
    n: u32,
    avail_h: f32,
    fill: crate::column_css::ColumnFill,
    children: &[NodeId],
    y_offset: f32,
    child_inputs: taffy::tree::LayoutInput,
    paging: Option<Paging>,
    shared: Option<f32>,
) -> (
    Vec<(NodeId, Point<f32>, Size<f32>)>,
    Vec<ColumnGroupGeometry>,
    f32,
) {
    // 1. Measure at the column width. This also lays out every
    //    descendant, so wrapper children have sizes and paragraphs have
    //    lines broken at `col_w`.
    //    A child with `width: auto` fills the column between its
    //    margins; any other width is resolved against the column.
    //    A group with a float among its children was laid out as part of
    //    one block instead (`shared`, the container's left inset), so that
    //    the content beside the float wraps around it.
    let mut measured: Vec<FlowChild> = Vec::with_capacity(children.len());
    let measure_alone = |tree: &mut FulgurLayoutTree<'_>, child: NodeId| {
        let margin = margins(tree, child, col_w);
        let auto_width = tree.get_core_container_style(child).size().width.is_auto();
        let mut inputs = child_inputs;
        inputs.known_dimensions.width =
            auto_width.then(|| (col_w - margin.left - margin.right).max(0.0));
        (tree.compute_child_layout(child, inputs).size, margin.left)
    };
    if let Some(inset_left) = shared {
        for &child in children {
            let layout = tree.get_unrounded_layout(child);
            let mut size = layout.size;
            if tree.is_multicol(child) {
                // Blitz laid it out without columns.
                size = measure_alone(tree, child).0;
            }
            measured.push(FlowChild {
                id: child,
                size,
                x: layout.location.x - inset_left,
                y: layout.location.y,
                frame: 0,
            });
        }
    } else {
        for (index, &child) in children.iter().enumerate() {
            let (size, x) = measure_alone(tree, child);
            measured.push(FlowChild {
                id: child,
                size,
                x,
                y: 0.0,
                frame: index as u32,
            });
        }
    }

    // 2. Flatten into flow units and place them in rows of columns.
    let flow = ColumnFlow::collect(tree, &measured, col_w, shared.is_some());
    let (tops, rows) = place_rows(&flow.units, n, fill, avail_h, paging);
    let content_top = |index: usize| tops[index].1 + flow.units[index].lead;
    let content_bottom = |index: usize| content_top(index) + flow.units[index].height;

    // 3. Column geometry, per row.
    let stride = col_w + gap;
    let col_x = |slot: u32| (slot % n) as f32 * stride;
    let row_of = |slot: u32| (slot / n) as usize;
    let mut col_heights: Vec<Vec<f32>> = vec![vec![0.0_f32; n as usize]; rows.len()];
    for (unit, &(slot, top)) in flow.units.iter().zip(&tops) {
        let row = row_of(slot);
        let h = &mut col_heights[row][(slot % n) as usize];
        *h = h.max(top - rows[row].top + unit.extent());
    }
    if let Some(&(slot, _)) = tops.last() {
        col_heights[row_of(slot)][(slot % n) as usize] += flow.trailing_margin;
    }
    // Each float's top, relative to the column set. A float above the top
    // of a column its anchor starts after a break is moved down to it.
    let float_tops: Vec<f32> = flow
        .floats
        .iter()
        .map(|float| {
            let (slot, _) = tops[float.anchor];
            let top = content_top(float.anchor) + float.dy;
            let starts_column = float.anchor == 0 || tops[float.anchor - 1].0 != slot;
            if starts_column && slot >= n {
                top.max(rows[row_of(slot)].top)
            } else if starts_column {
                top.max(0.0)
            } else {
                top
            }
        })
        .collect();
    for (float, &top) in flow.floats.iter().zip(&float_tops) {
        let slot = tops[float.anchor].0;
        let row = row_of(slot);
        let bottom = top + (float.margin_bottom - float.top);
        let h = &mut col_heights[row][(slot % n) as usize];
        *h = h.max(bottom - rows[row].top);
    }
    // Where a box broken at the end of a column ends: the column box.
    let column_end = |slot: u32, run_bottom: f32| {
        let row = rows[row_of(slot)];
        if row.budget.is_finite() {
            (row.top + row.budget).max(run_bottom)
        } else {
            run_bottom
        }
    };

    // 4. Absolute (group-frame) boxes. Invisible wrappers cover every
    //    column their units touch, from the highest to the lowest unit. A
    //    framed box in one column gets its own box there; one that breaks
    //    is lent the covering box too and painted from `box_splits`.
    let full_w = n as f32 * col_w + n.saturating_sub(1) as f32 * gap;
    let mut box_splits: Vec<BoxSplitEntry> = Vec::new();
    let mut wrapper_boxes: Vec<(Point<f32>, Size<f32>)> = Vec::with_capacity(flow.wrappers.len());
    for wrapper in &flow.wrappers {
        let range = wrapper.units.clone();
        let runs = column_runs(&tops, range.clone());
        let covering = || {
            let top = range
                .clone()
                .map(|i| tops[i].1)
                .fold(f32::INFINITY, f32::min);
            let bottom = range
                .clone()
                .map(|i| tops[i].1 + flow.units[i].extent())
                .fold(0.0_f32, f32::max);
            let top = if top.is_finite() { top } else { 0.0 };
            (top, bottom)
        };
        let boxed = match (wrapper.frame, runs.as_slice()) {
            (Some(frame), [(slot, first, last)]) => {
                let top = tops[*first].1 + frame.top_offset;
                let bottom = content_bottom(*last) + frame.bottom_offset;
                (
                    Point {
                        x: col_x(*slot) + wrapper.x,
                        y: y_offset + top,
                    },
                    Size {
                        width: frame.width,
                        height: (bottom - top).max(0.0),
                    },
                )
            }
            (Some(frame), [.., (_, _, last_unit)]) => {
                let box_top = tops[range.start].1 + frame.top_offset;
                let box_bottom = content_bottom(*last_unit) + frame.bottom_offset;
                let mut fragments = Vec::with_capacity(runs.len());
                let mut consumed = 0.0_f32;
                for (i, &(slot, _, last)) in runs.iter().enumerate() {
                    let top = if i == 0 {
                        box_top
                    } else {
                        rows[row_of(slot)].top
                    };
                    let bottom = if i + 1 == runs.len() {
                        box_bottom
                    } else {
                        column_end(slot, tops[last].1 + flow.units[last].extent())
                    };
                    let height = (bottom - top).max(0.0);
                    fragments.push(BoxFragment {
                        origin: Point {
                            x: (col_x(slot) + wrapper.x).as_px(),
                            y: top.as_px(),
                        },
                        size: Size {
                            width: frame.width.as_px(),
                            height: height.as_px(),
                        },
                        offset: consumed.as_px(),
                    });
                    consumed += height;
                }
                box_splits.push(BoxSplitEntry {
                    source_node_id: usize::from(wrapper.id),
                    height: consumed.as_px(),
                    fragments,
                });
                let (top, bottom) = covering();
                let top = top.min(box_top);
                (
                    Point {
                        x: 0.0,
                        y: y_offset + top,
                    },
                    Size {
                        width: full_w,
                        height: (bottom - top).max(0.0),
                    },
                )
            }
            _ => {
                let (top, bottom) = covering();
                (
                    Point {
                        x: 0.0,
                        y: y_offset + top,
                    },
                    Size {
                        width: full_w,
                        height: (bottom - top).max(0.0),
                    },
                )
            }
        };
        wrapper_boxes.push(boxed);
    }
    box_splits.sort_by_key(|b| b.source_node_id);

    let mut top_level: Vec<(NodeId, Point<f32>, Size<f32>)> = Vec::new();
    for (wrapper, &(abs, size)) in flow.wrappers.iter().zip(&wrapper_boxes) {
        let parent_origin = wrapper.parent.map(|p| wrapper_boxes[p].0);
        match parent_origin {
            None => top_level.push((wrapper.id, abs, size)),
            Some(origin) => {
                let mut layout = tree.doc.get_unrounded_layout(wrapper.id);
                layout.location = Point {
                    x: abs.x - origin.x,
                    y: abs.y - origin.y,
                };
                layout.size = size;
                tree.set_unrounded_layout(wrapper.id, &layout);
            }
        }
    }

    let mut place = |tree: &mut FulgurLayoutTree<'_>,
                     id: NodeId,
                     parent: Option<usize>,
                     abs: Point<f32>,
                     size: Size<f32>| {
        match parent {
            None => top_level.push((id, abs, size)),
            Some(p) => {
                let origin = wrapper_boxes[p].0;
                let mut layout = tree.doc.get_unrounded_layout(id);
                layout.location = Point {
                    x: abs.x - origin.x,
                    y: abs.y - origin.y,
                };
                layout.size = size;
                tree.set_unrounded_layout(id, &layout);
            }
        }
    };

    // 5. Blocks and paragraphs.
    let mut paragraph_lines: Vec<Vec<(u32, f32, f32)>> = vec![Vec::new(); flow.paragraphs.len()];
    for (index, unit) in flow.units.iter().enumerate() {
        let slot = tops[index].0;
        match unit.kind {
            FlowUnitKind::Block(b) => {
                let block = &flow.blocks[b];
                let abs = Point {
                    x: col_x(slot) + block.x,
                    y: y_offset + content_top(index),
                };
                place(tree, block.id, block.parent, abs, block.size);
            }
            FlowUnitKind::Line(paragraph) => {
                paragraph_lines[paragraph].push((slot, content_top(index), unit.height));
            }
        }
    }

    for (float, &top) in flow.floats.iter().zip(&float_tops) {
        let slot = tops[float.anchor].0;
        let abs = Point {
            x: col_x(slot) + float.block.x,
            y: y_offset + top,
        };
        place(
            tree,
            float.block.id,
            float.block.parent,
            abs,
            float.block.size,
        );
    }

    let mut paragraph_splits: Vec<ParagraphSplitEntry> = Vec::new();
    for (paragraph, lines) in flow.paragraphs.iter().zip(&paragraph_lines) {
        let Some(&(first_slot, first_top, _)) = lines.first() else {
            continue;
        };
        // Consecutive lines per column: (slot, first line, line count,
        // top, height).
        let mut runs: Vec<(u32, usize, usize, f32, f32)> = Vec::new();
        for (index, &(slot, top, height)) in lines.iter().enumerate() {
            match runs.last_mut() {
                Some(run) if run.0 == slot => {
                    run.2 += 1;
                    run.4 = top + height - run.3;
                }
                _ => runs.push((slot, index, 1, top, height)),
            }
        }
        let first_abs = Point {
            x: col_x(first_slot) + paragraph.x,
            y: y_offset + first_top,
        };
        if runs.len() == 1 {
            place(
                tree,
                paragraph.id,
                paragraph.parent,
                first_abs,
                paragraph.size,
            );
            continue;
        }
        // The box keeps the first slice; the convert pass draws every
        // slice from `paragraph_splits`. When the slices reach later rows
        // (pages), the box reaches down to the last one, so that the
        // paragraph is drawn on each of those pages.
        let last = runs[runs.len() - 1];
        let height = if row_of(last.0) == row_of(first_slot) {
            runs[0].4
        } else {
            last.3 + last.4 - first_top
        };
        let first_size = Size {
            width: paragraph.size.width,
            height,
        };
        place(tree, paragraph.id, paragraph.parent, first_abs, first_size);
        let column_slices: Vec<ColumnLineSlice> = runs
            .iter()
            .map(|&(slot, start, count, top, height)| ColumnLineSlice {
                line_range: start..start + count,
                origin: Point {
                    x: (col_x(slot) + paragraph.x).as_px(),
                    y: top.as_px(),
                },
                size: Size {
                    width: paragraph.size.width.as_px(),
                    height: height.as_px(),
                },
            })
            .collect();
        paragraph_splits.push(ParagraphSplitEntry {
            source_node_id: usize::from(paragraph.id),
            column_slices,
        });
    }
    // `paragraph_splits` flows into PDF byte order downstream: keep it in
    // node order, as the previous `BTreeMap` collector did.
    paragraph_splits.sort_by_key(|p| p.source_node_id);

    // `x_offset` / `y_offset` here are in the container's *content-box*
    // frame; `compute_multicol_layout` shifts them into the border-box
    // frame after every segment is placed. Split paragraphs and boxes are
    // recorded on the first row, in its frame.
    let mut paragraph_splits = Some(paragraph_splits);
    let mut box_splits = Some(box_splits);
    let geometries: Vec<ColumnGroupGeometry> = rows
        .iter()
        .zip(&col_heights)
        .map(|(row, heights)| ColumnGroupGeometry {
            x_offset: 0.0_f32.as_px(),
            y_offset: (y_offset + row.top).as_px(),
            col_w: col_w.as_px(),
            gap: gap.as_px(),
            n,
            col_heights: heights
                .iter()
                .copied()
                .map(crate::units::F32Units::as_px)
                .collect(),
            paragraph_splits: paragraph_splits.take().unwrap_or_default(),
            box_splits: box_splits.take().unwrap_or_default(),
        })
        .collect();
    let height = rows
        .last()
        .zip(col_heights.last())
        .map(|(row, heights)| row.top + heights.iter().copied().fold(0.0_f32, f32::max))
        .unwrap_or(0.0);

    (top_level, geometries, height)
}

/// Distribute `line_heights` (per-line total height in CSS px) across
/// `n` columns of `budget` height each.
///
/// Returns one `(line_range, slice_height)` pair per column; ranges are
/// half-open against the input slice index space.
///
/// Greedy fill semantics:
/// - Each column receives lines starting at the previous column's end.
/// - When the next line would push the column over `budget`, advance to
///   the next column.
/// - Exception: a column always receives at least one line even when
///   that line alone exceeds `budget` (CSS monolithic-line rule —
///   otherwise we loop forever and never make progress).
/// - When the column count is exhausted before all lines are placed,
///   the final column absorbs every remaining line. This violates the
///   per-column budget but never silently drops content; the caller
///   ensures the budget is `>= avail_h / n` so this branch fires only on
///   pathological inputs where the multicol container itself overflows
///   the page (handled elsewhere).
fn slice_lines_by_budget(
    line_heights: &[f32],
    budget: f32,
    n: u32,
) -> Vec<(std::ops::Range<usize>, f32)> {
    let n = n.max(1) as usize;
    let mut out = Vec::with_capacity(n);
    let mut start = 0_usize;
    for col in 0..n {
        let mut consumed = 0.0_f32;
        let mut end = start;
        let last_col = col == n - 1;
        while end < line_heights.len() {
            let h = line_heights[end];
            let line_count = end - start;
            // First line in this column always fits (monolithic-line rule).
            // Otherwise stop when adding this line would overflow,
            // unless we are the final column (absorb remainder).
            if line_count > 0 && consumed + h > budget && !last_col {
                break;
            }
            consumed += h;
            end += 1;
        }
        out.push((start..end, consumed));
        start = end;
    }
    out
}

/// fulgur-6q5 Fix 2: returns true when every line of `layout` contains
/// only `GlyphRun` items (no `InlineBox`).
///
/// `convert_multicol_paragraph_slices`'s materialiser
/// (`shape_paragraph_glyph_runs`) only reconstructs `GlyphRun`s when
/// turning `ParagraphSplitEntry` into `ParagraphSlice` — inline-image /
/// inline-block content would be silently dropped at slice render
/// time. Both layout-side split detectors (`layout_column_group` Case
/// B and `layout_self_inline_root_container` Case A) gate split
/// generation on this predicate so paragraphs the convert pass cannot
/// faithfully reproduce fall back to atomic placement (whole paragraph
/// in column 0). Until inline-box reconstruction lands in the convert
/// path, the fallback is a strict improvement over silent data loss.
fn parley_layout_is_glyph_run_only(layout: &parley::Layout<blitz_dom::node::TextBrush>) -> bool {
    for line in layout.lines() {
        for item in line.items() {
            match item {
                parley::PositionedLayoutItem::GlyphRun(_) => continue,
                parley::PositionedLayoutItem::InlineBox(_) => return false,
            }
        }
    }
    true
}

/// Recursively clear Taffy's layout cache for `node_id` and all its DOM
/// descendants. Depth is bounded by `MAX_DOM_DEPTH` to match the rest of
/// the DOM-walking helpers in this module.
fn clear_subtree_cache(doc: &mut BaseDocument, node_id: usize) {
    clear_subtree_cache_inner(doc, node_id, 0);
}

fn clear_subtree_cache_inner(doc: &mut BaseDocument, node_id: usize, depth: usize) {
    if depth >= crate::MAX_DOM_DEPTH {
        return;
    }
    // Single lookup: clear this node's cache and clone its children in one
    // `get_node_mut`, bailing early when the node is absent.
    let children = if let Some(node) = doc.get_node_mut(crate::blitz_adapter::bid(node_id)) {
        if let Some(ld) = node.try_layout_data_mut() {
            ld.cache.clear();
        }
        node.children.clone()
    } else {
        return;
    };
    for child_id in children {
        clear_subtree_cache_inner(doc, crate::blitz_adapter::uid(child_id), depth + 1);
    }
}

/// After a multicol subtree has been re-laid-out with a new height,
/// walk up the ancestor chain and keep the tree's geometry consistent:
///
/// - shift every sibling that comes *after* the multicol (in its
///   parent's child list) downward by the height delta
/// - grow (or shrink) each ancestor's `size.height` by the same delta
///
/// Updates both `unrounded_layout` and `final_layout` so the downstream
/// `convert.rs` reader (which goes through `final_layout`) sees the
/// corrected positions.
fn propagate_height_delta(doc: &mut BaseDocument, node_id: usize, delta: f32) {
    let mut current = node_id;
    while let Some(parent_id) = doc
        .get_node(crate::blitz_adapter::bid(current))
        .and_then(|n| n.parent)
    {
        let siblings_after: Vec<usize> = {
            let Some(parent_node) = doc.get_node(parent_id) else {
                break;
            };
            let Some(idx) = parent_node
                .children
                .iter()
                .position(|&c| c == crate::blitz_adapter::bid(current))
            else {
                break;
            };
            crate::blitz_adapter::uids(&parent_node.children[idx + 1..])
        };
        for sibling in siblings_after {
            if let Some(node) = doc.get_node_mut(crate::blitz_adapter::bid(sibling)) {
                // Text / comment siblings carry no layout in blitz-dom 0.3.
                if let Some(ld) = node.try_layout_data_mut() {
                    ld.unrounded_layout.location.y += delta;
                    ld.final_layout.location.y += delta;
                }
            }
        }
        if let Some(node) = doc.get_node_mut(parent_id) {
            if let Some(ld) = node.try_layout_data_mut() {
                ld.unrounded_layout.size.height += delta;
                ld.final_layout.size.height += delta;
                // Invalidate Taffy's cached layout output for this ancestor;
                // we just mutated its size, so a later layout pass must
                // recompute rather than trust the pre-propagation entry.
                ld.cache.clear();
            }
        }
        current = crate::blitz_adapter::uid(parent_id);
    }
}

/// Walk the tree from the document root collecting every node id whose
/// style makes it a multicol container. Top-down order.
fn collect_multicol_node_ids(doc: &BaseDocument) -> Vec<usize> {
    fn walk(doc: &BaseDocument, id: usize, depth: usize, out: &mut Vec<usize>) {
        if depth >= crate::MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(crate::blitz_adapter::bid(id)) else {
            return;
        };
        if crate::blitz_adapter::is_multicol_container(node) {
            out.push(id);
        }
        for &child in &node.children {
            walk(doc, crate::blitz_adapter::uid(child), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(
        doc,
        crate::blitz_adapter::uid(doc.root_element().id),
        0,
        &mut out,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::blitz_adapter::TaffyDocExt;

    #[test]
    fn column_group_geometry_default_paragraph_splits_is_empty() {
        let g = ColumnGroupGeometry::default();
        assert!(g.paragraph_splits.is_empty());
    }

    #[test]
    fn wrapper_intercepts_multicol_during_taffy_pass() {
        // Prove the custom compute actually fires when Taffy lays out a
        // multicol subtree through our wrapper. A-1b scaffold check only.
        let html = r#"<!doctype html><html><body>
            <p>before</p>
            <div id="mc" style="column-count: 2; column-gap: 10pt;">
              <p>AAA BBB CCC DDD EEE FFF GGG HHH III JJJ KKK</p>
            </div>
            <p>after</p>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        let laid_out = tree.layout_multicol_subtrees();
        assert_eq!(laid_out, 1, "one multicol container expected");
    }

    #[test]
    fn wrapper_leaves_non_multicol_fixture_untouched() {
        let html = r#"<!doctype html><html><body>
            <h1>hello</h1>
            <p>world</p>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        let laid_out = tree.layout_multicol_subtrees();
        assert_eq!(laid_out, 0);
    }

    #[test]
    fn multicol_children_laid_out_in_columns() {
        // After the hook runs, children should be placed in multiple
        // columns. For 4 identical children at column-count=2, we expect
        // 2 children per column (roughly).
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p>alpha alpha alpha alpha alpha</p>
              <p>beta beta beta beta beta</p>
              <p>gamma gamma gamma gamma gamma</p>
              <p>delta delta delta delta delta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        let laid_out = tree.layout_multicol_subtrees();
        assert_eq!(laid_out, 1);

        // Read back child positions from Taffy storage.
        let mc_ids = collect_multicol_node_ids(&doc);
        let mc_id = NodeId::from(mc_ids[0]);
        let child_count = doc.child_count(mc_id);
        let mut xs_by_child: Vec<f32> = Vec::new();
        for i in 0..child_count {
            let child_id = doc.get_child_id(mc_id, i);
            let layout = doc.get_unrounded_layout(child_id);
            xs_by_child.push(layout.location.x);
        }
        // At least two distinct x positions should appear (one per column).
        let mut unique_xs: Vec<f32> = xs_by_child.clone();
        unique_xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        unique_xs.dedup_by(|a, b| (*a - *b).abs() < 0.1);
        assert!(
            unique_xs.len() >= 2,
            "expected children across ≥2 column x positions, got {:?}",
            xs_by_child
        );
    }

    #[test]
    fn multicol_reports_balanced_height_not_single_column_total() {
        // Balance: total content of ~4 lines × 20pt = 80pt. At n=2, budget
        // ≈ 40pt so balanced container ≈ 40pt. We just assert the container
        // shrinks compared to the pre-hook (blitz-assigned) height.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p>alpha</p>
              <p>beta</p>
              <p>gamma</p>
              <p>delta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let mc_id_raw = collect_multicol_node_ids(&doc)[0];
        let mc_node_id = NodeId::from(mc_id_raw);
        let pre_hook_height = doc.get_unrounded_layout(mc_node_id).size.height;

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let post_hook_height = doc.get_unrounded_layout(mc_node_id).size.height;
        assert!(
            post_hook_height < pre_hook_height,
            "balanced height ({post_hook_height}) should be smaller than blitz's single-column total ({pre_hook_height})"
        );
    }

    #[test]
    fn siblings_after_multicol_get_repositioned_by_height_delta() {
        // Sanity check for the structural fix that v1 couldn't solve: the
        // "after" paragraph should sit BELOW the balanced multicol box,
        // not overlap its columns. The multicol balances shorter than
        // blitz's single-column estimate, so the delta is negative and
        // siblings move up.
        let html = r#"<!doctype html><html><body>
            <p id="before">before</p>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p>alpha alpha alpha alpha</p>
              <p>beta beta beta beta</p>
              <p>gamma gamma gamma gamma</p>
              <p>delta delta delta delta</p>
            </div>
            <p id="after">after</p>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        // Find nodes by element id for readability in the assertion.
        fn find_by_id(doc: &BaseDocument, id: &str) -> Option<usize> {
            fn walk(doc: &BaseDocument, node_id: usize, target: &str) -> Option<usize> {
                let node = doc.get_node(crate::blitz_adapter::bid(node_id))?;
                if let Some(ed) = node.element_data() {
                    if let Some(attr_id) = ed.attrs().iter().find(|a| (&a.name.local) == "id") {
                        if attr_id.value.as_str() == target {
                            return Some(node_id);
                        }
                    }
                }
                for &child in &node.children {
                    if let Some(found) = walk(doc, crate::blitz_adapter::uid(child), target) {
                        return Some(found);
                    }
                }
                None
            }
            walk(doc, crate::blitz_adapter::uid(doc.root_element().id), id)
        }

        let mc_id = find_by_id(&doc, "mc").expect("multicol node");
        let after_id = find_by_id(&doc, "after").expect("after paragraph");

        let mc_y_before = doc
            .get_node(crate::blitz_adapter::bid(mc_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .location
            .y;
        let mc_h_before = doc
            .get_node(crate::blitz_adapter::bid(mc_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .size
            .height;
        let mc_bottom_before = mc_y_before + mc_h_before;
        let after_y_before = doc
            .get_node(crate::blitz_adapter::bid(after_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .location
            .y;
        // Assert blitz stacked them in order with no overlap. CSS margins
        // can widen the gap, so just require `after` to start at or below
        // multicol's bottom.
        assert!(
            after_y_before >= mc_bottom_before - 0.5,
            "sanity: after must not overlap multicol (y={after_y_before}, mc_bottom={mc_bottom_before})"
        );

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let mc_h_after = doc
            .get_node(crate::blitz_adapter::bid(mc_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .size
            .height;
        let mc_bottom_after = doc
            .get_node(crate::blitz_adapter::bid(mc_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .location
            .y
            + mc_h_after;
        let after_y_after = doc
            .get_node(crate::blitz_adapter::bid(after_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .location
            .y;

        // After balance, the multicol is shorter and the sibling below
        // it has moved up by the same delta (propagate_height_delta).
        assert!(
            mc_h_after < mc_h_before,
            "multicol height should shrink after balance: before={mc_h_before}, after={mc_h_after}"
        );
        let delta_h = mc_h_after - mc_h_before; // negative when multicol shrinks
        let expected_after_y = after_y_before + delta_h;
        assert!(
            (after_y_after - expected_after_y).abs() < 0.5,
            "propagation: after_y expected {expected_after_y}, got {after_y_after} (mc_bottom_after={mc_bottom_after})"
        );
    }

    #[test]
    fn wrapper_intercepts_nested_multicol_from_outer_subtree() {
        // Taffy recursing through our wrapper from the OUTER multicol
        // subtree should also catch a nested multicol inside.
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2;">
              <div id="inner" style="column-count: 3;">
                <p>AAA BBB CCC DDD</p>
              </div>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        let laid_out = tree.layout_multicol_subtrees();
        assert_eq!(laid_out, 2);
    }

    // ── resolve_column_layout: count only ───────────────────────────
    #[test]
    fn resolve_count_only_three_columns() {
        let (n, w) = resolve_column_layout(300.0, Some(3), None, 10.0);
        assert_eq!(n, 3);
        assert!((w - 93.333_33).abs() < 1e-3, "got {w}");
    }

    #[test]
    fn resolve_count_only_one_column_no_gap_subtraction() {
        let (n, w) = resolve_column_layout(400.0, Some(1), None, 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 400.0);
    }

    #[test]
    fn resolve_count_only_zero_clamps_to_one() {
        let (n, w) = resolve_column_layout(400.0, Some(0), None, 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 400.0);
    }

    #[test]
    fn resolve_count_only_huge_count_is_defensively_capped() {
        // An attacker-supplied `column-count` up to i32::MAX must not become
        // the used count: it multiplies retained layout metadata (col_heights,
        // per-split column_slices). It is clamped to MAX_COLUMN_COUNT; the
        // columns collapse to zero width, as they already would past capacity.
        let (n, w) = resolve_column_layout(400.0, Some(2_000_000_000), None, 10.0);
        assert_eq!(n, crate::MAX_COLUMN_COUNT);
        assert_eq!(w, 0.0);
    }

    // ── resolve_column_layout: width only ───────────────────────────
    #[test]
    fn resolve_width_only_derives_count() {
        // (400 + 10) / (180 + 10) = 2.157 → floor = 2
        let (n, w) = resolve_column_layout(400.0, None, Some(180.0), 10.0);
        assert_eq!(n, 2);
        // (400 - 10) / 2 = 195
        assert!((w - 195.0).abs() < 1e-3);
    }

    #[test]
    fn resolve_width_only_too_wide_collapses_to_one() {
        let (n, w) = resolve_column_layout(200.0, None, Some(400.0), 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 200.0);
    }

    #[test]
    fn resolve_width_only_zero_gap() {
        let (n, w) = resolve_column_layout(300.0, None, Some(100.0), 0.0);
        assert_eq!(n, 3);
        assert!((w - 100.0).abs() < 1e-3);
    }

    #[test]
    fn resolve_width_only_huge_derived_count_is_defensively_capped() {
        // A tiny `column-width` on a very wide container derives an
        // unbounded fits_count; the same MAX_COLUMN_COUNT clamp inside
        // `capped` must bound this width-derived path too, not just the
        // explicit-count path.
        let (n, _w) = resolve_column_layout(1_000_000.0, None, Some(1.0), 0.0);
        assert_eq!(n, crate::MAX_COLUMN_COUNT);
    }

    // ── resolve_column_layout: both present ─────────────────────────
    #[test]
    fn resolve_both_count_wins_when_narrower() {
        // count=2 vs width-derived-max = floor((600+10)/(100+10)) = 5 → 2 used.
        let (n, w) = resolve_column_layout(600.0, Some(2), Some(100.0), 10.0);
        assert_eq!(n, 2);
        assert!((w - 295.0).abs() < 1e-3);
    }

    #[test]
    fn resolve_both_width_cap_wins_when_count_too_high() {
        let (n, w) = resolve_column_layout(400.0, Some(10), Some(180.0), 10.0);
        assert_eq!(n, 2);
        assert!((w - 195.0).abs() < 1e-3);
    }

    // ── resolve_column_layout: edge cases ───────────────────────────
    #[test]
    fn resolve_neither_present_falls_back_to_single_column() {
        let (n, w) = resolve_column_layout(400.0, None, None, 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 400.0);
    }

    #[test]
    fn resolve_zero_content_width_never_produces_negative() {
        let (n, w) = resolve_column_layout(0.0, Some(3), None, 10.0);
        assert_eq!(n, 3);
        assert!(w >= 0.0, "column width must be clamped non-negative");
    }

    #[test]
    fn resolve_gap_exceeds_content_width_clamps_col_width_to_zero() {
        let (n, w) = resolve_column_layout(50.0, Some(3), None, 40.0);
        assert_eq!(n, 3);
        assert_eq!(w, 0.0);
    }

    #[test]
    fn resolve_width_zero_degenerates_safely() {
        // column-width: 0 would divide by gap only; guard against it.
        let (n, w) = resolve_column_layout(300.0, None, Some(0.0), 10.0);
        assert_eq!(n, 1);
        assert_eq!(w, 300.0);
    }

    // ── flow units: margins, placement, balancing ────────────────────
    fn line(height: f32, gap_before: f32) -> FlowUnit {
        FlowUnit {
            kind: FlowUnitKind::Line(0),
            height,
            gap_before,
            lead: 0.0,
            trail: 0.0,
            keep_with_previous: false,
            overhang: 0.0,
        }
    }

    #[test]
    fn collapsed_margin_takes_largest_positive_plus_most_negative() {
        let mut m = CollapsedMargin::default();
        m.add(10.0);
        m.add(16.0);
        assert_eq!(m.value(), 16.0);
        m.add(-4.0);
        m.add(-6.0);
        assert_eq!(m.value(), 10.0);
    }

    #[test]
    fn place_units_keeps_margins_inside_a_column_and_drops_them_after_a_break() {
        // Two paragraphs of two 10px lines, 5px apart, in columns of 25px.
        let units = [
            line(10.0, 5.0),
            line(10.0, 0.0),
            line(10.0, 5.0),
            line(10.0, 0.0),
        ];
        let tops = place_units(&units, 2, 25.0, true).unwrap();
        // The first column keeps its leading margin: 5, 15. The third line
        // (5 + 10 = 15 → 25 + 5 + 10 = 40 > 25) moves to column 1, where
        // the margin before it is dropped.
        assert_eq!(tops, vec![(0, 5.0), (0, 15.0), (1, 0.0), (1, 10.0)]);
    }

    #[test]
    fn place_units_strict_fails_when_columns_run_out_and_lenient_overflows() {
        let units = [line(10.0, 0.0), line(10.0, 0.0), line(10.0, 0.0)];
        assert!(place_units(&units, 1, 15.0, true).is_none());
        let tops = place_units(&units, 1, 15.0, false).unwrap();
        assert_eq!(tops, vec![(0, 0.0), (0, 10.0), (0, 20.0)]);
    }

    #[test]
    fn place_units_puts_an_oversized_unit_alone_in_a_column() {
        let units = [line(10.0, 0.0), line(50.0, 0.0), line(10.0, 0.0)];
        let tops = place_units(&units, 3, 20.0, true).unwrap();
        assert_eq!(tops, vec![(0, 0.0), (1, 0.0), (2, 0.0)]);
    }

    #[test]
    fn balanced_budget_splits_lines_evenly() {
        // 21 lines of 22px in 3 columns: 7 lines each.
        let units: Vec<FlowUnit> = (0..21).map(|_| line(22.0, 0.0)).collect();
        let budget = balanced_budget(&units, 3);
        assert!((budget - 154.0).abs() < 0.1, "{budget}");
    }

    #[test]
    fn balanced_budget_accounts_for_margins_and_breaks() {
        // Seven lines, then a 10px margin, then seven more. In two columns
        // the margin falls at the break and is dropped: 7 lines per column.
        let mut units: Vec<FlowUnit> = (0..7).map(|_| line(20.0, 0.0)).collect();
        units.extend((0..7).map(|i| line(20.0, if i == 0 { 10.0 } else { 0.0 })));
        let budget = balanced_budget(&units, 2);
        assert!((budget - 140.0).abs() < 0.1, "{budget}");
        let tops = place_units(&units, 2, budget, true).unwrap();
        assert_eq!(tops[7], (1, 0.0));
    }

    #[test]
    fn balanced_budget_of_nothing_is_zero() {
        assert_eq!(balanced_budget(&[], 3), 0.0);
        assert_eq!(balanced_budget(&[line(0.0, 0.0)], 3), 0.0);
    }

    // ── slice_lines_by_budget ───────────────────────────────────────
    #[test]
    fn slice_lines_by_budget_short_paragraph_one_slice() {
        // Heights [10, 10, 10] — total 30, budget 100, n=2.
        // All three lines fit in column 0; column 1 stays empty.
        let heights = [10.0_f32, 10.0, 10.0];
        let slices = slice_lines_by_budget(&heights, /*budget*/ 100.0, /*n*/ 2);
        assert_eq!(slices.len(), 2);
        assert_eq!(slices[0], (0..3, 30.0));
        assert_eq!(slices[1], (3..3, 0.0));
    }

    #[test]
    fn slice_lines_by_budget_splits_at_budget_boundary() {
        // 10 lines × 10pt = 100pt total, budget 50, n=2.
        // 5 lines per column.
        let heights = [10.0_f32; 10];
        let slices = slice_lines_by_budget(&heights, 50.0, 2);
        assert_eq!(slices.len(), 2);
        assert_eq!(slices[0], (0..5, 50.0));
        assert_eq!(slices[1], (5..10, 50.0));
    }

    #[test]
    fn slice_lines_by_budget_admits_overflow_when_one_line_exceeds_budget() {
        // The first line exceeds the budget — we must not loop forever.
        // Spec: monolithic line, allowed to overflow.
        let heights = [80.0_f32, 20.0, 20.0];
        let slices = slice_lines_by_budget(&heights, 50.0, 2);
        assert_eq!(slices[0], (0..1, 80.0));
        assert_eq!(slices[1], (1..3, 40.0));
    }

    #[test]
    fn slice_lines_by_budget_overflow_into_unavailable_columns_truncates() {
        // 5 lines × 20pt = 100pt total, budget 30pt, n=2 → only 60pt of
        // capacity. Last two lines have nowhere to go; the helper assigns
        // them to the final column (overflow allowed) so we never silently
        // drop content.
        let heights = [20.0_f32; 5];
        let slices = slice_lines_by_budget(&heights, 30.0, 2);
        assert_eq!(slices[0], (0..1, 20.0)); // 1 line fits in 30pt
        assert_eq!(slices[1].0.start, 1); // last column absorbs the rest
        assert_eq!(slices[1].0.end, 5);
    }

    #[test]
    fn slice_lines_by_budget_empty_input_returns_n_empty_slices() {
        let slices = slice_lines_by_budget(&[], 100.0, 3);
        assert_eq!(slices.len(), 3);
        for (range, h) in &slices {
            assert!(range.is_empty());
            assert_eq!(*h, 0.0);
        }
    }

    // ── propagate_height_delta: edge cases ──────────────────────────
    /// Walk a document and dump (node_id, y, height) for every node with
    /// a non-zero final_layout size. Used for delta-propagation assertions.
    fn node_layouts(doc: &BaseDocument) -> Vec<(usize, f32, f32)> {
        let mut out = Vec::new();
        fn walk(doc: &BaseDocument, id: usize, out: &mut Vec<(usize, f32, f32)>) {
            let Some(node) = doc.get_node(crate::blitz_adapter::bid(id)) else {
                return;
            };
            let sz = node.unrounded_layout_or_zero().size;
            if sz.width > 0.0 || sz.height > 0.0 {
                out.push((id, node.unrounded_layout_or_zero().location.y, sz.height));
            }
            for &c in &node.children {
                walk(doc, crate::blitz_adapter::uid(c), out);
            }
        }
        walk(
            doc,
            crate::blitz_adapter::uid(doc.root_element().id),
            &mut out,
        );
        out
    }

    #[test]
    fn propagate_delta_zero_is_no_op() {
        let html = r#"<!doctype html><html><body>
            <p>before</p><p>after</p>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let before = node_layouts(&doc);
        let root_id = doc.root_element().id;
        propagate_height_delta(&mut doc, crate::blitz_adapter::uid(root_id), 0.0);
        assert_eq!(node_layouts(&doc), before);
    }

    #[test]
    fn propagate_delta_stops_at_root_without_panicking() {
        // The document root has no parent → loop exits on the first
        // `.parent` lookup without mutating anything.
        let html = r#"<!doctype html><html><body><p>x</p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let before = node_layouts(&doc);
        let root_id = doc.root_element().id;
        propagate_height_delta(&mut doc, crate::blitz_adapter::uid(root_id), 10.0);
        assert_eq!(node_layouts(&doc), before);
    }

    #[test]
    fn propagate_delta_leaves_earlier_siblings_alone() {
        // "before" comes BEFORE the multicol; its y must not change when
        // the multicol's height shrinks.
        let html = r#"<!doctype html><html><body>
            <p id="before">before</p>
            <div id="mc" style="column-count: 2;">
              <p>a</p><p>b</p><p>c</p><p>d</p>
            </div>
            <p id="after">after</p>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let before_id = {
            let mut found = None;
            fn walk(doc: &BaseDocument, id: usize, out: &mut Option<usize>) {
                if out.is_some() {
                    return;
                }
                let Some(node) = doc.get_node(crate::blitz_adapter::bid(id)) else {
                    return;
                };
                if let Some(ed) = node.element_data()
                    && ed
                        .attrs()
                        .iter()
                        .any(|a| (&a.name.local) == "id" && a.value.as_str() == "before")
                {
                    *out = Some(id);
                    return;
                }
                for &c in &node.children {
                    walk(doc, crate::blitz_adapter::uid(c), out);
                }
            }
            walk(
                &doc,
                crate::blitz_adapter::uid(doc.root_element().id),
                &mut found,
            );
            found.expect("before node")
        };

        let before_y_pre = doc
            .get_node(crate::blitz_adapter::bid(before_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .location
            .y;

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let before_y_post = doc
            .get_node(crate::blitz_adapter::bid(before_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .location
            .y;
        assert!(
            (before_y_pre - before_y_post).abs() < 0.01,
            "earlier sibling y should not move: pre={before_y_pre}, post={before_y_post}"
        );
    }

    // ── partition_children_into_segments ────────────────────────────

    fn parse_multicol(html: &str) -> (blitz_dom::BaseDocument, usize) {
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let doc: blitz_dom::BaseDocument = doc.into();
        let mc_id = collect_multicol_node_ids(&doc)[0];
        (doc, mc_id)
    }

    #[test]
    fn partition_all_children_are_columnar() {
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2;">
              <p>a</p><p>b</p><p>c</p>
            </div>
        </body></html>"#;
        let (doc, mc_id) = parse_multicol(html);
        let segments = partition_children_into_segments(&doc, mc_id);
        assert_eq!(segments.len(), 1);
        match &segments[0] {
            Segment::ColumnGroup(ids) => assert_eq!(ids.len(), 3),
            _ => panic!("expected ColumnGroup"),
        }
    }

    #[test]
    fn partition_span_at_top() {
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2;">
              <h1 style="column-span: all;">title</h1>
              <p>a</p><p>b</p>
            </div>
        </body></html>"#;
        let (doc, mc_id) = parse_multicol(html);
        let segments = partition_children_into_segments(&doc, mc_id);
        assert_eq!(segments.len(), 2);
        assert!(matches!(&segments[0], Segment::SpanAll(_)));
        match &segments[1] {
            Segment::ColumnGroup(ids) => assert_eq!(ids.len(), 2),
            _ => panic!("expected ColumnGroup"),
        }
    }

    #[test]
    fn partition_span_in_middle() {
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2;">
              <p>a</p>
              <h1 style="column-span: all;">title</h1>
              <p>b</p>
            </div>
        </body></html>"#;
        let (doc, mc_id) = parse_multicol(html);
        let segments = partition_children_into_segments(&doc, mc_id);
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[0], Segment::ColumnGroup(_)));
        assert!(matches!(&segments[1], Segment::SpanAll(_)));
        assert!(matches!(&segments[2], Segment::ColumnGroup(_)));
    }

    #[test]
    fn partition_span_at_end() {
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2;">
              <p>a</p><p>b</p>
              <h1 style="column-span: all;">title</h1>
            </div>
        </body></html>"#;
        let (doc, mc_id) = parse_multicol(html);
        let segments = partition_children_into_segments(&doc, mc_id);
        assert_eq!(segments.len(), 2);
        match &segments[0] {
            Segment::ColumnGroup(ids) => assert_eq!(ids.len(), 2),
            _ => panic!("expected ColumnGroup"),
        }
        assert!(matches!(&segments[1], Segment::SpanAll(_)));
    }

    #[test]
    fn partition_two_consecutive_spans() {
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2;">
              <h1 style="column-span: all;">t1</h1>
              <h2 style="column-span: all;">t2</h2>
              <p>a</p>
            </div>
        </body></html>"#;
        let (doc, mc_id) = parse_multicol(html);
        let segments = partition_children_into_segments(&doc, mc_id);
        assert_eq!(segments.len(), 3);
        assert!(matches!(&segments[0], Segment::SpanAll(_)));
        assert!(matches!(&segments[1], Segment::SpanAll(_)));
        assert!(matches!(&segments[2], Segment::ColumnGroup(_)));
    }

    #[test]
    fn partition_nested_span_is_ignored() {
        // column-span: all is evaluated only on direct children of the
        // multicol container. A <span style="column-span: all"> buried inside
        // a non-span <p> must NOT split the ColumnGroup.
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2;">
              <p>a <span style="column-span: all;">inline</span> tail</p>
              <p>b</p>
            </div>
        </body></html>"#;
        let (doc, mc_id) = parse_multicol(html);
        let segments = partition_children_into_segments(&doc, mc_id);
        assert_eq!(segments.len(), 1);
        match &segments[0] {
            Segment::ColumnGroup(ids) => assert_eq!(ids.len(), 2),
            _ => panic!("expected ColumnGroup"),
        }
    }

    #[test]
    fn partition_keeps_direct_inline_content_in_column_group() {
        // Regression for CodeRabbit review on PR #125: direct non-element
        // children (inline text + inline elements) must not be silently
        // dropped from segmentation. Pure HTML-formatting whitespace text
        // is filtered, but real content text and inline elements stay in
        // the ColumnGroup so their layout is preserved.
        let html = r#"<!doctype html><html><body><div style="column-count:2">hello <span>world</span></div></body></html>"#;
        let (doc, mc_id) = parse_multicol(html);
        let segments = partition_children_into_segments(&doc, mc_id);
        assert_eq!(segments.len(), 1);
        match &segments[0] {
            Segment::ColumnGroup(ids) => {
                assert_eq!(
                    ids.len(),
                    2,
                    "expected the 'hello ' text node AND the <span> to survive partition"
                );
            }
            _ => panic!("expected ColumnGroup"),
        }
    }

    #[test]
    fn propagate_delta_walks_multiple_ancestor_levels() {
        // Nested structure so the propagation pass has to walk through
        // multiple levels of parent containers. Both the outer wrapper
        // and the root-element should absorb the multicol's height delta.
        let html = r#"<!doctype html><html><body>
            <div id="outer">
              <div id="mc" style="column-count: 2;">
                <p>a</p><p>b</p><p>c</p><p>d</p>
              </div>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let outer_id = {
            let mut found = None;
            fn walk(doc: &BaseDocument, id: usize, out: &mut Option<usize>) {
                if out.is_some() {
                    return;
                }
                let Some(node) = doc.get_node(crate::blitz_adapter::bid(id)) else {
                    return;
                };
                if let Some(ed) = node.element_data()
                    && ed
                        .attrs()
                        .iter()
                        .any(|a| (&a.name.local) == "id" && a.value.as_str() == "outer")
                {
                    *out = Some(id);
                    return;
                }
                for &c in &node.children {
                    walk(doc, crate::blitz_adapter::uid(c), out);
                }
            }
            walk(
                &doc,
                crate::blitz_adapter::uid(doc.root_element().id),
                &mut found,
            );
            found.expect("outer div")
        };

        let outer_h_pre = doc
            .get_node(crate::blitz_adapter::bid(outer_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .size
            .height;

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let outer_h_post = doc
            .get_node(crate::blitz_adapter::bid(outer_id))
            .unwrap()
            .unrounded_layout_or_zero()
            .size
            .height;
        assert!(
            (outer_h_pre - outer_h_post).abs() > 0.1,
            "the multicol's ancestor should have absorbed the height delta: \
             pre={outer_h_pre}, post={outer_h_post}"
        );
    }

    #[test]
    fn layout_column_group_matches_legacy_flat_balance() {
        // Baseline: a container with no column-span: all children, laid out
        // through the new `layout_column_group` helper, must produce the
        // same placements as the current compute_multicol_layout.
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2; column-gap: 0;">
              <p>alpha alpha alpha alpha</p>
              <p>beta beta beta beta</p>
              <p>gamma gamma gamma gamma</p>
              <p>delta delta delta delta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        // Sanity: two distinct x positions exist after the refactor.
        let mc_id = collect_multicol_node_ids(&doc)[0];
        let mc_node_id = NodeId::from(mc_id);
        let child_count = doc.child_count(mc_node_id);
        let mut xs: Vec<f32> = (0..child_count)
            .map(|i| {
                doc.get_unrounded_layout(doc.get_child_id(mc_node_id, i))
                    .location
                    .x
            })
            .collect();
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        xs.dedup_by(|a, b| (*a - *b).abs() < 0.1);
        assert!(xs.len() >= 2, "expected ≥2 column x positions");
    }

    // ── segment dispatch inside compute_multicol_layout ─────────────

    /// Find the layout of a child node of the multicol container by its DOM id.
    fn layout_of_id(doc: &BaseDocument, html_id: &str) -> taffy::Layout {
        fn walk(doc: &BaseDocument, node_id: usize, target: &str) -> Option<usize> {
            let node = doc.get_node(crate::blitz_adapter::bid(node_id))?;
            if let Some(ed) = node.element_data()
                && ed
                    .attrs()
                    .iter()
                    .any(|a| (&a.name.local) == "id" && a.value.as_str() == target)
            {
                return Some(node_id);
            }
            for &c in &node.children {
                if let Some(found) = walk(doc, crate::blitz_adapter::uid(c), target) {
                    return Some(found);
                }
            }
            None
        }
        let id = walk(
            doc,
            crate::blitz_adapter::uid(doc.root_element().id),
            html_id,
        )
        .expect("id not found");
        doc.get_unrounded_layout(NodeId::from(id))
    }

    #[test]
    fn span_all_occupies_full_container_width() {
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p id="before">before</p>
              <h1 id="title" style="column-span: all; margin: 0">title</h1>
              <p id="after">after</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let mc_w = layout_of_id(&doc, "mc").size.width;
        let title = layout_of_id(&doc, "title");
        assert!(
            (title.size.width - mc_w).abs() < 0.5,
            "SpanAll width {} should match container width {}",
            title.size.width,
            mc_w
        );
        assert!(
            title.location.x.abs() < 0.5,
            "SpanAll should start at x=0, got {}",
            title.location.x
        );
    }

    #[test]
    fn segments_stack_vertically() {
        // segment 0 (ColumnGroup with 'before')
        // segment 1 (SpanAll 'title')
        // segment 2 (ColumnGroup with 'after')
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p id="before">before</p>
              <h1 id="title" style="column-span: all; margin: 0">title</h1>
              <p id="after">after</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let before = layout_of_id(&doc, "before");
        let title = layout_of_id(&doc, "title");
        let after = layout_of_id(&doc, "after");

        assert!(
            title.location.y + 0.5 >= before.location.y + before.size.height,
            "title ({}) must start at or below 'before' bottom ({})",
            title.location.y,
            before.location.y + before.size.height
        );
        assert!(
            after.location.y + 0.5 >= title.location.y + title.size.height,
            "after ({}) must start at or below 'title' bottom ({})",
            after.location.y,
            title.location.y + title.size.height
        );
    }

    #[test]
    fn span_at_top_produces_one_segment_below() {
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <h1 id="title" style="column-span: all; margin: 0">title</h1>
              <p id="a">a</p>
              <p id="b">b</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let title = layout_of_id(&doc, "title");
        let a = layout_of_id(&doc, "a");
        let b = layout_of_id(&doc, "b");

        let title_bottom = title.location.y + title.size.height;
        assert!(a.location.y + 0.5 >= title_bottom);
        assert!(b.location.y + 0.5 >= title_bottom);

        assert!(
            (a.location.x - b.location.x).abs() > 0.5,
            "a.x={} b.x={} should be in different columns",
            a.location.x,
            b.location.x
        );
    }

    #[test]
    fn span_at_end_sits_below_columns() {
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p id="a">a</p>
              <p id="b">b</p>
              <h1 id="title" style="column-span: all; margin: 0">title</h1>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let a = layout_of_id(&doc, "a");
        let b = layout_of_id(&doc, "b");
        let title = layout_of_id(&doc, "title");
        let col_bottom = (a.location.y + a.size.height).max(b.location.y + b.size.height);
        assert!(
            title.location.y + 0.5 >= col_bottom,
            "title ({}) must sit below column bottom ({})",
            title.location.y,
            col_bottom
        );
    }

    #[test]
    fn nested_span_does_not_break_column_layout() {
        // A descendant span with column-span: all deep inside a non-span
        // direct child must NOT create a segment break.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p id="a">a <span style="column-span: all;">inline</span> tail</p>
              <p id="b">b</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let a = layout_of_id(&doc, "a");
        let b = layout_of_id(&doc, "b");
        let mc_w = layout_of_id(&doc, "mc").size.width;
        assert!(
            a.size.width < mc_w * 0.9,
            "'a' width {} looks like full container width {} — nested span leaked out",
            a.size.width,
            mc_w
        );
        assert!(
            b.size.width < mc_w * 0.9,
            "'b' width {} looks like full container width {}",
            b.size.width,
            mc_w
        );
    }

    // ── multicol container padding ──────────────────────────────────

    #[test]
    fn multicol_container_padding_offsets_children_into_content_box() {
        // Regression for fulgur-4d3g: `.mc { padding: 6px 10px }` was being
        // ignored — children landed at (0, 0) relative to the container's
        // border-box, their column tracks spanned the full border-box width,
        // and the container height was purely content-driven with no padding
        // contribution. Reference examples: examples/multicol,
        // examples/multicol-span-all.
        let html = r#"<!doctype html><html><body><style>p { margin: 0 }</style>
            <div id="mc" style="column-count: 2; column-gap: 0; padding: 6px 10px;">
              <p id="a">alpha alpha alpha alpha</p>
              <p id="b">beta beta beta beta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let a = layout_of_id(&doc, "a");
        let b = layout_of_id(&doc, "b");
        let mc = layout_of_id(&doc, "mc");

        // First column starts at padding-left (10px), second at 10px + col_w.
        assert!(
            (a.location.x - 10.0).abs() < 0.5,
            "first-column child.x expected ≈ 10 (padding-left), got {}",
            a.location.x
        );
        // First child also starts at padding-top (6px).
        assert!(
            (a.location.y - 6.0).abs() < 0.5,
            "first child.y expected ≈ 6 (padding-top), got {}",
            a.location.y
        );
        // Second child is in the second column — x must be strictly greater
        // than the padding edge (padding-left + col_w).
        assert!(
            b.location.x > a.location.x + 50.0,
            "second-column child should sit right of first: a.x={}, b.x={}",
            a.location.x,
            b.location.x
        );

        // Column track width: (border-box − 2 × padding-x) / columns. Derive
        // the expected value from the realised container width so body margin
        // defaults don't skew the assertion.
        let expected_col_w = (mc.size.width - 20.0) / 2.0;
        assert!(
            (a.size.width - expected_col_w).abs() < 1.0,
            "col_w expected ≈ {expected_col_w} ((mc.width {} − 2×10) / 2), got {}",
            mc.size.width,
            a.size.width
        );

        // Container height must include padding-top + padding-bottom.
        let col_bottom_in_content =
            (a.location.y + a.size.height).max(b.location.y + b.size.height);
        // Expected container height ≥ bottom-of-content + padding-bottom (6px).
        // bottom-of-content already includes padding-top because child.y did.
        assert!(
            mc.size.height >= col_bottom_in_content + 6.0 - 0.5,
            "container height {} should include padding-bottom (content bottom {} + 6)",
            mc.size.height,
            col_bottom_in_content
        );
    }

    #[test]
    fn multicol_container_border_offsets_children_and_contributes_height() {
        let html = r#"<!doctype html><html><body><style>p { margin: 0 }</style>
            <div id="mc" style="column-count: 2; column-gap: 0; border: 5px solid black;">
              <p id="a">alpha alpha alpha alpha</p>
              <p id="b">beta beta beta beta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let a = layout_of_id(&doc, "a");
        let b = layout_of_id(&doc, "b");
        let mc = layout_of_id(&doc, "mc");

        assert!(
            (a.location.x - 5.0).abs() < 0.5,
            "first-column child.x expected ≈ 5 (border-left), got {}",
            a.location.x
        );
        assert!(
            (a.location.y - 5.0).abs() < 0.5,
            "first child.y expected ≈ 5 (border-top), got {}",
            a.location.y
        );
        assert!(
            b.location.x > a.location.x + 50.0,
            "second-column child should sit right of first: a.x={}, b.x={}",
            a.location.x,
            b.location.x
        );
        assert!(
            (mc.border.left - 5.0).abs() < 0.5 && (mc.border.top - 5.0).abs() < 0.5,
            "container border metrics should be preserved for painting, got left={} top={}",
            mc.border.left,
            mc.border.top
        );

        let expected_col_w = (mc.size.width - 10.0) / 2.0;
        assert!(
            (a.size.width - expected_col_w).abs() < 1.0,
            "col_w expected ≈ {expected_col_w} ((mc.width {} − 2×5) / 2), got {}",
            mc.size.width,
            a.size.width
        );

        let col_bottom = (a.location.y + a.size.height).max(b.location.y + b.size.height);
        assert!(
            mc.size.height >= col_bottom + 5.0 - 0.5,
            "container height {} should include border-bottom (content bottom {} + 5)",
            mc.size.height,
            col_bottom
        );
    }

    #[test]
    fn multicol_span_all_respects_container_padding() {
        // The SpanAll branch must honour the multicol's own padding too —
        // a column-span: all heading should start at padding-left, not at
        // the border edge.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0; padding: 8px 10px;">
              <h1 id="title" style="column-span: all; margin: 0">title</h1>
              <p id="a">a</p>
              <p id="b">b</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let title = layout_of_id(&doc, "title");
        let mc_w = layout_of_id(&doc, "mc").size.width;
        assert!(
            (title.location.x - 10.0).abs() < 0.5,
            "SpanAll x expected ≈ 10 (padding-left), got {}",
            title.location.x
        );
        assert!(
            (title.location.y - 8.0).abs() < 0.5,
            "SpanAll y expected ≈ 8 (padding-top), got {}",
            title.location.y
        );
        // SpanAll width must equal content width (border-box − 2 × padding-x = 380).
        assert!(
            (title.size.width - (mc_w - 20.0)).abs() < 1.0,
            "SpanAll width {} should match content width {}",
            title.size.width,
            mc_w - 20.0
        );
    }

    #[test]
    fn multicol_percentage_padding_resolves_against_parent_width() {
        // Regression for coderabbit review on PR #129: the hook used to
        // read `unrounded_layout.padding` (resolved against whatever width
        // the previous pass happened to assign). For a multicol using
        // `%` / `calc()` padding, that value is stale whenever a later
        // pass changes the container's width — notably nested multicol
        // where the inner container runs at `col_w` of the outer, not the
        // outer's full width. Re-resolve from style against the current
        // `parent_size.width` basis.
        //
        // Zero the body margin so the containing block basis is a clean
        // 400px, then `.mc { padding: 10% }` = 40px on every side.
        let html = r#"<!doctype html><html><body style="margin:0"><style>p { margin: 0 }</style>
            <div id="mc" style="column-count: 2; column-gap: 0; padding: 10%;">
              <p id="a">alpha</p>
              <p id="b">beta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let a = layout_of_id(&doc, "a");
        assert!(
            (a.location.x - 40.0).abs() < 1.0,
            "first-column child.x expected ≈ 40 (10% of 400px containing block), got {}",
            a.location.x
        );
        assert!(
            (a.location.y - 40.0).abs() < 1.0,
            "first child.y expected ≈ 40 (10% of 400px), got {}",
            a.location.y
        );
    }

    #[test]
    fn multicol_preserves_child_padding_on_writeback() {
        // Regression: when compute_multicol_layout wrote each child's final
        // Layout back to Taffy, it overwrote `padding` / `border` with
        // `Rect::zero()`. Children with their own padding lost it, producing
        // wrong content-inset when the renderer later read Layout.padding.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p id="a" style="padding: 5px 8px;">alpha</p>
              <p id="b">beta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let a = layout_of_id(&doc, "a");
        assert!(
            (a.padding.left - 8.0).abs() < 0.5,
            "child padding-left expected 8, got {}",
            a.padding.left
        );
        assert!(
            (a.padding.top - 5.0).abs() < 0.5,
            "child padding-top expected 5, got {}",
            a.padding.top
        );
    }

    // ── MulticolGeometry recording (Task 3) ─────────────────────────

    #[test]
    fn column_group_geometry_records_heights_matching_layout() {
        // Minimal multicol fixture: 4 roughly-equal paragraphs balanced into
        // 2 columns with no gap. The hook must record one MulticolGeometry
        // entry for the container, with one ColumnGroupGeometry describing
        // the balanced columns:
        //
        //   - groups.len() == 1 (one ColumnGroup segment, no SpanAll)
        //   - n == 2 (column-count: 2)
        //   - col_heights.len() == n
        //   - Both columns populated (balance did NOT dump all into col 0)
        //   - Per-column filled height ≈ total_h / n (within one balance step)
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p>alpha alpha alpha alpha</p>
              <p>beta beta beta beta</p>
              <p>gamma gamma gamma gamma</p>
              <p>delta delta delta delta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let mc_id = collect_multicol_node_ids(&doc)[0];
        let mc_node_id = NodeId::from(mc_id);

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let geometry_table = run_pass(&mut doc, &column_styles);

        let mc_geom = geometry_table
            .get(&mc_id)
            .expect("multicol container should have a geometry entry");
        assert_eq!(
            mc_geom.groups.len(),
            1,
            "expected one ColumnGroup segment for this fixture, got {}",
            mc_geom.groups.len()
        );

        let group = &mc_geom.groups[0];
        assert_eq!(group.n, 2, "column-count: 2");
        assert_eq!(
            group.col_heights.len(),
            2,
            "col_heights length must equal n"
        );
        assert!(
            group.col_heights[0].to_f32() > 0.0 && group.col_heights[1].to_f32() > 0.0,
            "balance should populate both columns, got col_heights={:?}",
            group.col_heights
        );

        // The tallest column's bottom in container-local coordinates
        // (group.y_offset + tallest) should match the container's border-box
        // content bottom (container_h minus inset_bottom). Since this fixture
        // has no padding/border, y_offset == 0 and tallest == container_h.
        let tallest = group
            .col_heights
            .iter()
            .copied()
            .map(crate::units::Px::to_f32)
            .fold(0.0_f32, f32::max);
        let container_h = doc.get_unrounded_layout(mc_node_id).size.height;
        assert!(
            (group.y_offset.to_f32() + tallest - container_h).abs() < 1.0,
            "group bottom ({}) should match container height ({})",
            group.y_offset.to_f32() + tallest,
            container_h
        );
    }

    #[test]
    fn take_geometry_drains_table_and_is_idempotent() {
        // `take_geometry` must use `mem::take` semantics: the first call
        // returns the populated table, the second returns an empty one
        // (no double-counting). This is the contract Task 4's wrapper
        // relies on when the engine threads geometry into convert.
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2; column-gap: 0;">
              <p>a</p><p>b</p><p>c</p><p>d</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let first = tree.take_geometry();
        assert!(
            !first.is_empty(),
            "first take should return the populated geometry"
        );
        let second = tree.take_geometry();
        assert!(
            second.is_empty(),
            "second take must be empty — no double-counting"
        );
    }

    // ── column-fill: auto (Task 5 Part A) ───────────────────────────

    #[test]
    fn column_fill_auto_leaves_later_columns_empty_when_content_fits() {
        // A single short paragraph inside a `column-count: 2; column-fill: auto`
        // container must land entirely in the first column — the second column
        // stays empty. Balance mode would split it roughly in half; auto must
        // not.
        //
        // We inject the `column-fill: auto` directive via the Phase A side-
        // table (not via inline CSS) because stylo 0.8.0 doesn't surface
        // `column-fill` for the servo engine blitz uses; that's exactly the
        // reason the side-table exists.
        //
        // The fixture text must fit in **one line at col_w**, not merely at
        // viewport width: the multicol hook re-breaks parley to col_w in
        // its measure step, and a paragraph that wraps to 2 lines there is
        // an overflow case that the inline-root split (Task 4) correctly
        // distributes across columns. Keep this single-word so this test
        // exercises the "content fits → don't split" path, not the overflow
        // branch.
        //
        // Test-coverage gap tracked at fulgur-aml4: this fixture used to
        // be `alpha alpha alpha alpha` and pinned auto-fill behaviour
        // closer to the budget boundary; after fulgur-6q5 the inline-root
        // split fired on it. fulgur-aml4 will add a separate test with a
        // bounded `avail_h` (e.g. `column-fill: auto; height: 40pt`) so
        // the auto-fill + overflow path is exercised against a non-trivial
        // budget.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p>alpha</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let mc_id = collect_multicol_node_ids(&doc)[0];

        let mut column_styles = crate::column_css::ColumnStyleTable::new();
        column_styles.insert(
            mc_id,
            crate::column_css::ColumnStyleProps {
                rule: None,
                fill: Some(crate::column_css::ColumnFill::Auto),
                ..Default::default()
            },
        );

        let geometry_table = run_pass(&mut doc, &column_styles);
        let mc_geom = geometry_table
            .get(&mc_id)
            .expect("multicol container should have a geometry entry");
        assert_eq!(mc_geom.groups.len(), 1);

        let group = &mc_geom.groups[0];
        assert_eq!(group.n, 2);
        assert_eq!(group.col_heights.len(), 2);
        assert!(
            group.col_heights[0].to_f32() > 0.0,
            "first column must contain the paragraph, got {:?}",
            group.col_heights
        );
        assert_eq!(
            group.col_heights[1].to_f32(),
            0.0,
            "column-fill: auto must leave the second column empty when content fits in the first, got {:?}",
            group.col_heights
        );
    }

    // ── Inline-root child split across columns (Task 4) ─────────────

    #[test]
    fn multicol_splits_single_inline_root_p_child_across_two_columns() {
        // Single <p> with enough text to force lines into both columns.
        // 200 px viewport, multicol uses column-count: 2, column-gap: 0 →
        // col_w ≈ 92 px. 30 alphas at 16px font is far more than 92 px ×
        // any sensible budget can hold in one column.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p style="font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 200.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let mc_id = collect_multicol_node_ids(&doc)[0];
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let geometry_table = run_pass(&mut doc, &column_styles);
        let mc_geom = geometry_table
            .get(&mc_id)
            .expect("multicol container should have a geometry entry");
        assert_eq!(mc_geom.groups.len(), 1);
        let group = &mc_geom.groups[0];
        assert_eq!(group.n, 2);

        // Both columns must be filled.
        assert!(
            group.col_heights[0].to_f32() > 0.0,
            "col 0 must be filled, got {:?}",
            group.col_heights
        );
        assert!(
            group.col_heights[1].to_f32() > 0.0,
            "col 1 must be filled (the bug we're fixing), got {:?}",
            group.col_heights
        );

        // The split metadata must be populated.
        assert_eq!(
            group.paragraph_splits.len(),
            1,
            "exactly one inline-root child should have been split"
        );
        let split = &group.paragraph_splits[0];
        assert_eq!(split.column_slices.len(), 2);
        assert!(!split.column_slices[0].line_range.is_empty());
        assert!(!split.column_slices[1].line_range.is_empty());
    }

    #[test]
    fn multicol_does_not_slice_generic_inline_root_block_child() {
        // Case B paragraph slicing materialises only glyph runs. Generic
        // block inline roots may carry box semantics such as overflow,
        // padding, outlines, and backgrounds; slicing them as text would
        // bypass the normal block draw path and lose those box effects.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="columns: 2;">
              <div>Column1</div>
              <div style="overflow: hidden; padding: 2px">Column2<br>Column2 line2</div>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let mc_id = collect_multicol_node_ids(&doc)[0];
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let geometry_table = run_pass(&mut doc, &column_styles);
        let mc_geom = geometry_table
            .get(&mc_id)
            .expect("multicol container should have a geometry entry");
        assert_eq!(mc_geom.groups.len(), 1);
        let group = &mc_geom.groups[0];

        assert!(
            group.paragraph_splits.is_empty(),
            "generic block children must stay on the atomic block path, got {:?}",
            group.paragraph_splits,
        );
    }

    /// fulgur-6q5 Fix 3: when an inline-root paragraph follows an atomic
    /// sibling that already consumed part of column 0, the slicer must
    /// see the reduced first-column remainder so its per-column line
    /// distribution lines up with where the distribute loop will actually
    /// place each slice. Otherwise the placement loop reroutes slices
    /// across columns and the per-column accumulator overwrites them —
    /// silently dropping lines.
    ///
    /// Regression invariant: total lines emitted across `column_slices`
    /// must equal the source parley layout's line count. Pre-fix the
    /// total would be smaller than the line count whenever the bug
    /// triggered.
    #[test]
    fn multicol_split_inline_root_after_atomic_sibling_does_not_drop_lines() {
        // Two children: a tall atomic <p> followed by a long inline-root
        // <p>. With column-count: 2 and the atomic child filling roughly
        // half of column 0, the inline-root paragraph's pre-slice must
        // start at the reduced first-column remainder (not a fresh
        // budget).
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p style="height: 60px; background: #eee; margin: 0;">short</p>
              <p style="font-size: 16px; margin: 0;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 200.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let mc_id = collect_multicol_node_ids(&doc)[0];
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let geometry_table = run_pass(&mut doc, &column_styles);
        let mc_geom = geometry_table
            .get(&mc_id)
            .expect("multicol container should have a geometry entry");
        let group = &mc_geom.groups[0];

        // Both columns must end up with content (the long paragraph
        // straddles them after the atomic sibling fills part of col 0).
        assert!(
            group.col_heights[0].to_f32() > 0.0,
            "col 0 must be filled, got {:?}",
            group.col_heights,
        );
        assert!(
            group.col_heights[1].to_f32() > 0.0,
            "col 1 must be filled, got {:?}",
            group.col_heights,
        );

        // Exactly one paragraph_split entry (for the long inline-root).
        assert_eq!(
            group.paragraph_splits.len(),
            1,
            "expected one ParagraphSplitEntry for the long inline-root, got {}",
            group.paragraph_splits.len(),
        );
        let split = &group.paragraph_splits[0];

        // The total number of lines distributed across columns must
        // equal the source parley layout's line count. If the slicer
        // didn't track the first-column remainder, two slices would
        // collide on the same column and the BTreeMap-backed
        // `by_col[idx] = Some(...)` collector would clobber the earlier
        // one, dropping its lines.
        let source_node = doc
            .get_node(crate::blitz_adapter::bid(split.source_node_id))
            .expect("source node");
        let elem = source_node.element_data().expect("element data");
        let text_layout = elem
            .inline_layout_data
            .as_ref()
            .expect("inline layout data");
        let parley_line_count = text_layout.layout.lines().count();
        let total_lines: usize = split.column_slices.iter().map(|s| s.line_range.len()).sum();
        assert_eq!(
            total_lines,
            parley_line_count,
            "no lines may be dropped by slice placement: \
             column_slices={:?}, parley_line_count={}",
            split
                .column_slices
                .iter()
                .map(|s| s.line_range.clone())
                .collect::<Vec<_>>(),
            parley_line_count,
        );
    }

    #[test]
    fn multicol_splits_self_inline_root_container_with_bare_text() {
        // Bare text directly inside the multicol container — the div itself
        // is inline-root, no <p> wrapper. Case A of fulgur-6q5: the
        // container's own parley layout is shaped at the container's content
        // width and Blitz does NOT call `compute_child_layout` on the
        // container itself, so re-breaking at `col_w` requires a local
        // clone+`break_all_lines` inside the multicol hook.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0; font-size: 16px;">alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 200.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let mc_id = collect_multicol_node_ids(&doc)[0];
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let geometry_table = run_pass(&mut doc, &column_styles);
        let mc_geom = geometry_table
            .get(&mc_id)
            .expect("multicol container should have a geometry entry");
        let group = &mc_geom.groups[0];
        assert!(
            group.col_heights[0].to_f32() > 0.0,
            "col 0 must be filled, got {:?}",
            group.col_heights
        );
        assert!(
            group.col_heights[1].to_f32() > 0.0,
            "self-inline-root container must spread lines across both columns, got {:?}",
            group.col_heights
        );
        assert_eq!(group.paragraph_splits.len(), 1);
        assert_eq!(group.paragraph_splits[0].source_node_id, mc_id);
    }

    #[test]
    fn multicol_self_inline_root_column_fill_auto_leaves_trailing_columns_empty() {
        // Case A + column-fill: auto → greedy fill (CSS Multi-column Level 1
        // §6.1): all lines that fit within `avail_h` go into col 0 first;
        // col 1 stays empty when col 0 absorbs everything. Exercises the
        // `ColumnFill::Auto` arm in `layout_self_inline_root_container`,
        // which uses `budget = avail_h` rather than `ceil(total_h / n)`.
        //
        // The fixture must wrap to ≥ 2 lines at `col_w` so the balance
        // branch (`budget = ceil(total_h / n)`) would distribute the lines
        // across both columns, while auto with `avail_h ≥ total_h` keeps
        // them all in col 0. We force `avail_h` large via `height: 200px`
        // on the container (Taffy passes the container's prior-layout
        // height as `available_space`); without that, `avail_h` collapses
        // to the single-line container height and auto/balance produce
        // the same `[h, h]` distribution.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0; font-size: 16px; height: 200px;">alpha alpha alpha alpha</div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 200.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let mc_id = collect_multicol_node_ids(&doc)[0];
        // Inject column-fill: auto via the Phase A side-table (stylo 0.8.0
        // doesn't surface `column-fill` for the servo engine blitz uses;
        // same pattern as
        // `column_fill_auto_leaves_later_columns_empty_when_content_fits`).
        let mut column_styles = crate::column_css::ColumnStyleTable::new();
        column_styles.insert(
            mc_id,
            crate::column_css::ColumnStyleProps {
                rule: None,
                fill: Some(crate::column_css::ColumnFill::Auto),
                ..Default::default()
            },
        );

        let geometry_table = run_pass(&mut doc, &column_styles);
        let mc_geom = geometry_table
            .get(&mc_id)
            .expect("multicol container should have a geometry entry");
        assert_eq!(mc_geom.groups.len(), 1);
        let group = &mc_geom.groups[0];
        assert_eq!(group.n, 2);
        assert!(
            group.col_heights[0].to_f32() > 0.0,
            "col 0 must be filled, got {:?}",
            group.col_heights
        );
        assert_eq!(
            group.col_heights[1].to_f32(),
            0.0,
            "column-fill: auto must leave col 1 empty when content fits in col 0, got {:?}",
            group.col_heights
        );
    }

    // ── convert.rs integration: MulticolRule entry emission (Task 5 Part C) ──

    #[test]
    fn convert_emits_multicol_rule_entry_when_rule_defined() {
        // End-to-end wiring probe: HTML with `column-rule` must populate a
        // `MulticolRuleEntry` in `Drawables.multicol_rules`. Drives the
        // full render pipeline (Engine → parse → column_css harvest →
        // multicol hook → convert) minus GCPM — which is irrelevant here —
        // via `build_drawables_for_testing_no_gcpm`.
        let html = r#"<!doctype html><html><head><style>
            .mc {
                column-count: 2;
                column-gap: 20pt;
                column-rule: 2pt solid red;
            }
        </style></head><body>
          <div class="mc">
            <p>alpha alpha alpha alpha</p>
            <p>beta beta beta beta</p>
            <p>gamma gamma gamma gamma</p>
            <p>delta delta delta delta</p>
          </div>
        </body></html>"#;

        let engine = crate::Engine::builder()
            .page_size(crate::config::PageSize {
                width: 400.0,
                height: 600.0,
            })
            .build();
        let drawables = engine.build_drawables_for_testing_no_gcpm(html);

        assert!(
            !drawables.multicol_rules.is_empty(),
            "convert must emit a MulticolRuleEntry when column-rule is defined"
        );
        let (_, entry) = drawables
            .multicol_rules
            .iter()
            .next()
            .expect("at least one multicol rule entry");
        assert_eq!(entry.rule.style, crate::column_css::ColumnRuleStyle::Solid);
        assert!(
            (entry.rule.width.to_f32() - 2.0).abs() < 1e-3,
            "width should be 2pt, got {}",
            entry.rule.width.to_f32()
        );
        assert_eq!(entry.rule.color, [255, 0, 0, 255]);
        assert!(
            !entry.groups.is_empty(),
            "geometry must have at least one ColumnGroup"
        );
    }

    #[test]
    fn convert_does_not_emit_multicol_rule_without_rule() {
        // Multicol container with no `column-rule` must not register a
        // `MulticolRuleEntry`. Guards against the entry leaking into every
        // multicol render.
        let html = r#"<!doctype html><html><body>
          <div style="column-count: 2; column-gap: 0;">
            <p>alpha alpha alpha alpha</p>
            <p>beta beta beta beta</p>
          </div>
        </body></html>"#;

        let engine = crate::Engine::builder()
            .page_size(crate::config::PageSize {
                width: 400.0,
                height: 600.0,
            })
            .build();
        let drawables = engine.build_drawables_for_testing_no_gcpm(html);
        assert!(
            drawables.multicol_rules.is_empty(),
            "multicol without column-rule must not produce a MulticolRuleEntry"
        );
    }

    // ── slice_lines_by_budget: untested branches ─────────────────────

    #[test]
    fn slice_lines_by_budget_n1_all_lines_go_to_single_column() {
        // When n=1, `last_col` is always true: the overflow guard
        // (`!last_col`) is never satisfied, so every line is absorbed by
        // the single column regardless of budget.
        let heights = [10.0_f32, 20.0, 30.0];
        let slices = slice_lines_by_budget(&heights, 15.0, 1);
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0], (0..3, 60.0));
    }

    #[test]
    fn slice_lines_by_budget_more_columns_than_lines_pads_empty_slices() {
        // 3 lines into 5 columns at budget=10: one line per column for
        // columns 0-2; columns 3 and 4 receive empty ranges.
        let heights = [10.0_f32, 10.0, 10.0];
        let slices = slice_lines_by_budget(&heights, 10.0, 5);
        assert_eq!(slices.len(), 5);
        assert_eq!(slices[0], (0..1, 10.0));
        assert_eq!(slices[1], (1..2, 10.0));
        assert_eq!(slices[2], (2..3, 10.0));
        assert_eq!(slices[3], (3..3, 0.0)); // empty — no more lines
        assert_eq!(slices[4], (3..3, 0.0)); // empty — no more lines
    }

    #[test]
    fn slice_lines_by_budget_zero_budget_monolithic_rule_forces_one_line_per_non_last_column() {
        // budget=0: adding any positive-height line to a non-last column
        // immediately exceeds the budget (consumed + h > 0.0 for h > 0).
        // The monolithic-line rule still forces the first line of each
        // non-last column to be placed, so each non-last column gets
        // exactly one line and the final column absorbs the remainder.
        let heights = [10.0_f32, 10.0, 10.0];
        let slices = slice_lines_by_budget(&heights, 0.0, 2);
        assert_eq!(slices.len(), 2);
        assert_eq!(slices[0], (0..1, 10.0)); // col 0: one line (monolithic rule)
        assert_eq!(slices[1], (1..3, 20.0)); // col 1 (last): absorbs the rest
    }

    // ── resolve_column_layout: count == fits_count tie ───────────────

    #[test]
    fn resolve_column_layout_count_equals_fits_count_tie_uses_same_result_as_count_only() {
        // When `count == fits_count(width)`, `min(count, fits_count)` equals
        // count, so the result is the same as the count-only path — but the
        // `(Some(n), Some(w))` branch is taken instead of `(Some(n), None)`.
        //
        // content_w=300, gap=10, column-width=100:
        //   fits_count(100) = floor((300+10)/(100+10)) = floor(2.818…) = 2
        // column-count=2: min(2, 2) = 2 (tie). col_w = (300 − 10) / 2 = 145.
        let (n_both, w_both) = resolve_column_layout(300.0, Some(2), Some(100.0), 10.0);
        let (n_count, w_count) = resolve_column_layout(300.0, Some(2), None, 10.0);
        assert_eq!(n_both, n_count, "tie must yield same n as count-only");
        assert!(
            (w_both - w_count).abs() < 1e-3,
            "tie must yield same col_w as count-only: both={w_both}, count={w_count}"
        );
        assert_eq!(n_both, 2);
        assert!(
            (w_both - 145.0).abs() < 1e-3,
            "col_w expected 145, got {w_both}"
        );
    }

    // ── resolve_column_layout: fits_count denom ≤ 0 branch ──────────────

    #[test]
    fn resolve_column_layout_negative_gap_clamps_fits_count_to_one() {
        // When column-gap magnitude exceeds column-width, the denominator in
        // fits_count (= w + gap) is ≤ 0. The defensive branch returns 1 to
        // avoid division-by-zero or negative counts.
        // gap=-2.0, width=1.0: denom = 1.0 + (-2.0) = -1.0 ≤ 0 → fits_count = 1.
        let (n, w) = resolve_column_layout(300.0, None, Some(1.0), -2.0);
        assert_eq!(n, 1);
        // With n=1 and gap contributing nothing, col_w = avail = 300.
        assert!((w - 300.0).abs() < 1e-3, "col_w={w}");
    }

    // ── layout_column_group: zero total_h → balance budget zero ─────────

    // ── parley_layout_is_glyph_run_only: InlineBox arm ──────────────────

    #[test]
    fn case_a_multicol_with_inline_block_falls_back_to_atomic_placement() {
        // A multicol container that IS an inline root (Case A) containing an
        // inline-block element (which Parley represents as an InlineBox) must
        // not be split column-by-column, since GlyphRun materialisation would
        // silently drop InlineBox content. The layout hook falls back:
        // geometry is recorded but `paragraph_splits` is empty (whole
        // paragraph stays atomic in column 0). Covers lines 847-902 and 1494.
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2; column-gap: 0;">
              text <span style="display: inline-block; width: 20px; height: 20px;"></span> more
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        let laid_out = tree.layout_multicol_subtrees();
        assert_eq!(laid_out, 1);

        let geom = tree.take_geometry();
        assert_eq!(
            geom.len(),
            1,
            "geometry must be recorded even on the fallback path"
        );
        let mc_geom = geom.values().next().unwrap();
        let group = &mc_geom.groups[0];
        assert!(
            group.paragraph_splits.is_empty(),
            "InlineBox fallback must not produce paragraph splits, got: {:?}",
            group.paragraph_splits
        );
    }

    // ── resolve_calc_value: CSS calc() with a percentage component ──────────

    #[test]
    fn multicol_calc_mixed_padding_triggers_resolve_calc_value() {
        // `calc(10% + 5px)` mixes a percentage and an absolute length.
        // Stylo cannot resolve it at computed time, so stylo_taffy stores it
        // as an opaque calc pointer (`CompactLength::is_calc()` == true).
        // When `compute_multicol_layout` calls `resolve_or_zero` on the
        // padding (lines 527-528 of multicol_layout.rs), the closure
        //   `|val, b| tree.resolve_calc_value(val, b)`
        // is invoked, which exercises `FulgurLayoutTree::resolve_calc_value`
        // (lines 389-391) delegating to `BaseDocument::resolve_calc_value`.
        //
        // In contrast, a pure-percentage padding (`padding: 10%`) is stored
        // as `Percent(0.1)` — `is_calc()` is false, the closure is skipped,
        // and those lines stay uncovered.
        //
        // With body margin:0 and viewport 400px the containing block is 400px
        // wide. `calc(10% + 5px)` resolves to 40 + 5 = 45px on every side.
        let html = r#"<!doctype html><html><body style="margin:0"><style>p { margin: 0 }</style>
            <div id="mc" style="column-count: 2; column-gap: 0; padding: calc(10% + 5px);">
              <p id="a">alpha alpha alpha alpha</p>
              <p id="b">beta beta beta beta</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let a = layout_of_id(&doc, "a");
        // padding-left = calc(10% + 5px) = 45px → child.x must reflect this inset.
        assert!(
            (a.location.x - 45.0).abs() < 2.0,
            "calc() padding-left expected ≈ 45, got {}",
            a.location.x
        );
        // padding-top = calc(10% + 5px) = 45px → first child.y must reflect this inset.
        assert!(
            (a.location.y - 45.0).abs() < 2.0,
            "calc() padding-top expected ≈ 45, got {}",
            a.location.y
        );
    }

    // ── resolve_column_layout: fits_count else branch ────────────────────────

    /// When `width + gap <= 0.0` the inner `fits_count` closure falls back to
    /// returning 1 rather than dividing by zero. This requires a negative gap
    /// which CSS does not produce, but the function accepts any `f32`.
    #[test]
    fn resolve_width_with_negative_gap_larger_than_width_falls_back_to_one() {
        // gap = -10.0, column-width hint = 5.0 → denom = 5 + (-10) = -5 ≤ 0 → else → 1
        let (n, w) = resolve_column_layout(200.0, None, Some(5.0), -10.0);
        assert_eq!(n, 1, "denom ≤ 0 must not produce more than one column");
        // col_w with n=1 and gap=-10: (200 - (-10)*(1-1)) / 1 = 200 → max(0) = 200
        assert!(w >= 0.0, "column width must not be negative: {w}");
    }

    // ── clear_subtree_cache_inner: depth-limit and absent-node branches ──────

    #[test]
    fn clear_subtree_cache_inner_at_max_depth_returns_immediately() {
        // When depth == MAX_DOM_DEPTH the function must bail out before
        // touching any node — exercising the `if depth >= MAX_DOM_DEPTH { return; }`
        // guard (line 1544).
        //
        // Verification strategy:
        //   1. After `resolve()`, Taffy populates every node's layout cache via
        //      `CacheTree::cache_store` — `cache.is_empty()` returns false.
        //   2. `clear_subtree_cache_inner` at `MAX_DOM_DEPTH` must be a no-op:
        //      the cache must remain non-empty afterward.
        //   3. By contrast, calling at depth=0 (the normal path) must actually
        //      clear the cache — confirming that clearing itself works and the
        //      MAX_DOM_DEPTH call was a genuine early return, not a silent no-op
        //      caused by some other reason.
        let html = r#"<!doctype html><html><body>
            <div style="column-count: 2;"><p>text</p></div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let root = doc.root_element().id;

        // Pre-condition: Taffy caches layout results during resolve().
        let pre = doc.get_node(root).is_none_or(|n| n.cache().is_empty());
        assert!(
            !pre,
            "resolve() must populate the root node cache (pre-condition for this test)"
        );

        // Depth-limit call: cache must be unchanged (still non-empty).
        clear_subtree_cache_inner(
            &mut doc,
            crate::blitz_adapter::uid(root),
            crate::MAX_DOM_DEPTH,
        );
        let after_limit = doc.get_node(root).is_none_or(|n| n.cache().is_empty());
        assert!(
            !after_limit,
            "depth-limit guard must not clear the root node cache"
        );

        // Contrast: depth=0 must actually clear the cache.
        clear_subtree_cache_inner(&mut doc, crate::blitz_adapter::uid(root), 0);
        let after_zero = doc.get_node(root).is_some_and(|n| n.cache().is_empty());
        assert!(
            after_zero,
            "depth=0 must clear the root node cache (confirms clearing itself works)"
        );

        // Document remains operational.
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        let laid_out = tree.layout_multicol_subtrees();
        assert_eq!(
            laid_out, 1,
            "document must remain operable after depth-limit no-op"
        );
    }

    #[test]
    fn clear_subtree_cache_inner_absent_node_returns_without_panic() {
        // When `node_id` does not exist in the document, `get_node_mut` returns
        // `None` — exercising the `else { return; }` branch (lines 1552–1553).
        // Passing a large id that is guaranteed not to be in the parsed document.
        let html = r#"<!doctype html><html><body><p>hello</p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        clear_subtree_cache_inner(&mut doc, 9_999_999, 0);
        // No panic ⇒ the absent-node guard fired correctly.
    }

    fn lay_out_multicol(html: &str) -> blitz_html::HtmlDocument {
        lay_out_multicol_with_geometry(html).0
    }

    fn lay_out_multicol_with_geometry(
        html: &str,
    ) -> (blitz_html::HtmlDocument, MulticolGeometryTable) {
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();
        let geometry = tree.take_geometry();
        (doc, geometry)
    }

    fn node_id_of(doc: &BaseDocument, html_id: &str) -> usize {
        fn walk(doc: &BaseDocument, node_id: usize, target: &str) -> Option<usize> {
            let node = doc.get_node(crate::blitz_adapter::bid(node_id))?;
            if node.element_data().is_some_and(|e| {
                e.attrs()
                    .iter()
                    .any(|a| (&a.name.local) == "id" && a.value.as_str() == target)
            }) {
                return Some(node_id);
            }
            node.children
                .iter()
                .find_map(|&c| walk(doc, crate::blitz_adapter::uid(c), target))
        }
        walk(
            doc,
            crate::blitz_adapter::uid(doc.root_element().id),
            html_id,
        )
        .expect("id")
    }

    #[test]
    fn a_bordered_box_breaks_across_columns_in_slices() {
        // Border 2 + padding 8 on each side; four 50px children in two
        // columns: 10 + 4×50 + 10 = 220, so 110px columns.
        let (doc, geometry) = lay_out_multicol_with_geometry(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 2; column-gap: 0; width: 400px">
              <div id="card" style="border: 2px solid red; padding: 8px; margin-left: 5px; width: 150px">
                <div id="a" style="height: 50px"></div><div id="b" style="height: 50px"></div>
                <div id="c" style="height: 50px"></div><div id="d" style="height: 50px"></div>
              </div>
            </div></body></html>"#,
        );
        let mc = node_id_of(&doc, "mc");
        let card = node_id_of(&doc, "card");
        let group = &geometry[&mc].groups[0];
        assert_eq!(group.box_splits.len(), 1);
        let split = &group.box_splits[0];
        assert_eq!(split.source_node_id, card);
        let frags: Vec<(f32, f32, f32, f32, f32)> = split
            .fragments
            .iter()
            .map(|f| {
                (
                    f.origin.x.to_f32(),
                    f.origin.y.to_f32(),
                    f.size.width.to_f32(),
                    f.size.height.to_f32(),
                    f.offset.to_f32(),
                )
            })
            .collect();
        assert_eq!(
            frags,
            vec![
                (5.0, 0.0, 170.0, 110.0, 0.0),
                (205.0, 0.0, 170.0, 110.0, 110.0)
            ]
        );
        assert_eq!(split.height.to_f32(), 220.0);
        // `c` starts column 2, inside the card's padding box: x = 200 + 5 +
        // 10. Positions are relative to the card's covering box at (0, 0).
        let card_box = layout_of_id(&doc, "card");
        let c = layout_of_id(&doc, "c");
        assert_eq!((card_box.location.x, card_box.location.y), (0.0, 0.0));
        assert_eq!((c.location.x, c.location.y), (215.0, 0.0));
        let a = layout_of_id(&doc, "a");
        assert_eq!((a.location.x, a.location.y), (15.0, 10.0));
        assert!((layout_of_id(&doc, "mc").size.height - 110.0).abs() < 0.5);
    }

    #[test]
    fn a_bordered_box_that_fits_one_column_keeps_its_own_box() {
        let (doc, geometry) = lay_out_multicol_with_geometry(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 2; column-gap: 0; width: 400px">
              <div id="x" style="height: 100px; margin-bottom: 10px"></div>
              <div id="card" style="border: 1px solid red; padding: 4px; margin-top: 6px">
                <div id="a" style="height: 80px; margin-top: 3px"></div>
              </div>
            </div></body></html>"#,
        );
        assert!(
            geometry[&node_id_of(&doc, "mc")].groups[0]
                .box_splits
                .is_empty()
        );
        let card = layout_of_id(&doc, "card");
        // The 10px and 6px margins collapse to 10px, which the break drops.
        assert_eq!((card.location.x, card.location.y), (200.0, 0.0));
        assert_eq!(card.size.width, 200.0);
        // 1 + 4 + 3 (the child's margin stays inside) + 80 + 4 + 1.
        assert!(
            (card.size.height - 93.0).abs() < 0.01,
            "{}",
            card.size.height
        );
        let a = layout_of_id(&doc, "a");
        assert_eq!((a.location.x, a.location.y), (5.0, 8.0));
    }

    #[test]
    fn text_between_column_blocks_is_laid_out_in_its_anonymous_block() {
        // Bare text next to blocks used to reach the column layout as a
        // text node, which has no styles: a panic.
        let doc = lay_out_multicol(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 2; width: 400px"><p>a</p>loose text<p>b</p></div>
            </body></html>"#,
        );
        assert!(layout_of_id(&doc, "mc").size.height > 0.0);
    }

    #[test]
    fn a_column_break_moves_the_units_beside_a_float_together() {
        let mut units: Vec<FlowUnit> = (0..6).map(|_| line(10.0, 0.0)).collect();
        // Units 2 and 3 sit beside a float anchored at unit 2.
        units[3].keep_with_previous = true;
        let placed = place_units(&units, 2, 30.0, false).unwrap();
        let columns: Vec<u32> = placed.iter().map(|&(c, _)| c).collect();
        assert_eq!(columns, vec![0, 0, 1, 1, 1, 1]);
        // A run that starts the column breaks where it must.
        units[1].keep_with_previous = true;
        units[2].keep_with_previous = true;
        let placed = place_units(&units, 2, 30.0, false).unwrap();
        let columns: Vec<u32> = placed.iter().map(|&(c, _)| c).collect();
        assert_eq!(columns, vec![0, 0, 0, 1, 1, 1]);
    }

    #[test]
    fn a_float_in_a_wrapper_moves_with_the_content_beside_it() {
        // Floats used to keep the whole wrapper in one column.
        let doc = lay_out_multicol(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 2; column-gap: 0; width: 400px">
              <div id="w">
                <div id="a" style="height: 50px"></div>
                <div id="f" style="float: left; width: 20px; height: 40px"></div>
                <div id="b" style="height: 30px"></div>
                <div id="c" style="height: 30px"></div>
                <div id="d" style="height: 50px"></div>
              </div>
            </div></body></html>"#,
        );
        let w = layout_of_id(&doc, "w");
        let abs = |id: &str| {
            let l = layout_of_id(&doc, id);
            (w.location.x + l.location.x, w.location.y + l.location.y)
        };
        // Balanced at 80px, `c` would start column 2 and leave the float
        // overhanging `b`; `b` and `c` stay together beside it instead.
        assert_eq!(abs("f"), (0.0, 50.0));
        assert_eq!(abs("c"), (0.0, 80.0));
        assert_eq!(abs("d"), (200.0, 0.0));
        assert!((layout_of_id(&doc, "mc").size.height - 110.0).abs() < 0.5);
    }

    #[test]
    fn text_beside_a_top_level_float_wraps_around_it() {
        let doc = lay_out_multicol(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 2; column-gap: 0; width: 400px; font: 10px/10px sans-serif">
              <div id="f" style="float: left; width: 150px; height: 30px"></div>
              <p id="p" style="margin: 0">aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp</p>
            </div></body></html>"#,
        );
        let f = layout_of_id(&doc, "f");
        assert_eq!((f.location.x, f.location.y), (0.0, 0.0));
        let node = doc
            .get_node(crate::blitz_adapter::bid(node_id_of(&doc, "p")))
            .unwrap();
        let layout = &node
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_ref()
            .unwrap()
            .layout;
        let first = layout.lines().next().unwrap();
        // The first line has the 50px beside the float, not the column.
        assert!(
            first.metrics().advance <= 50.5,
            "first line is {}px wide",
            first.metrics().advance
        );
    }

    #[test]
    fn a_spanner_before_a_float_group_keeps_its_full_width_lines() {
        // Measuring the float group lays the whole container out one column
        // wide; the spanner used to keep the lines broken at that width.
        let doc = lay_out_multicol(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 2; column-gap: 0; width: 400px; font: 10px/10px sans-serif">
              <p style="margin: 0">aaa</p>
              <h2 id="h" style="column-span: all; margin: 0; font: 10px/10px sans-serif">aaa bbb ccc ddd eee fff ggg hhh</h2>
              <div style="float: left; width: 20px; height: 20px"></div>
              <p style="margin: 0">aaa bbb</p>
            </div></body></html>"#,
        );
        let node = doc
            .get_node(crate::blitz_adapter::bid(node_id_of(&doc, "h")))
            .unwrap();
        let layout = &node
            .element_data()
            .unwrap()
            .inline_layout_data
            .as_ref()
            .unwrap()
            .layout;
        assert_eq!(layout.lines().count(), 1);
        assert!((layout_of_id(&doc, "h").size.height - 10.0).abs() < 0.5);
    }

    #[test]
    fn fill_row_stops_at_the_first_unit_that_does_not_fit() {
        let units: Vec<FlowUnit> = (0..7).map(|_| line(20.0, 0.0)).collect();
        // Two columns of 50px take two lines each.
        let placed = fill_row(&units, 2, 50.0, false);
        assert_eq!(placed, vec![(0, 0.0), (0, 20.0), (1, 0.0), (1, 20.0)]);
    }

    #[test]
    fn fill_row_leaves_a_short_page_rest_empty_rather_than_overflow_it() {
        let units = [line(40.0, 0.0), line(10.0, 0.0)];
        assert!(fill_row(&units, 2, 30.0, true).is_empty());
        // On a whole page, the unit is placed anyway.
        assert_eq!(fill_row(&units, 1, 30.0, false), vec![(0, 0.0)]);
    }

    #[test]
    fn place_rows_fills_pages_and_balances_the_last() {
        // 30 lines of 10px, 2 columns, 45px left on the first page, 100px
        // pages: 4 + 4, then 10 + 10, then the last 2 balanced as 1 + 1.
        let units: Vec<FlowUnit> = (0..30).map(|_| line(10.0, 0.0)).collect();
        let paging = Paging {
            first_h: 45.0,
            page_h: 100.0,
        };
        let (tops, rows) = place_rows(
            &units,
            2,
            crate::column_css::ColumnFill::Balance,
            f32::INFINITY,
            Some(paging),
        );
        let tops_of_rows: Vec<f32> = rows.iter().map(|r| r.top).collect();
        assert_eq!(tops_of_rows, vec![0.0, 45.0, 145.0]);
        assert_eq!(rows[2].budget, 10.0);
        assert_eq!(tops[0], (0, 0.0));
        assert_eq!(tops[4], (1, 0.0));
        assert_eq!(tops[8], (2, 45.0));
        assert_eq!(tops[18], (3, 45.0));
        assert_eq!(tops[28], (4, 145.0));
        assert_eq!(tops[29], (5, 145.0));
    }

    #[test]
    fn place_rows_balances_in_one_row_when_everything_fits_the_page() {
        let units: Vec<FlowUnit> = (0..6).map(|_| line(10.0, 0.0)).collect();
        let paging = Paging {
            first_h: 45.0,
            page_h: 100.0,
        };
        let (tops, rows) = place_rows(
            &units,
            2,
            crate::column_css::ColumnFill::Balance,
            f32::INFINITY,
            Some(paging),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].budget, 30.0);
        assert_eq!(tops[3], (1, 0.0));
    }

    #[test]
    fn framed_box_edges_move_with_the_first_and_last_unit() {
        // Unit extents include the lead and trail, so a box's edges never
        // stay behind at a column end.
        let mut first = line(20.0, 4.0);
        first.lead = 10.0;
        let mut last = line(20.0, 0.0);
        last.trail = 10.0;
        let units = [first, line(20.0, 0.0), last];
        assert_eq!(units[0].extent(), 30.0);
        let tops = place_units(&units, 2, 60.0, true).unwrap();
        assert_eq!(tops, vec![(0, 4.0), (0, 34.0), (1, 0.0)]);
    }

    #[test]
    fn children_of_a_plain_wrapper_flow_across_columns() {
        // Content wrapped in one <div> used to be placed whole in the
        // first column.
        let doc = lay_out_multicol(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 2; column-gap: 0; width: 400px">
              <div id="w"><div id="a" style="height: 50px"></div><div id="b" style="height: 50px"></div></div>
            </div></body></html>"#,
        );
        let (w, a, b) = (
            layout_of_id(&doc, "w"),
            layout_of_id(&doc, "a"),
            layout_of_id(&doc, "b"),
        );
        assert_eq!((a.location.x, a.location.y), (0.0, 0.0));
        // `b` is relative to the wrapper, which covers both columns.
        assert!(
            (w.location.x + b.location.x - 200.0).abs() < 0.5,
            "b in column 2"
        );
        assert!(
            (w.location.y + b.location.y).abs() < 0.5,
            "b at the column top"
        );
        assert!((w.size.width - 400.0).abs() < 0.5);
        assert!((layout_of_id(&doc, "mc").size.height - 50.0).abs() < 0.5);
    }

    #[test]
    fn margins_between_column_children_are_kept() {
        let doc = lay_out_multicol(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 1; width: 400px">
              <div id="a" style="height: 20px; margin-bottom: 10px"></div>
              <div id="b" style="height: 20px; margin-top: 4px; margin-bottom: 6px"></div>
            </div></body></html>"#,
        );
        // 10px and 4px collapse to 10px; the last margin stays inside the
        // multicol, which is a block formatting context root.
        assert!((layout_of_id(&doc, "b").location.y - 30.0).abs() < 0.5);
        assert!((layout_of_id(&doc, "mc").size.height - 56.0).abs() < 0.5);
    }

    #[test]
    fn a_wrapper_with_a_background_is_placed_whole() {
        // Its background would show the cut, so its children stay together.
        let doc = lay_out_multicol(
            r#"<!doctype html><html><body style="margin:0">
            <div id="mc" style="columns: 2; column-gap: 0; width: 400px">
              <div style="background: red"><div id="a" style="height: 50px"></div><div id="b" style="height: 50px"></div></div>
              <div id="c" style="height: 100px"></div>
            </div></body></html>"#,
        );
        let b = layout_of_id(&doc, "b");
        assert_eq!((b.location.x, b.location.y), (0.0, 50.0));
        assert!((layout_of_id(&doc, "c").location.x - 200.0).abs() < 0.5);
    }
}
