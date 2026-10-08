#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

const CSS: &str = "<style>@page {size:200px 150px;margin:0} body {margin:0;background:white;font:12px/16px 'Noto Sans Mono'} .box {width:100px;height:50px;background:red} .anchor {position:absolute;left:120px;top:100px}</style>";

fn raster(body: &str, css: &str) -> (image::RgbaImage, String) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    std::fs::write(
        &input,
        format!("{CSS}<style>{css}</style>{body}<p class=anchor>radius</p>"),
    )
    .unwrap();
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
    assert_eq!(text.trim(), "radius");
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
    let image = image::open(prefix.with_extension("png"))
        .unwrap()
        .to_rgba8();
    assert!(
        image
            .pixels()
            .filter(|p| p[0] < 80 && p[1] < 80 && p[2] < 80)
            .count()
            > 20,
        "missing glyph ink"
    );
    (image, text)
}

fn assert_white(image: &image::RgbaImage, x: u32, y: u32) {
    let pixel = image.get_pixel(x, y);
    assert!(
        pixel[0] > 245 && pixel[1] > 245 && pixel[2] > 245,
        "({x},{y}): {pixel:?}"
    );
}

fn assert_red(image: &image::RgbaImage, x: u32, y: u32) {
    let pixel = image.get_pixel(x, y);
    assert!(
        pixel[0] > 245 && pixel[1] < 20 && pixel[2] < 20,
        "({x},{y}): {pixel:?}"
    );
}

#[test]
fn slash_radii_keep_distinct_horizontal_and_vertical_axes() {
    let (image, _) = raster("<div class=box></div>", ".box {border-radius:30px / 15px}");
    assert_white(&image, 8, 2);
    assert_red(&image, 8, 8);
    assert_red(&image, 50, 20);
}

#[test]
fn percentage_axes_resolve_against_width_and_height() {
    let (image, _) = raster("<div class=box></div>", ".box {border-radius:50% / 25%}");
    assert_white(&image, 8, 2);
    assert_red(&image, 25, 8);
    assert_red(&image, 50, 20);
}

#[test]
fn two_value_longhands_round_each_corner() {
    for (property, outside, inside) in [
        ("border-top-left-radius", (8, 2), (8, 8)),
        ("border-top-right-radius", (91, 2), (91, 8)),
        ("border-bottom-right-radius", (91, 47), (91, 41)),
        ("border-bottom-left-radius", (8, 47), (8, 41)),
    ] {
        let (image, _) = raster(
            "<div class=box></div>",
            &format!(".box {{{property}:30px 15px}}"),
        );
        assert_white(&image, outside.0, outside.1);
        assert_red(&image, inside.0, inside.1);
    }
}

#[test]
fn either_zero_axis_leaves_a_square_corner() {
    for value in ["30px / 0", "0 / 15px"] {
        let (image, _) = raster(
            "<div class=box></div>",
            &format!(".box {{border-radius:{value}}}"),
        );
        assert_red(&image, 1, 1);
    }
}

#[test]
fn backgroundless_parent_clips_children_to_elliptical_padding_edge() {
    let (image, _) = raster(
        "<div class=box><div class=child></div></div>",
        ".box {background:none;overflow:hidden;border-radius:30px / 15px}.child {width:100px;height:50px;background:red}",
    );
    assert_white(&image, 8, 2);
    assert_red(&image, 8, 8);
}

#[test]
fn borders_and_padding_clip_use_the_same_ellipse() {
    let (image, _) = raster(
        "<div class=box></div>",
        ".box {box-sizing:border-box;border:4px solid red;background:blue;border-radius:30px / 15px}",
    );
    assert_white(&image, 8, 2);
    assert_red(&image, 15, 4);
    let pixel = image.get_pixel(30, 15);
    assert!(
        pixel[2] > 245 && pixel[0] < 20 && pixel[1] < 20,
        "inner: {pixel:?}"
    );
}

#[test]
fn opposite_border_crops_padding_ellipse_instead_of_rescaling_it() {
    for (corner, border, outside, inside) in [
        ("top-left", "right", (10, 10), (10, 25)),
        ("top-right", "left", (90, 10), (90, 25)),
        ("bottom-right", "left", (90, 49), (90, 34)),
        ("bottom-left", "right", (10, 49), (10, 34)),
    ] {
        let (image, _) = raster(
            "<div class=box></div>",
            &format!(
                ".box {{width:100px;height:60px;box-sizing:border-box;border-{border}:80px solid transparent;border-{corner}-radius:50px 30px;background-clip:padding-box}}"
            ),
        );
        assert_white(&image, outside.0, outside.1);
        assert_red(&image, inside.0, inside.1);
    }
}

#[test]
fn opposite_border_crops_overflow_ellipse_instead_of_rescaling_it() {
    let (image, _) = raster(
        "<div class=box><div class=child></div></div>",
        ".box {width:100px;height:60px;box-sizing:border-box;background:none;overflow:hidden;border-right:80px solid transparent;border-top-left-radius:50px 30px}.child {width:100px;height:60px;background:red}",
    );
    assert_white(&image, 10, 10);
    assert_red(&image, 10, 25);
    assert_white(&image, 30, 40);
}

#[test]
fn uniform_border_ring_crops_large_inner_ellipse_in_all_corners() {
    for (corner, red, blue) in [
        ("top-left", (50, 25), (75, 35)),
        ("top-right", (49, 25), (24, 35)),
        ("bottom-right", (49, 34), (24, 24)),
        ("bottom-left", (50, 34), (75, 24)),
    ] {
        let (image, _) = raster(
            "<div class=box></div>",
            &format!(
                ".box {{width:100px;height:60px;box-sizing:border-box;border:20px solid red;border-{corner}-radius:100px 60px;background:blue;background-clip:padding-box}}"
            ),
        );
        assert_red(&image, red.0, red.1);
        let pixel = image.get_pixel(blue.0, blue.1);
        assert!(
            pixel[2] > 245 && pixel[0] < 20 && pixel[1] < 20,
            "{corner} inner: {pixel:?}"
        );
    }
}

const DIAGONALS: [&str; 2] = [
    "border-top-left-radius:100px;border-bottom-right-radius:100px",
    "border-top-right-radius:100px;border-bottom-left-radius:100px",
];

#[test]
fn crossing_inner_ellipses_keep_only_the_common_padding_region() {
    for (radii, outside) in DIAGONALS.into_iter().zip([(78, 21), (21, 21)]) {
        let (image, _) = raster(
            "<div class=box></div>",
            &format!(
                ".box {{width:100px;height:100px;box-sizing:border-box;border:20px solid transparent;{radii};background-clip:padding-box}}"
            ),
        );
        assert_white(&image, outside.0, outside.1);
        assert_red(&image, 50, 50);
    }
}

#[test]
fn crossing_inner_ellipses_with_no_common_region_paint_no_background() {
    for radii in DIAGONALS {
        let (image, _) = raster(
            "<div class=box></div>",
            &format!(
                ".box {{width:100px;height:100px;box-sizing:border-box;border:40px solid transparent;{radii};background-clip:padding-box}}"
            ),
        );
        assert_white(&image, 50, 50);
    }
}

#[test]
fn crossing_inner_ellipses_with_no_common_region_leave_the_full_border() {
    for radii in DIAGONALS {
        let (image, _) = raster(
            "<div class=box></div>",
            &format!(
                ".box {{width:100px;height:100px;box-sizing:border-box;border:40px solid red;{radii};background:blue;background-clip:padding-box}}"
            ),
        );
        assert_red(&image, 50, 50);
    }
}

#[test]
fn crossing_inner_ellipses_with_no_common_region_clip_all_children() {
    for radii in DIAGONALS {
        let (image, _) = raster(
            "<div class=box><div class=child></div></div>",
            &format!(
                ".box {{width:100px;height:100px;box-sizing:border-box;border:40px solid transparent;{radii};background:none;overflow:hidden}} .child {{width:100px;height:100px;background:red}}"
            ),
        );
        assert_white(&image, 50, 50);
    }
}

#[test]
fn empty_inner_ellipse_mixed_border_colors_cover_the_center() {
    for radii in DIAGONALS {
        let (image, _) = raster(
            "<div class=box></div>",
            &format!(
                ".box {{width:100px;height:100px;box-sizing:border-box;border:40px solid red;border-top-color:blue;{radii};background:none}}"
            ),
        );
        for y in 42..58 {
            for x in 42..58 {
                // Side joins are antialiased; sample fully covered interiors.
                if (x as i32 - y as i32).abs() <= 1 || (x as i32 + y as i32 - 100).abs() <= 1 {
                    continue;
                }
                assert!(
                    image.get_pixel(x, y)[1] < 30,
                    "unpainted border center at ({x},{y}): {:?}",
                    image.get_pixel(x, y)
                );
            }
        }
    }
}
