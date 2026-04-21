# WPT rel=mismatch Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Wire up `rel=mismatch` (negative reftest) support in `fulgur-wpt` so the 4 mismatch tests in `expectations/css-page.txt` graduate from SKIP to either PASS or FAIL.

**Architecture:** Extend `ReftestKind` with a `Mismatch { ref_path, fuzzy }` variant, update `classify()` to emit it from single `<link rel="mismatch">`, and teach `harness::run_one` to flip the verdict (any page exceeding fuzzy tolerance → PASS; all pages within tolerance → FAIL).

**Tech Stack:** Rust 2024 edition, `fulgur-wpt` crate, `fulgur-vrt::diff`, scraper HTML parser, `tempfile` for integration tests.

**Reference:** `docs/plans/2026-04-21-wpt-rel-mismatch-design.md`

---

### Task 1: Commit the design doc

**Files:**

- Add: `docs/plans/2026-04-21-wpt-rel-mismatch-design.md` (already copied into worktree)

**Step 1: Stage and commit**

```bash
git add docs/plans/2026-04-21-wpt-rel-mismatch-design.md
git commit -m "docs(fulgur-wpt): design note for rel=mismatch support"
```

Expected: commit created on branch `feature/wpt-rel-mismatch`.

---

### Task 2: Add failing reftest.rs unit tests (RED)

**Files:**

- Modify: `crates/fulgur-wpt/src/reftest.rs` (add tests in `mod reftest_tests`)

**Step 1: Write failing tests**

Add these tests at the bottom of `mod reftest_tests` (after the existing `collect_reftest_files_recurses_and_filters` test):

```rust
#[test]
fn single_mismatch_classified_as_mismatch() {
    let (_d, p) = write_tmp(
        "t.html",
        r#"<!DOCTYPE html><link rel="mismatch" href="t-notref.html"><body></body>"#,
    );
    let r = classify(&p).unwrap();
    match r.classification {
        ReftestKind::Mismatch { ref_path, fuzzy } => {
            assert_eq!(ref_path.file_name().unwrap(), "t-notref.html");
            assert_eq!(fuzzy, FuzzyTolerance::strict());
        }
        other => panic!("expected Mismatch, got {other:?}"),
    }
}

#[test]
fn mismatch_with_fuzzy_meta() {
    let (_d, p) = write_tmp(
        "t.html",
        r#"<!DOCTYPE html>
<link rel="mismatch" href="t-notref.html">
<meta name="fuzzy" content="5-10;200-300">
<body></body>"#,
    );
    match classify(&p).unwrap().classification {
        ReftestKind::Mismatch { fuzzy, .. } => {
            assert_eq!(fuzzy.max_diff, 5..=10);
            assert_eq!(fuzzy.total_pixels, 200..=300);
        }
        other => panic!("expected Mismatch, got {other:?}"),
    }
}

#[test]
fn multiple_mismatches_skip() {
    let (_d, p) = write_tmp(
        "t.html",
        r#"<!DOCTYPE html>
<link rel="mismatch" href="a.html">
<link rel="mismatch" href="b.html">
<body></body>"#,
    );
    assert!(matches!(
        classify(&p).unwrap().classification,
        ReftestKind::Skip { reason: SkipReason::MultipleMismatches }
    ));
}

#[test]
fn mixed_match_and_mismatch_skip() {
    let (_d, p) = write_tmp(
        "t.html",
        r#"<!DOCTYPE html>
<link rel="match" href="a.html">
<link rel="mismatch" href="b.html">
<body></body>"#,
    );
    assert!(matches!(
        classify(&p).unwrap().classification,
        ReftestKind::Skip { reason: SkipReason::MixedMatchAndMismatch }
    ));
}
```

Delete the existing `mismatch_skip` test (it asserts the soon-to-be-obsolete `Skip(Mismatch)` behavior for single mismatch).

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur-wpt --lib reftest 2>&1 | tail -20
```

Expected: 4 new tests fail to compile (`Mismatch` variant, `MultipleMismatches`, `MixedMatchAndMismatch` don't exist). No commit yet — this is the RED step.

---

### Task 3: Update types in reftest.rs (make the tests compile, still RED)

**Files:**

- Modify: `crates/fulgur-wpt/src/reftest.rs:137-155`

**Step 1: Update `ReftestKind` and `SkipReason`**

Replace the `ReftestKind` enum:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum ReftestKind {
    /// Single rel=match + optional fuzzy tolerance.
    Match {
        ref_path: PathBuf,
        fuzzy: FuzzyTolerance,
    },
    /// Single rel=mismatch + optional fuzzy tolerance. The test PASSes
    /// when test and ref render *differ* beyond the fuzzy threshold.
    Mismatch {
        ref_path: PathBuf,
        fuzzy: FuzzyTolerance,
    },
    /// Skipped: out-of-scope reftest variant.
    Skip { reason: SkipReason },
}
```

Replace `SkipReason`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Legacy: retained so older expectations comments (`# Mismatch`) remain
    /// parseable. `classify()` no longer emits this for single mismatch —
    /// that path now returns `ReftestKind::Mismatch` instead.
    Mismatch,
    MultipleMatches,
    MultipleMismatches,
    MixedMatchAndMismatch,
    NoMatch,
    /// Reserved for Phase 2: reftest chain (ref HTML points at another ref).
    /// Not yet emitted by `classify()`.
    ChainedReference,
}
```

**Step 2: Run tests to verify compile + new tests fail on logic**

```bash
cargo test -p fulgur-wpt --lib reftest 2>&1 | tail -25
```

Expected: compiles; 4 new tests fail because `classify()` still returns `Skip(Mismatch)` for rel=mismatch. Existing tests still pass.

---

### Task 4: Update classify() logic (GREEN)

**Files:**

- Modify: `crates/fulgur-wpt/src/reftest.rs:159-256` (`classify` function)

**Step 1: Rewrite match/mismatch collection**

Replace the block that iterates `link[rel]` and the subsequent skip/match decision. The new logic collects both independently and picks by count:

```rust
let link_sel = scraper::Selector::parse("link[rel]").unwrap();
let mut matches: Vec<PathBuf> = Vec::new();
let mut mismatches: Vec<PathBuf> = Vec::new();

for el in doc.select(&link_sel) {
    for token in el
        .value()
        .attr("rel")
        .unwrap_or("")
        .split_ascii_whitespace()
    {
        match token.to_ascii_lowercase().as_str() {
            "match" => {
                let href = el.value().attr("href").ok_or_else(|| {
                    anyhow::anyhow!("rel=match link without href in {}", test_path.display())
                })?;
                matches.push(PathBuf::from(href));
            }
            "mismatch" => {
                let href = el.value().attr("href").ok_or_else(|| {
                    anyhow::anyhow!(
                        "rel=mismatch link without href in {}",
                        test_path.display()
                    )
                })?;
                mismatches.push(PathBuf::from(href));
            }
            _ => {}
        }
    }
}

// Decision table:
//   matches=N, mismatches=M
//   N≥2              → MultipleMatches
//   M≥2              → MultipleMismatches
//   N=1, M≥1         → MixedMatchAndMismatch
//   N=1, M=0         → Match
//   N=0, M=1         → Mismatch
//   N=0, M=0         → NoMatch
let (is_mismatch, ref_path) = match (matches.len(), mismatches.len()) {
    (n, _) if n >= 2 => {
        return Ok(Reftest {
            test: test_path.to_path_buf(),
            classification: ReftestKind::Skip { reason: SkipReason::MultipleMatches },
        });
    }
    (_, m) if m >= 2 => {
        return Ok(Reftest {
            test: test_path.to_path_buf(),
            classification: ReftestKind::Skip { reason: SkipReason::MultipleMismatches },
        });
    }
    (1, 1) => {
        return Ok(Reftest {
            test: test_path.to_path_buf(),
            classification: ReftestKind::Skip {
                reason: SkipReason::MixedMatchAndMismatch,
            },
        });
    }
    (1, 0) => (false, matches.into_iter().next().unwrap()),
    (0, 1) => (true, mismatches.into_iter().next().unwrap()),
    _ => {
        return Ok(Reftest {
            test: test_path.to_path_buf(),
            classification: ReftestKind::Skip { reason: SkipReason::NoMatch },
        });
    }
};
```

Then keep the existing fuzzy-meta selection block unchanged (it references `ref_path`, which still exists), and at the bottom dispatch on `is_mismatch`:

```rust
let classification = if is_mismatch {
    ReftestKind::Mismatch { ref_path, fuzzy: chosen }
} else {
    ReftestKind::Match { ref_path, fuzzy: chosen }
};
Ok(Reftest {
    test: test_path.to_path_buf(),
    classification,
})
```

**Step 2: Verify all reftest tests pass**

```bash
cargo test -p fulgur-wpt --lib reftest 2>&1 | tail -10
```

Expected: all reftest tests pass (new 4 + existing, minus deleted `mismatch_skip`).

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/src/reftest.rs
git commit -m "feat(fulgur-wpt): classify rel=mismatch as Mismatch variant

- Add ReftestKind::Mismatch { ref_path, fuzzy }
- Add SkipReason::MultipleMismatches, MixedMatchAndMismatch
- Keep SkipReason::Mismatch as legacy marker for old expectations
- Table-driven classify() decides match vs mismatch by link counts

Refs: fulgur-rx3f"
```

---

### Task 5: Add failing harness integration tests (RED)

**Files:**

- Modify: `crates/fulgur-wpt/tests/harness_smoke.rs`

**Step 1: Inspect existing harness_smoke.rs**

```bash
head -60 crates/fulgur-wpt/tests/harness_smoke.rs
```

Identify the existing helper that writes HTML pairs and invokes `run_one`. Reuse its pattern.

**Step 2: Add 3 new tests**

Append to `harness_smoke.rs`:

```rust
#[test]
fn mismatch_test_with_identical_ref_is_fail() {
    // test and ref render identically → mismatch test FAILs
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let test_html = r#"<!DOCTYPE html>
<link rel="mismatch" href="same-ref.html">
<body style="margin:0"><div style="width:50px;height:50px;background:green"></div></body>"#;
    let ref_html = r#"<!DOCTYPE html>
<body style="margin:0"><div style="width:50px;height:50px;background:green"></div></body>"#;
    std::fs::write(root.join("t.html"), test_html).unwrap();
    std::fs::write(root.join("same-ref.html"), ref_html).unwrap();

    let outcome = fulgur_wpt::harness::run_one(
        &root.join("t.html"),
        &root.join("work"),
        &root.join("diff"),
        96,
    )
    .unwrap();
    assert_eq!(outcome.observed, fulgur_wpt::expectations::Expectation::Fail);
    assert!(
        outcome.reason.as_deref().unwrap_or("").contains("mismatch expected"),
        "reason was: {:?}",
        outcome.reason,
    );
}

#[test]
fn mismatch_test_with_different_ref_is_pass() {
    // test and ref render differently → mismatch test PASSes
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let test_html = r#"<!DOCTYPE html>
<link rel="mismatch" href="diff-ref.html">
<body style="margin:0"><div style="width:50px;height:50px;background:green"></div></body>"#;
    let ref_html = r#"<!DOCTYPE html>
<body style="margin:0"><div style="width:50px;height:50px;background:red"></div></body>"#;
    std::fs::write(root.join("t.html"), test_html).unwrap();
    std::fs::write(root.join("diff-ref.html"), ref_html).unwrap();

    let outcome = fulgur_wpt::harness::run_one(
        &root.join("t.html"),
        &root.join("work"),
        &root.join("diff"),
        96,
    )
    .unwrap();
    assert_eq!(outcome.observed, fulgur_wpt::expectations::Expectation::Pass);
}

#[test]
fn mismatch_test_different_page_count_is_pass() {
    // page count differs → mismatch test PASSes (any difference satisfies)
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let test_html = r#"<!DOCTYPE html>
<link rel="mismatch" href="pc-ref.html">
<style>@page { size: 200px 200px; margin: 0; } div { height: 150px; break-after: page; }</style>
<body><div style="background:red"></div><div style="background:blue"></div></body>"#;
    let ref_html = r#"<!DOCTYPE html>
<style>@page { size: 200px 200px; margin: 0; }</style>
<body style="margin:0"><div style="width:50px;height:50px;background:green"></div></body>"#;
    std::fs::write(root.join("t.html"), test_html).unwrap();
    std::fs::write(root.join("pc-ref.html"), ref_html).unwrap();

    let outcome = fulgur_wpt::harness::run_one(
        &root.join("t.html"),
        &root.join("work"),
        &root.join("diff"),
        96,
    )
    .unwrap();
    // Either page-count-different path or fuzzy-diff-per-page path should yield PASS.
    assert_eq!(outcome.observed, fulgur_wpt::expectations::Expectation::Pass);
}
```

**Step 3: Verify new tests fail**

```bash
cargo test -p fulgur-wpt --test harness_smoke 2>&1 | tail -20
```

Expected: the 3 new tests fail (harness still returns `Skip` or mishandles Mismatch). Existing tests still pass.

---

### Task 6: Implement harness Mismatch branch (GREEN)

**Files:**

- Modify: `crates/fulgur-wpt/src/harness.rs`

**Step 1: Introduce Kind enum and dispatch**

Replace the current classification-unpacking block (`harness.rs:38-48`) with:

```rust
enum Kind {
    Match,
    Mismatch,
}

let reftest = classify(test_html_path)?;
let (kind, ref_rel, fuzzy) = match reftest.classification {
    ReftestKind::Match { ref_path, fuzzy } => (Kind::Match, ref_path, fuzzy),
    ReftestKind::Mismatch { ref_path, fuzzy } => (Kind::Mismatch, ref_path, fuzzy),
    ReftestKind::Skip { reason } => {
        return Ok(RunOutcome {
            observed: Expectation::Skip,
            reason: Some(format!("{reason:?}")),
            diff_dir: None,
        });
    }
};
```

Also update the import at `harness.rs:5`:

```rust
use crate::reftest::{ReftestKind, classify};
```

(no functional change — the wildcard was already imported, but make sure the file still compiles.)

**Step 2: Flip page-count-mismatch behaviour**

Replace the block at `harness.rs:60-70` with:

```rust
if test_out.pages.len() != ref_out.pages.len() {
    let msg = format!(
        "page count mismatch: test={} ref={}",
        test_out.pages.len(),
        ref_out.pages.len(),
    );
    return Ok(match kind {
        Kind::Match => RunOutcome {
            observed: Expectation::Fail,
            reason: Some(msg),
            diff_dir: None,
        },
        Kind::Mismatch => RunOutcome {
            observed: Expectation::Pass,
            reason: None,
            diff_dir: None,
        },
    });
}
```

**Step 3: Flip per-page verdict logic**

Replace the `for (idx, (t, r)) ...` loop and final `Ok(match first_failure ...)` (`harness.rs:79-123`) with:

```rust
let mut first_over_threshold: Option<String> = None;
for (idx, (t, r)) in test_out.pages.iter().zip(ref_out.pages.iter()).enumerate() {
    let total = u64::from(t.width()) * u64::from(t.height());
    let ratio_limit = if total == 0 {
        0.0f32
    } else {
        (max_total as f64 / total as f64) as f32
    };
    let tol = Tolerance {
        max_channel_diff: max_ch,
        max_diff_pixels_ratio: ratio_limit,
    };
    let report = compare(r, t, tol);
    if !report.pass && first_over_threshold.is_none() {
        first_over_threshold = Some(format!(
            "page {} diff: {}/{} pixels exceed tol (max_ch={})",
            idx + 1,
            report.diff_pixels,
            report.total_pixels,
            report.max_channel_diff,
        ));
        // For match failures, dump diff image for diagnosis.
        if matches!(kind, Kind::Match) {
            std::fs::create_dir_all(diff_out_dir)?;
            let out_path = diff_out_dir.join(format!("page{}.diff.png", idx + 1));
            write_diff_image(r, t, tol, &out_path)?;
        }
    } else if !report.pass && matches!(kind, Kind::Match) {
        // Additional pages: still dump for match cases.
        std::fs::create_dir_all(diff_out_dir)?;
        let out_path = diff_out_dir.join(format!("page{}.diff.png", idx + 1));
        write_diff_image(r, t, tol, &out_path)?;
    }
}

Ok(match (kind, first_over_threshold) {
    (Kind::Match, Some(reason)) => RunOutcome {
        observed: Expectation::Fail,
        reason: Some(reason),
        diff_dir: Some(diff_out_dir.to_path_buf()),
    },
    (Kind::Match, None) => RunOutcome {
        observed: Expectation::Pass,
        reason: None,
        diff_dir: None,
    },
    (Kind::Mismatch, Some(_)) => RunOutcome {
        observed: Expectation::Pass,
        reason: None,
        diff_dir: None,
    },
    (Kind::Mismatch, None) => RunOutcome {
        observed: Expectation::Fail,
        reason: Some(
            "mismatch expected but test matches ref within tolerance".to_string(),
        ),
        diff_dir: None,
    },
})
```

**Step 4: Run harness tests**

```bash
cargo test -p fulgur-wpt --test harness_smoke 2>&1 | tail -20
```

Expected: all existing + 3 new tests pass.

**Step 5: Run full fulgur-wpt test suite (regression check)**

```bash
cargo test -p fulgur-wpt 2>&1 | tail -15
```

Expected: all suites green (lib, harness_smoke, diff_pages, render_multi_page, wpt_smoke, wpt_css_multicol, wpt_css_page).

**Step 6: Commit**

```bash
git add crates/fulgur-wpt/src/harness.rs crates/fulgur-wpt/tests/harness_smoke.rs
git commit -m "feat(fulgur-wpt): flip verdict for rel=mismatch reftests

Harness treats Mismatch like Match through rendering and page-count
check, then inverts the final verdict: any page diff exceeding fuzzy
tolerance (or a page-count difference) yields PASS, while full
per-page parity yields FAIL with 'mismatch expected' reason.

Refs: fulgur-rx3f"
```

---

### Task 7: Measure the 4 mismatch tests in-subset

**Step 1: Ensure WPT is fetched**

```bash
ls target/wpt/css/css-page/basic-pagination-004-print.html 2>/dev/null || scripts/wpt/fetch.sh
```

Expected: file exists (fetched earlier in the parent session, accessible from the worktree via shared target dir).

If `target/wpt` is missing in the worktree: run `scripts/wpt/fetch.sh`.

**Step 2: Run each mismatch test and capture verdict**

```bash
for t in basic-pagination-004-print basic-pagination-005-print \
         page-orientation-on-portrait-002-print page-orientation-on-portrait-003-print; do
  echo "=== $t ==="
  cargo run -p fulgur-wpt --example run_one -- \
    target/wpt/css/css-page/$t.html 2>&1 | grep -E '^(verdict|reason):'
done
```

Write the observed verdict/reason of each test into a scratch note or directly into the edit of `expectations/css-page.txt` in the next task.

---

### Task 8: Update expectations/css-page.txt

**Files:**

- Modify: `crates/fulgur-wpt/expectations/css-page.txt`

**Step 1: Replace the 4 SKIP lines with observed status**

Example (adjust per observed verdict from Task 7):

```text
# Before
SKIP  css/css-page/basic-pagination-004-print.html  # Mismatch
SKIP  css/css-page/basic-pagination-005-print.html  # Mismatch
SKIP  css/css-page/page-orientation-on-portrait-002-print.html  # Mismatch
SKIP  css/css-page/page-orientation-on-portrait-003-print.html  # Mismatch

# After (if all FAIL, for example)
FAIL  css/css-page/basic-pagination-004-print.html  # mismatch expected but test matches ref within tolerance
FAIL  css/css-page/basic-pagination-005-print.html  # mismatch expected but test matches ref within tolerance
FAIL  css/css-page/page-orientation-on-portrait-002-print.html  # mismatch expected but test matches ref within tolerance
FAIL  css/css-page/page-orientation-on-portrait-003-print.html  # mismatch expected but test matches ref within tolerance
```

**Step 2: Update the summary header**

First line of `expectations/css-page.txt`:

```text
# Summary: <new_pass> PASS, <new_fail> FAIL, <new_skip> SKIP (total 257).
```

Recompute by counting the file:

```bash
awk '/^PASS/{p++} /^FAIL/{f++} /^SKIP/{s++} END{printf "PASS=%d FAIL=%d SKIP=%d total=%d\n", p, f, s, p+f+s}' \
  crates/fulgur-wpt/expectations/css-page.txt
```

Edit the header line to match.

**Step 3: Run the phase test**

```bash
cargo test -p fulgur-wpt --test wpt_css_page 2>&1 | tail -15
```

Expected: green (declared matches observed for every test in the subset).

**Step 4: Commit**

```bash
git add crates/fulgur-wpt/expectations/css-page.txt
git commit -m "test(fulgur-wpt): promote mismatch tests from SKIP in css-page expectations

- 4 rel=mismatch tests now run end-to-end via the new Mismatch harness path
- Baseline recorded as <PASS/FAIL per observed>

Refs: fulgur-rx3f"
```

---

### Task 9: Update README

**Files:**

- Modify: `crates/fulgur-wpt/README.md`

**Step 1: Add a one-liner under the Phase / expectations section**

Find the paragraph that describes supported reftest variants and append:

```markdown
- `rel=match` (single) — primary; fuzzy tolerance honored
- `rel=mismatch` (single) — negative reftest; PASSes when rendered output differs beyond fuzzy tolerance
```

(Place it where the reader will see the supported reftest variants. If no such section exists, add one under "Expectations の運用".)

**Step 2: Lint**

```bash
npx markdownlint-cli2 crates/fulgur-wpt/README.md
```

Expected: 0 errors.

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/README.md
git commit -m "docs(fulgur-wpt): note rel=mismatch support in README

Refs: fulgur-rx3f"
```

---

### Task 10: Final verification

**Step 1: Lint + format**

```bash
cargo fmt --check
cargo clippy -p fulgur-wpt -- -D warnings
```

Expected: both pass.

**Step 2: Full fulgur-wpt suite**

```bash
cargo test -p fulgur-wpt 2>&1 | tail -20
```

Expected: green.

**Step 3: Close the beads issue**

```bash
bd close fulgur-rx3f --reason "Implemented and landed on branch feature/wpt-rel-mismatch"
bd sync --flush-only
```

**Step 4: Report done**

Summarise:

- Branch: `feature/wpt-rel-mismatch`
- Commits: design, classify, harness, expectations, README (5 commits)
- Expectations delta (before → after per verdict)
- Next step: open PR (out of scope for this plan — handled by `superpowers:finishing-a-development-branch` separately)

---

## Out of scope (future work, not in this plan)

- css-break mismatch coverage (Phase 4)
- Chained reference support (`SkipReason::ChainedReference`)
- fulgur-side implementation fixes for `break-after: page` and `page-orientation` (separate beads issues should track these after Task 7 measures observed verdicts)
