# WPT Runner Phase 1 Finish Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** WPT CSS paged media reftest runner の Phase 1 を完成させる — `wptreport.json` 出力、css-page 全 reftest を実走行した expectations の初期 seed、CI ワークフロー統合を入れる。

**Architecture:** Phase 1 core (PR #134) で完成した `crates/fulgur-wpt/` に `report.rs` を追加、`scripts/wpt/pinned_sha.txt` を実 SHA に差し替え、`scripts/wpt/seed.sh` で css-page を一括実行して `expectations/css-page.txt` を生成、`.github/workflows/ci.yml` に PR job、新規 `wpt-nightly.yml` に cron workflow を追加する。

**Tech Stack:** Rust (edition 2024), `fulgur-wpt` (依存), `anyhow`, `serde_json`, GitHub Actions, `gh` CLI, `pdftocairo` (poppler-utils)

**Beads issues covered:** fulgur-2foo.7, .8, .9

**Not in scope (別 issue で追跡):** fulgur-lje5 (fulgur page-break-after 未配線)

---

## 実装順序の要点

- Task 1 の `report.rs` は TDD で単体テスト駆動、実 WPT 不要
- Task 2 で実 SHA に差し替えてから Task 3 の seed 実行が可能 (順序依存)
- Task 3 の seed 実行は時間がかかる (~220 reftest × ~2 秒 ≒ 7 分程度)
- Task 4-5 (CI) は seed が済んだ expectations.txt を前提にワークフローを書く
- 各タスクで `cargo fmt --check` / `cargo clippy -p fulgur-wpt --all-targets -- -D warnings` が通ること

---

## Task 1: `report.rs` — wptreport.json エミッター

**Beads:** fulgur-2foo.7

**Files:**

- Create: `crates/fulgur-wpt/src/report.rs`
- Modify: `crates/fulgur-wpt/src/lib.rs` (`pub mod report;` を追加)
- Modify: `crates/fulgur-wpt/Cargo.toml` (`serde` + `serde_json` を `[dependencies]` に追加)

**Step 1: 依存を追加**

`crates/fulgur-wpt/Cargo.toml` の `[dependencies]` セクションに追加:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

**Step 2: 型と failing test を書く**

`crates/fulgur-wpt/src/report.rs` を作成:

```rust
//! wptreport.json emitter. Minimal schema compatible with upstream
//! wpt.fyi submission (a subset — we omit subtests, screenshots, logs).

use crate::expectations::Expectation;
use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct WptReport {
    pub results: Vec<TestResult>,
    pub run_info: RunInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestResult {
    pub test: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub subtests: Vec<serde_json::Value>, // reftest has no subtests
    pub duration: u64, // milliseconds
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RunInfo {
    pub product: String,
    pub revision: String,
}

impl WptReport {
    pub fn new(run_info: RunInfo) -> Self {
        Self {
            results: Vec::new(),
            run_info,
        }
    }

    pub fn push(
        &mut self,
        test: impl Into<String>,
        observed: Expectation,
        message: Option<String>,
        duration: Duration,
    ) {
        let status = match observed {
            Expectation::Pass => "PASS",
            Expectation::Fail => "FAIL",
            Expectation::Skip => "SKIP",
        };
        self.results.push(TestResult {
            test: test.into(),
            status,
            message,
            subtests: Vec::new(),
            duration: duration.as_millis() as u64,
        });
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_three_statuses_and_maps_to_strings() {
        let mut r = WptReport::new(RunInfo {
            product: "fulgur".into(),
            revision: "abc123".into(),
        });
        r.push("a.html", Expectation::Pass, None, Duration::from_millis(10));
        r.push(
            "b.html",
            Expectation::Fail,
            Some("diff 5".into()),
            Duration::from_millis(20),
        );
        r.push("c.html", Expectation::Skip, None, Duration::ZERO);
        assert_eq!(r.results.len(), 3);
        assert_eq!(r.results[0].status, "PASS");
        assert_eq!(r.results[1].status, "FAIL");
        assert_eq!(r.results[1].message.as_deref(), Some("diff 5"));
        assert_eq!(r.results[2].status, "SKIP");
    }

    #[test]
    fn skips_none_message_in_serialization() {
        let mut r = WptReport::new(RunInfo::default());
        r.push("a.html", Expectation::Pass, None, Duration::from_millis(5));
        let s = serde_json::to_string(&r).unwrap();
        assert!(!s.contains("\"message\""), "unexpected message field: {s}");
    }

    #[test]
    fn serializes_minimal_valid_schema() {
        let mut r = WptReport::new(RunInfo {
            product: "fulgur".into(),
            revision: "abc123".into(),
        });
        r.push(
            "css/css-page/basic.html",
            Expectation::Pass,
            None,
            Duration::from_millis(15),
        );
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(v["run_info"]["product"], "fulgur");
        assert_eq!(v["run_info"]["revision"], "abc123");
        assert_eq!(v["results"][0]["test"], "css/css-page/basic.html");
        assert_eq!(v["results"][0]["status"], "PASS");
        assert_eq!(v["results"][0]["duration"], 15);
        assert!(v["results"][0]["subtests"].is_array());
    }

    #[test]
    fn write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("nested").join("wptreport.json");
        let r = WptReport::new(RunInfo::default());
        r.write(&out).unwrap();
        assert!(out.exists());
        let contents = std::fs::read_to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert!(v["results"].is_array());
    }
}
```

Add `pub mod report;` to `crates/fulgur-wpt/src/lib.rs` after `pub mod harness;`.

**Step 3: Run tests — verify they compile but fail initially**

Since the implementation is already in the code above, they should actually pass. Run:

```bash
cargo test -p fulgur-wpt report:: 2>&1 | tail -10
```

Expected: `4 passed; 0 failed`.

**Step 4: Lint**

```bash
cargo fmt --check
cargo clippy -p fulgur-wpt --all-targets -- -D warnings
```

**Step 5: Commit**

```bash
git add crates/fulgur-wpt/src crates/fulgur-wpt/Cargo.toml Cargo.lock
git commit -m "feat(fulgur-wpt): add wptreport.json emitter (2foo.7)"
```

---

## Task 2: pinned_sha.txt を実 WPT SHA に差し替え

**Beads:** fulgur-2foo.8 (前提作業)

**Files:**

- Modify: `scripts/wpt/pinned_sha.txt`
- Modify: `scripts/wpt/README.md` (placeholder 文言を削除)

**Step 1: pinned_sha.txt 更新**

現在の `DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF` を **`97ea26e26a2aac3eec7e770650b25e7049ed4a4e`** に置換。これは 2026-04-21 時点の `web-platform-tests/wpt` の master HEAD で、PR 作成者が確認済みの SHA。

```text
# WPT upstream commit SHA pinned for fulgur-wpt reproducibility.
# Update via PR. Current pin: 2026-04-21 (master HEAD at pin time).
# Verify before bumping: scripts/wpt/fetch.sh && cargo test -p fulgur-wpt
97ea26e26a2aac3eec7e770650b25e7049ed4a4e
```

(元にあった `# TBD: set actual SHA ...` 行は削除)

**Step 2: README.md 更新**

`scripts/wpt/README.md` の placeholder note を削除:

```text
The placeholder SHA (`DEADBEEF…`) intentionally fails `git fetch`; a real SHA is committed in the PR that seeds fulgur-2foo.8.
```

これを以下に置換:

```text
The pin is manually bumped via PR (see "Updating the pin" below) and verified by re-running seed. Run `scripts/wpt/fetch.sh` to materialize the snapshot under `target/wpt/`.
```

**Step 3: Verify fetch works**

```bash
scripts/wpt/fetch.sh 2>&1 | tail -3
ls target/wpt/css/css-page/ | grep -c "\.html$"
```

Expected: `WPT ready at ...` and >=200 html files.

**Step 4: Lint (markdown)**

```bash
npx markdownlint-cli2 'scripts/wpt/README.md'
```

Expected: 0 errors.

**Step 5: Commit**

```bash
git add scripts/wpt
git commit -m "chore(wpt): pin to real WPT SHA 97ea26e2 (2foo.8 prep)"
```

---

## Task 3: seed script — css-page 全 reftest を走らせて expectations.txt を生成

**Beads:** fulgur-2foo.8 (主作業)

**Files:**

- Create: `crates/fulgur-wpt/examples/seed.rs`
- Create: `crates/fulgur-wpt/expectations/css-page.txt` (generated)
- Modify: `crates/fulgur-wpt/README.md` (seed ワークフロー追記)

**Step 1: `examples/seed.rs` を作成**

```rust
//! Walk a directory of WPT reftests, run each through the harness, and
//! emit an `expectations/<subdir>.txt` with the observed PASS/FAIL/SKIP.
//!
//! Usage:
//!     cargo run -p fulgur-wpt --example seed -- \
//!         --subdir css-page \
//!         --wpt-root target/wpt \
//!         --out crates/fulgur-wpt/expectations/css-page.txt
//!
//! The expectations format matches `ExpectationFile::parse()`.

use anyhow::{Context, Result, bail};
use fulgur_wpt::expectations::Expectation;
use fulgur_wpt::harness::run_one;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() -> Result<()> {
    let mut subdir: Option<String> = None;
    let mut wpt_root: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--subdir" => subdir = Some(args.next().context("--subdir needs value")?),
            "--wpt-root" => wpt_root = Some(PathBuf::from(args.next().context("--wpt-root needs value")?)),
            "--out" => out = Some(PathBuf::from(args.next().context("--out needs value")?)),
            other => bail!("unknown flag: {other}"),
        }
    }
    let subdir = subdir.context("--subdir required (e.g. css-page)")?;
    let wpt_root = wpt_root.context("--wpt-root required")?;
    let out = out.context("--out required")?;

    let dir = wpt_root.join("css").join(&subdir);
    anyhow::ensure!(dir.is_dir(), "not a directory: {}", dir.display());

    // Collect test HTMLs: files ending in .html that don't end in -ref.html or -notref.html
    let mut tests: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            name.ends_with(".html")
                && !name.ends_with("-ref.html")
                && !name.ends_with("-notref.html")
        })
        .collect();
    tests.sort();

    let mut results: BTreeMap<String, (Expectation, Option<String>)> = BTreeMap::new();
    let total = tests.len();
    println!("Running {total} reftests in {}", dir.display());
    let start = Instant::now();

    for (i, test) in tests.iter().enumerate() {
        let rel = test.strip_prefix(&wpt_root).unwrap_or(test).to_string_lossy().replace('\\', "/");
        let rel = format!("{rel}"); // already good on unix
        let stem = test.file_stem().unwrap().to_string_lossy();
        let work = PathBuf::from("target/wpt-seed").join(&*stem).join("work");
        let diff = PathBuf::from("target/wpt-seed").join(&*stem).join("diff");
        let outcome = match run_one(test, &work, &diff, 96) {
            Ok(o) => (o.observed, o.reason),
            Err(e) => (Expectation::Fail, Some(format!("harness error: {e}"))),
        };
        let status_str = match outcome.0 {
            Expectation::Pass => "PASS",
            Expectation::Fail => "FAIL",
            Expectation::Skip => "SKIP",
        };
        println!("[{:>3}/{}] {status_str}  {rel}", i + 1, total);
        results.insert(rel, outcome);
    }

    let elapsed = start.elapsed();
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut skip = 0u32;
    for (_, (e, _)) in &results {
        match e {
            Expectation::Pass => pass += 1,
            Expectation::Fail => fail += 1,
            Expectation::Skip => skip += 1,
        }
    }

    // Write expectations file
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(&out)?;
    writeln!(f, "# Auto-generated by `cargo run -p fulgur-wpt --example seed`.")?;
    writeln!(f, "# Summary: {pass} PASS, {fail} FAIL, {skip} SKIP (total {total}).")?;
    writeln!(f, "# Promote to PASS by editing this file in a PR after verifying the harness output.")?;
    writeln!(f)?;
    for (rel, (status, reason)) in &results {
        let status_str = match status {
            Expectation::Pass => "PASS",
            Expectation::Fail => "FAIL",
            Expectation::Skip => "SKIP",
        };
        match reason {
            Some(r) => {
                // single-line: strip newlines from reason
                let r1 = r.replace(['\n', '\r'], " ");
                writeln!(f, "{status_str:4}  {rel}  # {r1}")?;
            }
            None => writeln!(f, "{status_str:4}  {rel}")?,
        }
    }

    println!(
        "\nSeeded {} entries in {:.1}s: {pass} PASS, {fail} FAIL, {skip} SKIP",
        results.len(),
        elapsed.as_secs_f64()
    );
    println!("Wrote {}", out.display());
    Ok(())
}
```

**Step 2: Build**

```bash
cargo build -p fulgur-wpt --example seed 2>&1 | tail -3
```

Expected: `Finished`.

**Step 3: Run on real WPT css-page (timing: ~5-10 min)**

```bash
scripts/wpt/fetch.sh   # Task 2 でもう動いているはず
cargo run -p fulgur-wpt --example seed -- \
  --subdir css-page \
  --wpt-root target/wpt \
  --out crates/fulgur-wpt/expectations/css-page.txt 2>&1 | tail -20
```

Expected tail:

```text
[218/220] FAIL  css/css-page/some-test.html
...
Seeded 220 entries in XXs: N PASS, M FAIL, K SKIP
Wrote crates/fulgur-wpt/expectations/css-page.txt
```

**Step 4: Spot-check the expectations file**

```bash
head -15 crates/fulgur-wpt/expectations/css-page.txt
wc -l crates/fulgur-wpt/expectations/css-page.txt
```

Expected: header comment lines, then one entry per test.

**Step 5: Commit the seed result + example binary**

```bash
git add crates/fulgur-wpt/examples/seed.rs \
        crates/fulgur-wpt/expectations/css-page.txt
git commit -m "feat(fulgur-wpt): seed css-page expectations with initial harness run (2foo.8)"
```

---

## Task 4: README ドキュメント — expectations 運用フロー

**Beads:** fulgur-2foo.8 (docs)

**Files:**

- Modify: `crates/fulgur-wpt/README.md`

**Step 1: README 末尾に運用セクションを追加**

````markdown
## Expectations の運用

WPT の各 test は `crates/fulgur-wpt/expectations/<subdir>.txt` に `PASS` / `FAIL` / `SKIP` として宣言する。ハーネスは実行結果と宣言を突き合わせ、

- 宣言 PASS × 実測 FAIL → 回帰 (CI が落ちる)
- 宣言 FAIL × 実測 PASS → 昇格候補 (警告のみ、CI は落ちない)
- 宣言 SKIP → テスト実行スキップ

で判定する。

### 初期 seed

新しいサブディレクトリを追加するときは:

```bash
# まず WPT ソースを取得
scripts/wpt/fetch.sh

# 対象サブディレクトリを全件流して expectations を自動生成
cargo run -p fulgur-wpt --example seed -- \
  --subdir css-page \
  --wpt-root target/wpt \
  --out crates/fulgur-wpt/expectations/css-page.txt
```

生成された `expectations/<subdir>.txt` をコミット。以降この PR が reference point。

### PASS 昇格フロー

fulgur を改善して新しいテストが通るようになったら:

1. ローカルで `cargo run -p fulgur-wpt --example run_one -- <test-path>` を実行して PASS を確認
2. `crates/fulgur-wpt/expectations/<subdir>.txt` の該当行を `FAIL` → `PASS` に書き換え
3. 行末のコメント (`# reason: ...`) は削除してよい
4. PR 化、CI の `wpt-css-page` job が green であることを確認してマージ

### 既知の FAIL を一時的に無効化

テストが flaky だったり、fulgur 側の修正中で一時的に壊れている場合は `SKIP` に書き換えて理由をコメントで残す:

```text
SKIP  css/css-page/flaky-test.html  # flaky on low-DPI rendering, tracked in fulgur-xxx
```

原因追跡 issue を beads に起票して、修正後に `FAIL` か `PASS` に戻す。

````

**Step 2: Lint**

```bash
npx markdownlint-cli2 'crates/fulgur-wpt/README.md'
```

Expected: 0 errors.

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/README.md
git commit -m "docs(fulgur-wpt): document expectations seed and promotion workflow (2foo.8)"
```

---

## Task 5: CI — PR 用 `wpt-css-page` job

**Beads:** fulgur-2foo.9

**Files:**

- Modify: `.github/workflows/ci.yml` (new job `wpt-css-page`)

**Step 1: 新規 job を `jobs:` セクション末尾に追加**

`.github/workflows/ci.yml` の `jobs:` ブロックの末尾に以下を追加:

```yaml
  wpt-css-page:
    name: wpt / css-page
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install poppler-utils
        run: sudo apt-get update && sudo apt-get install -y poppler-utils
      - name: Fetch WPT subset
        run: scripts/wpt/fetch.sh
      - name: Run css-page reftests against expectations
        run: |
          cargo test -p fulgur-wpt --test wpt_css_page -- --nocapture
```

ただし `tests/wpt_css_page.rs` が存在しないのでもう 1 ステップ必要。

**Step 2: Integration test を作成**

`crates/fulgur-wpt/tests/wpt_css_page.rs` を作成:

```rust
//! Phase 1 entry point: run all css-page reftests and compare against expectations.
//! Skipped if `target/wpt/css/css-page/` is absent (typical `cargo test` runs).

use fulgur_wpt::expectations::{Expectation, ExpectationFile, Verdict, judge};
use fulgur_wpt::harness::run_one;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn wpt_root() -> PathBuf {
    PathBuf::from("target/wpt")
}

fn poppler_available() -> bool {
    std::process::Command::new("pdftocairo")
        .arg("-v")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn wpt_css_page_expectations_hold() {
    let dir = wpt_root().join("css/css-page");
    if !dir.is_dir() {
        eprintln!("skip: {} missing (run scripts/wpt/fetch.sh)", dir.display());
        return;
    }
    if !poppler_available() {
        eprintln!("skip: pdftocairo not available");
        return;
    }

    let expect_path = PathBuf::from("crates/fulgur-wpt/expectations/css-page.txt");
    let declared = ExpectationFile::load(&expect_path)
        .unwrap_or_else(|e| panic!("load {}: {e}", expect_path.display()));

    let mut tests: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            name.ends_with(".html")
                && !name.ends_with("-ref.html")
                && !name.ends_with("-notref.html")
        })
        .collect();
    tests.sort();

    let mut regressions: Vec<(String, String)> = Vec::new();
    let mut promotions: Vec<String> = Vec::new();
    let mut verdicts: BTreeMap<&'static str, u32> = BTreeMap::new();

    for test in &tests {
        let rel = test
            .strip_prefix(&wpt_root())
            .unwrap_or(test)
            .to_string_lossy()
            .replace('\\', "/");
        let declared_exp = declared.get(&rel);
        let stem = test.file_stem().unwrap().to_string_lossy();
        let work = PathBuf::from("target/wpt-run").join(&*stem).join("work");
        let diff = PathBuf::from("target/wpt-run").join(&*stem).join("diff");
        let observed = match run_one(test, &work, &diff, 96) {
            Ok(o) => o.observed,
            Err(_) => Expectation::Fail,
        };
        let verdict = judge(declared_exp, observed);
        let key = match verdict {
            Verdict::Ok => "ok",
            Verdict::Regression => "regression",
            Verdict::Promotion => "promotion",
            Verdict::Skipped => "skipped",
            Verdict::UnknownTest => "unknown",
        };
        *verdicts.entry(key).or_insert(0) += 1;

        match verdict {
            Verdict::Regression => {
                regressions.push((rel.clone(), format!("{:?}", observed)));
            }
            Verdict::Promotion => {
                promotions.push(rel.clone());
            }
            Verdict::UnknownTest => {
                eprintln!("warn: {rel} has no expectation entry (observed {:?})", observed);
            }
            _ => {}
        }
    }

    eprintln!("\n=== css-page verdicts ===");
    for (k, v) in &verdicts {
        eprintln!("  {k}: {v}");
    }
    if !promotions.is_empty() {
        eprintln!("\nPromotion candidates ({} tests now pass but declared FAIL):", promotions.len());
        for p in &promotions {
            eprintln!("  - {p}");
        }
        eprintln!("Edit expectations/css-page.txt to promote them.");
    }

    assert!(
        regressions.is_empty(),
        "regressions detected: {regressions:#?}"
    );
}
```

lib.rs 側の `judge` public 化は既存 (confirm)。

**Step 3: Confirm `judge` is exported**

```bash
grep -n "pub fn judge" crates/fulgur-wpt/src/expectations.rs
```

Expected: `pub fn judge(` is present.

**Step 4: CI yml に対応するステップを置換**

Step 1 で書いた yml 片の最後のコマンドを:

```yaml
      - name: Run css-page reftests against expectations
        run: cargo test -p fulgur-wpt --test wpt_css_page -- --nocapture
```

に確定。

**Step 5: Local test run (dry-run of what CI will do)**

```bash
scripts/wpt/fetch.sh
cargo test -p fulgur-wpt --test wpt_css_page -- --nocapture 2>&1 | tail -30
```

Expected: `=== css-page verdicts ===` + pass の assert 成功。

**Step 6: Commit**

```bash
git add .github/workflows/ci.yml \
        crates/fulgur-wpt/tests/wpt_css_page.rs
git commit -m "ci(wpt): add wpt-css-page job + integration test vs expectations (2foo.9)"
```

---

## Task 6: CI — nightly `wpt-nightly.yml`

**Beads:** fulgur-2foo.9

**Files:**

- Create: `.github/workflows/wpt-nightly.yml`

**Step 1: Workflow ファイル作成**

```yaml
name: WPT nightly

on:
  schedule:
    # 02:00 UTC daily (11:00 JST)
    - cron: '0 2 * * *'
  workflow_dispatch:

permissions:
  contents: read
  issues: write

jobs:
  wpt-full:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install poppler-utils
        run: sudo apt-get update && sudo apt-get install -y poppler-utils
      - name: Fetch WPT subset
        run: scripts/wpt/fetch.sh
      - name: Run css-page reftests
        id: run
        continue-on-error: true
        run: |
          cargo test -p fulgur-wpt --test wpt_css_page -- --nocapture 2>&1 | tee target/wpt-css-page.log
          echo "css_page_exit=${PIPESTATUS[0]}" >> "$GITHUB_OUTPUT"
      - name: Upload logs
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: wpt-logs
          path: |
            target/wpt-css-page.log
            target/wpt-run/**/diff/*.png
          if-no-files-found: ignore
      - name: File regression issue
        if: steps.run.outputs.css_page_exit != '0'
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          gh issue create \
            --title "WPT nightly regression $(date -u +%Y-%m-%d)" \
            --body "See run: $GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID" \
            --label "wpt-nightly-regression" || true
```

注意: 本 workflow が初回走るときには `wpt-nightly-regression` ラベルが未作成なので `|| true` でエラーを飲む。必要になれば後日ラベルを手動作成。

**Step 2: Lint YAML**

GitHub Actions は YAML 構文が厳格。ローカルで `python -c "import yaml; yaml.safe_load(open('.github/workflows/wpt-nightly.yml'))"` などでチェック。

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/wpt-nightly.yml'))" && echo "yaml OK"
```

Expected: `yaml OK`.

**Step 3: Commit**

```bash
git add .github/workflows/wpt-nightly.yml
git commit -m "ci(wpt): add nightly workflow with artifact + regression issue (2foo.9)"
```

---

## Task 7: 最終ゲート

**Step 1: Full workspace test**

```bash
cargo test -p fulgur-wpt 2>&1 | tail -15
cargo test -p fulgur --lib 2>&1 | tail -3
cargo test -p fulgur-vrt --lib 2>&1 | tail -3
```

全 green (baseline 回帰なし)。

**Step 2: Lint**

```bash
cargo fmt --check
cargo clippy -p fulgur-wpt --all-targets -- -D warnings
```

両方 clean。

**Step 3: Markdown lint**

```bash
npx markdownlint-cli2 \
  'docs/plans/2026-04-21-wpt-runner-phase1-finish.md' \
  'crates/fulgur-wpt/README.md' \
  'scripts/wpt/README.md'
```

Expected: 0 errors.

**Step 4: Squash commit 不要、task 毎の commit を残す**

各 task の commit を feature branch に残してレビュアが task 単位で読めるようにする。

---

## Known risks

1. **seed 実行時間**: 220 tests × 2-5 sec 想定で ~10 分。CI の `wpt-css-page` job は timeout を明示していないが、デフォルトの 6 時間でおさまるはず。万一遅い場合は nightly workflow に退避する選択肢あり
2. **fulgur のパース失敗**: Phase 1 の fulgur は WPT テストの一部を「HTML が正常に render できない」で Err → seed 側で `FAIL + harness error:` として扱うのですでに安全
3. **Flaky テスト**: rasterize の subpixel 差で flaky になる可能性。seed 実行を複数回比較して不安定なものは `SKIP` + 理由コメントで除外する必要があるかもしれない。初回 PR は一発 seed を commit し、flaky 検出は後追いで対処

## Out of scope (別 PR / 別 issue)

- Phase 2 以降の tracker issue (2foo.10-17) の着手
- fulgur の `page-break-after` 実装 (fulgur-lje5)
- `wpt.fyi` への report アップロード (本 PR では artifact upload のみ)
