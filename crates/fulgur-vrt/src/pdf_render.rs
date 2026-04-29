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
    match name.to_ascii_uppercase().as_str() {
        "A4" => Ok(PageSize::A4),
        "A3" => Ok(PageSize::A3),
        "LETTER" => Ok(PageSize::LETTER),
        other => anyhow::bail!("unsupported page_size: {other}"),
    }
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
        .render_html(html)
        .map_err(|e| anyhow::anyhow!("fulgur render_html failed: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;

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
