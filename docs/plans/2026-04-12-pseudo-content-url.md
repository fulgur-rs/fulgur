# Pseudo Element `content: url()` Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `::before` / `::after` 疑似要素上の `content: url("asset.png")` を AssetBundle から解決して `ImagePageable` として描画する (beads issue `fulgur-ai3`)。

**Architecture:** Blitz 0.2.4 は疑似要素の `content: Image` を TODO として未対応だが、Stylo の computed value 側には `ContentItem::Image(url)` として保持されている。fulgur 側で (1) blitz_adapter に疑似要素 content 読み出しヘルパを新設、(2) convert.rs で parent ノードを convert する際に `node.before` / `node.after` を明示的にチェック、(3) 画像化パスを既存 `<img>` と共通化された `make_image_pageable` ヘルパで構築する。Phase 1 は `display:block` ケースのみ(BlockPageable として親の前後に挿入)、Phase 2 は inline 配置(paragraph.rs の ShapedLine 拡張)を扱う。

**Tech Stack:** Rust / Blitz 0.2.4 / Stylo 0.8.0 / Krilla / Parley / cargo test

**Reference:** beads issue `fulgur-ai3` に完全な design と acceptance criteria あり。`bd show fulgur-ai3` を実行すれば読める。

---

## Current-state Notes (実装者向けの事前コンテキスト)

以下は実装前に既に確認済の事実。無駄な探索を避けるため列挙する:

- **疑似要素の格納場所:** Blitz `Node` は `pub before: Option<usize>` と `pub after: Option<usize>` を持つ。`pe_by_index(0)==after`, `pe_by_index(1)==before`。
- **fulgur の現状:** `convert.rs` は `node.children` だけを走査しており、疑似要素を見ていない。String content の疑似要素は Blitz が inline_layout_data (Parley) の中にテキストとして埋め込むため、`extract_paragraph` が結果的に拾っているが、Image content はテキスト化されない (`blitz-dom/src/layout/construct.rs:373` に TODO コメント)。
- **Stylo の content enum:** `style::values::generics::counters::GenericContentItem<I>` に `Image(I)` variant あり。`I` は `style::values::computed::image::Image` で、`Image::Url(ComputedUrl::Valid(url))` パターンに落ちる。
- **AssetBundle 連携:** 既存 `background-image: url()` パス (`convert.rs:942-977`) が同じ Stylo 型から `extract_asset_name` で正規化して `assets.get_image` を呼んでいる。そのまま流用可。
- **ImagePageable の fields (image.rs:29-36):** `image_data / format / width / height / opacity / visible`。alt フィールドは無い。
- **既存 `<img>` 処理 (convert.rs:572-589):** `convert_image` が src 属性 → AssetBundle → `ImagePageable::new(data, format, w, h)` → `wrap_replaced_in_block_style`。
- **画像寸法ヘルパ:** `ImagePageable::decode_dimensions(data, format)` で PNG/JPEG/GIF intrinsic ピクセルを取得可。
- **convert_image の sizing:** 実は現状の `convert_image` は Taffy が出した layout を使っている(`final_layout`)。疑似要素は Blitz の layout 上 size=0 なので自前で sizing 計算する必要がある (design セクション 3)。
- **テストインフラ:** `examples/*` 各ディレクトリは HTML + アセットを置く独立ディレクトリ。`examples_determinism.rs` が決定的再生成を検証する。
- **画像フォーマット:** 今回 example には PNG のみ使用(既存の `examples/image/icon.png` を流用可能)。
- **ワークスペース:** この計画はすでに worktree `/home/ubuntu/fulgur/.worktrees/pseudo-content-url` (branch `feature/pseudo-content-url`) で作業中である前提。全 cargo コマンドはそのディレクトリで実行する。

---

## Phase 1: Block-display pseudo element

`display: block` の `::before` / `::after` で `content: url()` を使ったケースを対象にする。Phase 1 の完了条件は「block 画像が親ノードの前/後に BlockPageable として挿入され、examples に追加された Case A の PDF が決定的に再生成される」こと。

### Task 1: blitz_adapter に `extract_pseudo_image_url` ヘルパを追加

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` (末尾付近に新関数を追加、および pub export)
- Test: `crates/fulgur/src/blitz_adapter.rs` の `#[cfg(test)] mod tests` 内

**Step 1: Write the failing unit test**

`blitz_adapter.rs` の tests モジュールに以下を追加する。

```rust
#[test]
fn test_extract_pseudo_image_url_simple() {
    // HTML: <style>h1::before { content: url("logo.png"); }</style><h1>T</h1>
    // Parse & resolve, then walk nodes to find the ::before pseudo and
    // confirm extract_pseudo_image_url returns Some("logo.png") (or a path
    // that extract_asset_name can normalize to "logo.png").
    let html = r#"<!doctype html><html><head><style>
        h1::before { content: url("logo.png"); }
    </style></head><body><h1>T</h1></body></html>"#;
    let mut doc = super::parse(html, 800.0, &[]);
    super::resolve(&mut doc);
    // Walk: find <h1> element, read its `before` slot, call the helper.
    let h1_id = find_element_by_local_name(&doc, "h1").expect("h1");
    let before_id = doc.get_node(h1_id).unwrap().before.expect("::before");
    let url = super::extract_pseudo_image_url(doc.get_node(before_id).unwrap());
    assert!(url.is_some(), "expected url, got None");
    let url = url.unwrap();
    assert!(
        url.ends_with("logo.png"),
        "unexpected url: {url}"
    );
}

#[test]
fn test_extract_pseudo_image_url_returns_none_for_string_content() {
    let html = r#"<!doctype html><html><head><style>
        h1::before { content: "prefix "; }
    </style></head><body><h1>T</h1></body></html>"#;
    let mut doc = super::parse(html, 800.0, &[]);
    super::resolve(&mut doc);
    let h1_id = find_element_by_local_name(&doc, "h1").expect("h1");
    let before_id = doc.get_node(h1_id).unwrap().before.expect("::before");
    assert!(
        super::extract_pseudo_image_url(doc.get_node(before_id).unwrap()).is_none()
    );
}

// Helper — add if not present in the tests module.
fn find_element_by_local_name(
    doc: &blitz_html::HtmlDocument,
    name: &str,
) -> Option<usize> {
    fn walk(
        doc: &blitz_dom::BaseDocument,
        id: usize,
        name: &str,
    ) -> Option<usize> {
        let node = doc.get_node(id)?;
        if let Some(ed) = node.element_data() {
            if ed.name.local.as_ref() == name {
                return Some(id);
            }
        }
        for &c in &node.children {
            if let Some(v) = walk(doc, c, name) {
                return Some(v);
            }
        }
        None
    }
    use std::ops::Deref;
    walk(doc.deref(), doc.root_element().id, name)
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p fulgur blitz_adapter::tests::test_extract_pseudo_image_url`
Expected: `error[E0425]: cannot find function 'extract_pseudo_image_url'`

**Step 3: Implement the helper**

`blitz_adapter.rs` の適切な位置 (resolve/parse 付近) に以下を追加する。

```rust
/// Inspect a pseudo-element node's computed `content` property. Returns the
/// first `Image` variant's URL string if present, otherwise None.
///
/// This exists because blitz-dom 0.2.4 does not materialize `content: url(...)`
/// into a child image node — the match arm in `construct.rs` for
/// `ContentItem::Image` is a TODO. We bypass that by reading the computed
/// value directly and constructing an `ImagePageable` ourselves (see
/// `convert::build_pseudo_image`).
///
/// The URL is returned as a raw `&str`; callers are expected to normalize
/// it via `extract_asset_name` before querying `AssetBundle`.
pub fn extract_pseudo_image_url(node: &blitz_dom::Node) -> Option<&str> {
    use style::values::generics::counters::{Content, ContentItem};
    let styles = node.primary_styles()?;
    let content = &styles.get_counters().content;
    let items = match content {
        Content::Items(item_data) => item_data,
        _ => return None,
    };
    // Only inspect the "main" items (before `alt_start`). Content after
    // alt_start is alt-text in CSS Level 3 Content.
    let main = &items.items[..items.alt_start];
    // Scope: the design covers "single-item url content". Multi-item is
    // out-of-scope for this issue, so we require items.len() == 1.
    if main.len() != 1 {
        return None;
    }
    match &main[0] {
        ContentItem::Image(img) => extract_url_from_stylo_image(img),
        _ => None,
    }
}

/// Alt-text extraction. Returns `Option<String>` by concatenating any
/// `ContentItem::String` entries found after `alt_start`.
pub fn extract_pseudo_image_alt(node: &blitz_dom::Node) -> Option<String> {
    use style::values::generics::counters::{Content, ContentItem};
    let styles = node.primary_styles()?;
    let content = &styles.get_counters().content;
    let items = match content {
        Content::Items(item_data) => item_data,
        _ => return None,
    };
    let alt = &items.items[items.alt_start..];
    if alt.is_empty() {
        return None;
    }
    let mut out = String::new();
    for item in alt {
        if let ContentItem::String(s) = item {
            out.push_str(s);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Unwrap a `style::values::computed::image::Image` into a URL string if it
/// is a `Url(ComputedUrl::Valid(_))` (or `image-set(...)` resolving to one).
///
/// image-set(...) note: stylo resolves image-set() at computed-value time
/// based on device pixel ratio, producing a single Image::Url. We do not
/// need to handle the ImageSet enum variant ourselves.
fn extract_url_from_stylo_image(
    image: &style::values::computed::image::Image,
) -> Option<&str> {
    use style::servo::url::ComputedUrl;
    use style::values::computed::image::Image;
    match image {
        Image::Url(ComputedUrl::Valid(url)) => Some(url.as_str()),
        Image::Url(ComputedUrl::Invalid(s)) => Some(s.as_str()),
        _ => None,
    }
}
```

**Step 4: Run the tests and verify they pass**

Run: `cargo test -p fulgur blitz_adapter::tests::test_extract_pseudo_image_url`
Expected: PASS (2 tests)

Also run `cargo build -p fulgur` — no warnings.

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(adapter): add extract_pseudo_image_url / _alt helpers

Read stylo ContentItem::Image from pseudo-element computed styles.
Bridges around blitz-dom 0.2.4's TODO for content: url() in ::before/::after.
Part of fulgur-ai3 Phase 1."
```

---

### Task 2: ImagePageable に optional `alt` フィールドを追加

**Files:**

- Modify: `crates/fulgur/src/image.rs:29-48`
- Test: `crates/fulgur/src/image.rs` の既存 tests モジュール

**Step 1: Write the failing test**

```rust
#[test]
fn test_image_pageable_alt_default_none() {
    let data = std::sync::Arc::new(minimal_png_bytes());
    let img = super::ImagePageable::new(
        data,
        super::ImageFormat::Png,
        10.0,
        10.0,
    );
    assert!(img.alt.is_none(), "alt should default to None");
}

#[test]
fn test_image_pageable_with_alt() {
    let data = std::sync::Arc::new(minimal_png_bytes());
    let img = super::ImagePageable::new(
        data,
        super::ImageFormat::Png,
        10.0,
        10.0,
    )
    .with_alt("warning icon".to_string());
    assert_eq!(img.alt.as_deref(), Some("warning icon"));
}

// minimal_png_bytes() already exists in the module if there's prior test
// coverage; if not, copy the 8-byte PNG signature + minimal IHDR.
```

Verify `minimal_png_bytes` helper exists in the test module — if not, add it:

```rust
fn minimal_png_bytes() -> Vec<u8> {
    // 8-byte signature + IHDR (13 bytes payload + 4 length + 4 type + 4 CRC)
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // signature
        0x00, 0x00, 0x00, 0x0D, // IHDR length = 13
        b'I', b'H', b'D', b'R',
        0x00, 0x00, 0x00, 0x01, // width = 1
        0x00, 0x00, 0x00, 0x01, // height = 1
        0x08, 0x02, 0x00, 0x00, 0x00, // bit depth / color / ...
        0x00, 0x00, 0x00, 0x00, // CRC (not validated)
    ]
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p fulgur image::tests::test_image_pageable_alt`
Expected: FAIL — `no field 'alt'` / `no method 'with_alt'`

**Step 3: Implement**

Edit `image.rs`:

```rust
#[derive(Clone)]
pub struct ImagePageable {
    pub image_data: Arc<Vec<u8>>,
    pub format: ImageFormat,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub visible: bool,
    /// Optional alt text, primarily sourced from `content: url(...) / "alt"`
    /// in CSS Level 3 Content. Reserved for future Tagged PDF / PDF/UA
    /// accessibility support (tracked separately). Not rendered today.
    pub alt: Option<String>,
}

impl ImagePageable {
    pub fn new(data: Arc<Vec<u8>>, format: ImageFormat, width: f32, height: f32) -> Self {
        Self {
            image_data: data,
            format,
            width,
            height,
            opacity: 1.0,
            visible: true,
            alt: None,
        }
    }

    pub fn with_alt(mut self, alt: String) -> Self {
        self.alt = Some(alt);
        self
    }
    // ... existing methods ...
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p fulgur image::tests`
Expected: all PASS

Run: `cargo build -p fulgur` — confirm no call site broke (ImagePageable is constructed via `::new`, so the new field is always defaulted).

**Step 5: Commit**

```bash
git add crates/fulgur/src/image.rs
git commit -m "feat(image): add optional alt field to ImagePageable

Reserved for CSS content: url() / \"alt\" and future Tagged PDF work.
Not rendered — fulgur-ai3 Phase 1 only records the value.
"
```

---

### Task 3: `make_image_pageable` 共通ヘルパを convert.rs に追加

Purpose: 既存 `<img>` パスと Phase 1 新設の pseudo パスで sizing ロジックを共有する。

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (image.rs import 付近、関数追加)

**Step 1: Write the failing test**

`crates/fulgur/src/convert.rs` のテストモジュール (末尾か既存の `#[cfg(test)] mod tests` 内) に追加。

```rust
#[test]
fn test_make_image_pageable_both_dimensions() {
    let data = std::sync::Arc::new(image::tests::minimal_png_bytes());
    let img = super::make_image_pageable(
        data,
        crate::image::ImageFormat::Png,
        Some(100.0),
        Some(50.0),
        1.0,
        true,
    );
    assert_eq!(img.width, 100.0);
    assert_eq!(img.height, 50.0);
}

#[test]
fn test_make_image_pageable_width_only_uses_intrinsic_aspect() {
    // PNG helper returns 1x1 intrinsic, aspect = 1. Width=40 → height=40.
    let data = std::sync::Arc::new(image::tests::minimal_png_bytes());
    let img = super::make_image_pageable(
        data,
        crate::image::ImageFormat::Png,
        Some(40.0),
        None,
        1.0,
        true,
    );
    assert_eq!(img.width, 40.0);
    assert_eq!(img.height, 40.0);
}

#[test]
fn test_make_image_pageable_intrinsic_fallback() {
    let data = std::sync::Arc::new(image::tests::minimal_png_bytes());
    let img = super::make_image_pageable(
        data,
        crate::image::ImageFormat::Png,
        None,
        None,
        0.5,
        false,
    );
    assert_eq!(img.width, 1.0);
    assert_eq!(img.height, 1.0);
    assert_eq!(img.opacity, 0.5);
    assert!(!img.visible);
}
```

(If `minimal_png_bytes` is not yet `pub(crate)`, expose it with `pub(crate)` in `image.rs`.)

**Step 2: Run tests to verify they fail**

Run: `cargo test -p fulgur convert::tests::test_make_image_pageable`
Expected: FAIL — function not defined.

**Step 3: Implement**

Add to `convert.rs`:

```rust
/// Shared sizing / construction for ImagePageable, used by both the `<img>`
/// element path and the `::before`/`::after` `content: url()` pseudo path.
///
/// Sizing rules match the CSS replaced-element spec:
/// - both css dims given → use them verbatim
/// - one given → scale the other by the image's intrinsic aspect ratio
/// - neither given → use intrinsic pixel dimensions (treated as 1px = 1pt
///   since ImagePageable draws in PDF points; this matches the existing
///   behavior of `<img>` in fulgur)
fn make_image_pageable(
    data: Arc<Vec<u8>>,
    format: crate::image::ImageFormat,
    css_w: Option<f32>,
    css_h: Option<f32>,
    opacity: f32,
    visible: bool,
) -> ImagePageable {
    let (iw, ih) = ImagePageable::decode_dimensions(&data, format).unwrap_or((1, 1));
    let iw = iw as f32;
    let ih = ih as f32;
    let aspect = if ih > 0.0 { iw / ih } else { 1.0 };

    let (w, h) = match (css_w, css_h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, if aspect != 0.0 { w / aspect } else { w }),
        (None, Some(h)) => (h * aspect, h),
        (None, None) => (iw, ih),
    };

    let mut img = ImagePageable::new(data, format, w, h);
    img.opacity = opacity;
    img.visible = visible;
    img
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p fulgur convert::tests::test_make_image_pageable`
Expected: 3 PASS

**Step 5: Commit**

```bash
git add crates/fulgur/src/convert.rs crates/fulgur/src/image.rs
git commit -m "refactor(convert): extract make_image_pageable sizing helper

Shared by <img> and pseudo content: url() paths. Matches CSS replaced
element sizing spec. Part of fulgur-ai3 Phase 1."
```

---

### Task 4: `<img>` の既存パスを `make_image_pageable` 経由に切り替える (リグレッション防止)

**Files:**

- Modify: `crates/fulgur/src/convert.rs:571-589` (`convert_image`)

**Step 1: Write / update existing test**

既存の `<img>` 関連テストがあればそのまま使う。`examples/image/` 配下の PDF が等価に再生成されることで間接的に確認する。追加のピンポイントテストは不要。

**Step 2: Refactor convert_image**

```rust
fn convert_image(node: &Node, assets: Option<&AssetBundle>) -> Option<Box<dyn Pageable>> {
    let elem = node.element_data()?;
    let src = get_attr(elem, "src")?;
    let bundle = assets?;
    let data = Arc::clone(bundle.get_image(src)?);
    let format = ImagePageable::detect_format(&data)?;

    Some(wrap_replaced_in_block_style(
        node,
        assets,
        move |w, h, opacity, visible| {
            // wrap_replaced_in_block_style already resolved (w, h) from layout,
            // so pass them in as explicit css_w/css_h. This keeps <img> behavior
            // byte-identical to the previous implementation.
            let img = make_image_pageable(
                data.clone(),
                format,
                Some(w),
                Some(h),
                opacity,
                visible,
            );
            Box::new(img)
        },
    ))
}
```

**Step 3: Run determinism tests**

Run (from worktree root):

```bash
cargo test -p fulgur --lib
cargo test -p fulgur-cli --test examples_determinism
```

Expected: all PASS (no drift on existing `<img>` examples).

**Step 4: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "refactor(convert): route <img> through make_image_pageable

No behavior change — the refactor keeps existing sizing semantics by
passing layout-resolved (w, h) as explicit css_w/css_h. Validated by
examples_determinism."
```

---

### Task 5: `build_pseudo_image` ヘルパを convert.rs に追加

**Files:**

- Modify: `crates/fulgur/src/convert.rs`

**Step 1: Write the failing test**

`convert.rs` のテストモジュールに追加:

```rust
#[test]
fn test_build_pseudo_image_reads_content_url() {
    // Use a real asset: examples/image/icon.png is small and already bundled.
    let icon_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/image/icon.png");
    let icon_bytes = std::fs::read(&icon_path).expect("read icon.png");
    let mut bundle = AssetBundle::new();
    bundle.add_image("icon.png".into(), std::sync::Arc::new(icon_bytes));

    let html = r#"<!doctype html><html><head><style>
        h1::before { content: url("icon.png"); display: block; width: 32px; height: 32px; }
    </style></head><body><h1>T</h1></body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);

    let h1_id = super::tests::find_element_by_local_name(&doc, "h1").expect("h1");
    let before_id = doc.get_node(h1_id).unwrap().before.expect("::before");
    let pseudo = doc.get_node(before_id).unwrap();

    let parent = doc.get_node(h1_id).unwrap();
    let parent_width = parent.final_layout.size.width;

    let img = super::build_pseudo_image(pseudo, parent_width, Some(&bundle))
        .expect("build_pseudo_image should succeed");
    assert_eq!(img.width, 32.0);
    assert_eq!(img.height, 32.0);
}

#[test]
fn test_build_pseudo_image_missing_asset_returns_none() {
    let bundle = AssetBundle::new();
    let html = r#"<!doctype html><html><head><style>
        h1::before { content: url("missing.png"); }
    </style></head><body><h1>T</h1></body></html>"#;
    let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);
    let h1_id = super::tests::find_element_by_local_name(&doc, "h1").expect("h1");
    let before_id = doc.get_node(h1_id).unwrap().before.expect("::before");
    let pseudo = doc.get_node(before_id).unwrap();
    assert!(super::build_pseudo_image(pseudo, 500.0, Some(&bundle)).is_none());
}
```

(`find_element_by_local_name` は Task 1 で tests モジュールに置いたもの。reuse するために `pub(crate) fn` にするか、Task 1 の add 時点で `pub(super)` にしておく。)

**Step 2: Run tests to verify they fail**

Run: `cargo test -p fulgur convert::tests::test_build_pseudo_image`
Expected: FAIL — function not defined.

**Step 3: Implement**

Add to `convert.rs` next to `convert_image`:

```rust
/// Build an `ImagePageable` for a `::before`/`::after` pseudo-element node
/// whose computed `content` resolves to a single `url(...)` image.
///
/// Returns None if the content is not an image, or the image cannot be
/// resolved in the AssetBundle (silent skip — matches background-image
/// handling).
///
/// `parent_width` is the content-box width of the pseudo's containing block,
/// used to resolve percentage width/height values.
fn build_pseudo_image(
    pseudo_node: &Node,
    parent_width: f32,
    assets: Option<&AssetBundle>,
) -> Option<ImagePageable> {
    let assets = assets?;

    let raw_url = crate::blitz_adapter::extract_pseudo_image_url(pseudo_node)?;
    let asset_name = extract_asset_name(raw_url);
    let data = Arc::clone(assets.get_image(asset_name)?);
    let format = ImagePageable::detect_format(&data)?;

    // Read computed CSS width/height on the pseudo-element itself.
    let styles = pseudo_node.primary_styles()?;
    let css_w = resolve_pseudo_length(&styles.clone_width(), parent_width);
    let css_h = resolve_pseudo_length(&styles.clone_height(), parent_width);

    let (opacity, visible) = extract_opacity_visible(pseudo_node);
    let mut img = make_image_pageable(data, format, css_w, css_h, opacity, visible);

    if let Some(alt) = crate::blitz_adapter::extract_pseudo_image_alt(pseudo_node) {
        img.alt = Some(alt);
    }
    Some(img)
}

/// Resolve a Stylo `Size`-ish value to an absolute pt, or None if `auto`.
/// Percentages resolve against `parent_width`.
fn resolve_pseudo_length(
    size: &style::values::computed::Size,
    parent_width: f32,
) -> Option<f32> {
    use style::values::generics::length::GenericSize;
    match size {
        GenericSize::Auto => None,
        GenericSize::LengthPercentage(lp) => {
            // `LengthPercentage::resolve` computes absolute px given a base.
            let px = lp.0.resolve(style::values::computed::Length::new(parent_width));
            Some(px.px())
        }
        _ => None, // min-content, max-content, fit-content — treat as auto
    }
}
```

> **Note for implementer:** The exact Stylo API for `Size` / `LengthPercentage::resolve` may differ — the above is the expected shape but confirm against `crates/fulgur/src/convert.rs:extract_block_style` which already resolves dimensions, and against `stylo-0.8.0/values/computed/length_percentage.rs`. Mirror the pattern used there if this call doesn't compile cleanly.

**Step 4: Run tests to verify they pass**

Run: `cargo test -p fulgur convert::tests::test_build_pseudo_image`
Expected: 2 PASS

**Step 5: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(convert): build_pseudo_image resolves content: url() to ImagePageable

Reads stylo's pseudo content, looks up AssetBundle, applies CSS width/height.
Silent skip when asset missing. Part of fulgur-ai3 Phase 1."
```

---

### Task 6: convert_node で `::before` / `::after` (block) を emit する

Purpose: parent ノードを変換する際、`node.before` / `node.after` の pseudo slot を明示的にチェックし、block-display + content:Image の場合に BlockPageable を emit する。Phase 1 では inline / other display 値は無視(paragraph 側の既存挙動に委ねる)。

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (`collect_positioned_children` 付近)

**Step 1: Write the failing integration-style test**

`convert.rs` の tests モジュール末尾に追加:

```rust
#[test]
fn test_block_pseudo_image_is_emitted_as_child() {
    let icon_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/image/icon.png");
    let icon_bytes = std::fs::read(&icon_path).expect("read icon.png");
    let mut bundle = AssetBundle::new();
    bundle.add_image("icon.png".into(), std::sync::Arc::new(icon_bytes));

    let html = r#"<!doctype html><html><head><style>
        .wrap::before {
            content: url("icon.png");
            display: block;
            width: 20px; height: 20px;
        }
    </style></head><body><div class="wrap">hello</div></body></html>"#;

    let mut doc = crate::blitz_adapter::parse(html, 800.0, &[]);
    crate::blitz_adapter::resolve(&mut doc);

    let ctx_running = crate::gcpm::running::RunningElementStore::new();
    let mut ctx = ConvertContext {
        running_store: &ctx_running,
        assets: Some(&bundle),
        font_cache: std::collections::HashMap::new(),
        string_set_by_node: std::collections::HashMap::new(),
        counter_ops_by_node: std::collections::HashMap::new(),
    };
    let tree = super::dom_to_pageable(&doc, &mut ctx);

    // We only assert "something Image-shaped was emitted" via a recursive
    // visitor. The exact tree shape is implementation-defined, but any
    // ImagePageable must appear somewhere with width == 20, height == 20.
    let mut found = false;
    fn walk(p: &dyn Pageable, found: &mut bool) {
        if let Some(img) = p.as_any().downcast_ref::<ImagePageable>() {
            if img.width == 20.0 && img.height == 20.0 {
                *found = true;
                return;
            }
        }
        for child in p.debug_children() {
            walk(child, found);
            if *found { return; }
        }
    }
    walk(&*tree, &mut found);
    assert!(found, "Expected a 20x20 ImagePageable in the tree");
}
```

> **Implementer note:** The `Pageable` trait may not expose `as_any()` / `debug_children()` today. If not, you have two options:
>
> 1. Add small test-gated introspection helpers to `Pageable` (preferred: `#[cfg(test)] fn debug_children(&self) -> &[&dyn Pageable]` on each concrete type) and a downcast route.
> 2. Drive the test end-to-end by rendering to PDF bytes and checking that the PDF contains `/Image` or `/XObject` references. More brittle but requires no trait changes.
>
> Choose option 1 if feasible (additive, clearly test-only). If option 1 balloons into a large change, fall back to the end-to-end approach inside a `tests/pseudo_content_url.rs` integration test file at `crates/fulgur/tests/` level (not `src/`).

**Step 2: Run test to verify it fails**

Run: `cargo test -p fulgur test_block_pseudo_image_is_emitted`
Expected: FAIL — no image found in tree.

**Step 3: Implement — hook into collect_positioned_children**

Locate the spot in `convert.rs` where a node's regular `children` are walked to produce `PositionedChild`s (around line 272-347 — `collect_positioned_children`). Before and after the regular child loop, insert:

```rust
// --- ::before pseudo (block) ---
if let Some(before_id) = node.before {
    if let Some(pseudo_node) = doc.get_node(before_id) {
        if is_block_display(pseudo_node) {
            if let Some(img) = build_pseudo_image(
                pseudo_node,
                node.final_layout.size.width,
                ctx.assets,
            ) {
                // Position at top-left of the parent's content box.
                // Known limitation: subsequent children do not get shifted
                // down by the image's height — see fulgur-ai3 design doc.
                out.push(PositionedChild {
                    child: Box::new(img),
                    x: 0.0,
                    y: 0.0,
                });
            }
        }
    }
}

// ... existing child loop ...

// --- ::after pseudo (block) ---
if let Some(after_id) = node.after {
    if let Some(pseudo_node) = doc.get_node(after_id) {
        if is_block_display(pseudo_node) {
            if let Some(img) = build_pseudo_image(
                pseudo_node,
                node.final_layout.size.width,
                ctx.assets,
            ) {
                // Append at the bottom. Use parent's content-box height as y.
                let y = node.final_layout.size.height;
                out.push(PositionedChild {
                    child: Box::new(img),
                    x: 0.0,
                    y,
                });
            }
        }
    }
}
```

Helper:

```rust
fn is_block_display(node: &Node) -> bool {
    use style::properties::longhands::display::computed_value::T as Display;
    let styles = match node.primary_styles() {
        Some(s) => s,
        None => return false,
    };
    matches!(
        styles.clone_display(),
        // `Block`, `ListItem`, etc count as block outside. Be conservative
        // and only match plain Block for Phase 1 — list-item / table pseudos
        // are not in scope.
        Display::Block
    )
}
```

> **Implementer note on display matching:** Stylo's `Display` enum is rich. Confirm the exact variants by checking `stylo-0.8.0/properties/longhands/display.rs` or grep `clone_display` call sites in convert.rs. For Phase 1, matching only the literal `Block` variant is acceptable; broader matches (FlowRoot, etc.) can be added later if examples demand.

**Step 4: Run the test to verify it passes**

Run: `cargo test -p fulgur test_block_pseudo_image_is_emitted`
Expected: PASS

Run full crate tests: `cargo test -p fulgur --lib`
Expected: all PASS (no regressions).

**Step 5: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(convert): emit BlockPageable for ::before/::after content: url()

Hooks into collect_positioned_children. Only display:block is handled in
Phase 1; inline pseudos remain unhandled (follow-up: fulgur-ai3 Phase 2).
Known limitation: pseudos do not displace following children — users can
add margin to work around.

Part of fulgur-ai3 Phase 1."
```

---

### Task 7: `examples/pseudo-content-url/` を新設

**Files:**

- Create: `examples/pseudo-content-url/index.html`
- Create: `examples/pseudo-content-url/style.css`
- Create: `examples/pseudo-content-url/icon.png` (copy from `examples/image/icon.png`)
- Create: `examples/pseudo-content-url/.fontconfig/fonts.conf` (symlink or copy the existing shared config)
- Create: `examples/pseudo-content-url/.fonts/` (symlink or copy the existing Noto Sans bundle)

**Step 1: Scaffold the example**

Check how other examples organize shared fonts:

```bash
ls examples/image/.fonts 2>/dev/null || ls examples/.fonts 2>/dev/null
ls examples/image/.fontconfig 2>/dev/null || ls examples/.fontconfig 2>/dev/null
cat examples/image/mise.toml 2>/dev/null
find examples -name "README.md" -maxdepth 2
```

Follow the same layout as the other examples (determined by the output above — likely a shared `.fonts` / `.fontconfig` at the top level).

Create `index.html`:

```html
<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <title>pseudo content url — block</title>
  <link rel="stylesheet" href="style.css">
</head>
<body>
  <section class="chapter">
    <h2>Chapter 1</h2>
    <p>Body text follows the block-pseudo icon above.</p>
  </section>
  <section class="chapter">
    <h2>Chapter 2</h2>
    <p>Another chapter with a block pseudo icon.</p>
  </section>
</body>
</html>
```

Create `style.css`:

```css
/* Phase 1 only demonstrates display:block pseudo with content: url().
   Known limitation: the image does NOT displace subsequent content, so
   we reserve space manually via margin-top on the h2. Remove this
   workaround once Blitz layout round-trip lands (fulgur-XXX follow-up).
*/
.chapter h2::before {
  content: url("icon.png");
  display: block;
  width: 24px;
  height: 24px;
}
.chapter h2 {
  margin-top: 32px; /* reserve room for the 24px icon + padding */
}
```

Copy the icon:

```bash
cp examples/image/icon.png examples/pseudo-content-url/icon.png
```

**Step 2: Wire up fonts / fontconfig same as other examples**

Mirror the setup used by e.g. `examples/image/`. If fonts are shared at `examples/.fonts`, no per-example copy is needed. Verify by generating once manually:

```bash
FONTCONFIG_FILE=examples/.fontconfig/fonts.conf \
  cargo run --bin fulgur -- render \
    examples/pseudo-content-url/index.html \
    -o examples/pseudo-content-url/index.pdf
```

Expected: the PDF is generated without panics; manual inspection shows the icon in each chapter heading area.

**Step 3: Check in the generated PDF (for determinism test)**

```bash
git add examples/pseudo-content-url/
git commit -m "example(pseudo-content-url): add Phase 1 block pseudo case

Phase 1 of fulgur-ai3. display:block ::before with content: url(icon.png).
Baseline for examples_determinism. Known margin-top workaround documented
in style.css.
"
```

---

### Task 8: `examples_determinism.rs` に新 example を組み込む

**Files:**

- Modify: `crates/fulgur-cli/tests/examples_determinism.rs`

**Step 1: Read the existing harness**

```bash
cat crates/fulgur-cli/tests/examples_determinism.rs
```

The test likely iterates a list of example directories. Add `"pseudo-content-url"` to that list, or — if the harness auto-discovers by walking `examples/` — ensure the new directory is picked up and no explicit allowlist edit is needed.

**Step 2: Run the determinism test**

```bash
cargo test -p fulgur-cli --test examples_determinism
```

Expected: PASS (including the new example). The test regenerates the PDF and compares it byte-for-byte against the committed one.

**Step 3: Commit (if the harness needed a list edit)**

```bash
git add crates/fulgur-cli/tests/examples_determinism.rs
git commit -m "test(examples): add pseudo-content-url to determinism suite

fulgur-ai3 Phase 1."
```

---

### Task 9: Phase 1 の全体検証

**Step 1: Run the full workspace suite**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

Expected: all PASS.

**Step 2: Manual smoke**

```bash
FONTCONFIG_FILE=examples/.fontconfig/fonts.conf \
  cargo run --bin fulgur -- render \
    examples/pseudo-content-url/index.html \
    -o /tmp/smoke.pdf
```

Open `/tmp/smoke.pdf` in a viewer (or `pdftocairo /tmp/smoke.pdf -png /tmp/smoke`) and confirm the icon appears above each chapter heading.

**Step 3: Update markdownlint (if any markdown added)**

```bash
npx markdownlint-cli2 '**/*.md'
```

Expected: no errors. Fix any violations in the plan file or CLAUDE.md updates.

**Step 4: No commit — this is a gate, not a content change.**

---

## Phase 2: Inline-display pseudo element (sketch)

> Phase 2 is architecturally larger than Phase 1 because fulgur's current
> `extract_paragraph` consumes Parley's prebuilt inline layout, which does
> not naturally carry non-text runs. Before writing bite-sized tasks, an
> implementation spike is needed to pick between two routes:
>
> - **Route A — ShapedLine run enum extension:** Add an `Image(InlineImageRun)`
>   variant to `ShapedLine`'s content, and inject it as a post-processing
>   step after `extract_paragraph` returns. Requires synthesizing x-offsets
>   alongside Parley's glyph runs without re-running layout.
> - **Route B — U+FFFC shaping trick:** Before parsing, transform pseudo's
>   empty `content: url(img)` into a text node containing `U+FFFC` sized by
>   a sentinel font glyph, then overlay the image during draw. Simpler but
>   depends on font availability.
>
> The spike (~half day) should produce a decision memo on which route is
> more tractable, then Phase 2 tasks can be written. Tracking: open a
> follow-up beads issue `fulgur-ai3-phase2` blocked on Phase 1.

High-level Phase 2 task order (to be refined after the spike):

1. Spike: measure Parley's behavior on empty pseudo nodes; write a decision memo.
2. Extend `ShapedLine` / `ParagraphPageable` for mixed-content runs (if Route A).
3. Hook `build_pseudo_image` into `extract_paragraph` after-processing or a pre-layout DOM rewrite (if Route B).
4. Extend `examples/pseudo-content-url/` with an inline case (`li::before { content: url("bullet.png"); }`).
5. Re-baseline `examples_determinism`.
6. Manual smoke + full workspace test suite.

Phase 2 work should live in its own beads issue + worktree. Do not start it until Phase 1 is merged.

---

## Completion Checklist (Phase 1 only)

- [ ] Task 1 merged: `extract_pseudo_image_url` / `_alt` helpers (+2 tests)
- [ ] Task 2 merged: `ImagePageable.alt` field (+2 tests)
- [ ] Task 3 merged: `make_image_pageable` helper (+3 tests)
- [ ] Task 4 merged: `<img>` routed through `make_image_pageable` (no regression)
- [ ] Task 5 merged: `build_pseudo_image` + percentage length resolver (+2 tests)
- [ ] Task 6 merged: `collect_positioned_children` emits block pseudo images (+1 test)
- [ ] Task 7 merged: `examples/pseudo-content-url/` added
- [ ] Task 8 merged: `examples_determinism` covers the new example
- [ ] Task 9 gate: `cargo fmt / clippy / test --workspace` all green + manual PDF smoke
- [ ] beads issue `fulgur-ai3` notes updated with Phase 1 completion and Phase 2 follow-up issue link
