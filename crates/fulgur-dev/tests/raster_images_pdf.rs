#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(css: &str, jpeg: bool, alpha: bool) -> image::RgbaImage {
    raster_body(css, jpeg, alpha, "<img src='SOURCE'>")
}

fn raster_body(css: &str, jpeg: bool, alpha: bool, body: &str) -> image::RgbaImage {
    raster_result(css, jpeg, alpha, body).0
}

fn raster_result(css: &str, jpeg: bool, alpha: bool, body: &str) -> (image::RgbaImage, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("document");
    std::fs::create_dir(&root).unwrap();
    let input = root.join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("page");
    let source = if jpeg { "source.jpg" } else { "source.png" };
    if jpeg {
        let image = image::RgbImage::from_fn(64, 32, |x, y| {
            image::Rgb(match (x < 32, y < 16) {
                (true, true) => [255, 0, 0],
                (false, true) => [0, 255, 0],
                (true, false) => [0, 0, 255],
                (false, false) => [255, 255, 255],
            })
        });
        let file = std::fs::File::create(root.join(source)).unwrap();
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 100)
            .encode_image(&image)
            .unwrap();
    } else {
        let image = image::RgbaImage::from_fn(2, 2, |x, y| {
            image::Rgba(if alpha {
                [255, 0, 0, 128]
            } else {
                match (x, y) {
                    (0, 0) => [255, 0, 0, 255],
                    (1, 0) => [0, 255, 0, 255],
                    (0, 1) => [0, 0, 255, 255],
                    _ => [255, 255, 255, 255],
                }
            })
        });
        image.save(root.join(source)).unwrap();
    }
    // The outside source exists, so the fallback proves the directory boundary.
    std::fs::copy(root.join(source), dir.path().join(source)).unwrap();
    let body = body.replace("SOURCE", source);
    std::fs::write(&input,format!("<!doctype html><style>@page{{size:100px 100px;margin:0}}body{{margin:0;background:white}}img{{display:block;width:80px;height:80px;{css}}}</style>{body}")).unwrap();
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
        .args(["-png", "-singlefile", "-r", "96"])
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
fn local_png_pixels_are_painted_at_the_used_image_size() {
    let image = raster("", false, false);
    assert_eq!(image.dimensions(), (100, 100));
    for (x, y, expected) in [
        (5, 5, [255, 0, 0, 255]),
        (75, 5, [0, 255, 0, 255]),
        (5, 75, [0, 0, 255, 255]),
        (75, 75, [255, 255, 255, 255]),
    ] {
        color(&image, x, y, expected, 0);
    }
    color(&image, 90, 40, [255, 255, 255, 255], 0);
}

#[test]
fn jpeg_object_fit_preserves_the_ratio_and_content_clip() {
    for fit in ["contain", "cover"] {
        let image = raster(&format!("object-fit:{fit}"), true, false);
        let (top, bottom) = if fit == "contain" { (25, 55) } else { (5, 75) };
        for (x, y, expected) in [
            (5, top, [255, 0, 0, 255]),
            (75, top, [0, 255, 0, 255]),
            (5, bottom, [0, 0, 255, 255]),
            (75, bottom, [255, 255, 255, 255]),
        ] {
            color(&image, x, y, expected, 4);
        }
        if fit == "contain" {
            color(&image, 40, 10, [255, 255, 255, 255], 0);
            color(&image, 40, 70, [255, 255, 255, 255], 0);
        }
        color(&image, 90, 40, [255, 255, 255, 255], 0);
    }
}

#[test]
fn png_alpha_blends_once_against_the_pdf_background() {
    let image = raster("", false, true);
    color(&image, 40, 40, [255, 127, 127, 255], 1);
    color(&image, 90, 40, [255, 255, 255, 255], 0);
}

#[test]
fn raster_opacity_border_and_percentage_padding_preserve_the_content_box() {
    let image = raster(
        "padding:10%;border:2px solid black;opacity:.5",
        false,
        false,
    );
    color(&image, 20, 1, [127, 127, 127, 255], 1);
    color(&image, 8, 8, [255, 255, 255, 255], 0);
    color(&image, 14, 14, [255, 127, 127, 255], 1);
    color(&image, 86, 14, [127, 255, 127, 255], 1);
    color(&image, 14, 86, [127, 127, 255, 255], 1);
}

#[test]
fn ancestor_overflow_clips_pixels_without_changing_object_geometry() {
    let image = raster_body(
        "",
        false,
        false,
        "<div style='width:50px;height:50px;overflow:hidden'><img src='SOURCE'></div>",
    );
    color(&image, 5, 5, [255, 0, 0, 255], 0);
    color(&image, 75, 5, [255, 255, 255, 255], 0);
    color(&image, 5, 75, [255, 255, 255, 255], 0);
}

#[test]
fn hidden_missing_and_outside_directory_images_do_not_paint_cached_pixels() {
    let image = raster_body(
        "width:20px;height:20px",
        false,
        false,
        "<img src='SOURCE' style='visibility:hidden'><img src='missing.png'><img src='../source.png'>",
    );
    for (x, y) in [(5, 5), (5, 25), (5, 45), (90, 90)] {
        color(&image, x, y, [255, 255, 255, 255], 0);
    }
}

#[test]
fn repeated_sources_reuse_one_pdf_image_resource() {
    let (image, bytes) = raster_result(
        "width:20px;height:20px",
        false,
        false,
        "<img src='SOURCE'><img src='SOURCE'>",
    );
    color(&image, 2, 2, [255, 0, 0, 255], 0);
    color(&image, 2, 22, [255, 0, 0, 255], 0);
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
    assert_eq!(images, 1, "opaque repeated sources share one PDF image");
}

#[test]
fn replaced_images_keep_their_own_rounded_overflow_clip() {
    for jpeg in [false, true] {
        let actual = raster("border-radius:50%;overflow:hidden", jpeg, false);
        let expected = raster_body(
            "",
            jpeg,
            false,
            "<div style='width:80px;height:80px;border-radius:50%;overflow:hidden'><img src='SOURCE'></div>",
        );
        color(&expected, 2, 2, [255, 255, 255, 255], 0);
        color(&actual, 2, 2, [255, 255, 255, 255], 0);
        assert_eq!(
            actual
                .pixels()
                .zip(expected.pixels())
                .filter(|(a, b)| a != b)
                .count(),
            0,
            "rounded replaced clip, jpeg={jpeg}"
        );
    }
}

#[test]
fn multicol_images_keep_column_and_spanner_positions_with_rounded_clips() {
    let body = "<style>.mc{width:80px;column-count:2;column-gap:20px;orphans:1;widows:1}.span{column-span:all}</style><div class=mc><div><img src='SOURCE'></div><div><img src='SOURCE'></div><div class=span><img src='SOURCE'></div><div><img src='SOURCE'></div><div><img src='SOURCE'></div></div><img src='SOURCE'>";
    let reference = "<img src='SOURCE' style='position:absolute;left:0;top:0'><img src='SOURCE' style='position:absolute;left:50px;top:0'><img src='SOURCE' style='position:absolute;left:0;top:20px'><img src='SOURCE' style='position:absolute;left:0;top:40px'><img src='SOURCE' style='position:absolute;left:50px;top:40px'><img src='SOURCE' style='position:absolute;left:0;top:60px'>";
    for jpeg in [false, true] {
        let css = "width:20px;height:20px;border-radius:50%;overflow:hidden";
        let (actual, actual_pdf) = raster_result(css, jpeg, false, body);
        let (expected, expected_pdf) = raster_result(css, jpeg, false, reference);
        for bytes in [&actual_pdf, &expected_pdf] {
            let pdf = lopdf::Document::load_mem(bytes).unwrap();
            let pages = pdf.get_pages();
            assert_eq!(pages.len(), 1);
            let content = pdf.get_page_content(pages[&1]).unwrap();
            let content = lopdf::content::Content::decode(&content).unwrap();
            assert_eq!(
                content
                    .operations
                    .iter()
                    .filter(|operation| operation.operator == "Do")
                    .count(),
                6,
                "six image placements, jpeg={jpeg}"
            );
        }
        assert_eq!(actual.dimensions(), (100, 100));
        assert_eq!(actual.dimensions(), expected.dimensions());
        color(&expected, 0, 0, [255, 255, 255, 255], 0);
        color(&expected, 4, 4, [255, 0, 0, 255], if jpeg { 4 } else { 0 });
        assert_eq!(
            actual
                .pixels()
                .zip(expected.pixels())
                .filter(|(a, b)| a != b)
                .count(),
            0,
            "column and full-width spanner image placements, jpeg={jpeg}"
        );
    }
}
