//! Convert a Blitz DOM (after style resolution + layout) into a Pageable tree.

use crate::asset::AssetBundle;
use crate::gcpm::CounterOp;
use crate::gcpm::running::RunningElementStore;
use crate::image::ImagePageable;
use crate::pageable::{
    BackgroundLayer, BgBox, BgClip, BgImageContent, BgLengthPercentage, BgRepeat, BgSize,
    BlockPageable, BlockStyle, BorderStyleValue, CounterOpMarkerPageable, CounterOpWrapperPageable,
    ImageMarker, ListItemMarker, ListItemPageable, Pageable, PositionedChild,
    RunningElementMarkerPageable, RunningElementWrapperPageable, Size, SpacerPageable,
    StringSetPageable, StringSetWrapperPageable, TablePageable, TransformWrapperPageable,
};
use crate::paragraph::{
    InlineImage, LineFontMetrics, LineItem, LinkSpan, LinkTarget, ParagraphPageable, ShapedGlyph,
    ShapedGlyphRun, ShapedLine, TextDecoration, TextDecorationLine, TextDecorationStyle,
    VerticalAlign,
};
use crate::svg::SvgPageable;
use blitz_dom::{Node, NodeData};
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
mod table;

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
    /// Anchor (`<a href>`) resolution cache shared across the entire
    /// conversion. Lifted out of `extract_paragraph` because inline-box
    /// extraction recurses through `convert_node → extract_paragraph`, and a
    /// per-paragraph cache would hand back two distinct `Arc<LinkSpan>` for
    /// the same anchor — one for the outer inline-box rect and one for the
    /// glyphs inside the box — producing duplicate `/Link` annotations in
    /// the emitted PDF (LinkCollector dedupes by `Arc::ptr_eq`). A single
    /// long-lived cache guarantees pointer identity across the whole tree.
    pub(crate) link_cache: LinkCache,
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

fn debug_print_tree(doc: &blitz_dom::BaseDocument, node_id: usize, depth: usize) {
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
    doc: &blitz_dom::BaseDocument,
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
    doc: &blitz_dom::BaseDocument,
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
    doc: &blitz_dom::BaseDocument,
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
    doc: &blitz_dom::BaseDocument,
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
fn is_pseudo_node(doc: &blitz_dom::BaseDocument, node: &Node) -> bool {
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
    pub(crate) fn lookup(
        &mut self,
        doc: &blitz_dom::BaseDocument,
        start_id: usize,
    ) -> Option<Arc<LinkSpan>> {
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

/// Extract visual style (background, borders, padding, background-image) from a node.
fn extract_block_style(node: &Node, assets: Option<&AssetBundle>) -> BlockStyle {
    let layout = node.final_layout;
    let mut style = BlockStyle {
        border_widths: [
            px_to_pt(layout.border.top),
            px_to_pt(layout.border.right),
            px_to_pt(layout.border.bottom),
            px_to_pt(layout.border.left),
        ],
        padding: [
            px_to_pt(layout.padding.top),
            px_to_pt(layout.padding.right),
            px_to_pt(layout.padding.bottom),
            px_to_pt(layout.padding.left),
        ],
        ..Default::default()
    };

    // Extract colors from computed styles
    if let Some(styles) = node.primary_styles() {
        let current_color = styles.clone_color();

        // Background color — access the computed value directly
        let bg = styles.clone_background_color();
        let bg_rgba = absolute_to_rgba(bg.resolve_to_absolute(&current_color));
        if bg_rgba[3] > 0 {
            style.background_color = Some(bg_rgba);
        }

        // Border color (use top border color for all sides for simplicity)
        let bc = styles.clone_border_top_color();
        style.border_color = absolute_to_rgba(bc.resolve_to_absolute(&current_color));

        // Border radii. Stylo evaluates length-percentage values in CSS px
        // space, so we feed it the CSS-px border-box basis and convert the
        // returned radius to pt. border_radii is consumed downstream alongside
        // pt-space widths/heights (see `compute_padding_box_inner_radii`).
        let width = layout.size.width;
        let height = layout.size.height;
        let resolve_radius =
            |r: &style::values::computed::length_percentage::NonNegativeLengthPercentage,
             basis: f32|
             -> f32 {
                px_to_pt(
                    r.0.resolve(style::values::computed::Length::new(basis))
                        .px(),
                )
            };

        let tl = styles.clone_border_top_left_radius();
        let tr = styles.clone_border_top_right_radius();
        let br = styles.clone_border_bottom_right_radius();
        let bl = styles.clone_border_bottom_left_radius();

        style.border_radii = [
            [
                resolve_radius(&tl.0.width, width),
                resolve_radius(&tl.0.height, height),
            ],
            [
                resolve_radius(&tr.0.width, width),
                resolve_radius(&tr.0.height, height),
            ],
            [
                resolve_radius(&br.0.width, width),
                resolve_radius(&br.0.height, height),
            ],
            [
                resolve_radius(&bl.0.width, width),
                resolve_radius(&bl.0.height, height),
            ],
        ];

        // Box shadows
        let shadow_list = styles.clone_box_shadow();
        for shadow in shadow_list.0.iter() {
            if shadow.inset {
                log::warn!("box-shadow: inset is not yet supported; skipping");
                continue;
            }
            let blur_px = shadow.base.blur.px();
            if blur_px > 0.0 {
                log::warn!(
                    "box-shadow: blur-radius > 0 is not yet supported; \
                     drawing as blur=0 (blur={}px)",
                    blur_px
                );
            }
            let rgba = absolute_to_rgba(shadow.base.color.resolve_to_absolute(&current_color));
            if rgba[3] == 0 {
                continue; // fully transparent — skip
            }
            style.box_shadows.push(crate::pageable::BoxShadow {
                offset_x: px_to_pt(shadow.base.horizontal.px()),
                offset_y: px_to_pt(shadow.base.vertical.px()),
                blur: px_to_pt(blur_px),
                spread: px_to_pt(shadow.spread.px()),
                color: rgba,
                inset: false,
            });
        }

        // Border styles
        let convert_border_style = |bs: style::values::specified::BorderStyle| -> BorderStyleValue {
            use style::values::specified::BorderStyle as BS;
            match bs {
                BS::None | BS::Hidden => BorderStyleValue::None,
                BS::Dashed => BorderStyleValue::Dashed,
                BS::Dotted => BorderStyleValue::Dotted,
                BS::Double => BorderStyleValue::Double,
                BS::Groove => BorderStyleValue::Groove,
                BS::Ridge => BorderStyleValue::Ridge,
                BS::Inset => BorderStyleValue::Inset,
                BS::Outset => BorderStyleValue::Outset,
                BS::Solid => BorderStyleValue::Solid,
            }
        };
        style.border_styles = [
            convert_border_style(styles.clone_border_top_style()),
            convert_border_style(styles.clone_border_right_style()),
            convert_border_style(styles.clone_border_bottom_style()),
            convert_border_style(styles.clone_border_left_style()),
        ];

        // Overflow (CSS3 axis-independent interpretation)
        // PDF has no scroll concept: hidden/clip/scroll/auto all collapse to Clip.
        let map_overflow = |o: style::values::computed::Overflow| -> crate::pageable::Overflow {
            use style::values::computed::Overflow as S;
            match o {
                S::Visible => crate::pageable::Overflow::Visible,
                S::Hidden | S::Clip | S::Scroll | S::Auto => crate::pageable::Overflow::Clip,
            }
        };
        style.overflow_x = map_overflow(styles.clone_overflow_x());
        style.overflow_y = map_overflow(styles.clone_overflow_y());

        // Background image layers. Skip the six secondary `clone_*` calls
        // (sizes/positions/repeats/origins/clips) if no layer is actually
        // populated — the vast majority of DOM nodes have only `Image::None`.
        let bg_images = styles.clone_background_image();
        let has_real_bg_image = bg_images
            .0
            .iter()
            .any(|i| !matches!(i, style::values::computed::image::Image::None));
        if has_real_bg_image {
            let bg_sizes = styles.clone_background_size();
            let bg_pos_x = styles.clone_background_position_x();
            let bg_pos_y = styles.clone_background_position_y();
            let bg_repeats = styles.clone_background_repeat();
            let bg_origins = styles.clone_background_origin();
            let bg_clips = styles.clone_background_clip();

            for (i, image) in bg_images.0.iter().enumerate() {
                use style::values::computed::image::Image;

                // Resolve `content` + intrinsic size per image kind. URL images
                // require an `AssetBundle`; gradients are self-contained.
                let resolved: Option<(BgImageContent, f32, f32)> = match image {
                    Image::Url(url) => assets.and_then(|a| {
                        let raw_src = match url {
                            style::servo::url::ComputedUrl::Valid(u) => u.as_str(),
                            style::servo::url::ComputedUrl::Invalid(s) => s.as_str(),
                        };
                        let src = extract_asset_name(raw_src);
                        let data = a.get_image(src)?;

                        use crate::image::AssetKind;
                        match AssetKind::detect(data) {
                            AssetKind::Raster(format) => {
                                let (iw, ih) = ImagePageable::decode_dimensions(data, format)
                                    .unwrap_or((1, 1));
                                Some((
                                    BgImageContent::Raster {
                                        data: Arc::clone(data),
                                        format,
                                    },
                                    iw as f32,
                                    ih as f32,
                                ))
                            }
                            AssetKind::Svg => {
                                let opts = usvg::Options::default();
                                match usvg::Tree::from_data(data, &opts) {
                                    Ok(tree) => {
                                        let svg_size = tree.size();
                                        Some((
                                            BgImageContent::Svg {
                                                tree: Arc::new(tree),
                                            },
                                            svg_size.width(),
                                            svg_size.height(),
                                        ))
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "failed to parse SVG background-image '{src}': {e}"
                                        );
                                        None
                                    }
                                }
                            }
                            AssetKind::Unknown => None,
                        }
                    }),
                    Image::Gradient(g) => {
                        use style::values::computed::image::Gradient;
                        // g: &Box<Gradient> なので as_ref() で &Gradient を取って match。
                        match g.as_ref() {
                            Gradient::Linear { .. } => resolve_linear_gradient(g, &current_color),
                            Gradient::Radial { .. } => resolve_radial_gradient(g, &current_color),
                            Gradient::Conic { .. } => resolve_conic_gradient(g, &current_color),
                        }
                    }
                    _ => None,
                };

                if let Some((content, intrinsic_width, intrinsic_height)) = resolved {
                    let size = convert_bg_size(&bg_sizes.0, i);
                    let (px, py) = convert_bg_position(&bg_pos_x.0, &bg_pos_y.0, i);
                    let (rx, ry) = convert_bg_repeat(&bg_repeats.0, i);
                    let origin = convert_bg_origin(&bg_origins.0, i);
                    let clip = convert_bg_clip(&bg_clips.0, i);

                    style.background_layers.push(BackgroundLayer {
                        content,
                        intrinsic_width,
                        intrinsic_height,
                        size,
                        position_x: px,
                        position_y: py,
                        repeat_x: rx,
                        repeat_y: ry,
                        origin,
                        clip,
                    });
                }
            }
        }
    }

    style
}

/// Extract CSS opacity and visibility from computed styles.
/// Returns `(opacity, visible)` with defaults `(1.0, true)`.
fn extract_opacity_visible(node: &Node) -> (f32, bool) {
    use style::properties::longhands::visibility::computed_value::T as Visibility;
    node.primary_styles()
        .map(|s| {
            let opacity = s.clone_opacity();
            let v = s.clone_visibility();
            let visible = v != Visibility::Hidden && v != Visibility::Collapse;
            (opacity, visible)
        })
        .unwrap_or((1.0, true))
}

/// Extract the asset name from a URL that Stylo may have resolved to absolute.
/// e.g. "file:///bg.png" → "bg.png", "file:///images/bg.png" → "images/bg.png",
/// "bg.png" → "bg.png" (passthrough for unresolved URLs).
fn extract_asset_name(url: &str) -> &str {
    url.strip_prefix("file:///").unwrap_or(url)
}

fn absolute_to_rgba(c: style::color::AbsoluteColor) -> [u8; 4] {
    // `.round()` (not `as u8` truncation) so e.g. `rgb(127.5,…)` lands on 128
    // instead of 127. Truncation introduces a half-channel down-bias for
    // every fractional component, which is most visible in gradient stops.
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [
        q(c.components.0),
        q(c.components.1),
        q(c.components.2),
        q(c.alpha),
    ]
}

/// Convert a Stylo computed `Gradient` into fulgur's `BgImageContent`.
///
/// Phase 1 supports `linear-gradient(...)` only:
/// - Direction via explicit angle, `to top/right/bottom/left` keyword, or
///   `to <h> <v>` corner. Corner directions are stored as a flag and
///   resolved against the gradient box at draw time (CSS Images 3 §3.1.1
///   defines them in terms of W and H).
/// - Color stops with explicit `<percentage>` positions, plus auto stops
///   (positions filled in via even spacing between adjacent fixed stops, per
///   CSS Images §3.5.1).
/// - Length-typed stops (`linear-gradient(red 50px, blue)`) are unsupported
///   in Phase 1 because resolving them requires the gradient line length,
///   which depends on the box dimensions (only known at draw time). Falls
///   back to `None` for now.
/// - Repeating gradients, `radial-gradient`, `conic-gradient`, color
///   interpolation methods, and interpolation hints are unsupported.
///
/// Returned tuple: `(content, intrinsic_w, intrinsic_h)`. Gradients have no
/// intrinsic size, so we return `(0.0, 0.0)` and the draw path special-cases
/// gradients to fill the origin rect directly (`background.rs` does not
/// route gradients through `resolve_size` / tiling).
fn resolve_linear_gradient(
    g: &style::values::computed::Gradient,
    current_color: &style::color::AbsoluteColor,
) -> Option<(BgImageContent, f32, f32)> {
    use crate::pageable::{LinearGradientCorner, LinearGradientDirection};
    use style::values::computed::image::{Gradient, LineDirection};
    use style::values::generics::image::GradientFlags;
    use style::values::specified::position::{HorizontalPositionKeyword, VerticalPositionKeyword};

    let (direction, items, flags) = match g {
        Gradient::Linear {
            direction,
            items,
            flags,
            ..
        } => (direction, items, flags),
        Gradient::Radial { .. } | Gradient::Conic { .. } => return None,
    };

    let repeating = flags.contains(GradientFlags::REPEATING);
    // Non-default `color_interpolation_method` (e.g. `in oklch`) would change
    // the rendered colors. Phase 1 interpolates in sRGB only, so bail rather
    // than silently misrender.
    if !flags.contains(GradientFlags::HAS_DEFAULT_COLOR_INTERPOLATION_METHOD) {
        return None;
    }

    let direction = match direction {
        LineDirection::Angle(a) => LinearGradientDirection::Angle(a.radians()),
        LineDirection::Horizontal(HorizontalPositionKeyword::Right) => {
            LinearGradientDirection::Angle(std::f32::consts::FRAC_PI_2)
        }
        LineDirection::Horizontal(HorizontalPositionKeyword::Left) => {
            LinearGradientDirection::Angle(3.0 * std::f32::consts::FRAC_PI_2)
        }
        LineDirection::Vertical(VerticalPositionKeyword::Top) => {
            LinearGradientDirection::Angle(0.0)
        }
        LineDirection::Vertical(VerticalPositionKeyword::Bottom) => {
            LinearGradientDirection::Angle(std::f32::consts::PI)
        }
        LineDirection::Corner(h, v) => {
            use HorizontalPositionKeyword::*;
            use VerticalPositionKeyword::*;
            let corner = match (h, v) {
                (Left, Top) => LinearGradientCorner::TopLeft,
                (Right, Top) => LinearGradientCorner::TopRight,
                (Left, Bottom) => LinearGradientCorner::BottomLeft,
                (Right, Bottom) => LinearGradientCorner::BottomRight,
            };
            LinearGradientDirection::Corner(corner)
        }
    };

    let stops = resolve_color_stops(items, current_color, "linear-gradient")?;

    Some((
        BgImageContent::LinearGradient {
            direction,
            stops,
            repeating,
        },
        0.0,
        0.0,
    ))
}

/// CSS gradient items から GradientStop ベクタを解決する。linear / radial 共通。
///
/// position は `GradientStopPosition` で保持され (Auto / Fraction / LengthPx)、
/// draw 時に `background::resolve_gradient_stops` で gradient line 長さを
/// 使って fraction 化される。convert 時の fixup は行わない。
///
/// Bail 条件:
/// - stops.len() < 2 (規定上 invalid)
/// - interpolation hint (Phase 2 別 issue)
/// - position が percentage でも length でもない (calc() 等 — Phase 2)
fn resolve_color_stops(
    items: &[style::values::generics::image::GenericGradientItem<
        style::values::computed::Color,
        style::values::computed::LengthPercentage,
    >],
    current_color: &style::color::AbsoluteColor,
    gradient_kind: &'static str,
) -> Option<Vec<crate::pageable::GradientStop>> {
    use crate::pageable::{GradientStop, GradientStopPosition};
    use style::values::generics::image::GradientItem;

    let mut out: Vec<GradientStop> = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item {
            GradientItem::SimpleColorStop(c) => {
                let abs = c.resolve_to_absolute(current_color);
                out.push(GradientStop {
                    position: GradientStopPosition::Auto,
                    rgba: absolute_to_rgba(abs),
                });
            }
            GradientItem::ComplexColorStop { color, position } => {
                let abs = color.resolve_to_absolute(current_color);
                let pos = if let Some(pct) = position.to_percentage() {
                    GradientStopPosition::Fraction(pct.0)
                } else if let Some(len) = position.to_length() {
                    GradientStopPosition::LengthPx(len.px())
                } else {
                    log::warn!(
                        "{gradient_kind}: stop position is neither percentage \
                         nor length (calc() etc.). Layer dropped."
                    );
                    return None;
                };
                out.push(GradientStop {
                    position: pos,
                    rgba: absolute_to_rgba(abs),
                });
            }
            GradientItem::InterpolationHint(_) => {
                log::warn!(
                    "{gradient_kind}: interpolation hints are not yet supported \
                     (Phase 2). Layer dropped."
                );
                return None;
            }
        }
    }

    if out.len() < 2 {
        return None;
    }

    Some(out)
}

/// Convert a Stylo computed `Gradient::Radial` into fulgur's `BgImageContent::RadialGradient`.
///
/// Phase 1 scope (per beads issue fulgur-gm56 design):
/// - shape: circle / ellipse
/// - size: extent keyword (closest-side / farthest-side / closest-corner / farthest-corner) or
///   explicit length / length-percentage radii (resolved at draw time against gradient box)
/// - position: keyword + length-percentage の組合せ (BgLengthPercentage 経由)
/// - stops: linear と共通の resolve_color_stops を使用
///
/// Bail conditions (return None) — match resolve_linear_gradient:
/// - non-default color interpolation method
/// - length-typed / 範囲外 stop position, interpolation hint (resolve_color_stops 内)
///
/// `repeating-radial-gradient(...)` は `repeating: true` で受け、draw 時に
/// stop の周期展開で表現する (Krilla の RadialGradient は SpreadMethod::Pad のみ)。
fn resolve_radial_gradient(
    g: &style::values::computed::Gradient,
    current_color: &style::color::AbsoluteColor,
) -> Option<(BgImageContent, f32, f32)> {
    use crate::pageable::{RadialGradientShape, RadialGradientSize};
    use style::values::computed::image::Gradient;
    use style::values::generics::image::{Circle, Ellipse, EndingShape, GradientFlags};

    let (shape, position, items, flags) = match g {
        Gradient::Radial {
            shape,
            position,
            items,
            flags,
            ..
        } => (shape, position, items, flags),
        Gradient::Linear { .. } | Gradient::Conic { .. } => return None,
    };

    let repeating = flags.contains(GradientFlags::REPEATING);
    if !flags.contains(GradientFlags::HAS_DEFAULT_COLOR_INTERPOLATION_METHOD) {
        return None;
    }

    let (out_shape, out_size) = match shape {
        EndingShape::Circle(Circle::Radius(r)) => {
            // r: NonNegativeLength = NonNegative<Length>。.0.px() で CSS px、px_to_pt() で pt 化。
            let len_pt = px_to_pt(r.0.px());
            (
                RadialGradientShape::Circle,
                RadialGradientSize::Explicit {
                    rx: BgLengthPercentage::Length(len_pt),
                    ry: BgLengthPercentage::Length(len_pt),
                },
            )
        }
        EndingShape::Circle(Circle::Extent(ext)) => (
            RadialGradientShape::Circle,
            RadialGradientSize::Extent(map_extent(*ext)),
        ),
        EndingShape::Ellipse(Ellipse::Radii(rx, ry)) => (
            RadialGradientShape::Ellipse,
            RadialGradientSize::Explicit {
                rx: try_convert_lp_to_bg(&rx.0)?,
                ry: try_convert_lp_to_bg(&ry.0)?,
            },
        ),
        EndingShape::Ellipse(Ellipse::Extent(ext)) => (
            RadialGradientShape::Ellipse,
            RadialGradientSize::Extent(map_extent(*ext)),
        ),
    };

    // computed::Position::horizontal / vertical はどちらも LengthPercentage 直接 (wrapper なし)。
    // calc() 等 resolve 不能な値は silent 0 で誤描画させずに layer drop する。
    let position_x = try_convert_lp_to_bg(&position.horizontal)?;
    let position_y = try_convert_lp_to_bg(&position.vertical)?;

    let stops = resolve_color_stops(items, current_color, "radial-gradient")?;

    Some((
        BgImageContent::RadialGradient {
            shape: out_shape,
            size: out_size,
            position_x,
            position_y,
            stops,
            repeating,
        },
        0.0,
        0.0,
    ))
}

/// Convert Stylo `Gradient::Conic` into `BgImageContent::ConicGradient`.
///
/// stop position は `<angle>` を `angle / 2π` で fraction 化、`<percentage>` は
/// そのまま fraction として `GradientStopPosition::Fraction(f32)` に格納する。
/// `[0, 1]` 範囲外の生値もそのまま許容し (例: `-30deg → -0.083`, `120% → 1.2`)、
/// 最終的な周期展開 / 範囲ハンドリングは `background.rs::draw_conic_gradient`
/// と `sample_conic_color` 側に委ねる。
fn resolve_conic_gradient(
    g: &style::values::computed::Gradient,
    current_color: &style::color::AbsoluteColor,
) -> Option<(BgImageContent, f32, f32)> {
    use style::values::computed::AngleOrPercentage;
    use style::values::computed::image::Gradient;
    use style::values::generics::image::{GradientFlags, GradientItem};

    let (angle, position, items, flags) = match g {
        Gradient::Conic {
            angle,
            position,
            items,
            flags,
            ..
        } => (angle, position, items, flags),
        Gradient::Linear { .. } | Gradient::Radial { .. } => return None,
    };

    if !flags.contains(GradientFlags::HAS_DEFAULT_COLOR_INTERPOLATION_METHOD) {
        log::warn!(
            "conic-gradient: non-default color-interpolation-method is not yet \
             supported. Layer dropped."
        );
        return None;
    }

    let from_angle = angle.radians();
    let position_x = try_convert_lp_to_bg(&position.horizontal)?;
    let position_y = try_convert_lp_to_bg(&position.vertical)?;
    let repeating = flags.contains(GradientFlags::REPEATING);

    let mut stops: Vec<crate::pageable::GradientStop> = Vec::with_capacity(items.len());
    for item in items.iter() {
        use crate::pageable::{GradientStop, GradientStopPosition};
        match item {
            GradientItem::SimpleColorStop(c) => {
                let abs = c.resolve_to_absolute(current_color);
                stops.push(GradientStop {
                    position: GradientStopPosition::Auto,
                    rgba: absolute_to_rgba(abs),
                });
            }
            GradientItem::ComplexColorStop { color, position } => {
                let abs = color.resolve_to_absolute(current_color);
                let frac = match position {
                    AngleOrPercentage::Percentage(p) => p.0,
                    AngleOrPercentage::Angle(a) => a.radians() / std::f32::consts::TAU,
                };
                stops.push(GradientStop {
                    position: GradientStopPosition::Fraction(frac),
                    rgba: absolute_to_rgba(abs),
                });
            }
            GradientItem::InterpolationHint(_) => {
                log::warn!(
                    "conic-gradient: interpolation hints are not yet supported. \
                     Layer dropped."
                );
                return None;
            }
        }
    }
    if stops.len() < 2 {
        return None;
    }

    Some((
        BgImageContent::ConicGradient {
            from_angle,
            position_x,
            position_y,
            stops,
            repeating,
        },
        0.0,
        0.0,
    ))
}

fn map_extent(e: style::values::generics::image::ShapeExtent) -> crate::pageable::RadialExtent {
    use crate::pageable::RadialExtent;
    use style::values::generics::image::ShapeExtent;
    match e {
        ShapeExtent::ClosestSide => RadialExtent::ClosestSide,
        ShapeExtent::FarthestSide => RadialExtent::FarthestSide,
        ShapeExtent::ClosestCorner => RadialExtent::ClosestCorner,
        ShapeExtent::FarthestCorner => RadialExtent::FarthestCorner,
        // CSS Images §3.6.1: Contain == ClosestSide のエイリアス、Cover == FarthestCorner のエイリアス。
        ShapeExtent::Contain => RadialExtent::ClosestSide,
        ShapeExtent::Cover => RadialExtent::FarthestCorner,
    }
}

fn convert_bg_size(sizes: &[style::values::computed::BackgroundSize], i: usize) -> BgSize {
    use style::values::generics::background::BackgroundSize as StyloBS;
    use style::values::generics::length::GenericLengthPercentageOrAuto as LPAuto;
    let s = &sizes[i % sizes.len()];
    match s {
        StyloBS::Cover => BgSize::Cover,
        StyloBS::Contain => BgSize::Contain,
        StyloBS::ExplicitSize { width, height } => {
            let w = match width {
                LPAuto::Auto => None,
                LPAuto::LengthPercentage(lp) => Some(convert_lp_to_bg(&lp.0)),
            };
            let h = match height {
                LPAuto::Auto => None,
                LPAuto::LengthPercentage(lp) => Some(convert_lp_to_bg(&lp.0)),
            };
            if w.is_none() && h.is_none() {
                BgSize::Auto
            } else {
                BgSize::Explicit(w, h)
            }
        }
    }
}

/// Convert Stylo LengthPercentage to BgLengthPercentage.
/// Note: calc() values (e.g. `calc(50% + 10px)`) are not fully supported —
/// they fall back to 0.0 if neither pure percentage nor pure length.
/// 呼び出し側が "silent 0.0 で良い" 場面 (background-position / -size の Phase 1) のみ
/// 使うこと。radial-gradient の半径や中心位置のように 0 が誤描画になる場面では
/// `try_convert_lp_to_bg` を使って calc() を None にして bail する。
fn convert_lp_to_bg(lp: &style::values::computed::LengthPercentage) -> BgLengthPercentage {
    if let Some(pct) = lp.to_percentage() {
        BgLengthPercentage::Percentage(pct.0)
    } else {
        BgLengthPercentage::Length(lp.to_length().map(|l| px_to_pt(l.px())).unwrap_or(0.0))
    }
}

/// `convert_lp_to_bg` の Option 版。calc() 等の resolve 不能な値で `None` を返す。
/// silent 0.0 fallback では誤描画になる radial-gradient の半径 / 中心位置で使う
/// (CodeRabbit #238 で指摘)。
fn try_convert_lp_to_bg(
    lp: &style::values::computed::LengthPercentage,
) -> Option<BgLengthPercentage> {
    if let Some(pct) = lp.to_percentage() {
        Some(BgLengthPercentage::Percentage(pct.0))
    } else {
        lp.to_length()
            .map(|l| BgLengthPercentage::Length(px_to_pt(l.px())))
    }
}

fn convert_bg_position(
    pos_x: &[style::values::computed::LengthPercentage],
    pos_y: &[style::values::computed::LengthPercentage],
    i: usize,
) -> (BgLengthPercentage, BgLengthPercentage) {
    let px = &pos_x[i % pos_x.len()];
    let py = &pos_y[i % pos_y.len()];
    (convert_lp_to_bg(px), convert_lp_to_bg(py))
}

fn convert_bg_repeat(
    repeats: &[style::values::specified::background::BackgroundRepeat],
    i: usize,
) -> (BgRepeat, BgRepeat) {
    use style::values::specified::background::BackgroundRepeatKeyword as BRK;
    let r = &repeats[i % repeats.len()];
    let map = |k: BRK| match k {
        BRK::Repeat => BgRepeat::Repeat,
        BRK::NoRepeat => BgRepeat::NoRepeat,
        BRK::Space => BgRepeat::Space,
        BRK::Round => BgRepeat::Round,
    };
    (map(r.0), map(r.1))
}

fn convert_bg_origin(
    origins: &[style::properties::longhands::background_origin::single_value::computed_value::T],
    i: usize,
) -> BgBox {
    use style::properties::longhands::background_origin::single_value::computed_value::T as O;
    match origins[i % origins.len()] {
        O::BorderBox => BgBox::BorderBox,
        O::PaddingBox => BgBox::PaddingBox,
        O::ContentBox => BgBox::ContentBox,
    }
}

fn convert_bg_clip(
    clips: &[style::properties::longhands::background_clip::single_value::computed_value::T],
    i: usize,
) -> BgClip {
    use style::properties::longhands::background_clip::single_value::computed_value::T as C;
    match clips[i % clips.len()] {
        C::BorderBox => BgClip::BorderBox,
        C::PaddingBox => BgClip::PaddingBox,
        C::ContentBox => BgClip::ContentBox,
    }
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
fn get_text_color(doc: &blitz_dom::BaseDocument, node_id: usize) -> [u8; 4] {
    if let Some(node) = doc.get_node(node_id)
        && let Some(styles) = node.primary_styles()
    {
        return absolute_to_rgba(styles.clone_color());
    }
    [0, 0, 0, 255] // Default: black
}

/// Get text-decoration properties from a DOM node's computed styles.
fn get_text_decoration(doc: &blitz_dom::BaseDocument, node_id: usize) -> TextDecoration {
    if let Some(node) = doc.get_node(node_id)
        && let Some(styles) = node.primary_styles()
    {
        let current_color = styles.clone_color();

        // text-decoration-line (bitflags)
        let stylo_line = styles.clone_text_decoration_line();
        let mut line = TextDecorationLine::NONE;
        if stylo_line.contains(style::values::specified::TextDecorationLine::UNDERLINE) {
            line = line | TextDecorationLine::UNDERLINE;
        }
        if stylo_line.contains(style::values::specified::TextDecorationLine::OVERLINE) {
            line = line | TextDecorationLine::OVERLINE;
        }
        if stylo_line.contains(style::values::specified::TextDecorationLine::LINE_THROUGH) {
            line = line | TextDecorationLine::LINE_THROUGH;
        }

        // text-decoration-style
        use style::properties::longhands::text_decoration_style::computed_value::T as StyloTDS;
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
        fn walk(doc: &blitz_dom::BaseDocument, id: usize) -> Option<usize> {
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
        fn walk(doc: &blitz_dom::BaseDocument, node_id: usize, tag: &str) -> Option<usize> {
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
            link_cache: Default::default(),
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
            link_cache: Default::default(),
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
            link_cache: Default::default(),
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
        fn walk(doc: &blitz_dom::BaseDocument, id: usize, tag: &str) -> Option<usize> {
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
                link_cache: Default::default(),
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
