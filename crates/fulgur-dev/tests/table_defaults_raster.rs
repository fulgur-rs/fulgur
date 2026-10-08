#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(css: &str, table_attrs: &str) -> (image::RgbaImage, String) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    std::fs::write(&input, format!("<!doctype html><style>@page{{size:240px 160px;margin:0}}body{{margin:0;background:white;font:16px/24px 'Noto Sans Mono'}}table{{width:200px}}th{{background:rgb(255,0,0)}}td{{background:rgb(0,0,255)}}{css}</style><table {table_attrs}><caption>C</caption><tr><th>H</th></tr><tr><td>A</td></tr></table>")).unwrap();
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
        .expect("pdftotext is required; install poppler-utils");
    assert!(extracted.status.success());
    let output = Command::new("pdftocairo")
        .args(["-png", "-singlefile", "-r", "96"])
        .arg(pdf)
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
        String::from_utf8(extracted.stdout).unwrap(),
    )
}

fn ink(image: &image::RgbaImage, left: u32, right: u32, top: u32, bottom: u32) -> usize {
    image
        .enumerate_pixels()
        .filter(|(x, y, p)| {
            *x >= left
                && *x < right
                && *y >= top
                && *y < bottom
                && p[0] < 100
                && p[1] < 100
                && p[2] < 100
        })
        .count()
}

#[test]
fn plain_table_centers_caption_and_header_and_pads_each_row() {
    let (image, text) = raster("", "");
    for label in ["C", "H", "A"] {
        assert_eq!(text.matches(label).count(), 1, "{text:?}");
    }
    // 24px caption, 2px spacing, then 24px lines with 1px top/bottom padding.
    assert_eq!(image.get_pixel(50, 26).0, [255, 0, 0, 255]);
    assert_eq!(image.get_pixel(50, 51).0, [255, 0, 0, 255]);
    assert_eq!(image.get_pixel(50, 52).0, [255, 255, 255, 255]);
    assert_eq!(image.get_pixel(50, 54).0, [0, 0, 255, 255]);
    assert_eq!(image.get_pixel(50, 79).0, [0, 0, 255, 255]);
    assert!(ink(&image, 90, 110, 0, 24) > 10, "caption is not centered");
    assert_eq!(
        ink(&image, 0, 20, 0, 24),
        0,
        "caption remained at the start edge"
    );
    assert!(ink(&image, 90, 110, 26, 52) > 10, "header is not centered");
    assert!(
        ink(&image, 3, 15, 54, 80) > 10,
        "cell text lost its padded position"
    );
}

#[test]
fn author_padding_and_caption_alignment_override_ua_defaults() {
    let (image, _) = raster(
        "caption{text-align:left}th,td{padding:4px}th{font-weight:400}",
        "",
    );
    assert!(
        ink(&image, 0, 20, 0, 24) > 10,
        "author caption alignment was lost"
    );
    // A 24px line with 4px padding has a 32px cell followed by the 2px gap.
    assert_eq!(image.get_pixel(50, 57).0, [255, 0, 0, 255]);
    assert_eq!(image.get_pixel(50, 58).0, [255, 255, 255, 255]);
    assert_eq!(image.get_pixel(50, 60).0, [0, 0, 255, 255]);
}

#[test]
fn zero_cell_padding_hint_removes_the_ua_padding_in_pdf_boxes() {
    let (image, _) = raster("", "cellpadding='0'");
    // Attribute padding is zero: each line occupies 24px, with a 2px row gap.
    assert_eq!(image.get_pixel(50, 49).0, [255, 0, 0, 255]);
    assert_eq!(image.get_pixel(50, 50).0, [255, 255, 255, 255]);
    assert_eq!(image.get_pixel(50, 52).0, [0, 0, 255, 255]);
    assert_eq!(image.get_pixel(50, 75).0, [0, 0, 255, 255]);
}
