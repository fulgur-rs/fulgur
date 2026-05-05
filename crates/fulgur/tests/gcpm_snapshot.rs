//! Snapshot tests for inline `<style>` GCPM constructs (fulgur-gxv).
//!
//! Each test renders HTML with GCPM rules in an inline `<style>` block and
//! compares the uncompressed PDF structure against a golden `.txt` file in
//! `tests/snapshots/`. Set `FULGUR_SNAPSHOT_UPDATE=1` to regenerate goldens.
//!
//! Noto Sans is injected via AssetBundle so font data is identical on all
//! platforms and the golden files remain deterministic across Linux / macOS /
//! Windows CI runners.

use fulgur::asset::AssetBundle;
use fulgur::Engine;
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
    assets.add_css("body, div, p, h1, aside { font-family: 'Noto Sans', sans-serif; }");
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
