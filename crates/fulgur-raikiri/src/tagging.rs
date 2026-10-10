//! Tagged PDF for the Raikiri backend.
//!
//! The logical structure comes from the DOM, classified with the same HTML
//! mapping as the Blitz backend ([`fulgur_core::tagging`]). The painter wraps
//! each draw in marked content: text, images and list markers become content
//! of the structure element their node belongs to, while box decorations,
//! margin boxes and repeated copies (table headers and `position: fixed`
//! boxes after their first page) become artifacts. Link annotations are added
//! as tagged annotations of their `<a>` element. After the last page the
//! structure tree is assembled in document order.

use fulgur_core::tagging::{ListNumbering, PdfTag, TableHeaderScope, classify_element};
use krilla::annotation::{Annotation, LinkAnnotation};
use krilla::page::Page as PdfPage;
use krilla::surface::Surface;
use krilla::tagging::{
    ArtifactType, ContentTag, Identifier, Node, SpanTag, Tag, TagGroup, TagKind, TagTree,
};
use raikiri_html::computed::{ComputedDisplay, ComputedListStyleType, ComputedVisibility};
use raikiri_html::{
    DocumentLayout, DomView, GeneratedKind, NodeId, NodeKind, Page, PageMarginBoxSlot,
    PositionedGlyphRun, RunSource,
};
use std::collections::{HashMap, HashSet};

/// Deepest DOM nesting the structure walk follows.
const MAX_DEPTH: usize = 512;

/// What a draw is, for the structure tree.
#[derive(Clone, Copy)]
pub(crate) enum Target<'r, 'a> {
    /// A glyph run of the page body.
    Run(&'r PositionedGlyphRun<'a>),
    /// The content of a replaced element (an image or an inline SVG).
    Replaced(NodeId),
    /// The image marker of a list item.
    MarkerImage(NodeId),
    /// Drawing that is not part of the document's content.
    Artifact(ArtifactType),
}

/// The artifact type of a page-margin box (PDF 1.7 §14.8.2.2.2).
pub(crate) fn margin_box_artifact(slot: PageMarginBoxSlot) -> ArtifactType {
    use PageMarginBoxSlot::*;
    match slot {
        TopLeftCorner | TopLeft | TopCenter | TopRight | TopRightCorner => ArtifactType::Header,
        BottomLeftCorner | BottomLeft | BottomCenter | BottomRight | BottomRightCorner => {
            ArtifactType::Footer
        }
        _ => ArtifactType::Page,
    }
}

/// Marked content and structure for one document; `None` inside when the
/// output is not tagged, which makes every method a plain pass-through.
pub(crate) struct Tags(Option<Tagger>);

impl Tags {
    pub(crate) fn new(document: &DocumentLayout, enabled: bool) -> Self {
        Self(enabled.then(|| Tagger::new(document)))
    }

    /// Whether the output is tagged.
    pub(crate) fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// Untagged output: content drawn inside an artifact, and painting
    /// tests that have no document.
    pub(crate) fn disabled() -> Self {
        Self(None)
    }

    /// Start a page: record which nodes it repeats.
    pub(crate) fn begin_page(&mut self, page: &Page<'_>) {
        if let Some(tagger) = &mut self.0 {
            tagger.begin_page(page);
        }
    }

    /// Finish a page: its repeated nodes count as already shown.
    pub(crate) fn end_page(&mut self) {
        if let Some(tagger) = &mut self.0 {
            tagger.seen_repeats.extend(tagger.page_repeats.drain());
        }
    }

    /// Run `draw` inside the marked content for `target`.
    ///
    /// Krilla's marked content does not nest, so callers wrap single draws
    /// and apply clips before calling this, never inside an open sequence.
    pub(crate) fn mark<'s, R>(
        &mut self,
        surface: &mut Surface<'s>,
        page: &Page<'_>,
        target: Target<'_, '_>,
        draw: impl FnOnce(&mut Surface<'s>) -> R,
    ) -> R {
        let Some(tagger) = &mut self.0 else {
            return draw(surface);
        };
        match tagger.resolve(page.dom(), target) {
            Mark::Artifact(kind) => {
                surface.start_tagged(ContentTag::Artifact(kind));
                let result = draw(surface);
                surface.end_tagged();
                result
            }
            Mark::Content(element, key) => {
                let id = surface.start_tagged(ContentTag::Span(SpanTag::empty()));
                let result = draw(surface);
                surface.end_tagged();
                tagger.push_content(element, key, id);
                result
            }
        }
    }

    /// Add a link annotation for the `<a>` element `owner`, as a tagged
    /// annotation of its `Link` structure element when there is one.
    ///
    /// Copies of a repeated link on later pages are tagged as well: their
    /// content is an artifact, but PDF/UA-1 §7.18.1 requires every
    /// annotation to be in the structure tree.
    pub(crate) fn annotate(
        &mut self,
        pdf_page: &mut PdfPage<'_>,
        page: &Page<'_>,
        owner: NodeId,
        link: LinkAnnotation,
    ) {
        let Some(tagger) = &mut self.0 else {
            pdf_page.add_annotation(Annotation::new_link(link, None));
            return;
        };
        let dom = page.dom();
        let name = accessible_name(dom, owner, &tagger.structure.ids);
        let annotation = Annotation::new_link(link, name);
        match tagger.structure.by_node.get(&owner) {
            Some(&element) => {
                let id = pdf_page.add_tagged_annotation(annotation);
                tagger.annotations.entry(element).or_default().push(id);
            }
            None => pdf_page.add_annotation(annotation),
        }
    }

    /// Add a link annotation for the `<a>` element `owner` of `page`
    /// without a structure element: a link inside a running element, whose
    /// content is a pagination artifact.
    pub(crate) fn annotate_artifact(
        &self,
        pdf_page: &mut PdfPage<'_>,
        page: &Page<'_>,
        owner: NodeId,
        link: LinkAnnotation,
    ) {
        let alt = self
            .0
            .as_ref()
            .and_then(|tagger| accessible_name(page.dom(), owner, &tagger.structure.ids));
        pdf_page.add_annotation(Annotation::new_link(link, alt));
    }

    /// The structure tree, or `None` when the output is not tagged.
    pub(crate) fn finish(self, lang: Option<String>) -> Option<TagTree> {
        self.0.map(|tagger| tagger.finish(lang))
    }
}

/// Whether `node` is an `<img>` with an empty `alt`.
fn is_decorative_image(dom: DomView<'_>, node: NodeId) -> bool {
    dom.local_name(node) == Some("img")
        && dom
            .attr(node, "alt")
            .is_some_and(|alt| alt.trim().is_empty())
}

/// The accessible name of the link or heading `node`: the text of the
/// elements its `aria-labelledby` refers to, else its text, else its
/// `aria-label` or `title`, else the `alt` text of an image inside it.
fn accessible_name(dom: DomView<'_>, node: NodeId, ids: &Ids) -> Option<String> {
    let nonempty = |text: &str| Some(text.trim().to_owned()).filter(|text| !text.is_empty());
    dom.attr(node, "aria-labelledby")
        .and_then(|list| labelled_by(dom, ids, list))
        .and_then(|text| nonempty(&text))
        .or_else(|| nonempty(&dom.text_content(node)))
        .or_else(|| dom.attr(node, "aria-label").and_then(nonempty))
        .or_else(|| dom.attr(node, "title").and_then(nonempty))
        .or_else(|| image_alt(dom, node, 0))
}

/// The text of the elements named by the space-separated ID list `ids`,
/// joined with spaces, following accname §2C. IDs that match no element
/// are skipped.
fn labelled_by(dom: DomView<'_>, ids: &Ids, list: &str) -> Option<String> {
    let parts: Vec<String> = list
        .split_ascii_whitespace()
        .filter_map(|id| ids.get(id).copied())
        .map(|element| dom.text_content(element).trim().to_owned())
        .filter(|text| !text.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Element IDs to the first element in tree order that carries them.
type Ids = HashMap<String, NodeId>;

/// Index the element IDs of the document once, so that resolving
/// `aria-labelledby` does not walk the whole tree for every reference.
fn index_ids(dom: DomView<'_>) -> Ids {
    let mut ids = Ids::new();
    let mut stack = vec![dom.root()];
    while let Some(node) = stack.pop() {
        if dom.kind(node) == Some(NodeKind::Element)
            && let Some(id) = dom.attr(node, "id")
        {
            ids.entry(id.to_owned()).or_insert(node);
        }
        let start = stack.len();
        stack.extend(dom.children(node));
        stack[start..].reverse();
    }
    ids
}

fn image_alt(dom: DomView<'_>, node: NodeId, depth: usize) -> Option<String> {
    if depth >= MAX_DEPTH {
        return None;
    }
    dom.children(node).find_map(|child| {
        if dom.kind(child) != Some(NodeKind::Element) {
            return None;
        }
        if dom.local_name(child) == Some("img")
            && let Some(alt) = dom.attr(child, "alt").map(str::trim)
            && !alt.is_empty()
        {
            return Some(alt.to_owned());
        }
        image_alt(dom, child, depth + 1)
    })
}

enum Mark {
    Artifact(ArtifactType),
    /// Content of a structure element, with its document-order sort key.
    Content(usize, Key),
}

/// Sort key of an item within its structure element: the DOM preorder
/// position of its source, then the order in which it was drawn.
type Key = (u32, u64);

struct Element {
    tag: PdfTag,
    alt: Option<String>,
    title: Option<String>,
    order: u32,
    children: Vec<usize>,
    content: Vec<(Key, Identifier)>,
}

/// The logical structure classified from the DOM.
struct Structure {
    elements: Vec<Element>,
    roots: Vec<usize>,
    /// Classified element node to its structure element.
    by_node: HashMap<NodeId, usize>,
    /// Classified element node to the structure element its descendants'
    /// content belongs to: the element itself, or a list item's `LBody`.
    content_of: HashMap<NodeId, usize>,
    /// List item node to its `Lbl`.
    labels: HashMap<NodeId, usize>,
    /// DOM preorder position of each node, and the last position inside it.
    order: HashMap<NodeId, (u32, u32)>,
    /// Element IDs, for `aria-labelledby`.
    ids: Ids,
}

impl Structure {
    fn build(document: &DocumentLayout) -> Self {
        let mut structure = Self {
            elements: Vec::new(),
            roots: Vec::new(),
            by_node: HashMap::new(),
            content_of: HashMap::new(),
            labels: HashMap::new(),
            order: HashMap::new(),
            ids: Ids::new(),
        };
        if let Some(page) = document.page(0) {
            let dom = page.dom();
            structure.ids = index_ids(dom);
            let mut counter = 0;
            structure.walk(&page, dom, dom.root(), None, 0, &mut counter);
        }
        structure
    }

    fn walk(
        &mut self,
        page: &Page<'_>,
        dom: DomView<'_>,
        node: NodeId,
        parent: Option<usize>,
        depth: usize,
        counter: &mut u32,
    ) {
        if depth >= MAX_DEPTH {
            return;
        }
        let order = *counter;
        *counter += 1;
        let name = (dom.kind(node) == Some(NodeKind::Element))
            .then(|| dom.local_name(node))
            .flatten();
        // `<head>` and its descendants do not take part in the structure.
        if name == Some("head") {
            self.order.insert(node, (order, order));
            return;
        }
        let style = page.computed(node);
        // `display: none` generates no boxes for the element or its
        // descendants, so none of them has content to tag.
        if style.is_some_and(|style| matches!(style.display, ComputedDisplay::None)) {
            self.skip(dom, node, depth, counter);
            self.order.insert(node, (order, *counter - 1));
            return;
        }
        let mut child_parent = parent;
        let visible =
            style.is_none_or(|style| matches!(style.visibility, ComputedVisibility::Visible));
        if let Some(name) = name
            && let Some(tag) = classify(page, dom, node, name)
            // An invisible figure or heading would lack the `/Alt` or title
            // that PDF/UA requires, and has no content of its own to tag.
            && (visible || !matches!(tag, PdfTag::Figure | PdfTag::H { .. }))
        {
            let element = self.push(parent, tag.clone(), order);
            self.by_node.insert(node, element);
            self.content_of.insert(node, element);
            child_parent = Some(element);
            match tag {
                PdfTag::Figure => {
                    self.elements[element].alt = dom.attr(node, "alt").map(str::to_owned);
                }
                PdfTag::H { .. } => {
                    self.elements[element].title = accessible_name(dom, node, &self.ids);
                }
                PdfTag::Li => {
                    // PDF/UA orders a list item's label before its body.
                    let label = self.push(Some(element), PdfTag::Lbl, order);
                    let body = self.push(Some(element), PdfTag::LBody, order);
                    self.labels.insert(node, label);
                    self.content_of.insert(node, body);
                    child_parent = Some(body);
                }
                _ => {}
            }
        }
        for child in dom.children(node) {
            self.walk(page, dom, child, child_parent, depth + 1, counter);
        }
        self.order.insert(node, (order, *counter - 1));
    }

    /// Give the descendants of a skipped node their preorder positions.
    fn skip(&mut self, dom: DomView<'_>, node: NodeId, depth: usize, counter: &mut u32) {
        if depth >= MAX_DEPTH {
            return;
        }
        for child in dom.children(node) {
            let order = *counter;
            *counter += 1;
            self.skip(dom, child, depth + 1, counter);
            self.order.insert(child, (order, *counter - 1));
        }
    }

    fn push(&mut self, parent: Option<usize>, tag: PdfTag, order: u32) -> usize {
        let index = self.elements.len();
        self.elements.push(Element {
            tag,
            alt: None,
            title: None,
            order,
            children: Vec::new(),
            content: Vec::new(),
        });
        match parent {
            Some(parent) => self.elements[parent].children.push(index),
            None => self.roots.push(index),
        }
        index
    }

    /// The structure element that content drawn for `node` belongs to: the
    /// nearest classified ancestor-or-self.
    fn container(&self, dom: DomView<'_>, node: NodeId) -> Option<usize> {
        let mut current = Some(node);
        for _ in 0..MAX_DEPTH {
            let node = current?;
            if let Some(&element) = self.content_of.get(&node) {
                return Some(element);
            }
            current = dom.parent(node);
        }
        None
    }
}

/// The HTML mapping of [`classify_element`], refined by the element's
/// attributes and computed style the same way as the Blitz backend, plus
/// `<a href>` as `Link`.
fn classify(page: &Page<'_>, dom: DomView<'_>, node: NodeId, name: &str) -> Option<PdfTag> {
    // An image with an empty `alt` is decorative (HTML §4.8.4.4.4): it is
    // painted as an artifact rather than tagged as a figure.
    if is_decorative_image(dom, node) {
        return None;
    }
    if name == "a" {
        return dom
            .attr(node, "href")
            .is_some_and(|href| !href.trim().is_empty())
            .then_some(PdfTag::Link);
    }
    match classify_element(name)? {
        PdfTag::L { numbering } => Some(PdfTag::L {
            numbering: page
                .computed(node)
                .map_or(numbering, |style| list_numbering(&style.list_style_type)),
        }),
        PdfTag::Th { .. } => Some(PdfTag::Th {
            scope: match dom
                .attr(node, "scope")
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("row") => TableHeaderScope::Row,
                Some("col" | "column") => TableHeaderScope::Column,
                _ => TableHeaderScope::Both,
            },
        }),
        tag => Some(tag),
    }
}

fn list_numbering(style: &ComputedListStyleType) -> ListNumbering {
    match style {
        ComputedListStyleType::Disc => ListNumbering::Disc,
        ComputedListStyleType::Named(name) => match name.as_str() {
            "disc" => ListNumbering::Disc,
            "circle" => ListNumbering::Circle,
            "square" => ListNumbering::Square,
            "decimal" => ListNumbering::Decimal,
            "lower-alpha" | "lower-latin" => ListNumbering::LowerAlpha,
            "upper-alpha" | "upper-latin" => ListNumbering::UpperAlpha,
            "lower-roman" => ListNumbering::LowerRoman,
            "upper-roman" => ListNumbering::UpperRoman,
            _ => ListNumbering::None,
        },
        _ => ListNumbering::None,
    }
}

/// The Krilla structure type of `tag`. Kept in this crate rather than in
/// `fulgur_core` so that the backend can move to another Krilla version on
/// its own.
fn krilla_tag(tag: &PdfTag, title: Option<String>, alt: Option<String>) -> TagKind {
    use krilla::tagging::{ListNumbering as KN, TableHeaderScope as KS, kind};
    match tag {
        PdfTag::P => Tag::<kind::P>::P.into(),
        PdfTag::H { level } => {
            let level = std::num::NonZeroU16::new(u16::from((*level).clamp(1, 6)))
                .unwrap_or(std::num::NonZeroU16::MIN);
            Tag::Hn(level, title).into()
        }
        PdfTag::Div => Tag::<kind::Div>::Div.into(),
        PdfTag::Span => Tag::<kind::Span>::Span.into(),
        PdfTag::Figure => Tag::<kind::Figure>::Figure(alt).into(),
        PdfTag::L { numbering } => Tag::L(match numbering {
            ListNumbering::None => KN::None,
            ListNumbering::Disc => KN::Disc,
            ListNumbering::Circle => KN::Circle,
            ListNumbering::Square => KN::Square,
            ListNumbering::Decimal => KN::Decimal,
            ListNumbering::LowerRoman => KN::LowerRoman,
            ListNumbering::UpperRoman => KN::UpperRoman,
            ListNumbering::LowerAlpha => KN::LowerAlpha,
            ListNumbering::UpperAlpha => KN::UpperAlpha,
        })
        .into(),
        PdfTag::Lbl => Tag::<kind::Lbl>::Lbl.into(),
        PdfTag::LBody => Tag::<kind::LBody>::LBody.into(),
        PdfTag::Li => Tag::<kind::LI>::LI.into(),
        PdfTag::Table => Tag::<kind::Table>::Table.into(),
        PdfTag::THead => Tag::<kind::THead>::THead.into(),
        PdfTag::TBody => Tag::<kind::TBody>::TBody.into(),
        PdfTag::TFoot => Tag::<kind::TFoot>::TFoot.into(),
        PdfTag::Tr => Tag::<kind::TR>::TR.into(),
        PdfTag::Th { scope } => Tag::TH(match scope {
            TableHeaderScope::Row => KS::Row,
            TableHeaderScope::Column => KS::Column,
            TableHeaderScope::Both => KS::Both,
        })
        .into(),
        PdfTag::Td => Tag::<kind::TD>::TD.into(),
        PdfTag::Link => Tag::<kind::Link>::Link.into(),
    }
}

struct Tagger {
    structure: Structure,
    /// Structure elements created for content outside every classified
    /// element, by the element that holds that content and the classified
    /// root before it, so that a classified sibling splits the paragraph.
    orphans: HashMap<(Option<NodeId>, Option<usize>), usize>,
    /// DOM preorder positions of the classified roots, ascending.
    root_orders: Vec<u32>,
    annotations: HashMap<usize, Vec<Identifier>>,
    /// Nodes with a repeated fragment on the current page.
    page_repeats: HashSet<NodeId>,
    /// Repeated nodes already shown on an earlier page.
    seen_repeats: HashSet<NodeId>,
    sequence: u64,
}

impl Tagger {
    fn new(document: &DocumentLayout) -> Self {
        let structure = Structure::build(document);
        let root_orders = structure
            .roots
            .iter()
            .map(|&root| structure.elements[root].order)
            .collect();
        Self {
            structure,
            orphans: HashMap::new(),
            root_orders,
            annotations: HashMap::new(),
            page_repeats: HashSet::new(),
            seen_repeats: HashSet::new(),
            sequence: 0,
        }
    }

    fn begin_page(&mut self, page: &Page<'_>) {
        self.page_repeats = page
            .fragments()
            .filter(|fragment| fragment.repeat().is_some())
            .map(|fragment| fragment.node())
            .collect();
    }

    /// Whether `node` is drawn on this page as a copy of a repeated box
    /// that an earlier page already showed: a repeated table header or a
    /// `position: fixed` box. Only the first copy is content.
    fn is_repeat_copy(&self, dom: DomView<'_>, node: NodeId) -> bool {
        if self.page_repeats.is_empty() {
            return false;
        }
        let mut current = Some(node);
        for _ in 0..MAX_DEPTH {
            let Some(node) = current else { return false };
            if self.page_repeats.contains(&node) {
                return self.seen_repeats.contains(&node);
            }
            current = dom.parent(node);
        }
        false
    }

    fn resolve(&mut self, dom: DomView<'_>, target: Target<'_, '_>) -> Mark {
        let (source, element) = match target {
            Target::Artifact(kind) => return Mark::Artifact(kind),
            Target::Run(run) => match run.source {
                RunSource::Text(text) => (text, dom.parent(text)),
                RunSource::Generated(owner, kind) => {
                    if self.is_repeat_copy(dom, owner) {
                        return Mark::Artifact(ArtifactType::Other);
                    }
                    let (order, end) = self.order(owner);
                    if kind == GeneratedKind::Marker
                        && let Some(&label) = self.structure.labels.get(&owner)
                    {
                        return self.content(label, order);
                    }
                    let key = if kind == GeneratedKind::After {
                        end
                    } else {
                        order
                    };
                    return self.content_at(dom, owner, Some(owner), key);
                }
                // Ellipses and anything else stand in for content that is
                // already in the structure, or decorate it.
                _ => return Mark::Artifact(ArtifactType::Other),
            },
            Target::Replaced(node) if is_decorative_image(dom, node) => {
                return Mark::Artifact(ArtifactType::Other);
            }
            Target::Replaced(node) => (node, Some(node)),
            Target::MarkerImage(owner) => {
                if self.is_repeat_copy(dom, owner) {
                    return Mark::Artifact(ArtifactType::Other);
                }
                return match self.structure.labels.get(&owner) {
                    Some(&label) => {
                        let order = self.order(owner).0;
                        self.content(label, order)
                    }
                    None => Mark::Artifact(ArtifactType::Other),
                };
            }
        };
        if self.is_repeat_copy(dom, source) {
            return Mark::Artifact(ArtifactType::Other);
        }
        let key = self.order(source).0;
        self.content_at(dom, source, element, key)
    }

    fn order(&self, node: NodeId) -> (u32, u32) {
        self.structure
            .order
            .get(&node)
            .copied()
            .unwrap_or((u32::MAX, u32::MAX))
    }

    /// Content drawn for `source`, whose element is `element`.
    fn content_at(
        &mut self,
        dom: DomView<'_>,
        source: NodeId,
        element: Option<NodeId>,
        key: u32,
    ) -> Mark {
        let container = match self.structure.container(dom, source) {
            Some(container) => container,
            None => {
                // Text directly inside `<body>` or another unclassified
                // element: give it a paragraph of its own, since content
                // cannot sit at the root of the structure tree.
                let before = self.root_orders.partition_point(|&order| order < key);
                let segment = before.checked_sub(1);
                let order = element.map_or(key, |node| self.order(node).0);
                let order = segment.map_or(order, |root| order.max(self.root_orders[root] + 1));
                *self
                    .orphans
                    .entry((element, segment))
                    .or_insert_with(|| self.structure.push(None, PdfTag::P, order))
            }
        };
        self.content(container, key)
    }

    fn content(&mut self, element: usize, key: u32) -> Mark {
        self.sequence += 1;
        Mark::Content(element, (key, self.sequence))
    }

    fn push_content(&mut self, element: usize, key: Key, id: Identifier) {
        self.structure.elements[element].content.push((key, id));
    }

    fn finish(mut self, lang: Option<String>) -> TagTree {
        let mut tree = TagTree::new().with_lang(lang);
        let mut roots = std::mem::take(&mut self.structure.roots);
        roots.sort_by_key(|&root| self.structure.elements[root].order);
        for root in roots {
            tree.push(Node::Group(self.group(root)));
        }
        tree
    }

    fn group(&mut self, index: usize) -> TagGroup {
        let element = &mut self.structure.elements[index];
        let mut group = TagGroup::new(krilla_tag(
            &element.tag,
            element.title.take(),
            element.alt.take(),
        ));
        let children = std::mem::take(&mut element.children);
        let content = std::mem::take(&mut element.content);
        enum Item {
            Content(Identifier),
            Child(usize),
        }
        // Children sort before content at the same position, so a list
        // item's label precedes its body; equal keys keep their order.
        let mut items: Vec<(Key, Item)> = children
            .into_iter()
            .map(|child| {
                let order = self.structure.elements[child].order;
                ((order, 0), Item::Child(child))
            })
            .collect();
        items.extend(
            content
                .into_iter()
                .map(|(key, id)| (key, Item::Content(id))),
        );
        items.sort_by_key(|(key, _)| *key);
        for (_, item) in items {
            match item {
                Item::Content(id) => group.push(Node::Leaf(id)),
                Item::Child(child) => group.push(Node::Group(self.group(child))),
            }
        }
        // A link's annotations follow its content (PDF/UA-1 §7.18.5).
        for id in self.annotations.remove(&index).unwrap_or_default() {
            group.push(Node::Leaf(id));
        }
        group
    }
}

#[cfg(test)]
mod tests;
