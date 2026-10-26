//! Cross-engine comparison: render every fixture through another layout
//! backend and compare it, page by page, against the committed Blitz golden
//! and, where one exists, the committed Chromium golden.
//!
//! The Blitz goldens are asserted byte-equal to the current Blitz output by
//! `runner::run`, so they serve as the Blitz reference here without
//! rendering Blitz again. The Chromium goldens are PDFs printed by a pinned
//! Chrome build (see the `chrome` module), rasterized here like the others.
//! This is a measurement, not a gate: differences are recorded in
//! `report.json` and `summary.md`, never turned into errors.

use crate::diff;
use crate::manifest::{Fixture, Manifest, Tolerance};
use crate::pdf_render::{self, RenderSpec};
use crate::runner::RunnerContext;
use image::RgbaImage;
use serde::Serialize;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

/// Largest difference, in pixels per axis, between two page rasters that is
/// treated as rounding rather than a different page size. Chrome rounds the
/// paper to whole device pixels (A4 prints as 595.92 x 841.92 pt instead of
/// 595.28 x 841.89 pt), which moves the raster size by a pixel at 150 dpi.
/// Such pages are compared over their common area.
const SIZE_SLACK_PX: u32 = 2;

/// How one render compared against a reference.
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

/// Page-by-page comparison of one render against a reference.
#[derive(Debug, Clone, Serialize)]
pub struct PageComparison {
    pub outcome: Outcome,
    pub pages: Vec<PageDiff>,
}

impl PageComparison {
    /// The page with the largest share of differing pixels.
    pub fn worst_page(&self) -> Option<&PageDiff> {
        worst(&self.pages)
    }
}

/// Comparison of both engines against the Chromium golden.
#[derive(Debug, Clone, Serialize)]
pub struct ChromeComparison {
    /// Page count of the Chromium golden.
    pub pages: usize,
    /// The Blitz golden against the Chromium golden.
    pub blitz: PageComparison,
    /// The backend under comparison against the Chromium golden; `None` when
    /// it failed to render.
    pub engine: Option<PageComparison>,
}

/// Whether, and how, a fixture was compared against Chromium.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ChromeStatus {
    /// The manifest says Chromium's rendering is no reference for this
    /// fixture (`chrome_reference = false`).
    NoReference,
    /// The fixture should have a Chromium golden, but none is committed.
    NotGenerated,
    Compared(ChromeComparison),
}

/// Comparison result for one fixture.
#[derive(Debug, Clone, Serialize)]
pub struct FixtureComparison {
    pub path: PathBuf,
    /// The backend under comparison against the Blitz golden.
    pub outcome: Outcome,
    pub pages_reference: usize,
    pub pages_actual: Option<usize>,
    pub tolerance: Tolerance,
    pub pages: Vec<PageDiff>,
    pub error: Option<String>,
    pub chrome: ChromeStatus,
}

impl FixtureComparison {
    /// The page with the largest share of differing pixels against Blitz.
    pub fn worst_page(&self) -> Option<&PageDiff> {
        worst(&self.pages)
    }
}

/// Result of comparing every fixture for one backend.
#[derive(Debug, Clone, Serialize)]
pub struct CompareReport {
    /// Backend that produced the compared renders, e.g. `raikiri`.
    pub engine: String,
    /// Backend that produced the reference goldens.
    pub reference: String,
    /// Browser that printed the Chromium goldens (`goldens/chrome/VERSION`),
    /// when they exist.
    pub chrome_version: Option<String>,
    pub fixtures: Vec<FixtureComparison>,
}

impl CompareReport {
    pub fn count(&self, outcome: Outcome) -> usize {
        self.fixtures
            .iter()
            .filter(|f| f.outcome == outcome)
            .count()
    }

    /// Fixtures compared against Chromium, and how many of them match it
    /// for Blitz and for the backend under comparison.
    pub fn chrome_counts(&self) -> (usize, usize, usize) {
        let compared: Vec<_> = self
            .fixtures
            .iter()
            .filter_map(|f| match &f.chrome {
                ChromeStatus::Compared(c) => Some(c),
                _ => None,
            })
            .collect();
        let blitz = compared
            .iter()
            .filter(|c| c.blitz.outcome == Outcome::Match)
            .count();
        let engine = compared
            .iter()
            .filter(|c| c.engine.as_ref().map(|e| e.outcome) == Some(Outcome::Match))
            .count();
        (compared.len(), blitz, engine)
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
/// its Chromium golden, and write diff images for pages beyond the
/// fixture's tolerance under `report_dir/diff/<pair>/`.
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
    let chrome_dir = ctx.goldens_dir.join("chrome");
    let chrome_version = std::fs::read_to_string(chrome_dir.join("VERSION"))
        .ok()
        .map(|v| v.trim().to_string());

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
        let blitz_pages = rasterize_file(&golden, fx.dpi, &work_dir.join("ref"))?;
        let chrome_golden = chrome_dir.join(fx.path.with_extension("pdf"));
        let chrome_pages = if fx.chrome_reference && chrome_golden.is_file() {
            Some(rasterize_file(
                &chrome_golden,
                fx.dpi,
                &work_dir.join("chrome"),
            )?)
        } else {
            None
        };

        let actual = render_pages(fx, &html, render, &work_dir)?;
        let compare = |reference: &[RgbaImage], actual: &[RgbaImage], pair: &str| {
            compare_pages(
                reference,
                actual,
                fx.tolerance_chrome,
                &report_dir.join("diff").join(pair),
                &fx.path,
            )
        };

        let (outcome, pages) = match &actual {
            Ok(pages) => {
                let c = compare(&blitz_pages, pages, &format!("blitz-{engine}"))?;
                (c.outcome, c.pages)
            }
            Err(_) => (Outcome::RenderError, Vec::new()),
        };
        let chrome = match &chrome_pages {
            Some(chrome_pages) => ChromeStatus::Compared(ChromeComparison {
                pages: chrome_pages.len(),
                blitz: compare(chrome_pages, &blitz_pages, "chrome-blitz")?,
                engine: match &actual {
                    Ok(pages) => Some(compare(chrome_pages, pages, &format!("chrome-{engine}"))?),
                    Err(_) => None,
                },
            }),
            None if fx.chrome_reference => ChromeStatus::NotGenerated,
            None => ChromeStatus::NoReference,
        };

        results.push(FixtureComparison {
            path: fx.path.clone(),
            outcome,
            pages_reference: blitz_pages.len(),
            pages_actual: actual.as_ref().ok().map(Vec::len),
            tolerance: fx.tolerance_chrome,
            pages,
            error: actual.err(),
            chrome,
        });
    }

    Ok(CompareReport {
        engine: engine.to_string(),
        reference: "blitz".to_string(),
        chrome_version,
        fixtures: results,
    })
}

/// Render a fixture through `render` and rasterize the result. A backend
/// error or panic comes back as `Ok(Err(message))`.
fn render_pages(
    fx: &Fixture,
    html: &str,
    render: &RenderFn<'_>,
    work_dir: &Path,
) -> anyhow::Result<Result<Vec<RgbaImage>, String>> {
    let spec = RenderSpec {
        page_size: &fx.page_size,
        margin_pt: fx.margin_pt,
        dpi: fx.dpi,
        bookmarks: fx.bookmarks,
    };
    let pdf = match catch_unwind(AssertUnwindSafe(|| render(html, spec))) {
        Ok(Ok(pdf)) => pdf,
        Ok(Err(e)) => return Ok(Err(format!("{e:#}"))),
        Err(payload) => return Ok(Err(format!("panic: {}", panic_message(&payload)))),
    };
    let path = work_dir.join("actual.pdf");
    std::fs::create_dir_all(work_dir)?;
    std::fs::write(&path, &pdf)?;
    Ok(Ok(rasterize_file(&path, fx.dpi, &work_dir.join("act"))?))
}

/// Compare the pages two renders have in common and classify the result.
/// Diff images for pages beyond `tol` are written under `diff_dir`, at
/// [`diff_png_name`]; the recorded paths are relative to the report
/// directory, two levels up.
fn compare_pages(
    reference_pages: &[RgbaImage],
    actual_pages: &[RgbaImage],
    tol: Tolerance,
    diff_dir: &Path,
    fixture: &Path,
) -> anyhow::Result<PageComparison> {
    let mut pages = Vec::with_capacity(reference_pages.len().min(actual_pages.len()));
    for (i, (reference, actual)) in reference_pages.iter().zip(actual_pages).enumerate() {
        let page = i + 1;
        let (reference, actual) = common_area(reference, actual);
        let report = diff::compare(&reference, &actual, tol);
        let diff_png = if report.pass {
            None
        } else {
            let name = diff_png_name(fixture, page);
            diff::write_diff_image(&reference, &actual, tol, &diff_dir.join(&name))?;
            Some(relative_to_report(diff_dir).join(name))
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
    Ok(PageComparison { outcome, pages })
}

/// Crop two rasters to their common area when their sizes differ by at most
/// [`SIZE_SLACK_PX`] per axis. Larger differences are left alone, so
/// `diff::compare` reports them as a size mismatch.
fn common_area<'a>(
    a: &'a RgbaImage,
    b: &'a RgbaImage,
) -> (
    std::borrow::Cow<'a, RgbaImage>,
    std::borrow::Cow<'a, RgbaImage>,
) {
    use std::borrow::Cow;
    let (aw, ah) = a.dimensions();
    let (bw, bh) = b.dimensions();
    if (aw, ah) == (bw, bh) || aw.abs_diff(bw) > SIZE_SLACK_PX || ah.abs_diff(bh) > SIZE_SLACK_PX {
        return (Cow::Borrowed(a), Cow::Borrowed(b));
    }
    let (w, h) = (aw.min(bw), ah.min(bh));
    let crop = |img: &RgbaImage| image::imageops::crop_imm(img, 0, 0, w, h).to_image();
    (Cow::Owned(crop(a)), Cow::Owned(crop(b)))
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
    let engine = &report.engine;
    let reference = &report.reference;
    let mut md = String::new();
    let _ = writeln!(
        md,
        "## fulgur-vrt: {engine} vs {reference} and Chromium goldens\n"
    );
    let _ = writeln!(
        md,
        "Against {reference}: {} fixtures, {} match, {} differ, {} page count mismatch, {} render error.\n",
        report.fixtures.len(),
        report.count(Outcome::Match),
        report.count(Outcome::Differs),
        report.count(Outcome::PageCountMismatch),
        report.count(Outcome::RenderError),
    );
    let (compared, blitz_match, engine_match) = report.chrome_counts();
    match &report.chrome_version {
        Some(version) => {
            let _ = writeln!(
                md,
                "Against Chromium ({version}): {compared} fixtures with a reference, \
                 {reference} matches {blitz_match}, {engine} matches {engine_match}.\n"
            );
        }
        None => md.push_str("Chromium goldens are not generated yet.\n\n"),
    }
    md.push_str(
        "Pages are rasterized with pdftocairo and compared pixel by pixel. \
         The tolerance is the manifest's `tolerance_chrome`, used provisionally \
         until the cross-engine thresholds are decided. \
         The Blitz goldens stand in for Blitz output, so the numbers hold only \
         on hosts where `cargo test -p fulgur-vrt` passes (fonts differ elsewhere). \
         Fixtures marked `chrome_reference = false` use GCPM constructs Chromium \
         does not implement and have no Chromium column.\n\n",
    );
    let _ = writeln!(
        md,
        "| fixture | {engine} vs {reference} | pages ({reference} / {engine} / Chromium) | worst page diff | max channel diff | {reference} vs Chromium | {engine} vs Chromium |"
    );
    md.push_str("|---|---|---|---|---|---|---|\n");
    for f in &report.fixtures {
        let actual_pages = f
            .pages_actual
            .map_or_else(|| "-".to_string(), |n| n.to_string());
        let chrome_pages = match &f.chrome {
            ChromeStatus::Compared(c) => c.pages.to_string(),
            _ => "-".to_string(),
        };
        let pages = format!("{} / {actual_pages} / {chrome_pages}", f.pages_reference);
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
        let (blitz_chrome, engine_chrome) = match &f.chrome {
            ChromeStatus::Compared(c) => {
                (chrome_cell(Some(&c.blitz)), chrome_cell(c.engine.as_ref()))
            }
            ChromeStatus::NotGenerated => {
                ("not generated".to_string(), "not generated".to_string())
            }
            ChromeStatus::NoReference => ("n/a".to_string(), "n/a".to_string()),
        };
        let _ = writeln!(
            md,
            "| `{}` | {result} | {pages} | {worst} | {max_channel} | {blitz_chrome} | {engine_chrome} |",
            f.path.display(),
        );
    }
    md
}

/// One Chromium cell: the outcome and the worst page's share of differing
/// pixels.
fn chrome_cell(comparison: Option<&PageComparison>) -> String {
    let Some(c) = comparison else {
        return Outcome::RenderError.label().to_string();
    };
    match c.worst_page() {
        Some(p) => format!(
            "{} {:.2}% (p{})",
            c.outcome.label(),
            p.diff_ratio * 100.0,
            p.page
        ),
        None => c.outcome.label().to_string(),
    }
}

fn worst(pages: &[PageDiff]) -> Option<&PageDiff> {
    pages
        .iter()
        .max_by(|a, b| a.diff_ratio.total_cmp(&b.diff_ratio))
}

fn rasterize_file(pdf: &Path, dpi: u32, work_dir: &Path) -> anyhow::Result<Vec<RgbaImage>> {
    pdf_render::pdf_to_rgba_pages(pdf, dpi, work_dir)
        .map_err(|e| anyhow::anyhow!("failed to rasterize {}: {e}", pdf.display()))
}

/// File name of a page's diff image, keeping the fixture's directory.
fn diff_png_name(fixture: &Path, page: usize) -> PathBuf {
    let stem = fixture.with_extension("");
    PathBuf::from(format!("{}.p{page}.diff.png", stem.display()))
}

/// `diff/<pair>` from `<report>/diff/<pair>`.
fn relative_to_report(diff_dir: &Path) -> PathBuf {
    let mut parts: Vec<_> = diff_dir.iter().rev().take(2).collect();
    parts.reverse();
    parts.iter().collect()
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
            tolerance: TOL,
            pages,
            error: None,
            chrome: ChromeStatus::NoReference,
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

    fn compared(outcome: Outcome, pages: Vec<PageDiff>) -> PageComparison {
        PageComparison { outcome, pages }
    }

    fn report(fixtures: Vec<FixtureComparison>) -> CompareReport {
        CompareReport {
            engine: "raikiri".to_string(),
            reference: "blitz".to_string(),
            chrome_version: None,
            fixtures,
        }
    }

    #[test]
    fn diff_png_name_keeps_the_fixture_directory() {
        assert_eq!(
            diff_png_name(Path::new("gcpm/running.html"), 2),
            PathBuf::from("gcpm/running.p2.diff.png")
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
        let md = summary_markdown(&report(vec![
            fixture("basic/a.html", Outcome::Match, vec![page(1, 0.0)]),
            fixture("basic/b.html", Outcome::Differs, vec![page(1, 0.5)]),
            broken,
        ]));
        assert!(
            md.contains(
                "Against blitz: 3 fixtures, 1 match, 1 differ, 0 page count mismatch, 1 render error."
            ),
            "{md}"
        );
        assert!(md.contains("Chromium goldens are not generated yet."));
        assert!(
            md.contains("| `basic/b.html` | differs | 1 / 1 / - | 50.00% (p1) | 200 | n/a | n/a |"),
            "{md}"
        );
        assert!(md.contains("render error: bad \\| input |"), "{md}");
        assert!(!md.contains("second line"));
    }

    #[test]
    fn summary_shows_the_chromium_columns() {
        let mut both = fixture("paint/a.html", Outcome::Differs, vec![page(1, 0.5)]);
        both.chrome = ChromeStatus::Compared(ChromeComparison {
            pages: 1,
            blitz: compared(Outcome::Match, vec![page(1, 0.01)]),
            engine: Some(compared(Outcome::Differs, vec![page(1, 0.4)])),
        });
        let mut engine_failed = fixture("svg/b.html", Outcome::RenderError, Vec::new());
        engine_failed.pages_actual = None;
        engine_failed.error = Some("boom".to_string());
        engine_failed.chrome = ChromeStatus::Compared(ChromeComparison {
            pages: 2,
            blitz: compared(Outcome::PageCountMismatch, vec![page(1, 0.0)]),
            engine: None,
        });
        let mut missing = fixture("layout/c.html", Outcome::Match, vec![page(1, 0.0)]);
        missing.chrome = ChromeStatus::NotGenerated;
        let mut r = report(vec![both, engine_failed, missing]);
        r.chrome_version = Some("Chrome/155.0.8059.84".to_string());

        assert_eq!(r.chrome_counts(), (2, 1, 0));
        let md = summary_markdown(&r);
        assert!(
            md.contains(
                "Against Chromium (Chrome/155.0.8059.84): 2 fixtures with a reference, \
                 blitz matches 1, raikiri matches 0."
            ),
            "{md}"
        );
        assert!(
            md.contains(
                "| 1 / 1 / 1 | 50.00% (p1) | 200 | match 1.00% (p1) | differs 40.00% (p1) |"
            ),
            "{md}"
        );
        assert!(
            md.contains("| 1 / - / 2 | - | - | page count 0.00% (p1) | render error |"),
            "{md}"
        );
        assert!(md.contains("| not generated | not generated |"), "{md}");
    }

    #[test]
    fn report_serializes_outcomes_and_chrome_status() {
        let mut f = fixture(
            "layout/pages.html",
            Outcome::PageCountMismatch,
            vec![page(1, 0.0)],
        );
        f.chrome = ChromeStatus::Compared(ChromeComparison {
            pages: 1,
            blitz: compared(Outcome::Match, Vec::new()),
            engine: None,
        });
        let json = serde_json::to_string(&report(vec![f])).unwrap();
        assert!(
            json.contains(r#""outcome":"page-count-mismatch""#),
            "{json}"
        );
        assert!(
            json.contains(r#""chrome":{"status":"compared","pages":1"#),
            "{json}"
        );
        let none = serde_json::to_string(&ChromeStatus::NoReference).unwrap();
        assert_eq!(none, r#"{"status":"no-reference"}"#);
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
        let c = compare_pages(
            &[white.clone(), white.clone()],
            &[white.clone(), white],
            TOL,
            &out.path().join("diff/blitz-raikiri"),
            Path::new("a/b.html"),
        )
        .unwrap();
        assert_eq!(c.outcome, Outcome::Match);
        assert_eq!(c.pages.len(), 2);
        assert!(c.pages.iter().all(|p| p.diff_png.is_none()));
        assert!(!out.path().join("diff").exists());
    }

    #[test]
    fn a_differing_page_gets_a_diff_image() {
        let out = tempfile::tempdir().unwrap();
        let c = compare_pages(
            &[solid([255, 255, 255, 255]), solid([255, 255, 255, 255])],
            &[solid([255, 255, 255, 255]), solid([0, 0, 0, 255])],
            TOL,
            &out.path().join("diff/chrome-raikiri"),
            Path::new("a/b.html"),
        )
        .unwrap();
        assert_eq!(c.outcome, Outcome::Differs);
        assert!(c.pages[0].within_tolerance);
        assert_eq!(c.pages[1].diff_ratio, 1.0);
        let rel = c.pages[1].diff_png.as_ref().expect("diff image for page 2");
        assert_eq!(rel, Path::new("diff/chrome-raikiri/a/b.p2.diff.png"));
        assert!(out.path().join(rel).is_file());
    }

    #[test]
    fn a_page_count_difference_wins_over_matching_pages() {
        let out = tempfile::tempdir().unwrap();
        let white = solid([255, 255, 255, 255]);
        let c = compare_pages(
            std::slice::from_ref(&white),
            &[white.clone(), white.clone()],
            TOL,
            out.path(),
            Path::new("a.html"),
        )
        .unwrap();
        assert_eq!(c.outcome, Outcome::PageCountMismatch);
        assert_eq!(c.pages.len(), 1, "only shared pages are compared");
    }

    #[test]
    fn a_pixel_of_rounding_in_the_page_size_is_cropped() {
        let out = tempfile::tempdir().unwrap();
        let reference = RgbaImage::from_pixel(10, 10, image::Rgba([255, 255, 255, 255]));
        let rounded = RgbaImage::from_pixel(11, 10, image::Rgba([255, 255, 255, 255]));
        let c = compare_pages(
            std::slice::from_ref(&reference),
            &[rounded],
            TOL,
            out.path(),
            Path::new("a.html"),
        )
        .unwrap();
        assert_eq!(c.outcome, Outcome::Match);
        assert_eq!(c.pages[0].total_pixels, 100);

        let other_size = RgbaImage::from_pixel(20, 10, image::Rgba([255, 255, 255, 255]));
        let c = compare_pages(
            &[reference],
            &[other_size],
            TOL,
            out.path(),
            Path::new("a.html"),
        )
        .unwrap();
        assert_eq!(
            c.outcome,
            Outcome::Differs,
            "a real size difference stays a difference"
        );
    }
}
