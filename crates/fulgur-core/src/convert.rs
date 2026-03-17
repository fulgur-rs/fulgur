//! Convert a Blitz DOM (after style resolution + layout) into a Pageable tree.

use crate::pageable::{BlockPageable, BlockStyle, Pageable, PositionedChild, SpacerPageable};
use crate::paragraph::{ParagraphPageable, ShapedGlyph, ShapedGlyphRun, ShapedLine};
use blitz_dom::{Node, NodeData};
use blitz_html::HtmlDocument;
use std::ops::Deref;
use std::sync::Arc;

/// Convert a resolved Blitz document into a Pageable tree.
pub fn dom_to_pageable(doc: &HtmlDocument) -> Box<dyn Pageable> {
    let root = doc.root_element();
    // Debug: print layout tree structure
    if std::env::var("FULGUR_DEBUG").is_ok() {
        debug_print_tree(doc.deref(), root.id, 0);
    }
    convert_node(doc.deref(), root.id)
}

fn debug_print_tree(doc: &blitz_dom::BaseDocument, node_id: usize, depth: usize) {
    let Some(node) = doc.get_node(node_id) else { return };
    let layout = node.final_layout;
    let indent = "  ".repeat(depth);
    let tag = match &node.data {
        NodeData::Element(e) => e.name.local.to_string(),
        NodeData::Text(_) => "#text".to_string(),
        NodeData::Comment => "#comment".to_string(),
        _ => "#other".to_string(),
    };
    eprintln!("{indent}{tag} id={} pos=({},{}) size={}x{} inline_root={}",
        node_id, layout.location.x, layout.location.y,
        layout.size.width, layout.size.height, node.flags.is_inline_root());
    for &child_id in &node.children {
        debug_print_tree(doc, child_id, depth + 1);
    }
}

fn convert_node(doc: &blitz_dom::BaseDocument, node_id: usize) -> Box<dyn Pageable> {
    let node = doc.get_node(node_id).unwrap();
    let layout = node.final_layout;
    let height = layout.size.height;
    let width = layout.size.width;

    // Check if this is an inline root (contains text layout)
    if node.flags.is_inline_root() {
        if let Some(paragraph) = extract_paragraph(doc, node) {
            // Wrap in a BlockPageable to apply background/border/padding styles
            let style = extract_block_style(node);
            let has_style = style.background_color.is_some()
                || style.border_widths.iter().any(|&w| w > 0.0)
                || style.padding.iter().any(|&p| p > 0.0);
            if has_style {
                let child = PositionedChild { child: Box::new(paragraph), x: 0.0, y: 0.0 };
                let mut block = BlockPageable::with_positioned_children(vec![child]).with_style(style);
                block.wrap(width, height);
                return Box::new(block);
            }
            return Box::new(paragraph);
        }
    }

    let children: &[usize] = &node.children;

    if children.is_empty() {
        // Leaf node — create a spacer with the computed height
        let mut spacer = SpacerPageable::new(height);
        spacer.wrap(width, height);
        return Box::new(spacer);
    }

    // Container node — collect children with Taffy-computed positions
    let positioned_children = collect_positioned_children(doc, children);

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
) -> Vec<PositionedChild> {
    let mut result = Vec::new();
    for &child_id in child_ids {
        let Some(child_node) = doc.get_node(child_id) else { continue };

        if matches!(&child_node.data, NodeData::Comment) {
            continue;
        }
        if is_non_visual_element(child_node) {
            continue;
        }

        let child_layout = child_node.final_layout;

        // Zero-size leaf nodes (whitespace text, etc.) — skip
        if child_layout.size.height == 0.0 && child_layout.size.width == 0.0
            && child_node.children.is_empty()
        {
            continue;
        }

        // Zero-size container (thead, tbody, tr, etc.) — flatten children into parent
        if child_layout.size.height == 0.0 && child_layout.size.width == 0.0
            && !child_node.children.is_empty()
        {
            let nested = collect_positioned_children(doc, &child_node.children);
            result.extend(nested);
            continue;
        }

        let child_pageable = convert_node(doc, child_id);
        result.push(PositionedChild {
            child: child_pageable,
            x: child_layout.location.x,
            y: child_layout.location.y,
        });
    }
    result
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
    let mut style = BlockStyle::default();

    // Read border widths and padding from Taffy layout
    style.border_widths = [
        layout.border.top,
        layout.border.right,
        layout.border.bottom,
        layout.border.left,
    ];
    style.padding = [
        layout.padding.top,
        layout.padding.right,
        layout.padding.bottom,
        layout.padding.left,
    ];

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
    }

    style
}

/// Check if a node is a non-visual element (head, script, style, etc.)
fn is_non_visual_element(node: &Node) -> bool {
    if let Some(elem) = node.element_data() {
        let tag = elem.name.local.as_ref();
        matches!(tag, "head" | "script" | "style" | "link" | "meta" | "title" | "noscript")
    } else {
        false
    }
}

/// Get text color from a DOM node's computed styles.
fn get_text_color(doc: &blitz_dom::BaseDocument, node_id: usize) -> [u8; 4] {
    if let Some(node) = doc.get_node(node_id) {
        if let Some(styles) = node.primary_styles() {
            let color = styles.clone_color();
            let r = (color.components.0.clamp(0.0, 1.0) * 255.0) as u8;
            let g = (color.components.1.clamp(0.0, 1.0) * 255.0) as u8;
            let b = (color.components.2.clamp(0.0, 1.0) * 255.0) as u8;
            let a = (color.alpha.clamp(0.0, 1.0) * 255.0) as u8;
            return [r, g, b, a];
        }
    }
    [0, 0, 0, 255] // Default: black
}
