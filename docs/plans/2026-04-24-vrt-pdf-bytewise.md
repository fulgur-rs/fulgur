# VRT PDF Byte-wise Comparison Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace `crates/fulgur-vrt` の PNG ピクセル比較を PDF byte-wise 比較に置き換え、ローカル/CI 環境での pdftocairo (cairo/freetype) ビルド差由来のピクセル差を原理的に排除する。

**Architecture:**

```text
旧: HTML → fulgur → PDF → pdftocairo → PNG → pixel diff vs golden.png
新: HTML → fulgur → PDF → byte compare vs golden.pdf
       └→ (失敗時のみ) reference.pdf + actual.pdf を pdftocairo で PNG 化して diff 画像生成
```

PDF 自体の決定性は `crates/fulgur-cli/tests/examples_determinism.rs` で検証済み。
本計画は「fulgur が同入力で同 PDF を吐く」前提に乗る。

**Tech Stack:** Rust, krilla (PDF), pdftocairo (失敗時診断のみ), poppler-utils, fontconfig

---

## 制約と前提

- **build を絶対に壊さない順序**: manifest スキーマと runner の切替は同一コミット内で一括する。中間状態で参照壊れを残さない。
- **fonts.conf 全削除ではない**: fulgur 自体のフォント決定性 (`<dir>` / `<alias>`) は維持必須。削除するのは PR #195 で入れた `<match target="font">` ブロックのみ。CI の `FONTCONFIG_FILE=...` 環境変数も維持する。
- **`tolerance_chrome` は残す**: Chrome golden 比較 (`chrome-golden` feature) は本変更のスコープ外。
- **失敗時診断は捨てない**: 失敗時に `target/vrt-diff/<path>.diff.png` と `target/vrt-diff/<path>.actual.pdf` を保存し、CI-golden recovery (PR #195 で導入) を PDF にも適用する。
- **検証ゲート**: 最終タスクで CI を走らせて 18 fixture 全 pass を確認する。

---

## Task 1: pdf_render に `render_html_to_pdf` を追加

既存 `render_html_to_rgba` は失敗時診断パスで使うため残す。
ここでは PDF を返す純粋な関数を新設し、`render_html_to_rgba` の内部呼び出し先にする。

**Files:**

- Modify: `crates/fulgur-vrt/src/pdf_render.rs`

**Step 1: 失敗するテストを追加する**

`crates/fulgur-vrt/src/pdf_render.rs` の `tests` モジュールに以下を追加。

```rust
#[test]
fn render_html_to_pdf_returns_pdf_bytes() {
    let html = r#"<!DOCTYPE html><html><body style="margin:0">
<div style="width:100px;height:100px;background:#ff0000"></div>
</body></html>"#;
    let bytes = render_html_to_pdf(
        html,
        RenderSpec {
            page_size: "A4",
            margin_pt: 0.0,
            dpi: 150,
        },
    )
    .expect("render should succeed");
    assert!(bytes.starts_with(b"%PDF-"), "PDF magic missing");
    assert!(bytes.len() > 200, "suspiciously small PDF: {} bytes", bytes.len());
}

#[test]
fn render_html_to_pdf_is_deterministic_within_process() {
    let html = r#"<!DOCTYPE html><html><body style="margin:0">
<div style="width:50px;height:50px;background:#00ff00"></div>
</body></html>"#;
    let spec = RenderSpec { page_size: "A4", margin_pt: 0.0, dpi: 150 };
    let a = render_html_to_pdf(html, spec).unwrap();
    let b = render_html_to_pdf(html, spec).unwrap();
    assert_eq!(a, b, "two renders of same HTML should be byte-identical");
}
```

**Step 2: テストが落ちることを確認**

```bash
cd crates/fulgur-vrt
cargo test --lib render_html_to_pdf 2>&1 | tail -20
```

期待: `render_html_to_pdf` が見つからずコンパイルエラー。

**Step 3: 最小実装**

`render_html_to_pdf` を `crates/fulgur-vrt/src/pdf_render.rs` に追加。
`render_html_to_rgba` の中身を分割し、PDF 生成部分を流用する。

```rust
/// Render `html` through fulgur and return the resulting PDF bytes.
///
/// `spec.dpi` is unused here (rasterization happens only in the
/// failure-diagnosis path via `render_html_to_rgba`); it is kept in
/// `RenderSpec` so the same struct can drive both APIs.
pub fn render_html_to_pdf(html: &str, spec: RenderSpec<'_>) -> anyhow::Result<Vec<u8>> {
    let engine = Engine::builder()
        .page_size(page_size_from_name(spec.page_size)?)
        .margin(Margin::uniform(spec.margin_pt))
        .build();

    engine
        .render_html(html)
        .map_err(|e| anyhow::anyhow!("fulgur render_html failed: {e}"))
}
```

`render_html_to_rgba` を `render_html_to_pdf` 経由に書き換える。

```rust
pub fn render_html_to_rgba(
    html: &str,
    spec: RenderSpec<'_>,
    work_dir: &Path,
) -> anyhow::Result<RgbaImage> {
    std::fs::create_dir_all(work_dir)?;
    let pdf_bytes = render_html_to_pdf(html, spec)?;
    let pdf_path = work_dir.join("fixture.pdf");
    std::fs::write(&pdf_path, &pdf_bytes)?;
    pdf_to_rgba(&pdf_path, spec.dpi, work_dir)
}

/// Rasterize page 1 of `pdf_path` via `pdftocairo`. Used by the
/// failure-diagnosis path; the main VRT comparison no longer rasterizes.
pub fn pdf_to_rgba(pdf_path: &Path, dpi: u32, work_dir: &Path) -> anyhow::Result<RgbaImage> {
    std::fs::create_dir_all(work_dir)?;
    let prefix = work_dir.join("page");
    let status = Command::new("pdftocairo")
        .args(["-png", "-r", &dpi.to_string(), "-f", "1", "-l", "1"])
        .arg(pdf_path)
        .arg(&prefix)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn pdftocairo: {e}"))?;
    anyhow::ensure!(status.success(), "pdftocairo exited with {status}");
    let png_path = work_dir.join("page-1.png");
    let img = image::open(&png_path)?.to_rgba8();
    Ok(img)
}
```

**Step 4: テストが通ることを確認**

```bash
cargo test -p fulgur-vrt --lib pdf_render 2>&1 | tail -20
```

期待: `renders_solid_box_html_to_png` (既存) + 新規 2 テスト pass。

**Step 5: コミット**

```bash
git add crates/fulgur-vrt/src/pdf_render.rs
git commit -m "feat(vrt): add render_html_to_pdf for byte-wise comparison"
```

---

## Task 2: diff モジュールに PDF byte 比較関数を追加

**Files:**

- Modify: `crates/fulgur-vrt/src/diff.rs`

**Step 1: 失敗するテストを追加する**

`crates/fulgur-vrt/src/diff.rs` の `tests` モジュールに以下を追加。

```rust
#[test]
fn pdf_bytes_equal_returns_true_for_identical_bytes() {
    let a = b"%PDF-1.7\nfoo".to_vec();
    let b = a.clone();
    assert!(pdf_bytes_equal(&a, &b));
}

#[test]
fn pdf_bytes_equal_returns_false_for_different_bytes() {
    let a = b"%PDF-1.7\nfoo".to_vec();
    let b = b"%PDF-1.7\nbar".to_vec();
    assert!(!pdf_bytes_equal(&a, &b));
}

#[test]
fn pdf_bytes_equal_returns_false_for_size_mismatch() {
    let a = b"%PDF-1.7\nfoo".to_vec();
    let b = b"%PDF-1.7\nfoo extra".to_vec();
    assert!(!pdf_bytes_equal(&a, &b));
}
```

**Step 2: テストが落ちることを確認**

```bash
cargo test -p fulgur-vrt --lib pdf_bytes_equal 2>&1 | tail -20
```

期待: `pdf_bytes_equal` が見つからずコンパイルエラー。

**Step 3: 最小実装**

`crates/fulgur-vrt/src/diff.rs` の先頭付近 (既存 `compare` の上) に追加。

```rust
/// Byte-wise equality check for fulgur's deterministic PDF output.
///
/// fulgur produces byte-identical PDFs for the same input (verified by
/// `examples_determinism.rs`), so any difference is a real regression —
/// no tolerance, no normalization.
pub fn pdf_bytes_equal(reference: &[u8], actual: &[u8]) -> bool {
    reference == actual
}
```

**Step 4: テストが通ることを確認**

```bash
cargo test -p fulgur-vrt --lib diff 2>&1 | tail -20
```

期待: 既存 6 テスト + 新規 3 テスト pass。

**Step 5: コミット**

```bash
git add crates/fulgur-vrt/src/diff.rs
git commit -m "feat(vrt): add pdf_bytes_equal for byte-wise PDF comparison"
```

---

## Task 3: manifest スキーマから `tolerance_fulgur` を削除し runner を切り替え

**重要**: スキーマ変更と runner 切替を同一コミットで行う。中間状態で build を壊さないため。

**Files:**

- Modify: `crates/fulgur-vrt/src/manifest.rs`
- Modify: `crates/fulgur-vrt/manifest.toml`
- Modify: `crates/fulgur-vrt/src/runner.rs`
- Modify: `crates/fulgur-vrt/tests/vrt_test.rs`

**Step 1: manifest.rs から `tolerance_fulgur` を削除**

以下を削除する。

- `Defaults` 構造体: `pub tolerance_fulgur: Tolerance,`
- `FixtureRow` 構造体: `pub tolerance_fulgur: Option<Tolerance>,`
- `Fixture` 構造体: `pub tolerance_fulgur: Tolerance,`
- `Manifest::from_toml` 内: `tolerance_fulgur: row.tolerance_fulgur.unwrap_or(raw.defaults.tolerance_fulgur),`

`Tolerance` 構造体自体は `tolerance_chrome` で引き続き使うため残す。

`tests` モジュールの SAMPLE と assertion を更新:

```rust
const SAMPLE: &str = r#"
[defaults]
page_size = "A4"
dpi = 150
tolerance_chrome = { max_channel_diff = 16, max_diff_pixels_ratio = 0.02 }

[[fixture]]
path = "basic/solid-box.html"

[[fixture]]
path = "layout/grid-simple.html"
tolerance_chrome = { max_channel_diff = 24, max_diff_pixels_ratio = 0.03 }
"#;

#[test]
fn parses_defaults_and_inherits() {
    let m = Manifest::from_toml(SAMPLE).expect("parse");
    assert_eq!(m.fixtures.len(), 2);
    let solid = &m.fixtures[0];
    assert_eq!(solid.path, PathBuf::from("basic/solid-box.html"));
    assert_eq!(solid.dpi, 150);
    assert_eq!(solid.page_size, "A4");
    assert_eq!(solid.tolerance_chrome.max_channel_diff, 16);
}

#[test]
fn fixture_override_wins_over_defaults() {
    let m = Manifest::from_toml(SAMPLE).expect("parse");
    let grid = &m.fixtures[1];
    assert_eq!(grid.tolerance_chrome.max_channel_diff, 24);
}
```

**Step 2: manifest.toml から `tolerance_fulgur` 行を削除**

`crates/fulgur-vrt/manifest.toml` の `[defaults]` から該当行のみ削除。

```toml
[defaults]
page_size = "A4"
dpi = 150
tolerance_chrome = { max_channel_diff = 16, max_diff_pixels_ratio = 0.02 }
```

**Step 3: runner.rs を PDF byte 比較に切り替え**

`crates/fulgur-vrt/src/runner.rs` 全体を以下のように書き換える要点:

1. `RunnerContext::fulgur_golden`: 拡張子を `.pdf` にする。

   ```rust
   fn fulgur_golden(&self, fixture_path: &Path) -> PathBuf {
       let rel = fixture_path.with_extension("pdf");
       self.goldens_dir.join("fulgur").join(rel)
   }
   ```

2. `RunnerContext::actual_path`: 拡張子を `.actual.pdf` にする。

   ```rust
   fn actual_path(&self, fixture_path: &Path) -> PathBuf {
       let rel = fixture_path.with_extension("actual.pdf");
       self.diff_out_dir.join(rel)
   }
   ```

3. `FailedFixture` 構造体: `report: DiffReport` を簡素な情報に置き換える。

   ```rust
   #[derive(Debug, Clone)]
   pub struct FailedFixture {
       pub path: PathBuf,
       pub reference_size: u64,
       pub actual_size: u64,
       pub diff_png: PathBuf,
   }
   ```

4. `run` 関数: メインループを以下のように書き換える。

   ```rust
   pub fn run(ctx: &RunnerContext) -> anyhow::Result<RunResult> {
       let manifest = Manifest::load(&ctx.manifest_path())?;

       let mut result = RunResult {
           total: manifest.fixtures.len(),
           ..Default::default()
       };

       let mut fixtures = manifest.fixtures.clone();
       fixtures.sort_by(|a, b| a.path.cmp(&b.path));

       let work_root = tempfile::tempdir()?;

       for (idx, fx) in fixtures.iter().enumerate() {
           let html_path = ctx.fixtures_dir.join(&fx.path);
           let html = std::fs::read_to_string(&html_path).map_err(|e| {
               anyhow::anyhow!("failed to read fixture {}: {e}", fx.path.display())
           })?;

           let actual_pdf = pdf_render::render_html_to_pdf(
               &html,
               RenderSpec {
                   page_size: &fx.page_size,
                   margin_pt: 0.0,
                   dpi: fx.dpi,
               },
           )?;

           let golden_path = ctx.fulgur_golden(&fx.path);

           match ctx.update_mode {
               UpdateMode::All => {
                   save_pdf(&golden_path, &actual_pdf)?;
                   result.updated.push(golden_path);
                   continue;
               }
               UpdateMode::Off | UpdateMode::Failing => {
                   if !golden_path.exists() {
                       if matches!(ctx.update_mode, UpdateMode::Failing) {
                           save_pdf(&golden_path, &actual_pdf)?;
                           result.updated.push(golden_path);
                           continue;
                       } else {
                           anyhow::bail!(
                               "missing fulgur golden for {} (run `FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt` to seed it)",
                               fx.path.display()
                           );
                       }
                   }

                   let reference_pdf = std::fs::read(&golden_path)?;

                   if diff::pdf_bytes_equal(&reference_pdf, &actual_pdf) {
                       result.passed += 1;
                   } else if matches!(ctx.update_mode, UpdateMode::Failing) {
                       save_pdf(&golden_path, &actual_pdf)?;
                       result.updated.push(golden_path);
                   } else {
                       let work_dir = work_root.path().join(format!("fx-{idx}"));
                       let diff_png = ctx.diff_path(&fx.path);
                       let actual_pdf_artifact = ctx.actual_path(&fx.path);

                       // 失敗時のみ pdftocairo で両 PDF を PNG 化して diff 画像生成
                       write_pdf_diff_artifacts(
                           &reference_pdf,
                           &actual_pdf,
                           fx.dpi,
                           &work_dir,
                           &diff_png,
                           &actual_pdf_artifact,
                       )?;

                       result.failed.push(FailedFixture {
                           path: fx.path.clone(),
                           reference_size: reference_pdf.len() as u64,
                           actual_size: actual_pdf.len() as u64,
                           diff_png,
                       });
                   }
               }
           }
       }

       Ok(result)
   }

   fn save_pdf(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
       if let Some(parent) = path.parent() {
           std::fs::create_dir_all(parent)?;
       }
       std::fs::write(path, bytes)?;
       Ok(())
   }

   /// On byte-mismatch, rasterize both PDFs and save:
   /// - `diff_png`: pixel diff between reference and actual renders
   /// - `actual_pdf_path`: the actual PDF (for CI-golden recovery, mirrors PR #195)
   fn write_pdf_diff_artifacts(
       reference_pdf: &[u8],
       actual_pdf: &[u8],
       dpi: u32,
       work_dir: &Path,
       diff_png: &Path,
       actual_pdf_path: &Path,
   ) -> anyhow::Result<()> {
       std::fs::create_dir_all(work_dir)?;
       let ref_pdf_path = work_dir.join("reference.pdf");
       let act_pdf_path = work_dir.join("actual.pdf");
       std::fs::write(&ref_pdf_path, reference_pdf)?;
       std::fs::write(&act_pdf_path, actual_pdf)?;

       let ref_img = pdf_render::pdf_to_rgba(&ref_pdf_path, dpi, &work_dir.join("ref"))?;
       let act_img = pdf_render::pdf_to_rgba(&act_pdf_path, dpi, &work_dir.join("act"))?;

       // tolerance=0 で全差を赤く塗る (診断目的)
       let tol = crate::manifest::Tolerance {
           max_channel_diff: 0,
           max_diff_pixels_ratio: 0.0,
       };
       diff::write_diff_image(&ref_img, &act_img, tol, diff_png)?;

       // actual.pdf を artifact として保存 (CI-golden recovery)
       if let Some(parent) = actual_pdf_path.parent() {
           std::fs::create_dir_all(parent)?;
       }
       std::fs::write(actual_pdf_path, actual_pdf)?;
       Ok(())
   }
   ```

5. 不要になった import (`image::RgbaImage`, `crate::diff::DiffReport`) を削除する。
   `tempfile::TempDir` の使用箇所を再確認 (work_root は失敗時診断のみで必要)。

**Step 4: vrt_test.rs の失敗メッセージ更新**

`crates/fulgur-vrt/tests/vrt_test.rs`:

```rust
if !result.failed.is_empty() {
    let mut msg = format!(
        "VRT failed: {} of {} fixtures differ (PDF byte-wise)\n",
        result.failed.len(),
        result.total
    );
    for f in &result.failed {
        let _ = writeln!(
            msg,
            "  - {} (reference={} bytes, actual={} bytes)",
            f.path.display(),
            f.reference_size,
            f.actual_size,
        );
    }
    msg.push_str("\nTo update all goldens:    FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt\n");
    msg.push_str("To update failing only:   FULGUR_VRT_UPDATE=failing cargo test -p fulgur-vrt\n");
    msg.push_str("Inspect diff images:      ls target/vrt-diff/\n");
    panic!("{msg}");
}
```

**Step 5: ビルドと既存ユニットテスト確認**

```bash
cargo build -p fulgur-vrt 2>&1 | tail -20
cargo test -p fulgur-vrt --lib 2>&1 | tail -20
```

期待:

- ビルド成功
- ライブラリテスト全 pass (manifest テスト 3, diff テスト 6+3, pdf_render テスト 1+2, runner テスト 2)
- 注: VRT integration test (`tests/vrt_test.rs`) は次タスクまで失敗する (古い .png golden しか無いため)

**Step 6: コミット**

```bash
git add crates/fulgur-vrt/src/manifest.rs crates/fulgur-vrt/manifest.toml \
        crates/fulgur-vrt/src/runner.rs crates/fulgur-vrt/tests/vrt_test.rs
git commit -m "refactor(vrt): switch runner to PDF byte-wise comparison"
```

---

## Task 4: Golden を PDF に再生成し古い PNG を削除

**Files:**

- Delete: `crates/fulgur-vrt/goldens/fulgur/**/*.png` (18 files)
- Create: `crates/fulgur-vrt/goldens/fulgur/**/*.pdf` (18 files)

**Step 1: 古い PNG golden を削除**

```bash
git rm crates/fulgur-vrt/goldens/fulgur/**/*.png
```

期待: 18 files staged for deletion.

**Step 2: PDF golden を生成**

ピン留めされた fontconfig を使い、CI と同じ環境で生成する (フォント決定性のため)。

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt 2>&1 | tail -20
```

期待: `updated 18 goldens` が出る。

**Step 3: 生成された PDF を確認**

```bash
ls crates/fulgur-vrt/goldens/fulgur/**/*.pdf | wc -l
```

期待: 18

**Step 4: 比較モードで実行して全 pass を確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt 2>&1 | tail -20
```

期待: `test result: ok. 1 passed; 0 failed` (`run_fulgur_vrt`)

**Step 5: 同じテストをもう一度走らせて再現性を確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt 2>&1 | tail -20
```

期待: 同じく pass (re-render が byte-identical であることの確認)。

**Step 6: コミット**

```bash
git add crates/fulgur-vrt/goldens/fulgur/
git commit -m "test(vrt): regenerate goldens as PDF for byte-wise comparison"
```

---

## Task 5: fonts.conf から freetype/cairo レンダリングパラメータピンを削除

PDF byte 比較ではラスタライズ品質の env 差は無関係になるため、PR #195 で
追加した `<match target="font">` ブロックを削除する。
**`<dir>` / `<alias>` セクションは fulgur 自体のフォント決定性に必要なので残す。**

**Files:**

- Modify: `examples/.fontconfig/fonts.conf`

**Step 1: `<match target="font">` ブロックとその上のコメントを削除**

`examples/.fontconfig/fonts.conf` の以下の範囲を削除 (行 26-40):

```xml
  <!-- Pin freetype/cairo rendering parameters so PDF→PNG output is
       byte-identical across host environments. Even when the same font
       file is selected, different Ubuntu versions ship different
       system defaults for hinting/antialias/lcdfilter/rgba via
       /etc/fonts/conf.d, which causes ~0.2% pixel diffs in italic
       glyph outlines between local dev and CI. -->
  <match target="font">
    <edit name="antialias" mode="assign"><bool>true</bool></edit>
    <edit name="hinting" mode="assign"><bool>true</bool></edit>
    <edit name="hintstyle" mode="assign"><const>hintslight</const></edit>
    <edit name="autohint" mode="assign"><bool>false</bool></edit>
    <edit name="rgba" mode="assign"><const>none</const></edit>
    <edit name="lcdfilter" mode="assign"><const>lcddefault</const></edit>
    <edit name="embeddedbitmap" mode="assign"><bool>false</bool></edit>
  </match>
```

`<dir>`, `<alias>` セクションは触らない。

**Step 2: 削除後ファイルを目視確認**

```bash
sed -n '20,30p' examples/.fontconfig/fonts.conf
```

期待: `<dir>` と `<cachedir>` の直後に `<alias>` が来る (`<match>` ブロックなし)。

**Step 3: VRT が引き続き pass することを確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt 2>&1 | tail -10
```

期待: `1 passed`

**Step 4: 既存の examples_determinism も pass することを確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-cli --test examples_determinism 2>&1 | tail -10
```

期待: pass (fulgur の PDF 出力には match block は元々影響しないので)。

**Step 5: コミット**

```bash
git add examples/.fontconfig/fonts.conf
git commit -m "chore(vrt): remove freetype/cairo rendering pin from fonts.conf"
```

---

## Task 6: CI workflow から不要 step を削除

**Files:**

- Modify: `.github/workflows/ci.yml`

**Step 1: "Debug font resolution" step を削除**

`.github/workflows/ci.yml` の `vrt:` ジョブから、以下のステップ全体を削除する (行 147-156 付近):

```yaml
      - name: Debug font resolution
        run: |
          echo "=== sans-serif:italic ==="
          FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
            fc-match -v 'sans-serif:italic' | grep -E 'file|hinting|antialias|rgba|hintstyle|autohint|lcdfilter' || true
          echo "=== sans-serif ==="
          FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
            fc-match -v 'sans-serif' | grep -E 'file|hinting|antialias|rgba|hintstyle|autohint|lcdfilter' || true
          echo "=== DejaVu Sans (should be rejected) ==="
          FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
            fc-match -v 'DejaVu Sans' | grep 'file' || true
```

**Step 2: poppler-utils インストール step は維持**

失敗時の diff 画像生成 (`pdftocairo` 経由) で必要なので削除しない。
`Run VRT` step の `FONTCONFIG_FILE=...` も維持 (fulgur のフォント決定性のため)。

**Step 3: 編集後の vrt ジョブを目視**

```bash
sed -n '133,170p' .github/workflows/ci.yml
```

期待: Debug font resolution が消え、`Run VRT` の前は `Configure mold linker` のみ。

**Step 4: コミット**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(vrt): drop debug font resolution step (no longer needed)"
```

---

## Task 7: CLAUDE.md の VRT 関連記述を更新

**Files:**

- Modify: `CLAUDE.md`

**Step 1: 既存の VRT 関連 gotcha を確認**

```bash
grep -n -B1 -A5 "PDF → PNG for visual tests\|fulgur-vrt::pdf_render" CLAUDE.md
```

**Step 2: 該当行を新しい説明に置き換える**

`CLAUDE.md` の以下の bullet を:

```markdown
- **PDF → PNG for visual tests**: `pdftocairo -png -r 100 -f 1 -l 1 <pdf> <prefix>` (poppler-utils). Installed in CI; gate with skip-if-missing for local dev. `fulgur-vrt::pdf_render::render_html_to_rgba` wraps this but does not accept `base_path`, so integration tests that load local CSS must inline the call.
```

以下に置き換える:

```markdown
- **VRT は PDF byte 比較**: `crates/fulgur-vrt` は HTML → PDF を生成して `goldens/fulgur/**/*.pdf` と byte-wise 比較する (`crates/fulgur-cli/tests/examples_determinism.rs` と同じ哲学)。pdftocairo は失敗時の diff 画像生成のみで使う。golden 更新は `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt`。
```

**Step 3: markdownlint チェック**

```bash
npx markdownlint-cli2 'CLAUDE.md' 2>&1 | tail -5
```

期待: errors なし。

**Step 4: コミット**

```bash
git add CLAUDE.md
git commit -m "docs: update VRT section to describe PDF byte-wise comparison"
```

---

## Task 8: 全体検証

**Step 1: workspace 全体のテスト**

```bash
cargo test --workspace 2>&1 | tail -30
```

期待: 全 pass。

**Step 2: clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

期待: warnings なし。

**Step 3: fmt**

```bash
cargo fmt --check 2>&1 | tail -5
```

期待: 差分なし。

**Step 4: markdownlint**

```bash
npx markdownlint-cli2 '**/*.md' 2>&1 | tail -5
```

期待: errors なし。

**Step 5: VRT を意図的に壊して失敗パスの artifact 生成を確認**

```bash
# 1 つの fixture の golden を破壊
echo "broken" > crates/fulgur-vrt/goldens/fulgur/basic/solid-box.pdf

# 走らせて失敗するはず
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt 2>&1 | tail -20

# diff.png と actual.pdf が生成されているか確認
ls target/vrt-diff/basic/

# 元に戻す
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    FULGUR_VRT_UPDATE=failing cargo test -p fulgur-vrt 2>&1 | tail -10

# 再度実行して pass を確認
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt 2>&1 | tail -10
```

期待:

- 1 回目: `1 of 18 fixtures differ`、`target/vrt-diff/basic/solid-box.diff.png` と `target/vrt-diff/basic/solid-box.actual.pdf` が存在
- 2 回目: `updated 1 goldens`
- 3 回目: pass

**Step 6: PR を作成して CI で検証**

```bash
git push -u origin feature/vrt-pdf-bytewise
gh pr create --title "VRT: switch from PNG-based to PDF byte-wise comparison" --body "$(cat <<'EOF'
## 概要

VRT を PDF byte-wise 比較に移行し、ローカル/CI 環境間の pdftocairo (cairo/freetype) ビルド差由来のピクセル差を原理的に排除する。

詳細は beads issue `fulgur-sn3l` の design フィールドを参照。

## 変更点

- `pdf_render`: `render_html_to_pdf` 追加 (主経路)、`pdf_to_rgba` 追加 (失敗時診断用)
- `diff`: `pdf_bytes_equal` 追加
- `manifest`: `tolerance_fulgur` 削除 (byte 比較のため意味なし)、`tolerance_chrome` は維持
- `runner`: PDF byte 比較に切替、失敗時のみ diff PNG + actual.pdf を artifact 出力
- `goldens/fulgur/`: 18 fixture の `.png` → `.pdf` 再生成
- `examples/.fontconfig/fonts.conf`: `<match target="font">` (freetype/cairo パラメータピン) 削除、`<dir>` / `<alias>` は維持
- `.github/workflows/ci.yml`: "Debug font resolution" step 削除
- `CLAUDE.md`: VRT セクション更新

## 検証

- ローカル: `cargo test -p fulgur-vrt` 18/18 pass
- 失敗パス: golden を壊して diff.png + actual.pdf が `target/vrt-diff/` に出力されることを確認
- CI: PR push で 18 fixture 全 pass を確認 (本 PR の checks タブ参照)

## Closes

- fulgur-sn3l
EOF
)"
```

**Step 7: CI が green になるのを待つ**

```bash
gh pr checks --watch
```

期待: 全 check green。
特に `vrt` ジョブが pass することを確認 (これが本 PR の核心的検証ゲート)。

**Step 8: 全 task 完了報告**

ここまで pass したら、PR URL をユーザーに報告し、issue close の確認をする。

---

## 検証ゲート (Acceptance Criteria)

- [ ] VRT が PDF byte-wise で比較する → Task 3
- [ ] ローカルと CI で同一の pass/fail 判定 → Task 8 Step 6, 7
- [ ] review_card_inline_block.html が両環境で通過 → Task 4 (ローカル) + Task 8 (CI)
- [ ] 既存 17 fixture も引き続き通過 → Task 4 + Task 8
- [ ] 失敗時は診断用の PNG diff が artifact にアップロードされる → Task 8 Step 5
- [ ] fonts.conf の `<match target="font">` パラメータピン削除 → Task 5
- [ ] CI workflow の Debug font resolution step 削除 → Task 6
