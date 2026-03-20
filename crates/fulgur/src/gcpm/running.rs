//! Running element support for CSS Generated Content for Paged Media (GCPM).
//!
//! Manages running elements extracted from the DOM via `position: running(name)`.
//! These elements are serialized to HTML and stored for later re-layout in margin boxes.

use std::collections::HashMap;

/// Stores running elements keyed by name.
///
/// Running elements are DOM subtrees that have been extracted from the normal
/// document flow (via `position: running(...)`) and serialized to HTML strings.
/// They are later injected into margin boxes via `content: element(name)`.
pub struct RunningElementStore {
    elements: HashMap<String, String>,
}

impl RunningElementStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self {
            elements: HashMap::new(),
        }
    }

    /// Register a running element by name with its serialized HTML.
    pub fn register(&mut self, name: String, html: String) {
        self.elements.insert(name, html);
    }

    /// Look up a running element by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.elements.get(name).map(|s| s.as_str())
    }

    /// Convert to pairs for counter resolution or other consumers.
    pub fn to_pairs(&self) -> Vec<(String, String)> {
        self.elements
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

impl Default for RunningElementStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialize a Blitz DOM node subtree back to an HTML string.
///
/// Used to extract running elements for re-layout in margin boxes.
/// Does not serialize computed styles as inline styles — the CSS cascade
/// handles styling in the re-layout pass.
pub fn serialize_node(doc: &blitz_dom::BaseDocument, node_id: usize) -> String {
    let mut output = String::new();
    write_node(doc, node_id, &mut output);
    output
}

fn write_node(doc: &blitz_dom::BaseDocument, node_id: usize, writer: &mut String) {
    use blitz_dom::NodeData;

    let Some(node) = doc.get_node(node_id) else {
        return;
    };

    match &node.data {
        NodeData::Text(text_data) => {
            writer.push_str(&text_data.content);
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

            if !has_children {
                writer.push_str(" />");
            } else {
                writer.push('>');
                for &child_id in &node.children {
                    write_node(doc, child_id, writer);
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
    fn test_running_element_store_and_lookup() {
        let mut store = RunningElementStore::new();

        // Initially empty
        assert!(store.get("header").is_none());

        // Register and retrieve
        store.register("header".to_string(), "<h1>Title</h1>".to_string());
        assert_eq!(store.get("header"), Some("<h1>Title</h1>"));

        // Nonexistent key
        assert!(store.get("footer").is_none());

        // Overwrite
        store.register("header".to_string(), "<h1>New Title</h1>".to_string());
        assert_eq!(store.get("header"), Some("<h1>New Title</h1>"));
    }

    #[test]
    fn test_to_pairs() {
        let mut store = RunningElementStore::new();
        store.register("header".to_string(), "<h1>Title</h1>".to_string());
        store.register("footer".to_string(), "<footer>F</footer>".to_string());

        let mut pairs = store.to_pairs();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(pairs.len(), 2);
        assert_eq!(
            pairs[0],
            ("footer".to_string(), "<footer>F</footer>".to_string())
        );
        assert_eq!(
            pairs[1],
            ("header".to_string(), "<h1>Title</h1>".to_string())
        );
    }
}
