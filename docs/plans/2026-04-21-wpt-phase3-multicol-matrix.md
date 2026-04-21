# WPT Phase 3 (css-multicol) + Matrix CI — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** WPT runner に Phase 3 (css-multicol) の baseline seed を追加し、CI を matrix 化して並列実行できるようにする。同時に WPT の regression を **CI blocker ではなくメトリック** として扱うよう test 側を reporting-only に改修する。

**Architecture:**

- 共通 harness を `crates/fulgur-wpt/src/runner.rs` に切り出し、phase 別 integration test は薄いエントリのみ
- integration test は `assert!(...)` せず、verdicts summary + wptreport.json + regressions.json を `target/wpt-report/<phase>/` に書き出す
- `.github/workflows/ci.yml` の `wpt-css-page` job を `wpt` matrix job (`[css-page, css-multicol]`) にリファクタ、step summary に coverage を出す
- `wpt-nightly.yml` も matrix 化 (同じ phase リスト + 将来 css-break 等)、nightly だけが regressions.json を読んで issue を起票

**Tech Stack:** Rust, `serde_json`, GitHub Actions `strategy.matrix`, `$GITHUB_STEP_SUMMARY`

**Beads issues covered:** fulgur-2foo.11 (Phase 3 css-multicol)

**Not in scope (別 PR):** fulgur-2foo.10 (css-break seed), fulgur-2foo.12 (css-gcpm, manual golden で別戦略), fulgur-lje5 (fulgur page-break-after 配線)

---

## 実装順序

- TDD 順: runner.rs の shared helper を先に固め、phase エントリは機械的
- css-multicol seed は runner + matrix ci が動く前提で実行 (~15 分)
- Task 5/6 の CI 変更は seed 後。integration test 自体は expectations 無しでも動くよう設計

各タスクで `cargo fmt --check` / `cargo clippy -p fulgur-wpt --all-targets -- -D warnings` clean。

---

## Task 1: 共通 runner helper を `src/runner.rs` に切り出す

**Beads:** fulgur-2foo.11 (infra)

**Files:**

- Create: `crates/fulgur-wpt/src/runner.rs`
- Modify: `crates/fulgur-wpt/src/lib.rs` (`pub mod runner;` 追加)

**Step 1: 既存 `tests/wpt_css_page.rs` から共通ロジックを抽出**

`runner.rs` に以下を定義:

```rust
//! Shared entry point for phase-specific WPT integration tests.
//!
//! A phase runner:
//!   1. Reads `target/wpt/css/<subdir>/` (WPT must be fetched first)
//!   2. Loads `crates/fulgur-wpt/expectations/<subdir>.txt` if present
//!   3. Runs every reftest through `harness::run_one` (panic-safe)
//!   4. Emits `target/wpt-report/<subdir>/` artifacts:
//!      - `report.json` (wptreport.json schema)
//!      - `regressions.json` (list of {test, observed_status, message})
//!      - `summary.md` (GitHub step summary block)
//!   5. Prints a one-line verdict summary to stderr
//!
//! **Never panics on regressions.** The caller (integration test) just calls
//! `run_phase(subdir)` and asserts nothing. Nightly workflow inspects
//! `regressions.json` separately.

use crate::expectations::{Expectation, ExpectationFile, Verdict, judge};
use crate::harness::run_one;
use crate::reftest::collect_reftest_files;
use crate::report::{RunInfo, WptReport};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
pub struct Regression {
    pub test: String,
    pub observed: String,
    pub declared: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PhaseOutcome {
    pub subdir: String,
    pub total: usize,
    pub pass: u32,
    pub fail: u32,
    pub skip: u32,
    pub regressions: Vec<Regression>,
    pub promotions: Vec<String>,
    pub unknown: Vec<String>,
    pub report_dir: PathBuf,
}

/// Run every reftest under `css/<subdir>` and write artifacts under
/// `target/wpt-report/<subdir>/`. Returns None if prerequisites are
/// missing (target/wpt not fetched, pdftocairo missing).
pub fn run_phase(workspace_root: &Path, subdir: &str, dpi: u32) -> Result<Option<PhaseOutcome>> {
    let wpt_root = workspace_root.join("target/wpt");
    let dir = wpt_root.join("css").join(subdir);
    if !dir.is_dir() {
        eprintln!(
            "skip: {} missing (run scripts/wpt/fetch.sh first)",
            dir.display()
        );
        return Ok(None);
    }
    if !poppler_available() {
        eprintln!("skip: pdftocairo not available on PATH");
        return Ok(None);
    }

    let expect_path = workspace_root
        .join("crates/fulgur-wpt/expectations")
        .join(format!("{subdir}.txt"));
    let declared = if expect_path.exists() {
        ExpectationFile::load(&expect_path)
            .with_context(|| format!("load {}", expect_path.display()))?
    } else {
        eprintln!("note: {} missing, treating every test as unknown", expect_path.display());
        ExpectationFile::default()
    };

    let tests = collect_reftest_files(&dir)
        .with_context(|| format!("collect reftest files in {}", dir.display()))?;
    let total = tests.len();

    let report_dir = workspace_root
        .join("target/wpt-report")
        .join(subdir);
    std::fs::create_dir_all(&report_dir)?;

    let mut report = WptReport::new(RunInfo {
        product: "fulgur".into(),
        revision: env_revision(),
    });
    let mut regressions: Vec<Regression> = Vec::new();
    let mut promotions: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut verdicts: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut skip = 0u32;
    let start = Instant::now();

    for test in &tests {
        let rel = test
            .strip_prefix(&wpt_root)
            .unwrap_or(test)
            .to_string_lossy()
            .replace('\\', "/");
        let stem = test.file_stem().unwrap().to_string_lossy();
        let work = workspace_root
            .join("target/wpt-run")
            .join(&*stem)
            .join("work");
        let diff = workspace_root
            .join("target/wpt-run")
            .join(&*stem)
            .join("diff");

        let t0 = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| run_one(test, &work, &diff, dpi)));
        let duration = t0.elapsed();
        let (observed, message) = match outcome {
            Ok(Ok(o)) => (o.observed, o.reason),
            Ok(Err(e)) => (
                Expectation::Fail,
                Some(format!("harness error: {e}")),
            ),
            Err(p) => {
                let msg = panic_message(&p);
                (Expectation::Fail, Some(format!("harness panic: {msg}")))
            }
        };
        match observed {
            Expectation::Pass => pass += 1,
            Expectation::Fail => fail += 1,
            Expectation::Skip => skip += 1,
        }
        // Emit harness-level errors as ERROR, visual fail as FAIL.
        let is_harness_error = message
            .as_deref()
            .is_some_and(|m| m.starts_with("harness "));
        if is_harness_error {
            report.push_error(rel.clone(), message.clone().unwrap_or_default(), duration);
        } else {
            report.push(rel.clone(), observed, message.clone(), duration);
        }

        let declared_exp = declared.get(&rel);
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
                regressions.push(Regression {
                    test: rel.clone(),
                    observed: fmt_expectation(observed),
                    declared: declared_exp
                        .map(fmt_expectation)
                        .unwrap_or_else(|| "UNKNOWN".into()),
                    message,
                });
            }
            Verdict::Promotion => promotions.push(rel.clone()),
            Verdict::UnknownTest => unknown.push(rel.clone()),
            _ => {}
        }
    }
    let elapsed = start.elapsed();

    // Write artifacts.
    report.write(&report_dir.join("report.json"))?;
    std::fs::write(
        report_dir.join("regressions.json"),
        serde_json::to_string_pretty(&regressions)?,
    )?;
    write_summary(&report_dir, subdir, total, pass, fail, skip, &regressions, &promotions, &unknown, elapsed)?;

    eprintln!(
        "wpt-{subdir}: total={total} pass={pass} fail={fail} skip={skip} regressions={} promotions={} unknown={} ({:.1}s)",
        regressions.len(),
        promotions.len(),
        unknown.len(),
        elapsed.as_secs_f64(),
    );

    Ok(Some(PhaseOutcome {
        subdir: subdir.to_string(),
        total,
        pass,
        fail,
        skip,
        regressions,
        promotions,
        unknown,
        report_dir,
    }))
}

fn poppler_available() -> bool {
    std::process::Command::new("pdftocairo")
        .arg("-v")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn fmt_expectation(e: Expectation) -> String {
    match e {
        Expectation::Pass => "PASS".into(),
        Expectation::Fail => "FAIL".into(),
        Expectation::Skip => "SKIP".into(),
    }
}

fn env_revision() -> String {
    std::env::var("GITHUB_SHA")
        .or_else(|_| std::env::var("FULGUR_REVISION"))
        .unwrap_or_else(|_| "unknown".into())
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

#[allow(clippy::too_many_arguments)]
fn write_summary(
    dir: &Path,
    subdir: &str,
    total: usize,
    pass: u32,
    fail: u32,
    skip: u32,
    regressions: &[Regression],
    promotions: &[String],
    unknown: &[String],
    elapsed: Duration,
) -> Result<()> {
    let mut f = std::fs::File::create(dir.join("summary.md"))?;
    writeln!(f, "### WPT {subdir}")?;
    writeln!(f)?;
    writeln!(
        f,
        "- total: **{total}** ({elapsed_s:.1}s)",
        elapsed_s = elapsed.as_secs_f64(),
    )?;
    let pass_pct = if total == 0 { 0.0 } else { pass as f64 * 100.0 / total as f64 };
    writeln!(f, "- PASS: **{pass}** ({pass_pct:.1}%)")?;
    writeln!(f, "- FAIL: {fail}")?;
    writeln!(f, "- SKIP: {skip}")?;
    writeln!(f, "- regressions: {}", regressions.len())?;
    writeln!(f, "- promotion candidates: {}", promotions.len())?;
    writeln!(f, "- unknown (no expectation entry): {}", unknown.len())?;

    if !regressions.is_empty() {
        writeln!(f, "\n#### Regressions\n")?;
        for r in regressions {
            let msg = r.message.as_deref().unwrap_or("");
            writeln!(f, "- `{}` declared={} observed={} — {msg}", r.test, r.declared, r.observed)?;
        }
    }
    if !promotions.is_empty() {
        writeln!(f, "\n#### Promotion candidates ({} tests now pass)\n", promotions.len())?;
        for p in promotions.iter().take(30) {
            writeln!(f, "- `{p}`")?;
        }
        if promotions.len() > 30 {
            writeln!(f, "- ... (+{})", promotions.len() - 30)?;
        }
    }
    Ok(())
}
```

**Step 2: `lib.rs` に `pub mod runner;` 追加**

既存 `pub mod harness;` 等の並びに:

```rust
pub mod runner;
```

**Step 3: Build**

```bash
cargo build -p fulgur-wpt 2>&1 | tail -3
```

Expected: clean.

**Step 4: Commit**

```bash
git add crates/fulgur-wpt/src
git commit -m "feat(fulgur-wpt): extract shared phase runner (2foo.11 infra)"
```

---

## Task 2: `tests/wpt_css_page.rs` を runner 利用に置き換え

**Beads:** fulgur-2foo.11 (infra)

**Files:**

- Modify: `crates/fulgur-wpt/tests/wpt_css_page.rs`

**Step 1: Replace contents**

```rust
//! Phase 1 entry: run all css-page reftests via the shared phase runner.
//! Never panics on regressions — emits artifacts under target/wpt-report/css-page/.

use fulgur_wpt::runner::run_phase;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn wpt_css_page() {
    let outcome = run_phase(&workspace_root(), "css-page", 96)
        .expect("runner should not error");
    // Reporting only — CI reads target/wpt-report/css-page/ artifacts.
    if let Some(o) = outcome {
        eprintln!("css-page report at {}", o.report_dir.display());
    }
}
```

**Step 2: Local dry-run**

```bash
cd /home/ubuntu/fulgur/.worktrees/wpt-phase3-multicol
scripts/wpt/fetch.sh 2>&1 | tail -2
cargo test -p fulgur-wpt --test wpt_css_page -- --nocapture 2>&1 | tail -10
ls target/wpt-report/css-page/
```

Expected: 3 files (report.json, regressions.json, summary.md), test pass.

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/tests/wpt_css_page.rs
git commit -m "refactor(fulgur-wpt): wpt_css_page uses shared runner, reporting-only (2foo.11)"
```

---

## Task 3: css-multicol seed 実行

**Beads:** fulgur-2foo.11 (main)

**Files:**

- Create: `crates/fulgur-wpt/expectations/css-multicol.txt` (generated, ~300 entries)

**Step 1: Run seed (~10-15 min)**

```bash
cargo run -p fulgur-wpt --example seed -- \
  --subdir css-multicol \
  --wpt-root target/wpt \
  --out crates/fulgur-wpt/expectations/css-multicol.txt 2>&1 | tee /tmp/seed-multicol.log | tail -10
```

Use Bash `run_in_background: true` since this takes >5 min.

**Step 2: Verify**

```bash
head -5 crates/fulgur-wpt/expectations/css-multicol.txt
awk 'BEGIN{p=0;f=0;s=0} /^PASS/{p++} /^FAIL/{f++} /^SKIP/{s++} END{print "PASS="p" FAIL="f" SKIP="s}' crates/fulgur-wpt/expectations/css-multicol.txt
wc -l crates/fulgur-wpt/expectations/css-multicol.txt
```

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/expectations/css-multicol.txt
git commit -m "feat(fulgur-wpt): seed css-multicol expectations (Phase 3, 2foo.11)"
```

---

## Task 4: `tests/wpt_css_multicol.rs` を作成

**Beads:** fulgur-2foo.11

**Files:**

- Create: `crates/fulgur-wpt/tests/wpt_css_multicol.rs`

**Step 1: 1-file thin entry**

```rust
//! Phase 3 entry: run all css-multicol reftests via the shared runner.

use fulgur_wpt::runner::run_phase;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn wpt_css_multicol() {
    let outcome = run_phase(&workspace_root(), "css-multicol", 96)
        .expect("runner should not error");
    if let Some(o) = outcome {
        eprintln!("css-multicol report at {}", o.report_dir.display());
    }
}
```

**Step 2: Local dry-run**

```bash
cargo test -p fulgur-wpt --test wpt_css_multicol -- --nocapture 2>&1 | tail -15
cat target/wpt-report/css-multicol/summary.md | head -20
```

Expected: test pass (no regressions since we just seeded), summary.md populated.

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/tests/wpt_css_multicol.rs
git commit -m "test(fulgur-wpt): add Phase 3 css-multicol integration test (2foo.11)"
```

---

## Task 5: `.github/workflows/ci.yml` を matrix 化

**Beads:** fulgur-2foo.11 (CI)

**Files:**

- Modify: `.github/workflows/ci.yml`

**Step 1: 既存 `wpt-css-page` job を matrix job に書き換え**

既存:

```yaml
  wpt-css-page:
    name: wpt / css-page
    runs-on: ubuntu-latest
    steps:
      ...
      - name: Run css-page reftests against expectations
        run: cargo test -p fulgur-wpt --test wpt_css_page -- --nocapture
```

に置き換え:

```yaml
  wpt:
    name: wpt / ${{ matrix.phase }}
    runs-on: ubuntu-latest
    # WPT reftests are a reporting channel, not a blocker on PRs.
    # Regressions surface via the step summary and target/wpt-report artifacts.
    continue-on-error: true
    strategy:
      fail-fast: false
      matrix:
        phase: [css-page, css-multicol]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install poppler-utils
        run: sudo apt-get update && sudo apt-get install -y poppler-utils
      - name: Fetch WPT subset
        run: scripts/wpt/fetch.sh
      - name: Run ${{ matrix.phase }} reftests
        env:
          FULGUR_REVISION: ${{ github.sha }}
        run: cargo test -p fulgur-wpt --test wpt_${{ matrix.phase }} -- --nocapture
      - name: Append step summary
        if: always()
        run: |
          summary="target/wpt-report/${{ matrix.phase }}/summary.md"
          if [ -f "$summary" ]; then
            cat "$summary" >> "$GITHUB_STEP_SUMMARY"
          else
            echo "no summary for ${{ matrix.phase }}" >> "$GITHUB_STEP_SUMMARY"
          fi
      - name: Upload report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: wpt-${{ matrix.phase }}-report
          path: target/wpt-report/${{ matrix.phase }}
          if-no-files-found: warn
```

Cargo test 名は `wpt_css-page` ではなく `wpt_css_page` (integration test file 名) なので `--test wpt_${{ matrix.phase }}` の matrix 値は実際は `_` を使う必要がある。↑ の snippet は誤り。

正しい形:

```yaml
      matrix:
        include:
          - phase: css-page
            test: wpt_css_page
          - phase: css-multicol
            test: wpt_css_multicol
    steps:
      ...
      - name: Run ${{ matrix.phase }} reftests
        run: cargo test -p fulgur-wpt --test ${{ matrix.test }} -- --nocapture
```

**Step 2: YAML validation**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo "yaml OK"
```

**Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(wpt): matrix over [css-page, css-multicol], reporting-only (2foo.11)"
```

---

## Task 6: `.github/workflows/wpt-nightly.yml` を matrix 化 + regression 通知を新設計に対応

**Beads:** fulgur-2foo.11 (CI)

**Files:**

- Modify: `.github/workflows/wpt-nightly.yml`

**Step 1: 新構造**

```yaml
name: WPT nightly

on:
  schedule:
    - cron: '0 2 * * *'
  workflow_dispatch:

permissions:
  contents: read
  issues: write

jobs:
  wpt:
    name: wpt / ${{ matrix.phase }}
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        include:
          - phase: css-page
            test: wpt_css_page
          - phase: css-multicol
            test: wpt_css_multicol
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Install poppler-utils
        run: sudo apt-get update && sudo apt-get install -y poppler-utils
      - name: Fetch WPT subset
        run: scripts/wpt/fetch.sh
      - name: Run ${{ matrix.phase }} reftests
        id: run
        env:
          FULGUR_REVISION: ${{ github.sha }}
        run: cargo test -p fulgur-wpt --test ${{ matrix.test }} -- --nocapture
      - name: Append step summary
        if: always()
        run: |
          summary="target/wpt-report/${{ matrix.phase }}/summary.md"
          if [ -f "$summary" ]; then
            cat "$summary" >> "$GITHUB_STEP_SUMMARY"
          fi
      - name: Upload report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: wpt-${{ matrix.phase }}-nightly
          path: target/wpt-report/${{ matrix.phase }}
          if-no-files-found: warn
      - name: File regression issue
        if: always()
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          set -euo pipefail
          regressions="target/wpt-report/${{ matrix.phase }}/regressions.json"
          if [ ! -f "$regressions" ]; then
            echo "no regressions.json — skipping"
            exit 0
          fi
          count=$(jq 'length' "$regressions")
          if [ "$count" = "0" ]; then
            echo "no regressions — skipping"
            exit 0
          fi
          BODY="Nightly WPT ${{ matrix.phase }} detected $count regression(s). Run: $GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID

          Download the \`wpt-${{ matrix.phase }}-nightly\` artifact for details."
          if gh label list --search "wpt-nightly-regression" --limit 1 \
               | grep -q "^wpt-nightly-regression"; then
            gh issue create \
              --title "WPT nightly regression: ${{ matrix.phase }} $(date -u +%Y-%m-%d)" \
              --body "$BODY" \
              --label "wpt-nightly-regression"
          else
            gh issue create \
              --title "WPT nightly regression: ${{ matrix.phase }} $(date -u +%Y-%m-%d)" \
              --body "$BODY"
          fi
```

`jq` は GitHub Actions ubuntu-latest にデフォルト入っている。

**Step 2: YAML validation**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/wpt-nightly.yml'))" && echo "yaml OK"
```

**Step 3: Commit**

```bash
git add .github/workflows/wpt-nightly.yml
git commit -m "ci(wpt-nightly): matrix + regression detection via regressions.json (2foo.11)"
```

---

## Task 7: README 更新 — reporting-only モデルを説明

**Beads:** fulgur-2foo.11 (docs)

**Files:**

- Modify: `crates/fulgur-wpt/README.md`

既存 "### PASS 昇格フロー" の前に追加:

```markdown
### CI との関係

WPT reftest の結果は **CI を fail させません** (`continue-on-error: true`). PR でテストが「赤」になっても merge はブロックされないので、fulgur に広範な変更を加えた直後でも feedback loop が早く回ります。

カバレッジ推移は以下で見ます:

- **PR CI step summary**: 各 phase の total/PASS/FAIL/SKIP と PASS 率が自動で表示される
- **PR artifact**: `target/wpt-report/<phase>/report.json` (wptreport.json schema)、`regressions.json`、`summary.md`
- **nightly**: 同じ構造で全 phase 実行、regression があれば `wpt-nightly-regression` ラベルの issue を自動起票

expectations は「宣言と実測が一致すればまだ退化していない」という baseline。fulgur 改善で PASS 化したテストは **PR で expectations を編集して PASS に昇格** し、次の回から regression として検知される土俵になる。
```

markdownlint 通過を確認 (`npx markdownlint-cli2 crates/fulgur-wpt/README.md`)。

Commit:

```bash
git add crates/fulgur-wpt/README.md
git commit -m "docs(fulgur-wpt): document reporting-only CI model (2foo.11)"
```

---

## Task 8: 最終ゲート

**Step 1: Full test**

```bash
cargo test -p fulgur-wpt 2>&1 | grep "^test result" | head -10
```

Expected: lib + 5 integration tests (wpt_css_page, wpt_css_multicol, others) 全 pass.

**Step 2: Lint**

```bash
cargo fmt --check
cargo clippy -p fulgur-wpt --all-targets -- -D warnings
```

Both clean.

**Step 3: Markdown + YAML**

```bash
npx markdownlint-cli2 \
  'docs/plans/2026-04-21-wpt-phase3-multicol-matrix.md' \
  'crates/fulgur-wpt/README.md'
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/wpt-nightly.yml'))"
```

All clean / yaml OK.

**Step 4: Baseline 回帰なし**

```bash
cargo test -p fulgur --lib 2>&1 | grep "^test result" | head -3
cargo test -p fulgur-vrt --lib 2>&1 | grep "^test result" | head -3
```

Expected: 549 + 12 pass (変動なし).

---

## Known risks

1. **css-multicol seed time**: 337 tests × ~0.3s ≈ 100s. Full seed ~10-15 min with cold compile。問題なし
2. **fulgur multicol の成熟度**: 既に multicol 実装あり、PASS 率は 30-50% 程度と予想。低すぎたら多くのテストが明らかに機能未対応で仕方なし
3. **matrix job のコスト**: PR 毎に 2 並列 × ~2分 = ~4 min 相当の runner 時間。GitHub Actions の free tier (2000 min/月) 範囲内で余裕
4. **jq 依存**: ubuntu-latest にプリインストール、明示 install は不要

## Out of scope (別 PR)

- **fulgur-2foo.10** (css-break seed) — 968 tests、seed 時間が長いため別 PR
- **fulgur-2foo.12** (css-gcpm) — ref が無く manual golden が必要、別戦略
- **fulgur-lje5** (page-break-after 配線) — fulgur 本体修正、別 PR
- 実際のカバレッジ推移ダッシュボード (wpt.fyi 投稿 or Codecov 風) — 初期は step summary + artifact で目視
