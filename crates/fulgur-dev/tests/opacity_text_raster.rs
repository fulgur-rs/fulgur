#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

const CSS: &str = "<style>@page {size:250px 160px;margin:0} body {margin:0;background:white;font:16px/20px 'Noto Sans Mono'} section {position:relative;width:250px;height:160px} div {position:absolute;width:60px;height:60px;background:blue} .first {left:10px;top:10px} .second {left:40px;top:40px} p {position:absolute;left:120px;top:100px;margin:0} </style>";

fn raster(body: &str, css: &str) -> (image::RgbaImage, String) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    std::fs::write(&input, format!("{CSS}<style>{css}</style>{body}")).unwrap();
    let font =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fulgur-ruby/spec/fixtures/noto_sans.ttf");
    let render = Command::new(env!("CARGO_BIN_EXE_fulgur-dev"))
        .args([
            "render",
            "--engine",
            "raikiri",
            "--no-system-fonts",
            "--font",
        ])
        .arg(font)
        .arg(&input)
        .arg("-o")
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        render.status.success(),
        "{}",
        String::from_utf8_lossy(&render.stderr)
    );
    let extracted = Command::new("pdftotext")
        .arg(&pdf)
        .arg("-")
        .output()
        .unwrap();
    assert!(extracted.status.success(), "pdftotext failed");
    let text = String::from_utf8(extracted.stdout).unwrap();
    let output = Command::new("pdftocairo")
        .args(["-png", "-singlefile", "-r", "96"])
        .arg(&pdf)
        .arg(&prefix)
        .output()
        .expect("pdftocairo is required for Linux raster tests");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        image::open(prefix.with_extension("png"))
            .unwrap()
            .to_rgba8(),
        text,
    )
}

fn assert_group_alpha(image: &image::RgbaImage, expected: u8) {
    for (x, y) in [(20, 20), (50, 50), (90, 90)] {
        let pixel = image.get_pixel(x, y);
        assert!(
            pixel[0].abs_diff(expected) <= 1 && pixel[1].abs_diff(expected) <= 1 && pixel[2] == 255,
            "group compositing at ({x}, {y}): {pixel:?}, expected {expected}"
        );
    }
}

#[test]
fn generated_text_preserves_parent_group_opacity() {
    let (image, text) = raster(
        "<section style='opacity:.5'><div class='first'></div><div class='second'></div><p>Body</p></section>",
        "p::before {content:'marker'}",
    );
    assert!(text.contains("marker"), "missing generated text: {text:?}");
    assert!(
        image
            .enumerate_pixels()
            .filter(|(x, y, p)| *x >= 120 && *y >= 100 && p[0] < 220 && p[1] < 220 && p[2] < 220)
            .count()
            > 20,
        "missing generated glyph ink"
    );
    assert_group_alpha(&image, 128);
}

#[test]
fn ellipsis_preserves_parent_group_opacity() {
    let (image, text) = raster(
        "<section style='opacity:.5'><div class='first'></div><div class='second'></div><p>Hello world overflowing</p></section>",
        "p {width:40px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}",
    );
    assert!(text.contains('…'), "missing ellipsis: {text:?}");
    assert!(text.contains("Hel"), "missing source text: {text:?}");
    assert_group_alpha(&image, 128);
}

#[test]
fn nested_groups_include_counter_before_after_and_color_slices() {
    let (image, text) = raster(
        "<section style='opacity:.5'><article style='opacity:.5'><div class='first'></div><div class='second'></div><p>A<span style='color:red'>B</span>C</p></article></section>",
        "section {counter-reset:part 6} p {counter-increment:part;font-size:12px} p::before {content:counter(part) ' marker '} p::after {content:' tail'}",
    );
    assert!(
        text.contains("7 marker"),
        "missing counter/before text: {text:?}"
    );
    assert!(
        text.contains("ABC") && text.contains("tail"),
        "missing source/after text: {text:?}"
    );
    let ink: Vec<_> = image
        .enumerate_pixels()
        .filter(|(x, y, p)| *x >= 120 && *y >= 100 && p[0] < 240 && p[1] < 240 && p[2] < 240)
        .collect();
    assert!(ink.len() > 20, "missing composed glyph ink");
    assert!(
        ink.iter()
            .all(|(_, _, p)| p[0] >= 190 && p[1] >= 190 && p[2] >= 190),
        "glyph alpha differs from nested group alpha"
    );
    assert_group_alpha(&image, 191);
}

#[test]
fn wrapped_color_slices_preserve_all_text_inside_opacity() {
    let (image, text) = raster(
        "<section style='opacity:.5'><div class='first'></div><div class='second'></div><p>AB<span style='color:red'>CD</span>EF</p></section>",
        "p {width:20px;word-break:break-all}",
    );
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert_eq!(
        compact, "ABCDEF",
        "wrapped lines must be complete and drawn once"
    );
    let ink: Vec<_> = image
        .enumerate_pixels()
        .filter(|(x, y, p)| *x >= 120 && *y >= 100 && p[1] < 220 && p[2] < 220)
        .collect();
    assert!(ink.len() > 20, "missing wrapped glyph ink");
    assert!(
        ink.iter()
            .all(|(_, _, p)| p[0] >= 126 && p[1] >= 126 && p[2] >= 126),
        "glyphs escaped opacity group"
    );
    assert_group_alpha(&image, 128);
}
