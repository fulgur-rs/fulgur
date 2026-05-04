//! Convert a Blitz DOM (after style resolution + layout) into a `Drawables`
//! struct holding per-NodeId draw payload (Phase 4 PR 8i).
//!
//! Phase 4 PR 8i replaced the previous "build a Pageable tree, then walk it
//! to extract Drawables" scaffold with a single DOM walk that writes
//! directly into `Drawables`'s per-NodeId maps. The intermediate Pageable
//! tree (and the orphan-marker / wrapper machinery that supported it) is
//! gone; bookmark / string-set / counter-op / running-element side-channels
//! are read from their respective stores by the fragmenter and render pass
//! independently.

use crate::asset::AssetBundle;
use crate::blitz_adapter::{BaseDocument, Node, NodeData};
use crate::draw_primitives::{BlockStyle, Size};
use crate::drawables::{ImageMarker, ListItemMarker};
use crate::gcpm::CounterOp;
use crate::gcpm::running::RunningElementStore;
use crate::image::ImageRender;
use crate::paragraph::{
    InlineImage, LineFontMetrics, LineItem, LinkSpan, LinkTarget, ShapedGlyph, ShapedGlyphRun,
    ShapedLine, TextDecoration, TextDecorationLine, TextDecorationStyle, VerticalAlign,
};
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
/// the Drawables / Krilla render path works in pt. Values cross the boundary
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

/// Map a stylo `text-align` keyword to the corresponding parley
/// `Alignment`. Mirrors blitz-dom's own mapping
/// (`blitz-dom-0.2.4/src/layout/inline.rs:142-152`) so split paragraph
/// fragments render with the same alignment Blitz uses for the
/// non-split path. CSS values not directly representable in
/// `parley::Alignment` (e.g. legacy `-moz-*` keywords) collapse to
/// their nearest equivalent; anything entirely unknown falls back to
/// `Alignment::Start`.
fn css_text_align_to_parley_alignment(
    text_align: ::style::values::specified::TextAlignKeyword,
) -> parley::Alignment {
    use ::style::values::specified::TextAlignKeyword;
    match text_align {
        TextAlignKeyword::Start => parley::Alignment::Start,
        TextAlignKeyword::Left => parley::Alignment::Left,
        TextAlignKeyword::Right => parley::Alignment::Right,
        TextAlignKeyword::Center => parley::Alignment::Center,
        TextAlignKeyword::Justify => parley::Alignment::Justify,
        TextAlignKeyword::End => parley::Alignment::End,
        TextAlignKeyword::MozCenter => parley::Alignment::Center,
        TextAlignKeyword::MozLeft => parley::Alignment::Left,
        TextAlignKeyword::MozRight => parley::Alignment::Right,
    }
}

/// Context for DOM-to-Drawables conversion, bundling all shared state.
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
    /// keyed by node_id for O(1) lookup. `dom_to_drawables` snapshots this
    /// map before walking the DOM and uses the snapshot to populate
    /// `drawables.bookmark_anchors`; the convert path itself no longer
    /// drains entries.
    pub bookmark_by_node: HashMap<usize, crate::blitz_adapter::BookmarkInfo>,
    /// Phase A `column-*` side-table harvested by
    /// [`crate::blitz_adapter::extract_column_style_table`]. `record_multicol_rule`
    /// reads `rule` properties from here when registering multicol containers
    /// in `drawables.multicol_rules`.
    pub column_styles: crate::column_css::ColumnStyleTable,
    /// Per-multicol-container geometry recorded by the Taffy multicol hook
    /// (see [`crate::multicol_layout::run_pass`]). `record_multicol_rule`
    /// reads this to register `column-rule` paint specs without re-running
    /// layout.
    pub multicol_geometry: crate::multicol_layout::MulticolGeometryTable,
    /// fulgur-cj6u Phase 1.1: per-body-child page-fragment geometry
    /// recorded by [`crate::pagination_layout::run_pass_with_break_styles`].
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
    /// (CSS 2.1 §10.1.5). See `positioned::resolve_cb_for_absolute`.
    pub viewport_size_px: Option<(f32, f32)>,
}

impl ConvertContext<'_> {
    /// Return a shared Arc for the given font data, caching by data pointer + index.
    ///
    /// Safety assumption: Parley font data pointers remain stable for the lifetime of
    /// this ConvertContext (scoped to a single `dom_to_drawables` call). HashMap is used
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

/// Phase 4 (fulgur-9t3z) + PR 8i: convert a resolved Blitz document into a
/// `Drawables` struct holding per-NodeId draw payload, walking the DOM
/// directly and writing entries into `drawables` as it goes.
pub fn dom_to_drawables(
    doc: &HtmlDocument,
    ctx: &mut ConvertContext<'_>,
) -> crate::drawables::Drawables {
    // Snapshot the bookmark map up-front so deletions from
    // `ctx.bookmark_by_node` later in the pipeline (none in convert today,
    // but kept for symmetry with engine-level callers) don't perturb the
    // outline projection.
    let bookmark_snapshot = ctx.bookmark_by_node.clone();
    let mut drawables = crate::drawables::Drawables::new();
    let root = doc.root_element();
    if std::env::var("FULGUR_DEBUG").is_ok() {
        debug_print_tree(doc.deref(), root.id, 0);
    }
    convert_node(doc.deref(), root.id, ctx, 0, &mut drawables);
    drawables.bookmark_anchors = extract_bookmark_anchors(doc, &bookmark_snapshot, ctx.assets);
    drawables.body_offset_pt = extract_body_offset_pt(doc);
    drawables.root_id = Some(root.id);
    drawables.body_id = find_body_id_in_dom(doc);
    record_semantics_pass(doc, &mut drawables);
    drawables
}

/// Locate the `<body>` element id by walking the html root's children.
/// Mirrors `pagination_layout::find_body_id` but operates on the
/// `HtmlDocument` API (the latter is private to that module).
fn find_body_id_in_dom(doc: &HtmlDocument) -> Option<usize> {
    use std::ops::Deref;
    let base = doc.deref();
    let root = doc.root_element();
    let root_node = base.get_node(root.id)?;
    for &child_id in &root_node.children {
        let Some(child) = base.get_node(child_id) else {
            continue;
        };
        if let blitz_dom::NodeData::Element(elem) = &child.data
            && elem.name.local.as_ref() == "body"
        {
            return Some(child_id);
        }
    }
    None
}

/// Walk the DOM to find the first `<body>` and return its
/// `(location.x, location.y)` in pt. The fragmenter records body's own
/// fragment at `(body_x, 0)` (body-content-area relative); the html →
/// body offset that CSS margin collapsing puts onto `body.location` lives
/// here so `render_v2` can add it to per-fragment draw positions.
fn extract_body_offset_pt(doc: &HtmlDocument) -> (f32, f32) {
    use std::ops::Deref;
    let base = doc.deref();
    let root = doc.root_element();
    let Some(root_node) = base.get_node(root.id) else {
        return (0.0, 0.0);
    };
    for &child_id in &root_node.children {
        let Some(child) = base.get_node(child_id) else {
            continue;
        };
        if let blitz_dom::NodeData::Element(elem) = &child.data
            && elem.name.local.as_ref() == "body"
        {
            let (x, y, _, _) = layout_in_pt(&child.final_layout);
            return (x, y);
        }
    }
    (0.0, 0.0)
}

/// Snapshot the union of `NodeId` keys currently present in `out`'s
/// per-NodeId maps. Used by `record_transform`, `block::convert`,
/// `table::try_convert`, and `inline_root::extract_paragraph` to compute
/// `descendants = after - before` so wrappers (transform / clip / opacity /
/// inline-box) can list every node added by walking their inner subtree.
pub(super) fn collect_drawables_node_ids(
    out: &crate::drawables::Drawables,
) -> std::collections::BTreeSet<usize> {
    let mut ids = std::collections::BTreeSet::new();
    ids.extend(out.block_styles.keys().copied());
    ids.extend(out.paragraphs.keys().copied());
    ids.extend(out.images.keys().copied());
    ids.extend(out.svgs.keys().copied());
    ids.extend(out.tables.keys().copied());
    ids.extend(out.list_items.keys().copied());
    ids
}

/// Build the bookmark anchor map. The `bookmark_by_node` map on
/// `ConvertContext` is populated upstream (`engine.rs` runs
/// `BookmarkPass` before `dom_to_drawables`); we only project it into
/// the `Drawables` shape. The `_doc` / `_assets` arguments are
/// reserved for future enrichment.
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

/// Convert a single DOM node into Drawables entries.
///
/// Wraps `convert_node_inner` with the post-pass that records `transform` /
/// `multicol-rule` entries by snapshotting the per-NodeId map keys before
/// recursion and diffing afterwards to find every descendant the inner
/// walk added. Bookmark / string-set / counter-op / running-element
/// wrapping is handled separately:
///
/// - `bookmark_anchors` is populated from `dom_to_drawables`'s up-front
///   snapshot of `ctx.bookmark_by_node`.
/// - String-set / counter-op / running-element side-channels feed the
///   fragmenter and render pass directly via the corresponding stores
///   (see `engine.rs`).
pub(super) fn convert_node(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let before = collect_drawables_node_ids(out);
    convert_node_inner(doc, node_id, ctx, depth, out);
    record_multicol_rule(doc, node_id, ctx, out);
    convert_multicol_paragraph_slices(doc, node_id, ctx, out);
    record_transform(doc, node_id, &before, out);
}

/// Walk the DOM top-down from `<body>` and populate `out.semantics`
/// with one `SemanticEntry` per element whose local name is recognised
/// by `crate::tagging::classify_element`. Runs as a standalone pass
/// after `convert_node` so the classification covers elements (e.g.
/// `<thead>`, `<tbody>`) that the per-type converters traverse via
/// custom child walks instead of recursing through `convert_node`.
///
/// `<head>` and its descendants are intentionally skipped — none of
/// them participate in the StructTree, and starting from `<body>`
/// keeps later expansions of `classify_element` (e.g. promoting
/// `<header>` / `<footer>` to dedicated tags) from accidentally
/// classifying `<head>`'s `<title>` / `<style>` etc.
///
/// fulgur-izp.3: pure data layer. The render path does not consume
/// these entries yet, so PDF byte equality is preserved across this
/// change.
fn record_semantics_pass(doc: &HtmlDocument, out: &mut crate::drawables::Drawables) {
    use std::ops::Deref;
    let base = doc.deref();
    let Some(body_id) = out.body_id else {
        return;
    };
    walk_semantics(base, body_id, 0, out);
}

fn walk_semantics(
    doc: &BaseDocument,
    node_id: usize,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if let Some(elem) = node.element_data() {
        if let Some(tag) = crate::tagging::classify_element(elem.name.local.as_ref()) {
            // Walk up `node.parent` until an already-recorded ancestor
            // is found. The traversal is top-down so any classified
            // ancestor is guaranteed to be present.
            let mut parent = node.parent;
            let parent_node_id = loop {
                let Some(pid) = parent else { break None };
                if out.semantics.contains_key(&pid) {
                    break Some(pid);
                }
                parent = doc.get_node(pid).and_then(|p| p.parent);
            };
            let alt_text = if matches!(tag, crate::tagging::PdfTag::Figure) {
                get_attr(elem, "alt").map(|v| std::sync::Arc::from(v))
            } else {
                None
            };
            out.semantics.insert(
                node_id,
                crate::tagging::SemanticEntry {
                    tag,
                    parent: parent_node_id,
                    alt_text,
                },
            );
        }
    }
    for &child_id in &node.children {
        walk_semantics(doc, child_id, depth + 1, out);
    }
}

/// Inner dispatcher. Tries each specialized converter in order; falls
/// through to `block::convert` as the catch-all.
fn convert_node_inner(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
    out: &mut crate::drawables::Drawables,
) {
    // List-item dispatch: outside marker / display:list-item fallback / inside marker.
    if list_item::try_convert(doc, node_id, ctx, depth, out) {
        return;
    }

    // Table dispatch: <table>.
    if table::try_convert(doc, node_id, ctx, depth, out) {
        return;
    }

    // Replaced-element dispatch: <img>, <svg>, content: url().
    if replaced::try_convert(doc, node_id, ctx, out) {
        return;
    }

    // Inline-root dispatch: paragraph + inline pseudo images.
    if inline_root::try_convert(doc, node_id, ctx, depth, out) {
        return;
    }

    block::convert(doc, node_id, ctx, depth, out);
}

/// Register a `TransformEntry` for `node_id` if its computed style
/// resolves to a non-identity transform. `before` is the set of
/// `NodeId`s present in `out` at the start of this node's walk; the
/// difference between that and the post-walk set (excluding `node_id`
/// itself) is the strict descendant list the render pass needs to paint
/// inside the transform's `push_transform` / `pop` group.
fn record_transform(
    doc: &BaseDocument,
    node_id: usize,
    before: &std::collections::BTreeSet<usize>,
    out: &mut crate::drawables::Drawables,
) {
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    let Some(styles) = node.primary_styles() else {
        return;
    };
    // PR 8i note: `compute_transform` is documented to take CSS px (per
    // `.claude/rules/coordinate-system.md` and Stylo's `LengthPercentage`
    // contract). The render path (`render::draw_under_transform`), however,
    // treats both the resulting `origin` Point2 and any translate components
    // baked into the matrix as PDF pt — they are added directly to pt-space
    // fragment positions. v1 worked around this mismatch by feeding pt-valued
    // dims to `compute_transform`, making it self-consistent at the cost of
    // technically violating the Stylo contract (Length is unitless from
    // Stylo's perspective, so the math still holds — only `%` resolution
    // would behave differently against a pt basis vs px basis, and
    // `transform-origin: 50%` round-trips identically through either basis).
    //
    // To keep PR 8i non-regressive, restore v1's pt feed. Plumbing px →
    // origin → pt conversion through render is a separate cleanup tracked
    // for a future PR (see `render::draw_under_transform`'s consumer of
    // `tx.origin`). Re-enabled by `transform_integration::
    // rotate_90_at_default_center_origin_fixes_center`.
    let (width_pt, height_pt) = size_in_pt(node.final_layout.size);
    let Some((matrix, origin)) =
        crate::blitz_adapter::compute_transform(&styles, width_pt, height_pt)
    else {
        return;
    };
    let after = collect_drawables_node_ids(out);
    let descendants: Vec<usize> = after
        .difference(before)
        .copied()
        .filter(|&id| id != node_id)
        .collect();
    out.transforms.insert(
        node_id,
        crate::drawables::TransformEntry {
            matrix,
            origin,
            descendants,
        },
    );
}

/// Register a `MulticolRuleEntry` for `node_id` if it is a multicol
/// container with a renderable `column-rule` spec and Taffy-recorded
/// geometry. No-op for non-multicol containers.
fn record_multicol_rule(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &ConvertContext<'_>,
    out: &mut crate::drawables::Drawables,
) {
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if !crate::blitz_adapter::is_multicol_container(node) {
        return;
    }
    let Some(rule) = ctx
        .column_styles
        .get(&node_id)
        .and_then(|props| props.rule)
        .filter(|r| r.style != crate::column_css::ColumnRuleStyle::None && r.width > 0.0)
    else {
        return;
    };
    let Some(geometry) = ctx.multicol_geometry.get(&node_id) else {
        return;
    };
    // `ColumnGroupGeometry` is recorded in CSS px; convert to pt so
    // downstream paint matches every other Drawables entry's units.
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
            paragraph_splits: Vec::new(),
        })
        .collect();
    out.multicol_rules.insert(
        node_id,
        crate::drawables::MulticolRuleEntry {
            rule,
            groups: groups_pt,
        },
    );
}

/// fulgur-6q5 Task 7: populate `Drawables.paragraph_slices` for a multicol
/// container from `MulticolGeometry::paragraph_splits`.
///
/// For every `ParagraphSplitEntry` recorded by `multicol_layout` against
/// this container, materialise one `ParagraphSlice` per non-empty column.
/// Each slice carries `Vec<ShapedLine>` rebased to the slice's own top
/// edge, mirroring the `ParagraphPageable::split` convention used for
/// continuation fragments after a page break (commit 9c0e092).
///
/// Two source-layout sources, distinguished by whether
/// `source_node_id == node_id`:
///
/// - **Case A** (container is itself an inline root): the container's
///   `inline_layout_data.layout` was shaped at the container's content
///   width; multicol recorded line indices against a clone re-broken at
///   `col_w`. Convert reproduces that re-broken clone here.
/// - **Case B** (a child element is the inline root): Blitz already
///   re-broke the child's parley layout at `col_w` during
///   `compute_child_layout`, so the line indices stored against
///   `inline_layout_data` line up directly.
///
/// Scope: this path covers plain-text inline-root paragraphs only. Inline
/// boxes / replaced content aren't handled here — the paragraphs that
/// `multicol_layout` actually splits across columns never carry inline
/// boxes (Task 4 / 5 only emit `ParagraphSplitEntry` for pure-text inline
/// roots), so the simpler `GlyphRun`-only loop is sufficient and keeps
/// us out of `convert_inline_box_node`'s out-mutating descendant
/// machinery.
fn convert_multicol_paragraph_slices(
    doc: &BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    out: &mut crate::drawables::Drawables,
) {
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if !crate::blitz_adapter::is_multicol_container(node) {
        return;
    }
    let Some(geometry) = ctx.multicol_geometry.get(&node_id).cloned() else {
        return;
    };
    for group in &geometry.groups {
        if group.paragraph_splits.is_empty() {
            continue;
        }
        let group_x_pt = px_to_pt(group.x_offset);
        let group_y_pt = px_to_pt(group.y_offset);
        let col_w_pt = px_to_pt(group.col_w);

        for split in &group.paragraph_splits {
            let source_id = split.source_node_id;
            let case_a = source_id == node_id;

            // Pull the source `(layout, text)` pair from the source
            // node's `inline_layout_data`. For Case A we additionally
            // clone the layout and re-break at `col_w` so the line
            // indices recorded by `layout_self_inline_root_container`
            // resolve correctly. (Case B's layout was already re-broken
            // by Blitz during `compute_child_layout`.)
            let Some(source_node) = doc.get_node(source_id) else {
                continue;
            };
            let Some(elem) = source_node.element_data() else {
                continue;
            };
            let Some(text_layout) = elem.inline_layout_data.as_ref() else {
                continue;
            };
            let text = text_layout.text.clone();

            // Hold the rebroken clone alive across the per-line loop in
            // Case A. In Case B the borrowed reference into Blitz is
            // sufficient because Blitz already broke at `col_w`.
            //
            // For alignment: Blitz's own inline layout pass aligns each
            // inline-root layout with that node's resolved
            // `text-align`. Case B inherits that alignment for free
            // because we read the existing parley layout in place. Case
            // A re-clones + re-breaks here, which would otherwise
            // discard alignment unless we re-apply it. The container
            // (which IS the inline root in Case A — `source_id ==
            // node_id`) supplies the keyword via its primary styles,
            // matching the mapping at
            // `blitz-dom-0.2.4/src/layout/inline.rs:142-152`.
            let owned_layout: Option<parley::Layout<blitz_dom::node::TextBrush>> = if case_a {
                let alignment = source_node
                    .primary_styles()
                    .map(|s| css_text_align_to_parley_alignment(s.clone_text_align()))
                    .unwrap_or(parley::Alignment::Start);
                let mut cloned = text_layout.layout.clone();
                cloned.break_all_lines(Some(group.col_w));
                cloned.align(
                    Some(group.col_w),
                    alignment,
                    parley::AlignmentOptions::default(),
                );
                Some(cloned)
            } else {
                None
            };
            let layout_ref: &parley::Layout<blitz_dom::node::TextBrush> = match &owned_layout {
                Some(cloned) => cloned,
                None => &text_layout.layout,
            };

            // Materialise one `ShapedLine` per parley line. This is a
            // simplified version of the per-line shaping in
            // `inline_root::extract_paragraph` covering only `GlyphRun`s
            // — see this function's doc comment for scope notes.
            let all_lines = shape_paragraph_glyph_runs(doc, layout_ref, &text, ctx);
            if all_lines.is_empty() {
                continue;
            }

            let mut slices = Vec::new();
            for col_slice in &split.column_slices {
                if col_slice.line_range.is_empty() {
                    continue;
                }
                if col_slice.line_range.end > all_lines.len() {
                    debug_assert!(
                        false,
                        "ParagraphSplitEntry line_range {:?} exceeds shaped line count {}",
                        col_slice.line_range,
                        all_lines.len(),
                    );
                    continue;
                }

                // Rebase per-line baselines into slice-local space.
                // This mirrors `ParagraphPageable::split`'s rebase for
                // continuation fragments (commit 9c0e092):
                //
                // 1. `line.baseline -= consumed` shifts each line's
                //    baseline from parley-layout space (paragraph top →
                //    baseline) to slice-local space (slice top →
                //    baseline). `consumed` is the cumulative height of
                //    all *prior* parley lines that this slice does not
                //    own.
                // 2. `recalculate_paragraph_line_boxes` then defensively
                //    re-accumulates per-line `line_box` heights starting
                //    at zero — for GlyphRun-only slices the per-line
                //    height is already parley-final so this is a no-op
                //    on baselines, but the call keeps the slice contract
                //    aligned with `ParagraphEntry::lines`'s contract
                //    (every consumer of `Vec<ShapedLine>` expects
                //    `recalculate_paragraph_line_boxes` to have run).
                let consumed: f32 = all_lines[..col_slice.line_range.start]
                    .iter()
                    .map(|l| l.height)
                    .sum();
                let mut lines: Vec<crate::paragraph::ShapedLine> = all_lines
                    [col_slice.line_range.clone()]
                .iter()
                .cloned()
                .map(|mut l| {
                    l.baseline -= consumed;
                    l
                })
                .collect();
                inline_root::recalculate_paragraph_line_boxes(&mut lines);

                let origin_pt = (
                    group_x_pt + px_to_pt(col_slice.origin.x),
                    group_y_pt + px_to_pt(col_slice.origin.y),
                );
                let size_pt = (col_w_pt, px_to_pt(col_slice.size.height));

                slices.push(crate::drawables::ParagraphSlice {
                    origin_pt,
                    size_pt,
                    lines,
                });
            }

            if !slices.is_empty() {
                out.paragraph_slices.insert(
                    source_id,
                    crate::drawables::ParagraphSlicesEntry {
                        container_node_id: node_id,
                        slices,
                    },
                );
            }
        }
    }
}

/// Shape one `Vec<ShapedLine>` out of a parley layout, covering only
/// `GlyphRun` items (no inline boxes). Used by
/// [`convert_multicol_paragraph_slices`] — see that function for scope
/// notes. Mirrors the `GlyphRun` arm of `inline_root::extract_paragraph`'s
/// per-line loop.
///
/// `ShapedLine.baseline` for each emitted line is the **parley layout's
/// cumulative offset from the layout top edge to that line's baseline**
/// (not line-local; not page-absolute). For line `i`, this equals
/// `Σ_{k=0..i} line_height[k] - leading_below[i] + ascent[i]` as parley
/// reports it via `LineMetrics::baseline`. Convert consumers must rebase
/// this value (subtract the slice's prior consumed height, then call
/// `inline_root::recalculate_paragraph_line_boxes`) when emitting
/// per-slice fragments — see `convert_multicol_paragraph_slices`.
fn shape_paragraph_glyph_runs(
    doc: &BaseDocument,
    parley_layout: &parley::Layout<blitz_dom::node::TextBrush>,
    text: &str,
    ctx: &mut ConvertContext<'_>,
) -> Vec<ShapedLine> {
    let mut shaped_lines = Vec::new();
    for line in parley_layout.lines() {
        let metrics = line.metrics();
        let mut items = Vec::new();
        for item in line.items() {
            if let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                let run = glyph_run.run();
                let font_ref = run.font();
                let font_index = font_ref.index;
                let font_arc = ctx.get_or_insert_font(font_ref);
                let font_size_parley = run.font_size();
                let font_size = px_to_pt(font_size_parley);

                let brush = &glyph_run.style().brush;
                let color = get_text_color(doc, brush.id);
                let decoration = get_text_decoration(doc, brush.id);
                let link = ctx.link_cache.lookup(doc, brush.id);

                let text_len = text.len();
                let mut glyphs = Vec::new();
                for g in glyph_run.glyphs() {
                    glyphs.push(ShapedGlyph {
                        id: g.id,
                        x_advance: g.advance / font_size_parley,
                        x_offset: g.x / font_size_parley,
                        y_offset: g.y / font_size_parley,
                        text_range: 0..text_len,
                    });
                }

                if !glyphs.is_empty() {
                    let run_text = text.to_string();
                    let run_x_offset = px_to_pt(glyph_run.offset());
                    items.push(LineItem::Text(ShapedGlyphRun {
                        font_data: font_arc,
                        font_index,
                        font_size,
                        color,
                        decoration,
                        glyphs,
                        text: run_text,
                        x_offset: run_x_offset,
                        link,
                    }));
                }
            }
            // InlineBox items are intentionally not handled — see
            // `convert_multicol_paragraph_slices`'s scope note.
        }

        let line_height_pt = px_to_pt(metrics.line_height);
        shaped_lines.push(ShapedLine {
            height: line_height_pt,
            baseline: px_to_pt(metrics.baseline),
            items,
        });
    }
    shaped_lines
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

/// Whether `node` is a `::before` / `::after` pseudo-element, detected by
/// checking that its parent's `before` / `after` slot points back to it.
fn is_pseudo_node(doc: &BaseDocument, node: &Node) -> bool {
    node.parent
        .and_then(|pid| doc.get_node(pid))
        .is_some_and(|p| p.before == Some(node.id) || p.after == Some(node.id))
}

/// Geometry of a parent's content-box, used by the pseudo-image helpers so
/// `::before`/`::after` land at the content-box corners (not the border-box
/// corners) and percentage sizes resolve against the content-box dimensions.
///
/// `origin_x` / `origin_y` were once used by `wrap_with_block_pseudo_images`
/// to position pseudo images at the content-box top-left / bottom-left.
/// In v2 the render path derives those positions from `pagination_geometry`,
/// so the fields are kept (for the eventual abs/fixed migration) but
/// allowed to be dead-code-eliminated.
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct ContentBox {
    origin_x: f32,
    origin_y: f32,
    width: f32,
    height: f32,
}

/// Compute the content-box of `node` from its computed style + Taffy layout.
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
mod bookmark_outline_tests {
    //! Single-test mod kept post PR 8i: the migrated end-to-end check that
    //! bookmark anchors reach the v2 outline pipeline. The v1-extractor unit
    //! tests that previously lived in `extract_drawables_tests` were
    //! redundant after the convert layer started writing into Drawables
    //! directly, so they were deleted along with the extractor.

    /// Regression: `dom_to_drawables` must snapshot `ctx.bookmark_by_node`
    /// **before** walking the DOM. End-to-end test through `Engine::render_html`
    /// (defaulted to v2 in PR 7) with `bookmarks(true)`: the rendered PDF
    /// must contain `/Outlines` because the v2 path builds the outline.
    #[test]
    fn dom_to_drawables_preserves_bookmark_anchors_for_outline() {
        use crate::config::PageSize;
        use crate::engine::Engine;

        let html = "<!DOCTYPE html><html><head><style>body{margin:0;padding:0}</style></head><body><h1>Heading</h1></body></html>";
        let engine = Engine::builder()
            .page_size(PageSize::A4)
            .bookmarks(true)
            .build();
        let pdf = engine.render_html(html).expect("render v2");
        let pdf_str = String::from_utf8_lossy(&pdf);
        assert!(
            pdf_str.contains("/Outlines"),
            "bookmark anchors must reach the v2 outline pipeline; PDF missing /Outlines"
        );
    }
}

#[cfg(test)]
mod semantics_tests {
    //! fulgur-izp.3: convert-side semantic tag classification. Verifies
    //! `dom_to_drawables` populates `Drawables.semantics` with the
    //! expected `(tag, parent)` pairs for representative HTML fixtures.
    //!
    //! Render-side wire-up (`fulgur-izp.4`) and StructTree assembly
    //! (`fulgur-izp.5`) are out of scope; these tests assert the data
    //! shape only.

    use crate::tagging::PdfTag;
    use std::ops::DerefMut;

    fn build_drawables(html: &str) -> crate::drawables::Drawables {
        // Drive the convert pipeline directly without the full Engine
        // so the assertions stay focused on `dom_to_drawables`.
        // `parse_and_layout` already runs stylo + Taffy + the
        // `position: fixed` relayout, matching what the engine feeds
        // into convert at this point.
        let mut doc = crate::blitz_adapter::parse_and_layout(html, 595.0, 842.0, &[]);

        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc);
        let multicol_geometry = crate::multicol_layout::run_pass(doc.deref_mut(), &column_styles);
        let pagination_geometry = crate::pagination_layout::run_pass(doc.deref_mut(), 842.0);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = super::ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: Default::default(),
            string_set_by_node: Default::default(),
            counter_ops_by_node: Default::default(),
            bookmark_by_node: Default::default(),
            column_styles,
            multicol_geometry,
            pagination_geometry,
            link_cache: Default::default(),
            viewport_size_px: Some((595.0, 842.0)),
        };
        super::dom_to_drawables(&doc, &mut ctx)
    }

    fn entries_by_tag(
        d: &crate::drawables::Drawables,
        target: &PdfTag,
    ) -> Vec<(usize, Option<usize>)> {
        d.semantics
            .iter()
            .filter(|(_, e)| e.tag == *target)
            .map(|(id, e)| (*id, e.parent))
            .collect()
    }

    #[test]
    fn dom_to_drawables_records_semantic_entries_for_block_elements() {
        let html = "<!DOCTYPE html><html><body><h1>T</h1><p>x</p><div><img src='a.png' alt='a'></div><p>y <span>inside</span> z</p></body></html>";
        let d = build_drawables(html);

        let h1s = entries_by_tag(&d, &PdfTag::H { level: 1 });
        assert_eq!(h1s.len(), 1, "expected one h1 entry");
        let ps = entries_by_tag(&d, &PdfTag::P);
        assert_eq!(ps.len(), 2, "expected two p entries");
        let divs = entries_by_tag(&d, &PdfTag::Div);
        assert_eq!(divs.len(), 1, "expected one div entry");
        let figures = entries_by_tag(&d, &PdfTag::Figure);
        assert_eq!(figures.len(), 1, "expected one figure entry for <img>");
        let spans = entries_by_tag(&d, &PdfTag::Span);
        assert_eq!(spans.len(), 1, "expected one span entry");

        let (img_id, img_parent) = figures[0];
        let (div_id, _) = divs[0];
        assert_eq!(
            img_parent,
            Some(div_id),
            "img semantic parent should be its enclosing div, got {img_parent:?} for img id {img_id}"
        );

        // span's parent must be one of the recorded paragraphs — the
        // exact NodeId depends on Blitz's parse order which is stable
        // but not part of the contract under test. Asserting set
        // membership keeps the test robust to renumbering.
        let p_ids: std::collections::BTreeSet<_> = ps.iter().map(|(id, _)| *id).collect();
        let (_, span_parent) = spans[0];
        assert!(
            span_parent.map(|p| p_ids.contains(&p)).unwrap_or(false),
            "span parent must be one of the recorded p NodeIds, got {span_parent:?}"
        );
    }

    #[test]
    fn dom_to_drawables_records_semantic_entries_for_lists() {
        let html = "<!DOCTYPE html><html><body><ul><li>a</li><li>b</li></ul></body></html>";
        let d = build_drawables(html);
        let lists = entries_by_tag(&d, &PdfTag::L);
        assert_eq!(lists.len(), 1, "expected one ul entry");
        let items = entries_by_tag(&d, &PdfTag::Li);
        assert_eq!(items.len(), 2, "expected two li entries");

        let (ul_id, _) = lists[0];
        for (li_id, parent) in &items {
            assert_eq!(
                *parent,
                Some(ul_id),
                "li {li_id} parent should be ul {ul_id}, got {parent:?}"
            );
        }
    }

    #[test]
    fn dom_to_drawables_records_semantic_entries_for_tables() {
        let html = "<!DOCTYPE html><html><body><table><thead><tr><th>h</th></tr></thead><tbody><tr><td>d</td></tr></tbody></table></body></html>";
        let d = build_drawables(html);

        let tables = entries_by_tag(&d, &PdfTag::Table);
        assert_eq!(tables.len(), 1);
        let row_groups = entries_by_tag(&d, &PdfTag::TRowGroup);
        assert_eq!(row_groups.len(), 2, "thead + tbody");
        let rows = entries_by_tag(&d, &PdfTag::Tr);
        assert_eq!(rows.len(), 2);
        let ths = entries_by_tag(&d, &PdfTag::Th);
        assert_eq!(ths.len(), 1);
        let tds = entries_by_tag(&d, &PdfTag::Td);
        assert_eq!(tds.len(), 1);

        let (table_id, _) = tables[0];
        for (_, parent) in &row_groups {
            assert_eq!(*parent, Some(table_id), "row group should parent to table");
        }
        // Each row's parent must be one of the row-group ids; each
        // header/data cell's parent must be one of the row ids. We
        // assert containment rather than specific ids because Blitz
        // assigns NodeIds during parse — the order is stable but
        // hard-coding ids would couple the test to internal numbering.
        let row_group_ids: std::collections::BTreeSet<_> =
            row_groups.iter().map(|(id, _)| *id).collect();
        for (_, parent) in &rows {
            assert!(
                parent.map(|p| row_group_ids.contains(&p)).unwrap_or(false),
                "tr parent must be a row group, got {parent:?}"
            );
        }
        let row_ids: std::collections::BTreeSet<_> = rows.iter().map(|(id, _)| *id).collect();
        for (_, parent) in ths.iter().chain(tds.iter()) {
            assert!(
                parent.map(|p| row_ids.contains(&p)).unwrap_or(false),
                "th/td parent must be a tr, got {parent:?}"
            );
        }
    }

    #[test]
    fn dom_to_drawables_skips_unrecognised_elements() {
        // Fixture intentionally contains only one classifiable element
        // (`<p>`). Everything else (`<script>`, `<a>`, `<custom-tag>`,
        // `<body>`, `<html>`) must produce no `SemanticEntry`. The
        // assertions below pin the *exact* contents of `semantics` so
        // any future regression that synthesises an extra entry from
        // an unrecognised element fails immediately, regardless of
        // which tag variant it picks.
        let html = "<!DOCTYPE html><html><body><script>x=1</script><a href='#'>link</a><custom-tag>y</custom-tag><p>z</p></body></html>";
        let d = build_drawables(html);

        let tags: Vec<&PdfTag> = d.semantics.values().map(|e| &e.tag).collect();
        assert_eq!(
            d.semantics.len(),
            1,
            "expected exactly one semantic entry for the <p>, got {} entries: {tags:?}",
            d.semantics.len()
        );
        let only_entry = d.semantics.values().next().expect("one entry asserted");
        assert_eq!(
            only_entry.tag,
            PdfTag::P,
            "the single entry must be the <p>, got {:?}",
            only_entry.tag
        );
    }

    #[test]
    fn dom_to_drawables_records_alt_text_on_figure() {
        // alt あり
        let d = build_drawables(
            "<!DOCTYPE html><html><body><img src='a.png' alt='photo of cat'></body></html>",
        );
        let figures: Vec<_> = d
            .semantics
            .values()
            .filter(|e| e.tag == PdfTag::Figure)
            .collect();
        assert_eq!(figures.len(), 1);
        assert_eq!(
            figures[0].alt_text.as_deref(),
            Some("photo of cat"),
            "alt text should be captured"
        );

        // alt="" decorative
        let d2 = build_drawables(
            "<!DOCTYPE html><html><body><img src='a.png' alt=''></body></html>",
        );
        let figs2: Vec<_> = d2
            .semantics
            .values()
            .filter(|e| e.tag == PdfTag::Figure)
            .collect();
        assert_eq!(figs2[0].alt_text.as_deref(), Some(""), "empty alt should be Some(\"\")");

        // alt 未指定
        let d3 = build_drawables(
            "<!DOCTYPE html><html><body><img src='a.png'></body></html>",
        );
        let figs3: Vec<_> = d3
            .semantics
            .values()
            .filter(|e| e.tag == PdfTag::Figure)
            .collect();
        assert_eq!(figs3[0].alt_text, None, "missing alt should be None");
    }
}
