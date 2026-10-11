#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(span_columns: u32) -> image::RgbaImage {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    let width = 20 * span_columns;
    std::fs::write(&input, format!("<!doctype html><style>@page{{size:120px 100px;margin:0}}body{{margin:0;background:white;font:10px/20px 'Noto Sans Mono'}}table{{border-spacing:0}}td{{padding:0;width:20px;height:20px;color:transparent}}#s{{background:red;width:{width}px;height:40px}}#b{{background:blue}}#c{{background:lime}}#d{{background:yellow}}#e{{background:rgb(255,128,0)}}</style><table><tr><td id='s' rowspan='2' colspan='{span_columns}'>X</td><td id='b'>X</td></tr><tr><td id='c'>X</td></tr><tr><td id='d'>X</td><td id='e'>X</td></tr></table>")).unwrap();
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

fn assert_grid(span_columns: u32) {
    let image = raster(span_columns);
    assert_eq!(image.dimensions(), (120, 100));
    let following_column = 20 * span_columns + 10;
    // Fixed 20px columns and rows give an independent PDF coordinate oracle.
    assert_eq!(image.get_pixel(10, 10).0, [255, 0, 0, 255]);
    assert_eq!(image.get_pixel(10, 30).0, [255, 0, 0, 255]);
    assert_eq!(image.get_pixel(following_column, 10).0, [0, 0, 255, 255]);
    assert_eq!(image.get_pixel(following_column, 30).0, [0, 255, 0, 255]);
    assert_eq!(image.get_pixel(10, 50).0, [255, 255, 0, 255]);
    assert_eq!(image.get_pixel(30, 50).0, [255, 128, 0, 255]);
    assert_eq!(
        image.get_pixel(following_column + 20, 30).0,
        [255, 255, 255, 255]
    );
}

#[test]
fn a_rowspan_keeps_the_next_row_in_the_following_pdf_column() {
    assert_grid(1);
}

#[test]
fn a_rowspan_with_colspan_keeps_both_pdf_columns_reserved() {
    assert_grid(2);
}
