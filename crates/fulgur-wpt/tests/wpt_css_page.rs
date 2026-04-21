//! Phase 1 entry point: run all css-page reftests and compare against the
//! declared expectations at `crates/fulgur-wpt/expectations/css-page.txt`.
//!
//! Skipped when `target/wpt/css/css-page/` or `pdftocairo` is absent, so
//! a normal `cargo test -p fulgur-wpt` on a dev machine without WPT or
//! poppler is still green.

use fulgur_wpt::expectations::{Expectation, ExpectationFile, Verdict, judge};
use fulgur_wpt::harness::run_one;
use fulgur_wpt::reftest::collect_reftest_files;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

/// Cargo runs integration tests with `CWD = CARGO_MANIFEST_DIR`, so we
/// derive the workspace root to resolve `target/wpt` and the expectations
/// path regardless of where `cargo test` is invoked from.
fn workspace_root() -> PathBuf {
    // crates/fulgur-wpt -> crates -> workspace root
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn wpt_root() -> PathBuf {
    workspace_root().join("target/wpt")
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

    let expect_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("expectations/css-page.txt");
    let declared = ExpectationFile::load(&expect_path)
        .unwrap_or_else(|e| panic!("load {}: {e}", expect_path.display()));

    // Recursive walk via the shared helper — picks up nested subdirectories
    // (tentative/, margin-boxes/, cssom/, ...) and correctly excludes
    // `-ref.tentative.html` ref files that a suffix-only filter misses.
    let tests = collect_reftest_files(&dir)
        .unwrap_or_else(|e| panic!("collect reftest files in {}: {e}", dir.display()));

    let mut regressions: Vec<(String, String)> = Vec::new();
    let mut promotions: Vec<String> = Vec::new();
    let mut verdicts: BTreeMap<&'static str, u32> = BTreeMap::new();

    for test in &tests {
        let rel = test
            .strip_prefix(wpt_root())
            .unwrap_or(test)
            .to_string_lossy()
            .replace('\\', "/");
        let declared_exp = declared.get(&rel);
        let stem = test.file_stem().unwrap().to_string_lossy();
        let run_root = workspace_root().join("target/wpt-run");
        let work = run_root.join(&*stem).join("work");
        let diff = run_root.join(&*stem).join("diff");

        // Harness crashes (blitz unwrap, pdftocairo segfault, etc.) are
        // recorded as Fail so that a single bad test can't take down the run.
        let observed = match catch_unwind(AssertUnwindSafe(|| run_one(test, &work, &diff, 96))) {
            Ok(Ok(o)) => o.observed,
            Ok(Err(_)) | Err(_) => Expectation::Fail,
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
                regressions.push((rel.clone(), format!("{observed:?}")));
            }
            Verdict::Promotion => {
                promotions.push(rel.clone());
            }
            Verdict::UnknownTest => {
                eprintln!("warn: {rel} has no expectation entry (observed {observed:?})");
            }
            _ => {}
        }
    }

    eprintln!("\n=== css-page verdicts ===");
    for (k, v) in &verdicts {
        eprintln!("  {k}: {v}");
    }
    if !promotions.is_empty() {
        eprintln!(
            "\nPromotion candidates ({} tests now pass but declared FAIL):",
            promotions.len()
        );
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
