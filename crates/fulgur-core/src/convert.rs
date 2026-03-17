//! Convert a Blitz DOM (after style resolution + layout) into a Pageable tree.

use crate::pageable::{BlockPageable, Pageable, SpacerPageable};
use blitz_dom::NodeData;
use blitz_html::HtmlDocument;
use std::ops::Deref;

/// Convert a resolved Blitz document into a Pageable tree.
///
/// This is the initial, minimal implementation that treats every block-level
/// element as a BlockPageable and every leaf node as a SpacerPageable with
/// the height from Taffy's layout.
pub fn dom_to_pageable(doc: &HtmlDocument) -> Box<dyn Pageable> {
    let root = doc.root_element();
    convert_node(doc.deref(), root.id)
}

fn convert_node(doc: &blitz_dom::BaseDocument, node_id: usize) -> Box<dyn Pageable> {
    let node = doc.get_node(node_id).unwrap();
    let layout = node.final_layout;
    let height = layout.size.height;
    let width = layout.size.width;

    let children: &[usize] = &node.children;

    if children.is_empty() {
        // Leaf node — create a spacer with the computed height
        let mut spacer = SpacerPageable::new(height);
        spacer.wrap(width, height);
        return Box::new(spacer);
    }

    // Container node — recurse into children
    let child_pageables: Vec<Box<dyn Pageable>> = children
        .iter()
        .filter_map(|&child_id| {
            let child = doc.get_node(child_id)?;
            // Skip comment nodes
            if matches!(&child.data, NodeData::Comment) {
                return None;
            }
            Some(convert_node(doc, child_id))
        })
        .collect();

    let mut block = BlockPageable::new(child_pageables);
    block.wrap(width, 10000.0);
    Box::new(block)
}
