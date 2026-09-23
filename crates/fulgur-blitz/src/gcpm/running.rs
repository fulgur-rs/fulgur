//! Running element support for CSS Generated Content for Paged Media (GCPM).
//!
//! Manages running elements extracted from the DOM via `position: running(name)`.
//! These elements are serialized to HTML and stored for later re-layout in margin boxes.

use std::collections::BTreeMap;

/// A single running element assignment, identified by a numeric instance id.
#[derive(Debug, Clone)]
struct RunningInstance {
    name: String,
    html: String,
}

/// Stores running element instances in source order, keyed by a sequential id.
///
/// Multiple assignments to the same running name are preserved as separate
/// instances so per-page policy resolution (first/start/last/first-except)
/// can pick the correct one. The DOM `node_id` → `instance_id` map lets the
/// convert stage emit zero-size markers at the source position of each
/// running element.
///
/// Instances are append-only — once registered, they remain in the store for
/// the lifetime of the pass. This is what allows `instance_id` to be a stable
/// index into `instances`.
pub struct RunningElementStore {
    instances: Vec<RunningInstance>,
    node_to_instance: BTreeMap<usize, usize>,
}

impl RunningElementStore {
    pub fn new() -> Self {
        Self {
            instances: Vec::new(),
            node_to_instance: BTreeMap::new(),
        }
    }

    /// Register a running element instance. Returns the assigned instance_id.
    ///
    /// Invariant: each `node_id` is registered at most once, guaranteed by
    /// `RunningElementPass::walk_tree` not recursing into running element
    /// subtrees.
    pub fn register(&mut self, node_id: usize, name: String, html: String) -> usize {
        let id = self.instances.len();
        self.instances.push(RunningInstance { name, html });
        self.node_to_instance.insert(node_id, id);
        id
    }

    /// Look up the instance_id assigned to a DOM node, if any.
    pub fn instance_for_node(&self, node_id: usize) -> Option<usize> {
        self.node_to_instance.get(&node_id).copied()
    }

    /// Get the serialized HTML for a given instance_id.
    pub fn get_html(&self, instance_id: usize) -> Option<&str> {
        self.instances.get(instance_id).map(|i| i.html.as_str())
    }

    /// Get the running name for a given instance_id.
    pub fn name_of(&self, instance_id: usize) -> Option<&str> {
        self.instances.get(instance_id).map(|i| i.name.as_str())
    }
}

impl Default for RunningElementStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl RunningElementStore {
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }
}

/// Serialize a Blitz DOM node subtree back to an HTML string.
///
/// Used to extract running elements for re-layout in margin boxes.
/// Does not serialize computed styles as inline styles — the CSS cascade
/// handles styling in the re-layout pass.
pub fn serialize_node(doc: &blitz_dom::BaseDocument, node_id: usize) -> String {
    let mut output = String::new();
    write_node(doc, node_id, &mut output, 0, false);
    output
}

fn write_node(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    writer: &mut String,
    depth: usize,
    in_raw_text: bool,
) {
    use crate::MAX_DOM_DEPTH;
    use blitz_dom::NodeData;

    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };

    match &node.data {
        NodeData::Text(text_data) => {
            if in_raw_text {
                writer.push_str(&text_data.content);
            } else {
                escape_text_content(&text_data.content, writer);
            }
        }
        NodeData::Element(elem) => {
            let tag = elem.name.local.as_ref();
            let has_children = !node.children.is_empty();

            writer.push('<');
            writer.push_str(tag);

            for attr in elem.attrs() {
                writer.push(' ');
                writer.push_str(attr.name.local.as_ref());
                writer.push_str("=\"");
                escape_attribute_value(&attr.value, writer);
                writer.push('"');
            }

            if !has_children && accepts_self_closing(elem) {
                writer.push_str(" />");
            } else {
                writer.push('>');
                let children_are_raw_text = is_raw_text_element(elem);
                for &child_id in &node.children {
                    write_node(doc, child_id, writer, depth + 1, children_are_raw_text);
                }
                writer.push_str("</");
                writer.push_str(tag);
                writer.push('>');
            }
        }
        _ => {
            // Document, Comment, AnonymousBlock — skip
        }
    }
}

/// Elements whose children the HTML tokenizer reads **without** decoding
/// character references — `<style>` / `<script>` (raw text, HTML §13.2.5.1),
/// the elements the tree builder routes through generic raw text parsing
/// (`<xmp>`, `<iframe>`, `<noembed>`, `<noframes>`), and `<plaintext>`, which
/// switches the tokenizer to PLAINTEXT for the rest of the input.
///
/// For their children a verbatim write-back already is the parser's inverse.
/// Escaping would corrupt them instead of preserving them: `.a > b` inside a
/// `<style>` would come back as `.a &gt; b` and be handed to Stylo that way,
/// and `A &amp; B` inside an `<xmp>` — which never got decoded — would be
/// re-encoded to `A &amp;amp; B` and rendered with the extra entity visible.
///
/// The namespace check matters: `<style>` in the SVG namespace is *not* raw
/// text — foreign content decodes references normally — so it takes the
/// escaping path like any other element.
///
/// Deliberately absent, all verified against blitz's parser rather than
/// assumed: `<textarea>` and `<title>` are *escapable* raw text, where the
/// tokenizer does decode references; `<noscript>` only becomes raw text when
/// scripting is enabled, and blitz parses with it disabled. All three need the
/// same escaping as ordinary text.
///
/// `<plaintext>` cannot round-trip at all — it consumes everything to EOF, so
/// the end tag written after it reparses as more text — but it belongs here
/// regardless: not double-encoding its contents is strictly closer to the
/// input than escaping them.
fn is_raw_text_element(elem: &blitz_dom::node::ElementData) -> bool {
    elem.name.ns == blitz_dom::ns!(html)
        && matches!(
            elem.name.local.as_ref(),
            "style" | "script" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext"
        )
}

/// Whether a childless element may be written as `<tag />`.
///
/// Only HTML void elements and foreign content (SVG / MathML) may. The HTML
/// tokenizer ignores the trailing solidus on any other HTML start tag, so
/// `<div />` reparses as an *open* `<div>` that swallows every following
/// sibling as a child. For a raw text element the damage is worse:
/// `<style />` reparses as an open `<style>` and the rest of the margin box
/// becomes CSS text — an empty `<style>` in a running element used to erase
/// the header it sat in.
///
/// Foreign content is the exception the spec grants: the solidus *is*
/// honoured there, so `<path />` round-trips.
fn accepts_self_closing(elem: &blitz_dom::node::ElementData) -> bool {
    if elem.name.ns != blitz_dom::ns!(html) {
        return true;
    }
    matches!(
        elem.name.local.as_ref(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Escape a text node's content for safe HTML embedding.
///
/// The HTML parser decodes character references on the way in, so writing a
/// text node back verbatim is **not** its inverse: content that reached the
/// DOM as the escaped string `&lt;a href="…"&gt;` would be handed to the
/// margin-box reparse as a live anchor. That turns an escaped-by-construction
/// value — e.g. a `{{ variable }}` run through the forced
/// `AutoEscape::Html` of [`crate::template`] — back into markup, defeating
/// the escaping. Re-encoding here restores the round trip.
///
/// `>` is not strictly required in a text context, but is escaped anyway to
/// match [`escape_attribute_value`] and to neutralise `]]>` sequences.
fn escape_text_content(value: &str, writer: &mut String) {
    for ch in value.chars() {
        match ch {
            '&' => writer.push_str("&amp;"),
            '<' => writer.push_str("&lt;"),
            '>' => writer.push_str("&gt;"),
            _ => writer.push(ch),
        }
    }
}

/// Escape attribute values for safe HTML embedding.
fn escape_attribute_value(value: &str, writer: &mut String) {
    for ch in value.chars() {
        match ch {
            '&' => writer.push_str("&amp;"),
            '"' => writer.push_str("&quot;"),
            '<' => writer.push_str("&lt;"),
            '>' => writer.push_str("&gt;"),
            _ => writer.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_running_store_instance_registration() {
        let mut store = RunningElementStore::new();
        let id_a = store.register(10, "header".to_string(), "<h1>A</h1>".to_string());
        let id_b = store.register(20, "header".to_string(), "<h1>B</h1>".to_string());

        assert_ne!(id_a, id_b);
        assert_eq!(store.get_html(id_a), Some("<h1>A</h1>"));
        assert_eq!(store.get_html(id_b), Some("<h1>B</h1>"));
        assert_eq!(store.instance_for_node(10), Some(id_a));
        assert_eq!(store.instance_for_node(20), Some(id_b));
        assert_eq!(store.instance_for_node(99), None);
    }

    #[test]
    fn test_running_store_name_lookup() {
        let mut store = RunningElementStore::new();
        let id = store.register(5, "footer".to_string(), "<p>F</p>".to_string());
        assert_eq!(store.name_of(id), Some("footer"));
    }

    #[test]
    fn test_running_store_invalid_instance_id_returns_none() {
        let store = RunningElementStore::new();
        assert!(store.get_html(999).is_none());
        assert!(store.name_of(999).is_none());
    }

    #[test]
    fn default_gives_empty_store() {
        let store = RunningElementStore::default();
        assert_eq!(store.instance_count(), 0);
        assert_eq!(store.instance_for_node(0), None);
    }

    #[test]
    fn instance_count_tracks_registrations() {
        let mut store = RunningElementStore::new();
        assert_eq!(store.instance_count(), 0);
        store.register(1, "header".to_string(), "<h1>A</h1>".to_string());
        assert_eq!(store.instance_count(), 1);
        store.register(2, "header".to_string(), "<h1>B</h1>".to_string());
        assert_eq!(store.instance_count(), 2);
    }

    // --- serialize_node round trip ---

    /// Parse `body_html` and serialize the first `<div>` in it, i.e. the same
    /// shape `RunningElementPass` hands to `serialize_node`.
    fn serialize_first_div(body_html: &str) -> String {
        let html = format!("<html><body>{body_html}</body></html>");
        let doc = crate::blitz_adapter::parse(&html, 600.0, &[]);
        let root_id = doc.root_element().id;
        let div_id = find_first_tag(&doc, root_id, "div", 0).expect("no <div> in fixture");
        serialize_node(&doc, div_id)
    }

    fn find_first_tag(
        doc: &blitz_dom::BaseDocument,
        node_id: usize,
        tag: &str,
        depth: usize,
    ) -> Option<usize> {
        if depth >= crate::MAX_DOM_DEPTH {
            return None;
        }
        let node = doc.get_node(node_id)?;
        if let Some(elem) = node.element_data() {
            if elem.name.local.as_ref() == tag {
                return Some(node_id);
            }
        }
        node.children
            .iter()
            .find_map(|&child| find_first_tag(doc, child, tag, depth + 1))
    }

    /// Text that reached the DOM as escaped markup — the shape produced by
    /// `template::render_template`'s forced `AutoEscape::Html` — must be
    /// handed back as text, not as live markup for the margin-box reparse.
    #[test]
    fn serialize_node_re_escapes_markup_in_text_nodes() {
        let out = serialize_first_div(
            r#"<div>Report for &lt;a href=&quot;javascript:alert(1)&quot;&gt;x&lt;/a&gt;</div>"#,
        );
        assert!(
            !out.contains("<a "),
            "escaped anchor was resurrected as markup: {out}"
        );
        assert!(
            out.contains("&lt;a href=") && out.contains("&gt;x&lt;/a&gt;"),
            "anchor text was not re-escaped: {out}"
        );
    }

    #[test]
    fn serialize_node_escapes_bare_ampersand_in_text() {
        let out = serialize_first_div("<div>Tom &amp; Jerry</div>");
        assert!(out.contains("Tom &amp; Jerry"), "{out}");
    }

    /// Authored elements are DOM elements, not text — escaping text nodes
    /// must leave them alone, or legitimate running-element markup breaks.
    #[test]
    fn serialize_node_keeps_authored_elements_intact() {
        let out =
            serialize_first_div(r#"<div><b>bold</b> <a href="https://ok.test">link</a></div>"#);
        assert!(out.contains("<b>bold</b>"), "{out}");
        assert!(
            out.contains(r#"<a href="https://ok.test">link</a>"#),
            "{out}"
        );
    }

    /// `<tag />` only reparses as an empty element for void elements. Any
    /// other childless HTML element must get an explicit end tag or the
    /// reparse treats it as open and nests its following siblings inside it.
    #[test]
    fn serialize_node_closes_childless_non_void_elements() {
        let out = serialize_first_div("<div><span></span><b>X</b></div>");
        assert!(out.contains("<span></span>"), "{out}");
        assert!(out.contains("<b>X</b>"), "{out}");
        assert!(!out.contains("<span />"), "{out}");
    }

    /// The raw text variant of the above: `<style />` would make the reparse
    /// swallow everything after it as CSS text.
    #[test]
    fn serialize_node_closes_empty_style_element() {
        let out = serialize_first_div("<div><style></style><b>X</b></div>");
        assert!(out.contains("<style></style>"), "{out}");
        assert!(!out.contains("<style />"), "{out}");
    }

    #[test]
    fn serialize_node_keeps_void_elements_self_closing() {
        let out = serialize_first_div("<div>a<br>b</div>");
        assert!(out.contains("<br />"), "{out}");
    }

    /// The tree builder routes several more elements through raw text
    /// parsing, so their contents were never decoded either and must not be
    /// re-encoded. Escaping `<xmp>A &amp; B</xmp>` would render the entity
    /// itself in the header.
    #[test]
    fn serialize_node_keeps_every_raw_text_mode_verbatim() {
        for tag in ["style", "script", "xmp", "iframe", "noembed", "noframes"] {
            let out = serialize_first_div(&format!("<div><{tag}>A &amp; B</{tag}></div>"));
            assert!(
                out.contains(&format!("<{tag}>A &amp; B</{tag}>")),
                "{tag} content was re-encoded: {out}"
            );
        }
    }

    /// The counterpart: *escapable* raw text and, with scripting disabled,
    /// `<noscript>` all get decoded on the way in, so they take the escaping
    /// path like ordinary text.
    #[test]
    fn serialize_node_escapes_escapable_raw_text_elements() {
        for tag in ["textarea", "title", "noscript"] {
            let out = serialize_first_div(&format!("<div><{tag}>A &amp; B</{tag}></div>"));
            assert!(
                out.contains(&format!("<{tag}>A &amp; B</{tag}>")),
                "{tag} content lost its escaping: {out}"
            );
        }
    }

    /// Foreign content is the other side of both namespace checks: SVG
    /// children may self-close, and SVG `<style>` is *not* raw text — the
    /// parser decoded its references, so re-encoding them is the correct
    /// inverse and the foreign-content reparse decodes them again.
    #[test]
    fn serialize_node_handles_foreign_content() {
        let out = serialize_first_div(
            r#"<div><svg viewBox="0 0 10 10"><circle r="4"/><style>circle &gt; * {fill:red}</style></svg></div>"#,
        );
        assert!(out.contains(r#"<circle r="4" />"#), "{out}");
        assert!(
            out.contains("<style>circle &gt; * {fill:red}</style>"),
            "{out}"
        );
    }

    /// Over-escaping tripwire: `<style>` is a raw text element, so the parser
    /// never decoded references inside it and re-encoding would corrupt the
    /// selector (`.a > b` → `.a &gt; b`).
    #[test]
    fn serialize_node_keeps_raw_text_of_style_element_verbatim() {
        let out = serialize_first_div("<div><style>.a > b { color: red }</style><b>x</b></div>");
        assert!(
            out.contains(".a > b { color: red }"),
            "style CSS was corrupted by escaping: {out}"
        );
    }

    // --- escape_text_content ---

    fn escape_text(s: &str) -> String {
        let mut out = String::new();
        escape_text_content(s, &mut out);
        out
    }

    #[test]
    fn escape_text_content_escapes_markup_specials() {
        assert_eq!(escape_text("a<b>c"), "a&lt;b&gt;c");
        assert_eq!(escape_text("a&b"), "a&amp;b");
        assert_eq!(escape_text("&<>"), "&amp;&lt;&gt;");
    }

    #[test]
    fn escape_text_content_leaves_quotes_and_plain_text_alone() {
        // Text context — quotes need no escaping there.
        assert_eq!(escape_text(r#"say "hi" 123"#), r#"say "hi" 123"#);
        assert_eq!(escape_text(""), "");
    }

    // --- escape_attribute_value ---

    fn escape(s: &str) -> String {
        let mut out = String::new();
        escape_attribute_value(s, &mut out);
        out
    }

    #[test]
    fn escape_attribute_value_ampersand() {
        assert_eq!(escape("a&b"), "a&amp;b");
        assert_eq!(escape("&&"), "&amp;&amp;");
    }

    #[test]
    fn escape_attribute_value_double_quote() {
        assert_eq!(escape("say \"hi\""), "say &quot;hi&quot;");
    }

    #[test]
    fn escape_attribute_value_angle_brackets() {
        assert_eq!(escape("a<b>c"), "a&lt;b&gt;c");
        assert_eq!(escape("<>"), "&lt;&gt;");
    }

    #[test]
    fn escape_attribute_value_passthrough_regular_chars() {
        assert_eq!(escape("hello world!"), "hello world!");
        assert_eq!(escape("abc123"), "abc123");
    }

    #[test]
    fn escape_attribute_value_empty() {
        assert_eq!(escape(""), "");
    }

    #[test]
    fn escape_attribute_value_all_specials_combined() {
        assert_eq!(escape("&\"<>"), "&amp;&quot;&lt;&gt;");
    }
}
