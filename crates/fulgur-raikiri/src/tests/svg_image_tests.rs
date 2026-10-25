//! SVG used as an image (`<img>`, `background-image`, `list-style-image` and
//! `content: url()`): drawn as vector content unless the element, the
//! options or the painter ask for pixels.

use super::*;
use crate::SvgRendering;

const RED_RECT: &str = "<svg xmlns='http://www.w3.org/2000/svg' width='40' height='20' viewBox='0 0 40 20'><rect width='40' height='20' fill='red'/></svg>";

/// 1x1 PNG, to compare with the SVG.
const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// The percent-encoded `data:` URL of an SVG document.
fn svg_url(svg: &str) -> String {
    let mut encoded = String::from("data:image/svg+xml,");
    for byte in svg.bytes() {
        match byte {
            b'<' | b'>' | b'#' | b' ' | b'"' => encoded.push_str(&format!("%{byte:02X}")),
            _ => encoded.push(char::from(byte)),
        }
    }
    encoded
}

const PAGE: &str =
    "<style>@page{size:200px 200px;margin:0}body{margin:0}img{display:block}</style>";

/// The pixel widths of the image XObjects of `pdf`, soft masks included.
fn image_widths(pdf: &lopdf::Document) -> Vec<i64> {
    let mut widths: Vec<i64> = pdf
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter(|stream| {
            stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|value| value.as_name().ok())
                == Some(b"Image".as_slice())
        })
        .filter_map(|stream| stream.dict.get(b"Width").ok()?.as_i64().ok())
        .collect();
    widths.sort_unstable();
    widths
}

/// The number of fills in the drawing operations.
fn fills(operations: &[lopdf::content::Operation]) -> usize {
    count(operations, "f") + count(operations, "f*")
}

fn render(body: &str, css: &str) -> (lopdf::Document, Vec<lopdf::content::Operation>) {
    operations(&format!("{PAGE}<style>{css}</style>{body}"))
}

#[test]
fn an_svg_img_is_vector_content() {
    let (pdf, operations) = render(
        &format!("<img src=\"{}\" width=40 height=20>", svg_url(RED_RECT)),
        "",
    );
    assert!(image_widths(&pdf).is_empty(), "no pixels were drawn");
    assert!(fills(&operations) > 0, "the rectangle is a filled path");
}

#[test]
fn a_raster_img_is_still_pixels() {
    let (pdf, operations) = render(&format!("<img src=\"{PNG}\" width=40 height=20>"), "");
    assert!(!image_widths(&pdf).is_empty());
    assert_eq!(fills(&operations), 0);
}

#[test]
fn an_element_can_ask_for_pixels() {
    let (pdf, _) = render(
        &format!(
            "<img src=\"{}\" width=40 height=20 style='--raikiri-svg-rendering: raster'>",
            svg_url(RED_RECT)
        ),
        "",
    );
    // The picture of the 40 x 20 box, and its alpha channel.
    assert_eq!(image_widths(&pdf), vec![40]);
}

#[test]
fn the_options_decide_for_an_element_that_says_nothing() {
    let body = format!("<img src=\"{}\" width=40 height=20>", svg_url(RED_RECT));
    let options = RenderOptions {
        svg_rendering: SvgRendering::Raster,
        svg_raster_scale: 3.0,
        ..RenderOptions::default()
    };
    let (_dir, path) = input(&format!("{PAGE}{body}"));
    let bytes = render_with_options(&path, &Config::default(), &options).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert_eq!(image_widths(&pdf), vec![120]);

    // An element that says vector wins over the option.
    let body = format!(
        "<img src=\"{}\" width=40 height=20 style='--raikiri-svg-rendering: vector'>",
        svg_url(RED_RECT)
    );
    let (_dir, path) = input(&format!("{PAGE}{body}"));
    let bytes = render_with_options(&path, &Config::default(), &options).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(image_widths(&pdf).is_empty());
}

#[test]
fn the_opacity_of_an_svg_img_composites_once() {
    let (pdf, operations) = render(
        &format!(
            "<img src=\"{}\" width=40 height=20 style='opacity:.5'>",
            svg_url(RED_RECT)
        ),
        "",
    );
    assert!(has_fill_opacity(&pdf, 0.5));
    assert!(fills(&operations) > 0);
    assert!(image_widths(&pdf).is_empty());
}

#[test]
fn an_svg_background_is_vector_content_when_its_tiles_are_few() {
    let url = svg_url(RED_RECT);
    let div = |css: &str| {
        render(
            "<div></div>",
            &format!("div{{width:100px;height:100px;background:url(\"{url}\") {css}}}"),
        )
    };
    // One tile.
    let (pdf, operations) = div("no-repeat");
    assert!(image_widths(&pdf).is_empty());
    assert!(fills(&operations) > 0);
    // Four tiles of 50 x 50.
    let (pdf, _) = div("0 0 / 50px 50px repeat");
    assert!(image_widths(&pdf).is_empty());
    // A hundred tiles repeat the drawing a hundred times; pixels are drawn by
    // reference instead.
    let (pdf, _) = div("0 0 / 10px 10px repeat");
    assert!(!image_widths(&pdf).is_empty());
}

#[test]
fn a_background_asks_the_element_that_owns_it() {
    let url = svg_url(RED_RECT);
    let (pdf, _) = render(
        "<div class=raster></div>",
        &format!(
            "div{{width:100px;height:100px;background:url(\"{url}\") no-repeat}}\
             .raster{{--raikiri-svg-rendering:raster}}"
        ),
    );
    // The image is the size of the 40 x 20 tile.
    assert_eq!(image_widths(&pdf), vec![40]);
}

#[test]
fn the_canvas_and_margin_boxes_ask_the_root_element() {
    let url = svg_url(RED_RECT);
    let (pdf, _) = operations(&format!(
        "<style>@page{{size:200px 200px;margin:20px;\
         @top-center{{content:'';width:100px;height:10px;\
         background:url(\"{url}\") 0 0 / 20px 10px no-repeat}}}}\
         html{{--raikiri-svg-rendering:raster;background:url(\"{url}\") no-repeat}}</style>"
    ));
    // The canvas draws the picture of its 40 x 20 tile and the margin box
    // that of its 20 x 10 tile.
    assert_eq!(image_widths(&pdf), vec![20, 40]);
}

#[test]
fn marker_and_content_images_are_vector_content() {
    let url = svg_url(RED_RECT);
    let (pdf, operations) = render(
        "<ul><li>A</li></ul><p>B</p>",
        &format!("li{{list-style-image:url(\"{url}\")}}p::before{{content:url(\"{url}\")}}"),
    );
    assert!(image_widths(&pdf).is_empty());
    assert!(fills(&operations) >= 2, "one rectangle for each image");
}

#[test]
fn an_svg_the_scene_builder_leaves_to_the_raster_path_is_pixels() {
    // A mask is outside the vector subset.
    let masked = "<svg xmlns='http://www.w3.org/2000/svg' width='40' height='20'><mask id='m'><rect width='20' height='20' fill='white'/></mask><rect width='40' height='20' fill='red' mask='url(#m)'/></svg>";
    let (pdf, _) = render(
        &format!("<img src=\"{}\" width=40 height=20>", svg_url(masked)),
        "",
    );
    assert!(!image_widths(&pdf).is_empty());
}

/// Text stroked with glyphs stretched to fit a length when `stretched`:
/// Krilla strokes text in the space the glyph is drawn in, so the painter
/// draws such a picture as pixels instead.
fn stroked_text(stretched: bool) -> String {
    let fit = if stretched {
        " textLength='34' lengthAdjust='spacingAndGlyphs'"
    } else {
        ""
    };
    format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='40' height='20' viewBox='0 0 40 20'><text x='2' y='14' font-size='12' fill='none' stroke='red'{fit}>Ab</text></svg>"
    )
}

#[test]
fn a_background_scene_the_painter_cannot_draw_faithfully_is_pixels() {
    let draw = |stretched| {
        let url = svg_url(&stroked_text(stretched));
        render(
            "<div></div>",
            &format!("div{{width:100px;height:100px;background:url(\"{url}\") no-repeat}}"),
        )
        .0
    };
    assert!(
        image_widths(&draw(false)).is_empty(),
        "stroked text is vector"
    );
    assert!(!image_widths(&draw(true)).is_empty());
}

#[test]
fn a_marker_scene_the_painter_cannot_draw_faithfully_is_pixels() {
    let draw = |stretched| {
        let url = svg_url(&stroked_text(stretched));
        render(
            "<ul><li>A</li></ul>",
            &format!("li{{list-style-image:url(\"{url}\")}}"),
        )
        .0
    };
    assert!(
        image_widths(&draw(false)).is_empty(),
        "stroked text is vector"
    );
    assert!(!image_widths(&draw(true)).is_empty());
}
