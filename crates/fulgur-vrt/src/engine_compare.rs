//! Cross-engine comparison: render every fixture through another layout
//! backend and compare it, page by page, against the committed Blitz golden.
//!
//! The Blitz goldens are asserted byte-equal to the current Blitz output by
//! `runner::run`, so they serve as the Blitz reference here without
//! rendering Blitz again. This is a measurement, not a gate: differences
//! are recorded in `report.json` and `summary.md`, never turned into errors.

use crate::diff;
use crate::manifest::{Manifest, Tolerance};
use crate::pdf_render::{self, RenderSpec};
use crate::runner::RunnerContext;
use image::RgbaImage;
use serde::Serialize;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

/// How one fixture compared against its Blitz golden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Same page count, every page within the tolerance.
    Match,
    /// Same page count, at least one page beyond the tolerance.
    Differs,
    /// The two renders have a different number of pages.
    PageCountMismatch,
    /// The backend returned an error or panicked.
    RenderError,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Differs => "differs",
            Self::PageCountMismatch => "page count",
            Self::RenderError => "render error",
        }
    }
}

/// Pixel comparison of one page present in both renders.
#[derive(Debug, Clone, Serialize)]
pub struct PageDiff {
    /// 1-based page number.
    pub page: usize,
    pub total_pixels: u64,
    pub diff_pixels: u64,
    pub diff_ratio: f32,
    pub max_channel_diff: u8,
    pub within_tolerance: bool,
    /// Diff image, relative to the report directory. Written only for pages
    /// beyond the tolerance.
    pub diff_png: Option<PathBuf>,
}

/// Comparison result for one fixture.
#[derive(Debug, Clone, Serialize)]
pub struct FixtureComparison {
    pub path: PathBuf,
    pub outcome: Outcome,
    pub pages_reference: usize,
    pub pages_actual: Option<usize>,
    pub tolerance: Tolerance,
    pub pages: Vec<PageDiff>,
    pub error: Option<String>,
}

impl FixtureComparison {
    /// The page with the largest share of differing pixels.
    pub fn worst_page(&self) -> Option<&PageDiff> {
        self.pages
            .iter()
            .max_by(|a, b| a.diff_ratio.total_cmp(&b.diff_ratio))
    }
}

/// Result of comparing every fixture for one backend.
#[derive(Debug, Clone, Serialize)]
pub struct CompareReport {
    /// Backend that produced the compared renders, e.g. `raikiri`.
    pub engine: String,
    /// Backend that produced the reference goldens.
    pub reference: String,
    pub fixtures: Vec<FixtureComparison>,
}

impl CompareReport {
    pub fn count(&self, outcome: Outcome) -> usize {
        self.fixtures
            .iter()
            .filter(|f| f.outcome == outcome)
            .count()
    }
}

/// Renders a fixture to PDF bytes through the backend under comparison.
pub type RenderFn<'a> = dyn Fn(&str, RenderSpec<'_>) -> anyhow::Result<Vec<u8>> + 'a;

/// Directory receiving the comparison report for `engine`:
/// `<workspace>/target/vrt-report/<engine>`.
pub fn report_dir(ctx: &RunnerContext, engine: &str) -> PathBuf {
    ctx.diff_out_dir
        .parent()
        .map(|target| target.join("vrt-report"))
        .unwrap_or_else(|| ctx.crate_root.join("target").join("vrt-report"))
        .join(engine)
}

/// Compare every fixture rendered by `render` against its Blitz golden and
/// write diff images for pages beyond the fixture's tolerance under
/// `report_dir`.
///
/// The tolerance is the manifest's `tolerance_chrome`, used as a provisional
/// threshold until the cross-engine thresholds are decided.
///
/// # Errors
/// Fails when the manifest, a fixture or a golden cannot be read, or when
/// `pdftocairo` cannot rasterize a PDF. Backend failures are recorded as
/// `Outcome::RenderError` instead.
pub fn compare_with_goldens(
    ctx: &RunnerContext,
    engine: &str,
    render: &RenderFn<'_>,
    report_dir: &Path,
) -> anyhow::Result<CompareReport> {
    let manifest = Manifest::load(&ctx.crate_root.join("manifest.toml"))?;
    let mut fixtures = manifest.fixtures.clone();
    fixtures.sort_by(|a, b| a.path.cmp(&b.path));

    let work_root = tempfile::tempdir()?;
    let mut results = Vec::with_capacity(fixtures.len());

    for (idx, fx) in fixtures.iter().enumerate() {
        let html = std::fs::read_to_string(ctx.fixtures_dir.join(&fx.path))
            .map_err(|e| anyhow::anyhow!("failed to read fixture {}: {e}", fx.path.display()))?;
        let golden = ctx
            .goldens_dir
            .join("fulgur")
            .join(fx.path.with_extension("pdf"));
        let work_dir = work_root.path().join(format!("fx-{idx}"));
        let reference_pages = rasterize_file(&golden, fx.dpi, &work_dir.join("ref"))?;

        let spec = RenderSpec {
            page_size: &fx.page_size,
            margin_pt: fx.margin_pt,
            dpi: fx.dpi,
            bookmarks: fx.bookmarks,
        };
        let mut result = FixtureComparison {
            path: fx.path.clone(),
            outcome: Outcome::RenderError,
            pages_reference: reference_pages.len(),
            pages_actual: None,
            tolerance: fx.tolerance_chrome,
            pages: Vec::new(),
            error: None,
        };

        let actual_pdf = match catch_unwind(AssertUnwindSafe(|| render(&html, spec))) {
            Ok(Ok(pdf)) => pdf,
            Ok(Err(e)) => {
                result.error = Some(format!("{e:#}"));
                results.push(result);
                continue;
            }
            Err(payload) => {
                result.error = Some(format!("panic: {}", panic_message(payload.as_ref())));
                results.push(result);
                continue;
            }
        };
        let actual_path = work_dir.join("actual.pdf");
        std::fs::create_dir_all(&work_dir)?;
        std::fs::write(&actual_path, &actual_pdf)?;
        // A PDF the backend wrote but pdftocairo cannot read is the backend's
        // failure, not the harness's: record it and go on.
        let actual_pages = match rasterize_file(&actual_path, fx.dpi, &work_dir.join("act")) {
            Ok(pages) => pages,
            Err(e) => {
                result.error = Some(format!("{e:#}"));
                results.push(result);
                continue;
            }
        };
        result.pages_actual = Some(actual_pages.len());
        (result.pages, result.outcome) = compare_pages(
            &reference_pages,
            &actual_pages,
            fx.tolerance_chrome,
            &fx.path,
            report_dir,
        )?;
        results.push(result);
    }

    Ok(CompareReport {
        engine: engine.to_string(),
        reference: "blitz".to_string(),
        fixtures: results,
    })
}

/// Compare the pages two renders have in common and classify the fixture.
/// Diff images for pages beyond `tol` are written under `report_dir`.
fn compare_pages(
    reference_pages: &[RgbaImage],
    actual_pages: &[RgbaImage],
    tol: Tolerance,
    fixture: &Path,
    report_dir: &Path,
) -> anyhow::Result<(Vec<PageDiff>, Outcome)> {
    let mut pages = Vec::with_capacity(reference_pages.len().min(actual_pages.len()));
    for (i, (reference, actual)) in reference_pages.iter().zip(actual_pages).enumerate() {
        let page = i + 1;
        let report = diff::compare(reference, actual, tol);
        let diff_png = if report.pass {
            None
        } else {
            let rel = diff_png_path(fixture, page);
            diff::write_diff_image(reference, actual, tol, &report_dir.join(&rel))?;
            Some(rel)
        };
        pages.push(PageDiff {
            page,
            total_pixels: report.total_pixels,
            diff_pixels: report.diff_pixels,
            diff_ratio: report.ratio(),
            max_channel_diff: report.max_channel_diff,
            within_tolerance: report.pass,
            diff_png,
        });
    }

    let outcome = if reference_pages.len() != actual_pages.len() {
        Outcome::PageCountMismatch
    } else if pages.iter().all(|p| p.within_tolerance) {
        Outcome::Match
    } else {
        Outcome::Differs
    };
    Ok((pages, outcome))
}

/// Write `report.json` and `summary.md` for `report` into `report_dir`.
///
/// # Errors
/// Fails when the files cannot be written.
pub fn write_report(report: &CompareReport, report_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(report_dir)?;
    std::fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(report)?,
    )?;
    std::fs::write(report_dir.join("summary.md"), summary_markdown(report))?;
    Ok(())
}

/// Markdown summary with one row per fixture, for the CI step summary.
pub fn summary_markdown(report: &CompareReport) -> String {
    let mut md = String::new();
    let _ = writeln!(
        md,
        "## fulgur-vrt: {} vs {} goldens\n",
        report.engine, report.reference
    );
    let _ = writeln!(
        md,
        "{} fixtures: {} match, {} differ, {} page count mismatch, {} render error.\n",
        report.fixtures.len(),
        report.count(Outcome::Match),
        report.count(Outcome::Differs),
        report.count(Outcome::PageCountMismatch),
        report.count(Outcome::RenderError),
    );
    md.push_str(
        "Pages are rasterized with pdftocairo and compared pixel by pixel. \
         The tolerance is the manifest's `tolerance_chrome`, used provisionally \
         until the cross-engine thresholds are decided. \
         The Blitz goldens stand in for Blitz output, so the numbers hold only \
         on hosts where `cargo test -p fulgur-vrt` passes (fonts differ elsewhere). \
         Chromium goldens are not generated yet.\n\n",
    );
    let _ = writeln!(
        md,
        "| fixture | result | pages ({} / {}) | worst page diff | max channel diff | Chromium |",
        report.reference, report.engine
    );
    md.push_str("|---|---|---|---|---|---|\n");
    for f in &report.fixtures {
        let pages = match f.pages_actual {
            Some(n) => format!("{} / {}", f.pages_reference, n),
            None => format!("{} / -", f.pages_reference),
        };
        let (worst, max_channel) = match f.worst_page() {
            Some(p) => (
                format!("{:.2}% (p{})", p.diff_ratio * 100.0, p.page),
                p.max_channel_diff.to_string(),
            ),
            None => ("-".to_string(), "-".to_string()),
        };
        let result = match &f.error {
            Some(e) => format!("{}: {}", f.outcome.label(), table_cell(e)),
            None => f.outcome.label().to_string(),
        };
        let _ = writeln!(
            md,
            "| `{}` | {} | {} | {} | {} | n/a |",
            f.path.display(),
            result,
            pages,
            worst,
            max_channel
        );
    }
    md
}

fn rasterize_file(pdf: &Path, dpi: u32, work_dir: &Path) -> anyhow::Result<Vec<image::RgbaImage>> {
    pdf_render::pdf_to_rgba_pages(pdf, dpi, work_dir)
        .map_err(|e| anyhow::anyhow!("failed to rasterize {}: {e}", pdf.display()))
}

fn diff_png_path(fixture: &Path, page: usize) -> PathBuf {
    let stem = fixture.with_extension("");
    PathBuf::from("diff").join(format!("{}.p{page}.diff.png", stem.display()))
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Keep a message on one Markdown table row.
fn table_cell(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").replace('|', "\\|");
    if line.chars().count() > 120 {
        format!("{}…", line.chars().take(120).collect::<String>())
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &str, outcome: Outcome, pages: Vec<PageDiff>) -> FixtureComparison {
        FixtureComparison {
            path: PathBuf::from(path),
            outcome,
            pages_reference: 1,
            pages_actual: Some(pages.len()),
            tolerance: Tolerance {
                max_channel_diff: 16,
                max_diff_pixels_ratio: 0.02,
            },
            pages,
            error: None,
        }
    }

    fn page(page: usize, diff_ratio: f32) -> PageDiff {
        PageDiff {
            page,
            total_pixels: 100,
            diff_pixels: (diff_ratio * 100.0) as u64,
            diff_ratio,
            max_channel_diff: 200,
            within_tolerance: diff_ratio <= 0.02,
            diff_png: None,
        }
    }

    #[test]
    fn diff_png_path_keeps_the_fixture_directory() {
        assert_eq!(
            diff_png_path(Path::new("gcpm/running.html"), 2),
            PathBuf::from("diff/gcpm/running.p2.diff.png")
        );
    }

    #[test]
    fn worst_page_picks_the_largest_ratio() {
        let f = fixture(
            "a.html",
            Outcome::Differs,
            vec![page(1, 0.01), page(2, 0.30), page(3, 0.05)],
        );
        assert_eq!(f.worst_page().map(|p| p.page), Some(2));
    }

    #[test]
    fn summary_counts_outcomes_and_escapes_errors() {
        let mut broken = fixture("svg/filter.html", Outcome::RenderError, Vec::new());
        broken.pages_actual = None;
        broken.error = Some("bad | input\nsecond line".to_string());
        let report = CompareReport {
            engine: "raikiri".to_string(),
            reference: "blitz".to_string(),
            fixtures: vec![
                fixture("basic/a.html", Outcome::Match, vec![page(1, 0.0)]),
                fixture("basic/b.html", Outcome::Differs, vec![page(1, 0.5)]),
                broken,
            ],
        };
        let md = summary_markdown(&report);
        assert!(
            md.contains("3 fixtures: 1 match, 1 differ, 0 page count mismatch, 1 render error."),
            "{md}"
        );
        assert!(md.contains("| `basic/b.html` | differs | 1 / 1 | 50.00% (p1) | 200 | n/a |"));
        assert!(md.contains("render error: bad \\| input |"), "{md}");
        assert!(!md.contains("second line"));
    }

    #[test]
    fn report_serializes_outcomes_in_kebab_case() {
        let report = CompareReport {
            engine: "raikiri".to_string(),
            reference: "blitz".to_string(),
            fixtures: vec![fixture(
                "layout/pages.html",
                Outcome::PageCountMismatch,
                vec![page(1, 0.0)],
            )],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(
            json.contains(r#""outcome":"page-count-mismatch""#),
            "{json}"
        );
    }

    fn solid(rgba: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(10, 10, image::Rgba(rgba))
    }

    const TOL: Tolerance = Tolerance {
        max_channel_diff: 16,
        max_diff_pixels_ratio: 0.02,
    };

    #[test]
    fn identical_pages_match_without_diff_images() {
        let out = tempfile::tempdir().unwrap();
        let white = solid([255, 255, 255, 255]);
        let (pages, outcome) = compare_pages(
            &[white.clone(), white.clone()],
            &[white.clone(), white],
            TOL,
            Path::new("a/b.html"),
            out.path(),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Match);
        assert_eq!(pages.len(), 2);
        assert!(pages.iter().all(|p| p.diff_png.is_none()));
        assert!(!out.path().join("diff").exists());
    }

    #[test]
    fn a_differing_page_gets_a_diff_image() {
        let out = tempfile::tempdir().unwrap();
        let (pages, outcome) = compare_pages(
            &[solid([255, 255, 255, 255]), solid([255, 255, 255, 255])],
            &[solid([255, 255, 255, 255]), solid([0, 0, 0, 255])],
            TOL,
            Path::new("a/b.html"),
            out.path(),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Differs);
        assert!(pages[0].within_tolerance);
        assert_eq!(pages[1].diff_ratio, 1.0);
        let rel = pages[1].diff_png.as_ref().expect("diff image for page 2");
        assert_eq!(rel, Path::new("diff/a/b.p2.diff.png"));
        assert!(out.path().join(rel).is_file());
    }

    #[test]
    fn a_page_count_difference_wins_over_matching_pages() {
        let out = tempfile::tempdir().unwrap();
        let white = solid([255, 255, 255, 255]);
        let (pages, outcome) = compare_pages(
            std::slice::from_ref(&white),
            &[white.clone(), white.clone()],
            TOL,
            Path::new("a.html"),
            out.path(),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::PageCountMismatch);
        assert_eq!(pages.len(), 1, "only shared pages are compared");
    }

    /// A fixture whose HTML names the case the fake backend should play.
    fn case_html(case: &str, color: &str) -> String {
        format!(
            "<!DOCTYPE html><!-- case:{case} --><html><body style=\"margin:0\">\
             <div style=\"height:400px;background:{color}\"></div></body></html>"
        )
    }

    /// A backend that matches, differs, fails, panics or writes a broken PDF,
    /// depending on the fixture.
    fn fake_backend(html: &str, spec: RenderSpec<'_>) -> anyhow::Result<Vec<u8>> {
        if html.contains("case:differs") {
            pdf_render::render_html_to_pdf(&html.replace("#ff0000", "#0000ff"), spec)
        } else if html.contains("case:error") {
            anyhow::bail!("backend refused the input")
        } else if html.contains("case:panic") {
            panic!("backend panicked")
        } else if html.contains("case:broken") {
            Ok(b"not a pdf".to_vec())
        } else {
            pdf_render::render_html_to_pdf(html, spec)
        }
    }

    #[test]
    fn compare_with_goldens_records_every_outcome_and_writes_the_report() {
        let root = tempfile::tempdir().unwrap();
        let ctx = RunnerContext {
            crate_root: root.path().to_path_buf(),
            fixtures_dir: root.path().join("fixtures"),
            goldens_dir: root.path().join("goldens"),
            diff_out_dir: root.path().join("target").join("vrt-diff"),
            update_mode: crate::runner::UpdateMode::Off,
        };
        let cases = ["broken", "differs", "error", "match", "panic"];
        let mut manifest = String::from(
            "[defaults]\ntolerance_chrome = { max_channel_diff = 16, max_diff_pixels_ratio = 0.02 }\n",
        );
        for case in cases {
            let rel = PathBuf::from("c").join(format!("{case}.html"));
            let html = case_html(case, "#ff0000");
            std::fs::create_dir_all(ctx.fixtures_dir.join("c")).unwrap();
            std::fs::write(ctx.fixtures_dir.join(&rel), &html).unwrap();
            let golden = ctx
                .goldens_dir
                .join("fulgur")
                .join(rel.with_extension("pdf"));
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            let spec = RenderSpec {
                page_size: "A4",
                margin_pt: Some(0.0),
                dpi: 150,
                bookmarks: false,
            };
            std::fs::write(
                &golden,
                pdf_render::render_html_to_pdf(&html, spec).unwrap(),
            )
            .unwrap();
            manifest.push_str(&format!(
                "[[fixture]]\npath = \"{}\"\nmargin_pt = 0.0\n",
                rel.display()
            ));
        }
        std::fs::write(root.path().join("manifest.toml"), manifest).unwrap();

        let out = report_dir(&ctx, "fake");
        assert_eq!(out, root.path().join("target/vrt-report/fake"));
        let report = compare_with_goldens(&ctx, "fake", &fake_backend, &out).unwrap();
        write_report(&report, &out).unwrap();

        let outcomes: Vec<_> = report.fixtures.iter().map(|f| f.outcome).collect();
        assert_eq!(
            outcomes,
            [
                Outcome::RenderError,
                Outcome::Differs,
                Outcome::RenderError,
                Outcome::Match,
                Outcome::RenderError,
            ]
        );
        let errors: Vec<_> = report.fixtures.iter().map(|f| f.error.as_deref()).collect();
        assert!(
            errors[0].unwrap().contains("failed to rasterize"),
            "{errors:?}"
        );
        assert_eq!(errors[2], Some("backend refused the input"));
        assert_eq!(errors[4], Some("panic: backend panicked"));
        let differs = &report.fixtures[1];
        let diff_png = differs.pages[0].diff_png.as_ref().expect("diff image");
        assert!(out.join(diff_png).is_file());
        assert!(out.join("report.json").is_file());
        let summary = std::fs::read_to_string(out.join("summary.md")).unwrap();
        assert!(summary.contains("1 match, 1 differ, 0 page count mismatch, 3 render error"));
    }

    #[test]
    fn panic_messages_and_long_errors_fit_a_table_row() {
        let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("owned message"));
        assert_eq!(panic_message(owned.as_ref()), "owned message");
        let other: Box<dyn std::any::Any + Send> = Box::new(7_u8);
        assert_eq!(panic_message(other.as_ref()), "non-string panic payload");

        let long = "x".repeat(200);
        let cell = table_cell(&long);
        assert_eq!(cell.chars().count(), 121);
        assert!(cell.ends_with('…'));
        assert_eq!(Outcome::PageCountMismatch.label(), "page count");
    }
}
