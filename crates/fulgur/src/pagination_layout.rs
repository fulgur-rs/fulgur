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
//! The current implementation is **measurement-only**: it walks the body's
//! direct block children using their existing `final_layout` (set by
//! Blitz's first-pass `resolve()`) and records what fragments would be
//! produced if the page were sliced at `page_height_px`. It does not
//! re-run Taffy and it does not touch the existing `Pageable` pipeline.
//! The geometry table it returns is captured purely for comparison
//! against `paginate::paginate(...)` so we can measure agreement on
//! simple block documents and surface the cases where the two diverge.
//!
//! # Why "Taffy-hooked" matters even though we don't dispatch yet
//!
//! The wrapper still implements `LayoutPartialTree` / `RoundTree` /
//! `CacheTree` / `TraversePartialTree` because the next iteration of the
//! spike — running `taffy::compute_root_layout(self, body_id, ...)` with
//! a true page-strip-sized `available_space` — needs that scaffolding in
//! place. Establishing the trait shape now lets follow-up commits swap
//! the measurement-only walk for an actual layout intercept without
//! touching the public surface.
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
}

/// One-shot entry: run the block-only fragmenter for `doc` against a
/// `page_height_px` page strip and return the resulting geometry table.
///
/// Intended to be called **after** `blitz_adapter::resolve()` (and after
/// `multicol_layout::run_pass` when multicol is in play) so that
/// `final_layout` reflects the post-layout positions the spike walks.
/// Callers should treat the returned table as observational only — it is
/// not wired into the existing `Pageable` / `paginate` path.
pub fn run_pass(doc: &mut BaseDocument, page_height_px: f32) -> PaginationGeometryTable {
    let mut tree = PaginationLayoutTree::new(doc, page_height_px);
    tree.fragment_pagination_root();
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

    /// Walk the body's direct block children and record fragments.
    ///
    /// Returns the number of fragments emitted. `0` means either the
    /// document has no body or the body has no children — both are
    /// expected for empty documents and the convert-side comparison
    /// should treat them as equivalent to `Pageable` producing a single
    /// empty page.
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

            // If the child overflows the current page strip, advance to
            // the next page first. A child taller than a full page is
            // still emitted whole on its starting page in this spike —
            // true mid-element splitting is the next iteration.
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
        }

        emitted
    }
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
        // Spike: every node delegates to BaseDocument. The next iteration
        // will branch on `Some(usize::from(node_id)) == self.body_id`
        // and route through a `compute_pagination_layout(...)` analog of
        // `multicol_layout::compute_multicol_layout`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blitz_adapter;
    use std::sync::Arc;

    fn parse(html: &str, viewport_w: f32, viewport_h: u32) -> blitz_html::HtmlDocument {
        let fonts: Vec<Arc<Vec<u8>>> = Vec::new();
        let mut doc = blitz_adapter::parse(html, viewport_w, &fonts);
        // Mimic the engine pipeline: parse() leaves viewport_h hardcoded,
        // so we re-parse via parse_inner equivalents only when we need a
        // specific page height. For the spike we read final_layout that
        // resolve() produces, so the value passed to parse_inner is
        // mostly irrelevant — our `page_height_px` arg is what controls
        // strip slicing.
        let _ = viewport_h;
        blitz_adapter::resolve(&mut doc);
        doc
    }

    #[test]
    fn empty_document_emits_no_fragments() {
        let mut doc = parse("<html><body></body></html>", 600.0, 800);
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
        let mut doc = parse("<html></html>", 600.0, 800);
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
        let mut doc = parse(html, 600.0, 1000);
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
        let mut doc = parse(html, 600.0, 1000);
        let table = run_pass(&mut doc, 800.0);
        assert_eq!(table.len(), 2);
        let pages: Vec<u32> = table.values().map(|g| g.fragments[0].page_index).collect();
        assert_eq!(
            pages,
            vec![0, 1],
            "first child page 0, second child page 1, got {pages:?}"
        );
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
        let mut doc = parse(html, 600.0, 1500);
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
