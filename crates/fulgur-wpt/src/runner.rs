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
//! Never panics on regressions. Nightly workflow inspects `regressions.json`
//! separately to decide whether to open an issue.

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
/// `target/wpt-report/<subdir>/`.
///
/// Returns `Ok(None)` when prerequisites are missing (no WPT checkout,
/// no pdftocairo) so callers can skip silently on dev machines. Returns
/// `Ok(Some(..))` with the outcome otherwise.
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
        eprintln!(
            "note: {} missing, treating every test as unknown",
            expect_path.display()
        );
        ExpectationFile::default()
    };

    let tests = collect_reftest_files(&dir)
        .with_context(|| format!("collect reftest files in {}", dir.display()))?;
    let total = tests.len();

    let report_dir = workspace_root.join("target/wpt-report").join(subdir);
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
            Ok(Err(e)) => (Expectation::Fail, Some(format!("harness error: {e}"))),
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

    report.write(&report_dir.join("report.json"))?;
    std::fs::write(
        report_dir.join("regressions.json"),
        serde_json::to_string_pretty(&regressions)?,
    )?;
    write_summary(
        &report_dir,
        subdir,
        total,
        pass,
        fail,
        skip,
        &regressions,
        &promotions,
        &unknown,
        elapsed,
    )?;

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
    let pass_pct = if total == 0 {
        0.0
    } else {
        pass as f64 * 100.0 / total as f64
    };
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
            writeln!(
                f,
                "- `{}` declared={} observed={} — {msg}",
                r.test, r.declared, r.observed
            )?;
        }
    }
    if !promotions.is_empty() {
        writeln!(
            f,
            "\n#### Promotion candidates ({} tests now pass)\n",
            promotions.len()
        )?;
        for p in promotions.iter().take(30) {
            writeln!(f, "- `{p}`")?;
        }
        if promotions.len() > 30 {
            writeln!(f, "- ... (+{})", promotions.len() - 30)?;
        }
    }
    Ok(())
}
