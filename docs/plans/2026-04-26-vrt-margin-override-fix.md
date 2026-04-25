# VRT Margin Override Fix Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `crates/fulgur-vrt` の runner が CSS `@page { margin }` を尊重するよう、`RenderSpec.margin_pt` と manifest の `[[fixture]] margin_pt` を `Option<f32>` 化し、既存 18 fixture は manifest で明示 `margin_pt = 0.0` を入れて byte-identical を維持しつつ、`@page { margin }` を持つ 2 fixture の golden を再生成する。

**Architecture:** runner が `Engine::builder().margin(...)` を unconditionally 呼ぶことで `overrides.margin = true` になり CSS が無視される問題を、`Option<f32>` の None で `.margin()` 自体を skip することで解消。Engine 公開 API は変更しない。

**Tech Stack:** Rust, fulgur (Engine builder), serde + toml (manifest), pdftocairo (poppler-utils, golden 検証)

**Working directory:** `/home/ubuntu/fulgur/.worktrees/fix-vrt-margin-override`

**beads issue:** fulgur-6bya

---

### Task 1: `manifest.rs` に `margin_pt: Option<f32>` を追加

**Files:**

- Modify: `crates/fulgur-vrt/src/manifest.rs`

**Step 1: 既存テスト `parses_defaults_and_inherits` の隣に failing test を追加**

`crates/fulgur-vrt/src/manifest.rs` の `mod tests` 末尾に追加:

```rust
#[test]
fn margin_pt_is_propagated_when_specified() {
    const SAMPLE_WITH_MARGIN: &str = r#"
[defaults]
page_size = "A4"
dpi = 150
tolerance_chrome = { max_channel_diff = 16, max_diff_pixels_ratio = 0.02 }

[[fixture]]
path = "a.html"
margin_pt = 0.0

[[fixture]]
path = "b.html"
"#;
    let m = Manifest::from_toml(SAMPLE_WITH_MARGIN).expect("parse");
    assert_eq!(m.fixtures[0].margin_pt, Some(0.0));
    assert_eq!(m.fixtures[1].margin_pt, None);
}
```

**Step 2: テストが compile fail することを確認**

Run: `cargo test -p fulgur-vrt --lib manifest::tests::margin_pt_is_propagated_when_specified 2>&1 | tail -20`
Expected: コンパイルエラー (`Fixture` に `margin_pt` フィールドがない)

**Step 3: `FixtureRow` と `Fixture` に `margin_pt: Option<f32>` を追加し、`from_toml` で渡す**

`FixtureRow` に追加:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRow {
    pub path: String,
    pub tolerance_chrome: Option<Tolerance>,
    pub page_size: Option<String>,
    pub dpi: Option<u32>,
    pub margin_pt: Option<f32>,
}
```

`Fixture` に追加:

```rust
pub struct Fixture {
    pub path: PathBuf,
    pub page_size: String,
    pub dpi: u32,
    pub tolerance_chrome: Tolerance,
    pub margin_pt: Option<f32>,
}
```

`from_toml` の `.map(|row| Fixture { ... })` ブロックに `margin_pt: row.margin_pt,` を追加。

**Step 4: テストが pass することを確認**

Run: `cargo test -p fulgur-vrt --lib manifest:: 2>&1 | tail -10`
Expected: `manifest::tests::margin_pt_is_propagated_when_specified ... ok` を含む 4 tests passing

**Step 5: Commit**

```bash
cd /home/ubuntu/fulgur/.worktrees/fix-vrt-margin-override
git add crates/fulgur-vrt/src/manifest.rs
git commit -m "feat(vrt): add Option<f32> margin_pt field to manifest schema"
```

---

### Task 2: `pdf_render.rs` の `RenderSpec.margin_pt` を `Option<f32>` 化

**Files:**

- Modify: `crates/fulgur-vrt/src/pdf_render.rs`

**Step 1: failing test を追加**

`crates/fulgur-vrt/src/pdf_render.rs` の `mod tests` の末尾に追加:

```rust
#[test]
fn css_at_page_margin_is_respected_when_margin_pt_is_none() {
    // @page { margin: 30mm } を指定した HTML
    let html = r#"<!DOCTYPE html>
<html><head><style>
@page { size: A4; margin: 30mm; }
</style></head>
<body><div style="width:100px;height:100px;background:#ff0000"></div></body></html>"#;
    let with_none = render_html_to_pdf(
        html,
        RenderSpec {
            page_size: "A4",
            margin_pt: None,
            dpi: 150,
        },
    )
    .expect("render with None should succeed");
    let with_zero = render_html_to_pdf(
        html,
        RenderSpec {
            page_size: "A4",
            margin_pt: Some(0.0),
            dpi: 150,
        },
    )
    .expect("render with Some(0.0) should succeed");
    // None は CSS @page margin を尊重するので、Some(0.0) (CSS を上書き) と異なる出力になる
    assert_ne!(
        with_none, with_zero,
        "None should respect CSS @page margin; Some(0.0) should override it"
    );
}
```

**Step 2: コンパイルが失敗することを確認**

Run: `cargo build -p fulgur-vrt --tests 2>&1 | tail -20`
Expected: コンパイルエラー (`margin_pt: f32` への `None` / `Some(0.0)` の type mismatch)

**Step 3: `RenderSpec.margin_pt` を `Option<f32>` 化、`render_html_to_pdf` を修正**

`pdf_render.rs:10-15`:

```rust
#[derive(Debug, Clone, Copy)]
pub struct RenderSpec<'a> {
    pub page_size: &'a str,
    pub margin_pt: Option<f32>,
    pub dpi: u32,
}
```

`render_html_to_pdf` (`pdf_render.rs:31-40`):

```rust
pub fn render_html_to_pdf(html: &str, spec: RenderSpec<'_>) -> anyhow::Result<Vec<u8>> {
    let mut builder = Engine::builder().page_size(page_size_from_name(spec.page_size)?);
    if let Some(mpt) = spec.margin_pt {
        builder = builder.margin(Margin::uniform(mpt));
    }
    let engine = builder.build();

    engine
        .render_html(html)
        .map_err(|e| anyhow::anyhow!("fulgur render_html failed: {e}"))
}
```

既存テスト 3 件 (`renders_solid_box_html_to_png`, `render_html_to_pdf_returns_pdf_bytes`, `render_html_to_pdf_is_deterministic_within_process`) の `margin_pt: 0.0` をすべて `margin_pt: Some(0.0)` に変更。

**Step 4: テストが pass することを確認**

Run: `cargo test -p fulgur-vrt --lib pdf_render:: 2>&1 | tail -15`
Expected: 4 tests passing (既存 3 + 新規 1)

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/src/pdf_render.rs
git commit -m "feat(vrt): make RenderSpec.margin_pt Option<f32> to respect CSS @page margin"
```

---

### Task 3: `runner.rs` で `fx.margin_pt` を渡す

**Files:**

- Modify: `crates/fulgur-vrt/src/runner.rs`

**Step 1: `runner.rs:137-144` の `RenderSpec` 構築を修正**

`crates/fulgur-vrt/src/runner.rs:141`:

```rust
let actual_pdf = pdf_render::render_html_to_pdf(
    &html,
    RenderSpec {
        page_size: &fx.page_size,
        margin_pt: fx.margin_pt,
        dpi: fx.dpi,
    },
)?;
```

**Step 2: ビルドが通ることを確認**

Run: `cargo build -p fulgur-vrt 2>&1 | tail -5`
Expected: ビルド成功 (warning なし)

**Step 3: lib テストが全部 pass することを確認**

Run: `cargo test -p fulgur-vrt --lib 2>&1 | tail -10`
Expected: 19 tests passing (元の 17 + Task 1 で +1、Task 2 で +1)

**Step 4: Commit**

```bash
git add crates/fulgur-vrt/src/runner.rs
git commit -m "feat(vrt): wire manifest margin_pt through runner to RenderSpec"
```

---

### Task 4: 既存 18 fixture の manifest entry に `margin_pt = 0.0` を追加

**Files:**

- Modify: `crates/fulgur-vrt/manifest.toml`

**Step 1: `@page { margin }` を持たない 18 fixture に `margin_pt = 0.0` を追加**

以下の fixture (= `bugs/grid-row-promote-background.html` と `bugs/cover-page-break-after.html` 以外) の各 `[[fixture]]` ブロックに `margin_pt = 0.0` を追加:

- basic/solid-box.html
- basic/borders.html
- basic/border-radius.html
- basic/box-shadow.html
- layout/flex-row.html
- layout/grid-simple.html
- layout/multicol-2.html
- layout/overflow-hidden.html
- layout/overflow-hidden-rounded.html
- layout/inline-block-basic.html
- layout/inline-block-overflow-hidden.html
- layout/inline-block-nested.html
- layout/inline-flex-smoke.html
- layout/inline-grid-smoke.html
- paint/background-gradient.html
- paint/opacity.html
- svg/shapes.html
- layout/review_card_inline_block.html

例:

```toml
[[fixture]]
path = "basic/solid-box.html"
margin_pt = 0.0
```

**Step 2: VRT を走らせ 18 fixture が byte-identical で pass、2 fixture が fail することを確認**

Run: `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -40`
Expected: 18 fixtures pass、2 fixtures fail (grid-row-promote-background, cover-page-break-after)。fail メッセージは「golden mismatch」を含む

**Step 3: 失敗が想定の 2 件のみであることを diff_out_dir で確認**

Run: `ls /home/ubuntu/fulgur/.worktrees/fix-vrt-margin-override/target/vrt-diff/bugs/ 2>&1`
Expected: `grid-row-promote-background.diff.png`, `cover-page-break-after.diff.png` のみ (他のディレクトリは存在しない)

**Step 4: Commit (golden 更新前なのでテストは fail のまま)**

```bash
git add crates/fulgur-vrt/manifest.toml
git commit -m "test(vrt): pin existing fixtures to margin_pt=0.0 for byte-identity"
```

---

### Task 5: `@page` を持つ 2 fixture の golden を再生成

**Files:**

- Modify: `crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-promote-background.pdf`
- Modify: `crates/fulgur-vrt/goldens/fulgur/bugs/cover-page-break-after.pdf`

**Step 1: failing fixture のみ golden を更新する**

Run:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  FULGUR_VRT_UPDATE=failing \
  cargo test -p fulgur-vrt 2>&1 | tail -20
```

Expected: 「updated」に 2 件 (grid-row-promote-background, cover-page-break-after) が含まれる。fail は 0 件

**Step 2: 再度 VRT を走らせて全 fixture が pass することを確認**

Run: `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10`
Expected: 全 fixture pass、updated 0、failed 0

**Step 3: 更新された golden 2 件の page count と margin を pdftocairo で目視確認**

Run:

```bash
mkdir -p /tmp/vrt-verify
pdftocairo -png -r 100 \
  crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-promote-background.pdf \
  /tmp/vrt-verify/grid-row
pdftocairo -png -r 100 \
  crates/fulgur-vrt/goldens/fulgur/bugs/cover-page-break-after.pdf \
  /tmp/vrt-verify/cover
ls /tmp/vrt-verify/
```

Expected: `grid-row-*.png` と `cover-*.png` の出力。`cover-page-break-after.html` は本来「cover (height:100vh) + page-break-after: always + 続きの page」で 2 ページ構成なので `cover-1.png` と `cover-2.png` の 2 ファイルが生成される (空白の先頭ページが先に来ていたら 3 ファイルになる)。実際のファイル数を確認

**Step 4: cover-page-break-after の regression net (fulgur-y9pu) が壊れていないことを確認**

`/tmp/vrt-verify/cover-1.png` を Read tool で開いて目視:

- 1 ページ目に「Cover」が中央に大きく配置されている (期待)
- 1 ページ目が空白で 2 ページ目に Cover が来る (NG: regression)

Read: `/tmp/vrt-verify/cover-1.png`
Expected: 中央付近に "Cover" 等のコンテンツが見える

**Step 5: grid-row-promote-background の margin が CSS 通り (20mm 18mm) になっていることを確認**

Read: `/tmp/vrt-verify/grid-row-1.png`
Expected: page edge と中身の間に **白い余白 (margin)** が見える (以前は edge まで spacer が伸びていた)

**Step 6: Commit**

```bash
git add crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-promote-background.pdf \
        crates/fulgur-vrt/goldens/fulgur/bugs/cover-page-break-after.pdf
git commit -m "test(vrt): regenerate goldens that exercise CSS @page margin"
```

---

### Task 6: workspace 全体で regression がないことを確認

**Files:** (no edits)

**Step 1: workspace の lib テストを走らせる**

Run: `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test --lib 2>&1 | tail -20`
Expected: 全 crate の lib test が pass

**Step 2: fulgur 本体の test も走らせる**

Run: `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur 2>&1 | tail -10`
Expected: pass

**Step 3: clippy + fmt check**

Run:

```bash
cargo clippy -p fulgur-vrt --all-targets -- -D warnings 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -10
```

Expected: 両方とも noerror

---

### Task 7: PR 作成

**Step 1: branch を push して PR 作成**

Run:

```bash
git push -u origin fix/vrt-margin-override
gh pr create --title "fix(vrt): respect CSS @page margin in runner" --body "$(cat <<'EOF'
## Summary

- `crates/fulgur-vrt/src/runner.rs` が `RenderSpec { margin_pt: 0.0 }` をハードコードしていたため、`Engine::builder().margin(...)` が `overrides.margin = true` を立て、CSS `@page { margin }` を問答無用で上書きしていた問題を修正
- `RenderSpec.margin_pt` と `manifest` の `[[fixture]] margin_pt` を `Option<f32>` 化、`None` のときは `.margin()` を呼ばず CSS を尊重
- 既存 18 fixture (margin 指定なし) は manifest で `margin_pt = 0.0` を明示して byte-identical を維持
- `@page { margin: 20mm 18mm }` を持つ 2 fixture (`bugs/grid-row-promote-background.html`, `bugs/cover-page-break-after.html`) は CSS が効くようになり、golden を再生成

## Why

PR #205 (fulgur-86fo) のレビューで coderabbit が「printable height 728.5pt を前提にすると spacer が page boundary をまたがない」と Major 指摘してきた根本原因がこれ。VRT の fixture HTML に書いた `@page` 指定が一切効かないため、レビュアー (人間 / AI) が rendering 想定を読み取れず、GCPM `@page` margin (page_settings.rs) のテスト盲点も発生していた。

## Notes

- Issue (`fulgur-6bya`) の description には「既存 17 fixture」とあるが、現在の manifest は 20 fixture 構成 (= 18 margin 指定なし + 2 @page あり)。本 PR では 18 件を `margin_pt = 0.0` で pin、2 件は CSS 尊重で golden 再生成
- `cover-page-break-after.html` は fulgur-y9pu の regression net (page-break-after で先頭 blank page が出ない) なので、新 golden を pdftocairo で目視確認し、blank page が再発していないことを保証

## Test plan

- [x] `cargo test -p fulgur-vrt --lib` で 18 tests passing (manifest, pdf_render に新 test 追加)
- [x] `FULGUR_VRT_UPDATE` 無し / cargo test で全 fixture pass (byte-identical 18 + 再生成済み 2)
- [x] `pdftocairo` で再生成 golden 2 件を目視: grid-row-promote-background は 20mm 余白あり、cover-page-break-after は 1 ページ目に Cover が中央配置
- [x] `cargo clippy -p fulgur-vrt --all-targets -- -D warnings` clean
- [x] `cargo fmt --check` clean

Closes fulgur-6bya
EOF
)"
```

Expected: PR URL が返る

---

## Out of scope (将来 issue 化)

- Engine 側に `default_margin()` setter を追加する案 (issue description の案 C) は採用せず。VRT 専用ニーズのため Engine API を汚さない判断
- WPT runner の同等問題 (もし存在すれば) は本 PR 範囲外
