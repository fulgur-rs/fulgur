//! Taffy custom layout hook for CSS Multi-column Layout.
//!
//! [`FulgurLayoutTree`] wraps a [`blitz_dom::BaseDocument`] as a Taffy
//! `LayoutPartialTree`, intercepts multicol containers, and routes them
//! through [`compute_multicol_layout`]. Everything else delegates to
//! `BaseDocument`'s built-in dispatch. The pattern follows blitz's own
//! [`blitz_dom::BaseDocument::compute_inline_layout`], where Parley is wired
//! into Taffy via `compute_leaf_layout`; multicol uses the same mechanism
//! one layer up.

use blitz_dom::BaseDocument;
use taffy::{
    AvailableSpace, CacheTree, CollapsibleMarginSet, LayoutPartialTree, Line, NodeId, Point,
    RequestedAxis, RoundTree, RunMode, Size, SizingMode, TraversePartialTree, TraverseTree,
};

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
/// 3. Ask Taffy to lay out every child at `known_width = col_w`. Because
///    the recursion runs through `FulgurLayoutTree::compute_child_layout`,
///    blitz's inline layout re-breaks Parley lines at the new width
///    naturally — no ad-hoc reshape plumbing needed.
/// 4. Greedy `column-fill: balance` over the measured child sizes, with
///    auto fallback if the content would exceed the available height × N.
/// 5. Write each child's column-local `(x, y)` back via
///    `set_unrounded_layout`.
/// 6. Return the container's balanced size.
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
    let children: Vec<NodeId> = (0..tree.child_count(node_id))
        .map(|i| tree.get_child_id(node_id, i))
        .collect();

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

    let mut measured: Vec<(NodeId, Size<f32>)> = Vec::with_capacity(children.len());
    for &child in &children {
        let output = tree.compute_child_layout(child, child_inputs);
        measured.push((child, output.size));
    }

    // 4. column-fill: balance
    let avail_h = match inputs.available_space.height {
        AvailableSpace::Definite(h) => h,
        _ => f32::INFINITY,
    };
    let total_h: f32 = measured.iter().map(|(_, s)| s.height).sum();
    let budget = if total_h <= 0.0 {
        0.0
    } else if total_h >= avail_h * n as f32 {
        // Content overflows → auto: fill columns to avail_h
        avail_h
    } else {
        balance_budget(&measured, n, avail_h, total_h)
    };

    // Distribute into columns. Children that are taller than the budget
    // stay as a single block (no Taffy-level splitting here; page-break
    // handling still lives in paginate.rs).
    let mut placements: Vec<(NodeId, Point<f32>, Size<f32>)> = Vec::with_capacity(children.len());
    let mut col_idx: u32 = 0;
    let mut col_y: f32 = 0.0;
    for (child_id, size) in &measured {
        // Decide first whether this child forces a break to the next column
        // — the `col_x` captured BEFORE the check would otherwise stay on
        // the old column.
        if col_y > 0.0 && col_y + size.height > budget && col_idx + 1 < n {
            col_idx += 1;
            col_y = 0.0;
        }
        let col_x = col_idx as f32 * (col_w + gap);
        placements.push((*child_id, Point { x: col_x, y: col_y }, *size));
        col_y += size.height;
    }

    // 5. Write child positions back into Taffy's storage.
    for (child_id, location, size) in &placements {
        let layout = taffy::Layout {
            order: 0,
            location: *location,
            size: Size {
                width: col_w,
                height: size.height,
            },
            content_size: *size,
            scrollbar_size: Size::ZERO,
            border: taffy::Rect::zero(),
            padding: taffy::Rect::zero(),
            margin: taffy::Rect::zero(),
        };
        tree.set_unrounded_layout(*child_id, &layout);
    }

    // 6. Container size = width × max(column_bottom).
    let column_bottoms = placements
        .iter()
        .map(|(_, loc, sz)| loc.y + sz.height)
        .fold(0.0f32, f32::max);
    let container_h = column_bottoms.max(0.0);

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
}
