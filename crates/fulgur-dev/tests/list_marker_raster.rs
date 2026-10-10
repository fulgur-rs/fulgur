#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(body: &str, css: &str) -> (image::RgbaImage, String) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    std::fs::write(&input, format!("<style>@page{{size:240px 160px;margin:0}}body{{margin:0;background:white;font:16px/24px 'Noto Sans Mono'}}ol,ul{{margin:10px 0 0;padding-left:80px}}li{{margin:0;padding:0}}{css}</style>{body}")).unwrap();
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
        .arg(input)
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
        .arg(pdf)
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

fn gutter_ink(image: &image::RgbaImage) -> Vec<[u8; 4]> {
    image
        .enumerate_pixels()
        .filter(|(x, y, p)| {
            *x >= 40 && *x < 79 && *y >= 10 && *y < 75 && p[0] < 230 && p[1] < 230 && p[2] < 230
        })
        .map(|(_, _, p)| p.0)
        .collect()
}

#[test]
fn outside_numbers_are_embedded_once_and_painted_in_the_gutter() {
    let (image, text) = raster("<ol><li>Alpha</li><li>Beta</li></ol>", "");
    assert_eq!(text.matches("1.").count(), 1, "{text:?}");
    assert_eq!(text.matches("2.").count(), 1, "{text:?}");
    assert!(text.contains("Alpha") && text.contains("Beta"));
    assert!(
        gutter_ink(&image).len() > 30,
        "missing marker ink in the independent gutter region"
    );
}

#[test]
fn item_overflow_does_not_clip_outside_markers_and_opacity_includes_them() {
    let (image, text) = raster(
        "<ol><li>Alpha</li><li>Beta</li></ol>",
        "li{overflow:hidden;opacity:.5}",
    );
    assert!(text.contains("1.") && text.contains("2."), "{text:?}");
    let ink = gutter_ink(&image);
    assert!(
        ink.len() > 30,
        "outside markers were clipped by their own item"
    );
    assert!(
        ink.iter()
            .all(|p| p[0] >= 126 && p[1] >= 126 && p[2] >= 126),
        "marker escaped item opacity"
    );
}

#[test]
fn ancestor_overflow_still_clips_the_marker_gutter() {
    let (image, text) = raster(
        "<section><ol><li>Alpha</li><li>Beta</li></ol></section>",
        "section{margin-left:80px;width:140px;overflow:hidden}ol{padding-left:0}",
    );
    assert!(text.contains("Alpha") && text.contains("Beta"));
    assert!(
        gutter_ink(&image).is_empty(),
        "marker escaped ancestor overflow"
    );
}

#[test]
fn empty_items_keep_their_marker_and_inside_markers_are_not_duplicated() {
    let (image, text) = raster("<ol><li></li></ol>", "");
    assert_eq!(text.matches("1.").count(), 1, "{text:?}");
    assert!(gutter_ink(&image).len() > 10, "empty item lost marker ink");
    let (_, text) = raster(
        "<ol><li>Alpha</li><li>Beta</li></ol>",
        "li{list-style-position:inside}",
    );
    assert_eq!(text.matches("1.").count(), 1, "{text:?}");
    assert_eq!(text.matches("2.").count(), 1, "{text:?}");
}

#[test]
fn unordered_disc_marker_is_painted_in_the_gutter() {
    let (image, text) = raster("<ul><li>Alpha</li></ul>", "");
    assert!(text.contains("Alpha"));
    assert!(text.contains('•'), "missing bullet text: {text:?}");
    assert!(gutter_ink(&image).len() > 10, "missing bullet ink");
}

#[test]
fn continued_item_does_not_repeat_its_number_on_later_pdf_pages() {
    let (_, text) = raster(
        "<ol><li>Alpha</li><li>Beta</li></ol>",
        "li:first-child{height:340px}",
    );
    let pages: Vec<_> = text.split('\u{c}').collect();
    assert_eq!(
        pages.len(),
        4,
        "expected three pages and trailing separator: {text:?}"
    );
    assert_eq!(pages[0].matches("1.").count(), 1);
    assert!(!pages[1].contains("1.") && !pages[1].contains("2."));
    assert_eq!(pages[2].matches("2.").count(), 1);
    assert!(!pages[2].contains("1."));
}
