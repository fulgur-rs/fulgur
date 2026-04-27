# fulgur-wasm B-2 font bridge Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `crates/fulgur-wasm` に Engine builder ミラーを追加し、JS から `Uint8Array` でフォントを渡して text を含む HTML を PDF にレンダリングできるようにする。

**Architecture:** `#[wasm_bindgen] pub struct Engine` を expose し、内部に `fulgur::AssetBundle` を保持する。`add_font(bytes)` で `AssetBundle::add_font_bytes` に委譲（family 引数は不要、TTF name table から自動取得）、`render(html)` で `fulgur::Engine::builder().assets(...).build().render_html(...)` を呼ぶ。B-1 後方互換のため既存スタンドアロン関数 `render_html(html)` は残す。native target で `cargo test` できるように `_impl` 内部関数を切り出して `wasm-bindgen` メソッドはその thin wrapper にする。

**Tech Stack:** Rust 2024, `wasm-bindgen` 0.2, `fulgur` (workspace path dep), `wasm-pack` (browser bundle), Noto Sans Regular TTF (`examples/.fonts/`).

**beads issue:** fulgur-7js9 (B-2) — design field 参照
**worktree:** `/home/ubuntu/fulgur/.worktrees/fulgur-wasm-b2-font-bridge`
**ブランチ:** `feat/fulgur-wasm-b2-font-bridge`

---

## Task 1: native target で動く失敗テストを書く

**Files:**

- Modify: `crates/fulgur-wasm/Cargo.toml` — dev-dep に `lopdf` を追加
- Create: `crates/fulgur-wasm/src/lib.rs` 末尾に `#[cfg(test)] mod tests`

**Step 1: dev-dependency を追加**

`crates/fulgur-wasm/Cargo.toml` の末尾に追記:

```toml
[dev-dependencies]
lopdf = "0.40.0"
```

**Step 2: 失敗テストを書く**

`crates/fulgur-wasm/src/lib.rs` の末尾に追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn noto_sans_regular() -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/.fonts/NotoSans-Regular.ttf"
        );
        std::fs::read(path).expect("Noto Sans Regular fixture")
    }

    #[test]
    fn engine_new_renders_text_with_added_font() {
        let mut engine = Engine::new();
        engine
            .add_font(noto_sans_regular())
            .expect("add_font should accept TTF bytes");
        let pdf = engine
            .render("<h1>Hello World</h1>")
            .expect("render should succeed");
        assert_eq!(&pdf[..4], b"%PDF", "PDF magic missing");

        // 「text が描画されたこと」を強く保証するため lopdf で text を抽出して検査。
        // krilla は ToUnicode CMap を生成するので extract_text で復元できる
        // (fulgur 本体の `inspect::extract_text_items` も同じ前提に立つ)。
        let doc = lopdf::Document::load_mem(&pdf).expect("PDF parses");
        let text = doc.extract_text(&[1]).expect("page 1 text extracts");
        assert!(
            text.contains("Hello"),
            "expected 'Hello' in extracted text, got: {text:?}"
        );
        assert!(
            text.contains("World"),
            "expected 'World' in extracted text, got: {text:?}"
        );
    }

    #[test]
    fn render_html_standalone_still_works() {
        // B-1 後方互換: フォント不要の HTML はスタンドアロン関数で動く。
        let pdf = render_html(r#"<div style="background:red; width:100px; height:100px"></div>"#)
            .expect("render_html should succeed");
        assert_eq!(&pdf[..4], b"%PDF");
    }
}
```

**Step 3: テストが失敗することを確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-wasm-b2-font-bridge
cargo test -p fulgur-wasm 2>&1 | tail -20
```

Expected: コンパイルエラー `cannot find struct Engine in scope` (まだ作っていない)。

**Step 4: コミット**

```bash
git add crates/fulgur-wasm/Cargo.toml crates/fulgur-wasm/src/lib.rs
git commit -m "test(fulgur-wasm): add failing test for Engine builder mirror"
```

---

## Task 2: `Engine` struct と最小実装を追加してテストを通す

**Files:**

- Modify: `crates/fulgur-wasm/src/lib.rs`

**Step 1: 実装を追加**

ファイルヘッダの doc コメントを B-2 仕様に更新し、`Engine` struct と impl block を追加する。最終的な `crates/fulgur-wasm/src/lib.rs` の構造:

```rust
//! WebAssembly bindings for fulgur.
//!
//! This crate exposes two entry points:
//!
//! 1. [`render_html`] (B-1 compatible) — single-shot, no fonts/CSS/images.
//! 2. [`Engine`] (B-2) — builder mirror with `add_font` for registering
//!    `Uint8Array` font payloads (TTF / OTF / WOFF2). WOFF2 is auto-decoded
//!    by `fulgur::AssetBundle::add_font_bytes`; WOFF1 is rejected.
//!
//! Browser-class targets (`wasm32-unknown-unknown`) only.
//!
//! Tracking: fulgur-iym (strategic v0.7.0), fulgur-7js9 (this step, B-2).

use fulgur::AssetBundle;
use wasm_bindgen::prelude::*;

/// Render the given HTML string to a PDF byte array (B-1 compatible).
///
/// Equivalent to `Engine::new().render(html)`. Kept for back-compat with
/// callers built against the B-1 API; new code should use [`Engine`].
#[wasm_bindgen]
pub fn render_html(html: &str) -> Result<Vec<u8>, JsError> {
    Engine::new().render(html)
}

/// Builder-style engine that mirrors `fulgur::Engine`'s configuration
/// surface for the WASM target.
#[wasm_bindgen]
pub struct Engine {
    assets: AssetBundle,
}

impl Engine {
    fn add_font_impl(&mut self, bytes: Vec<u8>) -> fulgur::Result<()> {
        self.assets.add_font_bytes(bytes)
    }

    fn render_impl(&self, html: &str) -> fulgur::Result<Vec<u8>> {
        fulgur::Engine::builder()
            .assets(self.assets.clone())
            .build()
            .render_html(html)
    }
}

#[wasm_bindgen]
impl Engine {
    /// Create a new engine with no registered assets.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            assets: AssetBundle::new(),
        }
    }

    /// Register a font from raw bytes (TTF / OTF / WOFF2).
    ///
    /// `wasm-bindgen` accepts a `Uint8Array` from JS for the `bytes`
    /// parameter. WOFF2 is decoded to TTF in-process; WOFF1 is rejected.
    /// Family name is extracted from the font's `name` table — no
    /// `family` argument is needed.
    pub fn add_font(&mut self, bytes: Vec<u8>) -> Result<(), JsError> {
        self.add_font_impl(bytes)
            .map_err(|e| JsError::new(&format!("{e}")))
    }

    /// Render the given HTML string to a PDF byte array.
    pub fn render(&self, html: &str) -> Result<Vec<u8>, JsError> {
        self.render_impl(html)
            .map_err(|e| JsError::new(&format!("{e}")))
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
```

既存の `render_html` 実装は新版に置き換える（B-1 同等の振る舞いは `Engine::new().render(html)` で再現される）。テストモジュールは Task 1 のものをそのまま残す。

**Step 2: テストが通ることを確認**

```bash
cargo test -p fulgur-wasm 2>&1 | tail -20
```

Expected: 2 passed.

**Step 3: wasm32 ターゲットでも build できることを確認**

```bash
cargo build -p fulgur-wasm --target wasm32-unknown-unknown 2>&1 | tail -10
```

Expected: `Finished ...` (warning は OK だが error は無し)。

**Step 4: コミット**

```bash
git add crates/fulgur-wasm/src/lib.rs
git commit -m "feat(fulgur-wasm): add Engine builder mirror with add_font"
```

---

## Task 3: demo HTML を Engine API + フォントロードに更新

**Files:**

- Modify: `examples/wasm-demo/index.html`

**Step 1: HTML を書き換える**

`examples/wasm-demo/index.html` の以下を変更:

1. `<p class="note">` の説明を B-2 (font bridge) に書き換え
2. `<textarea id="html">` のデフォルト値を `<h1>Hello World</h1>` に
3. `import` 文を `import init, { Engine } from "./pkg/fulgur_wasm.js";` に
4. WASM 初期化後にフォントを fetch して `Engine` インスタンスに登録、その engine をクリックハンドラで使う

最終的な `<script>` ブロック:

```html
<script type="module">
  import init, { Engine } from "./pkg/fulgur_wasm.js";

  const button = document.getElementById("render");
  const statusEl = document.getElementById("status");

  function setStatus(text, kind) {
    statusEl.textContent = text;
    statusEl.className = kind ?? "";
  }

  let engine;

  try {
    await init();
    setStatus("Loading font…", "");
    const fontResponse = await fetch("../.fonts/NotoSans-Regular.ttf");
    if (!fontResponse.ok) {
      throw new Error(`Font fetch failed: ${fontResponse.status}`);
    }
    const fontBytes = new Uint8Array(await fontResponse.arrayBuffer());
    engine = new Engine();
    engine.add_font(fontBytes);
    button.disabled = false;
    button.textContent = "Render PDF";
    setStatus("WASM ready (Noto Sans Regular registered).", "ok");
  } catch (e) {
    setStatus(`Failed to initialise: ${e}`, "error");
    throw e;
  }

  button.addEventListener("click", () => {
    const html = document.getElementById("html").value;
    const t0 = performance.now();
    try {
      const bytes = engine.render(html);
      const elapsed = (performance.now() - t0).toFixed(1);
      const blob = new Blob([bytes], { type: "application/pdf" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "output.pdf";
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 0);
      setStatus(`Rendered ${bytes.length} bytes in ${elapsed} ms.`, "ok");
    } catch (e) {
      setStatus(`Render failed: ${e}`, "error");
    }
  });
</script>
```

textarea のデフォルトを `<h1>Hello World</h1>` に置き換える:

```html
<textarea id="html"><h1>Hello World</h1></textarea>
```

`<p class="note">` を:

```html
<p class="note">
  Browser-side HTML &rarr; PDF rendering, no server required.
  This is the B-2 step (font bridge) &mdash; Noto Sans Regular is
  fetched from <code>../.fonts/</code> and registered via
  <code>Engine.add_font</code>. CSS resources, images, and richer
  options follow in B-3.
</p>
```

**Step 2: コミット**

```bash
git add examples/wasm-demo/index.html
git commit -m "feat(wasm-demo): switch to Engine API + Noto Sans font load"
```

---

## Task 4: demo README を B-2 仕様に更新

**Files:**

- Modify: `examples/wasm-demo/README.md`

**Step 1: README を書き換える**

`examples/wasm-demo/README.md` の `## Scope (B-1)` セクションと `## Tracking` を B-2 ベースに書き換える。

新しい `## Scope` セクション:

```markdown
## Scope (B-2)

- `Engine` builder mirror with `new()`, `add_font(bytes)`, `render(html)`.
- Default sample HTML is `<h1>Hello World</h1>`. The demo fetches
  `../.fonts/NotoSans-Regular.ttf` and registers it via
  `engine.add_font(bytes)` before enabling the Render button.
- B-1 standalone `render_html(html)` entry point is preserved for
  callers that don't need fonts.
- CSS resources, images, page-size / metadata options, CJK fallback,
  and bundle-size optimisation are out of scope here — see B-3.
```

`## Tracking` の最後の bullet に B-2 を追加:

```markdown
## Tracking

- `fulgur-iym` (strategic v0.7.0) — overall WASM bet
- `fulgur-id9x` (closed) — B-1: bare wasm-bindgen wrapper
- `fulgur-7js9` (this step) — B-2: font bridge via `AssetBundle::add_font_bytes`
- `crates/fulgur/CLAUDE.md` ※memory `project_wasm_resource_bridging.md` —
  scope 1 / 3a / 3b stage design
```

ビルドコマンドは B-1 と同じだが、demo を起動した直後にフォントが fetch されることを Notes に追記:

```markdown
- The demo fetches `../.fonts/NotoSans-Regular.ttf` over the static
  server at startup. If you serve the demo directory on its own (not
  from the repo root), copy the TTF next to `index.html` or adjust the
  `fetch` URL.
```

**Step 2: markdownlint を通す**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-wasm-b2-font-bridge
npx markdownlint-cli2 'examples/wasm-demo/README.md' 2>&1 | tail -10
```

Expected: no findings。

**Step 3: コミット**

```bash
git add examples/wasm-demo/README.md
git commit -m "docs(wasm-demo): describe B-2 font bridge"
```

---

## Task 5: lint / fmt / ワークスペース横断テスト

**Files:** なし（検証のみ）

**Step 1: fmt**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-wasm-b2-font-bridge
cargo fmt --check 2>&1 | tail -5
```

Expected: 出力なし (clean)。失敗したら `cargo fmt` で fix し、コミットする。

**Step 2: clippy on fulgur-wasm**

```bash
cargo clippy -p fulgur-wasm --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: warning なし。

**Step 3: fulgur-wasm の test (再実行)**

```bash
cargo test -p fulgur-wasm 2>&1 | tail -10
```

Expected: 2 passed.

**Step 4: fulgur 本体への regression がないことを確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 既存テストが全て pass (この変更で fulgur 本体は触らないので影響無いはず)。

**Step 5: wasm32 ターゲットの release build**

```bash
cargo build -p fulgur-wasm --target wasm32-unknown-unknown --release 2>&1 | tail -10
```

Expected: `Finished release ...`。

**Step 6: もし fmt 修正があった場合のみコミット**

```bash
git add -u
git commit -m "style(fulgur-wasm): cargo fmt"
```

(無ければスキップ)

---

## Task 6: wasm-pack ビルドと demo 視覚検証

**Files:** なし（生成物 `examples/wasm-demo/pkg/` は `.gitignore` 済みのため commit しない）

**Step 1: wasm-pack で demo bundle を生成**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-wasm-b2-font-bridge
wasm-pack build crates/fulgur-wasm --target web --dev \
  --out-dir ../../examples/wasm-demo/pkg 2>&1 | tail -20
```

Expected: `[INFO]: ✨   Done` 系の成功メッセージ。`examples/wasm-demo/pkg/fulgur_wasm.js` と `..._bg.wasm` が生成される。

**Step 2: 生成された JS が `Engine` を export しているか確認**

```bash
grep -E "export class Engine|export function render_html" \
  examples/wasm-demo/pkg/fulgur_wasm.js | head -5
```

Expected: `Engine` クラスと `render_html` 関数の両方が見える。

**Step 3: ブラウザ視覚検証 (USER 側で実施)**

agent はブラウザを起動できないため、以下を **完了レポートで user に依頼** する:

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-wasm-b2-font-bridge/examples/wasm-demo
python3 -m http.server 8000
# 別タブで http://localhost:8000/ を開く
# - "WASM ready (Noto Sans Regular registered)." が出ること
# - textarea に <h1>Hello World</h1> が入っていること
# - "Render PDF" を押すと output.pdf がダウンロードされ、
#   開いた PDF に "Hello World" が見えること
```

成功条件: PDF を開いて視覚で "Hello World" が読めること。

**注**: Task 1 で導入した lopdf-based smoke test が「text が PDF に正しく載る」を agent 側でも検証する。視覚確認は user のみが実行可能な最終チェック。

**Step 4: コミット不要**

`pkg/` は gitignore 済み (B-1 で確認済み)。視覚検証の結果はこの後 PR 本文に書く。

---

## Task 7: 完了確認とブランチ仕上げ

**Step 1: superpowers:verification-before-completion を実行**

最終的に以下が全てクリアであることを確認:

- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy -p fulgur-wasm --all-targets -- -D warnings` clean
- [ ] `cargo test -p fulgur-wasm` 2 tests passed
- [ ] `cargo test -p fulgur --lib` 既存数 (~340) と同じ pass 数
- [ ] `cargo build -p fulgur-wasm --target wasm32-unknown-unknown --release` 成功
- [ ] `wasm-pack build ... --dev` 成功
- [ ] ブラウザ視覚確認で "Hello World" が PDF に出る

**Step 2: superpowers:finishing-a-development-branch でブランチをまとめる**

- diff 全体を `git log feat/fulgur-wasm-b2-font-bridge ^main --oneline` でレビュー
- PR を作るかマージするか judgement
- PR body は日本語で（memory: feedback_pr_body_japanese）

**Step 3: beads issue close**

```bash
bd close fulgur-7js9
bd sync --flush-only
```

---

## Notes / 落とし穴 (実装中の参照用)

- **Blitz inline SVG fontdb は WASM で空**: `<svg><text>` 系は本 step では描画されない可能性が高い。HTML body の `<h1>`/`<p>` のみが対象。
- **parley system font fallback も WASM で空**: AssetBundle で渡したフォントだけが使える。Noto Sans Regular 1 種で足りる範囲のテストにとどめる。
- **`Engine` の名前衝突**: wasm crate 側 `pub struct Engine` と fulgur 本体の `fulgur::Engine` は別 crate なので `use fulgur::AssetBundle;` のみ import し、`fulgur::Engine::builder()` はフルパスで呼ぶ（モジュール外スコープから一意）。
- **`cargo test -p fulgur-wasm` が走る理由**: `#[wasm_bindgen]` は `target_arch = "wasm32"` でない場合は no-op になり、`#[wasm_bindgen]` 付きの fn は普通の Rust 関数として native compile される。`JsError::new` も native target で動く（内部的に `JsValue` ラッパーを介すが native では string holder）。
- **`Vec<u8>` の wasm 越境コピー**: B-2 では受容するコスト。B-3 で zero-copy / streaming の最適化検討予定。
- **後方互換**: B-1 でリリースされた `render_html(html)` シグネチャは変えない。内部実装が `Engine::new().render(html)` に変わるだけ。
