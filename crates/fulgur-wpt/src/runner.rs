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

    let outcome = execute_and_report(workspace_root, subdir, tests, declared, dpi)?;
    Ok(Some(outcome))
}

/// Run exactly the reftests enumerated in `expectations_path` — keys of
/// the expectations file are interpreted as WPT-root-relative paths
/// (e.g. `css/css-page/foo.html`). Cross-subdir: paths may live under
/// any `css/<subdir>` as long as they exist in the fetched WPT snapshot.
///
/// `list_name` is used as the report subdirectory name
/// (`target/wpt-report/<list_name>/`) and as the stderr verdict tag
/// (`wpt-<list_name>:`). Keep it filesystem-safe (alphanumerics, `-`, `_`).
///
/// Returns `Ok(None)` when prerequisites are missing (no WPT checkout,
/// no expectations file, or no pdftocairo).
pub fn run_list(
    workspace_root: &Path,
    list_name: &str,
    expectations_path: &Path,
    dpi: u32,
) -> Result<Option<PhaseOutcome>> {
    // list_name ends up as a directory component under target/wpt-report/,
    // so reject anything that could escape (path separators, parent refs)
    // or otherwise produce surprising paths. A contract violation is a
    // programming error — fail loud rather than silently skip.
    if !is_safe_list_name(list_name) {
        anyhow::bail!(
            "invalid list_name `{list_name}`: must be [A-Za-z0-9_-]+ \
             (used as a directory component under target/wpt-report/)"
        );
    }

    let wpt_root = workspace_root.join("target/wpt");
    if !wpt_root.is_dir() {
        eprintln!(
            "skip: {} missing (run scripts/wpt/fetch.sh first)",
            wpt_root.display()
        );
        return Ok(None);
    }
    // Require a regular file so passing a directory doesn't sneak past
    // the skip precheck only to error later at load time.
    if !expectations_path.is_file() {
        eprintln!(
            "skip: {} missing (list has no expectations file)",
            expectations_path.display()
        );
        return Ok(None);
    }
    if !poppler_available() {
        eprintln!("skip: pdftocairo not available on PATH");
        return Ok(None);
    }

    let declared = ExpectationFile::load(expectations_path)
        .with_context(|| format!("load {}", expectations_path.display()))?;

    // declared.paths() yields BTreeMap-sorted keys; wpt_root.join preserves
    // relative order, so tests is naturally sorted without an extra pass.
    let mut tests: Vec<PathBuf> = Vec::with_capacity(declared.len());
    let mut missing: Vec<String> = Vec::new();
    for rel in declared.paths() {
        let rel_path = Path::new(rel);
        // Reject entries that could escape wpt_root: absolute paths and
        // any component that walks upward (`..`). These are declared as
        // missing so subset.txt maintainers see a diagnostic instead of
        // a silent traversal.
        if rel_path.is_absolute()
            || rel_path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            missing.push(rel.to_string());
            continue;
        }
        let abs = wpt_root.join(rel_path);
        if abs.is_file() {
            tests.push(abs);
        } else {
            missing.push(rel.to_string());
        }
    }

    if !missing.is_empty() {
        eprintln!(
            "warning: {} test(s) declared in {} are missing from the WPT snapshot:",
            missing.len(),
            expectations_path.display()
        );
        for m in &missing {
            eprintln!("  - {m}");
        }
    }

    Ok(Some(execute_and_report(
        workspace_root,
        list_name,
        tests,
        declared,
        dpi,
    )?))
}

/// Iterate `tests`, judge each observed result against `declared`, and
/// write `report.json` / `regressions.json` / `summary.md` under
/// `target/wpt-report/<label>/`. Shared by `run_phase` (label = subdir)
/// and `run_list` (label = list name).
fn execute_and_report(
    workspace_root: &Path,
    label: &str,
    tests: Vec<PathBuf>,
    declared: ExpectationFile,
    dpi: u32,
) -> Result<PhaseOutcome> {
    let wpt_root = workspace_root.join("target/wpt");
    let fonts_bundle = crate::fonts::load_fonts_dir(&wpt_root.join("fonts")).unwrap_or_else(|e| {
        log::warn!("fonts loader failed: {e}; proceeding without bundled fonts");
        fulgur::asset::AssetBundle::new()
    });
    let fonts_arg = if fonts_bundle.fonts.is_empty() {
        None
    } else {
        Some(&fonts_bundle)
    };
    let total = tests.len();

    let report_dir = workspace_root.join("target/wpt-report").join(label);
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
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            run_one(test, &work, &diff, dpi, fonts_arg)
        }));
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
        label,
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
        "wpt-{label}: total={total} pass={pass} fail={fail} skip={skip} regressions={} promotions={} unknown={} ({:.1}s)",
        regressions.len(),
        promotions.len(),
        unknown.len(),
        elapsed.as_secs_f64(),
    );

    Ok(PhaseOutcome {
        subdir: label.to_string(),
        total,
        pass,
        fail,
        skip,
        regressions,
        promotions,
        unknown,
        report_dir,
    })
}

/// list_name becomes a directory component under `target/wpt-report/` and
/// part of the stderr verdict tag. Restrict it to a safe whitelist so no
/// caller can produce path traversal or shell-surprising output.
fn is_safe_list_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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
    label: &str,
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
    writeln!(f, "### WPT {label}")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn run_list_returns_none_when_wpt_root_missing() {
        let ws = tempdir().unwrap();
        let expectations = ws.path().join("nonexistent.txt");
        let out = run_list(ws.path(), "missing-wpt", &expectations, 96).unwrap();
        assert!(out.is_none(), "expected Ok(None) when target/wpt is absent");
    }

    #[test]
    fn execute_and_report_with_empty_tests_emits_empty_reports() {
        let ws = tempdir().unwrap();
        let outcome = execute_and_report(
            ws.path(),
            "empty-label",
            Vec::new(),
            ExpectationFile::default(),
            96,
        )
        .unwrap();

        assert_eq!(outcome.total, 0);
        assert_eq!(outcome.pass, 0);
        assert_eq!(outcome.fail, 0);
        assert_eq!(outcome.skip, 0);
        assert!(outcome.regressions.is_empty());
        assert!(outcome.promotions.is_empty());
        assert!(outcome.unknown.is_empty());
        assert_eq!(outcome.subdir, "empty-label");

        let report_dir = ws.path().join("target/wpt-report/empty-label");
        assert!(report_dir.join("report.json").is_file());
        assert!(report_dir.join("regressions.json").is_file());
        assert!(report_dir.join("summary.md").is_file());

        let summary = std::fs::read_to_string(report_dir.join("summary.md")).unwrap();
        assert!(summary.contains("### WPT empty-label"));
        assert!(summary.contains("- total: **0**"));
    }

    #[test]
    fn run_list_rejects_unsafe_list_name() {
        let ws = tempdir().unwrap();
        let expectations = ws.path().join("nonexistent.txt");
        let err = run_list(ws.path(), "../escape", &expectations, 96).unwrap_err();
        assert!(
            err.to_string().contains("invalid list_name"),
            "expected bail on traversal-unsafe list_name, got: {err}"
        );
    }

    #[test]
    fn run_list_returns_none_when_expectations_is_a_directory() {
        let ws = tempdir().unwrap();
        // Satisfy the wpt_root guard so control reaches the expectations check.
        std::fs::create_dir_all(ws.path().join("target/wpt")).unwrap();
        let fake_expectations = ws.path().join("actually-a-dir");
        std::fs::create_dir_all(&fake_expectations).unwrap();
        let out = run_list(ws.path(), "dirtest", &fake_expectations, 96).unwrap();
        assert!(
            out.is_none(),
            "expected Ok(None) when expectations_path is a directory (is_file() == false)"
        );
    }

    #[test]
    fn is_safe_list_name_accepts_valid_names() {
        assert!(is_safe_list_name("bugs"));
        assert!(is_safe_list_name("multicol-1"));
        assert!(is_safe_list_name("multicol_1"));
        assert!(is_safe_list_name("abc123"));
    }

    #[test]
    fn is_safe_list_name_rejects_unsafe_names() {
        assert!(!is_safe_list_name(""));
        assert!(!is_safe_list_name("../escape"));
        assert!(!is_safe_list_name("foo/bar"));
        assert!(!is_safe_list_name("foo.bar"));
        assert!(!is_safe_list_name("hello world"));
    }
}
