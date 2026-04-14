# @media print 対応 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** fulgur が生成する PDF に `@media print { ... }` / `<link rel=stylesheet media=print>` CSS ルールを適用する。fulgur は PDF 生成専用なので media type は常に print 固定。

**Architecture:** `blitz-dom` の `make_device` が `MediaType::screen()` をハードコードしているため upstream を改修する必要がある。フォーク `mitsuru/blitz:set-media-type-v0.2.x` に `DocumentConfig::media_type` と `BaseDocument::set_media_type()` API を追加済み。fulgur は `[patch.crates-io]` でフォークを指し、`parse_inner` で `media_type = Print` を設定する。

**Tech Stack:** Rust, Blitz (forked), stylo, Krilla, tempfile, image, pdftocairo (poppler-utils)

**Tracking issues:**

- `fulgur-70v` — fulgur 側の実装（このプラン）
- `fulgur-j4y` — Blitz upstream PR tracking（並行・マージされたら patch 解除）

---

## Task 1: Workspace Cargo.toml に `[patch.crates-io]` を追加

**Files:**

- Modify: `Cargo.toml`（workspace root）

**Step 1: `[patch.crates-io]` セクションを追加**

workspace root `Cargo.toml` の末尾に以下を追記する。

```toml
[patch.crates-io]
blitz-dom = { git = "https://github.com/mitsuru/blitz", branch = "set-media-type-v0.2.x" }
blitz-traits = { git = "https://github.com/mitsuru/blitz", branch = "set-media-type-v0.2.x" }
blitz-html = { git = "https://github.com/mitsuru/blitz", branch = "set-media-type-v0.2.x" }
stylo_taffy = { git = "https://github.com/mitsuru/blitz", branch = "set-media-type-v0.2.x" }
```

**注意**: `blitz-dom` だけ patch すると `blitz_traits` が crates.io 版と fork 版で型二重化し、build error になる。4 crate すべて必須。

**Step 2: lockfile 更新と build 確認**

Run: `cargo update && cargo build`
Expected: 全 blitz 系 crate が fork branch から解決され、build 成功。

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: patch blitz crates with set-media-type fork"
```

---

## Task 2: 失敗する統合テストを先に書く（TDD）

**Files:**

- Create: `crates/fulgur/tests/media_type_print.rs`

**Step 1: テストファイルを作成**

以下を新規作成。`link_media_attribute.rs` の `render_contains_red` と同じパターンを使う（pdftocairo で1ページ目を PNG 化し赤ピクセル検出）。

```rust
//! `@media print` ルールが fulgur 生成 PDF に適用されることを確認する
//! 統合テスト。fulgur は PDF 生成専用であり、常に print media として
//! レンダリングされる。

use std::fs;
use std::path::Path;
use std::process::Command;

use fulgur::{Engine, PageSize};
use tempfile::tempdir;

fn render_contains_red(html: &str, base: &Path) -> Option<bool> {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .base_path(base.to_path_buf())
        .build();
    let pdf = engine.render_html(html).expect("render must succeed");

    let work = tempdir().unwrap();
    let pdf_path = work.path().join("fixture.pdf");
    fs::write(&pdf_path, &pdf).unwrap();

    let prefix = work.path().join("page");
    let status = match Command::new("pdftocairo")
        .args(["-png", "-r", "100", "-f", "1", "-l", "1"])
        .arg(&pdf_path)
        .arg(&prefix)
        .status()
    {
        Ok(s) => s,
        Err(_) => return None,
    };
    assert!(status.success(), "pdftocairo failed");

    let png_path = work.path().join("page-1.png");
    let img = image::open(&png_path).expect("decode PNG").to_rgba8();
    Some(
        img.pixels()
            .any(|p| p[0] > 200 && p[1] < 60 && p[2] < 60 && p[3] > 0),
    )
}

#[test]
fn inline_media_print_applies() {
    let dir = tempdir().unwrap();
    let html = r#"
        <!DOCTYPE html>
        <html><head><style>
            body { background: white; }
            @media print { body { background: red; } }
        </style></head><body><p>hi</p></body></html>
    "#;

    let result = match render_contains_red(html, dir.path()) {
        Some(v) => v,
        None => return,
    };
    assert!(
        result,
        "@media print rules must apply to fulgur's PDF output"
    );
}

#[test]
fn inline_media_screen_does_not_apply() {
    let dir = tempdir().unwrap();
    let html = r#"
        <!DOCTYPE html>
        <html><head><style>
            body { background: white; }
            @media screen { body { background: red; } }
        </style></head><body><p>hi</p></body></html>
    "#;

    let result = match render_contains_red(html, dir.path()) {
        Some(v) => v,
        None => return,
    };
    assert!(
        !result,
        "@media screen rules must NOT apply to fulgur's print-mode PDF output"
    );
}
```

**Step 2: テストが FAIL することを確認**

Run: `cargo test -p fulgur --test media_type_print`
Expected: `inline_media_print_applies` が FAIL（current media = screen のため）、`inline_media_screen_does_not_apply` は逆に PASS してしまうので両方 FAIL / unexpected-PASS 状態。

**注**: poppler-utils がない環境では None 返却で skip される。CI 環境で poppler-utils が入っていることを前提。

**Step 3: Commit**

```bash
git add crates/fulgur/tests/media_type_print.rs
git commit -m "test(media-print): add failing tests for @media print/screen"
```

---

## Task 3: `parse_inner` で `media_type = Print` を設定

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`（import セクション + `parse_inner`）

**Step 1: `MediaType` を import**

`blitz_adapter.rs` の既存 import ブロック（`use blitz_html::HtmlDocument;` / `use blitz_traits::shell::...;` 付近）に追加:

```rust
use blitz_dom::MediaType;
```

**Step 2: `parse_inner` の `DocumentConfig` に `media_type` を追加**

`parse_inner`（`crates/fulgur/src/blitz_adapter.rs:161-190`）の `DocumentConfig` リテラルを修正:

```rust
let config = DocumentConfig {
    viewport: Some(viewport),
    font_ctx,
    base_url: Some(base_url.unwrap_or_else(|| "file:///".to_string())),
    net_provider,
    media_type: Some(MediaType::print()),
    ..DocumentConfig::default()
};
```

**Step 3: build 確認**

Run: `cargo build -p fulgur`
Expected: 警告なく build 成功。

**Step 4: Task 2 のテストが PASS することを確認**

Run: `cargo test -p fulgur --test media_type_print`
Expected: `inline_media_print_applies` と `inline_media_screen_does_not_apply` 両方 PASS。

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(blitz): render with MediaType::print() for @media print support"
```

---

## Task 4: 既存の link_media_attribute.rs テストを print semantics に反転

**Files:**

- Modify: `crates/fulgur/tests/link_media_attribute.rs`

**背景**: 既存4テストは「fulgur = screen media」前提で書かれている。Task 3 で fulgur が print media になったので assertion を反転する必要がある。

**Step 1: 影響するテストを反転**

変更対象:

| 関数 | 旧 assertion | 新 assertion |
|---|---|---|
| `link_media_print_does_not_apply_on_screen` | `!result`（print.css 不適用） | `result`（print.css 適用） |
| `link_media_matching_screen_still_loads_via_rewrite` | `result`（screen.css 適用） | `!result`（screen.css 不適用） |
| `link_media_print_nested_import_also_excluded_on_screen` | `!result`（nested print 不適用） | `result`（nested print 適用） |
| `link_media_all_still_applies` | `result` | `result` 変更なし |
| `link_without_media_still_applies` | `result` | `result` 変更なし |

関数名も print semantics に合わせてリネーム:

- `link_media_print_does_not_apply_on_screen` → `link_media_print_applies_under_print`
- `link_media_matching_screen_still_loads_via_rewrite` → `link_media_screen_excluded_under_print`
- `link_media_print_nested_import_also_excluded_on_screen` → `link_media_print_nested_import_applied_under_print`

各テストの doc comment / assertion メッセージも「print mode」に合わせて書き換える。

**Step 2: テストが PASS することを確認**

Run: `cargo test -p fulgur --test link_media_attribute`
Expected: 全テスト PASS（pdftocairo 不在環境では skip）。

**Step 3: Commit**

```bash
git add crates/fulgur/tests/link_media_attribute.rs
git commit -m "test(link-media): invert assertions for fulgur print-mode rendering"
```

---

## Task 5: `examples/link-media/` を print semantics に更新

**Files:**

- Modify: `examples/link-media/index.html`
- Modify: `examples/link-media/README.md`
- Regenerate: `examples/link-media/index.pdf`

**Step 1: index.html の説明文を print 前提に書き直す**

旧:

> The `print-only.css` stylesheet is restricted to `media="print"`; fulgur currently renders with `media="screen"`, so its rules are excluded.

新:

> The `print-only.css` stylesheet is restricted to `media="print"` and applies because fulgur renders as print media.

**Step 2: README.md を同じく更新**

旧文言（"fulgur currently renders with media=screen" 等）を print に修正。

**Step 3: PDF を再生成**

Run: `mise run update-examples`
Expected: `examples/link-media/index.pdf` が print-only.css の赤/黄背景 + 赤点線アンダーラインで再生成される。

**Step 4: 生成 PDF の目視確認**

`examples/link-media/index.pdf` を確認し、body background が黄色で body color が赤であることを確認。

**Step 5: Commit**

```bash
git add examples/link-media/
git commit -m "example(link-media): update narrative for print-mode rendering"
```

---

## Task 6: 新しい `examples/media-print/` を追加

**Files:**

- Create: `examples/media-print/index.html`
- Create: `examples/media-print/style.css`
- Create: `examples/media-print/README.md`
- Create: `examples/media-print/index.pdf`（update-examples で生成）

**Step 1: `examples/media-print/index.html` を作成**

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>@media print example</title>
  <link rel="stylesheet" href="style.css">
</head>
<body>
  <h1>Report</h1>
  <p class="screen-only">This note only appears on screen.</p>
  <p class="print-only">This note only appears in print.</p>
  <p>This paragraph is always shown.</p>
</body>
</html>
```

**Step 2: `style.css` を作成**

```css
body { font-family: "Noto Sans", sans-serif; color: #1f2937; }
h1 { color: #064e3b; }

@media screen {
  .print-only { display: none; }
  .screen-only { color: #0f766e; }
}

@media print {
  .screen-only { display: none; }
  .print-only { color: #9f1239; font-weight: bold; }
}
```

**Step 3: README.md を作成**

fulgur が print media で render することを説明し、screen-only 段落が消え print-only 段落が赤字太字で表示されることを記載。

**Step 4: PDF を生成**

Run: `mise run update-examples`
Expected: `examples/media-print/index.pdf` に print-only 段落が赤字太字で表示され、screen-only 段落は表示されない。

**Step 5: Commit**

```bash
git add examples/media-print/
git commit -m "example(media-print): add @media print/@media screen example"
```

---

## Task 7: CHANGELOG を更新

**Files:**

- Modify: `CHANGELOG.md`

**Step 1: `[Unreleased]` の `### Added` に追記**

既存の `### Added` セクションに以下を追加（最初の項目として）:

```markdown
- fulgur は PDF 生成専用ツールとして常に CSS media type `print` で
  レンダリングするようになりました。これまで `@media print { ... }`
  ルールや `<link rel=stylesheet media=print>` は blitz-dom の
  `make_device` が `MediaType::screen()` をハードコードしていた
  ため適用されませんでしたが、upstream フォーク
  (`mitsuru/blitz:set-media-type-v0.2.x`) で `DocumentConfig::media_type`
  API を追加し、fulgur 側でそれを `MediaType::print()` に設定することで
  `@media print` が正しく適用されるようになります。`@media screen`
  ルールは除外されます。`[patch.crates-io]` で blitz 関連 4 crate を
  fork に差し替えている状態で、upstream PR がマージされ次第
  crates.io 版に戻します。(fulgur-70v)
```

**Step 2: Markdownlint 通過確認**

Run: `npx markdownlint-cli2 'CHANGELOG.md'`
Expected: エラーなし。

**Step 3: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): note @media print support landing"
```

---

## Task 8: Lint / format / markdownlint 最終確認

**Files:**

- Fix any findings

**Step 1: cargo fmt**

Run: `cargo fmt --all`
Expected: 差分なし（あれば即コミット）。

**Step 2: cargo clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: warnings/errors なし。

**Step 3: markdownlint**

Run: `npx markdownlint-cli2 '**/*.md'`
Expected: エラーなし。

**Step 4: フルテスト再実行**

Run: `cargo test -p fulgur`
Expected: 全 PASS。

**Step 5: fmt 差分や clippy 指摘があれば都度 commit**

---

## Task 9: 完了確認

**Step 1: 追加された/変更されたファイルを git で確認**

Run: `git log --oneline main..HEAD && git diff --stat main..HEAD`

確認項目:

- [ ] `Cargo.toml` + `Cargo.lock` に patch 追加
- [ ] `crates/fulgur/src/blitz_adapter.rs` に `media_type: Some(MediaType::print())`
- [ ] `crates/fulgur/tests/media_type_print.rs` 新規
- [ ] `crates/fulgur/tests/link_media_attribute.rs` assertion 反転
- [ ] `examples/link-media/` narrative 更新 + PDF 再生成
- [ ] `examples/media-print/` 新規
- [ ] `CHANGELOG.md` エントリ追加

**Step 2: beads issue で parent epic 確認**

Run: `bd show fulgur-1c2`
Expected: v0.4.5 epic に fulgur-70v が含まれている。

**Step 3: `fulgur-70v` の acceptance criteria を満たしているか手元で照合**

`bd show fulgur-70v` の acceptance セクションを確認。全項目チェック。

---

## Notes

- **フォーク branch maintenance**: `mitsuru/blitz:set-media-type-v0.2.x` は Blitz upstream の v0.2.4 ベース。今後 blitz が 0.2.5 等をリリースしたら rebase が必要。
- **upstream PR マージ後**: `fulgur-j4y` が close されたら、`Cargo.toml` の `[patch.crates-io]` を削除し、crates.io から入手する blitz-dom 新バージョンに戻す。
- **font-relative units**: `ex`/`ch`/`cap`/`ic`/`rem` はフォークでも正しく動作（`BlitzFontMetricsProvider` が保持される）。
