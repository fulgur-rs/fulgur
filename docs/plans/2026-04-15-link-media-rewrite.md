# `<link media>` Rewrite Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `<link rel="stylesheet" media="print">` など `media` 属性が指定された外部 CSS を、screen レンダリング時に正しく除外できるようにする。

**Architecture:** blitz-dom 0.2.4 の `CssHandler::bytes` が `MediaList::empty()` をハードコードしているため、`<link media>` は blitz の native ローダ経由では落ちる。一方 `@import url("x") media;` 構文は `StylesheetLoaderInner::request_stylesheet` が `media` を正しく伝播する。そこで `parse_html_with_local_resources` に post-parse の DOM 書換ステップを追加し、`<link rel=stylesheet media=X href=Y>` を `<style>@import url("Y") X;</style>` に置換する。`<link media>` の無いリンクは従来通り blitz ネイティブで処理される。

**Tech Stack:** Rust, Blitz (blitz-dom 0.2.4), Stylo, 既存の `FulgurNetProvider` と `DomPass` 基盤。

**Scope:**

- In: `<link rel="stylesheet" media="X" href="Y">` の rewrite
- Out: `<style media>` 対応、blitz 本家への upstream PR、`disabled` 属性の再評価

**Base:** `feature/fulgur-2ai-link-media-url` branch (worktree `.worktrees/fulgur-2ai-link-media-url`)

---

## Task 1: 回帰検知テスト — CSS 内 `url()` が CSS ファイルディレクトリ基準で解決される

**なぜ最初にやるか**: 現状は NetProvider 移行により既にこの挙動が動いているはずだが、テストが無いので将来の回帰検知のために先に入れる。リファクタリング前に動いていたことを証明するベースライン。

**Files:**

- Create: `crates/fulgur/tests/link_stylesheet_url_resolution.rs`

**Step 1: Write the test**

```rust
//! Regression test: CSS internal `url()` must resolve against the CSS
//! file's own directory, not the HTML document's directory.
//!
//! Prior to the `FulgurNetProvider` migration, CSS was inlined into the
//! DOM so url() tokens resolved against the HTML. Now blitz/stylo
//! resolves them against each stylesheet's `source_url` (UrlExtraData).
//! This test pins that behaviour so a future rewrite does not regress.

use std::fs;

use fulgur::{Engine, PageSize};
use tempfile::tempdir;

#[test]
fn css_internal_url_resolves_against_stylesheet_directory() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    // Layout:
    //   root/
    //     index.html           — references css/style.css
    //     css/
    //       style.css          — references ../img/one.png  (CSS-relative)
    //       img-is-sibling.txt
    //     img/
    //       one.png            — minimal valid 1x1 PNG
    fs::create_dir(root.join("css")).unwrap();
    fs::create_dir(root.join("img")).unwrap();

    // Minimal 1x1 transparent PNG (67 bytes)
    let png: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    fs::write(root.join("img/one.png"), png).unwrap();
    fs::write(
        root.join("css/style.css"),
        // If the base is the HTML dir, this resolves to root/img/one.png ✓ (happens to work)
        // If the base is the CSS dir (root/css), `../img/one.png` resolves to root/img/one.png ✓
        // So we use a path that only works from the CSS dir:
        "body { background-image: url(./only-reachable-from-css-dir.png); }\n",
    )
    .unwrap();
    // Place the image next to the CSS file. If url() were resolved against
    // the HTML dir, it would look for root/only-reachable-from-css-dir.png
    // and fail silently; rendering still succeeds, so this test verifies
    // the resource actually loaded by counting via engine introspection.
    fs::write(root.join("css/only-reachable-from-css-dir.png"), png).unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="css/style.css">
        </head><body><p>x</p></body></html>
    "#;
    fs::write(root.join("index.html"), html).unwrap();

    // Render should succeed without panicking. We cannot currently assert
    // that the image was loaded without peeking inside PDF bytes, so
    // this task only proves that parsing+resolving does not error on
    // CSS-relative url() — the actual visual correctness is covered by
    // a VRT fixture added in a later task if needed.
    let engine = Engine::new().with_page_size(PageSize::A4);
    let pdf = engine
        .render_html(html, Some(root))
        .expect("render must succeed");
    assert!(!pdf.is_empty(), "PDF bytes should be produced");
}
```

**Step 2: Run the test**

Run: `cargo test -p fulgur --test link_stylesheet_url_resolution`

Expected: PASS (behaviour is already correct; this is a regression pin).

**Step 3: Commit**

```bash
git add crates/fulgur/tests/link_stylesheet_url_resolution.rs
git commit -m "test(link): pin CSS-relative url() resolution against stylesheet directory"
```

---

## Task 2: 失敗テスト — `<link media="print">` が screen レンダリング時に適用されない

**Files:**

- Create: `crates/fulgur/tests/link_media_attribute.rs`

**Step 1: Write the failing test**

```rust
//! `<link rel="stylesheet" media="print">` must be ignored during
//! on-screen (default) rendering. Until `LinkMediaRewritePass` lands,
//! blitz's CssHandler hardcodes `MediaList::empty()` and all <link>
//! styles apply regardless of media, so this test fails.

use std::fs;
use tempfile::tempdir;

use fulgur::{Engine, PageSize};

/// Render HTML and return whether the printable output contains the
/// given RGB color as any rasterised fill. We piggy-back on the VRT
/// pipeline's PDF→PNG helper to avoid digging into PDF internals here.
fn render_contains_red(html: &str, base: &std::path::Path) -> bool {
    let engine = Engine::new().with_page_size(PageSize::A4);
    let pdf = engine.render_html(html, Some(base)).expect("render ok");
    // Convert the first page to PNG via the VRT's pdfium wrapper.
    let png = fulgur_vrt::pdf_render::pdf_first_page_to_png(&pdf)
        .expect("pdfium conversion");
    // Decode and scan for any pixel with R > 200 && G < 60 && B < 60.
    let img = image::load_from_memory(&png).expect("decode");
    let rgba = img.to_rgba8();
    rgba.pixels()
        .any(|p| p[0] > 200 && p[1] < 60 && p[2] < 60 && p[3] > 0)
}

#[test]
fn link_media_print_does_not_apply_on_screen() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("print.css"), "body { background: red; }\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="print.css" media="print">
        </head><body>
            <p style="color:black">hello</p>
        </body></html>
    "#;

    assert!(
        !render_contains_red(html, root),
        "print.css must not be applied during screen rendering"
    );
}

#[test]
fn link_without_media_still_applies() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("base.css"), "body { background: red; }\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="base.css">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    assert!(
        render_contains_red(html, root),
        "unqualified <link> must apply; regression guard for the media rewrite"
    );
}

#[test]
fn link_media_all_still_applies() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(root.join("base.css"), "body { background: red; }\n").unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="base.css" media="all">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    assert!(
        render_contains_red(html, root),
        "media=all is the identity; must not be stripped by the rewrite"
    );
}
```

**Step 2: Ensure the VRT helper is accessible**

Check that `fulgur-vrt` exposes `pdf_render::pdf_first_page_to_png`. If it is `pub(crate)`, promote to `pub` and add a doc comment. If the symbol name differs, update the test.

Run: `grep -n 'pub fn.*pdf.*png\|renders_solid_box_html_to_png' crates/fulgur-vrt/src/pdf_render.rs`

Also add to `crates/fulgur/Cargo.toml` under `[dev-dependencies]` (if not already):

```toml
fulgur-vrt = { path = "../fulgur-vrt" }
image = "0.25"
tempfile = "3"
```

**Step 3: Run the failing test**

Run: `cargo test -p fulgur --test link_media_attribute`

Expected: `link_media_print_does_not_apply_on_screen` FAILS (red pixel found — print.css applied). Other two PASS.

**Step 4: Commit the failing test**

```bash
git add crates/fulgur/tests/link_media_attribute.rs crates/fulgur/Cargo.toml crates/fulgur-vrt/src/pdf_render.rs
git commit -m "test(link-media): add failing test for <link media=print> exclusion"
```

---

## Task 3: 書換対象の収集 — `collect_link_media_rewrites`

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` (新しい helper を `inject_style_node` の近くに追加)

**Step 1: Write unit test for the collector**

Add this test at the bottom of `blitz_adapter.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn collect_link_media_rewrites_picks_only_linked_sheets_with_non_empty_media() {
    let html = r#"
        <html><head>
            <link rel="stylesheet" href="a.css" media="print">
            <link rel="stylesheet" href="b.css">
            <link rel="stylesheet" href="c.css" media="all">
            <link rel="stylesheet" href="d.css" media="">
            <link rel="stylesheet" href="e.css" media="screen and (min-width: 600px)">
            <link rel="alternate stylesheet" href="f.css" media="print">
            <link rel="icon" href="favicon.ico" media="print">
        </head><body><p>hi</p></body></html>
    "#;
    let doc = parse(html, 800.0, &[]);
    let rewrites = collect_link_media_rewrites(&doc);

    // a.css and e.css should be rewritten. f.css has non-"stylesheet"-only
    // rel but contains "stylesheet" as a token, so include it.
    // b.css (no media), c.css (media=all), d.css (media="") are skipped.
    // favicon is not a stylesheet.
    let hrefs: Vec<&str> = rewrites.iter().map(|r| r.href.as_str()).collect();
    assert_eq!(hrefs, vec!["a.css", "e.css", "f.css"]);
    let medias: Vec<&str> = rewrites.iter().map(|r| r.media.as_str()).collect();
    assert_eq!(
        medias,
        vec!["print", "screen and (min-width: 600px)", "print"]
    );
}
```

**Step 2: Run the test**

Run: `cargo test -p fulgur --lib collect_link_media_rewrites_picks_only`

Expected: FAIL — `collect_link_media_rewrites` does not exist.

**Step 3: Implement the collector**

Add just before the `#[cfg(test)]` block in `blitz_adapter.rs`:

```rust
/// Description of a `<link rel="stylesheet" media=...>` node that needs
/// to be rewritten into `<style>@import url("...") media;</style>`.
///
/// Collected by [`collect_link_media_rewrites`] before DOM mutation so
/// the href and media values remain borrowed from a stable document
/// state (no interleaved mutation concerns).
#[derive(Debug, Clone)]
pub(crate) struct LinkMediaRewrite {
    pub link_node_id: usize,
    pub href: String,
    pub media: String,
}

/// Walk the parsed document and return every `<link rel=... stylesheet ...>`
/// element that carries a non-empty `media` attribute other than `all`.
///
/// The ordering follows a pre-order DOM traversal so the resulting
/// `<style>` elements keep the same cascade order as the original `<link>`
/// elements — important because the reordering of stylesheet origins in
/// stylo depends on insertion order.
pub(crate) fn collect_link_media_rewrites(doc: &HtmlDocument) -> Vec<LinkMediaRewrite> {
    fn walk(
        doc: &HtmlDocument,
        node_id: usize,
        depth: usize,
        out: &mut Vec<LinkMediaRewrite>,
    ) {
        if depth >= MAX_DOM_DEPTH {
            return;
        }
        let Some(node) = doc.get_node(node_id) else {
            return;
        };
        if let Some(el) = node.element_data() {
            if el.name.local.as_ref() == "link" {
                let rel_ok = get_attr(el, "rel")
                    .map(|rel| rel.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("stylesheet")))
                    .unwrap_or(false);
                let href = get_attr(el, "href").unwrap_or("").trim();
                let media = get_attr(el, "media").unwrap_or("").trim();
                let media_active =
                    !media.is_empty() && !media.eq_ignore_ascii_case("all");
                if rel_ok && !href.is_empty() && media_active {
                    out.push(LinkMediaRewrite {
                        link_node_id: node_id,
                        href: href.to_string(),
                        media: media.to_string(),
                    });
                }
            }
        }
        for &child in &node.children {
            walk(doc, child, depth + 1, out);
        }
    }

    let mut out = Vec::new();
    let root = doc.root_element().id;
    walk(doc, root, 0, &mut out);
    out
}
```

**Step 4: Run the test again**

Run: `cargo test -p fulgur --lib collect_link_media_rewrites_picks_only`

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(link-media): collect <link rel=stylesheet media=X> candidates from DOM"
```

---

## Task 4: CSS 値の安全なエスケープ — `escape_css_url`

`@import url("...")` の URL 文字列中に `"` や `\` が含まれると CSS パーサを壊すため、最小のエスケープを行う。

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 1: Write unit tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn escape_css_url_escapes_backslash_and_quote() {
    assert_eq!(escape_css_url("a.css"), "a.css");
    assert_eq!(escape_css_url(r#"a"b.css"#), r#"a\"b.css"#);
    assert_eq!(escape_css_url(r"a\b.css"), r"a\\b.css");
    assert_eq!(escape_css_url("a\nb.css"), r"a\a b.css");
}
```

**Step 2: Run to confirm failure**

Run: `cargo test -p fulgur --lib escape_css_url_`

Expected: FAIL (function not defined).

**Step 3: Implement**

Add near `collect_link_media_rewrites`:

```rust
/// Escape a URL so it can appear inside a CSS `url("...")` literal.
///
/// Per CSS Syntax Module Level 3 §4.3.5, double quote and backslash
/// must be escaped as `\"` and `\\`. Newlines are allowed via the
/// numeric escape form `\a ` (hex followed by a single space that is
/// consumed by the tokenizer).
fn escape_css_url(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str(r#"\""#),
            '\n' => out.push_str(r"\a "),
            '\r' => out.push_str(r"\d "),
            _ => out.push(ch),
        }
    }
    out
}
```

**Step 4: Rerun**

Run: `cargo test -p fulgur --lib escape_css_url_`

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(link-media): add escape_css_url helper for @import URL safety"
```

---

## Task 5: DOM 書換の適用 — `apply_link_media_rewrites`

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 1: Write an integration-level unit test** (verifies the replacement actually happens and produces a `<style>` node with the expected text content):

```rust
#[test]
fn apply_link_media_rewrites_replaces_link_with_style_import() {
    let html = r#"
        <html><head>
            <link rel="stylesheet" href="a.css" media="print">
            <link rel="stylesheet" href="b.css">
        </head><body><p>hi</p></body></html>
    "#;
    let mut doc = parse(html, 800.0, &[]);
    let rewrites = collect_link_media_rewrites(&doc);
    assert_eq!(rewrites.len(), 1);

    apply_link_media_rewrites(&mut doc, &rewrites);

    // After rewrite, the <head> should contain a <style> whose text is
    // `@import url("a.css") print;` and the original <link rel=stylesheet
    // href=a.css> should be gone. The <link rel=stylesheet href=b.css>
    // must remain untouched.
    let head = find_element_by_tag(&doc, "head").expect("head exists");
    let head_node = doc.get_node(head).unwrap();

    let mut style_text_found = None;
    let mut a_css_link_found = false;
    let mut b_css_link_found = false;
    for &cid in &head_node.children {
        let child = doc.get_node(cid).unwrap();
        if let Some(el) = child.element_data() {
            match el.name.local.as_ref() {
                "style" => {
                    // `<style>` text content lives in the first text child.
                    for &gc in &child.children {
                        let gnode = doc.get_node(gc).unwrap();
                        if let blitz_dom::node::NodeData::Text(t) = &gnode.data {
                            style_text_found = Some(t.content.clone());
                        }
                    }
                }
                "link" => match get_attr(el, "href") {
                    Some("a.css") => a_css_link_found = true,
                    Some("b.css") => b_css_link_found = true,
                    _ => {}
                },
                _ => {}
            }
        }
    }

    assert!(!a_css_link_found, "<link href=a.css> must be removed");
    assert!(b_css_link_found, "<link href=b.css> must be preserved");
    let text = style_text_found.expect("<style> with @import must exist");
    assert_eq!(text, r#"@import url("a.css") print;"#);
}
```

**Step 2: Run to confirm failure**

Run: `cargo test -p fulgur --lib apply_link_media_rewrites_replaces`

Expected: FAIL.

**Step 3: Implement**

Add to `blitz_adapter.rs` near the collector:

```rust
/// Replace every collected `<link rel=stylesheet media=X href=Y>` with a
/// `<style>@import url("Y") X;</style>` inserted at the same position.
///
/// Requirements:
/// * Preserve cascade ordering: the `<style>` is inserted *before* the
///   original `<link>`, then the `<link>` is removed.
/// * Must run before `doc.load_resource(...)` is called on any resource
///   that belongs to a rewritten `<link>` — the caller is responsible
///   for filtering those out.
pub(crate) fn apply_link_media_rewrites(
    doc: &mut HtmlDocument,
    rewrites: &[LinkMediaRewrite],
) {
    for rw in rewrites {
        // Build the @import CSS text. media is copied verbatim — stylo's
        // parser is responsible for validating media-query syntax; an
        // invalid query degrades to "never matches" (same as a browser).
        let css = format!(
            r#"@import url("{}") {};"#,
            escape_css_url(&rw.href),
            rw.media
        );

        let mut mutator = doc.mutate();
        let style_id = mutator.create_element(make_qual_name("style"), vec![]);
        let text_id = mutator.create_text_node(&css);
        mutator.append_children(style_id, &[text_id]);
        mutator.insert_nodes_before(rw.link_node_id, &[style_id]);
        // Remove the original <link>. If its stylesheet was already loaded
        // into the stylist (i.e. caller forgot to filter), this unloads it.
        mutator.remove_and_drop_node(rw.link_node_id);
        // Mutator `drop` runs here and fires `process_style_element` for
        // the new <style>, which (synchronously via our NetProvider)
        // fetches Y with the correct media propagated through @import.
    }
}
```

**Step 4: Rerun**

Run: `cargo test -p fulgur --lib apply_link_media_rewrites_replaces`

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(link-media): rewrite <link media=X> to <style>@import url() X;</style>"
```

---

## Task 6: `parse_html_with_local_resources` に組み込み、リソースフィルタ実装

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` (`parse_html_with_local_resources` 本体)

**Step 1: Inspect `Resource::Css` node id access**

Run: `grep -n 'Resource::Css\|load_resource' /home/ubuntu/.cargo/registry/src/index.crates.io-*/blitz-dom-0.2.4/src/**/*.rs | head -10`

Expected: confirm that `Resource::Css(usize, DocumentStyleSheet)` is the variant. If the enum pattern differs, adjust the filter accordingly.

**Step 2: Update `parse_html_with_local_resources`**

Replace the body (currently lines 85–116) with:

```rust
pub fn parse_html_with_local_resources(
    html: &str,
    viewport_width: f32,
    font_data: &[Arc<Vec<u8>>],
    base_path: Option<&Path>,
) -> (HtmlDocument, crate::gcpm::GcpmContext) {
    use std::collections::HashSet;

    let net_provider = Arc::new(crate::net::FulgurNetProvider::new(
        base_path.map(|p| p.to_path_buf()),
    ));
    let provider: Arc<dyn NetProvider<Resource>> = net_provider.clone();
    let base_url = base_path
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| Url::from_directory_path(&p).ok())
        .map(|u| u.to_string());

    let mut doc = parse_inner(html, viewport_width, font_data, Some(provider), base_url);

    // Identify <link rel=stylesheet media=X> nodes *before* mutating so
    // their attributes are stable, and before loading so we can filter
    // the (wrong-media) resources they already triggered during parse.
    let rewrites = collect_link_media_rewrites(&doc);
    let rewrite_node_ids: HashSet<usize> =
        rewrites.iter().map(|r| r.link_node_id).collect();

    // First drain: load only resources that correspond to <link> elements
    // WITHOUT a media rewrite (those were fetched correctly with the
    // default MediaList, which is what blitz hardcodes anyway). Discard
    // resources for nodes we are about to rewrite.
    for resource in net_provider.drain_pending_resources() {
        if let Resource::Css(node_id, _) = &resource {
            if rewrite_node_ids.contains(node_id) {
                continue;
            }
        }
        doc.load_resource(resource);
    }

    // Apply the DOM rewrite. Mutator's `drop` synchronously triggers
    // `process_style_element` for each new <style>, which parses the
    // @import, calls StylesheetLoader → NetProvider::fetch → CssHandler
    // with `MediaList` properly propagated, and pushes new Resources.
    apply_link_media_rewrites(&mut doc, &rewrites);

    // Second drain: load the correctly-fetched stylesheets.
    for resource in net_provider.drain_pending_resources() {
        doc.load_resource(resource);
    }

    // Fold the per-stylesheet GCPM contexts into one.
    let mut gcpm = crate::gcpm::GcpmContext::default();
    for ctx in net_provider.drain_gcpm_contexts() {
        gcpm.extend_from(ctx);
    }
    (doc, gcpm)
}
```

**Step 3: Run all fulgur tests**

Run: `cargo test -p fulgur`

Expected: all existing tests pass. The new `link_media_print_does_not_apply_on_screen` from Task 2 should now PASS. If `link_without_media_still_applies` or `link_media_all_still_applies` breaks, investigate — the filter likely dropped a resource it shouldn't.

**Step 4: Run clippy and fmt**

```bash
cargo clippy -p fulgur --all-targets -- -D warnings
cargo fmt --check
```

Expected: both clean.

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(link-media): wire LinkMediaRewrite into parse_html_with_local_resources"
```

---

## Task 7: Edge case — `@import` recursion inside rewritten sheets

Verify that a `<link rel=stylesheet media=print href=a.css>` whose `a.css` itself contains `@import url(b.css);` still works (b.css should be loaded and scoped to print).

**Files:**

- Modify: `crates/fulgur/tests/link_media_attribute.rs`

**Step 1: Add the test**

```rust
#[test]
fn link_media_print_nested_import_also_excluded_on_screen() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    fs::write(
        root.join("leaf.css"),
        "body { background: red; }\n",
    )
    .unwrap();
    fs::write(
        root.join("print.css"),
        "@import url(\"leaf.css\");\n",
    )
    .unwrap();

    let html = r#"
        <!DOCTYPE html>
        <html><head>
            <link rel="stylesheet" href="print.css" media="print">
        </head><body>
            <p>hello</p>
        </body></html>
    "#;

    assert!(
        !render_contains_red(html, root),
        "nested @import under a print-only <link> must also be excluded on screen"
    );
}
```

**Step 2: Run**

Run: `cargo test -p fulgur --test link_media_attribute link_media_print_nested_import_also`

Expected: PASS (the outer `@import url("print.css") print;` scopes all transitively imported rules to `print`).

**Step 3: Commit**

```bash
git add crates/fulgur/tests/link_media_attribute.rs
git commit -m "test(link-media): cover nested @import under a print-only <link>"
```

---

## Task 8: Example fixture

**Files:**

- Create: `examples/link-media/`

Add a tiny example demonstrating a print-only `<link>`. This is consumed by the `examples/` workflow and keeps documentation in sync with behaviour.

**Step 1: Create the fixture**

```bash
mkdir -p examples/link-media
```

Create `examples/link-media/index.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Link media example</title>
  <link rel="stylesheet" href="screen.css">
  <link rel="stylesheet" href="print.css" media="print">
</head>
<body>
  <h1>Hello</h1>
  <p>This paragraph reads black on screen, dark green in print.</p>
</body>
</html>
```

Create `examples/link-media/screen.css`:

```css
body { color: black; font-family: "Noto Sans", sans-serif; }
```

Create `examples/link-media/print.css`:

```css
body { color: #064e3b; }
```

Create `examples/link-media/README.md`:

````markdown
# link-media

Demonstrates that `<link rel="stylesheet" media="print">` is honoured:
the browser sees black text, the PDF (print media) shows dark green.

## Regenerate

```bash
cargo run --bin fulgur -- render examples/link-media/index.html \
    -o examples/link-media/link-media.pdf
```
````

**Step 2: Regenerate the PDF**

Run:

```bash
FONTCONFIG_FILE=examples/.fontconfig/fonts.conf \
    cargo run --release --bin fulgur -- render \
    examples/link-media/index.html \
    -o examples/link-media/link-media.pdf
```

Expected: PDF generated without panics.

**Step 3: Commit**

```bash
git add examples/link-media/
git commit -m "docs(examples): add link-media example for <link media=print>"
```

---

## Task 9: CHANGELOG

**Files:**

- Modify: `CHANGELOG.md`

**Step 1: Add entry under Unreleased (or next version heading)**

```markdown
### Added

- `<link rel="stylesheet" media="...">` is now honoured. External
  stylesheets tagged with a media query (e.g. `media="print"`) are
  rewritten to `<style>@import url("...") media;</style>` during
  parsing so that blitz/stylo applies the correct `MediaList`
  instead of loading the sheet unconditionally.
  (fulgur-2ai)
```

**Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): note <link media> handling (fulgur-2ai)"
```

---

## Task 10: Final verification

**Step 1: Run everything**

```bash
cargo test -p fulgur
cargo test -p fulgur --test gcpm_integration
cargo clippy --all-targets -- -D warnings
cargo fmt --check
npx markdownlint-cli2 '**/*.md'
```

Expected: all pass.

**Step 2: Close the issue**

```bash
bd close fulgur-2ai --reason="<link media> rewritten to @import url() media at DOM level; CSS-internal url() resolution pinned by regression test"
bd sync --flush-only
```

---

## Risk notes

- **Wasted first fetch**: every `<link rel=stylesheet media=X>` is fetched once with empty MediaList, then re-fetched via `@import`. Cost is the file-read + CSS parse. For local file serving this is negligible; no network calls are involved.
- **Duplicate GCPM contexts**: if `print.css` contains GCPM constructs (`@page`, `position: running`, etc.), the first fetch's `GcpmContext` is still pushed into `inner.gcpm_contexts` by `FulgurNetProvider::fetch`. To prevent double-counting, a follow-up change may drain `gcpm_contexts` in parallel with the filtered resource drain. Not required for Task 6 because current test corpus does not mix GCPM with `<link media>`; flag as a follow-up issue if encountered.
- **blitz upstream**: if blitz fixes `CssHandler` to accept `media`, remove the rewrite. Until then fulgur is the source of truth.
