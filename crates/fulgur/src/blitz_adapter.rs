//! Thin adapter over Blitz APIs. All Blitz-specific code is isolated here
//! so that upstream API changes only require changes in this module.
//!
//! # Thread safety
//!
//! Blitz (as of `blitz-dom 0.2.4` / `blitz-html 0.2.0`) is thread-safe for the
//! operations fulgur uses: multiple threads may call `parse`, `resolve`, and
//! pass application concurrently on independent documents. The adapter does
//! not take any process-wide lock.
//!
//! # Stdout hygiene
//!
//! `blitz-html` prints `println!("ERROR: {error}")` in its `TreeSink::finish`
//! implementation for every non-fatal html5ever parse error (unclosed tags,
//! unexpected tokens, etc.). fulgur does *not* suppress these at the library
//! level because doing so required manipulating process-wide fd 1, which is
//! fundamentally racy in multi-threaded contexts. Callers that need clean
//! stdout (notably `fulgur-cli` when rendering to stdout with `-o -`) must
//! redirect fd 1 at their own call site where single-threaded execution is
//! guaranteed. See `docs/plans/2026-04-11-blitz-thread-safety-investigation.md`
//! for the full investigation and rationale.

use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};
use parley::FontContext;
use std::path::PathBuf;
use std::sync::Arc;

/// Parse HTML and return a fully resolved document (styles + layout computed).
///
/// We pass the content width as the viewport width so Taffy wraps text
/// at the right column. The viewport height is set very large so that
/// Taffy lays out the full document without clipping — our own pagination
/// algorithm handles page breaks.
pub fn parse_and_layout(
    html: &str,
    viewport_width: f32,
    _viewport_height: f32,
    font_data: &[Arc<Vec<u8>>],
) -> HtmlDocument {
    let mut doc = parse(html, viewport_width, font_data);
    resolve(&mut doc);
    doc
}

/// Context available to each DOM pass.
pub struct PassContext<'a> {
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub font_data: &'a [Arc<Vec<u8>>],
}

/// A single transformation step applied to the parsed DOM before layout resolution.
pub trait DomPass {
    fn apply(&self, doc: &mut HtmlDocument, ctx: &PassContext<'_>);
}

/// Parse HTML into a document without resolving styles or layout.
pub fn parse(html: &str, viewport_width: f32, font_data: &[Arc<Vec<u8>>]) -> HtmlDocument {
    let viewport = Viewport::new(viewport_width as u32, 10000, 1.0, ColorScheme::Light);

    let font_ctx = if font_data.is_empty() {
        None
    } else {
        let mut ctx = FontContext::new();
        for data in font_data {
            let blob: parley::fontique::Blob<u8> = (**data).clone().into();
            ctx.collection.register_fonts(blob, None);
        }
        Some(ctx)
    };

    let config = DocumentConfig {
        viewport: Some(viewport),
        font_ctx,
        base_url: Some("file:///".to_string()),
        ..DocumentConfig::default()
    };

    HtmlDocument::from_html(html, config)
}

/// Apply a sequence of DOM passes to a parsed document.
pub fn apply_passes(doc: &mut HtmlDocument, passes: &[Box<dyn DomPass>], ctx: &PassContext<'_>) {
    for pass in passes {
        pass.apply(doc, ctx);
    }
}

/// Apply a single `DomPass` to a document.
///
/// Thin adapter that lets callers invoke a typed pass directly while still
/// going through `blitz_adapter`, preserving the module's role as the single
/// Blitz API surface (see `CLAUDE.md`: "Adapter isolation: Blitz API surface
/// is contained in `blitz_adapter.rs`"). Callers can retain access to
/// pass-specific accessors (for example `RunningElementPass::into_running_store`)
/// by borrowing the pass here rather than consuming it via `apply_passes`.
pub fn apply_single_pass<P: DomPass + ?Sized>(
    pass: &P,
    doc: &mut HtmlDocument,
    ctx: &PassContext<'_>,
) {
    pass.apply(doc, ctx);
}

/// Resolve styles (Stylo) and compute layout (Taffy).
pub fn resolve(doc: &mut HtmlDocument) {
    doc.resolve(0.0);
}

use crate::MAX_DOM_DEPTH;

/// Walk the DOM tree to find the first element with the given tag name.
/// Returns the node id if found.
fn find_element_by_tag(doc: &HtmlDocument, tag: &str) -> Option<usize> {
    let root = doc.root_element();
    find_element_by_tag_recursive(doc, root.id, tag, 0)
}

fn find_element_by_tag_recursive(
    doc: &HtmlDocument,
    node_id: usize,
    tag: &str,
    depth: usize,
) -> Option<usize> {
    if depth >= MAX_DOM_DEPTH {
        return None;
    }
    let node = doc.get_node(node_id)?;
    if let Some(el) = node.element_data() {
        if el.name.local.as_ref() == tag {
            return Some(node_id);
        }
    }
    for &child_id in &node.children {
        if let Some(found) = find_element_by_tag_recursive(doc, child_id, tag, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// Get the value of an attribute by name from an element.
pub fn get_attr<'a>(elem: &'a blitz_dom::node::ElementData, name: &str) -> Option<&'a str> {
    elem.attrs()
        .iter()
        .find(|a| a.name.local.as_ref() == name)
        .map(|a| a.value.as_ref())
}

/// Extract the parsed `usvg::Tree` from an inline `<svg>` element, if present.
///
/// Blitz parses inline `<svg>` elements during DOM construction (default `svg`
/// feature on `blitz-dom`) and stores the result on `ElementData::image_data()`
/// as `ImageData::Svg(Box<usvg::Tree>)`. This helper hides the `ImageData`
/// enum and the deref-and-clone dance so callers in `convert.rs` don't need
/// to import blitz internals — preserving the adapter isolation rule.
///
/// The clone is required because Blitz only exposes `&Box<Tree>` via `&Node`;
/// it is shallow in practice because `usvg::Tree`'s heavy collections (paths,
/// gradients, fontdb) are `Arc`-shared internally.
pub fn extract_inline_svg_tree(
    elem: &blitz_dom::node::ElementData,
) -> Option<std::sync::Arc<usvg::Tree>> {
    use blitz_dom::node::ImageData;
    match elem.image_data()? {
        ImageData::Svg(tree) => Some(std::sync::Arc::new((**tree).clone())),
        _ => None,
    }
}

fn make_qual_name(local: &str) -> blitz_dom::QualName {
    blitz_dom::QualName::new(
        None,
        blitz_dom::ns!(html),
        blitz_dom::LocalName::from(local),
    )
}

/// Create a `<style>` element with the given CSS text, attach it to the DOM,
/// and register it with Stylo. Returns the style node id.
///
/// If `insert_before` is `Some(sibling_id)`, the style element is inserted before
/// that sibling. Otherwise it is appended to `parent_id`.
fn inject_style_node(
    doc: &mut HtmlDocument,
    parent_id: usize,
    css: &str,
    insert_before: Option<usize>,
) -> usize {
    let style_id = {
        let mut mutator = doc.mutate();
        let style_id = mutator.create_element(make_qual_name("style"), vec![]);
        let text_id = mutator.create_text_node(css);
        if let Some(sibling) = insert_before {
            mutator.insert_nodes_before(sibling, &[style_id]);
        } else {
            mutator.append_children(parent_id, &[style_id]);
        }
        mutator.append_children(style_id, &[text_id]);
        style_id
    };
    doc.upsert_stylesheet_for_node(style_id);
    style_id
}

/// Injects CSS text as a `<style>` element into the document's `<head>`.
pub struct InjectCssPass {
    pub css: String,
}

impl DomPass for InjectCssPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.css.is_empty() {
            return;
        }

        // Find or create <head>
        let head_id = match find_element_by_tag(doc, "head") {
            Some(id) => id,
            None => {
                // Create <head> as first child of <html>
                let html_id = doc.root_element().id;
                let mut mutator = doc.mutate();
                let head_id = mutator.create_element(make_qual_name("head"), vec![]);
                let children = mutator.child_ids(html_id);
                if let Some(&first_child) = children.first() {
                    mutator.insert_nodes_before(first_child, &[head_id]);
                } else {
                    mutator.append_children(html_id, &[head_id]);
                }
                drop(mutator);
                head_id
            }
        };

        inject_style_node(doc, head_id, &self.css, None);
    }
}

/// Resolves `<link rel="stylesheet" href="...">` tags by reading local CSS files
/// and injecting them as `<style>` elements.
pub struct LinkStylesheetPass {
    pub base_path: PathBuf,
}

impl DomPass for LinkStylesheetPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        // Phase 1: Collect link elements and their CSS content
        let head_id = match find_element_by_tag(doc, "head") {
            Some(id) => id,
            None => return,
        };

        let mut css_entries: Vec<(usize, String)> = Vec::new(); // (link_node_id, css_content)

        let canonical_base = match self.base_path.canonicalize() {
            Ok(p) => p,
            Err(_) => return,
        };

        let head_children: Vec<usize> = doc
            .get_node(head_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();

        for &child_id in &head_children {
            let Some(node) = doc.get_node(child_id) else {
                continue;
            };
            let Some(elem) = node.element_data() else {
                continue;
            };
            if elem.name.local.as_ref() != "link" {
                continue;
            }

            let is_stylesheet = get_attr(elem, "rel").is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|t| t.eq_ignore_ascii_case("stylesheet"))
            });
            if !is_stylesheet {
                continue;
            }

            let Some(href) = get_attr(elem, "href") else {
                continue;
            };
            let href = href.to_string();

            // Skip http/https URLs (offline-first design)
            if href.starts_with("http://") || href.starts_with("https://") {
                continue;
            }

            // Resolve path — restrict to base_path to prevent path traversal
            let path = if std::path::Path::new(&href).is_absolute() {
                PathBuf::from(&href)
            } else {
                self.base_path.join(&href)
            };

            // Canonicalize both paths and verify the resolved path is within base_path.
            // This prevents directory traversal attacks (e.g. href="../../etc/passwd").
            let canonical_path = match path.canonicalize() {
                Ok(p) => p,
                Err(_) => {
                    eprintln!(
                        "Warning: could not resolve stylesheet '{}' (resolved to '{}')",
                        href,
                        path.display()
                    );
                    continue;
                }
            };
            if !canonical_path.starts_with(&canonical_base) {
                eprintln!(
                    "Warning: stylesheet '{}' is outside base path, skipped",
                    href
                );
                continue;
            }

            // Read file (skip with warning if missing)
            if let Ok(css) = std::fs::read_to_string(&canonical_path) {
                css_entries.push((child_id, css));
            } else {
                eprintln!(
                    "Warning: could not read stylesheet '{}' (resolved to '{}')",
                    href,
                    path.display()
                );
            }
        }

        // Phase 2: Replace each <link> with a <style> element
        for (link_id, css) in css_entries {
            inject_style_node(doc, head_id, &css, Some(link_id));
            doc.mutate().remove_node(link_id);
        }
    }
}

use crate::gcpm::counter::{CounterState, format_counter};
use crate::gcpm::running::{RunningElementStore, serialize_node};
use crate::gcpm::string_set::{StringSetEntry, StringSetStore, extract_text_content};
use crate::gcpm::{
    ContentCounterMapping, ContentItem, CounterMapping, CounterOp, ParsedSelector, PseudoElement,
    RunningMapping, StringSetMapping, StringSetValue,
};
use std::cell::RefCell;

/// Returns true for elements that should never be walked for GCPM detection
/// (head, script, style, etc.) — they contain no user-visible content.
fn is_non_visual_tag(tag: &str) -> bool {
    matches!(
        tag,
        "head" | "script" | "style" | "link" | "meta" | "title" | "noscript"
    )
}

/// Check whether a `ParsedSelector` (simple class/id/tag selector) matches the given element.
fn selector_matches(selector: &ParsedSelector, elem: &blitz_dom::node::ElementData) -> bool {
    match selector {
        ParsedSelector::Class(name) => get_attr(elem, "class")
            .is_some_and(|cls| cls.split_whitespace().any(|c| c == name.as_str())),
        ParsedSelector::Id(name) => get_attr(elem, "id") == Some(name.as_str()),
        ParsedSelector::Tag(name) => elem.name.local.as_ref().eq_ignore_ascii_case(name),
    }
}

/// Extracts running elements from the DOM and stores their serialized HTML.
pub struct RunningElementPass {
    mappings: Vec<RunningMapping>,
    store: RefCell<RunningElementStore>,
}

impl RunningElementPass {
    pub fn new(mappings: Vec<RunningMapping>) -> Self {
        Self {
            mappings,
            store: RefCell::new(RunningElementStore::new()),
        }
    }

    pub fn into_running_store(self) -> RunningElementStore {
        self.store.into_inner()
    }
}

impl DomPass for RunningElementPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id, 0);
    }
}

impl RunningElementPass {
    fn walk_tree(&self, doc: &HtmlDocument, node_id: usize, depth: usize) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if let Some(elem) = node.element_data() {
            if is_non_visual_tag(elem.name.local.as_ref()) {
                return;
            }
            if let Some(running_name) = self.find_running_name(elem) {
                let html = serialize_node(doc, node_id);
                self.store
                    .borrow_mut()
                    .register(node_id, running_name, html);
                // Running elements are removed from flow — don't recurse.
                return;
            }
        }

        for &child_id in &node.children {
            self.walk_tree(doc, child_id, depth + 1);
        }
    }

    fn find_running_name(&self, elem: &blitz_dom::node::ElementData) -> Option<String> {
        self.mappings
            .iter()
            .find(|m| selector_matches(&m.parsed, elem))
            .map(|m| m.running_name.clone())
    }
}

/// Extracts string-set values from the DOM by walking the tree and resolving text content.
pub struct StringSetPass {
    mappings: Vec<StringSetMapping>,
    store: RefCell<StringSetStore>,
}

impl StringSetPass {
    pub fn new(mappings: Vec<StringSetMapping>) -> Self {
        Self {
            mappings,
            store: RefCell::new(StringSetStore::new()),
        }
    }

    pub fn into_store(self) -> StringSetStore {
        self.store.into_inner()
    }
}

impl DomPass for StringSetPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id, 0);
    }
}

impl StringSetPass {
    fn walk_tree(&self, doc: &HtmlDocument, node_id: usize, depth: usize) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if let Some(elem) = node.element_data() {
            if is_non_visual_tag(elem.name.local.as_ref()) {
                return;
            }
            if let Some(mapping) = self.find_string_set(elem) {
                let value = resolve_string_set_values(doc, node_id, elem, &mapping.values);
                self.store.borrow_mut().push(StringSetEntry {
                    name: mapping.name.clone(),
                    value,
                    node_id,
                });
            }
        }

        // string-set targets stay in document flow — always recurse into children.
        for &child_id in &node.children {
            self.walk_tree(doc, child_id, depth + 1);
        }
    }

    fn find_string_set(&self, elem: &blitz_dom::node::ElementData) -> Option<&StringSetMapping> {
        self.mappings
            .iter()
            .find(|m| selector_matches(&m.parsed, elem))
    }
}

/// Walks the DOM applying counter-reset/increment/set operations and resolves
/// `content: counter()` in ::before/::after pseudo-elements by generating override CSS.
pub struct CounterPass {
    counter_mappings: Vec<CounterMapping>,
    content_mappings: Vec<ContentCounterMapping>,
    state: RefCell<CounterState>,
    generated_css: RefCell<String>,
    counter_id: RefCell<usize>,
    /// Counter ops keyed by node_id, for later use in Pageable markers.
    ops_by_node: RefCell<Vec<(usize, Vec<CounterOp>)>>,
}

impl CounterPass {
    pub fn new(
        counter_mappings: Vec<CounterMapping>,
        content_mappings: Vec<ContentCounterMapping>,
    ) -> Self {
        Self {
            counter_mappings,
            content_mappings,
            state: RefCell::new(CounterState::new()),
            generated_css: RefCell::new(String::new()),
            counter_id: RefCell::new(0),
            ops_by_node: RefCell::new(Vec::new()),
        }
    }

    pub fn generated_css(&self) -> String {
        self.generated_css.borrow().clone()
    }

    /// Consume self and return (ops_by_node for Pageable markers, generated CSS for body).
    pub fn into_parts(self) -> (Vec<(usize, Vec<CounterOp>)>, String) {
        (
            self.ops_by_node.into_inner(),
            self.generated_css.into_inner(),
        )
    }
}

impl DomPass for CounterPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.counter_mappings.is_empty() && self.content_mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id, 0);
    }
}

impl CounterPass {
    fn walk_tree(&self, doc: &mut HtmlDocument, node_id: usize, depth: usize) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        // Phase 1: Read element data immutably to collect matched operations
        // and matched content mapping indices. We must drop the immutable borrow
        // before calling doc.get_node_mut().
        let phase1 = {
            let Some(node) = doc.get_node(node_id) else {
                return;
            };
            let Some(elem) = node.element_data() else {
                // Not an element — just recurse into children
                let children: Vec<usize> = node.children.clone();
                for child_id in children {
                    self.walk_tree(doc, child_id, depth + 1);
                }
                return;
            };

            if is_non_visual_tag(elem.name.local.as_ref()) {
                return;
            }

            // Collect counter operations
            let mut matched_ops = Vec::new();
            for mapping in &self.counter_mappings {
                if selector_matches(&mapping.parsed, elem) {
                    matched_ops.extend(mapping.ops.clone());
                }
            }

            // Collect indices of matching content mappings (resolve values
            // after counter state is updated in phase 2)
            let mut matched_content_indices: Vec<usize> = Vec::new();
            for (i, mapping) in self.content_mappings.iter().enumerate() {
                if selector_matches(&mapping.parsed, elem) {
                    matched_content_indices.push(i);
                }
            }

            Some((matched_ops, matched_content_indices))
        };
        // immutable borrow of doc is now dropped

        let Some((matched_ops, matched_content_indices)) = phase1 else {
            return;
        };

        // Phase 2: Apply counter state changes (no doc borrow needed)
        if !matched_ops.is_empty() {
            let mut state = self.state.borrow_mut();
            for op in &matched_ops {
                match op {
                    CounterOp::Reset { name, value } => state.reset(name, *value),
                    CounterOp::Increment { name, value } => state.increment(name, *value),
                    CounterOp::Set { name, value } => state.set(name, *value),
                }
            }
            drop(state);
            self.ops_by_node.borrow_mut().push((node_id, matched_ops));
        }

        // Phase 3: Split ::before (resolve now) and ::after (resolve after children).
        // CSS spec: ::before is a first child, ::after is a last child, so
        // ::after must see counter state changes from descendants.
        let (before_indices, after_indices): (Vec<usize>, Vec<usize>) = matched_content_indices
            .into_iter()
            .partition(|&idx| self.content_mappings[idx].pseudo == PseudoElement::Before);

        // Allocate a cid if any content mappings matched (needed for both phases)
        let attr_value = if !before_indices.is_empty() || !after_indices.is_empty() {
            let mut id = self.counter_id.borrow_mut();
            let v = format!("{}", *id);
            *id += 1;
            drop(id);

            // Set data attribute once
            let qual = make_qual_name("data-fulgur-cid");
            if let Some(node_mut) = doc.get_node_mut(node_id) {
                if let Some(elem_mut) = node_mut.element_data_mut() {
                    elem_mut.attrs.set(qual, &v);
                }
            }
            Some(v)
        } else {
            None
        };

        // Resolve ::before now (before child traversal)
        if let Some(ref cid) = attr_value {
            use std::fmt::Write;
            let mut css = self.generated_css.borrow_mut();
            for idx in &before_indices {
                let mapping = &self.content_mappings[*idx];
                let resolved = self.resolve_content(&mapping.content);
                let _ = write!(
                    css,
                    "[data-fulgur-cid=\"{}\"]::before{{content:\"{}\"}}",
                    cid,
                    css_escape_string(&resolved)
                );
            }
        }

        // Recurse into children (re-read children since we may have mutated doc)
        let children: Vec<usize> = doc
            .get_node(node_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        for child_id in children {
            self.walk_tree(doc, child_id, depth + 1);
        }

        // Resolve ::after now (after child traversal, sees descendant counter changes)
        if let Some(ref cid) = attr_value {
            use std::fmt::Write;
            let mut css = self.generated_css.borrow_mut();
            for idx in &after_indices {
                let mapping = &self.content_mappings[*idx];
                let resolved = self.resolve_content(&mapping.content);
                let _ = write!(
                    css,
                    "[data-fulgur-cid=\"{}\"]::after{{content:\"{}\"}}",
                    cid,
                    css_escape_string(&resolved)
                );
            }
        }
    }

    fn resolve_content(&self, items: &[ContentItem]) -> String {
        let state = self.state.borrow();
        let mut out = String::new();
        for item in items {
            match item {
                ContentItem::String(s) => out.push_str(s),
                ContentItem::Counter { name, style } => {
                    let value = state.get(name);
                    out.push_str(&format_counter(value, *style));
                }
                _ => {}
            }
        }
        out
    }
}

fn css_escape_string(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\a "),
            '\r' => out.push_str("\\d "),
            '\0' => out.push_str("\\0 "),
            c if c.is_control() => {
                out.push_str(&format!("\\{:x} ", c as u32));
            }
            _ => out.push(ch),
        }
    }
    out
}

fn resolve_string_set_values(
    doc: &HtmlDocument,
    node_id: usize,
    elem: &blitz_dom::node::ElementData,
    values: &[StringSetValue],
) -> String {
    let mut out = String::new();
    for val in values {
        match val {
            StringSetValue::ContentText => {
                out.push_str(&extract_text_content(doc, node_id));
            }
            // content(before)/content(after) require pseudo-element computed styles.
            StringSetValue::ContentBefore | StringSetValue::ContentAfter => {}
            StringSetValue::Attr(attr_name) => {
                if let Some(v) = get_attr(elem, attr_name) {
                    out.push_str(v);
                }
            }
            StringSetValue::Literal(s) => out.push_str(s),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoOpPass;
    impl DomPass for NoOpPass {
        fn apply(&self, _doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {}
    }

    #[test]
    fn test_parse_resolve_roundtrip() {
        let html = "<html><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(NoOpPass)], &ctx);
        resolve(&mut doc);
        let root = doc.root_element();
        assert!(!root.children.is_empty());
    }

    #[test]
    fn test_parse_and_layout_unchanged() {
        let html = "<html><body><p>Test</p></body></html>";
        let doc = parse_and_layout(html, 400.0, 600.0, &[]);
        let root = doc.root_element();
        assert!(!root.children.is_empty());
    }

    #[test]
    fn test_inject_css_pass_adds_style() {
        let html = "<html><head></head><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);
        let pass = InjectCssPass {
            css: "p { color: red; }".to_string(),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_some(),
            "Expected a <style> element to be injected into the DOM"
        );
    }

    #[test]
    fn test_inject_css_pass_empty_css_is_noop() {
        let html = "<html><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);
        let pass = InjectCssPass { css: String::new() };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_none(),
            "Expected no <style> element when CSS is empty"
        );
    }

    #[test]
    fn test_running_element_pass_extracts_by_class() {
        let html = r#"<html><head><style>.header { display: none; }</style></head><body>
            <div class="header">Header Content</div>
            <p>Body text</p>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![crate::gcpm::RunningMapping {
                parsed: crate::gcpm::ParsedSelector::Class("header".to_string()),
                running_name: "pageHeader".to_string(),
            }],
            string_set_mappings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            page_settings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm.running_mappings);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert_eq!(
            store.instance_count(),
            1,
            "Expected exactly one running element instance to be registered"
        );
        assert_eq!(store.name_of(0), Some("pageHeader"));
        let html_content = store.get_html(0).unwrap();
        assert!(
            html_content.contains("Header Content"),
            "Expected serialized HTML to contain 'Header Content', got: {html_content}"
        );
    }

    #[test]
    fn test_running_element_pass_extracts_by_id() {
        let html = r#"<html><head><style>#title { display: none; }</style></head><body>
            <h1 id="title">Doc Title</h1>
            <p>Body text</p>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![crate::gcpm::RunningMapping {
                parsed: crate::gcpm::ParsedSelector::Id("title".to_string()),
                running_name: "pageTitle".to_string(),
            }],
            string_set_mappings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            page_settings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm.running_mappings);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert_eq!(store.instance_count(), 1);
        assert_eq!(store.name_of(0), Some("pageTitle"));
        assert!(store.get_html(0).unwrap().contains("Doc Title"));
    }

    #[test]
    fn test_running_element_pass_no_mappings_is_noop() {
        let html = "<html><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![],
            string_set_mappings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            page_settings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm.running_mappings);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert_eq!(store.instance_count(), 0);
    }

    #[test]
    fn test_running_element_pass_skips_head_elements() {
        let html = r#"<html><head><style id="injected">p { color: red; }</style></head><body>
            <p>Body text</p>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![crate::gcpm::RunningMapping {
                parsed: crate::gcpm::ParsedSelector::Id("injected".to_string()),
                running_name: "shouldNotMatch".to_string(),
            }],
            string_set_mappings: vec![],
            counter_mappings: vec![],
            content_counter_mappings: vec![],
            page_settings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm.running_mappings);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert_eq!(
            store.instance_count(),
            0,
            "Elements inside <head> (like <style>) should not be matched as running elements"
        );
    }

    #[test]
    fn test_link_stylesheet_pass_resolves_local_css() {
        let dir = tempfile::tempdir().unwrap();
        let css_path = dir.path().join("style.css");
        std::fs::write(&css_path, "p { color: red; }").unwrap();

        let html = r#"<html><head><link rel="stylesheet" href="style.css"></head><body><p>Hello</p></body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let pass = LinkStylesheetPass {
            base_path: dir.path().to_path_buf(),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_some(),
            "Expected a <style> element to be injected from <link> stylesheet"
        );
    }

    #[test]
    fn test_link_stylesheet_pass_ignores_https() {
        let html = r#"<html><head><link rel="stylesheet" href="https://example.com/style.css"></head><body><p>Hello</p></body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let pass = LinkStylesheetPass {
            base_path: PathBuf::from("/tmp"),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_none(),
            "Expected no <style> element for https:// link"
        );
    }

    #[test]
    fn test_link_stylesheet_pass_ignores_http() {
        let html = r#"<html><head><link rel="stylesheet" href="http://example.com/style.css"></head><body><p>Hello</p></body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let pass = LinkStylesheetPass {
            base_path: PathBuf::from("/tmp"),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_none(),
            "Expected no <style> element for http:// link"
        );
    }

    #[test]
    fn test_link_stylesheet_pass_ignores_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let html = r#"<html><head><link rel="stylesheet" href="nonexistent.css"></head><body><p>Hello</p></body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let pass = LinkStylesheetPass {
            base_path: dir.path().to_path_buf(),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_none(),
            "Expected no <style> element for missing file"
        );
    }

    #[test]
    fn test_link_stylesheet_pass_multiple_links() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.css"), "p { color: red; }").unwrap();
        std::fs::write(dir.path().join("b.css"), "h1 { font-size: 2em; }").unwrap();

        let html = r#"<html><head><link rel="stylesheet" href="a.css"><link rel="stylesheet" href="b.css"></head><body><p>Hello</p></body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let pass = LinkStylesheetPass {
            base_path: dir.path().to_path_buf(),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);

        // Count <style> elements by walking head children
        let head_id = find_element_by_tag(&doc, "head").unwrap();
        let head_node = doc.get_node(head_id).unwrap();
        let style_count = head_node
            .children
            .iter()
            .filter(|&&cid| {
                doc.get_node(cid)
                    .and_then(|n| n.element_data())
                    .is_some_and(|e| e.name.local.as_ref() == "style")
            })
            .count();
        assert_eq!(
            style_count, 2,
            "Expected 2 <style> elements for 2 CSS files"
        );
    }

    #[test]
    fn test_link_stylesheet_pass_absolute_path_within_base() {
        let dir = tempfile::tempdir().unwrap();
        let css_path = dir.path().join("abs.css");
        std::fs::write(&css_path, "body { margin: 0; }").unwrap();

        let html = format!(
            r#"<html><head><link rel="stylesheet" href="{}"></head><body><p>Hello</p></body></html>"#,
            css_path.display()
        );
        let mut doc = parse(&html, 400.0, &[]);
        // base_path is the same dir, so absolute path is allowed
        let pass = LinkStylesheetPass {
            base_path: dir.path().to_path_buf(),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_some(),
            "Expected a <style> element when using absolute path within base_path"
        );
    }

    #[test]
    fn test_link_stylesheet_pass_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        // Create a file outside the sub directory
        std::fs::write(dir.path().join("secret.css"), "body { color: red; }").unwrap();

        let html = r#"<html><head><link rel="stylesheet" href="../secret.css"></head><body><p>Hello</p></body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let pass = LinkStylesheetPass {
            base_path: sub.clone(),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_none(),
            "Path traversal outside base_path should be rejected"
        );
    }

    #[test]
    fn test_link_stylesheet_pass_rejects_absolute_outside_base() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("evil.css"), "body { color: red; }").unwrap();

        let html = format!(
            r#"<html><head><link rel="stylesheet" href="{}"></head><body><p>Hello</p></body></html>"#,
            other.path().join("evil.css").display()
        );
        let mut doc = parse(&html, 400.0, &[]);
        let pass = LinkStylesheetPass {
            base_path: dir.path().to_path_buf(),
        };
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        apply_passes(&mut doc, &[Box::new(pass)], &ctx);
        resolve(&mut doc);
        assert!(
            find_element_by_tag(&doc, "style").is_none(),
            "Absolute path outside base_path should be rejected"
        );
    }

    #[test]
    fn test_counter_pass_generates_css() {
        use crate::gcpm::{
            ContentCounterMapping, CounterMapping, CounterOp, CounterStyle, PseudoElement,
        };

        let html = r#"<html><body>
            <h2>Chapter One</h2>
            <h2>Chapter Two</h2>
        </body></html>"#;
        let mut doc = parse(html, 400.0, &[]);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };

        let counter_mappings = vec![
            CounterMapping {
                parsed: crate::gcpm::ParsedSelector::Tag("body".into()),
                ops: vec![CounterOp::Reset {
                    name: "chapter".into(),
                    value: 0,
                }],
            },
            CounterMapping {
                parsed: crate::gcpm::ParsedSelector::Tag("h2".into()),
                ops: vec![CounterOp::Increment {
                    name: "chapter".into(),
                    value: 1,
                }],
            },
        ];

        let content_mappings = vec![ContentCounterMapping {
            parsed: crate::gcpm::ParsedSelector::Tag("h2".into()),
            pseudo: PseudoElement::Before,
            content: vec![
                crate::gcpm::ContentItem::Counter {
                    name: "chapter".into(),
                    style: CounterStyle::Decimal,
                },
                crate::gcpm::ContentItem::String(". ".into()),
            ],
        }];

        let pass = CounterPass::new(counter_mappings, content_mappings);
        pass.apply(&mut doc, &ctx);

        let css = pass.generated_css();
        // Should contain resolved values "1. " and "2. "
        assert!(
            css.contains("1. "),
            "CSS should contain resolved '1. ', got: {css}"
        );
        assert!(
            css.contains("2. "),
            "CSS should contain resolved '2. ', got: {css}"
        );

        let (ops_by_node, _) = pass.into_parts();
        // Should have 3 ops: body reset + h2 increment + h2 increment
        assert_eq!(
            ops_by_node.len(),
            3,
            "Should have exactly 3 ops: body reset + 2 h2 increments"
        );
    }
}
