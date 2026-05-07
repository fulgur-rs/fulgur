use super::{ContentItem, CounterStyle, StringPolicy};
use crate::gcpm::ElementPolicy;
use crate::gcpm::running::RunningElementStore;
use crate::gcpm::target_ref::AnchorMap;
use crate::pagination_layout::{PageRunningState, StringSetPageState};
use std::collections::BTreeMap;

/// Resolve content items to a plain string.
///
/// `Element` references are skipped in plain string mode.
///
/// This is a thin shim over [`resolve_content_to_string_with_anchor`]
/// that passes `None` for the anchor map and implicit href — preserving
/// the original signature for callers that don't (yet) need
/// `target-counter()` / `target-text()` resolution.
pub fn resolve_content_to_string(
    items: &[ContentItem],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page: usize,
    total_pages: usize,
    custom_counters: &BTreeMap<String, i32>,
) -> String {
    resolve_content_to_string_with_anchor(
        items,
        string_set_states,
        page,
        total_pages,
        custom_counters,
        None,
        None,
    )
}

/// Like [`resolve_content_to_string`], but also resolves
/// `target-counter()` / `target-counters()` / `target-text()` against
/// the supplied [`AnchorMap`]. `implicit_href` is the `attr(href)` value
/// to use for the bare `attr(href)` URL form (the only form currently
/// supported).
///
/// When `anchor_map` is `None`, the three target-* variants emit empty
/// strings. Margin-box content is empty during pass 1 (output is
/// discarded), so placeholder width is irrelevant here — the
/// `::before` / `::after` placeholder injection that *does* affect
/// pass-1 layout lives at the parser/serializer level (see
/// `blitz_adapter::CounterPass::resolve_content`).
#[allow(clippy::too_many_arguments)]
pub fn resolve_content_to_string_with_anchor(
    items: &[ContentItem],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page: usize,
    total_pages: usize,
    custom_counters: &BTreeMap<String, i32>,
    anchor_map: Option<&AnchorMap>,
    implicit_href: Option<&str>,
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
            // `content()` / `content(before|after)` / `attr(X)` are only
            // meaningful inside `bookmark-label`, where a DOM-element
            // context provides the value. In margin-box content they
            // have no referent, so they emit nothing.
            // `leader()` produces a fill character at render time, not a
            // plain string; emit nothing in string-resolution context.
            ContentItem::Counters {
                name,
                separator: _,
                style,
            } => out.push_str(&resolve_counters_margin_box(
                name,
                *style,
                page,
                total_pages,
                custom_counters,
            )),
            ContentItem::TargetCounter {
                url_attr,
                counter_name,
                style,
            } => {
                if url_attr != "href" {
                    continue;
                }
                let href = implicit_href.unwrap_or("");
                if let Some(map) = anchor_map {
                    out.push_str(&crate::gcpm::target_ref::resolve_target_counter(
                        href,
                        counter_name,
                        *style,
                        map,
                    ));
                }
                // else: pass-1 placeholder mode — write nothing.
            }
            ContentItem::TargetCounters {
                url_attr,
                counter_name,
                separator,
                style,
            } => {
                if url_attr != "href" {
                    continue;
                }
                let href = implicit_href.unwrap_or("");
                if let Some(map) = anchor_map {
                    out.push_str(&crate::gcpm::target_ref::resolve_target_counters(
                        href,
                        counter_name,
                        separator,
                        *style,
                        map,
                    ));
                }
            }
            ContentItem::TargetText { url_attr } => {
                if url_attr != "href" {
                    continue;
                }
                let href = implicit_href.unwrap_or("");
                if let Some(map) = anchor_map {
                    out.push_str(&crate::gcpm::target_ref::resolve_target_text(href, map));
                }
            }
            ContentItem::ContentText
            | ContentItem::ContentBefore
            | ContentItem::ContentAfter
            | ContentItem::Attr(_)
            | ContentItem::Leader { .. } => {}
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
///
/// When the content list contains a `Leader` item, the output switches to a
/// flex-container mode: every item is wrapped in a `<span>`, and the leader
/// becomes a `flex:1; overflow:hidden` span filled with repeated characters
/// so that it expands to fill the available line width.
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
    resolve_content_to_html_with_anchor(
        items,
        store,
        running_states,
        string_set_states,
        page_num,
        total_pages,
        page_idx,
        custom_counters,
        None,
        None,
    )
}

/// Like [`resolve_content_to_html`], but also resolves
/// `target-counter()` / `target-counters()` / `target-text()` against
/// the supplied [`AnchorMap`]. See
/// [`resolve_content_to_string_with_anchor`] for parameter semantics.
///
/// Both content modes (flat and flex/leader) honour the new variants.
#[allow(clippy::too_many_arguments)]
pub fn resolve_content_to_html_with_anchor(
    items: &[ContentItem],
    store: &RunningElementStore,
    running_states: &[BTreeMap<String, PageRunningState>],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page_num: usize,
    total_pages: usize,
    page_idx: usize,
    custom_counters: &BTreeMap<String, i32>,
    anchor_map: Option<&AnchorMap>,
    implicit_href: Option<&str>,
) -> String {
    let has_leader = items
        .iter()
        .any(|i| matches!(i, ContentItem::Leader { .. }));

    if !has_leader {
        // Flat mode: backward-compatible plain concatenation.
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
                ContentItem::Counters {
                    name,
                    separator: _,
                    style,
                } => out.push_str(&resolve_counters_margin_box(
                    name,
                    *style,
                    page_num,
                    total_pages,
                    custom_counters,
                )),
                ContentItem::TargetCounter {
                    url_attr,
                    counter_name,
                    style,
                } => {
                    if url_attr != "href" {
                        continue;
                    }
                    let href = implicit_href.unwrap_or("");
                    if let Some(map) = anchor_map {
                        push_escaped_html_text(
                            &mut out,
                            &crate::gcpm::target_ref::resolve_target_counter(
                                href,
                                counter_name,
                                *style,
                                map,
                            ),
                        );
                    }
                }
                ContentItem::TargetCounters {
                    url_attr,
                    counter_name,
                    separator,
                    style,
                } => {
                    if url_attr != "href" {
                        continue;
                    }
                    let href = implicit_href.unwrap_or("");
                    if let Some(map) = anchor_map {
                        push_escaped_html_text(
                            &mut out,
                            &crate::gcpm::target_ref::resolve_target_counters(
                                href,
                                counter_name,
                                separator,
                                *style,
                                map,
                            ),
                        );
                    }
                }
                ContentItem::TargetText { url_attr } => {
                    if url_attr != "href" {
                        continue;
                    }
                    let href = implicit_href.unwrap_or("");
                    if let Some(map) = anchor_map {
                        push_escaped_html_text(
                            &mut out,
                            &crate::gcpm::target_ref::resolve_target_text(href, map),
                        );
                    }
                }
                ContentItem::ContentText
                | ContentItem::ContentBefore
                | ContentItem::ContentAfter
                | ContentItem::Attr(_)
                | ContentItem::Leader { .. } => {}
            }
        }
        return out;
    }

    // Flex mode: wrap everything in a flex container so that leader items
    // expand to fill the remaining line width.
    let mut parts: Vec<String> = Vec::new();
    for item in items {
        match item {
            ContentItem::Leader { style } => {
                let ch = style.leader_char();
                // Repeat enough chars to fill any reasonable column width;
                // overflow:hidden clips any excess in the browser/Blitz layout.
                const LEADER_FILL_REPEAT: usize = 400;
                let unit_len = ch.chars().count().max(1);
                let fill_len = (LEADER_FILL_REPEAT / unit_len).max(1);
                let fill = ch.repeat(fill_len);
                let mut escaped_fill = String::new();
                push_escaped_html_text(&mut escaped_fill, &fill);
                parts.push(format!(
                    "<span style=\"flex:1;overflow:hidden;white-space:nowrap;\
                     min-width:0;padding:0 0.15em\">{escaped_fill}</span>"
                ));
            }
            other => {
                let mut inner = String::new();
                match other {
                    ContentItem::String(s) => push_escaped_html_text(&mut inner, s),
                    ContentItem::Counter { name, style } => match name.as_str() {
                        "page" => inner.push_str(&format_counter(page_num as i32, *style)),
                        "pages" => inner.push_str(&format_counter(total_pages as i32, *style)),
                        _ => {
                            let value = custom_counters.get(name.as_str()).copied().unwrap_or(0);
                            inner.push_str(&format_counter(value, *style));
                        }
                    },
                    ContentItem::Element { name, policy } => {
                        if let Some(html) =
                            resolve_element_policy(name, *policy, page_idx, running_states, store)
                        {
                            inner.push_str(html);
                        }
                    }
                    ContentItem::StringRef { name, policy } => {
                        if let Some(state) = string_set_states.get(name) {
                            push_escaped_html_text(
                                &mut inner,
                                resolve_string_policy(state, *policy),
                            );
                        }
                    }
                    ContentItem::Counters {
                        name,
                        separator: _,
                        style,
                    } => inner.push_str(&resolve_counters_margin_box(
                        name,
                        *style,
                        page_num,
                        total_pages,
                        custom_counters,
                    )),
                    ContentItem::TargetCounter {
                        url_attr,
                        counter_name,
                        style,
                    } if url_attr == "href" => {
                        let href = implicit_href.unwrap_or("");
                        if let Some(map) = anchor_map {
                            push_escaped_html_text(
                                &mut inner,
                                &crate::gcpm::target_ref::resolve_target_counter(
                                    href,
                                    counter_name,
                                    *style,
                                    map,
                                ),
                            );
                        }
                    }
                    ContentItem::TargetCounters {
                        url_attr,
                        counter_name,
                        separator,
                        style,
                    } if url_attr == "href" => {
                        let href = implicit_href.unwrap_or("");
                        if let Some(map) = anchor_map {
                            push_escaped_html_text(
                                &mut inner,
                                &crate::gcpm::target_ref::resolve_target_counters(
                                    href,
                                    counter_name,
                                    separator,
                                    *style,
                                    map,
                                ),
                            );
                        }
                    }
                    ContentItem::TargetText { url_attr } if url_attr == "href" => {
                        let href = implicit_href.unwrap_or("");
                        if let Some(map) = anchor_map {
                            push_escaped_html_text(
                                &mut inner,
                                &crate::gcpm::target_ref::resolve_target_text(href, map),
                            );
                        }
                    }
                    _ => {}
                }
                if !inner.is_empty() {
                    parts.push(format!("<span>{inner}</span>"));
                }
            }
        }
    }

    format!(
        "<span style=\"display:flex;width:100%;align-items:baseline\">{}</span>",
        parts.join("")
    )
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

/// Format a list of counter values according to the given
/// [`CounterStyle`] and join them by `separator`. Returns an empty
/// string when `values` is empty.
pub fn format_counter_chain(values: &[i32], separator: &str, style: CounterStyle) -> String {
    let mut out = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(separator);
        }
        out.push_str(&format_counter(*v, style));
    }
    out
}

/// Resolve a `counters(name, sep, style)` reference in a margin-box
/// context where there is no DOM tree and only flat per-page counter
/// snapshots are available. Built-in `page` / `pages` use the page
/// scalars; custom names degrade to a single-value chain (equivalent
/// to `counter()`) because `custom_counters` only holds innermost
/// values. The separator is unused — single-element chains have
/// nowhere to insert it.
fn resolve_counters_margin_box(
    name: &str,
    style: CounterStyle,
    page: usize,
    total_pages: usize,
    custom_counters: &BTreeMap<String, i32>,
) -> String {
    match name {
        "page" => format_counter(page as i32, style),
        "pages" => format_counter(total_pages as i32, style),
        _ => custom_counters
            .get(name)
            .map(|v| format_counter(*v, style))
            .unwrap_or_default(),
    }
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

/// Sentinel `parent_id` for instances created via the legacy flat API
/// (`reset` / `increment` / `set`) and for implicit-root instances
/// created when `*_in_scope` is called without a prior reset.
/// `usize::MAX` is unreachable as a real DOM node id because Blitz's
/// node ids are dense low integers, so `leave_element(usize::MAX)`
/// will never accidentally match.
pub const COUNTER_ROOT_PARENT: usize = usize::MAX;

/// A single counter instance: a value scoped to the originating
/// element's parent's subtree (CSS Lists 3 §4.5).
#[derive(Debug, Clone)]
struct CounterInstance {
    /// Parent node id of the element that pushed this instance.
    /// `leave_element(X)` pops while top.parent_id == X.
    parent_id: usize,
    value: i32,
}

/// Tracks CSS counter values during DOM traversal.
///
/// Internally stores a stack of instances per name (CSS Lists 3 scope:
/// element + descendants + following siblings + their descendants).
/// `counter()` returns the innermost (top of stack) value;
/// `counters()` joins all values from outermost to innermost.
#[derive(Debug, Clone, Default)]
pub struct CounterState {
    stacks: BTreeMap<String, Vec<CounterInstance>>,
}

impl CounterState {
    pub fn new() -> Self {
        Self::default()
    }

    // ----- Legacy flat API (no scope tracking) -----

    /// Reset (or create) a counter at the implicit-root scope.
    /// Used by `pagination_layout::collect_counter_states` which
    /// reuses one `CounterState` across all pages and never calls
    /// `leave_element`. Therefore this overwrites the existing root
    /// instance rather than pushing — preserving the old flat-map
    /// `BTreeMap::insert` semantics and preventing unbounded stack
    /// growth across pages.
    pub fn reset(&mut self, name: &str, value: i32) {
        let stack = self.stacks.entry(name.to_string()).or_default();
        if let Some(top) = stack.last_mut() {
            if top.parent_id == COUNTER_ROOT_PARENT {
                top.value = value;
                return;
            }
        }
        stack.push(CounterInstance {
            parent_id: COUNTER_ROOT_PARENT,
            value,
        });
    }

    pub fn increment(&mut self, name: &str, value: i32) {
        self.increment_in_scope(name, value, COUNTER_ROOT_PARENT);
    }

    pub fn set(&mut self, name: &str, value: i32) {
        self.set_in_scope(name, value, COUNTER_ROOT_PARENT);
    }

    /// Innermost active value for `name`, or 0 if none.
    pub fn get(&self, name: &str) -> i32 {
        self.stacks
            .get(name)
            .and_then(|s| s.last())
            .map(|i| i.value)
            .unwrap_or(0)
    }

    /// Flat snapshot mapping each name to its innermost value.
    /// Used by margin-box rendering via `collect_counter_states`.
    pub fn snapshot(&self) -> BTreeMap<String, i32> {
        self.stacks
            .iter()
            .filter_map(|(k, v)| v.last().map(|i| (k.clone(), i.value)))
            .collect()
    }

    // ----- Scope-aware API (used by CounterPass) -----

    /// Push a new counter instance scoped to `parent_id`'s subtree.
    pub fn reset_in_scope(&mut self, name: &str, value: i32, parent_id: usize) {
        self.stacks
            .entry(name.to_string())
            .or_default()
            .push(CounterInstance { parent_id, value });
    }

    /// Increment the innermost active instance of `name`. If no
    /// instance exists, create an implicit-root instance and apply
    /// the increment (CSS Lists 3: implicit root reset).
    pub fn increment_in_scope(&mut self, name: &str, value: i32, _parent_id: usize) {
        let stack = self.stacks.entry(name.to_string()).or_default();
        if let Some(top) = stack.last_mut() {
            top.value += value;
        } else {
            stack.push(CounterInstance {
                parent_id: COUNTER_ROOT_PARENT,
                value,
            });
        }
    }

    /// Set the innermost active instance of `name` to `value`. If no
    /// instance exists, create an implicit-root one at `value`.
    pub fn set_in_scope(&mut self, name: &str, value: i32, _parent_id: usize) {
        let stack = self.stacks.entry(name.to_string()).or_default();
        if let Some(top) = stack.last_mut() {
            top.value = value;
        } else {
            stack.push(CounterInstance {
                parent_id: COUNTER_ROOT_PARENT,
                value,
            });
        }
    }

    /// Pop instances created by direct children of `node_id`. Call in
    /// post-order (after recursing into a node's children, when the
    /// node itself is about to return to its parent).
    pub fn leave_element(&mut self, node_id: usize) {
        for stack in self.stacks.values_mut() {
            while stack.last().is_some_and(|i| i.parent_id == node_id) {
                stack.pop();
            }
        }
    }

    /// Chain of values for `name`, from outermost to innermost.
    /// Empty if no instance exists.
    pub fn chain(&self, name: &str) -> Vec<i32> {
        self.stacks
            .get(name)
            .map(|s| s.iter().map(|i| i.value).collect())
            .unwrap_or_default()
    }

    /// Snapshot of every counter's full chain (outer→inner). Used by
    /// `CounterPass::take_node_snapshots` for `BookmarkPass`.
    pub fn chain_snapshot(&self) -> BTreeMap<String, Vec<i32>> {
        self.stacks
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|i| i.value).collect()))
            .collect()
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

    #[test]
    fn test_resolve_content_to_html_with_leader_wraps_in_flex() {
        use crate::gcpm::running::RunningElementStore;
        use crate::gcpm::{ContentItem, CounterStyle, LeaderStyle};
        use std::collections::BTreeMap;

        let items = vec![
            ContentItem::String("Title".into()),
            ContentItem::Leader {
                style: LeaderStyle::Dotted,
            },
            ContentItem::Counter {
                name: "page".into(),
                style: CounterStyle::Decimal,
            },
        ];
        let store = RunningElementStore::new();
        let html = resolve_content_to_html(
            &items,
            &store,
            &[],
            &BTreeMap::new(),
            1,
            5,
            0,
            &BTreeMap::new(),
        );

        assert!(
            html.contains("display:flex"),
            "expected flex container, got: {html}"
        );
        assert!(
            html.contains("flex:1"),
            "expected flex:1 leader, got: {html}"
        );
        assert!(html.contains("Title"), "expected Title, got: {html}");
        assert!(html.contains('1'), "expected page number, got: {html}");
        assert!(
            html.contains(".."),
            "expected dotted leader fill, got: {html}"
        );
    }

    #[test]
    fn test_resolve_content_to_html_without_leader_is_flat() {
        use crate::gcpm::running::RunningElementStore;
        use crate::gcpm::{ContentItem, CounterStyle};
        use std::collections::BTreeMap;

        let items = vec![
            ContentItem::String("Page ".into()),
            ContentItem::Counter {
                name: "page".into(),
                style: CounterStyle::Decimal,
            },
        ];
        let store = RunningElementStore::new();
        let html = resolve_content_to_html(
            &items,
            &store,
            &[],
            &BTreeMap::new(),
            3,
            10,
            2,
            &BTreeMap::new(),
        );

        assert!(
            !html.contains("display:flex"),
            "unexpected flex wrapper: {html}"
        );
        assert!(html.contains("Page "), "expected 'Page ', got: {html}");
        assert!(html.contains('3'), "expected page number 3, got: {html}");
    }

    #[test]
    fn test_resolve_content_to_string_with_string_ref() {
        let items = vec![
            ContentItem::String("Ch: ".into()),
            ContentItem::StringRef {
                name: "chapter".to_string(),
                policy: StringPolicy::First,
            },
        ];
        let mut states = BTreeMap::new();
        states.insert(
            "chapter".to_string(),
            StringSetPageState {
                start: None,
                first: Some("Introduction".to_string()),
                last: Some("Introduction".to_string()),
            },
        );
        assert_eq!(
            resolve_content_to_string(&items, &states, 1, 5, &BTreeMap::new()),
            "Ch: Introduction"
        );
    }

    #[test]
    fn test_resolve_content_to_string_leader_is_empty() {
        use crate::gcpm::LeaderStyle;

        let items = vec![
            ContentItem::String("A".into()),
            ContentItem::Leader {
                style: LeaderStyle::Dotted,
            },
            ContentItem::String("B".into()),
        ];
        assert_eq!(
            resolve_content_to_string(&items, &BTreeMap::new(), 1, 1, &BTreeMap::new()),
            "AB"
        );
    }

    #[test]
    fn test_resolve_content_to_html_flat_custom_counter() {
        use crate::gcpm::running::RunningElementStore;
        use crate::gcpm::{ContentItem, CounterStyle};
        use std::collections::BTreeMap;

        let items = vec![
            ContentItem::String("§".into()),
            ContentItem::Counter {
                name: "section".into(),
                style: CounterStyle::Decimal,
            },
        ];
        let mut custom = BTreeMap::new();
        custom.insert("section".to_string(), 7_i32);
        let store = RunningElementStore::new();
        let html = resolve_content_to_html(&items, &store, &[], &BTreeMap::new(), 1, 1, 0, &custom);

        assert!(!html.contains("display:flex"), "unexpected flex: {html}");
        assert!(html.contains('7'), "expected section 7, got: {html}");
    }

    #[test]
    fn test_resolve_content_to_html_flex_pages_and_custom_counter() {
        use crate::gcpm::{ContentItem, CounterStyle, LeaderStyle};

        let mut custom = BTreeMap::new();
        custom.insert("ch".to_string(), 3_i32);
        let items = vec![
            ContentItem::Counter {
                name: "ch".into(),
                style: CounterStyle::Decimal,
            },
            ContentItem::Leader {
                style: LeaderStyle::Dotted,
            },
            ContentItem::Counter {
                name: "pages".into(),
                style: CounterStyle::Decimal,
            },
        ];
        let store = RunningElementStore::new();
        let html = resolve_content_to_html(&items, &store, &[], &BTreeMap::new(), 1, 7, 0, &custom);

        assert!(html.contains("display:flex"), "expected flex: {html}");
        assert!(html.contains('3'), "expected ch=3, got: {html}");
        assert!(html.contains('7'), "expected total pages=7, got: {html}");
    }

    #[test]
    fn test_resolve_content_to_html_flex_with_element_and_string_ref() {
        use crate::gcpm::{ContentItem, LeaderStyle, StringPolicy};
        use crate::pagination_layout::StringSetPageState;

        let (store, states) = single_page_store("hdr", "<b>Title</b>");
        let mut str_states = BTreeMap::new();
        str_states.insert(
            "chap".to_string(),
            StringSetPageState {
                start: None,
                first: Some("Intro".to_string()),
                last: Some("Intro".to_string()),
            },
        );

        let items = vec![
            ContentItem::Element {
                name: "hdr".into(),
                policy: ElementPolicy::First,
            },
            ContentItem::Leader {
                style: LeaderStyle::Solid,
            },
            ContentItem::StringRef {
                name: "chap".into(),
                policy: StringPolicy::First,
            },
        ];
        let html = resolve_content_to_html(
            &items,
            &store,
            &states,
            &str_states,
            1,
            1,
            0,
            &BTreeMap::new(),
        );

        assert!(html.contains("display:flex"), "expected flex: {html}");
        assert!(
            html.contains("<b>Title</b>"),
            "expected element html: {html}"
        );
        assert!(html.contains("Intro"), "expected string ref: {html}");
        assert!(html.contains('_'), "expected solid leader: {html}");
    }

    #[test]
    fn test_resolve_content_to_html_flex_noop_variants() {
        use crate::gcpm::{ContentItem, LeaderStyle};

        let items = vec![
            ContentItem::ContentText,
            ContentItem::Attr("href".into()),
            ContentItem::Leader {
                style: LeaderStyle::Space,
            },
        ];
        let store = RunningElementStore::new();
        let html = resolve_content_to_html(
            &items,
            &store,
            &[],
            &BTreeMap::new(),
            1,
            1,
            0,
            &BTreeMap::new(),
        );

        assert!(html.contains("display:flex"), "expected flex: {html}");
        // ContentText and Attr produce no output, so no inner <span> should appear
        assert!(
            !html.contains("<span></span>"),
            "unexpected empty non-leader span: {html}"
        );
    }

    #[test]
    fn test_resolve_content_to_html_flat_noop_variants() {
        use crate::gcpm::ContentItem;

        let items = vec![
            ContentItem::ContentText,
            ContentItem::ContentBefore,
            ContentItem::ContentAfter,
            ContentItem::Attr("href".into()),
        ];
        let store = RunningElementStore::new();
        let html = resolve_content_to_html(
            &items,
            &store,
            &[],
            &BTreeMap::new(),
            1,
            1,
            0,
            &BTreeMap::new(),
        );

        assert!(!html.contains("display:flex"), "unexpected flex: {html}");
        assert_eq!(html, "", "expected empty output, got: {html}");
    }

    #[test]
    fn test_counter_state_in_scope_basic() {
        let mut s = CounterState::new();
        s.reset_in_scope("section", 0, 1); // E (parent=1) creates instance
        s.increment_in_scope("section", 1, 1);
        assert_eq!(s.get("section"), 1);
        assert_eq!(s.chain("section"), vec![1]);
    }

    #[test]
    fn test_counter_state_nested_chain() {
        // Stack model: parent_id is the parent of the resetting element.
        let mut s = CounterState::new();
        s.reset_in_scope("item", 0, 1); // ol#2's reset: parent=1
        s.increment_in_scope("item", 1, 2); // li#3's increment, target=top
        s.reset_in_scope("item", 0, 3); // ol#4's reset: parent=3
        s.increment_in_scope("item", 1, 4); // li#5's increment, target=top
        assert_eq!(s.get("item"), 1);
        assert_eq!(s.chain("item"), vec![1, 1]);
    }

    #[test]
    fn test_counter_state_leave_element_pops_children() {
        let mut s = CounterState::new();
        s.reset_in_scope("item", 0, 1); // pushed by element whose parent is 1
        s.reset_in_scope("item", 0, 2); // pushed by element whose parent is 2
        assert_eq!(s.chain("item"), vec![0, 0]);
        s.leave_element(2); // children of 2 popped
        assert_eq!(s.chain("item"), vec![0]);
        s.leave_element(1); // children of 1 popped
        assert_eq!(s.chain("item"), Vec::<i32>::new());
    }

    #[test]
    fn test_counter_state_increment_without_reset_implicit_root() {
        // CSS spec: counter-increment on a counter not in scope creates an
        // implicit root counter at value 0 then applies the increment.
        let mut s = CounterState::new();
        s.increment_in_scope("foo", 5, 42);
        assert_eq!(s.get("foo"), 5);
        assert_eq!(s.chain("foo"), vec![5]);
        // The implicit root instance lives forever — leave_element on the
        // recorded parent never pops it (sentinel value differs).
        s.leave_element(42);
        assert_eq!(s.chain("foo"), vec![5]);
    }

    #[test]
    fn test_counter_state_existing_api_backwards_compatible() {
        // Public flat API (used by collect_counter_states) must keep working.
        let mut s = CounterState::new();
        s.reset("page", 0);
        s.increment("page", 1);
        assert_eq!(s.get("page"), 1);
        let snap = s.snapshot();
        assert_eq!(snap.get("page"), Some(&1));
    }

    #[test]
    fn test_counter_state_legacy_reset_does_not_grow_stack() {
        // Regression: collect_counter_states reuses one CounterState across
        // all pages and applies counter-reset on every page. Legacy `reset`
        // must overwrite the root instance, not push — otherwise chain_snapshot
        // grows O(pages).
        let mut s = CounterState::new();
        s.reset("page", 0);
        s.reset("page", 0);
        s.reset("page", 0);
        assert_eq!(s.chain("page"), vec![0]);
    }

    #[test]
    fn test_counter_state_chain_snapshot() {
        let mut s = CounterState::new();
        s.reset_in_scope("item", 0, 1);
        s.increment_in_scope("item", 1, 1);
        s.reset_in_scope("item", 0, 2);
        s.increment_in_scope("item", 2, 2);
        let chain_snap = s.chain_snapshot();
        assert_eq!(chain_snap.get("item"), Some(&vec![1, 2]));
    }

    #[test]
    fn test_format_counter_chain_basic() {
        assert_eq!(
            format_counter_chain(&[1, 2, 3], ".", CounterStyle::Decimal),
            "1.2.3"
        );
        assert_eq!(format_counter_chain(&[], ".", CounterStyle::Decimal), "");
        assert_eq!(format_counter_chain(&[5], ".", CounterStyle::Decimal), "5");
    }

    #[test]
    fn test_format_counter_chain_with_style() {
        assert_eq!(
            format_counter_chain(&[1, 4, 9], "-", CounterStyle::UpperRoman),
            "I-IV-IX"
        );
    }

    #[test]
    fn test_resolve_counters_in_margin_box_falls_back_to_single() {
        // Margin-box `counters()` only sees flat custom_counters; the
        // resolver degrades to single-value chain (equivalent to counter()).
        let items = vec![ContentItem::Counters {
            name: "chapter".into(),
            separator: ".".into(),
            style: CounterStyle::Decimal,
        }];
        let mut custom = BTreeMap::new();
        custom.insert("chapter".to_string(), 7);
        assert_eq!(
            resolve_content_to_string(&items, &BTreeMap::new(), 1, 1, &custom),
            "7"
        );
    }

    #[test]
    fn test_resolve_counters_margin_box_page_and_pages() {
        // Built-in `page` / `pages` route through the page-scalar branch,
        // bypassing custom_counters entirely.
        let page_item = vec![ContentItem::Counters {
            name: "page".into(),
            separator: ".".into(),
            style: CounterStyle::Decimal,
        }];
        let pages_item = vec![ContentItem::Counters {
            name: "pages".into(),
            separator: ".".into(),
            style: CounterStyle::UpperRoman,
        }];
        assert_eq!(
            resolve_content_to_string(&page_item, &BTreeMap::new(), 5, 12, &BTreeMap::new()),
            "5"
        );
        assert_eq!(
            resolve_content_to_string(&pages_item, &BTreeMap::new(), 5, 12, &BTreeMap::new()),
            "XII"
        );
    }

    #[test]
    fn test_set_in_scope_without_reset_creates_implicit_root() {
        // Like increment_in_scope, set_in_scope on an empty stack creates
        // an implicit-root instance at the given value.
        let mut s = CounterState::new();
        s.set_in_scope("foo", 9, 42);
        assert_eq!(s.get("foo"), 9);
        assert_eq!(s.chain("foo"), vec![9]);
        // Implicit-root instance survives leave_element on the recorded
        // parent — the sentinel parent_id (COUNTER_ROOT_PARENT) never
        // matches a real node id.
        s.leave_element(42);
        assert_eq!(s.chain("foo"), vec![9]);
    }

    #[test]
    fn test_resolve_counters_in_html_flat_mode() {
        // Covers the resolve_content_to_html flat-mode Counters arm
        // (no leader present → flat mode is selected).
        let items = vec![ContentItem::Counters {
            name: "chapter".into(),
            separator: ".".into(),
            style: CounterStyle::Decimal,
        }];
        let mut custom = BTreeMap::new();
        custom.insert("chapter".to_string(), 3);
        let store = RunningElementStore::new();
        let result =
            resolve_content_to_html(&items, &store, &[], &BTreeMap::new(), 1, 1, 0, &custom);
        assert_eq!(result, "3");
    }

    #[test]
    fn resolve_target_counter_in_margin_box() {
        use crate::gcpm::target_ref::{AnchorEntry, AnchorMap};
        let mut map = AnchorMap::new();
        let mut counters = BTreeMap::new();
        counters.insert("page".into(), vec![5]);
        map.insert(
            "x",
            AnchorEntry {
                page_num: 5,
                counters,
                text: "Hello".into(),
            },
        );
        let items = vec![ContentItem::TargetCounter {
            url_attr: "href".into(),
            counter_name: "page".into(),
            style: CounterStyle::Decimal,
        }];
        let states = BTreeMap::new();
        let custom = BTreeMap::new();
        let out = resolve_content_to_string_with_anchor(
            &items,
            &states,
            1,
            10,
            &custom,
            Some(&map),
            Some("#x"),
        );
        assert_eq!(out, "5");
    }

    #[test]
    fn test_resolve_counters_in_html_flex_mode_with_leader() {
        // A Leader item triggers flex mode in resolve_content_to_html.
        // The Counters value is wrapped in <span> like Counter is.
        let items = vec![
            ContentItem::Counters {
                name: "chapter".into(),
                separator: ".".into(),
                style: CounterStyle::Decimal,
            },
            ContentItem::Leader {
                style: super::super::LeaderStyle::Dotted,
            },
        ];
        let mut custom = BTreeMap::new();
        custom.insert("chapter".to_string(), 7);
        let store = RunningElementStore::new();
        let result =
            resolve_content_to_html(&items, &store, &[], &BTreeMap::new(), 1, 1, 0, &custom);
        assert!(
            result.contains("<span>7</span>"),
            "expected flex-mode counters() output to wrap value in <span>, got {result:?}"
        );
    }
}
