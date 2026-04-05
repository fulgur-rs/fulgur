# GCPM string-set/string() Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS Named Strings (`string-set` property + `string()` function) support for dynamic page headers/footers that reflect document content (e.g. chapter titles).

**Architecture:** DOM walk detects `string-set` declarations and extracts text values. Convert stage inserts zero-size `StringSetPageable` markers into the Pageable tree. Paginate collects per-page string states (start/first/last). Render resolves `string(name, policy)` in margin boxes using these states.

**Tech Stack:** Rust, cssparser, Blitz DOM, existing GCPM infrastructure (parser.rs, counter.rs, running.rs, pageable.rs, paginate.rs, render.rs, engine.rs)

---

## Task 1: Data Types in gcpm/mod.rs

**Files:**

- Modify: `crates/fulgur/src/gcpm/mod.rs`

**Step 1: Add StringSet types and extend ContentItem**

Add the following types after the existing `RunningMapping` struct and extend `ContentItem`:

```rust
/// Policy for string() function — determines which value to use on a given page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringPolicy {
    /// Value at start of page (inherited from previous page's last value).
    Start,
    /// First value set on this page (falls back to start).
    First,
    /// Last value set on this page.
    Last,
    /// Same as First, but empty on pages where the string is actually set.
    FirstExcept,
}

/// A single value component in a string-set declaration.
/// `string-set: title content(text) " - " attr(data-subtitle)`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringSetValue {
    /// `content(text)` — element's text content.
    ContentText,
    /// `content(before)` — ::before pseudo-element content.
    ContentBefore,
    /// `content(after)` — ::after pseudo-element content.
    ContentAfter,
    /// `attr(<name>)` — HTML attribute value.
    Attr(String),
    /// A literal string, e.g. `"Chapter "`.
    Literal(String),
}

/// Maps a CSS selector to a string-set declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringSetMapping {
    /// The parsed CSS selector.
    pub parsed: ParsedSelector,
    /// The named string identifier.
    pub name: String,
    /// Value components to concatenate.
    pub values: Vec<StringSetValue>,
}
```

Add a new variant to `ContentItem`:

```rust
pub enum ContentItem {
    Element(String),
    Counter(CounterType),
    String(String),
    /// A named string reference, e.g. `string(chapter-title, first)`.
    StringRef { name: String, policy: StringPolicy },
}
```

**Step 2: Add string_set_mappings to GcpmContext**

```rust
pub struct GcpmContext {
    pub margin_boxes: Vec<MarginBoxRule>,
    pub running_mappings: Vec<RunningMapping>,
    pub string_set_mappings: Vec<StringSetMapping>,
    pub cleaned_css: String,
}
```

Update `is_empty()` to include `string_set_mappings`:

```rust
pub fn is_empty(&self) -> bool {
    self.margin_boxes.is_empty()
        && self.running_mappings.is_empty()
        && self.string_set_mappings.is_empty()
}
```

**Step 3: Update existing tests for new field**

Add `string_set_mappings: vec![]` to all existing `GcpmContext` construction sites in tests.

**Step 4: Run tests**

```bash
cargo test -p fulgur --lib gcpm::tests
```

Expected: All existing tests pass with the new field.

**Step 5: Commit**

```bash
git add crates/fulgur/src/gcpm/mod.rs
git commit -m "feat(gcpm): add StringSet data types and StringRef content item"
```

---

## Task 2: Parser — string-set property and string() function

**Files:**

- Modify: `crates/fulgur/src/gcpm/parser.rs`

**Step 1: Write tests for string-set parsing**

Add to `mod tests` in parser.rs:

```rust
#[test]
fn test_parse_string_set_content_text() {
    let css = "h1 { string-set: chapter-title content(text); }";
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.string_set_mappings.len(), 1);
    let m = &ctx.string_set_mappings[0];
    assert_eq!(m.parsed, ParsedSelector::Tag("h1".to_string()));
    assert_eq!(m.name, "chapter-title");
    assert_eq!(m.values, vec![StringSetValue::ContentText]);
    // string-set should be removed from cleaned_css
    assert!(!ctx.cleaned_css.contains("string-set"));
}

#[test]
fn test_parse_string_set_multiple_values() {
    let css = r#"h1 { string-set: title "Chapter " content(text) " - " attr(data-sub); }"#;
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.string_set_mappings.len(), 1);
    let m = &ctx.string_set_mappings[0];
    assert_eq!(m.name, "title");
    assert_eq!(
        m.values,
        vec![
            StringSetValue::Literal("Chapter ".to_string()),
            StringSetValue::ContentText,
            StringSetValue::Literal(" - ".to_string()),
            StringSetValue::Attr("data-sub".to_string()),
        ]
    );
}

#[test]
fn test_parse_string_set_content_before_after() {
    let css = "h2 { string-set: sec content(before) content(text) content(after); }";
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.string_set_mappings.len(), 1);
    let m = &ctx.string_set_mappings[0];
    assert_eq!(
        m.values,
        vec![
            StringSetValue::ContentBefore,
            StringSetValue::ContentText,
            StringSetValue::ContentAfter,
        ]
    );
}

#[test]
fn test_parse_string_function_default_policy() {
    let css = r#"@page { @top-center { content: string(chapter-title); } }"#;
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.margin_boxes.len(), 1);
    assert_eq!(
        ctx.margin_boxes[0].content,
        vec![ContentItem::StringRef {
            name: "chapter-title".to_string(),
            policy: StringPolicy::First,
        }]
    );
}

#[test]
fn test_parse_string_function_with_policy() {
    let css = r#"@page { @top-center { content: string(title, last); } }"#;
    let ctx = parse_gcpm(css);
    assert_eq!(
        ctx.margin_boxes[0].content,
        vec![ContentItem::StringRef {
            name: "title".to_string(),
            policy: StringPolicy::Last,
        }]
    );
}

#[test]
fn test_parse_string_function_all_policies() {
    for (policy_str, policy) in [
        ("first", StringPolicy::First),
        ("start", StringPolicy::Start),
        ("last", StringPolicy::Last),
        ("first-except", StringPolicy::FirstExcept),
    ] {
        let css = format!(
            r#"@page {{ @top-center {{ content: string(title, {}); }} }}"#,
            policy_str
        );
        let ctx = parse_gcpm(&css);
        assert_eq!(
            ctx.margin_boxes[0].content,
            vec![ContentItem::StringRef {
                name: "title".to_string(),
                policy,
            }],
            "Failed for policy: {}",
            policy_str,
        );
    }
}

#[test]
fn test_parse_string_set_with_class_selector() {
    let css = ".chapter-heading { string-set: chapter content(text); }";
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.string_set_mappings.len(), 1);
    assert_eq!(
        ctx.string_set_mappings[0].parsed,
        ParsedSelector::Class("chapter-heading".to_string())
    );
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur --lib gcpm::parser::tests
```

Expected: Compilation errors (new types not yet used in parser).

**Step 3: Implement string-set parsing in StyleRuleParser**

Modify `StyleRuleParser` to also detect `string-set` declarations alongside `position: running(...)`.

In the `StyleRuleParser` struct, add a field:

```rust
struct StyleRuleParser<'a> {
    edits: &'a mut Vec<CssEdit>,
    running_name: &'a mut Option<String>,
    string_set: &'a mut Option<(String, Vec<StringSetValue>)>,
}
```

In `StyleRuleParser::parse_value`, add handling for `string-set`:

```rust
if name.eq_ignore_ascii_case("string-set") {
    // Parse: <ident> <value>+
    let set_name = input.expect_ident()?.clone().to_string();
    let mut values = Vec::new();
    while !input.is_exhausted() {
        let result: Result<(), ParseError<'_, ()>> = input.try_parse(|input| {
            let token = input.next_including_whitespace()?.clone();
            match token {
                Token::QuotedString(ref s) => {
                    values.push(StringSetValue::Literal(s.to_string()));
                }
                Token::Function(ref fn_name) => {
                    let fn_name = fn_name.clone();
                    input.parse_nested_block(|input| {
                        if fn_name.eq_ignore_ascii_case("content") {
                            let arg = input.expect_ident()?.clone();
                            match &*arg {
                                "text" => values.push(StringSetValue::ContentText),
                                "before" => values.push(StringSetValue::ContentBefore),
                                "after" => values.push(StringSetValue::ContentAfter),
                                _ => {}
                            }
                        } else if fn_name.eq_ignore_ascii_case("attr") {
                            let arg = input.expect_ident()?.clone();
                            values.push(StringSetValue::Attr(arg.to_string()));
                        }
                        Ok(())
                    })?;
                }
                Token::WhiteSpace(_) | Token::Comment(_) => {}
                _ => {}
            }
            Ok(())
        });
        if result.is_err() {
            break;
        }
    }
    if !values.is_empty() {
        *self.string_set = Some((set_name, values));
        // Record edit to remove string-set declaration from cleaned_css
        let start_byte = decl_start.position().byte_index();
        let end_byte = input.position().byte_index();
        self.edits.push(CssEdit::Remove {
            start: start_byte,
            end: end_byte,
        });
    }
    return Ok(());
}
```

Update `GcpmSheetParser::parse_block` for qualified rules to collect string_set:

```rust
fn parse_block<'t>(
    &mut self,
    prelude: Self::Prelude,
    _start: &cssparser::ParserState,
    input: &mut Parser<'i, 't>,
) -> Result<Self::QualifiedRule, ParseError<'i, ()>> {
    let Some(selector) = prelude else {
        while input.next().is_ok() {}
        return Ok(TopLevelItem::StyleRule);
    };

    let mut running_name: Option<String> = None;
    let mut string_set: Option<(String, Vec<StringSetValue>)> = None;

    let mut parser = StyleRuleParser {
        edits: self.edits,
        running_name: &mut running_name,
        string_set: &mut string_set,
    };
    let iter = RuleBodyParser::new(input, &mut parser);
    for item in iter {
        let _ = item;
    }

    if let Some(running_name) = running_name {
        self.running_mappings.push(RunningMapping {
            parsed: selector.clone(),
            running_name,
        });
    }

    if let Some((name, values)) = string_set {
        self.string_set_mappings.push(StringSetMapping {
            parsed: selector,
            name,
            values,
        });
    }

    Ok(TopLevelItem::StyleRule)
}
```

Add `string_set_mappings` to `GcpmSheetParser` struct and `parse_gcpm` function.

**Step 4: Implement string() function in parse_content_value**

In `parse_content_value`, add handling for the `string()` function:

```rust
} else if fn_name.eq_ignore_ascii_case("string") {
    input.parse_nested_block(|input| {
        let name = input.expect_ident()?.clone().to_string();
        let policy = input
            .try_parse(|input| {
                input.expect_comma()?;
                let ident = input.expect_ident()?.clone();
                match &*ident {
                    "start" => Ok(StringPolicy::Start),
                    "first" => Ok(StringPolicy::First),
                    "last" => Ok(StringPolicy::Last),
                    "first-except" => Ok(StringPolicy::FirstExcept),
                    _ => Err(input.new_error::<()>(
                        BasicParseErrorKind::QualifiedRuleInvalid,
                    )),
                }
            })
            .unwrap_or(StringPolicy::First);
        items.push(ContentItem::StringRef { name, policy });
        Ok(())
    })?;
}
```

**Step 5: Run tests**

```bash
cargo test -p fulgur --lib gcpm::parser::tests
```

Expected: All tests pass.

**Step 6: Commit**

```bash
git add crates/fulgur/src/gcpm/parser.rs
git commit -m "feat(gcpm): parse string-set property and string() function"
```

---

## Task 3: StringSetPass — DOM walk for text extraction

**Files:**

- Create: `crates/fulgur/src/gcpm/string_set.rs`
- Modify: `crates/fulgur/src/gcpm/mod.rs` (add `pub mod string_set;`)
- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 1: Write tests for StringSetStore**

Create `crates/fulgur/src/gcpm/string_set.rs`:

```rust
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

/// Extract the text content of a DOM node (recursive).
pub fn extract_text_content(doc: &blitz_dom::BaseDocument, node_id: usize) -> String {
    let mut out = String::new();
    collect_text(doc, node_id, &mut out);
    out
}

fn collect_text(doc: &blitz_dom::BaseDocument, node_id: usize, out: &mut String) {
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    match &node.data {
        blitz_dom::NodeData::Text(text_data) => {
            out.push_str(&text_data.content);
        }
        _ => {
            for &child_id in &node.children {
                collect_text(doc, child_id, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_set_store_basic() {
        let mut store = StringSetStore::new();
        assert!(store.is_empty());

        store.push(StringSetEntry {
            name: "title".to_string(),
            value: "Chapter 1".to_string(),
            node_id: 42,
        });
        assert!(!store.is_empty());
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.entries()[0].name, "title");
        assert_eq!(store.entries()[0].value, "Chapter 1");
        assert_eq!(store.entries()[0].node_id, 42);
    }

    #[test]
    fn test_string_set_store_multiple_entries() {
        let mut store = StringSetStore::new();
        store.push(StringSetEntry {
            name: "title".to_string(),
            value: "Ch1".to_string(),
            node_id: 10,
        });
        store.push(StringSetEntry {
            name: "title".to_string(),
            value: "Ch2".to_string(),
            node_id: 20,
        });
        store.push(StringSetEntry {
            name: "section".to_string(),
            value: "Intro".to_string(),
            node_id: 30,
        });
        assert_eq!(store.entries().len(), 3);
    }
}
```

**Step 2: Add module to mod.rs**

In `crates/fulgur/src/gcpm/mod.rs`, add:

```rust
pub mod string_set;
```

**Step 3: Run tests**

```bash
cargo test -p fulgur --lib gcpm::string_set::tests
```

Expected: Pass.

**Step 4: Implement StringSetPass in blitz_adapter.rs**

Add after the `RunningElementPass` implementation:

```rust
use crate::gcpm::string_set::{extract_text_content, StringSetEntry, StringSetStore};
use crate::gcpm::{StringSetMapping, StringSetValue};

/// Extracts string-set values from the DOM.
pub struct StringSetPass {
    gcpm: GcpmContext,
    store: RefCell<StringSetStore>,
}

impl StringSetPass {
    pub fn new(gcpm: GcpmContext) -> Self {
        Self {
            gcpm,
            store: RefCell::new(StringSetStore::new()),
        }
    }

    pub fn into_store(self) -> StringSetStore {
        self.store.into_inner()
    }
}

impl DomPass for StringSetPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.gcpm.string_set_mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        let root_id = root.id;
        self.walk_tree(doc, root_id);
    }
}

impl StringSetPass {
    fn walk_tree(&self, doc: &HtmlDocument, node_id: usize) {
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if let Some(elem) = node.element_data() {
            if matches!(
                elem.name.local.as_ref(),
                "head" | "script" | "style" | "link" | "meta" | "title" | "noscript"
            ) {
                return;
            }
            if let Some((name, values)) = self.find_string_set(elem) {
                let value = self.resolve_values(doc, node_id, elem, &values);
                self.store.borrow_mut().push(StringSetEntry {
                    name,
                    value,
                    node_id,
                });
            }
        }

        // Always recurse (unlike RunningElementPass which stops at matched nodes)
        let children: Vec<usize> = doc
            .get_node(node_id)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        for child_id in children {
            self.walk_tree(doc, child_id);
        }
    }

    fn find_string_set(
        &self,
        elem: &blitz_dom::node::ElementData,
    ) -> Option<(String, Vec<StringSetValue>)> {
        self.gcpm
            .string_set_mappings
            .iter()
            .find(|m| self.matches_selector(&m.parsed, elem))
            .map(|m| (m.name.clone(), m.values.clone()))
    }

    fn matches_selector(
        &self,
        selector: &ParsedSelector,
        elem: &blitz_dom::node::ElementData,
    ) -> bool {
        match selector {
            ParsedSelector::Class(name) => get_attr(elem, "class")
                .is_some_and(|cls| cls.split_whitespace().any(|c| c == name.as_str())),
            ParsedSelector::Id(name) => get_attr(elem, "id") == Some(name.as_str()),
            ParsedSelector::Tag(name) => elem.name.local.as_ref().eq_ignore_ascii_case(name),
        }
    }

    fn resolve_values(
        &self,
        doc: &HtmlDocument,
        node_id: usize,
        elem: &blitz_dom::node::ElementData,
        values: &[StringSetValue],
    ) -> String {
        let mut out = String::new();
        for val in values {
            match val {
                StringSetValue::ContentText => {
                    out.push_str(&extract_text_content(doc, node_id));
                }
                StringSetValue::ContentBefore | StringSetValue::ContentAfter => {
                    // Pseudo-element content requires Stylo computed styles.
                    // For now, skip — these are rarely used with string-set.
                }
                StringSetValue::Attr(attr_name) => {
                    if let Some(v) = get_attr(elem, attr_name) {
                        out.push_str(v);
                    }
                }
                StringSetValue::Literal(s) => {
                    out.push_str(s);
                }
            }
        }
        out
    }
}
```

**Step 5: Run tests**

```bash
cargo test -p fulgur --lib
```

Expected: All tests pass.

**Step 6: Commit**

```bash
git add crates/fulgur/src/gcpm/string_set.rs crates/fulgur/src/gcpm/mod.rs crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(gcpm): add StringSetPass for DOM text extraction"
```

---

## Task 4: StringSetPageable — zero-size marker

**Files:**

- Modify: `crates/fulgur/src/pageable.rs`

**Step 1: Write tests**

Add to the test module in pageable.rs:

```rust
#[test]
fn test_string_set_pageable_zero_size() {
    let mut p = StringSetPageable::new("title".to_string(), "Chapter 1".to_string());
    let size = p.wrap(100.0, 100.0);
    assert_eq!(size.width, 0.0);
    assert_eq!(size.height, 0.0);
    assert_eq!(p.height(), 0.0);
}

#[test]
fn test_string_set_pageable_no_split() {
    let p = StringSetPageable::new("title".to_string(), "Chapter 1".to_string());
    assert!(p.split(100.0, 100.0).is_none());
}

#[test]
fn test_string_set_pageable_fields() {
    let p = StringSetPageable::new("title".to_string(), "Chapter 1".to_string());
    assert_eq!(p.name, "title");
    assert_eq!(p.value, "Chapter 1");
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur --lib pageable::tests
```

Expected: Compilation error — `StringSetPageable` not defined.

**Step 3: Implement StringSetPageable**

Add after `SpacerPageable` in pageable.rs:

```rust
// ─── StringSetPageable ──────────────────────────────────────

/// Zero-size marker for named string values.
/// Inserted into the Pageable tree to track string-set positions during pagination.
#[derive(Clone)]
pub struct StringSetPageable {
    pub name: String,
    pub value: String,
}

impl StringSetPageable {
    pub fn new(name: String, value: String) -> Self {
        Self { name, value }
    }
}

impl Pageable for StringSetPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        Size {
            width: 0.0,
            height: 0.0,
        }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        None
    }

    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {
        // Markers are invisible
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        0.0
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
```

**Step 4: Run tests**

```bash
cargo test -p fulgur --lib pageable::tests
```

Expected: All tests pass.

**Step 5: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(gcpm): add StringSetPageable zero-size marker"
```

---

## Task 5: Convert — insert StringSetPageable markers

**Files:**

- Modify: `crates/fulgur/src/convert.rs`

**Step 1: Add StringSetStore to ConvertContext**

```rust
pub struct ConvertContext<'a> {
    pub running_store: &'a mut RunningElementStore,
    pub assets: Option<&'a AssetBundle>,
    pub(crate) font_cache: HashMap<(usize, u32), Arc<Vec<u8>>>,
    /// String-set entries from DOM walk, keyed by node_id for O(1) lookup.
    pub string_set_by_node: HashMap<usize, Vec<(String, String)>>,
}
```

**Step 2: Insert markers in convert_node**

At the top of `convert_node`, after getting the node, check for string-set entries for this node_id. If found, wrap the converted Pageable in a BlockPageable with a `StringSetPageable` inserted before the actual content:

Find the location where children are built for a `BlockPageable` and insert `StringSetPageable` markers. The simplest approach: after `convert_node` produces the child Pageable, check if the node has string-set entries and prepend markers.

Add a helper at the end of `convert_node` (before return):

```rust
fn maybe_prepend_string_set(
    node_id: usize,
    child: Box<dyn Pageable>,
    ctx: &mut ConvertContext<'_>,
) -> Box<dyn Pageable> {
    let entries = ctx.string_set_by_node.remove(&node_id);
    match entries {
        Some(entries) if !entries.is_empty() => {
            let mut children = Vec::with_capacity(entries.len() + 1);
            for (name, value) in entries {
                let marker = StringSetPageable::new(name, value);
                children.push(PositionedChild {
                    child: Box::new(marker),
                    x: 0.0,
                    y: 0.0,
                });
            }
            children.push(PositionedChild {
                child,
                x: 0.0,
                y: 0.0,
            });
            Box::new(BlockPageable::new(children))
        }
        _ => child,
    }
}
```

Call this at the end of `convert_node`:

```rust
let result = /* existing conversion logic */;
maybe_prepend_string_set(node_id, result, ctx)
```

**Step 3: Run tests**

```bash
cargo test -p fulgur --lib
```

Expected: All tests pass. No behavior change when `string_set_by_node` is empty.

**Step 4: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(gcpm): insert StringSetPageable markers during convert"
```

---

## Task 6: Paginate — collect per-page string states

**Files:**

- Modify: `crates/fulgur/src/paginate.rs`

**Step 1: Add StringSetPageState type**

```rust
use std::collections::BTreeMap;
use crate::pageable::StringSetPageable;

/// Per-page state for a named string.
#[derive(Debug, Clone, Default)]
pub struct StringSetPageState {
    /// Value at start of page (carried from previous page's `last`).
    pub start: Option<String>,
    /// First value set on this page.
    pub first: Option<String>,
    /// Last value set on this page.
    pub last: Option<String>,
}
```

**Step 2: Write tests**

```rust
#[cfg(test)]
mod string_set_tests {
    use super::*;
    use crate::pageable::{BlockPageable, SpacerPageable, StringSetPageable, PositionedChild};

    fn make_spacer(h: Pt) -> Box<dyn Pageable> {
        let mut s = SpacerPageable::new(h);
        s.wrap(100.0, 1000.0);
        Box::new(s)
    }

    fn make_marker(name: &str, value: &str) -> Box<dyn Pageable> {
        Box::new(StringSetPageable::new(name.to_string(), value.to_string()))
    }

    #[test]
    fn test_collect_string_sets_single_page() {
        // marker("title", "Ch1") + spacer(50)
        let block = BlockPageable::new(vec![
            PositionedChild { child: make_marker("title", "Ch1"), x: 0.0, y: 0.0 },
            PositionedChild { child: make_spacer(50.0), x: 0.0, y: 0.0 },
        ]);
        let pages = paginate(Box::new(block), 100.0, 200.0);
        let states = collect_string_set_states(&pages);
        assert_eq!(states.len(), 1);
        let page_state = &states[0]["title"];
        assert_eq!(page_state.start, None);
        assert_eq!(page_state.first, Some("Ch1".to_string()));
        assert_eq!(page_state.last, Some("Ch1".to_string()));
    }

    #[test]
    fn test_collect_string_sets_across_pages() {
        // Page 1: marker("title", "Ch1") + spacer(150)
        // Page 2: marker("title", "Ch2") + spacer(50)
        let block = BlockPageable::new(vec![
            PositionedChild { child: make_marker("title", "Ch1"), x: 0.0, y: 0.0 },
            PositionedChild { child: make_spacer(150.0), x: 0.0, y: 0.0 },
            PositionedChild { child: make_marker("title", "Ch2"), x: 0.0, y: 0.0 },
            PositionedChild { child: make_spacer(50.0), x: 0.0, y: 0.0 },
        ]);
        let pages = paginate(Box::new(block), 100.0, 100.0);
        let states = collect_string_set_states(&pages);
        // Page 2 should have start = "Ch1" (from page 1's last)
        assert!(states.len() >= 2);
        let p2 = &states[1]["title"];
        assert_eq!(p2.start, Some("Ch1".to_string()));
    }

    #[test]
    fn test_collect_string_sets_no_markers() {
        let block = BlockPageable::new(vec![
            PositionedChild { child: make_spacer(50.0), x: 0.0, y: 0.0 },
        ]);
        let pages = paginate(Box::new(block), 100.0, 200.0);
        let states = collect_string_set_states(&pages);
        assert_eq!(states.len(), 1);
        assert!(states[0].is_empty());
    }
}
```

**Step 3: Implement collect_string_set_states**

```rust
/// Walk a paginated Pageable tree and collect StringSetPageable markers per page.
pub fn collect_string_set_states(
    pages: &[Box<dyn Pageable>],
) -> Vec<BTreeMap<String, StringSetPageState>> {
    let mut result: Vec<BTreeMap<String, StringSetPageState>> = Vec::with_capacity(pages.len());
    // Track the last known value for each name across pages (for `start` field)
    let mut carry: BTreeMap<String, String> = BTreeMap::new();

    for page in pages {
        let mut page_state: BTreeMap<String, StringSetPageState> = BTreeMap::new();

        // Initialize start values from carry
        for (name, value) in &carry {
            page_state
                .entry(name.clone())
                .or_default()
                .start = Some(value.clone());
        }

        // Collect markers from this page
        let mut markers = Vec::new();
        collect_markers(page.as_ref(), &mut markers);

        for (name, value) in &markers {
            let state = page_state.entry(name.clone()).or_default();
            if state.first.is_none() {
                state.first = Some(value.clone());
            }
            state.last = Some(value.clone());
            // Update carry for next page
            carry.insert(name.clone(), value.clone());
        }

        result.push(page_state);
    }

    result
}

/// Recursively find all StringSetPageable markers in a Pageable tree.
fn collect_markers(pageable: &dyn Pageable, markers: &mut Vec<(String, String)>) {
    if let Some(marker) = pageable.as_any().downcast_ref::<StringSetPageable>() {
        markers.push((marker.name.clone(), marker.value.clone()));
        return;
    }
    if let Some(block) = pageable.as_any().downcast_ref::<BlockPageable>() {
        for child in &block.children {
            collect_markers(child.child.as_ref(), markers);
        }
    }
    // Also check TablePageable, ListItemPageable if needed
    if let Some(table) = pageable.as_any().downcast_ref::<TablePageable>() {
        for child in &table.header_cells {
            collect_markers(child.child.as_ref(), markers);
        }
        for child in &table.body_cells {
            collect_markers(child.child.as_ref(), markers);
        }
    }
    if let Some(list_item) = pageable.as_any().downcast_ref::<ListItemPageable>() {
        collect_markers(list_item.body.as_ref(), markers);
    }
}
```

**Step 4: Run tests**

```bash
cargo test -p fulgur --lib paginate
```

Expected: All tests pass.

**Step 5: Commit**

```bash
git add crates/fulgur/src/paginate.rs
git commit -m "feat(gcpm): collect per-page string-set states during pagination"
```

---

## Task 7: Counter resolution — resolve string() references

**Files:**

- Modify: `crates/fulgur/src/gcpm/counter.rs`

**Step 1: Write tests**

Add to `mod tests` in counter.rs:

```rust
#[test]
fn test_resolve_string_ref_first() {
    let items = vec![ContentItem::StringRef {
        name: "title".to_string(),
        policy: StringPolicy::First,
    }];
    let state = {
        let mut m = BTreeMap::new();
        m.insert("title".to_string(), StringSetPageState {
            start: Some("Previous".to_string()),
            first: Some("Current".to_string()),
            last: Some("Current".to_string()),
        });
        m
    };
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
    let state = {
        let mut m = BTreeMap::new();
        m.insert("title".to_string(), StringSetPageState {
            start: Some("Inherited".to_string()),
            first: None,
            last: None,
        });
        m
    };
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
    let state = {
        let mut m = BTreeMap::new();
        m.insert("title".to_string(), StringSetPageState {
            start: Some("Start Value".to_string()),
            first: Some("First Value".to_string()),
            last: Some("Last Value".to_string()),
        });
        m
    };
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
    let state = {
        let mut m = BTreeMap::new();
        m.insert("title".to_string(), StringSetPageState {
            start: None,
            first: Some("First".to_string()),
            last: Some("Last".to_string()),
        });
        m
    };
    assert_eq!(
        resolve_content_to_html(&items, &[], &state, 1, 1),
        "Last"
    );
}

#[test]
fn test_resolve_string_ref_first_except_on_set_page() {
    let items = vec![ContentItem::StringRef {
        name: "title".to_string(),
        policy: StringPolicy::FirstExcept,
    }];
    let state = {
        let mut m = BTreeMap::new();
        m.insert("title".to_string(), StringSetPageState {
            start: Some("Old".to_string()),
            first: Some("New".to_string()),
            last: Some("New".to_string()),
        });
        m
    };
    // first-except: empty on pages where string is set
    assert_eq!(
        resolve_content_to_html(&items, &[], &state, 1, 1),
        ""
    );
}

#[test]
fn test_resolve_string_ref_first_except_on_no_set_page() {
    let items = vec![ContentItem::StringRef {
        name: "title".to_string(),
        policy: StringPolicy::FirstExcept,
    }];
    let state = {
        let mut m = BTreeMap::new();
        m.insert("title".to_string(), StringSetPageState {
            start: Some("Inherited".to_string()),
            first: None,
            last: None,
        });
        m
    };
    // first-except: same as first when not set on this page
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
    let state = BTreeMap::new();
    assert_eq!(
        resolve_content_to_html(&items, &[], &state, 1, 1),
        ""
    );
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur --lib gcpm::counter::tests
```

Expected: Compilation error.

**Step 3: Update resolve functions**

Add the `string_set_states` parameter to `resolve_content_to_html` and `resolve_content_to_string`:

```rust
use super::{ContentItem, CounterType, StringPolicy};
use crate::paginate::StringSetPageState;
use std::collections::BTreeMap;

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
            ContentItem::Counter(CounterType::Page) => out.push_str(&page.to_string()),
            ContentItem::Counter(CounterType::Pages) => out.push_str(&total_pages.to_string()),
            ContentItem::Element(_) => {}
            ContentItem::StringRef { name, policy } => {
                if let Some(state) = string_set_states.get(name) {
                    out.push_str(&resolve_string_policy(state, *policy));
                }
            }
        }
    }
    out
}

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
            ContentItem::Counter(CounterType::Page) => out.push_str(&page.to_string()),
            ContentItem::Counter(CounterType::Pages) => out.push_str(&total_pages.to_string()),
            ContentItem::Element(name) => {
                if let Some((_, html)) = running_elements.iter().find(|(n, _)| n == name) {
                    out.push_str(html);
                }
            }
            ContentItem::StringRef { name, policy } => {
                if let Some(state) = string_set_states.get(name) {
                    out.push_str(&resolve_string_policy(state, *policy));
                }
            }
        }
    }
    out
}

fn resolve_string_policy(state: &StringSetPageState, policy: StringPolicy) -> String {
    match policy {
        StringPolicy::Start => state.start.clone().unwrap_or_default(),
        StringPolicy::First => state
            .first
            .clone()
            .or_else(|| state.start.clone())
            .unwrap_or_default(),
        StringPolicy::Last => state
            .last
            .clone()
            .or_else(|| state.first.clone())
            .or_else(|| state.start.clone())
            .unwrap_or_default(),
        StringPolicy::FirstExcept => {
            if state.first.is_some() {
                // On pages where string is set, return empty
                String::new()
            } else {
                // Same as First when not set on this page
                state.start.clone().unwrap_or_default()
            }
        }
    }
}
```

**Step 4: Update existing tests to pass new parameter**

Add `&BTreeMap::new()` to all existing `resolve_content_to_string` and `resolve_content_to_html` calls in the existing tests.

**Step 5: Run tests**

```bash
cargo test -p fulgur --lib gcpm::counter::tests
```

Expected: All tests pass.

**Step 6: Commit**

```bash
git add crates/fulgur/src/gcpm/counter.rs
git commit -m "feat(gcpm): resolve string() references in content"
```

---

## Task 8: Render — wire string-set states into margin box rendering

**Files:**

- Modify: `crates/fulgur/src/render.rs`

**Step 1: Update render_to_pdf_with_gcpm**

After pagination in Pass 1, collect string-set states:

```rust
let pages = paginate(root, content_width, content_height);
let total_pages = pages.len();
let string_set_states = crate::paginate::collect_string_set_states(&pages);
```

In Pass 2, pass the per-page state to `resolve_content_to_html`:

```rust
let page_string_state = &string_set_states[page_idx];
let content_html = resolve_content_to_html(
    &rule.content,
    &running_pairs,
    page_string_state,
    page_num,
    total_pages,
);
```

**Step 2: Fix compilation errors**

Update all call sites of `resolve_content_to_string` and `resolve_content_to_html` in render.rs to include the new parameter.

**Step 3: Run tests**

```bash
cargo test -p fulgur --lib
```

Expected: All tests pass.

**Step 4: Commit**

```bash
git add crates/fulgur/src/render.rs
git commit -m "feat(gcpm): wire string-set states into margin box rendering"
```

---

## Task 9: Engine — wire StringSetPass into pipeline

**Files:**

- Modify: `crates/fulgur/src/engine.rs`

**Step 1: Add StringSetPass to render_html pipeline**

After `RunningElementPass`, add `StringSetPass`:

```rust
use crate::blitz_adapter::StringSetPass;
use crate::gcpm::string_set::StringSetStore;

// Extract string-set values via DomPass (before resolve, after running elements)
let string_set_store = if !gcpm.string_set_mappings.is_empty() {
    let pass = StringSetPass::new(gcpm.clone());
    pass.apply(&mut doc, &ctx);
    pass.into_store()
} else {
    StringSetStore::new()
};
```

Pass string_set_store to ConvertContext:

```rust
let mut convert_ctx = ConvertContext {
    running_store: &mut running_store,
    assets: self.assets.as_ref(),
    font_cache: HashMap::new(),
    string_set_by_node: build_string_set_by_node(&string_set_store),
};
```

Add helper:

```rust
fn build_string_set_by_node(
    store: &StringSetStore,
) -> HashMap<usize, Vec<(String, String)>> {
    let mut map: HashMap<usize, Vec<(String, String)>> = HashMap::new();
    for entry in store.entries() {
        map.entry(entry.node_id)
            .or_default()
            .push((entry.name.clone(), entry.value.clone()));
    }
    map
}
```

**Step 2: Update GcpmContext.is_empty() usage**

The condition `if gcpm.is_empty()` now also checks `string_set_mappings`, which is correct — if there are string-set mappings but no margin boxes, we still need the 2-pass rendering.

Actually, review: string-set mappings alone without margin boxes using `string()` would be a no-op. The `is_empty()` check is fine as-is since string-set mappings only make sense when margin boxes exist.

**Step 3: Run tests**

```bash
cargo test -p fulgur --lib
```

Expected: All tests pass.

**Step 4: Commit**

```bash
git add crates/fulgur/src/engine.rs
git commit -m "feat(gcpm): wire StringSetPass into engine pipeline"
```

---

## Task 10: Integration test

**Files:**

- Modify: `crates/fulgur/tests/gcpm_integration.rs`

**Step 1: Add integration test**

```rust
#[test]
fn test_gcpm_string_set_chapter_title() {
    let css = r#"
        h1 { string-set: chapter-title content(text); }
        @page {
            @top-center { content: string(chapter-title); }
            @bottom-center { content: "Page " counter(page) " of " counter(pages); }
        }
    "#;

    let mut paragraphs = String::new();
    for i in 0..3 {
        paragraphs.push_str(&format!(
            "<h1>Chapter {}</h1>\n<p>Content for chapter {}. This paragraph has enough text to take some space on the page.</p>\n",
            i + 1, i + 1
        ));
        for j in 0..20 {
            paragraphs.push_str(&format!(
                "<p>Paragraph {} of chapter {}.</p>\n",
                j + 1, i + 1
            ));
        }
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head></head>
<body>
{}
</body>
</html>"#,
        paragraphs
    );

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(&html).unwrap();

    assert!(!pdf.is_empty(), "PDF output should not be empty");
    assert!(
        pdf.starts_with(b"%PDF-"),
        "PDF output should start with %PDF-"
    );
}

#[test]
fn test_gcpm_string_set_with_attr() {
    let css = r#"
        h1 { string-set: title attr(data-title); }
        @page {
            @top-left { content: string(title); }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <h1 data-title="Custom Title">Visible Heading</h1>
  <p>Some body content.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(html).unwrap();

    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_gcpm_string_set_with_literal_concat() {
    let css = r#"
        h1 { string-set: header "Section: " content(text); }
        @page {
            @top-center { content: string(header); }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <h1>Introduction</h1>
  <p>Body text.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(html).unwrap();

    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_gcpm_string_set_with_policies() {
    let css = r#"
        h2 { string-set: section content(text); }
        @page {
            @top-left { content: string(section, start); }
            @top-right { content: string(section, last); }
        }
    "#;

    let mut body = String::new();
    for i in 0..30 {
        body.push_str(&format!("<h2>Section {}</h2>\n<p>Content.</p>\n", i + 1));
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html><head></head><body>{}</body></html>"#,
        body
    );

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(&html).unwrap();

    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}
```

**Step 2: Run integration tests**

```bash
cargo test -p fulgur --test gcpm_integration -- --test-threads=1
```

Expected: All tests pass (including existing ones).

**Step 3: Commit**

```bash
git add crates/fulgur/tests/gcpm_integration.rs
git commit -m "test(gcpm): add integration tests for string-set/string()"
```

---

## Task 11: Final verification

**Step 1: Run full test suite**

```bash
cargo test -p fulgur --lib
cargo test -p fulgur --test gcpm_integration -- --test-threads=1
```

**Step 2: Run clippy and fmt**

```bash
cargo clippy
cargo fmt --check
```

**Step 3: Fix any issues found**

**Step 4: Final commit if needed**

```bash
git add -A
git commit -m "chore: fix clippy warnings and formatting"
```
