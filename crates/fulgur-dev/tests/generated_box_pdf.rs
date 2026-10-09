#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(body: &str, css: &str) -> image::RgbaImage {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("page");
    std::fs::write(&input, format!("<!doctype html><style>@page{{size:100px 100px;margin:0}}body{{margin:0;background:white;font:20px/20px sans-serif}}{css}</style>{body}")).unwrap();
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
    image::open(prefix.with_extension("png"))
        .unwrap()
        .to_rgba8()
}

#[test]
fn generated_backgrounds_match_literal_inlines() {
    let actual = raster(
        "<div>A</div>",
        "div::before{content:'X';background:red;color:transparent}div::after{content:'Y';background:blue;color:transparent}",
    );
    let expected = raster(
        "<div><span style='background:red;color:transparent'>X</span>A<span style='background:blue;color:transparent'>Y</span></div>",
        "",
    );
    assert!(expected.pixels().any(|p| p[0] > p[1] && p[0] > p[2]));
    assert!(expected.pixels().any(|p| p[2] > p[0] && p[2] > p[1]));
    assert!(
        actual == expected,
        "generated decoration pixels differ from the literal reference"
    );
}

#[test]
fn block_generated_backgrounds_follow_their_lines() {
    let actual = raster(
        "<div>A</div>",
        "div::before{display:block;content:'X';background:red;color:transparent}div::after{display:block;content:'Y';background:blue;color:transparent}",
    );
    let expected = raster(
        "<div><span style='background:red;color:transparent'>X</span><br>A<br><span style='background:blue;color:transparent'>Y</span></div>",
        "",
    );
    assert!(
        actual == expected,
        "generated decoration pixels differ from the literal reference"
    );
}

fn compare(body: &str, css: &str, literal: &str, reference_css: &str) {
    let actual = raster(body, css);
    let expected = raster(literal, reference_css);
    assert!(
        actual == expected,
        "generated decoration pixels differ from the literal reference"
    );
}

#[test]
fn pseudo_borders_padding_and_owner_opacity_match_literal_boxes() {
    compare(
        "<div>A</div>",
        "div{opacity:.5;padding-top:20px}div::before,span{background:red;color:transparent;border:2px solid blue;padding:3px}div::before{content:'X'}",
        "<div><span>X</span>A</div>",
        "div{opacity:.5;padding-top:20px}span{background:red;color:transparent;border:2px solid blue;padding:3px}",
    );
}

#[test]
fn pseudo_decorations_use_the_owners_overflow_chain() {
    compare(
        "<div></div>",
        "div{width:15px;overflow:hidden;opacity:.5}div::before,span{background:red;color:transparent}div::before{content:'XXX'}",
        "<div><span>XXX</span></div>",
        "div{width:15px;overflow:hidden;opacity:.5}span{background:red;color:transparent}",
    );
}

#[test]
fn pseudo_background_text_clip_excludes_owner_and_other_pseudo_glyphs() {
    let actual = raster(
        "<div>A</div>",
        "div::before{content:'X';color:transparent;background:red;background-clip:text}div::after{content:'Y';color:transparent;background:blue;background-clip:text}",
    );
    let expected = raster(
        "<div><span style='color:transparent;background:red;background-clip:text'>X</span>A<span style='color:transparent;background:blue;background-clip:text'>Y</span></div>",
        "",
    );
    assert!(
        actual == expected,
        "pseudo glyph clip differs from independent inline reference"
    );
}

#[test]
fn generated_counter_and_attribute_backgrounds_keep_resolved_text() {
    let css = "body{counter-reset:n}ol{margin:0;padding:0}li{list-style:none;counter-increment:n}li::before{content:attr(data-label) counter(n);background:red;color:transparent}";
    let actual = raster(
        "<ol><li data-label='L'>A</li><li data-label='R'>B</li></ol>",
        css,
    );
    let expected = raster(
        "<ol><li><span style='background:red;color:transparent'>L1</span>A</li><li><span style='background:red;color:transparent'>R2</span>B</li></ol>",
        "ol{margin:0;padding:0}li{list-style:none}",
    );
    assert!(
        actual == expected,
        "resolved generated counter/attribute text differs from literals"
    );
}

#[test]
fn wrapped_pseudo_keeps_borders_only_at_its_outer_inline_edges() {
    let image = raster(
        "<div></div>",
        "div{width:20px}div::before{content:'X X X';background:red;color:transparent;border-left:2px solid blue;border-right:2px solid blue}",
    );
    for (x, y, expected) in [
        (1, 8, [0, 0, 255, 255]),
        (1, 28, [255, 0, 0, 255]),
        (12, 48, [0, 0, 255, 255]),
    ] {
        assert!(
            image
                .get_pixel(x, y)
                .0
                .into_iter()
                .zip(expected)
                .all(|(a, b)| a.abs_diff(b) <= 1),
            "({x},{y}) wrong inline edge pixel {:?}",
            image.get_pixel(x, y).0
        );
    }
}

#[test]
fn rtl_wrapped_pseudo_starts_on_the_physical_right_and_ends_on_the_left() {
    let image = raster(
        "<div></div>",
        "div{width:20px}div::before{direction:rtl;content:'X X X';background:red;color:transparent;border-left:2px solid blue;border-right:3px solid lime}",
    );
    let blue = |y| {
        (0..100)
            .filter(|&x| {
                let p = image.get_pixel(x, y);
                p[2] > p[0] && p[2] > p[1]
            })
            .count()
    };
    let green = |y| {
        (0..100)
            .filter(|&x| {
                let p = image.get_pixel(x, y);
                p[1] > p[0] && p[1] > p[2]
            })
            .count()
    };
    assert_eq!(blue(8), 0);
    assert!(green(8) > 0);
    assert!(blue(48) > 0);
    assert_eq!(green(48), 0);
    assert_eq!((blue(28), green(28)), (0, 0));
}
