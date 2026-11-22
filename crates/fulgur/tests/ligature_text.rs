//! A ligature is one glyph for several letters (`ffi` in `office`). The PDF
//! must map that glyph to all of its letters, or text extraction, search,
//! copy and screen readers lose them: `office` would read as `ofce`.

use fulgur::{AssetBundle, Engine};
use std::path::PathBuf;

/// Noto Sans has the common `fi`, `fl`, `ffi` and `ffl` ligatures, which
/// are on by default.
fn render(html: &str, tagged: bool) -> Vec<u8> {
    let font_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/.fonts/NotoSans-Regular.ttf");
    let mut assets = AssetBundle::default();
    assets
        .add_font_file(&font_path)
        .unwrap_or_else(|e| panic!("failed to load Noto Sans from {}: {e}", font_path.display()));
    assets.add_css("body { font-family: 'Noto Sans'; }");
    Engine::builder()
        .assets(assets)
        .tagged(tagged)
        .lang("en")
        .build()
        .render(html)
        .expect("render")
}

/// The page text as pdftotext reads it through the fonts' ToUnicode maps,
/// or `None` when pdftotext is not installed.
fn extract_text(pdf: &[u8]) -> Option<String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ligatures.pdf");
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

const WORDS: &str = "office affluent fluff Workflow";

fn assert_words_survive(html: &str) {
    for tagged in [false, true] {
        let Some(text) = extract_text(&render(html, tagged)) else {
            eprintln!("pdftotext not installed; skipping");
            return;
        };
        for word in WORDS.split(' ') {
            assert!(
                text.contains(word),
                "{word:?} is missing (tagged: {tagged}): {text:?}"
            );
        }
    }
}

#[test]
fn ligature_glyphs_keep_all_their_letters() {
    assert_words_survive(&format!("<p>{WORDS}</p>"));
}

#[test]
fn ligature_glyphs_keep_their_letters_in_columns() {
    assert_words_survive(&format!(
        "<div style=\"columns:2\"><p>{WORDS}</p><p>{WORDS}</p></div>"
    ));
}

#[test]
fn ligature_glyphs_keep_their_letters_in_a_paragraph_split_across_columns() {
    // A paragraph that breaks across columns is shaped again per column
    // slice (`convert_multicol_paragraph_slices`).
    let long = [WORDS; 12].join(" ");
    assert_words_survive(&format!(
        "<div style=\"columns:3; width:500px\"><p>{long}</p></div>"
    ));
}

#[test]
fn ligature_glyphs_keep_their_letters_in_a_list_marker() {
    // An outside marker hangs left of the item: indent enough to keep it
    // on the page.
    assert_words_survive(&format!(
        "<ul style=\"padding-left: 20em; list-style-type: '{WORDS} '\"><li>item</li></ul>"
    ));
}

#[test]
fn turning_ligatures_off_still_gives_the_same_text() {
    assert_words_survive(&format!(
        "<p style=\"font-variant-ligatures: none\">{WORDS}</p>"
    ));
}
