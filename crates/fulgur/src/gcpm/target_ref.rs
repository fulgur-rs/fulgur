//! Cross-reference resolution for CSS GCPM `target-counter()` /
//! `target-counters()` / `target-text()`.
//!
//! `AnchorMap` is built at the end of pass 1 (after pagination has
//! assigned each DOM element a page) and consumed by pass 2 via the
//! resolver helpers below.

use crate::gcpm::CounterStyle;
use crate::gcpm::counter::{format_counter, format_counter_chain};
use crate::pagination_layout::PaginationGeometryTable;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct AnchorMap {
    entries: BTreeMap<String, AnchorEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct AnchorEntry {
    pub page_num: u32,
    /// Counter name -> outer-to-inner instance chain at the target
    /// element. Mirrors `CounterState::chain_snapshot`.
    pub counters: BTreeMap<String, Vec<i32>>,
    pub text: String,
}

impl AnchorMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, fragment_id: impl Into<String>, entry: AnchorEntry) {
        self.entries.insert(fragment_id.into(), entry);
    }

    pub fn get(&self, fragment_id: &str) -> Option<&AnchorEntry> {
        self.entries.get(fragment_id)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Convert an attribute value (e.g. `"#sec1"`) to a fragment identifier.
/// Returns `None` for non-fragment URLs (anything not starting with `#`,
/// or empty after the `#`). The leading `#` is stripped; URL-decoding
/// and case normalization are NOT applied — HTML id matching is
/// case-sensitive in HTML5.
pub fn fragment_id_from_href(href: &str) -> Option<&str> {
    href.strip_prefix('#').filter(|s| !s.is_empty())
}

/// Resolve `target-counter(attr(<url_attr>), <counter_name>)`.
/// Returns the formatted value, or empty string on any failure.
pub fn resolve_target_counter(
    href: &str,
    counter_name: &str,
    style: CounterStyle,
    map: &AnchorMap,
) -> String {
    let Some(frag) = fragment_id_from_href(href) else {
        return String::new();
    };
    let Some(entry) = map.get(frag) else {
        return String::new();
    };
    // CSS spec treats `page` as the implicit page counter. Resolve it
    // straight from `entry.page_num` regardless of whether the snapshot
    // happens to carry a user-defined `page` counter — `target-counter(..,
    // page)` is documented to mean "the page number where this anchor
    // lands", and giving precedence to a custom `counter-reset: page`
    // produces a result that disagrees with the rendered page sequence.
    if counter_name == "page" {
        return format_counter(entry.page_num as i32, style);
    }
    let Some(chain) = entry.counters.get(counter_name) else {
        return String::new();
    };
    chain
        .last()
        .copied()
        .map(|v| format_counter(v, style))
        .unwrap_or_default()
}

pub fn resolve_target_counters(
    href: &str,
    counter_name: &str,
    separator: &str,
    style: CounterStyle,
    map: &AnchorMap,
) -> String {
    let Some(frag) = fragment_id_from_href(href) else {
        return String::new();
    };
    let Some(entry) = map.get(frag) else {
        return String::new();
    };
    let Some(chain) = entry.counters.get(counter_name) else {
        return String::new();
    };
    format_counter_chain(chain, separator, style)
}

pub fn resolve_target_text(href: &str, map: &AnchorMap) -> String {
    let Some(frag) = fragment_id_from_href(href) else {
        return String::new();
    };
    map.get(frag).map(|e| e.text.clone()).unwrap_or_default()
}

/// Return the **1-based** page number for a DOM node, derived from
/// the first fragment in the node's pagination geometry. Returns
/// `None` if the node has no fragments (out-of-flow nodes the
/// fragmenter skipped, or non-laid-out subtrees).
pub fn page_for_node(geometry: &PaginationGeometryTable, node_id: usize) -> Option<u32> {
    geometry
        .get(&node_id)
        .and_then(|g| g.fragments.first())
        .map(|f| f.page_index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_map() -> AnchorMap {
        let mut m = AnchorMap::new();
        let mut counters = BTreeMap::new();
        counters.insert("section".into(), vec![1, 2]);
        m.insert(
            "sec-1-2",
            AnchorEntry {
                page_num: 7,
                counters,
                text: "Introduction".into(),
            },
        );
        m
    }

    #[test]
    fn fragment_id_strips_hash() {
        assert_eq!(fragment_id_from_href("#sec1"), Some("sec1"));
    }

    #[test]
    fn fragment_id_rejects_external() {
        assert_eq!(fragment_id_from_href("https://example.com/"), None);
        assert_eq!(fragment_id_from_href("foo.html#bar"), None);
        assert_eq!(fragment_id_from_href("#"), None);
        assert_eq!(fragment_id_from_href(""), None);
    }

    #[test]
    fn target_counter_page_uses_page_num() {
        let m = make_map();
        assert_eq!(
            resolve_target_counter("#sec-1-2", "page", CounterStyle::Decimal, &m),
            "7"
        );
    }

    #[test]
    fn target_counter_page_name_ignores_user_defined_counter() {
        // `target-counter(href, page)` always resolves to the actual
        // page number where the anchor lands. A user-defined
        // `counter-reset: page` on the target element must not shadow
        // the implicit page counter.
        let mut m = AnchorMap::new();
        let mut counters = BTreeMap::new();
        counters.insert("page".into(), vec![999]);
        m.insert(
            "x",
            AnchorEntry {
                page_num: 5,
                counters,
                text: String::new(),
            },
        );
        assert_eq!(
            resolve_target_counter("#x", "page", CounterStyle::Decimal, &m),
            "5"
        );
    }

    #[test]
    fn target_counter_named_uses_innermost() {
        let m = make_map();
        assert_eq!(
            resolve_target_counter("#sec-1-2", "section", CounterStyle::Decimal, &m),
            "2"
        );
    }

    #[test]
    fn target_counter_missing_fragment_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_counter("#nope", "page", CounterStyle::Decimal, &m),
            ""
        );
    }

    #[test]
    fn target_counter_external_href_returns_empty() {
        let m = make_map();
        assert_eq!(
            resolve_target_counter("https://example.com/", "page", CounterStyle::Decimal, &m),
            ""
        );
    }

    #[test]
    fn target_counters_joins_chain() {
        let m = make_map();
        assert_eq!(
            resolve_target_counters("#sec-1-2", "section", ".", CounterStyle::Decimal, &m),
            "1.2"
        );
    }

    #[test]
    fn target_text_returns_text() {
        let m = make_map();
        assert_eq!(resolve_target_text("#sec-1-2", &m), "Introduction");
    }

    #[test]
    fn target_text_missing_returns_empty() {
        let m = make_map();
        assert_eq!(resolve_target_text("#nope", &m), "");
    }

    #[test]
    fn page_for_node_returns_first_page_for_split_node() {
        use crate::pagination_layout::{Fragment, PaginationGeometry};
        let mut table = PaginationGeometryTable::new();
        table.insert(
            42,
            PaginationGeometry {
                fragments: vec![
                    Fragment {
                        page_index: 2,
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    },
                    Fragment {
                        page_index: 3,
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    },
                ],
                is_repeat: false,
            },
        );
        assert_eq!(page_for_node(&table, 42), Some(3)); // page_index 2 -> 1-based page 3
    }

    #[test]
    fn page_for_node_returns_none_for_absent_node() {
        let table = PaginationGeometryTable::new();
        assert_eq!(page_for_node(&table, 999), None);
    }

    #[test]
    fn page_for_node_returns_none_for_node_with_no_fragments() {
        use crate::pagination_layout::PaginationGeometry;
        let mut table = PaginationGeometryTable::new();
        table.insert(
            7,
            PaginationGeometry {
                fragments: vec![],
                is_repeat: false,
            },
        );
        assert_eq!(page_for_node(&table, 7), None);
    }
}
