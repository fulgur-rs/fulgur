# `@page` inside inline `<style>` (fulgur-mq5)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Honor `@page { size: A4 landscape }` (and other GCPM constructs) when declared inside an inline `<style>` block, matching the behavior of `<link rel="stylesheet">`.

**Architecture:** After `parse_html_with_local_resources` returns the parsed document, walk the DOM for `<style>` elements and run `parse_gcpm` on each element's text-node content. Merge the resulting context into the engine-level `gcpm` alongside AssetBundle and link-loaded contexts. Do NOT modify the `<style>` element's text — stylo's servo engine silently drops `@page` at-rules, so non-GCPM declarations in the same stylesheet keep flowing to stylo unchanged.

**Tech Stack:** Rust, blitz-dom DOM walk, existing `parse_gcpm`.

**Root cause (verified):** `engine.rs::render_html` only feeds two CSS sources into `parse_gcpm`:

1. `AssetBundle::combined_css()` (line 62)
2. CSS fetched via `FulgurNetProvider` for `<link>` / `@import` (line 85–91)

Inline `<style>` tags in the HTML never go through `parse_gcpm`, so their `@page`, running-element, and margin-box rules are silently dropped. Reproduced with a minimal HTML and verified the same CSS via `<link>` produces the correct landscape PDF.

**Out of scope:**

- Rewriting `<style>` text to strip GCPM constructs (not needed — stylo ignores them).
- Any change to `FulgurNetProvider`, `parse_html_with_local_resources`, or the `link_gcpm` contract.

---

## Task 1: End-to-end integration test (TDD — MUST fail before the fix)

**Files:**

- Create: `crates/fulgur/tests/page_size_from_css.rs`

**Step 1: Write the failing test**

```rust
//! Integration test for fulgur-mq5: `@page { size: A4 landscape }` inside
//! an inline `<style>` block must produce a landscape PDF, matching the
//! behavior of the same CSS loaded via `<link rel="stylesheet">`.

use fulgur::Engine;

/// Returns true if the PDF bytes contain a landscape A4 MediaBox.
fn has_landscape_a4_mediabox(pdf: &[u8]) -> bool {
    // Krilla emits `/MediaBox [0 0 841.89 595.28]` for landscape A4.
    let s = std::str::from_utf8(pdf).unwrap_or("");
    s.contains("/MediaBox [0 0 841")
}

#[test]
fn page_size_landscape_from_inline_style_block() {
    let html = r#"<!doctype html><html><head>
        <style>@page { size: A4 landscape; } body { margin: 0; }</style>
    </head><body>test</body></html>"#;

    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("render");
    assert!(
        has_landscape_a4_mediabox(&pdf),
        "expected A4 landscape (841 × 595) from inline <style>"
    );
}

#[test]
fn page_size_landscape_from_link_stylesheet() {
    // Control: the same CSS via `<link>` already works — guards against
    // accidentally breaking it while fixing the inline case.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("page.css"),
        "@page { size: A4 landscape; } body { margin: 0; }",
    )
    .expect("css write");
    let html_path = dir.path().join("index.html");
    std::fs::write(
        &html_path,
        r#"<!doctype html><html><head>
            <link rel="stylesheet" href="page.css">
        </head><body>test</body></html>"#,
    )
    .expect("html write");
    let html = std::fs::read_to_string(&html_path).expect("html read");

    let engine = Engine::builder().base_path(dir.path()).build();
    let pdf = engine.render_html(&html).expect("render");
    assert!(
        has_landscape_a4_mediabox(&pdf),
        "expected A4 landscape from <link> stylesheet"
    );
}
```

**Step 2: Run — first test must fail, second must pass**

```text
cargo test -p fulgur --test page_size_from_css
```

Expected: `page_size_landscape_from_inline_style_block` fails (portrait MediaBox), `page_size_landscape_from_link_stylesheet` passes.

**Step 3: Commit**

```bash
git add crates/fulgur/tests/page_size_from_css.rs
git commit -m "test(fulgur): failing regression for @page in inline <style> (fulgur-mq5)"
```

---

## Task 2: Add `extract_gcpm_from_inline_styles`

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 1: Write a unit test first**

Append in `#[cfg(test)] mod tests`:

```rust
#[test]
fn extract_gcpm_from_inline_styles_picks_up_at_page() {
    let html = r#"<!doctype html><html><head>
        <style>@page { size: A4 landscape; }</style>
    </head><body>x</body></html>"#;
    let (doc, _) = parse_html_with_local_resources(html, 400.0, &[], None);
    let gcpm = extract_gcpm_from_inline_styles(&doc);
    assert_eq!(
        gcpm.page_settings.len(),
        1,
        "expected the @page rule to be extracted"
    );
}

#[test]
fn extract_gcpm_from_inline_styles_returns_empty_for_no_style_tag() {
    let html = r#"<!doctype html><html><body>x</body></html>"#;
    let (doc, _) = parse_html_with_local_resources(html, 400.0, &[], None);
    let gcpm = extract_gcpm_from_inline_styles(&doc);
    assert!(gcpm.page_settings.is_empty());
}

#[test]
fn extract_gcpm_from_inline_styles_folds_multiple_style_blocks() {
    let html = r#"<!doctype html><html><head>
        <style>@page { size: A4 landscape; }</style>
        <style>@page :first { margin-top: 5cm; }</style>
    </head><body>x</body></html>"#;
    let (doc, _) = parse_html_with_local_resources(html, 400.0, &[], None);
    let gcpm = extract_gcpm_from_inline_styles(&doc);
    assert_eq!(
        gcpm.page_settings.len(),
        2,
        "expected both <style> blocks' @page rules to be folded"
    );
}
```

**Step 2: Add the function**

Add near the other GCPM-adjacent helpers in `blitz_adapter.rs`:

```rust
/// Walk the DOM for `<style>` elements and fold every inline stylesheet's
/// GCPM context into one. This is the inline-HTML counterpart of the
/// link-loaded context returned by [`parse_html_with_local_resources`].
///
/// Inline `<style>` blocks are normally consumed by stylo for regular
/// CSS, but fulgur's `parse_gcpm` is only wired to stylesheets fetched
/// via the `NetProvider` path (`<link rel="stylesheet">` / `@import`).
/// Without this helper, `@page`, margin-box, running-element, and
/// counter rules placed directly in an inline `<style>` would be lost
/// (fulgur-mq5).
///
/// Note: synthetic `<style>` elements injected by
/// [`apply_link_media_rewrites`] contain `@import url(...) media;` only,
/// so `parse_gcpm` returns an empty context for them — harmless and
/// intentionally not filtered.
pub fn extract_gcpm_from_inline_styles(doc: &HtmlDocument) -> crate::gcpm::GcpmContext {
    let mut out = crate::gcpm::GcpmContext::default();
    let root = doc.root_element();
    walk_for_inline_styles(doc, root.id, &mut out, 0);
    out
}

fn walk_for_inline_styles(
    doc: &HtmlDocument,
    node_id: usize,
    out: &mut crate::gcpm::GcpmContext,
    depth: usize,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if let Some(el) = node.element_data()
        && el.name.local.as_ref() == "style"
    {
        let mut css = String::new();
        for &child_id in &node.children {
            if let Some(child) = doc.get_node(child_id)
                && let blitz_dom::node::NodeData::Text(t) = &child.data
            {
                css.push_str(&t.content);
            }
        }
        if !css.is_empty() {
            let ctx = crate::gcpm::parser::parse_gcpm(&css);
            out.extend_from(ctx);
        }
        // Don't recurse into <style> — its children are text nodes only.
        return;
    }
    for &child_id in &node.children {
        walk_for_inline_styles(doc, child_id, out, depth + 1);
    }
}
```

**Step 3: Verify unit tests**

```text
cargo test -p fulgur --lib blitz_adapter::tests::extract_gcpm
```

All three unit tests pass.

**Step 4: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(fulgur): extract_gcpm_from_inline_styles helper (fulgur-mq5)"
```

---

## Task 3: Wire into `engine.rs::render_html`

**Files:**

- Modify: `crates/fulgur/src/engine.rs`

**Step 1: Apply the wiring**

Immediately after line 91 (`gcpm.extend_from(link_gcpm);`), add:

```rust
        // Inline `<style>` blocks in the HTML are parsed by stylo for
        // regular CSS but never passed through `parse_gcpm`. Walk the
        // DOM to collect any `@page`, margin-box, running-element, and
        // counter constructs declared inline so they are honored
        // alongside the AssetBundle / link-loaded contexts (fulgur-mq5).
        let inline_gcpm = crate::blitz_adapter::extract_gcpm_from_inline_styles(&doc);
        gcpm.extend_from(inline_gcpm);
```

**Step 2: Run the failing integration test from Task 1 — it must now pass**

```text
cargo test -p fulgur --test page_size_from_css
```

Both tests pass.

**Step 3: Full verification**

```text
cargo test -p fulgur --lib
cargo test -p fulgur
cargo clippy -p fulgur --lib --tests -- -D warnings
cargo fmt --check
```

All green.

**Step 4: Manual reproduction must also pass**

```text
cargo build --bin fulgur --release
./target/release/fulgur render /tmp/pagetest.html -o /tmp/pagetest.pdf
pdfinfo /tmp/pagetest.pdf | grep "Page size"
# expect: 841.89 x 595.28 pts (A4)
```

**Step 5: Commit**

```bash
git add crates/fulgur/src/engine.rs
git commit -m "fix(fulgur): honor @page inside inline <style> blocks (fulgur-mq5)"
```

---

## Acceptance Checklist

- [ ] Integration test `page_size_landscape_from_inline_style_block` passes.
- [ ] Control test `page_size_landscape_from_link_stylesheet` still passes.
- [ ] Unit tests on `extract_gcpm_from_inline_styles` pass.
- [ ] Manual reproduction from the issue renders landscape.
- [ ] No regression in `cargo test -p fulgur`.
- [ ] `cargo clippy` and `cargo fmt --check` clean.
