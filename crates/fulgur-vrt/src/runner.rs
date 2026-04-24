//! Orchestrates VRT: walks the manifest, renders each fixture through fulgur,
//! compares against committed fulgur goldens (PDF byte-wise), and — depending
//! on `FULGUR_VRT_UPDATE` — either writes diff artifacts on failure or updates
//! goldens in place.

use crate::diff;
use crate::manifest::Manifest;
use crate::pdf_render::{self, RenderSpec};
use std::path::{Path, PathBuf};

/// Controls whether the runner compares against goldens (`Off`), rewrites
/// every golden from the current fulgur output (`All`), or rewrites only the
/// goldens whose comparison failed (`Failing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMode {
    Off,
    All,
    Failing,
}

impl UpdateMode {
    /// Parse from the `FULGUR_VRT_UPDATE` environment variable.
    ///
    /// - `1` or `all` → `UpdateMode::All`
    /// - `failing` → `UpdateMode::Failing`
    /// - anything else (including unset) → `UpdateMode::Off`
    pub fn from_env() -> Self {
        match std::env::var("FULGUR_VRT_UPDATE").ok().as_deref() {
            Some("1") | Some("all") => UpdateMode::All,
            Some("failing") => UpdateMode::Failing,
            _ => UpdateMode::Off,
        }
    }
}

/// Filesystem layout for a VRT run.
#[derive(Debug, Clone)]
pub struct RunnerContext {
    pub crate_root: PathBuf,
    pub fixtures_dir: PathBuf,
    pub goldens_dir: PathBuf,
    pub diff_out_dir: PathBuf,
    pub update_mode: UpdateMode,
}

impl RunnerContext {
    /// Build a context using `CARGO_MANIFEST_DIR` as the crate root and
    /// `$CARGO_MANIFEST_DIR/../../target/vrt-diff` as the diff artifact dir.
    pub fn discover() -> anyhow::Result<Self> {
        let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let fixtures_dir = crate_root.join("fixtures");
        let goldens_dir = crate_root.join("goldens");
        let diff_out_dir = crate_root
            .parent()
            .and_then(|p| p.parent())
            .map(|ws| ws.join("target").join("vrt-diff"))
            .unwrap_or_else(|| crate_root.join("target").join("vrt-diff"));

        Ok(Self {
            crate_root,
            fixtures_dir,
            goldens_dir,
            diff_out_dir,
            update_mode: UpdateMode::from_env(),
        })
    }

    fn manifest_path(&self) -> PathBuf {
        self.crate_root.join("manifest.toml")
    }

    fn fulgur_golden(&self, fixture_path: &Path) -> PathBuf {
        let rel = fixture_path.with_extension("pdf");
        self.goldens_dir.join("fulgur").join(rel)
    }

    fn diff_path(&self, fixture_path: &Path) -> PathBuf {
        let rel = fixture_path.with_extension("diff.png");
        self.diff_out_dir.join(rel)
    }

    fn actual_path(&self, fixture_path: &Path) -> PathBuf {
        let rel = fixture_path.with_extension("actual.pdf");
        self.diff_out_dir.join(rel)
    }
}

/// Details of a fixture whose fulgur golden comparison failed.
#[derive(Debug, Clone)]
pub struct FailedFixture {
    pub path: PathBuf,
    pub reference_size: u64,
    pub actual_size: u64,
    pub diff_png: PathBuf,
}

/// Summary of a VRT run.
#[derive(Debug, Default, Clone)]
pub struct RunResult {
    pub total: usize,
    pub passed: usize,
    pub failed: Vec<FailedFixture>,
    pub updated: Vec<PathBuf>,
}

/// Execute the VRT pipeline for every fixture declared in the manifest.
///
/// Main path is PDF byte-wise comparison; rasterization runs only on failure
/// to produce a diff PNG for human inspection.
///
/// In `UpdateMode::Off`:
/// - missing golden → error out (user must run `FULGUR_VRT_UPDATE=1`)
/// - mismatch → write diff.png + actual.pdf to `diff_out_dir` and record the failure
/// - match → increment `result.passed`
///
/// In `UpdateMode::All`, every golden is rewritten from the current render.
/// In `UpdateMode::Failing`, missing goldens are seeded and mismatched ones rewritten.
pub fn run(ctx: &RunnerContext) -> anyhow::Result<RunResult> {
    let manifest = Manifest::load(&ctx.manifest_path())?;

    let mut result = RunResult {
        total: manifest.fixtures.len(),
        ..Default::default()
    };

    // Deterministic order for diagnostics.
    let mut fixtures = manifest.fixtures.clone();
    fixtures.sort_by(|a, b| a.path.cmp(&b.path));

    let work_root = tempfile::tempdir()?;

    for (idx, fx) in fixtures.iter().enumerate() {
        let html_path = ctx.fixtures_dir.join(&fx.path);
        let html = std::fs::read_to_string(&html_path)
            .map_err(|e| anyhow::anyhow!("failed to read fixture {}: {e}", fx.path.display()))?;

        let actual_pdf = pdf_render::render_html_to_pdf(
            &html,
            RenderSpec {
                page_size: &fx.page_size,
                margin_pt: 0.0,
                dpi: fx.dpi,
            },
        )?;

        let golden_path = ctx.fulgur_golden(&fx.path);

        match ctx.update_mode {
            UpdateMode::All => {
                save_pdf(&golden_path, &actual_pdf)?;
                result.updated.push(golden_path);
                continue;
            }
            UpdateMode::Off | UpdateMode::Failing => {
                if !golden_path.exists() {
                    if matches!(ctx.update_mode, UpdateMode::Failing) {
                        save_pdf(&golden_path, &actual_pdf)?;
                        result.updated.push(golden_path);
                        continue;
                    } else {
                        anyhow::bail!(
                            "missing fulgur golden for {} (run `FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt` to seed it)",
                            fx.path.display()
                        );
                    }
                }

                let reference_pdf = std::fs::read(&golden_path)?;

                if diff::pdf_bytes_equal(&reference_pdf, &actual_pdf) {
                    result.passed += 1;
                } else if matches!(ctx.update_mode, UpdateMode::Failing) {
                    save_pdf(&golden_path, &actual_pdf)?;
                    result.updated.push(golden_path);
                } else {
                    let work_dir = work_root.path().join(format!("fx-{idx}"));
                    let diff_png = ctx.diff_path(&fx.path);
                    let actual_pdf_artifact = ctx.actual_path(&fx.path);

                    write_pdf_diff_artifacts(
                        &reference_pdf,
                        &actual_pdf,
                        fx.dpi,
                        &work_dir,
                        &diff_png,
                        &actual_pdf_artifact,
                    )?;

                    result.failed.push(FailedFixture {
                        path: fx.path.clone(),
                        reference_size: reference_pdf.len() as u64,
                        actual_size: actual_pdf.len() as u64,
                        diff_png,
                    });
                }
            }
        }
    }

    Ok(result)
}

fn save_pdf(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

/// On byte-mismatch, rasterize both PDFs and save diagnostic artifacts:
/// - `diff_png`: pixel diff between reference and actual renders (tolerance 0)
/// - `actual_pdf_path`: the actual PDF (mirrors PR #195's CI-golden recovery flow)
fn write_pdf_diff_artifacts(
    reference_pdf: &[u8],
    actual_pdf: &[u8],
    dpi: u32,
    work_dir: &Path,
    diff_png: &Path,
    actual_pdf_path: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(work_dir)?;
    let ref_pdf_path = work_dir.join("reference.pdf");
    let act_pdf_path = work_dir.join("actual.pdf");
    std::fs::write(&ref_pdf_path, reference_pdf)?;
    std::fs::write(&act_pdf_path, actual_pdf)?;

    let ref_img = pdf_render::pdf_to_rgba(&ref_pdf_path, dpi, &work_dir.join("ref"))?;
    let act_img = pdf_render::pdf_to_rgba(&act_pdf_path, dpi, &work_dir.join("act"))?;

    let tol = crate::manifest::Tolerance {
        max_channel_diff: 0,
        max_diff_pixels_ratio: 0.0,
    };
    diff::write_diff_image(&ref_img, &act_img, tol, diff_png)?;

    if let Some(parent) = actual_pdf_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(actual_pdf_path, actual_pdf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_mode_parses_env_strings() {
        fn parse(s: Option<&str>) -> UpdateMode {
            match s {
                Some("1") | Some("all") => UpdateMode::All,
                Some("failing") => UpdateMode::Failing,
                _ => UpdateMode::Off,
            }
        }
        assert_eq!(parse(Some("1")), UpdateMode::All);
        assert_eq!(parse(Some("all")), UpdateMode::All);
        assert_eq!(parse(Some("failing")), UpdateMode::Failing);
        assert_eq!(parse(Some("0")), UpdateMode::Off);
        assert_eq!(parse(Some("")), UpdateMode::Off);
        assert_eq!(parse(None), UpdateMode::Off);
    }

    #[test]
    fn discover_resolves_crate_root() {
        let ctx = RunnerContext::discover().expect("discover");
        assert!(ctx.crate_root.ends_with("fulgur-vrt"));
        assert_eq!(ctx.fixtures_dir, ctx.crate_root.join("fixtures"));
        assert_eq!(ctx.goldens_dir, ctx.crate_root.join("goldens"));
        assert!(
            ctx.diff_out_dir.ends_with("target/vrt-diff"),
            "diff_out_dir was {:?}",
            ctx.diff_out_dir
        );
    }
}
