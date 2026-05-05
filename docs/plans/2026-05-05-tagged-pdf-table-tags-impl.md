# Tagged PDF Table Structure Tags Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement Tagged PDF structure tags for HTML table elements (Table / THead / TBody / TFoot / TR / TH / TD), including `<th scope>` attribute reading and cell-content Identifier leaf attachment.

**Architecture:** Three-layer change — (1) extend `PdfTag` enum in `tagging.rs` to split `TRowGroup` into `THead/TBody/TFoot` and add `scope` field to `Th`; (2) read the `scope` attribute in `walk_semantics` (`convert/mod.rs`); (3) add `Th`/`Td` to the content-tagging match in `try_start_tagged` (`render.rs`) so cell text gets Identifier leaves in the StructTree.

**Tech Stack:** Rust, Krilla 0.7.0 (`krilla::tagging::{TableHeaderScope, TagKind, Tag, kind}`), existing `tagged_render_with_noto` test helper in `tests/render_smoke.rs`.

---

## Context for the implementer

All work happens in the worktree at
`.worktrees/feat/fulgur-izp.8-table-tags/`.

The relevant files:

- `crates/fulgur/src/tagging.rs` — `PdfTag` enum, `classify_element`, `pdf_tag_to_krilla_tag`
- `crates/fulgur/src/convert/mod.rs` — `walk_semantics` (lines ~373-420)
- `crates/fulgur/src/render.rs` — `try_start_tagged` (lines ~771-810)
- `crates/fulgur/tests/render_smoke.rs` — existing tagged tests around line 2487

Current state before this PR:

- `PdfTag::TRowGroup` covers `<thead>`, `<tbody>`, `<tfoot>` and always maps to `TBody`
- `PdfTag::Th` has no `scope` field and always maps to `TH(TableHeaderScope::Both)`
- `try_start_tagged` only handles `P | H { .. } | Span`

Run all tests with:

```bash
cargo test -p fulgur --manifest-path Cargo.toml 2>&1 | tail -5
```

---

### Task 1: Update unit tests in tagging.rs to assert desired behavior (failing first)

**Files:**

- Modify: `crates/fulgur/src/tagging.rs` (tests section, ~line 150)

**Step 1: Open `crates/fulgur/src/tagging.rs` and update the existing test `classify_element_recognises_lists_and_tables`**

Find the test (around line 151) and replace the three `TRowGroup` assertions and the `Th` assertion with the new expected values:

```rust
#[test]
fn classify_element_recognises_lists_and_tables() {
    assert_eq!(classify_element("ul"), Some(PdfTag::L));
    assert_eq!(classify_element("ol"), Some(PdfTag::L));
    assert_eq!(classify_element("li"), Some(PdfTag::Li));
    assert_eq!(classify_element("table"), Some(PdfTag::Table));
    assert_eq!(classify_element("thead"), Some(PdfTag::THead));
    assert_eq!(classify_element("tbody"), Some(PdfTag::TBody));
    assert_eq!(classify_element("tfoot"), Some(PdfTag::TFoot));
    assert_eq!(classify_element("tr"), Some(PdfTag::Tr));
    assert_eq!(
        classify_element("th"),
        Some(PdfTag::Th {
            scope: krilla::tagging::TableHeaderScope::Both
        })
    );
    assert_eq!(classify_element("td"), Some(PdfTag::Td));
}
```

**Step 2: Update the test `pdf_tag_to_krilla_tag_covers_all_variants`**

Replace the TRowGroup/Th assertions (around line 218-235) with:

```rust
assert!(matches!(
    pdf_tag_to_krilla_tag(&PdfTag::THead, None, None),
    TagKind::THead(_)
));
assert!(matches!(
    pdf_tag_to_krilla_tag(&PdfTag::TBody, None, None),
    TagKind::TBody(_)
));
assert!(matches!(
    pdf_tag_to_krilla_tag(&PdfTag::TFoot, None, None),
    TagKind::TFoot(_)
));
assert!(matches!(
    pdf_tag_to_krilla_tag(
        &PdfTag::Th {
            scope: krilla::tagging::TableHeaderScope::Both
        },
        None,
        None
    ),
    TagKind::TH(_)
));
assert!(matches!(
    pdf_tag_to_krilla_tag(&PdfTag::Td, None, None),
    TagKind::TD(_)
));
```

**Step 3: Run tests — expect compile error (TRowGroup still exists, THead/TBody/TFoot/Th{scope} don't exist yet)**

```bash
cargo test -p fulgur --lib 2>&1 | head -30
```

Expected: compile errors about unknown variants.

---

### Task 2: Implement PdfTag enum changes in tagging.rs

**Files:**

- Modify: `crates/fulgur/src/tagging.rs`

**Step 1: Replace `TRowGroup` and `Th` variants in the `PdfTag` enum**

Find the enum (lines ~20-33) and change from:

```rust
Table,
TRowGroup,
Tr,
Th,
Td,
```

to:

```rust
Table,
THead,
TBody,
TFoot,
Tr,
Th { scope: krilla::tagging::TableHeaderScope },
Td,
```

**Step 2: Update `classify_element`**

Change the three `TRowGroup` arms and the `Th` arm (around lines 75-79):

```rust
"table" => Some(PdfTag::Table),
"thead" => Some(PdfTag::THead),
"tbody" => Some(PdfTag::TBody),
"tfoot" => Some(PdfTag::TFoot),
"tr" => Some(PdfTag::Tr),
"th" => Some(PdfTag::Th {
    scope: krilla::tagging::TableHeaderScope::Both,
}),
"td" => Some(PdfTag::Td),
```

**Step 3: Update `pdf_tag_to_krilla_tag`**

Replace the `TRowGroup` and `Th` arms (around lines 115-120):

```rust
PdfTag::Table => krilla::tagging::Tag::<krilla::tagging::kind::Table>::Table.into(),
PdfTag::THead => krilla::tagging::Tag::<krilla::tagging::kind::THead>::THead.into(),
PdfTag::TBody => krilla::tagging::Tag::<krilla::tagging::kind::TBody>::TBody.into(),
PdfTag::TFoot => krilla::tagging::Tag::<krilla::tagging::kind::TFoot>::TFoot.into(),
PdfTag::Tr => krilla::tagging::Tag::<krilla::tagging::kind::TR>::TR.into(),
PdfTag::Th { scope } => krilla::tagging::Tag::TH(*scope).into(),
PdfTag::Td => krilla::tagging::Tag::<krilla::tagging::kind::TD>::TD.into(),
```

**Step 4: Fix any compile errors from other files that still reference `TRowGroup` or plain `Th`**

Run:

```bash
cargo build -p fulgur 2>&1 | grep "error\|TRowGroup\|PdfTag::Th"
```

The likely places that need updating:

- `crates/fulgur/src/tagging.rs` itself (the `pdf_tag_to_krilla_tag_covers_all_variants` test was just updated in Task 1)
- `crates/fulgur/src/render.rs` — `try_start_tagged` has `PdfTag::Th { .. }` pattern? No, currently it only matches P/H/Span so it won't fail. But check anyway.

Fix each compile error by updating to the new variant names.

**Step 5: Run tagging unit tests**

```bash
cargo test -p fulgur --lib tagging 2>&1
```

Expected: all tests in `tagging::tests` pass.

**Step 6: Commit**

```bash
cd .worktrees/feat/fulgur-izp.8-table-tags
git add crates/fulgur/src/tagging.rs
git commit -m "refactor(tagging): split TRowGroup into THead/TBody/TFoot, add scope to Th"
```

---

### Task 3: Read `scope` attribute from `<th>` in walk_semantics

**Files:**

- Modify: `crates/fulgur/src/convert/mod.rs` (around line 385)

**Step 1: In `walk_semantics`, make `tag` mutable and override scope for `Th`**

Find the block that starts with `if let Some(tag) = crate::tagging::classify_element(...)` and change `tag` to `mut tag`, then add scope override right before the `out.semantics.insert(...)` call:

```rust
if let Some(mut tag) = crate::tagging::classify_element(elem.name.local.as_ref()) {
    // ... (existing parent walk-up code — do not touch) ...

    let alt_text = if matches!(tag, crate::tagging::PdfTag::Figure) {
        crate::blitz_adapter::get_attr(elem, "alt").map(|v| v.to_owned())
    } else {
        None
    };

    // Override scope for <th> from the DOM `scope` attribute.
    if matches!(tag, crate::tagging::PdfTag::Th { .. }) {
        let scope = crate::blitz_adapter::get_attr(elem, "scope")
            .and_then(|s| match s {
                "row" => Some(krilla::tagging::TableHeaderScope::Row),
                "col" | "column" => Some(krilla::tagging::TableHeaderScope::Column),
                _ => None,
            })
            .unwrap_or(krilla::tagging::TableHeaderScope::Both);
        tag = crate::tagging::PdfTag::Th { scope };
    }

    out.semantics.insert(
        node_id,
        crate::tagging::SemanticEntry {
            tag,
            parent: parent_node_id,
            alt_text,
        },
    );
}
```

Note: `get_attr` is already imported via `use crate::blitz_adapter::{extract_inline_svg_tree, get_attr};` at the top of the file. Use that import directly (no `crate::blitz_adapter::` prefix needed).

**Step 2: Run all lib tests**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: all pass.

**Step 3: Commit**

```bash
git add crates/fulgur/src/convert/mod.rs
git commit -m "feat(convert): read scope attribute from <th> in walk_semantics"
```

---

### Task 4: Add Th/Td to try_start_tagged in render.rs

**Files:**

- Modify: `crates/fulgur/src/render.rs` (around line 783)

**Step 1: Write a failing integration test first**

At the end of `crates/fulgur/tests/render_smoke.rs` add:

```rust
#[test]
fn tagged_table_basic_structure() {
    let html = r#"<!DOCTYPE html><html><body>
        <table>
            <thead><tr><th>Name</th><th>Score</th></tr></thead>
            <tbody>
                <tr><td>Alice</td><td>95</td></tr>
                <tr><td>Bob</td><td>87</td></tr>
            </tbody>
        </table>
    </body></html>"#;
    let pdf = tagged_render_with_noto(html);
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"), "must have StructTreeRoot");
    assert!(s.contains("/Table"), "must have /Table tag");
    assert!(s.contains("/THead"), "must have /THead tag");
    assert!(s.contains("/TBody"), "must have /TBody tag");
    assert!(s.contains("/TH"), "must have /TH tag");
    assert!(s.contains("/TD"), "must have /TD tag");
    assert!(s.contains("/TR"), "must have /TR tag");
}
```

**Step 2: Run that specific test**

```bash
cargo test -p fulgur tagged_table_basic_structure 2>&1 | tail -10
```

Expected: **FAIL** — the test likely fails on `/TH` or `/TD` because cells have no Identifier leaves, so Krilla may omit those groups.

If it happens to pass, skip Task 4's implementation steps and go straight to Task 5.

**Step 3: Update `try_start_tagged` in `render.rs` to handle `Th` and `Td`**

Find the `if !matches!(...)` guard inside `try_start_tagged` (around line 782):

```rust
if !matches!(
    semantic.tag,
    crate::tagging::PdfTag::P | crate::tagging::PdfTag::H { .. } | crate::tagging::PdfTag::Span
) {
    return None;
}
```

Change to:

```rust
if !matches!(
    semantic.tag,
    crate::tagging::PdfTag::P
        | crate::tagging::PdfTag::H { .. }
        | crate::tagging::PdfTag::Span
        | crate::tagging::PdfTag::Th { .. }
        | crate::tagging::PdfTag::Td
) {
    return None;
}
```

**Step 4: Run the test again**

```bash
cargo test -p fulgur tagged_table_basic_structure 2>&1 | tail -10
```

Expected: PASS.

**Step 5: Run all lib tests to check no regressions**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: all pass.

**Step 6: Commit**

```bash
git add crates/fulgur/src/render.rs crates/fulgur/tests/render_smoke.rs
git commit -m "feat(render): tag TH/TD cell content in try_start_tagged"
```

---

### Task 5: Add remaining integration tests

**Files:**

- Modify: `crates/fulgur/tests/render_smoke.rs`

**Step 1: Add thead/tbody/tfoot distinction test**

```rust
#[test]
fn tagged_table_thead_tbody_tfoot_distinction() {
    let html = r#"<!DOCTYPE html><html><body>
        <table>
            <thead><tr><th>Header</th></tr></thead>
            <tbody><tr><td>Body</td></tr></tbody>
            <tfoot><tr><td>Footer</td></tr></tfoot>
        </table>
    </body></html>"#;
    let pdf = tagged_render_with_noto(html);
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/THead"), "must have /THead");
    assert!(s.contains("/TBody"), "must have /TBody");
    assert!(s.contains("/TFoot"), "must have /TFoot");
}
```

**Step 2: Add scope attribute test**

```rust
#[test]
fn tagged_th_scope_attribute_preserved() {
    let html = r#"<!DOCTYPE html><html><body>
        <table>
            <tr>
                <th scope="col">Column Header</th>
                <th scope="row">Row Header</th>
            </tr>
        </table>
    </body></html>"#;
    let pdf = tagged_render_with_noto(html);
    let s = String::from_utf8_lossy(&pdf);
    // Krilla writes /Scope /Column and /Scope /Row in the PDF stream
    assert!(s.contains("/Column"), "must have Column scope for col-scoped TH");
    assert!(s.contains("/Row"), "must have Row scope for row-scoped TH");
}
```

**Step 3: Run both new tests**

```bash
cargo test -p fulgur tagged_table_thead_tbody_tfoot_distinction 2>&1 | tail -5
cargo test -p fulgur tagged_th_scope_attribute_preserved 2>&1 | tail -5
```

Expected: both PASS.

**Step 4: Run all fulgur tests**

```bash
cargo test -p fulgur 2>&1 | tail -5
```

Expected: all pass.

**Step 5: Commit**

```bash
git add crates/fulgur/tests/render_smoke.rs
git commit -m "test(tagged): add table structure, thead/tbody/tfoot, and scope tests"
```

---

### Task 6: Final check and lint

**Step 1: Run cargo clippy**

```bash
cargo clippy -p fulgur 2>&1 | grep "^error"
```

Expected: no errors.

**Step 2: Run cargo fmt check**

```bash
cargo fmt --check -p fulgur 2>&1
```

If any formatting issues, run `cargo fmt -p fulgur` and commit.

**Step 3: Run full test suite one more time**

```bash
cargo test -p fulgur 2>&1 | tail -5
```

Expected: all pass.
