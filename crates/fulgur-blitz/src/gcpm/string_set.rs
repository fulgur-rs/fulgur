//! Named string support for CSS Generated Content for Paged Media (GCPM).
//!
//! Manages string-set values extracted from the DOM via `string-set: name content(text)`.
//! Values are stored with their DOM node IDs for later insertion into the drawable tree.

/// A single string-set entry extracted from the DOM.
#[derive(Debug, Clone)]
pub struct StringSetEntry {
    /// The named string identifier (e.g. "chapter-title").
    pub name: String,
    /// The resolved text value.
    pub value: String,
    /// Blitz DOM node ID, used to position the marker in the drawable tree.
    pub node_id: usize,
}

/// Stores string-set entries collected during DOM traversal.
pub struct StringSetStore {
    entries: Vec<StringSetEntry>,
    /// Running total of stored `name` + `value` bytes, enforcing the
    /// [`crate::MAX_STRING_SET_STORE_BYTES`] budget in [`Self::push`].
    total_bytes: usize,
}

impl StringSetStore {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            total_bytes: 0,
        }
    }

    /// Record a resolved `string-set` assignment.
    ///
    /// DoS budget: `string-set` values are attacker-controlled and one entry is
    /// pushed per (element, matching rule), so a repeated literal
    /// (`p { string-set: x "BIG" }` matched by N elements) accumulates
    /// O(N × value_size) here from a single-copy input — and the values are
    /// cloned again downstream into the per-node render map. Each entry is
    /// charged its `name` + `value` payload plus a per-record
    /// [`crate::STRING_ENTRY_OVERHEAD_BYTES`], so the budget bounds the entry
    /// *count* as well as payload bytes (empty / tiny values such as
    /// `p { string-set: x "" }` would otherwise admit millions of near-zero-byte
    /// records). Once a new entry would push the aggregate past
    /// [`crate::MAX_STRING_SET_STORE_BYTES`], it is dropped (its `string()`
    /// degrades to the last recorded value — acceptable under adversarial input).
    /// Capping at this source point bounds the downstream clones dependently.
    /// Smaller later entries can still fit, so the aggregate is a hard ceiling
    /// (no overshoot).
    pub fn push(&mut self, entry: StringSetEntry) {
        let add = crate::STRING_ENTRY_OVERHEAD_BYTES + entry.name.len() + entry.value.len();
        if self.total_bytes.saturating_add(add) > crate::MAX_STRING_SET_STORE_BYTES {
            return;
        }
        self.total_bytes += add;
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

    #[test]
    fn default_produces_empty_store() {
        let store = StringSetStore::default();
        assert!(store.is_empty());
        assert_eq!(store.entries().len(), 0);
    }

    #[test]
    fn push_drops_entry_when_budget_exceeded() {
        let mut store = StringSetStore::new();

        // Fill the store to just below the limit with one large entry whose
        // name + value payload + STRING_ENTRY_OVERHEAD_BYTES equals exactly
        // MAX_STRING_SET_STORE_BYTES.  The exact payload size needed:
        //   payload = MAX_STRING_SET_STORE_BYTES - STRING_ENTRY_OVERHEAD_BYTES
        let budget = crate::MAX_STRING_SET_STORE_BYTES;
        let overhead = crate::STRING_ENTRY_OVERHEAD_BYTES;
        let payload = budget - overhead; // name.len() + value.len()
        let value = "x".repeat(payload);
        store.push(StringSetEntry {
            name: String::new(),
            value,
            node_id: 1,
        });
        assert_eq!(store.entries().len(), 1, "first entry should fit exactly");

        // A second entry of any size must be dropped — the budget is now full.
        store.push(StringSetEntry {
            name: "overflow".into(),
            value: "ignored".into(),
            node_id: 2,
        });
        assert_eq!(
            store.entries().len(),
            1,
            "entry over budget must be silently dropped"
        );
    }

    #[test]
    fn push_allows_small_entries_after_earlier_large_entry_fails() {
        let mut store = StringSetStore::new();

        let budget = crate::MAX_STRING_SET_STORE_BYTES;
        let overhead = crate::STRING_ENTRY_OVERHEAD_BYTES;

        // Push an entry that is just one byte over the budget: it must be
        // dropped because the total would exceed MAX_STRING_SET_STORE_BYTES.
        let too_large = "x".repeat(budget - overhead + 1);
        store.push(StringSetEntry {
            name: String::new(),
            value: too_large,
            node_id: 1,
        });
        assert!(store.is_empty(), "over-budget entry must be dropped");

        // A small entry should still be accepted since the budget wasn't consumed.
        store.push(StringSetEntry {
            name: "k".into(),
            value: "v".into(),
            node_id: 2,
        });
        assert_eq!(
            store.entries().len(),
            1,
            "small entry must be accepted after a dropped entry"
        );
        assert_eq!(store.entries()[0].node_id, 2);
    }

    #[test]
    fn push_tracks_total_bytes_correctly() {
        let mut store = StringSetStore::new();
        let overhead = crate::STRING_ENTRY_OVERHEAD_BYTES;

        // Push two entries whose combined size is well within the budget.
        store.push(StringSetEntry {
            name: "a".into(),   // 1 byte
            value: "bb".into(), // 2 bytes
            node_id: 1,
        });
        store.push(StringSetEntry {
            name: "cc".into(),   // 2 bytes
            value: "ddd".into(), // 3 bytes
            node_id: 2,
        });

        // Both entries should be present; total_bytes = 2*(overhead) + 1+2+2+3.
        assert_eq!(store.entries().len(), 2);

        // Now push an entry whose size alone equals the remaining budget.
        // Expected total after the two entries: 2*overhead + 8
        let consumed = 2 * overhead + 8;
        let remaining = crate::MAX_STRING_SET_STORE_BYTES - consumed;
        // Subtract overhead for the new entry's accounting.
        let fill_value = "y".repeat(remaining - overhead);
        store.push(StringSetEntry {
            name: String::new(),
            value: fill_value,
            node_id: 3,
        });
        assert_eq!(store.entries().len(), 3, "third entry must fit");

        // Now the budget is full — a fourth entry must be dropped.
        store.push(StringSetEntry {
            name: "x".into(),
            value: "x".into(),
            node_id: 4,
        });
        assert_eq!(
            store.entries().len(),
            3,
            "fourth entry must be dropped when budget is full"
        );
    }
}
