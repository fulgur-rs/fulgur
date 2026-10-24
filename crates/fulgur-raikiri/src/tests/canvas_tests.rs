use super::*;

/// The `rg` color of each fill (`f`) of `operations`, in paint order.
fn fills(operations: &[lopdf::content::Operation]) -> Vec<[u8; 3]> {
    operations
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op.operator.as_str(), "f" | "f*"))
        .filter_map(|(index, _)| fill_colors(&operations[..index]).last().copied())
        .collect()
}

const PAGES: &str = "<style>@page { size: 300px 200px; margin: 20px } \
    p { margin: 0; height: 100px }</style>";

#[test]
fn body_background_fills_the_page_area_of_every_page() {
    let (_dir, path) = input(&format!(
        "{PAGES}<body style=\"margin: 0; background: rgb(0, 128, 0)\">\
         <p>one</p><p>two</p><p>three</p></body>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    assert_eq!(pdf.get_pages().len(), 2);
    for page in [1, 2] {
        // CSS Backgrounds 3 §2.11.2: the body background is the canvas
        // background, painted once over the page area and not again inside
        // the body's own box.
        assert_eq!(
            fills(&page_operations(&pdf, page)),
            [[0, 128, 0]],
            "page {page}"
        );
    }
}

#[test]
fn root_background_wins_over_body_background() {
    let (_dir, path) = input(&format!(
        "{PAGES}<html style=\"background: rgb(0, 0, 255)\">\
         <body style=\"margin: 0; background: rgb(0, 128, 0)\"><p>one</p></body></html>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    // The root's background covers the page area; the body keeps its own,
    // painted inside its box over the canvas.
    assert_eq!(fills(&page_operations(&pdf, 1)), [[0, 0, 255], [0, 128, 0]]);
}

#[test]
fn transparent_root_and_body_paint_no_canvas() {
    let (_dir, path) = input(&format!(
        "{PAGES}<body style=\"margin: 0\"><p style=\"background: rgb(0, 128, 0)\">one</p></body>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    assert_eq!(fills(&page_operations(&pdf, 1)), [[0, 128, 0]]);
}

#[test]
fn canvas_background_is_an_artifact_in_tagged_pdf() {
    let (_dir, path) = input(&format!(
        "{PAGES}<html lang=\"en\"><title>Canvas</title><body style=\"margin: 0; background: rgb(0, 128, 0)\">\
         <p>one</p></body></html>"
    ));
    let config = Config::builder().pdf_ua(true).build();
    let bytes = render(&path, &config).expect("a PDF/UA document");
    assert!(bytes.starts_with(b"%PDF"));
}

#[test]
fn a_display_none_owner_leaves_the_canvas_transparent() {
    // CSS Backgrounds 3 §2.11: an element that generates no box gives no
    // background to the canvas, and the body does not take over from the
    // root.
    for (html, canvas) in [
        (
            "<html style=\"display: none; background: rgb(0, 0, 255)\">\
             <body style=\"margin: 0\"><p>one</p></body></html>",
            [0, 0, 255],
        ),
        (
            "<body style=\"display: none; background: rgb(0, 128, 0)\"><p>one</p></body>",
            [0, 128, 0],
        ),
    ] {
        let (_dir, path) = input(&format!("{PAGES}{html}"));
        let bytes = render(&path, &Config::default()).expect("PDF bytes");
        let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
        for page in pdf.get_pages().keys() {
            let fills = fills(&page_operations(&pdf, *page));
            assert!(!fills.contains(&canvas), "{html}: {fills:?}");
        }
    }
}

#[test]
fn background_clip_does_not_shrink_the_canvas() {
    for clip in ["text", "border-area", "content-box"] {
        let (_dir, path) = input(&format!(
            "{PAGES}<body style=\"margin: 0; padding: 10px; border: 5px solid rgb(255, 0, 0); \
             background: rgb(0, 128, 0); background-clip: {clip}\"><p>one</p></body>"
        ));
        let bytes = render(&path, &Config::default()).expect("PDF bytes");
        let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
        let fills = fills(&page_operations(&pdf, 1));
        assert_eq!(fills.first(), Some(&[0, 128, 0]), "{clip}: {fills:?}");
    }
}

#[test]
fn the_canvas_shares_the_root_opacity_group() {
    // Compositing and Blending 1 §3.3.1: the canvas background is painted
    // into the root element's group, with the root's content, so the root's
    // opacity fades the two together.
    let (_dir, path) = input(&format!(
        "{PAGES}<html style=\"opacity: 0.5; background: rgb(0, 0, 255)\">\
         <body style=\"margin: 0\">\
         <div style=\"height: 50px; background: rgb(255, 0, 0)\"></div>\
         </body></html>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    let operations = page_operations(&pdf, 1);
    // The canvas and the content are drawn through one transparency group,
    // not straight onto the page.
    assert!(
        !operations.iter().any(|op| op.operator == "f")
            && operations.iter().filter(|op| op.operator == "Do").count() == 1,
        "{operations:?}"
    );
}

#[test]
fn the_canvas_goes_under_the_margin_boxes() {
    // CSS Paged Media 3 §5.1: the canvas is painted below the page-margin
    // boxes, so one that reaches into the page area stays visible.
    let (_dir, path) = input(
        "<style>@page { size: 300px 200px; margin: 20px; \
         @top-center { content: 'x'; background: rgb(255, 0, 0) } }</style>\
         <html style=\"background: rgb(0, 0, 255)\"><body></body></html>",
    );
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    let fills = fills(&page_operations(&pdf, 1));
    assert_eq!(fills.first(), Some(&[0, 0, 255]), "{fills:?}");
    assert!(fills.contains(&[255, 0, 0]), "{fills:?}");
}

#[test]
fn a_root_without_a_box_draws_no_margin_boxes() {
    // Browsers print a document whose root has `display: none` as blank
    // paper, without its margin boxes.
    let css = "<style>@page { size: 300px 200px; margin: 20px; \
        @top-center { content: 'head'; background: rgb(255, 0, 0) } }</style>";
    let (_dir, path) = input(&format!(
        "<html style=\"display: none\"><head>{css}</head><body><p>one</p></body></html>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    assert!(fills(&page_operations(&pdf, 1)).is_empty());
    assert_eq!(pdf.extract_text(&[1]).unwrap().trim(), "");
    // With a box, the same margin box is drawn.
    let (_dir, path) = input(&format!(
        "<html><head>{css}</head><body><p>one</p></body></html>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    assert_eq!(fills(&page_operations(&pdf, 1)), [[255, 0, 0]]);
}

/// Concatenate the matrix `inner` with `outer`: a point goes through `inner`
/// first, as `cm` does to the current transformation matrix.
fn concat(inner: [f32; 6], outer: [f32; 6]) -> [f32; 6] {
    let [a1, b1, c1, d1, e1, f1] = inner;
    let [a2, b2, c2, d2, e2, f2] = outer;
    [
        a1 * a2 + b1 * c2,
        a1 * b2 + b1 * d2,
        c1 * a2 + d1 * c2,
        c1 * b2 + d1 * d2,
        e1 * a2 + f1 * c2 + e2,
        e1 * b2 + f1 * d2 + f2,
    ]
}

/// The bounding boxes `[x0, y0, x1, y1]`, in the units of the content stream
/// after its transformation matrix, of what `paints` draws: a rectangle per
/// painted path for `f`, and a rectangle per image for `Do`, whose unit
/// square the matrix places.
fn painted_bounds(operations: &[lopdf::content::Operation], paints: &[&str]) -> Vec<[f32; 4]> {
    let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut matrices = vec![identity];
    let mut points: Vec<(f32, f32)> = Vec::new();
    let mut bounds = Vec::new();
    let bbox = |points: &[(f32, f32)]| {
        points.iter().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        )
    };
    for operation in operations {
        let values: Vec<f32> = operation
            .operands
            .iter()
            .filter_map(|value| value.as_float().ok())
            .collect();
        let matrix = *matrices.last().unwrap();
        let place = |(x, y): (f32, f32)| {
            (
                matrix[0] * x + matrix[2] * y + matrix[4],
                matrix[1] * x + matrix[3] * y + matrix[5],
            )
        };
        match operation.operator.as_str() {
            "q" => matrices.push(matrix),
            "Q" => {
                matrices.pop();
            }
            "cm" => {
                let inner: [f32; 6] = values.try_into().unwrap();
                *matrices.last_mut().unwrap() = concat(inner, matrix);
            }
            "m" | "l" | "c" => points.extend(
                values
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&p| place((p[0], p[1]))),
            ),
            "re" => {
                let (x, y, w, h) = (values[0], values[1], values[2], values[3]);
                points.extend([(x, y), (x + w, y + h)].map(place));
            }
            "n" | "W" | "W*" => points.clear(),
            "Do" if paints.contains(&"Do") => {
                let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(place);
                let (x0, y0, x1, y1) = bbox(&corners);
                bounds.push([x0, y0, x1, y1]);
            }
            operator if paints.contains(&operator) => {
                let (x0, y0, x1, y1) = bbox(&points);
                bounds.push([x0, y0, x1, y1]);
                points.clear();
            }
            "f" | "f*" | "S" | "B" | "b" => points.clear(),
            _ => {}
        }
    }
    bounds
}

/// `bounds` of a page content stream, which is in pt with y up, in CSS px
/// with y down for the 300 x 200 px pages of [`ROOT_PAGES`].
fn page_px(bounds: Vec<[f32; 4]>) -> Vec<[f32; 4]> {
    bounds
        .into_iter()
        .map(|[x0, y0, x1, y1]| {
            [
                x0 / 0.75,
                (150.0 - y1) / 0.75,
                x1 / 0.75,
                (150.0 - y0) / 0.75,
            ]
        })
        .collect()
}

fn assert_bounds(got: &[[f32; 4]], want: &[[f32; 4]]) {
    assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
    for (got, want) in got.iter().zip(want) {
        assert_close(got, want);
    }
}

/// `html` rendered with the 8 x 8 px dot image bundled as `dot.png`.
fn render_with_dot(html: &str) -> lopdf::Document {
    let mut bundle = AssetBundle::new();
    bundle.add_image("dot.png", super::assets_tests::dot_png());
    let (_dir, path) = input(html);
    let options = RenderOptions {
        assets: Some(&bundle),
        system_fonts: true,
    };
    let bytes = render_with_options(&path, &Config::default(), &options).expect("PDF bytes");
    lopdf::Document::load_mem(&bytes).expect("a readable PDF")
}

/// A 300 x 200 px page with a 20 px margin, so its page area is
/// `[20, 20, 280, 180]`, and no margin on the root or the body.
const ROOT_PAGES: &str = "<style>@page { size: 300px 200px; margin: 20px } \
    html, body { margin: 0 } p { margin: 0 }</style>";

/// The images drawn on `page` of `pdf`, in CSS px.
fn images(pdf: &lopdf::Document, page: u32) -> Vec<[f32; 4]> {
    page_px(painted_bounds(&page_operations(pdf, page), &["Do"]))
}

#[test]
fn canvas_images_are_positioned_in_the_root_box() {
    // CSS Backgrounds 3 §2.11.2: the image is placed in the root element's
    // box, which is 100 px high here, not in the 160 px page area.
    let pdf = render_with_dot(&format!(
        "{ROOT_PAGES}<html style=\"background: url(dot.png) no-repeat right bottom / 10px 10px\">\
         <body><p style=\"height: 100px\">one</p></body></html>"
    ));
    assert_bounds(&images(&pdf, 1), &[[270.0, 110.0, 280.0, 120.0]]);
}

#[test]
fn fixed_canvas_images_are_positioned_in_the_page_area() {
    // CSS Backgrounds 3 §2.5: a `fixed` layer is positioned in the initial
    // containing block, which here is the page area.
    let pdf = render_with_dot(&format!(
        "{ROOT_PAGES}<html style=\"background: url(dot.png) no-repeat right bottom / 10px 10px fixed\">\
         <body><p style=\"height: 100px\">one</p></body></html>"
    ));
    assert_bounds(&images(&pdf, 1), &[[270.0, 170.0, 280.0, 180.0]]);
}

#[test]
fn background_origin_insets_the_root_box_by_its_border_and_padding() {
    // The root has a 5 px border and 10 px of padding, so its border box,
    // padding box and content box start 0, 5 and 15 px inside the page area.
    for (origin, inset) in [
        ("border-box", 0.0),
        ("padding-box", 5.0),
        ("content-box", 15.0),
    ] {
        let pdf = render_with_dot(&format!(
            "{ROOT_PAGES}<html style=\"border: 5px solid; padding: 10px; \
             background: url(dot.png) no-repeat 0 0 / 10px 10px; background-origin: {origin}\">\
             <body><p style=\"height: 100px\">one</p></body></html>"
        ));
        let (x, y) = (20.0 + inset, 20.0 + inset);
        assert_bounds(&images(&pdf, 1), &[[x, y, x + 10.0, y + 10.0]]);
    }
}

#[test]
fn canvas_images_continue_across_pages() {
    // The canvas runs through the pages as one surface: the root's box sits
    // one page area (160 px) higher on the second page, so an image placed
    // 200 px down the root shows there and not on the first page.
    let pdf = render_with_dot(&format!(
        "{ROOT_PAGES}<html style=\"background: url(dot.png) no-repeat 0 200px / 10px 10px\">\
         <body><p style=\"height: 150px\">one</p><p style=\"height: 150px\">two</p></body></html>"
    ));
    assert_eq!(pdf.get_pages().len(), 2);
    // On the first page the image lies 40 px below the page area, which cuts
    // it away.
    assert_bounds(&images(&pdf, 1), &[[20.0, 220.0, 30.0, 230.0]]);
    assert_bounds(&images(&pdf, 2), &[[20.0, 60.0, 30.0, 70.0]]);
}

#[test]
fn the_root_transform_moves_the_canvas_images_and_the_page_content() {
    // CSS Transforms 1 §6: a transform on the root moves the whole canvas,
    // its images included, and the page content. The canvas color is not
    // part of the root's box and keeps covering the page area.
    let pdf = render_with_dot(&format!(
        "{ROOT_PAGES}<html style=\"transform: translate(40px, 30px); \
         background: rgb(0, 0, 255) url(dot.png) no-repeat 0 0 / 10px 10px\">\
         <body><p style=\"height: 20px; background: rgb(0, 128, 0)\">one</p></body></html>"
    ));
    let operations = page_operations(&pdf, 1);
    assert_eq!(fills(&operations), [[0, 0, 255], [0, 128, 0]]);
    assert_bounds(
        &page_px(painted_bounds(&operations, &["f"])),
        &[[20.0, 20.0, 280.0, 180.0], [60.0, 50.0, 320.0, 70.0]],
    );
    assert_bounds(&images(&pdf, 1), &[[60.0, 50.0, 70.0, 60.0]]);
    // The moved layers are cut to the page area.
    assert!(clip_bounds(&operations).contains(&[20.0, 20.0, 280.0, 180.0]));
}

#[test]
fn a_scaled_root_scales_the_canvas_images_around_its_origin() {
    // The transform origin is the middle of the root's border box: the
    // 260 x 100 px box at (20, 20) scales around (150, 70).
    let pdf = render_with_dot(&format!(
        "{ROOT_PAGES}<html style=\"transform: scale(2); \
         background: url(dot.png) no-repeat 0 0 / 10px 10px\">\
         <body><p style=\"height: 100px\">one</p></body></html>"
    ));
    assert_bounds(&images(&pdf, 1), &[[-110.0, -30.0, -90.0, -10.0]]);
}

#[test]
fn a_singular_root_transform_leaves_only_the_canvas_color() {
    // CSS Transforms 1 §6: a matrix that cannot be inverted renders nothing
    // of the root. The canvas color is still painted.
    let pdf = render_with_dot(&format!(
        "{ROOT_PAGES}<html style=\"transform: scale(0); \
         background: rgb(0, 0, 255) url(dot.png) no-repeat 0 0 / 10px 10px\">\
         <body><p style=\"height: 20px; background: rgb(0, 128, 0)\">one</p></body></html>"
    ));
    let operations = page_operations(&pdf, 1);
    assert_eq!(fills(&operations), [[0, 0, 255]]);
    assert!(images(&pdf, 1).is_empty());
    assert_eq!(count(&operations, "BT"), 0);
}

#[test]
fn the_root_transform_leaves_the_margin_boxes_alone() {
    let (_dir, path) = input(
        "<style>@page { size: 300px 200px; margin: 20px; \
         @top-center { content: 'x'; background: rgb(255, 0, 0) } } \
         html, body { margin: 0 }</style>\
         <html style=\"transform: translate(40px, 30px)\">\
         <body><p style=\"height: 20px; background: rgb(0, 128, 0)\">one</p></body></html>",
    );
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    let operations = page_operations(&pdf, 1);
    let bounds = page_px(painted_bounds(&operations, &["f"]));
    let red = fills(&operations)
        .iter()
        .position(|c| *c == [255, 0, 0])
        .unwrap();
    // The top-center box sits in the top margin, 20 px high.
    assert!(bounds[red][3] <= 20.0 + 0.01, "{bounds:?}");
    let green = fills(&operations)
        .iter()
        .position(|c| *c == [0, 128, 0])
        .unwrap();
    assert_bounds(&bounds[green..=green], &[[60.0, 50.0, 320.0, 70.0]]);
}

#[test]
fn the_faded_root_transforms_its_canvas_images_inside_the_group() {
    // The opacity group holds the canvas and the page content; the root's
    // transform moves what is in it. The group's own content is in CSS px
    // with y down.
    let pdf = render_with_dot(&format!(
        "{ROOT_PAGES}<html style=\"opacity: 0.5; transform: translate(40px, 30px); \
         background: url(dot.png) no-repeat 0 0 / 10px 10px\">\
         <body><p style=\"height: 20px\">one</p></body></html>"
    ));
    let mut found = Vec::new();
    for object in pdf.objects.values() {
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        let is_form = stream
            .dict
            .get(b"Subtype")
            .is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Form"));
        if is_form {
            let content = lopdf::content::Content::decode(&stream.get_plain_content().unwrap())
                .unwrap()
                .operations;
            found.extend(painted_bounds(&content, &["Do"]));
        }
    }
    // The image comes first; the group of the text runs follows it.
    assert_bounds(&found[..1], &[[60.0, 50.0, 70.0, 60.0]]);
}

#[test]
fn an_identity_root_transform_changes_nothing() {
    let draw = |transform: &str| {
        let pdf = render_with_dot(&format!(
            "{ROOT_PAGES}<html style=\"transform: {transform}; \
             background: rgb(0, 0, 255) url(dot.png) no-repeat 0 0 / 10px 10px\">\
             <body><p style=\"height: 20px; background: rgb(0, 128, 0)\">one</p></body></html>"
        ));
        let operations = page_operations(&pdf, 1);
        (
            page_px(painted_bounds(&operations, &["f"])),
            images(&pdf, 1),
            count(&operations, "BT"),
        )
    };
    let plain = draw("none");
    assert_eq!(draw("translate(0px)"), plain);
    assert_bounds(&plain.1, &[[20.0, 20.0, 30.0, 30.0]]);
}
