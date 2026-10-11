#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn render(body: &str) -> (Vec<image::RgbaImage>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("page");
    std::fs::write(&input, format!("<!doctype html><style>@page{{size:100px 100px;margin:0}}body{{margin:0;background:white;font:10px/10px 'Noto Sans Mono';color:white}}table{{border-spacing:0}}th,td{{padding:0;width:40px;vertical-align:top}}th{{height:10px;font-weight:400;text-align:left}}td{{height:20px}}thead{{background:red}}tbody{{background:blue}}</style>{body}")).unwrap();
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
        .args(["-png", "-r", "96"])
        .arg(&pdf)
        .arg(&prefix)
        .output()
        .expect("pdftocairo is required; install poppler-utils");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("pdftotext")
        .args(["-layout"])
        .arg(&pdf)
        .arg("-")
        .output()
        .expect("pdftotext is required; install poppler-utils");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let pages: Vec<String> = text
        .strip_suffix('\x0c')
        .unwrap_or(&text)
        .split('\x0c')
        .map(str::to_owned)
        .collect();
    let images = (1..=pages.len())
        .map(|page| {
            image::open(dir.path().join(format!("page-{page}.png")))
                .unwrap()
                .to_rgba8()
        })
        .collect();
    (images, pages)
}

fn table(rows: usize, extra: &str) -> String {
    format!(
        "<table {extra}><thead><tr><th>H</th></tr></thead><tbody>{}</tbody></table>",
        "<tr><td>X</td></tr>".repeat(rows)
    )
}

fn assert_page(image: &image::RgbaImage, text: &str, rows: usize) {
    assert_eq!(image.dimensions(), (100, 100));
    assert_eq!(text.matches('H').count(), 1);
    assert_eq!(text.matches('X').count(), rows);
    // The CSS fixes the header and row bands independently of renderer geometry.
    for y in 0..100 {
        let color = if y < 10 {
            [255, 0, 0, 255]
        } else if y < 10 + rows * 20 {
            [0, 0, 255, 255]
        } else {
            [255, 255, 255, 255]
        };
        assert_eq!(image.get_pixel(35, y as u32).0, color, "y={y}");
        assert_eq!(image.get_pixel(50, y as u32).0, [255, 255, 255, 255]);
    }
    assert!(
        (0..10).any(|y| (0..10).any(|x| image.get_pixel(x, y).0 != [255, 0, 0, 255])),
        "header glyph must be painted"
    );
}

#[test]
fn every_table_pdf_page_has_a_complete_header_and_reserved_body_space() {
    let (images, pages) = render(&table(9, ""));
    assert_eq!(pages.len(), 3);
    for (page, rows) in [4, 4, 1].into_iter().enumerate() {
        assert_page(&images[page], &pages[page], rows);
    }
}

#[test]
fn initial_header_and_first_row_move_together_to_the_next_pdf_page() {
    for spacer in [85, 95] {
        let (images, pages) = render(&format!(
            "<div style='height:{spacer}px'></div>{}",
            table(5, "")
        ));
        assert_eq!(pages.len(), 3);
        assert!(!pages[0].contains('H'));
        assert!(
            images[0]
                .pixels()
                .all(|pixel| pixel.0 == [255, 255, 255, 255])
        );
        assert_page(&images[1], &pages[1], 4);
        assert_page(&images[2], &pages[2], 1);
    }
}

#[test]
fn a_single_page_pdf_table_has_one_header() {
    let (images, pages) = render(&table(2, ""));
    assert_eq!(pages.len(), 1);
    assert_page(&images[0], &pages[0], 2);
}

#[test]
fn repeated_pdf_header_decorations_keep_the_table_opacity() {
    let (images, pages) = render(&table(5, "style='opacity:0.5'"));
    assert_eq!(pages.len(), 2);
    for (page, rows) in [4, 1].into_iter().enumerate() {
        assert_eq!(pages[page].matches('H').count(), 1);
        assert_eq!(pages[page].matches('X').count(), rows);
        for y in 0..100 {
            let color = if y < 10 {
                [255, 128, 128, 255]
            } else if y < 10 + rows * 20 {
                [128, 128, 255, 255]
            } else {
                [255, 255, 255, 255]
            };
            let actual = images[page].get_pixel(35, y as u32).0;
            assert!(
                actual
                    .into_iter()
                    .zip(color)
                    .all(|(a, b)| a.abs_diff(b) <= 1),
                "page={page}, y={y}: {actual:?}"
            );
        }
    }
}

#[test]
fn repeated_pdf_header_overflow_clip_moves_with_its_contents() {
    let rows = "<tr><td></td></tr>".repeat(5);
    let (images, pages) = render(&format!(
        "<table><thead><tr><th style='overflow:hidden'><div style='height:10px;padding-top:5px;box-sizing:border-box'><div style='height:10px;background:lime'></div></div></th></tr></thead><tbody>{rows}</tbody></table>"
    ));
    assert_eq!(pages.len(), 2);
    for (page, rows) in [4, 1].into_iter().enumerate() {
        for y in 0..100 {
            let color = if y < 5 {
                [255, 0, 0, 255]
            } else if y < 10 {
                [0, 255, 0, 255]
            } else if y < 10 + rows * 20 {
                [0, 0, 255, 255]
            } else {
                [255, 255, 255, 255]
            };
            assert_eq!(
                images[page].get_pixel(35, y as u32).0,
                color,
                "page={page}, y={y}"
            );
        }
    }
}
