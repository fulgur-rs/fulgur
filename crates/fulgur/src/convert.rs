//! Convert a Blitz DOM (after style resolution + layout) into a Pageable tree.

use crate::gcpm::GcpmContext;
use crate::gcpm::running::{RunningElementStore, serialize_node};
use crate::pageable::{
    BlockPageable, BlockStyle, ListItemPageable, Pageable, PositionedChild, Size, SpacerPageable,
};
use crate::paragraph::{
    ParagraphPageable, ShapedGlyph, ShapedGlyphRun, ShapedLine, TextDecoration, TextDecorationLine,
    TextDecorationStyle,
};
use blitz_dom::{Node, NodeData};
use blitz_html::HtmlDocument;
use std::ops::Deref;
use std::sync::Arc;

/// Convert a resolved Blitz document into a Pageable tree.
pub fn dom_to_pageable(
    doc: &HtmlDocument,
    gcpm: Option<&GcpmContext>,
    running_store: &mut RunningElementStore,
) -> Box<dyn Pageable> {
    let root = doc.root_element();
    // Debug: print layout tree structure
    if std::env::var("FULGUR_DEBUG").is_ok() {
        debug_print_tree(doc.deref(), root.id, 0);
    }
    convert_node(doc.deref(), root.id, gcpm, running_store)
}

fn debug_print_tree(doc: &blitz_dom::BaseDocument, node_id: usize, depth: usize) {
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
    gcpm: Option<&GcpmContext>,
    running_store: &mut RunningElementStore,
) -> Box<dyn Pageable> {
    let node = doc.get_node(node_id).unwrap();
    let layout = node.final_layout;
    let height = layout.size.height;
    let width = layout.size.width;

    // Check if this is a list item with an outside marker (must be before inline root check)
    if let Some(elem_data) = node.element_data()
        && elem_data.list_item_data.is_some()
    {
        let (marker_lines, marker_width) = extract_marker_lines(doc, node);
        let style = extract_block_style(node);

        // Build body: if inline root, use paragraph; otherwise collect block children
        let body: Box<dyn Pageable> = if node.flags.is_inline_root()
            && let Some(paragraph) = extract_paragraph(doc, node)
        {
            let has_style = style.background_color.is_some()
                || style.border_widths.iter().any(|&w| w > 0.0)
                || style.padding.iter().any(|&p| p > 0.0);
            if has_style {
                let child_x = style.border_widths[3] + style.padding[3];
                let child_y = style.border_widths[0] + style.padding[0];
                let child = PositionedChild {
                    child: Box::new(paragraph),
                    x: child_x,
                    y: child_y,
                };
                let mut block =
                    BlockPageable::with_positioned_children(vec![child]).with_style(style);
                block.wrap(width, height);
                block.layout_size = Some(Size { width, height });
                Box::new(block)
            } else {
                Box::new(paragraph)
            }
        } else {
            let children: &[usize] = &node.children;
            let positioned_children =
                collect_positioned_children(doc, children, gcpm, running_store);
            let mut block =
                BlockPageable::with_positioned_children(positioned_children).with_style(style);
            block.wrap(width, 10000.0);
            Box::new(block)
        };

        let mut item = ListItemPageable {
            marker_lines,
            marker_width,
            body,
            style: BlockStyle::default(),
            width,
            height: 0.0,
        };
        item.wrap(width, 10000.0);
        return Box::new(item);
    }

    // Check if this is an inline root (contains text layout)
    if node.flags.is_inline_root()
        && let Some(paragraph) = extract_paragraph(doc, node)
    {
        // Wrap in a BlockPageable to apply background/border/padding styles
        let style = extract_block_style(node);
        let has_style = style.background_color.is_some()
            || style.border_widths.iter().any(|&w| w > 0.0)
            || style.padding.iter().any(|&p| p > 0.0);
        if has_style {
            let child_x = style.border_widths[3] + style.padding[3]; // left border + left padding
            let child_y = style.border_widths[0] + style.padding[0]; // top border + top padding
            let child = PositionedChild {
                child: Box::new(paragraph),
                x: child_x,
                y: child_y,
            };
            let mut block = BlockPageable::with_positioned_children(vec![child]).with_style(style);
            block.wrap(width, height);
            // Use Taffy's computed height (includes padding + border) instead of children-only height
            block.layout_size = Some(Size { width, height });
            return Box::new(block);
        }
        return Box::new(paragraph);
    }

    let children: &[usize] = &node.children;

    if children.is_empty() {
        // Leaf node with style (background/border/radius) — use BlockPageable to draw visuals
        let style = extract_block_style(node);
        let has_style = style.background_color.is_some()
            || style.border_widths.iter().any(|&w| w > 0.0)
            || style.border_radii.iter().any(|r| r[0] > 0.0 || r[1] > 0.0);
        if has_style {
            let mut block = BlockPageable::with_positioned_children(vec![]).with_style(style);
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
    let positioned_children = collect_positioned_children(doc, children, gcpm, running_store);

    let style = extract_block_style(node);
    let mut block = BlockPageable::with_positioned_children(positioned_children).with_style(style);
    block.wrap(width, 10000.0);
    Box::new(block)
}

/// Collect positioned children, flattening zero-size pass-through containers
/// (like thead, tbody, tr) so their children appear directly in the parent.
fn collect_positioned_children(
    doc: &blitz_dom::BaseDocument,
    child_ids: &[usize],
    gcpm: Option<&GcpmContext>,
    running_store: &mut RunningElementStore,
) -> Vec<PositionedChild> {
    let mut result = Vec::new();
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

        // GCPM: skip running elements and store their HTML
        if let Some(ctx) = gcpm {
            if is_running_element(child_node, ctx) {
                let html = serialize_node(doc, child_id);
                if let Some(name) = get_running_name(child_node, ctx) {
                    running_store.register(name, html);
                }
                continue;
            }
        }

        let child_layout = child_node.final_layout;

        // Zero-size leaf nodes (whitespace text, etc.) — skip
        if child_layout.size.height == 0.0
            && child_layout.size.width == 0.0
            && child_node.children.is_empty()
        {
            continue;
        }

        // Zero-size container (thead, tbody, tr, etc.) — flatten children into parent
        if child_layout.size.height == 0.0
            && child_layout.size.width == 0.0
            && !child_node.children.is_empty()
        {
            let nested =
                collect_positioned_children(doc, &child_node.children, gcpm, running_store);
            result.extend(nested);
            continue;
        }

        let child_pageable = convert_node(doc, child_id, gcpm, running_store);
        result.push(PositionedChild {
            child: child_pageable,
            x: child_layout.location.x,
            y: child_layout.location.y,
        });
    }
    result
}

/// Check if a node is a running element.
/// Since the CSS preprocessor replaced `position: running(name)` with `display: none`,
/// we identify running elements by checking if they have a class matching a known running name.
/// Elements with `display: none` will typically have zero-size layout, but we rely on the
/// class name match against `running_names` from the GCPM context.
fn is_running_element(node: &Node, ctx: &GcpmContext) -> bool {
    if ctx.running_names.is_empty() {
        return false;
    }
    let Some(elem) = node.element_data() else {
        return false;
    };
    has_matching_running_name(elem, ctx)
}

fn get_class_attr(elem: &blitz_dom::node::ElementData) -> Option<&str> {
    elem.attrs()
        .iter()
        .find(|a| a.name.local.as_ref() == "class")
        .map(|a| a.value.as_ref())
}

fn has_matching_running_name(elem: &blitz_dom::node::ElementData, ctx: &GcpmContext) -> bool {
    let Some(class_attr) = get_class_attr(elem) else {
        return false;
    };
    class_attr
        .split_whitespace()
        .any(|cls| ctx.running_names.contains(cls))
}

fn get_running_name(node: &Node, ctx: &GcpmContext) -> Option<String> {
    let elem = node.element_data()?;
    let class_attr = get_class_attr(elem)?;
    class_attr
        .split_whitespace()
        .find(|cls| ctx.running_names.contains(*cls))
        .map(|s| s.to_string())
}

/// Extract a ParagraphPageable from an inline root node.
fn extract_paragraph(doc: &blitz_dom::BaseDocument, node: &Node) -> Option<ParagraphPageable> {
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
                let font_data = run.font();
                let font_bytes: Vec<u8> = font_data.data.data().to_vec();
                let font_index = font_data.index;
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
                        font_data: Arc::new(font_bytes),
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

/// Extract visual style (background, borders, padding) from a node.
fn extract_block_style(node: &Node) -> BlockStyle {
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
    }

    style
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

/// Extract shaped lines from a list marker's Parley layout.
fn extract_marker_lines(doc: &blitz_dom::BaseDocument, node: &Node) -> (Vec<ShapedLine>, f32) {
    let elem_data = match node.element_data() {
        Some(d) => d,
        None => return (Vec::new(), 0.0),
    };
    let list_item_data = match &elem_data.list_item_data {
        Some(d) => d,
        None => return (Vec::new(), 0.0),
    };
    let parley_layout = match &list_item_data.position {
        blitz_dom::node::ListItemLayoutPosition::Outside(layout) => layout,
        blitz_dom::node::ListItemLayoutPosition::Inside => return (Vec::new(), 0.0),
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

    for line in parley_layout.lines() {
        let metrics = line.metrics();
        let mut glyph_runs = Vec::new();
        let mut line_width: f32 = 0.0;

        for item in line.items() {
            if let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item {
                let run = glyph_run.run();
                let font_data = run.font();
                let font_bytes: Vec<u8> = font_data.data.data().to_vec();
                let font_index = font_data.index;
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
                        font_data: Arc::new(font_bytes),
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

    (shaped_lines, max_width)
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
