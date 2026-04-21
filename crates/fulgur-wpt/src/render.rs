//! Render a WPT test HTML through fulgur and rasterize every page via
//! pdftocairo. CRITICAL: must not pass `-f 1 -l 1` to pdftocairo — we
//! need every page to catch multi-page regressions (advisor P1-1).

use anyhow::{Context, Result, bail};
use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct RenderedTest {
    pub pages: Vec<RgbaImage>,
    pub pdf_path: PathBuf,
}

/// Render `test_html_path` and return one RgbaImage per page.
///
/// The path is canonicalized, and its parent directory is used as
/// fulgur's `base_path` for resolving CSS/asset links. `work_dir`
/// receives the PDF and per-page PNGs (left behind for debugging).
/// `dpi` controls pdftocairo's rasterization resolution.
pub fn render_test(test_html_path: &Path, work_dir: &Path, dpi: u32) -> Result<RenderedTest> {
    use fulgur::engine::Engine;

    std::fs::create_dir_all(work_dir)
        .with_context(|| format!("create work dir {}", work_dir.display()))?;
    let abs = test_html_path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", test_html_path.display()))?;
    let html = std::fs::read_to_string(&abs).with_context(|| format!("read {}", abs.display()))?;
    let base = abs
        .parent()
        .ok_or_else(|| anyhow::anyhow!("test has no parent dir: {}", abs.display()))?;

    let engine = Engine::builder().base_path(base).build();
    let pdf_bytes = engine
        .render_html(&html)
        .map_err(|e| anyhow::anyhow!("fulgur render_html failed for {}: {e}", abs.display()))?;

    let pdf_path = work_dir.join("fixture.pdf");
    std::fs::write(&pdf_path, &pdf_bytes)
        .with_context(|| format!("write PDF to {}", pdf_path.display()))?;

    let prefix = work_dir.join("page");
    // NOTE: intentionally NOT passing -f/-l so pdftocairo emits every page.
    let out = Command::new("pdftocairo")
        .args(["-png", "-r", &dpi.to_string()])
        .arg(&pdf_path)
        .arg(&prefix)
        .output()
        .context("spawn pdftocairo")?;
    if !out.status.success() {
        bail!(
            "pdftocairo exited with {} for {}\nstderr: {}",
            out.status,
            pdf_path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    // Enumerate generated files: pdftocairo names them `<prefix>-<n>.png`.
    // For 10+ pages the index is zero-padded to the width of the max; lexical
    // sort therefore works for both single-digit and padded forms.
    let stem = prefix
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("bad prefix"))?
        .to_string_lossy()
        .into_owned();
    let needle = format!("{stem}-");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(work_dir)
        .with_context(|| format!("read dir {}", work_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            name.starts_with(&needle) && name.ends_with(".png")
        })
        .collect();
    entries.sort();

    if entries.is_empty() {
        bail!("pdftocairo produced no PNGs in {}", work_dir.display());
    }

    let pages = entries
        .iter()
        .map(|p| {
            image::open(p)
                .map(|i| i.to_rgba8())
                .with_context(|| format!("decode PNG {}", p.display()))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(RenderedTest { pages, pdf_path })
}
