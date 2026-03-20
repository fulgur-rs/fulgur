use fulgur_core::asset::AssetBundle;
use fulgur_core::config::PageSize;
use fulgur_core::engine::Engine;

#[test]
fn test_bundled_font() {
    let font_path = "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf";
    if !std::path::Path::new(font_path).exists() {
        eprintln!("Skipping test: IPA Gothic font not found at {font_path}");
        return;
    }

    let mut assets = AssetBundle::new();
    assets.add_font_file(font_path).unwrap();
    assets.add_css("body { font-family: 'IPAGothic', sans-serif; }");

    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .assets(assets)
        .build();

    let html = r#"<html><body>
        <h1>請求書</h1>
        <p>株式会社フルグル — バンドルフォントテスト</p>
    </body></html>"#;

    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    // Bundled font should make the PDF larger
    assert!(pdf.len() > 5000);
}
