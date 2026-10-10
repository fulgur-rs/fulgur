use fulgur_core::{Error, Result, units::PX_TO_PT};
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
    closed: bool,
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
            ("bookmark-state", ConsumerPropertyValue::Keyword(state)) => {
                heading.closed = state == "closed"
            }
            _ => {}
        }
        Ok(())
    }
}

pub(super) fn registrations() -> Vec<ConsumerPropertyRegistration> {
    vec![
        ConsumerPropertyRegistration::integer_or_none("bookmark-level").non_inherited(),
        ConsumerPropertyRegistration::text("bookmark-label").non_inherited(),
        ConsumerPropertyRegistration::keyword("bookmark-state", &["open", "closed"])
            .non_inherited(),
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

/// A PDF outline and the open state of each entry in preorder.
#[derive(Default)]
pub(super) struct BookmarkOutline {
    pub(super) outline: Outline,
    pub(super) open: Vec<bool>,
}

/// The document outline of a laid-out document.
pub(super) fn outline(document: &DocumentLayout, collector: BookmarkCollector) -> BookmarkOutline {
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
    closed: bool,
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
                closed: heading.closed,
            });
        }
    }

    /// Nest the placed headings by level, in source order.
    pub(super) fn finish(mut self) -> BookmarkOutline {
        self.entries
            .sort_by_key(|entry| (entry.source_order, entry.node));
        let mut outline = Outline::new();
        let mut open = Vec::new();
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
            // The initial value of `bookmark-state` is `open` (CSS GCPM 3).
            open.push(!entry.closed);
            pending.push((entry.level, OutlineNode::new(entry.label, entry.position)));
        }
        while !pending.is_empty() {
            attach(&mut pending, &mut outline);
        }
        BookmarkOutline { outline, open }
    }
}

/// Rewrite the outline `/Count` entries of a finished PDF so that entries
/// marked open in `open` (preorder, matching the outline passed to Krilla)
/// start expanded.
///
/// Krilla 0.7 writes every entry closed, with a negative count of its direct
/// children. ISO 32000-2 §12.3.3 defines an open entry's count as its number
/// of visible descendants and a closed entry's as the negated number that
/// would be visible if it were opened; the outline root counts every visible
/// entry. A document with no open parent entry is returned unchanged.
pub(super) fn apply_open_state(pdf: Vec<u8>, open: &[bool]) -> Result<Vec<u8>> {
    if !open.contains(&true) {
        return Ok(pdf);
    }
    let error = |error: lopdf::Error| Error::PdfGeneration(format!("outline state: {error}"));
    let mut document = lopdf::Document::load_mem(&pdf).map_err(error)?;
    let root = document
        .catalog()
        .and_then(|catalog| catalog.get(b"Outlines"))
        .and_then(lopdf::Object::as_reference)
        .map_err(error)?;
    let mut states = open.iter().copied();
    let mut counts = Vec::new();
    let visible = visible_descendants(&document, root, &mut states, &mut counts).map_err(error)?;
    let changed = counts.iter().any(|(_, count)| *count > 0);
    if !changed {
        return Ok(pdf);
    }
    counts.push((root, visible));
    for (id, count) in counts {
        document
            .get_dictionary_mut(id)
            .map_err(error)?
            .set("Count", count);
    }
    let mut bytes = Vec::with_capacity(pdf.len());
    document.save_to(&mut bytes).map_err(Error::Io)?;
    Ok(bytes)
}

/// Count the entries below `parent` that are visible when it is open, and
/// record the `/Count` of every child that has children of its own.
fn visible_descendants(
    document: &lopdf::Document,
    parent: lopdf::ObjectId,
    states: &mut impl Iterator<Item = bool>,
    counts: &mut Vec<(lopdf::ObjectId, i64)>,
) -> lopdf::Result<i64> {
    let mut visible = 0;
    let mut next = document.get_dictionary(parent)?.get(b"First").ok().cloned();
    while let Some(child) = next {
        let child = child.as_reference()?;
        let open = states.next().unwrap_or(false);
        let entry = document.get_dictionary(child)?;
        let below = if entry.has(b"First") {
            let below = visible_descendants(document, child, states, counts)?;
            counts.push((child, if open { below } else { -below }));
            below
        } else {
            0
        };
        visible += 1 + if open { below } else { 0 };
        next = document.get_dictionary(child)?.get(b"Next").ok().cloned();
    }
    Ok(visible)
}

fn visible(page: &Page<'_>, node: NodeId) -> bool {
    page.computed(node)
        .is_some_and(|style| matches!(style.visibility, ComputedVisibility::Visible))
}

#[cfg(test)]
mod tests;
