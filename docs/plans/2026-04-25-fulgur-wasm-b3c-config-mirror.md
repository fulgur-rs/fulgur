# fulgur-wasm B-3c config mirror Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** WASM `Engine` に `configure(options)` メソッドを追加し、JS の POJO で
`page_size` / `margin` / `landscape` / メタデータ / `bookmarks` を一括設定できるようにする。
`fulgur::EngineBuilder` のうち WASM で意味のある全フィールドを mirror（`base_path` /
`template` / `data` / `assets` は除外）。

**Architecture:** `Engine` struct に各 config フィールドを `Option<...>` / `Vec<...>` で持たせ、
`configure(JsValue)` で POJO を `serde_wasm_bindgen` 経由で `EngineOptions` に
deserialize → 内部状態に partial merge（複数回呼びを許す, 後勝ち）。`render()` 時に
`fulgur::Engine::builder()` へ流し込む。

**Tech Stack:** Rust 2024, `wasm-bindgen` 0.2, `serde` 1, `serde_wasm_bindgen` 0.6,
`fulgur` (workspace path dep), `wasm-pack` (browser bundle).

**beads issue:** fulgur-ufda (B-3c)
**worktree:** `/home/ubuntu/fulgur/.worktrees/fulgur-wasm-b3c-config-mirror`
**ブランチ:** `feat/fulgur-wasm-b3c-config-mirror`

---

## API 仕様（JS 側）

```javascript
const engine = new Engine();
engine.configure({
  pageSize: "A4",                    // または { widthMm: 210, heightMm: 297 }
  margin: { mm: 20 },                // または { pt: 72 } / { topMm, rightMm, bottomMm, leftMm }
  landscape: true,
  title: "Invoice",
  authors: ["Alice"],
  description: "Q1 invoice",
  keywords: ["invoice", "billing"],
  creator: "fulgur-wasm-demo",
  producer: "fulgur",
  creationDate: "D:20260425000000Z",
  lang: "ja",
  bookmarks: true,
});
engine.addFont(fontBytes);
engine.addCss(css);
const pdf = engine.render(html);
```

**バリデーション方針**:

- 不明な `pageSize` 文字列 (`"Foo"` 等) → `JsError`
- 負数 / NaN: `fulgur` 本体側でも検証していないので **WASM 側でも検証しない**（YAGNI）
- 未知のフィールド: `serde` の `deny_unknown_fields` で typo を弾く
- `null` / `undefined` フィールド: 既定で skip（`Option` の `None` のまま）

---

## Task 1: 失敗テストを書く（TDD red）

**Files:**

- Modify: `crates/fulgur-wasm/Cargo.toml` — `serde` + `serde_wasm_bindgen` を runtime dep に追加
- Modify: `crates/fulgur-wasm/src/lib.rs` — テスト追加

**Step 1: dependency を追加**

`crates/fulgur-wasm/Cargo.toml` の `[dependencies]` に追記:

```toml
serde = { version = "1", features = ["derive"] }
serde_wasm_bindgen = "0.6"
```

**Step 2: 失敗テストを書く**

`crates/fulgur-wasm/src/lib.rs` の `#[cfg(test)] mod tests` に追加:

```rust
#[test]
fn configure_applies_landscape_and_page_size() {
    // Letter landscape (792 x 612 pt) と A4 portrait (595 x 842 pt) で
    // PDF の MediaBox が異なることを検証する。
    let mut engine = Engine::new();
    engine
        .configure(
            serde_wasm_bindgen::to_value(&serde_json::json!({
                "pageSize": "Letter",
                "landscape": true,
            }))
            .unwrap(),
        )
        .expect("configure should succeed");
    let pdf = engine
        .render(r#"<div style="width:10px;height:10px"></div>"#)
        .expect("render should succeed");
    assert_eq!(&pdf[..4], b"%PDF");

    let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
    let media_box = find_media_box(&doc).expect("MediaBox missing");
    // Letter landscape = 792 x 612
    assert!(
        (media_box.0 - 792.0).abs() < 1.0 && (media_box.1 - 612.0).abs() < 1.0,
        "expected Letter landscape (792 x 612), got {:?}",
        media_box,
    );
}

#[test]
fn configure_applies_metadata() {
    // Info dictionary に title / author が反映されることを検証する。
    let mut engine = Engine::new();
    engine
        .configure(
            serde_wasm_bindgen::to_value(&serde_json::json!({
                "title": "B3C Test",
                "authors": ["Alice", "Bob"],
            }))
            .unwrap(),
        )
        .expect("configure should succeed");
    let pdf = engine
        .render("<p>x</p>")
        .expect("render should succeed");

    let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
    let info = find_info_string(&doc, b"Title").expect("Title missing");
    assert!(info.contains("B3C Test"), "Title was: {info:?}");
    let author = find_info_string(&doc, b"Author").expect("Author missing");
    assert!(author.contains("Alice"), "Author was: {author:?}");
}

#[test]
fn configure_custom_page_size_mm() {
    // pageSize に { widthMm, heightMm } object を渡せること。
    let mut engine = Engine::new();
    engine
        .configure(
            serde_wasm_bindgen::to_value(&serde_json::json!({
                "pageSize": { "widthMm": 100.0, "heightMm": 200.0 },
            }))
            .unwrap(),
        )
        .expect("configure should succeed");
    let pdf = engine.render("<p>x</p>").expect("render");
    let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
    let media_box = find_media_box(&doc).expect("MediaBox missing");
    // 100mm = 283.46 pt, 200mm = 566.93 pt
    assert!(
        (media_box.0 - 283.46).abs() < 1.0 && (media_box.1 - 566.93).abs() < 1.0,
        "expected ~283 x 567, got {:?}", media_box,
    );
}

#[test]
fn configure_rejects_unknown_page_size() {
    let mut engine = Engine::new();
    let result = engine.configure(
        serde_wasm_bindgen::to_value(&serde_json::json!({
            "pageSize": "Foo",
        }))
        .unwrap(),
    );
    assert!(result.is_err(), "unknown page size should be rejected");
}

#[test]
fn configure_rejects_unknown_field() {
    // typo を deny_unknown_fields で検出できることを保証する。
    let mut engine = Engine::new();
    let result = engine.configure(
        serde_wasm_bindgen::to_value(&serde_json::json!({
            "pageSizeTypo": "A4",
        }))
        .unwrap(),
    );
    assert!(result.is_err(), "unknown field should be rejected");
}

#[test]
fn configure_partial_merge_preserves_earlier_values() {
    // 2 回呼んで一部だけ上書き、他は残ることを検証する。
    let mut engine = Engine::new();
    engine.configure(
        serde_wasm_bindgen::to_value(&serde_json::json!({
            "title": "First",
            "landscape": true,
        }))
        .unwrap(),
    ).unwrap();
    engine.configure(
        serde_wasm_bindgen::to_value(&serde_json::json!({
            "title": "Second",
        }))
        .unwrap(),
    ).unwrap();
    let pdf = engine.render("<p>x</p>").expect("render");
    let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
    let title = find_info_string(&doc, b"Title").expect("Title missing");
    assert!(title.contains("Second"), "Title was: {title:?}");
    // landscape=true は維持されているはず
    let media_box = find_media_box(&doc).expect("MediaBox missing");
    assert!(
        media_box.0 > media_box.1,
        "expected landscape (w > h), got {:?}", media_box,
    );
}

// --- helpers (test-only) ---

fn find_media_box(doc: &lopdf::Document) -> Option<(f32, f32)> {
    for obj in doc.objects.values() {
        let lopdf::Object::Dictionary(dict) = obj else { continue };
        if dict.get(b"Type").and_then(|o| o.as_name()).ok() != Some(b"Page".as_slice()) {
            continue;
        }
        let mb = dict.get(b"MediaBox").ok()?.as_array().ok()?;
        if mb.len() == 4 {
            let w = mb[2].as_float().ok()?;
            let h = mb[3].as_float().ok()?;
            return Some((w, h));
        }
    }
    None
}

fn find_info_string(doc: &lopdf::Document, key: &[u8]) -> Option<String> {
    let info_ref = doc.trailer.get(b"Info").ok()?;
    let info = doc.dereference(info_ref).ok()?.1.as_dict().ok()?;
    let raw = info.get(key).ok()?;
    let bytes = raw.as_str().ok()?;
    Some(String::from_utf8_lossy(bytes).into_owned())
}
```

`dev-dependencies` に `serde_json` を追加（テストフィクスチャ用）:

```toml
[dev-dependencies]
lopdf = "0.40.0"
serde_json = "1"
```

**Step 3: テストが失敗することを確認**

```bash
cargo test -p fulgur-wasm 2>&1 | tail -30
```

Expected: `cannot find method named configure on Engine` 系の compile error。

**Step 4: コミット**

```bash
git add crates/fulgur-wasm/Cargo.toml crates/fulgur-wasm/src/lib.rs
git commit -m "test(fulgur-wasm): add failing tests for Engine.configure (B-3c)"
```

---

## Task 2: `EngineOptions` と `configure` を実装してテストを通す（TDD green）

**Files:**

- Modify: `crates/fulgur-wasm/src/lib.rs`

**Step 1: 実装**

ファイル冒頭の doc comment を B-3c 仕様に更新し、以下の追加を行う:

1. `use fulgur::{AssetBundle, Margin, PageSize};` に拡張
2. `EngineOptions` / `PageSizeOption` / `MarginOption` を private struct/enum として追加
3. `Engine` struct に config フィールドを追加
4. `configure` メソッドを `#[wasm_bindgen]` impl に追加
5. `render_impl` で builder に流し込む

最終的な `Engine` struct と関連コード:

```rust
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngineOptions {
    #[serde(default)]
    page_size: Option<PageSizeOption>,
    #[serde(default)]
    margin: Option<MarginOption>,
    #[serde(default)]
    landscape: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    authors: Option<Vec<String>>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    keywords: Option<Vec<String>>,
    #[serde(default)]
    creator: Option<String>,
    #[serde(default)]
    producer: Option<String>,
    #[serde(default)]
    creation_date: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    bookmarks: Option<bool>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", untagged)]
enum PageSizeOption {
    Named(String),
    Custom { width_mm: f32, height_mm: f32 },
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", untagged)]
enum MarginOption {
    Mm { mm: f32 },
    Pt { pt: f32 },
    Full {
        top_mm: f32,
        right_mm: f32,
        bottom_mm: f32,
        left_mm: f32,
    },
}

impl PageSizeOption {
    fn to_page_size(&self) -> Result<PageSize, String> {
        match self {
            Self::Named(name) => match name.to_ascii_lowercase().as_str() {
                "a4" => Ok(PageSize::A4),
                "a3" => Ok(PageSize::A3),
                "letter" => Ok(PageSize::LETTER),
                other => Err(format!("unknown page size: {other}")),
            },
            Self::Custom { width_mm, height_mm } => {
                Ok(PageSize::custom(*width_mm, *height_mm))
            }
        }
    }
}

impl MarginOption {
    fn to_margin(&self) -> Margin {
        match self {
            Self::Mm { mm } => Margin::uniform_mm(*mm),
            Self::Pt { pt } => Margin::uniform(*pt),
            Self::Full { top_mm, right_mm, bottom_mm, left_mm } => {
                let to_pt = |mm: f32| mm * 72.0 / 25.4;
                Margin {
                    top: to_pt(*top_mm),
                    right: to_pt(*right_mm),
                    bottom: to_pt(*bottom_mm),
                    left: to_pt(*left_mm),
                }
            }
        }
    }
}

#[wasm_bindgen]
pub struct Engine {
    assets: AssetBundle,
    page_size: Option<PageSize>,
    margin: Option<Margin>,
    landscape: Option<bool>,
    title: Option<String>,
    authors: Vec<String>,
    description: Option<String>,
    keywords: Vec<String>,
    creator: Option<String>,
    producer: Option<String>,
    creation_date: Option<String>,
    lang: Option<String>,
    bookmarks: Option<bool>,
}
```

`render_impl` を更新:

```rust
fn render_impl(&self, html: &str) -> fulgur::Result<Vec<u8>> {
    let mut builder = fulgur::Engine::builder().assets(self.assets.clone());
    if let Some(s) = self.page_size { builder = builder.page_size(s); }
    if let Some(m) = self.margin { builder = builder.margin(m); }
    if let Some(l) = self.landscape { builder = builder.landscape(l); }
    if let Some(ref t) = self.title { builder = builder.title(t.clone()); }
    if !self.authors.is_empty() { builder = builder.authors(self.authors.clone()); }
    if let Some(ref d) = self.description { builder = builder.description(d.clone()); }
    if !self.keywords.is_empty() { builder = builder.keywords(self.keywords.clone()); }
    if let Some(ref c) = self.creator { builder = builder.creator(c.clone()); }
    if let Some(ref p) = self.producer { builder = builder.producer(p.clone()); }
    if let Some(ref cd) = self.creation_date { builder = builder.creation_date(cd.clone()); }
    if let Some(ref l) = self.lang { builder = builder.lang(l.clone()); }
    if let Some(b) = self.bookmarks { builder = builder.bookmarks(b); }
    builder.build().render_html(html)
}
```

`configure` を `#[wasm_bindgen] impl Engine` に追加:

```rust
/// Apply configuration options from a JS object (B-3c).
///
/// 受け付けるキーは `pageSize` / `margin` / `landscape` / `title` /
/// `authors` / `description` / `keywords` / `creator` / `producer` /
/// `creationDate` / `lang` / `bookmarks`。未知のキーや不正な値はエラー。
/// 複数回呼び出すと後勝ちで partial merge される。
pub fn configure(&mut self, options: JsValue) -> Result<(), JsError> {
    let opts: EngineOptions = serde_wasm_bindgen::from_value(options)
        .map_err(|e| JsError::new(&format!("invalid options: {e}")))?;
    self.apply_options(opts).map_err(|e| JsError::new(&e))
}
```

private apply method:

```rust
impl Engine {
    fn apply_options(&mut self, opts: EngineOptions) -> Result<(), String> {
        if let Some(ps) = opts.page_size {
            self.page_size = Some(ps.to_page_size()?);
        }
        if let Some(m) = opts.margin {
            self.margin = Some(m.to_margin());
        }
        if let Some(l) = opts.landscape { self.landscape = Some(l); }
        if let Some(t) = opts.title { self.title = Some(t); }
        if let Some(a) = opts.authors { self.authors = a; }
        if let Some(d) = opts.description { self.description = Some(d); }
        if let Some(k) = opts.keywords { self.keywords = k; }
        if let Some(c) = opts.creator { self.creator = Some(c); }
        if let Some(p) = opts.producer { self.producer = Some(p); }
        if let Some(cd) = opts.creation_date { self.creation_date = Some(cd); }
        if let Some(l) = opts.lang { self.lang = Some(l); }
        if let Some(b) = opts.bookmarks { self.bookmarks = Some(b); }
        Ok(())
    }
}
```

constructor `new()` のフィールド初期化を更新:

```rust
pub fn new() -> Self {
    Self {
        assets: AssetBundle::new(),
        page_size: None,
        margin: None,
        landscape: None,
        title: None,
        authors: Vec::new(),
        description: None,
        keywords: Vec::new(),
        creator: None,
        producer: None,
        creation_date: None,
        lang: None,
        bookmarks: None,
    }
}
```

**Step 2: テストが通ることを確認**

```bash
cargo test -p fulgur-wasm 2>&1 | tail -20
```

Expected: 9 passed (既存 4 + 新規 5)。

**Step 3: wasm32 ターゲットで build できることを確認**

```bash
cargo build -p fulgur-wasm --target wasm32-unknown-unknown 2>&1 | tail -10
```

Expected: `Finished ...`

**Step 4: コミット**

```bash
git add crates/fulgur-wasm/src/lib.rs
git commit -m "feat(fulgur-wasm): add Engine.configure with POJO options (B-3c)"
```

---

## Task 3: demo HTML に config UI を追加

**Files:**

- Modify: `examples/wasm-demo/index.html`

**Step 1: HTML を更新**

`<form>` 風セクションを追加してページサイズ選択 / orientation toggle / title 入力を可能にする。`engine.configure(...)` を render 直前に呼ぶ。詳細は実装時に調整するが、最低限以下を提供:

- `<select id="page-size">` で A4 / Letter / A3 を選べる
- `<input type="checkbox" id="landscape">` で landscape 切替
- `<input type="text" id="title">` で title 入力

クリックハンドラ内で:

```javascript
engine.configure({
  pageSize: pageSizeEl.value,
  landscape: landscapeEl.checked,
  title: titleEl.value || undefined,
});
const bytes = engine.render(html);
```

**Step 2: README を B-3c 仕様に更新**

`examples/wasm-demo/README.md` の Scope セクションを B-3c 用に書き換え、Tracking に
`fulgur-ufda (B-3c)` を追加。markdownlint をかける:

```bash
npx markdownlint-cli2 'examples/wasm-demo/README.md' 2>&1 | tail -5
```

**Step 3: コミット**

```bash
git add examples/wasm-demo/index.html examples/wasm-demo/README.md
git commit -m "feat(wasm-demo): expose page size / orientation / title via configure (B-3c)"
```

---

## Task 4: lint / fmt / clippy / wasm32 build

**Files:** なし（検証のみ）

```bash
cargo fmt --check
cargo clippy -p fulgur-wasm --all-targets -- -D warnings
cargo test -p fulgur-wasm
cargo test -p fulgur --lib                                       # regression check
cargo build -p fulgur-wasm --target wasm32-unknown-unknown --release
```

修正があれば `style(fulgur-wasm): cargo fmt` でコミット。

---

## Task 5: wasm-pack ビルドと demo 視覚検証

```bash
wasm-pack build crates/fulgur-wasm --target web --dev \
  --out-dir ../../examples/wasm-demo/pkg
grep -E "configure|export class Engine" examples/wasm-demo/pkg/fulgur_wasm.js | head -5
```

ブラウザでの視覚検証は user に依頼:

```bash
cd examples/wasm-demo && python3 -m http.server 8000
# - ページサイズ選択 / landscape toggle / title 入力 UI が表示される
# - "Render PDF" を押した PDF を開いて (a) ページサイズが選択値どおり,
#   (b) PDF properties に title が反映されている, ことを確認
```

---

## Task 6: 完了確認とブランチ仕上げ

- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy -p fulgur-wasm --all-targets -- -D warnings` clean
- [ ] `cargo test -p fulgur-wasm` 9 tests passed
- [ ] `cargo test -p fulgur --lib` 既存数と同じ pass
- [ ] `cargo build -p fulgur-wasm --target wasm32-unknown-unknown --release` 成功
- [ ] `wasm-pack build ... --dev` 成功
- [ ] ブラウザ視覚確認

その後 superpowers:finishing-a-development-branch で PR を作る（PR body は日本語）→
`bd close fulgur-ufda` → `bd sync --flush-only`。

---

## Notes / 落とし穴

- **`deny_unknown_fields` と `serde_wasm_bindgen`**: serde_wasm_bindgen は JS object →
  Rust struct の deserialize 時に未知フィールドを検出できる。typo 防止用に有効化。
- **`untagged` enum の優先順**: `MarginOption` で `{ mm: 20 }` と `{ pt: 72 }` を区別する
  必要がある。`untagged` は上から順に match するので、より具体的なケースを先に書く。
  `Mm { mm }` → `Pt { pt }` → `Full { top_mm, ... }` の順。
- **`PageSize::custom` は mm**: `width_mm` / `height_mm` をそのまま渡せば内部で pt 変換される
  (`config.rs:22`)。
- **B-2 / B-3a の API は不変**: `add_font` / `add_css` / `add_image` / `render` は触らない。
  既存テスト 4 個も壊れないこと。
- **`producer` を上書きしないでも fulgur 既定値 `"fulgur"` が入る**: `Config::default()` 参照。
  ユーザーが上書きしたい時だけ `producer: "..."` を渡せば良い。
- **bundle size**: serde_wasm_bindgen は数 KB の追加コード。許容範囲。bundle 最適化は B-3γ
  (別 step) で。
