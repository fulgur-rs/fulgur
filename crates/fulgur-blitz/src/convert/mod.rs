//! Convert a Blitz DOM (after style resolution + layout) into a `Drawables`
//! struct holding per-NodeId draw payload.
//!
//! A single DOM walk writes directly into `Drawables`'s per-NodeId maps.
//! Bookmark / string-set / counter-op / running-element side-channels are
//! read from their respective stores by the fragmenter and render pass
//! independently.

use crate::asset::AssetBundle;
use crate::blitz_adapter::NodeLayoutExt;
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
use crate::units::{F32Units, Pt, Px};
use blitz_dom::CopyStyleGetters;
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

/// Convert a Taffy `Layout` (CSS px) to PDF pt as `(x, y, width, height)`.
#[inline]
fn layout_in_pt(layout: &taffy::Layout) -> (Pt, Pt, Pt, Pt) {
    (
        layout.location.x.as_px().in_pt(),
        layout.location.y.as_px().in_pt(),
        layout.size.width.as_px().in_pt(),
        layout.size.height.as_px().in_pt(),
    )
}

/// Convert a Taffy `Size<f32>` (CSS px) to PDF pt as `(width, height)`.
#[inline]
fn size_in_pt(size: taffy::Size<f32>) -> (Pt, Pt) {
    (size.width.as_px().in_pt(), size.height.as_px().in_pt())
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
    pub(crate) font_cache: HashMap<(usize, u32), crate::paragraph::FontBytes>,
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
    pub(crate) column_styles: crate::column_css::ColumnStyleTable,
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
    /// Return the given font's bytes, shared with Parley's blob (no copy),
    /// caching by data pointer + index.
    ///
    /// Safety assumption: Parley font data pointers remain stable for the lifetime of
    /// this ConvertContext (scoped to a single `dom_to_drawables` call). HashMap is used
    /// (not BTreeMap) because this cache is lookup-only — iteration order does not
    /// affect PDF output.
    fn get_or_insert_font(&mut self, font: &parley::FontData) -> crate::paragraph::FontBytes {
        let key = (font.data.data().as_ptr() as usize, font.index);
        self.font_cache
            .entry(key)
            .or_insert_with(|| crate::paragraph::FontBytes::from_blob(&font.data))
            .clone()
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
        debug_print_tree(doc.deref(), crate::blitz_adapter::uid(root.id), 0);
    }
    convert_node(
        doc.deref(),
        crate::blitz_adapter::uid(root.id),
        ctx,
        0,
        &mut drawables,
    );
    drawables.bookmark_anchors = extract_bookmark_anchors(doc, &bookmark_snapshot, ctx.assets);
    drawables.body_offset_pt = extract_body_offset_pt(doc);
    drawables.root_dir_rtl = extract_root_dir_rtl(doc);
    drawables.root_id = Some(crate::blitz_adapter::uid(root.id));
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
            && (&elem.name.local) == "body"
        {
            return Some(crate::blitz_adapter::uid(child_id));
        }
    }
    None
}

/// Walk the DOM to find the first `<body>` and return its
/// `(location.x, location.y)` in pt. The fragmenter records body's own
/// fragment at `(body_x, 0)` (body-content-area relative); the html →
/// body offset that CSS margin collapsing puts onto `body.location` lives
/// here so `render_v2` can add it to per-fragment draw positions.
fn extract_body_offset_pt(doc: &HtmlDocument) -> (crate::units::Pt, crate::units::Pt) {
    use std::ops::Deref;
    let base = doc.deref();
    let root = doc.root_element();
    let Some(root_node) = base.get_node(root.id) else {
        return (crate::units::Pt::ZERO, crate::units::Pt::ZERO);
    };
    for &child_id in &root_node.children {
        let Some(child) = base.get_node(child_id) else {
            continue;
        };
        if let blitz_dom::NodeData::Element(elem) = &child.data
            && (&elem.name.local) == "body"
        {
            let (x, y, _, _) = layout_in_pt(child.final_layout_or_zero());
            return (x, y);
        }
    }
    (crate::units::Pt::ZERO, crate::units::Pt::ZERO)
}

/// Return `true` when the root `<html>` element has `direction: rtl`.
/// Used by `render_v2` to determine which page is the first `:left` page
/// (CSS Paged Media §5: RTL docs start on a `:left` page, LTR on `:right`).
fn extract_root_dir_rtl(doc: &HtmlDocument) -> bool {
    use ::style::properties::longhands::direction::computed_value::T as Dir;
    let base = doc.deref();
    base.get_node(doc.root_element().id)
        .and_then(|n| n.primary_styles())
        .is_some_and(|s| matches!(s.get_inherited_box().slow_clone_direction(), Dir::Rtl))
}

/// O(1) probe answering "did *any* per-NodeId map grow?" — sums `.len()`
/// across the same six maps `Drawables::draw_mark` / `drawn_since` track. Callers
/// that only need the boolean "produced anything?" answer (not the
/// descendant set) should compare this value before/after a recursion
/// instead of constructing two `BTreeSet<usize>`s. Convert never removes
/// entries from these maps, so the sum is monotonic and a strict
/// inequality is exactly equivalent to set-difference being non-empty.
/// (fulgur-vrkv)
pub(super) fn drawables_total_len(out: &crate::drawables::Drawables) -> usize {
    out.block_styles.len()
        + out.paragraphs.len()
        + out.images.len()
        + out.svgs.len()
        + out.tables.len()
        + out.list_items.len()
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
    let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id)) else {
        return;
    };
    let (x, y, width, height) = layout_in_pt(node.final_layout_or_zero());
    let indent = "  ".repeat(depth);
    let tag = match &node.data {
        NodeData::Element(e) => e.name.local.to_string(),
        NodeData::Text(_) => "#text".to_string(),
        NodeData::Comment { .. } => "#comment".to_string(),
        _ => "#other".to_string(),
    };
    // `Pt` has no `Display` by design (a length must not silently format as a
    // bare number), so this dev-only dump formats the typed values with `{:?}`
    // rather than `.to_f32()`. Bonus: since the branch is FULGUR_DEBUG-gated and
    // never runs under test, `{:?}` keeps the migrated diff to one format-string
    // line instead of four uncovered `.to_f32()` arg lines in the patch.
    eprintln!(
        "{indent}{tag} id={} pos=({:?},{:?}) size={:?}x{:?} inline_root={}",
        node_id,
        x,
        y,
        width,
        height,
        node.flags.is_inline_root()
    );
    for &child_id in &node.children {
        debug_print_tree(doc, crate::blitz_adapter::uid(child_id), depth + 1);
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
    // Only `record_transform` consumes `before`, and `record_transform`
    // is a no-op for any node without a CSS transform. Snapshotting
    // every drawables map for every walked node was the dominant
    // O(N²) cost in document-grade documents (each `convert_node` call
    // copying every NodeId already inserted, summing to ~N²/2 inserts
    // for N nodes). Gate the snapshot on the transform check so the
    // common case stays O(N). (fulgur-v1cm)
    let mark = node_has_transform(doc, node_id).then(|| out.draw_mark());
    convert_node_inner(doc, node_id, ctx, depth, out);
    record_multicol_rule(doc, node_id, ctx, out);
    convert_multicol_paragraph_slices(doc, node_id, ctx, out);
    convert_multicol_box_slices(node_id, ctx, out);
    if let Some(mark) = mark {
        record_transform(doc, node_id, mark, out);
    }
}

/// Cheap pre-check that mirrors `record_transform`'s own bail conditions:
/// returns true only when this node would actually need a snapshot to
/// compute its transform descendants. Computing the matrix here costs
/// the same as `record_transform` would (and is bypassed by the early
/// `return`s in nodes without a `<style transform>`), but it keeps the
/// branch-free invariant that `record_transform`'s snapshot consumer
/// only fires for nodes that produce a `TransformEntry`.
fn node_has_transform(doc: &BaseDocument, node_id: usize) -> bool {
    doc.get_node(crate::blitz_adapter::bid(node_id))
        .and_then(|node| {
            let styles = node.primary_styles()?;
            let (w, h) = size_in_pt(node.final_layout_or_zero().size);
            crate::blitz_adapter::compute_transform(&styles, w.to_f32(), h.to_f32())
        })
        .is_some()
}

/// Walk the DOM top-down from `<body>` and populate `out.semantics`
/// with one `SemanticEntry` per element that becomes a structure
/// element. Runs as a standalone pass after `convert_node`, so it can
/// ask what was actually drawn (`out.svgs`, `out.paragraphs`, the
/// opacity in `out.block_styles`), and so the classification covers
/// elements (e.g. `<thead>`, `<tbody>`) that the per-type converters
/// traverse via custom child walks instead of recursing through
/// `convert_node`.
///
/// `<head>` and its descendants are intentionally skipped — none of
/// them participate in the StructTree, and starting from `<body>`
/// keeps later expansions of `classify_element` (e.g. promoting
/// `<header>` / `<footer>` to dedicated tags) from accidentally
/// classifying `<head>`'s `<title>` / `<style>` etc.
fn record_semantics_pass(doc: &HtmlDocument, out: &mut crate::drawables::Drawables) {
    use std::ops::Deref;
    let base = doc.deref();
    let Some(body_id) = out.body_id else {
        return;
    };
    let mut left_out = std::collections::BTreeSet::new();
    walk_semantics(base, body_id, 0, None, false, &mut left_out, out);
    record_anonymous_blocks(base, &left_out, out);
    record_semantic_order(base, body_id, out);
    record_element_langs(base, crate::blitz_adapter::uid(doc.root_element().id), out);
}

/// Records the `/Lang` of each structure element whose language, its own
/// `lang` or an ancestor's, is not that of the structure element it is in:
/// for a top-level element, the document's `<html lang>`. Without it a
/// `<p lang="fr">` in an English document reads as English. The synthetic
/// `Lbl` and `LBody` of a list item have the item's language.
fn record_element_langs(doc: &BaseDocument, root_id: usize, out: &mut crate::drawables::Drawables) {
    let mut langs = crate::blitz_adapter::LangCache::new();
    let document_lang = crate::blitz_adapter::effective_lang(doc, root_id, &mut langs);
    let mut found = Vec::new();
    for (&node_id, entry) in &out.semantics {
        if doc.get_node(crate::blitz_adapter::bid(node_id)).is_none() {
            continue;
        }
        let Some(lang) = crate::blitz_adapter::effective_lang(doc, node_id, &mut langs) else {
            continue;
        };
        // The nearest structure element around it that is a node.
        let mut parent = entry.parent;
        let parent_lang = loop {
            let Some(parent_id) = parent else {
                break document_lang.clone();
            };
            if doc.get_node(crate::blitz_adapter::bid(parent_id)).is_some() {
                break crate::blitz_adapter::effective_lang(doc, parent_id, &mut langs);
            }
            parent = out.semantics.get(&parent_id).and_then(|e| e.parent);
        };
        if parent_lang.as_deref() != Some(&*lang) {
            found.push((node_id, lang));
        }
    }
    out.element_langs.extend(found);
}

/// Text in an anonymous block (a run of inline content next to block
/// siblings, as "Design" in `<td>Design<small style="display:block">…`)
/// has no element of its own, and neither has a block `::before` or
/// `::after` (Blitz makes both anonymous nodes). Each such block becomes
/// a `P` inside the nearest ancestor that may hold a paragraph (the
/// `TD`, `Div` or `LBody`). A block inside a subtree that is left out of
/// the structure tree (`left_out`: `aria-hidden`, a figure's content)
/// stays out with it.
fn record_anonymous_blocks(
    doc: &BaseDocument,
    left_out: &std::collections::BTreeSet<usize>,
    out: &mut crate::drawables::Drawables,
) {
    let anonymous: Vec<usize> = out
        .paragraphs
        .keys()
        .copied()
        .filter(|id| !out.semantics.contains_key(id))
        .filter(|&id| {
            doc.get_node(crate::blitz_adapter::bid(id))
                .is_some_and(|n| n.is_anonymous())
        })
        .collect();
    'blocks: for id in anonymous {
        let Some(node) = doc.get_node(crate::blitz_adapter::bid(id)) else {
            continue;
        };
        let mut owner = None;
        let mut parent = node.parent;
        while let Some(pid) = parent {
            let uid = crate::blitz_adapter::uid(pid);
            if left_out.contains(&uid) {
                continue 'blocks;
            }
            if let Some(entry) = out.semantics.get(&uid)
                && may_hold_paragraph(&entry.tag)
            {
                owner = Some(out.li_lbody_ids.get(&uid).copied().unwrap_or(uid));
                break;
            }
            parent = doc.get_node(pid).and_then(|n| n.parent);
        }
        out.semantics.insert(
            id,
            crate::tagging::SemanticEntry {
                tag: crate::tagging::PdfTag::P,
                parent: owner,
                alt_text: None,
            },
        );
    }
}

/// Whether a `P` may be a child of a structure element of this type. In
/// PDF a list holds only list items, and a table only its row groups,
/// rows and cells, so a paragraph found inside one (`<ul>Intro<li>…`)
/// goes to the element around it: the `LBody` of the item that holds a
/// nested list, else whatever holds the list or the table.
fn may_hold_paragraph(tag: &crate::tagging::PdfTag) -> bool {
    use crate::tagging::PdfTag;
    !matches!(
        tag,
        PdfTag::L { .. }
            | PdfTag::Table
            | PdfTag::THead
            | PdfTag::TBody
            | PdfTag::TFoot
            | PdfTag::Tr
    )
}

/// Give every structure element its place in reading order: its index in
/// one pre-order walk of the boxes under `<body>`. Node ids do not give
/// that order, because Blitz creates anonymous blocks and `::before` /
/// `::after` nodes after the DOM: in `<div class=tip>Save often.<p>…</p>`
/// with a `.tip::before`, the block around the pseudo-element and the
/// text has a higher id than the `<p>` it comes before.
///
/// The synthetic `Lbl` / `LBody` of a list item get no place: they are
/// only ever siblings of each other, and sort by id, which puts `Lbl`
/// first (it is allocated after `LBody`, from a counter that counts
/// down).
fn record_semantic_order(
    doc: &BaseDocument,
    body_id: usize,
    out: &mut crate::drawables::Drawables,
) {
    let mut order = HashMap::new();
    number_in_document_order(doc, body_id, 0, &mut order);
    let synthetic: std::collections::BTreeSet<usize> = out
        .li_lbl_ids
        .values()
        .chain(out.li_lbody_ids.values())
        .copied()
        .collect();
    // Every element the semantics pass sees is reached by the walk; a node
    // it somehow missed still needs a key, or it would sort by its id
    // among its siblings' indexes. It goes after them.
    let mut next = order.len();
    let mut semantic_order = std::collections::BTreeMap::new();
    for &id in out.semantics.keys() {
        if synthetic.contains(&id) {
            continue;
        }
        let key = order.get(&id).copied().unwrap_or_else(|| {
            next += 1;
            next - 1
        });
        semantic_order.insert(id, key);
    }
    out.semantic_order = semantic_order;
}

/// Number `node_id` and everything under it in the order it is laid out.
/// `layout_children` hold the anonymous blocks and the pseudo-elements
/// in their place, so they come first where Blitz builds them. An inline
/// root's `layout_children` list only its inline boxes, and a table's
/// only its cells, so these walk their DOM children instead, with the
/// pseudo-elements around them. Whatever one list leaves out follows from
/// the other.
fn number_in_document_order(
    doc: &BaseDocument,
    node_id: usize,
    depth: usize,
    order: &mut HashMap<usize, usize>,
) {
    if depth >= MAX_DOM_DEPTH || order.contains_key(&node_id) {
        return;
    }
    let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id)) else {
        return;
    };
    // The children of a `display: contents` element are laid out as
    // children of the element around it, so the walk meets them before
    // the element itself: number the elements it skipped first.
    let mut skipped = Vec::new();
    let mut parent = node.parent;
    while let Some(pid) = parent {
        let pid = crate::blitz_adapter::uid(pid);
        if order.contains_key(&pid) {
            break;
        }
        skipped.push(pid);
        parent = doc
            .get_node(crate::blitz_adapter::bid(pid))
            .and_then(|n| n.parent);
    }
    for id in skipped.into_iter().rev() {
        let next = order.len();
        order.insert(id, next);
    }
    let next = order.len();
    order.insert(node_id, next);

    let dom: Vec<usize> = node
        .before()
        .into_iter()
        .chain(node.children.iter().copied())
        .chain(node.after())
        .map(crate::blitz_adapter::uid)
        .collect();
    let layout: Option<Vec<usize>> = node
        .layout_children
        .borrow()
        .as_deref()
        .map(crate::blitz_adapter::uids);
    let children = match layout {
        Some(layout) if !node.flags.is_inline_root() && !node.flags.is_table_root() => {
            [layout, dom].concat()
        }
        Some(layout) => [dom, layout].concat(),
        None => dom,
    };
    for child in children {
        number_in_document_order(doc, child, depth + 1, order);
    }
}

/// The first token of an element's `role`, in lower case (ARIA uses the
/// first role it knows; these are the ones read here).
fn aria_role(elem: &blitz_dom::node::ElementData) -> Option<String> {
    get_attr(elem, "role")?
        .split_ascii_whitespace()
        .next()
        .map(str::to_ascii_lowercase)
}

/// The element's label: the text of what `aria-labelledby` names, else
/// `aria-label`, trimmed, when it says something.
fn aria_label(doc: &BaseDocument, elem: &blitz_dom::node::ElementData) -> Option<String> {
    crate::blitz_adapter::aria_labelledby_text(doc, elem).or_else(|| {
        get_attr(elem, "aria-label")
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
    })
}

/// The text alternative of an `<img>` (`alt`) or an inline `<svg>`
/// (`aria-labelledby` / `aria-label`, else its `<title>` child).
fn svg_or_img_alt(
    doc: &BaseDocument,
    node: &blitz_dom::Node,
    elem: &blitz_dom::node::ElementData,
) -> Option<String> {
    if &*elem.name.local != "svg" {
        return get_attr(elem, "alt").map(str::to_owned);
    }
    if let Some(label) = aria_label(doc, elem) {
        return Some(label);
    }
    node.children.iter().find_map(|&child| {
        let child = doc.get_node(child)?;
        let is_title = child
            .element_data()
            .is_some_and(|e| &*e.name.local == "title");
        let text = child.text_content();
        (is_title && !text.trim().is_empty()).then(|| text.trim().to_owned())
    })
}

/// Whether drawing `node_id` opens a krilla group (a fractional opacity
/// is drawn as a transparency group). Marked content opened inside a
/// group is not linked into the structure tree, so the render pass tags
/// everything in the group as the content of this element, and the
/// element needs a structure element for that. A paragraph root is a
/// `P` already, so `out.paragraphs` is not asked.
fn opens_group(out: &crate::drawables::Drawables, node_id: usize) -> bool {
    out.block_styles
        .get(&node_id)
        .is_some_and(|b| b.opacity < 1.0)
        || out
            .list_items
            .get(&node_id)
            .is_some_and(|l| l.opacity < 1.0)
        || out.tables.get(&node_id).is_some_and(|t| t.opacity < 1.0)
}

/// Parse a `colspan` / `rowspan` value as browsers do (the HTML rules for
/// parsing non-negative integers): leading whitespace, an optional `+`,
/// then digits up to the first non-digit. A missing, unparsable or zero
/// value spans one; larger values are capped at `max`. Blitz lays out
/// `rowspan="0"` as one row too, so it spans one here.
fn parse_span(value: Option<&str>, max: u32) -> u32 {
    let Some(value) = value else { return 1 };
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let value = value.strip_prefix('+').unwrap_or(value);
    let digits = value.bytes().take_while(u8::is_ascii_digit);
    let mut span: u32 = 0;
    let mut any = false;
    for d in digits {
        any = true;
        span = span.saturating_mul(10).saturating_add(u32::from(d - b'0'));
    }
    if !any || span == 0 { 1 } else { span.min(max) }
}

/// How an element appears in the structure tree.
enum SemanticRole {
    /// Neither the element nor anything inside it: `aria-hidden`,
    /// `display: none`, a decorative or invisible image, an `<svg>` that
    /// paints nothing.
    LeftOut,
    /// One `Figure` with this text alternative, drawn as one picture;
    /// nothing inside it gets a structure element of its own.
    Figure(Option<String>),
    /// A structure element with this tag, holding what is inside it.
    Element(crate::tagging::PdfTag),
    /// No structure element; what is inside it belongs to the element
    /// around it.
    Transparent,
}

/// Decide how the element `node_id` appears in the structure tree, from
/// its name, its ARIA attributes, its computed style and what the convert
/// pass drew for it.
fn semantic_role(
    doc: &BaseDocument,
    node: &blitz_dom::Node,
    node_id: usize,
    elem: &blitz_dom::node::ElementData,
    layout_table: bool,
    out: &crate::drawables::Drawables,
) -> SemanticRole {
    use crate::tagging::PdfTag;
    let name: &str = &elem.name.local;
    let hidden =
        get_attr(elem, "aria-hidden").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    let display_none = node
        .primary_styles()
        .is_some_and(|s| s.clone_display().is_none());
    if hidden || display_none {
        return SemanticRole::LeftOut;
    }
    let role = aria_role(elem);
    let presentational = matches!(role.as_deref(), Some("presentation" | "none"));
    let img_label = (role.as_deref() == Some("img"))
        .then(|| aria_label(doc, elem))
        .flatten();
    // `visibility: hidden` / `collapse` takes an image out of the
    // accessibility tree (CSS 2 §11.2 + WAI-ARIA; matched by Chromium and
    // Firefox), and nothing of it is drawn, so it gets no `Figure`, whose
    // alt text would describe a picture that is not on the page.
    // Web-aware authoring uses sr-only (position:absolute + clip) or
    // aria-hidden to keep alt available to assistive tech while hiding
    // the visual — not `visibility: hidden`.
    let (_opacity, visible) = extract_opacity_visible(node);
    match name {
        // An inline SVG is a figure, described by `aria-label` or its
        // `<title>`, when it paints something: a sprite sheet of
        // `<symbol>`s or an svg of `<defs>` draws nothing that needs a
        // description. Its content is the picture, never walked: a nested
        // `<svg>` is part of it.
        "svg" => {
            let painted = out.svgs.get(&node_id).is_some_and(|svg| {
                svg.visible
                    && svg.width.to_f32() > 0.0
                    && svg.height.to_f32() > 0.0
                    && svg.tree.root().has_children()
            });
            if painted && !presentational {
                SemanticRole::Figure(svg_or_img_alt(doc, node, elem))
            } else {
                SemanticRole::LeftOut
            }
        }
        // A decorative image (`alt=""`, `role="none"`) has no structure
        // element; it is drawn as an artifact.
        "img" if presentational || !visible || get_attr(elem, "alt") == Some("") => {
            SemanticRole::LeftOut
        }
        "img" => SemanticRole::Figure(svg_or_img_alt(doc, node, elem)),
        // `role="img"` makes a picture of any element (a QR code drawn as
        // a table of black cells): one figure, its content not read out.
        _ if img_label.is_some() => {
            if visible {
                SemanticRole::Figure(img_label)
            } else {
                SemanticRole::LeftOut
            }
        }
        _ => {
            let tag = match name {
                // A table with `role="presentation"` / `"none"` only lays
                // out its content: neither it nor its rows and cells are
                // table structure (a real table nested in a cell keeps
                // its own).
                "table" if presentational => None,
                "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" if layout_table => None,
                // A list whose items run inline (a breadcrumb trail,
                // `li { display: inline }`) is one line of text: the list
                // element holds that paragraph itself, and a list may only
                // hold items. It is read as a paragraph, and its inline
                // items are not list items.
                "ul" | "ol" if out.paragraphs.contains_key(&node_id) => Some(PdfTag::P),
                "li" if node
                    .primary_styles()
                    .is_some_and(|s| s.clone_display().is_inline_flow()) =>
                {
                    None
                }
                _ => crate::tagging::classify_element(name),
            }
            // An element with no semantic mapping that holds text itself
            // (`<blockquote>`, `<dd>`, `<address>`, a layout table's
            // cell, custom elements) becomes a paragraph, so its text is
            // tagged rather than left out of the structure tree.
            .or_else(|| out.paragraphs.contains_key(&node_id).then_some(PdfTag::P))
            // One drawn as a group needs an element to tag the group as.
            .or_else(|| opens_group(out, node_id).then_some(PdfTag::Div));
            match tag {
                // A heading that is not drawn (`visibility: hidden`) is not
                // read: it holds only what of it is visible, as a Div.
                Some(PdfTag::H { .. }) if !visible => SemanticRole::Element(PdfTag::Div),
                Some(tag) => SemanticRole::Element(tag),
                None => SemanticRole::Transparent,
            }
        }
    }
}

/// Record the structure element of `node_id`, if it has one, and walk
/// its DOM children. `layout_table` is true inside a `role="presentation"`
/// / `"none"` table, whose rows and cells are not table structure.
/// `left_out` collects the roots of the subtrees that get no structure
/// elements of their own, so `record_anonymous_blocks` leaves the
/// anonymous blocks inside them out too.
fn walk_semantics(
    doc: &BaseDocument,
    node_id: usize,
    depth: usize,
    parent_override: Option<usize>,
    layout_table: bool,
    left_out: &mut std::collections::BTreeSet<usize>,
    out: &mut crate::drawables::Drawables,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id)) else {
        return;
    };

    let mut child_layout_table = layout_table;
    let child_override = if let Some(elem) = node.element_data() {
        if &*elem.name.local == "table" {
            child_layout_table =
                matches!(aria_role(elem).as_deref(), Some("presentation" | "none"));
        }
        let (tag, alt_text) = match semantic_role(doc, node, node_id, elem, layout_table, out) {
            SemanticRole::LeftOut => {
                left_out.insert(node_id);
                return;
            }
            SemanticRole::Figure(alt) => (Some(crate::tagging::PdfTag::Figure), alt),
            SemanticRole::Element(tag) => (Some(tag), None),
            SemanticRole::Transparent => (None, None),
        };
        if let Some(mut tag) = tag {
            // CSS list-style-type を読んで ListNumbering をオーバーライド
            if matches!(tag, crate::tagging::PdfTag::L { .. }) {
                if let Some(styles) = node.primary_styles() {
                    use crate::tagging::ListNumbering;
                    use ::style::counter_style::CounterStyle as CS;
                    let lst = styles.slow_clone_list_style_type();
                    let numbering = match &lst.0 {
                        CS::Name(name) => match &*name.0 {
                            "disc" => ListNumbering::Disc,
                            "circle" => ListNumbering::Circle,
                            "square" => ListNumbering::Square,
                            "decimal" => ListNumbering::Decimal,
                            "lower-alpha" | "lower-latin" => ListNumbering::LowerAlpha,
                            "upper-alpha" | "upper-latin" => ListNumbering::UpperAlpha,
                            _ => ListNumbering::None,
                        },
                        _ => ListNumbering::None,
                    };
                    tag = crate::tagging::PdfTag::L { numbering };
                }
            }

            // parent を決定: override があればそれを使い、なければ DOM walk-up
            let parent_node_id = if let Some(ov) = parent_override {
                Some(ov)
            } else {
                let mut p = node.parent;
                loop {
                    let Some(pid) = p else { break None };
                    if out.semantics.contains_key(&crate::blitz_adapter::uid(pid)) {
                        break Some(pid);
                    }
                    p = doc.get_node(pid).and_then(|n| n.parent);
                }
                .map(crate::blitz_adapter::uid)
            };

            let is_li = matches!(tag, crate::tagging::PdfTag::Li);
            let is_figure = matches!(tag, crate::tagging::PdfTag::Figure);

            if matches!(tag, crate::tagging::PdfTag::Th { .. }) {
                let scope = get_attr(elem, "scope")
                    .and_then(|s| match s {
                        "row" => Some(crate::tagging::TableHeaderScope::Row),
                        "col" | "column" => Some(crate::tagging::TableHeaderScope::Column),
                        _ => None,
                    })
                    .unwrap_or(crate::tagging::TableHeaderScope::Both);
                tag = crate::tagging::PdfTag::Th { scope };
            }

            // A cell spanning several columns or rows says so in the
            // structure tree (`/ColSpan`, `/RowSpan`), or assistive
            // technology pairs the cells after it with the wrong headers.
            if matches!(
                tag,
                crate::tagging::PdfTag::Td | crate::tagging::PdfTag::Th { .. }
            ) {
                let colspan = parse_span(get_attr(elem, "colspan"), 1000);
                let rowspan = parse_span(get_attr(elem, "rowspan"), 65534);
                if colspan > 1 || rowspan > 1 {
                    out.table_cell_spans.insert(node_id, (colspan, rowspan));
                }
            }

            out.semantics.insert(
                node_id,
                crate::tagging::SemanticEntry {
                    tag,
                    parent: parent_node_id,
                    alt_text,
                },
            );

            if is_figure {
                // The picture is drawn as one; nothing inside it is read.
                left_out.insert(node_id);
                return;
            }

            if is_li {
                // Lbl / LBody 合成エントリを作成。
                // alloc_synthetic_id() は降順に ID を払い出すため、先に lbody_id を取ると
                // lbl_id < lbody_id となり、BTreeMap のキー昇順イテレーション
                // (build_struct_tree) で Lbl → LBody の正しい PDF/UA 順序が得られる。
                let lbody_id = out.alloc_synthetic_id();
                let lbl_id = out.alloc_synthetic_id();
                out.semantics.insert(
                    lbl_id,
                    crate::tagging::SemanticEntry {
                        tag: crate::tagging::PdfTag::Lbl,
                        parent: Some(node_id),
                        alt_text: None,
                    },
                );
                out.semantics.insert(
                    lbody_id,
                    crate::tagging::SemanticEntry {
                        tag: crate::tagging::PdfTag::LBody,
                        parent: Some(node_id),
                        alt_text: None,
                    },
                );
                out.li_lbl_ids.insert(node_id, lbl_id);
                out.li_lbody_ids.insert(node_id, lbody_id);
                // li の子は lbody_id を parent として再帰
                for &child_id in &node.children {
                    walk_semantics(
                        doc,
                        crate::blitz_adapter::uid(child_id),
                        depth + 1,
                        Some(lbody_id),
                        child_layout_table,
                        left_out,
                        out,
                    );
                }
                return; // 通常の再帰をスキップ
            }

            None // 分類済み要素 (Li 以外): 子への override はリセット
        } else {
            parent_override // 非分類要素: override を引き継ぐ
        }
    } else {
        parent_override // 要素でないノード: override を引き継ぐ
    };

    for &child_id in &node.children {
        walk_semantics(
            doc,
            crate::blitz_adapter::uid(child_id),
            depth + 1,
            child_override,
            child_layout_table,
            left_out,
            out,
        );
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
    // An `<a href>` laid out as a box of its own (block, inline-block, flex,
    // ...) is clickable over its whole box, as in browsers, not only over its
    // text.
    if let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id))
        && node
            .element_data()
            .is_some_and(|e| &*e.name.local == "a" && get_attr(e, "href").is_some())
        && node.primary_styles().is_some_and(|s| {
            use ::style::values::specified::box_::{DisplayInside, DisplayOutside};
            let display = s.clone_display();
            !display.is_none()
                && !(display.outside() == DisplayOutside::Inline
                    && display.inside() == DisplayInside::Flow)
        })
        && let Some(link) = ctx.link_cache.lookup(doc, node_id)
    {
        out.block_links.insert(node_id, link);
    }

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
/// resolves to a non-identity transform. `mark` was captured before this
/// node's walk; the `NodeId`s inserted since then (excluding `node_id`
/// itself) are the strict descendant list the render pass needs to paint
/// inside the transform's `push_transform` / `pop` group. See
/// [`crate::drawables::Drawables::drawn_since`].
fn record_transform(
    doc: &BaseDocument,
    node_id: usize,
    mark: crate::drawables::DrawMark,
    out: &mut crate::drawables::Drawables,
) {
    let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id)) else {
        return;
    };
    let Some(styles) = node.primary_styles() else {
        return;
    };
    // PR 8i note: `compute_transform` is documented to take CSS px (per
    // `.claude/rules/coordinate-system.md` and Stylo's `LengthPercentage`
    // contract), but is fed pt-valued box dims below (`width_pt`/`height_pt`),
    // technically violating that contract. This is intentional and remains
    // correct for percentage resolution (Length is unitless from Stylo's
    // perspective, so `%` round-trips identically against a pt basis or a
    // px basis — `transform-origin: 50%` is unaffected either way).
    //
    // Absolute-length components (`translate(Npx)`, `matrix()` tx/ty,
    // `transform-origin: Npx ...`) are a different story: `resolve()` ignores
    // this basis for them and returns the literal CSS px value, so
    // `compute_transform`/`op_to_matrix` fold px → pt for real
    // (`resolve_length_component`, fulgur-9vw5) before returning. The px →
    // origin → pt conversion this note used to defer to render as "a
    // separate cleanup tracked for a future PR" is therefore already done
    // by the time `render::draw_under_transform` consumes `tx.origin` and
    // the matrix — do not fold again there. Re-enabled by
    // `transform_integration::rotate_90_at_default_center_origin_fixes_center`.
    let (width_pt, height_pt) = size_in_pt(node.final_layout_or_zero().size);
    let Some((matrix, origin)) =
        crate::blitz_adapter::compute_transform(&styles, width_pt.to_f32(), height_pt.to_f32())
    else {
        return;
    };
    let descendants: Vec<usize> = out
        .drawn_since(mark)
        .into_iter()
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
    let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id)) else {
        return;
    };
    if !crate::blitz_adapter::is_multicol_container(node) {
        return;
    }
    let props = ctx.column_styles.get(&node_id);
    let Some(mut rule) = props.and_then(|props| props.rule).filter(|r| {
        r.style != crate::column_css::ColumnRuleStyle::None && r.width > crate::units::Pt::ZERO
    }) else {
        return;
    };
    if let Some(source) = props.and_then(|p| p.rule_color_source.as_deref()) {
        // A `var()` that does not resolve makes the value invalid at
        // computed-value time, which falls back to the initial value,
        // `currentcolor`.
        if let Some(rgba) = node.primary_styles().and_then(|styles| {
            resolve_css_color(source, &styles)
                .or_else(|| resolve_css_color("currentcolor", &styles))
        }) {
            rule.color = rgba;
        }
    }
    let Some(geometry) = ctx.multicol_geometry.get(&node_id) else {
        return;
    };
    // `ColumnGroupGeometry` is recorded in CSS px; convert to the pt-typed
    // ColumnRuleGeometry carrier so downstream paint matches every other
    // Drawables entry's units. Each conversion is one multiply (byte-neutral).
    let groups: Vec<crate::drawables::ColumnRuleGeometry> = geometry
        .groups
        .iter()
        .map(|g| crate::drawables::ColumnRuleGeometry {
            x_offset: g.x_offset.in_pt(),
            y_offset: g.y_offset.in_pt(),
            col_w: g.col_w.in_pt(),
            gap: g.gap.in_pt(),
            n: g.n,
            col_heights: g.col_heights.iter().copied().map(|h| h.in_pt()).collect(),
        })
        .collect();
    out.multicol_rules.insert(
        node_id,
        crate::drawables::MulticolRuleEntry { rule, groups },
    );
}

/// fulgur-6q5 Task 7: populate `Drawables.paragraph_slices` for a multicol
/// container from `MulticolGeometry::paragraph_splits`.
///
/// For every `ParagraphSplitEntry` recorded by `multicol_layout` against
/// this container, materialise one `ParagraphSlice` per non-empty column.
/// Each slice carries `Vec<ShapedLine>` rebased to the slice's own top
/// edge, using the same rebase convention as continuation fragments after
/// a page break (commit 9c0e092).
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
    let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id)) else {
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
        let group_x_pt = group.x_offset.in_pt();
        let group_y_pt = group.y_offset.in_pt();
        let col_w_pt = group.col_w.in_pt();

        for split in &group.paragraph_splits {
            let source_id = split.source_node_id;
            let case_a = source_id == node_id;

            // Pull the source `(layout, text)` pair from the source
            // node's `inline_layout_data`. For Case A we additionally
            // clone the layout and re-break at `col_w` so the line
            // indices recorded by `layout_self_inline_root_container`
            // resolve correctly. (Case B's layout was already re-broken
            // by Blitz during `compute_child_layout`.)
            let Some(source_node) = doc.get_node(crate::blitz_adapter::bid(source_id)) else {
                continue;
            };
            let Some(elem) = source_node.element_data() else {
                continue;
            };
            let Some(text_layout) = elem.inline_layout_data.as_ref() else {
                continue;
            };
            let text: std::sync::Arc<str> = std::sync::Arc::from(text_layout.text.as_str());

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
                cloned.set_alignment(alignment, parley::AlignmentOptions::default());
                cloned.break_all_lines(Some(group.col_w.to_f32()));
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
            let all_lines = shape_paragraph_glyph_runs(doc, source_node, layout_ref, &text, ctx);
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
                // Same rebase used for continuation fragments after a page
                // break (commit 9c0e092):
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
                let consumed: crate::units::Pt = all_lines[..col_slice.line_range.start]
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

                // `group_*_pt`/`col_w_pt` (Pt, hoisted above) plus the `Px`
                // `col_slice` geometry, summed in Pt space. Same fold as the
                // pre-migration f32 code — byte-neutral.
                let origin_pt = (
                    group_x_pt + col_slice.origin.x.in_pt(),
                    group_y_pt + col_slice.origin.y.in_pt(),
                );
                let size_pt = (col_w_pt, col_slice.size.height.in_pt());

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

/// Record the boxes `multicol_layout` broke across the columns of the
/// multicol container `node_id`, in container border-box pt, for
/// `render::paint_multicol_block_slices`.
fn convert_multicol_box_slices(
    node_id: usize,
    ctx: &ConvertContext<'_>,
    out: &mut crate::drawables::Drawables,
) {
    let Some(geometry) = ctx.multicol_geometry.get(&node_id) else {
        return;
    };
    for group in &geometry.groups {
        let group_x_pt = group.x_offset.in_pt();
        let group_y_pt = group.y_offset.in_pt();
        for split in &group.box_splits {
            let slices = split
                .fragments
                .iter()
                .map(|f| crate::drawables::BlockSlice {
                    origin_pt: (
                        group_x_pt + f.origin.x.in_pt(),
                        group_y_pt + f.origin.y.in_pt(),
                    ),
                    size_pt: (f.size.width.in_pt(), f.size.height.in_pt()),
                    offset_pt: f.offset.in_pt(),
                })
                .collect();
            out.block_slices.insert(
                split.source_node_id,
                crate::drawables::BlockSlicesEntry {
                    container_node_id: node_id,
                    height_pt: split.height.in_pt(),
                    slices,
                },
            );
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
    source_node: &Node,
    parley_layout: &parley::Layout<blitz_dom::node::TextBrush>,
    text: &std::sync::Arc<str>,
    ctx: &mut ConvertContext<'_>,
) -> Vec<ShapedLine> {
    let mut shaped_lines = Vec::new();
    let mut langs = crate::blitz_adapter::LangCache::new();
    let paragraph_lang = crate::blitz_adapter::effective_lang(
        doc,
        crate::blitz_adapter::uid(source_node.id),
        &mut langs,
    );
    let mut tab_lines = Vec::new();
    for line in parley_layout.lines() {
        let metrics = line.metrics();
        let mut items = Vec::new();
        let mut cursor = inline_root::RunCursor::default();
        let mut has_rtl = false;
        for item in line.items() {
            if let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                has_rtl |= glyph_run.run().is_rtl();
                inline_root::push_glyph_runs(
                    doc,
                    &glyph_run,
                    text,
                    paragraph_lang.as_deref(),
                    &mut langs,
                    &mut cursor,
                    ctx,
                    &mut items,
                );
            }
            // InlineBox items are intentionally not handled — see
            // `convert_multicol_paragraph_slices`'s scope note.
        }
        tab_lines.push(inline_root::tab_line_metrics(metrics, has_rtl));

        let line_height = metrics.line_height.as_px().in_pt();
        shaped_lines.push(ShapedLine {
            height: line_height,
            baseline: metrics.baseline.as_px().in_pt(),
            items,
        });
    }
    crate::paragraph::apply_tab_stops(
        &mut shaped_lines,
        &tab_lines,
        inline_root::tab_stops(source_node),
    );
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
        .is_some_and(|p| p.before() == Some(node.id) || p.after() == Some(node.id))
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
    origin_x: Px,
    origin_y: Px,
    width: Px,
    height: Px,
}

/// Compute the content-box of `node` from its computed style + Taffy layout.
/// Thin wrapper that projects `Node` + `BlockStyle` to primitives and
/// delegates the arithmetic to `content_box_from_geometry` (which is unit-
/// tested).
fn compute_content_box(node: &Node, style: &BlockStyle) -> ContentBox {
    let border_w = node.final_layout_or_zero().size.width.as_px();
    let border_h = node.final_layout_or_zero().size.height.as_px();
    content_box_from_geometry(border_w, border_h, &style.border_widths, &style.padding)
}

/// Pure arithmetic behind `compute_content_box`. Extracted so the
/// contract can be locked in by unit tests without fabricating a Blitz
/// `Node`.
///
/// All returned values are in **CSS px** (Blitz/Taffy layout space) — see
/// `.claude/rules/coordinate-system.md`.
///
/// Each inset side runs `.in_px()` on the border and padding operand
/// *separately* before summing (`(border[i].in_px()) + (padding[i].in_px())`
/// rather than `(border[i] + padding[i]).in_px()`). This is intentional:
/// it matches the float-op order that fulgur-m0xl's byte-neutral caller
/// stopgap performed, so any drift beyond the reordering documented in the
/// fulgur-bfu9 commit message stays out of the goldens. Do not fold the
/// two `.in_px()` calls per side into one.
fn content_box_from_geometry(
    border_w: Px,
    border_h: Px,
    border_widths: &[Pt; 4],
    padding: &[Pt; 4],
) -> ContentBox {
    // `border_widths` / `padding` are stored in CSS TRBL order:
    // [0]=top, [1]=right, [2]=bottom, [3]=left.
    let left_inset = border_widths[3].in_px() + padding[3].in_px();
    let top_inset = border_widths[0].in_px() + padding[0].in_px();
    let right_inset = border_widths[1].in_px() + padding[1].in_px();
    let bottom_inset = border_widths[2].in_px() + padding[2].in_px();
    ContentBox {
        origin_x: left_inset,
        origin_y: top_inset,
        width: (border_w - left_inset - right_inset).max(Px::ZERO),
        height: (border_h - top_inset - bottom_inset).max(Px::ZERO),
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

/// Resolve a CSS colour written for `styles`' element: `var()` references
/// are replaced by the element's computed custom properties (whose own
/// `var()`s Stylo has already substituted), then Stylo's colour parser
/// computes the value, `currentcolor` included. For properties Stylo does
/// not compute for Servo, such as `column-rule-color`.
pub(crate) fn resolve_css_color(
    source: &str,
    styles: &::style::properties::ComputedValues,
) -> Option<[u8; 4]> {
    use blitz_dom::CopyStyleGetters;

    let text = substitute_css_vars(source, |name| {
        styles
            .custom_properties()
            .get_for_cssom(&::style::Atom::from(name))
            .map(::style_traits::ToCss::to_css_string)
    })?;
    let computed = crate::column_css::parse_stylo_color(&text)?;
    let rgba = style::absolute_to_rgba(computed.resolve_to_absolute(&styles.clone_color()));
    Some(rgba)
}

/// Replace every `var(--name[, fallback])` in `css` with `lookup(name)`
/// (the name without its leading `--`), or with the fallback when the
/// lookup fails. Returns `None` when a reference has neither. Nesting
/// (`var(--a, var(--b))`) is handled by repeating the pass, a bounded
/// number of times.
fn substitute_css_vars(css: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    const MAX_PASSES: usize = 16;
    let mut text = css.to_string();
    for _ in 0..MAX_PASSES {
        let lower = text.to_ascii_lowercase();
        let Some(start) = lower.find("var(") else {
            return Some(text);
        };
        // The matching close parenthesis, and the first top-level comma.
        let mut depth = 0usize;
        let mut comma = None;
        let mut end = None;
        for (i, c) in text[start + 4..].char_indices() {
            let i = start + 4 + i;
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    end = Some(i);
                    break;
                }
                ')' => depth -= 1,
                ',' if depth == 0 && comma.is_none() => comma = Some(i),
                _ => {}
            }
        }
        let end = end?;
        let name_end = comma.unwrap_or(end);
        let name = text[start + 4..name_end].trim();
        let value = name
            .strip_prefix("--")
            .and_then(&lookup)
            .or_else(|| comma.map(|c| text[c + 1..end].trim().to_string()))?;
        text.replace_range(start..=end, &value);
    }
    None
}

/// Extract the asset name from a URL that Stylo may have resolved to absolute.
fn extract_asset_name(url: &str) -> &str {
    url.strip_prefix("file:///").unwrap_or(url)
}

/// Image bytes for an `<img src>`, `content`, `list-style-image` or
/// `background-image` reference: `data:` URLs are decoded in place (they are
/// part of the document, so no bundle entry is needed), anything else is
/// looked up in the asset bundle.
pub(crate) fn image_bytes(assets: Option<&AssetBundle>, src: &str) -> Option<Arc<Vec<u8>>> {
    if src
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
    {
        let url = data_url::DataUrl::process(src).ok()?;
        let (body, _fragment) = url.decode_to_vec().ok()?;
        return Some(Arc::new(body));
    }
    assets?.get_image(src).cloned()
}

/// Check if a node is a non-visual element (head, script, style, etc.)
fn is_non_visual_element(node: &Node) -> bool {
    if let Some(elem) = node.element_data() {
        let tag = &*elem.name.local;
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
    if let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id))
        && let Some(styles) = node.primary_styles()
    {
        return absolute_to_rgba(styles.clone_color());
    }
    [0, 0, 0, 255] // Default: black
}

/// The background colors of the inline elements a text run is in, outermost
/// first: the run's node and its ancestors while they are `display: inline`
/// (an inline root, an inline-block or a block ends the walk, since those
/// paint their own box). Transparent backgrounds are left out.
fn get_inline_backgrounds(doc: &BaseDocument, node_id: usize) -> Arc<[[u8; 4]]> {
    use ::style::values::specified::box_::{DisplayInside, DisplayOutside};
    let mut colors = Vec::new();
    let mut node = doc.get_node(crate::blitz_adapter::bid(node_id));
    let mut depth = 0;
    while let Some(current) = node {
        if depth >= crate::MAX_DOM_DEPTH {
            break;
        }
        depth += 1;
        // Text nodes carry no styles of their own: their element does.
        if current.is_element() {
            let Some(styles) = current.primary_styles() else {
                break;
            };
            let display = styles.clone_display();
            if display.outside() != DisplayOutside::Inline
                || display.inside() != DisplayInside::Flow
            {
                break;
            }
            let rgba = style::absolute_to_rgba(
                styles
                    .slow_clone_background_color()
                    .resolve_to_absolute(&styles.clone_color()),
            );
            if rgba[3] > 0 {
                colors.push(rgba);
            }
        }
        node = current.parent_node();
    }
    colors.reverse();
    colors.into()
}

/// The nearest inline element with `role="img"` around a text run (the run's
/// node or an ancestor, while they are `display: inline`), whose `Figure`
/// holds the run's text.
fn get_inline_figure(doc: &BaseDocument, node_id: usize) -> Option<usize> {
    use ::style::values::specified::box_::{DisplayInside, DisplayOutside};
    let mut node = doc.get_node(crate::blitz_adapter::bid(node_id));
    let mut depth = 0;
    while let Some(current) = node {
        if depth >= crate::MAX_DOM_DEPTH {
            break;
        }
        depth += 1;
        if let Some(elem) = current.element_data() {
            let display = current.primary_styles()?.clone_display();
            if display.outside() != DisplayOutside::Inline
                || display.inside() != DisplayInside::Flow
            {
                return None;
            }
            if aria_role(elem).as_deref() == Some("img") {
                return Some(crate::blitz_adapter::uid(current.id));
            }
        }
        node = current.parent_node();
    }
    None
}

/// Get text-decoration properties from a DOM node's computed styles.
fn get_text_decoration(doc: &BaseDocument, node_id: usize) -> TextDecoration {
    if let Some(node) = doc.get_node(crate::blitz_adapter::bid(node_id))
        && let Some(styles) = node.primary_styles()
    {
        let current_color = styles.clone_color();

        // text-decoration-line (bitflags)
        let stylo_line = styles.slow_clone_text_decoration_line();
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
        let style = match styles.slow_clone_text_decoration_style() {
            StyloTDS::Solid => TextDecorationStyle::Solid,
            StyloTDS::Dashed => TextDecorationStyle::Dashed,
            StyloTDS::Dotted => TextDecorationStyle::Dotted,
            StyloTDS::Double => TextDecorationStyle::Double,
            StyloTDS::Wavy => TextDecorationStyle::Wavy,
            _ => TextDecorationStyle::Solid,
        };

        // text-decoration-color (resolve currentcolor)
        let deco_color = styles.slow_clone_text_decoration_color();
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
        let pdf = engine.render(html).expect("render v2");
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
    use crate::units::F32Units;
    use std::ops::DerefMut;

    fn build_drawables(html: &str) -> crate::drawables::Drawables {
        build(html).1
    }

    /// The element with this `id` attribute.
    fn by_id(doc: &blitz_html::HtmlDocument, id: &str) -> usize {
        crate::blitz_adapter::uid(doc.get_element_by_id(id).unwrap_or_else(|| panic!("#{id}")))
    }

    fn is_anonymous(doc: &blitz_html::HtmlDocument, id: usize) -> bool {
        doc.get_node(crate::blitz_adapter::bid(id))
            .is_some_and(|n| n.is_anonymous())
    }

    fn build(html: &str) -> (blitz_html::HtmlDocument, crate::drawables::Drawables) {
        // Drive the convert pipeline directly without the full Engine
        // so the assertions stay focused on `dom_to_drawables`.
        // `parse_and_layout` already runs stylo + Taffy + the
        // `position: fixed` relayout, matching what the engine feeds
        // into convert at this point.
        let mut doc = crate::blitz_adapter::parse_and_layout(
            html,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            true,
        );

        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc, &[]);
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
        let d = super::dom_to_drawables(&doc, &mut ctx);
        (doc, d)
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
        let lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::Disc,
            },
        );
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
    fn dom_to_drawables_records_semantic_entries_for_ordered_lists() {
        let html = "<!DOCTYPE html><html><body><ol><li>item</li></ol></body></html>";
        let d = build_drawables(html);
        let lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::Decimal,
            },
        );
        assert_eq!(lists.len(), 1, "one ol in semantics");
    }

    #[test]
    fn dom_to_drawables_records_semantic_entries_for_tables() {
        let html = "<!DOCTYPE html><html><body><table><thead><tr><th>h</th></tr></thead><tbody><tr><td>d</td></tr></tbody></table></body></html>";
        let d = build_drawables(html);

        let tables = entries_by_tag(&d, &PdfTag::Table);
        assert_eq!(tables.len(), 1);
        let theads = entries_by_tag(&d, &PdfTag::THead);
        assert_eq!(theads.len(), 1, "one thead");
        let tbodies = entries_by_tag(&d, &PdfTag::TBody);
        assert_eq!(tbodies.len(), 1, "one tbody");
        // Collect thead + tbody together so the parent-check loop below works
        // the same way as before (both must parent to the table).
        let row_groups: Vec<_> = theads.iter().chain(tbodies.iter()).copied().collect();
        assert_eq!(row_groups.len(), 2, "thead + tbody");
        let rows = entries_by_tag(&d, &PdfTag::Tr);
        assert_eq!(rows.len(), 2);
        let ths = entries_by_tag(
            &d,
            &PdfTag::Th {
                scope: crate::tagging::TableHeaderScope::Both,
            },
        );
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
        // `<script>`, `<a>`, `<custom-tag>`, `<body>` and `<html>` get no
        // `SemanticEntry` of their own. Their inline text ("link", "y")
        // sits in an anonymous block beside the block `<p>`, and that
        // block becomes a paragraph so the text is tagged: two P entries,
        // the `<p>` and the anonymous block, and nothing else.
        let html = "<!DOCTYPE html><html><body><script>x=1</script><a href='#'>link</a><custom-tag>y</custom-tag><p>z</p></body></html>";
        let (doc, d) = build(html);

        let tags: Vec<&PdfTag> = d.semantics.values().map(|e| &e.tag).collect();
        assert_eq!(
            tags,
            vec![&PdfTag::P, &PdfTag::P],
            "expected the <p> and one anonymous paragraph, got {tags:?}"
        );
        // The anonymous block's id is allocated after the DOM, so only its
        // place in reading order puts it before the <p>.
        let anonymous = *d
            .semantics
            .keys()
            .find(|&&k| is_anonymous(&doc, k))
            .expect("an anonymous paragraph");
        let p = *d.semantics.keys().find(|&&k| k != anonymous).unwrap();
        assert!(anonymous > p, "Blitz numbers anonymous blocks last");
        assert!(
            d.semantic_order[&anonymous] < d.semantic_order[&p],
            "the anonymous paragraph comes before the <p> in reading order"
        );
    }

    #[test]
    fn walk_semantics_li_creates_lbl_and_lbody_synthetic_entries() {
        let html = "<!DOCTYPE html><html><body><ul><li>item</li></ul></body></html>";
        let d = build_drawables(html);

        // li_lbl_ids と li_lbody_ids に 1 エントリずつあること
        assert_eq!(d.li_lbl_ids.len(), 1, "one li_lbl_ids entry");
        assert_eq!(d.li_lbody_ids.len(), 1, "one li_lbody_ids entry");

        // li_lbl_ids のキー = li NodeId、値 = Lbl 合成 NodeId
        let (&li_id, &lbl_id) = d.li_lbl_ids.iter().next().unwrap();
        let lbody_id = d.li_lbody_ids[&li_id];

        // semantics に Lbl エントリがあり、parent = li_id
        let lbl_entry = &d.semantics[&lbl_id];
        assert_eq!(lbl_entry.tag, PdfTag::Lbl);
        assert_eq!(lbl_entry.parent, Some(li_id));

        // semantics に LBody エントリがあり、parent = li_id
        let lbody_entry = &d.semantics[&lbody_id];
        assert_eq!(lbody_entry.tag, PdfTag::LBody);
        assert_eq!(lbody_entry.parent, Some(li_id));
    }

    #[test]
    fn walk_semantics_li_child_span_parent_is_lbody() {
        // li 直下の <span> は lbody_id を親に持つ
        let html = "<!DOCTYPE html><html><body><ul><li><span>text</span></li></ul></body></html>";
        let d = build_drawables(html);
        let (&li_id, _) = d.li_lbl_ids.iter().next().unwrap();
        let lbody_id = d.li_lbody_ids[&li_id];

        let span_entries: Vec<_> = d
            .semantics
            .iter()
            .filter(|(_, e)| e.tag == PdfTag::Span)
            .collect();
        assert!(
            !span_entries.is_empty(),
            "span inside li should be in semantics"
        );
        for (_, entry) in &span_entries {
            assert_eq!(
                entry.parent,
                Some(lbody_id),
                "span inside li should have lbody_id as parent, not li_id"
            );
        }
    }

    #[test]
    fn walk_semantics_nested_list_structure() {
        let html = "<!DOCTYPE html><html><body>\
            <ul><li><ol><li>nested</li></ol></li></ul>\
            </body></html>";
        let d = build_drawables(html);

        // 2 つの li があるので li_lbl_ids に 2 エントリ
        assert_eq!(d.li_lbl_ids.len(), 2);

        // inner ol の L エントリが存在すること
        let decimal_lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::Decimal,
            },
        );
        assert_eq!(decimal_lists.len(), 1, "one ol (Decimal)");
        let (_, inner_ol_parent) = decimal_lists[0];

        // inner ol の parent は outer li の lbody_id であること
        let outer_li_ids: Vec<_> = d
            .li_lbl_ids
            .keys()
            .copied()
            .filter(|&id| d.li_lbody_ids.get(&id).copied() == inner_ol_parent)
            .collect();
        assert_eq!(
            outer_li_ids.len(),
            1,
            "inner ol parent should be outer li's lbody"
        );
    }

    #[test]
    fn dom_to_drawables_records_alt_text_on_figure() {
        let figure_alt = |html: &str| {
            let d = build_drawables(html);
            let figures: Vec<_> = d
                .semantics
                .values()
                .filter(|e| e.tag == PdfTag::Figure)
                .collect();
            assert_eq!(figures.len(), 1);
            figures[0].alt_text.clone()
        };

        assert_eq!(
            figure_alt(
                "<!DOCTYPE html><html><body><img src='a.png' alt='photo of cat'></body></html>"
            )
            .as_deref(),
            Some("photo of cat"),
            "alt text should be captured"
        );
        // alt="" marks a decorative image: no Figure, drawn as an artifact.
        let d =
            build_drawables("<!DOCTYPE html><html><body><img src='a.png' alt=''></body></html>");
        assert!(
            d.semantics.values().all(|e| e.tag != PdfTag::Figure),
            "an image with an empty alt is decorative"
        );
        assert_eq!(
            figure_alt("<!DOCTYPE html><html><body><img src='a.png'></body></html>"),
            None,
            "missing alt should be None"
        );
    }

    #[test]
    fn dom_to_drawables_drops_invisible_figure() {
        // CSS 2 §11.2 + WAI-ARIA: `visibility: hidden` / `collapse`
        // excludes the subtree from the accessibility tree (Chromium /
        // Firefox agreement). An invisible <img> draws nothing, so it gets
        // no Figure at all: neither its `alt` text (same regression class
        // as the heading `/T` gate, heading_title_of in render.rs) nor an
        // empty Figure that PDF/UA would refuse for having no alt text.
        for visibility in ["hidden", "collapse"] {
            let d = build_drawables(&format!(
                "<!DOCTYPE html><html><body>\
                 <img src='a.png' alt='hidden secret' style='visibility:{visibility}'>\
                 <svg aria-label='hidden chart' width='20' height='20' style='visibility:{visibility}'>\
                 <rect width='20' height='20'/></svg>\
                 </body></html>"
            ));
            assert!(
                d.semantics.values().all(|e| e.tag != PdfTag::Figure),
                "visibility:{visibility} images get no Figure: {:?}",
                d.semantics
            );
        }
    }

    // ── walk_semantics: what is left out, what is one figure ─────────────

    #[test]
    fn walk_semantics_svg_is_a_figure_only_when_painted() {
        // A sprite sheet hidden with display:none, an svg of <defs>, an
        // svg of unused <symbol>s and the svg nested in a figure paint
        // nothing of their own; only the outer labelled svg is a Figure,
        // and nothing inside any svg is walked.
        let (doc, d) = build(
            "<!DOCTYPE html><html><body>\
             <svg id='sprite' style='display:none'><symbol id='i'><rect width='9' height='9'/></symbol></svg>\
             <svg id='defs' width='0' height='0'><defs><linearGradient id='g'/></defs></svg>\
             <svg id='symbols' width='20' height='20'><symbol id='s'><rect width='9' height='9'/></symbol></svg>\
             <svg id='chart' aria-label='Chart' width='40' height='40'>\
             <title>Ignored</title><rect width='40' height='40'/>\
             <svg id='inner' width='20' height='20'><rect width='20' height='20'/></svg>\
             </svg>\
             </body></html>",
        );
        let chart = by_id(&doc, "chart");
        // The body holds only inline content here, so it is a paragraph.
        assert_eq!(
            d.semantics
                .keys()
                .copied()
                .filter(|&id| Some(id) != d.body_id)
                .collect::<Vec<_>>(),
            vec![chart],
            "only the painted outer svg has an entry: {:?}",
            d.semantics
        );
        assert_eq!(d.semantics[&chart].tag, PdfTag::Figure);
        assert_eq!(d.semantics[&chart].alt_text.as_deref(), Some("Chart"));
    }

    #[test]
    fn walk_semantics_aria_hidden_leaves_the_whole_subtree_out() {
        // Every element under aria-hidden, the text in an anonymous block
        // ("Loose") among them, stays out of the structure tree.
        let (doc, d) = build(
            "<!DOCTYPE html><html><body>\
             <div aria-hidden='true'><p>Decor</p>\
             <table><tr><td>Cell</td></tr></table>\
             Loose<div>Block</div>\
             <img src='a.png' alt='Logo'></div>\
             <p id='shown'>Shown</p>\
             </body></html>",
        );
        assert_eq!(
            d.semantics.keys().copied().collect::<Vec<_>>(),
            vec![by_id(&doc, "shown")],
            "{:?}",
            d.semantics
        );
    }

    #[test]
    fn walk_semantics_role_img_with_a_label_is_one_figure() {
        // A QR code drawn as a table of cells: one Figure with the label,
        // nothing for the table or the text inside it.
        let (doc, d) = build(
            "<!DOCTYPE html><html><body>\
             <div id='qr' role='img' aria-label='  QR code '>\
             <table><tr><td>a</td><td>b</td></tr></table>Loose<p>Text</p></div>\
             <div id='plain' role='img'><p id='inner'>Unlabelled</p></div>\
             </body></html>",
        );
        let qr = by_id(&doc, "qr");
        assert_eq!(d.semantics[&qr].tag, PdfTag::Figure);
        assert_eq!(d.semantics[&qr].alt_text.as_deref(), Some("QR code"));
        assert!(
            d.semantics
                .values()
                .all(|e| e.parent != Some(qr) && !matches!(e.tag, PdfTag::Table | PdfTag::Td)),
            "nothing inside the figure: {:?}",
            d.semantics
        );
        // Without a label there is nothing to describe the picture with,
        // so the element is read as usual.
        let plain = by_id(&doc, "plain");
        assert_eq!(d.semantics[&plain].tag, PdfTag::Div);
        assert_eq!(d.semantics[&by_id(&doc, "inner")].parent, Some(plain));
    }

    #[test]
    fn walk_semantics_layout_table_has_no_table_structure() {
        for role in ["presentation", "none"] {
            let (doc, d) = build(&format!(
                "<!DOCTYPE html><html><body>\
                 <table role='{role}'><thead><tr><th id='head'>Head</th></tr></thead>\
                 <tbody><tr><td id='left'>Left<table id='real'><tr><td id='inner'>In</td></tr></table></td>\
                 <td id='right' colspan='2'>Right</td></tr></tbody></table>\
                 </body></html>"
            ));
            let tags: Vec<&PdfTag> = d.semantics.values().map(|e| &e.tag).collect();
            // Only the nested real table keeps table structure.
            let real = by_id(&doc, "real");
            assert_eq!(
                entries_by_tag(&d, &PdfTag::Table),
                vec![(real, None)],
                "{tags:?}"
            );
            assert_eq!(entries_by_tag(&d, &PdfTag::Tr).len(), 1, "{tags:?}");
            assert_eq!(entries_by_tag(&d, &PdfTag::TBody).len(), 1, "{tags:?}");
            assert_eq!(
                entries_by_tag(&d, &PdfTag::Td),
                vec![(
                    by_id(&doc, "inner"),
                    d.semantics[&by_id(&doc, "inner")].parent
                )],
                "{tags:?}"
            );
            assert!(entries_by_tag(&d, &PdfTag::THead).is_empty(), "{tags:?}");
            assert!(
                !d.semantics
                    .values()
                    .any(|e| matches!(e.tag, PdfTag::Th { .. })),
                "{tags:?}"
            );
            // The cells' text is paragraphs, read at the table's level.
            for id in ["head", "right"] {
                let entry = &d.semantics[&by_id(&doc, id)];
                assert_eq!(entry.tag, PdfTag::P, "#{id}");
                assert_eq!(entry.parent, None, "#{id}");
            }
            assert!(!d.semantics.contains_key(&by_id(&doc, "left")));
            assert!(
                d.table_cell_spans.is_empty(),
                "a layout cell has no span to describe"
            );
        }
    }

    #[test]
    fn walk_semantics_records_cell_spans() {
        let (doc, d) = build(
            "<!DOCTYPE html><html><body><table><tr>\
             <td id='a' colspan='2'>a</td>\
             <td id='b' rowspan='3'>b</td>\
             <td id='c' colspan='0'>c</td>\
             <td id='d' colspan=' +5x'>d</td>\
             <td id='e' colspan='2000' rowspan='70000'>e</td>\
             <th id='f' colspan='-1' rowspan='zero'>f</th>\
             <th id='g' colspan='1' rowspan='2'>g</th>\
             </tr></table></body></html>",
        );
        let spans: Vec<(usize, (u32, u32))> = ["a", "b", "d", "e", "g"]
            .iter()
            .map(|id| by_id(&doc, id))
            .zip([(2, 1), (1, 3), (5, 1), (1000, 65534), (1, 2)])
            .collect();
        assert_eq!(
            d.table_cell_spans
                .iter()
                .map(|(&k, &v)| (k, v))
                .collect::<Vec<_>>(),
            spans
        );
    }

    #[test]
    fn parse_span_follows_the_html_integer_rules() {
        assert_eq!(super::parse_span(None, 1000), 1);
        assert_eq!(super::parse_span(Some(""), 1000), 1);
        assert_eq!(super::parse_span(Some("abc"), 1000), 1);
        assert_eq!(super::parse_span(Some("0"), 1000), 1);
        assert_eq!(super::parse_span(Some("-3"), 1000), 1);
        assert_eq!(super::parse_span(Some("3"), 1000), 3);
        assert_eq!(super::parse_span(Some("\t\n 4"), 1000), 4);
        assert_eq!(super::parse_span(Some("+6"), 1000), 6);
        assert_eq!(super::parse_span(Some("7.9"), 1000), 7);
        assert_eq!(super::parse_span(Some("8px"), 1000), 8);
        assert_eq!(super::parse_span(Some("1001"), 1000), 1000);
        assert_eq!(
            super::parse_span(Some("99999999999999999999"), 65534),
            65534
        );
    }

    #[test]
    fn walk_semantics_element_drawn_as_a_group_gets_a_div() {
        // Each of these draws its content inside an opacity group, which
        // the render pass tags as the content of the element.
        let (doc, d) = build(
            "<!DOCTYPE html><html><body>\
             <blockquote id='bq' style='opacity:.5'><p id='quoted'>x</p></blockquote>\
             <figure id='fig' style='opacity:.8'><p>y</p></figure>\
             <form id='form' style='opacity:.5'><p>z</p></form>\
             <a id='link' href='#t' style='display:block;opacity:.9'><div>w</div></a>\
             <dl><dt id='item' style='display:list-item;opacity:.5'><p>v</p></dt></dl>\
             <p>Text <em id='ib' style='display:inline-block;opacity:.5'><span style='display:block'>u</span></em></p>\
             <table id='layout' role='presentation' style='opacity:.5'><tr><td>t</td></tr></table>\
             <dl><dd id='dd' style='opacity:.5'>Text</dd></dl>\
             <blockquote id='plain'><p>s</p></blockquote>\
             </body></html>",
        );
        for id in ["bq", "fig", "form", "link", "item", "ib", "layout"] {
            assert_eq!(d.semantics[&by_id(&doc, id)].tag, PdfTag::Div, "#{id}");
        }
        assert_eq!(
            d.semantics[&by_id(&doc, "quoted")].parent,
            Some(by_id(&doc, "bq"))
        );
        // A paragraph root is a P already, and an element drawn without a
        // group gets nothing.
        assert_eq!(d.semantics[&by_id(&doc, "dd")].tag, PdfTag::P);
        assert!(!d.semantics.contains_key(&by_id(&doc, "plain")));
    }

    // ── record_semantic_order ───────────────────────────────────────────

    #[test]
    fn semantic_order_puts_pseudo_elements_where_they_are_drawn() {
        // The block around `::before` and "Save often." is created after
        // the DOM, and so is the block `::after`.
        let (doc, d) = build(
            "<!DOCTYPE html><html><head><style>\
             .tip::before { content: 'Tip: ' }\
             .tip::after { content: 'End'; display: block }\
             </style></head><body>\
             <div class='tip' id='tip'>Save often.<p id='details'>Details.</p></div>\
             </body></html>",
        );
        let tip = by_id(&doc, "tip");
        let details = by_id(&doc, "details");
        let mut children: Vec<usize> = d
            .semantics
            .iter()
            .filter(|(_, e)| e.parent == Some(tip))
            .map(|(&id, _)| id)
            .collect();
        assert_eq!(children.len(), 3, "{:?}", d.semantics);
        children.sort_by_key(|id| d.semantic_order[id]);
        assert!(
            is_anonymous(&doc, children[0]),
            "the ::before block reads first"
        );
        assert_eq!(children[1], details);
        assert!(
            is_anonymous(&doc, children[2]),
            "the ::after block reads last"
        );
        assert!(
            children[0] > details,
            "Blitz numbers the block after the DOM"
        );
    }

    #[test]
    fn semantic_order_covers_every_element_and_skips_synthetic_ids() {
        let (doc, d) = build(
            "<!DOCTYPE html><html><body>\
             <div id='outer'><section id='contents' style='display:contents'><p id='a'>A</p></section>\
             <p id='b'>B</p></div>\
             <ul><li>One<span>two</span></li></ul>\
             <table><tr><td>c</td></tr></table>\
             </body></html>",
        );
        let synthetic: Vec<usize> = d
            .li_lbl_ids
            .values()
            .chain(d.li_lbody_ids.values())
            .copied()
            .collect();
        for id in d.semantics.keys() {
            assert_eq!(
                d.semantic_order.contains_key(id),
                !synthetic.contains(id),
                "node {id}: {:?}",
                d.semantics[id]
            );
        }
        // Lbl sorts before LBody by id, the fallback the render pass uses
        // for ids without a place.
        for (li, lbl) in &d.li_lbl_ids {
            assert!(*lbl < d.li_lbody_ids[li]);
        }
        // A display:contents element comes before what it contains.
        let key = |id: &str| d.semantic_order[&by_id(&doc, id)];
        assert!(key("outer") < key("contents"));
        assert!(key("contents") < key("a"));
        assert!(key("a") < key("b"));
    }

    // ── record_anonymous_blocks: where the paragraph goes ───────────────

    #[test]
    fn anonymous_paragraph_is_not_put_in_a_list() {
        let (doc, d) = build(
            "<!DOCTYPE html><html><body>\
             <div id='wrap'><ul>Intro<li>One</li></ul></div>\
             <ul><li id='outer'>Item<ul>Stray<li>Nested</li></ul></li></ul>\
             </body></html>",
        );
        let parents: Vec<Option<usize>> = d
            .semantics
            .iter()
            .filter(|&(&id, _)| is_anonymous(&doc, id))
            .map(|(_, e)| e.parent)
            .collect();
        let wrap = by_id(&doc, "wrap");
        let outer_body = d.li_lbody_ids[&by_id(&doc, "outer")];
        assert!(parents.contains(&Some(wrap)), "{parents:?}");
        assert!(parents.contains(&Some(outer_body)), "{parents:?}");
        for parent in parents.into_iter().flatten() {
            assert!(
                super::may_hold_paragraph(&d.semantics[&parent].tag),
                "a P under {:?}",
                d.semantics[&parent].tag
            );
        }
    }

    // ── walk_semantics: <th scope> attribute handling ─────────────────────

    #[test]
    fn walk_semantics_th_scope_row_overrides_default() {
        // <th scope="row"> must produce TableHeaderScope::Row, not the
        // default Both that classify_element initialises.
        let html = "<!DOCTYPE html><html><body>\
            <table><tr>\
                <th scope=\"row\">Row header</th>\
                <td>data</td>\
            </tr></table>\
            </body></html>";
        let d = build_drawables(html);
        let row_ths = entries_by_tag(
            &d,
            &PdfTag::Th {
                scope: crate::tagging::TableHeaderScope::Row,
            },
        );
        assert_eq!(row_ths.len(), 1, "one th with scope=row");
        let both_ths = entries_by_tag(
            &d,
            &PdfTag::Th {
                scope: crate::tagging::TableHeaderScope::Both,
            },
        );
        assert_eq!(
            both_ths.len(),
            0,
            "no default-scope th when scope=row is set"
        );
    }

    #[test]
    fn walk_semantics_th_scope_col_overrides_default() {
        // <th scope="col"> must produce TableHeaderScope::Column.
        let html = "<!DOCTYPE html><html><body>\
            <table><tr>\
                <th scope=\"col\">Col header 1</th>\
                <th scope=\"column\">Col header 2</th>\
            </tr></table>\
            </body></html>";
        let d = build_drawables(html);
        let col_ths = entries_by_tag(
            &d,
            &PdfTag::Th {
                scope: crate::tagging::TableHeaderScope::Column,
            },
        );
        assert_eq!(col_ths.len(), 2, "two th with scope=col/column");
    }

    #[test]
    fn walk_semantics_th_unrecognised_scope_falls_back_to_both() {
        // An unrecognised scope value (e.g. "rowgroup") produces the Both
        // fallback set by the `unwrap_or` in walk_semantics.
        let html = "<!DOCTYPE html><html><body>\
            <table><tr>\
                <th scope=\"rowgroup\">Group header</th>\
            </tr></table>\
            </body></html>";
        let d = build_drawables(html);
        let both_ths = entries_by_tag(
            &d,
            &PdfTag::Th {
                scope: crate::tagging::TableHeaderScope::Both,
            },
        );
        assert_eq!(both_ths.len(), 1, "unrecognised scope falls back to Both");
    }

    // ── walk_semantics: list-style-type CSS override ──────────────────────

    #[test]
    fn walk_semantics_list_style_type_circle_maps_to_circle() {
        // <ul style="list-style-type:circle"> must override the default Disc
        // numbering that classify_element("ul") sets.
        let html = "<!DOCTYPE html><html><body>\
            <ul style=\"list-style-type:circle\"><li>item</li></ul>\
            </body></html>";
        let d = build_drawables(html);
        let circle_lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::Circle,
            },
        );
        assert_eq!(circle_lists.len(), 1, "one ul with Circle numbering");
        let disc_lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::Disc,
            },
        );
        assert_eq!(disc_lists.len(), 0, "no Disc list when circle is set");
    }

    #[test]
    fn walk_semantics_list_style_type_square_maps_to_square() {
        let html = "<!DOCTYPE html><html><body>\
            <ul style=\"list-style-type:square\"><li>item</li></ul>\
            </body></html>";
        let d = build_drawables(html);
        let square_lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::Square,
            },
        );
        assert_eq!(square_lists.len(), 1, "one ul with Square numbering");
    }

    #[test]
    fn walk_semantics_list_style_type_lower_alpha_maps_to_lower_alpha() {
        let html = "<!DOCTYPE html><html><body>\
            <ol style=\"list-style-type:lower-alpha\"><li>a</li></ol>\
            </body></html>";
        let d = build_drawables(html);
        let alpha_lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::LowerAlpha,
            },
        );
        assert_eq!(alpha_lists.len(), 1, "one ol with LowerAlpha numbering");
    }

    #[test]
    fn walk_semantics_list_style_type_upper_alpha_maps_to_upper_alpha() {
        let html = "<!DOCTYPE html><html><body>\
            <ol style=\"list-style-type:upper-alpha\"><li>A</li></ol>\
            </body></html>";
        let d = build_drawables(html);
        let alpha_lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::UpperAlpha,
            },
        );
        assert_eq!(alpha_lists.len(), 1, "one ol with UpperAlpha numbering");
    }

    #[test]
    fn walk_semantics_list_style_type_unhandled_maps_to_none() {
        // lower-greek is a CSS keyword Stylo/servo recognises (it is in servo's
        // single_keyword list) but is NOT in our explicit match arms, so it hits
        // `_ => ListNumbering::None`. lower-roman is NOT in servo's list, so
        // the inline style is rejected and the UA-stylesheet default takes over.
        let html = "<!DOCTYPE html><html><body>\
            <ol style=\"list-style-type:lower-greek\"><li>α</li></ol>\
            </body></html>";
        let d = build_drawables(html);
        let none_lists = entries_by_tag(
            &d,
            &PdfTag::L {
                numbering: crate::tagging::ListNumbering::None,
            },
        );
        assert_eq!(
            none_lists.len(),
            1,
            "lower-greek maps to ListNumbering::None"
        );
    }
}

#[cfg(test)]
mod debug_print_tree_tests {
    //! Tests that directly exercise `debug_print_tree`.
    //! Calling the function directly with a parsed `BaseDocument` avoids
    //! process-global env-var mutation and is safe under the parallel test
    //! harness (no `set_var` / `remove_var`, no mutex needed).

    use crate::units::F32Units;
    use std::ops::Deref;

    fn parsed_doc(html: &str) -> blitz_html::HtmlDocument {
        crate::blitz_adapter::parse_and_layout(
            html,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            true,
        )
    }

    #[test]
    fn debug_print_tree_traverses_dom_without_panic() {
        // Covers the main body of debug_print_tree: all NodeData arms
        // (Element, Text, Comment) and the child-recursion loop.
        let doc = parsed_doc(
            "<!DOCTYPE html><html><body><p>text</p><!-- comment --><div><span>nested</span></div></body></html>",
        );
        let root_id = doc.root_element().id;
        super::debug_print_tree(doc.deref(), crate::blitz_adapter::uid(root_id), 0);
    }

    #[test]
    fn debug_print_tree_max_depth_guard_returns_early() {
        // Pass depth = MAX_DOM_DEPTH directly so the first line of
        // debug_print_tree hits the early-return guard without building a
        // pathologically deep DOM or running a large-stack thread.
        let doc = parsed_doc("<!DOCTYPE html><html><body><p>x</p></body></html>");
        let root_id = doc.root_element().id;
        super::debug_print_tree(
            doc.deref(),
            crate::blitz_adapter::uid(root_id),
            crate::MAX_DOM_DEPTH,
        );
    }
}

#[cfg(test)]
mod utility_fn_tests {
    //! Unit tests for pure utility functions in this module.
    //! These exercise branches that the integration tests (semantics_tests,
    //! bookmark_outline_tests) do not reach because they rely on unusual CSS
    //! keyword variants or specific URL schemes.

    use super::*;

    // --- css_text_align_to_parley_alignment ---

    #[test]
    fn text_align_all_variants() {
        use ::style::values::specified::TextAlignKeyword;
        let cases: &[(TextAlignKeyword, parley::Alignment)] = &[
            (TextAlignKeyword::Start, parley::Alignment::Start),
            (TextAlignKeyword::Left, parley::Alignment::Left),
            (TextAlignKeyword::Right, parley::Alignment::Right),
            (TextAlignKeyword::Center, parley::Alignment::Center),
            (TextAlignKeyword::Justify, parley::Alignment::Justify),
            (TextAlignKeyword::End, parley::Alignment::End),
            // Moz* aliases map to their logical equivalents.
            // These keywords can't be produced from normal author CSS in Stylo
            // (they originate from internal browser UA/quirks paths), so the
            // only way to test them is through direct enum construction.
            (TextAlignKeyword::MozCenter, parley::Alignment::Center),
            (TextAlignKeyword::MozLeft, parley::Alignment::Left),
            (TextAlignKeyword::MozRight, parley::Alignment::Right),
        ];
        for &(keyword, expected) in cases {
            assert_eq!(
                css_text_align_to_parley_alignment(keyword),
                expected,
                "keyword {keyword:?} should map to {expected:?}"
            );
        }
    }

    // --- extract_asset_name ---

    #[test]
    fn extract_asset_name_strips_file_scheme() {
        assert_eq!(extract_asset_name("file:///foo/bar.png"), "foo/bar.png");
        assert_eq!(extract_asset_name("file:///"), "");
    }

    #[test]
    fn image_bytes_decodes_data_urls_without_a_bundle() {
        assert_eq!(
            image_bytes(None, "data:image/png;base64,iVBORw0K").as_deref(),
            Some(&b"\x89PNG\r\n"[..].to_vec())
        );
        assert_eq!(
            image_bytes(None, "DATA:text/plain,a%20b").as_deref(),
            Some(&b"a b".to_vec())
        );
        assert!(image_bytes(None, "data:image/png;base64,***").is_none());
        assert!(image_bytes(None, "logo.png").is_none());
    }

    #[test]
    fn image_bytes_looks_up_other_references_in_the_bundle() {
        let mut bundle = AssetBundle::new();
        bundle.add_image("logo.png", vec![1, 2, 3]);
        assert_eq!(
            image_bytes(Some(&bundle), "./logo.png").as_deref(),
            Some(&vec![1, 2, 3])
        );
        assert!(image_bytes(Some(&bundle), "other.png").is_none());
    }

    #[test]
    fn extract_asset_name_passthrough_non_file_url() {
        assert_eq!(extract_asset_name("logo.png"), "logo.png");
        assert_eq!(
            extract_asset_name("http://example.com/img.png"),
            "http://example.com/img.png"
        );
    }

    // --- layout_in_pt ---

    #[test]
    fn layout_in_pt_converts_px_pt() {
        // 1 CSS px = 0.75 PDF pt, so 4 px → 3 pt, 100 px → 75 pt.
        let layout = taffy::Layout {
            location: taffy::geometry::Point { x: 4.0, y: 8.0 },
            size: taffy::geometry::Size {
                width: 100.0,
                height: 200.0,
            },
            ..taffy::Layout::new()
        };
        let (x, y, w, h) = layout_in_pt(&layout);
        assert_eq!(x, 3.0_f32.as_pt());
        assert_eq!(y, 6.0_f32.as_pt());
        assert_eq!(w, 75.0_f32.as_pt());
        assert_eq!(h, 150.0_f32.as_pt());
    }

    #[test]
    fn layout_in_pt_zero_layout_stays_zero() {
        let layout = taffy::Layout::new();
        let (x, y, w, h) = layout_in_pt(&layout);
        assert_eq!(x, 0.0_f32.as_pt());
        assert_eq!(y, 0.0_f32.as_pt());
        assert_eq!(w, 0.0_f32.as_pt());
        assert_eq!(h, 0.0_f32.as_pt());
    }

    // --- size_in_pt ---

    #[test]
    fn size_in_pt_converts_px_pt() {
        // 80 px → 60 pt, 120 px → 90 pt
        let size = taffy::geometry::Size {
            width: 80.0,
            height: 120.0,
        };
        let (w, h) = size_in_pt(size);
        assert_eq!(w, 60.0_f32.as_pt());
        assert_eq!(h, 90.0_f32.as_pt());
    }

    #[test]
    fn size_in_pt_zero_size_stays_zero() {
        let size = taffy::geometry::Size {
            width: 0.0,
            height: 0.0,
        };
        let (w, h) = size_in_pt(size);
        assert_eq!(w, 0.0_f32.as_pt());
        assert_eq!(h, 0.0_f32.as_pt());
    }

    // --- content_box_from_geometry ---
    //
    // The `Node` / `BlockStyle` projection lives in `compute_content_box`;
    // these tests exercise the pure arithmetic. TRBL order: [0]=top,
    // [1]=right, [2]=bottom, [3]=left.

    fn tf(v: f32) -> crate::units::Pt {
        v.as_pt()
    }

    #[test]
    fn content_box_from_geometry_zero_insets_matches_border_box() {
        let cb = content_box_from_geometry(
            80.0_f32.as_px(),
            120.0_f32.as_px(),
            &[tf(0.0); 4],
            &[tf(0.0); 4],
        );
        assert_eq!(cb.origin_x, 0.0_f32.as_px());
        assert_eq!(cb.origin_y, 0.0_f32.as_px());
        assert_eq!(cb.width, 80.0_f32.as_px());
        assert_eq!(cb.height, 120.0_f32.as_px());
    }

    #[test]
    fn content_box_from_geometry_respects_trbl_index_mapping() {
        // Distinct border and padding per side so any transposition of
        // TRBL indices produces a different origin / width / height.
        // border TRBL = [1, 2, 3, 4] pt, padding TRBL = [5, 6, 7, 8] pt.
        let cb = content_box_from_geometry(
            80.0_f32.as_px(),
            120.0_f32.as_px(),
            &[tf(1.0), tf(2.0), tf(3.0), tf(4.0)],
            &[tf(5.0), tf(6.0), tf(7.0), tf(8.0)],
        );
        // origin_x = left  = border[3] + padding[3] = (4 + 8) pt → Px.
        assert_eq!(cb.origin_x, tf(4.0).in_px() + tf(8.0).in_px());
        // origin_y = top   = border[0] + padding[0] = (1 + 5) pt → Px.
        assert_eq!(cb.origin_y, tf(1.0).in_px() + tf(5.0).in_px());
        // width  = border_w - left - right  = 80px - (4+8)pt.in_px() - (2+6)pt.in_px()
        let left = tf(4.0).in_px() + tf(8.0).in_px();
        let right = tf(2.0).in_px() + tf(6.0).in_px();
        assert_eq!(cb.width, 80.0_f32.as_px() - left - right);
        // height = border_h - top - bottom = 120px - (1+5)pt.in_px() - (3+7)pt.in_px()
        let top = tf(1.0).in_px() + tf(5.0).in_px();
        let bottom = tf(3.0).in_px() + tf(7.0).in_px();
        assert_eq!(cb.height, 120.0_f32.as_px() - top - bottom);
    }

    #[test]
    fn content_box_from_geometry_folds_per_side_bit_identically() {
        // Guards the docstring's "do not fold" rule: summing then
        // converting must be bit-identical to converting-then-summing at
        // the values we actually feed. `Pt` addition uses raw f32 ops so
        // the equivalence is exact for these small integers, but the
        // test locks in the observable equality so a future
        // "cleanup" that changed the order would either have to update
        // the assertion or fail.
        let border = [tf(1.0), tf(2.0), tf(3.0), tf(4.0)];
        let padding = [tf(5.0), tf(6.0), tf(7.0), tf(8.0)];
        let cb = content_box_from_geometry(80.0_f32.as_px(), 120.0_f32.as_px(), &border, &padding);
        // Recompute each side out-of-line matching the callee's exact
        // per-operand `.in_px()` shape.
        let l = border[3].in_px() + padding[3].in_px();
        let t = border[0].in_px() + padding[0].in_px();
        let r = border[1].in_px() + padding[1].in_px();
        let b = border[2].in_px() + padding[2].in_px();
        assert_eq!(cb.origin_x, l);
        assert_eq!(cb.origin_y, t);
        assert_eq!(
            cb.width,
            (80.0_f32.as_px() - l - r).max(crate::units::Px::ZERO)
        );
        assert_eq!(
            cb.height,
            (120.0_f32.as_px() - t - b).max(crate::units::Px::ZERO),
        );
    }

    #[test]
    fn content_box_from_geometry_clamps_to_zero_when_insets_exceed_box() {
        // border_w = 10 px, insets = 100 pt/side border + 100 pt/side padding on left+right
        // → negative content width, must clamp to 0.
        let cb = content_box_from_geometry(
            10.0_f32.as_px(),
            10.0_f32.as_px(),
            &[tf(100.0); 4],
            &[tf(100.0); 4],
        );
        assert_eq!(cb.width, crate::units::Px::ZERO);
        assert_eq!(cb.height, crate::units::Px::ZERO);
    }

    // --- LinkCache::lookup cache hit paths ---
    //
    // Two paths through the cache-hit branch of `lookup` (line 1063) are not
    // reached by integration tests:
    //   line 1064: `let anchor_id = (*cached)?;` — None short-circuit
    //   line 1065: `return self.by_anchor.get(&anchor_id).cloned();` — Some hit
    // Covered here by calling `lookup` twice for the same node id.

    fn find_first_by_tag_in_tests(doc: &BaseDocument, start_id: usize, tag: &str) -> Option<usize> {
        let node = doc.get_node(crate::blitz_adapter::bid(start_id))?;
        if node.element_data().is_some_and(|e| (&e.name.local) == tag) {
            return Some(start_id);
        }
        for &c in &node.children {
            if let Some(found) = find_first_by_tag_in_tests(doc, crate::blitz_adapter::uid(c), tag)
            {
                return Some(found);
            }
        }
        None
    }

    #[test]
    fn link_cache_second_lookup_hits_cached_none() {
        // First call for a node not inside any <a> inserts None into by_start.
        // Second call finds the cached None and short-circuits at line 1064.
        let doc = crate::blitz_adapter::parse_and_layout(
            "<!DOCTYPE html><html><body><p>no anchor here</p></body></html>",
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let base: &BaseDocument = doc.deref();
        let root_id = doc.root_element().id;

        let mut cache = LinkCache::default();
        let first = cache.lookup(base, crate::blitz_adapter::uid(root_id));
        // Second call: cache hit → `(*cached)?` at line 1064 returns None.
        let second = cache.lookup(base, crate::blitz_adapter::uid(root_id));

        assert!(
            first.is_none(),
            "root element should not be inside an anchor"
        );
        assert!(
            second.is_none(),
            "cached None hit must also return None (line 1064)"
        );
    }

    #[test]
    fn link_cache_second_lookup_returns_cached_arc() {
        // First call for a node inside <a href="…"> resolves and caches the anchor.
        // Second call returns the Arc from by_anchor at line 1065.
        let doc = crate::blitz_adapter::parse_and_layout(
            r#"<!DOCTYPE html><html><body><a href="https://example.com"><span>link</span></a></body></html>"#,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        let base: &BaseDocument = doc.deref();
        let root_id = doc.root_element().id;
        let span_id = find_first_by_tag_in_tests(base, crate::blitz_adapter::uid(root_id), "span")
            .expect("<span> should exist inside <a>");

        let mut cache = LinkCache::default();
        let first = cache.lookup(base, span_id);
        // Second call: cache hit → returns Arc from by_anchor at line 1065.
        let second = cache.lookup(base, span_id);

        assert!(
            first.is_some(),
            "span inside <a href> should resolve to Some link"
        );
        assert!(
            second.is_some(),
            "cached anchor hit must also return Some link (line 1065)"
        );
        assert!(
            Arc::ptr_eq(first.as_ref().unwrap(), second.as_ref().unwrap()),
            "both calls must return the same Arc (memoization invariant)"
        );
    }

    // --- get_text_color fallback ---
    //
    // Production callers always pass valid, styled node ids, so the fallback
    // path at mod.rs:1110 (when `doc.get_node` returns None) is only reachable
    // via an out-of-range id. Tested here directly on the private function.

    #[test]
    fn get_text_color_falls_back_to_black_for_missing_node() {
        let doc = crate::blitz_adapter::parse_and_layout(
            "<!DOCTYPE html><html><body><p>text</p></body></html>",
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            false,
        );
        use std::ops::Deref;
        let base: &BaseDocument = doc.deref();
        assert_eq!(
            get_text_color(base, usize::MAX),
            [0, 0, 0, 255],
            "get_text_color must return opaque black for an out-of-range node id"
        );
    }
}

#[cfg(test)]
mod edge_case_tests {
    //! Tests for defensive guards and edge cases in convert/mod.rs:
    //! MAX_DOM_DEPTH truncation and empty/whitespace id="" attributes.
    //! These exercise branches that normal documents never reach but that
    //! protect against adversarial or malformed HTML.

    use std::ops::DerefMut;

    use crate::tagging::PdfTag;
    use crate::units::F32Units;

    fn build_drawables(html: &str) -> crate::drawables::Drawables {
        let mut doc = crate::blitz_adapter::parse_and_layout(
            html,
            595.0_f32.as_px(),
            842.0_f32.as_px(),
            &[],
            true,
        );
        let column_styles = crate::blitz_adapter::extract_column_style_table(&doc, &[]);
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

    // --- border colors per side ---

    #[test]
    fn a_lone_bottom_border_keeps_its_color() {
        // The other sides keep the initial `currentcolor` (red here), so the
        // sides differ and each keeps its own color.
        let d = build_drawables(
            "<!DOCTYPE html><body style='color:#f00'>\
             <div style='height:10px;border-bottom:2px solid #3346a8'></div></body>",
        );
        let colors: Vec<_> = d
            .block_styles
            .iter()
            .filter(|(_, e)| e.style.border_widths[2].to_f32() > 0.0)
            .map(|(_, e)| e.style.side_colors)
            .collect();
        assert_eq!(
            colors,
            vec![Some([
                [255, 0, 0, 255],
                [255, 0, 0, 255],
                [0x33, 0x46, 0xa8, 255],
                [255, 0, 0, 255]
            ])]
        );
        // One color for all sides needs no per-side drawing.
        let d = build_drawables(
            "<!DOCTYPE html><body><div style='height:10px;border:2px solid #3346a8'></div></body>",
        );
        let block = d
            .block_styles
            .iter()
            .find(|(_, e)| e.style.border_widths[0].to_f32() > 0.0)
            .expect("bordered block");
        assert_eq!(block.1.style.side_colors, None);
        assert_eq!(block.1.style.border_color, [0x33, 0x46, 0xa8, 255]);
    }

    // --- MAX_DOM_DEPTH guard in convert_node and walk_semantics ---

    #[test]
    fn deeply_nested_html_does_not_panic_and_truncates_at_max_depth() {
        // MAX_DOM_DEPTH = 512. Build HTML with 560 levels of nesting so
        // both convert_node (line 333) and walk_semantics (line 401) hit
        // their depth guards. The render must complete without panicking,
        // shallow entries must be present, and elements past the depth cap
        // must NOT appear in drawables (verifying the guard actually fires).
        //
        // Sentinel placement: div at index 510.
        //   Depth from <html>: html=0, body=1, div[0]=2, …, div[510]=512.
        //   convert_node's guard fires at depth >= MAX_DOM_DEPTH (512), so
        //   div[510] is the *first* element NOT processed by convert_node.
        //   Removing convert_node's guard makes div[510] appear in
        //   block_styles, failing the assertion below.
        //
        //   Note: positioned::walk_children_into_drawables has its own
        //   independent guard at the same threshold (positioned.rs:25-27).
        //   That guard fires on the *parent's* depth, so when convert_node's
        //   guard is absent, div[510] is still processed (walk_children was
        //   called with depth=511) but div[511]+ are blocked by walk_children.
        //   The sentinel at div[510] therefore targets convert_node's guard
        //   specifically, not walk_children's backup guard.
        //
        // Run in a thread with a large stack because the Blitz/Taffy parse
        // + layout pipelines recurse through the DOM tree before fulgur's
        // depth guard triggers.
        let mut html = String::from("<!DOCTYPE html><html><body>");
        for i in 0..560 {
            if i == 510 {
                html.push_str("<div id=\"beyond-depth-limit\">");
            } else {
                html.push_str("<div>");
            }
        }
        html.push_str("deep text");
        for _ in 0..560 {
            html.push_str("</div>");
        }
        html.push_str("</body></html>");

        let d = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024) // 64 MiB: blitz/taffy recurse before fulgur's guard fires
            .spawn(move || build_drawables(&html))
            .expect("thread spawn")
            .join()
            .expect("thread did not panic");

        // Shallow entries (before the depth cap) must be recorded.
        assert!(
            !d.block_styles.is_empty(),
            "shallow block entries must be recorded even with deep nesting"
        );
        // The element beyond MAX_DOM_DEPTH must not appear in any drawable map,
        // confirming the depth guard in convert_node / walk_semantics fires.
        let deep_in_blocks = d
            .block_styles
            .values()
            .any(|e| e.id.as_ref().map(|s| s.as_str()) == Some("beyond-depth-limit"));
        let deep_in_paras = d
            .paragraphs
            .values()
            .any(|e| e.id.as_ref().map(|s| s.as_str()) == Some("beyond-depth-limit"));
        assert!(
            !deep_in_blocks && !deep_in_paras,
            "element at depth > MAX_DOM_DEPTH must be absent from block_styles and paragraphs"
        );
        // Verify that walk_semantics also respects the depth cap.
        // walk_semantics starts from <body> at depth 0, so it processes
        // exactly MAX_DOM_DEPTH - 1 = 511 nested <div> elements before the
        // guard fires (div[0] at depth 1 … div[510] at depth 511; div[511]
        // at depth 512 >= MAX_DOM_DEPTH is skipped). Use strict `<` to
        // detect an off-by-one regression where the guard is relaxed from
        // `>= MAX_DOM_DEPTH` to `> MAX_DOM_DEPTH` — that would let div[511]
        // through, producing MAX_DOM_DEPTH entries and silently passing a
        // `<=` comparison. (Note: convert_node starts from <html> at depth 0,
        // so it truncates 2 levels earlier — the off-by-2 is intentional and
        // NOT a bug in walk_semantics.)
        let div_sem_count = d
            .semantics
            .values()
            .filter(|e| e.tag == PdfTag::Div)
            .count();
        assert!(
            div_sem_count < crate::MAX_DOM_DEPTH,
            "walk_semantics depth guard failed: {} Div entries in semantics, expected < {}",
            div_sem_count,
            crate::MAX_DOM_DEPTH
        );
    }

    // --- extract_block_id: empty id="" attribute ---

    #[test]
    fn empty_id_attribute_is_not_stored_in_block_entry() {
        // id="" trims to empty; extract_block_id must return None rather
        // than storing an empty Arc<String>. Use trim().is_empty() in the
        // filter so a regression that stores the untrimmed value is caught.
        // Use a border to force block entry creation so we can check both
        // block and paragraph id fields.
        let html = "<!DOCTYPE html><html><body>\
            <div id=\"\" style=\"border:1px solid black\">content with empty id</div>\
            <div id=\"valid-id\" style=\"border:1px solid red\">content with valid id</div>\
            </body></html>";

        let d = build_drawables(html);

        // No entry in block_styles or paragraphs should carry an empty/blank id.
        let empty_block_ids: Vec<_> = d
            .block_styles
            .values()
            .filter(|e| e.id.as_deref().map(|s| s.trim().is_empty()) == Some(true))
            .collect();
        assert!(
            empty_block_ids.is_empty(),
            "extract_block_id must not store empty id strings in block entries; found {}",
            empty_block_ids.len()
        );
        let empty_para_ids: Vec<_> = d
            .paragraphs
            .values()
            .filter(|e| e.id.as_deref().map(|s| s.trim().is_empty()) == Some(true))
            .collect();
        assert!(
            empty_para_ids.is_empty(),
            "extract_block_id must not store empty id strings in paragraph entries; found {}",
            empty_para_ids.len()
        );

        // The valid id must appear in at least one block or paragraph entry.
        let valid_block = d
            .block_styles
            .values()
            .any(|e| e.id.as_ref().map(|s| s.as_str()) == Some("valid-id"));
        let valid_para = d
            .paragraphs
            .values()
            .any(|e| e.id.as_ref().map(|s| s.as_str()) == Some("valid-id"));
        assert!(
            valid_block || valid_para,
            "valid-id should be stored in at least one block or paragraph entry"
        );
    }

    #[test]
    fn whitespace_only_id_attribute_is_not_stored() {
        // id="   " (spaces only) also trims to empty and must return None.
        // Use trim().is_empty() so a regression storing the untrimmed "   "
        // is detected. A <p> is an inline root, so its id lands in d.paragraphs.
        let html = "<!DOCTYPE html><html><body><p id=\"   \">paragraph</p></body></html>";

        let d = build_drawables(html);

        let bad_para_ids: Vec<_> = d
            .paragraphs
            .values()
            .filter(|e| e.id.as_deref().map(|s| s.trim().is_empty()) == Some(true))
            .collect();
        assert!(
            bad_para_ids.is_empty(),
            "whitespace-only id must not be stored in paragraph entries; found {}",
            bad_para_ids.len()
        );
        let bad_block_ids: Vec<_> = d
            .block_styles
            .values()
            .filter(|e| e.id.as_deref().map(|s| s.trim().is_empty()) == Some(true))
            .collect();
        assert!(
            bad_block_ids.is_empty(),
            "whitespace-only id must not be stored in block entries; found {}",
            bad_block_ids.len()
        );
    }
}

#[cfg(test)]
mod css_color_tests {
    use super::{resolve_css_color, substitute_css_vars};

    fn lookup(name: &str) -> Option<String> {
        match name {
            "a" => Some("red".into()),
            "b" => Some("var(--a)".into()),
            _ => None,
        }
    }

    #[test]
    fn substitutes_vars_fallbacks_and_nesting() {
        assert_eq!(
            substitute_css_vars("var(--a)", lookup).as_deref(),
            Some("red")
        );
        assert_eq!(
            substitute_css_vars("var(--b)", lookup).as_deref(),
            Some("red")
        );
        assert_eq!(
            substitute_css_vars("var(--x, var(--a))", lookup).as_deref(),
            Some("red")
        );
        assert_eq!(
            substitute_css_vars("color-mix(in srgb, var(--a) 50%, var(--x, blue))", lookup)
                .as_deref(),
            Some("color-mix(in srgb, red 50%, blue)")
        );
        assert_eq!(substitute_css_vars("var(--x)", lookup), None);
        assert_eq!(substitute_css_vars("var(--a", lookup), None);
    }

    fn styles_of_body(html: &str) -> ::style::servo_arc::Arc<::style::properties::ComputedValues> {
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let body = (0..10_000)
            .filter_map(|id| doc.get_node(crate::blitz_adapter::bid(id)))
            .find(|n| n.element_data().is_some_and(|e| &*e.name.local == "body"))
            .expect("body");
        body.primary_styles().expect("styles").clone()
    }

    #[test]
    fn resolves_custom_properties_modern_colours_and_currentcolor() {
        let styles = styles_of_body(
            r#"<!doctype html><html><head><style>
            :root { --ink: rgb(0 0 255); --line: color-mix(in srgb, var(--ink) 50%, white); }
            body { color: rgb(0 128 0); }
            </style></head><body>x</body></html>"#,
        );
        assert_eq!(
            resolve_css_color("var(--ink)", &styles),
            Some([0, 0, 255, 255])
        );
        let mixed = resolve_css_color("var(--line)", &styles).expect("color-mix");
        assert!(mixed[0].abs_diff(128) <= 1 && mixed[2] == 255, "{mixed:?}");
        assert_eq!(
            resolve_css_color("currentcolor", &styles),
            Some([0, 128, 0, 255])
        );
        let oklch = resolve_css_color("oklch(62.8% 0.2577 29.23)", &styles).expect("oklch");
        assert!(
            oklch[0] >= 250 && oklch[1] <= 5 && oklch[2] <= 5,
            "{oklch:?}"
        );
        assert_eq!(resolve_css_color("var(--missing)", &styles), None);
        assert_eq!(resolve_css_color("not-a-colour", &styles), None);
    }
}
