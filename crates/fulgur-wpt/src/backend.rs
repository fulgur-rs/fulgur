//! Layout backend selection for the WPT runner.
//!
//! The runner renders through the Blitz-based `fulgur` facade by default.
//! Setting `FULGUR_WPT_ENGINE=raikiri` switches to the unpublished Raikiri
//! backend (`fulgur-raikiri`), which is only compiled in with the `raikiri`
//! cargo feature. Each backend keeps its own expectations and report
//! directories so their baselines never mix.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// Environment variable that selects the backend.
pub const ENGINE_ENV: &str = "FULGUR_WPT_ENGINE";

/// Layout backend used to render test and reference HTML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// The `fulgur` facade (Blitz layout).
    #[default]
    Blitz,
    /// The `fulgur-raikiri` backend.
    Raikiri,
}

impl Backend {
    /// Read the backend from `FULGUR_WPT_ENGINE`. Unset or empty means Blitz.
    ///
    /// # Errors
    /// Fails for an unknown name, or for `raikiri` when this crate was built
    /// without the `raikiri` feature.
    pub fn from_env() -> Result<Self> {
        Self::from_name(std::env::var(ENGINE_ENV).ok().as_deref())
    }

    /// Parse a backend name as `from_env` does.
    ///
    /// # Errors
    /// See [`Backend::from_env`].
    pub fn from_name(name: Option<&str>) -> Result<Self> {
        let backend = match name.map(str::trim) {
            None | Some("") | Some("blitz") => Self::Blitz,
            Some("raikiri") => Self::Raikiri,
            Some(other) => {
                bail!("unknown {ENGINE_ENV} value `{other}` (expected blitz or raikiri)")
            }
        };
        if backend == Self::Raikiri && !cfg!(feature = "raikiri") {
            bail!("{ENGINE_ENV}=raikiri requires building fulgur-wpt with `--features raikiri`");
        }
        Ok(backend)
    }

    /// Short lowercase name, as accepted by `FULGUR_WPT_ENGINE`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Blitz => "blitz",
            Self::Raikiri => "raikiri",
        }
    }

    /// Product name written to `report.json`.
    pub fn product(self) -> &'static str {
        match self {
            Self::Blitz => "fulgur",
            Self::Raikiri => "fulgur-raikiri",
        }
    }

    /// Subdirectory this backend's files live in under a shared root.
    /// Blitz, the default, uses the root itself.
    pub fn subdir(self) -> Option<&'static str> {
        match self {
            Self::Blitz => None,
            Self::Raikiri => Some(self.name()),
        }
    }

    /// `base`, or its backend subdirectory (see [`Backend::subdir`]).
    pub fn scoped(self, base: &Path) -> PathBuf {
        match self.subdir() {
            None => base.to_path_buf(),
            Some(sub) => base.join(sub),
        }
    }

    /// Directory holding this backend's expectations files.
    pub fn expectations_dir(self, base: &Path) -> PathBuf {
        self.scoped(base)
    }

    /// Directory receiving per-phase reports (`report.json`, ...):
    /// `target/wpt-report`, scoped by [`Backend::scoped`].
    pub fn report_root(self, workspace_root: &Path) -> PathBuf {
        self.scoped(&workspace_root.join("target/wpt-report"))
    }

    /// Scratch directory for rendered PDFs, PNGs and diffs:
    /// `target/wpt-run`, scoped by [`Backend::scoped`].
    pub fn run_root(self, workspace_root: &Path) -> PathBuf {
        self.scoped(&workspace_root.join("target/wpt-run"))
    }

    /// Render `html` to PDF bytes. Relative URLs resolve against `base`.
    ///
    /// # Errors
    /// Returns the backend's render error.
    pub fn render_pdf(
        self,
        html: &str,
        base: &Path,
        assets: Option<&fulgur::asset::AssetBundle>,
    ) -> Result<Vec<u8>> {
        match self {
            Self::Blitz => {
                let mut builder = fulgur::engine::Engine::builder().base_path(base);
                if let Some(b) = assets {
                    builder = builder.assets(b.clone());
                }
                builder
                    .build()
                    .render(html)
                    .map_err(|e| anyhow::anyhow!("fulgur render failed: {e}"))
            }
            Self::Raikiri => render_raikiri(html, base, assets),
        }
    }
}

#[cfg(feature = "raikiri")]
fn render_raikiri(
    html: &str,
    base: &Path,
    assets: Option<&fulgur::asset::AssetBundle>,
) -> Result<Vec<u8>> {
    // Same page defaults as `Engine::builder().build()` on the Blitz side.
    let config = fulgur::config::Config::default();
    let options = fulgur_raikiri::RenderOptions {
        assets,
        system_fonts: true,
        ..Default::default()
    };
    fulgur_raikiri::render_html_with_options(html, Some(base), &config, &options)
        .map_err(|e| anyhow::anyhow!("fulgur-raikiri render failed: {e}"))
}

#[cfg(not(feature = "raikiri"))]
fn render_raikiri(
    _html: &str,
    _base: &Path,
    _assets: Option<&fulgur::asset::AssetBundle>,
) -> Result<Vec<u8>> {
    bail!("fulgur-wpt was built without the `raikiri` feature")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_and_blitz_select_blitz() {
        assert_eq!(Backend::from_name(None).unwrap(), Backend::Blitz);
        assert_eq!(Backend::from_name(Some("")).unwrap(), Backend::Blitz);
        assert_eq!(Backend::from_name(Some(" blitz ")).unwrap(), Backend::Blitz);
    }

    #[test]
    fn unknown_name_is_rejected() {
        let err = Backend::from_name(Some("servo")).unwrap_err();
        assert!(
            err.to_string().contains("unknown FULGUR_WPT_ENGINE"),
            "{err}"
        );
    }

    #[cfg(feature = "raikiri")]
    #[test]
    fn raikiri_name_selects_raikiri() {
        assert_eq!(
            Backend::from_name(Some("raikiri")).unwrap(),
            Backend::Raikiri
        );
    }

    #[cfg(not(feature = "raikiri"))]
    #[test]
    fn raikiri_name_needs_the_feature() {
        let err = Backend::from_name(Some("raikiri")).unwrap_err();
        assert!(err.to_string().contains("--features raikiri"), "{err}");
    }

    #[test]
    fn names_and_products() {
        assert_eq!(Backend::Blitz.subdir(), None);
        assert_eq!(Backend::Raikiri.subdir(), Some("raikiri"));
        assert_eq!(Backend::Blitz.name(), "blitz");
        assert_eq!(Backend::Raikiri.name(), "raikiri");
        assert_eq!(Backend::Blitz.product(), "fulgur");
        assert_eq!(Backend::Raikiri.product(), "fulgur-raikiri");
    }

    #[test]
    fn raikiri_paths_live_in_a_subdirectory() {
        let ws = Path::new("/ws");
        let exp = Path::new("/ws/exp");
        assert_eq!(Backend::Blitz.expectations_dir(exp), exp);
        assert_eq!(
            Backend::Raikiri.expectations_dir(exp),
            Path::new("/ws/exp/raikiri")
        );
        assert_eq!(
            Backend::Blitz.report_root(ws),
            Path::new("/ws/target/wpt-report")
        );
        assert_eq!(
            Backend::Raikiri.report_root(ws),
            Path::new("/ws/target/wpt-report/raikiri")
        );
        assert_eq!(Backend::Blitz.run_root(ws), Path::new("/ws/target/wpt-run"));
        assert_eq!(
            Backend::Raikiri.run_root(ws),
            Path::new("/ws/target/wpt-run/raikiri")
        );
    }

    #[test]
    fn blitz_renders_with_assets() {
        let dir = tempfile::tempdir().unwrap();
        let assets = fulgur::asset::AssetBundle::new();
        let pdf = Backend::Blitz
            .render_pdf("<p>hi</p>", dir.path(), Some(&assets))
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[cfg(feature = "raikiri")]
    #[test]
    fn raikiri_renders_a_pdf() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = Backend::Raikiri
            .render_pdf("<p>hi</p>", dir.path(), None)
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[cfg(not(feature = "raikiri"))]
    #[test]
    fn raikiri_render_needs_the_feature() {
        let dir = tempfile::tempdir().unwrap();
        let err = Backend::Raikiri
            .render_pdf("<p>hi</p>", dir.path(), None)
            .unwrap_err();
        assert!(
            err.to_string().contains("without the `raikiri` feature"),
            "{err}"
        );
    }
}
