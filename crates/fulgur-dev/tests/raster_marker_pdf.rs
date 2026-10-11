#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(css: &str, alpha: bool) -> image::RgbaImage {
    raster_body(css, alpha, "<ul><li>A</li></ul>")
}

fn raster_body(css: &str, alpha: bool, body: &str) -> image::RgbaImage {
    raster_result(css, alpha, body).0
}

fn raster_result(css: &str, alpha: bool, body: &str) -> (image::RgbaImage, Vec<u8>) {
    raster_result_page(css, alpha, body, 1)
}

fn raster_result_page(
    css: &str,
    alpha: bool,
    body: &str,
    page: u32,
) -> (image::RgbaImage, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("document");
    std::fs::create_dir(&root).unwrap();
    let input = root.join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("page");
    let source = "source.png";
    let image = image::RgbaImage::from_pixel(
        8,
        4,
        image::Rgba([255, 0, 0, if alpha { 128 } else { 255 }]),
    );
    image.save(root.join(source)).unwrap();
    // The outside source exists, so the fallback proves the directory boundary.
    std::fs::copy(root.join(source), dir.path().join(source)).unwrap();
    let body = body.replace("SOURCE", source);
    let css = css.replace("SOURCE", source);
    std::fs::write(&input,format!("<!doctype html><style>@page{{size:100px 100px;margin:0}}body{{margin:0;background:white}}body{{font:20px/20px sans-serif}}ul{{margin:0;padding:0}}li{{margin-left:20px;list-style-image:url({source});color:transparent}}{css}</style>{body}")).unwrap();
    let font =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fulgur-ruby/spec/fixtures/noto_sans.ttf");
    let output = Command::new(env!("CARGO_BIN_EXE_fulgur-dev"))
        .args([
            "render",
            "--engine",
            "raikiri",
            "--no-system-fonts",
            "--font",
        ])
        .arg(font)
        .arg(input)
        .arg("-o")
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("pdftocairo")
        .args(["-png", "-singlefile", "-r", "96", "-f"])
        .arg(page.to_string())
        .arg("-l")
        .arg(page.to_string())
        .arg(&pdf)
        .arg(&prefix)
        .output()
        .expect("pdftocairo is required; install poppler-utils");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        image::open(prefix.with_extension("png"))
            .unwrap()
            .to_rgba8(),
        std::fs::read(pdf).unwrap(),
    )
}

fn color(image: &image::RgbaImage, x: u32, y: u32, expected: [u8; 4], tolerance: u8) {
    let actual = image.get_pixel(x, y).0;
    assert!(
        actual
            .into_iter()
            .zip(expected)
            .all(|(a, b)| a.abs_diff(b) <= tolerance),
        "({x},{y}): {actual:?}, expected {expected:?}"
    );
}

#[test]
fn outside_png_marker_has_literal_geometry_and_owner_opacity() {
    let image = raster("li{opacity:.5;overflow:hidden}", false);
    for (x, y) in [(9, 1), (14, 2)] {
        color(&image, x, y, [255, 127, 127, 255], 1);
    }
    for (x, y) in [(7, 1), (16, 1), (10, 5), (30, 1)] {
        color(&image, x, y, [255, 255, 255, 255], 0);
    }
}

#[test]
fn inside_png_marker_reserves_inline_space_and_paints_one_atomic_box() {
    let image = raster("li{list-style-position:inside}", false);
    // The bundled font places the baseline between pixel rows. Integrating
    // coverage verifies the 8x4 image area without treating antialiased edges
    // as missing image pixels.
    let colored: Vec<_> = image
        .enumerate_pixels()
        .filter(|(_, _, p)| p[0] > p[1])
        .map(|(x, y, _)| (x, y))
        .collect();
    let coverage: f64 = image.pixels().map(|p| f64::from(255 - p[1]) / 255.0).sum();
    assert!((coverage - 32.0).abs() < 0.1, "coverage={coverage}");
    assert_eq!(colored.iter().map(|p| p.0).min(), Some(20));
    assert_eq!(colored.iter().map(|p| p.0).max(), Some(27));
    let ymin = colored.iter().map(|p| p.1).min().unwrap();
    let ymax = colored.iter().map(|p| p.1).max().unwrap();
    assert!((4..=5).contains(&(ymax - ymin + 1)));
    assert!(ymin < 20);
}

#[test]
fn marker_alpha_and_ancestor_clip_preserve_the_outside_rectangle() {
    let image = raster("li{opacity:.5}", true);
    color(&image, 10, 1, [255, 191, 191, 255], 1);
    let image = raster_body(
        "",
        false,
        "<div style='margin-left:12px;width:50px;height:20px;overflow:hidden'><ul><li>A</li></ul></div>",
    );
    color(&image, 21, 1, [255, 0, 0, 255], 0);
    let image = raster_body(
        "li{margin-left:0}",
        false,
        "<div style='margin-left:12px;width:50px;height:20px;overflow:hidden'><ul><li>A</li></ul></div>",
    );
    color(&image, 2, 1, [255, 255, 255, 255], 0);
}

#[test]
fn missing_or_outside_source_preserves_literal_text_marker_fallback() {
    let css = "li{color:blue;list-style-image:none;list-style-type:decimal;margin-left:50px}";
    let expected = raster(css, false);
    assert!(expected.pixels().any(|p| p[2] > p[0]));
    for source in ["missing.png", "../source.png"] {
        let actual = raster(&format!("{css}li{{list-style-image:url({source})}}"), false);
        assert_eq!(actual, expected);
    }
    let expected = raster(&format!("{css}li::marker{{content:'X '}}"), false);
    let actual = raster(
        "li{color:blue;margin-left:50px}li::marker{content:'X '}",
        false,
    );
    assert_eq!(actual, expected);
}

#[test]
fn only_the_first_fragment_paints_the_marker_and_sources_share_pdf_resources() {
    let body = "<ul><li>AA<br>BB<br>CC<br>DD<br>EE<br>FF<br>GG</li></ul>";
    let (first, bytes) = raster_result_page("", false, body, 1);
    let (second, _) = raster_result_page("", false, body, 2);
    color(&first, 10, 1, [255, 0, 0, 255], 0);
    assert!(second.pixels().all(|p| p.0 == [255, 255, 255, 255]));
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(pdf.get_pages().len() > 1);
    let (_, bytes) = raster_result("", false, "<ul><li>A</li><li>B</li><li>C</li></ul>");
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let images = pdf
        .objects
        .values()
        .filter(|object| {
            object.as_stream().is_ok_and(|stream| {
                stream
                    .dict
                    .get(b"Subtype")
                    .and_then(lopdf::Object::as_name)
                    .ok()
                    == Some(b"Image".as_slice())
            })
        })
        .count();
    assert_eq!(images, 1);
}
