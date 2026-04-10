use super::{ContentItem, CounterStyle, StringPolicy};
use crate::gcpm::ElementPolicy;
use crate::gcpm::running::RunningElementStore;
use crate::paginate::{PageRunningState, StringSetPageState};
use std::collections::BTreeMap;

/// Resolve content items to a plain string.
///
/// `Element` references are skipped in plain string mode.
pub fn resolve_content_to_string(
    items: &[ContentItem],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page: usize,
    total_pages: usize,
    custom_counters: &BTreeMap<String, i32>,
) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::Counter { name, style } => match name.as_str() {
                "page" => out.push_str(&format_counter(page as i32, *style)),
                "pages" => out.push_str(&format_counter(total_pages as i32, *style)),
                _ => {
                    let value = custom_counters.get(name.as_str()).copied().unwrap_or(0);
                    out.push_str(&format_counter(value, *style));
                }
            },
            ContentItem::Element { .. } => {}
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
/// `Element { name, policy }` references are resolved via the per-page
/// running-element state and `RunningElementStore`, using the
/// WeasyPrint-compatible policy rules (see `resolve_element_policy`).
/// `StringRef` values come from the DOM (via `string-set: content(text) |
/// attr(...)`) and are HTML-escaped before concatenation so characters like
/// `<` and `&` do not corrupt the margin box.
#[allow(clippy::too_many_arguments)]
pub fn resolve_content_to_html(
    items: &[ContentItem],
    store: &RunningElementStore,
    running_states: &[BTreeMap<String, PageRunningState>],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page_num: usize,
    total_pages: usize,
    page_idx: usize,
    custom_counters: &BTreeMap<String, i32>,
) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => push_escaped_html_text(&mut out, s),
            ContentItem::Counter { name, style } => match name.as_str() {
                "page" => out.push_str(&format_counter(page_num as i32, *style)),
                "pages" => out.push_str(&format_counter(total_pages as i32, *style)),
                _ => {
                    let value = custom_counters.get(name.as_str()).copied().unwrap_or(0);
                    out.push_str(&format_counter(value, *style));
                }
            },
            ContentItem::Element { name, policy } => {
                if let Some(html) =
                    resolve_element_policy(name, *policy, page_idx, running_states, store)
                {
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

/// Resolve an `element(name, policy)` reference to the HTML of the chosen
/// running element instance for the given page (0-based `page_idx`).
///
/// WeasyPrint-compatible semantics:
/// - `first`: first instance assigned on the current page.
/// - `start`: ignores current-page assignments and returns the last instance
///   of the most recent preceding page (the value in effect at page start).
/// - `last`: last instance assigned on the current page.
/// - `first-except`: returns `None` if the current page has any assignment.
/// - Fallback (any policy, no resolution on current page): the last instance
///   of the most recent preceding page that had an assignment.
pub fn resolve_element_policy<'a>(
    name: &str,
    policy: ElementPolicy,
    page_idx: usize,
    page_states: &[BTreeMap<String, PageRunningState>],
    store: &'a RunningElementStore,
) -> Option<&'a str> {
    let current = page_states.get(page_idx).and_then(|s| s.get(name));

    let chosen_id: Option<usize> = match policy {
        ElementPolicy::First => current.and_then(|s| s.instance_ids.first().copied()),
        ElementPolicy::Last => current.and_then(|s| s.instance_ids.last().copied()),
        // Start ignores assignments on the current page entirely — it must
        // return the element that was in effect when this page began, which
        // is the last instance of the most recent preceding page. Fall
        // through to the fallback scan below.
        ElementPolicy::Start => None,
        ElementPolicy::FirstExcept => {
            // Current page has an assignment → suppress.
            // (`collect_running_element_states` only inserts an entry when
            // it pushes an instance_id, so `current.is_some()` suffices.)
            if current.is_some() {
                return None;
            }
            None
        }
    };

    if let Some(id) = chosen_id {
        return store.get_html(id);
    }

    // Fallback: scan preceding pages for the most recent assignment.
    for prev in (0..page_idx).rev() {
        if let Some(state) = page_states.get(prev).and_then(|s| s.get(name)) {
            if let Some(&last_id) = state.instance_ids.last() {
                return store.get_html(last_id);
            }
        }
    }

    None
}

/// Format a counter value according to the given [`CounterStyle`].
pub fn format_counter(value: i32, style: CounterStyle) -> String {
    match style {
        CounterStyle::Decimal => value.to_string(),
        CounterStyle::UpperRoman => to_roman(value).unwrap_or_else(|| value.to_string()),
        CounterStyle::LowerRoman => to_roman(value)
            .map(|s| s.to_lowercase())
            .unwrap_or_else(|| value.to_string()),
        CounterStyle::UpperAlpha => to_alpha(value, b'A').unwrap_or_else(|| value.to_string()),
        CounterStyle::LowerAlpha => to_alpha(value, b'a').unwrap_or_else(|| value.to_string()),
    }
}

/// Convert a positive integer (1..=3999) to an upper-case Roman numeral string.
fn to_roman(value: i32) -> Option<String> {
    if !(1..=3999).contains(&value) {
        return None;
    }
    const TABLE: &[(i32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    let mut rem = value;
    for &(threshold, symbol) in TABLE {
        while rem >= threshold {
            out.push_str(symbol);
            rem -= threshold;
        }
    }
    Some(out)
}

/// Convert a positive integer to an alphabetic label (A=1 .. Z=26, AA=27 ..).
fn to_alpha(value: i32, base: u8) -> Option<String> {
    if value < 1 {
        return None;
    }
    let mut n = value as u32;
    let mut chars = Vec::new();
    while n > 0 {
        n -= 1;
        chars.push((base + (n % 26) as u8) as char);
        n /= 26;
    }
    chars.reverse();
    Some(chars.into_iter().collect())
}

/// Tracks CSS counter values during DOM traversal.
///
/// Simplified model (no `counters()` nesting): a flat map where
/// `counter-reset` overwrites any existing value.
#[derive(Debug, Clone, Default)]
pub struct CounterState {
    values: BTreeMap<String, i32>,
}

impl CounterState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self, name: &str, value: i32) {
        self.values.insert(name.to_string(), value);
    }

    pub fn increment(&mut self, name: &str, value: i32) {
        let entry = self.values.entry(name.to_string()).or_insert(0);
        *entry += value;
    }

    pub fn set(&mut self, name: &str, value: i32) {
        self.values.insert(name.to_string(), value);
    }

    pub fn get(&self, name: &str) -> i32 {
        self.values.get(name).copied().unwrap_or(0)
    }

    /// Return a snapshot of all counter values.
    pub fn snapshot(&self) -> BTreeMap<String, i32> {
        self.values.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_counters() {
        let items = vec![
            ContentItem::String("Page ".into()),
            ContentItem::Counter {
                name: "page".into(),
                style: CounterStyle::Decimal,
            },
            ContentItem::String(" of ".into()),
            ContentItem::Counter {
                name: "pages".into(),
                style: CounterStyle::Decimal,
            },
        ];
        assert_eq!(
            resolve_content_to_string(&items, &BTreeMap::new(), 3, 10, &BTreeMap::new()),
            "Page 3 of 10"
        );
    }

    fn single_page_store(
        name: &str,
        html: &str,
    ) -> (RunningElementStore, Vec<BTreeMap<String, PageRunningState>>) {
        let mut store = RunningElementStore::new();
        let id = store.register(1, name.to_string(), html.to_string());
        let mut page_state = BTreeMap::new();
        page_state.insert(
            name.to_string(),
            PageRunningState {
                instance_ids: vec![id],
            },
        );
        (store, vec![page_state])
    }

    #[test]
    fn test_element_becomes_empty() {
        let items = vec![
            ContentItem::String("Before".into()),
            ContentItem::Element {
                name: "hdr".into(),
                policy: ElementPolicy::First,
            },
            ContentItem::String("After".into()),
        ];
        assert_eq!(
            resolve_content_to_string(&items, &BTreeMap::new(), 1, 5, &BTreeMap::new()),
            "BeforeAfter"
        );
    }

    #[test]
    fn test_resolve_html_with_running_element() {
        let items = vec![ContentItem::Element {
            name: "hdr".into(),
            policy: ElementPolicy::First,
        }];
        let (store, states) = single_page_store("hdr", "<b>Header</b>");
        assert_eq!(
            resolve_content_to_html(
                &items,
                &store,
                &states,
                &BTreeMap::new(),
                1,
                1,
                0,
                &BTreeMap::new()
            ),
            "<b>Header</b>"
        );
    }

    #[test]
    fn test_resolve_html_mixed() {
        let items = vec![
            ContentItem::Element {
                name: "hdr".into(),
                policy: ElementPolicy::First,
            },
            ContentItem::String(" - Page ".into()),
            ContentItem::Counter {
                name: "page".into(),
                style: CounterStyle::Decimal,
            },
            ContentItem::String("/".into()),
            ContentItem::Counter {
                name: "pages".into(),
                style: CounterStyle::Decimal,
            },
        ];

        // Build 3 pages of state with distinct running-element instances per
        // page so that mixing page_num (1-based, for counter(page)) with
        // page_idx (0-based, for element policy) would be detectable if
        // swapped.
        let mut store = RunningElementStore::new();
        let id0 = store.register(1, "hdr".into(), "<span>P1</span>".into());
        let id1 = store.register(2, "hdr".into(), "<span>P2</span>".into());
        let id2 = store.register(3, "hdr".into(), "<span>P3</span>".into());
        let mk = |ids: Vec<usize>| -> BTreeMap<String, PageRunningState> {
            let mut m = BTreeMap::new();
            m.insert("hdr".to_string(), PageRunningState { instance_ids: ids });
            m
        };
        let states = vec![mk(vec![id0]), mk(vec![id1]), mk(vec![id2])];

        // page_num=2 (1-based), page_idx=1 (0-based) → must pick P2.
        assert_eq!(
            resolve_content_to_html(
                &items,
                &store,
                &states,
                &BTreeMap::new(),
                2,
                3,
                1,
                &BTreeMap::new()
            ),
            "<span>P2</span> - Page 2/3"
        );
    }

    #[test]
    fn test_resolve_html_escapes_literal_string() {
        // ContentItem::String comes from CSS `content: "literal"` and may
        // contain `<`, `>`, `&`. It must be HTML-escaped before concatenation
        // so attackers (or mischievous authors) cannot inject markup into the
        // margin box via CSS string literals.
        let items = vec![ContentItem::String("A & B <script>".into())];
        let store = RunningElementStore::new();
        let states: Vec<BTreeMap<String, PageRunningState>> = vec![BTreeMap::new()];
        assert_eq!(
            resolve_content_to_html(
                &items,
                &store,
                &states,
                &BTreeMap::new(),
                1,
                1,
                0,
                &BTreeMap::new()
            ),
            "A &amp; B &lt;script&gt;"
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
            resolve_content_to_html(
                &items,
                &RunningElementStore::new(),
                &[],
                &state,
                1,
                1,
                0,
                &BTreeMap::new()
            ),
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
            resolve_content_to_html(
                &items,
                &RunningElementStore::new(),
                &[],
                &state,
                1,
                1,
                0,
                &BTreeMap::new()
            ),
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
            resolve_content_to_html(
                &items,
                &RunningElementStore::new(),
                &[],
                &state,
                1,
                1,
                0,
                &BTreeMap::new()
            ),
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
        assert_eq!(
            resolve_content_to_html(
                &items,
                &RunningElementStore::new(),
                &[],
                &state,
                1,
                1,
                0,
                &BTreeMap::new()
            ),
            "Last"
        );
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
        assert_eq!(
            resolve_content_to_html(
                &items,
                &RunningElementStore::new(),
                &[],
                &state,
                1,
                1,
                0,
                &BTreeMap::new()
            ),
            ""
        );
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
            resolve_content_to_html(
                &items,
                &RunningElementStore::new(),
                &[],
                &state,
                1,
                1,
                0,
                &BTreeMap::new()
            ),
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
            resolve_content_to_html(
                &items,
                &RunningElementStore::new(),
                &[],
                &BTreeMap::new(),
                1,
                1,
                0,
                &BTreeMap::new(),
            ),
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
            resolve_content_to_html(
                &items,
                &RunningElementStore::new(),
                &[],
                &state,
                1,
                1,
                0,
                &BTreeMap::new()
            ),
            "A &amp; B &lt;script&gt;"
        );
    }

    #[test]
    fn test_resolve_element_policy_scenarios() {
        let mut store = RunningElementStore::new();
        let id_a = store.register(1, "hdr".into(), "<h1>A</h1>".into());
        let id_b = store.register(2, "hdr".into(), "<h1>B</h1>".into());
        let id_c = store.register(3, "hdr".into(), "<h1>C</h1>".into());

        // P0 = [A, B], P1 = [C], P2 = []
        let mut p0 = BTreeMap::new();
        p0.insert(
            "hdr".to_string(),
            PageRunningState {
                instance_ids: vec![id_a, id_b],
            },
        );
        let mut p1 = BTreeMap::new();
        p1.insert(
            "hdr".to_string(),
            PageRunningState {
                instance_ids: vec![id_c],
            },
        );
        let p2 = BTreeMap::new();
        let states = vec![p0, p1, p2];

        // first
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::First, 0, &states, &store),
            Some("<h1>A</h1>")
        );
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::First, 1, &states, &store),
            Some("<h1>C</h1>")
        );
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::First, 2, &states, &store),
            Some("<h1>C</h1>")
        );

        // last
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::Last, 0, &states, &store),
            Some("<h1>B</h1>")
        );
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::Last, 1, &states, &store),
            Some("<h1>C</h1>")
        );
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::Last, 2, &states, &store),
            Some("<h1>C</h1>")
        );

        // start: ignores current-page assignments, returns the last
        // instance of the most recent preceding page (i.e. the value in
        // effect at the page boundary).
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::Start, 0, &states, &store),
            None, // no preceding pages
        );
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::Start, 1, &states, &store),
            Some("<h1>B</h1>"), // P0.instance_ids.last()
        );
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::Start, 2, &states, &store),
            Some("<h1>C</h1>"), // P1.instance_ids.last()
        );

        // first-except: empty where assigned, fallback where unassigned
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::FirstExcept, 0, &states, &store),
            None
        );
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::FirstExcept, 1, &states, &store),
            None
        );
        assert_eq!(
            resolve_element_policy("hdr", ElementPolicy::FirstExcept, 2, &states, &store),
            Some("<h1>C</h1>")
        );
    }

    #[test]
    fn test_resolve_element_policy_no_assignments_anywhere() {
        let store = RunningElementStore::new();
        let states: Vec<BTreeMap<String, PageRunningState>> = vec![BTreeMap::new(); 3];

        for policy in [
            ElementPolicy::First,
            ElementPolicy::Start,
            ElementPolicy::Last,
            ElementPolicy::FirstExcept,
        ] {
            for page in 0..3 {
                assert_eq!(
                    resolve_element_policy("hdr", policy, page, &states, &store),
                    None,
                );
            }
        }
    }

    #[test]
    fn test_resolve_element_policy_name_not_found() {
        let mut store = RunningElementStore::new();
        store.register(1, "other".into(), "<h1>X</h1>".into());
        let mut p0 = BTreeMap::new();
        p0.insert(
            "other".to_string(),
            PageRunningState {
                instance_ids: vec![0],
            },
        );
        let states = vec![p0];

        assert_eq!(
            resolve_element_policy("missing", ElementPolicy::First, 0, &states, &store),
            None,
        );
    }

    #[test]
    fn test_format_counter_decimal() {
        assert_eq!(format_counter(1, CounterStyle::Decimal), "1");
        assert_eq!(format_counter(42, CounterStyle::Decimal), "42");
        assert_eq!(format_counter(0, CounterStyle::Decimal), "0");
        assert_eq!(format_counter(-5, CounterStyle::Decimal), "-5");
    }

    #[test]
    fn test_format_counter_upper_roman() {
        assert_eq!(format_counter(1, CounterStyle::UpperRoman), "I");
        assert_eq!(format_counter(4, CounterStyle::UpperRoman), "IV");
        assert_eq!(format_counter(9, CounterStyle::UpperRoman), "IX");
        assert_eq!(format_counter(14, CounterStyle::UpperRoman), "XIV");
        assert_eq!(format_counter(3999, CounterStyle::UpperRoman), "MMMCMXCIX");
        // Fallback to decimal for out-of-range values
        assert_eq!(format_counter(0, CounterStyle::UpperRoman), "0");
        assert_eq!(format_counter(4000, CounterStyle::UpperRoman), "4000");
    }

    #[test]
    fn test_format_counter_lower_roman() {
        assert_eq!(format_counter(1, CounterStyle::LowerRoman), "i");
        assert_eq!(format_counter(14, CounterStyle::LowerRoman), "xiv");
    }

    #[test]
    fn test_format_counter_upper_alpha() {
        assert_eq!(format_counter(1, CounterStyle::UpperAlpha), "A");
        assert_eq!(format_counter(26, CounterStyle::UpperAlpha), "Z");
        assert_eq!(format_counter(27, CounterStyle::UpperAlpha), "AA");
        // Fallback to decimal for 0
        assert_eq!(format_counter(0, CounterStyle::UpperAlpha), "0");
    }

    #[test]
    fn test_format_counter_lower_alpha() {
        assert_eq!(format_counter(1, CounterStyle::LowerAlpha), "a");
        assert_eq!(format_counter(26, CounterStyle::LowerAlpha), "z");
    }

    #[test]
    fn test_resolve_custom_counter() {
        let items = vec![
            ContentItem::String("Chapter ".into()),
            ContentItem::Counter {
                name: "chapter".into(),
                style: CounterStyle::UpperRoman,
            },
        ];
        let mut custom_counters = BTreeMap::new();
        custom_counters.insert("chapter".to_string(), 4);
        assert_eq!(
            resolve_content_to_string(&items, &BTreeMap::new(), 1, 1, &custom_counters),
            "Chapter IV"
        );
    }

    #[test]
    fn test_counter_state_reset_and_get() {
        let mut state = CounterState::new();
        state.reset("chapter", 0);
        assert_eq!(state.get("chapter"), 0);
    }

    #[test]
    fn test_counter_state_increment() {
        let mut state = CounterState::new();
        state.reset("chapter", 0);
        state.increment("chapter", 1);
        assert_eq!(state.get("chapter"), 1);
        state.increment("chapter", 1);
        assert_eq!(state.get("chapter"), 2);
    }

    #[test]
    fn test_counter_state_set() {
        let mut state = CounterState::new();
        state.reset("chapter", 0);
        state.set("chapter", 5);
        assert_eq!(state.get("chapter"), 5);
    }

    #[test]
    fn test_counter_state_implicit_zero() {
        let mut state = CounterState::new();
        state.increment("chapter", 1);
        assert_eq!(state.get("chapter"), 1);
    }

    #[test]
    fn test_counter_state_snapshot() {
        let mut state = CounterState::new();
        state.reset("chapter", 0);
        state.increment("chapter", 1);
        state.reset("section", 0);
        let snap = state.snapshot();
        assert_eq!(snap.get("chapter"), Some(&1));
        assert_eq!(snap.get("section"), Some(&0));
    }
}
