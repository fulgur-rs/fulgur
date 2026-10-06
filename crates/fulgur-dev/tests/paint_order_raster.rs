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
