#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(contents: [&str; 3]) -> image::RgbaImage {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    let rows: String = contents
        .into_iter()
        .zip(["red", "blue", "lime"])
        .map(|(content, color)| format!("<tr><td style='background:{color}'>{content}</td></tr>"))
        .collect();
    std::fs::write(&input, format!("<!doctype html><style>@page{{size:100px 100px;margin:0}}body{{margin:0;background:white;font:10px/20px 'Noto Sans Mono'}}table{{border-spacing:0}}td{{padding:0;width:20px;height:20px;color:transparent}}td>div{{width:20px;height:20px}}</style><table>{rows}</table>")).unwrap();
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
    image::open(prefix.with_extension("png"))
        .unwrap()
        .to_rgba8()
}

fn assert_rows(contents: [&str; 3]) {
    let image = raster(contents);
    assert_eq!(image.dimensions(), (100, 100));
    // Fixed 20px row heights define the expected PDF bands independently.
    assert_eq!(image.get_pixel(10, 10).0, [255, 0, 0, 255]);
    assert_eq!(image.get_pixel(10, 30).0, [0, 0, 255, 255]);
    assert_eq!(image.get_pixel(10, 50).0, [0, 255, 0, 255]);
    assert_eq!(image.get_pixel(10, 70).0, [255, 255, 255, 255]);
    assert_eq!(image.get_pixel(30, 30).0, [255, 255, 255, 255]);
}

#[test]
fn empty_pdf_cells_follow_their_rows_once() {
    assert_rows(["", "", ""]);
}

#[test]
fn block_content_pdf_cells_follow_their_rows_once() {
    assert_rows(["<div></div>", "<div></div>", "<div></div>"]);
}

#[test]
fn empty_and_text_pdf_cells_share_row_coordinates() {
    assert_rows(["", "X", ""]);
}
