use super::{ContentItem, CounterType, StringPolicy};
use crate::paginate::StringSetPageState;
use std::collections::BTreeMap;

/// Resolve content items to a plain string.
///
/// `Element` references are skipped in plain string mode.
pub fn resolve_content_to_string(
    items: &[ContentItem],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page: usize,
    total_pages: usize,
) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::Counter(CounterType::Page) => {
                out.push_str(&page.to_string());
            }
            ContentItem::Counter(CounterType::Pages) => {
                out.push_str(&total_pages.to_string());
            }
            ContentItem::Element(_) => {}
            ContentItem::StringRef { name, policy } => {
                if let Some(state) = string_set_states.get(name) {
                    out.push_str(resolve_string_policy(state, *policy));
                }
            }
        }
    }
    out
}

/// Resolve content items to an HTML string.
///
/// `Element(name)` references are looked up in `running_elements` (a `&[(name, html)]` slice)
/// and the matching HTML is appended. `StringRef` values come from the DOM
/// (via `string-set: content(text) | attr(...)`) and are HTML-escaped before
/// concatenation so characters like `<` and `&` do not corrupt the margin box.
pub fn resolve_content_to_html(
    items: &[ContentItem],
    running_elements: &[(String, String)],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page: usize,
    total_pages: usize,
) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::Counter(CounterType::Page) => {
                out.push_str(&page.to_string());
            }
            ContentItem::Counter(CounterType::Pages) => {
                out.push_str(&total_pages.to_string());
            }
            ContentItem::Element(name) => {
                if let Some((_, html)) = running_elements.iter().find(|(n, _)| n == name) {
                    out.push_str(html);
                }
            }
            ContentItem::StringRef { name, policy } => {
                if let Some(state) = string_set_states.get(name) {
                    push_escaped_html_text(&mut out, resolve_string_policy(state, *policy));
                }
            }
        }
    }
    out
}

/// Append `text` to `out` with HTML special characters escaped.
///
/// Used for string-set values, which originate from arbitrary DOM text and
/// would otherwise break the margin box HTML if they contained `<`, `>`, or `&`.
fn push_escaped_html_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn resolve_string_policy(state: &StringSetPageState, policy: StringPolicy) -> &str {
    match policy {
        StringPolicy::Start => state.start.as_deref().unwrap_or(""),
        StringPolicy::First => state
            .first
            .as_deref()
            .or(state.start.as_deref())
            .unwrap_or(""),
        StringPolicy::Last => state
            .last
            .as_deref()
            .or(state.first.as_deref())
            .or(state.start.as_deref())
            .unwrap_or(""),
        // first-except: empty on pages where the string was set this page,
        // otherwise falls back to the inherited start value.
        StringPolicy::FirstExcept if state.first.is_some() => "",
        StringPolicy::FirstExcept => state.start.as_deref().unwrap_or(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_counters() {
        let items = vec![
            ContentItem::String("Page ".into()),
            ContentItem::Counter(CounterType::Page),
            ContentItem::String(" of ".into()),
            ContentItem::Counter(CounterType::Pages),
        ];
        assert_eq!(
            resolve_content_to_string(&items, &BTreeMap::new(), 3, 10),
            "Page 3 of 10"
        );
    }

    #[test]
    fn test_element_becomes_empty() {
        let items = vec![
            ContentItem::String("Before".into()),
            ContentItem::Element("hdr".into()),
            ContentItem::String("After".into()),
        ];
        assert_eq!(
            resolve_content_to_string(&items, &BTreeMap::new(), 1, 5),
            "BeforeAfter"
        );
    }

    #[test]
    fn test_resolve_html_with_running_element() {
        let items = vec![ContentItem::Element("hdr".into())];
        let running = vec![("hdr".to_string(), "<b>Header</b>".to_string())];
        assert_eq!(
            resolve_content_to_html(&items, &running, &BTreeMap::new(), 1, 1),
            "<b>Header</b>"
        );
    }

    #[test]
    fn test_resolve_html_mixed() {
        let items = vec![
            ContentItem::Element("hdr".into()),
            ContentItem::String(" - Page ".into()),
            ContentItem::Counter(CounterType::Page),
            ContentItem::String("/".into()),
            ContentItem::Counter(CounterType::Pages),
        ];
        let running = vec![("hdr".to_string(), "<span>Title</span>".to_string())];
        assert_eq!(
            resolve_content_to_html(&items, &running, &BTreeMap::new(), 2, 8),
            "<span>Title</span> - Page 2/8"
        );
    }

    #[test]
    fn test_resolve_string_ref_first() {
        let items = vec![ContentItem::StringRef {
            name: "title".to_string(),
            policy: StringPolicy::First,
        }];
        let mut state = BTreeMap::new();
        state.insert(
            "title".to_string(),
            StringSetPageState {
                start: Some("Previous".to_string()),
                first: Some("Current".to_string()),
                last: Some("Current".to_string()),
            },
        );
        assert_eq!(
            resolve_content_to_html(&items, &[], &state, 1, 1),
            "Current"
        );
    }

    #[test]
    fn test_resolve_string_ref_first_falls_back_to_start() {
        let items = vec![ContentItem::StringRef {
            name: "title".to_string(),
            policy: StringPolicy::First,
        }];
        let mut state = BTreeMap::new();
        state.insert(
            "title".to_string(),
            StringSetPageState {
                start: Some("Inherited".to_string()),
                first: None,
                last: None,
            },
        );
        assert_eq!(
            resolve_content_to_html(&items, &[], &state, 1, 1),
            "Inherited"
        );
    }

    #[test]
    fn test_resolve_string_ref_start() {
        let items = vec![ContentItem::StringRef {
            name: "title".to_string(),
            policy: StringPolicy::Start,
        }];
        let mut state = BTreeMap::new();
        state.insert(
            "title".to_string(),
            StringSetPageState {
                start: Some("Start Value".to_string()),
                first: Some("First Value".to_string()),
                last: Some("Last Value".to_string()),
            },
        );
        assert_eq!(
            resolve_content_to_html(&items, &[], &state, 1, 1),
            "Start Value"
        );
    }

    #[test]
    fn test_resolve_string_ref_last() {
        let items = vec![ContentItem::StringRef {
            name: "title".to_string(),
            policy: StringPolicy::Last,
        }];
        let mut state = BTreeMap::new();
        state.insert(
            "title".to_string(),
            StringSetPageState {
                start: None,
                first: Some("First".to_string()),
                last: Some("Last".to_string()),
            },
        );
        assert_eq!(resolve_content_to_html(&items, &[], &state, 1, 1), "Last");
    }

    #[test]
    fn test_resolve_string_ref_first_except_on_set_page() {
        let items = vec![ContentItem::StringRef {
            name: "title".to_string(),
            policy: StringPolicy::FirstExcept,
        }];
        let mut state = BTreeMap::new();
        state.insert(
            "title".to_string(),
            StringSetPageState {
                start: Some("Old".to_string()),
                first: Some("New".to_string()),
                last: Some("New".to_string()),
            },
        );
        assert_eq!(resolve_content_to_html(&items, &[], &state, 1, 1), "");
    }

    #[test]
    fn test_resolve_string_ref_first_except_on_no_set_page() {
        let items = vec![ContentItem::StringRef {
            name: "title".to_string(),
            policy: StringPolicy::FirstExcept,
        }];
        let mut state = BTreeMap::new();
        state.insert(
            "title".to_string(),
            StringSetPageState {
                start: Some("Inherited".to_string()),
                first: None,
                last: None,
            },
        );
        assert_eq!(
            resolve_content_to_html(&items, &[], &state, 1, 1),
            "Inherited"
        );
    }

    #[test]
    fn test_resolve_string_ref_unknown_name() {
        let items = vec![ContentItem::StringRef {
            name: "nonexistent".to_string(),
            policy: StringPolicy::First,
        }];
        assert_eq!(
            resolve_content_to_html(&items, &[], &BTreeMap::new(), 1, 1),
            ""
        );
    }

    #[test]
    fn test_resolve_string_ref_html_escapes_special_characters() {
        let items = vec![ContentItem::StringRef {
            name: "title".to_string(),
            policy: StringPolicy::First,
        }];
        let mut state = BTreeMap::new();
        state.insert(
            "title".to_string(),
            StringSetPageState {
                start: None,
                first: Some("A & B <script>".to_string()),
                last: Some("A & B <script>".to_string()),
            },
        );
        assert_eq!(
            resolve_content_to_html(&items, &[], &state, 1, 1),
            "A &amp; B &lt;script&gt;"
        );
    }
}
