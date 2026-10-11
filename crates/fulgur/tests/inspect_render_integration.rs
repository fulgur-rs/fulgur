//! End-to-end checks for `fulgur::inspect` against PDFs rendered by `Engine`.
//!
//! These lived in the `inspect` module's unit tests before `inspect` moved to
//! `fulgur-core`, which cannot depend on a layout backend.

use fulgur::asset::AssetBundle;
use fulgur::engine::Engine;
use fulgur::inspect::{InspectResult, inspect};

fn inspect_bytes(bytes: &[u8]) -> InspectResult {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), bytes).unwrap();
    inspect(tmp.path()).unwrap()
}

fn render_test_pdf(html: &str) -> Vec<u8> {
    Engine::builder().build().render(html).unwrap()
}

#[test]
fn inspect_page_count() {
    let pdf = render_test_pdf("<html><body><p>Hello</p></body></html>");
    let result = inspect_bytes(&pdf);
    assert_eq!(result.pages, 1);
}

#[test]
fn inspect_metadata_title() {
    let pdf = Engine::builder()
        .title("Test Title".to_string())
        .build()
        .render("<html><body><p>Hi</p></body></html>")
        .unwrap();
    let result = inspect_bytes(&pdf);
    assert_eq!(result.metadata.title.as_deref(), Some("Test Title"));
}

#[test]
fn inspect_text_items_non_empty() {
    let pdf = render_test_pdf("<html><body><p>Hello World</p></body></html>");
    let result = inspect_bytes(&pdf);
    assert!(!result.text_items.is_empty(), "expected text items");
}

#[test]
fn inspect_text_item_fields() {
    let pdf = render_test_pdf("<html><body><p>Hello</p></body></html>");
    let result = inspect_bytes(&pdf);
    let item = result
        .text_items
        .first()
        .expect("text items should not be empty");
    assert!(item.page >= 1);
    assert!(item.font_size > 0.0);
    assert!(!item.text.is_empty());
}

#[test]
fn inspect_result_serializes_to_json() {
    let pdf = render_test_pdf("<html><body><p>Test</p></body></html>");
    let result = inspect_bytes(&pdf);
    let json = serde_json::to_string_pretty(&result).unwrap();
    assert!(json.contains("\"pages\""));
    assert!(json.contains("\"metadata\""));
    assert!(json.contains("\"text_items\""));
    assert!(json.contains("\"images\""));
}

#[test]
fn inspect_multi_page_pdf() {
    // Force two pages by making content taller than a single A4 page
    let html = "<html><body>\
        <p style='margin-bottom:2000pt'>Page one</p>\
        <p>Page two</p>\
        </body></html>";
    let pdf = render_test_pdf(html);
    let result = inspect_bytes(&pdf);
    assert!(result.pages >= 2, "expected at least 2 pages");
}

#[test]
fn inspect_metadata_all_fields() {
    let pdf = Engine::builder()
        .title("My Title".to_string())
        .authors(vec!["Alice".to_string()])
        .creator("TestApp".to_string())
        .build()
        .render("<html><body><p>x</p></body></html>")
        .unwrap();
    let result = inspect_bytes(&pdf);
    assert_eq!(result.metadata.title.as_deref(), Some("My Title"));
    assert_eq!(result.metadata.author.as_deref(), Some("Alice"));
    assert_eq!(result.metadata.creator.as_deref(), Some("TestApp"));
}

#[test]
fn inspect_image_embedded() {
    // Generate a valid 4x4 red PNG via the image crate (already a dev-dep)
    let img = image::RgbImage::from_fn(4, 4, |_, _| image::Rgb([255u8, 0, 0]));
    let mut png_bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut png_bytes),
        image::ImageFormat::Png,
    )
    .unwrap();
    let mut bundle = AssetBundle::new();
    bundle.add_image("test.png", png_bytes);
    let pdf = Engine::builder()
        .assets(bundle)
        .build()
        .render(r#"<html><body><img src="test.png" width="50" height="50"></body></html>"#)
        .unwrap();
    let result = inspect_bytes(&pdf);
    assert!(!result.images.is_empty(), "expected at least one image");
    let img = &result.images[0];
    assert_eq!(img.page, 1);
    assert!(img.width > 0.0, "image width should be positive");
    assert!(img.height > 0.0, "image height should be positive");
}

/// The bounds must not change what a realistic document extracts, and must
/// sit far above what one needs.
///
/// (The extracted `text` is glyph-id soup rather than readable text because
/// `inspect` does not consult the ToUnicode CMap — a separate, pre-existing
/// limitation. This test asserts on record structure, which is what the
/// bounds can affect.)
#[test]
fn bounds_do_not_alter_realistic_extraction() {
    let html = "<html><body>\
        <h1>Heading</h1>\
        <p>First paragraph with several words.</p>\
        <p>Second paragraph, also with words.</p>\
        </body></html>";
    let result = inspect_bytes(&render_test_pdf(html));
    // At least one record per source block: heading plus the two
    // paragraphs. The exact count depends on line breaking, which depends
    // on font metrics, so only the floor is asserted.
    assert!(
        result.text_items.len() >= 3,
        "expected >=3 text items, got {}",
        result.text_items.len()
    );
    for item in &result.text_items {
        assert_eq!(item.page, 1);
        assert!(!item.text.is_empty());
        assert!(item.font_size > 0.0);
        assert!(item.width > 0.0);
    }
    // Headroom check: the cap is orders of magnitude above this document,
    // so the result must be nowhere near truncation.
    assert!(result.text_items.len() < fulgur_core::MAX_PDF_INSPECT_ITEMS / 1000);
}
