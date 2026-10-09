#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn raster(body: &str, css: &str) -> (image::RgbaImage, String) {
    let (mut pages, text) = raster_pages(body, css);
    assert_eq!(pages.len(), 1);
    (pages.remove(0), text)
}

fn compare(body: &str, css: &str, reference: &str, letters: &str) {
    let (actual, text) = raster(body, css);
    let (expected, reference_text) = raster(reference, "");
    compare_rasters(&actual, &text, &expected, &reference_text, letters);
}

fn compare_rasters(
    actual: &image::RgbaImage,
    text: &str,
    expected: &image::RgbaImage,
    reference_text: &str,
    letters: &str,
) {
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
fn two_column_rules_match_literal_pdf_geometry_and_once_only_text() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div><p>E</p>",
        ".mc{column-rule:2px solid red}",
        "<div style='position:absolute;left:49px;top:0;width:2px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:0;top:40px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn three_column_rules_match_independent_pdf_rectangles() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D<br>E<br>F</div>",
        ".mc{width:160px;column-count:3;column-rule:2px solid blue}",
        "<div style='position:absolute;left:49px;top:0;width:2px;height:40px;background:blue'></div><div style='position:absolute;left:109px;top:0;width:2px;height:40px;background:blue'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:120px;top:0'>E<br>F</div>",
        "ABCDEF",
    );
}

#[test]
fn rule_longhands_use_the_owner_currentcolor_in_pdf() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        "body{color:blue}.mc{column-rule-width:2px;column-rule-style:solid;column-rule-color:currentcolor}",
        "<style>body{color:blue}</style><div style='position:absolute;left:49px;top:0;width:2px;height:40px;background:blue'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
        "ABCD",
    );
}

#[test]
fn wide_rules_stay_below_pdf_column_content() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        ".mc{column-rule:40px solid red}",
        "<div style='position:absolute;left:30px;top:0;width:40px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
        "ABCD",
    );
}

#[test]
fn rule_shares_the_owners_pdf_opacity_group() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        ".mc{background:lime;opacity:.5;column-rule:2px solid red}",
        "<div style='position:absolute;left:0;top:0;width:100px;height:40px;background:lime;opacity:.5'><div style='position:absolute;left:49px;top:0;width:2px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div></div>",
        "ABCD",
    );
}

#[test]
fn wide_pdf_rule_obeys_the_owner_overflow_clip() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        ".mc{height:40px;overflow:hidden;column-rule:160px solid red}",
        "<div style='position:absolute;left:0;top:0;width:100px;height:40px;overflow:hidden'><div style='position:absolute;left:-30px;top:0;width:160px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div></div>",
        "ABCD",
    );
}

#[test]
fn pdf_rule_covers_the_full_height_of_a_short_last_column() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        ".mc{height:60px;column-rule:2px solid red}",
        "<div style='position:absolute;left:49px;top:0;width:2px;height:60px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B<br>C</div><div style='position:absolute;left:60px;top:0'>D</div>",
        "ABCD",
    );
}

#[test]
fn pdf_rule_is_omitted_for_an_empty_adjacent_column() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        ".mc{height:40px;width:160px;column-count:3;column-rule:2px solid red}",
        "<div style='position:absolute;left:49px;top:0;width:2px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
        "ABCD",
    );
}

#[test]
fn double_pdf_rule_matches_two_literal_bands() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        ".mc{column-rule:6px double red}",
        "<div style='position:absolute;left:47px;top:0;width:2px;height:40px;background:red'></div><div style='position:absolute;left:51px;top:0;width:2px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
        "ABCD",
    );
}

#[test]
fn dashed_pdf_rule_matches_literal_dashes() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        ".mc{height:42px;column-rule:2px dashed red}",
        "<div style='position:absolute;left:49px;top:0;width:2px;height:6px;background:red'></div><div style='position:absolute;left:49px;top:12px;width:2px;height:6px;background:red'></div><div style='position:absolute;left:49px;top:24px;width:2px;height:6px;background:red'></div><div style='position:absolute;left:49px;top:36px;width:2px;height:6px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
        "ABCD",
    );
}

#[test]
fn padded_pdf_rules_use_the_producer_content_origin() {
    compare(
        "<div class=mc>A<br>B<br>C<br>D</div><p>E</p>",
        ".mc{padding:4px 5px;border:1px solid black;background:lime;column-rule:2px solid red}",
        "<div style='position:absolute;left:0;top:0;box-sizing:border-box;width:112px;height:50px;border:1px solid black;background:lime'></div><div style='position:absolute;left:55px;top:5px;width:2px;height:40px;background:red'></div><div style='position:absolute;left:6px;top:5px'>A<br>B</div><div style='position:absolute;left:66px;top:5px'>C<br>D</div><div style='position:absolute;left:0;top:50px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn dotted_pdf_rules_match_literal_clipped_circles() {
    let (actual, text) = raster(
        "<div class=mc>A<br>B<br>C<br>D</div>",
        ".mc{column-rule:2px dotted red}",
    );
    let dots = literal_dots_pdf(
        2.0,
        (0..=10).map(|index| (50.0, (index * 4) as f32)),
        [49.0, 0.0, 2.0, 40.0],
        160.0,
    );
    let (expected, reference_text) = raster_pages_with_overlays(
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
        "",
        &[dots],
    );
    assert_eq!(expected.len(), 1);
    compare_rasters(&actual, &text, &expected[0], &reference_text, "ABCD");
}

#[test]
fn ridge_groove_inset_outset_pdf_rules_match_literal_shaded_bands() {
    for (style, left, right) in [
        ("ridge", "rgb(255,127,127)", "rgb(127,0,0)"),
        ("inset", "rgb(255,127,127)", "rgb(127,0,0)"),
        ("groove", "rgb(127,0,0)", "rgb(255,127,127)"),
        ("outset", "rgb(127,0,0)", "rgb(255,127,127)"),
    ] {
        compare(
            "<div class=mc>A<br>B<br>C<br>D</div>",
            &format!(".mc{{column-rule:6px {style} red}}"),
            &format!(
                "<div style='position:absolute;left:47px;top:0;width:3px;height:40px;background:{left}'></div><div style='position:absolute;left:50px;top:0;width:3px;height:40px;background:{right}'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>"
            ),
            "ABCD",
        );
    }
}

#[test]
fn fractional_pdf_rule_widths_use_snapped_producer_widths() {
    for (specified, width) in [(0.3, 1.0), (0.9, 1.0), (1.9, 1.0), (3.9, 3.0)] {
        compare(
            "<div class=mc>A<br>B<br>C<br>D</div>",
            &format!(".mc{{column-rule:{specified}px solid red}}"),
            &format!(
                "<div style='position:absolute;left:{}px;top:0;width:{width}px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div>",
                50.0 - width / 2.0
            ),
            "ABCD",
        );
    }
}

#[test]
fn outside_pdf_marker_paints_above_wide_rules_without_owner_clipping() {
    for overflow in ["", "overflow:hidden"] {
        compare(
            "<div class=mc>A<br>B<br>C<br>D</div>",
            &format!(
                ".mc{{display:list-item;list-style-position:outside;margin-left:40px;height:40px;column-rule:200px solid red;{overflow}}}.mc::marker{{content:'X';color:blue}}"
            ),
            &format!(
                "<style>.control::marker{{content:'X';color:blue}}</style><div style='position:absolute;left:40px;top:0;width:100px;height:40px'><div style='position:absolute;left:0;top:0;width:100px;height:40px;{overflow}'><div style='position:absolute;left:-50px;top:0;width:200px;height:40px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div></div><div class=control style='position:absolute;left:0;top:0;width:100px;height:40px;display:list-item;list-style-position:outside'></div></div>"
            ),
            "XABCD",
        );
    }
}

fn raster_pages(body: &str, css: &str) -> (Vec<image::RgbaImage>, String) {
    raster_pages_with_overlays(body, css, &[])
}

fn raster_pages_with_overlays(
    body: &str,
    css: &str,
    overlays: &[Vec<u8>],
) -> (Vec<image::RgbaImage>, String) {
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
    if !overlays.is_empty() {
        let mut document = lopdf::Document::load(&pdf).unwrap();
        let pages = document.get_pages();
        assert_eq!(pages.len(), overlays.len());
        for (page, overlay) in pages.values().zip(overlays) {
            document.add_page_contents(*page, overlay.clone()).unwrap();
        }
        document.save(&pdf).unwrap();
    }
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

#[test]
fn rule_page_slices_match_all_pdf_pages_and_preserve_every_letter_once() {
    let letters = ('A'..='Z')
        .map(|letter| letter.to_string())
        .collect::<Vec<_>>()
        .join("<br>");
    let (actual, text) = raster_pages(
        &format!("<div class=mc>{letters}</div>"),
        "@page{margin:20px}.mc{height:260px;column-rule:2px solid red}",
    );
    let reference = "<div style='height:120px;position:relative;break-after:page'><div style='position:absolute;left:49px;top:0;width:2px;height:120px;background:red'></div><div style='position:absolute;left:0;top:0'>A<br>B<br>C<br>D<br>E<br>F</div><div style='position:absolute;left:60px;top:0'>N<br>O<br>P<br>Q<br>R<br>S</div></div><div style='height:120px;position:relative;break-after:page'><div style='position:absolute;left:49px;top:0;width:2px;height:120px;background:red'></div><div style='position:absolute;left:0;top:0'>G<br>H<br>I<br>J<br>K<br>L</div><div style='position:absolute;left:60px;top:0'>T<br>U<br>V<br>W<br>X<br>Y</div></div><div style='height:20px;position:relative'><div style='position:absolute;left:49px;top:0;width:2px;height:20px;background:red'></div><div style='position:absolute;left:0;top:0'>M</div><div style='position:absolute;left:60px;top:0'>Z</div></div>";
    let (expected, reference_text) = raster_pages(reference, "@page{margin:20px}");
    assert_eq!(actual.len(), 3);
    assert_eq!(expected.len(), 3);
    for letter in 'A'..='Z' {
        assert_eq!(text.matches(letter).count(), 1);
        assert_eq!(reference_text.matches(letter).count(), 1);
    }
    for (actual, expected) in actual.iter().zip(&expected) {
        assert_eq!(actual.dimensions(), (180, 160));
        assert_eq!(
            actual
                .pixels()
                .zip(expected.pixels())
                .filter(|(a, b)| a != b)
                .count(),
            0
        );
    }
}

// Independent PDF circles avoid fragmenting the reference's tiny CSS boxes
// at the page boundary. All centers and clip rectangles come from literals.
fn literal_dots_pdf(
    diameter: f32,
    centers: impl Iterator<Item = (f32, f32)>,
    clip: [f32; 4],
    page_height: f32,
) -> Vec<u8> {
    let [x, y, width, height] = clip;
    let radius = diameter / 2.0;
    let handle = radius * 0.552_284_8;
    let mut content = format!(
        "q\n0.75 0 0 -0.75 0 {} cm\n{x} {y} {width} {height} re W n\n1 0 0 rg\n",
        page_height * 0.75
    );
    for (x, y) in centers {
        content.push_str(&format!(
            "{} {} m\n{} {} {} {} {} {} c\n{} {} {} {} {} {} c\n{} {} {} {} {} {} c\n{} {} {} {} {} {} c\nh f\n",
            x, y - radius,
            x + handle, y - radius, x + radius, y - handle, x + radius, y,
            x + radius, y + handle, x + handle, y + radius, x, y + radius,
            x - handle, y + radius, x - radius, y + handle, x - radius, y,
            x - radius, y - handle, x - handle, y - radius, x, y - radius,
        ));
    }
    content.push_str("Q\n");
    content.into_bytes()
}

fn compare_rule_pattern_pages(style: &str, height: i32, shapes: &str, dots: bool) {
    let letters = ('A'..='Z')
        .map(|letter| letter.to_string())
        .collect::<Vec<_>>()
        .join("<br>");
    let (actual, text) = raster_pages(
        &format!("<div class=mc>{letters}</div>"),
        &format!(
            "@page{{size:180px 150px;margin:20px}}.mc{{height:{height}px;column-rule:2px {style} red}}"
        ),
    );
    // The 110px page interval deliberately differs from the pattern periods.
    let reference = [(0, 110), (110, 110), (220, height - 220)]
        .into_iter()
        .map(|(offset, visible)| format!("<div style='position:relative;height:{visible}px;break-after:page'><div style='position:absolute;left:49px;top:0;width:2px;height:{visible}px;overflow:hidden'><div style='position:absolute;left:0;top:-{offset}px'>{shapes}</div></div></div>"))
        .collect::<String>();
    let overlays = if dots {
        [0, 110, 220]
            .into_iter()
            .map(|offset| {
                literal_dots_pdf(
                    2.0,
                    (0..=65).map(|index| (70.0, (20 + index * 4 - offset) as f32)),
                    [69.0, 20.0, 2.0, (height - offset).min(110) as f32],
                    150.0,
                )
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let (expected, _) =
        raster_pages_with_overlays(&reference, "@page{size:180px 150px;margin:20px}", &overlays);
    assert_eq!(actual.len(), 3);
    assert_eq!(expected.len(), 3);
    for letter in 'A'..='Z' {
        assert_eq!(text.matches(letter).count(), 1);
    }
    let red = |pixel: &image::Rgba<u8>| {
        pixel[0] > pixel[1].saturating_add(20) && pixel[0] > pixel[2].saturating_add(20)
    };
    for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual.dimensions(), (180, 150));
        let mask = |page: &image::RgbaImage| {
            page.enumerate_pixels()
                .filter_map(|(x, y, pixel)| red(pixel).then_some((x, y, pixel.0)))
                .collect::<Vec<_>>()
        };
        assert!(!mask(expected).is_empty());
        assert_eq!(mask(actual), mask(expected), "pattern page {}", index + 1);
    }
}

#[test]
fn dashed_pdf_rule_keeps_its_phase_on_every_page() {
    let dashes = (0..23).map(|index| format!("<div style='position:absolute;left:0;top:{}px;width:2px;height:6px;background:red'></div>", index * 12)).collect::<String>();
    compare_rule_pattern_pages("dashed", 270, &dashes, false);
}

#[test]
fn dotted_pdf_rule_keeps_its_phase_on_every_page() {
    compare_rule_pattern_pages("dotted", 260, "", true);
}
