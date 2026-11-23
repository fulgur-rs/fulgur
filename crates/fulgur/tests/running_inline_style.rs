//! A running element (`position: running(name)`) is drawn only in the margin
//! box that pulls it with `element(name)`, never in the body, whichever way
//! the stylesheet arrives: the AssetBundle, or the page's own `<style>`.

use fulgur::{AssetBundle, Engine};
use std::path::PathBuf;

const RUNNING_CSS: &str =
    "@page { @top-right { content: element(head) } } .head { position: running(head) }";

fn render(html: &str, extra_css: Option<&str>) -> Vec<u8> {
    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans from {}: {e}", font_path.display()));
    assets.add_css("body { font-family: 'Noto Sans'; }");
    if let Some(css) = extra_css {
        assets.add_css(css);
    }
    Engine::builder()
        .assets(assets)
        .build()
        .render(html)
        .expect("render")
}

/// The page text as pdftotext reads it, or `None` when it is not installed.
fn extract_text(pdf: &[u8]) -> Option<String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("running.pdf");
    std::fs::write(&path, pdf).expect("write pdf");
    let output = match std::process::Command::new("pdftotext")
        .arg("-raw")
        .arg(&path)
        .arg("-")
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => panic!("pdftotext: {err}"),
    };
    assert!(output.status.success(), "pdftotext failed: {output:?}");
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn assert_drawn_once(pdf: &[u8]) {
    let Some(text) = extract_text(pdf) else {
        eprintln!("pdftotext not installed; skipping");
        return;
    };
    assert_eq!(text.matches("RUNHEAD").count(), 1, "{text:?}");
    assert!(text.contains("Body text"), "{text:?}");
}

const BODY: &str = r#"<div class="head">RUNHEAD</div><p>Body text</p>"#;

#[test]
fn running_element_from_asset_css_is_drawn_once() {
    let html = format!("<!DOCTYPE html><html><body>{BODY}</body></html>");
    assert_drawn_once(&render(&html, Some(RUNNING_CSS)));
}

#[test]
fn running_element_from_inline_style_is_drawn_once() {
    let html = format!(
        "<!DOCTYPE html><html><head><style>{RUNNING_CSS}</style></head><body>{BODY}</body></html>"
    );
    assert_drawn_once(&render(&html, None));
}
