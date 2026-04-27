# WPT Fonts Registration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** fulgur-wpt で `target/wpt/fonts/` 配下の全フォントを `AssetBundle` に登録し、Ahem/CSSTest 等に依存する reftest が Parley の system font fallback に流れて誤描画するのを防ぐ。

**Architecture:** 3 commits。

```text
1) fulgur core: AssetBundle に #[derive(Clone)] を追加（1 行＋clone 共有テスト）
2) fulgur-wpt: src/fonts.rs を新設し load_fonts_dir(dir) で .ttf/.otf/.woff/.woff2 を再帰 walk
3) fulgur-wpt: runner/harness/render_test にバンドルを通す配管＋end-to-end 回帰テスト
```

仮説「`@font-face { src: url("/fonts/Ahem.ttf") }` の URL 解決が失敗しても、bundle に Ahem bytes が入っていれば `font-family: "Ahem"` の指定が Parley の FontContext.collection 経由で解決される」は **spike で検証済み**（Ahem バンドル有/無で PDF bytes が 7550→4004 と有意に変化）。

**Tech Stack:** Rust, fulgur `AssetBundle`, Parley FontContext, WPT `target/wpt/fonts/` sparse-checkout（`scripts/wpt/fetch.sh`）

---

## 制約と前提

- **fulgur core への追加は最小限**: `#[derive(Clone)]` の 1 行のみ。新しい公開 API は生やさない。
- **既存 API を使う**: `EngineBuilder::assets(AssetBundle)` と `AssetBundle::add_font_file(path)` はすでに存在する。
- **パフォーマンス**: フォント登録は runner の test ループ開始前に 1 回だけ実行する（170+ ファイル × テスト数の再読み込みを避ける）。per-test では `AssetBundle::clone()` で Arc を共有する。
- **fonts dir 欠落時の挙動**: `target/wpt/fonts/` が存在しない場合はログを出して空バンドルを返す（`wpt_root` 欠落と同じ "skip" 思想）。既存 `run_phase`/`run_list` は `wpt_root` 不在時に `Ok(None)` で抜けるので、実質は fetch 済みのとき以外は呼ばれないが防御的に実装する。
- **root-relative URL resolver は非対応**: `@font-face src: url("/fonts/...")` の path rewrite は本 issue では対応しない（issue fulgur-hxfo の明記事項、別 issue で検討）。
- **`render_test` のシグネチャ変更**: 現在 `(test_html_path, work_dir, dpi)`。`assets: Option<&AssetBundle>` を追加する。呼び出し元は `harness::run_one`（シグネチャ変更伝播）、`tests/render_multi_page.rs`、`examples/run_one.rs`、`examples/seed.rs` の 4 箇所。
- **検証の核**: 本プランの最終確認は `cargo test -p fulgur-wpt` の `wpt_list_*` / `wpt_css_*` jobs を CI で走らせて PASS 率の推移を観測すること。ローカルでも `wpt_root` を fetch 済みなら回帰しないことを確認する。

---

## Task 1: `fulgur` core に AssetBundle::Clone を追加

**Files:**

- Modify: `crates/fulgur/src/asset.rs:10` — `AssetBundle` 定義に `#[derive(Clone)]` を追加
- Modify: `crates/fulgur/src/asset.rs` tests セクション — clone 共有検証テストを追加

**Step 1: failing テストを追加**

`crates/fulgur/src/asset.rs` の `#[cfg(test)] mod tests` 末尾に次を追加する:

```rust
#[test]
fn clone_shares_font_arc() {
    use std::sync::Arc;
    let mut bundle = AssetBundle::new();
    let data = vec![0u8; 64];
    bundle.fonts.push(Arc::new(data));

    let cloned = bundle.clone();
    assert_eq!(bundle.fonts.len(), 1);
    assert_eq!(cloned.fonts.len(), 1);
    // Arc の共有を確認（同じヒープ上の Vec を指している）
    assert!(Arc::ptr_eq(&bundle.fonts[0], &cloned.fonts[0]));
}
```

**Step 2: テストが通らないことを確認**

```bash
cargo test -p fulgur --lib asset::tests::clone_shares_font_arc 2>&1 | tail -10
```

期待: `AssetBundle` が `Clone` を実装していないためコンパイルエラー（`the trait bound AssetBundle: Clone is not satisfied`）。

**Step 3: 最小実装**

`crates/fulgur/src/asset.rs:10` 付近の `pub struct AssetBundle` の定義直上に `#[derive(Clone)]` を追加する:

```rust
/// Collection of external assets (CSS, fonts, images) for PDF generation.
#[derive(Clone)]
pub struct AssetBundle {
    pub css: Vec<String>,
    pub fonts: Vec<Arc<Vec<u8>>>,
    pub images: HashMap<String, Arc<Vec<u8>>>,
}
```

`Vec<String>`, `Vec<Arc<Vec<u8>>>`, `HashMap<String, Arc<Vec<u8>>>` はいずれも `Clone` 済みなので自動導出で OK。

**Step 4: テストが通ることを確認**

```bash
cargo test -p fulgur --lib asset::tests::clone_shares_font_arc 2>&1 | tail -5
cargo test -p fulgur --lib 2>&1 | tail -5
```

期待: `clone_shares_font_arc ... ok`、全 ~340 件が引き続き PASS。

**Step 5: clippy / fmt**

```bash
cargo clippy -p fulgur --lib --tests 2>&1 | tail -10
cargo fmt --check
```

期待: warning/エラーなし。

**Step 6: コミット**

```bash
git add crates/fulgur/src/asset.rs
git commit -m "feat(fulgur): derive Clone for AssetBundle

AssetBundle の中身は Vec<Arc<...>> と HashMap<String, Arc<...>> で
既に cheap-clone の型で構成されている。fulgur-wpt が runner レベルで
1 回ロードしたフォント群を test ごとに複数の render_test 呼び出しへ
共有するために必要。"
```

---

## Task 2: `fulgur-wpt::fonts::load_fonts_dir` を実装

**Files:**

- Create: `crates/fulgur-wpt/src/fonts.rs` — ローダ本体
- Modify: `crates/fulgur-wpt/src/lib.rs` — `pub mod fonts;` を追加

**Step 1: failing テストを先に追加**

新規ファイル `crates/fulgur-wpt/src/fonts.rs` を次の skeleton で作成する:

```rust
//! Walk a directory tree and register every `.ttf`/`.otf`/`.woff`/`.woff2`
//! file into a fresh `AssetBundle`. Used by the WPT runner to shove
//! `target/wpt/fonts/` (Ahem, CSSTest, Lato, ...) into fulgur's Parley
//! FontContext so reftests that declare `@font-face { family: "Ahem"; ... }`
//! resolve by family name instead of falling back to system fonts.

use anyhow::{Context, Result};
use fulgur::asset::AssetBundle;
use std::path::Path;

pub fn load_fonts_dir(dir: &Path) -> Result<AssetBundle> {
    let mut bundle = AssetBundle::new();
    if !dir.is_dir() {
        log::debug!(
            "load_fonts_dir: {} not a directory, returning empty bundle",
            dir.display()
        );
        return Ok(bundle);
    }
    walk(dir, &mut bundle)?;
    Ok(bundle)
}

fn walk(dir: &Path, bundle: &mut AssetBundle) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read_dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    // 決定性のため sort（Vec<Arc<Vec<u8>>> の登録順が PDF 出力に影響する可能性）
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, bundle)?;
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase());
        match ext.as_deref() {
            Some("ttf" | "otf" | "woff" | "woff2") => {
                if let Err(e) = bundle.add_font_file(&path) {
                    log::warn!("load_fonts_dir: skipping {}: {e}", path.display());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    /// Minimal valid TTF header: 0x00010000 magic + zero-filled rest.
    /// `AssetBundle::add_font_file` accepts Unknown/TTF/OTF/TTC bytes as-is
    /// (magic is inspected but not validated beyond format detection), so
    /// a synthetic header is enough to exercise the walker.
    fn write_fake_ttf(dir: &Path, name: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(&[0x00, 0x01, 0x00, 0x00]).unwrap();
        f.write_all(&[0u8; 64]).unwrap();
    }

    #[test]
    fn missing_dir_returns_empty_bundle() {
        let bundle = load_fonts_dir(Path::new("/definitely/does/not/exist")).unwrap();
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn empty_dir_returns_empty_bundle() {
        let tmp = tempdir().unwrap();
        let bundle = load_fonts_dir(tmp.path()).unwrap();
        assert_eq!(bundle.fonts.len(), 0);
    }

    #[test]
    fn loads_ttf_files() {
        let tmp = tempdir().unwrap();
        write_fake_ttf(tmp.path(), "a.ttf");
        write_fake_ttf(tmp.path(), "b.ttf");
        let bundle = load_fonts_dir(tmp.path()).unwrap();
        assert_eq!(bundle.fonts.len(), 2);
    }

    #[test]
    fn ignores_non_font_extensions() {
        let tmp = tempdir().unwrap();
        write_fake_ttf(tmp.path(), "a.ttf");
        std::fs::write(tmp.path().join("README.md"), b"ignore me").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"also ignore").unwrap();
        let bundle = load_fonts_dir(tmp.path()).unwrap();
        assert_eq!(bundle.fonts.len(), 1);
    }

    #[test]
    fn recurses_into_subdirs() {
        let tmp = tempdir().unwrap();
        let sub = tmp.path().join("CSSTest");
        std::fs::create_dir(&sub).unwrap();
        write_fake_ttf(tmp.path(), "Ahem.ttf");
        write_fake_ttf(&sub, "csstest-ascii.ttf");
        let bundle = load_fonts_dir(tmp.path()).unwrap();
        assert_eq!(bundle.fonts.len(), 2);
    }
}
```

**Step 2: lib.rs への公開**

`crates/fulgur-wpt/src/lib.rs` の最上位 `pub mod ...` 群に次を追加する:

```rust
pub mod fonts;
```

**Step 3: `log` 依存の確認**

`crates/fulgur-wpt/Cargo.toml` の `[dependencies]` に `log` が無ければ追加する（workspace に `log` が登録されていないなら `log = "0.4"`）。現状未使用なので追加が必要。

確認:

```bash
grep -n '^log' crates/fulgur-wpt/Cargo.toml 2>&1
cargo tree -p fulgur-wpt --no-default-features 2>&1 | grep -E '^log ' | head -3
```

`log` が dependencies に無ければ次の行を `Cargo.toml` の `[dependencies]` 末尾に追加:

```toml
log = "0.4"
```

**Step 4: 単体テストを走らせる**

```bash
cargo test -p fulgur-wpt --lib fonts 2>&1 | tail -20
```

期待: 5 テストすべて PASS。

**Step 5: clippy / fmt**

```bash
cargo clippy -p fulgur-wpt --lib --tests 2>&1 | tail -10
cargo fmt --check
```

期待: warning/エラーなし。

**Step 6: コミット**

```bash
git add crates/fulgur-wpt/src/fonts.rs crates/fulgur-wpt/src/lib.rs crates/fulgur-wpt/Cargo.toml
git commit -m "feat(fulgur-wpt): add fonts::load_fonts_dir loader

target/wpt/fonts/ のような再帰ディレクトリを walk し、.ttf/.otf/.woff/.woff2
を AssetBundle::add_font_file 経由で登録するヘルパ。ディレクトリ欠落時は
警告ログを出して空バンドルを返す（既存の wpt_root 欠落ポリシーに揃える）。
登録順は決定性のため sort で固定する。"
```

---

## Task 3: render 経路にバンドルを通して WPT runner でロードする

**Files:**

- Modify: `crates/fulgur-wpt/src/render.rs` — `render_test` に `assets: Option<AssetBundle>` 引数を追加
- Modify: `crates/fulgur-wpt/src/harness.rs` — `run_one` に `assets: Option<&AssetBundle>` 引数を追加し、`render_test` に clone して渡す
- Modify: `crates/fulgur-wpt/src/runner.rs` — `execute_and_report` で test ループ前に `fonts::load_fonts_dir(&wpt_root.join("fonts"))` を 1 回呼び、`run_one` に `&bundle` を渡す
- Modify: `crates/fulgur-wpt/tests/render_multi_page.rs` — 呼び出しに `None` を追加
- Modify: `crates/fulgur-wpt/examples/run_one.rs` — `run_one` 呼び出しに `None` を追加
- Modify: `crates/fulgur-wpt/examples/seed.rs` — `run_one` 呼び出しに `None` を追加
- Create: `crates/fulgur-wpt/tests/fonts_integration.rs` — end-to-end 回帰テスト

**Step 1: failing 統合テストを先に作成**

新規ファイル `crates/fulgur-wpt/tests/fonts_integration.rs`:

```rust
//! End-to-end regression: confirm that a bundled Ahem.ttf changes the
//! rendering output for an HTML that declares `@font-face family:"Ahem"`.
//! Skipped when `target/wpt/fonts/Ahem.ttf` is not fetched.

use fulgur::asset::AssetBundle;
use fulgur::engine::Engine;
use std::path::PathBuf;

const HTML: &str = r#"<!DOCTYPE html>
<html><head><style>
@font-face { font-family: "Ahem"; src: url("/fonts/Ahem.ttf"); }
p { font-size: 40px; font-family: "Ahem"; }
</style></head>
<body><p>XXXX</p></body></html>
"#;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn ahem_bundle_differs_from_no_bundle() {
    let ahem = workspace_root().join("target/wpt/fonts/Ahem.ttf");
    if !ahem.exists() {
        eprintln!("skip: {} missing (run scripts/wpt/fetch.sh)", ahem.display());
        return;
    }
    let bundle = fulgur_wpt::fonts::load_fonts_dir(&workspace_root().join("target/wpt/fonts"))
        .expect("load fonts");
    assert!(
        !bundle.fonts.is_empty(),
        "fonts dir must contain at least one font"
    );

    let pdf_none = Engine::builder()
        .build()
        .render_html(HTML)
        .expect("render without bundle");
    let pdf_with = Engine::builder()
        .assets(bundle)
        .build()
        .render_html(HTML)
        .expect("render with bundle");

    assert_ne!(
        pdf_none, pdf_with,
        "PDFs identical — bundled Ahem is NOT being resolved for \
         `font-family:\"Ahem\"`. Re-scope: @font-face URL rewrite required."
    );
}
```

**Step 2: テストが通らないことを確認**

```bash
cargo test -p fulgur-wpt --test fonts_integration 2>&1 | tail -10
```

期待: `fulgur_wpt::fonts` は Task 2 で追加済み、`Engine::builder().assets(bundle)` は既存なので **実はテストは通る可能性がある** — これは期待される結果（Task 2 だけで spike 相当の end-to-end が動く）。ただし実際の runner がフォントを登録していないので、本来の目的は次の Step 以降の wiring。

もしこの時点でテストが通る場合: OK、wiring 実装へ進む。通らない場合は理由を確認（例: Ahem.ttf が fetch されていない → skip する / `fonts` モジュールの signature ミス → 修正）。

**Step 3: `render_test` の signature を変更**

`crates/fulgur-wpt/src/render.rs` の `render_test`:

```rust
pub fn render_test(
    test_html_path: &Path,
    work_dir: &Path,
    dpi: u32,
    assets: Option<&fulgur::asset::AssetBundle>,
) -> Result<RenderedTest> {
    // ... 既存の std::fs / canonicalize / html 読み込み ...

    let mut builder = Engine::builder().base_path(base);
    if let Some(b) = assets {
        builder = builder.assets(b.clone());
    }
    let engine = builder.build();

    // ... 既存の render_html 以降はそのまま ...
}
```

**Step 4: `run_one` の signature を変更**

`crates/fulgur-wpt/src/harness.rs::run_one`:

```rust
pub fn run_one(
    test_html_path: &Path,
    work_dir: &Path,
    diff_out_dir: &Path,
    dpi: u32,
    assets: Option<&fulgur::asset::AssetBundle>,
) -> Result<RunOutcome> {
    // ... 既存の classify 等そのまま ...

    let test_out = render_test(test_html_path, &test_work, dpi, assets)?;
    let ref_out = render_test(&ref_abs, &ref_work, dpi, assets)?;

    // ... 既存の page 比較そのまま ...
}
```

**Step 5: `runner::execute_and_report` でバンドルをロード**

`crates/fulgur-wpt/src/runner.rs` の `execute_and_report` 関数の `let wpt_root = workspace_root.join("target/wpt");` の直後に:

```rust
let fonts_bundle = crate::fonts::load_fonts_dir(&wpt_root.join("fonts"))
    .unwrap_or_else(|e| {
        log::warn!("fonts loader failed: {e}; proceeding without bundled fonts");
        fulgur::asset::AssetBundle::new()
    });
let fonts_arg = if fonts_bundle.fonts.is_empty() {
    None
} else {
    Some(&fonts_bundle)
};
```

test ループ内の `run_one(test, &work, &diff, dpi)` を次に差し替える:

```rust
let outcome = catch_unwind(AssertUnwindSafe(|| {
    run_one(test, &work, &diff, dpi, fonts_arg)
}));
```

（`fonts_arg` は `Option<&AssetBundle>`、`run_one` が `&AssetBundle` を要求するので `.as_ref()` 不要。ただし `fonts_arg` のライフタイム上はループ外で stack-bound になっている）

**Step 6: 非 runner 呼び出し元の更新**

- `crates/fulgur-wpt/tests/render_multi_page.rs:46`:

  ```rust
  let out = render_test(&html_path, &work, 96, None).expect("render should succeed");
  ```

- `crates/fulgur-wpt/examples/run_one.rs:48`:

  ```rust
  let outcome = run_one(&test_path, &work_dir, &diff_dir, dpi, None)?;
  ```

- `crates/fulgur-wpt/examples/seed.rs:66`:

  ```rust
  run_one(test, &work as &Path, &diff as &Path, 96, None)
  ```

**Step 7: ビルドと既存テスト**

```bash
cargo check -p fulgur-wpt --tests --examples 2>&1 | tail -10
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur-wpt --lib 2>&1 | tail -10
cargo test -p fulgur-wpt --test harness_smoke --test render_multi_page 2>&1 | tail -10
```

期待: すべて PASS、シグネチャ変更を反映した呼び出しがコンパイルを通る。

**Step 8: 統合テスト**

```bash
cargo test -p fulgur-wpt --test fonts_integration -- --nocapture 2>&1 | tail -10
```

期待: `ahem_bundle_differs_from_no_bundle ... ok` または Ahem.ttf 未 fetch 時は skip メッセージ。

**Step 9: 実 runner の smoke**

`target/wpt/` が fetch 済みなら、runner 経由で少なくとも 1 つの Ahem 依存テストの挙動を確認する:

```bash
cargo test -p fulgur-wpt --test wpt_css_page -- --nocapture 2>&1 | tail -15
```

ログに "fonts loaded N" 相当のノイズが無いことと、run_phase が PASS/FAIL を返すことを確認（harness panic が増えていなければ OK）。

**Step 10: clippy / fmt**

```bash
cargo clippy -p fulgur-wpt --tests --examples 2>&1 | tail -10
cargo fmt --check
```

期待: warning/エラーなし。

**Step 11: コミット**

```bash
git add crates/fulgur-wpt/src/render.rs crates/fulgur-wpt/src/harness.rs crates/fulgur-wpt/src/runner.rs \
  crates/fulgur-wpt/tests/render_multi_page.rs crates/fulgur-wpt/tests/fonts_integration.rs \
  crates/fulgur-wpt/examples/run_one.rs crates/fulgur-wpt/examples/seed.rs
git commit -m "feat(fulgur-wpt): register target/wpt/fonts/ for every test render

Ahem や CSSTest のフォントに依存する reftest が Parley の system font
fallback に流れて誤描画するのを防ぐため、runner の execute_and_report で
test ループ開始前に fonts::load_fonts_dir(target/wpt/fonts) を 1 回実行し、
得られた AssetBundle を Option<&AssetBundle> として render_test まで
propagate する。AssetBundle は Arc-shared clone なので per-test の clone は
cheap。fonts_integration テストで Ahem バンドル有無での PDF 差分を回帰確認する。

@font-face src url 解決（root-relative /fonts/...）は本 issue では対応せず、
family 名 fallback のみでカバーする（fulgur-hxfo の明記事項）。"
```

---

## 完了確認

**Final verification:**

```bash
cargo clippy -p fulgur -p fulgur-wpt --tests --examples 2>&1 | tail -5
cargo fmt --check
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur-wpt 2>&1 | tail -20
npx markdownlint-cli2 'docs/plans/2026-04-25-wpt-fonts-registration.md' 2>&1 | tail -5
```

期待:

- clippy: warning なし
- fmt: diff なし
- `fulgur --lib`: ~340 PASS
- `fulgur-wpt`: harness_smoke / render_multi_page / fonts_integration / wpt_lists_*（target/wpt あるなら wpt_list_* も）全 PASS
- markdownlint: 本プランにエラーなし

**追加の観測（CI で追う）:**

- `wpt-*` job の `target/wpt-report/<label>/summary.md` に書かれる PASS/FAIL/SKIP の数値推移
- 既存の FAIL-expectations が PASS に promote される場合は `target/wpt-report/<label>/promotions.json` に出る（expectations 更新は follow-up issue で対応、本 plan の scope 外）

**Follow-up（本 issue では対応しない）:**

- `@font-face src: url("/fonts/...")` の root-relative URL resolver
- font 管理 CLI subcommand（diagnose/bundle）
- Ahem 以外で host fallback に負けている family の網羅調査
- WPT expectations の PASS promotion コミット
