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

#[test]
fn at_page_rule_does_not_override_image_canvas_size() {
    // An `@page` rule in the HTML must NOT change the fixed image canvas size:
    // the image canvas marks page_size/margin/landscape as explicit overrides
    // so `resolve_page_settings` cannot re-apply the `@page` dimensions.
    let engine = Engine::builder().build();
    let html = r#"<html><head><style>@page { size: 999px 999px; margin: 50px; }</style></head>
        <body style="margin:0"><div style="width:100px;height:100px;background:#0000ff"></div></body></html>"#;
    let mut opts = ImageOptions::new(200, 100, ImageFormat::Png);
    opts.background = Background::Solid([255, 255, 255, 255]);
    let bytes = engine.render_html_to_image(html, &opts).unwrap();
    let img = image::load_from_memory(&bytes)
        .expect("decode png")
        .to_rgba8();
    assert_eq!(
        img.dimensions(),
        (200, 100),
        "@page must not override the fixed image canvas"
    );
}

#[test]
fn renders_inline_svg_composite_to_png() {
    let engine = Engine::builder().build();
    // A 100x100 canvas; an inline SVG drawing a blue rect filling its box.
    let html = r##"<html><body style="margin:0">
        <svg width="100" height="100" xmlns="http://www.w3.org/2000/svg">
            <rect x="0" y="0" width="100" height="100" fill="#0000ff"/>
        </svg>
    </body></html>"##;
    let mut opts = ImageOptions::new(100, 100, ImageFormat::Png);
    opts.background = Background::Solid([255, 255, 255, 255]);
    let bytes = engine.render_html_to_image(html, &opts).unwrap();
    let img = image::load_from_memory(&bytes)
        .expect("decode png")
        .to_rgba8();
    assert_eq!(img.dimensions(), (100, 100));
    let p = img.get_pixel(50, 50);
    assert!(
        p[2] > 200 && p[0] < 60 && p[1] < 60,
        "centre should be blue (composited SVG), got {p:?}"
    );
}
