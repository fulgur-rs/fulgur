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

#[test]
fn a_fixed_height_column_container_splits_a_nested_plain_block_in_pdf() {
    compare(
        "<div class=mc><div><p>A<br>B<br>C<br>D</p></div></div><p>E</p>",
        ".mc{height:40px}",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:0;top:40px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn a_fixed_height_plain_chain_uses_three_columns_below_a_prefix_in_pdf() {
    compare(
        "<p>X</p><div class=mc><div><div><p>A<br>B<br>C<br>D<br>E<br>F</p></div></div></div><p>G</p>",
        ".mc{width:160px;height:40px;column-count:3}",
        "<div style='position:absolute;left:0;top:0'>X</div><div style='position:absolute;left:0;top:20px'>A<br>B</div><div style='position:absolute;left:60px;top:20px'>C<br>D</div><div style='position:absolute;left:120px;top:20px'>E<br>F</div><div style='position:absolute;left:0;top:60px'>G</div>",
        "XABCDEFG",
    );
}

#[test]
fn a_fixed_height_plain_wrapper_respects_the_parent_content_origin_in_pdf() {
    compare(
        "<div class=mc><div><p>A<br>B<br>C<br>D</p></div></div><p>E</p>",
        ".mc{height:40px;padding:4px 5px;border:1px solid black;background:lime}",
        "<div style='position:absolute;left:0;top:0;box-sizing:border-box;width:112px;height:50px;border:1px solid black;background:lime'></div><div style='position:absolute;left:6px;top:5px'>A<br>B</div><div style='position:absolute;left:66px;top:5px'>C<br>D</div><div style='position:absolute;left:0;top:50px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn a_fixed_height_border_box_wrapper_uses_the_inner_column_width_in_pdf() {
    compare(
        "<div class=mc><div><p>A<br>B<br>C<br>D</p></div></div><p>E</p>",
        ".mc{box-sizing:border-box;height:50px;padding:4px 5px;border:1px solid black;background:lime}",
        "<div style='position:absolute;left:0;top:0;box-sizing:border-box;width:100px;height:50px;border:1px solid black;background:lime'></div><div style='position:absolute;left:6px;top:5px'>A<br>B</div><div style='position:absolute;left:60px;top:5px'>C<br>D</div><div style='position:absolute;left:0;top:50px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn a_deep_plain_wrapper_keeps_column_clips_below_a_prefix_in_pdf() {
    compare(
        "<p>X</p><div class=mc><div><div><p>A<br>B<br>C<br>D<br>E<br>F</p></div></div></div><p>G</p>",
        ".mc{width:160px;height:40px;column-count:3;padding:4px 5px;border:1px solid black;background:lime}.mc p{overflow:hidden}",
        "<div style='position:absolute;left:0;top:0'>X</div><div style='position:absolute;left:0;top:20px;box-sizing:border-box;width:172px;height:50px;border:1px solid black;background:lime'></div><div style='position:absolute;left:6px;top:25px'>A<br>B</div><div style='position:absolute;left:66px;top:25px'>C<br>D</div><div style='position:absolute;left:126px;top:25px'>E<br>F</div><div style='position:absolute;left:0;top:70px'>G</div>",
        "XABCDEFG",
    );
}

#[test]
fn constrained_wrapper_width_preserves_its_literal_pdf_lines() {
    compare(
        "<div class=mc><div style='width:80px'><p>A A A A A</p></div></div><p>E</p>",
        ".mc{height:40px}",
        "<div style='position:absolute;left:0;top:0'>A A A<br>A A</div><div style='position:absolute;left:0;top:40px'>E</div>",
        "E",
    );
}

#[test]
fn constrained_wrapper_margin_continues_into_visible_overflow_columns_in_pdf() {
    compare(
        "<div class=mc><div style='overflow:hidden'><p style='margin-top:20px'>A<br>B<br>C<br>D</p></div></div><p>E</p>",
        ".mc{height:40px}",
        "<div style='position:absolute;left:0;top:20px'>A</div><div style='position:absolute;left:60px;top:0'>B<br>C</div><div style='position:absolute;left:120px;top:0'>D</div><div style='position:absolute;left:0;top:40px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn deep_wrapper_clips_wide_pdf_lines_at_each_column() {
    compare(
        "<p>@</p><div class=mc><div><div><p>ABCDEFGHIJKL<br>mnopqrstuvwx<br>0123456789<br>abcdefghijk<br>QRSTUVWXYZ<br>ABCDEFGHIJK</p></div></div></div><p>#</p>",
        ".mc{width:160px;height:40px;column-count:3;padding:4px 5px;border:1px solid black;background:lime}.mc p{white-space:pre;overflow:hidden}",
        "<div style='position:absolute;left:0;top:0'>@</div><div style='position:absolute;left:0;top:20px;box-sizing:border-box;width:172px;height:50px;border:1px solid black;background:lime'></div><div style='position:absolute;left:6px;top:25px;width:40px;height:40px;white-space:pre;overflow:hidden'>ABCDEFGHIJKL<br>mnopqrstuvwx</div><div style='position:absolute;left:66px;top:25px;width:40px;height:40px;white-space:pre;overflow:hidden'>0123456789<br>abcdefghijk</div><div style='position:absolute;left:126px;top:25px;width:40px;height:40px;white-space:pre;overflow:hidden'>QRSTUVWXYZ<br>ABCDEFGHIJK</div><div style='position:absolute;left:0;top:70px'>#</div>",
        "@#",
    );
}

#[test]
fn multiple_paragraphs_balance_through_a_paragraph_break_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D</p><p>E<br>F</p></div><p>G</p>",
        "",
        "<div style='position:absolute;left:0;top:0'>A<br>B<br>C</div><div style='position:absolute;left:60px;top:0'>D<br>E<br>F</div><div style='position:absolute;left:0;top:60px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn multiple_paragraphs_fill_three_columns_in_one_group_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D</p><p>E<br>F</p></div><p>G</p>",
        ".mc{width:160px;column-count:3}",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:120px;top:0'>E<br>F</div><div style='position:absolute;left:0;top:40px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn multiple_paragraphs_balance_with_two_orphans_and_widows_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D<br>E<br>F</p><p>G<br>H</p></div><p>I</p>",
        ".mc{orphans:2;widows:2}",
        "<div style='position:absolute;left:0;top:0'>A<br>B<br>C<br>D</div><div style='position:absolute;left:60px;top:0'>E<br>F<br>G<br>H</div><div style='position:absolute;left:0;top:80px'>I</div>",
        "ABCDEFGHI",
    );
}

#[test]
fn paragraph_break_minima_override_the_column_container_in_pdf() {
    compare(
        "<div class=mc><p style='orphans:3;widows:3'>A<br>B<br>C<br>D<br>E<br>F</p><p>G<br>H</p></div><p>I</p>",
        "",
        "<div style='position:absolute;left:0;top:0'>A<br>B<br>C</div><div style='position:absolute;left:60px;top:0'>D<br>E<br>F<br>G<br>H</div><div style='position:absolute;left:0;top:100px'>I</div>",
        "ABCDEFGHI",
    );
}

#[test]
fn paragraph_bottom_margins_participate_in_balancing_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D</p><p>E<br>F</p></div><p>G</p>",
        ".mc p{margin-bottom:8px}",
        "<div style='position:absolute;left:0;top:0'>A<br>B<br>C</div><div style='position:absolute;left:60px;top:0'>D</div><div style='position:absolute;left:60px;top:28px'>E<br>F</div><div style='position:absolute;left:0;top:76px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn adjoining_paragraph_margins_collapse_within_a_column_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B</p><p>C<br>D</p><p>E<br>F</p></div><p>G</p>",
        ".mc p{margin-bottom:8px}.mc p+p{margin-top:12px}",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:0;top:52px'>C</div><div style='position:absolute;left:60px;top:0'>D</div><div style='position:absolute;left:60px;top:32px'>E<br>F</div><div style='position:absolute;left:0;top:80px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn source_whitespace_and_parent_insets_preserve_group_continuations_in_pdf() {
    compare(
        "<p>@</p><div class=mc>\n <p>A<br>B<br>C<br>D</p>\n <p>E<br>F</p>\n</div><p>G</p>",
        ".mc{padding:4px 5px;border:1px solid black}",
        "<div style='position:absolute;left:0;top:0'>@</div><div style='position:absolute;left:0;top:20px;box-sizing:border-box;width:112px;height:70px;border:1px solid black'></div><div style='position:absolute;left:6px;top:25px'>A<br>B<br>C</div><div style='position:absolute;left:66px;top:25px'>D<br>E<br>F</div><div style='position:absolute;left:0;top:90px'>G</div>",
        "@ABCDEFG",
    );
}

#[test]
fn short_paragraphs_move_whole_and_truncate_adjoining_break_margins_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B</p><p>C<br>D</p><p>E<br>F</p></div><p>G</p>",
        ".mc{orphans:3;widows:3}.mc p{margin-bottom:8px}.mc p+p{margin-top:12px}",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:0;top:52px'>C<br>D</div><div style='position:absolute;left:60px;top:0'>E<br>F</div><div style='position:absolute;left:0;top:92px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn mid_column_paragraph_continuations_keep_wide_pdf_line_clips() {
    compare(
        "<div class=mc><p>ABCDEFGH<br>IJKLMNOP</p><p>QRSTUVWX<br>abcdefgh</p><p>ijklmnop<br>qrstuvwx</p></div><p>Y</p>",
        ".mc p{white-space:pre;overflow:hidden;margin-bottom:8px}.mc p+p{margin-top:12px}",
        "<div style='position:absolute;left:0;top:0;width:40px;height:40px;white-space:pre;overflow:hidden'>ABCDEFGH<br>IJKLMNOP</div><div style='position:absolute;left:0;top:52px;width:40px;height:28px;white-space:pre;overflow:hidden'>QRSTUVWX</div><div style='position:absolute;left:60px;top:0;width:40px;height:20px;white-space:pre;overflow:hidden'>abcdefgh</div><div style='position:absolute;left:60px;top:32px;width:40px;height:40px;white-space:pre;overflow:hidden'>ijklmnop<br>qrstuvwx</div><div style='position:absolute;left:0;top:80px'>Y</div>",
        "ABCDEFGHIJKLMNOPQRSTUVWXYabcdefghijklmnopqrstuvwx",
    );
}

#[test]
fn an_empty_paragraph_does_not_disable_group_balancing_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B<br>C<br>D</p><p></p><p>E<br>F</p></div><p>G</p>",
        "",
        "<div style='position:absolute;left:0;top:0'>A<br>B<br>C</div><div style='position:absolute;left:60px;top:0'>D<br>E<br>F</div><div style='position:absolute;left:0;top:60px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn a_leading_margin_cannot_skip_an_empty_column_in_pdf() {
    compare(
        "<div class=mc><p style='margin-top:100px'>A</p><p>B</p></div><p>C</p>",
        "",
        "<div style='position:absolute;left:0;top:100px'>A</div><div style='position:absolute;left:60px;top:0'>B</div><div style='position:absolute;left:0;top:120px'>C</div>",
        "ABC",
    );
}

#[test]
fn an_inline_background_moves_with_its_continuation_glyphs_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B</p><p><span style='background:lime'>C<br>D</span></p><p>E<br>F</p></div><p>G</p>",
        ".mc p{margin-bottom:8px}.mc p+p{margin-top:12px}",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:0;top:52px'><span style='background:lime'>C</span></div><div style='position:absolute;left:60px;top:0'><span style='background:lime'>D</span></div><div style='position:absolute;left:60px;top:32px'>E<br>F</div><div style='position:absolute;left:0;top:80px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn an_unfinished_paragraph_background_fills_its_column_extent_in_pdf() {
    compare(
        "<div class=mc><p style='background:lime;orphans:3;widows:3'>A<br>B<br>C<br>D<br>E<br>F</p><p>G<br>H</p></div><p>I</p>",
        "body{color:transparent}",
        "<div style='position:absolute;left:0;top:0;width:40px;height:100px;background:lime'></div><div style='position:absolute;left:60px;top:0;width:40px;height:60px;background:lime'></div>",
        "",
    );
}

#[test]
fn empty_paragraph_margin_chains_keep_both_signed_extrema_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B</p><p class=empty></p><p class=later>C<br>D</p><p class=last>E<br>F</p></div><p>G</p>",
        ".mc p{margin-bottom:8px}.mc .empty{margin-top:12px;margin-bottom:-4px}.mc .later{margin-top:20px}.mc .last{margin-top:12px}",
        "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:0;top:56px'>C</div><div style='position:absolute;left:60px;top:0'>D</div><div style='position:absolute;left:60px;top:32px'>E<br>F</div><div style='position:absolute;left:0;top:80px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn a_subpixel_noto_line_box_keeps_each_source_and_following_flow_in_pdf() {
    compare(
        "<div class=mc><p class=zero>A</p><p>B</p></div><p>C</p>",
        ".mc .zero{line-height:0}",
        "<div style='position:absolute;left:0;top:0;line-height:0'>A</div><div style='position:absolute;left:60px;top:0'>B</div><div style='position:absolute;left:0;top:20px'>C</div>",
        "ABC",
    );
}

#[test]
fn a_nonfinal_background_fills_only_the_remaining_mid_column_extent_in_pdf() {
    compare(
        "<div class=mc><p>A<br>B</p><p style='background:lime'>C<br>D</p><p>E<br>F</p></div><p>G</p>",
        "body{color:transparent}.mc p{margin-bottom:8px}.mc p+p{margin-top:12px}",
        "<div style='position:absolute;left:0;top:52px;width:40px;height:28px;background:lime'></div><div style='position:absolute;left:60px;top:0;width:40px;height:20px;background:lime'></div>",
        "",
    );
}

#[test]
fn one_nonempty_paragraph_balances_beside_empty_blocks_in_pdf() {
    for body in [
        "<div class=mc><p>A<br>B<br>C<br>D</p><p></p></div><p>E</p>",
        "<div class=mc><p></p><p>A<br>B<br>C<br>D</p></div><p>E</p>",
        "<div class=mc><p>A<br>B<br>C<br>D</p><p> </p></div><p>E</p>",
        "<div class=mc><p> </p><p>A<br>B<br>C<br>D</p></div><p>E</p>",
    ] {
        compare(
            body,
            "",
            "<div style='position:absolute;left:0;top:0'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:0;top:40px'>E</div>",
            "ABCDE",
        );
    }
}

#[test]
fn an_anonymous_table_cell_keeps_ordinary_pdf_text() {
    compare(
        "<div style='display:table;border-spacing:0'>A</div>",
        "",
        "<div style='position:absolute;left:0;top:0'>A</div>",
        "A",
    );
}

#[test]
fn constrained_wrapper_authored_widths_keep_wide_lines_in_each_pdf_column() {
    for width in [
        "width:80px",
        "min-width:80px",
        "width:200%",
        "width:calc(200%)",
    ] {
        compare(
            &format!(
                "<div class=mc><div style='{width}'><p>ABCD<br>EFGH<br>IJKL<br>MNOP</p></div></div><p>Q</p>"
            ),
            ".mc{height:40px}",
            "<div style='position:absolute;left:0;top:0;width:80px'>ABCD<br>EFGH</div><div style='position:absolute;left:60px;top:0;width:80px'>IJKL<br>MNOP</div><div style='position:absolute;left:0;top:40px'>Q</div>",
            "ABCDEFGHIJKLMNOPQ",
        );
    }
}

#[test]
fn constrained_wrapper_margin_consumes_only_the_first_pdf_column_budget() {
    compare(
        "<div class=mc><div style='overflow:hidden'><p style='margin-top:20px'>A<br>B<br>C<br>D</p></div></div><p>E</p>",
        ".mc{height:60px}",
        "<div style='position:absolute;left:0;top:20px'>A<br>B</div><div style='position:absolute;left:60px;top:0'>C<br>D</div><div style='position:absolute;left:0;top:60px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn constrained_wrapper_auto_margins_center_in_each_pdf_column() {
    compare(
        "<div class=mc><div style='width:20px;margin-left:auto;margin-right:auto'><p>A<br>B<br>C<br>D</p></div></div><p>E</p>",
        ".mc{height:40px}",
        "<div style='position:absolute;left:10px;top:0'>A<br>B</div><div style='position:absolute;left:70px;top:0'>C<br>D</div><div style='position:absolute;left:0;top:40px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn constrained_deep_wrapper_keeps_parent_insets_and_clips_in_pdf() {
    compare(
        "<p>X</p><div class=mc><div style='width:80px;overflow:hidden'><div><p style='margin-top:20px'>A<br>B<br>C<br>D</p></div></div></div><p>E</p>",
        ".mc{height:40px;width:160px;column-count:3;padding:4px 5px;border:1px solid black;background:lime}",
        "<div style='position:absolute;left:0;top:0'>X</div><div style='position:absolute;left:0;top:20px;box-sizing:border-box;width:172px;height:50px;border:1px solid black;background:lime'></div><div style='position:absolute;left:6px;top:45px'>A</div><div style='position:absolute;left:66px;top:25px'>B<br>C</div><div style='position:absolute;left:126px;top:25px'>D</div><div style='position:absolute;left:0;top:70px'>E</div>",
        "XABCDE",
    );
}

#[test]
fn constrained_wrapper_refills_middle_orphans_for_final_widows_in_pdf() {
    compare(
        "<div class=mc><div style='width:80px'><p style='orphans:4;widows:4'>A<br>B<br>C<br>D<br>E<br>F<br>G<br>H<br>I<br>J<br>K<br>L</p></div></div><p>M</p>",
        ".mc{height:100px;width:160px;column-count:3}",
        "<div style='position:absolute;left:0;top:0'>A<br>B<br>C<br>D</div><div style='position:absolute;left:60px;top:0'>E<br>F<br>G<br>H</div><div style='position:absolute;left:120px;top:0'>I<br>J<br>K<br>L</div><div style='position:absolute;left:0;top:100px'>M</div>",
        "ABCDEFGHIJKLM",
    );
}

#[test]
fn constrained_wrapper_preserves_visible_unbreakable_pdf_line_overflow() {
    compare(
        "<div class=mc><div style='width:20px'><p style='white-space:nowrap'>ABCDEF</p></div></div><p>G</p>",
        ".mc{height:60px}",
        "<div style='position:absolute;left:0;top:0;white-space:nowrap'>ABCDEF</div><div style='position:absolute;left:0;top:60px'>G</div>",
        "ABCDEFG",
    );
}

#[test]
fn constrained_wrapper_final_background_keeps_its_child_bottom_margin_in_pdf() {
    compare(
        "<div class=mc><div style='width:80px;overflow:hidden;background:lime'><p style='margin-bottom:20px'>A</p></div></div><p>B</p>",
        ".mc{height:60px}",
        "<div style='position:absolute;left:0;top:0;width:80px;height:40px;background:lime'>A</div><div style='position:absolute;left:0;top:60px'>B</div>",
        "AB",
    );
}

#[test]
fn constrained_wrapper_inline_background_moves_inside_explicit_pdf_clips() {
    // Noto Sans inline boxes extend beyond a 20px line. Match explicit overflow
    // clips in both fixtures; visible ink overflow is tracked in 9vjb.39.1.12.
    compare(
        "<div class=mc><div style='width:80px;overflow:hidden'><p><span style='background:lime'>A<br>B<br>C<br>D</span></p></div></div><p>E</p>",
        ".mc{height:40px}",
        "<div style='position:absolute;left:0;top:0;width:80px;height:40px;overflow:hidden'><span style='background:lime'>A<br>B</span></div><div style='position:absolute;left:60px;top:0;width:80px;height:40px;overflow:hidden'><span style='background:lime'>C<br>D</span></div><div style='position:absolute;left:0;top:40px'>E</div>",
        "ABCDE",
    );
}

#[test]
fn constrained_wrapper_retains_all_pdf_lines_when_minima_cannot_fit() {
    compare(
        "<div class=mc><div style='width:80px'><p style='orphans:4;widows:5'>A<br>B<br>C<br>D<br>E<br>F<br>G<br>H<br>I<br>J<br>K<br>L</p></div></div><p>M</p>",
        ".mc{height:100px;width:160px;column-count:3}",
        "<div style='position:absolute;left:0;top:0'>A<br>B<br>C<br>D</div><div style='position:absolute;left:60px;top:0'>E<br>F<br>G</div><div style='position:absolute;left:120px;top:0'>H<br>I<br>J<br>K<br>L</div><div style='position:absolute;left:0;top:100px'>M</div>",
        "ABCDEFGHIJKLM",
    );
}

#[test]
fn constrained_wrapper_variable_height_lines_keep_feasible_pdf_break_minima() {
    compare(
        "<div class=mc><div style='width:80px'><p style='orphans:1;widows:3'><span style='line-height:20px'>A</span><br><span style='line-height:5px'>B</span><br><span style='line-height:10px'>C</span><br><span style='line-height:15px'>D</span><br><span style='line-height:20px'>E</span><br><span style='line-height:10px'>F</span><br><span style='line-height:5px'>G</span><br><span style='line-height:5px'>H</span></p></div></div><p>I</p>",
        "body{font:5px/5px 'Noto Sans'}.mc{height:40px;width:160px;column-count:3}",
        "<style>body{font:5px/5px 'Noto Sans'}</style><div style='position:absolute;left:0;top:0'><span style='line-height:20px'>A</span></div><div style='position:absolute;left:60px;top:0'><span style='line-height:5px'>B</span><br><span style='line-height:10px'>C</span><br><span style='line-height:15px'>D</span></div><div style='position:absolute;left:120px;top:0'><span style='line-height:20px'>E</span><br><span style='line-height:10px'>F</span><br><span style='line-height:5px'>G</span><br><span style='line-height:5px'>H</span></div><div style='position:absolute;left:0;top:40px'>I</div>",
        "ABCDEFGHI",
    );
}
