//! End-to-end regression: confirm that a bundled Ahem.ttf changes the
//! rendering output for an HTML that declares `@font-face family:"Ahem"`.
//! Skipped when `target/wpt/fonts/Ahem.ttf` is not fetched.

use fulgur::engine::Engine;
use std::path::PathBuf;

const HTML: &str = r#"<!DOCTYPE html>
<html><head><style>
@font-face { font-family: "Ahem"; src: url("/fonts/Ahem.ttf"); }
p { font-size: 40px; font-family: "Ahem"; }
</style></head>
<body><p>XXXX</p></body></html>
"#;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn ahem_bundle_differs_from_no_bundle() {
    let ahem = workspace_root().join("target/wpt/fonts/Ahem.ttf");
    if !ahem.exists() {
        eprintln!(
            "skip: {} missing (run scripts/wpt/fetch.sh)",
            ahem.display()
        );
        return;
    }
    let bundle = fulgur_wpt::fonts::load_fonts_dir(&workspace_root().join("target/wpt/fonts"))
        .expect("load fonts");
    assert!(
        !bundle.fonts.is_empty(),
        "fonts dir must contain at least one font"
    );

    let pdf_none = Engine::builder()
        .build()
        .render_html(HTML)
        .expect("render without bundle");
    let pdf_with = Engine::builder()
        .assets(bundle)
        .build()
        .render_html(HTML)
        .expect("render with bundle");

    assert_ne!(
        pdf_none, pdf_with,
        "PDFs identical — bundled Ahem is NOT being resolved for \
         `font-family:\"Ahem\"`. Re-scope: @font-face URL rewrite required."
    );
}
