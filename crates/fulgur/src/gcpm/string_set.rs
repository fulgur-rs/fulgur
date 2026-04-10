//! Named string support for CSS Generated Content for Paged Media (GCPM).
//!
//! Manages string-set values extracted from the DOM via `string-set: name content(text)`.
//! Values are stored with their DOM node IDs for later insertion into the Pageable tree.

/// A single string-set entry extracted from the DOM.
#[derive(Debug, Clone)]
pub struct StringSetEntry {
    /// The named string identifier (e.g. "chapter-title").
    pub name: String,
    /// The resolved text value.
    pub value: String,
    /// Blitz DOM node ID, used to position the marker in the Pageable tree.
    pub node_id: usize,
}

/// Stores string-set entries collected during DOM traversal.
pub struct StringSetStore {
    entries: Vec<StringSetEntry>,
}

impl StringSetStore {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: StringSetEntry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[StringSetEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for StringSetStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the text content of a DOM subtree.
///
/// Runs of ASCII whitespace are collapsed to a single space and leading/
/// trailing whitespace is trimmed, matching CSS's default white-space handling.
/// Without this, indented templates like
/// `<h1>\n    Chapter 1\n  </h1>` would produce a named string with stray
/// newlines and indentation.
pub fn extract_text_content(doc: &blitz_dom::BaseDocument, node_id: usize) -> String {
    let mut raw = String::new();
    collect_text(doc, node_id, &mut raw, 0);
    normalize_whitespace(&raw)
}

fn collect_text(doc: &blitz_dom::BaseDocument, node_id: usize, out: &mut String, depth: usize) {
    use crate::MAX_DOM_DEPTH;

    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    // Skip non-rendered subtrees so <script>/<style> bodies don't leak into
    // named strings when a broad selector (e.g. `body`) is used as the
    // string-set target.
    if let Some(elem) = node.element_data() {
        if matches!(
            elem.name.local.as_ref(),
            "head" | "script" | "style" | "link" | "meta" | "title" | "noscript"
        ) {
            return;
        }
    }
    match &node.data {
        blitz_dom::NodeData::Text(text_data) => out.push_str(&text_data.content),
        _ => {
            for &child_id in &node.children {
                collect_text(doc, child_id, out, depth + 1);
            }
        }
    }
}

fn normalize_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_space = false;
    for ch in input.chars() {
        if ch.is_ascii_whitespace() {
            in_space = true;
        } else {
            if in_space && !out.is_empty() {
                out.push(' ');
            }
            out.push(ch);
            in_space = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_set_store_basic() {
        let mut store = StringSetStore::new();
        assert!(store.is_empty());
        store.push(StringSetEntry {
            name: "title".into(),
            value: "Chapter 1".into(),
            node_id: 42,
        });
        assert!(!store.is_empty());
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.entries()[0].name, "title");
    }

    #[test]
    fn test_string_set_store_multiple() {
        let mut store = StringSetStore::new();
        store.push(StringSetEntry {
            name: "title".into(),
            value: "Ch1".into(),
            node_id: 10,
        });
        store.push(StringSetEntry {
            name: "title".into(),
            value: "Ch2".into(),
            node_id: 20,
        });
        store.push(StringSetEntry {
            name: "section".into(),
            value: "Intro".into(),
            node_id: 30,
        });
        assert_eq!(store.entries().len(), 3);
    }

    #[test]
    fn test_normalize_whitespace_collapses_runs() {
        assert_eq!(normalize_whitespace("Chapter  1"), "Chapter 1");
        assert_eq!(normalize_whitespace("  Chapter\n\t 1 "), "Chapter 1");
        assert_eq!(normalize_whitespace("\n    Chapter 1\n  "), "Chapter 1");
        assert_eq!(normalize_whitespace(""), "");
        assert_eq!(normalize_whitespace("   "), "");
        assert_eq!(normalize_whitespace("no-whitespace"), "no-whitespace");
    }
}
