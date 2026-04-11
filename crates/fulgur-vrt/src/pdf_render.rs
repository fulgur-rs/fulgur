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
    pub margin_pt: f32,
    pub dpi: u32,
}

fn page_size_from_name(name: &str) -> anyhow::Result<PageSize> {
    match name.to_ascii_uppercase().as_str() {
        "A4" => Ok(PageSize::A4),
        "A3" => Ok(PageSize::A3),
        "LETTER" => Ok(PageSize::LETTER),
        other => anyhow::bail!("unsupported page_size: {other}"),
    }
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

    let engine = Engine::builder()
        .page_size(page_size_from_name(spec.page_size)?)
        .margin(Margin::uniform(spec.margin_pt))
        .build();

    let pdf_bytes = engine
        .render_html(html)
        .map_err(|e| anyhow::anyhow!("fulgur render_html failed: {e}"))?;

    let pdf_path = work_dir.join("fixture.pdf");
    std::fs::write(&pdf_path, &pdf_bytes)?;

    let prefix = work_dir.join("page");
    let status = Command::new("pdftocairo")
        .args(["-png", "-r", &spec.dpi.to_string(), "-f", "1", "-l", "1"])
        .arg(&pdf_path)
        .arg(&prefix)
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn pdftocairo: {e}"))?;
    anyhow::ensure!(status.success(), "pdftocairo exited with {status}");

    // pdftocairo names single-page outputs as `<prefix>-1.png`
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
                margin_pt: 0.0,
                dpi: 150,
            },
            tmp.path(),
        )
        .expect("render should succeed");
        assert!(img.width() > 100);
        assert!(img.height() > 100);
    }
}
