#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

const CSS: &str = "<style>@page {size:250px 130px;margin:0} body {margin:0;background:white;font:32px/50px 'Noto Sans Mono'} p {margin:0;padding-left:10px} .line {text-decoration:underline red} </style>";

fn raster(body: &str, css: &str) -> (image::RgbaImage, String) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let pdf = dir.path().join("output.pdf");
    let prefix = dir.path().join("raster");
    std::fs::write(&input, format!("{CSS}<style>{css}</style>{body}")).unwrap();
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
    assert!(extracted.status.success(), "pdftotext failed");
    let text = String::from_utf8(extracted.stdout).unwrap();
    let output = Command::new("pdftocairo")
        .args(["-png", "-singlefile", "-r", "96"])
        .arg(&pdf)
        .arg(&prefix)
        .output()
        .expect("pdftocairo is required for Linux raster tests");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        image::open(prefix.with_extension("png"))
            .unwrap()
            .to_rgba8(),
        text,
    )
}

fn red(p: &image::Rgba<u8>) -> bool {
    p[0] > 240 && p[1] < 180 && p[2] < 180
}

#[test]
fn all_styles_have_visible_geometry_and_real_glyphs() {
    for style in ["solid", "double", "dotted", "dashed", "wavy"] {
        let (image, text) = raster(
            "<p class='line'>MMMMMM</p>",
            &format!(
                ".line {{text-decoration-style:{style};{}}}",
                if style == "double" {
                    "font-size:64px;line-height:80px"
                } else {
                    ""
                }
            ),
        );
        assert_eq!(text.trim(), "MMMMMM");
        assert!(
            image
                .pixels()
                .filter(|p| p[0] < 80 && p[1] < 80 && p[2] < 80)
                .count()
                > 100,
            "{style}: missing glyphs"
        );
        let ys: Vec<_> = (0..120)
            .filter(|&y| (10..120).any(|x| red(image.get_pixel(x, y))))
            .collect();
        assert!(!ys.is_empty(), "{style}: missing decoration");
        let row = *ys
            .iter()
            .max_by_key(|&&y| (15..115).filter(|&x| red(image.get_pixel(x, y))).count())
            .unwrap();
        let hits = (15..115).filter(|&x| red(image.get_pixel(x, row))).count();
        if matches!(style, "dotted" | "dashed" | "wavy") {
            assert!(hits < 95, "{style}: missing gaps");
        } else {
            assert!(hits > 95, "{style}: discontinuous line");
        }
        if style == "double" {
            assert!(
                ys.windows(2).any(|y| y[1] > y[0] + 1),
                "double: no gap between bands"
            );
        }
        if style == "wavy" {
            assert!(
                ys.last().unwrap() - ys[0] >= 3,
                "wavy: no vertical amplitude"
            );
        }
    }
}

#[test]
fn decorations_are_composited_once_inside_opacity_group() {
    let (image, text) = raster(
        "<section style='opacity:.5'><p class='line'>MMMMMM</p></section>",
        "",
    );
    assert_eq!(text.trim(), "MMMMMM");
    let pixels: Vec<_> = image
        .pixels()
        .filter(|p| p[0] > 250 && p[1] < 150 && p[2] < 150)
        .collect();
    assert!(pixels.len() > 100, "missing half-transparent decoration");
    assert!(
        pixels.iter().all(|p| p[1] >= 126 && p[2] >= 126),
        "decoration opacity applied more than once"
    );
}

#[test]
fn decorations_respect_ancestor_clip() {
    let (image, _) = raster(
        "<div style='width:60px;overflow:hidden'><p class='line'>MMMMMM</p></div>",
        "",
    );
    assert!(image.enumerate_pixels().any(|(x, _, p)| x < 60 && red(p)));
    assert!(
        !image.enumerate_pixels().any(|(x, _, p)| x >= 60 && red(p)),
        "decoration outside ancestor clip"
    );
}

#[test]
fn split_runs_keep_dashed_dotted_and_wavy_phase() {
    for style in ["dotted", "dashed", "wavy"] {
        let css = format!(".line {{color:transparent;text-decoration-style:{style}}}");
        let (whole, _) = raster("<p class='line'>MMMMMM</p>", &css);
        let (split, _) = raster(
            "<p class='line'>MM<span style='color:rgba(0,0,255,0)'>MM</span>MM</p>",
            &css,
        );
        assert!(whole.pixels().any(red), "{style}: missing decoration");
        let diff = whole
            .pixels()
            .zip(split.pixels())
            .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 8))
            .count();
        assert!(
            diff <= 8,
            "{style}: {diff} differing pixels after run splitting"
        );
    }
}

#[test]
fn line_through_is_above_glyphs() {
    let (plain, text) = raster("<p>MMMMMM</p>", "");
    let (decorated, _) = raster("<p style='text-decoration:line-through red'>MMMMMM</p>", "");
    assert_eq!(text.trim(), "MMMMMM");
    assert!(
        plain
            .pixels()
            .zip(decorated.pixels())
            .filter(|(a, b)| a[0] < 80 && a[1] < 80 && a[2] < 80 && red(b))
            .count()
            > 20,
        "line-through does not cover glyph ink"
    );
}

#[test]
fn dotted_caps_survive_a_split_inside_a_dot() {
    let css =
        ".line {font-size:96px;line-height:120px;color:transparent;text-decoration-style:dotted}";
    let (whole, _) = raster("<p class='line'>MMMMMM</p>", css);
    let (split, _) = raster(
        "<p class='line'>MMM<span style='color:rgba(0,0,255,0)'>MMM</span></p>",
        css,
    );
    assert!(whole.pixels().any(red));
    for (x, y, p) in whole.enumerate_pixels() {
        assert_eq!(
            red(p),
            red(split.get_pixel(x, y)),
            "dotted split changes ({x},{y})"
        );
    }
}

#[test]
fn underlines_stay_below_neighboring_run_glyphs() {
    for generated in ["", "p::before {content:'X'}"] {
        let css = format!(
            ".line {{font-size:64px;line-height:120px;letter-spacing:-5px;text-underline-offset:-30px}} {generated}"
        );
        let (whole, _) = raster("<p class='line'>MMMMMM</p>", &css);
        let (split, _) = raster(
            "<p class='line'>MMM<span style='color:rgb(1,0,0)'>MMM</span></p>",
            &css,
        );
        assert!(whole.pixels().any(red));
        for (x, y, p) in whole.enumerate_pixels() {
            assert_eq!(
                red(p),
                red(split.get_pixel(x, y)),
                "underline phase changes ({x},{y}) with {generated}"
            );
        }
    }
}

#[test]
fn long_clipped_lines_keep_one_pattern_budget_after_splitting() {
    let text = "M".repeat(1000);
    let split_text = format!(
        "{}<span style='color:rgba(0,0,255,0)'>{}</span>",
        "M".repeat(500),
        "M".repeat(500)
    );
    for style in ["dotted", "dashed", "wavy"] {
        let css = format!(
            ".line {{color:transparent;width:250px;white-space:nowrap;overflow:hidden;text-decoration-style:{style}}}"
        );
        let (whole, _) = raster(&format!("<p class='line'>{text}</p>"), &css);
        let (split, _) = raster(&format!("<p class='line'>{split_text}</p>"), &css);
        assert!(whole.pixels().any(red));
        let changed = whole
            .pixels()
            .zip(split.pixels())
            .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 8))
            .count();
        assert!(
            changed <= 8,
            "{style}: {changed} visible pixels change when an offscreen run boundary moves"
        );
    }
}

#[test]
fn insets_do_not_open_gaps_at_color_run_boundaries() {
    for insets in ["4px", "-4px -6px"] {
        let css = format!(".line {{color:transparent;text-decoration-inset:{insets}}}");
        let (whole, _) = raster("<p class='line'>MMMMMM</p>", &css);
        let (split, _) = raster(
            "<p class='line'>MM<span style='color:rgba(0,0,255,0)'>MM</span>MM</p>",
            &css,
        );
        assert!(whole.pixels().any(red));
        for (x, y, p) in whole.enumerate_pixels() {
            assert_eq!(
                red(p),
                red(split.get_pixel(x, y)),
                "inset {insets} opens a gap at ({x},{y})"
            );
        }
    }
}
