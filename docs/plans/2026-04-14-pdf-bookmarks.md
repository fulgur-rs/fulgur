# PDF Bookmarks (Outline) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Auto-generate PDF bookmarks (outline tree) from `h1`-`h6` heading elements, opt-in via `--bookmarks` CLI flag / `.bookmarks(true)` engine builder.

**Architecture:** Insert zero-sized `HeadingMarkerPageable` into the pageable tree for each heading during DOM conversion. A `HeadingMarkerWrapperPageable` keeps the marker attached to the first fragment on split. During page rendering, inject a shared `HeadingCollector` into `Canvas`; the marker's `draw()` records `(page_idx, y_pt, level, text)`. After all pages are drawn, build a `krilla::Outline` from the collected entries using a stack-based algorithm, then call `document.set_outline()`.

**Tech Stack:** Rust, Krilla (`krilla::interchange::outline::Outline`, `OutlineNode`, `XyzDestination`), existing fulgur Pageable trait system.

---

## Context

### Key files

- `crates/fulgur/src/pageable.rs` — Pageable trait, `Canvas`, `StringSetPageable` (template to follow)
- `crates/fulgur/src/convert.rs` — DOM → Pageable conversion; `convert_node_inner` handles elements
- `crates/fulgur/src/render.rs` — `render_to_pdf` + `render_to_pdf_with_gcpm`, draws pages
- `crates/fulgur/src/config.rs` — Config struct
- `crates/fulgur/src/engine.rs` — `Engine`, `EngineBuilder`
- `crates/fulgur-cli/src/main.rs` — CLI flags

### Krilla API (from `krilla-0.6.0/src/interchange/outline.rs`)

```rust
use krilla::interchange::outline::{Outline, OutlineNode};
use krilla::interactive::destination::XyzDestination;
use krilla::geom::Point;

let mut outline = Outline::new();
let mut node = OutlineNode::new("Chapter 1".to_string(),
    XyzDestination::new(0 /* page index */, Point::from_xy(0.0, 100.0)));
node.push_child(OutlineNode::new("Section 1.1".to_string(),
    XyzDestination::new(1, Point::from_xy(0.0, 50.0))));
outline.push_child(node);
document.set_outline(outline); // must be before document.finish()
```

### Existing marker template: `StringSetPageable`

`pageable.rs:1411-1456` — zero-sized, Clone, `draw()` is a no-op. Wrapped by `StringSetWrapperPageable` (`pageable.rs:1739`) which keeps markers attached to the first fragment on `split()`. `paginate.rs:90` walks the tree to collect them.

Follow the same pattern for headings, but because we need the **y-coordinate on the rendered page** (not just which page), the collection happens in `draw()` via an optional `Canvas.heading_collector`, not via a pre-render tree walk.

### Position convention

Krilla `XyzDestination::new(page_index, Point)` — page index is 0-based. `Point` is in Krilla's (top-left origin, y grows down) coord system; `XyzDestination::serialize` inverts y internally. fulgur's `Canvas` already uses the same top-left y-down convention (`render.rs:40` passes `config.margin.top` as the y origin), so we can pass the marker's draw-time `y` directly.

---

## Task 1: Add `HeadingMarkerPageable` and `HeadingCollector`

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (add near `StringSetPageable` at line 1411; extend `Canvas` at line 183)

**Step 1: Write the failing test**

Append to `pageable.rs` (under `#[cfg(test)] mod tests`):

```rust
#[test]
fn heading_marker_is_zero_sized_and_draws_nothing() {
    let m = HeadingMarkerPageable::new(1, "Chapter 1".to_string());
    let size = {
        let mut c = m.clone();
        c.wrap(100.0, 100.0)
    };
    assert_eq!(size.width, 0.0);
    assert_eq!(size.height, 0.0);
    assert_eq!(m.height(), 0.0);
    assert_eq!(m.level, 1);
    assert_eq!(m.text, "Chapter 1");
}

#[test]
fn heading_collector_records_entry_on_draw() {
    use crate::pageable::HeadingCollector;
    let mut collector = HeadingCollector::new();
    collector.set_current_page(2);

    let marker = HeadingMarkerPageable::new(2, "Section".to_string());

    // Build a krilla surface stand-in. Since we can't easily construct a real
    // Surface in unit tests, only verify the collector path: the marker
    // records to the collector via a helper, not via Canvas plumbing directly.
    //
    // Therefore: expose a `HeadingMarkerPageable::record_if_collecting(y, collector)`
    // helper that the test calls directly.
    marker.record_if_collecting(42.0, Some(&mut collector));

    let entries = collector.into_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].page_idx, 2);
    assert_eq!(entries[0].y_pt, 42.0);
    assert_eq!(entries[0].level, 2);
    assert_eq!(entries[0].text, "Section");
}
```

**Step 2: Run and verify failure**

```bash
cargo test -p fulgur --lib heading_marker
```

Expected: FAIL with "cannot find struct `HeadingMarkerPageable`" / "HeadingCollector".

**Step 3: Implement**

Add to `pageable.rs` (place near `StringSetPageable`, around line 1410):

```rust
// ─── HeadingMarkerPageable ──────────────────────────────

/// One record captured by `HeadingCollector` during draw.
#[derive(Debug, Clone)]
pub struct HeadingEntry {
    pub page_idx: usize,
    pub y_pt: Pt,
    pub level: u8,
    pub text: String,
}

/// Shared, mutable collector threaded through `Canvas` during page
/// rendering. `render.rs` sets `current_page_idx` before drawing each page;
/// `HeadingMarkerPageable::draw` pushes an entry for each marker it sees.
#[derive(Debug, Default)]
pub struct HeadingCollector {
    current_page_idx: usize,
    entries: Vec<HeadingEntry>,
}

impl HeadingCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_current_page(&mut self, idx: usize) {
        self.current_page_idx = idx;
    }

    pub fn record(&mut self, level: u8, text: String, y_pt: Pt) {
        self.entries.push(HeadingEntry {
            page_idx: self.current_page_idx,
            y_pt,
            level,
            text,
        });
    }

    pub fn into_entries(self) -> Vec<HeadingEntry> {
        self.entries
    }
}

/// Zero-size marker for a heading element, for PDF outline generation.
/// Attached to the heading's block so the marker travels with the first
/// fragment on page splits (see `HeadingMarkerWrapperPageable`).
#[derive(Clone)]
pub struct HeadingMarkerPageable {
    pub level: u8,
    pub text: String,
}

impl HeadingMarkerPageable {
    pub fn new(level: u8, text: String) -> Self {
        Self { level, text }
    }

    /// Helper used by both `draw` and unit tests — records into the collector
    /// if one is present.
    pub fn record_if_collecting(&self, y: Pt, collector: Option<&mut HeadingCollector>) {
        if let Some(c) = collector {
            c.record(self.level, self.text.clone(), y);
        }
    }
}

impl Pageable for HeadingMarkerPageable {
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

    fn draw(&self, canvas: &mut Canvas<'_, '_>, _x: Pt, y: Pt, _aw: Pt, _ah: Pt) {
        self.record_if_collecting(y, canvas.heading_collector.as_deref_mut());
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

Extend `Canvas`:

```rust
pub struct Canvas<'a, 'b> {
    pub surface: &'a mut krilla::surface::Surface<'b>,
    pub heading_collector: Option<&'a mut HeadingCollector>,
}
```

**Step 4: Fix Canvas construction sites**

`cargo build` will flag every place `Canvas { surface: ... }` is constructed. Fix each with `heading_collector: None` (let render.rs opt in later). Expected sites (grep first):

```bash
grep -rn "Canvas {" crates/fulgur/src/
```

Expected matches: `render.rs` (2 sites, non-GCPM and GCPM paths), possibly `pageable.rs` tests.

Replace each `Canvas { surface: ... }` with `Canvas { surface: ..., heading_collector: None }`.

**Step 5: Run test to verify pass**

```bash
cargo test -p fulgur --lib heading_marker
cargo build
```

Expected: tests PASS, build succeeds.

**Step 6: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(bookmarks): add HeadingMarkerPageable and HeadingCollector"
```

---

## Task 2: Add `HeadingMarkerWrapperPageable`

Wraps a heading's block with its marker so the marker stays with the first fragment after split (matches `StringSetWrapperPageable`).

**Files:**

- Modify: `crates/fulgur/src/pageable.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn heading_wrapper_keeps_marker_with_first_fragment() {
    // Build a tall child so it definitely splits.
    let child: Box<dyn Pageable> = Box::new(SpacerPageable::new(1000.0));
    let marker = HeadingMarkerPageable::new(1, "Title".into());
    let wrapper = HeadingMarkerWrapperPageable::new(marker, child);

    // Split at 500pt.
    let split = wrapper.split(500.0, 500.0);
    let (first, _second) = split.expect("tall child must split");

    // First must contain the HeadingMarkerPageable.
    let any = first.as_any();
    let w = any
        .downcast_ref::<HeadingMarkerWrapperPageable>()
        .expect("first fragment wraps marker");
    assert_eq!(w.marker.text, "Title");
}
```

**Step 2: Verify failure**

```bash
cargo test -p fulgur --lib heading_wrapper
```

Expected: FAIL (struct missing).

**Step 3: Implement**

Append after `HeadingMarkerPageable`:

```rust
/// Wraps a Pageable with a `HeadingMarkerPageable`, keeping the marker
/// attached to the first fragment on `split()` so outline anchors land on
/// the page where the heading visually starts.
#[derive(Clone)]
pub struct HeadingMarkerWrapperPageable {
    pub marker: HeadingMarkerPageable,
    pub child: Box<dyn Pageable>,
}

impl HeadingMarkerWrapperPageable {
    pub fn new(marker: HeadingMarkerPageable, child: Box<dyn Pageable>) -> Self {
        Self { marker, child }
    }
}

impl Pageable for HeadingMarkerWrapperPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        self.child.wrap(avail_width, avail_height)
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (first, second) = self.child.split(avail_width, avail_height)?;
        let first_wrapped = HeadingMarkerWrapperPageable {
            marker: self.marker.clone(),
            child: first,
        };
        // Second fragment does NOT carry the marker — the heading started on
        // the previous page.
        Some((Box::new(first_wrapped), second))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, aw: Pt, ah: Pt) {
        // Record the marker's y before drawing the child.
        self.marker.draw(canvas, x, y, aw, ah);
        self.child.draw(canvas, x, y, aw, ah);
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.child.height()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
```

**Step 4: Run tests**

```bash
cargo test -p fulgur --lib heading_wrapper
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(bookmarks): add HeadingMarkerWrapperPageable"
```

---

## Task 3: Wrap headings in `convert.rs`

Detect `h1`-`h6` elements in `convert_node_inner`, wrap the resulting Pageable with `HeadingMarkerWrapperPageable` using the element's text content and level.

**Files:**

- Modify: `crates/fulgur/src/convert.rs`

**Step 1: Write the failing test**

In `convert.rs` tests module:

```rust
#[test]
fn h1_wraps_block_with_heading_marker() {
    use crate::pageable::HeadingMarkerWrapperPageable;

    let html = r#"<html><body><h1>Chapter One</h1></body></html>"#;
    let doc = crate::blitz_adapter::parse_and_layout(html, 500.0, 500.0, &[]);
    let running_store = crate::gcpm::running::RunningElementStore::new();
    let mut ctx = ConvertContext {
        running_store: &running_store,
        assets: None,
        font_cache: HashMap::new(),
        string_set_by_node: HashMap::new(),
        counter_ops_by_node: HashMap::new(),
    };
    let root = dom_to_pageable(&doc, &mut ctx);

    // Walk the tree; exactly one HeadingMarkerWrapperPageable, level 1, text "Chapter One".
    fn collect(p: &dyn crate::pageable::Pageable, out: &mut Vec<(u8, String)>) {
        let any = p.as_any();
        if let Some(w) = any.downcast_ref::<HeadingMarkerWrapperPageable>() {
            out.push((w.marker.level, w.marker.text.clone()));
            collect(w.child.as_ref(), out);
            return;
        }
        if let Some(b) = any.downcast_ref::<crate::pageable::BlockPageable>() {
            for c in &b.children {
                collect(c.child.as_ref(), out);
            }
        }
    }
    let mut found = vec![];
    collect(root.as_ref(), &mut found);
    assert_eq!(found, vec![(1u8, "Chapter One".to_string())]);
}

#[test]
fn h3_produces_level_3_marker() {
    use crate::pageable::HeadingMarkerWrapperPageable;

    let html = r#"<html><body><h3>Subsection</h3></body></html>"#;
    let doc = crate::blitz_adapter::parse_and_layout(html, 500.0, 500.0, &[]);
    let running_store = crate::gcpm::running::RunningElementStore::new();
    let mut ctx = ConvertContext {
        running_store: &running_store,
        assets: None,
        font_cache: HashMap::new(),
        string_set_by_node: HashMap::new(),
        counter_ops_by_node: HashMap::new(),
    };
    let root = dom_to_pageable(&doc, &mut ctx);

    fn find(p: &dyn crate::pageable::Pageable) -> Option<(u8, String)> {
        let any = p.as_any();
        if let Some(w) = any.downcast_ref::<HeadingMarkerWrapperPageable>() {
            return Some((w.marker.level, w.marker.text.clone()));
        }
        if let Some(b) = any.downcast_ref::<crate::pageable::BlockPageable>() {
            for c in &b.children {
                if let Some(h) = find(c.child.as_ref()) {
                    return Some(h);
                }
            }
        }
        None
    }
    assert_eq!(find(root.as_ref()), Some((3u8, "Subsection".to_string())));
}
```

**Step 2: Verify failure**

```bash
cargo test -p fulgur --lib h1_wraps_block_with_heading_marker h3_produces_level_3_marker
```

Expected: FAIL (no wrapping yet).

**Step 3: Implement**

In `convert_node` (the outer function at line 104), add a post-processing step. Add this helper above `convert_node`:

```rust
/// Return heading level (1-6) for `<h1>` … `<h6>`, else None.
fn heading_level(node: &Node) -> Option<u8> {
    let elem = node.element_data()?;
    let tag = elem.name.local.as_ref();
    let bytes = tag.as_bytes();
    if bytes.len() == 2 && bytes[0] == b'h' && (b'1'..=b'6').contains(&bytes[1]) {
        Some(bytes[1] - b'0')
    } else {
        None
    }
}

/// Extract plain text content from a DOM subtree, collapsing whitespace and
/// trimming. Used for outline/bookmark labels.
fn extract_text_content(doc: &blitz_dom::BaseDocument, node_id: usize) -> String {
    let mut buf = String::new();
    walk_text(doc, node_id, &mut buf);
    buf.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn walk_text(doc: &blitz_dom::BaseDocument, node_id: usize, buf: &mut String) {
    let Some(node) = doc.get_node(node_id) else { return; };
    if let NodeData::Text(t) = &node.data {
        buf.push_str(&t.content);
        return;
    }
    for &c in &node.children {
        walk_text(doc, c, buf);
    }
}
```

Modify `convert_node` (around line 104) to wrap the result when the node is a heading. Replace:

```rust
fn convert_node(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    if depth >= MAX_DOM_DEPTH {
        return Box::new(SpacerPageable::new(0.0));
    }
    let result = convert_node_inner(doc, node_id, ctx, depth);
    let result = maybe_prepend_string_set(node_id, result, ctx);
    let result = maybe_prepend_counter_ops(node_id, result, ctx);
    maybe_wrap_transform(doc, node_id, result)
}
```

with:

```rust
fn convert_node(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    if depth >= MAX_DOM_DEPTH {
        return Box::new(SpacerPageable::new(0.0));
    }
    let result = convert_node_inner(doc, node_id, ctx, depth);
    let result = maybe_prepend_string_set(node_id, result, ctx);
    let result = maybe_prepend_counter_ops(node_id, result, ctx);
    let result = maybe_wrap_transform(doc, node_id, result);
    maybe_wrap_heading(doc, node_id, result)
}

/// Wrap the result with `HeadingMarkerWrapperPageable` if the node is an
/// `h1`-`h6` element, so its position is captured during draw for PDF outline.
fn maybe_wrap_heading(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    result: Box<dyn Pageable>,
) -> Box<dyn Pageable> {
    use crate::pageable::{HeadingMarkerPageable, HeadingMarkerWrapperPageable};
    let Some(node) = doc.get_node(node_id) else {
        return result;
    };
    let Some(level) = heading_level(node) else {
        return result;
    };
    let text = extract_text_content(doc, node_id);
    if text.is_empty() {
        // An empty heading has no useful bookmark label; skip it.
        return result;
    }
    Box::new(HeadingMarkerWrapperPageable::new(
        HeadingMarkerPageable::new(level, text),
        result,
    ))
}
```

Add the new import at the top of `convert.rs` (the `HeadingMarkerPageable` / `HeadingMarkerWrapperPageable` imports are referenced via `crate::pageable::` paths in helpers so no import edit is needed — but re-export verification in Step 4 may require adjustment).

**Step 4: Re-export from pageable if needed**

Verify `HeadingMarkerPageable` and `HeadingMarkerWrapperPageable` are `pub` in `pageable.rs`. Re-run build:

```bash
cargo build
```

Fix any name-resolution errors.

**Step 5: Run tests**

```bash
cargo test -p fulgur --lib h1_wraps_block_with_heading_marker h3_produces_level_3_marker
cargo test -p fulgur --lib
```

Expected: PASS; all existing tests still PASS.

**Step 6: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(bookmarks): wrap h1-h6 with HeadingMarkerWrapperPageable"
```

---

## Task 4: Add `bookmarks` config flag

**Files:**

- Modify: `crates/fulgur/src/config.rs`
- Modify: `crates/fulgur/src/engine.rs`

**Step 1: Write the failing test**

In `engine.rs` tests module (or create one):

```rust
#[test]
fn builder_bookmarks_defaults_to_false() {
    let engine = Engine::builder().build();
    assert!(!engine.config().bookmarks);
}

#[test]
fn builder_bookmarks_opt_in() {
    let engine = Engine::builder().bookmarks(true).build();
    assert!(engine.config().bookmarks);
}
```

**Step 2: Verify failure**

```bash
cargo test -p fulgur --lib builder_bookmarks
```

Expected: FAIL (field/method missing).

**Step 3: Implement**

In `config.rs`, add `bookmarks: bool` to `Config`:

```rust
pub struct Config {
    // ... existing fields ...
    pub lang: Option<String>,
    pub bookmarks: bool,
}
```

Update `Default` impl to `bookmarks: false`.

In `engine.rs`, add a builder method near the other `pub fn`s:

```rust
pub fn bookmarks(mut self, enabled: bool) -> Self {
    self.config.bookmarks = enabled;
    self
}
```

(Match the existing builder style — most setters look like `pub fn landscape(mut self, landscape: bool) -> Self`.)

**Step 4: Run tests**

```bash
cargo test -p fulgur --lib builder_bookmarks
cargo build
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/config.rs crates/fulgur/src/engine.rs
git commit -m "feat(bookmarks): add Config.bookmarks flag and builder method"
```

---

## Task 5: Build outline in `render.rs` (stack-based)

**Files:**

- Create: `crates/fulgur/src/outline.rs`
- Modify: `crates/fulgur/src/lib.rs` (add `mod outline;`)
- Modify: `crates/fulgur/src/render.rs`

**Step 1: Write the failing test**

Create `crates/fulgur/src/outline.rs` with the function signature and tests:

```rust
//! Build a krilla Outline (PDF bookmark tree) from flat HeadingEntry records.

use krilla::interactive::destination::XyzDestination;
use krilla::interchange::outline::{Outline, OutlineNode};

use crate::pageable::{HeadingEntry, Pt};

/// Build a krilla `Outline` from a flat, source-ordered list of heading
/// entries. Headings are nested according to their level: a heading with
/// level L becomes a child of the most recent heading whose level < L.
///
/// Orphan levels (e.g. an h3 with no preceding h1/h2) are promoted to the
/// outermost open level; if the stack is empty they become top-level.
pub fn build_outline(entries: &[HeadingEntry]) -> Outline {
    // ... implementation added in Step 3.
    let _ = entries;
    Outline::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(page: usize, y: Pt, level: u8, text: &str) -> HeadingEntry {
        HeadingEntry {
            page_idx: page,
            y_pt: y,
            level,
            text: text.to_string(),
        }
    }

    // We can't introspect a built Outline via public krilla API, so the
    // tests here focus on the stack-building invariants by testing a
    // private helper `build_tree` that returns a plain Rust tree.

    use crate::outline::build_tree;

    #[derive(Debug, PartialEq)]
    struct DebugNode {
        text: String,
        level: u8,
        page: usize,
        children: Vec<DebugNode>,
    }

    fn to_debug(n: &crate::outline::TreeNode) -> DebugNode {
        DebugNode {
            text: n.text.clone(),
            level: n.level,
            page: n.page_idx,
            children: n.children.iter().map(to_debug).collect(),
        }
    }

    #[test]
    fn simple_hierarchy() {
        let entries = vec![
            entry(0, 10.0, 1, "Chapter 1"),
            entry(0, 50.0, 2, "Section 1.1"),
            entry(1, 10.0, 2, "Section 1.2"),
            entry(2, 10.0, 1, "Chapter 2"),
        ];
        let tree = build_tree(&entries);
        let debug: Vec<_> = tree.iter().map(to_debug).collect();
        assert_eq!(
            debug,
            vec![
                DebugNode {
                    text: "Chapter 1".into(),
                    level: 1,
                    page: 0,
                    children: vec![
                        DebugNode {
                            text: "Section 1.1".into(),
                            level: 2,
                            page: 0,
                            children: vec![],
                        },
                        DebugNode {
                            text: "Section 1.2".into(),
                            level: 2,
                            page: 1,
                            children: vec![],
                        },
                    ],
                },
                DebugNode {
                    text: "Chapter 2".into(),
                    level: 1,
                    page: 2,
                    children: vec![],
                },
            ]
        );
    }

    #[test]
    fn orphan_h3_becomes_top_level_when_stack_empty() {
        let entries = vec![entry(0, 10.0, 3, "Stray")];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].text, "Stray");
        assert_eq!(tree[0].level, 3);
        assert!(tree[0].children.is_empty());
    }

    #[test]
    fn skipped_level_nests_under_nearest_shallower() {
        // h1, h3 → h3 is child of h1 (no h2 between)
        let entries = vec![
            entry(0, 10.0, 1, "A"),
            entry(0, 50.0, 3, "A.x"),
        ];
        let tree = build_tree(&entries);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].text, "A.x");
    }

    #[test]
    fn empty_entries_produce_empty_outline() {
        let tree = build_tree(&[]);
        assert!(tree.is_empty());
    }
}
```

**Step 2: Verify failure**

```bash
cargo test -p fulgur --lib outline::
```

Expected: FAIL — `build_tree` / `TreeNode` do not exist.

**Step 3: Implement**

Replace `outline.rs` contents:

```rust
//! Build a krilla Outline (PDF bookmark tree) from flat HeadingEntry records.

use krilla::geom::Point;
use krilla::interactive::destination::XyzDestination;
use krilla::interchange::outline::{Outline, OutlineNode};

use crate::pageable::{HeadingEntry, Pt};

/// Intermediate, testable tree node. Converted to `OutlineNode` by
/// `to_krilla_tree` before being attached to an `Outline`.
#[derive(Debug)]
pub(crate) struct TreeNode {
    pub text: String,
    pub level: u8,
    pub page_idx: usize,
    pub y_pt: Pt,
    pub children: Vec<TreeNode>,
}

/// Build the nested tree using a stack of currently-open ancestors.
pub(crate) fn build_tree(entries: &[HeadingEntry]) -> Vec<TreeNode> {
    let mut roots: Vec<TreeNode> = Vec::new();
    // Stack of (level, path-of-indices-from-roots) — we use path indices
    // instead of references to avoid borrow-checker headaches while mutating
    // the tree.
    let mut open: Vec<(u8, Vec<usize>)> = Vec::new();

    for e in entries {
        // Pop any open ancestor whose level is >= this entry's level.
        while open.last().is_some_and(|(lvl, _)| *lvl >= e.level) {
            open.pop();
        }

        let new_node = TreeNode {
            text: e.text.clone(),
            level: e.level,
            page_idx: e.page_idx,
            y_pt: e.y_pt,
            children: vec![],
        };

        if let Some((_, path)) = open.last() {
            let parent_path = path.clone();
            let parent = walk_mut(&mut roots, &parent_path);
            parent.children.push(new_node);
            let mut new_path = parent_path.clone();
            new_path.push(parent.children.len() - 1);
            open.push((e.level, new_path));
        } else {
            roots.push(new_node);
            open.push((e.level, vec![roots.len() - 1]));
        }
    }

    roots
}

fn walk_mut<'a>(roots: &'a mut Vec<TreeNode>, path: &[usize]) -> &'a mut TreeNode {
    let (&first, rest) = path.split_first().expect("non-empty path");
    let mut node = &mut roots[first];
    for &i in rest {
        node = &mut node.children[i];
    }
    node
}

/// Build a krilla `Outline` from a flat, source-ordered list of heading
/// entries.
pub fn build_outline(entries: &[HeadingEntry]) -> Outline {
    let tree = build_tree(entries);
    let mut outline = Outline::new();
    for node in tree {
        outline.push_child(to_krilla_node(node));
    }
    outline
}

fn to_krilla_node(node: TreeNode) -> OutlineNode {
    let dest = XyzDestination::new(node.page_idx, Point::from_xy(0.0, node.y_pt));
    let mut o = OutlineNode::new(node.text, dest);
    for child in node.children {
        o.push_child(to_krilla_node(child));
    }
    o
}
```

Add to `lib.rs`:

```rust
mod outline;
pub use outline::build_outline;
```

(match the existing module/re-export style; if `lib.rs` uses `pub mod`, match that.)

**Step 4: Run tests**

```bash
cargo test -p fulgur --lib outline::
```

Expected: PASS (4 tests).

**Step 5: Commit**

```bash
git add crates/fulgur/src/outline.rs crates/fulgur/src/lib.rs
git commit -m "feat(bookmarks): add outline tree builder"
```

---

## Task 6: Wire up `HeadingCollector` in render paths

**Files:**

- Modify: `crates/fulgur/src/render.rs`

**Step 1: Write the failing integration test**

Create `crates/fulgur/tests/bookmarks_integration.rs`:

```rust
//! Integration tests: end-to-end rendering with bookmarks enabled.

use fulgur::{Engine, PageSize};

fn render_with_bookmarks(html: &str, bookmarks: bool) -> Vec<u8> {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .bookmarks(bookmarks)
        .build();
    engine.render_html(html).expect("render ok")
}

#[test]
fn bookmarks_disabled_produces_no_outline_marker() {
    let html = r#"<html><body><h1>A</h1><h2>B</h2></body></html>"#;
    let pdf = render_with_bookmarks(html, false);
    let s = String::from_utf8_lossy(&pdf);
    // Krilla only emits an `/Outlines` entry when an outline is set;
    // without bookmarks, there should be no `/Outlines` dictionary.
    assert!(
        !s.contains("/Outlines"),
        "PDF should not contain /Outlines when bookmarks disabled"
    );
}

#[test]
fn bookmarks_enabled_emits_outline_with_heading_titles() {
    let html = r#"<html><body><h1>Chapter One</h1><p>Body</p><h2>Section</h2></body></html>"#;
    let pdf = render_with_bookmarks(html, true);
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/Outlines"), "PDF must contain /Outlines");
    // Outline titles are TextStr encoded — ASCII subset appears as a literal
    // `(Chapter One)` string in the PDF body.
    assert!(
        s.contains("(Chapter One)") || s.contains("Chapter One"),
        "PDF must reference `Chapter One` title"
    );
    assert!(
        s.contains("(Section)") || s.contains("Section"),
        "PDF must reference `Section` title"
    );
}
```

**Step 2: Verify failure**

```bash
cargo test -p fulgur --test bookmarks_integration
```

Expected: FAIL — bookmarks path not hooked into render yet.

**Step 3: Implement**

In `render.rs`, modify both `render_to_pdf` and `render_to_pdf_with_gcpm` to:

1. Create `let mut collector = HeadingCollector::new()` if `config.bookmarks` is true.
2. For each page, `collector.set_current_page(page_idx)` before drawing.
3. Pass `heading_collector: Some(&mut collector)` to `Canvas` on those pages.
4. After the page loop (before `document.set_metadata`), if bookmarks enabled:

   ```rust
   let entries = collector.into_entries();
   if !entries.is_empty() {
       document.set_outline(crate::outline::build_outline(&entries));
   }
   ```

Concrete diff for `render_to_pdf` (render.rs lines 13-56):

```rust
pub fn render_to_pdf(root: Box<dyn Pageable>, config: &Config) -> Result<Vec<u8>> {
    let content_width = config.content_width();
    let content_height = config.content_height();

    let pages = paginate(root, content_width, content_height);

    let mut document = krilla::Document::new();

    let page_size = if config.landscape {
        config.page_size.landscape()
    } else {
        config.page_size
    };

    let mut collector = if config.bookmarks {
        Some(crate::pageable::HeadingCollector::new())
    } else {
        None
    };

    for (page_idx, page_content) in pages.iter().enumerate() {
        let settings = krilla::page::PageSettings::from_wh(page_size.width, page_size.height)
            .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;

        let mut page = document.start_page_with(settings);
        let mut surface = page.surface();

        if let Some(c) = collector.as_mut() {
            c.set_current_page(page_idx);
        }

        let mut canvas = Canvas {
            surface: &mut surface,
            heading_collector: collector.as_mut(),
        };
        page_content.draw(
            &mut canvas,
            config.margin.left,
            config.margin.top,
            content_width,
            content_height,
        );
    }

    if let Some(c) = collector {
        let entries = c.into_entries();
        if !entries.is_empty() {
            document.set_outline(crate::outline::build_outline(&entries));
        }
    }

    document.set_metadata(build_metadata(config));

    let pdf_bytes = document
        .finish()
        .map_err(|e| Error::PdfGeneration(format!("{e:?}")))?;
    Ok(pdf_bytes)
}
```

Apply the equivalent change to `render_to_pdf_with_gcpm` (around line 172, the matching page loop at ~214-428 and the `document.set_metadata` at ~430).

**Note on Canvas heading_collector type:** `collector.as_mut()` yields `Option<&mut HeadingCollector>`, which matches the `Option<&'a mut HeadingCollector>` field. Lifetimes line up because `collector` outlives each loop iteration.

**Step 4: Run tests**

```bash
cargo test -p fulgur --test bookmarks_integration
cargo test -p fulgur --lib
cargo test -p fulgur
```

Expected: all PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/render.rs crates/fulgur/tests/bookmarks_integration.rs
git commit -m "feat(bookmarks): wire HeadingCollector into render paths and emit outline"
```

---

## Task 7: Expose `--bookmarks` CLI flag

**Files:**

- Modify: `crates/fulgur-cli/src/main.rs`

**Step 1: Add a CLI integration test**

Create `crates/fulgur-cli/tests/bookmarks_cli.rs`:

```rust
use std::process::Command;

fn cargo_run_args(args: &[&str]) -> (std::process::Output, String) {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["run", "--quiet", "--bin", "fulgur", "--"]);
    cmd.args(args);
    let out = cmd.output().expect("cargo run failed to launch");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out, stderr)
}

#[test]
fn cli_bookmarks_flag_produces_outline() {
    let tmp = tempfile::tempdir().unwrap();
    let html_path = tmp.path().join("doc.html");
    let pdf_path = tmp.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<html><body><h1>Title</h1><h2>Sub</h2></body></html>",
    )
    .unwrap();

    let (out, stderr) = cargo_run_args(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--bookmarks",
    ]);
    assert!(out.status.success(), "CLI failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/Outlines"));
}

#[test]
fn cli_without_flag_produces_no_outline() {
    let tmp = tempfile::tempdir().unwrap();
    let html_path = tmp.path().join("doc.html");
    let pdf_path = tmp.path().join("doc.pdf");
    std::fs::write(&html_path, "<html><body><h1>Title</h1></body></html>").unwrap();

    let (out, stderr) = cargo_run_args(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "CLI failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    assert!(!s.contains("/Outlines"));
}
```

If `tempfile` is not already a dev-dependency of `fulgur-cli`, add it:

```bash
cargo add --dev --package fulgur-cli tempfile
```

**Step 2: Verify failure**

```bash
cargo test -p fulgur-cli --test bookmarks_cli
```

Expected: FAIL — `--bookmarks` flag unknown.

**Step 3: Implement**

In `crates/fulgur-cli/src/main.rs`, add to the `Render` subcommand fields (near `creation_date`):

```rust
/// Generate PDF bookmarks (outline) from h1–h6 headings.
#[arg(long)]
bookmarks: bool,
```

Where the `Render` handler builds the engine, thread the flag through:

```rust
let engine = Engine::builder()
    // ... existing calls ...
    .bookmarks(bookmarks)
    .build();
```

Locate by searching for the builder chain in `main.rs` (probably around the Render match arm).

**Step 4: Run tests**

```bash
cargo test -p fulgur-cli --test bookmarks_cli
cargo build
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur-cli/
git commit -m "feat(bookmarks): add --bookmarks CLI flag"
```

---

## Task 8: Final verification + docs

**Step 1: Full test + lint**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: all pass.

**Step 2: CLI smoke test**

```bash
cat > /tmp/book.html <<'EOF'
<html><body>
<h1>Introduction</h1>
<p>Body text.</p>
<h2>Background</h2>
<p>More body.</p>
<h1>Conclusion</h1>
</body></html>
EOF

cargo run --bin fulgur -- render /tmp/book.html -o /tmp/book.pdf --bookmarks
ls -l /tmp/book.pdf
```

Open in a PDF viewer (or grep `/Outlines` in the file) to confirm 2 top-level bookmarks with "Background" nested under "Introduction".

**Step 3: README / docs update (optional)**

If `README.md` lists CLI flags, add `--bookmarks`. Keep it minimal.

**Step 4: Commit docs (if any)**

```bash
git add README.md
git commit -m "docs(bookmarks): note --bookmarks flag"
```

---

## Out of scope for this plan

- **Manual bookmark annotations** (e.g. `<a id="toc-entry" class="pdf-bookmark">…`). The beads issue mentions this as a future option; defer unless a separate issue demands it.
- **Collapsed/expanded outline state** — the Krilla `OutlineNode` API currently offers no knob for this; skip.
- **CSS-driven `bookmark-level` / `bookmark-label` properties** (CSS GCPM). Mentioned only as a stretch goal; would warrant a separate feature issue.
- **x-position for destinations**: we anchor at `x=0` (page origin). PDF viewers scroll to the target y regardless; exact x rarely matters for outline navigation.

## Risks & notes

- **Nested inline headings**: `<p>intro <h3>…</h3> …</p>` is invalid HTML; Blitz tends to reparent. If a heading ends up inline-only, `convert_node_inner` may return a `ParagraphPageable` instead of a `BlockPageable`. The wrapper still captures the y of the paragraph's top — acceptable.
- **Empty headings** (`<h1></h1>`): skipped (no bookmark label). Documented in `maybe_wrap_heading`.
- **Multiple pages for a single heading**: marker is only on the first fragment (see Task 2 split semantics), so the bookmark always points to where the heading visually starts.
- **Deterministic output**: heading entries are appended in draw order (source order, by page). No `HashMap` iteration involved, so PDF output stays deterministic.
- **GCPM path**: because `render_to_pdf_with_gcpm` also paginates body content once and draws each page linearly, the collector approach is identical.
