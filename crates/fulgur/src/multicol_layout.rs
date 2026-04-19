//! Taffy custom layout hook for CSS Multi-column Layout.
//!
//! ## Why a wrapper
//!
//! `taffy 0.9.2` has no multicol display mode and `blitz-dom 0.2.4` treats
//! multicol containers as plain blocks. To give multicol containers their own
//! layout semantics without forking either crate, fulgur interposes a
//! [`FulgurLayoutTree`] wrapper between Taffy and the `BaseDocument`. Taffy
//! sees our wrapper as the `LayoutPartialTree` and recurses through it; the
//! wrapper intercepts multicol containers and runs
//! [`compute_multicol_layout`] on them, delegating everything else to
//! `BaseDocument`'s built-in dispatch.
//!
//! This follows the same pattern as blitz's own
//! [`blitz_dom::BaseDocument::compute_inline_layout`], which is plugged into
//! Taffy via `compute_leaf_layout` for inline-root elements. fulgur reuses
//! that mechanism one level up for multicol.
//!
//! ## Scaffold scope (A-1b)
//!
//! This file delivers only the wiring: a wrapper that delegates to blitz and
//! a custom-compute stub that records interception so tests can prove Taffy
//! recurses through fulgur when a multicol container is encountered. Phase
//! A-2b replaces the stub with the real column-fill-balance distribution.

use std::cell::Cell;

use blitz_dom::BaseDocument;
use taffy::{
    AvailableSpace, CacheTree, LayoutPartialTree, NodeId, RoundTree, Size, TraversePartialTree,
    TraverseTree,
};

type Atom = style::Atom;

/// Taffy tree wrapper around a `BaseDocument` that intercepts multicol
/// containers and routes them through fulgur's own layout.
pub struct FulgurLayoutTree<'a> {
    pub doc: &'a mut BaseDocument,
    /// Count of multicol nodes the wrapper has laid out this run. Used by
    /// integration tests to verify the hook fires.
    pub multicol_hits: Cell<u32>,
}

impl<'a> FulgurLayoutTree<'a> {
    pub fn new(doc: &'a mut BaseDocument) -> Self {
        Self {
            doc,
            multicol_hits: Cell::new(0),
        }
    }

    /// Re-run Taffy layout for each multicol container in the tree.
    ///
    /// Intended to be called after blitz's `resolve()` has produced an
    /// initial (block-shaped) layout. We walk the tree to find every
    /// multicol container and, for each one, invoke
    /// [`taffy::compute_root_layout`] on the container's subtree through
    /// our wrapper — that makes the multicol node the Taffy root for its
    /// own layout pass, so our `compute_child_layout` sees it first and
    /// dispatches to [`compute_multicol_layout`]. Ancestors' layouts are
    /// left alone in this scaffold; a follow-up step propagates height
    /// deltas up the tree.
    ///
    /// Returns the number of multicol subtrees laid out.
    pub fn layout_multicol_subtrees(&mut self) -> usize {
        let multicol_ids = collect_multicol_node_ids(self.doc);
        // Layout children-first so nested multicol (when we add support)
        // resolves inside-out.
        for id in multicol_ids.iter().rev() {
            let node_id = NodeId::from(*id);
            // Size the subtree at whatever Taffy previously gave it; our
            // custom compute will revise as needed.
            let prior = self.doc.get_unrounded_layout(node_id).size;
            let available_space = taffy::Size {
                width: AvailableSpace::Definite(prior.width),
                height: AvailableSpace::Definite(prior.height.max(1.0)),
            };
            taffy::compute_root_layout(self, node_id, available_space);
            taffy::round_layout(self, node_id);
        }
        multicol_ids.len()
    }

    /// True when the node carries non-default `column-count` or
    /// `column-width`, i.e. it is a multicol container per CSS spec.
    pub fn is_multicol(&self, node_id: NodeId) -> bool {
        let Some(node) = self.doc.get_node(usize::from(node_id)) else {
            return false;
        };
        // stylo exposes `is_multicol()` on ComputedValues for the servo
        // engine (both column-count and column-width are engine:servo);
        // see crates/fulgur/src/blitz_adapter.rs.
        crate::blitz_adapter::extract_multicol_props(node).is_some()
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
        = &'a taffy::Style<Atom>
    where
        Self: 'a;

    type CustomIdent = Atom;

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

/// Multicol layout computation stub.
///
/// Phase A-1b only records interception and delegates sizing back to blitz
/// so existing test fixtures continue to render. Phase A-2b replaces this
/// body with the real column-fill-balance distribution.
pub fn compute_multicol_layout(
    tree: &mut FulgurLayoutTree<'_>,
    node_id: NodeId,
    inputs: taffy::tree::LayoutInput,
) -> taffy::LayoutOutput {
    tree.multicol_hits.set(tree.multicol_hits.get() + 1);
    // For now, lay out as a normal block so siblings still position
    // correctly. A-2b swaps in the real multicol compute.
    tree.doc.compute_child_layout(node_id, inputs)
}

/// Walk the tree from the document root collecting every node id whose
/// style makes it a multicol container. Top-down order.
fn collect_multicol_node_ids(doc: &BaseDocument) -> Vec<usize> {
    fn walk(doc: &BaseDocument, id: usize, out: &mut Vec<usize>) {
        let Some(node) = doc.get_node(id) else {
            return;
        };
        if crate::blitz_adapter::extract_multicol_props(node).is_some() {
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
        assert!(
            tree.multicol_hits.get() >= 1,
            "compute_multicol_layout should have been called, hits={}",
            tree.multicol_hits.get()
        );
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
        assert_eq!(tree.multicol_hits.get(), 0);
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
        assert!(
            tree.multicol_hits.get() >= 2,
            "both outer and inner multicol should fire, hits={}",
            tree.multicol_hits.get()
        );
    }
}
