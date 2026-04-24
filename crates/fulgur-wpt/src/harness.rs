//! Run a single WPT reftest end-to-end: classify, render test + ref,
//! compare each page, and return an observed PASS/FAIL/SKIP.

use crate::expectations::Expectation;
use crate::reftest::{ReftestKind, classify};
use crate::render::render_test;
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct RunOutcome {
    pub observed: Expectation,
    pub reason: Option<String>,
    pub diff_dir: Option<PathBuf>,
}

/// Run one reftest and return an observed PASS/FAIL/SKIP.
///
/// - `test_html_path`: path to the test HTML; its parent is passed to
///   `render_test` as the base_path, and the rel=match href is resolved
///   relative to the same parent. Prefer an absolute path. Ref resolution
///   uses `test_html_path.parent().join(ref_href)`; if the test is a bare
///   filename, `parent()` is empty and `render_test` will canonicalize
///   against the current working directory.
/// - `work_dir`: scratch dir for PDFs/PNGs. `test/` and `ref/` subdirs
///   will be created under it.
/// - `diff_out_dir`: where to dump per-page diff PNGs on failure.
/// - `dpi`: pdftocairo rasterization resolution.
pub fn run_one(
    test_html_path: &Path,
    work_dir: &Path,
    diff_out_dir: &Path,
    dpi: u32,
    assets: Option<&fulgur::asset::AssetBundle>,
) -> Result<RunOutcome> {
    use fulgur_vrt::diff::{compare, write_diff_image};
    use fulgur_vrt::manifest::Tolerance;

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

    let test_dir = test_html_path
        .parent()
        .ok_or_else(|| anyhow!("test has no parent"))?;
    let ref_abs = test_dir.join(&ref_rel);

    let test_work = work_dir.join("test");
    let ref_work = work_dir.join("ref");
    let test_out = render_test(test_html_path, &test_work, dpi, assets)?;
    let ref_out = render_test(&ref_abs, &ref_work, dpi, assets)?;

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

    // Map FuzzyTolerance → fulgur_vrt::Tolerance. WPT fuzzy uses inclusive
    // ranges; fulgur-vrt uses a single upper-bound threshold. We take the
    // upper bound (most permissive) as the threshold — this matches the
    // semantics of "any diff within this range is acceptable".
    let max_ch = *fuzzy.max_diff.end();
    let max_total = *fuzzy.total_pixels.end();

    let mut first_over_threshold: Option<String> = None;
    for (idx, (t, r)) in test_out.pages.iter().zip(ref_out.pages.iter()).enumerate() {
        let total = u64::from(t.width()) * u64::from(t.height());
        // Note: ratio_limit may legitimately exceed 1.0 when the test's fuzzy
        // pixel cap is larger than the page area — fulgur-vrt's compare()
        // clamps to [0, 1] internally, so a >1 limit simply means "unbounded"
        // under WPT's upper-bound semantics.
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
        if !report.pass {
            if first_over_threshold.is_none() {
                first_over_threshold = Some(format!(
                    "page {} diff: {}/{} pixels exceed tol (max_ch={})",
                    idx + 1,
                    report.diff_pixels,
                    report.total_pixels,
                    report.max_channel_diff,
                ));
            }
            // Only Match cares about diff images — Mismatch differences are
            // expected, so no actionable artifact.
            if matches!(kind, Kind::Match) {
                std::fs::create_dir_all(diff_out_dir)?;
                let out_path = diff_out_dir.join(format!("page{}.diff.png", idx + 1));
                write_diff_image(r, t, tol, &out_path)?;
            }
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
            reason: Some("mismatch expected but test matches ref within tolerance".to_string()),
            diff_dir: None,
        },
    })
}
