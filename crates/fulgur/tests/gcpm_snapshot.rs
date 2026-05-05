//! Snapshot tests for GCPM constructs (fulgur-gxv, fulgur-fpq9).
//!
//! Each test renders HTML with GCPM rules and compares the uncompressed PDF
//! structure against a golden `.txt` file in `tests/snapshots/`.
//! Set `FULGUR_SNAPSHOT_UPDATE=1` to regenerate goldens.
//!
//! Noto Sans is injected via AssetBundle so font data is identical on all
//! platforms and the golden files remain deterministic across Linux / macOS /
//! Windows CI runners.

use fulgur::Engine;
use fulgur::asset::AssetBundle;
use krilla::SerializeSettings;
use std::path::PathBuf;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn snapshot_settings() -> SerializeSettings {
    SerializeSettings {
        compress_content_streams: false,
        ascii_compatible: true,
        ..Default::default()
    }
}

fn noto_assets() -> AssetBundle {
    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans: {e}"));
    assets.add_css("body, div, p, h1, h2, aside, header { font-family: 'Noto Sans', sans-serif; }");
    assets
}

fn check_snapshot(name: &str, actual: &[u8]) {
    let path = snapshot_dir().join(format!("{name}.txt"));
    if !path.exists() || std::env::var("FULGUR_SNAPSHOT_UPDATE").is_ok() {
        std::fs::write(&path, actual).unwrap();
        if std::env::var("FULGUR_SNAPSHOT_UPDATE").is_ok() {
            return;
        }
        panic!("new snapshot created at {}", path.display());
    }
    let expected = std::fs::read(&path).unwrap();
    if actual != expected.as_slice() {
        let actual_str = String::from_utf8_lossy(actual);
        let expected_str = String::from_utf8_lossy(&expected);
        assert_eq!(
            actual_str, expected_str,
            "snapshot mismatch for '{name}' — run with FULGUR_SNAPSHOT_UPDATE=1 to regenerate"
        );
    }
}

#[test]
fn gcpm_counter_via_inline_style_snapshot() {
    let html = r#"<!doctype html><html><head>
        <style>
            body { counter-reset: pg; }
            @page { @bottom-center { content: "Page " counter(pg); } }
        </style>
    </head><body>
        <p>Hello inline counter.</p>
    </body></html>"#;

    let pdf = Engine::builder()
        .assets(noto_assets())
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_counter_via_inline_style", &pdf);
}

#[test]
fn gcpm_running_element_via_inline_style_snapshot() {
    let html = r#"<!doctype html><html><head>
        <style>
            .hdr { position: running(pageHeader); }
            @page { @top-center { content: element(pageHeader); } }
        </style>
    </head><body>
        <div class="hdr">Running Header</div>
        <p>Body text.</p>
    </body></html>"#;

    let pdf = Engine::builder()
        .assets(noto_assets())
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_running_element_via_inline_style", &pdf);
}

#[test]
fn gcpm_string_set_via_inline_style_snapshot() {
    let html = r#"<!doctype html><html><head>
        <style>
            h1 { string-set: chap content(text); }
            @page { @top-center { content: string(chap); } }
        </style>
    </head><body>
        <h1>Chapter One</h1>
        <p>Some content here.</p>
    </body></html>"#;

    let pdf = Engine::builder()
        .assets(noto_assets())
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_string_set_via_inline_style", &pdf);
}

// ── Migrated from gcpm_integration.rs (fulgur-fpq9) ─────────────────────────

#[test]
fn gcpm_header_footer_snapshot() {
    let css = r#"
        .header { position: running(pageHeader); }
        .footer { position: running(pageFooter); }
        @page {
            @top-center { content: element(pageHeader); }
            @bottom-center { content: element(pageFooter) " - " counter(page) " / " counter(pages); }
        }
    "#;
    let html = r#"<!DOCTYPE html>
<html><head></head><body>
  <div class="header">Document Header</div>
  <div class="footer">Document Footer</div>
  <p>Body content for the document.</p>
  <p>Second paragraph of content.</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_header_footer", &pdf);
}

#[test]
fn gcpm_multipage_counter_snapshot() {
    let css = r#"
        @page {
            @bottom-center { content: "Page " counter(page) " of " counter(pages); }
        }
    "#;

    let mut paragraphs = String::new();
    for i in 0..100 {
        paragraphs.push_str(&format!(
            "<p>Paragraph {} with enough text to take up space on the page.</p>\n",
            i + 1
        ));
    }
    let html = format!(
        "<!DOCTYPE html><html><head></head><body>{}</body></html>",
        paragraphs
    );

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(&html)
        .expect("render");

    check_snapshot("gcpm_multipage_counter", &pdf);
}

#[test]
fn gcpm_counter_only_no_running_snapshot() {
    let css = r#"
        @page {
            @bottom-center { content: "Page " counter(page); }
        }
    "#;
    let html = r#"<!DOCTYPE html>
<html><head></head><body>
  <p>Simple body text with page counter only, no running elements.</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_counter_only_no_running", &pdf);
}

#[test]
fn gcpm_id_selector_running_element_snapshot() {
    let css = r#"
        #doc-title { position: running(pageTitle); }
        @page { @top-center { content: element(pageTitle); } }
    "#;
    let html = r#"<!DOCTYPE html>
<html><body>
  <div id="doc-title">My Document</div>
  <p>Body content</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_id_selector_running_element", &pdf);
}

#[test]
fn gcpm_tag_selector_running_element_snapshot() {
    let css = r#"
        header { position: running(pageHeader); }
        @page { @top-center { content: element(pageHeader); } }
    "#;
    let html = r#"<!DOCTYPE html>
<html><body>
  <header>Document Header</header>
  <p>Body content</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_tag_selector_running_element", &pdf);
}

#[test]
fn gcpm_left_right_margin_boxes_snapshot() {
    let css = r#"
        @page {
            margin: 72pt;
            @left-middle { content: "Left Side"; font-size: 8px; }
            @right-middle { content: "Page " counter(page); font-size: 8px; }
        }
    "#;
    let html = r#"<!DOCTYPE html>
<html><head></head><body>
  <p>Body content with left and right margin boxes.</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_left_right_margin_boxes", &pdf);
}

#[test]
fn gcpm_all_side_margin_boxes_snapshot() {
    let css = r#"
        @page {
            margin: 72pt;
            @left-top { content: "LT"; }
            @left-middle { content: "LM"; }
            @left-bottom { content: "LB"; }
            @right-top { content: "RT"; }
            @right-middle { content: "RM"; }
            @right-bottom { content: "RB"; }
            @top-center { content: "Page " counter(page); }
            @bottom-center { content: "Footer"; }
        }
    "#;
    let html = r#"<!DOCTYPE html>
<html><head></head><body>
  <p>Body content with all margin box positions.</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_all_side_margin_boxes", &pdf);
}

#[test]
fn gcpm_left_right_with_running_element_snapshot() {
    let css = r#"
        .sidebar-label { position: running(sideLabel); }
        @page {
            margin: 72pt;
            @left-top { content: element(sideLabel); }
            @right-bottom { content: "Page " counter(page) " / " counter(pages); font-size: 8px; }
        }
    "#;
    let html = r#"<!DOCTYPE html>
<html><head></head><body>
  <div class="sidebar-label">Chapter 1</div>
  <p>Content of chapter 1.</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_left_right_with_running_element", &pdf);
}

/// Regression: same running element on both sides with asymmetric margins
/// exercises the height_cache width-dependent key and per-side measurement.
#[test]
fn gcpm_side_boxes_asymmetric_margins_snapshot() {
    let css = r#"
        .sidebar-label { position: running(sideLabel); }
        @page {
            margin-top: 72pt;
            margin-right: 144pt;
            margin-bottom: 72pt;
            margin-left: 36pt;
            @left-middle { content: element(sideLabel) " - " counter(page); font-size: 8px; }
            @right-middle { content: element(sideLabel) " - " counter(page); font-size: 8px; }
        }
    "#;
    let html = r#"<!DOCTYPE html>
<html><head></head><body>
  <div class="sidebar-label">A very long chapter label that should wrap differently on each side</div>
  <p>Body content with asymmetric side margins.</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_side_boxes_asymmetric_margins", &pdf);
}

#[test]
fn gcpm_string_set_chapter_title_snapshot() {
    let css = r#"
        h1 { string-set: chapter-title content(text); }
        @page {
            @top-center { content: string(chapter-title); }
            @bottom-center { content: "Page " counter(page) " of " counter(pages); }
        }
    "#;

    let mut paragraphs = String::new();
    for i in 0..3 {
        paragraphs.push_str(&format!(
            "<h1>Chapter {}</h1>\n<p>Content for chapter {}.</p>\n",
            i + 1,
            i + 1
        ));
        for j in 0..20 {
            paragraphs.push_str(&format!(
                "<p>Paragraph {} of chapter {}.</p>\n",
                j + 1,
                i + 1
            ));
        }
    }
    let html = format!(
        "<!DOCTYPE html><html><head></head><body>{}</body></html>",
        paragraphs
    );

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(&html)
        .expect("render");

    check_snapshot("gcpm_string_set_chapter_title", &pdf);
}

#[test]
fn gcpm_string_set_with_attr_snapshot() {
    let css = r#"
        h1 { string-set: title attr(data-title); }
        @page {
            @top-left { content: string(title); }
        }
    "#;
    let html = r#"<!DOCTYPE html>
<html><head></head><body>
  <h1 data-title="Custom Title">Visible Heading</h1>
  <p>Some body content.</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_string_set_with_attr", &pdf);
}

#[test]
fn gcpm_string_set_with_literal_concat_snapshot() {
    let css = r#"
        h1 { string-set: header "Section: " content(text); }
        @page {
            @top-center { content: string(header); }
        }
    "#;
    let html = r#"<!DOCTYPE html>
<html><head></head><body>
  <h1>Introduction</h1>
  <p>Body text.</p>
</body></html>"#;

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(html)
        .expect("render");

    check_snapshot("gcpm_string_set_with_literal_concat", &pdf);
}

#[test]
fn gcpm_string_set_with_policies_snapshot() {
    let css = r#"
        h2 { string-set: section content(text); }
        @page {
            @top-left { content: string(section, start); }
            @top-right { content: string(section, last); }
        }
    "#;

    let mut body = String::new();
    for i in 0..30 {
        body.push_str(&format!("<h2>Section {}</h2>\n<p>Content.</p>\n", i + 1));
    }
    let html = format!(
        "<!DOCTYPE html><html><head></head><body>{}</body></html>",
        body
    );

    let mut assets = noto_assets();
    assets.add_css(css);

    let pdf = Engine::builder()
        .assets(assets)
        .serialize_settings(snapshot_settings())
        .build()
        .render_html(&html)
        .expect("render");

    check_snapshot("gcpm_string_set_with_policies", &pdf);
}
