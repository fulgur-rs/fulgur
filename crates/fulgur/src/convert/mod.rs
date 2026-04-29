//! Convert a Blitz DOM (after style resolution + layout) into a Pageable tree.

use crate::asset::AssetBundle;
use crate::blitz_adapter::{BaseDocument, Node, NodeData};
use crate::gcpm::CounterOp;
use crate::gcpm::running::RunningElementStore;
use crate::image::ImagePageable;
use crate::pageable::{
    BlockPageable, BlockStyle, CounterOpMarkerPageable, CounterOpWrapperPageable, ImageMarker,
    ListItemMarker, ListItemPageable, Pageable, PositionedChild, RunningElementMarkerPageable,
    RunningElementWrapperPageable, Size, SpacerPageable, StringSetPageable,
    StringSetWrapperPageable, TablePageable, TransformWrapperPageable,
};
use crate::paragraph::{
    InlineImage, LineFontMetrics, LineItem, LinkSpan, LinkTarget, ParagraphPageable, ShapedGlyph,
    ShapedGlyphRun, ShapedLine, TextDecoration, TextDecorationLine, TextDecorationStyle,
    VerticalAlign,
};
use crate::svg::SvgPageable;
use blitz_html::HtmlDocument;
use skrifa::MetadataProvider;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

use crate::MAX_DOM_DEPTH;

mod block;
mod inline_root;
mod list_item;
mod list_marker;
mod positioned;
mod pseudo;
mod replaced;
// Local submodule shadows the Stylo extern crate `style` for any sibling that
// uses `use super::*;`. Such siblings must reach Stylo via `::style::...`.
// Code inside `convert/style/*.rs` is unaffected — `style::...` there resolves
// to the extern crate via Rust 2018 absolute-path rules.
mod style;
mod table;

use self::style::{absolute_to_rgba, extract_block_style, extract_opacity_visible};

/// CSS px → PDF pt conversion factor (1 CSS px = 0.75 PDF pt).
///
/// Taffy lays out in CSS px (because we feed Blitz a CSS px viewport), but
/// the Pageable tree and Krilla work in pt. Values cross the boundary
/// through [`px_to_pt`] / [`pt_to_px`] and the tuple helpers
/// [`layout_in_pt`] / [`size_in_pt`].
const PX_TO_PT: f32 = 0.75;

/// Convert a CSS-px scalar to PDF pt.
#[inline]
pub(crate) fn px_to_pt(v: f32) -> f32 {
    v * PX_TO_PT
}

/// Convert a PDF-pt scalar to CSS px — use when feeding the Blitz viewport.
#[inline]
pub(crate) fn pt_to_px(v: f32) -> f32 {
    v / PX_TO_PT
}

/// Convert a Taffy `Layout` (CSS px) to PDF pt as `(x, y, width, height)`.
#[inline]
fn layout_in_pt(layout: &taffy::Layout) -> (f32, f32, f32, f32) {
    (
        px_to_pt(layout.location.x),
        px_to_pt(layout.location.y),
        px_to_pt(layout.size.width),
        px_to_pt(layout.size.height),
    )
}

/// Convert a Taffy `Size<f32>` (CSS px) to PDF pt as `(width, height)`.
#[inline]
fn size_in_pt(size: taffy::Size<f32>) -> (f32, f32) {
    (px_to_pt(size.width), px_to_pt(size.height))
}

/// Default CSS line-height multiplier when the actual computed value is
/// unavailable (CSS 2 §10.8.1 initial value for `line-height: normal`).
const DEFAULT_LINE_HEIGHT_RATIO: f32 = 1.2;

/// Context for DOM-to-Pageable conversion, bundling all shared state.
pub struct ConvertContext<'a> {
    pub running_store: &'a RunningElementStore,
    pub assets: Option<&'a AssetBundle>,
    /// Cache font data by (data pointer address, font index) to avoid redundant .to_vec() copies.
    pub(crate) font_cache: HashMap<(usize, u32), Arc<Vec<u8>>>,
    /// String-set entries from DOM walk, keyed by node_id for O(1) lookup.
    pub string_set_by_node: HashMap<usize, Vec<(String, String)>>,
    /// Counter operations from CounterPass, keyed by node_id for O(1) lookup.
    pub counter_ops_by_node: HashMap<usize, Vec<CounterOp>>,
    /// Resolved bookmark entries from [`crate::blitz_adapter::BookmarkPass`],
    /// keyed by node_id for O(1) lookup. When a node_id is present in this
    /// map, `convert_node` wraps the produced pageable with a
    /// `BookmarkMarkerWrapperPageable` carrying the CSS-resolved
    /// level/label. Nodes absent from the map are passed through unchanged;
    /// defaults for `h1`-`h6` come from `FULGUR_UA_CSS` applied by the
    /// engine before `BookmarkPass` runs.
    pub bookmark_by_node: HashMap<usize, crate::blitz_adapter::BookmarkInfo>,
    /// Phase A `column-*` side-table harvested by
    /// [`crate::blitz_adapter::extract_column_style_table`]. Task 5 reads
    /// `rule` properties from here when wrapping multicol containers in
    /// `MulticolRulePageable`. `BTreeMap` keeps iteration deterministic
    /// — which matters because the wrapper draws rule segments in table
    /// iteration order and drives PDF output.
    pub column_styles: crate::column_css::ColumnStyleTable,
    /// Per-multicol-container geometry recorded by the Taffy multicol hook
    /// (see [`crate::multicol_layout::run_pass`]). Task 4's
    /// `MulticolRulePageable` reads this to paint `column-rule` lines
    /// between adjacent non-empty columns without re-running layout.
    /// Keyed by container `usize` NodeId — same convention as
    /// `column_styles`.
    pub multicol_geometry: crate::multicol_layout::MulticolGeometryTable,
    /// fulgur-cj6u Phase 1.1: per-body-child page-fragment geometry
    /// recorded by [`crate::pagination_layout::run_pass_with_break_styles`].
    /// Currently captured but not yet consumed — the next phase
    /// (parity assertion) reads `implied_page_count` from this table
    /// and compares it to `paginate(...).len()` to catch divergence
    /// between the fragmenter and Pageable's split decisions.
    /// Empty when the document had no body or no in-flow children.
    /// Keyed by source `usize` NodeId — same convention as
    /// `column_styles` and `multicol_geometry`.
    pub pagination_geometry: crate::pagination_layout::PaginationGeometryTable,
    /// Anchor (`<a href>`) resolution cache shared across the entire
    /// conversion. Lifted out of `extract_paragraph` because inline-box
    /// extraction recurses through `convert_node → extract_paragraph`, and a
    /// per-paragraph cache would hand back two distinct `Arc<LinkSpan>` for
    /// the same anchor — one for the outer inline-box rect and one for the
    /// glyphs inside the box — producing duplicate `/Link` annotations in
    /// the emitted PDF (LinkCollector dedupes by `Arc::ptr_eq`). A single
    /// long-lived cache guarantees pointer identity across the whole tree.
    pub(crate) link_cache: LinkCache,
    /// Initial CB approximation: page area dimensions in CSS px.
    /// `position: fixed` resolves its containing block against the viewport
    /// (CSS 2.1 §10.1.5), but Blitz lays the body out with height = sum of
    /// in-flow children — which is `0` when every direct child is
    /// out-of-flow (the fixedpos-* WPT family). Without this fallback,
    /// `bottom: 0` against body would resolve to `0 - child_h = -child_h`
    /// and the fixed element snaps to the top of the page instead of the
    /// bottom. `None` (the test harness default) falls back to the body's
    /// Taffy size, preserving the historical behavior for unit tests that
    /// don't run through `Engine::render_html`.
    pub viewport_size_px: Option<(f32, f32)>,
}

impl ConvertContext<'_> {
    /// Return a shared Arc for the given font data, caching by data pointer + index.
    ///
    /// Safety assumption: Parley font data pointers remain stable for the lifetime of
    /// this ConvertContext (scoped to a single `dom_to_pageable` call). HashMap is used
    /// (not BTreeMap) because this cache is lookup-only — iteration order does not
    /// affect PDF output.
    fn get_or_insert_font(&mut self, font: &parley::FontData) -> Arc<Vec<u8>> {
        let key = (font.data.data().as_ptr() as usize, font.index);
        Arc::clone(
            self.font_cache
                .entry(key)
                .or_insert_with(|| Arc::new(font.data.data().to_vec())),
        )
    }
}

/// Convert a resolved Blitz document into a Pageable tree.
pub fn dom_to_pageable(doc: &HtmlDocument, ctx: &mut ConvertContext<'_>) -> Box<dyn Pageable> {
    let root = doc.root_element();
    // Debug: print layout tree structure
    if std::env::var("FULGUR_DEBUG").is_ok() {
        debug_print_tree(doc.deref(), root.id, 0);
    }
    convert_node(doc.deref(), root.id, ctx, 0)
}

/// Phase 4 (fulgur-9t3z): convert a resolved Blitz document into a
/// `Drawables` struct holding per-NodeId draw payload.
///
/// **Migration scaffolding (PRs 2-6)**: this implementation runs
/// `dom_to_pageable` internally and walks the resulting tree to
/// extract each per-type payload into its `Drawables` map. The
/// Pageable tree is dropped before return — wasteful but lets each
/// subsequent PR migrate one Pageable type at a time without
/// duplicating convert logic. PR 8 (after Pageable deletion) will
/// replace this body with a native DOM walk that doesn't allocate
/// the intermediate Pageable tree.
pub fn dom_to_drawables(
    doc: &HtmlDocument,
    ctx: &mut ConvertContext<'_>,
) -> crate::drawables::Drawables {
    let root_pageable = dom_to_pageable(doc, ctx);
    let mut drawables = crate::drawables::Drawables::new();
    extract_drawables_from_pageable(root_pageable.as_ref(), &mut drawables);
    let bookmark_anchors_drained = extract_bookmark_anchors(doc, &ctx.bookmark_by_node, ctx.assets);
    drawables.bookmark_anchors = bookmark_anchors_drained;
    drawables
}

/// Walk a Pageable tree and populate each `Drawables` map by
/// downcasting concrete types. PR 2 covers `ImagePageable` and
/// `SvgPageable`; subsequent PRs add the rest.
fn extract_drawables_from_pageable(
    pageable: &dyn crate::pageable::Pageable,
    out: &mut crate::drawables::Drawables,
) {
    use crate::drawables::{ImageEntry, SvgEntry};
    use crate::image::ImagePageable;
    use crate::pageable::{
        BlockPageable, BookmarkMarkerWrapperPageable, CounterOpWrapperPageable, ListItemPageable,
        RunningElementWrapperPageable, StringSetWrapperPageable, TablePageable,
        TransformWrapperPageable,
    };
    use crate::svg::SvgPageable;

    let any = pageable.as_any();

    // Image leaf — record per-NodeId payload.
    if let Some(img) = any.downcast_ref::<ImagePageable>() {
        if let Some(node_id) = img.node_id {
            out.images.insert(
                node_id,
                ImageEntry {
                    image_data: img.image_data.clone(),
                    format: img.format,
                    width: img.width,
                    height: img.height,
                    opacity: img.opacity,
                    visible: img.visible,
                },
            );
        }
        return;
    }
    // SVG leaf — record per-NodeId payload.
    if let Some(svg) = any.downcast_ref::<SvgPageable>() {
        if let Some(node_id) = svg.node_id {
            out.svgs.insert(
                node_id,
                SvgEntry {
                    tree: svg.tree.clone(),
                    width: svg.width,
                    height: svg.height,
                    opacity: svg.opacity,
                    visible: svg.visible,
                },
            );
        }
        return;
    }
    // Block / List / Table / wrappers — recurse into children. PR 4+
    // will record their own draw payload (BlockEntry, etc.); for PR 2
    // we only walk past them to reach the leaves.
    if let Some(block) = any.downcast_ref::<BlockPageable>() {
        for pc in &block.children {
            extract_drawables_from_pageable(pc.child.as_ref(), out);
        }
        return;
    }
    if let Some(table) = any.downcast_ref::<TablePageable>() {
        for pc in &table.header_cells {
            extract_drawables_from_pageable(pc.child.as_ref(), out);
        }
        for pc in &table.body_cells {
            extract_drawables_from_pageable(pc.child.as_ref(), out);
        }
        return;
    }
    if let Some(list_item) = any.downcast_ref::<ListItemPageable>() {
        extract_drawables_from_pageable(list_item.body.as_ref(), out);
        return;
    }
    // Wrappers delegate.
    if let Some(w) = any.downcast_ref::<TransformWrapperPageable>() {
        extract_drawables_from_pageable(w.inner.as_ref(), out);
        return;
    }
    if let Some(w) = any.downcast_ref::<BookmarkMarkerWrapperPageable>() {
        extract_drawables_from_pageable(w.child.as_ref(), out);
        return;
    }
    if let Some(w) = any.downcast_ref::<StringSetWrapperPageable>() {
        extract_drawables_from_pageable(w.child.as_ref(), out);
        return;
    }
    if let Some(w) = any.downcast_ref::<RunningElementWrapperPageable>() {
        extract_drawables_from_pageable(w.child.as_ref(), out);
        return;
    }
    if let Some(w) = any.downcast_ref::<CounterOpWrapperPageable>() {
        extract_drawables_from_pageable(w.child.as_ref(), out);
    }
    // Other types (Spacer, ParagraphPageable, MulticolRulePageable,
    // marker-only Pageables) have no PR 2 payload — markers and
    // Spacers stay no-op in v2; Paragraph / Multicol land in PR 3 / 6.
}

#[cfg(test)]
mod extract_drawables_tests {
    use super::*;
    use crate::drawables::Drawables;
    use crate::image::{ImageFormat, ImagePageable};
    use crate::pageable::{
        BlockPageable, BookmarkMarkerPageable, BookmarkMarkerWrapperPageable, PositionedChild,
    };
    use crate::svg::SvgPageable;
    use std::sync::Arc;
    use usvg::{Options, Tree};

    /// Build a Pageable tree containing one block-level `ImagePageable`,
    /// run the extractor, and verify the image lands in
    /// `drawables.images` keyed by its `node_id`.
    #[test]
    fn extracts_block_level_image_into_drawables() {
        let img = ImagePageable::new(Arc::new(vec![0u8; 4]), ImageFormat::Png, 50.0, 30.0)
            .with_node_id(Some(42));
        let block = BlockPageable::with_positioned_children(vec![PositionedChild::in_flow(
            Box::new(img),
            0.0,
            0.0,
        )]);
        let mut out = Drawables::new();
        extract_drawables_from_pageable(&block, &mut out);

        let entry = out.images.get(&42).expect("image entry recorded");
        assert_eq!(entry.width, 50.0);
        assert_eq!(entry.height, 30.0);
        assert!(entry.visible);
    }

    /// Same shape for SVG.
    #[test]
    fn extracts_block_level_svg_into_drawables() {
        let tree = Arc::new(
            Tree::from_str(
                "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 10 10'></svg>",
                &Options::default(),
            )
            .expect("parse svg"),
        );
        let svg = SvgPageable::new(tree, 80.0, 60.0).with_node_id(Some(7));
        let block = BlockPageable::with_positioned_children(vec![PositionedChild::in_flow(
            Box::new(svg),
            0.0,
            0.0,
        )]);
        let mut out = Drawables::new();
        extract_drawables_from_pageable(&block, &mut out);

        let entry = out.svgs.get(&7).expect("svg entry recorded");
        assert_eq!(entry.width, 80.0);
        assert_eq!(entry.height, 60.0);
    }

    /// Marker wrappers must be transparent — the extractor descends
    /// into `child` and finds the image inside.
    #[test]
    fn descends_into_bookmark_wrapper_to_reach_image() {
        let img = ImagePageable::new(Arc::new(vec![0u8; 4]), ImageFormat::Png, 10.0, 10.0)
            .with_node_id(Some(99));
        let wrapped = BookmarkMarkerWrapperPageable::new(
            BookmarkMarkerPageable::new(1, "X".into()),
            Box::new(img),
        );
        let mut out = Drawables::new();
        extract_drawables_from_pageable(&wrapped, &mut out);
        assert!(out.images.contains_key(&99));
    }
}

/// Build the bookmark anchor map. The `bookmark_by_node` map on
/// `ConvertContext` is populated upstream (`engine.rs` runs
/// `BookmarkPass` before `dom_to_pageable`); we only project it into
/// the `Drawables` shape. The `_doc` / `_assets` arguments are
/// reserved for future enrichment (PR 6+).
fn extract_bookmark_anchors(
    _doc: &HtmlDocument,
    bookmark_by_node: &std::collections::HashMap<usize, crate::blitz_adapter::BookmarkInfo>,
    _assets: Option<&crate::asset::AssetBundle>,
) -> std::collections::BTreeMap<usize, crate::drawables::BookmarkAnchorEntry> {
    let mut out = std::collections::BTreeMap::new();
    for (&node_id, info) in bookmark_by_node {
        out.insert(
            node_id,
            crate::drawables::BookmarkAnchorEntry {
                level: info.level,
                label: info.label.clone(),
            },
        );
    }
    out
}

fn debug_print_tree(doc: &BaseDocument, node_id: usize, depth: usize) {
    if depth >= MAX_DOM_DEPTH {
        eprintln!("{}... (max depth reached)", "  ".repeat(depth));
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    let (x, y, width, height) = layout_in_pt(&node.final_layout);
    let indent = "  ".repeat(depth);
    let tag = match &node.data {
        NodeData::Element(e) => e.name.local.to_string(),
        NodeData::Text(_) => "#text".to_string(),
        NodeData::Comment => "#comment".to_string(),
        _ => "#other".to_string(),
    };
    eprintln!(
        "{indent}{tag} id={} pos=({},{}) size={}x{} inline_root={}",
        node_id,
        x,
        y,
        width,
        height,
        node.flags.is_inline_root()
    );
    for &child_id in &node.children {
        debug_print_tree(doc, child_id, depth + 1);
    }
}

fn convert_node(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    if depth >= MAX_DOM_DEPTH {
        return Box::new(SpacerPageable::new(0.0));
    }
    let result = convert_node_inner(doc, node_id, ctx, depth);
    // Wrap multicol containers in `MulticolRulePageable` when the Phase A
    // side-table carries a renderable `column-rule` spec and the Taffy
    // layout hook recorded geometry for this container. Applied here
    // (once per node) rather than at each of the ~11
    // `BlockPageable::with_positioned_children` construction sites in
    // `convert_node_inner`, because this is the single choke point all
    // paths funnel through before downstream wrappers
    // (string-set / counter-ops / transform / bookmark). The helper is
    // a no-op for non-multicol nodes and for multicol nodes without a
    // visible rule.
    let result = maybe_wrap_multicol_rule(doc, node_id, ctx, result);
    let result = maybe_prepend_string_set(node_id, result, ctx);
    let result = maybe_prepend_counter_ops(node_id, result, ctx);
    let result = maybe_wrap_transform(doc, node_id, result);
    // CSS-driven bookmark wrapping. Entries are populated by
    // `BookmarkPass` (see `blitz_adapter::run_bookmark_pass`). Nodes absent
    // from the map are passed through unchanged — there is no hardcoded
    // h1-h6 fallback; defaults come from `FULGUR_UA_CSS`.
    if let Some(info) = ctx.bookmark_by_node.remove(&node_id) {
        use crate::pageable::{BookmarkMarkerPageable, BookmarkMarkerWrapperPageable};
        Box::new(BookmarkMarkerWrapperPageable::new(
            BookmarkMarkerPageable::new(info.level, info.label),
            result,
        ))
    } else {
        result
    }
}

/// If the given node has string-set entries, wrap the pageable in a
/// `StringSetWrapperPageable` that keeps markers attached to the child during
/// pagination. Otherwise return the pageable as-is.
fn maybe_prepend_string_set(
    node_id: usize,
    child: Box<dyn Pageable>,
    ctx: &mut ConvertContext<'_>,
) -> Box<dyn Pageable> {
    let entries = ctx.string_set_by_node.remove(&node_id);
    match entries {
        Some(entries) if !entries.is_empty() => {
            let markers = entries
                .into_iter()
                .map(|(name, value)| StringSetPageable::new(name, value))
                .collect();
            Box::new(StringSetWrapperPageable::new(markers, child))
        }
        _ => child,
    }
}

/// If the given node has counter operations, wrap the pageable in a
/// `CounterOpWrapperPageable` that keeps counter operations attached to the
/// child during pagination. The wrapper is atomic when the child cannot split,
/// preventing the operations from being stranded on the wrong page.
fn maybe_prepend_counter_ops(
    node_id: usize,
    child: Box<dyn Pageable>,
    ctx: &mut ConvertContext<'_>,
) -> Box<dyn Pageable> {
    let ops = ctx.counter_ops_by_node.remove(&node_id);
    match ops {
        Some(ops) if !ops.is_empty() => Box::new(CounterOpWrapperPageable::new(ops, child)),
        _ => child,
    }
}

/// If the given node is a multicol container (`column-count` or
/// `column-width` non-auto) AND the Phase A `column-*` side-table carries a
/// visible `column-rule` spec for it AND the Taffy multicol hook recorded
/// geometry for it, wrap the pageable in a [`MulticolRulePageable`] so the
/// draw pass paints vertical rules between adjacent non-empty columns.
///
/// No-op in all other cases — non-multicol nodes, multicol nodes without
/// a rule, or rules with `style: none` / non-positive width. The helper is
/// called once per node at the choke point in [`convert_node`], so adding
/// it there covers every `BlockPageable::with_positioned_children`
/// construction path without requiring per-site adjustments.
fn maybe_wrap_multicol_rule(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &ConvertContext<'_>,
    child: Box<dyn Pageable>,
) -> Box<dyn Pageable> {
    let Some(node) = doc.get_node(node_id) else {
        return child;
    };
    if !crate::blitz_adapter::is_multicol_container(node) {
        return child;
    }
    let Some(rule) = ctx
        .column_styles
        .get(&node_id)
        .and_then(|props| props.rule)
        .filter(|r| r.style != crate::column_css::ColumnRuleStyle::None && r.width > 0.0)
    else {
        return child;
    };
    let Some(geometry) = ctx.multicol_geometry.get(&node_id) else {
        return child;
    };
    // `ColumnGroupGeometry` is recorded by the Taffy hook in CSS pixels
    // (Taffy's native unit). Every other Pageable consumes pt, so convert
    // at the wrapper boundary: the downstream `MulticolRulePageable::draw`
    // and `split_boxed` can then mix these values with pt-valued `x`/`y`
    // and pt-valued `cutoff` without a unit mismatch. See `px_to_pt`.
    let groups_pt: Vec<crate::multicol_layout::ColumnGroupGeometry> = geometry
        .groups
        .iter()
        .map(|g| crate::multicol_layout::ColumnGroupGeometry {
            x_offset: px_to_pt(g.x_offset),
            y_offset: px_to_pt(g.y_offset),
            col_w: px_to_pt(g.col_w),
            gap: px_to_pt(g.gap),
            n: g.n,
            col_heights: g.col_heights.iter().copied().map(px_to_pt).collect(),
        })
        .collect();
    Box::new(crate::pageable::MulticolRulePageable::new(
        child, rule, groups_pt,
    ))
}

/// If the given node has a non-identity `transform`, wrap the pageable in a
/// `TransformWrapperPageable`. The wrapper holds a pre-resolved affine matrix
/// and enforces atomic pagination (a transformed element never splits across
/// a page boundary).
fn maybe_wrap_transform(
    doc: &BaseDocument,
    node_id: usize,
    child: Box<dyn Pageable>,
) -> Box<dyn Pageable> {
    let Some(node) = doc.get_node(node_id) else {
        return child;
    };
    let Some(styles) = node.primary_styles() else {
        return child;
    };
    let (width, height) = size_in_pt(node.final_layout.size);
    match crate::blitz_adapter::compute_transform(&styles, width, height) {
        Some((matrix, origin)) => Box::new(TransformWrapperPageable::new(child, matrix, origin)),
        None => child,
    }
}

/// Emit bare `StringSetPageable` markers for a node that is about to be
/// skipped by pagination (zero-size leaf) or flattened (zero-size container).
///
/// Without this, `string-set` on an empty element — e.g.
/// `<div class="chapter" data-title="Ch 1"></div>` with
/// `.chapter { string-set: title attr(data-title); }` — would never reach the
/// Pageable tree because `convert_node` is never called for the node.
///
/// The `x`/`y` arguments are the node's Taffy-computed `final_layout.location`.
/// They MUST be propagated to the `PositionedChild` because `BlockPageable::split`
/// uses `children[split_index].y` as the rebase point for the next page; a
/// marker hardcoded to `y = 0` would corrupt the y-offsets of all children
/// following it on the next page when a split lands on its index.
///
/// Bare markers are appended directly (no `StringSetWrapperPageable` wrapper):
/// there is no real child content to keep them attached to, and their
/// position in the parent's child list already represents the point in the
/// document flow where the string was set.
fn emit_orphan_string_set_markers(
    node_id: usize,
    x: f32,
    y: f32,
    ctx: &mut ConvertContext<'_>,
    out: &mut Vec<PositionedChild>,
) {
    if let Some(entries) = ctx.string_set_by_node.remove(&node_id) {
        for (name, value) in entries {
            out.push(PositionedChild {
                child: Box::new(StringSetPageable::new(name, value)),
                x,
                y,
                out_of_flow: false,
                is_fixed: false,
            });
        }
    }
}

/// Emit counter-op markers for a node, similar to `emit_orphan_string_set_markers`.
///
/// If `counter_ops_by_node` contains entries for `node_id`, they are removed
/// and pushed as a `CounterOpMarkerPageable` at `(x, y)`.
fn emit_counter_op_markers(
    node_id: usize,
    x: f32,
    y: f32,
    ctx: &mut ConvertContext<'_>,
    out: &mut Vec<PositionedChild>,
) {
    if let Some(ops) = ctx.counter_ops_by_node.remove(&node_id) {
        out.push(PositionedChild {
            child: Box::new(CounterOpMarkerPageable::new(ops)),
            x,
            y,
            out_of_flow: false,
            is_fixed: false,
        });
    }
}

/// Emit a bare `BookmarkMarkerPageable` for a node that is about to be
/// skipped or flattened by pagination (zero-size leaf / flattened container).
///
/// Without this, an element that carries CSS `bookmark-level` / `bookmark-label`
/// but has no visible content would never reach the Pageable tree because
/// `convert_node` is never called for it (or its result is flattened away),
/// so the outline entry would silently disappear.
///
/// The `x` / `y` arguments are the node's Taffy-computed `final_layout.location`;
/// propagated for the same reason as `emit_orphan_string_set_markers` —
/// `BlockPageable::split` uses the child's `y` as the rebase point on page
/// break, so a marker hardcoded to `y = 0` could corrupt the y-offsets of
/// trailing children.
///
/// Because both this path and `convert_node`'s bookmark wrapper call
/// `ctx.bookmark_by_node.remove(&node_id)`, each node_id produces *at most
/// one* marker — whichever path runs first consumes the entry.
fn emit_orphan_bookmark_marker(
    node_id: usize,
    x: f32,
    y: f32,
    ctx: &mut ConvertContext<'_>,
    out: &mut Vec<PositionedChild>,
) {
    use crate::pageable::BookmarkMarkerPageable;
    if let Some(info) = ctx.bookmark_by_node.remove(&node_id) {
        out.push(PositionedChild {
            child: Box::new(BookmarkMarkerPageable::new(info.level, info.label)),
            x,
            y,
            out_of_flow: false,
            is_fixed: false,
        });
    }
}

/// If `node_id` corresponds to a running element instance registered by
/// `RunningElementPass`, return a fresh `RunningElementMarkerPageable` for it.
///
/// Running elements are rewritten to `display: none` by the GCPM parser, so
/// their DOM nodes land in the zero-size branches of
/// `collect_positioned_children`. Instead of pushing the marker directly into
/// the parent's child list, the caller buffers it and attaches it to the
/// following real child via `RunningElementWrapperPageable` — otherwise the
/// marker could be stranded on the previous page when the following child
/// overflows to the next page.
fn take_running_marker(
    node_id: usize,
    ctx: &ConvertContext<'_>,
) -> Option<RunningElementMarkerPageable> {
    let instance_id = ctx.running_store.instance_for_node(node_id)?;
    let name = ctx.running_store.name_of(instance_id)?;
    Some(RunningElementMarkerPageable::new(
        name.to_string(),
        instance_id,
    ))
}

fn convert_node_inner(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    // List-item dispatch: outside marker / display:list-item fallback / inside marker — see list_item::try_convert.
    if let Some(p) = list_item::try_convert(doc, node_id, ctx, depth) {
        return p;
    }

    // Table dispatch: <table> — see table::try_convert.
    if let Some(p) = table::try_convert(doc, node_id, ctx, depth) {
        return p;
    }

    // Replaced-element dispatch: <img>, <svg>, content: url() — see replaced::try_convert.
    if let Some(p) = replaced::try_convert(doc, node_id, ctx) {
        return p;
    }

    // Inline-root dispatch: paragraph + inline pseudo images — see inline_root::try_convert.
    if let Some(p) = inline_root::try_convert(doc, node_id, ctx, depth) {
        return p;
    }

    block::convert(doc, node_id, ctx, depth)
}

use crate::blitz_adapter::{extract_inline_svg_tree, get_attr};

/// Extract a trimmed, non-empty HTML `id` attribute from `node` and wrap it
/// in an `Arc<String>` so split fragments can share without cloning the string.
///
/// Returns `None` if the node has no element data, no `id` attribute, or an
/// empty/whitespace-only value.
fn extract_block_id(node: &Node) -> Option<Arc<String>> {
    let el = node.element_data()?;
    let raw = get_attr(el, "id")?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(Arc::new(raw.to_string()))
    }
}

/// Build a [`Pagination`] for `node` from the fulgur-ftp column_css sniffer.
///
/// Maps `break-inside`, `break-after`, and `break-before` from the column CSS
/// props into [`Pagination`]. Absence of the node from `ctx.column_styles`
/// collapses cleanly to the `Auto` variants, so every
/// `BlockPageable::with_positioned_children` site can call this
/// unconditionally without regressing the baseline behaviour that the
/// existing test suite depends on.
fn extract_pagination_from_column_css(
    ctx: &ConvertContext<'_>,
    node: &Node,
) -> crate::pageable::Pagination {
    use crate::pageable::{BreakAfter, BreakBefore, BreakInside, Pagination};
    let props = ctx.column_styles.get(&node.id).copied().unwrap_or_default();
    Pagination {
        break_inside: props.break_inside.unwrap_or(BreakInside::Auto),
        break_after: props.break_after.unwrap_or(BreakAfter::Auto),
        break_before: props.break_before.unwrap_or(BreakBefore::Auto),
        ..Pagination::default()
    }
}

/// Whether `node` is a `::before` / `::after` pseudo-element, detected by
/// checking that its parent's `before` / `after` slot points back to it.
///
/// Blitz doesn't expose a direct "is pseudo" flag on `Node`; pseudo element
/// nodes look like synthetic `<div>` / `<span>` elements. This helper is
/// used to scope behavior that is only correct for pseudos — notably the
/// `convert_inline_box_node` guard that suppresses absolutely-positioned
/// pseudos so `build_absolute_pseudo_children` can re-emit them at the
/// right place. Regular absolutely-positioned elements do not have a
/// corresponding re-emit path yet and must fall through to
/// `convert_node` instead of being silently dropped.
fn is_pseudo_node(doc: &BaseDocument, node: &Node) -> bool {
    node.parent
        .and_then(|pid| doc.get_node(pid))
        .is_some_and(|p| p.before == Some(node.id) || p.after == Some(node.id))
}

/// Geometry of a parent's content-box, used by the pseudo-image helpers so
/// `::before`/`::after` land at the content-box corners (not the border-box
/// corners) and percentage sizes resolve against the content-box dimensions.
///
/// `origin_x` / `origin_y` are the top-left of the content-box relative to
/// the parent's border-box origin (i.e. `border_left + padding_left`,
/// `border_top + padding_top`). `width` / `height` are the content-box
/// dimensions (border-box size minus both-side insets).
#[derive(Clone, Copy)]
struct ContentBox {
    origin_x: f32,
    origin_y: f32,
    width: f32,
    height: f32,
}

/// Compute the content-box of `node` from its computed style + Taffy layout.
///
/// Taffy's `final_layout.size` is the border-box; we back out the padding +
/// border on both sides to get the content-box dimensions. This mirrors the
/// pattern used inside `wrap_replaced_in_block_style` (search for
/// `content_inset` / `right_inset` in this file).
fn compute_content_box(node: &Node, style: &BlockStyle) -> ContentBox {
    let (left_inset, top_inset) = style.content_inset();
    let right_inset = style.border_widths[1] + style.padding[1];
    let bottom_inset = style.border_widths[2] + style.padding[2];
    let (border_w, border_h) = size_in_pt(node.final_layout.size);
    ContentBox {
        origin_x: left_inset,
        origin_y: top_inset,
        width: (border_w - left_inset - right_inset).max(0.0),
        height: (border_h - top_inset - bottom_inset).max(0.0),
    }
}

/// Memoized lookup of the enclosing `<a href>` for a node.
///
/// Two-level cache to ensure pointer identity per anchor:
/// - `by_start` maps the starting node ID (e.g. a glyph run's brush.id) to
///   the resolved anchor's node ID (or `None` if no anchor ancestor).
/// - `by_anchor` maps the anchor's node ID to the canonical `Arc<LinkSpan>`.
///
/// This guarantees that two glyph runs under the same `<a>` receive the
/// SAME `Arc<LinkSpan>` (verified via `Arc::ptr_eq`), which is required for
/// correct quad_points deduplication during PDF /Link emission.
#[derive(Default)]
pub(crate) struct LinkCache {
    by_start: HashMap<usize, Option<usize>>,
    by_anchor: HashMap<usize, Arc<LinkSpan>>,
}

impl LinkCache {
    pub(crate) fn lookup(&mut self, doc: &BaseDocument, start_id: usize) -> Option<Arc<LinkSpan>> {
        if let Some(cached) = self.by_start.get(&start_id) {
            let anchor_id = (*cached)?;
            return self.by_anchor.get(&anchor_id).cloned();
        }
        match inline_root::resolve_enclosing_anchor(doc, start_id) {
            Some((anchor_id, span)) => {
                self.by_start.insert(start_id, Some(anchor_id));
                let arc = self
                    .by_anchor
                    .entry(anchor_id)
                    .or_insert_with(|| Arc::new(span))
                    .clone();
                Some(arc)
            }
            None => {
                self.by_start.insert(start_id, None);
                None
            }
        }
    }
}

/// Extract the asset name from a URL that Stylo may have resolved to absolute.
/// e.g. "file:///bg.png" → "bg.png", "file:///images/bg.png" → "images/bg.png",
/// "bg.png" → "bg.png" (passthrough for unresolved URLs).
fn extract_asset_name(url: &str) -> &str {
    url.strip_prefix("file:///").unwrap_or(url)
}

/// Check if a node is a non-visual element (head, script, style, etc.)
fn is_non_visual_element(node: &Node) -> bool {
    if let Some(elem) = node.element_data() {
        let tag = elem.name.local.as_ref();
        matches!(
            tag,
            "head" | "script" | "style" | "link" | "meta" | "title" | "noscript"
        )
    } else {
        false
    }
}

/// Check whether a Pageable contains a ParagraphPageable (directly or nested).
pub(super) fn has_paragraph_descendant(p: &dyn Pageable) -> bool {
    if p.as_any().downcast_ref::<ParagraphPageable>().is_some() {
        return true;
    }
    if let Some(block) = p.as_any().downcast_ref::<BlockPageable>() {
        return block
            .children
            .iter()
            .any(|c| has_paragraph_descendant(c.child.as_ref()));
    }
    false
}

/// Get text color from a DOM node's computed styles.
fn get_text_color(doc: &BaseDocument, node_id: usize) -> [u8; 4] {
    if let Some(node) = doc.get_node(node_id)
        && let Some(styles) = node.primary_styles()
    {
        return absolute_to_rgba(styles.clone_color());
    }
    [0, 0, 0, 255] // Default: black
}

/// Get text-decoration properties from a DOM node's computed styles.
fn get_text_decoration(doc: &BaseDocument, node_id: usize) -> TextDecoration {
    if let Some(node) = doc.get_node(node_id)
        && let Some(styles) = node.primary_styles()
    {
        let current_color = styles.clone_color();

        // text-decoration-line (bitflags)
        let stylo_line = styles.clone_text_decoration_line();
        let mut line = TextDecorationLine::NONE;
        if stylo_line.contains(::style::values::specified::TextDecorationLine::UNDERLINE) {
            line = line | TextDecorationLine::UNDERLINE;
        }
        if stylo_line.contains(::style::values::specified::TextDecorationLine::OVERLINE) {
            line = line | TextDecorationLine::OVERLINE;
        }
        if stylo_line.contains(::style::values::specified::TextDecorationLine::LINE_THROUGH) {
            line = line | TextDecorationLine::LINE_THROUGH;
        }

        // text-decoration-style
        use ::style::properties::longhands::text_decoration_style::computed_value::T as StyloTDS;
        let style = match styles.clone_text_decoration_style() {
            StyloTDS::Solid => TextDecorationStyle::Solid,
            StyloTDS::Dashed => TextDecorationStyle::Dashed,
            StyloTDS::Dotted => TextDecorationStyle::Dotted,
            StyloTDS::Double => TextDecorationStyle::Double,
            StyloTDS::Wavy => TextDecorationStyle::Wavy,
            _ => TextDecorationStyle::Solid,
        };

        // text-decoration-color (resolve currentcolor)
        let deco_color = styles.clone_text_decoration_color();
        let color = absolute_to_rgba(deco_color.resolve_to_absolute(&current_color));

        return TextDecoration { line, style, color };
    }
    TextDecoration::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal 1x1 red PNG — matches crates/fulgur/src/image.rs tests but is
    // duplicated here so convert.rs tests don't depend on image.rs internals.
    const TEST_PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    pub(super) fn sample_png_arc() -> Arc<Vec<u8>> {
        Arc::new(TEST_PNG_1X1.to_vec())
    }

    pub(super) fn find_h1(doc: &blitz_html::HtmlDocument) -> usize {
        fn walk(doc: &BaseDocument, id: usize) -> Option<usize> {
            let node = doc.get_node(id)?;
            if let Some(ed) = node.element_data() {
                if ed.name.local.as_ref() == "h1" {
                    return Some(id);
                }
            }
            for &c in &node.children {
                if let Some(v) = walk(doc, c) {
                    return Some(v);
                }
            }
            None
        }
        walk(doc.deref(), doc.root_element().id).expect("h1 not found")
    }

    /// Recursively walk a Pageable tree and push any ImagePageable found.
    pub(super) fn collect_images(p: &dyn Pageable, out: &mut Vec<(f32, f32)>) {
        if let Some(img) = p.as_any().downcast_ref::<ImagePageable>() {
            out.push((img.width, img.height));
            return;
        }
        if let Some(block) = p.as_any().downcast_ref::<BlockPageable>() {
            for child in &block.children {
                collect_images(child.child.as_ref(), out);
            }
        }
    }

    /// Walk a Pageable tree visiting every nested child via known container
    /// types. Used by tests that need to peek through `ListItemPageable`'s
    /// body, which the simple BlockPageable-only walker does not descend into.
    pub(super) fn walk_all_children(p: &dyn Pageable, visit: &mut dyn FnMut(&dyn Pageable)) {
        visit(p);
        if let Some(block) = p.as_any().downcast_ref::<BlockPageable>() {
            for c in &block.children {
                walk_all_children(c.child.as_ref(), visit);
            }
        }
        if let Some(item) = p.as_any().downcast_ref::<ListItemPageable>() {
            walk_all_children(item.body.as_ref(), visit);
        }
    }

    /// Walk the DOM to find the first element with `tag` and return its node id.
    ///
    /// Used by bookmark fixtures below to populate `bookmark_by_node` directly
    /// without running the full `BookmarkPass` pipeline — these tests exercise
    /// the `convert_node` wrapping path in isolation.
    fn find_node_by_tag(doc: &blitz_html::HtmlDocument, tag: &str) -> Option<usize> {
        fn walk(doc: &BaseDocument, node_id: usize, tag: &str) -> Option<usize> {
            let node = doc.get_node(node_id)?;
            if let Some(el) = node.element_data() {
                if el.name.local.as_ref() == tag {
                    return Some(node_id);
                }
            }
            for &child_id in &node.children {
                if let Some(found) = walk(doc, child_id, tag) {
                    return Some(found);
                }
            }
            None
        }
        let root = doc.root_element();
        walk(doc.deref(), root.id, tag)
    }

    #[test]
    fn h1_wraps_block_with_bookmark_marker() {
        use crate::blitz_adapter::BookmarkInfo;
        use crate::pageable::BookmarkMarkerWrapperPageable;

        let html = r#"<html><body><h1>Chapter One</h1></body></html>"#;
        let doc = crate::blitz_adapter::parse_and_layout(html, 500.0, 500.0, &[]);
        let h1_id = find_node_by_tag(&doc, "h1").expect("h1 present in DOM");
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut bookmark_by_node = HashMap::new();
        bookmark_by_node.insert(
            h1_id,
            BookmarkInfo {
                level: 1,
                label: "Chapter One".to_string(),
            },
        );
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node,
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            pagination_geometry: ::std::collections::BTreeMap::new(),
            link_cache: Default::default(),
            viewport_size_px: None,
        };
        let root = dom_to_pageable(&doc, &mut ctx);

        fn collect(p: &dyn crate::pageable::Pageable, out: &mut Vec<(u8, String)>) {
            let any = p.as_any();
            if let Some(w) = any.downcast_ref::<BookmarkMarkerWrapperPageable>() {
                out.push((w.marker.level, w.marker.label.clone()));
                collect(w.child.as_ref(), out);
                return;
            }
            if let Some(b) = any.downcast_ref::<crate::pageable::BlockPageable>() {
                for c in &b.children {
                    collect(c.child.as_ref(), out);
                }
            }
        }
        let mut found = vec![];
        collect(root.as_ref(), &mut found);
        assert_eq!(found, vec![(1u8, "Chapter One".to_string())]);
    }

    #[test]
    fn h3_produces_level_3_marker() {
        use crate::blitz_adapter::BookmarkInfo;
        use crate::pageable::BookmarkMarkerWrapperPageable;

        let html = r#"<html><body><h3>Subsection</h3></body></html>"#;
        let doc = crate::blitz_adapter::parse_and_layout(html, 500.0, 500.0, &[]);
        let h3_id = find_node_by_tag(&doc, "h3").expect("h3 present in DOM");
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut bookmark_by_node = HashMap::new();
        bookmark_by_node.insert(
            h3_id,
            BookmarkInfo {
                level: 3,
                label: "Subsection".to_string(),
            },
        );
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node,
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            pagination_geometry: ::std::collections::BTreeMap::new(),
            link_cache: Default::default(),
            viewport_size_px: None,
        };
        let root = dom_to_pageable(&doc, &mut ctx);

        fn find(p: &dyn crate::pageable::Pageable) -> Option<(u8, String)> {
            let any = p.as_any();
            if let Some(w) = any.downcast_ref::<BookmarkMarkerWrapperPageable>() {
                return Some((w.marker.level, w.marker.label.clone()));
            }
            if let Some(b) = any.downcast_ref::<crate::pageable::BlockPageable>() {
                for c in &b.children {
                    if let Some(h) = find(c.child.as_ref()) {
                        return Some(h);
                    }
                }
            }
            None
        }
        assert_eq!(find(root.as_ref()), Some((3u8, "Subsection".to_string())));
    }

    /// Regression: a bookmark-bearing element that is 0-size/empty (and would
    /// normally be skipped in the zero-size-leaf branch of
    /// `collect_positioned_children`) must still produce a bookmark marker
    /// somewhere in the Pageable tree so that the outline entry is emitted.
    ///
    /// Mirrors `emit_orphan_string_set_markers`' regression case: without the
    /// orphan-emit path, `convert_node` is never called for the empty <div>
    /// and the marker is silently dropped.
    #[test]
    fn orphan_bookmark_marker_survives_empty_element() {
        use crate::blitz_adapter::BookmarkInfo;
        use crate::pageable::{BookmarkMarkerPageable, BookmarkMarkerWrapperPageable};

        // Forcing `width: 0; height: 0` yields a 0x0 block leaf — this is
        // the scenario `collect_positioned_children` skips via `continue`
        // (see `test_dom_to_pageable_emits_pseudo_on_zero_size_block_leaf`
        // for the analogous pseudo-image regression). Without
        // `emit_orphan_bookmark_marker`, the bookmark on the <div> would
        // be silently dropped.
        let html = r#"<!doctype html><html><head><style>
            .sentinel { display: block; width: 0; height: 0; }
        </style></head><body><section><div class="sentinel"></div></section></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 500.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let div_id = find_node_by_tag(&doc, "div").expect("div present in DOM");
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut bookmark_by_node = HashMap::new();
        bookmark_by_node.insert(
            div_id,
            BookmarkInfo {
                level: 1,
                label: "Chapter Empty".to_string(),
            },
        );
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node,
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            pagination_geometry: ::std::collections::BTreeMap::new(),
            link_cache: Default::default(),
            viewport_size_px: None,
        };
        let root = dom_to_pageable(&doc, &mut ctx);

        // The node should have been consumed from the map exactly once.
        assert!(
            ctx.bookmark_by_node.is_empty(),
            "bookmark_by_node entry must be removed by the orphan-emit path"
        );

        /// Recursively search the Pageable tree for any bookmark marker
        /// (bare `BookmarkMarkerPageable` or wrapped `BookmarkMarkerWrapperPageable`).
        fn find_marker(p: &dyn crate::pageable::Pageable) -> Option<(u8, String)> {
            let any = p.as_any();
            if let Some(m) = any.downcast_ref::<BookmarkMarkerPageable>() {
                return Some((m.level, m.label.clone()));
            }
            if let Some(w) = any.downcast_ref::<BookmarkMarkerWrapperPageable>() {
                return Some((w.marker.level, w.marker.label.clone()));
            }
            if let Some(b) = any.downcast_ref::<crate::pageable::BlockPageable>() {
                for c in &b.children {
                    if let Some(h) = find_marker(c.child.as_ref()) {
                        return Some(h);
                    }
                }
            }
            None
        }

        assert_eq!(
            find_marker(root.as_ref()),
            Some((1u8, "Chapter Empty".to_string())),
            "expected bookmark marker to survive empty-element skip/flatten"
        );
    }

    /// Locate the first element with the given tag by DFS from the document root.
    pub(super) fn find_tag(doc: &blitz_html::HtmlDocument, tag: &str) -> Option<usize> {
        fn walk(doc: &BaseDocument, id: usize, tag: &str) -> Option<usize> {
            let node = doc.get_node(id)?;
            if let Some(ed) = node.element_data() {
                if ed.name.local.as_ref() == tag {
                    return Some(id);
                }
            }
            for &c in &node.children {
                if let Some(v) = walk(doc, c, tag) {
                    return Some(v);
                }
            }
            None
        }
        walk(doc.deref(), doc.root_element().id, tag)
    }

    macro_rules! make_ctx {
        ($store:ident) => {{
            $crate::convert::ConvertContext {
                running_store: &$store,
                assets: None,
                font_cache: ::std::collections::HashMap::new(),
                string_set_by_node: ::std::collections::HashMap::new(),
                counter_ops_by_node: ::std::collections::HashMap::new(),
                bookmark_by_node: ::std::collections::HashMap::new(),
                column_styles: $crate::column_css::ColumnStyleTable::new(),
                multicol_geometry: $crate::multicol_layout::MulticolGeometryTable::new(),
                pagination_geometry: ::std::collections::BTreeMap::new(),
                link_cache: Default::default(),
                viewport_size_px: None,
            }
        }};
    }
    pub(super) use make_ctx;

    // ---- inside marker tests ----

    /// Walk a Pageable tree and check whether any ParagraphPageable's first line
    /// has a Text item whose text starts with the given marker string.
    pub(super) fn find_marker_text_in_tree(p: &dyn Pageable, marker: &str) -> bool {
        if let Some(para) = p.as_any().downcast_ref::<ParagraphPageable>() {
            if let Some(first_line) = para.lines.first() {
                for item in &first_line.items {
                    if let LineItem::Text(run) = item {
                        if run.text.starts_with(marker) {
                            return true;
                        }
                    }
                }
            }
        }
        if let Some(block) = p.as_any().downcast_ref::<BlockPageable>() {
            for c in &block.children {
                if find_marker_text_in_tree(c.child.as_ref(), marker) {
                    return true;
                }
            }
        }
        if let Some(item) = p.as_any().downcast_ref::<ListItemPageable>() {
            if find_marker_text_in_tree(item.body.as_ref(), marker) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod unit_oracle_tests {
    //! Oracle tests asserting that `BlockPageable.layout_size` (set directly
    //! from Taffy) has the correct width for a handful of CSS length units.
    //!
    //! Relative units (vw, %) are deliberately avoided for the absolute-
    //! width oracles because a viewport-relative unit compared against a
    //! content_width()-derived expectation is tautological: numerator and
    //! denominator scale together under a unit bug.
    use crate::pageable::{BlockPageable, Pageable};

    fn find_block_by_id<'a>(node: &'a dyn Pageable, id: &str) -> Option<&'a BlockPageable> {
        if let Some(block) = node.as_any().downcast_ref::<BlockPageable>() {
            if block.id.as_deref().map(|s| s.as_str()) == Some(id) {
                return Some(block);
            }
            for positioned in &block.children {
                if let Some(found) = find_block_by_id(positioned.child.as_ref(), id) {
                    return Some(found);
                }
            }
        }
        None
    }

    // The default `body { margin: 8px }` would leave `width:100%` ~12 pt
    // short of content_width — unrelated to the unit bug these tests
    // discriminate — so every fixture resets it.
    const BODY_RESET: &str = "<style>body{margin:0}</style>";

    fn assert_target_width(style: &str, expected_fn: impl FnOnce(&crate::Engine) -> f32) {
        let html = format!(
            r#"<html><head>{BODY_RESET}</head><body><div id="target" style="{style};background:red"></div></body></html>"#
        );
        let eng = crate::Engine::builder().build();
        let root = eng.build_pageable_for_testing_no_gcpm(&html);
        let block = find_block_by_id(root.as_ref(), "target").expect("target block");
        let size = block.layout_size.expect("layout_size populated");
        let expected = expected_fn(&eng);
        assert!(
            (size.width - expected).abs() < 0.5,
            "[{style}] expected {expected}pt, got {}pt",
            size.width
        );
    }

    #[test]
    fn width_100_percent_equals_content_width() {
        assert_target_width("width:100%;height:10pt", |e| e.config().content_width());
    }

    #[test]
    fn width_10cm_is_283_46_pt() {
        assert_target_width("width:10cm;height:1cm", |_| 10.0 * 72.0 / 2.54);
    }

    #[test]
    fn width_360px_is_270_pt() {
        assert_target_width("width:360px;height:10px", |_| 360.0 * 0.75);
    }

    #[test]
    fn width_1in_is_72_pt() {
        assert_target_width("width:1in;height:0.1in", |_| 72.0);
    }
}
