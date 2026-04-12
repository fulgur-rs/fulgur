//! Convert a Blitz DOM (after style resolution + layout) into a Pageable tree.

use crate::asset::AssetBundle;
use crate::gcpm::CounterOp;
use crate::gcpm::running::RunningElementStore;
use crate::image::ImagePageable;
use crate::pageable::{
    BackgroundLayer, BgBox, BgClip, BgLengthPercentage, BgRepeat, BgSize, BlockPageable,
    BlockStyle, BorderStyleValue, CounterOpMarkerPageable, CounterOpWrapperPageable, ImageMarker,
    ListItemMarker, ListItemPageable, Pageable, PositionedChild, RunningElementMarkerPageable,
    RunningElementWrapperPageable, Size, SpacerPageable, StringSetPageable,
    StringSetWrapperPageable, TablePageable,
};
use crate::paragraph::{
    ParagraphPageable, ShapedGlyph, ShapedGlyphRun, ShapedLine, TextDecoration, TextDecorationLine,
    TextDecorationStyle,
};
use crate::svg::SvgPageable;
use blitz_dom::{Node, NodeData};
use blitz_html::HtmlDocument;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

use crate::MAX_DOM_DEPTH;

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
    let layout = node.final_layout;
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
        layout.location.x,
        layout.location.y,
        layout.size.width,
        layout.size.height,
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
    let result = maybe_prepend_string_set(node_id, result, ctx);
    maybe_prepend_counter_ops(node_id, result, ctx)
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
    let node = doc.get_node(node_id).unwrap();
    let layout = node.final_layout;
    let height = layout.size.height;
    let width = layout.size.width;

    // Check if this is a list item with an outside marker (must be before inline root check)
    if let Some(elem_data) = node.element_data()
        && elem_data.list_item_data.is_some()
    {
        let (marker_lines, marker_width, marker_line_height) = extract_marker_lines(doc, node, ctx);
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);

        // Try list-style-image first; fall back to text marker if unresolved.
        let marker = resolve_list_marker(node, marker_line_height, ctx.assets).unwrap_or(
            ListItemMarker::Text {
                lines: marker_lines,
                width: marker_width,
            },
        );

        // Build body WITHOUT opacity — ListItemPageable wraps everything in
        // a single opacity group. But DO propagate visibility to the body's
        // own content (paragraph/image), since those are synthetic children
        // representing the node's own content, not real CSS children.
        let body: Box<dyn Pageable> = if node.flags.is_inline_root()
            && let Some(paragraph) = extract_paragraph(doc, node, ctx)
        {
            if style.has_visual_style() {
                let (child_x, child_y) = style.content_inset();
                let mut p = paragraph;
                p.visible = visible;
                let child = PositionedChild {
                    child: Box::new(p),
                    x: child_x,
                    y: child_y,
                };
                let mut block = BlockPageable::with_positioned_children(vec![child])
                    .with_style(style)
                    .with_visible(visible);
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                Box::new(block)
            } else {
                let mut p = paragraph;
                p.visible = visible;
                Box::new(p)
            }
        } else {
            let children: &[usize] = &node.children;
            let positioned_children = collect_positioned_children(doc, children, ctx, depth);
            let mut block = BlockPageable::with_positioned_children(positioned_children)
                .with_style(style)
                .with_visible(visible);
            block.wrap(width, 10000.0);
            Box::new(block)
        };
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

    // Check if this is a table element
    if let Some(elem_data) = node.element_data() {
        let tag = elem_data.name.local.as_ref();
        if tag == "table" {
            return convert_table(doc, node, ctx, depth);
        }
        if tag == "img" {
            if let Some(img) = convert_image(node, ctx.assets) {
                return img;
            }
            // Fall through to generic handling below to preserve Taffy-computed dimensions
        }
        if tag == "svg" {
            if let Some(svg) = convert_svg(node, ctx.assets) {
                return svg;
            }
            // Fall through — e.g., ImageData::None (parse failure upstream)
        }
    }

    // Check if this is an inline root (contains text layout)
    if node.flags.is_inline_root()
        && let Some(paragraph) = extract_paragraph(doc, node, ctx)
    {
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);
        if style.has_visual_style() {
            let (child_x, child_y) = style.content_inset();
            // Propagate visibility to the inner paragraph — it's not a real CSS child
            // but the node's own text content, so it must respect the node's visibility.
            // Do NOT propagate opacity — the wrapping block handles it via push_opacity.
            let mut p = paragraph;
            p.visible = visible;
            let child = PositionedChild {
                child: Box::new(p),
                x: child_x,
                y: child_y,
            };
            let mut block = BlockPageable::with_positioned_children(vec![child])
                .with_style(style)
                .with_opacity(opacity)
                .with_visible(visible);
            block.wrap(width, height);
            // Use Taffy's computed height (includes padding + border) instead of children-only height
            block.layout_size = Some(Size { width, height });
            return Box::new(block);
        }
        let mut p = paragraph;
        p.opacity = opacity;
        p.visible = visible;
        return Box::new(p);
    }

    let children: &[usize] = &node.children;

    if children.is_empty() {
        let style = extract_block_style(node, ctx.assets);
        if style.has_visual_style() || style.has_radius() {
            let (opacity, visible) = extract_opacity_visible(node);
            let mut block = BlockPageable::with_positioned_children(vec![])
                .with_style(style)
                .with_opacity(opacity)
                .with_visible(visible);
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
    let positioned_children = collect_positioned_children(doc, children, ctx, depth);

    let style = extract_block_style(node, ctx.assets);
    let has_style = style.has_visual_style() || style.has_radius();
    let (opacity, visible) = extract_opacity_visible(node);
    let mut block = BlockPageable::with_positioned_children(positioned_children)
        .with_style(style)
        .with_opacity(opacity)
        .with_visible(visible);
    block.wrap(width, 10000.0);
    if has_style {
        block.layout_size = Some(Size { width, height });
    }
    Box::new(block)
}

/// Collect positioned children, flattening zero-size pass-through containers
/// (like thead, tbody, tr) so their children appear directly in the parent.
///
/// Running element markers discovered on zero-size nodes are buffered and
/// attached to the next real child via `RunningElementWrapperPageable`. This
/// keeps the marker with its associated content when pagination pushes the
/// content to the next page.
fn collect_positioned_children(
    doc: &blitz_dom::BaseDocument,
    child_ids: &[usize],
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Vec<PositionedChild> {
    if depth >= MAX_DOM_DEPTH {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut pending_running_markers: Vec<RunningElementMarkerPageable> = Vec::new();

    for &child_id in child_ids {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };

        if matches!(&child_node.data, NodeData::Comment) {
            continue;
        }
        if is_non_visual_element(child_node) {
            continue;
        }

        let child_layout = child_node.final_layout;

        // Zero-size leaf nodes (whitespace text, etc.) — skip, but first
        // harvest any string-set entries so `string-set: name attr(...)` on
        // an empty element still propagates into the page tree.
        if child_layout.size.height == 0.0
            && child_layout.size.width == 0.0
            && child_node.children.is_empty()
        {
            emit_orphan_string_set_markers(
                child_id,
                child_layout.location.x,
                child_layout.location.y,
                ctx,
                &mut result,
            );
            emit_counter_op_markers(
                child_id,
                child_layout.location.x,
                child_layout.location.y,
                ctx,
                &mut result,
            );
            if let Some(marker) = take_running_marker(child_id, ctx) {
                pending_running_markers.push(marker);
            }
            continue;
        }

        // Zero-size container (thead, tbody, tr, etc.) — flatten children
        // into the parent. Harvest the container's own string-set entries
        // before recursing so they aren't dropped.
        if child_layout.size.height == 0.0
            && child_layout.size.width == 0.0
            && !child_node.children.is_empty()
        {
            emit_orphan_string_set_markers(
                child_id,
                child_layout.location.x,
                child_layout.location.y,
                ctx,
                &mut result,
            );
            emit_counter_op_markers(
                child_id,
                child_layout.location.x,
                child_layout.location.y,
                ctx,
                &mut result,
            );
            if let Some(marker) = take_running_marker(child_id, ctx) {
                pending_running_markers.push(marker);
            }
            let mut nested = collect_positioned_children(doc, &child_node.children, ctx, depth + 1);
            // Flush pending running markers to the first real nested child so
            // they travel with the flattened content on page break. Without
            // this, the markers would skip over the container's children and
            // incorrectly attach to the next outer sibling.
            if !pending_running_markers.is_empty()
                && let Some(first) = nested.first_mut()
            {
                let original = std::mem::replace(
                    &mut first.child,
                    // Temporary placeholder; overwritten below.
                    Box::new(SpacerPageable::new(0.0)),
                );
                first.child = Box::new(RunningElementWrapperPageable::new(
                    std::mem::take(&mut pending_running_markers),
                    original,
                ));
            }
            result.extend(nested);
            continue;
        }

        let mut child_pageable = convert_node(doc, child_id, ctx, depth + 1);
        if !pending_running_markers.is_empty() {
            child_pageable = Box::new(RunningElementWrapperPageable::new(
                std::mem::take(&mut pending_running_markers),
                child_pageable,
            ));
        }
        result.push(PositionedChild {
            child: child_pageable,
            x: child_layout.location.x,
            y: child_layout.location.y,
        });
    }

    // Running markers with no subsequent real child — emit as bare
    // PositionedChild fallback so they aren't lost entirely. This covers the
    // edge case of a running element at the very end of a parent.
    for marker in pending_running_markers {
        result.push(PositionedChild {
            child: Box::new(marker),
            x: 0.0,
            y: 0.0,
        });
    }

    result
}

use crate::blitz_adapter::{extract_inline_svg_tree, get_attr};

/// Wrap an atomic replaced element (image, svg) in a styled `BlockPageable`
/// when the node has visual styling, or return the inner Pageable directly.
///
/// `build_inner` is invoked once with the dimensions and the opacity/visibility
/// values that should be applied to the inner element. In the styled branch
/// the inner receives `opacity = 1.0` (the wrapping block handles opacity)
/// and the dimensions are the content-box, not the border-box. In the unstyled
/// branch the inner receives the node's own opacity/visibility and full size.
fn wrap_replaced_in_block_style<F>(
    node: &Node,
    assets: Option<&AssetBundle>,
    build_inner: F,
) -> Box<dyn Pageable>
where
    F: FnOnce(f32, f32, f32, bool) -> Box<dyn Pageable>,
{
    let layout = node.final_layout;
    let width = layout.size.width;
    let height = layout.size.height;

    let style = extract_block_style(node, assets);
    let (opacity, visible) = extract_opacity_visible(node);

    if style.has_visual_style() {
        let (cx, cy) = style.content_inset();
        // content_inset returns (left, top); compute right/bottom insets for content-box
        let right_inset = style.border_widths[1] + style.padding[1];
        let bottom_inset = style.border_widths[2] + style.padding[2];
        let content_width = (width - cx - right_inset).max(0.0);
        let content_height = (height - cy - bottom_inset).max(0.0);
        // Inner element receives visibility (it IS the node's own content) but
        // NOT opacity — the wrapping block handles opacity once for the whole
        // border-box, otherwise the border would also be faded.
        let inner = build_inner(content_width, content_height, 1.0, visible);
        let child = PositionedChild {
            child: inner,
            x: cx,
            y: cy,
        };
        let mut block = BlockPageable::with_positioned_children(vec![child])
            .with_style(style)
            .with_opacity(opacity)
            .with_visible(visible);
        block.wrap(width, height);
        block.layout_size = Some(Size { width, height });
        Box::new(block)
    } else {
        build_inner(width, height, opacity, visible)
    }
}

/// Convert an `<img>` element into an `ImagePageable`, wrapped in `BlockPageable` if styled.
fn convert_image(node: &Node, assets: Option<&AssetBundle>) -> Option<Box<dyn Pageable>> {
    let elem = node.element_data()?;
    let src = get_attr(elem, "src")?;
    let bundle = assets?;
    let data = Arc::clone(bundle.get_image(src)?);
    let format = ImagePageable::detect_format(&data)?;

    Some(wrap_replaced_in_block_style(
        node,
        assets,
        move |w, h, opacity, visible| {
            let mut img = ImagePageable::new(data, format, w, h);
            img.opacity = opacity;
            img.visible = visible;
            Box::new(img)
        },
    ))
}

/// Convert an inline `<svg>` element into an `SvgPageable`, wrapped in `BlockPageable` if styled.
///
/// Blitz parses the inline SVG into a `usvg::Tree` during DOM construction;
/// `blitz_adapter::extract_inline_svg_tree` retrieves it without exposing
/// blitz-internal types here.
fn convert_svg(node: &Node, assets: Option<&AssetBundle>) -> Option<Box<dyn Pageable>> {
    let elem = node.element_data()?;
    let tree = extract_inline_svg_tree(elem)?;

    Some(wrap_replaced_in_block_style(
        node,
        assets,
        move |w, h, opacity, visible| {
            let mut svg = SvgPageable::new(tree, w, h);
            svg.opacity = opacity;
            svg.visible = visible;
            Box::new(svg)
        },
    ))
}

/// Convert a table element into a TablePageable with header/body cell groups.
fn convert_table(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    let layout = node.final_layout;
    let width = layout.size.width;
    let height = layout.size.height;
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
        let layout = node.final_layout;
        let out: &mut Vec<PositionedChild> = if is_header { header_cells } else { body_cells };
        emit_counter_op_markers(node_id, layout.location.x, layout.location.y, ctx, out);
    }

    for &child_id in &node.children {
        let Some(child_node) = doc.get_node(child_id) else {
            continue;
        };
        if matches!(&child_node.data, NodeData::Comment) {
            continue;
        }
        if is_non_visual_element(child_node) {
            continue;
        }

        let child_layout = child_node.final_layout;

        // Zero-size container (tr, thead, tbody) — recurse into children
        if child_layout.size.height == 0.0
            && child_layout.size.width == 0.0
            && !child_node.children.is_empty()
        {
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
        if child_layout.size.height == 0.0 && child_layout.size.width == 0.0 {
            continue;
        }

        // Actual cell (td/th) — convert and add to appropriate group
        let cell_pageable = convert_node(doc, child_id, ctx, depth + 1);
        let positioned = PositionedChild {
            child: cell_pageable,
            x: child_layout.location.x,
            y: child_layout.location.y,
        };

        if is_header {
            header_cells.push(positioned);
        } else {
            body_cells.push(positioned);
        }
    }
}

/// Extract a ParagraphPageable from an inline root node.
fn extract_paragraph(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
) -> Option<ParagraphPageable> {
    let elem_data = node.element_data()?;
    let text_layout = elem_data.inline_layout_data.as_ref()?;

    let parley_layout = &text_layout.layout;
    let text = &text_layout.text;

    let mut shaped_lines = Vec::new();

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        let mut glyph_runs = Vec::new();

        for item in line.items() {
            if let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                let run = glyph_run.run();
                let font_ref = run.font();
                let font_index = font_ref.index;
                let font_arc = ctx.get_or_insert_font(font_ref);
                let font_size = run.font_size();

                // Get text color from the brush (node ID) → computed styles
                let brush = &glyph_run.style().brush;
                let color = get_text_color(doc, brush.id);
                let decoration = get_text_decoration(doc, brush.id);

                // Extract raw glyphs (relative offsets, not absolute positions)
                let text_len = text.len();
                let mut glyphs = Vec::new();
                for g in glyph_run.glyphs() {
                    glyphs.push(ShapedGlyph {
                        id: g.id,
                        x_advance: g.advance / font_size,
                        x_offset: g.x / font_size,
                        y_offset: g.y / font_size,
                        text_range: 0..text_len,
                    });
                }

                if !glyphs.is_empty() {
                    let run_text = text.clone();

                    glyph_runs.push(ShapedGlyphRun {
                        font_data: font_arc,
                        font_index,
                        font_size,
                        color,
                        decoration,
                        glyphs,
                        text: run_text,
                        x_offset: glyph_run.offset(),
                    });
                }
            }
        }

        shaped_lines.push(ShapedLine {
            height: metrics.line_height,
            baseline: metrics.baseline,
            glyph_runs,
        });
    }

    if shaped_lines.is_empty() {
        return None;
    }

    Some(ParagraphPageable::new(shaped_lines))
}

/// Extract visual style (background, borders, padding, background-image) from a node.
fn extract_block_style(node: &Node, assets: Option<&AssetBundle>) -> BlockStyle {
    let layout = node.final_layout;
    let mut style = BlockStyle {
        border_widths: [
            layout.border.top,
            layout.border.right,
            layout.border.bottom,
            layout.border.left,
        ],
        padding: [
            layout.padding.top,
            layout.padding.right,
            layout.padding.bottom,
            layout.padding.left,
        ],
        ..Default::default()
    };

    // Extract colors from computed styles
    if let Some(styles) = node.primary_styles() {
        let current_color = styles.clone_color();

        // Background color — access the computed value directly
        let bg = styles.clone_background_color();
        let bg_abs = bg.resolve_to_absolute(&current_color);
        let r = (bg_abs.components.0.clamp(0.0, 1.0) * 255.0) as u8;
        let g = (bg_abs.components.1.clamp(0.0, 1.0) * 255.0) as u8;
        let b = (bg_abs.components.2.clamp(0.0, 1.0) * 255.0) as u8;
        let a = (bg_abs.alpha.clamp(0.0, 1.0) * 255.0) as u8;
        if a > 0 {
            style.background_color = Some([r, g, b, a]);
        }

        // Border color (use top border color for all sides for simplicity)
        let bc = styles.clone_border_top_color();
        let bc_abs = bc.resolve_to_absolute(&current_color);
        style.border_color = [
            (bc_abs.components.0.clamp(0.0, 1.0) * 255.0) as u8,
            (bc_abs.components.1.clamp(0.0, 1.0) * 255.0) as u8,
            (bc_abs.components.2.clamp(0.0, 1.0) * 255.0) as u8,
            (bc_abs.alpha.clamp(0.0, 1.0) * 255.0) as u8,
        ];

        // Border radii
        let width = layout.size.width;
        let height = layout.size.height;
        let resolve_radius =
            |r: &style::values::computed::length_percentage::NonNegativeLengthPercentage,
             basis: f32|
             -> f32 {
                r.0.resolve(style::values::computed::Length::new(basis))
                    .px()
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

        // Background image layers
        if let Some(assets) = assets {
            let bg_images = styles.clone_background_image();
            let bg_sizes = styles.clone_background_size();
            let bg_pos_x = styles.clone_background_position_x();
            let bg_pos_y = styles.clone_background_position_y();
            let bg_repeats = styles.clone_background_repeat();
            let bg_origins = styles.clone_background_origin();
            let bg_clips = styles.clone_background_clip();

            for (i, image) in bg_images.0.iter().enumerate() {
                use style::values::computed::image::Image;
                if let Image::Url(url) = image {
                    let raw_src = match url {
                        style::servo::url::ComputedUrl::Valid(u) => u.as_str(),
                        style::servo::url::ComputedUrl::Invalid(s) => s.as_str(),
                    };
                    // Stylo resolves URLs to absolute (e.g. "file:///bg.png").
                    // Extract the path/filename for AssetBundle lookup.
                    let src = extract_asset_name(raw_src);
                    if let Some(data) = assets.get_image(src) {
                        if let Some(format) = ImagePageable::detect_format(data) {
                            let (iw, ih) =
                                ImagePageable::decode_dimensions(data, format).unwrap_or((1, 1));

                            let size = convert_bg_size(&bg_sizes.0, i);
                            let (px, py) = convert_bg_position(&bg_pos_x.0, &bg_pos_y.0, i);
                            let (rx, ry) = convert_bg_repeat(&bg_repeats.0, i);
                            let origin = convert_bg_origin(&bg_origins.0, i);
                            let clip = convert_bg_clip(&bg_clips.0, i);

                            style.background_layers.push(BackgroundLayer {
                                image_data: Arc::clone(data),
                                format,
                                intrinsic_width: iw as f32,
                                intrinsic_height: ih as f32,
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
fn convert_lp_to_bg(lp: &style::values::computed::LengthPercentage) -> BgLengthPercentage {
    if let Some(pct) = lp.to_percentage() {
        BgLengthPercentage::Percentage(pct.0)
    } else {
        BgLengthPercentage::Length(lp.to_length().map(|l| l.px()).unwrap_or(0.0))
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

/// Resolve a list-style-image marker from the node's computed styles.
///
/// Returns `Some(ListItemMarker::Image { ... })` when the node's
/// `list-style-image` is a URL that resolves to a supported image
/// (PNG/JPEG/GIF or SVG) inside `ctx.assets`. Returns `None` for any
/// failure (no bundle, URL not found, unknown format, parse error),
/// and the caller must then fall back to the text marker produced by
/// `extract_marker_lines` — matching CSS spec fallback semantics.
fn resolve_list_marker(
    node: &Node,
    line_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<ListItemMarker> {
    use crate::image::AssetKind;
    use style::values::computed::image::Image;

    let assets = assets?;
    let styles = node.primary_styles()?;
    let image = styles.clone_list_style_image();
    let url = match image {
        Image::Url(u) => u,
        _ => return None,
    };
    let raw_src = match &url {
        style::servo::url::ComputedUrl::Valid(u) => u.as_str(),
        style::servo::url::ComputedUrl::Invalid(s) => s.as_str(),
    };
    let src = extract_asset_name(raw_src);
    let data = assets.get_image(src)?;
    match AssetKind::detect(data) {
        AssetKind::Raster(format) => {
            let (iw, ih) = ImagePageable::decode_dimensions(data, format)?;
            // px → pt (1px = 0.75pt)
            let intrinsic_w = iw as f32 * 0.75;
            let intrinsic_h = ih as f32 * 0.75;
            let (width, height) =
                crate::pageable::clamp_marker_size(intrinsic_w, intrinsic_h, line_height);
            let img = ImagePageable::new(Arc::clone(data), format, width, height);
            Some(ListItemMarker::Image {
                marker: ImageMarker::Raster(img),
                width,
                height,
            })
        }
        AssetKind::Svg => {
            let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
            let size = tree.size();
            // SVG user units = CSS px → PDF pt (1px = 0.75pt)
            let intrinsic_w = size.width() * 0.75;
            let intrinsic_h = size.height() * 0.75;
            let (width, height) =
                crate::pageable::clamp_marker_size(intrinsic_w, intrinsic_h, line_height);
            let svg = SvgPageable::new(Arc::new(tree), width, height);
            Some(ListItemMarker::Image {
                marker: ImageMarker::Svg(svg),
                width,
                height,
            })
        }
        AssetKind::Unknown => None,
    }
}

/// Extract shaped lines from a list marker's Parley layout.
fn extract_marker_lines(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
) -> (Vec<ShapedLine>, f32, f32) {
    let elem_data = match node.element_data() {
        Some(d) => d,
        None => return (Vec::new(), 0.0, 0.0),
    };
    let list_item_data = match &elem_data.list_item_data {
        Some(d) => d,
        None => return (Vec::new(), 0.0, 0.0),
    };
    let parley_layout = match &list_item_data.position {
        blitz_dom::node::ListItemLayoutPosition::Outside(layout) => layout,
        blitz_dom::node::ListItemLayoutPosition::Inside => return (Vec::new(), 0.0, 0.0),
    };

    let marker_text = match &list_item_data.marker {
        blitz_dom::node::Marker::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).to_string()
        }
        blitz_dom::node::Marker::String(s) => s.clone(),
    };

    let mut shaped_lines = Vec::new();
    let mut max_width: f32 = 0.0;
    let mut line_height_pt: f32 = 0.0;

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        if line_height_pt == 0.0 {
            line_height_pt = metrics.line_height;
        }
        let mut glyph_runs = Vec::new();
        let mut line_width: f32 = 0.0;

        for item in line.items() {
            if let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                let run = glyph_run.run();
                let font_ref = run.font();
                let font_index = font_ref.index;
                let font_arc = ctx.get_or_insert_font(font_ref);
                let font_size = run.font_size();

                let brush = &glyph_run.style().brush;
                let color = get_text_color(doc, brush.id);

                let text_len = marker_text.len();
                let mut glyphs = Vec::new();
                for g in glyph_run.glyphs() {
                    line_width += g.advance;
                    glyphs.push(ShapedGlyph {
                        id: g.id,
                        x_advance: g.advance / font_size,
                        x_offset: g.x / font_size,
                        y_offset: g.y / font_size,
                        text_range: 0..text_len,
                    });
                }

                if !glyphs.is_empty() {
                    glyph_runs.push(ShapedGlyphRun {
                        font_data: font_arc,
                        font_index,
                        font_size,
                        color,
                        decoration: Default::default(),
                        glyphs,
                        text: marker_text.clone(),
                        x_offset: glyph_run.offset(),
                    });
                }
            }
        }

        max_width = max_width.max(line_width);
        shaped_lines.push(ShapedLine {
            height: metrics.line_height,
            baseline: metrics.baseline,
            glyph_runs,
        });
    }

    (shaped_lines, max_width, line_height_pt)
}

/// Get text color from a DOM node's computed styles.
fn get_text_color(doc: &blitz_dom::BaseDocument, node_id: usize) -> [u8; 4] {
    if let Some(node) = doc.get_node(node_id)
        && let Some(styles) = node.primary_styles()
    {
        let color = styles.clone_color();
        let r = (color.components.0.clamp(0.0, 1.0) * 255.0) as u8;
        let g = (color.components.1.clamp(0.0, 1.0) * 255.0) as u8;
        let b = (color.components.2.clamp(0.0, 1.0) * 255.0) as u8;
        let a = (color.alpha.clamp(0.0, 1.0) * 255.0) as u8;
        return [r, g, b, a];
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
        let resolved = deco_color.resolve_to_absolute(&current_color);
        let color = [
            (resolved.components.0.clamp(0.0, 1.0) * 255.0) as u8,
            (resolved.components.1.clamp(0.0, 1.0) * 255.0) as u8,
            (resolved.components.2.clamp(0.0, 1.0) * 255.0) as u8,
            (resolved.alpha.clamp(0.0, 1.0) * 255.0) as u8,
        ];

        return TextDecoration { line, style, color };
    }
    TextDecoration::default()
}
