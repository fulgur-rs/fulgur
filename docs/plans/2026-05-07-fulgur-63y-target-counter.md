# GCPM target-counter() / target-text() Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` (or `superpowers:subagent-driven-development`) to implement this plan task-by-task.

**Goal:** Implement CSS GCPM `target-counter(<url>, <name>)`, `target-counters(<url>, <name>, <sep>)`, and `target-text(<url>)` so that table-of-contents authoring (`a::after { content: leader('.') target-counter(attr(href), page); }`) produces correct page numbers in the rendered PDF, including evaluation inside `@page` margin boxes.

**Architecture:**

- 2-pass render orchestrated by `Engine::render_html`. Pass 1 substitutes a fixed-width placeholder for every `target-*` call, runs the full pipeline, and harvests an `AnchorMap` (`fragment_id → { page_num, counter_chain_snapshot, text }`). Pass 2 re-runs the pipeline with the `AnchorMap` threaded into the content resolvers.
- Content `target-*` items are added as new `ContentItem` enum variants and threaded through (a) `CounterPass::resolve_content` for `::before` / `::after` injection CSS and (b) `gcpm::counter::resolve_content_to_string` / `resolve_content_to_html` for `@page` margin boxes.
- Failure resolves to empty string, never panics. URL form is restricted to `attr(href)` (literal URLs and other attributes are out of scope, follow-up issues exist on `bd`).

**Tech Stack:** Rust, cssparser, blitz-dom, krilla, fulgur's existing `gcpm` module.

**Coordinate / unit notes:** This change does not touch geometry or PDF coordinates. Fragment-id resolution operates on DOM node ids and pagination geometry's per-node page-number map only.

**Beads:** `fulgur-63y` (in_progress) is the parent. Follow-ups: `fulgur-5ho3`, `fulgur-11gd`, `fulgur-s4ec`, `fulgur-x70n`, `fulgur-ejw9`, `fulgur-38y2` — all blocked by `fulgur-63y`.

**Note on `fulgur-5ho3`:** This plan parses the `<counter-style>` 3rd argument AND threads it through `format_counter(value, *style)` end-to-end. All five existing `CounterStyle` variants (`Decimal`, `UpperRoman`, `LowerRoman`, `UpperAlpha`, `LowerAlpha`) work in `target-counter` / `target-counters`. `fulgur-5ho3` therefore closes alongside `fulgur-63y` in this PR. Any *new* counter styles (`decimal-leading-zero`, `@counter-style` rules) are out of scope and should be tracked under a separately scoped follow-up issue.

**Verified API surface (worktree-local recon at plan time):**

| Symbol / pattern | Location | Notes |
|---|---|---|
| `PaginationGeometryTable` | `pagination_layout.rs:129` | Type alias for `BTreeMap<usize, PaginationGeometry>`. No `page_for_node` method — add one in `target_ref.rs`. |
| `PaginationGeometry { fragments: Vec<Fragment>, is_repeat: bool }` | `pagination_layout.rs:108` | `Fragment.page_index: u32` (zero-based). Convert with `+1` for the 1-based page number used in the `AnchorMap`. |
| `text_data().content` | `pagination_layout.rs:516-517` | Field is `content: String`. |
| `blitz_adapter::get_attr(elem, name) -> Option<&str>` | `blitz_adapter.rs:1045` | Use this for href/id reads — do not improvise. Public from the adapter. |
| `GcpmContext.margin_boxes` | `gcpm/mod.rs:331` | Field name is `margin_boxes`, NOT `margin_box_rules`. |

---

## Task 1: Add `ContentItem::TargetCounter` / `TargetCounters` / `TargetText` variants

**Files:**

- Modify: `crates/fulgur/src/gcpm/mod.rs` — extend the `ContentItem` enum.
- Modify (touch only): every other file that exhaustively matches `ContentItem` so the project still compiles. Discovered list: `crates/fulgur/src/blitz_adapter.rs`, `crates/fulgur/src/gcpm/parser.rs`, `crates/fulgur/src/gcpm/counter.rs`, `crates/fulgur/src/gcpm/ua_css.rs`. Update each `match` over `ContentItem` to add a no-op arm for the three new variants.

**Step 1: Write a failing test**

Add to `crates/fulgur/src/gcpm/mod.rs` test module (or create one if not present):

```rust
#[cfg(test)]
mod content_item_target_tests {
    use super::*;

    #[test]
    fn target_counter_variant_default_style_is_decimal() {
        let item = ContentItem::TargetCounter {
            url_attr: "href".into(),
            counter_name: "page".into(),
            style: CounterStyle::Decimal,
        };
        match item {
            ContentItem::TargetCounter { counter_name, .. } => {
                assert_eq!(counter_name, "page");
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

**Step 2: Run test (must fail to compile because the variant doesn't exist)**

```bash
cargo test -p fulgur --lib content_item_target_tests::target_counter_variant_default_style_is_decimal
```

Expected: compile error "no variant `TargetCounter` on `ContentItem`".

**Step 3: Add the variants**

In `crates/fulgur/src/gcpm/mod.rs`, append to the `ContentItem` enum:

```rust
/// `target-counter(<url-attr>, <counter-name>)` — resolves the named
/// counter at the element identified by the URL fragment in
/// `attr(<url-attr>)`. fulgur-63y currently restricts `<url-attr>`
/// to `"href"`; other attribute names yield an empty string.
TargetCounter {
    /// Attribute name read from the matched element. Always lowercase.
    url_attr: String,
    /// Counter name being looked up at the target element.
    counter_name: String,
    /// Display style applied to the resolved value.
    style: CounterStyle,
},
/// `target-counters(<url-attr>, <counter-name>, <separator>)` —
/// like `TargetCounter` but joins the entire counter chain at the
/// target with `separator`.
TargetCounters {
    url_attr: String,
    counter_name: String,
    separator: String,
    style: CounterStyle,
},
/// `target-text(<url-attr>)` — resolves to the text content of the
/// target element. Only the default `content` form is implemented.
TargetText {
    url_attr: String,
},
```

**Step 4: Add no-op match arms in every exhaustive match**

For each file in the discovered list, locate `match` over a `ContentItem` value and add the pattern `ContentItem::TargetCounter { .. } | ContentItem::TargetCounters { .. } | ContentItem::TargetText { .. } => {}` (or a parallel placeholder branch returning the appropriate empty value).

After editing, confirm with:

```bash
grep -rn "ContentItem::" crates/fulgur/src/ --include="*.rs" | wc -l
cargo build -p fulgur 2>&1 | grep -E "error|warning: unreachable" | head
```

Expected: build succeeds; no E0004 (non-exhaustive) errors.

**Step 5: Run the new unit test**

```bash
cargo test -p fulgur --lib content_item_target_tests::target_counter_variant_default_style_is_decimal
```

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/fulgur/src/gcpm/mod.rs crates/fulgur/src/blitz_adapter.rs crates/fulgur/src/gcpm/parser.rs crates/fulgur/src/gcpm/counter.rs crates/fulgur/src/gcpm/ua_css.rs
git commit -m "feat(gcpm): add ContentItem::TargetCounter / TargetCounters / TargetText variants"
```

---

## Task 2: Parser support for `target-counter(...)` / `target-counters(...)` / `target-text(...)`

**Files:**

- Modify: `crates/fulgur/src/gcpm/parser.rs` — extend `parse_content_value` (currently around line 968-1100) to recognize the three new function tokens.
- Test: `crates/fulgur/src/gcpm/parser.rs` (existing `#[cfg(test)] mod tests`).

**Constraints (per design):**

- `<url>` argument: only `attr(href)` form. Anything else (string literal, `url(...)`, `attr(<other-name>)`) → emit nothing (skip the item gracefully).
- `target-counter` second argument: counter name (ident).
- `target-counters` arguments: counter name (ident), separator (string literal).
- `target-text` second argument: not implemented in this issue. If present, the entire item is dropped (so `target-text(attr(href), before)` does NOT silently behave like the default form). Tracked under fulgur-x70n.
- Counter-style 3rd argument: parsed AND honored end-to-end via `format_counter(value, *style)`. All five base `CounterStyle` variants (`Decimal`, `UpperRoman`, `LowerRoman`, `UpperAlpha`, `LowerAlpha`) work in `target-counter` / `target-counters`. The default is `CounterStyle::Decimal` when the argument is absent. Closes `fulgur-5ho3` (any *new* counter styles such as `decimal-leading-zero` or `@counter-style` rules are out of scope and should be tracked under a separately scoped issue).

**Step 1: Write failing parser tests**

Add to the parser tests module (search for `mod tests` in parser.rs):

```rust
#[test]
fn parse_target_counter_attr_href_page() {
    let css = r#".toc a::after { content: target-counter(attr(href), page); }"#;
    let g = parse_gcpm(css);
    let mapping = &g.content_counter_mappings[0];
    assert_eq!(
        mapping.content,
        vec![ContentItem::TargetCounter {
            url_attr: "href".into(),
            counter_name: "page".into(),
            style: CounterStyle::Decimal,
        }]
    );
}

#[test]
fn parse_target_counters_with_separator() {
    let css = r#".toc a::after { content: target-counters(attr(href), section, "."); }"#;
    let g = parse_gcpm(css);
    let mapping = &g.content_counter_mappings[0];
    assert_eq!(
        mapping.content,
        vec![ContentItem::TargetCounters {
            url_attr: "href".into(),
            counter_name: "section".into(),
            separator: ".".into(),
            style: CounterStyle::Decimal,
        }]
    );
}

#[test]
fn parse_target_text_default_form() {
    let css = r#".toc a::after { content: target-text(attr(href)); }"#;
    let g = parse_gcpm(css);
    let mapping = &g.content_counter_mappings[0];
    assert_eq!(
        mapping.content,
        vec![ContentItem::TargetText { url_attr: "href".into() }]
    );
}

#[test]
fn parse_target_counter_non_attr_url_drops_item() {
    let css = r#".toc a::after { content: target-counter("#sec1", page); }"#;
    let g = parse_gcpm(css);
    let mapping = &g.content_counter_mappings[0];
    assert!(mapping.content.iter().all(|i| !matches!(i, ContentItem::TargetCounter { .. })));
}
```

**Step 2: Run the tests (must FAIL)**

```bash
cargo test -p fulgur --lib parser::tests::parse_target_counter_attr_href_page
```

Expected: FAIL — parser silently skips the unknown function.

**Step 3: Extend `parse_content_value`**

In `parse_content_value`, after the existing `else if fn_name.eq_ignore_ascii_case("counters")` block, add three new branches:

```rust
} else if fn_name.eq_ignore_ascii_case("target-counter") {
    // Grammar: target-counter( attr(<name>) , <counter-name> [, <counter-style>]? )
    let url_attr = match parse_target_url_attr(input) {
        Some(name) => name,
        None => return Ok(()), // unsupported URL form: drop item
    };
    input.expect_comma()?;
    let counter_name = input.expect_ident()?.to_string();
    let style = input
        .try_parse(|input| {
            input.expect_comma()?;
            parse_counter_style(input)
        })
        .unwrap_or(CounterStyle::Decimal);
    items.push(ContentItem::TargetCounter {
        url_attr,
        counter_name,
        style,
    });
} else if fn_name.eq_ignore_ascii_case("target-counters") {
    let url_attr = match parse_target_url_attr(input) {
        Some(name) => name,
        None => return Ok(()),
    };
    input.expect_comma()?;
    let counter_name = input.expect_ident()?.to_string();
    input.expect_comma()?;
    let separator = match input.try_parse(|input| input.expect_string().map(|s| s.to_string())) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let style = input
        .try_parse(|input| {
            input.expect_comma()?;
            parse_counter_style(input)
        })
        .unwrap_or(CounterStyle::Decimal);
    items.push(ContentItem::TargetCounters {
        url_attr,
        counter_name,
        separator,
        style,
    });
} else if fn_name.eq_ignore_ascii_case("target-text") {
    let url_attr = match parse_target_url_attr(input) {
        Some(name) => name,
        None => return Ok(()),
    };
    // Optional 2nd ident is consumed but ignored (follow-up: fulgur-x70n).
    let _ = input.try_parse(|input| {
        input.expect_comma()?;
        input.expect_ident().map(|i| i.to_string())
    });
    items.push(ContentItem::TargetText { url_attr });
}
```

Add the helper near the top of the parser module (private):

```rust
/// Parse the `<url>` argument of a `target-*` function.
/// Currently only the `attr(<ident>)` form is recognized — everything
/// else (string literal, url(), `attr(<name>, <type>)`) returns `None`,
/// causing the surrounding item to be dropped silently per design.
fn parse_target_url_attr<'i>(input: &mut Parser<'i, '_>) -> Option<String> {
    input
        .try_parse(|input| {
            let token = input.next()?.clone();
            match token {
                Token::Function(ref name) if name.eq_ignore_ascii_case("attr") => {
                    input.parse_nested_block(|inner| {
                        let id = inner.expect_ident()?.to_string();
                        Ok::<_, ParseError<'_, ()>>(id)
                    })
                }
                _ => Err(input.new_error_for_next_token()),
            }
        })
        .ok()
        .map(|s| s.to_ascii_lowercase())
}
```

(The existing parser already imports `Parser`, `Token`, `ParseError`.)

**Step 4: Run tests (must PASS)**

```bash
cargo test -p fulgur --lib parser::tests::parse_target_
```

Expected: 4 passing tests.

**Step 5: Commit**

```bash
git add crates/fulgur/src/gcpm/parser.rs
git commit -m "feat(gcpm): parse target-counter / target-counters / target-text grammar"
```

---

## Task 3: `gcpm::target_ref` module — AnchorMap data type and resolvers

**Files:**

- Create: `crates/fulgur/src/gcpm/target_ref.rs`
- Modify: `crates/fulgur/src/gcpm/mod.rs` — add `pub mod target_ref;`

**Step 1: Write failing tests in the new file**

Create `crates/fulgur/src/gcpm/target_ref.rs`:

```rust
//! Cross-reference resolution for CSS GCPM `target-counter()` /
//! `target-counters()` / `target-text()`.
//!
//! `AnchorMap` is built at the end of pass 1 (after pagination has
//! assigned each DOM element a page) and consumed by pass 2 via the
//! resolver helpers below. See `docs/plans/2026-05-07-fulgur-63y-target-counter.md`.

use crate::gcpm::CounterStyle;
use crate::gcpm::counter::{format_counter, format_counter_chain};
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
    let Some(chain) = entry.counters.get(counter_name) else {
        if counter_name == "page" {
            return format_counter(entry.page_num as i32, style);
        }
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
}
```

**Step 2: Add module declaration**

In `crates/fulgur/src/gcpm/mod.rs`, near the other `pub mod` lines:

```rust
pub mod target_ref;
```

**Step 3: Run tests**

```bash
cargo test -p fulgur --lib gcpm::target_ref
```

Expected: 9 PASS.

**Step 4: Commit**

```bash
git add crates/fulgur/src/gcpm/target_ref.rs crates/fulgur/src/gcpm/mod.rs
git commit -m "feat(gcpm): add target_ref module with AnchorMap and resolver helpers"
```

---

## Task 4: Detection helper — does the GCPM context contain any `target-*` item?

**Files:**

- Modify: `crates/fulgur/src/gcpm/mod.rs` — add a helper on `ContentItem` and on `GcpmContext`.

**Step 1: Failing test**

Add to `gcpm/mod.rs` test module:

```rust
#[test]
fn gcpm_context_target_detection() {
    let mut ctx = GcpmContext::default();
    assert!(!ctx.has_target_references());
    ctx.content_counter_mappings.push(ContentCounterMapping {
        parsed: ParsedSelector::dummy_for_test(),
        pseudo: PseudoElement::Before,
        content: vec![ContentItem::TargetCounter {
            url_attr: "href".into(),
            counter_name: "page".into(),
            style: CounterStyle::Decimal,
        }],
    });
    assert!(ctx.has_target_references());
}
```

If `ParsedSelector::dummy_for_test` does not exist, replace the `parsed` field with the existing zero-construct path used by other tests in `gcpm/parser.rs`. Search for an existing test that constructs a `ContentCounterMapping` and reuse the same pattern.

**Step 2: Run (FAIL — `has_target_references` not defined)**

```bash
cargo test -p fulgur --lib gcpm_context_target_detection
```

**Step 3: Implement detection**

Add in `gcpm/mod.rs`:

```rust
impl ContentItem {
    /// Returns true if this item is a `target-counter` / `target-counters`
    /// / `target-text` reference. Used to gate fulgur's 2-pass render.
    pub fn is_target_reference(&self) -> bool {
        matches!(
            self,
            ContentItem::TargetCounter { .. }
                | ContentItem::TargetCounters { .. }
                | ContentItem::TargetText { .. }
        )
    }
}

impl GcpmContext {
    /// Returns true if any margin-box rule, content-counter mapping,
    /// or string-set value contains a `target-*` item. Triggers the
    /// 2-pass pipeline; otherwise the single-pass fast path runs.
    pub fn has_target_references(&self) -> bool {
        self.margin_boxes
            .iter()
            .any(|r| r.content.iter().any(ContentItem::is_target_reference))
            || self
                .content_counter_mappings
                .iter()
                .any(|m| m.content.iter().any(ContentItem::is_target_reference))
    }
}
```

**Step 4: Run**

```bash
cargo test -p fulgur --lib gcpm_context_target_detection
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/gcpm/mod.rs
git commit -m "feat(gcpm): add has_target_references gate for 2-pass render"
```

---

## Task 5: Plumb optional `AnchorMap` through margin-box content resolvers

**Files:**

- Modify: `crates/fulgur/src/gcpm/counter.rs` — extend `resolve_content_to_string` and `resolve_content_to_html` to accept `Option<&AnchorMap>`.
- Modify: every caller in `crates/fulgur/src/render.rs` (margin-box rendering uses these functions). Also any test caller in `gcpm/counter.rs::tests` and integration tests `crates/fulgur/tests/gcpm_*`.

**Note on breaking the public signature:** these helpers are crate-internal; cross-crate consumers go through `Engine::render_html`. We add a new optional parameter at the END of the existing argument list to keep the diff minimal.

**Step 1: Failing test**

Add to `gcpm/counter.rs` tests:

```rust
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
    // implicit href = "#x" (margin-box context: caller passes the
    // current-page implicit href; in unit test we pass directly).
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
```

**Step 2: Run (FAIL: function does not exist)**

**Step 3: Implement**

In `gcpm/counter.rs`, introduce a new public function `resolve_content_to_string_with_anchor` that takes `(items, states, page, total_pages, custom_counters, anchor_map: Option<&AnchorMap>, implicit_href: Option<&str>)`. Refactor the existing `resolve_content_to_string` to call the new function with `None, None`. Add match arms for the three new variants:

```rust
ContentItem::TargetCounter { url_attr, counter_name, style } => {
    if url_attr != "href" { continue; }
    let href = implicit_href.unwrap_or("");
    if let Some(map) = anchor_map {
        out.push_str(&crate::gcpm::target_ref::resolve_target_counter(
            href, counter_name, *style, map,
        ));
    }
    // else: pass-1 placeholder mode — write nothing (the placeholder
    // injection happens at the parser/serializer level for ::before/::after;
    // margin boxes simply emit empty during pass 1).
}
ContentItem::TargetCounters { url_attr, counter_name, separator, style } => {
    if url_attr != "href" { continue; }
    let href = implicit_href.unwrap_or("");
    if let Some(map) = anchor_map {
        out.push_str(&crate::gcpm::target_ref::resolve_target_counters(
            href, counter_name, separator, *style, map,
        ));
    }
}
ContentItem::TargetText { url_attr } => {
    if url_attr != "href" { continue; }
    let href = implicit_href.unwrap_or("");
    if let Some(map) = anchor_map {
        out.push_str(&crate::gcpm::target_ref::resolve_target_text(href, map));
    }
}
```

Apply the same refactor to `resolve_content_to_html` → `resolve_content_to_html_with_anchor`. Make the existing public functions thin wrappers that pass `None, None` so external callers remain compiling.

**Step 4: Run**

```bash
cargo test -p fulgur --lib resolve_target_counter_in_margin_box
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/gcpm/counter.rs
git commit -m "feat(gcpm): thread Option<&AnchorMap> through margin-box content resolvers"
```

---

## Task 6: Plumb `AnchorMap` through `CounterPass::resolve_content` for `::before` / `::after`

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` — `CounterPass::resolve_content` (around line 1886) and constructor.

**Step 1: Failing test**

Add to the existing `mod tests` block in `blitz_adapter.rs` that already covers CounterPass:

```rust
#[test]
fn counter_pass_resolves_target_counter_with_anchor_map() {
    use crate::gcpm::target_ref::{AnchorEntry, AnchorMap};
    use crate::gcpm::{ContentCounterMapping, ContentItem, CounterStyle, PseudoElement};

    let html = r#"<html><body><a class="ref" href="#sec1">Sec1</a></body></html>"#;
    let mut doc = blitz_html::HtmlDocument::from_html(html, Default::default());
    let parsed = parse_selector(".ref").unwrap();
    let content = vec![ContentItem::TargetCounter {
        url_attr: "href".into(),
        counter_name: "page".into(),
        style: CounterStyle::Decimal,
    }];
    let mappings = vec![ContentCounterMapping {
        parsed,
        pseudo: PseudoElement::After,
        content,
    }];

    let mut anchor = AnchorMap::new();
    let mut counters = std::collections::BTreeMap::new();
    counters.insert("page".into(), vec![3]);
    anchor.insert(
        "sec1",
        AnchorEntry {
            page_num: 3,
            counters,
            text: String::new(),
        },
    );

    let pass = CounterPass::new(Vec::new(), mappings).with_anchor_map(anchor);
    let ctx = PassContext { font_data: &[] };
    pass.apply(&mut doc, &ctx);
    let (_, css) = pass.into_parts();
    assert!(css.contains("\"3\""), "CSS = {css}");
}
```

(The exact `parse_selector` helper or a similar utility is already used in nearby CounterPass tests. Reuse the same pattern.)

**Step 2: Run (FAIL: `with_anchor_map` does not exist)**

**Step 3: Implement**

In `blitz_adapter.rs`:

- Add a private `anchor_map: RefCell<Option<AnchorMap>>` field to `CounterPass`.
- Add `pub fn with_anchor_map(mut self, map: AnchorMap) -> Self` that stores it.
- Read the matched element's `href` attribute (if any) using the existing element-data API, and pass that as `implicit_href` to a new branch in `CounterPass::resolve_content` that handles the three new variants. Use the same fallback (`""`) when `href` is missing.
- For pass 1 (no anchor map set), substitute a fixed-width placeholder: `"00"` for `TargetCounter` / `TargetCounters` (decimal-style), `" "` for `TargetText`. This keeps line breaks roughly stable across the two passes.

Concrete edit (pseudo-diff inside `resolve_content`):

```rust
fn resolve_content(&self, items: &[ContentItem], element_href: Option<&str>) -> String {
    let state = self.state.borrow();
    let anchor = self.anchor_map.borrow();
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::Counter { name, style } => { ... existing ... }
            ContentItem::Counters { ... } => { ... existing ... }
            ContentItem::TargetCounter { url_attr, counter_name, style } => {
                if url_attr != "href" { continue; }
                let href = element_href.unwrap_or("");
                match anchor.as_ref() {
                    Some(map) => out.push_str(&crate::gcpm::target_ref::resolve_target_counter(
                        href, counter_name, *style, map,
                    )),
                    None => out.push_str("00"),
                }
            }
            ContentItem::TargetCounters { url_attr, counter_name, separator, style } => {
                if url_attr != "href" { continue; }
                let href = element_href.unwrap_or("");
                match anchor.as_ref() {
                    Some(map) => out.push_str(&crate::gcpm::target_ref::resolve_target_counters(
                        href, counter_name, separator, *style, map,
                    )),
                    None => out.push_str("00"),
                }
            }
            ContentItem::TargetText { url_attr } => {
                if url_attr != "href" { continue; }
                let href = element_href.unwrap_or("");
                match anchor.as_ref() {
                    Some(map) => out.push_str(&crate::gcpm::target_ref::resolve_target_text(href, map)),
                    None => out.push_str(" "),
                }
            }
            _ => {}
        }
    }
    out
}
```

Update the two call sites (`for idx in &before_indices` / `for idx in &after_indices`) to extract `element_href`. Use the public `get_attr` helper already defined in `blitz_adapter.rs:1045`:

```rust
let element_href = doc
    .get_node(node_id)
    .and_then(|n| n.element_data())
    .and_then(|e| get_attr(e, "href").map(str::to_owned));
let resolved = self.resolve_content(&mapping.content, element_href.as_deref());
```

**Step 4: Run**

```bash
cargo test -p fulgur --lib counter_pass_resolves_target_counter_with_anchor_map
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(gcpm): wire AnchorMap into CounterPass for target-* in ::before/::after"
```

---

## Task 7: Build the AnchorMap from pagination geometry + counter snapshots + DOM

**Files:**

- Create: helper `crate::gcpm::target_ref::collect_anchor_map` (extend `target_ref.rs`).
- Modify: `crates/fulgur/src/engine.rs` — call the collector at the end of pass 1.

**Inputs available at the end of pass 1:**

- `pagination_geometry: PaginationGeometryTable` — gives page assignment per node.
- `counter_snapshots: BTreeMap<usize, BTreeMap<String, Vec<i32>>>` — already harvested by `CounterPass::take_node_snapshots` when bookmarks are enabled. We will need this snapshot regardless of bookmark setting when target references exist; gate `record_node_snapshots` on `gcpm.has_target_references()` too.
- `doc: HtmlDocument` — used to read each element's `id` attribute and `text_content`.

**Step 1: Failing test**

In `target_ref.rs` test module, add:

```rust
#[test]
fn collect_anchor_map_from_synthetic_inputs() {
    use crate::gcpm::target_ref::{collect_anchor_map_from_records, FragmentRecord};
    let records = vec![
        FragmentRecord {
            fragment_id: "sec1".into(),
            page_num: 2,
            counters: {
                let mut m = BTreeMap::new();
                m.insert("section".into(), vec![1]);
                m
            },
            text: "Section One".into(),
        },
    ];
    let map = collect_anchor_map_from_records(records);
    let entry = map.get("sec1").expect("should have entry");
    assert_eq!(entry.page_num, 2);
    assert_eq!(entry.counters.get("section"), Some(&vec![1]));
    assert_eq!(entry.text, "Section One");
}
```

**Step 2: FAIL**

**Step 3: Implement helpers**

Add in `target_ref.rs`:

```rust
pub struct FragmentRecord {
    pub fragment_id: String,
    pub page_num: u32,
    pub counters: BTreeMap<String, Vec<i32>>,
    pub text: String,
}

pub fn collect_anchor_map_from_records(records: Vec<FragmentRecord>) -> AnchorMap {
    let mut map = AnchorMap::new();
    for r in records {
        map.insert(
            r.fragment_id,
            AnchorEntry {
                page_num: r.page_num,
                counters: r.counters,
                text: r.text,
            },
        );
    }
    map
}
```

Then add the higher-level builder. `target_ref.rs` keeps a small typed `page_for_node` helper that operates directly on the public `PaginationGeometryTable` alias (no `impl` block needed because of the type alias):

```rust
// in target_ref.rs (no foreign imports needed beyond what's already there)
use crate::pagination_layout::PaginationGeometryTable;

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
```

Place the higher-level builder in `engine.rs` (it needs `HtmlDocument` and crate-internal access). Use the existing `blitz_adapter::get_attr` helper rather than improvising attribute reads, and read `text_data().content` per the verified field.

```rust
use crate::blitz_adapter::get_attr;
use crate::gcpm::target_ref::{page_for_node, AnchorEntry, AnchorMap};
use blitz_dom::BaseDocument;
use std::collections::BTreeMap;
use std::ops::Deref;

fn build_anchor_map(
    doc: &BaseDocument,
    pagination_geometry: &PaginationGeometryTable,
    counter_snapshots: &BTreeMap<usize, BTreeMap<String, Vec<i32>>>,
) -> AnchorMap {
    let mut map = AnchorMap::new();
    walk_anchors(doc, doc.root_element().id, pagination_geometry, counter_snapshots, &mut map);
    map
}

fn walk_anchors(
    doc: &BaseDocument,
    node_id: usize,
    geometry: &PaginationGeometryTable,
    snapshots: &BTreeMap<usize, BTreeMap<String, Vec<i32>>>,
    out: &mut AnchorMap,
) {
    let Some(node) = doc.get_node(node_id) else { return };
    if let Some(elem) = node.element_data() {
        if let Some(frag) = get_attr(elem, "id") {
            let page_num = page_for_node(geometry, node_id).unwrap_or(0);
            let counters = snapshots.get(&node_id).cloned().unwrap_or_default();
            let text = collect_text_content(doc, node_id);
            out.insert(
                frag.to_string(),
                AnchorEntry { page_num, counters, text },
            );
        }
    }
    let children: Vec<usize> = node.children.clone();
    for c in children {
        walk_anchors(doc, c, geometry, snapshots, out);
    }
}

fn collect_text_content(doc: &BaseDocument, node_id: usize) -> String {
    fn walk(doc: &BaseDocument, id: usize, out: &mut String) {
        if let Some(node) = doc.get_node(id) {
            if let Some(text) = node.text_data() {
                out.push_str(&text.content);
            }
            for c in &node.children {
                walk(doc, *c, out);
            }
        }
    }
    let mut s = String::new();
    walk(doc, node_id, &mut s);
    s.trim().split_whitespace().collect::<Vec<_>>().join(" ")
}
```

The exact `BaseDocument` reference style (`&doc`, `doc.deref()`, etc.) follows whatever the surrounding `Engine::render_html` already uses — check around line 296 (`doc.deref_mut()` for the multicol pass) and use the read-only equivalent.

**Step 4: Run unit test + a smoke compile**

```bash
cargo test -p fulgur --lib gcpm::target_ref::tests::collect_anchor_map_from_synthetic_inputs
cargo build -p fulgur
```

Expected: PASS, build OK.

**Step 5: Commit**

```bash
git add crates/fulgur/src/gcpm/target_ref.rs crates/fulgur/src/engine.rs crates/fulgur/src/pagination_layout.rs
git commit -m "feat(gcpm): build AnchorMap from pagination geometry + counter snapshots"
```

---

## Task 8: 2-pass orchestration in `Engine::render_html`

**Files:**

- Modify: `crates/fulgur/src/engine.rs`.

**Strategy:** Extract the body of `render_html` from line 51 onward into an internal `fn render_pass(&self, html: &str, anchor_map: Option<&AnchorMap>) -> Result<(Vec<u8>, AnchorMap)>` so we can call it twice. The returned `AnchorMap` is the one collected during this pass (always built; cheap when there are no fragment IDs). Then the public `render_html` becomes:

```rust
pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
    // Cheap GCPM probe — parse just enough to know whether any
    // target-* references exist anywhere across asset CSS, link CSS,
    // or inline <style> blocks.
    let needs_two_pass = self.contains_target_references(html);
    if !needs_two_pass {
        let (pdf, _) = self.render_pass(html, None)?;
        return Ok(pdf);
    }
    let (_, anchor_map) = self.render_pass(html, None)?;
    let (pdf, _) = self.render_pass(html, Some(&anchor_map))?;
    Ok(pdf)
}
```

The probe `contains_target_references`:

```rust
fn contains_target_references(&self, html: &str) -> bool {
    let combined = self.assets.as_ref().map(|a| a.combined_css()).unwrap_or_default();
    let asset_gcpm = crate::gcpm::parser::parse_gcpm(&combined);
    if asset_gcpm.has_target_references() { return true; }
    // Cheap textual probe to short-circuit before doing a full
    // link-resolution + inline-extraction round trip. Lower-case so
    // case variants in author CSS still match.
    let lower = html.to_ascii_lowercase();
    lower.contains("target-counter(")
        || lower.contains("target-counters(")
        || lower.contains("target-text(")
}
```

**Wiring inside `render_pass`:**

- After computing `gcpm` (line 61) and counter snapshots (line 216), broaden the existing `record_bookmark_snapshots` gate (`engine.rs:186-187`) so target-* presence also enables snapshot recording. Concretely, change:

  ```rust
  let record_bookmark_snapshots =
      self.config.effective_bookmarks() && !gcpm.bookmark_mappings.is_empty();
  ```

  to

  ```rust
  let record_node_snapshots =
      (self.config.effective_bookmarks() && !gcpm.bookmark_mappings.is_empty())
          || gcpm.has_target_references();
  ```

  and rename downstream uses. The OR with `has_target_references()` is non-negotiable — without it `counter_snapshots` is empty and `target-counter(attr(href), section)` returns "" for every named counter (page-counter still works because we fall back to `entry.page_num` in `resolve_target_counter`).
- After pagination_geometry is fully assembled (line ~393), call `let collected_map = build_anchor_map(&doc, &pagination_geometry, &counter_snapshots);`.
- When `anchor_map: Some(map)` was passed in:
  - Before invoking `apply_single_pass(&pass, ...)` for `CounterPass`, call `let pass = pass.with_anchor_map(map.clone());`. This requires bumping the constructor builder pattern in CounterPass (Task 6 already added `with_anchor_map`).
  - After pagination, when invoking `render::render_v2`, pass an additional `anchor_map: Option<&AnchorMap>` argument so margin-box rendering can resolve target references via the new `resolve_content_to_*_with_anchor` helpers.
- When `anchor_map: None`: CounterPass uses placeholders (Task 6 default), and `render_v2` is called with `None` for the anchor map. Margin-box content will emit empty for target references during pass 1 (acceptable — pass 1 PDF is discarded).

**Modify `render::render_v2` signature:**

- Add a trailing `anchor_map: Option<&AnchorMap>` parameter. Thread it into the per-page margin-box resolution call sites that currently invoke `resolve_content_to_html` / `resolve_content_to_string`. Use the new `_with_anchor` variants.

**Implicit-href for margin boxes (per design, Prince behavior):** at the per-page resolution site, scan `pagination_geometry` for the first `<a href="#...">` whose page is the current page, and pass its `href` as `implicit_href`. If absent, pass `None` → resolvers return empty string. Implementation: the existing render code already iterates through the page's drawables — extend the iteration to capture the first href.

**Step 1: Failing test (smoke)**

Add to `crates/fulgur/tests/render_smoke.rs`. We use `fulgur::inspect::inspect` (decoded text via lopdf with cmap awareness) as the **primary** assertion path — substring-on-PDF-bytes is unreliable once Krilla emits ToUnicode/cmap-encoded strings.

```rust
#[test]
fn target_counter_in_toc_renders_page_number() {
    use fulgur::{Engine, inspect::inspect};
    use tempfile::tempdir;

    let html = r#"
<!doctype html>
<html><head><style>
  body { font: 12pt sans-serif; }
  .toc a::after { content: " (p." target-counter(attr(href), page) ")"; }
  h2 { page-break-before: always; }
</style></head>
<body>
  <nav class="toc">
    <a href="#a">Chapter A</a><br>
    <a href="#b">Chapter B</a>
  </nav>
  <h2 id="a">Chapter A</h2>
  <p>aaa</p>
  <h2 id="b">Chapter B</h2>
  <p>bbb</p>
</body></html>"#;

    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("toc.pdf");
    std::fs::write(&path, &pdf).expect("write pdf");

    let inspected = inspect(&path).expect("inspect pdf");
    let all_text: String = inspected
        .text_items
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(all_text.contains("(p.2)"), "missing p.2 — got: {all_text}");
    assert!(all_text.contains("(p.3)"), "missing p.3 — got: {all_text}");
}
```

**Step 2: Run (FAIL — pass 1 only, no resolution)**

**Step 3: Implement orchestration as described above.**

**Step 4: Run all related tests**

```bash
cargo test -p fulgur --lib
cargo test -p fulgur --test render_smoke
cargo test -p fulgur --test gcpm_integration
```

Expected: all green.

**Step 5: Commit**

```bash
git add crates/fulgur/src/engine.rs crates/fulgur/src/render.rs crates/fulgur/tests/render_smoke.rs
git commit -m "feat(engine): orchestrate 2-pass render when target-* references exist"
```

---

## Task 9: VRT golden — TOC with leader and target-counter

**Files:**

- Create: `crates/fulgur-vrt/cases/gcpm/toc-target-counter.html` (or whatever directory the existing `gcpm` cases live in — discover with `ls crates/fulgur-vrt/cases/`).
- Create: `crates/fulgur-vrt/goldens/fulgur/gcpm/toc-target-counter.pdf` (generated).

**Step 1: Author the fixture**

Create the HTML test:

```html
<!doctype html>
<html><head><style>
  @page { size: A5; margin: 18mm; }
  body { font: 12pt 'Noto Sans', sans-serif; }
  .toc a { text-decoration: none; }
  .toc a::after {
    content: leader('.') target-counter(attr(href), page);
  }
  .toc li { list-style: none; }
  h2 { page-break-before: always; }
</style></head>
<body>
  <h1>Contents</h1>
  <ul class="toc">
    <li><a href="#intro">Introduction</a></li>
    <li><a href="#methods">Methods</a></li>
    <li><a href="#results">Results</a></li>
  </ul>
  <h2 id="intro">Introduction</h2>
  <p>Lorem ipsum.</p>
  <h2 id="methods">Methods</h2>
  <p>Dolor sit amet.</p>
  <h2 id="results">Results</h2>
  <p>Consectetur adipiscing.</p>
</body></html>
```

**Step 2: Generate golden**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 \
  cargo test -p fulgur-vrt -- toc_target_counter
```

Inspect the generated PDF visually (e.g. `pdftocairo -png ...`) to confirm the page numbers are correct and the leader fills the line.

**Step 3: Run normally**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt -- toc_target_counter
```

Expected: PASS (byte-equal to golden).

**Step 4: Commit**

```bash
git add crates/fulgur-vrt/
git commit -m "test(vrt): add TOC + target-counter golden"
```

---

## Task 10: WPT expectation flips and parser-edge unit tests

**Files:**

- Modify: `crates/fulgur-wpt/expectations/*.toml` (or whatever the project uses) — flip any `css/css-gcpm/target-counter-*.html` expectations from FAIL to PASS that now pass after the implementation.
- Tests: parser edge cases — confirm that:
  - `target-counter(  attr( href )  ,  page  )` (whitespace) parses.
  - Mixed-case `Target-Counter(...)` parses.
  - `target-counter(attr(href), section, lower-roman)` accepts but discards the style argument (per Q5 follow-up; until `fulgur-5ho3` lands the value is rendered decimal, but the item must NOT be silently dropped).

**Step 1: Run current WPT to find target-counter test names**

```bash
cargo test -p fulgur-wpt -- target_counter 2>&1 | tail -40
```

Inspect the output to see which fixtures are now passing vs still failing.

**Step 2: Flip newly-passing expectations**

Use the project's `wpt-promote` skill (`/wpt-promote <fixture-path>`) for each test that flipped from FAIL to PASS. If none flipped, document that and move on.

**Step 3: Add parser whitespace / case / discard-style tests**

Add to `gcpm/parser.rs::tests`:

```rust
#[test]
fn parse_target_counter_with_whitespace_and_case() {
    let css = r#".toc a::after { content: Target-Counter(  attr( href )  ,  page  ); }"#;
    let g = parse_gcpm(css);
    let mapping = &g.content_counter_mappings[0];
    assert_eq!(
        mapping.content,
        vec![ContentItem::TargetCounter {
            url_attr: "href".into(),
            counter_name: "page".into(),
            style: CounterStyle::Decimal,
        }]
    );
}

#[test]
fn parse_target_counter_accepts_style_arg_for_forward_compat() {
    let css = r#".toc a::after { content: target-counter(attr(href), section, lower-roman); }"#;
    let g = parse_gcpm(css);
    let mapping = &g.content_counter_mappings[0];
    // Style argument is parsed and stored even if downstream evaluator
    // ignores it for now; the item itself MUST NOT be dropped.
    let item = mapping.content.iter().find_map(|i| match i {
        ContentItem::TargetCounter { counter_name, style, .. } => {
            Some((counter_name.clone(), *style))
        }
        _ => None,
    });
    assert_eq!(
        item,
        Some(("section".into(), CounterStyle::LowerRoman))
    );
}
```

**Step 4: Run**

```bash
cargo test -p fulgur --lib parser::tests
cargo test -p fulgur-wpt 2>&1 | tail -20
```

Expected: green, including any flipped fixtures.

**Step 5: Commit**

```bash
git add .
git commit -m "test(gcpm): parser edge cases + WPT expectation flips for target-*"
```

---

## Task 11: Final verification + clippy + fmt + bd close

**Files:** none (verification only).

**Step 1: Full lib + integration test pass**

```bash
cargo test -p fulgur 2>&1 | tail -10
cargo test -p fulgur-vrt -- toc_target_counter 2>&1 | tail -5
```

Expected: 0 failures.

**Step 2: Lint**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -5
npx markdownlint-cli2 'docs/plans/2026-05-07-fulgur-63y-target-counter.md' 2>&1 | tail -5
```

Expected: clean.

**Step 3: Beads update**

Close `fulgur-63y` AND `fulgur-5ho3` together — Tasks 2/3 ship the parsing AND the formatter call (`format_counter(value, *style)`), so all five existing `CounterStyle` variants work end-to-end in `target-counter`:

```bash
bd close fulgur-63y fulgur-5ho3 --reason="GCPM target-counter / target-counters / target-text shipped via 2-pass render. counter-style 3rd arg (fulgur-5ho3) honored end-to-end through CounterStyle. Followups: fulgur-11gd (URL literals), fulgur-s4ec (non-href attrs), fulgur-x70n (target-text 2nd arg), fulgur-ejw9 (string-set/running), fulgur-38y2 (fixed-point iteration)."
```

Any *new* counter styles beyond the 5 base `CounterStyle` variants (`decimal-leading-zero`, `@counter-style` rules) are out of scope here — file a fresh issue rather than reopening `fulgur-5ho3`.

**Step 4: PR**

Use the project's normal PR creation flow (PR title in English, body in Japanese — see memory `feedback_pr_body_japanese.md`).

---

## Out-of-scope explicit list (filed as follow-up beads issues)

- **fulgur-5ho3** — *closed by this PR* (`target-counter(url, name, <counter-style>)` 3rd argument honored end-to-end for the 5 base `CounterStyle` variants). Any *new* counter styles (`decimal-leading-zero`, `@counter-style` rules) need a fresh issue.
- **fulgur-11gd** — `<url>` accepts string literal `"#sec1"` and `url("#sec1")`.
- **fulgur-s4ec** — `attr(<other-name>)` (e.g. `data-ref`) supported.
- **fulgur-x70n** — `target-text(url, before|after|first-letter)` 2nd argument.
- **fulgur-ejw9** — `target-*` evaluable inside `string-set` / `running()`.
- **fulgur-38y2** — Fixed-point iteration when value-length differences perturb pagination.
