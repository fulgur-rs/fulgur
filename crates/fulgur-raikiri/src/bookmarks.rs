use fulgur_core::units::PX_TO_PT;
use krilla::{
    destination::XyzDestination,
    geom::Point,
    outline::{Outline, OutlineNode},
};
use raikiri_html::{
    ConsumerPropertyRegistration, DocumentLayout, DomView, FragmentKind, NodeId, Page,
    computed::ComputedVisibility,
};
use raikiri_traits::{
    ConsumerPropertyEvent, ConsumerPropertyObserver, ConsumerPropertyValue, NodeKind,
};
use std::collections::HashMap;

#[derive(Default)]
struct Heading {
    source_order: u32,
    level: Option<i32>,
    label: Option<String>,
}

#[derive(Default)]
pub(super) struct BookmarkCollector {
    headings: HashMap<NodeId, Heading>,
}

impl ConsumerPropertyObserver for BookmarkCollector {
    fn observe_event(&mut self, event: ConsumerPropertyEvent) -> std::io::Result<()> {
        let heading = self.headings.entry(event.node_id).or_default();
        heading.source_order = event.source_order;
        match (event.property_name.as_str(), event.value) {
            ("bookmark-level", ConsumerPropertyValue::Integer(level)) => {
                heading.level = Some(level)
            }
            ("bookmark-level", _) => heading.level = None,
            ("bookmark-label", ConsumerPropertyValue::Text(label)) => heading.label = Some(label),
            ("bookmark-label", _) => heading.label = None,
            _ => {}
        }
        Ok(())
    }
}

pub(super) fn registrations() -> Vec<ConsumerPropertyRegistration> {
    vec![
        ConsumerPropertyRegistration::integer_or_none("bookmark-level").non_inherited(),
        ConsumerPropertyRegistration::text("bookmark-label").non_inherited(),
    ]
}

pub(super) fn heading_stylesheet() -> &'static str {
    "h1,h2,h3,h4,h5,h6 {bookmark-label:content(text)}
     h1 {bookmark-level:1} h2 {bookmark-level:2} h3 {bookmark-level:3}
     h4 {bookmark-level:4} h5 {bookmark-level:5} h6 {bookmark-level:6}"
}

fn first_descendant(
    dom: DomView<'_>,
    node: NodeId,
    positions: &HashMap<NodeId, XyzDestination>,
) -> Option<XyzDestination> {
    let mut pending: Vec<_> = dom.children(node).collect();
    pending.reverse();
    while let Some(node) = pending.pop() {
        if let Some(position) = positions.get(&node) {
            return Some(position.clone());
        }
        let children: Vec<_> = dom.children(node).collect();
        pending.extend(children.into_iter().rev());
    }
    None
}

/// The document outline of a laid-out document.
pub(super) fn outline(document: &DocumentLayout, collector: BookmarkCollector) -> Outline {
    let mut builder = OutlineBuilder::default();
    for page in document.pages() {
        builder.add_page(&page);
    }
    if let Some(first_page) = document.page(0) {
        builder.add_headings(&first_page, collector);
    }
    builder.finish()
}

/// A heading placed in the outline.
struct Entry {
    source_order: u32,
    node: NodeId,
    level: i32,
    label: String,
    position: XyzDestination,
}

/// Builds the outline from pages that arrive in order.
///
/// Each heading takes the position of its first box fragment, or else of
/// the first fragment of its descendants, among the pages added so far.
#[derive(Default)]
pub(super) struct OutlineBuilder {
    positions: HashMap<NodeId, XyzDestination>,
    boxes: HashMap<NodeId, XyzDestination>,
    entries: Vec<Entry>,
}

impl OutlineBuilder {
    /// Record the positions of the visible fragments of `page`.
    pub(super) fn add_page(&mut self, page: &Page<'_>) {
        let dom = page.dom();
        for fragment in page.fragments() {
            let node = fragment.node();
            let element = if dom.kind(node) == Some(NodeKind::Text) {
                dom.parent(node).unwrap_or(node)
            } else {
                node
            };
            let rect = fragment.rect();
            if !visible(page, element)
                || ![rect.x, rect.y, rect.width, rect.height]
                    .iter()
                    .all(|value| value.is_finite())
                || rect.width <= 0.0
                || rect.height <= 0.0
            {
                continue;
            }
            let position = XyzDestination::new(
                page.index() as usize,
                Point::from_xy(rect.x * PX_TO_PT, rect.y * PX_TO_PT),
            );
            self.positions
                .entry(node)
                .or_insert_with(|| position.clone());
            if fragment.kind() == FragmentKind::Box {
                self.boxes.entry(node).or_insert(position);
            }
        }
    }

    /// Place the headings of `collector`, using the DOM and styles of `page`.
    pub(super) fn add_headings(&mut self, page: &Page<'_>, collector: BookmarkCollector) {
        let dom = page.dom();
        for (node, heading) in collector.headings {
            let Some(level) = heading.level.filter(|level| *level > 0) else {
                continue;
            };
            let Some(label) = heading.label else {
                continue;
            };
            let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
            if label.is_empty() || !visible(page, node) {
                continue;
            }
            let Some(position) = self
                .boxes
                .get(&node)
                .cloned()
                .or_else(|| first_descendant(dom, node, &self.positions))
            else {
                continue;
            };
            self.entries.push(Entry {
                source_order: heading.source_order,
                node,
                level,
                label,
                position,
            });
        }
    }

    /// Nest the placed headings by level, in source order.
    pub(super) fn finish(mut self) -> Outline {
        self.entries
            .sort_by_key(|entry| (entry.source_order, entry.node));
        let mut outline = Outline::new();
        let mut pending: Vec<(i32, OutlineNode)> = Vec::new();
        let attach = |pending: &mut Vec<(i32, OutlineNode)>, outline: &mut Outline| {
            if let Some((_, node)) = pending.pop() {
                if let Some((_, parent)) = pending.last_mut() {
                    parent.push_child(node);
                } else {
                    outline.push_child(node);
                }
            }
        };
        for entry in self.entries {
            while pending
                .last()
                .is_some_and(|(parent_level, _)| *parent_level >= entry.level)
            {
                attach(&mut pending, &mut outline);
            }
            pending.push((entry.level, OutlineNode::new(entry.label, entry.position)));
        }
        while !pending.is_empty() {
            attach(&mut pending, &mut outline);
        }
        outline
    }
}

fn visible(page: &Page<'_>, node: NodeId) -> bool {
    page.computed(node)
        .is_some_and(|style| matches!(style.visibility, ComputedVisibility::Visible))
}

#[cfg(test)]
mod tests;
