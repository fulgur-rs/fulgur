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

/// Build the body pageable for a list-item node.
///
/// Both the primary list-item path (where Blitz populates `list_item_data`)
/// and the fallback path (image-only markers with `list-style-type: none`)
/// share this logic. It handles inline pseudo images, paragraph extraction,
/// `needs_block_wrapper` + `layout_size`, synthesised paragraphs for
/// pseudo-only items, and non-inline-root block child collection.
#[allow(clippy::too_many_arguments)]
fn build_list_item_body(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    style: BlockStyle,
    visible: bool,
    width: f32,
    height: f32,
    content_box: ContentBox,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    if node.flags.is_inline_root() {
        let paragraph_opt = extract_paragraph(doc, node, ctx, depth);

        // Inline pseudo images for list item body
        let before_inline = node
            .before
            .and_then(|id| doc.get_node(id))
            .filter(|p| !pseudo::is_block_pseudo(p))
            .and_then(|p| {
                pseudo::build_inline_pseudo_image(
                    p,
                    content_box.width,
                    content_box.height,
                    ctx.assets,
                )
            })
            .map(|mut img| {
                pseudo::attach_link_to_inline_image(&mut img, doc, node.id);
                img
            });
        let after_inline = node
            .after
            .and_then(|id| doc.get_node(id))
            .filter(|p| !pseudo::is_block_pseudo(p))
            .and_then(|p| {
                pseudo::build_inline_pseudo_image(
                    p,
                    content_box.width,
                    content_box.height,
                    ctx.assets,
                )
            })
            .map(|mut img| {
                pseudo::attach_link_to_inline_image(&mut img, doc, node.id);
                img
            });

        if let Some(mut paragraph) = paragraph_opt {
            if before_inline.is_some() || after_inline.is_some() {
                pseudo::inject_inline_pseudo_images(
                    &mut paragraph.lines,
                    before_inline,
                    after_inline,
                );
                recalculate_paragraph_line_boxes(&mut paragraph.lines);
                paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
            }

            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_pseudo_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            let pagination = extract_pagination_from_column_css(ctx, node);
            let needs_wrapper = style.needs_block_wrapper()
                || has_pseudo
                || pagination != crate::pageable::Pagination::default();
            if needs_wrapper {
                let (child_x, child_y) = style.content_inset();
                let mut p = paragraph;
                p.visible = visible;
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(p),
                    x: child_x,
                    y: child_y,
                }];
                let mut children = pseudo::wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    paragraph_children,
                );
                children.extend(abs_pseudos);
                let mut block = BlockPageable::with_positioned_children(children)
                    .with_pagination(pagination)
                    .with_style(style)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                Box::new(block)
            } else {
                let mut p = paragraph;
                p.visible = visible;
                Box::new(p)
            }
        } else if before_inline.is_some() || after_inline.is_some() {
            // Synthesize a minimal paragraph for pseudo-only list items
            let mut line = ShapedLine {
                height: 0.0,
                baseline: 0.0,
                items: vec![],
            };
            pseudo::inject_inline_pseudo_images(
                std::slice::from_mut(&mut line),
                before_inline,
                after_inline,
            );
            let font_metrics = metrics_from_line(&line);
            crate::paragraph::recalculate_line_box(&mut line, &font_metrics);
            let mut paragraph = ParagraphPageable::new(vec![line]);
            paragraph.visible = visible;

            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_pseudo_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            let pagination = extract_pagination_from_column_css(ctx, node);
            if style.needs_block_wrapper()
                || has_pseudo
                || pagination != crate::pageable::Pagination::default()
            {
                let (child_x, child_y) = style.content_inset();
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(paragraph),
                    x: child_x,
                    y: child_y,
                }];
                let mut children = pseudo::wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    paragraph_children,
                );
                children.extend(abs_pseudos);
                let mut block = BlockPageable::with_positioned_children(children)
                    .with_pagination(pagination)
                    .with_style(style)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                Box::new(block)
            } else {
                Box::new(paragraph)
            }
        } else {
            // Inline root with no text and no inline pseudo images —
            // fall through to the non-inline-root path below.
            let layout_children_guard_1 = node.layout_children.borrow();
            let children: &[usize] = layout_children_guard_1.as_deref().unwrap_or(&node.children);
            let positioned_children =
                positioned::collect_positioned_children(doc, children, ctx, depth);
            let (positioned_children, _has_pseudo) = pseudo::wrap_with_pseudo_content(
                doc,
                node,
                ctx,
                depth,
                content_box,
                positioned_children,
            );
            let mut block = BlockPageable::with_positioned_children(positioned_children)
                .with_pagination(extract_pagination_from_column_css(ctx, node))
                .with_style(style)
                .with_visible(visible)
                .with_id(extract_block_id(node));
            block.wrap(width, 10000.0);
            Box::new(block)
        }
    } else {
        let layout_children_guard_2 = node.layout_children.borrow();
        let children: &[usize] = layout_children_guard_2.as_deref().unwrap_or(&node.children);
        let positioned_children =
            positioned::collect_positioned_children(doc, children, ctx, depth);
        let (positioned_children, _has_pseudo) = pseudo::wrap_with_pseudo_content(
            doc,
            node,
            ctx,
            depth,
            content_box,
            positioned_children,
        );
        let mut block = BlockPageable::with_positioned_children(positioned_children)
            .with_pagination(extract_pagination_from_column_css(ctx, node))
            .with_style(style)
            .with_visible(visible)
            .with_id(extract_block_id(node));
        block.wrap(width, 10000.0);
        Box::new(block)
    }
}

fn convert_node_inner(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    let node = doc.get_node(node_id).unwrap();
    let (width, height) = size_in_pt(node.final_layout.size);

    // Check if this is a list item with an outside marker (must be before inline root check).
    //
    // Inside-positioned markers are injected into Parley's inline layout by Blitz
    // (blitz-dom/src/layout/construct.rs in `build_inline_layout`), so when the
    // `<li>` IS an inline root they fall through to the normal paragraph path below
    // and render correctly. For `list-style-image` + inside, `resolve_inside_image_marker`
    // injects the image at the start of the paragraph's first line.
    //
    // Known limitation: when `<li>` is NOT an inline root (contains only block
    // children, e.g. `<li><p>...</p></li>`) or is empty, neither Blitz nor
    // fulgur injects the marker, and the marker is not rendered. This matches
    // upstream Blitz behavior — Blitz's inline-layout injection only fires for
    // inline-root elements.
    if let Some(elem_data) = node.element_data()
        && elem_data.list_item_data.as_ref().is_some_and(|d| {
            matches!(
                d.position,
                blitz_dom::node::ListItemLayoutPosition::Outside(_)
            )
        })
    {
        let (marker_lines, marker_width, marker_line_height) =
            list_marker::extract_marker_lines(doc, node, ctx);
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);

        // Try list-style-image first; fall back to text marker if unresolved.
        let marker = list_marker::resolve_list_marker(node, marker_line_height, ctx.assets)
            .unwrap_or(ListItemMarker::Text {
                lines: marker_lines,
                width: marker_width,
            });

        // Build body WITHOUT opacity — ListItemPageable wraps everything in
        // a single opacity group. But DO propagate visibility to the body's
        // own content (paragraph/image), since those are synthetic children
        // representing the node's own content, not real CSS children.
        let content_box = compute_content_box(node, &style);
        let body = build_list_item_body(
            doc,
            node,
            style,
            visible,
            width,
            height,
            content_box,
            ctx,
            depth,
        );
        let mut item = ListItemPageable {
            marker,
            marker_line_height,
            body,
            style: BlockStyle::default(),
            width,
            height: 0.0,
            opacity,
            visible,
        };
        item.wrap(width, 10000.0);
        return Box::new(item);
    }

    // Fallback: display: list-item with list-style-image but no list_item_data
    // (Blitz 0.2.4 skips list_item_data when list-style-type: none).
    //
    // The primary guard above now only matches Outside-positioned items, so we
    // must additionally require `list_item_data.is_none()` here to avoid
    // intercepting inside-positioned items that DO have list_item_data — those
    // should fall through to the inline-root path so `resolve_inside_image_marker`
    // can inject the marker inline.
    if let Some(styles) = node.primary_styles()
        && styles.get_box().display.is_list_item()
        && node
            .element_data()
            .is_none_or(|e| e.list_item_data.is_none())
    {
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);

        // Derive line_height from computed styles since there is no Parley layout.
        // Honour explicit line-height first; fall back to font-size * 1.2 for
        // `normal`, matching the same heuristic Blitz uses internally.
        let line_height = {
            use style::values::computed::font::LineHeight;
            let font_size_pt = px_to_pt(styles.clone_font_size().used_size().px());
            match styles.clone_line_height() {
                LineHeight::Normal => font_size_pt * DEFAULT_LINE_HEIGHT_RATIO,
                LineHeight::Number(num) => font_size_pt * num.0,
                LineHeight::Length(value) => px_to_pt(value.0.px()),
            }
        };

        if let Some(marker) = list_marker::resolve_list_marker(node, line_height, ctx.assets) {
            let content_box = compute_content_box(node, &style);
            let body = build_list_item_body(
                doc,
                node,
                style,
                visible,
                width,
                height,
                content_box,
                ctx,
                depth,
            );
            let mut item = ListItemPageable {
                marker,
                marker_line_height: line_height,
                body,
                style: BlockStyle::default(),
                width,
                height: 0.0,
                opacity,
                visible,
            };
            item.wrap(width, 10000.0);
            return Box::new(item);
        }
    }

    // Inside-positioned marker on non-inline-root <li> (e.g. `<li><p>text</p></li>`
    // or empty `<li>`). Blitz only injects inside markers via `build_inline_layout`,
    // which doesn't run for non-inline-root elements. We shape the marker with
    // skrifa and inject it into the first child ParagraphPageable.
    if let Some(elem_data) = node.element_data()
        && let Some(list_data) = &elem_data.list_item_data
        && matches!(
            list_data.position,
            blitz_dom::node::ListItemLayoutPosition::Inside
        )
        && !node.flags.is_inline_root()
    {
        let marker = &list_data.marker;
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);
        let content_box = compute_content_box(node, &style);

        // Derive font_size and line_height from computed styles.
        let (font_size_pt, line_height) = if let Some(styles) = node.primary_styles() {
            let fs = px_to_pt(styles.clone_font_size().used_size().px());
            let lh = {
                use style::values::computed::font::LineHeight;
                match styles.clone_line_height() {
                    LineHeight::Normal => fs * DEFAULT_LINE_HEIGHT_RATIO,
                    LineHeight::Number(num) => fs * num.0,
                    LineHeight::Length(value) => px_to_pt(value.0.px()),
                }
            };
            (fs, lh)
        } else {
            (px_to_pt(12.0), px_to_pt(12.0) * DEFAULT_LINE_HEIGHT_RATIO)
        };

        let color = get_text_color(doc, node_id);

        let layout_children_guard_inside = node.layout_children.borrow();
        let children: &[usize] = layout_children_guard_inside
            .as_deref()
            .unwrap_or(&node.children);
        if children.is_empty() {
            // Empty <li>: create a standalone paragraph with just the marker.
            // Try image marker first (list-style-image), then text fallback.
            let marker_item: Option<LineItem> =
                list_marker::resolve_inside_image_marker(node, line_height, ctx.assets)
                    .map(LineItem::Image)
                    .or_else(|| {
                        let (fd, fi) = list_marker::find_marker_font(marker, ctx.assets, &[])?;
                        let run = list_marker::shape_marker_with_skrifa(
                            marker,
                            &fd,
                            fi,
                            font_size_pt,
                            color,
                        )?;
                        Some(LineItem::Text(run))
                    });
            if let Some(item) = marker_item {
                let paragraph = ParagraphPageable::new(vec![ShapedLine {
                    height: line_height,
                    baseline: line_height / DEFAULT_LINE_HEIGHT_RATIO,
                    items: vec![item],
                }]);
                let (child_x, child_y) = style.content_inset();
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(paragraph),
                    x: child_x,
                    y: child_y,
                }];
                let (positioned_children, _has_pseudo) = pseudo::wrap_with_pseudo_content(
                    doc,
                    node,
                    ctx,
                    depth,
                    content_box,
                    paragraph_children,
                );
                let needs_wrapper = style.needs_block_wrapper();
                let mut block = BlockPageable::with_positioned_children(positioned_children)
                    .with_pagination(extract_pagination_from_column_css(ctx, node))
                    .with_style(style)
                    .with_opacity(opacity)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, 10000.0);
                if needs_wrapper {
                    block.layout_size = Some(Size { width, height });
                }
                return Box::new(block);
            }
            // No marker resolved — fall through to normal empty-element handling
        } else {
            // Non-empty <li> with block children: convert children, then inject
            // marker into the first ParagraphPageable found in the tree.
            // Try image marker first (list-style-image), then text fallback.
            let mut positioned_children =
                positioned::collect_positioned_children(doc, children, ctx, depth);

            let marker_item: Option<LineItem> =
                list_marker::resolve_inside_image_marker(node, line_height, ctx.assets)
                    .map(LineItem::Image)
                    .or_else(|| {
                        let (fd, fi) = list_marker::find_marker_font(
                            marker,
                            ctx.assets,
                            &positioned_children,
                        )?;
                        let run = list_marker::shape_marker_with_skrifa(
                            marker,
                            &fd,
                            fi,
                            font_size_pt,
                            color,
                        )?;
                        Some(LineItem::Text(run))
                    });

            if let Some(item) = marker_item {
                if !list_marker::inject_inside_marker_item_into_children(
                    &mut positioned_children,
                    item.clone(),
                ) {
                    // No paragraph descendant found — insert a standalone marker paragraph.
                    let paragraph = ParagraphPageable::new(vec![ShapedLine {
                        height: line_height,
                        baseline: line_height / DEFAULT_LINE_HEIGHT_RATIO,
                        items: vec![item],
                    }]);
                    positioned_children.insert(
                        0,
                        PositionedChild {
                            child: Box::new(paragraph),
                            x: 0.0,
                            y: 0.0,
                        },
                    );
                }
            }

            let (positioned_children, _has_pseudo) = pseudo::wrap_with_pseudo_content(
                doc,
                node,
                ctx,
                depth,
                content_box,
                positioned_children,
            );
            let has_style = style.needs_block_wrapper();
            let mut block = BlockPageable::with_positioned_children(positioned_children)
                .with_pagination(extract_pagination_from_column_css(ctx, node))
                .with_style(style)
                .with_opacity(opacity)
                .with_visible(visible)
                .with_id(extract_block_id(node));
            block.wrap(width, 10000.0);
            if has_style {
                block.layout_size = Some(Size { width, height });
            }
            return Box::new(block);
        }
    }

    // Check if this is a table element
    if let Some(elem_data) = node.element_data() {
        let tag = elem_data.name.local.as_ref();
        if tag == "table" {
            return convert_table(doc, node, ctx, depth);
        }
    }

    // Replaced-element dispatch: <img>, <svg>, content: url() — see replaced::try_convert.
    if let Some(p) = replaced::try_convert(doc, node_id, ctx) {
        return p;
    }

    // Check if this is an inline root (contains text layout)
    if node.flags.is_inline_root() {
        let paragraph_opt = extract_paragraph(doc, node, ctx, depth);
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);
        let content_box = compute_content_box(node, &style);

        // Inline pseudo images (display: inline is the CSS default for pseudos)
        let before_inline = node
            .before
            .and_then(|id| doc.get_node(id))
            .filter(|p| !pseudo::is_block_pseudo(p))
            .and_then(|p| {
                pseudo::build_inline_pseudo_image(
                    p,
                    content_box.width,
                    content_box.height,
                    ctx.assets,
                )
            })
            .map(|mut img| {
                pseudo::attach_link_to_inline_image(&mut img, doc, node.id);
                img
            });
        let after_inline = node
            .after
            .and_then(|id| doc.get_node(id))
            .filter(|p| !pseudo::is_block_pseudo(p))
            .and_then(|p| {
                pseudo::build_inline_pseudo_image(
                    p,
                    content_box.width,
                    content_box.height,
                    ctx.assets,
                )
            })
            .map(|mut img| {
                pseudo::attach_link_to_inline_image(&mut img, doc, node.id);
                img
            });

        if let Some(mut paragraph) = paragraph_opt {
            // Inject pseudo images BEFORE the list marker so the marker stays
            // at index 0 of the first line after both injections. CSS order
            // for list-style-position: inside is: marker → ::before → content.
            // Blitz already pushes text markers to the inline layout before
            // ::before, so when list-style-image triggers marker injection we
            // must put it at index 0 last.
            if before_inline.is_some() || after_inline.is_some() {
                pseudo::inject_inline_pseudo_images(
                    &mut paragraph.lines,
                    before_inline,
                    after_inline,
                );
                recalculate_paragraph_line_boxes(&mut paragraph.lines);
                paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
            }

            // Inject inside list-style-image as inline image at start of first line.
            // Runs AFTER pseudo image injection so the marker ends up at index 0
            // and pushes existing items (including ::before) to index 1+.
            if !paragraph.lines.is_empty() {
                let first_line_height = paragraph.lines[0].height;
                if let Some(inline_img) =
                    list_marker::resolve_inside_image_marker(node, first_line_height, ctx.assets)
                {
                    let shift = inline_img.width;
                    for item in &mut paragraph.lines[0].items {
                        match item {
                            LineItem::Text(run) => run.x_offset += shift,
                            LineItem::Image(i) => i.x_offset += shift,
                            LineItem::InlineBox(ib) => ib.x_offset += shift,
                        }
                    }
                    paragraph.lines[0]
                        .items
                        .insert(0, LineItem::Image(inline_img));
                    recalculate_paragraph_line_boxes(&mut paragraph.lines);
                    paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
                }
            }

            // Then existing block pseudo check
            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_pseudo_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            let pagination = extract_pagination_from_column_css(ctx, node);
            if style.needs_block_wrapper()
                || has_pseudo
                || pagination != crate::pageable::Pagination::default()
            {
                let (child_x, child_y) = style.content_inset();
                // Propagate visibility to the inner paragraph — it's not a real CSS child
                // but the node's own text content, so it must respect the node's visibility.
                // Do NOT propagate opacity — the wrapping block handles it via push_opacity.
                let mut p = paragraph;
                p.visible = visible;
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(p),
                    x: child_x,
                    y: child_y,
                }];
                let mut children = pseudo::wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    paragraph_children,
                );
                children.extend(abs_pseudos);
                let mut block = BlockPageable::with_positioned_children(children)
                    .with_pagination(pagination)
                    .with_style(style)
                    .with_opacity(opacity)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, height);
                // Use Taffy's computed height (includes padding + border) instead of children-only height
                block.layout_size = Some(Size { width, height });
                return Box::new(block);
            }
            let mut p = paragraph;
            p.opacity = opacity;
            p.visible = visible;
            return Box::new(p);
        } else if before_inline.is_some() || after_inline.is_some() {
            // Synthesize a minimal paragraph for pseudo-only elements (e.g.
            // `<span class="icon"></span>` with `::before { content: url(...) }`)
            let mut line = ShapedLine {
                height: 0.0,
                baseline: 0.0,
                items: vec![],
            };
            pseudo::inject_inline_pseudo_images(
                std::slice::from_mut(&mut line),
                before_inline,
                after_inline,
            );
            let font_metrics = metrics_from_line(&line);
            crate::paragraph::recalculate_line_box(&mut line, &font_metrics);
            let mut paragraph = ParagraphPageable::new(vec![line]);
            paragraph.opacity = opacity;
            paragraph.visible = visible;

            // Check for block pseudo images too
            let (before_pseudo, after_pseudo) =
                pseudo::build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let abs_pseudos = positioned::build_absolute_pseudo_children(doc, node, ctx, depth);
            let has_pseudo =
                before_pseudo.is_some() || after_pseudo.is_some() || !abs_pseudos.is_empty();
            let pagination = extract_pagination_from_column_css(ctx, node);
            if style.needs_block_wrapper()
                || has_pseudo
                || pagination != crate::pageable::Pagination::default()
            {
                let (child_x, child_y) = style.content_inset();
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(paragraph),
                    x: child_x,
                    y: child_y,
                }];
                let mut children = pseudo::wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    paragraph_children,
                );
                children.extend(abs_pseudos);
                let mut block = BlockPageable::with_positioned_children(children)
                    .with_pagination(pagination)
                    .with_style(style)
                    .with_opacity(opacity)
                    .with_visible(visible)
                    .with_id(extract_block_id(node));
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                return Box::new(block);
            }
            return Box::new(paragraph);
        }
        // Fall through: inline root with no text and no inline pseudo images
    }

    let layout_children_guard = node.layout_children.borrow();
    let children: &[usize] = layout_children_guard.as_deref().unwrap_or(&node.children);

    if children.is_empty() {
        let style = extract_block_style(node, ctx.assets);
        let content_box = compute_content_box(node, &style);
        // Check for pseudo images even on childless elements — e.g.
        // `<div class="icon"></div>` with `.icon::before { content: url(...) }`
        // should emit the image. Without this the pseudo is silently dropped.
        let (positioned_children, has_pseudo) =
            pseudo::wrap_with_pseudo_content(doc, node, ctx, depth, content_box, Vec::new());
        let pagination = extract_pagination_from_column_css(ctx, node);
        if style.needs_block_wrapper()
            || has_pseudo
            || pagination != crate::pageable::Pagination::default()
        {
            let (opacity, visible) = extract_opacity_visible(node);
            let mut block = BlockPageable::with_positioned_children(positioned_children)
                .with_pagination(pagination)
                .with_style(style)
                .with_opacity(opacity)
                .with_visible(visible)
                .with_id(extract_block_id(node));
            block.wrap(width, height);
            block.layout_size = Some(Size { width, height });
            return Box::new(block);
        }
        // Plain leaf node — create a spacer with the computed height
        let mut spacer = SpacerPageable::new(height);
        spacer.wrap(width, height);
        return Box::new(spacer);
    }

    // Container node — collect children with Taffy-computed positions
    let positioned_children = positioned::collect_positioned_children(doc, children, ctx, depth);
    let style = extract_block_style(node, ctx.assets);
    let content_box = compute_content_box(node, &style);
    let (positioned_children, _has_pseudo) =
        pseudo::wrap_with_pseudo_content(doc, node, ctx, depth, content_box, positioned_children);

    let has_style = style.needs_block_wrapper();
    let (opacity, visible) = extract_opacity_visible(node);
    let mut block = BlockPageable::with_positioned_children(positioned_children)
        .with_pagination(extract_pagination_from_column_css(ctx, node))
        .with_style(style)
        .with_opacity(opacity)
        .with_visible(visible)
        .with_id(extract_block_id(node));
    block.wrap(width, 10000.0);
    if has_style {
        block.layout_size = Some(Size { width, height });
    }
    Box::new(block)
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

/// Extract `LineFontMetrics` from a `ShapedLine`'s Text items using skrifa.
/// Returns per-line accurate metrics instead of reusing a single set from the
/// first glyph run in the paragraph. Falls back to defaults if no text items.
fn metrics_from_line(line: &ShapedLine) -> LineFontMetrics {
    let default = LineFontMetrics {
        ascent: 12.0,
        descent: 4.0,
        x_height: 8.0,
        subscript_offset: 4.0,
        superscript_offset: 6.0,
    };
    for item in &line.items {
        let run = match item {
            LineItem::Text(r) => r,
            LineItem::Image(_) => continue,
            LineItem::InlineBox(_) => continue,
        };
        if let Ok(font_ref) = skrifa::FontRef::from_index(&run.font_data, run.font_index) {
            let metrics = font_ref.metrics(
                skrifa::instance::Size::new(run.font_size),
                skrifa::instance::LocationRef::default(),
            );
            // skrifa Metrics exposes x_height but not subscript/superscript
            // offsets directly. Approximate from ascent (same ratios as CSS
            // typographic conventions).
            return LineFontMetrics {
                ascent: metrics.ascent,
                descent: metrics.descent.abs(),
                x_height: metrics.x_height.unwrap_or(metrics.ascent * 0.5),
                subscript_offset: metrics.ascent * 0.3,
                superscript_offset: metrics.ascent * 0.4,
            };
        }
    }
    default
}

/// Recalculate line boxes for all lines in a paragraph, correctly handling
/// the coordinate system difference between paragraph-absolute baselines and
/// line-local coordinates expected by `recalculate_line_box`.
///
/// `recalculate_line_box` assumes `line.baseline` is line-local (i.e. relative
/// to the line's own top edge), but Parley sets baselines as paragraph-absolute
/// offsets. For the first line these coincide, but for subsequent lines the
/// baseline is offset by the cumulative height of preceding lines. This helper
/// converts to line-local before calling `recalculate_line_box`, then converts
/// back to paragraph-absolute and promotes `computed_y` to paragraph-absolute
/// so `draw_shaped_lines` can use `y + img.computed_y` directly (matching the
/// `y + line.baseline` pattern used for text).
///
/// Font metrics are extracted per-line from the line's own Text items via
/// skrifa, so lines with different font sizes get accurate vertical-align.
fn recalculate_paragraph_line_boxes(lines: &mut [ShapedLine]) {
    // Track original and new cumulative heights separately.
    // Parley baselines are computed against original heights, so we use
    // original_y_acc for paragraph→line-local conversion. After expansion,
    // new_y_acc tracks the updated positions for line-local→paragraph.
    let mut original_y_acc: f32 = 0.0;
    let mut new_y_acc: f32 = 0.0;
    for line in lines.iter_mut() {
        let original_height = line.height;
        let font_metrics = metrics_from_line(line);
        // Convert baseline from paragraph-absolute to line-local
        // using original cumulative heights (what Parley computed against)
        line.baseline -= original_y_acc;
        crate::paragraph::recalculate_line_box(line, &font_metrics);
        // Convert computed_y from line-local to new paragraph-absolute
        for item in &mut line.items {
            if let LineItem::Image(img) = item {
                img.computed_y += new_y_acc;
            }
        }
        // Convert baseline to new paragraph-absolute
        line.baseline += new_y_acc;
        original_y_acc += original_height;
        new_y_acc += line.height;
    }
}

/// Convert a table element into a TablePageable with header/body cell groups.
fn convert_table(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    let (width, height) = size_in_pt(node.final_layout.size);
    let style = extract_block_style(node, ctx.assets);

    let mut header_cells: Vec<PositionedChild> = Vec::new();
    let mut body_cells: Vec<PositionedChild> = Vec::new();

    // Walk table children to separate thead from tbody
    for &child_id in &node.children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        let is_thead = is_table_section(child_node, "thead");

        collect_table_cells(
            doc,
            child_id,
            is_thead,
            &mut header_cells,
            &mut body_cells,
            ctx,
            depth,
        );
    }

    // Calculate header height from header cells
    let header_height = header_cells
        .iter()
        .fold(0.0f32, |max_h, pc| max_h.max(pc.y + pc.child.height()));

    let (opacity, visible) = extract_opacity_visible(node);
    let table = TablePageable {
        header_cells,
        body_cells,
        header_height,
        style,
        layout_size: Some(Size { width, height }),
        width,
        cached_height: height,
        opacity,
        visible,
        id: extract_block_id(node),
    };
    Box::new(table)
}

/// Check if a node is a specific table section element.
fn is_table_section(node: &Node, section_name: &str) -> bool {
    if let Some(elem) = node.element_data() {
        elem.name.local.as_ref() == section_name
    } else {
        false
    }
}

/// Recursively collect table cells (td/th) from a table subtree.
fn collect_table_cells(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    is_header: bool,
    header_cells: &mut Vec<PositionedChild>,
    body_cells: &mut Vec<PositionedChild>,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };

    // Drain counter ops on the current section/row node itself so that
    // counter-reset / counter-increment / counter-set declared on
    // <thead>/<tbody>/<tr> reach `collect_counter_states()` for margin boxes.
    // Without this, ops on these intermediate nodes stay in
    // `ctx.counter_ops_by_node` forever and never propagate.
    {
        let (x, y, _, _) = layout_in_pt(&node.final_layout);
        let out: &mut Vec<PositionedChild> = if is_header { header_cells } else { body_cells };
        emit_counter_op_markers(node_id, x, y, ctx, out);
        emit_orphan_bookmark_marker(node_id, x, y, ctx, out);
    }

    let layout_children_guard = node.layout_children.borrow();
    let effective_children = layout_children_guard.as_deref().unwrap_or(&node.children);
    for &child_id in effective_children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        if matches!(&child_node.data, NodeData::Comment) {
            continue;
        }
        if is_non_visual_element(child_node) {
            continue;
        }

        let (cx, cy, cw, ch) = layout_in_pt(&child_node.final_layout);

        // Zero-size container (tr, thead, tbody) — recurse into children
        let child_effective_is_empty = child_node
            .layout_children
            .borrow()
            .as_deref()
            .unwrap_or(&child_node.children)
            .is_empty();
        if ch == 0.0 && cw == 0.0 && !child_effective_is_empty {
            let child_is_header = is_header || is_table_section(child_node, "thead");
            collect_table_cells(
                doc,
                child_id,
                child_is_header,
                header_cells,
                body_cells,
                ctx,
                depth + 1,
            );
            continue;
        }

        // Skip zero-size leaves
        if ch == 0.0 && cw == 0.0 {
            continue;
        }

        // Actual cell (td/th) — convert and add to appropriate group
        let cell_pageable = convert_node(doc, child_id, ctx, depth + 1);
        let positioned = PositionedChild {
            child: cell_pageable,
            x: cx,
            y: cy,
        };

        if is_header {
            header_cells.push(positioned);
        } else {
            body_cells.push(positioned);
        }
    }
}

/// Walk up from `start_id` to find the closest `<a href>` ancestor and build
/// a `LinkSpan` describing its target. Returns `None` if no ancestor is an
/// anchor with a non-empty `href`.
///
/// Caller should memoize results per anchor node ID so multiple glyph runs
/// descended from the same `<a>` share one `Arc<LinkSpan>` (pointer identity,
/// required for later rect-dedup in PDF emission).
fn resolve_enclosing_anchor(
    doc: &blitz_dom::BaseDocument,
    start_id: usize,
) -> Option<(usize, LinkSpan)> {
    let mut cur = Some(start_id);
    let mut depth: usize = 0;
    while let Some(id) = cur {
        // Defense-in-depth against pathological / malformed parent chains,
        // matching the bounds applied in `debug_print_tree`,
        // `collect_positioned_children`, and `blitz_adapter::element_text`.
        if depth >= MAX_DOM_DEPTH {
            return None;
        }
        let node = doc.get_node(id)?;
        if let NodeData::Element(el) = &node.data {
            if el.name.local.as_ref() == "a" {
                let href = crate::blitz_adapter::get_attr(el, "href")?.trim();
                if href.is_empty() {
                    return None;
                }
                let target = if let Some(frag) = href.strip_prefix('#') {
                    LinkTarget::Internal(Arc::new(frag.to_string()))
                } else {
                    LinkTarget::External(Arc::new(href.to_string()))
                };
                let alt = crate::blitz_adapter::element_text(doc, id);
                let alt_text = if alt.is_empty() { None } else { Some(alt) };
                return Some((id, LinkSpan { target, alt_text }));
            }
        }
        cur = node.parent;
        depth += 1;
    }
    None
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
        match resolve_enclosing_anchor(doc, start_id) {
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

/// Recursively convert the Blitz node referenced by a Parley `InlineBox.id`
/// and return the resulting `Pageable` as the inline-box content.
///
/// `InlineBoxContent` is `Box<dyn Pageable>`, so any wrapper chain emitted
/// by `convert_node` (Transform / StringSet / CounterOp / BookmarkMarker /
/// RunningElement) survives verbatim and its side effects apply around the
/// inner Block / Paragraph when the inline-box is drawn. `MAX_DOM_DEPTH`
/// is already enforced inside `convert_node`, so a depth-exhausted node
/// returning a `SpacerPageable` flows through as a zero-height content
/// rather than dropping the inline-box.
fn convert_inline_box_node(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> crate::paragraph::InlineBoxContent {
    // This function processes an inline box emitted by Parley during
    // paragraph layout. Per CSS, `position: absolute | fixed` elements are
    // out of normal flow and should never appear in Parley's inline
    // sequence, but Blitz currently routes absolutely-positioned pseudo
    // elements (`::before` / `::after`) through Parley's inline layout
    // anyway, which would paint them at `(0, 0)` of the surrounding flow.
    //
    // Suppress that rendering path ONLY for pseudos (detected by
    // `is_pseudo_node`), because `build_absolute_pseudo_children` re-emits
    // pseudos at the CSS-correct position by walking to the containing
    // block. It does NOT handle regular (non-pseudo) absolute children —
    // those have no re-emit path, so letting them fall through to
    // `convert_node` at least preserves their content (even if they end up
    // at Parley's inline position). Suppressing non-pseudos here would
    // silently drop them, which is worse.
    if let Some(node) = doc.get_node(node_id) {
        if positioned::is_absolutely_positioned(node) && is_pseudo_node(doc, node) {
            return Box::new(SpacerPageable::new(0.0));
        }
    }
    convert_node(doc, node_id, ctx, depth + 1)
}

/// Extract a ParagraphPageable from an inline root node.
fn extract_paragraph(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Option<ParagraphPageable> {
    use crate::paragraph::InlineBoxItem;
    let elem_data = node.element_data()?;
    let text_layout = elem_data.inline_layout_data.as_ref()?;

    let parley_layout = &text_layout.layout;
    let text = &text_layout.text;

    let mut shaped_lines = Vec::new();
    // Running total of line heights seen so far (pt). Parley reports
    // `PositionedInlineBox.y` in paragraph-relative coordinates, but
    // `InlineBoxItem.computed_y` is expected to be line-relative; subtract
    // `accumulated_line_top` when building the item.
    let mut accumulated_line_top: f32 = 0.0;

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        let mut items = Vec::new();

        for item in line.items() {
            match item {
                parley::PositionedLayoutItem::GlyphRun(glyph_run) => {
                    let run = glyph_run.run();
                    let font_ref = run.font();
                    let font_index = font_ref.index;
                    let font_arc = ctx.get_or_insert_font(font_ref);
                    // Parley (wired through blitz at scale=1.0) reports font
                    // size in CSS px. The Pageable tree works in PDF pt, and
                    // krilla's `draw_glyphs` also wants pt. Convert here so
                    // every downstream computation (glyph advances,
                    // decoration widths, link rects) naturally lands in pt.
                    // `glyph.advance / font_size` stays a unitless ratio.
                    let font_size_parley = run.font_size();
                    let font_size = px_to_pt(font_size_parley);

                    // Get text color from the brush (node ID) → computed styles
                    let brush = &glyph_run.style().brush;
                    let color = get_text_color(doc, brush.id);
                    let decoration = get_text_decoration(doc, brush.id);
                    let link = ctx.link_cache.lookup(doc, brush.id);

                    // Extract raw glyphs (relative offsets, not absolute positions)
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
                        let run_text = text.clone();
                        // Run-level x_offset is also in parley px; convert.
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
                parley::PositionedLayoutItem::InlineBox(positioned) => {
                    let node_id = positioned.id as usize;
                    // Absolute/fixed pseudos are out of normal flow and must
                    // NOT reserve inline width or contribute to line metrics.
                    // Returning a `SpacerPageable` from
                    // `convert_inline_box_node` alone is insufficient because
                    // this branch would still push an `InlineBoxItem` built
                    // from Parley's `positioned.width` / `positioned.height`,
                    // which reserves space even when the content is blank.
                    // Skip the whole `items.push` for such pseudos — the
                    // containing block's converter re-emits them at their
                    // CSS-correct position via
                    // `build_absolute_pseudo_children`.
                    if let Some(box_node) = doc.get_node(node_id) {
                        if positioned::is_absolutely_positioned(box_node)
                            && is_pseudo_node(doc, box_node)
                        {
                            continue;
                        }
                    }
                    let content = convert_inline_box_node(doc, node_id, ctx, depth);
                    let link = ctx.link_cache.lookup(doc, node_id);
                    // Parley's `PositionedInlineBox` has no baseline field
                    // (Parley 0.6), so it defaults `y` so that the box's
                    // bottom edge sits on the surrounding text baseline
                    // (`y + height = surrounding_baseline`). CSS 2.1
                    // §10.8.1 instead wants the box's *inner* last-line
                    // baseline to coincide with the surrounding baseline.
                    // Shift the box down by `height - inner_baseline_offset`
                    // to realize that. When the box has no in-flow baseline
                    // (empty, overflow clipped, flex/grid without text),
                    // fall back to Parley's default — which is the CSS
                    // bottom-edge fallback described in the same clause.
                    let height_pt = px_to_pt(positioned.height);
                    let baseline_shift =
                        crate::paragraph::inline_box_baseline_offset(content.as_ref())
                            .map(|bo| height_pt - bo)
                            .unwrap_or(0.0);
                    let computed_y = px_to_pt(positioned.y) - accumulated_line_top + baseline_shift;
                    // Propagate `visibility: hidden` from the inner pageable
                    // (set by `extract_block_style`) so the inline-box is
                    // treated as invisible at draw time — link rect emission
                    // is then also suppressed by the `!ib.visible` guard in
                    // `draw_shaped_lines`. `Pageable::is_visible()` walks
                    // wrappers for us so a `visibility: hidden` inline-block
                    // keeps that state through a transform / marker chain.
                    let visible = content.is_visible();
                    items.push(LineItem::InlineBox(InlineBoxItem {
                        content,
                        width: px_to_pt(positioned.width),
                        height: height_pt,
                        x_offset: px_to_pt(positioned.x),
                        computed_y,
                        link,
                        opacity: 1.0,
                        visible,
                    }));
                }
            }
        }

        let line_height_pt = px_to_pt(metrics.line_height);
        shaped_lines.push(ShapedLine {
            height: line_height_pt,
            baseline: px_to_pt(metrics.baseline),
            items,
        });
        accumulated_line_top += line_height_pt;
    }

    if shaped_lines.is_empty() {
        return None;
    }

    // Propagate the inline-root `id` so headings like `<h1 id="top">` that
    // end up as plain `ParagraphPageable` (no block wrapper triggered by the
    // default style) still register with `DestinationRegistry` for
    // `href="#top"` resolution.
    Some(ParagraphPageable::new(shaped_lines).with_id(extract_block_id(node)))
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
    use crate::image::ImageFormat;

    // Minimal 1x1 red PNG — matches crates/fulgur/src/image.rs tests but is
    // duplicated here so convert.rs tests don't depend on image.rs internals.
    const TEST_PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn sample_png_arc() -> Arc<Vec<u8>> {
        Arc::new(TEST_PNG_1X1.to_vec())
    }

    #[test]
    fn test_make_image_pageable_both_dimensions() {
        let img = super::replaced::make_image_pageable(
            sample_png_arc(),
            ImageFormat::Png,
            Some(100.0),
            Some(50.0),
            1.0,
            true,
        );
        assert_eq!(img.width, 100.0);
        assert_eq!(img.height, 50.0);
        assert_eq!(img.opacity, 1.0);
        assert!(img.visible);
    }

    #[test]
    fn test_make_image_pageable_width_only_uses_intrinsic_aspect() {
        // Intrinsic 1x1 → aspect 1.0 → width=40 produces height=40.
        let img = super::replaced::make_image_pageable(
            sample_png_arc(),
            ImageFormat::Png,
            Some(40.0),
            None,
            1.0,
            true,
        );
        assert_eq!(img.width, 40.0);
        assert_eq!(img.height, 40.0);
    }

    #[test]
    fn test_make_image_pageable_height_only_uses_intrinsic_aspect() {
        let img = super::replaced::make_image_pageable(
            sample_png_arc(),
            ImageFormat::Png,
            None,
            Some(25.0),
            1.0,
            true,
        );
        assert_eq!(img.width, 25.0);
        assert_eq!(img.height, 25.0);
    }

    #[test]
    fn test_make_image_pageable_intrinsic_fallback() {
        let img = super::replaced::make_image_pageable(
            sample_png_arc(),
            ImageFormat::Png,
            None,
            None,
            0.5,
            false,
        );
        assert_eq!(img.width, 1.0);
        assert_eq!(img.height, 1.0);
        assert_eq!(img.opacity, 0.5);
        assert!(!img.visible);
    }

    fn find_h1(doc: &blitz_html::HtmlDocument) -> usize {
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

    #[test]
    fn test_build_pseudo_image_reads_content_url() {
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .expect("read examples/image/icon.png");
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            h1::before {
                content: url("icon.png");
                display: block;
                width: 48px;
                height: 48px;
            }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let h1_id = find_h1(&doc);
        let before_id = doc
            .get_node(h1_id)
            .unwrap()
            .before
            .expect("::before pseudo");
        let pseudo = doc.get_node(before_id).unwrap();
        let (parent_w, parent_h) = size_in_pt(doc.get_node(h1_id).unwrap().final_layout.size);

        let img = super::pseudo::build_pseudo_image(pseudo, parent_w, parent_h, Some(&bundle))
            .expect("build_pseudo_image should return Some for content: url()");
        // 48 CSS px × 0.75 = 36 pt
        assert_eq!(img.width, 36.0);
        assert_eq!(img.height, 36.0);
    }

    #[test]
    fn test_build_pseudo_image_width_only_uses_intrinsic_aspect() {
        // icon.png is 32x32 so aspect = 1.0. width:20px → 15 pt, height
        // back-propagates via intrinsic aspect → 15 pt.
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("icon.png"); display: block; width: 20px; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.unwrap();
        let pseudo = doc.get_node(before_id).unwrap();
        let (parent_w, parent_h) = size_in_pt(doc.get_node(h1_id).unwrap().final_layout.size);

        let img =
            super::pseudo::build_pseudo_image(pseudo, parent_w, parent_h, Some(&bundle)).unwrap();
        assert_eq!(img.width, 15.0);
        assert_eq!(img.height, 15.0);
    }

    #[test]
    fn test_build_pseudo_image_missing_asset_returns_none() {
        let bundle = AssetBundle::new();
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("missing.png"); display: block; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.unwrap();
        let pseudo = doc.get_node(before_id).unwrap();
        assert!(
            super::pseudo::build_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle)).is_none(),
            "missing asset should silently return None"
        );
    }

    #[test]
    fn test_build_pseudo_image_no_assets_returns_none() {
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("icon.png"); }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.unwrap();
        let pseudo = doc.get_node(before_id).unwrap();
        assert!(super::pseudo::build_pseudo_image(pseudo, 800.0, 600.0, None).is_none());
    }

    #[test]
    fn test_build_pseudo_image_height_percent_resolves_against_parent_height() {
        // Verifies the coderabbit fix: height: 50% on the pseudo should
        // resolve against parent_content_height, not parent_content_width.
        // icon.png is 32x32 intrinsic, so with height=50% of 200 = 100 and
        // no explicit width, the aspect ratio gives width = 100.
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("icon.png"); display: block; height: 50%; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.unwrap();
        let pseudo = doc.get_node(before_id).unwrap();

        // Explicitly call with distinguishable width (400) and height (200)
        // so we can verify which basis is used for `height: 50%`.
        let img = super::pseudo::build_pseudo_image(pseudo, 400.0, 200.0, Some(&bundle)).unwrap();
        assert_eq!(
            img.height, 100.0,
            "height: 50% should resolve against parent_content_height (200.0)"
        );
        assert_eq!(
            img.width, 100.0,
            "intrinsic aspect (1:1) should give width = height"
        );
    }

    /// Recursively walk a Pageable tree and push any ImagePageable found.
    fn collect_images(p: &dyn Pageable, out: &mut Vec<(f32, f32)>) {
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

    #[test]
    fn test_dom_to_pageable_emits_block_pseudo_image() {
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            .wrap::before {
                content: url("icon.png");
                display: block;
                width: 24px;
                height: 24px;
            }
        </style></head><body><div class="wrap">hello</div></body></html>"#;

        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.iter().any(|(w, h)| *w == 18.0 && *h == 18.0),
            "expected an 18x18 pt ImagePageable (24 CSS px × 0.75) from ::before pseudo, got {:?}",
            images
        );
    }

    #[test]
    fn test_dom_to_pageable_inline_pseudo_ignored_phase1() {
        // Phase 1 only handles display:block pseudos. An inline pseudo with
        // content: url() should be silently ignored (Phase 2 will handle it).
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            p::before { content: url("icon.png"); width: 10px; height: 10px; }
        </style></head><body><p>hello</p></body></html>"#;

        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.is_empty(),
            "Phase 1 must not emit inline pseudo images; got {:?}",
            images
        );
    }

    #[test]
    fn test_dom_to_pageable_emits_pseudo_on_childless_element() {
        // Regression for Devin Review comment on PR #70: the children.is_empty()
        // branch used to skip pseudo injection. `<div class="icon"></div>` with
        // a block pseudo should still render the image.
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            .icon::before {
                content: url("icon.png");
                display: block;
                width: 16px;
                height: 16px;
            }
        </style></head><body><div class="icon"></div></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.iter().any(|(w, h)| *w == 12.0 && *h == 12.0),
            "childless element ::before pseudo should emit a 12x12 pt image (16 CSS px × 0.75); got {:?}",
            images
        );
    }

    #[test]
    fn test_dom_to_pageable_emits_pseudo_on_zero_size_block_leaf() {
        // Regression for coderabbit follow-up on PR #70: a 0x0 block leaf
        // was being skipped by the collect_positioned_children zero-size
        // leaf filter BEFORE reaching the convert_node `children.is_empty()`
        // branch. The pseudo probe (`node_has_block_pseudo_image`) now lets
        // such leaves fall through.
        //
        // Scope note: this test specifically targets a BLOCK element with
        // explicit width:0;height:0 that still has a ::before image — e.g.
        // a decorative sentinel div a template sets to 0x0 with a pseudo
        // icon. Inline `<span>` with a block ::before is a different
        // edge case that requires Phase 2 inline-flow handling.
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            .zero { display: block; width: 0; height: 0; }
            .zero::before {
                content: url("icon.png");
                display: block;
                width: 18px;
                height: 18px;
            }
        </style></head><body><section><div class="zero"></div></section></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        let mut images = Vec::new();
        walk_all_children(&*tree, &mut |p| collect_images(p, &mut images));
        assert!(
            images.iter().any(|(w, h)| *w == 13.5 && *h == 13.5),
            "zero-size block leaf with block pseudo should emit a 13.5x13.5 pt image (18 CSS px × 0.75); got {:?}",
            images
        );
    }

    #[test]
    fn test_dom_to_pageable_emits_pseudo_on_list_item_with_text() {
        // Regression for Devin Review comment on PR #70: the list item
        // inline-root body path used to skip pseudo injection. A <li> with
        // inline text content and a block ::before should still render.
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            li::before {
                content: url("icon.png");
                display: block;
                width: 12px;
                height: 12px;
            }
        </style></head><body><ul><li>item text</li></ul></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        let mut images = Vec::new();
        walk_all_children(&*tree, &mut |p| collect_images(p, &mut images));
        assert!(
            images.iter().any(|(w, h)| *w == 9.0 && *h == 9.0),
            "list item with text + block pseudo should emit a 9x9 pt image (12 CSS px × 0.75); got {:?}",
            images
        );
    }

    /// Walk a Pageable tree visiting every nested child via known container
    /// types. Used by tests that need to peek through `ListItemPageable`'s
    /// body, which the simple BlockPageable-only walker does not descend into.
    fn walk_all_children(p: &dyn Pageable, visit: &mut dyn FnMut(&dyn Pageable)) {
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

    // ---- inline pseudo image tests ----

    use crate::paragraph::VerticalAlign;

    fn make_test_inline_image(w: f32, h: f32) -> InlineImage {
        InlineImage {
            data: sample_png_arc(),
            format: ImageFormat::Png,
            width: w,
            height: h,
            x_offset: 0.0,
            vertical_align: VerticalAlign::Baseline,
            opacity: 1.0,
            visible: true,
            computed_y: 0.0,
            link: None,
        }
    }

    fn make_test_text_run(x_offset: f32, advance: f32) -> ShapedGlyphRun {
        ShapedGlyphRun {
            font_data: sample_png_arc(), // dummy — not rendered in unit tests
            font_index: 0,
            font_size: 12.0,
            color: [0, 0, 0, 255],
            decoration: TextDecoration::default(),
            glyphs: vec![ShapedGlyph {
                id: 0,
                x_advance: advance / 12.0, // normalized by font_size
                x_offset: 0.0,
                y_offset: 0.0,
                text_range: 0..1,
            }],
            text: "A".to_string(),
            x_offset,
            link: None,
        }
    }

    #[test]
    fn test_inject_before_shifts_existing_items() {
        let run = make_test_text_run(0.0, 60.0);
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Text(run)],
        }];
        let img = make_test_inline_image(20.0, 16.0);
        super::pseudo::inject_inline_pseudo_images(&mut lines, Some(img), None);

        assert_eq!(lines[0].items.len(), 2);
        // First item should be the image at x_offset 0
        if let LineItem::Image(ref i) = lines[0].items[0] {
            assert!((i.x_offset).abs() < 0.01, "img x_offset={}", i.x_offset);
            assert!((i.width - 20.0).abs() < 0.01);
        } else {
            panic!("expected Image at index 0");
        }
        // Second item (text) should be shifted by 20.0
        if let LineItem::Text(ref r) = lines[0].items[1] {
            assert!(
                (r.x_offset - 20.0).abs() < 0.01,
                "text x_offset={}",
                r.x_offset,
            );
        } else {
            panic!("expected Text at index 1");
        }
    }

    #[test]
    fn test_inject_after_appends_at_end() {
        let run = make_test_text_run(0.0, 60.0);
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Text(run)],
        }];
        let img = make_test_inline_image(15.0, 16.0);
        super::pseudo::inject_inline_pseudo_images(&mut lines, None, Some(img));

        assert_eq!(lines[0].items.len(), 2);
        // Last item should be the image
        if let LineItem::Image(ref i) = lines[0].items[1] {
            // Text run width = advance (normalized x_advance * font_size) = (60/12) * 12 = 60
            assert!(
                (i.x_offset - 60.0).abs() < 0.01,
                "after img x_offset={}",
                i.x_offset,
            );
        } else {
            panic!("expected Image at index 1");
        }
    }

    #[test]
    fn test_inject_both_before_and_after() {
        let run = make_test_text_run(0.0, 36.0);
        let mut lines = vec![ShapedLine {
            height: 16.0,
            baseline: 12.0,
            items: vec![LineItem::Text(run)],
        }];
        let before = make_test_inline_image(10.0, 16.0);
        let after = make_test_inline_image(10.0, 16.0);
        super::pseudo::inject_inline_pseudo_images(&mut lines, Some(before), Some(after));

        assert_eq!(lines[0].items.len(), 3);
        // Before image at 0
        if let LineItem::Image(ref i) = lines[0].items[0] {
            assert!((i.x_offset).abs() < 0.01);
        }
        // Text shifted by 10
        if let LineItem::Text(ref r) = lines[0].items[1] {
            assert!((r.x_offset - 10.0).abs() < 0.01);
        }
        // After image at 10 (before width) + 36 (text width) = 46
        if let LineItem::Image(ref i) = lines[0].items[2] {
            assert!(
                (i.x_offset - 46.0).abs() < 0.01,
                "after x_offset={}",
                i.x_offset,
            );
        }
    }

    #[test]
    fn test_build_inline_pseudo_image_returns_some_for_inline_pseudo() {
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .expect("icon.png fixture must exist");
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("icon.png"); width: 24px; height: 24px; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.expect("::before");
        let pseudo = doc.get_node(before_id).unwrap();

        // Inline pseudos have display: inline by default (not block)
        assert!(
            !super::pseudo::is_block_pseudo(pseudo),
            "pseudo should be inline by default"
        );

        let img = super::pseudo::build_inline_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle));
        assert!(img.is_some(), "should return Some for inline pseudo");
        let img = img.unwrap();
        // 24 CSS px × 0.75 = 18 pt
        assert_eq!(img.width, 18.0);
        assert_eq!(img.height, 18.0);
    }

    #[test]
    fn test_build_inline_pseudo_image_does_not_filter_display() {
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .expect("icon.png fixture must exist");
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("icon.png"); display: block; width: 24px; height: 24px; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let h1_id = find_h1(&doc);
        let before_id = doc.get_node(h1_id).unwrap().before.expect("::before");
        let pseudo = doc.get_node(before_id).unwrap();

        assert!(
            super::pseudo::is_block_pseudo(pseudo),
            "pseudo with display:block should be block"
        );

        // The inline builder should NOT produce an image for block pseudos
        // (the caller filters with !is_block_pseudo, but we verify the function
        // itself still returns Some — the filtering is done at the call site)
        // Here we verify the function works, the call-site filter is tested
        // by the integration test above.
        let img = super::pseudo::build_inline_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle));
        // build_inline_pseudo_image doesn't check display, so this will be Some.
        // The call site filters with !is_block_pseudo.
        assert!(
            img.is_some(),
            "build_inline_pseudo_image itself doesn't filter display"
        );
    }

    #[test]
    fn test_convert_content_url_normal_element() {
        // A normal element with `content: url(...)` + explicit width/height
        // should produce an ImagePageable, replacing its text children.
        let icon_bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("examples/image/icon.png"),
        )
        .unwrap();
        let mut bundle = AssetBundle::new();
        bundle.add_image("icon.png", icon_bytes);

        let html = r#"<!doctype html><html><head><style>
            .replaced { content: url("icon.png"); width: 24px; height: 24px; }
        </style></head><body><div class="replaced">This text should be replaced</div></body></html>"#;

        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.iter().any(|(w, h)| *w == 18.0 && *h == 18.0),
            "expected an 18x18 pt ImagePageable (24 CSS px × 0.75) from content: url(), got {:?}",
            images
        );
    }

    #[test]
    fn test_convert_content_url_no_content_falls_through() {
        // A normal div without content: url() should NOT produce an ImagePageable.
        let html = r#"<!doctype html><html><head><style>
            div { width: 100px; height: 50px; background: red; }
        </style></head><body><div>Normal text</div></body></html>"#;

        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.is_empty(),
            "normal div without content: url() should not produce images, got {:?}",
            images
        );
    }

    #[test]
    fn test_convert_content_url_missing_asset_falls_through() {
        // content: url("missing.png") where the asset is not in the bundle
        // should silently fall through to the normal conversion path.
        let bundle = AssetBundle::new(); // empty bundle

        let html = r#"<!doctype html><html><head><style>
            .replaced { content: url("missing.png"); width: 24px; height: 24px; }
        </style></head><body><div class="replaced">fallback text</div></body></html>"#;

        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: Some(&bundle),
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.is_empty(),
            "missing asset should not produce images, got {:?}",
            images
        );
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
    fn find_tag(doc: &blitz_html::HtmlDocument, tag: &str) -> Option<usize> {
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
            ConvertContext {
                running_store: &$store,
                assets: None,
                font_cache: HashMap::new(),
                string_set_by_node: HashMap::new(),
                counter_ops_by_node: HashMap::new(),
                bookmark_by_node: HashMap::new(),
                column_styles: crate::column_css::ColumnStyleTable::new(),
                multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
                link_cache: Default::default(),
            }
        }};
    }

    #[test]
    fn paragraph_attaches_external_link_to_glyph_run_inside_anchor() {
        let html =
            r#"<html><body><p>Go to <a href="https://example.com">example</a>.</p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        let mut found_external = false;
        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    if let Some(ls) = &run.link {
                        if let LinkTarget::External(u) = &ls.target {
                            if u.as_str() == "https://example.com" {
                                found_external = true;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            found_external,
            "expected at least one glyph run under <a> to carry an External link"
        );
    }

    #[test]
    fn paragraph_attaches_internal_link_for_fragment_href() {
        let html = r##"<html><body><p>See <a href="#intro">intro</a></p></body></html>"##;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        let mut found = false;
        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    if let Some(ls) = &run.link {
                        if let LinkTarget::Internal(frag) = &ls.target {
                            if frag.as_str() == "intro" {
                                found = true;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            found,
            "expected fragment link to produce LinkTarget::Internal(\"intro\")"
        );
    }

    #[test]
    fn paragraph_shares_arc_linkspan_across_glyph_runs_under_same_anchor() {
        // <em> forces two separate glyph runs (different style) under one <a>.
        let html =
            r#"<html><body><p><a href="https://x.test"><em>foo</em> bar</a></p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        let mut links: Vec<Arc<LinkSpan>> = Vec::new();
        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    if let Some(ls) = &run.link {
                        links.push(Arc::clone(ls));
                    }
                }
            }
        }
        assert!(
            links.len() >= 2,
            "expected at least two linked glyph runs (got {})",
            links.len()
        );
        let first = &links[0];
        for other in &links[1..] {
            assert!(
                Arc::ptr_eq(first, other),
                "all glyph runs inside the same <a> must share one Arc<LinkSpan>"
            );
        }
    }

    #[test]
    fn paragraph_leaves_link_none_for_anchor_without_href() {
        let html = r#"<html><body><p>Text <a>no href</a> here.</p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    assert!(
                        run.link.is_none(),
                        "glyph runs under <a> without href must have link: None"
                    );
                }
            }
        }
    }

    #[test]
    fn paragraph_leaves_link_none_for_anchor_with_empty_href() {
        let html = r#"<html><body><p><a href="">empty</a></p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    assert!(
                        run.link.is_none(),
                        "glyph runs under <a href=\"\"> must have link: None"
                    );
                }
            }
        }
    }

    #[test]
    fn paragraph_linkspan_alt_text_uses_anchor_text_content() {
        let html = r#"<html><body><p><a href="https://x.test">hello world</a></p></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 400.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);

        let store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = make_ctx!(store);
        let p_id = find_tag(&doc, "p").expect("p exists");
        let p_node = doc.get_node(p_id).expect("p node");
        let para = extract_paragraph(doc.deref(), p_node, &mut ctx, 0).expect("paragraph");

        let mut alt: Option<String> = None;
        for line in &para.lines {
            for item in &line.items {
                if let LineItem::Text(run) = item {
                    if let Some(ls) = &run.link {
                        alt = ls.alt_text.clone();
                    }
                }
            }
        }
        assert_eq!(alt.as_deref(), Some("hello world"));
    }

    // ---- inside marker tests ----

    /// Walk a Pageable tree and check whether any ParagraphPageable's first line
    /// has a Text item whose text starts with the given marker string.
    fn find_marker_text_in_tree(p: &dyn Pageable, marker: &str) -> bool {
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

    #[test]
    fn inside_marker_on_block_child_li() {
        let html = r#"<html><body><ul style="list-style-position:inside"><li><p>hello</p></li></ul></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        assert!(
            find_marker_text_in_tree(&*tree, "\u{2022}"),
            "inside marker bullet should be injected into <li><p>hello</p></li>"
        );
    }

    #[test]
    fn inside_marker_on_empty_li() {
        let html =
            r#"<html><body><ul style="list-style-position:inside"><li></li></ul></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        // Empty <li> with no AssetBundle fonts: marker may not render if no
        // system font covers the bullet. We still verify no panic occurs.
        // When a system font IS available, the marker should be present.
        let _found = find_marker_text_in_tree(&*tree, "\u{2022}");
        // Not asserting found==true because system font availability varies.
    }

    #[test]
    fn inside_marker_on_block_child_ol() {
        let html = r#"<html><body><ol style="list-style-position:inside"><li><p>hello</p></li></ol></body></html>"#;
        let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
        crate::blitz_adapter::resolve(&mut doc);
        let running_store = crate::gcpm::running::RunningElementStore::new();
        let mut ctx = ConvertContext {
            running_store: &running_store,
            assets: None,
            font_cache: HashMap::new(),
            string_set_by_node: HashMap::new(),
            counter_ops_by_node: HashMap::new(),
            bookmark_by_node: HashMap::new(),
            column_styles: crate::column_css::ColumnStyleTable::new(),
            multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
            link_cache: Default::default(),
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        assert!(
            find_marker_text_in_tree(&*tree, "1."),
            "inside marker '1.' should be injected into <li><p>hello</p></li> in <ol>"
        );
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

#[cfg(test)]
mod inline_box_extraction_tests {
    use crate::engine::Engine;
    use crate::pageable::{BlockPageable, Pageable, PositionedChild};
    use crate::paragraph::{LineItem, ParagraphPageable};

    fn find_paragraph(root: &dyn Pageable) -> Option<&ParagraphPageable> {
        if let Some(p) = root.as_any().downcast_ref::<ParagraphPageable>() {
            return Some(p);
        }
        if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
            for PositionedChild { child, .. } in &block.children {
                if let Some(p) = find_paragraph(child.as_ref()) {
                    return Some(p);
                }
            }
        }
        None
    }

    fn build_tree(html: &str) -> Box<dyn Pageable> {
        Engine::builder()
            .build()
            .build_pageable_for_testing_no_gcpm(html)
    }

    #[test]
    fn inline_block_becomes_line_item_inline_box() {
        let html = r#"<!DOCTYPE html><html><body><p>before <span style="display:inline-block;width:40px;height:20px;background:red"></span> after</p></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");

        let found = para
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .find(|it| matches!(it, LineItem::InlineBox(_)));
        assert!(
            found.is_some(),
            "inline-block should appear as LineItem::InlineBox"
        );

        // Value assertions: the extracted InlineBox must carry the CSS
        // sizes (40px × 20px → 30pt × 15pt), be visible at full opacity,
        // and sit at a non-zero x offset because it comes after "before ".
        let ib = match found.unwrap() {
            LineItem::InlineBox(ib) => ib,
            _ => unreachable!(),
        };
        let expected_w = super::px_to_pt(40.0);
        let expected_h = super::px_to_pt(20.0);
        assert!(
            (ib.width - expected_w).abs() < 0.5,
            "width: expected ~{expected_w}pt, got {}pt",
            ib.width
        );
        assert!(
            (ib.height - expected_h).abs() < 0.5,
            "height: expected ~{expected_h}pt, got {}pt",
            ib.height
        );
        assert_eq!(ib.opacity, 1.0, "opacity should default to 1.0");
        assert!(ib.visible, "InlineBox should be visible by default");
        assert!(
            ib.x_offset > 0.0,
            "x_offset should be non-zero (text precedes the inline-block), got {}",
            ib.x_offset
        );
    }

    #[test]
    fn inline_block_with_block_child_has_block_content() {
        // Note: `<p>` cannot contain `<div>` in HTML5 (auto-closes). Use a
        // `<div>` inline root so the parser keeps the block-child shape.
        let html = r#"<!DOCTYPE html><html><body><div>text <span style="display:inline-block;width:40px;height:20px"><div>inner</div></span> more</div></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");

        // Locate the line containing the InlineBox and the box itself,
        // so we can also assert the line-relative `computed_y` invariant.
        let (line, ib) = para
            .lines
            .iter()
            .find_map(|l| {
                l.items.iter().find_map(|it| match it {
                    LineItem::InlineBox(ib) => Some((l, ib)),
                    _ => None,
                })
            })
            .expect("InlineBox expected");
        assert!(
            ib.content
                .as_any()
                .downcast_ref::<BlockPageable>()
                .is_some(),
            "inline-block content should surface as BlockPageable"
        );

        // `computed_y` is line-relative. It may be negative for
        // baseline-aligned inline-blocks: an empty inline-block has its
        // baseline at its bottom edge (CSS 2.1 §10.8), so a box taller
        // than the line's ascent legitimately extends above line-top.
        // The invariant we can assert without rejecting that case is
        // that the box overlaps the line box — bottom below line-top,
        // top above line-bottom. That still catches "paragraph-relative
        // leak" on a multi-line paragraph (y would push the box out of
        // the first line entirely) and unconverted Parley values.
        assert!(
            ib.computed_y + ib.height > 0.0 && ib.computed_y < line.height,
            "computed_y should place the box overlapping the line, got y={} h={} line.height={}",
            ib.computed_y,
            ib.height,
            line.height
        );
    }

    #[test]
    fn inline_block_with_transform_preserves_wrapper() {
        // Addresses the CodeRabbit "wrapper semantics drop" finding that
        // prompted the `Box<dyn Pageable>` refactor of `InlineBoxContent`:
        // an inline-block with a CSS `transform` is wrapped by `convert_node`
        // in `TransformWrapperPageable`, and now that wrapper survives at
        // the top of `ib.content` (previously it was peeled and the
        // transform effect lost).
        let html = r#"<!DOCTYPE html><html><body><div>text <span style="display:inline-block;transform:rotate(2deg);width:40px;height:20px;background:red">x</span> more</div></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");
        let ib = para
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .find_map(|it| match it {
                LineItem::InlineBox(ib) => Some(ib),
                _ => None,
            })
            .expect("inline-block should appear as LineItem::InlineBox");
        assert!(
            ib.content
                .as_any()
                .downcast_ref::<crate::pageable::TransformWrapperPageable>()
                .is_some(),
            "transform should survive as TransformWrapperPageable at the top \
             of the inline-box content"
        );
    }

    #[test]
    fn inline_block_inner_id_is_registered_with_destination_registry() {
        use crate::pageable::DestinationRegistry;
        // A `<span id="target">` placed as an inline-block inside a paragraph
        // must still register with the destination registry so that
        // `href="#target"` links can resolve. Before Fix 2 to
        // `ParagraphPageable::collect_ids`, the registry walk stopped at the
        // paragraph and ignored nested inline-box content.
        let html = r#"<!DOCTYPE html><html><body><div>before <span id="target" style="display:inline-block;width:40px;height:20px;background:red">x</span> after</div></body></html>"#;
        let tree = build_tree(html);
        let mut reg = DestinationRegistry::default();
        tree.collect_ids(0.0, 0.0, 400.0, 600.0, &mut reg);
        assert!(
            reg.get("target").is_some(),
            "inline-block inner id should be registered with DestinationRegistry"
        );
    }

    #[test]
    fn inline_block_baseline_aligns_with_surrounding_text() {
        // An inline-block with text "boxed" inside, surrounded by "before" /
        // "after" text. Per CSS 2.1 §10.8.1, the inline-block's baseline is
        // the baseline of its last inner line, which should coincide with
        // the baseline of the surrounding text line.
        let html = r#"<!DOCTYPE html><html><body><div>before <span style="display:inline-block;padding:6px 10px;border:2px solid #333;background:#def">boxed</span> after</div></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");

        // Locate the inline-box and the line it sits on.
        let (ib, line) = para
            .lines
            .iter()
            .find_map(|l| {
                l.items.iter().find_map(|it| match it {
                    LineItem::InlineBox(ib) => Some((ib, l)),
                    _ => None,
                })
            })
            .expect("InlineBox expected");

        // Compute the inner baseline of the inline-box (offset from its
        // top edge).
        let inner_baseline = crate::paragraph::inline_box_baseline_offset(ib.content.as_ref())
            .expect("inline-box with visible text should have an inner baseline");

        // Fixture places the inline-box on the first (and only) line, so
        // `line_top = 0`. `ib.computed_y` is line-relative, and
        // `line.baseline` is paragraph-relative; with `line_top = 0` they
        // share the same origin, so we can compare directly.
        let line_top = 0.0_f32;
        let box_inner_baseline_abs = line_top + ib.computed_y + inner_baseline;
        let expected = line.baseline;
        let delta = (box_inner_baseline_abs - expected).abs();
        assert!(
            delta < 0.5,
            "inline-block inner baseline {} should align with surrounding line baseline {} (delta={})",
            box_inner_baseline_abs,
            expected,
            delta
        );
    }
}
