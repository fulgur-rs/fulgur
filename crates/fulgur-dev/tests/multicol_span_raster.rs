#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster_pages(body: &str, css: &str) -> (Vec<image::RgbaImage>, String) {
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
        .args(["-png", "-r", "96"])
        .arg(&pdf)
        .arg(&prefix)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut paths = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "png"))
        .collect::<Vec<_>>();
    paths.sort();
    (
        paths
            .into_iter()
            .map(|path| image::open(path).unwrap().to_rgba8())
            .collect(),
        String::from_utf8(extracted.stdout).unwrap(),
    )
}

fn compare_pages(body: &str, css: &str, controls: &[&str], height: u32, letters: &str) {
    let css = format!(
        "@page{{size:180px {height}px}}.mc{{column-rule:2px solid red}}.span{{column-span:all}}{css}"
    );
    let (actual, text) = raster_pages(body, &css);
    let reference = controls
        .iter()
        .map(|page| format!("<div class=control>{page}</div>"))
        .collect::<String>();
    let reference_css = format!(
        "@page{{size:180px {height}px}}.control{{position:relative;height:{height}px}}.control+.control{{break-before:page}}"
    );
    let (expected, expected_text) = raster_pages(&reference, &reference_css);
    assert_eq!(actual.len(), controls.len());
    assert_eq!(expected.len(), controls.len());
    for letter in letters.chars() {
        assert_eq!(text.matches(letter).count(), 1, "column text {text:?}");
        assert_eq!(
            expected_text.matches(letter).count(),
            1,
            "control text {expected_text:?}"
        );
    }
    for (page, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual.dimensions(), (180, height));
        assert_eq!(actual.dimensions(), expected.dimensions());
        assert_eq!(
            actual
                .pixels()
                .zip(expected.pixels())
                .filter(|(a, b)| a != b)
                .count(),
            0,
            "page {}",
            page + 1
        );
    }
}

#[test]
fn fullwidth_pdf_heading_interrupts_rules_between_independent_column_groups() {
    compare_pages(
        "<div class=mc><p>A<br>B<br>C<br>D</p><div class=span>E</div><p>F<br>G<br>H<br>I</p></div><p>J</p>",
        "",
        &[
            "<div style='position:absolute;left:49px;top:0;width:2px;height:40px;background:red'></div><div style='position:absolute;left:49px;top:60px;width:2px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:0;top:40px'>E</div><div style='position:absolute;left:0;top:60px'>F<br>G</div><div style='position:absolute;left:60px;top:60px'>H<br>I</div><div style='position:absolute;left:0;top:100px'>J</div>",
        ],
        160,
        "ABCDEFGHIJ",
    );
}

#[test]
fn paginated_pdf_groups_keep_column_order_clips_rules_and_following_text() {
    compare_pages(
        "<div class=mc><p>A<br>B<br>C<br>D<br>E<br>F<br>G<br>H</p><div class=span>I</div><p>J<br>K</p></div><p>L</p>",
        "",
        &[
            "<div style='position:absolute;left:49px;top:0;width:2px;height:60px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B<br>C</div><div style='position:absolute;left:60px;top:0'>D<br>E<br>F</div>",
            "<div style='position:absolute;left:49px;top:0;width:2px;height:20px;background:red'></div><div style='position:absolute;left:49px;top:40px;width:2px;height:20px;background:red'></div><div style='position:absolute;left:0;top:0'>G</div><div style='position:absolute;left:60px;top:0'>H</div><div style='position:absolute;left:0;top:20px'>I</div><div style='position:absolute;left:0;top:40px'>J</div><div style='position:absolute;left:60px;top:40px'>K</div>",
            "<div style='position:absolute;left:0;top:0'>L</div>",
        ],
        60,
        "ABCDEFGHIJKL",
    );
}

#[test]
fn pdf_spanner_moves_to_a_page_before_the_final_group_resumes() {
    compare_pages(
        "<div class=mc><p>A<br>B<br>C<br>D</p><div class=span>E</div><p>F<br>G<br>H<br>I</p></div><p>J</p>",
        "",
        &[
            "<div style='position:absolute;left:49px;top:0;width:2px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
            "<div style='position:absolute;left:49px;top:20px;width:2px;height:30px;background:red'></div><div style='position:absolute;left:0;top:0'>E</div><div style='position:absolute;left:0;top:20px'>F</div><div style='position:absolute;left:60px;top:20px'>G</div>",
            "<div style='position:absolute;left:49px;top:0;width:2px;height:20px;background:red'></div><div style='position:absolute;left:0;top:0'>H</div><div style='position:absolute;left:60px;top:0'>I</div><div style='position:absolute;left:0;top:20px'>J</div>",
        ],
        50,
        "ABCDEFGHIJ",
    );
}

#[test]
fn nested_pdf_columns_inside_a_spanner_keep_wrapper_offsets_and_once_only_text() {
    compare_pages(
        "<div class=mc><p>A<br>B</p><div class=span><section><div class=inner><p>C<br>D</p></div></section></div><p>E<br>F</p></div><p>G</p>",
        "section{padding:10px 5px}.inner{column-count:2;column-gap:20px;column-rule:2px solid red}",
        &[
            "<div style='position:absolute;left:49px;top:0;width:2px;height:20px;background:red'></div><div style='position:absolute;left:49px;top:30px;width:2px;height:20px;background:red'></div><div style='position:absolute;left:49px;top:60px;width:2px;height:20px;background:red'></div><div style='position:absolute;left:0;top:0'>A</div><div style='position:absolute;left:60px;top:0'>B</div><div style='position:absolute;left:5px;top:30px'>C</div><div style='position:absolute;left:60px;top:30px'>D</div><div style='position:absolute;left:0;top:60px'>E</div><div style='position:absolute;left:60px;top:60px'>F</div><div style='position:absolute;left:0;top:80px'>G</div>",
        ],
        160,
        "ABCDEFG",
    );
}
