//! Orchestrates VRT: walks the manifest, renders each fixture through fulgur,
//! compares against committed fulgur goldens, and — depending on
//! `FULGUR_VRT_UPDATE` — either writes diff images on failure or updates
//! goldens in place.

use crate::diff::{self, DiffReport};
use crate::manifest::Manifest;
use crate::pdf_render::{self, RenderSpec};
use image::RgbaImage;
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
        let rel = fixture_path.with_extension("png");
        self.goldens_dir.join("fulgur").join(rel)
    }

    fn diff_path(&self, fixture_path: &Path) -> PathBuf {
        let rel = fixture_path.with_extension("diff.png");
        self.diff_out_dir.join(rel)
    }

    fn actual_path(&self, fixture_path: &Path) -> PathBuf {
        let rel = fixture_path.with_extension("actual.png");
        self.diff_out_dir.join(rel)
    }
}

/// Details of a fixture whose fulgur golden comparison failed.
#[derive(Debug, Clone)]
pub struct FailedFixture {
    pub path: PathBuf,
    pub report: DiffReport,
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
/// In `UpdateMode::Off`:
/// - missing golden → error out (the user must run with `FULGUR_VRT_UPDATE=1`
///   to seed the golden)
/// - mismatch → write a diff image to `diff_out_dir` and record the fixture
///   in `result.failed`
/// - match → increment `result.passed`
///
/// In `UpdateMode::All`, every fixture's golden is rewritten from the current
/// fulgur output and comparison is skipped.
///
/// In `UpdateMode::Failing`, missing goldens are seeded and mismatched
/// goldens are rewritten; passing fixtures are left alone.
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

        let work_dir = work_root.path().join(format!("fx-{idx}"));
        let actual = pdf_render::render_html_to_rgba(
            &html,
            RenderSpec {
                page_size: &fx.page_size,
                margin_pt: 0.0,
                dpi: fx.dpi,
            },
            &work_dir,
        )?;

        let golden_path = ctx.fulgur_golden(&fx.path);

        match ctx.update_mode {
            UpdateMode::All => {
                save_golden(&golden_path, &actual)?;
                result.updated.push(golden_path);
                continue;
            }
            UpdateMode::Off | UpdateMode::Failing => {
                if !golden_path.exists() {
                    if matches!(ctx.update_mode, UpdateMode::Failing) {
                        save_golden(&golden_path, &actual)?;
                        result.updated.push(golden_path);
                        continue;
                    } else {
                        anyhow::bail!(
                            "missing fulgur golden for {} (run `FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt` to seed it)",
                            fx.path.display()
                        );
                    }
                }
                let reference = diff::load_png(&golden_path)?;
                let report = diff::compare(&reference, &actual, fx.tolerance_fulgur);

                if report.pass {
                    result.passed += 1;
                } else if matches!(ctx.update_mode, UpdateMode::Failing) {
                    save_golden(&golden_path, &actual)?;
                    result.updated.push(golden_path);
                } else {
                    let diff_png = ctx.diff_path(&fx.path);
                    diff::write_diff_image(&reference, &actual, fx.tolerance_fulgur, &diff_png)?;
                    // Also persist the actual render next to the diff so a
                    // CI-generated rendering can be copied verbatim into
                    // goldens/ when local and CI freetype/cairo versions
                    // diverge.
                    save_golden(&ctx.actual_path(&fx.path), &actual)?;
                    result.failed.push(FailedFixture {
                        path: fx.path.clone(),
                        report,
                        diff_png,
                    });
                }
            }
        }
    }

    Ok(result)
}

fn save_golden(path: &Path, img: &RgbaImage) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    img.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_mode_parses_env_strings() {
        // Save and restore env var to avoid leaking between tests.
        // We can't use std::env::set_var safely from multiple tests in parallel,
        // so we only test the parse via a thin wrapper below.
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
