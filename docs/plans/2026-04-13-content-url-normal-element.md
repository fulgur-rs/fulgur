# content: url() Phase 3 — 通常要素の画像置換 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 通常要素（非疑似要素）に `content: url()` が指定された場合、要素の内容を画像に置換してPDFに出力する。

**Architecture:** `blitz_adapter::extract_pseudo_image_url` を `extract_content_image_url` にリネームして疑似・通常要素共用にし、`convert_node_inner` の img/svg/table チェック直後に通常要素の content: url() 検出パスを挿入。ヒット時は `wrap_replaced_in_block_style` + `make_image_pageable` で `ImagePageable` を即返却し、疑似要素処理をスキップする。

**Tech Stack:** Rust, stylo (CSS computed values), blitz-dom

---

### Task 1: blitz_adapter.rs — extract_pseudo_image_url を extract_content_image_url にリネーム

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs:255` (関数定義)
- Modify: `crates/fulgur/src/blitz_adapter.rs:1269,1288,1307` (テスト内の呼び出し)
- Modify: `crates/fulgur/src/convert.rs:886,930,949,1075` (呼び出し箇所)

**Step 1: リネーム実行**

`blitz_adapter.rs` の関数名とdocコメントを更新:

```rust
/// Inspect a node's computed `content` property and return the first `Image`
/// variant's URL as an owned `String` if the content is a single `url(...)`
/// / `image-set(url(...))` item.
///
/// Works for both pseudo-element nodes (`::before`/`::after`) and normal
/// element nodes whose CSS `content` property replaces the element content.
///
/// This exists because `blitz-dom` 0.2.4 does not materialize `content: url(...)`
/// into a child image node — the match arm in
/// `blitz-dom/src/layout/construct.rs` for non-`String` ContentItem variants is
/// a TODO. fulgur bypasses that by reading the stylo computed value directly
/// and constructing an `ImagePageable` itself (see `convert::build_pseudo_image`
/// for pseudos, `convert::convert_content_url` for normal elements).
///
/// Scope: only single-item content is matched (per the fulgur-ai3 design scope
/// — multi-item content that mixes text + image is out-of-scope). The URL is
/// returned owned because `primary_styles()` yields a short-lived borrow guard
/// that cannot outlive this function; callers normalize it (e.g. via
/// `convert::extract_asset_name`) before querying `AssetBundle`.
pub fn extract_content_image_url(node: &blitz_dom::Node) -> Option<String> {
```

`convert.rs` の4箇所を更新:

- L886: `crate::blitz_adapter::extract_content_image_url(pseudo_node)?;`
- L930: `crate::blitz_adapter::extract_content_image_url(pseudo).is_some()`
- L949: `crate::blitz_adapter::extract_content_image_url(pseudo).is_some()`
- L1075: `crate::blitz_adapter::extract_content_image_url(pseudo_node)?;`

`blitz_adapter.rs` テスト内の3箇所を更新:

- L1281: `extract_content_image_url(doc.get_node(before_id).unwrap());`
- L1301: `extract_content_image_url(doc.get_node(before_id).unwrap()).is_none(),`
- L1321: `extract_content_image_url(doc.get_node(before_id).unwrap());`

テスト関数名もリネーム:

- `test_extract_pseudo_image_url_simple` → `test_extract_content_image_url_simple`
- `test_extract_pseudo_image_url_returns_none_for_string_content` → `test_extract_content_image_url_returns_none_for_string_content`
- `test_extract_pseudo_image_url_image_set` → `test_extract_content_image_url_image_set`

**Step 2: ビルド・テスト確認**

Run: `cargo test -p fulgur --lib 2>&1 | tail -5`
Expected: 全テストパス、挙動変更なし

**Step 3: コミット**

```bash
git add crates/fulgur/src/blitz_adapter.rs crates/fulgur/src/convert.rs
git commit -m "refactor: rename extract_pseudo_image_url to extract_content_image_url

Generalize the function name and docstring to reflect that it works for
both pseudo-element and normal element content: url() extraction.
No behavioral change — pure rename."
```

---

### Task 2: convert.rs — 通常要素の content: url() → ImagePageable 変換関数を追加

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (新関数追加 + convert_node_inner へ挿入)

**Step 1: テストを先に書く**

`convert.rs` の `#[cfg(test)] mod tests` 内に追加:

```rust
#[test]
fn test_convert_content_url_normal_element() {
    // A normal element with `content: url(...)` + explicit width/height
    // should produce an ImagePageable, replacing its text children.
    let icon_bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/image/icon.png"),
    )
    .unwrap();
    let mut bundle = AssetBundle::new();
    bundle.add_image("icon.png", icon_bytes);

    let html = r#"<!doctype html><html><head><style>
        .replaced { content: url("icon.png"); width: 24px; height: 24px; }
    </style></head><body><div class="replaced">This text should be replaced</div></body></html>"#;

    let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);

    let running_store = crate::gcpm::running::RunningElementStore::new();
    let mut ctx = ConvertContext {
        running_store: &running_store,
        assets: Some(&bundle),
        font_cache: HashMap::new(),
        string_set_by_node: HashMap::new(),
        counter_ops_by_node: HashMap::new(),
    };
    let tree = super::dom_to_pageable(&doc, &mut ctx);

    let mut images = Vec::new();
    collect_images(&*tree, &mut images);
    assert!(
        images.iter().any(|(w, h)| *w == 24.0 && *h == 24.0),
        "expected a 24x24 ImagePageable from content: url() on normal element, got {:?}",
        images
    );
}
```

**Step 2: テストが失敗することを確認**

Run: `cargo test -p fulgur --lib test_convert_content_url_normal_element -- --exact 2>&1 | tail -10`
Expected: FAIL（ImagePageable が生成されない）

**Step 3: convert_content_url 関数を実装**

`convert.rs` の `convert_image` 関数の直前（L1247付近）に追加:

```rust
/// Convert a normal element whose computed `content` resolves to a single
/// `url(...)` image into an `ImagePageable`. Per CSS spec, `content` on a
/// normal element replaces the element's children — so we return early and
/// skip pseudo-element processing.
///
/// Returns `None` when the element has no `content: url()`, the asset is
/// missing, or the format is unsupported — callers fall through to the
/// standard conversion path.
fn convert_content_url(node: &Node, assets: Option<&AssetBundle>) -> Option<Box<dyn Pageable>> {
    let raw_url = crate::blitz_adapter::extract_content_image_url(node)?;
    let asset_name = extract_asset_name(&raw_url);
    let bundle = assets?;
    let data = Arc::clone(bundle.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    Some(wrap_replaced_in_block_style(
        node,
        assets,
        move |w, h, opacity, visible| {
            let img = make_image_pageable(data.clone(), format, Some(w), Some(h), opacity, visible);
            Box::new(img)
        },
    ))
}
```

**Step 4: convert_node_inner に挿入**

`convert_node_inner` の svg チェック（L440-445付近）の直後、inline root チェック（L448付近）の前に挿入:

```rust
    if tag == "svg" {
        if let Some(svg) = convert_svg(node, ctx.assets) {
            return svg;
        }
    }
}  // ← 既存の elem_data ブロック閉じ

// CSS `content: url(...)` on a normal element replaces its children with
// the image (CSS Content L3 §2). Blitz 0.2.4 does not materialise this
// in layout, so we read the computed value and build an ImagePageable.
// Early return skips pseudo-element processing (spec-correct: replaced
// elements do not generate ::before/::after).
if let Some(img) = convert_content_url(node, ctx.assets) {
    return img;
}

// Check if this is an inline root (contains text layout)
```

**Step 5: テストがパスすることを確認**

Run: `cargo test -p fulgur --lib test_convert_content_url_normal_element -- --exact 2>&1 | tail -5`
Expected: PASS

**Step 6: コミット**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(convert): support content: url() on normal elements (Phase 3)

When a normal element has CSS content: url(...), replace it with an
ImagePageable using Taffy's computed size. Pseudo-element processing is
skipped per CSS spec (replaced elements don't generate ::before/::after)."
```

---

### Task 3: 追加テスト — エッジケース

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (テスト追加)

**Step 1: content: url() が無い要素は既存動作のままであるテスト**

```rust
#[test]
fn test_convert_content_url_no_content_falls_through() {
    // A normal div without content: url() should NOT produce an ImagePageable.
    let html = r#"<!doctype html><html><head><style>
        div { width: 100px; height: 50px; background: red; }
    </style></head><body><div>Normal text</div></body></html>"#;

    let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);

    let running_store = crate::gcpm::running::RunningElementStore::new();
    let mut ctx = ConvertContext {
        running_store: &running_store,
        assets: None,
        font_cache: HashMap::new(),
        string_set_by_node: HashMap::new(),
        counter_ops_by_node: HashMap::new(),
    };
    let tree = super::dom_to_pageable(&doc, &mut ctx);

    let mut images = Vec::new();
    collect_images(&*tree, &mut images);
    assert!(
        images.is_empty(),
        "normal div without content: url() should not produce images, got {:?}",
        images
    );
}
```

**Step 2: アセット未登録時は静かにスキップされるテスト**

```rust
#[test]
fn test_convert_content_url_missing_asset_falls_through() {
    // content: url("missing.png") where the asset is not in the bundle
    // should silently fall through to the normal conversion path.
    let bundle = AssetBundle::new(); // empty bundle

    let html = r#"<!doctype html><html><head><style>
        .replaced { content: url("missing.png"); width: 24px; height: 24px; }
    </style></head><body><div class="replaced">fallback text</div></body></html>"#;

    let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);

    let running_store = crate::gcpm::running::RunningElementStore::new();
    let mut ctx = ConvertContext {
        running_store: &running_store,
        assets: Some(&bundle),
        font_cache: HashMap::new(),
        string_set_by_node: HashMap::new(),
        counter_ops_by_node: HashMap::new(),
    };
    let tree = super::dom_to_pageable(&doc, &mut ctx);

    let mut images = Vec::new();
    collect_images(&*tree, &mut images);
    assert!(
        images.is_empty(),
        "missing asset should not produce images, got {:?}",
        images
    );
}
```

**Step 3: テスト実行**

Run: `cargo test -p fulgur --lib test_convert_content_url -- 2>&1 | tail -10`
Expected: 3テストすべてPASS

**Step 4: コミット**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "test(convert): add edge case tests for content: url() normal element

Cover: no content property (falls through), missing asset (silent skip)."
```

---

### Task 4: 全テスト + lint 確認

**Step 1: 全テスト**

Run: `cargo test -p fulgur --lib 2>&1 | tail -5`
Expected: 全テストパス（既存 + 新規）

**Step 2: clippy**

Run: `cargo clippy 2>&1 | tail -10`
Expected: warnings/errors なし

**Step 3: fmt**

Run: `cargo fmt --check 2>&1`
Expected: 差分なし

**Step 4: 問題があれば修正してコミット**
