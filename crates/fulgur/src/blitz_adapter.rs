//! Thin adapter over Blitz APIs. All Blitz-specific code is isolated here
//! so that upstream API changes only require changes in this module.

use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_traits::shell::{ColorScheme, Viewport};
use parley::FontContext;
use std::sync::Arc;

/// Suppress stdout during a closure. Blitz's HTML parser unconditionally prints
/// `println!("ERROR: {error}")` for non-fatal parse errors (e.g., "Unexpected token").
/// These are html5ever's error-recovery messages and do not indicate real failures.
fn suppress_stdout<F: FnOnce() -> T, T>(f: F) -> T {
    use std::io::Write;

    // Flush any pending stdout first
    let _ = std::io::stdout().flush();

    // On Unix, redirect fd 1 to /dev/null temporarily
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let devnull = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .ok();
        let saved_fd = devnull.as_ref().map(|_| {
            // dup(1) to save original stdout
            let saved = unsafe { libc::dup(1) };
            if saved < 0 {
                return -1;
            }
            // dup2(devnull_fd, 1) to redirect stdout
            if let Some(ref dn) = devnull {
                unsafe { libc::dup2(dn.as_raw_fd(), 1) };
            }
            saved
        });

        let result = f();

        // Restore original stdout
        if let Some(Some(saved)) = saved_fd.map(|fd| if fd >= 0 { Some(fd) } else { None }) {
            let _ = std::io::stdout().flush();
            unsafe { libc::dup2(saved, 1) };
            unsafe { libc::close(saved) };
        }

        result
    }

    #[cfg(not(unix))]
    {
        f()
    }
}

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

    suppress_stdout(|| HtmlDocument::from_html(html, config))
}

/// Apply a sequence of DOM passes to a parsed document.
pub fn apply_passes(doc: &mut HtmlDocument, passes: &[Box<dyn DomPass>], ctx: &PassContext<'_>) {
    for pass in passes {
        pass.apply(doc, ctx);
    }
}

/// Resolve styles (Stylo) and compute layout (Taffy).
pub fn resolve(doc: &mut HtmlDocument) {
    doc.resolve(0.0);
}

/// Walk the DOM tree to find the first element with the given tag name.
/// Returns the node id if found.
fn find_element_by_tag(doc: &HtmlDocument, tag: &str) -> Option<usize> {
    let root = doc.root_element();
    find_element_by_tag_recursive(doc, root.id, tag)
}

fn find_element_by_tag_recursive(doc: &HtmlDocument, node_id: usize, tag: &str) -> Option<usize> {
    let node = doc.get_node(node_id)?;
    if let Some(el) = node.element_data() {
        if el.name.local.as_ref() == tag {
            return Some(node_id);
        }
    }
    for &child_id in &node.children {
        if let Some(found) = find_element_by_tag_recursive(doc, child_id, tag) {
            return Some(found);
        }
    }
    None
}

fn make_qual_name(local: &str) -> blitz_dom::QualName {
    blitz_dom::QualName::new(
        None,
        blitz_dom::ns!(html),
        blitz_dom::LocalName::from(local),
    )
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

        // Create <style> element with a text node child, then register with Stylo.
        // Note: set_inner_html doesn't work because Blitz uses DummyHtmlParserProvider.
        let style_id = {
            let mut mutator = doc.mutate();
            let style_id = mutator.create_element(make_qual_name("style"), vec![]);
            let text_id = mutator.create_text_node(&self.css);
            mutator.append_children(head_id, &[style_id]);
            mutator.append_children(style_id, &[text_id]);
            style_id
        };
        doc.upsert_stylesheet_for_node(style_id);
    }
}

use crate::gcpm::running::{RunningElementStore, serialize_node};
use crate::gcpm::{GcpmContext, ParsedSelector};
use std::cell::RefCell;

/// Extracts running elements from the DOM and stores their serialized HTML.
pub struct RunningElementPass {
    gcpm: GcpmContext,
    store: RefCell<RunningElementStore>,
}

impl RunningElementPass {
    pub fn new(gcpm: GcpmContext) -> Self {
        Self {
            gcpm,
            store: RefCell::new(RunningElementStore::new()),
        }
    }

    pub fn into_running_store(self) -> RunningElementStore {
        self.store.into_inner()
    }
}

impl DomPass for RunningElementPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.gcpm.running_mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id);
    }
}

impl RunningElementPass {
    fn walk_tree(&self, doc: &HtmlDocument, node_id: usize) {
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if let Some(elem) = node.element_data() {
            // Skip non-visual elements (head, script, style, etc.) to match
            // the old convert.rs behavior and avoid false matches inside <head>.
            if matches!(
                elem.name.local.as_ref(),
                "head" | "script" | "style" | "link" | "meta" | "title" | "noscript"
            ) {
                return;
            }
            if let Some(running_name) = self.find_running_name(elem) {
                let html = serialize_node(doc, node_id);
                self.store.borrow_mut().register(running_name, html);
                return;
            }
        }

        for &child_id in &node.children {
            self.walk_tree(doc, child_id);
        }
    }

    fn find_running_name(&self, elem: &blitz_dom::node::ElementData) -> Option<String> {
        self.gcpm
            .running_mappings
            .iter()
            .find(|m| self.matches_selector(&m.parsed, elem))
            .map(|m| m.running_name.clone())
    }

    fn matches_selector(
        &self,
        selector: &ParsedSelector,
        elem: &blitz_dom::node::ElementData,
    ) -> bool {
        match selector {
            ParsedSelector::Class(name) => elem
                .attrs()
                .iter()
                .find(|a| a.name.local.as_ref() == "class")
                .is_some_and(|a| {
                    let cls: &str = &a.value;
                    cls.split_whitespace().any(|c| c == name.as_str())
                }),
            ParsedSelector::Id(name) => elem
                .attrs()
                .iter()
                .find(|a| a.name.local.as_ref() == "id")
                .is_some_and(|a| {
                    let id: &str = &a.value;
                    id == name.as_str()
                }),
            ParsedSelector::Tag(name) => elem.name.local.as_ref().eq_ignore_ascii_case(name),
        }
    }
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
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert!(
            store.get("pageHeader").is_some(),
            "Expected running element 'pageHeader' to be extracted"
        );
        let html_content = store.get("pageHeader").unwrap();
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
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert!(store.get("pageTitle").is_some());
        assert!(store.get("pageTitle").unwrap().contains("Doc Title"));
    }

    #[test]
    fn test_running_element_pass_no_mappings_is_noop() {
        let html = "<html><body><p>Hello</p></body></html>";
        let mut doc = parse(html, 400.0, &[]);

        let gcpm = crate::gcpm::GcpmContext {
            margin_boxes: vec![],
            running_mappings: vec![],
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert!(store.get("anything").is_none());
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
            cleaned_css: String::new(),
        };

        let pass = RunningElementPass::new(gcpm);
        let ctx = PassContext {
            viewport_width: 400.0,
            viewport_height: 10000.0,
            font_data: &[],
        };
        pass.apply(&mut doc, &ctx);

        let store = pass.into_running_store();
        assert!(
            store.get("shouldNotMatch").is_none(),
            "Elements inside <head> (like <style>) should not be matched as running elements"
        );
    }
}
