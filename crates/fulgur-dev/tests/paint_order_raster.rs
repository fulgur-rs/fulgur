#![cfg(target_os = "linux")]

use std::process::Command;

fn raster(html: &str) -> image::RgbaImage {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    std::fs::write(&input, html).unwrap();
    let render = Command::new(env!("CARGO_BIN_EXE_fulgur-dev"))
        .args(["render", "--engine", "raikiri"])
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
    let png = std::fs::read(prefix.with_extension("png")).unwrap();
    image::load_from_memory(&png).unwrap().to_rgba8()
}

fn assert_color(image: &image::RgbaImage, x: u32, y: u32, expected: [u8; 4]) {
    let actual = image.get_pixel(x, y).0;
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            actual.abs_diff(expected) <= 1,
            "pixel ({x},{y}): {actual} vs {expected}"
        );
    }
}

const CSS: &str = "<style>@page {size:200px 150px;margin:0} body {margin:0;background:white} div {position:absolute;width:60px;height:60px;background:blue}</style>";

#[test]
fn ordered_group_opacity_composites_children_once() {
    let image = raster(&format!(
        "{CSS}<section style='opacity:0.5'><div style='left:10px;top:10px'></div><div style='left:40px;top:40px'></div></section>"
    ));
    assert_color(&image, 20, 20, [128, 128, 255, 255]);
    assert_color(&image, 50, 50, [128, 128, 255, 255]);
    assert_color(&image, 90, 90, [128, 128, 255, 255]);
}

#[test]
fn ordered_siblings_follow_z_index() {
    let image = raster(&format!(
        "{CSS}<div style='left:10px;top:10px;z-index:2'></div><div style='left:10px;top:10px;z-index:1;background:red'></div>"
    ));
    assert_color(&image, 30, 30, [0, 0, 255, 255]);
}

#[test]
fn positioned_background_covers_prior_text() {
    let image = raster(
        "<style>@page {size:200px 150px;margin:0} body {margin:0;background:white} p {margin:0;font-size:30px}</style><p>MMMM</p><div style='position:absolute;left:0;top:0;width:100px;height:50px;background:blue'></div>",
    );
    for x in [5, 15, 30, 50, 80] {
        for y in [5, 15, 25, 35, 45] {
            assert_color(&image, x, y, [0, 0, 255, 255]);
        }
    }
}

#[test]
fn rounded_and_axis_clips_keep_inside_and_outside_colors() {
    let image = raster(include_str!(
        "../../../tests/fixtures/raikiri-dev/clips.html"
    ));
    assert_color(&image, 30, 30, [255, 0, 0, 255]);
    assert_color(&image, 10, 10, [255, 255, 255, 255]);
    assert_color(&image, 65, 30, [255, 255, 255, 255]);
    assert_color(&image, 30, 65, [255, 255, 255, 255]);
    assert_color(&image, 120, 30, [0, 0, 255, 255]);
    assert_color(&image, 120, 70, [0, 0, 255, 255]);
    assert_color(&image, 155, 30, [255, 255, 255, 255]);
}
