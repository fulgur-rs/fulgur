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
use taffy::{
    AvailableSpace, CacheTree, CollapsibleMarginSet, LayoutPartialTree, Line, NodeId, Point,
    RequestedAxis, RoundTree, RunMode, Size, SizingMode, TraversePartialTree, TraverseTree,
};

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
pub struct FulgurLayoutTree<'a> {
    pub(crate) doc: &'a mut BaseDocument,
}

/// One-shot entry used by the render pipeline after `blitz_adapter::resolve`.
/// Runs the multicol Taffy hook on every multicol subtree in the document.
pub fn run_pass(doc: &mut BaseDocument) {
    FulgurLayoutTree::new(doc).layout_multicol_subtrees();
}

impl<'a> FulgurLayoutTree<'a> {
    pub fn new(doc: &'a mut BaseDocument) -> Self {
        Self { doc }
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

    // 2. Container content width. Prefer the width Taffy fixed for us;
    //    fall back to available_space.
    let container_w = inputs
        .known_dimensions
        .width
        .or(match inputs.available_space.width {
            AvailableSpace::Definite(w) => Some(w),
            _ => None,
        })
        .unwrap_or(0.0);

    let gap = props.column_gap.max(0.0);
    let (n, col_w) =
        resolve_column_layout(container_w, props.column_count, props.column_width, gap);

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
    // content width rather than per-column width. Otherwise identical in
    // shape to `child_inputs` so the rest of blitz's dispatch treats it
    // the same way.
    let span_child_inputs = taffy::tree::LayoutInput {
        run_mode: RunMode::PerformLayout,
        sizing_mode: SizingMode::InherentSize,
        axis: RequestedAxis::Both,
        known_dimensions: Size {
            width: Some(container_w),
            height: None,
        },
        parent_size: Size {
            width: Some(container_w),
            height: inputs.parent_size.height,
        },
        available_space: Size {
            width: AvailableSpace::Definite(container_w),
            height: AvailableSpace::MaxContent,
        },
        vertical_margins_are_collapsible: Line::FALSE,
    };

    // 4. column-fill: balance (computed inside layout_column_group).
    let avail_h = match inputs.available_space.height {
        AvailableSpace::Definite(h) => h,
        _ => f32::INFINITY,
    };

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
    for segment in segments {
        match segment {
            Segment::ColumnGroup(children) => {
                let (group_placements, seg_h) = layout_column_group(
                    tree,
                    col_w,
                    gap,
                    n,
                    avail_h,
                    &children,
                    cursor_y,
                    child_inputs,
                );
                placements.extend(group_placements);
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

    // 6. Write child positions back into Taffy's storage. The measured
    //    `size.width` is preserved so intrinsically narrower children
    //    (images, shrink-to-fit blocks, inline-level content) keep their
    //    real width rather than being stretched to the track width.
    for (child_id, location, size) in &placements {
        let layout = taffy::Layout {
            order: 0,
            location: *location,
            size: *size,
            content_size: *size,
            scrollbar_size: Size::ZERO,
            border: taffy::Rect::zero(),
            padding: taffy::Rect::zero(),
            margin: taffy::Rect::zero(),
        };
        tree.set_unrounded_layout(*child_id, &layout);
    }

    let container_h = cursor_y.max(0.0);

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
/// Returns `(placements, segment_height)` where `segment_height` is the
/// max column bottom relative to `y_offset` (i.e. the vertical extent
/// this segment contributes to the container).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn layout_column_group(
    tree: &mut FulgurLayoutTree<'_>,
    col_w: f32,
    gap: f32,
    n: u32,
    avail_h: f32,
    children: &[NodeId],
    y_offset: f32,
    child_inputs: taffy::tree::LayoutInput,
) -> (Vec<(NodeId, Point<f32>, Size<f32>)>, f32) {
    // 1. Measure
    let mut measured: Vec<(NodeId, Size<f32>)> = Vec::with_capacity(children.len());
    for &child in children {
        let output = tree.compute_child_layout(child, child_inputs);
        measured.push((child, output.size));
    }

    // 2. Balance budget
    let total_h: f32 = measured.iter().map(|(_, s)| s.height).sum();
    let budget = if total_h <= 0.0 {
        0.0
    } else if total_h >= avail_h * n as f32 {
        avail_h
    } else {
        balance_budget(&measured, n, avail_h, total_h)
    };

    // 3. Distribute
    let mut placements: Vec<(NodeId, Point<f32>, Size<f32>)> = Vec::with_capacity(children.len());
    let mut col_idx: u32 = 0;
    let mut col_y: f32 = 0.0;
    for (child_id, size) in &measured {
        if col_y > 0.0 && col_y + size.height > budget && col_idx + 1 < n {
            col_idx += 1;
            col_y = 0.0;
        }
        let col_x = col_idx as f32 * (col_w + gap);
        placements.push((
            *child_id,
            Point {
                x: col_x,
                y: y_offset + col_y,
            },
            *size,
        ));
        col_y += size.height;
    }

    // 4. Segment height = tallest column bottom relative to y_offset
    let seg_h = placements
        .iter()
        .map(|(_, loc, sz)| (loc.y - y_offset) + sz.height)
        .fold(0.0f32, f32::max)
        .max(0.0);

    (placements, seg_h)
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
    fn walk(doc: &BaseDocument, id: usize, out: &mut Vec<usize>) {
        let Some(node) = doc.get_node(id) else {
            return;
        };
        if crate::blitz_adapter::is_multicol_container(node) {
            out.push(id);
        }
        for &child in &node.children {
            walk(doc, child, out);
        }
    }
    let mut out = Vec::new();
    walk(doc, doc.root_element().id, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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

        let mut tree = FulgurLayoutTree::new(&mut doc);
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
        let mut tree = FulgurLayoutTree::new(&mut doc);
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
        let mut tree = FulgurLayoutTree::new(&mut doc);
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
        let mut tree = FulgurLayoutTree::new(&mut doc);
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
        let mut tree = FulgurLayoutTree::new(&mut doc);
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
        let mut tree = FulgurLayoutTree::new(&mut doc);
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
}
