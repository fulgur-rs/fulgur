use fulgur_core::units::PX_TO_PT;
use krilla::{
    destination::XyzDestination,
    geom::Point,
    outline::{Outline, OutlineNode},
};
use raikiri_html::{
    ConsumerPropertyRegistration, DocumentLayout, DomView, FragmentKind, NodeId,
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

pub(super) fn outline(document: &DocumentLayout, collector: &BookmarkCollector) -> Outline {
    let mut outline = Outline::new();
    let Some(first_page) = document.page(0) else {
        return outline;
    };
    let dom = first_page.dom();
    let visible = |node| {
        first_page
            .computed(node)
            .is_some_and(|style| matches!(style.visibility, ComputedVisibility::Visible))
    };
    let mut positions = HashMap::new();
    let mut boxes = HashMap::new();
    for page in document.pages() {
        for fragment in page.fragments() {
            let node = fragment.node();
            let element = if dom.kind(node) == Some(NodeKind::Text) {
                dom.parent(node).unwrap_or(node)
            } else {
                node
            };
            let rect = fragment.rect();
            if !visible(element)
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
            positions.entry(node).or_insert_with(|| position.clone());
            if fragment.kind() == FragmentKind::Box {
                boxes.entry(node).or_insert(position);
            }
        }
    }
    let mut headings: Vec<_> = collector.headings.iter().collect();
    headings.sort_by_key(|(node, heading)| (heading.source_order, **node));
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
    for (&node, heading) in headings {
        let Some(level) = heading.level.filter(|level| *level > 0) else {
            continue;
        };
        let Some(label) = &heading.label else {
            continue;
        };
        let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
        if label.is_empty() || !visible(node) {
            continue;
        }
        let Some(position) = boxes
            .get(&node)
            .cloned()
            .or_else(|| first_descendant(dom, node, &positions))
        else {
            continue;
        };
        while pending
            .last()
            .is_some_and(|(parent_level, _)| *parent_level >= level)
        {
            attach(&mut pending, &mut outline);
        }
        pending.push((level, OutlineNode::new(label, position)));
    }
    while !pending.is_empty() {
        attach(&mut pending, &mut outline);
    }
    outline
}

#[cfg(test)]
mod tests;
