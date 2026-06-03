#![cfg(feature = "image-export")]

use fulgur::engine::Engine;
use fulgur::image_export::{Background, ImageFormat, ImageOptions};

#[test]
fn renders_red_div_to_png_with_correct_dims_and_pixel() {
    let engine = Engine::builder().build();
    let html = r#"<html><body style="margin:0">
        <div style="width:200px;height:100px;background:#ff0000"></div>
    </body></html>"#;
    let mut opts = ImageOptions::new(200, 100, ImageFormat::Png);
    opts.background = Background::Solid([255, 255, 255, 255]);

    let bytes = engine.render_html_to_image(html, &opts).unwrap();
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    );

    let img = image::load_from_memory(&bytes)
        .expect("decode png")
        .to_rgba8();
    assert_eq!(img.dimensions(), (200, 100));
    let p = img.get_pixel(100, 50);
    assert!(
        p[0] > 200 && p[1] < 60 && p[2] < 60,
        "centre should be red, got {p:?}"
    );
}

#[test]
fn scale_doubles_pixel_dimensions() {
    let engine = Engine::builder().build();
    let html = r#"<html><body style="margin:0"><div style="width:50px;height:40px;background:#00ff00"></div></body></html>"#;
    let mut opts = ImageOptions::new(50, 40, ImageFormat::Png);
    opts.scale = 2.0;
    let bytes = engine.render_html_to_image(html, &opts).unwrap();
    let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (100, 80));
}
