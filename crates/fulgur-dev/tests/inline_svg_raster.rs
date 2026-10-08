#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

fn render(body: &str, css: &str) -> (Vec<image::RgbaImage>, String, lopdf::Document) {
    render_with_fonts(body, css, &[])
}

fn render_with_fonts(
    body: &str,
    css: &str,
    additional_fonts: &[PathBuf],
) -> (Vec<image::RgbaImage>, String, lopdf::Document) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, format!("<style>@page{{size:200px 150px;margin:0}}body{{margin:0;background:white;font:12px/16px 'Noto Sans Mono'}}svg{{display:block}}.anchor{{position:fixed;left:130px;top:110px;color:black;font-size:12px}}{css}</style>{body}<p class=anchor>anchor</p>")).unwrap();
    let font =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fulgur-ruby/spec/fixtures/noto_sans.ttf");
    let mut command = Command::new(env!("CARGO_BIN_EXE_fulgur-dev"));
    command
        .arg("render")
        .args(["--engine", "raikiri", "--no-system-fonts", "--font"])
        .arg(font);
    for font in additional_fonts {
        command.arg("--font").arg(font);
    }
    let result = command.arg(input).arg("-o").arg(&output).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let extracted = Command::new("pdftotext")
        .arg(&output)
        .arg("-")
        .output()
        .unwrap();
    assert!(extracted.status.success());
    let text = String::from_utf8(extracted.stdout).unwrap();
    assert!(text.contains("anchor"), "missing body text: {text:?}");
    let pdf = lopdf::Document::load(&output).unwrap();
    let mut images = Vec::new();
    for page in 1..=pdf.get_pages().len() {
        let prefix = dir.path().join(format!("page-{page}"));
        let raster = Command::new("pdftocairo")
            .args(["-png", "-singlefile", "-r", "96", "-f"])
            .arg(page.to_string())
            .arg("-l")
            .arg(page.to_string())
            .arg(&output)
            .arg(&prefix)
            .output()
            .unwrap();
        assert!(
            raster.status.success(),
            "{}",
            String::from_utf8_lossy(&raster.stderr)
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
            "missing body glyph ink"
        );
        images.push(image);
    }
    (images, text, pdf)
}

fn rgb(image: &image::RgbaImage, x: u32, y: u32, expected: [u8; 3]) {
    let actual = image.get_pixel(x, y);
    for (actual, expected) in actual.0[..3].iter().zip(expected) {
        assert!(
            actual.abs_diff(expected) <= 4,
            "({x},{y}): {:?}, expected {expected:?}",
            image.get_pixel(x, y)
        );
    }
}

#[test]
fn inline_shapes_remain_vectors_with_original_colors() {
    let (images, _, pdf) = render(
        "<svg xmlns='http://www.w3.org/2000/svg' width='60' height='40'><rect width='30' height='20' fill='red'/><circle cx='45' cy='20' r='10' fill='blue'/></svg>",
        "",
    );
    rgb(&images[0], 4, 4, [255, 0, 0]);
    rgb(&images[0], 45, 20, [0, 0, 255]);
    rgb(&images[0], 45, 3, [255, 255, 255]);
    assert!(
        !pdf.objects.values().any(|value| value
            .as_stream()
            .ok()
            .and_then(|stream| stream.dict.get(b"Subtype").ok())
            .and_then(|value| value.as_name().ok())
            == Some(b"Image".as_slice())),
        "SVG unexpectedly became a raster image"
    );
}

#[test]
fn svg_viewbox_maps_paths_and_strokes_into_the_css_viewport() {
    let (images, _, _) = render(
        "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='50' viewBox='0 0 200 100'><path d='M0 50 L200 50' fill='none' stroke='green' stroke-width='10'/></svg>",
        "",
    );
    rgb(&images[0], 50, 25, [0, 128, 0]);
    rgb(&images[0], 50, 15, [255, 255, 255]);
}

#[test]
fn svg_gradients_retain_their_coordinate_mapping() {
    let (images, _, _) = render(
        "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='40'><defs><linearGradient id='g'><stop offset='0' stop-color='red'/><stop offset='1' stop-color='blue'/></linearGradient></defs><rect width='100' height='40' fill='url(#g)'/></svg>",
        "",
    );
    let left = images[0].get_pixel(5, 20);
    let right = images[0].get_pixel(95, 20);
    assert!(left[0] > 230 && left[2] < 25, "left {left:?}");
    assert!(right[2] > 230 && right[0] < 25, "right {right:?}");
}

#[test]
fn svg_text_uses_bundled_fonts_and_remains_extractable() {
    let (images, text, _) = render(
        "<svg xmlns='http://www.w3.org/2000/svg' width='100' height='40'><text x='4' y='25' font-size='16' font-family='Noto Sans Mono'>svgtext</text></svg>",
        "",
    );
    assert!(text.contains("svgtext"), "missing SVG text: {text:?}");
    assert!(
        (0..40)
            .flat_map(|y| (0..100).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = images[0].get_pixel(x, y);
                p[0] < 80 && p[1] < 80 && p[2] < 80
            })
            .count()
            > 50
    );
}

#[test]
fn svg_content_starts_after_border_and_padding() {
    let (images, _, _) = render(
        "<svg xmlns='http://www.w3.org/2000/svg' width='50' height='30'><rect width='50' height='30' fill='red'/></svg>",
        "svg{padding:10px;border:5px solid black}",
    );
    rgb(&images[0], 2, 2, [0, 0, 0]);
    rgb(&images[0], 10, 10, [255, 255, 255]);
    rgb(&images[0], 20, 20, [255, 0, 0]);
    rgb(&images[0], 70, 20, [255, 255, 255]);
}

#[test]
fn svg_inherited_opacity_and_host_box_composite_once() {
    let (images, _, _) = render(
        "<div><svg xmlns='http://www.w3.org/2000/svg' width='50' height='30'><rect width='50' height='30' fill='red' opacity='inherit'/></svg></div>",
        "div{opacity:.4}svg{opacity:.5;border:5px solid blue}",
    );
    rgb(&images[0], 2, 2, [204, 204, 255]);
    rgb(&images[0], 20, 20, [255, 230, 230]);
}

#[test]
fn svg_use_resolves_inherited_opacity_before_host_neutralization() {
    let (images, _, _) = render(
        "<div><svg xmlns='http://www.w3.org/2000/svg' xmlns:xlink='http://www.w3.org/1999/xlink' width='50' height='30'><defs><rect id='r' width='50' height='30' fill='red'/></defs><use xlink:href='#r' opacity='inherit'/></svg></div>",
        "div{opacity:.4}svg{opacity:.5}",
    );
    rgb(&images[0], 20, 20, [255, 230, 230]);
}

#[test]
fn svg_replaced_content_obeys_ancestor_overflow_clips() {
    let (images, _, _) = render(
        "<div><svg xmlns='http://www.w3.org/2000/svg' width='60' height='40'><rect width='60' height='40' fill='red'/></svg></div>",
        "div{width:40px;height:20px;overflow:hidden}",
    );
    rgb(&images[0], 10, 10, [255, 0, 0]);
    rgb(&images[0], 50, 10, [255, 255, 255]);
    rgb(&images[0], 10, 30, [255, 255, 255]);
}

#[test]
fn fixed_svg_repeats_at_its_negative_offset_on_every_page() {
    let (images, _, _) = render(
        "<div></div><svg xmlns='http://www.w3.org/2000/svg' width='20' height='10'><rect width='20' height='10' fill='red'/></svg>",
        "@page{size:200px 100px;margin:10px}.anchor{top:50px}div{height:180px}svg{position:fixed;top:-4px;left:-5px}",
    );
    assert!(images.len() > 1);
    for image in &images {
        rgb(image, 11, 11, [255, 0, 0]);
        rgb(image, 31, 11, [255, 255, 255]);
    }
}

#[test]
fn paginated_svg_uses_one_original_gradient_viewport() {
    let (images, _, _) = render(
        "<svg xmlns='http://www.w3.org/2000/svg' width='40' height='180'><defs><linearGradient id='g' x1='0' y1='0' x2='0' y2='1'><stop offset='0' stop-color='red'/><stop offset='1' stop-color='blue'/></linearGradient></defs><rect width='40' height='180' fill='url(#g)'/></svg>",
        "@page{size:200px 100px;margin:10px}.anchor{top:50px}",
    );
    assert_eq!(images.len(), 3);
    let first = images[0].get_pixel(20, 20);
    let second = images[1].get_pixel(20, 20);
    let third = images[2].get_pixel(20, 20);
    assert!(first[0] > 230 && first[2] < 25, "first {first:?}");
    assert!(
        second[0] > 115 && second[0] < 140 && second[2] > 115 && second[2] < 140,
        "second {second:?}"
    );
    assert!(third[2] > 230 && third[0] < 25, "third {third:?}");
}

#[test]
fn svg_default_root_opacity_does_not_repeat_parent_compositing() {
    let (images, _, _) = render(
        "<div><svg xmlns='http://www.w3.org/2000/svg' width='40' height='30'><rect width='40' height='30' fill='red' opacity='inherit'/></svg></div>",
        "div{opacity:.4}",
    );
    rgb(&images[0], 10, 10, [255, 153, 153]);
}

#[test]
fn svg_resolved_host_color_preserves_original_attribute_selectors() {
    for (color, sheet, svg_sheet, expected) in [
        ("inherit", "body{color:blue}", "", [0, 0, 255]),
        ("red", "svg{color:blue}", "", [0, 0, 255]),
        (
            "red",
            "svg{color:blue}",
            "<style>svg[color=red] rect{fill:green}</style>",
            [0, 128, 0],
        ),
    ] {
        let (images, _, _) = render(
            &format!(
                "<svg xmlns='http://www.w3.org/2000/svg' width='40' height='30' color='{color}'>{svg_sheet}<rect width='40' height='30' fill='currentColor'/></svg>"
            ),
            sheet,
        );
        rgb(&images[0], 10, 10, expected);
    }
}

#[test]
fn svg_layered_css_dimensions_override_presentation_attributes() {
    let (images, _, _) = render(
        "<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='100%' height='100%' fill='red'/></svg>",
        "@layer overrides{svg{width:30px;height:15px}}",
    );
    rgb(&images[0], 24, 10, [255, 0, 0]);
    rgb(&images[0], 34, 10, [255, 255, 255]);
    rgb(&images[0], 14, 18, [255, 255, 255]);
}

#[test]
fn hidden_svg_roots_draw_visible_children_without_host_box_ink() {
    for hidden in ["style='visibility:hidden'", "visibility='hidden'"] {
        let body = format!(
            "<svg width='20' height='10' {hidden}><rect width='10' height='10' fill='red'/><rect x='10' width='10' height='10' fill='blue' visibility='visible'/></svg>"
        );
        let (images, _, _) = render(
            &body,
            "svg{padding:3px;border:2px solid red;background:green}",
        );
        rgb(&images[0], 1, 1, [255, 255, 255]);
        rgb(&images[0], 10, 10, [255, 255, 255]);
        rgb(&images[0], 20, 10, [0, 0, 255]);
    }
}

#[test]
fn svg_relative_root_font_sizes_match_resolved_absolute_sizes() {
    let root = |attributes: &str| {
        format!("<svg width='180' height='90' {attributes}><text x='0' y='65'>TEST</text></svg>")
    };
    let expected = render(&root("style='font-size:24px'"), "").0;
    for attributes in [
        "style='font:2em Noto Sans Mono !important'",
        "style='font-size:2em'",
        "style='font-size:200%'",
        "style='font-size:2em !important'",
        "style='font:2em Noto Sans Mono'",
        "font-size='24'",
        "font-size='2em'",
    ] {
        let (images, text, _) = render(&root(attributes), "");
        assert!(text.contains("TEST"));
        assert!(images[0] == expected[0], "{attributes}");
    }
    for font in [
        "font-size:2em !important",
        "font:2em Noto Sans Mono !important",
    ] {
        let body = format!(
            "<svg width='180' height='90'><style>svg {{{font}}}</style><text x='0' y='65'>TEST</text></svg>"
        );
        let images = render(&body, "").0;
        assert!(images[0] == expected[0], "{font}");
    }
    let bold = render(&root("style='font:bold 24px Noto Sans Mono'"), "").0;
    let relative_bold = render(&root("style='font:bold 2em Noto Sans Mono !important'"), "").0;
    assert!(bold[0] == relative_bold[0], "bold font shorthand");
}

fn renamed_test_font() -> Vec<u8> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../fulgur-ruby/spec/fixtures/noto_sans.ttf");
    let mut data = std::fs::read(path).unwrap();
    let count = u16::from_be_bytes(data[4..6].try_into().unwrap()) as usize;
    let mut name_record = None;
    let mut head_offset = None;
    for record in (12..12 + count * 16).step_by(16) {
        let offset = u32::from_be_bytes(data[record + 8..record + 12].try_into().unwrap()) as usize;
        if &data[record..record + 4] == b"name" {
            let length =
                u32::from_be_bytes(data[record + 12..record + 16].try_into().unwrap()) as usize;
            name_record = Some((record, offset, length));
        } else if &data[record..record + 4] == b"head" {
            head_offset = Some(offset);
        }
    }
    let (record, offset, length) = name_record.unwrap();
    // A renamed face makes fallback selection observable without another binary fixture.
    for (old, new) in [
        ("Noto Sans Mono", "Test Font Mono"),
        ("NotoSansMono", "TestFontMono"),
    ] {
        for (old, new) in [
            (old.as_bytes().to_vec(), new.as_bytes().to_vec()),
            (
                old.encode_utf16().flat_map(u16::to_be_bytes).collect(),
                new.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            ),
        ] {
            assert_eq!(old.len(), new.len());
            let table = &mut data[offset..offset + length];
            for position in 0..=table.len() - old.len() {
                if table[position..position + old.len()] == old {
                    table[position..position + new.len()].copy_from_slice(&new);
                }
            }
        }
    }
    let checksum = |bytes: &[u8]| {
        bytes.chunks(4).fold(0u32, |sum, chunk| {
            let mut word = [0; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            sum.wrapping_add(u32::from_be_bytes(word))
        })
    };
    let name_checksum = checksum(&data[offset..offset + length]);
    data[record + 4..record + 8].copy_from_slice(&name_checksum.to_be_bytes());
    let adjustment = head_offset.unwrap() + 8;
    data[adjustment..adjustment + 4].fill(0);
    let total = 0xb1b0afbau32.wrapping_sub(checksum(&data));
    data[adjustment..adjustment + 4].copy_from_slice(&total.to_be_bytes());
    data
}

#[test]
fn svg_inherited_font_families_keep_missing_and_bundled_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let font = dir.path().join("fallback.ttf");
    std::fs::write(&font, renamed_test_font()).unwrap();
    let (_, text, pdf) = render_with_fonts(
        "<svg width='180' height='90'><text x='0' y='65'>TEST</text></svg>",
        "body{font-family:Missing,'Test Font Mono';font-size:24px}",
        &[font],
    );
    assert!(text.contains("TEST"));
    let names: Vec<_> = pdf
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok()?.get(b"BaseFont").ok()?.as_name().ok())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect();
    assert!(
        names.iter().any(|name| name.contains("TestFontMono")),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name.contains("NotoSansMono")),
        "{names:?}"
    );
}
