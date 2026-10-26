//! Render HTML through fulgur and rasterize the resulting PDF to an RgbaImage
//! via `pdftocairo` (poppler-utils).

use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;
use image::RgbaImage;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Copy)]
pub struct RenderSpec<'a> {
    pub page_size: &'a str,
    pub margin_pt: Option<f32>,
    pub dpi: u32,
    pub bookmarks: bool,
}

fn page_size_from_name(name: &str) -> anyhow::Result<PageSize> {
    PageSize::from_css_keyword(name).ok_or_else(|| anyhow::anyhow!("unsupported page_size: {name}"))
}

/// Render `html` through fulgur and return the resulting PDF bytes.
///
/// `spec.dpi` is unused here (rasterization happens only in the
/// failure-diagnosis path via `render_html_to_rgba`); it is kept in
/// `RenderSpec` so the same struct can drive both APIs.
pub fn render_html_to_pdf(html: &str, spec: RenderSpec<'_>) -> anyhow::Result<Vec<u8>> {
    let mut builder = Engine::builder().page_size(page_size_from_name(spec.page_size)?);
    if let Some(mpt) = spec.margin_pt {
        builder = builder.margin(Margin::uniform(mpt));
    }
    if spec.bookmarks {
        builder = builder.bookmarks(true);
    }
    let engine = builder.build();

    engine
        .render(html)
        .map_err(|e| anyhow::anyhow!("fulgur render failed: {e}"))
}

/// Render `html` through fulgur, write the PDF into `work_dir`, rasterize
/// page 1 with `pdftocairo` at `spec.dpi`, and return the resulting image.
///
/// `work_dir` must exist OR be creatable. Intermediate files (`fixture.pdf`,
/// `page-1.png`) are written there and left behind for debugging.
pub fn render_html_to_rgba(
    html: &str,
    spec: RenderSpec<'_>,
    work_dir: &Path,
) -> anyhow::Result<RgbaImage> {
    std::fs::create_dir_all(work_dir)?;
    let pdf_bytes = render_html_to_pdf(html, spec)?;
    let pdf_path = work_dir.join("fixture.pdf");
    std::fs::write(&pdf_path, &pdf_bytes)?;
    pdf_to_rgba(&pdf_path, spec.dpi, work_dir)
}

/// Rasterize page 1 of `pdf_path` via `pdftocairo`. Used by the
/// failure-diagnosis path; the main VRT comparison no longer rasterizes.
pub fn pdf_to_rgba(pdf_path: &Path, dpi: u32, work_dir: &Path) -> anyhow::Result<RgbaImage> {
    std::fs::create_dir_all(work_dir)?;
    let prefix = work_dir.join("page");
    let status = Command::new("pdftocairo")
        .args(["-png", "-r", &dpi.to_string(), "-f", "1", "-l", "1"])
        .arg(pdf_path)
        .arg(&prefix)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn pdftocairo: {e}"))?;
    anyhow::ensure!(status.success(), "pdftocairo exited with {status}");
    let png_path = work_dir.join("page-1.png");
    let img = image::open(&png_path)?.to_rgba8();
    Ok(img)
}

/// Build the `Config` that `render_html_to_pdf` hands the Blitz engine, so
/// another backend can render a fixture with the same page setup.
pub fn render_config(spec: RenderSpec<'_>) -> anyhow::Result<fulgur::config::Config> {
    let mut builder =
        fulgur::config::Config::builder().page_size(page_size_from_name(spec.page_size)?);
    if let Some(mpt) = spec.margin_pt {
        builder = builder.margin(Margin::uniform(mpt));
    }
    if spec.bookmarks {
        builder = builder.bookmarks(true);
    }
    Ok(builder.build())
}

/// Render `html` through the Raikiri backend with the page setup of `spec`.
#[cfg(feature = "raikiri")]
pub fn render_html_to_pdf_raikiri(html: &str, spec: RenderSpec<'_>) -> anyhow::Result<Vec<u8>> {
    let config = render_config(spec)?;
    let options = fulgur_raikiri::RenderOptions::default();
    fulgur_raikiri::render_html_with_options(html, None, &config, &options)
        .map_err(|e| anyhow::anyhow!("fulgur-raikiri render failed: {e}"))
}

/// Rasterize every page of `pdf_path` via `pdftocairo`, in page order.
///
/// Unlike `pdf_to_rgba` this does not pass `-f`/`-l`, so a page-count
/// difference between two renders stays visible.
pub fn pdf_to_rgba_pages(
    pdf_path: &Path,
    dpi: u32,
    work_dir: &Path,
) -> anyhow::Result<Vec<RgbaImage>> {
    std::fs::create_dir_all(work_dir)?;
    let prefix = work_dir.join("page");
    let status = Command::new("pdftocairo")
        .args(["-png", "-r", &dpi.to_string()])
        .arg(pdf_path)
        .arg(&prefix)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn pdftocairo: {e}"))?;
    anyhow::ensure!(status.success(), "pdftocairo exited with {status}");

    // pdftocairo names pages `page-<n>.png`, zero-padding <n> to the width
    // of the last page number, so a lexical sort is page order.
    let mut pngs: Vec<_> = std::fs::read_dir(work_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            name.starts_with("page-") && name.ends_with(".png")
        })
        .collect();
    pngs.sort();
    anyhow::ensure!(
        !pngs.is_empty(),
        "pdftocairo produced no pages for {}",
        pdf_path.display()
    );
    pngs.iter()
        .map(|p| Ok(image::open(p)?.to_rgba8()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_from_name_accepts_newly_added_css_keywords() {
        // fulgur-5oav: this used to hardcode A4/A3/Letter only, drifting
        // from the shared `PageSize::from_css_keyword` table used by CSS,
        // the CLI, and the language bindings.
        assert!(page_size_from_name("A5").is_ok());
        assert!(page_size_from_name("jis-b4").is_ok());
        assert!(page_size_from_name("banana").is_err());
    }

    #[test]
    fn renders_solid_box_html_to_png() {
        let html = r#"<!DOCTYPE html>
<html><body style="margin:0">
<div style="width:100px;height:100px;background:#ff0000"></div>
</body></html>"#;

        let tmp = tempfile::tempdir().unwrap();
        let img = render_html_to_rgba(
            html,
            RenderSpec {
                page_size: "A4",
                margin_pt: Some(0.0),
                dpi: 150,
                bookmarks: false,
            },
            tmp.path(),
        )
        .expect("render should succeed");
        assert!(img.width() > 100);
        assert!(img.height() > 100);
    }

    #[test]
    fn render_config_carries_the_page_setup() {
        let config = render_config(RenderSpec {
            page_size: "A5",
            margin_pt: Some(12.0),
            dpi: 150,
            bookmarks: true,
        })
        .unwrap();
        let a5 = PageSize::from_css_keyword("A5").unwrap();
        assert_eq!(
            (config.page_size.width, config.page_size.height),
            (a5.width, a5.height)
        );
        assert_eq!(config.margin.top, 12.0);
        assert!(config.bookmarks);
        assert!(config.overrides.page_size && config.overrides.margin);

        let css_margin = render_config(RenderSpec {
            page_size: "A4",
            margin_pt: None,
            dpi: 150,
            bookmarks: false,
        })
        .unwrap();
        assert!(!css_margin.overrides.margin);
        assert!(
            render_config(RenderSpec {
                page_size: "banana",
                margin_pt: None,
                dpi: 150,
                bookmarks: false,
            })
            .is_err()
        );
    }

    #[cfg(feature = "raikiri")]
    #[test]
    fn raikiri_renders_and_every_page_is_rasterized() {
        let html = r#"<!DOCTYPE html><html><body style="margin:0">
<div style="height:100px;background:#ff0000"></div>
<div style="break-before:page;height:100px;background:#0000ff"></div>
</body></html>"#;
        let spec = RenderSpec {
            page_size: "A4",
            margin_pt: Some(0.0),
            dpi: 50,
            bookmarks: false,
        };
        let pdf = render_html_to_pdf_raikiri(html, spec).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("r.pdf");
        std::fs::write(&path, pdf).unwrap();
        let pages = pdf_to_rgba_pages(&path, 50, &tmp.path().join("png")).unwrap();
        assert_eq!(pages.len(), 2);
    }

    #[test]
    fn render_html_to_pdf_returns_pdf_bytes() {
        let html = r#"<!DOCTYPE html><html><body style="margin:0">
<div style="width:100px;height:100px;background:#ff0000"></div>
</body></html>"#;
        let bytes = render_html_to_pdf(
            html,
            RenderSpec {
                page_size: "A4",
                margin_pt: Some(0.0),
                dpi: 150,
                bookmarks: false,
            },
        )
        .expect("render should succeed");
        assert!(bytes.starts_with(b"%PDF-"), "PDF magic missing");
        assert!(
            bytes.len() > 200,
            "suspiciously small PDF: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn render_html_to_pdf_is_deterministic_within_process() {
        let html = r#"<!DOCTYPE html><html><body style="margin:0">
<div style="width:50px;height:50px;background:#00ff00"></div>
</body></html>"#;
        let spec = RenderSpec {
            page_size: "A4",
            margin_pt: Some(0.0),
            dpi: 150,
            bookmarks: false,
        };
        let a = render_html_to_pdf(html, spec).unwrap();
        let b = render_html_to_pdf(html, spec).unwrap();
        assert_eq!(a, b, "two renders of same HTML should be byte-identical");
    }

    #[test]
    fn css_at_page_margin_is_respected_when_margin_pt_is_none() {
        // @page { margin: 30mm } を指定した HTML
        let html = r#"<!DOCTYPE html>
<html><head><style>
@page { size: A4; margin: 30mm; }
</style></head>
<body><div style="width:100px;height:100px;background:#ff0000"></div></body></html>"#;
        let with_none = render_html_to_pdf(
            html,
            RenderSpec {
                page_size: "A4",
                margin_pt: None,
                dpi: 150,
                bookmarks: false,
            },
        )
        .expect("render with None should succeed");
        let with_zero = render_html_to_pdf(
            html,
            RenderSpec {
                page_size: "A4",
                margin_pt: Some(0.0),
                dpi: 150,
                bookmarks: false,
            },
        )
        .expect("render with Some(0.0) should succeed");
        // None は CSS @page margin を尊重するので、Some(0.0) (CSS を上書き) と異なる出力になる
        assert_ne!(
            with_none, with_zero,
            "None should respect CSS @page margin; Some(0.0) should override it"
        );
    }
}
