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
use blitz_dom::net::Resource;
use blitz_html::HtmlDocument;
use blitz_traits::net::{NetProvider, Url};
use blitz_traits::shell::{ColorScheme, Viewport};
use parley::FontContext;
use std::path::Path;
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
///
/// Uses Blitz's built-in `DummyNetProvider`, so `<link rel="stylesheet">`
/// and `@import` are silently ignored. For real-world rendering call
/// [`parse_html_with_local_resources`] instead, which wires fulgur's
/// own [`crate::net::FulgurNetProvider`] into Blitz.
pub fn parse(html: &str, viewport_width: f32, font_data: &[Arc<Vec<u8>>]) -> HtmlDocument {
    parse_inner(html, viewport_width, font_data, None, None)
}

/// Parse HTML and load any `<link rel="stylesheet">` / `@import` files
/// found inside the configured `base_path`.
///
/// Returns the parsed document plus a [`crate::gcpm::GcpmContext`]
/// containing every GCPM construct extracted from the loaded
/// stylesheets. The caller is expected to merge that context into its
/// own engine-level context (typically the one derived from `--css`).
///
/// This is the **only** entry point engine code should use for
/// production rendering: it owns construction of
/// [`crate::net::FulgurNetProvider`], the trait-object cast,
/// `base_path → file://` URL derivation, and resource draining,
/// keeping the Blitz API surface fully encapsulated in
/// `blitz_adapter.rs` (CLAUDE.md adapter-isolation rule).
pub fn parse_html_with_local_resources(
    html: &str,
    viewport_width: f32,
    font_data: &[Arc<Vec<u8>>],
    base_path: Option<&Path>,
) -> (HtmlDocument, crate::gcpm::GcpmContext) {
    let net_provider = Arc::new(crate::net::FulgurNetProvider::new(
        base_path.map(|p| p.to_path_buf()),
    ));
    let provider: Arc<dyn NetProvider<Resource>> = net_provider.clone();
    let base_url = base_path
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| Url::from_directory_path(&p).ok())
        .map(|u| u.to_string());

    let mut doc = parse_inner(html, viewport_width, font_data, Some(provider), base_url);

    // Replay every Resource (= parsed stylesheet) that the provider
    // captured during HTML parsing into the document so the stylist
    // sees them before `resolve()`.
    for resource in net_provider.drain_pending_resources() {
        doc.load_resource(resource);
    }

    // Fold the per-stylesheet GCPM contexts into one. The caller still
    // has to merge this with its own AssetBundle-derived context.
    let mut gcpm = crate::gcpm::GcpmContext::default();
    for ctx in net_provider.drain_gcpm_contexts() {
        gcpm.extend_from(ctx);
    }
    (doc, gcpm)
}

/// The single primitive that actually constructs an `HtmlDocument`.
/// All other `parse*` functions in this module funnel through here.
fn parse_inner(
    html: &str,
    viewport_width: f32,
    font_data: &[Arc<Vec<u8>>],
    net_provider: Option<Arc<dyn NetProvider<Resource>>>,
    base_url: Option<String>,
) -> HtmlDocument {
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
        base_url: Some(base_url.unwrap_or_else(|| "file:///".to_string())),
        net_provider,
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

/// Inspect a pseudo-element node's computed `content` property and return the
/// first `Image` variant's URL as an owned `String` if the content is a single
/// `url(...)` / `image-set(url(...))` item.
///
/// This exists because `blitz-dom` 0.2.4 does not materialize `content: url(...)`
/// into a child image node — the match arm in
/// `blitz-dom/src/layout/construct.rs` for non-`String` ContentItem variants is
/// a TODO. fulgur bypasses that by reading the stylo computed value directly
/// and constructing an `ImagePageable` itself (see `convert::build_pseudo_image`).
///
/// Scope: only single-item content is matched (per the fulgur-ai3 design scope
/// — multi-item content that mixes text + image is out-of-scope). The URL is
/// returned owned because `primary_styles()` yields a short-lived borrow guard
/// that cannot outlive this function; callers normalize it (e.g. via
/// `convert::extract_asset_name`) before querying `AssetBundle`.
pub fn extract_pseudo_image_url(node: &blitz_dom::Node) -> Option<String> {
    use style::values::generics::counters::{Content, ContentItem};
    let styles = node.primary_styles()?;
    let content = &styles.get_counters().content;
    let item_data = match content {
        Content::Items(item_data) => item_data,
        _ => return None,
    };
    // Only inspect the "main" items (before `alt_start`). Content after
    // `alt_start` is alt-text in CSS Level 3 Content.
    let main = &item_data.items[..item_data.alt_start];
    if main.len() != 1 {
        return None;
    }
    match &main[0] {
        ContentItem::Image(img) => extract_url_from_stylo_image(img).map(|s| s.to_string()),
        _ => None,
    }
}

/// Unwrap a `style::values::computed::image::Image` into a URL string when it
/// is an `Image::Url(ComputedUrl)` — or `image-set(...)` that selects one.
///
/// `image-set(...)` note: stylo tracks `selected_index` on `GenericImageSet`,
/// picking a specific candidate at computed-value time based on device pixel
/// ratio. We follow that index and then recurse once into the selected item's
/// own `Image`. Stylo does not produce nested image-sets in practice, so a
/// shallow recursion is sufficient.
fn extract_url_from_stylo_image(image: &style::values::computed::image::Image) -> Option<&str> {
    use style::servo::url::ComputedUrl;
    use style::values::generics::image::Image as StyloImage;
    match image {
        StyloImage::Url(ComputedUrl::Valid(url)) => Some(url.as_str()),
        StyloImage::Url(ComputedUrl::Invalid(s)) => Some(s.as_str()),
        StyloImage::ImageSet(image_set) => {
            let idx = image_set.selected_index;
            let item = image_set.items.get(idx)?;
            extract_url_from_stylo_image(&item.image)
        }
        _ => None,
    }
}

/// Extract the CSS `vertical-align` value from a node's computed styles and
/// map it to fulgur's `VerticalAlign` enum.
pub fn extract_vertical_align(node: &blitz_dom::Node) -> crate::paragraph::VerticalAlign {
    use crate::paragraph::VerticalAlign;
    let Some(styles) = node.primary_styles() else {
        return VerticalAlign::Baseline;
    };
    let va = styles.clone_vertical_align();
    match va {
        style::values::generics::box_::VerticalAlign::Keyword(kw) => {
            use style::values::generics::box_::VerticalAlignKeyword;
            match kw {
                VerticalAlignKeyword::Baseline => VerticalAlign::Baseline,
                VerticalAlignKeyword::Middle => VerticalAlign::Middle,
                VerticalAlignKeyword::Top => VerticalAlign::Top,
                VerticalAlignKeyword::Bottom => VerticalAlign::Bottom,
                VerticalAlignKeyword::Sub => VerticalAlign::Sub,
                VerticalAlignKeyword::Super => VerticalAlign::Super,
                VerticalAlignKeyword::TextTop => VerticalAlign::TextTop,
                VerticalAlignKeyword::TextBottom => VerticalAlign::TextBottom,
                #[allow(unreachable_patterns)]
                _ => VerticalAlign::Baseline,
            }
        }
        style::values::generics::box_::VerticalAlign::Length(lp) => {
            if let Some(pct) = lp.to_percentage() {
                VerticalAlign::Percent(pct.0)
            } else {
                // Note: for calc() mixed values like `calc(10px + 50%)`,
                // to_percentage() returns None and we resolve with basis=0,
                // which drops the percentage component. This is a known
                // limitation — calc() in vertical-align is extremely rare.
                VerticalAlign::Length(lp.resolve(style::values::computed::Length::new(0.0)).px())
            }
        }
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

// ─── transform support ────────────────────────────────────

use crate::pageable::{Affine2D, Point2};

/// Read the computed `transform` and `transform-origin` from `styles` and
/// fold the `TransformOperation` list into a single pre-resolved affine
/// matrix.
///
/// Percentages in `translate` and `transform-origin` are resolved against
/// the caller-supplied `border_box_width` / `border_box_height` (Taffy's
/// final layout size — CSS `transform` does not affect layout, so this is
/// correct).
///
/// Returns `None` if the transform is absent, folds to identity, or
/// produces a non-finite matrix. Callers use this to suppress wrapper
/// construction in the fast path.
///
/// 3D operations (`translate3d`, `rotate3d`, `scale3d`, `matrix3d`,
/// `perspective`, etc.) are treated as identity with a `log::warn!`.
/// fulgur is a 2D PDF renderer.
pub(crate) fn compute_transform(
    styles: &style::properties::ComputedValues,
    border_box_width: f32,
    border_box_height: f32,
) -> Option<(Affine2D, Point2)> {
    use style::values::computed::Length;

    // Fast path: most DOM nodes have no transform. Reading the
    // `OwnedSlice` through `get_box()` avoids cloning it for the empty
    // case, and lets the non-empty path borrow instead of clone.
    let transform = &styles.get_box().transform.0;
    if transform.is_empty() {
        return None;
    }

    let mut m = Affine2D::IDENTITY;
    for op in transform.iter() {
        m = m * op_to_matrix(op, border_box_width, border_box_height);
    }

    if !m.a.is_finite()
        || !m.b.is_finite()
        || !m.c.is_finite()
        || !m.d.is_finite()
        || !m.e.is_finite()
        || !m.f.is_finite()
    {
        log::warn!("transform produced non-finite matrix; falling back to identity");
        return None;
    }
    if m.is_identity() {
        return None;
    }

    let origin = styles.clone_transform_origin();
    let origin_x = origin
        .horizontal
        .resolve(Length::new(border_box_width))
        .px();
    let origin_y = origin.vertical.resolve(Length::new(border_box_height)).px();

    Some((m, Point2::new(origin_x, origin_y)))
}

fn op_to_matrix(
    op: &style::values::computed::transform::TransformOperation,
    w: f32,
    h: f32,
) -> Affine2D {
    use style::values::computed::Length;
    use style::values::generics::transform::GenericTransformOperation::*;

    match op {
        Matrix(m) => Affine2D {
            a: m.a,
            b: m.b,
            c: m.c,
            d: m.d,
            e: m.e,
            f: m.f,
        },
        Translate(x, y) => Affine2D::translation(
            x.resolve(Length::new(w)).px(),
            y.resolve(Length::new(h)).px(),
        ),
        TranslateX(x) => Affine2D::translation(x.resolve(Length::new(w)).px(), 0.0),
        TranslateY(y) => Affine2D::translation(0.0, y.resolve(Length::new(h)).px()),
        Scale(sx, sy) => Affine2D::scale(*sx, *sy),
        ScaleX(sx) => Affine2D::scale(*sx, 1.0),
        ScaleY(sy) => Affine2D::scale(1.0, *sy),
        Rotate(angle) | RotateZ(angle) => Affine2D::rotation(angle.radians()),
        Skew(ax, ay) => Affine2D::skew(ax.radians(), ay.radians()),
        SkewX(ax) => Affine2D::skew(ax.radians(), 0.0),
        SkewY(ay) => Affine2D::skew(0.0, ay.radians()),
        Matrix3D(_)
        | Translate3D(..)
        | TranslateZ(_)
        | Scale3D(..)
        | ScaleZ(_)
        | Rotate3D(..)
        | RotateX(_)
        | RotateY(_)
        | Perspective(_)
        | InterpolateMatrix { .. }
        | AccumulateMatrix { .. } => {
            log::warn!("unsupported 3D/animation transform op: {:?}", op);
            Affine2D::IDENTITY
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_html_with_local_resources_orders_imports_before_parent() {
        // CSS cascade: `@import "child.css"` in parent.css must be
        // treated as if child.css were inlined at the top of parent.css,
        // so the parent's *own* rules override the imported ones when
        // they have the same specificity. The merged `cleaned_css` that
        // comes back from `parse_html_with_local_resources` feeds the
        // Pass-2 margin-box renderer, so the ordering there must match:
        // child rules first, parent rules last (so that later rules win).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("parent.css"),
            r#"@import "child.css"; .parent-rule { color: red; }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("child.css"),
            r#".child-rule { color: blue; }"#,
        )
        .unwrap();

        let html = r#"<!DOCTYPE html>
<html><head><link rel="stylesheet" href="parent.css"></head>
<body><p class="parent-rule child-rule">x</p></body></html>"#;

        let (_doc, gcpm) = parse_html_with_local_resources(html, 400.0, &[], Some(dir.path()));

        let cleaned = &gcpm.cleaned_css;
        let child_pos = cleaned
            .find(".child-rule")
            .expect("child.css content should be in cleaned_css");
        let parent_pos = cleaned
            .find(".parent-rule")
            .expect("parent.css content should be in cleaned_css");
        assert!(
            child_pos < parent_pos,
            "child @import rules must come before parent's own rules in cleaned_css \
             to preserve CSS cascade. child at {child_pos}, parent at {parent_pos}.\n\
             cleaned_css:\n{cleaned}"
        );
    }

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

    // NOTE: The previous LinkStylesheetPass tests have been removed.
    // <link rel="stylesheet"> resolution now happens via Blitz's own
    // loader, driven by `crate::net::FulgurNetProvider`. Path-traversal,
    // http(s) rejection and missing-file behaviour are tested in
    // `crates/fulgur/src/net.rs`.

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

    /// Walk the DOM tree to find the first element with the given local name.
    /// Used by pseudo-content tests below.
    fn find_element_by_local_name(doc: &HtmlDocument, name: &str) -> Option<usize> {
        fn walk(doc: &blitz_dom::BaseDocument, id: usize, name: &str) -> Option<usize> {
            let node = doc.get_node(id)?;
            if let Some(ed) = node.element_data() {
                if ed.name.local.as_ref() == name {
                    return Some(id);
                }
            }
            for &c in &node.children {
                if let Some(v) = walk(doc, c, name) {
                    return Some(v);
                }
            }
            None
        }
        use std::ops::Deref;
        walk(doc.deref(), doc.root_element().id, name)
    }

    #[test]
    fn test_extract_pseudo_image_url_simple() {
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: url("logo.png"); }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = parse(html, 800.0, &[]);
        resolve(&mut doc);
        let h1_id = find_element_by_local_name(&doc, "h1").expect("h1");
        let before_id = doc
            .get_node(h1_id)
            .unwrap()
            .before
            .expect("::before pseudo");
        let url = extract_pseudo_image_url(doc.get_node(before_id).unwrap());
        assert!(url.is_some(), "expected Some(url), got None");
        let url = url.unwrap();
        assert!(url.ends_with("logo.png"), "unexpected url: {url}");
    }

    #[test]
    fn test_extract_pseudo_image_url_returns_none_for_string_content() {
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: "prefix "; }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = parse(html, 800.0, &[]);
        resolve(&mut doc);
        let h1_id = find_element_by_local_name(&doc, "h1").expect("h1");
        let before_id = doc
            .get_node(h1_id)
            .unwrap()
            .before
            .expect("::before pseudo");
        assert!(
            extract_pseudo_image_url(doc.get_node(before_id).unwrap()).is_none(),
            "string content should not return a url"
        );
    }

    #[test]
    fn test_extract_pseudo_image_url_image_set() {
        // image-set(url(...) 1x) should resolve to the same URL after stylo
        // picks the selected candidate.
        let html = r#"<!doctype html><html><head><style>
            h1::before { content: image-set(url("hi.png") 1x); }
        </style></head><body><h1>T</h1></body></html>"#;
        let mut doc = parse(html, 800.0, &[]);
        resolve(&mut doc);
        let h1_id = find_element_by_local_name(&doc, "h1").expect("h1");
        let before_id = doc
            .get_node(h1_id)
            .unwrap()
            .before
            .expect("::before pseudo");
        let url = extract_pseudo_image_url(doc.get_node(before_id).unwrap());
        assert!(url.is_some(), "expected Some from image-set, got None");
        assert!(
            url.unwrap().ends_with("hi.png"),
            "image-set should resolve to the selected url"
        );
    }
}

#[cfg(test)]
mod transform_tests {
    use super::*;
    use crate::pageable::{Affine2D, Point2, matrix_test_util::approx};

    /// Parse a minimal HTML snippet and return the computed transform of
    /// the first `<div>` it contains, via `compute_transform()`.
    fn compute_for_div(html: &str, box_w: f32, box_h: f32) -> Option<(Affine2D, Point2)> {
        let doc = parse_and_layout(html, 400.0, 2000.0, &[]);
        let div_id = find_element_by_tag(&doc, "div")?;
        let node = doc.get_node(div_id)?;
        let styles = node.primary_styles()?;
        compute_transform(&styles, box_w, box_h)
    }

    #[test]
    fn no_transform_returns_none() {
        let html = r#"<!DOCTYPE html><html><body><div>hi</div></body></html>"#;
        assert!(compute_for_div(html, 100.0, 100.0).is_none());
    }

    #[test]
    fn translate_px_returns_translation_matrix() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(10px, 20px)">hi</div>
        </body></html>"#;
        let (m, _) = compute_for_div(html, 100.0, 100.0).expect("should have transform");
        assert!(approx(m.e, 10.0));
        assert!(approx(m.f, 20.0));
        assert!(approx(m.a, 1.0));
        assert!(approx(m.d, 1.0));
    }

    #[test]
    fn translate_percent_resolves_against_border_box() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(50%, 25%)">hi</div>
        </body></html>"#;
        let (m, _) = compute_for_div(html, 200.0, 80.0).expect("should have transform");
        assert!(approx(m.e, 100.0), "expected 100 (50% of 200), got {}", m.e);
        assert!(approx(m.f, 20.0), "expected 20 (25% of 80), got {}", m.f);
    }

    #[test]
    fn matrix_is_preserved_verbatim() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: matrix(1, 2, 3, 4, 5, 6)">hi</div>
        </body></html>"#;
        let (m, _) = compute_for_div(html, 100.0, 100.0).expect("should have transform");
        assert!(approx(m.a, 1.0));
        assert!(approx(m.b, 2.0));
        assert!(approx(m.c, 3.0));
        assert!(approx(m.d, 4.0));
        assert!(approx(m.e, 5.0));
        assert!(approx(m.f, 6.0));
    }

    #[test]
    fn origin_default_is_center() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: rotate(45deg)">hi</div>
        </body></html>"#;
        let (_, origin) = compute_for_div(html, 100.0, 60.0).expect("should have transform");
        assert!(
            approx(origin.x, 50.0),
            "default origin x should be 50% of 100, got {}",
            origin.x
        );
        assert!(
            approx(origin.y, 30.0),
            "default origin y should be 50% of 60, got {}",
            origin.y
        );
    }

    #[test]
    fn identity_transform_returns_none() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(0, 0)">hi</div>
        </body></html>"#;
        assert!(compute_for_div(html, 100.0, 100.0).is_none());
    }

    #[test]
    fn three_d_op_folds_to_identity_and_is_suppressed() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate3d(0, 0, 50px)">hi</div>
        </body></html>"#;
        assert!(compute_for_div(html, 100.0, 100.0).is_none());
    }

    #[test]
    fn rotate_z_is_treated_as_2d_rotation() {
        // CSS spec: rotateZ(angle) is equivalent to rotate(angle).
        // Both must produce the same 2D rotation matrix, not fall back
        // to identity through the 3D arm.
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: rotateZ(90deg); transform-origin: 0 0">hi</div>
        </body></html>"#;
        let (m, _) = compute_for_div(html, 100.0, 100.0).expect("rotateZ should produce a wrapper");
        // 90° rotation: (1, 0) → (0, 1).
        let x = m.a * 1.0 + m.c * 0.0 + m.e;
        let y = m.b * 1.0 + m.d * 0.0 + m.f;
        assert!(approx(x, 0.0), "x expected 0.0, got {x}");
        assert!(approx(y, 1.0), "y expected 1.0, got {y}");
    }
}
