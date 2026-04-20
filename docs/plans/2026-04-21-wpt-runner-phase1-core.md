# WPT Runner Phase 1 Core Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** W3C web-platform-tests の CSS paged media 系 reftest を fulgur で走らせる自前 runner (`crates/fulgur-wpt/`) の中核部分 (scaffolding + WPT fetch + render + reftest parser + diff harness + expectations) を作る。

**Architecture:** 新規 crate `crates/fulgur-wpt/` (publish=false) に Blitz の WPT runner を参考にした薄いレイヤーを実装。test と ref を両方 fulgur で PDF 化して pdftocairo で全ページ PNG に展開し、`fulgur-vrt::diff` を dev-dep で再利用して比較。fuzzy meta は仕様 5 バリアント全対応。expectations は Blitz の `wpt_expectations.txt` 形式を踏襲して PASS/FAIL/SKIP を宣言管理する。

**Tech Stack:** Rust (edition 2024), `fulgur` (依存), `fulgur-vrt` (dev-dep, diff 再利用), `image`, `anyhow`, `tempfile`, `scraper` または `kuchikiki` (HTML parse for reftest metadata), `pdftocairo` (CLI, PDF→PNG), `git` (WPT sparse-checkout)

**Beads issues covered:** fulgur-2foo.1, .2, .3, .4, .5, .6

**Out of scope (2nd PR):** fulgur-2foo.7 (wptreport.json), .8 (css-page seed), .9 (CI integration)

---

## 実装順序の要点

- TDD で順番を組み替え: parser (fuzzy / reftest meta / expectations) を先に単体テストで固めてから、render → harness に進める
- 各タスクは fresh subagent で独立実装可能、タスク間でコードレビュー
- `fulgur-vrt` の `diff.rs` と `pdf_render.rs` を参考にし、必要なら薄く wrap (DRY)
- 全タスクで `cargo test -p fulgur-wpt` が pass すること + `cargo fmt --check` / `cargo clippy -- -D warnings` が通ること

---

## Task 1: fulgur-wpt crate scaffolding

**Beads:** fulgur-2foo.1

**Files:**

- Create: `crates/fulgur-wpt/Cargo.toml`
- Create: `crates/fulgur-wpt/README.md`
- Create: `crates/fulgur-wpt/src/lib.rs`
- Create: `crates/fulgur-wpt/tests/wpt_smoke.rs`
- Modify: `Cargo.toml` (workspace members に追加)

**Step 1: Workspace に member を追加**

`Cargo.toml` の `members` に `"crates/fulgur-wpt"` を末尾に追加:

```toml
members = ["crates/fulgur", "crates/fulgur-cli", "crates/fulgur-ruby", "crates/fulgur-vrt", "crates/fulgur-wpt", "crates/pyfulgur"]
```

**Step 2: Cargo.toml を作成**

```toml
[package]
name = "fulgur-wpt"
version = "0.0.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish = false
description = "W3C web-platform-tests runner for fulgur (dev-only, not published)"

[dependencies]
fulgur = { path = "../fulgur" }
image = { version = "0.25", default-features = false, features = ["png"] }
anyhow = "1"
scraper = "0.20"

[dev-dependencies]
fulgur-vrt = { path = "../fulgur-vrt" }
tempfile = "3"
```

**Step 3: README.md を作成 (責務分担を記載)**

```markdown
# fulgur-wpt

W3C web-platform-tests (WPT) の CSS paged media 系サブセット reftest を fulgur で走らせる自前ランナー。

## 他 crate との責務分担

| crate | 役割 |
|---|---|
| `fulgur` | HTML → PDF 本体 |
| `fulgur-vrt` | 手書きフィクスチャの visual regression, ゆるい tolerance |
| `fulgur-wpt` | 外部 WPT reftest, WPT 規約準拠 (fuzzy meta, rel=match 等) |

diff ロジックは `fulgur-vrt::diff` を dev-dep 経由で再利用する (Rule of Three 未達のため共有 crate は切り出さない)。

## 使い方

詳細は epic fulgur-2foo と `docs/plans/2026-04-21-wpt-reftest-runner-design.md` を参照。
```

**Step 4: src/lib.rs を空で作成**

```rust
//! WPT reftest runner for fulgur. See README for scope.
```

**Step 5: tests/wpt_smoke.rs を作成**

```rust
#[test]
fn crate_builds_and_links() {
    // Placeholder smoke test: ensures the crate is wired into the workspace
    // and `cargo test -p fulgur-wpt` runs at all. Replaced by real tests once
    // modules are filled in.
    assert_eq!(2 + 2, 4);
}
```

**Step 6: Build と test で pass を確認**

```bash
cd <worktree-root>
cargo build --workspace
cargo test -p fulgur-wpt
```

期待: `1 passed; 0 failed`。

**Step 7: Commit**

```bash
git add Cargo.toml crates/fulgur-wpt
git commit -m "feat(fulgur-wpt): add crate scaffolding (2foo.1)"
```

---

## Task 2: WPT fetch script + SHA pin + subset

**Beads:** fulgur-2foo.2

**Files:**

- Create: `scripts/wpt/fetch.sh`
- Create: `scripts/wpt/pinned_sha.txt`
- Create: `scripts/wpt/subset.txt`
- Create: `scripts/wpt/README.md`
- Modify: `.gitignore` (target/wpt/ を無視)

**Step 1: subset.txt を作成 (support も含める)**

```text
# WPT subset needed for fulgur-wpt. Keep in sync with sparse-checkout.
# Phase 1 targets css-page; later phases extend below.
css/css-page
css/css-break
css/css-gcpm
css/css-multicol
css/support
css/fonts
css/CSS2/support
fonts
resources
```

**Step 2: pinned_sha.txt を作成**

```text
# WPT upstream commit SHA pinned for fulgur-wpt reproducibility.
# Update via PR. Current pin: 2026-04-21 (HEAD at pin time).
# Verify before bumping: scripts/wpt/fetch.sh && cargo test -p fulgur-wpt
# TBD: set actual SHA in the PR that first wires up Phase 1.
DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF
```

(実 SHA は PR で上流 WPT `main` の最新安定 commit に差し替える。このタスクではフォーマット確立まで。)

**Step 3: fetch.sh を作成 (冪等、bash strict mode)**

```bash
#!/usr/bin/env bash
# Shallow-clone WPT upstream and sparse-checkout only the paths needed
# by fulgur-wpt. Idempotent: re-running updates to the pinned SHA.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WPT_DIR="$REPO_ROOT/target/wpt"
SHA_FILE="$SCRIPT_DIR/pinned_sha.txt"
SUBSET_FILE="$SCRIPT_DIR/subset.txt"
REMOTE_URL="${WPT_REMOTE_URL:-https://github.com/web-platform-tests/wpt.git}"

SHA="$(grep -v '^#' "$SHA_FILE" | head -n1 | tr -d '[:space:]')"
if [ -z "$SHA" ]; then
  echo "error: no SHA in $SHA_FILE" >&2
  exit 1
fi

if [ ! -d "$WPT_DIR/.git" ]; then
  mkdir -p "$WPT_DIR"
  git -C "$WPT_DIR" init -q
  git -C "$WPT_DIR" remote add origin "$REMOTE_URL"
  git -C "$WPT_DIR" config core.sparseCheckout true
  git -C "$WPT_DIR" config extensions.partialClone origin
fi

# Write sparse-checkout patterns (strip comments and blanks)
mkdir -p "$WPT_DIR/.git/info"
grep -v '^#' "$SUBSET_FILE" | sed '/^[[:space:]]*$/d' > "$WPT_DIR/.git/info/sparse-checkout"

# Fetch only the pinned SHA, filter=blob:none to keep it lean
git -C "$WPT_DIR" fetch --depth=1 --filter=blob:none origin "$SHA"
git -C "$WPT_DIR" checkout -q --detach FETCH_HEAD

echo "WPT ready at $WPT_DIR (SHA: $SHA)"
```

`chmod +x scripts/wpt/fetch.sh` を忘れないこと。

**Step 4: scripts/wpt/README.md を作成**

````markdown
# scripts/wpt/

`fetch.sh` executes a sparse, shallow clone of the W3C web-platform-tests
repository into `target/wpt/`, pinned to the SHA in `pinned_sha.txt`.

The set of fetched paths is controlled by `subset.txt` (one pattern per line,
Git sparse-checkout syntax). Keep in sync with the subset `fulgur-wpt`
actually exercises.

## Usage

```bash
scripts/wpt/fetch.sh
```

Idempotent: re-running updates to the current pinned SHA. Override the remote
URL with `WPT_REMOTE_URL=...` (useful for mirrors or CI cache warmup).

## Updating the pin

1. Inspect upstream WPT `main` and pick a commit that is green on the
   relevant subsections.
2. Replace the SHA line in `pinned_sha.txt`.
3. Re-run `scripts/wpt/fetch.sh` and `cargo test -p fulgur-wpt`.
4. Commit both in one PR.

````

**Step 5: .gitignore に target/wpt を追加**

`.gitignore` に以下を追加 (既存の `target/` 行で既にカバーされていれば不要、要確認):

```text
# (target/ is already ignored; no change needed if so)
```

**Step 6: スクリプト構文チェック**

```bash
bash -n scripts/wpt/fetch.sh
```

期待: エラーなし (実クローンは Task 8 以降のタスクで実行)。

**Step 7: Commit**

```bash
git add scripts/wpt
git commit -m "feat(wpt): add WPT shallow-clone fetch script with SHA pin (2foo.2)"
```

---

## Task 3: Fuzzy tolerance parser

**Beads:** fulgur-2foo.4 (fuzzy 部分)

**Files:**

- Create: `crates/fulgur-wpt/src/reftest.rs`
- Modify: `crates/fulgur-wpt/src/lib.rs`

**Step 1: reftest.rs に FuzzyTolerance 型と parse_fuzzy() の unit test を書く**

```rust
// crates/fulgur-wpt/src/reftest.rs
use anyhow::{Result, bail};
use std::ops::RangeInclusive;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyTolerance {
    pub url: Option<PathBuf>,
    pub max_diff: RangeInclusive<u8>,
    pub total_pixels: RangeInclusive<u32>,
}

impl FuzzyTolerance {
    /// Permissive tolerance (max_diff 0-255, total_pixels 0-u32::MAX).
    /// Used when a test declares no fuzzy meta.
    pub fn any() -> Self {
        Self {
            url: None,
            max_diff: 0..=255,
            total_pixels: 0..=u32::MAX,
        }
    }
}

/// Parse a WPT `<meta name=fuzzy content=...>` value into a canonical
/// `FuzzyTolerance`. Accepts every variant from the WPT reftest spec:
///
/// - numeric: `10;300`, `5-10;200-300`
/// - named:   `maxDifference=10;totalPixels=300`, or named + range
/// - url prefix: `ref.html:10-15;200-300`
/// - open range: `5-`, `-300`
pub fn parse_fuzzy(src: &str) -> Result<FuzzyTolerance> {
    todo!("implement after writing tests")
}

#[cfg(test)]
mod fuzzy_tests {
    use super::*;

    #[test]
    fn plain_numeric() {
        let t = parse_fuzzy("10;300").unwrap();
        assert_eq!(t.url, None);
        assert_eq!(t.max_diff, 10..=10);
        assert_eq!(t.total_pixels, 300..=300);
    }

    #[test]
    fn numeric_range_both() {
        let t = parse_fuzzy("5-10;200-300").unwrap();
        assert_eq!(t.max_diff, 5..=10);
        assert_eq!(t.total_pixels, 200..=300);
    }

    #[test]
    fn named_single() {
        let t = parse_fuzzy("maxDifference=10;totalPixels=300").unwrap();
        assert_eq!(t.max_diff, 10..=10);
        assert_eq!(t.total_pixels, 300..=300);
    }

    #[test]
    fn named_range() {
        let t = parse_fuzzy("maxDifference=5-10;totalPixels=200-300").unwrap();
        assert_eq!(t.max_diff, 5..=10);
        assert_eq!(t.total_pixels, 200..=300);
    }

    #[test]
    fn url_prefix() {
        let t = parse_fuzzy("ref.html:10-15;200-300").unwrap();
        assert_eq!(t.url.as_deref().map(|p| p.to_str().unwrap()), Some("ref.html"));
        assert_eq!(t.max_diff, 10..=15);
        assert_eq!(t.total_pixels, 200..=300);
    }

    #[test]
    fn open_range_lower_only() {
        let t = parse_fuzzy("5-;200-").unwrap();
        assert_eq!(t.max_diff, 5..=255);
        assert_eq!(t.total_pixels, 200..=u32::MAX);
    }

    #[test]
    fn open_range_upper_only() {
        let t = parse_fuzzy("-10;-300").unwrap();
        assert_eq!(t.max_diff, 0..=10);
        assert_eq!(t.total_pixels, 0..=300);
    }

    #[test]
    fn whitespace_tolerated() {
        let t = parse_fuzzy("  10 ; 300  ").unwrap();
        assert_eq!(t.max_diff, 10..=10);
        assert_eq!(t.total_pixels, 300..=300);
    }

    #[test]
    fn rejects_missing_semicolon() {
        assert!(parse_fuzzy("10").is_err());
    }

    #[test]
    fn rejects_reversed_range() {
        assert!(parse_fuzzy("10-5;300").is_err());
    }

    #[test]
    fn rejects_max_diff_over_255() {
        assert!(parse_fuzzy("256;300").is_err());
    }
}
```

lib.rs に `pub mod reftest;` を追加。

**Step 2: Run test to verify they fail**

```bash
cargo test -p fulgur-wpt fuzzy_
```

期待: 全テスト compile は通り、runtime で `todo!()` により panic する (FAIL 扱い)。

**Step 3: parse_fuzzy() の実装を書く**

```rust
pub fn parse_fuzzy(src: &str) -> Result<FuzzyTolerance> {
    let src = src.trim();

    // URL prefix: split at first ':' if present AND the prefix doesn't
    // contain '=' or ';' (those belong to value syntax, not URL)
    let (url, body) = match src.find(':') {
        Some(idx)
            if !src[..idx].contains('=') && !src[..idx].contains(';') && !src[..idx].is_empty() =>
        {
            let (u, rest) = src.split_at(idx);
            (Some(PathBuf::from(u.trim())), &rest[1..])
        }
        _ => (None, src),
    };

    // Split into two halves at ';'
    let mut parts = body.split(';');
    let first = parts.next().ok_or_else(|| anyhow::anyhow!("empty fuzzy"))?;
    let second = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing ';' in fuzzy: {src}"))?;
    if parts.next().is_some() {
        bail!("too many ';' in fuzzy: {src}");
    }

    let (k1, v1) = split_named(first.trim());
    let (k2, v2) = split_named(second.trim());

    let (max_diff_src, total_src) = match (k1, k2) {
        (Some("maxDifference"), Some("totalPixels")) | (None, None) => (v1, v2),
        (Some("totalPixels"), Some("maxDifference")) => (v2, v1),
        (Some(k), _) => bail!("unknown fuzzy key: {k}"),
        (_, Some(k)) => bail!("unknown fuzzy key: {k}"),
    };

    let max_diff = parse_u8_range(max_diff_src)?;
    let total_pixels = parse_u32_range(total_src)?;
    Ok(FuzzyTolerance {
        url,
        max_diff,
        total_pixels,
    })
}

fn split_named(s: &str) -> (Option<&str>, &str) {
    match s.find('=') {
        Some(idx) => (Some(&s[..idx]), &s[idx + 1..]),
        None => (None, s),
    }
}

fn parse_u8_range(src: &str) -> Result<RangeInclusive<u8>> {
    let src = src.trim();
    let (lo, hi) = parse_range(src, 0u32, 255u32)?;
    if lo > hi {
        bail!("reversed range: {src}");
    }
    if hi > 255 {
        bail!("max_diff out of u8 range: {src}");
    }
    Ok((lo as u8)..=(hi as u8))
}

fn parse_u32_range(src: &str) -> Result<RangeInclusive<u32>> {
    let src = src.trim();
    let (lo, hi) = parse_range(src, 0u32, u32::MAX)?;
    if lo > hi {
        bail!("reversed range: {src}");
    }
    Ok(lo..=hi)
}

fn parse_range(src: &str, default_lo: u32, default_hi: u32) -> Result<(u32, u32)> {
    match src.find('-') {
        None => {
            let n: u32 = src.parse()?;
            Ok((n, n))
        }
        Some(0) => {
            // "-N" → 0..=N
            let n: u32 = src[1..].trim().parse()?;
            Ok((default_lo, n))
        }
        Some(idx) if idx == src.len() - 1 => {
            // "N-" → N..=MAX
            let n: u32 = src[..idx].trim().parse()?;
            Ok((n, default_hi))
        }
        Some(idx) => {
            let lo: u32 = src[..idx].trim().parse()?;
            let hi: u32 = src[idx + 1..].trim().parse()?;
            Ok((lo, hi))
        }
    }
}
```

**Step 4: Run tests to verify PASS**

```bash
cargo test -p fulgur-wpt fuzzy_
```

期待: 11 passed; 0 failed.

**Step 5: Commit**

```bash
git add crates/fulgur-wpt/src
git commit -m "feat(fulgur-wpt): add fuzzy meta parser with full WPT variant coverage (2foo.4)"
```

---

## Task 4: Reftest HTML parser (rel=match + meta fuzzy extraction)

**Beads:** fulgur-2foo.4 (reftest HTML 部分)

**Files:**

- Modify: `crates/fulgur-wpt/src/reftest.rs`

**Step 1: 拡張型とテストを書く**

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Reftest {
    pub test: PathBuf,
    pub classification: ReftestKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReftestKind {
    /// Single rel=match + optional fuzzy tolerance. Phase 1 target.
    Match {
        ref_path: PathBuf,
        fuzzy: FuzzyTolerance,
    },
    /// Skipped: out-of-scope reftest variant.
    Skip { reason: SkipReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Mismatch,
    MultipleMatches,
    NoMatch,
    ChainedReference,
}

/// Inspect the test HTML at `test_path` and classify it for Phase 1.
/// File I/O only — does not render.
pub fn classify(test_path: &Path) -> Result<Reftest> {
    todo!()
}
```

テスト (reftest_tests モジュール):

```rust
#[cfg(test)]
mod reftest_tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        (dir, p)
    }

    #[test]
    fn single_match_no_fuzzy() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html><link rel="match" href="t-ref.html"><body></body>"#,
        );
        let r = classify(&p).unwrap();
        match r.classification {
            ReftestKind::Match { ref_path, fuzzy } => {
                assert_eq!(ref_path.file_name().unwrap(), "t-ref.html");
                assert_eq!(fuzzy, FuzzyTolerance::any());
            }
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn single_match_with_fuzzy() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="t-ref.html">
<meta name="fuzzy" content="5-10;200-300">
<body></body>"#,
        );
        let r = classify(&p).unwrap();
        let fuzzy = match r.classification {
            ReftestKind::Match { fuzzy, .. } => fuzzy,
            _ => unreachable!(),
        };
        assert_eq!(fuzzy.max_diff, 5..=10);
        assert_eq!(fuzzy.total_pixels, 200..=300);
    }

    #[test]
    fn multiple_matches_skip() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="a.html">
<link rel="match" href="b.html">
<body></body>"#,
        );
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip { reason: SkipReason::MultipleMatches }
        ));
    }

    #[test]
    fn mismatch_skip() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html><link rel="mismatch" href="a.html"><body></body>"#,
        );
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip { reason: SkipReason::Mismatch }
        ));
    }

    #[test]
    fn no_match_skip() {
        let (_d, p) = write_tmp("t.html", r#"<!DOCTYPE html><body></body>"#);
        assert!(matches!(
            classify(&p).unwrap().classification,
            ReftestKind::Skip { reason: SkipReason::NoMatch }
        ));
    }

    #[test]
    fn fuzzy_url_prefix_matching_ref_is_used() {
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="t-ref.html">
<meta name="fuzzy" content="t-ref.html:5-10;200-300">
<body></body>"#,
        );
        let r = classify(&p).unwrap();
        match r.classification {
            ReftestKind::Match { fuzzy, .. } => {
                assert_eq!(fuzzy.max_diff, 5..=10);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn fuzzy_url_prefix_mismatched_is_ignored() {
        // Prefix points at a different ref → Phase 1 falls back to permissive
        let (_d, p) = write_tmp(
            "t.html",
            r#"<!DOCTYPE html>
<link rel="match" href="t-ref.html">
<meta name="fuzzy" content="other.html:5-10;200-300">
<body></body>"#,
        );
        let r = classify(&p).unwrap();
        match r.classification {
            ReftestKind::Match { fuzzy, .. } => {
                assert_eq!(fuzzy, FuzzyTolerance::any());
            }
            _ => unreachable!(),
        }
    }
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur-wpt reftest_
```

期待: `todo!()` で panic。

**Step 3: classify() を実装**

```rust
pub fn classify(test_path: &Path) -> Result<Reftest> {
    let html = std::fs::read_to_string(test_path)?;
    let doc = scraper::Html::parse_document(&html);

    let link_sel = scraper::Selector::parse("link[rel]").unwrap();
    let mut matches: Vec<PathBuf> = Vec::new();
    let mut has_mismatch = false;

    for el in doc.select(&link_sel) {
        let rel = el.value().attr("rel").unwrap_or("").to_ascii_lowercase();
        match rel.as_str() {
            "match" => {
                if let Some(href) = el.value().attr("href") {
                    matches.push(PathBuf::from(href));
                }
            }
            "mismatch" => {
                has_mismatch = true;
            }
            _ => {}
        }
    }

    if has_mismatch {
        return Ok(Reftest {
            test: test_path.to_path_buf(),
            classification: ReftestKind::Skip { reason: SkipReason::Mismatch },
        });
    }
    let ref_path = match matches.as_slice() {
        [] => return Ok(Reftest {
            test: test_path.to_path_buf(),
            classification: ReftestKind::Skip { reason: SkipReason::NoMatch },
        }),
        [one] => one.clone(),
        _ => return Ok(Reftest {
            test: test_path.to_path_buf(),
            classification: ReftestKind::Skip { reason: SkipReason::MultipleMatches },
        }),
    };

    // Collect fuzzy metas
    let meta_sel = scraper::Selector::parse(r#"meta[name="fuzzy"]"#).unwrap();
    let mut chosen = FuzzyTolerance::any();
    let mut unscoped_count = 0usize;
    for el in doc.select(&meta_sel) {
        let content = match el.value().attr("content") {
            Some(c) => c,
            None => continue,
        };
        let parsed = match parse_fuzzy(content) {
            Ok(p) => p,
            Err(_) => continue, // ignore malformed, leave permissive
        };
        match &parsed.url {
            Some(u) if u == &ref_path => {
                chosen = parsed;
            }
            None => {
                if unscoped_count == 0 || !matches!(chosen.url, None) {
                    chosen = parsed;
                } else {
                    // multiple unscoped — take last (Phase 1 rule)
                    chosen = parsed;
                }
                unscoped_count += 1;
            }
            _ => {} // different url prefix: ignore
        }
    }

    Ok(Reftest {
        test: test_path.to_path_buf(),
        classification: ReftestKind::Match { ref_path, fuzzy: chosen },
    })
}
```

**Step 4: Tests should pass**

```bash
cargo test -p fulgur-wpt reftest_
```

期待: 7 passed; 0 failed.

**Step 5: Commit**

```bash
git add crates/fulgur-wpt/src/reftest.rs
git commit -m "feat(fulgur-wpt): classify reftests from HTML (rel=match, fuzzy meta) (2foo.4)"
```

---

## Task 5: Multi-page render module

**Beads:** fulgur-2foo.3

**Files:**

- Create: `crates/fulgur-wpt/src/render.rs`
- Modify: `crates/fulgur-wpt/src/lib.rs`

**Step 1: API と failing test を書く**

```rust
// crates/fulgur-wpt/src/render.rs
//! Render a WPT test HTML through fulgur and rasterize every page via
//! pdftocairo. CRITICAL: must not pass `-f 1 -l 1` to pdftocairo — we
//! need every page to catch multi-page regressions (advisor P1-1).

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct RenderedTest {
    pub pages: Vec<RgbaImage>,
    pub pdf_path: PathBuf,
}

/// Render `test_html` and return one RgbaImage per page.
///
/// `base_path` is the directory the test HTML lives in, used to resolve
/// support/ and CSS links. `work_dir` receives the PDF and per-page PNGs
/// (left behind for debugging).
pub fn render_test(test_html_path: &Path, work_dir: &Path, dpi: u32) -> Result<RenderedTest> {
    todo!()
}
```

lib.rs に `pub mod render;` を追加。

テストは実 fulgur を噛ませるため、integration style (tests/) に置く:

```rust
// crates/fulgur-wpt/tests/render_multi_page.rs
use fulgur_wpt::render::render_test;
use std::io::Write;

#[test]
fn renders_two_pages_from_page_break() {
    let html = r#"<!DOCTYPE html>
<html><head><style>
  @page { size: 200px 200px; margin: 0; }
  .p { page-break-after: always; width: 100px; height: 100px; background: red; }
</style></head>
<body>
  <div class="p"></div>
  <div style="width:100px;height:100px;background:blue"></div>
</body></html>"#;

    let dir = tempfile::tempdir().unwrap();
    let html_path = dir.path().join("t.html");
    std::fs::File::create(&html_path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    let work = dir.path().join("work");
    let out = render_test(&html_path, &work, 96).expect("render should succeed");
    assert_eq!(out.pages.len(), 2, "expected 2 pages");
    assert!(out.pages[0].width() > 100);
}
```

**Step 2: Run to verify fail**

```bash
cargo test -p fulgur-wpt --test render_multi_page
```

期待: `todo!()` panic.

**Step 3: 実装**

```rust
pub fn render_test(test_html_path: &Path, work_dir: &Path, dpi: u32) -> Result<RenderedTest> {
    use fulgur::engine::Engine;

    std::fs::create_dir_all(work_dir)?;
    let html = std::fs::read_to_string(test_html_path)
        .with_context(|| format!("read {}", test_html_path.display()))?;
    let base = test_html_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test has no parent dir: {}", test_html_path.display()))?;

    let engine = Engine::builder().base_path(base).build();
    let pdf_bytes = engine
        .render_html(&html)
        .map_err(|e| anyhow::anyhow!("fulgur render_html failed: {e}"))?;

    let pdf_path = work_dir.join("fixture.pdf");
    std::fs::write(&pdf_path, &pdf_bytes)?;

    let prefix = work_dir.join("page");
    // NOTE: intentionally NOT passing -f/-l so pdftocairo emits every page.
    let status = Command::new("pdftocairo")
        .args(["-png", "-r", &dpi.to_string()])
        .arg(&pdf_path)
        .arg(&prefix)
        .status()
        .context("spawn pdftocairo")?;
    if !status.success() {
        bail!("pdftocairo exited with {status}");
    }

    // Enumerate generated files: pdftocairo names them `<prefix>-<n>.png`
    // (or `<prefix>-01.png` if >9 pages). We glob, sort, and load.
    let parent = work_dir;
    let stem = prefix
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("bad prefix"))?
        .to_string_lossy()
        .into_owned();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(parent)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            name.starts_with(&format!("{stem}-")) && name.ends_with(".png")
        })
        .collect();
    entries.sort();

    if entries.is_empty() {
        bail!("pdftocairo produced no PNGs in {}", work_dir.display());
    }

    let pages = entries
        .iter()
        .map(|p| image::open(p).map(|i| i.to_rgba8()).map_err(Into::into))
        .collect::<Result<Vec<_>>>()?;

    Ok(RenderedTest { pages, pdf_path })
}
```

**Step 4: Test に poppler ガード (skip-if-missing)**

pdftocairo が無い CI 環境向けに、先頭で probe:

```rust
#[test]
fn renders_two_pages_from_page_break() {
    if std::process::Command::new("pdftocairo")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("skip: pdftocairo not available");
        return;
    }
    // ... 既存テスト本体
}
```

**Step 5: Tests pass**

```bash
cargo test -p fulgur-wpt --test render_multi_page
```

期待: 1 passed (or "skip: pdftocairo not available" でも OK)。

**Step 6: Commit**

```bash
git add crates/fulgur-wpt
git commit -m "feat(fulgur-wpt): add multi-page render with pdftocairo (2foo.3)"
```

---

## Task 6: Expectations file I/O

**Beads:** fulgur-2foo.6

**Files:**

- Create: `crates/fulgur-wpt/src/expectations.rs`
- Modify: `crates/fulgur-wpt/src/lib.rs`

**Step 1: 型と failing test を書く**

```rust
// crates/fulgur-wpt/src/expectations.rs
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expectation {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Clone, Default)]
pub struct ExpectationFile {
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone)]
struct Entry {
    expectation: Expectation,
    comment: Option<String>,
}

impl ExpectationFile {
    pub fn parse(src: &str) -> Result<Self> {
        todo!()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::parse(&s)
    }

    pub fn get(&self, test_path: &str) -> Option<Expectation> {
        self.entries.get(test_path).map(|e| e.expectation)
    }

    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Regression,
    Promotion,
    Skipped,
    UnknownTest,
}

/// Compare the declared expectation with the observed result.
pub fn judge(declared: Option<Expectation>, observed: Option<Expectation>) -> Verdict {
    match (declared, observed) {
        (Some(Expectation::Skip), _) => Verdict::Skipped,
        (Some(d), Some(o)) if d == o => Verdict::Ok,
        (Some(Expectation::Pass), Some(Expectation::Fail)) => Verdict::Regression,
        (Some(Expectation::Fail), Some(Expectation::Pass)) => Verdict::Promotion,
        (None, _) => Verdict::UnknownTest,
        _ => Verdict::Ok,
    }
}
```

テスト (同ファイル内 `mod tests`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_three_statuses() {
        let src = "\
# comment line
PASS css/css-page/a.html
FAIL css/css-page/b.html  # reason
SKIP css/css-page/c.html  # manual

";
        let f = ExpectationFile::parse(src).unwrap();
        assert_eq!(f.get("css/css-page/a.html"), Some(Expectation::Pass));
        assert_eq!(f.get("css/css-page/b.html"), Some(Expectation::Fail));
        assert_eq!(f.get("css/css-page/c.html"), Some(Expectation::Skip));
        assert_eq!(f.len(), 3);
    }

    #[test]
    fn ignores_blank_and_comment_lines() {
        let src = "\n  \n# foo\nPASS x\n";
        let f = ExpectationFile::parse(src).unwrap();
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn rejects_unknown_status() {
        assert!(ExpectationFile::parse("XYZZY a.html\n").is_err());
    }

    #[test]
    fn rejects_duplicate_entry() {
        let src = "PASS a.html\nFAIL a.html\n";
        assert!(ExpectationFile::parse(src).is_err());
    }

    #[test]
    fn judge_pass_pass_is_ok() {
        assert_eq!(
            judge(Some(Expectation::Pass), Some(Expectation::Pass)),
            Verdict::Ok
        );
    }

    #[test]
    fn judge_pass_fail_is_regression() {
        assert_eq!(
            judge(Some(Expectation::Pass), Some(Expectation::Fail)),
            Verdict::Regression
        );
    }

    #[test]
    fn judge_fail_pass_is_promotion() {
        assert_eq!(
            judge(Some(Expectation::Fail), Some(Expectation::Pass)),
            Verdict::Promotion
        );
    }

    #[test]
    fn judge_skip_wins() {
        assert_eq!(
            judge(Some(Expectation::Skip), Some(Expectation::Pass)),
            Verdict::Skipped
        );
    }

    #[test]
    fn judge_unknown_declared() {
        assert_eq!(judge(None, Some(Expectation::Pass)), Verdict::UnknownTest);
    }
}
```

lib.rs に `pub mod expectations;` 追加。

**Step 2: Run — expect fail**

```bash
cargo test -p fulgur-wpt expectations::
```

**Step 3: parse() 実装**

```rust
impl ExpectationFile {
    pub fn parse(src: &str) -> Result<Self> {
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for (lineno, raw) in src.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (body, comment) = match line.find('#') {
                Some(idx) => (line[..idx].trim(), Some(line[idx + 1..].trim().to_string())),
                None => (line, None),
            };
            let mut parts = body.splitn(2, char::is_whitespace);
            let status = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("").trim();
            if path.is_empty() {
                bail!("line {}: missing path", lineno + 1);
            }
            let expectation = match status {
                "PASS" => Expectation::Pass,
                "FAIL" => Expectation::Fail,
                "SKIP" => Expectation::Skip,
                other => bail!("line {}: unknown status {other}", lineno + 1),
            };
            if entries.contains_key(path) {
                bail!("line {}: duplicate entry for {path}", lineno + 1);
            }
            entries.insert(
                path.to_string(),
                Entry { expectation, comment },
            );
        }
        Ok(Self { entries })
    }
}
```

**Step 4: Tests pass**

```bash
cargo test -p fulgur-wpt expectations::
```

期待: 15 passed.

**Step 5: Commit**

```bash
git add crates/fulgur-wpt
git commit -m "feat(fulgur-wpt): add expectations file parser and judge() (2foo.6)"
```

---

## Task 7: Diff wrapper

**Beads:** fulgur-2foo.5 (diff 部分)

**Files:**

- Create: `crates/fulgur-wpt/tests/diff_pages.rs`
- (fulgur-vrt は dev-dep で十分。ラッパ module は作らず、harness.rs 内で直接呼ぶ)

実装の重複を避けるため、薄いラッパは作らない。`fulgur-vrt::diff::compare` を fuzzy tolerance にアダプトする関数は Task 8 (harness.rs) で登場する。このタスクでは dev-dep 経由で diff が呼べることを統合テストで確認するだけ。

**Step 1: tests/diff_pages.rs に fulgur-vrt 経由の diff が動くことを確認するテスト**

```rust
use fulgur_vrt::diff::compare;
use fulgur_vrt::manifest::Tolerance;
use image::{ImageBuffer, Rgba};

#[test]
fn fulgur_vrt_diff_is_reachable_as_devdep() {
    let a: image::RgbaImage = ImageBuffer::from_pixel(4, 4, Rgba([0, 0, 0, 255]));
    let b = a.clone();
    let tol = Tolerance { max_channel_diff: 0, max_diff_pixels_ratio: 0.0 };
    let r = compare(&a, &b, tol);
    assert!(r.pass);
}
```

**Step 2: Run**

```bash
cargo test -p fulgur-wpt --test diff_pages
```

期待: 1 passed.

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/tests/diff_pages.rs
git commit -m "test(fulgur-wpt): verify fulgur-vrt diff reachable as dev-dep (2foo.5)"
```

---

## Task 8: Harness — end-to-end reftest judgement

**Beads:** fulgur-2foo.5

**Files:**

- Create: `crates/fulgur-wpt/src/harness.rs`
- Modify: `crates/fulgur-wpt/src/lib.rs`
- Create: `crates/fulgur-wpt/tests/harness_smoke.rs`

**Step 1: 型と API**

```rust
// crates/fulgur-wpt/src/harness.rs
use crate::expectations::Expectation;
use crate::reftest::{FuzzyTolerance, classify, ReftestKind, SkipReason};
use crate::render::render_test;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct RunOutcome {
    pub observed: Expectation,
    pub reason: Option<String>,
    pub diff_dir: Option<PathBuf>,
}

/// Run one reftest and return an observed PASS/FAIL/SKIP.
///
/// - `test_html_path`: absolute or relative path to the test HTML.
/// - `work_dir`: scratch dir for PDFs/PNGs.
/// - `diff_out_dir`: where to dump per-page diff PNGs on failure.
pub fn run_one(
    test_html_path: &Path,
    work_dir: &Path,
    diff_out_dir: &Path,
    dpi: u32,
) -> Result<RunOutcome> {
    todo!()
}
```

lib.rs に `pub mod harness;` 追加。

**Step 2: smoke integration test — 2 ページ test/ref が一致する自作ケース**

```rust
// crates/fulgur-wpt/tests/harness_smoke.rs
use fulgur_wpt::expectations::Expectation;
use fulgur_wpt::harness::run_one;
use std::io::Write;

fn poppler_available() -> bool {
    std::process::Command::new("pdftocairo").arg("-v").output().is_ok()
}

#[test]
fn identical_test_and_ref_pass() {
    if !poppler_available() { eprintln!("skip: no pdftocairo"); return; }

    let dir = tempfile::tempdir().unwrap();
    let test = dir.path().join("t.html");
    let refh = dir.path().join("t-ref.html");

    let shared = r#"<!DOCTYPE html>
<html><head><style>
  @page { size: 200px 200px; margin: 0; }
  .p { width: 100px; height: 100px; background: green; page-break-after: always; }
</style></head>
<body>
  <div class="p"></div>
  <div style="width:100px;height:100px;background:orange"></div>
</body></html>"#;

    // Test HTML links to ref
    let test_body = format!(r#"<!DOCTYPE html><link rel="match" href="t-ref.html"><meta name="fuzzy" content="0-2;0-500">{shared}"#);

    std::fs::File::create(&test).unwrap().write_all(test_body.as_bytes()).unwrap();
    std::fs::File::create(&refh).unwrap().write_all(shared.as_bytes()).unwrap();

    let work = dir.path().join("work");
    let diff = dir.path().join("diff");
    let out = run_one(&test, &work, &diff, 96).unwrap();
    assert_eq!(out.observed, Expectation::Pass, "reason: {:?}", out.reason);
}

#[test]
fn page_count_mismatch_fails() {
    if !poppler_available() { eprintln!("skip: no pdftocairo"); return; }

    let dir = tempfile::tempdir().unwrap();
    let test = dir.path().join("t.html");
    let refh = dir.path().join("t-ref.html");

    let test_body = r#"<!DOCTYPE html>
<link rel="match" href="t-ref.html">
<style>@page{size:200px 200px;margin:0}.p{width:100px;height:100px;background:green;page-break-after:always}</style>
<div class="p"></div>
<div class="p"></div>
<div style="width:100px;height:100px"></div>"#;
    let ref_body = r#"<!DOCTYPE html>
<style>@page{size:200px 200px;margin:0}.p{width:100px;height:100px;background:green;page-break-after:always}</style>
<div class="p"></div>
<div style="width:100px;height:100px"></div>"#;

    std::fs::File::create(&test).unwrap().write_all(test_body.as_bytes()).unwrap();
    std::fs::File::create(&refh).unwrap().write_all(ref_body.as_bytes()).unwrap();

    let work = dir.path().join("work");
    let diff = dir.path().join("diff");
    let out = run_one(&test, &work, &diff, 96).unwrap();
    assert_eq!(out.observed, Expectation::Fail);
    assert!(out.reason.as_deref().unwrap_or("").contains("page count"));
}

#[test]
fn skipped_reftest_reports_skip() {
    let dir = tempfile::tempdir().unwrap();
    let test = dir.path().join("t.html");
    std::fs::File::create(&test).unwrap()
        .write_all(br#"<!DOCTYPE html><link rel="mismatch" href="x.html">"#).unwrap();

    let work = dir.path().join("work");
    let diff = dir.path().join("diff");
    let out = run_one(&test, &work, &diff, 96).unwrap();
    assert_eq!(out.observed, Expectation::Skip);
}
```

**Step 3: Run — fail**

```bash
cargo test -p fulgur-wpt --test harness_smoke
```

期待: `todo!()` panic.

**Step 4: run_one() 実装**

```rust
pub fn run_one(
    test_html_path: &Path,
    work_dir: &Path,
    diff_out_dir: &Path,
    dpi: u32,
) -> Result<RunOutcome> {
    use fulgur_vrt::diff::{compare, write_diff_image};
    use fulgur_vrt::manifest::Tolerance;

    let reftest = classify(test_html_path)?;
    let (ref_rel, fuzzy) = match reftest.classification {
        ReftestKind::Match { ref_path, fuzzy } => (ref_path, fuzzy),
        ReftestKind::Skip { reason } => {
            return Ok(RunOutcome {
                observed: Expectation::Skip,
                reason: Some(format!("{:?}", reason)),
                diff_dir: None,
            });
        }
    };

    let test_dir = test_html_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test has no parent"))?;
    let ref_abs = test_dir.join(&ref_rel);

    let test_work = work_dir.join("test");
    let ref_work = work_dir.join("ref");
    let test_out = render_test(test_html_path, &test_work, dpi)?;
    let ref_out = render_test(&ref_abs, &ref_work, dpi)?;

    if test_out.pages.len() != ref_out.pages.len() {
        return Ok(RunOutcome {
            observed: Expectation::Fail,
            reason: Some(format!(
                "page count mismatch: test={} ref={}",
                test_out.pages.len(),
                ref_out.pages.len(),
            )),
            diff_dir: None,
        });
    }

    // Map fuzzy→fulgur-vrt Tolerance. fulgur-vrt uses a single threshold,
    // WPT uses inclusive ranges; we take the upper bound (most permissive).
    let max_ch = *fuzzy.max_diff.end();
    let max_total = *fuzzy.total_pixels.end();

    let mut first_failure: Option<String> = None;
    for (idx, (t, r)) in test_out.pages.iter().zip(ref_out.pages.iter()).enumerate() {
        let total = u64::from(t.width()) * u64::from(t.height());
        let ratio_limit = if total == 0 {
            0.0
        } else {
            (max_total as f64 / total as f64) as f32
        };
        let tol = Tolerance {
            max_channel_diff: max_ch,
            max_diff_pixels_ratio: ratio_limit,
        };
        let report = compare(r, t, tol);
        if !report.pass {
            std::fs::create_dir_all(diff_out_dir)?;
            let out = diff_out_dir.join(format!("page{}.diff.png", idx + 1));
            write_diff_image(r, t, tol, &out)?;
            if first_failure.is_none() {
                first_failure = Some(format!(
                    "page {} diff: {}/{} pixels exceed tol (max_ch={})",
                    idx + 1,
                    report.diff_pixels,
                    report.total_pixels,
                    report.max_channel_diff,
                ));
            }
        }
    }

    Ok(match first_failure {
        Some(reason) => RunOutcome {
            observed: Expectation::Fail,
            reason: Some(reason),
            diff_dir: Some(diff_out_dir.to_path_buf()),
        },
        None => RunOutcome {
            observed: Expectation::Pass,
            reason: None,
            diff_dir: None,
        },
    })
}
```

**Step 5: Tests pass**

```bash
cargo test -p fulgur-wpt --test harness_smoke
```

期待: 3 passed (poppler 無ければ 3 skipped-via-early-return)。

**Step 6: Commit**

```bash
git add crates/fulgur-wpt
git commit -m "feat(fulgur-wpt): end-to-end reftest harness with multi-page diff (2foo.5)"
```

---

## Task 9: 全テスト再実行 + clippy + fmt の最終ゲート

**Step 1: 全体テスト**

```bash
cargo test -p fulgur-wpt
```

期待: 全 unit test + integration test が PASS (pdftocairo 無し環境では該当 2 件が skip)。

**Step 2: Workspace build/test 回帰なし**

```bash
cargo test -p fulgur --lib
cargo test -p fulgur-vrt --lib
```

期待: 既存テスト数と同数 PASS。

**Step 3: Lint**

```bash
cargo fmt --check
cargo clippy -p fulgur-wpt -- -D warnings
```

期待: 両方 clean。

**Step 4: Markdown lint**

```bash
npx markdownlint-cli2 'docs/plans/2026-04-21-wpt-runner-phase1-core.md' 'crates/fulgur-wpt/README.md' 'scripts/wpt/README.md'
```

期待: 0 errors。

**Step 5: Squash commit 不要、タスク毎の commit をそのまま残す**

各 task の commit をそのまま feature branch に残す (レビュアが追いやすい)。

---

## 実装時のプランからの差分

- `FuzzyTolerance::any()` を `strict()` にリネームし、デフォルト値を `max_diff=0..=0, total_pixels=0..=0` に修正。WPT 仕様では fuzzy meta 無し = strict 一致のため。
- `scraper` を 0.20 → 0.26 に bump（Task 4 直前）。API は後方互換。
- `judge(observed: Option<Expectation>, ...)` を `judge(observed: Expectation, ...)` に締め付け、catch-all `_ => Ok` を排除して型で網羅性を強制。
- `fulgur-vrt` を `[dev-dependencies]` → `[dependencies]` に昇格。harness.rs が src 配下から `fulgur_vrt::diff` を使うため必須。両 crate とも `publish = false` で publish 汚染はゼロ。
- `render.rs` で `pdftocairo .status()` → `.output()` に変更し stderr をエラーに含める（2foo.8 triage 用）。
- `render_test` で test HTML path を `canonicalize` してから parent を取る（bare filename 時の base_path 空落ちを防止）。
- fetch.sh の remote URL 反映を毎回 `git remote set-url` で上書き（mirror override の冪等性）。
- `page-break-after: always` が fulgur 本体で未配線のため、multi-page render の smoke test は natural overflow で書いた。`page-break-*` 配線は別 beads issue で追跡予定。
- `rel=match` で href 欠落時はエラーに（final review 指摘、malformed reftest を silent NoMatch に落とさない）。

## Out of scope (別 PR で実装)

- **fulgur-2foo.7** `wptreport.json` 出力 (report.rs) — runner コアが動いてから
- **fulgur-2foo.8** css-page 全件 FAIL seed — fetch.sh 実行 + harness 実走が必要
- **fulgur-2foo.9** CI workflow 追加 — 本体が動いてから PR / nightly 分岐

## Known risks

1. **WPT 実ファイルを使わない smoke**: harness テストは自作 HTML で完結。実 WPT ファイルでの検証は 2foo.8 (seed) で行う
2. **pdftocairo 出力ファイル名の 2桁 padding**: 10ページ以上だと `page-01.png` にならず `page-01.png`/`page-10.png` で sort 順が崩れる可能性 — `std::fs::read_dir` + 自然順 sort が必要かは実測後判断。Phase 1 smoke は 2 ページで事足りるため初版は lexical sort で OK
3. **fulgur-vrt の Tolerance へのマッピング**: fuzzy range を単一閾値に丸めると false pass の余地 — WPT 公式 runner も likewise なので許容範囲。必要なら Phase 5 以降で再設計
