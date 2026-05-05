# PDF/UA-1 Metadata and Validation Smoke Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `pdf_ua=true` rendering succeed by auto-extracting the HTML `<title>` element as
the document title, and replace the "expects failure" smoke test with passing tests that verify
PDF/UA-1 output fields.

**Architecture:** Add `extract_html_title` to `blitz_adapter.rs` to walk the DOM and find the
`<title>` element text. Pipe the result from `engine.rs` through to `render_v2` and
`build_metadata` in `render.rs`. Update smoke tests to cover success + failure cases.

**Tech Stack:** Rust, krilla (PDF generation with `Validator::UA1`), blitz-dom (HTML DOM walker)

---

## Background: why rendering currently fails

When `pdf_ua=true`, `render_v2` uses `Validator::UA1`. Krilla's UA1 validator *prohibits*
`ValidationError::NoDocumentTitle` — i.e., serialization returns `Err` if no title is set in
metadata. `config.title` is `None` by default, so all tests that call `.pdf_ua(true)` without
`.title(...)` fail today.

The existing test `pdf_ua_fails_ua1_validation_until_full_compliance_lands` documents this with
`assert!(result.is_err(), ...)`.

---

### Task 1: Add `extract_html_title` to `blitz_adapter.rs`

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 1: Write the failing test (unit test inside blitz_adapter.rs `#[cfg(test)] mod tests`)**

Find the `#[cfg(test)] mod tests` block inside `blitz_adapter.rs` and add these tests at the end
(before the closing `}`):

```rust
#[test]
fn extract_html_title_finds_title_element() {
    let doc = parse(
        r#"<html><head><title>My Document</title></head><body></body></html>"#,
        600.0,
        &[],
    );
    assert_eq!(
        super::extract_html_title(&doc),
        Some("My Document".to_string())
    );
}

#[test]
fn extract_html_title_returns_none_when_absent() {
    let doc = parse(
        r#"<html><head></head><body><p>No title here</p></body></html>"#,
        600.0,
        &[],
    );
    assert_eq!(super::extract_html_title(&doc), None);
}

#[test]
fn extract_html_title_trims_whitespace() {
    let doc = parse(
        r#"<html><head><title>  Padded Title  </title></head><body></body></html>"#,
        600.0,
        &[],
    );
    assert_eq!(
        super::extract_html_title(&doc),
        Some("Padded Title".to_string())
    );
}

#[test]
fn extract_html_title_returns_none_for_empty_title() {
    let doc = parse(
        r#"<html><head><title>   </title></head><body></body></html>"#,
        600.0,
        &[],
    );
    assert_eq!(super::extract_html_title(&doc), None);
}
```

**Step 2: Run test to verify it fails**

```bash
cd /home/ubuntu/fulgur/.worktrees/feat/fulgur-izp.10-pdf-ua-metadata
cargo test -p fulgur --lib blitz_adapter::tests::extract_html_title 2>&1 | tail -20
```

Expected: FAIL with "cannot find function `extract_html_title`"

**Step 3: Write the implementation**

In `crates/fulgur/src/blitz_adapter.rs`, add this public function **before** the `#[cfg(test)]`
block (good place: after the `extract_column_style_table` function, around line 630+):

```rust
/// Extract the text content of the HTML `<title>` element, if present.
///
/// Returns `None` when no `<title>` element exists or its text content is
/// blank. The returned string is whitespace-trimmed.
pub fn extract_html_title(doc: &HtmlDocument) -> Option<String> {
    use std::ops::Deref;

    fn find_title(doc: &blitz_dom::BaseDocument, node_id: usize) -> Option<usize> {
        let node = doc.get_node(node_id)?;
        if let Some(el) = node.element_data() {
            if el.name.local.as_ref() == "title" {
                return Some(node_id);
            }
        }
        for &child_id in &node.children {
            if let Some(found) = find_title(doc, child_id) {
                return Some(found);
            }
        }
        None
    }

    let base = doc.deref();
    let title_id = find_title(base, doc.root_element().id)?;
    let title_node = base.get_node(title_id)?;

    let mut text = String::new();
    for &child_id in &title_node.children {
        if let Some(child) = base.get_node(child_id) {
            if let blitz_dom::node::NodeData::Text(t) = &child.data {
                text.push_str(&t.content);
            }
        }
    }

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test -p fulgur --lib blitz_adapter::tests::extract_html_title 2>&1 | tail -20
```

Expected: 4 tests pass

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(blitz_adapter): add extract_html_title to read <title> from DOM"
```

---

### Task 2: Wire title extraction through engine and render

**Files:**

- Modify: `crates/fulgur/src/engine.rs` — call `extract_html_title`, pass result to `render_v2`
- Modify: `crates/fulgur/src/render.rs` — add `html_title` param to `render_v2` + `build_metadata`

**Step 1: Update `render_v2` signature in `render.rs`**

Find the `render_v2` function signature (starts around line 27):

```rust
pub fn render_v2(
    config: &Config,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    ...
    serialize_settings: SerializeSettings,
) -> Result<Vec<u8>> {
```

Add `html_title: Option<String>` as the **last parameter before `serialize_settings`**:

```rust
pub fn render_v2(
    config: &Config,
    geometry: &crate::pagination_layout::PaginationGeometryTable,
    drawables: &Drawables,
    gcpm: &GcpmContext,
    running_store: &RunningElementStore,
    font_data: &[Arc<Vec<u8>>],
    system_fonts: bool,
    string_set_by_node: &HashMap<usize, Vec<(String, String)>>,
    counter_ops_by_node: &BTreeMap<usize, Vec<crate::gcpm::CounterOp>>,
    html_title: Option<String>,
    serialize_settings: SerializeSettings,
) -> Result<Vec<u8>> {
```

**Step 2: Update `build_metadata` call and signature in `render.rs`**

Find the call to `build_metadata` (line ~274):

```rust
document.set_metadata(build_metadata(config));
```

Change to:

```rust
document.set_metadata(build_metadata(config, html_title.as_deref()));
```

Find the `build_metadata` function signature (line ~3163):

```rust
fn build_metadata(config: &Config) -> krilla::metadata::Metadata {
    let mut metadata = krilla::metadata::Metadata::new();
    if let Some(ref title) = config.title {
        metadata = metadata.title(title.clone());
    }
```

Change to:

```rust
fn build_metadata(config: &Config, html_title: Option<&str>) -> krilla::metadata::Metadata {
    let mut metadata = krilla::metadata::Metadata::new();
    let effective_title = config.title.as_deref().or(html_title);
    if let Some(title) = effective_title {
        metadata = metadata.title(title.to_string());
    }
```

**Step 3: Verify it compiles (will fail due to call site)**

```bash
cargo build -p fulgur 2>&1 | grep "error" | head -10
```

Expected: compile error about `render_v2` call in `engine.rs` having wrong argument count.

**Step 4: Update `render_v2` call in `engine.rs`**

Find the `render_v2` call (around line 407):

```rust
crate::render::render_v2(
    &self.config,
    &convert_ctx.pagination_geometry,
    &drawables,
    &gcpm,
    &running_store,
    fonts,
    self.system_fonts,
    &string_set_for_render,
    &counter_ops_for_render,
    self.serialize_settings.clone(),
)
```

Before this call, extract the HTML title (add after the `dom_to_drawables` line):

```rust
let html_title = if self.config.effective_tagging() {
    crate::blitz_adapter::extract_html_title(&doc)
} else {
    None
};
```

Then update the `render_v2` call to pass `html_title`:

```rust
crate::render::render_v2(
    &self.config,
    &convert_ctx.pagination_geometry,
    &drawables,
    &gcpm,
    &running_store,
    fonts,
    self.system_fonts,
    &string_set_for_render,
    &counter_ops_for_render,
    html_title,
    self.serialize_settings.clone(),
)
```

**Step 5: Verify it compiles**

```bash
cargo build -p fulgur 2>&1 | grep "error" | head -10
```

Expected: no errors

**Step 6: Run lib tests to check nothing is broken**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: all 909+ tests pass

**Step 7: Commit**

```bash
git add crates/fulgur/src/engine.rs crates/fulgur/src/render.rs
git commit -m "feat(render): auto-extract HTML <title> for PDF/UA-1 document title"
```

---

### Task 3: Update smoke tests in `render_smoke.rs`

**Files:**

- Modify: `crates/fulgur/tests/render_smoke.rs`

**Step 1: Locate and replace the existing failing test**

Find this test (around line 2569):

```rust
fn pdf_ua_fails_ua1_validation_until_full_compliance_lands() {
    // pdf_ua=true enables UA1 validation which requires structural
    // attributes (document title, heading /Title entries) beyond
    // what this issue wires. Full PDF/UA-1 compliance is out of scope
    // for fulgur-izp.4; this test documents the known failure mode so
    // regressions (e.g. a panic instead of a clean Err) are visible.
    let result = Engine::builder()
        .pdf_ua(true)
        .build()
        .render_html("<html><body><h1>Hello</h1><p>World</p></body></html>");
    assert!(
        result.is_err(),
        "expected UA1 validation error until full compliance lands"
    );
}
```

Replace with these 4 tests:

```rust
#[test]
fn pdf_ua_without_title_returns_error() {
    // pdf_ua=true requires a document title (PDF/UA-1 §7.1).
    // Neither config.title nor HTML <title> is provided → krilla
    // emits ValidationError::NoDocumentTitle → Err.
    let result = Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .build()
        .render_html("<html><body><h1>Hello</h1><p>World</p></body></html>");
    assert!(
        result.is_err(),
        "pdf_ua without title must return Err (NoDocumentTitle)"
    );
}

#[test]
fn pdf_ua_with_html_title_succeeds() {
    // PDF/UA-1 smoke: <title> in HTML head provides the document title,
    // satisfying krilla's UA1 requirement without explicit config.title.
    // lang + outline (h1 → bookmark) complete the required metadata.
    //
    // Manual validation: veraPDF (https://verapdf.org) can perform
    // full PDF/UA-1 validation. Run:
    //   java -jar verapdf.jar --flavour ua1 output.pdf
    // CI relies on krilla's own UA1 validator (build-time check).
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head><title>Test Document</title></head>
<body><h1>Hello</h1><p>World</p></body>
</html>"#;

    let pdf = Engine::builder()
        .pdf_ua(true)
        .lang("en")
        .build()
        .render_html(html)
        .expect("pdf_ua with <title> must succeed");

    assert!(!pdf.is_empty(), "pdf must be non-empty");
    let text = String::from_utf8_lossy(&pdf);
    assert!(
        text.contains("pdfuaid"),
        "pdf must contain pdfuaid XMP namespace"
    );
    assert!(
        text.contains("/StructTreeRoot"),
        "pdf must contain /StructTreeRoot"
    );
    assert!(
        text.contains("/Lang"),
        "pdf must contain /Lang when lang is set"
    );
}

#[test]
fn pdf_ua_with_explicit_title_succeeds() {
    // config.title takes priority over HTML <title>.
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head><title>HTML Title</title></head>
<body><h1>Hello</h1><p>World</p></body>
</html>"#;

    let pdf = Engine::builder()
        .pdf_ua(true)
        .title("Explicit Title")
        .lang("en")
        .build()
        .render_html(html)
        .expect("pdf_ua with explicit title must succeed");

    assert!(!pdf.is_empty());
}

#[test]
fn pdf_ua_without_lang_succeeds() {
    // PDF/UA-1 strongly recommends lang but does NOT hard-fail
    // when absent (krilla UA1 prohibits(NoDocumentLanguage) = false).
    // Without lang, /Lang is absent from the catalog — semantically
    // incomplete but valid per krilla's enforcement.
    let html = r#"<!DOCTYPE html>
<html>
<head><title>No Lang</title></head>
<body><h1>Hello</h1><p>World</p></body>
</html>"#;

    let pdf = Engine::builder()
        .pdf_ua(true)
        .build()
        .render_html(html)
        .expect("pdf_ua without lang must succeed");

    assert!(!pdf.is_empty());
}
```

**Step 2: Run the new tests to verify they fail first**

```bash
cargo test -p fulgur --test render_smoke pdf_ua 2>&1 | tail -20
```

Expected before the Task 2 changes: compile error or test failures (proving tests aren't vacuously passing).

After Task 2 is already done (tests run in order): verify they pass:

```bash
cargo test -p fulgur --test render_smoke pdf_ua 2>&1 | tail -20
```

Expected: 4 tests pass

**Step 3: Run full integration test suite to check for regressions**

```bash
cargo test -p fulgur 2>&1 | tail -10
```

Expected: all tests pass (no regressions)

**Step 4: Commit**

```bash
git add crates/fulgur/tests/render_smoke.rs
git commit -m "test(pdf_ua): replace failure-doc test with 4 passing PDF/UA-1 smoke tests"
```

---

## Final verification

```bash
# Full test suite
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur 2>&1 | tail -5

# Lint
cargo clippy -p fulgur 2>&1 | grep "^error" | head -10
cargo fmt --check 2>&1
```

All commands must produce no errors.

---

## Notes

### Lang behavior

`NoDocumentLanguage` is **not prohibited** by krilla's UA1 validator
(`prohibits(NoDocumentLanguage)` returns `false` for `Validator::UA1`). Rendering succeeds
without `lang`, but the output PDF has no `/Lang` entry in the catalog. For fully compliant
PDF/UA-1 output, callers should always set `.lang("...")`.

### External validator

veraPDF (Java, https://verapdf.org) provides complete PDF/UA-1 validation. It is not integrated
into CI due to the Java runtime dependency. krilla's own `Validator::UA1` catches the most
critical structural violations at serialize time, providing build-time guarantees without external
dependencies.
