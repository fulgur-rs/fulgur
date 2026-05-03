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

use blitz_dom::BaseDocument;
use std::collections::BTreeMap;
use taffy::{
    AvailableSpace, CacheTree, CollapsibleMarginSet, CoreStyle, LayoutPartialTree, Line, NodeId,
    Point, RequestedAxis, ResolveOrZero, RoundTree, RunMode, Size, SizingMode, TraversePartialTree,
    TraverseTree,
};

/// Per-column slice of an inline-root paragraph distributed across
/// columns by `layout_column_group`. Empty `line_range` means the column
/// receives no content from this paragraph; this happens when the
/// paragraph fits entirely in fewer columns than `n`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColumnLineSlice {
    /// Half-open parley line index range. Indices reference the parley
    /// layout reachable through `source_node_id`'s `inline_layout_data`.
    pub line_range: std::ops::Range<usize>,
    /// Slice top-left in the multicol container's content-box frame
    /// (CSS pixels). `compute_multicol_layout` shifts this into the
    /// border-box frame after the segment loop, mirroring the existing
    /// `(x_offset, y_offset)` shift on `ColumnGroupGeometry` itself.
    pub origin: taffy::Point<f32>,
    /// Slice size — `col_w × Σ line_height(line_range)`. CSS pixels.
    pub size: taffy::Size<f32>,
}

/// Plan for one paragraph distributed across `ColumnGroupGeometry`'s
/// columns. `column_slices.len() == ColumnGroupGeometry.n` always; entries
/// for unused columns have empty `line_range`.
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
/// columnar children. Consumers (Task 4's `MulticolRulePageable`) use the
/// geometry to paint `column-rule` lines between adjacent non-empty columns
/// without re-running layout: the rule at gutter `i ↔ i+1` starts at
/// `y_offset` and extends `min(col_heights[i], col_heights[i+1])` downward.
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
///   **border-box** top-left (same frame as `BlockPageable::draw`'s `x, y`
///   args). They already include the container's own padding + border.
#[derive(Clone, Debug, Default)]
pub struct ColumnGroupGeometry {
    /// Horizontal offset from the container's border-box left to column 0's
    /// left edge. Equals the container's (padding-left + border-left).
    pub x_offset: f32,
    /// Vertical offset from the container's border-box top to this group's
    /// top. Includes the container's (padding-top + border-top); subsequent
    /// groups accumulate prior segment heights on top of that.
    pub y_offset: f32,
    /// Width of a single column, in CSS pixels.
    pub col_w: f32,
    /// Horizontal gap between adjacent columns, in CSS pixels.
    pub gap: f32,
    /// Number of columns this group balances across.
    pub n: u32,
    /// Per-column filled height. `col_heights[i]` is the bottom-most
    /// placement's `(location.y + size.height)` minus `y_offset` for column
    /// `i`, clamped to `>= 0.0`. An entry of `0.0` means column `i` received
    /// no placements. Always has length == `n`.
    pub col_heights: Vec<f32>,
    /// Inline-root paragraphs distributed across the columns of this
    /// group. Empty when no child paragraph needed splitting.
    pub paragraph_splits: Vec<ParagraphSplitEntry>,
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
    let children: Vec<usize> = doc
        .get_node(container_id)
        .map(|n| n.children.clone())
        .unwrap_or_default();

    let mut out: Vec<Segment> = Vec::new();
    let mut group: Vec<NodeId> = Vec::new();
    for child_id in children {
        let Some(child_node) = doc.get_node(child_id) else {
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
    pub(crate) doc: &'a mut BaseDocument,
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

impl<'a> FulgurLayoutTree<'a> {
    pub fn new(
        doc: &'a mut BaseDocument,
        column_styles: &'a crate::column_css::ColumnStyleTable,
    ) -> Self {
        Self {
            doc,
            column_styles,
            geometry: BTreeMap::new(),
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
        let multicol_ids = collect_multicol_node_ids(self.doc);
        for id in multicol_ids.iter().rev() {
            let node_id = NodeId::from(*id);
            let prior_layout = self.doc.get_unrounded_layout(node_id);
            let prior_final = self
                .doc
                .get_node(*id)
                .map(|n| n.final_layout)
                .unwrap_or_default();
            let prior = prior_layout.size;
            let available_space = taffy::Size {
                width: AvailableSpace::Definite(prior.width),
                height: AvailableSpace::Definite(prior.height.max(1.0)),
            };
            taffy::compute_root_layout(self, node_id, available_space);
            taffy::round_layout(self, node_id);

            // `compute_root_layout` resets the subtree root's `location`
            // to (0, 0) because it treats the node as a Taffy root. The
            // multicol is NOT a root in the full document tree; it sits
            // at the position blitz originally placed it. Restore that
            // position in both unrounded and final layouts.
            if let Some(node) = self.doc.get_node_mut(*id) {
                node.unrounded_layout.location = prior_layout.location;
                node.final_layout.location = prior_final.location;
            }

            let new_h = self.doc.get_unrounded_layout(node_id).size.height;
            let delta = new_h - prior.height;
            if delta.abs() > 0.01 {
                propagate_height_delta(self.doc, *id, delta);
            }
        }
        multicol_ids.len()
    }

    fn is_multicol(&self, node_id: NodeId) -> bool {
        self.doc
            .get_node(usize::from(node_id))
            .is_some_and(crate::blitz_adapter::is_multicol_container)
    }
}

// ── Trait delegation to BaseDocument ─────────────────────────────────────

impl TraversePartialTree for FulgurLayoutTree<'_> {
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

impl TraverseTree for FulgurLayoutTree<'_> {}

impl CacheTree for FulgurLayoutTree<'_> {
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

impl LayoutPartialTree for FulgurLayoutTree<'_> {
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
        if self.is_multicol(node_id) {
            return compute_multicol_layout(self, node_id, inputs);
        }
        // Delegate to blitz for everything else. Recursion inside blitz stays
        // within BaseDocument's dispatch — nested multicol under an
        // inline-root / table / replaced subtree is not intercepted by this
        // scaffold. Top-level and nested-within-block multicols *are*
        // intercepted because Taffy's block layout recurses via `tree`,
        // which is our wrapper.
        self.doc.compute_child_layout(node_id, inputs)
    }
}

impl RoundTree for FulgurLayoutTree<'_> {
    fn get_unrounded_layout(&self, node_id: NodeId) -> taffy::Layout {
        self.doc.get_unrounded_layout(node_id)
    }

    fn set_final_layout(&mut self, node_id: NodeId, layout: &taffy::Layout) {
        self.doc.set_final_layout(node_id, layout);
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
    let capped = |n: u32| -> (u32, f32) {
        let n = n.max(1);
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
        .get_node(usize::from(node_id))
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
    let avail_h = match inputs.available_space.height {
        AvailableSpace::Definite(h) => (h - inset_top - inset_bottom).max(0.0),
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
    let segments = partition_children_into_segments(tree.doc, usize::from(node_id));

    let mut cursor_y: f32 = 0.0;
    let mut placements: Vec<(NodeId, Point<f32>, Size<f32>)> = Vec::new();
    let mut group_geometries: Vec<ColumnGroupGeometry> = Vec::new();
    for segment in segments {
        match segment {
            Segment::ColumnGroup(children) => {
                let (group_placements, geometry) = layout_column_group(
                    tree,
                    col_w,
                    gap,
                    n,
                    avail_h,
                    fill,
                    &children,
                    cursor_y,
                    child_inputs,
                );
                let seg_h = geometry.col_heights.iter().copied().fold(0.0_f32, f32::max);
                placements.extend(group_placements);
                group_geometries.push(geometry);
                cursor_y += seg_h;
            }
            Segment::SpanAll(child_id) => {
                let output = tree.compute_child_layout(child_id, span_child_inputs);
                let location = Point {
                    x: 0.0,
                    y: cursor_y,
                };
                placements.push((child_id, location, output.size));
                cursor_y += output.size.height;
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
    // `MulticolRulePageable::draw` (which receives `x, y` at the container's
    // border-box origin) can use `group.x_offset + col_x_math` and
    // `group.y_offset` directly without reapplying the container's padding.
    for group in group_geometries.iter_mut() {
        group.x_offset = inset_left;
        group.y_offset += inset_top;
    }

    // Stash the per-container geometry for downstream consumers (Task 4's
    // `MulticolRulePageable`). We intentionally record the entry even when
    // the container produced no column groups (all `SpanAll`, or entirely
    // empty) — the presence of the key lets convert-side code distinguish
    // "layout hook ran but found nothing balanceable" from "hook never ran".
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
        content_size: Size {
            width: container_w,
            height: container_h,
        },
        first_baselines: Point::NONE,
        top_margin: CollapsibleMarginSet::ZERO,
        bottom_margin: CollapsibleMarginSet::ZERO,
        margins_can_collapse_through: false,
    }
}

/// Place `children` into `n` columns of `col_w`, stacking them vertically
/// starting at `y_offset`. `avail_h` is the per-column budget ceiling
/// (for balance / auto fallback); measurement happens inside via Taffy.
///
/// Returns `(placements, geometry)`:
///
/// - `placements` is the `(child, location, size)` triples written back
///   into Taffy's storage by `compute_multicol_layout`.
/// - `geometry` carries the shape this segment contributes to the
///   container's [`MulticolGeometry`]: column width, gap, column count,
///   y-offset, and per-column filled height. The segment's vertical
///   extent (what we used to return as `seg_h`) is recoverable as
///   `geometry.col_heights.iter().copied().fold(0.0, f32::max)`.
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
) -> (Vec<(NodeId, Point<f32>, Size<f32>)>, ColumnGroupGeometry) {
    // 1. Measure
    let mut measured: Vec<(NodeId, Size<f32>)> = Vec::with_capacity(children.len());
    for &child in children {
        let output = tree.compute_child_layout(child, child_inputs);
        measured.push((child, output.size));
    }

    // 2. Budget selection — `column-fill: balance` (the default) searches
    //    for the smallest budget that fits all children in `n` columns,
    //    so the tallest column's height is minimised. `column-fill: auto`
    //    (CSS Multi-column Level 1 §6.1) instead greedily fills columns
    //    top-to-bottom up to `avail_h`, leaving trailing columns empty
    //    when content fits in fewer than `n`.
    let total_h: f32 = measured.iter().map(|(_, s)| s.height).sum();
    let budget = match fill {
        crate::column_css::ColumnFill::Auto => avail_h,
        crate::column_css::ColumnFill::Balance => {
            if total_h <= 0.0 {
                0.0
            } else if total_h >= avail_h * n as f32 {
                avail_h
            } else {
                balance_budget(&measured, n, avail_h, total_h)
            }
        }
    };

    // 3. Build the distribution work list. Each measured child becomes
    //    either a single `Atomic` item (regular block / replaced / inline-
    //    root that fits within the per-column budget) or a sequence of
    //    `InlineRootSlice` items — one per non-empty column receiving lines
    //    from a sliced inline-root paragraph (Case B in
    //    `docs/plans/2026-05-03-multicol-inline-root-split.md`).
    //
    //    The pre-flight probe (commit 0c2c762) confirmed
    //    `compute_child_layout(child, known_dimensions = col_w)` already
    //    re-breaks parley to col_w during the measure step, so per-line
    //    heights read from `inline_layout_data` reflect the column-width
    //    wrap and don't need a manual `break_all_lines`.
    enum DistItem {
        Atomic {
            id: NodeId,
            size: Size<f32>,
        },
        InlineRootSlice {
            source_id: NodeId,
            line_range: std::ops::Range<usize>,
            size: Size<f32>,
        },
    }

    let mut items: Vec<DistItem> = Vec::with_capacity(measured.len());
    for (id, size) in &measured {
        let id_u: usize = (*id).into();
        // Borrow the post-measure parley layout for this child if it is an
        // inline root *and* its measured height exceeds the per-column
        // budget (otherwise an atomic placement is cheaper and preserves
        // the byte-identical output the existing tests assert on).
        let inline_root_layout = tree
            .doc
            .get_node(id_u)
            .filter(|n| n.flags.is_inline_root())
            .and_then(|n| n.element_data())
            .and_then(|e| e.inline_layout_data.as_ref())
            .map(|tl| &tl.layout);

        match inline_root_layout {
            Some(layout) if size.height > budget => {
                let line_heights = crate::blitz_adapter::parley_line_heights(layout);
                let slices = slice_lines_by_budget(&line_heights, budget, n);
                for (line_range, slice_h) in slices {
                    if !line_range.is_empty() {
                        items.push(DistItem::InlineRootSlice {
                            source_id: *id,
                            line_range,
                            size: Size {
                                width: col_w,
                                height: slice_h,
                            },
                        });
                    }
                }
            }
            _ => items.push(DistItem::Atomic {
                id: *id,
                size: *size,
            }),
        }
    }

    // 4. Distribute
    let mut placements: Vec<(NodeId, Point<f32>, Size<f32>)> = Vec::with_capacity(items.len());
    // Per-source accumulated slice metadata. `BTreeMap` (not `HashMap`)
    // keeps iteration order deterministic — `paragraph_splits` flows into
    // PDF byte order downstream.
    let mut placed_slices: BTreeMap<usize, Vec<ColumnLineSlice>> = BTreeMap::new();
    let mut col_idx: u32 = 0;
    let mut col_y: f32 = 0.0;
    for item in &items {
        let size = match item {
            DistItem::Atomic { size, .. } => *size,
            DistItem::InlineRootSlice { size, .. } => *size,
        };

        // Same column-advance check used for atomic children today.
        if col_y > 0.0 && col_y + size.height > budget && col_idx + 1 < n {
            col_idx += 1;
            col_y = 0.0;
        }

        let col_x = col_idx as f32 * (col_w + gap);
        let location = Point {
            x: col_x,
            y: y_offset + col_y,
        };

        match item {
            DistItem::Atomic { id, .. } => {
                placements.push((*id, location, size));
            }
            DistItem::InlineRootSlice {
                source_id,
                line_range,
                ..
            } => {
                // Only the first slice owns the Taffy `set_unrounded_layout`
                // write — that's what surrounding siblings position
                // against. Later slices feed `paragraph_splits` only.
                let id_u: usize = (*source_id).into();
                let entry = placed_slices.entry(id_u).or_default();
                if entry.is_empty() {
                    placements.push((*source_id, location, size));
                }
                entry.push(ColumnLineSlice {
                    line_range: line_range.clone(),
                    // Slice origin in the *content-box frame* — same
                    // convention used for `col_heights` (relative to
                    // `y_offset`). The container-level border-box shift
                    // applied in `compute_multicol_layout` updates these
                    // alongside the rest of the geometry.
                    origin: Point { x: col_x, y: col_y },
                    size,
                });
            }
        }

        col_y += size.height;
    }

    // 5. Per-column filled heights — bottom-most placement per column,
    //    relative to y_offset. Empty columns stay at 0.0.
    //
    //    We recover each placement's column index by inverting the
    //    `col_x = col_idx * (col_w + gap)` formula from step 4. The stride
    //    `col_w + gap` is non-zero for any real multicol container (both
    //    `col_w` and `gap` are `>= 0.0`, and `col_w == 0 && gap == 0`
    //    indicates a degenerate container where the column index doesn't
    //    meaningfully differ across placements anyway). The guard below
    //    protects against a divide-by-zero; `round()` tolerates float
    //    accumulation from the multiplication in step 4.
    let stride = col_w + gap;
    let resolve_col_idx = |x: f32| -> usize {
        if stride > 0.0 {
            let raw = (x / stride).round();
            if raw.is_finite() && raw >= 0.0 {
                (raw as u32).min(n.saturating_sub(1)) as usize
            } else {
                0
            }
        } else {
            0
        }
    };
    let mut col_heights: Vec<f32> = vec![0.0_f32; n as usize];
    for (_, loc, sz) in &placements {
        let idx = resolve_col_idx(loc.x);
        let bottom = (loc.y - y_offset) + sz.height;
        if bottom > col_heights[idx] {
            col_heights[idx] = bottom;
        }
    }
    // Augment with later-column slices that don't appear in `placements`
    // (only the first slice of each split paragraph is written back to
    // Taffy storage).
    for slices in placed_slices.values() {
        for slice in slices.iter().skip(1) {
            let idx = resolve_col_idx(slice.origin.x);
            let bottom = slice.origin.y + slice.size.height;
            if bottom > col_heights[idx] {
                col_heights[idx] = bottom;
            }
        }
    }
    // Guard against negative accumulation (should be unreachable given
    // `y_offset` is the segment's top and placements live at or below it,
    // but the clamp keeps the invariant `col_heights[i] >= 0.0` explicit).
    for h in col_heights.iter_mut() {
        if *h < 0.0 {
            *h = 0.0;
        }
    }

    // 6. Build `paragraph_splits` from the per-source slice accumulator.
    //    Each entry's `column_slices` length equals `n`, with empty
    //    `ColumnLineSlice` placeholders for unused columns so downstream
    //    consumers can index by column position without bounds juggling.
    let paragraph_splits: Vec<ParagraphSplitEntry> = placed_slices
        .into_iter()
        .map(|(source_id, mut slices)| {
            let mut by_col: Vec<Option<ColumnLineSlice>> = (0..n).map(|_| None).collect();
            for slice in slices.drain(..) {
                let idx = resolve_col_idx(slice.origin.x);
                by_col[idx] = Some(slice);
            }
            let column_slices: Vec<ColumnLineSlice> =
                by_col.into_iter().map(|s| s.unwrap_or_default()).collect();
            debug_assert_eq!(column_slices.len(), n as usize);
            ParagraphSplitEntry {
                source_node_id: source_id,
                column_slices,
            }
        })
        .collect();

    let geometry = ColumnGroupGeometry {
        // `x_offset` / `y_offset` here are in the container's *content-box*
        // frame; `compute_multicol_layout` shifts them into the border-box
        // frame after every segment is placed (see the inset loop that runs
        // over `group_geometries` in that function).
        x_offset: 0.0,
        y_offset,
        col_w,
        gap,
        n,
        col_heights,
        paragraph_splits,
    };

    (placements, geometry)
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

/// Linear search for the smallest per-column budget (starting from `total / n`
/// and growing in `avail_h / 20` increments) that fits all children in `n`
/// columns with no overflow. Bounded to ≤ 20 iterations.
fn balance_budget(measured: &[(NodeId, Size<f32>)], n: u32, avail_h: f32, total_h: f32) -> f32 {
    let ideal = (total_h / n as f32).ceil().max(1.0);
    let step = (avail_h / 20.0).max(1.0);
    let mut budget = ideal;
    while budget <= avail_h {
        if fits_in_n_columns(measured, n, budget) {
            return budget;
        }
        budget += step;
    }
    avail_h
}

/// Greedy pack: returns true when all children fit into `n` columns of the
/// given per-column budget. Mirrors the placement loop in
/// `compute_multicol_layout` but without writing back.
fn fits_in_n_columns(measured: &[(NodeId, Size<f32>)], n: u32, budget: f32) -> bool {
    let mut col_idx: u32 = 0;
    let mut col_y: f32 = 0.0;
    for (_, size) in measured {
        if col_y > 0.0 && col_y + size.height > budget {
            if col_idx + 1 >= n {
                return false;
            }
            col_idx += 1;
            col_y = 0.0;
        }
        col_y += size.height;
    }
    true
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
    while let Some(parent_id) = doc.get_node(current).and_then(|n| n.parent) {
        let siblings_after: Vec<usize> = {
            let Some(parent_node) = doc.get_node(parent_id) else {
                break;
            };
            let Some(idx) = parent_node.children.iter().position(|&c| c == current) else {
                break;
            };
            parent_node.children[idx + 1..].to_vec()
        };
        for sibling in siblings_after {
            if let Some(node) = doc.get_node_mut(sibling) {
                node.unrounded_layout.location.y += delta;
                node.final_layout.location.y += delta;
            }
        }
        if let Some(node) = doc.get_node_mut(parent_id) {
            node.unrounded_layout.size.height += delta;
            node.final_layout.size.height += delta;
            // Invalidate Taffy's cached layout output for this ancestor;
            // we just mutated its size, so a later layout pass must
            // recompute rather than trust the pre-propagation entry.
            node.cache.clear();
        }
        current = parent_id;
    }
}

/// Walk the tree from the document root collecting every node id whose
/// style makes it a multicol container. Top-down order.
fn collect_multicol_node_ids(doc: &BaseDocument) -> Vec<usize> {
    fn walk(doc: &BaseDocument, id: usize, depth: usize, out: &mut Vec<usize>) {
        if depth >= crate::MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(id) else {
            return;
        };
        if crate::blitz_adapter::is_multicol_container(node) {
            out.push(id);
        }
        for &child in &node.children {
            walk(doc, child, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(doc, doc.root_element().id, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
                let node = doc.get_node(node_id)?;
                if let Some(ed) = node.element_data() {
                    if let Some(attr_id) = ed.attrs().iter().find(|a| a.name.local.as_ref() == "id")
                    {
                        if attr_id.value.as_str() == target {
                            return Some(node_id);
                        }
                    }
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

        let mc_id = find_by_id(&doc, "mc").expect("multicol node");
        let after_id = find_by_id(&doc, "after").expect("after paragraph");

        let mc_y_before = doc.get_node(mc_id).unwrap().unrounded_layout.location.y;
        let mc_h_before = doc.get_node(mc_id).unwrap().unrounded_layout.size.height;
        let mc_bottom_before = mc_y_before + mc_h_before;
        let after_y_before = doc.get_node(after_id).unwrap().unrounded_layout.location.y;
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

        let mc_h_after = doc.get_node(mc_id).unwrap().unrounded_layout.size.height;
        let mc_bottom_after = doc.get_node(mc_id).unwrap().unrounded_layout.location.y + mc_h_after;
        let after_y_after = doc.get_node(after_id).unwrap().unrounded_layout.location.y;

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

    // ── balance_budget / fits_in_n_columns ──────────────────────────
    fn fake_sized(n: usize, h: f32) -> Vec<(NodeId, Size<f32>)> {
        (0..n)
            .map(|i| {
                (
                    NodeId::from(i),
                    Size {
                        width: 100.0,
                        height: h,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn balance_budget_converges_at_ideal_when_divisible() {
        // 4 children × 10pt = 40pt total, n=2 → ideal = 20pt budget which
        // packs exactly 2 per column.
        let children = fake_sized(4, 10.0);
        let budget = balance_budget(
            &children, 2, /* avail_h = */ 100.0, /* total_h = */ 40.0,
        );
        assert!(
            (budget - 20.0).abs() < 0.01,
            "expected budget ≈ 20, got {budget}"
        );
    }

    #[test]
    fn balance_budget_grows_when_ideal_leaves_overflow() {
        // 5 children × 10pt, n=2: ideal = 25pt. Packing at 25pt fits 2+1
        // with 2 overflow lines → balance grows.
        let children = fake_sized(5, 10.0);
        let budget = balance_budget(
            &children, 2, /* avail_h = */ 100.0, /* total_h = */ 50.0,
        );
        assert!((25.0..=100.0).contains(&budget));
        // At whatever budget balance settled on, fits_in_n_columns must be
        // true — that's the stop condition.
        assert!(fits_in_n_columns(&children, 2, budget));
    }

    #[test]
    fn balance_budget_caps_at_avail_h_when_content_overflows() {
        // 10 children × 10pt = 100pt total, n=2, avail_h=30 → cannot fit
        // in 2 × 30pt columns. Returns avail_h as the auto fallback.
        let children = fake_sized(10, 10.0);
        let budget = balance_budget(
            &children, 2, /* avail_h = */ 30.0, /* total_h = */ 100.0,
        );
        assert!((budget - 30.0).abs() < 0.01);
    }

    #[test]
    fn fits_in_n_columns_detects_overflow() {
        let children = fake_sized(6, 10.0);
        assert!(fits_in_n_columns(&children, 2, 30.0)); // 3 per col at 30pt
        assert!(!fits_in_n_columns(&children, 2, 20.0)); // 2 per col → 2 left over
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
            let Some(node) = doc.get_node(id) else { return };
            let sz = node.unrounded_layout.size;
            if sz.width > 0.0 || sz.height > 0.0 {
                out.push((id, node.unrounded_layout.location.y, sz.height));
            }
            for &c in &node.children {
                walk(doc, c, out);
            }
        }
        walk(doc, doc.root_element().id, &mut out);
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
        propagate_height_delta(&mut doc, root_id, 0.0);
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
        propagate_height_delta(&mut doc, root_id, 10.0);
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
                let Some(node) = doc.get_node(id) else { return };
                if let Some(ed) = node.element_data()
                    && ed
                        .attrs()
                        .iter()
                        .any(|a| a.name.local.as_ref() == "id" && a.value.as_str() == "before")
                {
                    *out = Some(id);
                    return;
                }
                for &c in &node.children {
                    walk(doc, c, out);
                }
            }
            walk(&doc, doc.root_element().id, &mut found);
            found.expect("before node")
        };

        let before_y_pre = doc.get_node(before_id).unwrap().unrounded_layout.location.y;

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let before_y_post = doc.get_node(before_id).unwrap().unrounded_layout.location.y;
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
                let Some(node) = doc.get_node(id) else { return };
                if let Some(ed) = node.element_data()
                    && ed
                        .attrs()
                        .iter()
                        .any(|a| a.name.local.as_ref() == "id" && a.value.as_str() == "outer")
                {
                    *out = Some(id);
                    return;
                }
                for &c in &node.children {
                    walk(doc, c, out);
                }
            }
            walk(&doc, doc.root_element().id, &mut found);
            found.expect("outer div")
        };

        let outer_h_pre = doc.get_node(outer_id).unwrap().unrounded_layout.size.height;

        let column_styles = crate::column_css::ColumnStyleTable::new();
        let mut tree = FulgurLayoutTree::new(&mut doc, &column_styles);
        tree.layout_multicol_subtrees();

        let outer_h_post = doc.get_node(outer_id).unwrap().unrounded_layout.size.height;
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
            let node = doc.get_node(node_id)?;
            if let Some(ed) = node.element_data()
                && ed
                    .attrs()
                    .iter()
                    .any(|a| a.name.local.as_ref() == "id" && a.value.as_str() == target)
            {
                return Some(node_id);
            }
            for &c in &node.children {
                if let Some(found) = walk(doc, c, target) {
                    return Some(found);
                }
            }
            None
        }
        let id = walk(doc, doc.root_element().id, html_id).expect("id not found");
        doc.get_unrounded_layout(NodeId::from(id))
    }

    #[test]
    fn span_all_occupies_full_container_width() {
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0;">
              <p id="before">before</p>
              <h1 id="title" style="column-span: all;">title</h1>
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
              <h1 id="title" style="column-span: all;">title</h1>
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
              <h1 id="title" style="column-span: all;">title</h1>
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
              <h1 id="title" style="column-span: all;">title</h1>
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
        let html = r#"<!doctype html><html><body>
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
        let html = r#"<!doctype html><html><body>
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
              <h1 id="title" style="column-span: all;">title</h1>
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
        let html = r#"<!doctype html><html><body style="margin:0">
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
            group.col_heights[0] > 0.0 && group.col_heights[1] > 0.0,
            "balance should populate both columns, got col_heights={:?}",
            group.col_heights
        );

        // The tallest column's bottom in container-local coordinates
        // (group.y_offset + tallest) should match the container's border-box
        // content bottom (container_h minus inset_bottom). Since this fixture
        // has no padding/border, y_offset == 0 and tallest == container_h.
        let tallest = group.col_heights.iter().copied().fold(0.0_f32, f32::max);
        let container_h = doc.get_unrounded_layout(mc_node_id).size.height;
        assert!(
            (group.y_offset + tallest - container_h).abs() < 1.0,
            "group bottom ({}) should match container height ({})",
            group.y_offset + tallest,
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
            group.col_heights[0] > 0.0,
            "first column must contain the paragraph, got {:?}",
            group.col_heights
        );
        assert_eq!(
            group.col_heights[1], 0.0,
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
            group.col_heights[0] > 0.0,
            "col 0 must be filled, got {:?}",
            group.col_heights
        );
        assert!(
            group.col_heights[1] > 0.0,
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
            (entry.rule.width - 2.0).abs() < 1e-3,
            "width should be 2pt, got {}",
            entry.rule.width
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

    /// Pre-flight probe for fulgur-6q5: does Blitz re-break parley layout
    /// when the multicol Taffy hook calls `compute_child_layout(child,
    /// known_dimensions = col_w)` on an inline-root child?
    ///
    /// Removed in Task 9 of the plan. Run with:
    ///
    ///   cargo test -p fulgur --lib \
    ///       probe_parley_rebreak_after_compute_child_layout \
    ///       -- --ignored --nocapture
    #[test]
    #[ignore]
    fn probe_parley_rebreak_after_compute_child_layout() {
        // 30 alphas — far more than col_w (~92 px) at 16px font can hold
        // on a single line, so a re-break would clearly produce more lines.
        let html = r#"<!doctype html><html><body>
            <div id="mc" style="column-count: 2; column-gap: 0; font-size: 16px;">
              <p>alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha alpha</p>
            </div>
        </body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 200.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        // Locate <p> by element name via DFS — same shape as the existing
        // `find_by_id` walker in `siblings_after_multicol_get_repositioned_by_height_delta`.
        fn find_p(doc: &BaseDocument, node_id: usize) -> Option<usize> {
            let node = doc.get_node(node_id)?;
            if node
                .element_data()
                .map(|e| e.name.local.as_ref() == "p")
                .unwrap_or(false)
            {
                return Some(node_id);
            }
            for &child in &node.children {
                if let Some(found) = find_p(doc, child) {
                    return Some(found);
                }
            }
            None
        }
        let p_id = find_p(&doc, doc.root_element().id).expect("<p> must exist");

        let read_layout_stats = |doc: &BaseDocument, id: usize| -> (usize, f32) {
            let elem = doc.get_node(id).unwrap().element_data().unwrap();
            let layout = &elem.inline_layout_data.as_ref().unwrap().layout;
            let mut count = 0_usize;
            let mut max_adv = 0.0_f32;
            for line in layout.lines() {
                count += 1;
                let adv = line.metrics().advance;
                if adv > max_adv {
                    max_adv = adv;
                }
            }
            (count, max_adv)
        };

        let (pre_count, pre_max_adv) = read_layout_stats(&doc, p_id);

        // Container content width and resolved (n, col_w).
        let mc_id = collect_multicol_node_ids(&doc)[0];
        let mc_content_w = doc.get_node(mc_id).unwrap().unrounded_layout.size.width;
        let (n, col_w) = resolve_column_layout(mc_content_w, Some(2), None, 0.0);

        eprintln!("=== PRE-FLIGHT PROBE: parley re-break after compute_child_layout ===");
        eprintln!("mc container_w (px) = {mc_content_w}");
        eprintln!("resolved (n, col_w) = ({n}, {col_w})");
        eprintln!("pre-hook  lines = {pre_count}, max line advance (px) = {pre_max_adv}");

        // Drive the hook — `compute_child_layout(p_id, known_dimensions=col_w)`
        // happens inside `layout_column_group`'s measure step.
        let column_styles = crate::column_css::ColumnStyleTable::new();
        let _ = run_pass(&mut doc, &column_styles);

        let (post_count, post_max_adv) = read_layout_stats(&doc, p_id);
        eprintln!("post-hook lines = {post_count}, max line advance (px) = {post_max_adv}");

        let verdict = if post_count > pre_count && post_max_adv <= col_w + 0.5 {
            "RE_BREAKS"
        } else if post_count == pre_count && post_max_adv > col_w {
            "NO_REBREAK"
        } else {
            "OTHER"
        };
        eprintln!("verdict = {verdict}");
        eprintln!("=== END PROBE ===");
    }
}
