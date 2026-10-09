#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(body: &str, css: &str) -> (image::RgbaImage, String) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    std::fs::write(&input, format!("<!doctype html><style>@page{{size:180px 160px;margin:0}}body{{margin:0;background:white;font:20px/20px 'Noto Sans'}}p{{margin:0}}.mc{{width:100px;column-count:2;column-gap:20px;orphans:1;widows:1}}{css}</style>{body}")).unwrap();
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
    assert!(extracted.status.success());
    let output = Command::new("pdftocairo")
        .args(["-png", "-singlefile", "-r", "96"])
        .arg(&pdf)
        .arg(&prefix)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        image::open(prefix.with_extension("png"))
            .unwrap()
            .to_rgba8(),
        String::from_utf8(extracted.stdout).unwrap(),
    )
}

fn compare(body: &str, css: &str, reference: &str, letters: &str) {
    let (actual, text) = raster(body, css);
    let (expected, reference_text) = raster(reference, "");
    for letter in letters.chars() {
        assert_eq!(
            reference_text.matches(letter).count(),
            1,
            "control text {reference_text:?}"
        );
        assert_eq!(text.matches(letter).count(), 1, "column text {text:?}");
    }
    assert_eq!(actual.dimensions(), (180, 160));
    assert_eq!(actual.dimensions(), expected.dimensions());
    assert_eq!(
        actual
            .pixels()
            .zip(expected.pixels())
            .filter(|(a, b)| a != b)
            .count(),
        0
    );
}

#[test]
fn balanced_paragraph_columns_match_literal_pdf_text_and_pixels() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        "",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
        "ABCD",
    );
}

#[test]
fn a_split_child_paragraph_and_following_flow_match_literal_pdf_positions() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D</p></div><p>E</p>",
        "",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:0;top:40px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn three_columns_keep_all_six_source_letters_once() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D<br>E<br>F</div>",
        ".mc{width:160px;column-count:3}",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:120px;top:0'>E<br>F</div>",
        "ABCDEF",
    );
}

#[test]
fn column_source_boxes_match_independent_literal_rectangles() {
    compare(
        "<div class=mc><p style='background:red'>A</p><p style='background:lime'>B</p><p style='background:blue'>C</p><p style='background:yellow'>D</p></div>",
        "body{color:transparent}",
        "<div style='position:absolute;left:0;top:0;width:40px;height:20px;background:red'></div><div style='position:absolute;left:0;top:20px;width:40px;height:20px;background:lime'></div><div style='position:absolute;left:60px;top:0;width:40px;height:20px;background:blue'></div><div style='position:absolute;left:60px;top:20px;width:40px;height:20px;background:yellow'></div>",
        "",
    );
}

#[test]
fn clipped_wide_lines_follow_the_producer_column_placement() {
    compare(
        "<div class=mc><p>ABCDEFGHIJKLMNOPQRST<br>abcdefghijklmnopqrst<br>UVWXYZ0123456789<br>uvwxyz0123456789</p></div>",
        ".mc p{white-space:pre;overflow:hidden}",
        "<div style='position:absolute;left:0;top:0;width:40px;height:40px;white-space:pre;overflow:hidden'>ABCDEFGHIJKLMNOPQRST<br>abcdefghijklmnopqrst</div><div style='position:absolute;left:60px;top:0;width:40px;height:40px;white-space:pre;overflow:hidden'>UVWXYZ0123456789<br>uvwxyz0123456789</div>",
        "",
    );
}

#[test]
fn column_clip_composites_background_and_glyphs_in_one_opacity_group() {
    compare(
        "<div class=mc><p>ABCDEFGHIJKLMNOPQRST<br>abcdefghijklmnopqrst<br>UVWXYZ0123456789<br>uvwxyz0123456789</p></div>",
        ".mc p{white-space:pre;overflow:hidden;opacity:.5;background:lime}",
        "<div style='position:absolute;left:0;top:0;width:40px;height:40px;white-space:pre;overflow:hidden;opacity:.5;background:lime'>ABCDEFGHIJKLMNOPQRST<br>abcdefghijklmnopqrst</div><div style='position:absolute;left:60px;top:0;width:40px;height:40px;white-space:pre;overflow:hidden;opacity:.5;background:lime'>UVWXYZ0123456789<br>uvwxyz0123456789</div>",
        "",
    );
}

#[test]
fn padded_columns_match_literal_pdf_positions() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D</p></div><p>E</p>",
        ".mc{padding:4px 5px;border:1px solid black;background:lime}",
        "<div style='position:absolute;left:0;top:0;box-sizing:border-box;width:112px;height:50px;border:1px solid black;background:lime'></div><div style='position:absolute;left:6px;top:5px'>A<br>B</div><div style='position:absolute;left:66px;top:5px'>C<br>D</div><div style='position:absolute;left:0;top:50px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn padded_border_box_columns_match_literal_pdf_positions() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D</p></div><p>E</p>",
        ".mc{box-sizing:border-box;padding:4px 5px;border:1px solid black;background:lime}",
        "<div style='position:absolute;left:0;top:0;box-sizing:border-box;width:100px;height:50px;border:1px solid black;background:lime'></div><div style='position:absolute;left:6px;top:5px'>A<br>B</div><div style='position:absolute;left:60px;top:5px'>C<br>D</div><div style='position:absolute;left:0;top:50px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn padded_minimum_height_columns_match_literal_pdf_positions() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D</p></div><p>E</p>",
        ".mc{min-height:60px;padding:4px 5px;border:1px solid black;background:lime}",
        "<div style='position:absolute;left:0;top:0;box-sizing:border-box;width:112px;height:70px;border:1px solid black;background:lime'></div><div style='position:absolute;left:6px;top:5px'>A<br>B</div><div style='position:absolute;left:66px;top:5px'>C<br>D</div><div style='position:absolute;left:0;top:70px'>E</div>",
        "ABCDE",
    );
}
