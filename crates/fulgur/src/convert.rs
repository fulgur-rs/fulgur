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
    InlineImage, LineFontMetrics, LineItem, ParagraphPageable, ShapedGlyph, ShapedGlyphRun,
    ShapedLine, TextDecoration, TextDecorationLine, TextDecorationStyle,
};
use crate::svg::SvgPageable;
use blitz_dom::{Node, NodeData};
use blitz_html::HtmlDocument;
use skrifa::MetadataProvider;
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
    let result = maybe_prepend_counter_ops(node_id, result, ctx);
    maybe_wrap_transform(doc, node_id, result)
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
    let layout = node.final_layout;
    match crate::blitz_adapter::compute_transform(&styles, layout.size.width, layout.size.height) {
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
        let content_box = compute_content_box(node, &style);
        let body: Box<dyn Pageable> = if node.flags.is_inline_root() {
            let paragraph_opt = extract_paragraph(doc, node, ctx);

            // Inline pseudo images for list item body
            let before_inline = node
                .before
                .and_then(|id| doc.get_node(id))
                .filter(|p| !is_block_pseudo(p))
                .and_then(|p| {
                    build_inline_pseudo_image(p, content_box.width, content_box.height, ctx.assets)
                });
            let after_inline = node
                .after
                .and_then(|id| doc.get_node(id))
                .filter(|p| !is_block_pseudo(p))
                .and_then(|p| {
                    build_inline_pseudo_image(p, content_box.width, content_box.height, ctx.assets)
                });

            if let Some(mut paragraph) = paragraph_opt {
                if before_inline.is_some() || after_inline.is_some() {
                    inject_inline_pseudo_images(&mut paragraph.lines, before_inline, after_inline);
                    recalculate_paragraph_line_boxes(&mut paragraph.lines);
                    paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
                }

                let (before_pseudo, after_pseudo) =
                    build_block_pseudo_images(doc, node, content_box, ctx.assets);
                let has_pseudo = before_pseudo.is_some() || after_pseudo.is_some();
                if style.needs_block_wrapper() || has_pseudo {
                    let (child_x, child_y) = style.content_inset();
                    let mut p = paragraph;
                    p.visible = visible;
                    let paragraph_children = vec![PositionedChild {
                        child: Box::new(p),
                        x: child_x,
                        y: child_y,
                    }];
                    let children = wrap_with_block_pseudo_images(
                        before_pseudo,
                        after_pseudo,
                        content_box,
                        paragraph_children,
                    );
                    let mut block = BlockPageable::with_positioned_children(children)
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
            } else if before_inline.is_some() || after_inline.is_some() {
                // Synthesize a minimal paragraph for pseudo-only list items
                let mut line = ShapedLine {
                    height: 0.0,
                    baseline: 0.0,
                    items: vec![],
                };
                inject_inline_pseudo_images(
                    std::slice::from_mut(&mut line),
                    before_inline,
                    after_inline,
                );
                let font_metrics = metrics_from_line(&line);
                crate::paragraph::recalculate_line_box(&mut line, &font_metrics);
                let mut paragraph = ParagraphPageable::new(vec![line]);
                paragraph.visible = visible;

                let (before_pseudo, after_pseudo) =
                    build_block_pseudo_images(doc, node, content_box, ctx.assets);
                let has_pseudo = before_pseudo.is_some() || after_pseudo.is_some();
                if style.needs_block_wrapper() || has_pseudo {
                    let (child_x, child_y) = style.content_inset();
                    let paragraph_children = vec![PositionedChild {
                        child: Box::new(paragraph),
                        x: child_x,
                        y: child_y,
                    }];
                    let children = wrap_with_block_pseudo_images(
                        before_pseudo,
                        after_pseudo,
                        content_box,
                        paragraph_children,
                    );
                    let mut block = BlockPageable::with_positioned_children(children)
                        .with_style(style)
                        .with_visible(visible);
                    block.wrap(width, height);
                    block.layout_size = Some(Size { width, height });
                    Box::new(block)
                } else {
                    Box::new(paragraph)
                }
            } else {
                // Inline root with no text and no inline pseudo images —
                // fall through to the non-inline-root path below.
                let children: &[usize] = &node.children;
                let positioned_children = collect_positioned_children(doc, children, ctx, depth);
                let (before_pseudo, after_pseudo) =
                    build_block_pseudo_images(doc, node, content_box, ctx.assets);
                let positioned_children = wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    positioned_children,
                );
                let mut block = BlockPageable::with_positioned_children(positioned_children)
                    .with_style(style)
                    .with_visible(visible);
                block.wrap(width, 10000.0);
                Box::new(block)
            }
        } else {
            let children: &[usize] = &node.children;
            let positioned_children = collect_positioned_children(doc, children, ctx, depth);
            let (before_pseudo, after_pseudo) =
                build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let positioned_children = wrap_with_block_pseudo_images(
                before_pseudo,
                after_pseudo,
                content_box,
                positioned_children,
            );
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

    // CSS `content: url(...)` on a normal element replaces its children with
    // the image (CSS Content L3 §2). Blitz 0.2.4 does not materialise this
    // in layout, so we read the computed value and build an ImagePageable.
    // Early return skips pseudo-element processing (spec-correct: replaced
    // elements do not generate ::before/::after).
    if let Some(img) = convert_content_url(node, ctx.assets) {
        return img;
    }

    // Check if this is an inline root (contains text layout)
    if node.flags.is_inline_root() {
        let paragraph_opt = extract_paragraph(doc, node, ctx);
        let style = extract_block_style(node, ctx.assets);
        let (opacity, visible) = extract_opacity_visible(node);
        let content_box = compute_content_box(node, &style);

        // Inline pseudo images (display: inline is the CSS default for pseudos)
        let before_inline = node
            .before
            .and_then(|id| doc.get_node(id))
            .filter(|p| !is_block_pseudo(p))
            .and_then(|p| {
                build_inline_pseudo_image(p, content_box.width, content_box.height, ctx.assets)
            });
        let after_inline = node
            .after
            .and_then(|id| doc.get_node(id))
            .filter(|p| !is_block_pseudo(p))
            .and_then(|p| {
                build_inline_pseudo_image(p, content_box.width, content_box.height, ctx.assets)
            });

        if let Some(mut paragraph) = paragraph_opt {
            // Existing path: inject inline pseudo images into real paragraph
            if before_inline.is_some() || after_inline.is_some() {
                inject_inline_pseudo_images(&mut paragraph.lines, before_inline, after_inline);
                recalculate_paragraph_line_boxes(&mut paragraph.lines);
                paragraph.cached_height = paragraph.lines.iter().map(|l| l.height).sum();
            }

            // Then existing block pseudo check
            let (before_pseudo, after_pseudo) =
                build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let has_pseudo = before_pseudo.is_some() || after_pseudo.is_some();
            if style.needs_block_wrapper() || has_pseudo {
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
                let children = wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    paragraph_children,
                );
                let mut block = BlockPageable::with_positioned_children(children)
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
        } else if before_inline.is_some() || after_inline.is_some() {
            // Synthesize a minimal paragraph for pseudo-only elements (e.g.
            // `<span class="icon"></span>` with `::before { content: url(...) }`)
            let mut line = ShapedLine {
                height: 0.0,
                baseline: 0.0,
                items: vec![],
            };
            inject_inline_pseudo_images(
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
                build_block_pseudo_images(doc, node, content_box, ctx.assets);
            let has_pseudo = before_pseudo.is_some() || after_pseudo.is_some();
            if style.needs_block_wrapper() || has_pseudo {
                let (child_x, child_y) = style.content_inset();
                let paragraph_children = vec![PositionedChild {
                    child: Box::new(paragraph),
                    x: child_x,
                    y: child_y,
                }];
                let children = wrap_with_block_pseudo_images(
                    before_pseudo,
                    after_pseudo,
                    content_box,
                    paragraph_children,
                );
                let mut block = BlockPageable::with_positioned_children(children)
                    .with_style(style)
                    .with_opacity(opacity)
                    .with_visible(visible);
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                return Box::new(block);
            }
            return Box::new(paragraph);
        }
        // Fall through: inline root with no text and no inline pseudo images
    }

    let children: &[usize] = &node.children;

    if children.is_empty() {
        let style = extract_block_style(node, ctx.assets);
        let content_box = compute_content_box(node, &style);
        // Check for pseudo images even on childless elements — e.g.
        // `<div class="icon"></div>` with `.icon::before { content: url(...) }`
        // should emit the image. Without this the pseudo is silently dropped.
        let (before_pseudo, after_pseudo) =
            build_block_pseudo_images(doc, node, content_box, ctx.assets);
        let has_pseudo = before_pseudo.is_some() || after_pseudo.is_some();
        if style.needs_block_wrapper() || has_pseudo {
            let (opacity, visible) = extract_opacity_visible(node);
            let positioned_children =
                wrap_with_block_pseudo_images(before_pseudo, after_pseudo, content_box, Vec::new());
            let mut block = BlockPageable::with_positioned_children(positioned_children)
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
    let content_box = compute_content_box(node, &style);
    let (before_pseudo, after_pseudo) =
        build_block_pseudo_images(doc, node, content_box, ctx.assets);
    let positioned_children = wrap_with_block_pseudo_images(
        before_pseudo,
        after_pseudo,
        content_box,
        positioned_children,
    );

    let has_style = style.needs_block_wrapper();
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
        //
        // Exception: if the 0x0 leaf has a block pseudo image, fall through
        // to `convert_node` so `convert_node_inner`'s `children.is_empty()`
        // branch can emit it. Without this, `<span class="icon"></span>`
        // + `span::before { content: url(...); display: block }` silently
        // drops the image even though the empty-children branch is wired up.
        if child_layout.size.height == 0.0
            && child_layout.size.width == 0.0
            && child_node.children.is_empty()
            && !node_has_block_pseudo_image(doc, child_node)
            && !node_has_inline_pseudo_image(doc, child_node)
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

/// Shared sizing / construction for `ImagePageable`, used by both the `<img>`
/// element path and the `::before`/`::after` `content: url()` pseudo path.
///
/// Sizing rules match the CSS replaced-element spec:
///
/// - both css dims given → use them verbatim
/// - one given → scale the other by the image's intrinsic aspect ratio
/// - neither given → use intrinsic pixel dimensions (treated as 1px = 1pt
///   since `ImagePageable` draws in PDF points; this matches the existing
///   `<img>` behavior when Taffy has nothing to resolve the size from)
///
/// The intrinsic dimensions come from `ImagePageable::decode_dimensions`.
/// A zero-height decode result silently degrades to a 1:1 aspect so width-only
/// sizing does not produce NaN.
/// Resolve CSS width/height against intrinsic image dimensions + aspect ratio.
fn resolve_image_dimensions(
    data: &[u8],
    format: crate::image::ImageFormat,
    css_w: Option<f32>,
    css_h: Option<f32>,
) -> (f32, f32) {
    let (iw, ih) = ImagePageable::decode_dimensions(data, format).unwrap_or((1, 1));
    let iw = iw as f32;
    let ih = ih as f32;
    let aspect = if ih > 0.0 { iw / ih } else { 1.0 };
    match (css_w, css_h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, if aspect > 0.0 { w / aspect } else { w }),
        (None, Some(h)) => (h * aspect, h),
        (None, None) => (iw, ih),
    }
}

fn make_image_pageable(
    data: Arc<Vec<u8>>,
    format: crate::image::ImageFormat,
    css_w: Option<f32>,
    css_h: Option<f32>,
    opacity: f32,
    visible: bool,
) -> ImagePageable {
    let (w, h) = resolve_image_dimensions(&data, format, css_w, css_h);
    let mut img = ImagePageable::new(data, format, w, h);
    img.opacity = opacity;
    img.visible = visible;
    img
}

/// Build an `ImagePageable` for a `::before`/`::after` pseudo-element node
/// whose computed `content` resolves to a single `url(...)` image.
///
/// Returns `None` if:
///
/// - `assets` is `None`
/// - the pseudo's computed content is not a single image URL
/// - the URL cannot be resolved in the `AssetBundle` (silent skip — matches
///   background-image handling in `extract_block_style`)
/// - the image format is unsupported by `ImagePageable::detect_format`
///
/// `parent_content_width` / `parent_content_height` are the content-box
/// dimensions of the pseudo's containing block, used to resolve percentage
/// `width` / `height` on the pseudo itself. Passing the values separately
/// (instead of a single `parent_size`) ensures `height: 50%` resolves
/// against the parent height, not the parent width — which was the bug
/// flagged by coderabbit in PR #70.
fn build_pseudo_image(
    pseudo_node: &Node,
    parent_content_width: f32,
    parent_content_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<ImagePageable> {
    let assets = assets?;

    let raw_url = crate::blitz_adapter::extract_content_image_url(pseudo_node)?;
    let asset_name = extract_asset_name(&raw_url);
    let data = Arc::clone(assets.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    // Read computed CSS width / height on the pseudo-element itself. Blitz
    // does not propagate these to `final_layout` for pseudos that lack a text
    // child, so we must go directly to stylo.
    let styles = pseudo_node.primary_styles()?;
    let css_w = resolve_pseudo_size(&styles.clone_width(), parent_content_width);
    let css_h = resolve_pseudo_size(&styles.clone_height(), parent_content_height);

    let (opacity, visible) = extract_opacity_visible(pseudo_node);
    Some(make_image_pageable(
        data, format, css_w, css_h, opacity, visible,
    ))
}

/// True iff the pseudo-element has `display: block` outside.
///
/// Phase 1 only emits pseudo images for block-outside pseudos. Inline pseudos
/// fall through to Phase 2 work (tracked separately) where the image has to
/// be injected into `ParagraphPageable`'s line layout.
fn is_block_pseudo(pseudo: &Node) -> bool {
    use style::values::specified::box_::DisplayOutside;
    pseudo
        .primary_styles()
        .is_some_and(|s| s.clone_display().outside() == DisplayOutside::Block)
}

/// Cheap probe: does `node` have at least one `::before` / `::after` pseudo
/// slot whose computed `content` resolves to a block-display image URL?
///
/// Used by `collect_positioned_children` to opt zero-sized leaves (e.g.
/// `<span class="icon"></span>`) out of its zero-size skip, so the leaf can
/// reach `convert_node_inner`'s `children.is_empty()` branch and emit its
/// pseudo image. Does not resolve the AssetBundle or decode the image — if
/// the asset is missing, `build_block_pseudo_images` later silently skips,
/// which is harmless but slightly wasteful; that trade-off is fine because
/// zero-size elements with `content: url()` are rare.
fn node_has_block_pseudo_image(doc: &blitz_dom::BaseDocument, node: &Node) -> bool {
    for pseudo_id in [node.before, node.after].into_iter().flatten() {
        if let Some(pseudo) = doc.get_node(pseudo_id)
            && is_block_pseudo(pseudo)
            && crate::blitz_adapter::extract_content_image_url(pseudo).is_some()
        {
            return true;
        }
    }
    false
}

/// Returns `true` if `node` has a `::before` or `::after` pseudo-element that
/// is an inline (non-block) pseudo with a `content: url(...)` image.
///
/// Used by the zero-size leaf filter to let elements like
/// `<span class="icon"></span>` with `::before { content: url(...) }` through
/// to `convert_node_inner` where the inline pseudo path can synthesize a
/// `ParagraphPageable`.
fn node_has_inline_pseudo_image(doc: &blitz_dom::BaseDocument, node: &Node) -> bool {
    for pseudo_id in [node.before, node.after].into_iter().flatten() {
        if let Some(pseudo) = doc.get_node(pseudo_id)
            && !is_block_pseudo(pseudo)
            && crate::blitz_adapter::extract_content_image_url(pseudo).is_some()
        {
            return true;
        }
    }
    false
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
    let border_w = node.final_layout.size.width;
    let border_h = node.final_layout.size.height;
    ContentBox {
        origin_x: left_inset,
        origin_y: top_inset,
        width: (border_w - left_inset - right_inset).max(0.0),
        height: (border_h - top_inset - bottom_inset).max(0.0),
    }
}

/// Build `ImagePageable` instances for `::before` and `::after` pseudos on
/// `parent` when their `content` resolves to a single `url(...)` image and
/// their `display` is block-outside. Returns `(before, after)`, either of
/// which may be `None`.
///
/// This is the single walk of the pseudo slots — callers use it both to
/// decide whether to take the `BlockPageable` wrapping path in the
/// inline-root branch and to materialize the children to inject.
///
/// Pseudo sizes resolve against the parent's content-box (`parent_cb.width`
/// for `width`, `parent_cb.height` for `height`), so `width: 50%` and
/// `height: 100%` behave per spec.
///
/// **Known limitation (fulgur-ai3 Phase 1):** Because Blitz assigns a
/// zero-sized layout to text-less pseudo elements, the pseudo image does not
/// push subsequent real children down. Authors can work around this by
/// adding `margin-top` / `margin-bottom` on the first / last real child to
/// reserve space. Properly pushing content will be handled in a follow-up
/// issue that round-trips the synthetic pseudo size through Taffy.
fn build_block_pseudo_images(
    doc: &blitz_dom::BaseDocument,
    parent: &Node,
    parent_cb: ContentBox,
    assets: Option<&AssetBundle>,
) -> (Option<ImagePageable>, Option<ImagePageable>) {
    if assets.is_none() {
        return (None, None);
    }
    let load = |pseudo_id: Option<usize>| -> Option<ImagePageable> {
        let pseudo = doc.get_node(pseudo_id?)?;
        if !is_block_pseudo(pseudo) {
            return None;
        }
        build_pseudo_image(pseudo, parent_cb.width, parent_cb.height, assets)
    };
    (load(parent.before), load(parent.after))
}

/// Prepend / append block pseudo images around `children`. `::before` lands
/// at the content-box top-left `(origin_x, origin_y)` and `::after` at the
/// content-box bottom-left `(origin_x, origin_y + height)`.
///
/// This returns a new vec instead of mutating in place so `::before` does
/// not trigger an O(n) shift on large child lists.
fn wrap_with_block_pseudo_images(
    before: Option<ImagePageable>,
    after: Option<ImagePageable>,
    parent_cb: ContentBox,
    children: Vec<PositionedChild>,
) -> Vec<PositionedChild> {
    let mut out = Vec::with_capacity(children.len() + 2);
    if let Some(img) = before {
        out.push(PositionedChild {
            child: Box::new(img),
            x: parent_cb.origin_x,
            y: parent_cb.origin_y,
        });
    }
    out.extend(children);
    if let Some(img) = after {
        out.push(PositionedChild {
            child: Box::new(img),
            x: parent_cb.origin_x,
            y: parent_cb.origin_y + parent_cb.height,
        });
    }
    out
}

/// Build an `InlineImage` for a `::before`/`::after` pseudo-element whose
/// computed `content` resolves to a single `url(...)` image and whose
/// `display` is NOT block-outside (i.e. it is inline, the CSS default for
/// pseudo-elements).
///
/// Returns `None` under the same conditions as `build_pseudo_image`.
fn build_inline_pseudo_image(
    pseudo_node: &Node,
    parent_content_width: f32,
    parent_content_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<InlineImage> {
    let assets = assets?;
    let raw_url = crate::blitz_adapter::extract_content_image_url(pseudo_node)?;
    let asset_name = extract_asset_name(&raw_url);
    let data = Arc::clone(assets.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    let styles = pseudo_node.primary_styles()?;
    let css_w = resolve_pseudo_size(&styles.clone_width(), parent_content_width);
    let css_h = resolve_pseudo_size(&styles.clone_height(), parent_content_height);
    let (w, h) = resolve_image_dimensions(&data, format, css_w, css_h);
    let (opacity, visible) = extract_opacity_visible(pseudo_node);
    let vertical_align = crate::blitz_adapter::extract_vertical_align(pseudo_node);
    Some(InlineImage {
        data,
        format,
        width: w,
        height: h,
        x_offset: 0.0,
        vertical_align,
        opacity,
        visible,
        computed_y: 0.0,
    })
}

/// Inject an inline pseudo image at the start (::before) and/or end (::after)
/// of the shaped lines. The ::before image is prepended to the first line and
/// all existing items are shifted right by its width. The ::after image is
/// appended to the last line at the end of existing content.
fn inject_inline_pseudo_images(
    lines: &mut [ShapedLine],
    before: Option<InlineImage>,
    after: Option<InlineImage>,
) {
    if let Some(mut img) = before {
        if let Some(first_line) = lines.first_mut() {
            let shift = img.width;
            for item in &mut first_line.items {
                match item {
                    LineItem::Text(run) => run.x_offset += shift,
                    LineItem::Image(i) => i.x_offset += shift,
                }
            }
            img.x_offset = 0.0;
            first_line.items.insert(0, LineItem::Image(img));
        }
    }
    if let Some(mut img) = after {
        if let Some(last_line) = lines.last_mut() {
            let last_end = last_line
                .items
                .iter()
                .map(|item| match item {
                    LineItem::Text(run) => {
                        run.x_offset
                            + run
                                .glyphs
                                .iter()
                                .map(|g| g.x_advance * run.font_size)
                                .sum::<f32>()
                    }
                    LineItem::Image(i) => i.x_offset + i.width,
                })
                .fold(0.0_f32, f32::max);
            img.x_offset = last_end;
            last_line.items.push(LineItem::Image(img));
        }
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

/// Resolve a stylo `Size` (i.e. `width` / `height`) to an absolute `f32` in
/// pt, or `None` if the size is `auto` or one of the intrinsic keywords.
///
/// Percentages resolve against `parent_width` — the containing block width.
/// (Percentage heights on replaced elements technically reference the parent
/// height, but Phase 1 only cares about block-display pseudo icons whose
/// height is typically an explicit px value; using parent_width as the basis
/// for both dimensions is a conscious simplification.)
fn resolve_pseudo_size(size: &style::values::computed::Size, parent_width: f32) -> Option<f32> {
    use style::values::computed::Length;
    use style::values::generics::length::GenericSize;
    match size {
        GenericSize::LengthPercentage(lp) => {
            // NonNegativeLengthPercentage is a tuple struct with `.0` being
            // the inner LengthPercentage.
            Some(lp.0.resolve(Length::new(parent_width)).px())
        }
        // auto / min-content / max-content / fit-content / stretch etc. are
        // all treated as "not specified" here. The `make_image_pageable`
        // helper will fall back to intrinsic dimensions / aspect-ratio.
        _ => None,
    }
}

/// Convert a normal element whose computed `content` resolves to a single
/// `url(...)` image into an `ImagePageable`. Per CSS spec, `content` on a
/// normal element replaces the element's children — so we return early and
/// skip pseudo-element processing.
///
/// Returns `None` when the element has no `content: url()`, the asset is
/// missing, or the format is unsupported — callers fall through to the
/// standard conversion path.
fn convert_content_url(node: &Node, assets: Option<&AssetBundle>) -> Option<Box<dyn Pageable>> {
    let raw_url = crate::blitz_adapter::extract_content_image_url(node)?;
    let asset_name = extract_asset_name(&raw_url);
    let bundle = assets?;
    let data = Arc::clone(bundle.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    Some(wrap_replaced_in_block_style(
        node,
        assets,
        move |w, h, opacity, visible| {
            let img = make_image_pageable(data.clone(), format, Some(w), Some(h), opacity, visible);
            Box::new(img)
        },
    ))
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
            // `wrap_replaced_in_block_style` has already resolved (w, h) from
            // Taffy's final layout, so we pass them as explicit css_w/css_h.
            // The shared helper then applies the same `ImagePageable::new`
            // construction path as the pseudo-content url() case, keeping
            // sizing behavior byte-identical to the previous <img> path.
            let img = make_image_pageable(data.clone(), format, Some(w), Some(h), opacity, visible);
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
        let mut items = Vec::new();

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

                    items.push(LineItem::Text(ShapedGlyphRun {
                        font_data: font_arc,
                        font_index,
                        font_size,
                        color,
                        decoration,
                        glyphs,
                        text: run_text,
                        x_offset: glyph_run.offset(),
                    }));
                }
            }
        }

        shaped_lines.push(ShapedLine {
            height: metrics.line_height,
            baseline: metrics.baseline,
            items,
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
                        use crate::image::AssetKind;

                        // Resolve content + intrinsic size per asset kind.
                        let resolved: Option<(BgImageContent, f32, f32)> =
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

    // Zero or negative line-height (e.g. list-style-position: inside where
    // extract_marker_lines returns 0.0) would clamp image size to 0x0.
    // Return None so the caller falls back to the text marker instead of
    // creating an invisible image marker that suppresses the fallback.
    if line_height <= 0.0 {
        return None;
    }
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
        let mut items = Vec::new();
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
                    items.push(LineItem::Text(ShapedGlyphRun {
                        font_data: font_arc,
                        font_index,
                        font_size,
                        color,
                        decoration: Default::default(),
                        glyphs,
                        text: marker_text.clone(),
                        x_offset: glyph_run.offset(),
                    }));
                }
            }
        }

        max_width = max_width.max(line_width);
        shaped_lines.push(ShapedLine {
            height: metrics.line_height,
            baseline: metrics.baseline,
            items,
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
        let img = make_image_pageable(
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
        let img = make_image_pageable(
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
        let img = make_image_pageable(
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
        let img = make_image_pageable(sample_png_arc(), ImageFormat::Png, None, None, 0.5, false);
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
        let parent_layout = doc.get_node(h1_id).unwrap().final_layout.size;

        let img = build_pseudo_image(
            pseudo,
            parent_layout.width,
            parent_layout.height,
            Some(&bundle),
        )
        .expect("build_pseudo_image should return Some for content: url()");
        assert_eq!(img.width, 48.0);
        assert_eq!(img.height, 48.0);
    }

    #[test]
    fn test_build_pseudo_image_width_only_uses_intrinsic_aspect() {
        // icon.png is 32x32 so aspect = 1.0. width=20 → height=20.
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
        let parent_layout = doc.get_node(h1_id).unwrap().final_layout.size;

        let img = build_pseudo_image(
            pseudo,
            parent_layout.width,
            parent_layout.height,
            Some(&bundle),
        )
        .unwrap();
        assert_eq!(img.width, 20.0);
        assert_eq!(img.height, 20.0);
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
            build_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle)).is_none(),
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
        assert!(build_pseudo_image(pseudo, 800.0, 600.0, None).is_none());
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
        let img = build_pseudo_image(pseudo, 400.0, 200.0, Some(&bundle)).unwrap();
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
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.iter().any(|(w, h)| *w == 24.0 && *h == 24.0),
            "expected a 24x24 ImagePageable from ::before pseudo, got {:?}",
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
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.iter().any(|(w, h)| *w == 16.0 && *h == 16.0),
            "childless element ::before pseudo should emit a 16x16 image; got {:?}",
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
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        let mut images = Vec::new();
        walk_all_children(&*tree, &mut |p| collect_images(p, &mut images));
        assert!(
            images.iter().any(|(w, h)| *w == 18.0 && *h == 18.0),
            "zero-size block leaf with block pseudo should emit an 18x18 image; got {:?}",
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
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);
        let mut images = Vec::new();
        walk_all_children(&*tree, &mut |p| collect_images(p, &mut images));
        assert!(
            images.iter().any(|(w, h)| *w == 12.0 && *h == 12.0),
            "list item with text + block pseudo should emit a 12x12 image; got {:?}",
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
        inject_inline_pseudo_images(&mut lines, Some(img), None);

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
        inject_inline_pseudo_images(&mut lines, None, Some(img));

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
        inject_inline_pseudo_images(&mut lines, Some(before), Some(after));

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
            !is_block_pseudo(pseudo),
            "pseudo should be inline by default"
        );

        let img = build_inline_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle));
        assert!(img.is_some(), "should return Some for inline pseudo");
        let img = img.unwrap();
        assert_eq!(img.width, 24.0);
        assert_eq!(img.height, 24.0);
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
            is_block_pseudo(pseudo),
            "pseudo with display:block should be block"
        );

        // The inline builder should NOT produce an image for block pseudos
        // (the caller filters with !is_block_pseudo, but we verify the function
        // itself still returns Some — the filtering is done at the call site)
        // Here we verify the function works, the call-site filter is tested
        // by the integration test above.
        let img = build_inline_pseudo_image(pseudo, 800.0, 600.0, Some(&bundle));
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
        };
        let tree = super::dom_to_pageable(&doc, &mut ctx);

        let mut images = Vec::new();
        collect_images(&*tree, &mut images);
        assert!(
            images.iter().any(|(w, h)| *w == 24.0 && *h == 24.0),
            "expected a 24x24 ImagePageable from content: url() on normal element, got {:?}",
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
}
