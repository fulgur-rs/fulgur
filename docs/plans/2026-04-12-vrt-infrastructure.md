# VRT 基盤整備 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** `crates/fulgur-vrt/` という dev-only crate を追加し、HTML fixture → fulgur PDF → pdftocairo で PNG 化 → 保存済み fulgur golden と比較する Visual Regression Testing 基盤を整備する。将来 `--features chrome-golden` で Chromium スクショとも比較可能な拡張フックを用意する。

**Architecture:**

- fulgur 本体には一切手を入れない。新規 crate `crates/fulgur-vrt/` (`publish = false`) を追加し、workspace に登録
- 純 Rust。Node / Playwright 依存なし。Chromium は `chromiumoxide` 経由
- fixture は `crates/fulgur-vrt/fixtures/`、golden は `crates/fulgur-vrt/goldens/{fulgur,chrome}/`、設定は `crates/fulgur-vrt/manifest.toml` に集約
- diff は「最大色差 + 超過ピクセル率」のシンプル実装。fail 時に `target/vrt-diff/<fixture>.diff.png` を出力

**Tech Stack:**

- `fulgur` (path dep, 本体 API)
- `image` crate (PNG 読み書き + ピクセル操作)
- `toml` crate (manifest パース)
- `serde` (manifest struct)
- `chromiumoxide` + `chromiumoxide_fetcher` (feature "chrome-golden" のみ)
- `pdftocairo` (system binary, poppler-utils)

**運用方式:**

- 通常テスト: `cargo test -p fulgur-vrt`
- golden 更新: `FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt` / `FULGUR_VRT_UPDATE=failing ...`
- Chrome golden 更新: `cargo test -p fulgur-vrt --features chrome-golden -- --ignored`

---

## Context for the implementer

### 既存の fulgur API (必須把握)

`crates/fulgur/src/engine.rs` に `Engine` 構造体があり、以下の最小呼び出しで HTML → PDF bytes を生成できる:

```rust
use fulgur::engine::Engine;
use fulgur::config::{Config, Margin, PageSize};

let config = Config::builder()
    .page_size(PageSize::A4)
    .margin(Margin::uniform(36.0))
    .build();

let engine = Engine::builder()
    .config(config)
    .build()
    .unwrap();

let pdf_bytes: Vec<u8> = engine.render_html(html_str).unwrap();
```

`render_html` の戻り値は `Result<Vec<u8>, fulgur::error::Error>`。エラーは `thiserror` ベース。

### プロジェクト規約

- `cargo fmt --check` と `cargo clippy` は CI 必須。commit 前に両方走らせる
- `BTreeMap` を `HashMap` より優先（PDF 決定性のため）
- Markdown 編集後は `npx markdownlint-cli2 '**/*.md'`（fenced block は言語 tag 必須、リスト前後に空行）
- fulgur-vrt 内ではテストの並行実行に注意（`target/vrt-diff/` への書き込みは fixture 名で衝突しないよう path を分けること）

### Sandbox 状態の確認

ホスト (`/home/ubuntu/fulgur/.worktrees/fulgur-o4k-vrt/`) では既に:

- `cargo build` OK（231 lib tests passing）
- `pdftocairo` / `pdftoppm` が `/usr/bin/` に存在
- `.worktrees/` は gitignore 済み

---

## Task 1: fulgur-vrt crate のスキャフォールド

**Files:**

- Create: `crates/fulgur-vrt/Cargo.toml`
- Create: `crates/fulgur-vrt/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

**Step 1: workspace に crate を追加**

`Cargo.toml` の `members` に `"crates/fulgur-vrt"` を追加:

```toml
[workspace]
resolver = "2"
members = ["crates/fulgur", "crates/fulgur-cli", "crates/fulgur-vrt"]
```

**Step 2: `crates/fulgur-vrt/Cargo.toml` を作成**

```toml
[package]
name = "fulgur-vrt"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish = false
description = "Visual regression testing harness for fulgur (dev-only, not published)"

[dependencies]
fulgur = { path = "../fulgur" }
image = { version = "0.25", default-features = false, features = ["png"] }
toml = "0.8"
serde = { version = "1", features = ["derive"] }
anyhow = "1"

# chrome-golden feature is optional: only pulled in when generating or comparing
# against Chromium screenshots. The default PR flow uses fulgur goldens only.
chromiumoxide = { version = "0.7", optional = true, features = ["tokio-runtime"] }
tokio = { version = "1", optional = true, features = ["rt-multi-thread", "macros"] }
futures = { version = "0.3", optional = true }

[features]
default = []
chrome-golden = ["dep:chromiumoxide", "dep:tokio", "dep:futures"]

[lints]
workspace = true
```

> `chromiumoxide` の正確な最新バージョンはこの PR 時点で確認（`cargo search chromiumoxide` → `Cargo.toml` に反映）。 0.7.x 系が入らない場合は 0.5/0.6 系で差し替えて OK。コンパイルさえ通れば最終版は CI で pin する。

**Step 3: `crates/fulgur-vrt/src/lib.rs` を作成**

```rust
//! Visual regression testing harness for fulgur.
//!
//! This crate is `publish = false` — it exists only to run VRT via
//! `cargo test -p fulgur-vrt`. It is not shipped to crates.io.

pub mod diff;
pub mod manifest;
pub mod pdf_render;
pub mod runner;

#[cfg(feature = "chrome-golden")]
pub mod chrome;
```

(各モジュールはまだ空でも後の Task で作成する)

**Step 4: 空モジュールを作って cargo check を通す**

とりあえず各 module を空 stub で作る:

- `crates/fulgur-vrt/src/manifest.rs` — 空ファイル
- `crates/fulgur-vrt/src/diff.rs` — 空ファイル
- `crates/fulgur-vrt/src/pdf_render.rs` — 空ファイル
- `crates/fulgur-vrt/src/runner.rs` — 空ファイル

**Step 5: Verify**

```bash
cargo check -p fulgur-vrt
```

Expected: 警告はあっても OK、エラーなしで通る

**Step 6: Commit**

```bash
git add Cargo.toml crates/fulgur-vrt/
git commit -m "feat(fulgur-vrt): scaffold dev-only VRT crate"
```

---

## Task 2: Manifest パーサ (TDD)

**Files:**

- Create: `crates/fulgur-vrt/src/manifest.rs` (本実装)
- Test: `crates/fulgur-vrt/src/manifest.rs` (同ファイル内 `#[cfg(test)]`)

**データモデル:**

```rust
// crates/fulgur-vrt/src/manifest.rs
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct Tolerance {
    pub max_channel_diff: u8,
    pub max_diff_pixels_ratio: f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_page_size")]
    pub page_size: String,
    #[serde(default = "default_dpi")]
    pub dpi: u32,
    pub tolerance_fulgur: Tolerance,
    pub tolerance_chrome: Tolerance,
}

fn default_page_size() -> String { "A4".to_string() }
fn default_dpi() -> u32 { 150 }

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRow {
    pub path: String,
    pub tolerance_fulgur: Option<Tolerance>,
    pub tolerance_chrome: Option<Tolerance>,
    pub page_size: Option<String>,
    pub dpi: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawManifest {
    pub defaults: Defaults,
    #[serde(rename = "fixture", default)]
    pub fixtures: Vec<FixtureRow>,
}

/// Fully resolved fixture with defaults applied.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub path: PathBuf,        // relative to manifest dir
    pub page_size: String,
    pub dpi: u32,
    pub tolerance_fulgur: Tolerance,
    pub tolerance_chrome: Tolerance,
}

#[derive(Debug, Clone)]
pub struct Manifest {
    pub fixtures: Vec<Fixture>,
}

impl Manifest {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let raw: RawManifest = toml::from_str(text)?;
        let fixtures = raw
            .fixtures
            .into_iter()
            .map(|row| Fixture {
                path: PathBuf::from(&row.path),
                page_size: row.page_size.unwrap_or_else(|| raw.defaults.page_size.clone()),
                dpi: row.dpi.unwrap_or(raw.defaults.dpi),
                tolerance_fulgur: row.tolerance_fulgur.unwrap_or(raw.defaults.tolerance_fulgur),
                tolerance_chrome: row.tolerance_chrome.unwrap_or(raw.defaults.tolerance_chrome),
            })
            .collect();
        Ok(Self { fixtures })
    }
}
```

**Step 1: Write failing tests first**

`crates/fulgur-vrt/src/manifest.rs` の末尾に追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[defaults]
page_size = "A4"
dpi = 150
tolerance_fulgur = { max_channel_diff = 2, max_diff_pixels_ratio = 0.001 }
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
        assert_eq!(solid.tolerance_fulgur.max_channel_diff, 2);
        assert_eq!(solid.tolerance_chrome.max_channel_diff, 16);
    }

    #[test]
    fn fixture_override_wins_over_defaults() {
        let m = Manifest::from_toml(SAMPLE).expect("parse");
        let grid = &m.fixtures[1];
        assert_eq!(grid.tolerance_chrome.max_channel_diff, 24);
        // fulgur tolerance still inherits defaults
        assert_eq!(grid.tolerance_fulgur.max_channel_diff, 2);
    }

    #[test]
    fn rejects_missing_defaults_section() {
        let bad = "[[fixture]]\npath = \"a.html\"\n";
        assert!(Manifest::from_toml(bad).is_err());
    }
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur-vrt manifest
```

Expected: compilation error（実装がまだ無い）

**Step 3: Implement Manifest module (上記のコード)**

**Step 4: Run tests to verify they pass**

```bash
cargo test -p fulgur-vrt manifest -- --nocapture
```

Expected: 3 passed

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/src/manifest.rs
git commit -m "feat(fulgur-vrt): manifest.toml parser with tolerance defaults"
```

---

## Task 3: Diff engine (TDD)

**Files:**

- Modify: `crates/fulgur-vrt/src/diff.rs`

**API 設計:**

```rust
// crates/fulgur-vrt/src/diff.rs
use crate::manifest::Tolerance;
use image::{GenericImageView, ImageBuffer, Rgba, RgbaImage};
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct DiffReport {
    pub total_pixels: u64,
    pub diff_pixels: u64,
    pub max_channel_diff: u8,
    pub pass: bool,
}

impl DiffReport {
    pub fn ratio(&self) -> f32 {
        if self.total_pixels == 0 { 0.0 }
        else { self.diff_pixels as f32 / self.total_pixels as f32 }
    }
}

/// Compare two RGBA images pixel-by-pixel.
///
/// A pixel is "different" when any of R/G/B exceeds `tol.max_channel_diff`
/// from the corresponding pixel in the reference. Alpha is ignored.
/// `pass` is true iff the ratio of differing pixels is within
/// `tol.max_diff_pixels_ratio`.
///
/// If dimensions do not match, returns a report with pass=false and
/// diff_pixels=total_pixels (treat size mismatch as total diff).
pub fn compare(reference: &RgbaImage, actual: &RgbaImage, tol: Tolerance) -> DiffReport;

/// Write a diff visualization: grayscale original as background, differing
/// pixels highlighted in red. Output is sized to `max(ref, actual)`.
pub fn write_diff_image(
    reference: &RgbaImage,
    actual: &RgbaImage,
    tol: Tolerance,
    out_path: &Path,
) -> anyhow::Result<()>;

/// Read a PNG from disk into an RgbaImage (owned).
pub fn load_png(path: &Path) -> anyhow::Result<RgbaImage>;
```

**Step 1: Write failing tests**

`crates/fulgur-vrt/src/diff.rs` に `#[cfg(test)] mod tests` を追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        ImageBuffer::from_pixel(w, h, Rgba(rgba))
    }

    const STRICT: Tolerance = Tolerance { max_channel_diff: 0, max_diff_pixels_ratio: 0.0 };
    const LOOSE:  Tolerance = Tolerance { max_channel_diff: 4, max_diff_pixels_ratio: 0.01 };

    #[test]
    fn identical_images_pass_strict() {
        let a = solid(10, 10, [255, 0, 0, 255]);
        let b = solid(10, 10, [255, 0, 0, 255]);
        let r = compare(&a, &b, STRICT);
        assert!(r.pass);
        assert_eq!(r.diff_pixels, 0);
        assert_eq!(r.max_channel_diff, 0);
    }

    #[test]
    fn small_channel_diff_below_tolerance_passes() {
        let a = solid(10, 10, [100, 100, 100, 255]);
        let b = solid(10, 10, [103, 100, 100, 255]); // diff=3 < 4
        let r = compare(&a, &b, LOOSE);
        assert!(r.pass);
        assert_eq!(r.diff_pixels, 0, "3 <= 4 should not be counted as diff");
        assert_eq!(r.max_channel_diff, 3);
    }

    #[test]
    fn channel_diff_above_tolerance_counts_as_diff() {
        let a = solid(10, 10, [0, 0, 0, 255]);
        let b = solid(10, 10, [10, 0, 0, 255]); // diff=10 > 4
        let r = compare(&a, &b, LOOSE);
        assert!(!r.pass); // 100% of pixels differ, 1.0 > 0.01
        assert_eq!(r.diff_pixels, 100);
        assert_eq!(r.max_channel_diff, 10);
    }

    #[test]
    fn sparse_diff_within_ratio_passes() {
        // 100 pixels total, only 1 differs; ratio = 0.01 -> within LOOSE
        let a = solid(10, 10, [0, 0, 0, 255]);
        let mut b = a.clone();
        b.put_pixel(0, 0, Rgba([50, 0, 0, 255]));
        let r = compare(&a, &b, LOOSE);
        assert_eq!(r.diff_pixels, 1);
        assert!(r.pass, "1/100 = 0.01 must be within 0.01 ratio limit");
    }

    #[test]
    fn size_mismatch_fails_with_all_diff() {
        let a = solid(10, 10, [0, 0, 0, 255]);
        let b = solid(12, 10, [0, 0, 0, 255]);
        let r = compare(&a, &b, LOOSE);
        assert!(!r.pass);
        assert_eq!(r.diff_pixels, r.total_pixels);
    }

    #[test]
    fn write_diff_image_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("diff.png");
        let a = solid(4, 4, [0, 0, 0, 255]);
        let mut b = a.clone();
        b.put_pixel(1, 1, Rgba([255, 0, 0, 255]));
        write_diff_image(&a, &b, LOOSE, &out).unwrap();
        assert!(out.exists());
        // verify round-trip
        let loaded = load_png(&out).unwrap();
        assert_eq!(loaded.dimensions(), (4, 4));
    }
}
```

> `tempfile` crate は `fulgur` の dev-dependency にしかないので、`fulgur-vrt/Cargo.toml` の `[dev-dependencies]` に `tempfile = "3"` を追加すること。

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur-vrt diff
```

Expected: compile errors

**Step 3: Implement diff module**

```rust
// full implementation at top of crates/fulgur-vrt/src/diff.rs
use crate::manifest::Tolerance;
use image::{GenericImageView, ImageBuffer, Rgba, RgbaImage};
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct DiffReport {
    pub total_pixels: u64,
    pub diff_pixels: u64,
    pub max_channel_diff: u8,
    pub pass: bool,
}

impl DiffReport {
    pub fn ratio(&self) -> f32 {
        if self.total_pixels == 0 {
            0.0
        } else {
            self.diff_pixels as f32 / self.total_pixels as f32
        }
    }
}

fn channel_diff(a: &Rgba<u8>, b: &Rgba<u8>) -> u8 {
    let dr = a[0].abs_diff(b[0]);
    let dg = a[1].abs_diff(b[1]);
    let db = a[2].abs_diff(b[2]);
    dr.max(dg).max(db)
}

pub fn compare(reference: &RgbaImage, actual: &RgbaImage, tol: Tolerance) -> DiffReport {
    let (rw, rh) = reference.dimensions();
    let (aw, ah) = actual.dimensions();

    if (rw, rh) != (aw, ah) {
        let total = u64::from(rw) * u64::from(rh);
        return DiffReport {
            total_pixels: total,
            diff_pixels: total,
            max_channel_diff: 255,
            pass: false,
        };
    }

    let total = u64::from(rw) * u64::from(rh);
    let mut diff = 0u64;
    let mut max_ch: u8 = 0;

    for (pa, pb) in reference.pixels().zip(actual.pixels()) {
        let c = channel_diff(pa, pb);
        if c > max_ch {
            max_ch = c;
        }
        if c > tol.max_channel_diff {
            diff += 1;
        }
    }

    let ratio = if total == 0 { 0.0 } else { diff as f32 / total as f32 };
    let pass = ratio <= tol.max_diff_pixels_ratio;

    DiffReport {
        total_pixels: total,
        diff_pixels: diff,
        max_channel_diff: max_ch,
        pass,
    }
}

pub fn write_diff_image(
    reference: &RgbaImage,
    actual: &RgbaImage,
    tol: Tolerance,
    out_path: &Path,
) -> anyhow::Result<()> {
    let (rw, rh) = reference.dimensions();
    let (aw, ah) = actual.dimensions();
    let (w, h) = (rw.max(aw), rh.max(ah));

    let mut out: RgbaImage = ImageBuffer::from_pixel(w, h, Rgba([255, 255, 255, 255]));

    for y in 0..h {
        for x in 0..w {
            let ref_px = reference.get_pixel_checked(x, y);
            let act_px = actual.get_pixel_checked(x, y);
            let bg = ref_px.copied().unwrap_or(Rgba([255, 255, 255, 255]));
            // grayscale luminance
            let l = (0.299 * bg[0] as f32 + 0.587 * bg[1] as f32 + 0.114 * bg[2] as f32) as u8;
            let l_dim = l.saturating_add(80); // brighten so highlight pops

            match (ref_px, act_px) {
                (Some(r), Some(a)) if channel_diff(r, a) > tol.max_channel_diff => {
                    out.put_pixel(x, y, Rgba([255, 0, 0, 255]));
                }
                (Some(_), Some(_)) => {
                    out.put_pixel(x, y, Rgba([l_dim, l_dim, l_dim, 255]));
                }
                // out-of-bounds area in mismatched sizes
                _ => {
                    out.put_pixel(x, y, Rgba([255, 255, 0, 255]));
                }
            }
        }
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    out.save(out_path)?;
    Ok(())
}

pub fn load_png(path: &Path) -> anyhow::Result<RgbaImage> {
    let img = image::open(path)?.to_rgba8();
    Ok(img)
}
```

**Step 4: Run tests**

```bash
cargo test -p fulgur-vrt diff -- --nocapture
```

Expected: 6 passed

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/
git commit -m "feat(fulgur-vrt): pixel diff engine with diff image output"
```

---

## Task 4: PDF render pipeline (fulgur → pdftocairo → PNG)

**Files:**

- Modify: `crates/fulgur-vrt/src/pdf_render.rs`

**API 設計:**

```rust
// crates/fulgur-vrt/src/pdf_render.rs
use fulgur::config::{Config, Margin, PageSize};
use fulgur::engine::Engine;
use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy)]
pub struct RenderSpec<'a> {
    pub page_size: &'a str,  // "A4", "Letter", …
    pub margin_pt: f32,
    pub dpi: u32,
}

pub fn render_html_to_rgba(
    html: &str,
    spec: RenderSpec<'_>,
    work_dir: &Path,
) -> anyhow::Result<RgbaImage>;
```

**Step 1: Implement helper for page size parse**

```rust
fn page_size_from_name(name: &str) -> anyhow::Result<PageSize> {
    match name.to_ascii_uppercase().as_str() {
        "A4" => Ok(PageSize::A4),
        "LETTER" => Ok(PageSize::Letter),
        other => anyhow::bail!("unsupported page_size: {other}"),
    }
}
```

(`PageSize` に他の variant がある場合は必要分だけ追加。最小は A4 だけで OK)

**Step 2: Implement `render_html_to_rgba`**

```rust
pub fn render_html_to_rgba(
    html: &str,
    spec: RenderSpec<'_>,
    work_dir: &Path,
) -> anyhow::Result<RgbaImage> {
    std::fs::create_dir_all(work_dir)?;

    let config = Config::builder()
        .page_size(page_size_from_name(spec.page_size)?)
        .margin(Margin::uniform(spec.margin_pt))
        .build();

    let engine = Engine::builder()
        .config(config)
        .build()
        .map_err(|e| anyhow::anyhow!("engine build: {e}"))?;

    let pdf_bytes = engine
        .render_html(html)
        .map_err(|e| anyhow::anyhow!("render_html: {e}"))?;

    let pdf_path = work_dir.join("fixture.pdf");
    std::fs::write(&pdf_path, &pdf_bytes)?;

    let prefix = work_dir.join("page");
    let status = Command::new("pdftocairo")
        .args(["-png", "-r", &spec.dpi.to_string(), "-f", "1", "-l", "1"])
        .arg(&pdf_path)
        .arg(&prefix)
        .status()
        .map_err(|e| anyhow::anyhow!("spawn pdftocairo: {e}"))?;
    anyhow::ensure!(status.success(), "pdftocairo failed with {status}");

    // pdftocairo writes page-1.png for single-page output
    let png_path = work_dir.join("page-1.png");
    let img = image::open(&png_path)?.to_rgba8();
    Ok(img)
}
```

**Step 3: Add a smoke test**

`crates/fulgur-vrt/src/pdf_render.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_solid_box_html_to_png() {
        let html = r#"<!DOCTYPE html>
<html><body style="margin:0">
<div style="width:100px;height:100px;background:#ff0000"></div>
</body></html>"#;

        let tmp = tempfile::tempdir().unwrap();
        let img = render_html_to_rgba(
            html,
            RenderSpec { page_size: "A4", margin_pt: 0.0, dpi: 150 },
            tmp.path(),
        )
        .expect("render");
        assert!(img.width() > 100);
        assert!(img.height() > 100);
    }
}
```

**Step 4: Run**

```bash
cargo test -p fulgur-vrt pdf_render -- --nocapture
```

Expected: 1 passed（pdftocairo が /usr/bin にあることに依存。CI 設定で apt install する）

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/src/pdf_render.rs
git commit -m "feat(fulgur-vrt): render fulgur PDF to RGBA via pdftocairo"
```

---

## Task 5: Chromium スクショ adapter (feature = "chrome-golden")

**Files:**

- Modify: `crates/fulgur-vrt/src/chrome.rs` (new)

**設計方針:**

このタスクは **feature-gated** で、CI では通常走らない。Chromium バージョン固定と revision 管理の骨組みを置くのが目的。fully working にはしなくて良いが、`cargo check -p fulgur-vrt --features chrome-golden` がクリーンに通ること。

**Step 1: chromiumoxide の fetcher API を確認**

crate のバージョンを決めたら、`cargo doc --no-deps -p chromiumoxide` で Browser / Fetcher の API を確認。0.5 系は `chromiumoxide_fetcher::BrowserFetcher`、0.7 系もほぼ同じ。

**Step 2: 骨組み実装**

```rust
// crates/fulgur-vrt/src/chrome.rs
//! Chromium screenshot adapter, used only when generating or verifying
//! chrome goldens. Gated behind the `chrome-golden` feature.

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use futures::StreamExt;
use image::RgbaImage;
use std::path::{Path, PathBuf};

/// Pinned Chromium revision. Update this value when refreshing chrome goldens;
/// bump together with the goldens in a single PR.
pub const CHROMIUM_REVISION: &str = "1280000";

fn fetcher_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fulgur-vrt")
        .join(format!("chromium-{CHROMIUM_REVISION}"))
}

pub async fn screenshot_html(html: &str, viewport: (u32, u32)) -> anyhow::Result<RgbaImage> {
    // NOTE: Intentionally lightweight. See tests/vrt_chrome.rs for end-to-end
    // usage. Implementation details will be refined when chrome goldens are
    // first generated.
    let (browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .viewport(chromiumoxide::handler::viewport::Viewport {
                width: viewport.0,
                height: viewport.1,
                device_scale_factor: Some(2.0),
                emulating_mobile: false,
                is_landscape: false,
                has_touch: false,
            })
            .build()
            .map_err(|e| anyhow::anyhow!("BrowserConfig: {e}"))?,
    )
    .await?;

    let handle = tokio::spawn(async move {
        while let Some(_ev) = handler.next().await {
            // drain
        }
    });

    let page = browser.new_page("about:blank").await?;
    page.set_content(html).await?;
    page.wait_for_navigation().await?;

    let bytes = page
        .screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .full_page(true)
                .build(),
        )
        .await?;

    drop(browser);
    handle.abort();

    let img = image::load_from_memory(&bytes)?.to_rgba8();
    Ok(img)
}
```

> Chromium バイナリの fetcher 起動ロジックは、この Task ではスケルトンだけで止める。最初の chrome golden 生成時に `BrowserConfig::builder().executable(...)` に fetcher で落とした path を渡す形に拡張する。

**Step 3: dirs crate を依存に追加**

`crates/fulgur-vrt/Cargo.toml` の `[features]` ブロックと `[dependencies]`:

```toml
dirs = { version = "5", optional = true }

[features]
default = []
chrome-golden = ["dep:chromiumoxide", "dep:tokio", "dep:futures", "dep:dirs"]
```

**Step 4: Verify**

```bash
cargo check -p fulgur-vrt --features chrome-golden
cargo check -p fulgur-vrt                # default 無効時
```

Expected: 両方通る。default では chromiumoxide がリンクされない。

> **If chromiumoxide の API がドキュメント通りに動かない場合**: 実装を `todo!()` に置き換え、`#[ignore]` を付けた stub テストだけ残す。重要なのは「default feature で crate 全体が壊れないこと」と「将来 chrome golden を追加する住所ができていること」の 2 点のみ。

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/
git commit -m "feat(fulgur-vrt): skeleton chromium screenshot adapter (feature-gated)"
```

---

## Task 6: Runner — manifest 走査・golden 比較・update mode

**Files:**

- Modify: `crates/fulgur-vrt/src/runner.rs`

**API 設計:**

```rust
pub struct RunnerContext {
    pub crate_root: PathBuf,       // crates/fulgur-vrt
    pub fixtures_dir: PathBuf,     // crate_root/fixtures
    pub goldens_dir: PathBuf,      // crate_root/goldens
    pub diff_out_dir: PathBuf,     // target/vrt-diff
    pub update_mode: UpdateMode,
}

pub enum UpdateMode {
    Off,
    All,
    Failing,
}

impl UpdateMode {
    pub fn from_env() -> Self {
        match std::env::var("FULGUR_VRT_UPDATE").ok().as_deref() {
            Some("1") | Some("all") => UpdateMode::All,
            Some("failing") => UpdateMode::Failing,
            _ => UpdateMode::Off,
        }
    }
}

pub struct RunResult {
    pub total: usize,
    pub passed: usize,
    pub failed: Vec<FailedFixture>,
    pub updated: Vec<PathBuf>,
}

pub struct FailedFixture {
    pub path: PathBuf,
    pub report: crate::diff::DiffReport,
    pub diff_png: PathBuf,
}

/// Entrypoint used by the integration test.
pub fn run(ctx: &RunnerContext) -> anyhow::Result<RunResult>;
```

**Step 1: Implement `RunnerContext::discover()`**

```rust
impl RunnerContext {
    pub fn discover() -> anyhow::Result<Self> {
        // CARGO_MANIFEST_DIR is set when running tests via cargo test
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures_dir = crate_root.join("fixtures");
        let goldens_dir = crate_root.join("goldens");
        // target is two levels up from crate_root when running in workspace
        let diff_out_dir = crate_root
            .parent()
            .and_then(|p| p.parent())
            .map(|ws| ws.join("target").join("vrt-diff"))
            .unwrap_or_else(|| crate_root.join("target").join("vrt-diff"));

        Ok(Self {
            crate_root,
            fixtures_dir,
            goldens_dir,
            diff_out_dir,
            update_mode: UpdateMode::from_env(),
        })
    }

    fn manifest_path(&self) -> PathBuf {
        self.crate_root.join("manifest.toml")
    }

    fn fulgur_golden(&self, fixture_path: &Path) -> PathBuf {
        let rel = fixture_path.with_extension("png");
        self.goldens_dir.join("fulgur").join(rel)
    }

    fn diff_path(&self, fixture_path: &Path) -> PathBuf {
        let rel = fixture_path.with_extension("diff.png");
        self.diff_out_dir.join(rel)
    }
}
```

**Step 2: Implement `run()`**

```rust
pub fn run(ctx: &RunnerContext) -> anyhow::Result<RunResult> {
    let manifest = crate::manifest::Manifest::load(&ctx.manifest_path())?;

    let mut result = RunResult {
        total: manifest.fixtures.len(),
        passed: 0,
        failed: Vec::new(),
        updated: Vec::new(),
    };

    // sorted for deterministic diagnostics
    let mut fixtures = manifest.fixtures.clone();
    fixtures.sort_by(|a, b| a.path.cmp(&b.path));

    let work_root = tempfile::tempdir()?;

    for (idx, fx) in fixtures.iter().enumerate() {
        let html_path = ctx.fixtures_dir.join(&fx.path);
        let html = std::fs::read_to_string(&html_path)
            .map_err(|e| anyhow::anyhow!("read fixture {}: {e}", fx.path.display()))?;

        let work_dir = work_root.path().join(format!("fx-{idx}"));
        let actual = crate::pdf_render::render_html_to_rgba(
            &html,
            crate::pdf_render::RenderSpec {
                page_size: &fx.page_size,
                margin_pt: 0.0,
                dpi: fx.dpi,
            },
            &work_dir,
        )?;

        let golden_path = ctx.fulgur_golden(&fx.path);

        match ctx.update_mode {
            UpdateMode::All => {
                save_golden(&golden_path, &actual)?;
                result.updated.push(golden_path);
                continue;
            }
            UpdateMode::Off | UpdateMode::Failing => {
                if !golden_path.exists() {
                    if matches!(ctx.update_mode, UpdateMode::Failing) {
                        save_golden(&golden_path, &actual)?;
                        result.updated.push(golden_path);
                        continue;
                    } else {
                        anyhow::bail!(
                            "missing fulgur golden for {} (run FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt to create)",
                            fx.path.display()
                        );
                    }
                }
                let reference = crate::diff::load_png(&golden_path)?;
                let report = crate::diff::compare(&reference, &actual, fx.tolerance_fulgur);

                if report.pass {
                    result.passed += 1;
                } else if matches!(ctx.update_mode, UpdateMode::Failing) {
                    save_golden(&golden_path, &actual)?;
                    result.updated.push(golden_path);
                } else {
                    let diff_png = ctx.diff_path(&fx.path);
                    crate::diff::write_diff_image(&reference, &actual, fx.tolerance_fulgur, &diff_png)?;
                    result.failed.push(FailedFixture {
                        path: fx.path.clone(),
                        report,
                        diff_png,
                    });
                }
            }
        }
    }

    Ok(result)
}

fn save_golden(path: &Path, img: &RgbaImage) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    img.save(path)?;
    Ok(())
}
```

**Step 3: Add dev-dep `tempfile` and re-check types**

```bash
cargo check -p fulgur-vrt
```

Expected: clean

**Step 4: Commit**

```bash
git add crates/fulgur-vrt/
git commit -m "feat(fulgur-vrt): runner with fulgur golden compare and update modes"
```

---

## Task 7: Integration test + fixtures

**Files:**

- Create: `crates/fulgur-vrt/tests/vrt_test.rs`
- Create: `crates/fulgur-vrt/manifest.toml`
- Create: `crates/fulgur-vrt/fixtures/basic/solid-box.html`
- Create: `crates/fulgur-vrt/fixtures/basic/borders.html`
- Create: `crates/fulgur-vrt/fixtures/basic/border-radius.html`
- Create: `crates/fulgur-vrt/fixtures/layout/flex-row.html`
- Create: `crates/fulgur-vrt/fixtures/layout/grid-simple.html`
- Create: `crates/fulgur-vrt/fixtures/layout/multicol-2.html`
- Create: `crates/fulgur-vrt/fixtures/paint/background-gradient.html`
- Create: `crates/fulgur-vrt/fixtures/paint/opacity.html`
- Create: `crates/fulgur-vrt/fixtures/svg/shapes.html`

**Step 1: Write manifest.toml**

```toml
[defaults]
page_size = "A4"
dpi = 150
tolerance_fulgur = { max_channel_diff = 2, max_diff_pixels_ratio = 0.001 }
tolerance_chrome = { max_channel_diff = 16, max_diff_pixels_ratio = 0.02 }

[[fixture]]
path = "basic/solid-box.html"

[[fixture]]
path = "basic/borders.html"

[[fixture]]
path = "basic/border-radius.html"

[[fixture]]
path = "layout/flex-row.html"

[[fixture]]
path = "layout/grid-simple.html"

[[fixture]]
path = "layout/multicol-2.html"

[[fixture]]
path = "paint/background-gradient.html"

[[fixture]]
path = "paint/opacity.html"

[[fixture]]
path = "svg/shapes.html"
```

**Step 2: Create fixtures**

All fixtures follow this template — no text, no external font, just shapes.

`basic/solid-box.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0}
  .box{width:200px;height:200px;background:#e53935;margin:40px}
</style>
</head><body><div class="box"></div></body></html>
```

`basic/borders.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0}
  .row{display:flex;gap:20px;padding:40px}
  .b{width:80px;height:80px;background:#eee}
  .solid{border:6px solid #1e88e5}
  .dashed{border:6px dashed #43a047}
  .dotted{border:6px dotted #e53935}
  .double{border:8px double #8e24aa}
</style>
</head><body><div class="row">
<div class="b solid"></div>
<div class="b dashed"></div>
<div class="b dotted"></div>
<div class="b double"></div>
</div></body></html>
```

`basic/border-radius.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0}
  .row{display:flex;gap:20px;padding:40px}
  .c{width:100px;height:100px;background:#1e88e5}
  .r1{border-radius:8px}
  .r2{border-radius:24px}
  .r3{border-radius:50%}
  .r4{border-radius:8px 48px}
</style>
</head><body><div class="row">
<div class="c r1"></div>
<div class="c r2"></div>
<div class="c r3"></div>
<div class="c r4"></div>
</div></body></html>
```

`layout/flex-row.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0}
  .flex{display:flex;gap:16px;padding:40px}
  .i{height:80px;background:#43a047}
  .a{flex:1;background:#e53935}
  .b{flex:2;background:#1e88e5}
  .c{width:80px}
</style>
</head><body><div class="flex">
<div class="i a"></div>
<div class="i b"></div>
<div class="i c"></div>
</div></body></html>
```

`layout/grid-simple.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0}
  .g{display:grid;grid-template-columns:1fr 1fr;grid-template-rows:100px 100px;gap:16px;padding:40px;width:360px}
  .c{background:#1e88e5}
  .c:nth-child(2){background:#43a047}
  .c:nth-child(3){background:#e53935}
  .c:nth-child(4){background:#fdd835}
</style>
</head><body><div class="g">
<div class="c"></div><div class="c"></div>
<div class="c"></div><div class="c"></div>
</div></body></html>
```

`layout/multicol-2.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0}
  .m{column-count:2;column-gap:24px;padding:40px;width:400px}
  .b{height:60px;margin-bottom:16px;background:#1e88e5;break-inside:avoid}
  .b:nth-child(2n){background:#43a047}
</style>
</head><body><div class="m">
<div class="b"></div><div class="b"></div>
<div class="b"></div><div class="b"></div>
<div class="b"></div><div class="b"></div>
</div></body></html>
```

`paint/background-gradient.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0}
  .g{width:400px;height:200px;margin:40px;background:linear-gradient(90deg,#e53935,#1e88e5)}
</style>
</head><body><div class="g"></div></body></html>
```

`paint/opacity.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0}
  .stack{position:relative;width:300px;height:200px;margin:40px}
  .a{position:absolute;left:0;top:0;width:180px;height:180px;background:#e53935;opacity:0.7}
  .b{position:absolute;left:100px;top:20px;width:180px;height:180px;background:#1e88e5;opacity:0.7}
</style>
</head><body><div class="stack">
<div class="a"></div>
<div class="b"></div>
</div></body></html>
```

`svg/shapes.html`:

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8">
<style>html,body{margin:0;padding:0}</style>
</head><body>
<svg width="400" height="200" viewBox="0 0 400 200" xmlns="http://www.w3.org/2000/svg" style="margin:40px">
  <rect x="10" y="10" width="80" height="80" fill="#e53935"/>
  <circle cx="180" cy="50" r="40" fill="#1e88e5"/>
  <polygon points="270,10 330,90 210,90" fill="#43a047"/>
  <line x1="10" y1="150" x2="390" y2="150" stroke="#333" stroke-width="4"/>
</svg>
</body></html>
```

**Step 3: Write integration test**

`crates/fulgur-vrt/tests/vrt_test.rs`:

```rust
use fulgur_vrt::runner::{self, RunnerContext};
use std::fmt::Write;

#[test]
fn run_fulgur_vrt() {
    let ctx = RunnerContext::discover().expect("discover context");
    let result = runner::run(&ctx).expect("runner execution failed");

    if !result.updated.is_empty() {
        eprintln!("updated {} goldens", result.updated.len());
        return;
    }

    if !result.failed.is_empty() {
        let mut msg = format!(
            "VRT failed: {} of {} fixtures differ\n",
            result.failed.len(),
            result.total
        );
        for f in &result.failed {
            let _ = writeln!(
                msg,
                "  - {} (max_channel_diff={}, diff_pixels={}/{} = {:.3}%)",
                f.path.display(),
                f.report.max_channel_diff,
                f.report.diff_pixels,
                f.report.total_pixels,
                f.report.ratio() * 100.0,
            );
        }
        msg.push_str("\nTo update all goldens:    FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt\n");
        msg.push_str("To update failing only:   FULGUR_VRT_UPDATE=failing cargo test -p fulgur-vrt\n");
        msg.push_str("Inspect diff images:      ls target/vrt-diff/\n");
        panic!("{msg}");
    }

    assert_eq!(result.passed, result.total);
}
```

**Step 4: First run creates goldens (one-shot seeding)**

```bash
FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt run_fulgur_vrt -- --nocapture
```

Expected: runner が全 fixture を走らせて `goldens/fulgur/**/*.png` を生成。テストは pass。

**Step 5: 生成された golden を目視確認**

```bash
ls -R crates/fulgur-vrt/goldens/fulgur/
# 必要なら file viewer で画像を開いて「意味のある矩形/図形になっているか」確認
```

もし fulgur の現状ではうまくレンダリングできない fixture (例: grid, multicol) がある場合、`manifest.toml` から一旦コメントアウトし、別 beads issue で追跡する（この VRT task のスコープではない）。

**Step 6: 2 回目実行は回帰なしで pass すること**

```bash
cargo test -p fulgur-vrt run_fulgur_vrt
```

Expected: pass（diff_pixels=0 であるべき）

**Step 7: Commit**

```bash
git add crates/fulgur-vrt/fixtures/ crates/fulgur-vrt/manifest.toml crates/fulgur-vrt/goldens/ crates/fulgur-vrt/tests/
git commit -m "test(fulgur-vrt): add initial fixture set and fulgur goldens"
```

---

## Task 8: CI 統合

**Files:**

- Modify: `.github/workflows/ci.yml`

**Step 1: 既存 ci.yml の確認**

```bash
cat .github/workflows/ci.yml
```

既存の build / test job を読み、cache 戦略・rust-toolchain の設定方法に合わせる。

**Step 2: VRT job を追加**

既存 job の末尾に追加:

```yaml
  vrt:
    name: Visual Regression Tests
    runs-on: ubuntu-latest
    # Skip docs-only PRs. For pushes to main, always run.
    if: github.event_name != 'pull_request' || contains(github.event.pull_request.changed_files, 'crates/fulgur/') || contains(github.event.pull_request.changed_files, 'crates/fulgur-vrt/')
    steps:
      - uses: actions/checkout@v4
      - name: Install poppler-utils
        run: sudo apt-get update && sudo apt-get install -y poppler-utils
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Run VRT
        run: cargo test -p fulgur-vrt --test vrt_test
      - name: Upload diff artifacts on failure
        if: failure()
        uses: actions/upload-artifact@v4
        with:
          name: vrt-diffs
          path: target/vrt-diff/
          retention-days: 14
```

> Note: `contains(changed_files, ...)` は paths filter の代替。より厳密には `dorny/paths-filter` Action を使って事前ジョブで判定する方法がベスト。初期はこの方式でも OK。

**Step 3: Verify locally**

```bash
npx markdownlint-cli2 '**/*.md' || true
# (markdown changes 無ければ skip)
cargo test -p fulgur-vrt --test vrt_test
```

**Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run fulgur-vrt visual regression tests in PRs touching fulgur"
```

---

## Task 9: Chrome golden 更新 workflow (手動 dispatch)

**Files:**

- Create: `.github/workflows/vrt-chrome-update.yml`

**Step 1: Write workflow**

```yaml
name: VRT Chrome Golden Update

on:
  workflow_dispatch:

jobs:
  chrome-goldens:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install deps
        run: sudo apt-get update && sudo apt-get install -y poppler-utils libnss3 libatk-bridge2.0-0 libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 libxrandr2 libgbm1 libasound2
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - uses: actions/cache@v4
        with:
          path: ~/.cache/fulgur-vrt/
          key: chromium-${{ hashFiles('crates/fulgur-vrt/src/chrome.rs') }}
      - name: Generate Chrome goldens
        run: cargo test -p fulgur-vrt --features chrome-golden -- --ignored
      - name: Upload updated goldens
        uses: actions/upload-artifact@v4
        with:
          name: chrome-goldens
          path: crates/fulgur-vrt/goldens/chrome/
          retention-days: 30
```

> 初期フェーズでは `#[ignore]` の chrome golden テスト本体がまだ無いため、この workflow は dry-run 用のスケルトン。Task 5 の chrome adapter が動くようになった後に、`crates/fulgur-vrt/tests/vrt_chrome.rs` を追加する別 issue で実動作化する。

**Step 2: Verify yaml syntax**

```bash
# (actionlint があれば)
actionlint .github/workflows/vrt-chrome-update.yml || true
```

**Step 3: Commit**

```bash
git add .github/workflows/vrt-chrome-update.yml
git commit -m "ci: add manual workflow skeleton for chrome golden updates"
```

---

## Task 10: ドキュメント整備

**Files:**

- Create: `crates/fulgur-vrt/README.md`
- Modify: `README.md` (リポジトリルート、Contributing セクションに 1 行追加)

**Step 1: Write `crates/fulgur-vrt/README.md`**

````markdown
# fulgur-vrt

Visual regression testing harness for `fulgur`.

This crate is `publish = false` — it exists only to exercise fulgur's rendering
output via `cargo test`. It is never released to crates.io.

## How it works

1. Each fixture in `fixtures/` is a self-contained, font-free HTML snippet (rectangles, gradients, SVG shapes).
2. `cargo test -p fulgur-vrt` renders each fixture through fulgur, converts the resulting PDF to PNG via `pdftocairo` at 150 DPI, and compares the output to a committed `goldens/fulgur/<path>.png` using a maximum-channel-diff + ratio tolerance.
3. On failure, a diff image is written to `target/vrt-diff/<path>.diff.png` — differing pixels are highlighted in red against a grayscale copy of the expected image.

## Running

```bash
# install once
sudo apt-get install -y poppler-utils

# normal run
cargo test -p fulgur-vrt

# update all fulgur goldens (after an intentional rendering change)
FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt

# update only failing goldens
FULGUR_VRT_UPDATE=failing cargo test -p fulgur-vrt

# regenerate chrome goldens (manual, heavy)
cargo test -p fulgur-vrt --features chrome-golden -- --ignored
```

## Adding a fixture

1. Create `fixtures/<category>/<name>.html`. Keep it font-free (no text), use inline styles, and prefer shapes/colors that diff cleanly.
2. Add an entry to `manifest.toml`.
3. Run `FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt` to seed the golden.
4. Inspect `goldens/fulgur/<category>/<name>.png` and commit both the fixture and golden.

## Directory layout

```text
fixtures/          HTML inputs grouped by category
goldens/fulgur/    fulgur PDF → PNG references (committed)
goldens/chrome/    Chromium screenshot references (generated manually)
manifest.toml      fixture list + tolerance defaults
src/
  manifest.rs      TOML parser
  diff.rs          pixel diff + diff image writer
  pdf_render.rs    fulgur → pdftocairo bridge
  chrome.rs        chromiumoxide adapter (feature = "chrome-golden")
  runner.rs        orchestrates manifest → diff → update
tests/vrt_test.rs  single entrypoint run by `cargo test`
```

````

**Step 2: Run markdownlint**

```bash
npx markdownlint-cli2 'crates/fulgur-vrt/README.md'
```

Expected: no violations

**Step 3: Commit**

```bash
git add crates/fulgur-vrt/README.md
git commit -m "docs(fulgur-vrt): add crate README with workflow instructions"
```

---

## Final verification

After all tasks complete:

```bash
# format
cargo fmt --check

# lint
cargo clippy -p fulgur-vrt --all-targets -- -D warnings
cargo clippy -p fulgur -p fulgur-cli --all-targets -- -D warnings

# baseline fulgur still passes
cargo test -p fulgur --lib

# fulgur-vrt passes
cargo test -p fulgur-vrt

# chrome-golden feature compiles cleanly
cargo check -p fulgur-vrt --features chrome-golden

# docs lint
npx markdownlint-cli2 '**/*.md'
```

All green → hand off to `superpowers:finishing-a-development-branch`.

## Out of scope (tracked separately)

- Chromium revision を実際に download して使う end-to-end chrome golden 生成（別 issue）
- 複数ページ fixture サポート (Phase 2)
- fulgur レンダリングが現状失敗する CSS 機能の修正（fixture は用意するが golden 作成で失敗したものは manifest からコメントアウトして別 issue に切り出す）
- `target/vrt-diff/` のクリーンアップ自動化
