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
}
