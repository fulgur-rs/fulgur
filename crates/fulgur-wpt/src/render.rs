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

/// List `<stem>-<n>.png` files directly inside `work_dir`, sorted.
///
/// Propagates `ReadDir` entry errors instead of silently dropping them: a
/// dropped entry would under-count pages, and both callers rely on an
/// accurate count — `remove_stale_pngs` to avoid leaving a stale file behind
/// uncounted, `render_test` to avoid reporting success on a truncated page
/// set.
///
/// Shared between the two callers so the naming scheme lives in exactly one
/// place. Each caller still derives its own `stem` from `prefix.file_name()`
/// with its own error policy — see call sites.
fn list_page_pngs(work_dir: &Path, stem: &str) -> Result<Vec<PathBuf>> {
    let needle = format!("{stem}-");
    let mut matches = Vec::new();
    for entry in
        std::fs::read_dir(work_dir).with_context(|| format!("read dir {}", work_dir.display()))?
    {
        let entry = entry.with_context(|| format!("read entry in {}", work_dir.display()))?;
        let p = entry.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with(&needle) && name.ends_with(".png") {
            matches.push(p);
        }
    }
    matches.sort();
    Ok(matches)
}

/// Delete `<prefix>-*.png` files left in `work_dir` by a previous run.
///
/// Propagates cleanup failures — a leftover PNG from a prior run would mix
/// into the current page count and skew diff results, so we must fail loud
/// rather than silently continue with stale data. The one tolerated failure
/// is `NotFound`: the directory listing is a snapshot, so an entry can
/// legitimately disappear between listing and removal.
///
/// Scoping (which files are stale, via [`list_page_pngs`]) and acting
/// (deleting them) are separate passes: the scope check can never run
/// interleaved with, or after, removal side effects.
fn remove_stale_pngs(work_dir: &Path, prefix: &Path) -> Result<()> {
    let stem = prefix
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    for p in list_page_pngs(work_dir, &stem)? {
        if let Err(e) = std::fs::remove_file(&p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e).with_context(|| format!("remove stale PNG {}", p.display()));
        }
    }
    Ok(())
}

/// Render `test_html_path` and return one RgbaImage per page.
///
/// The path is canonicalized, and its parent directory is used as
/// fulgur's `base_path` for resolving CSS/asset links. `work_dir`
/// receives the PDF and per-page PNGs (left behind for debugging).
/// `dpi` controls pdftocairo's rasterization resolution.
/// `assets`: optional bundle of fonts/images injected into the engine
/// (cloned internally; `AssetBundle` stores shared `Arc`s so clones are cheap).
pub fn render_test(
    test_html_path: &Path,
    work_dir: &Path,
    dpi: u32,
    assets: Option<&fulgur::asset::AssetBundle>,
) -> Result<RenderedTest> {
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

    let mut builder = Engine::builder().base_path(base);
    if let Some(b) = assets {
        builder = builder.assets(b.clone());
    }
    let engine = builder.build();
    let pdf_bytes = engine
        .render(&html)
        .map_err(|e| anyhow::anyhow!("fulgur render failed for {}: {e}", abs.display()))?;

    // Remove stale page PNGs from prior runs so page count is accurate.
    let prefix = work_dir.join("page");
    remove_stale_pngs(work_dir, &prefix)?;

    let pdf_path = work_dir.join("fixture.pdf");
    std::fs::write(&pdf_path, &pdf_bytes)
        .with_context(|| format!("write PDF to {}", pdf_path.display()))?;
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
    let entries = list_page_pngs(work_dir, &stem)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn removes_only_matching_stale_pngs() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        for name in ["page-1.png", "page-10.png"] {
            std::fs::write(dir.join(name), b"stale").unwrap();
        }
        // Non-matching: wrong stem, wrong extension, and the exact prefix
        // without the `-` separator (`page.png` is not `page-<n>.png`).
        for name in ["other-1.png", "page-1.txt", "page.png"] {
            std::fs::write(dir.join(name), b"keep").unwrap();
        }

        remove_stale_pngs(dir, &dir.join("page")).unwrap();

        assert!(!dir.join("page-1.png").exists());
        assert!(!dir.join("page-10.png").exists());
        assert!(dir.join("other-1.png").exists());
        assert!(dir.join("page-1.txt").exists());
        assert!(dir.join("page.png").exists());
    }

    #[test]
    fn propagates_removal_failure() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path();
        // A *directory* named like a stale page PNG: `remove_file` fails with
        // something other than `NotFound`, which must not be swallowed.
        std::fs::create_dir(dir.join("page-1.png")).unwrap();

        let err = remove_stale_pngs(dir, &dir.join("page")).unwrap_err();
        assert!(
            format!("{err:#}").contains("remove stale PNG"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn propagates_read_dir_failure() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");

        let err = remove_stale_pngs(&missing, &missing.join("page")).unwrap_err();
        assert!(
            format!("{err:#}").contains("read dir"),
            "unexpected error: {err:#}"
        );
    }
}
