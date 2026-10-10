use super::*;
use fulgur_core::{Config, Error, Margin, PageSize};
use raikiri_html::{DocumentLayout, DomView, FragmentKind, LayoutConfig, LayoutStatus, NodeId};
use raikiri_traits::AbortController;
use std::path::{Path, PathBuf};

const CSS: &str = "<style>@page { size: 300px 200px; margin: 20px } body { margin:0 } p { margin:0; height:30px }</style>";

fn input(html: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.html");
    std::fs::write(&path, html).unwrap();
    (dir, path)
}

fn completed(path: &Path) -> DocumentLayout {
    match layout_file(path, &Config::default(), LayoutConfig::default()).unwrap() {
        LayoutStatus::Completed(result) => result,
        _ => panic!("expected a completed layout"),
    }
}

fn find_id(view: DomView<'_>, node: NodeId, id: &str) -> Option<NodeId> {
    if view.attr(node, "id") == Some(id) {
        return Some(node);
    }
    view.children(node)
        .find_map(|child| find_id(view, child, id))
}

#[test]
fn single_page_geometry_and_fragment_origin() {
    let (_dir, path) = input(&format!("{CSS}<p>Hello</p>"));
    let result = completed(&path);
    assert_eq!(result.page_count(), 1);
    let page = result.page(0).unwrap();
    let geometry = page.geometry();
    let r = geometry.page_box;
    assert_eq!((r.x, r.y, r.width, r.height), (0.0, 0.0, 300.0, 200.0));
    let r = geometry.content_box;
    assert_eq!((r.x, r.y, r.width, r.height), (20.0, 20.0, 260.0, 160.0));
    let m = geometry.margins;
    assert_eq!((m.top, m.right, m.bottom, m.left), (20.0, 20.0, 20.0, 20.0));
    let paragraph = page
        .fragments()
        .find(|f| page.dom().local_name(f.node()) == Some("p"))
        .unwrap();
    assert_eq!((paragraph.rect().x, paragraph.rect().y), (20.0, 20.0));
    assert!(
        page.fragments()
            .any(|f| f.kind() == FragmentKind::Text && f.line_range().is_some())
    );
}

#[test]
fn explicit_break_creates_second_page() {
    let (_dir, path) = input(&format!(
        "{CSS}<p id=first>Hello</p><p id=second style='break-before:page'>World</p>"
    ));
    let result = completed(&path);
    assert_eq!(result.page_count(), 2);
    for (index, id) in [(0, "first"), (1, "second")] {
        let page = result.page(index).unwrap();
        assert_eq!(page.index(), index);
        assert!(
            page.fragments()
                .any(|f| page.dom().attr(f.node(), "id") == Some(id))
        );
        assert!(!page.fragments().any(|f| page.dom().attr(f.node(), "id")
            == Some(if index == 0 { "second" } else { "first" })));
    }
}

#[test]
fn hidden_element_has_no_fragment() {
    let (_dir, path) = input("<div id=hidden style='display:none'>Hidden</div><p>Visible</p>");
    let result = completed(&path);
    let view = result.page(0).unwrap().dom();
    let hidden = find_id(view, view.root(), "hidden").unwrap();
    assert!(!result.is_rendered(hidden));
    assert!(
        result
            .pages()
            .all(|page| page.fragments().all(|f| f.node() != hidden))
    );
}

#[test]
fn already_aborted_layout_returns_aborted() {
    let (_dir, path) = input("<p>Hello</p>");
    let controller = AbortController::new();
    controller.abort();
    let config = LayoutConfig::builder()
        .signal(Some(controller.signal.clone()))
        .build();
    let status = layout_file(&path, &Config::default(), config).unwrap();
    assert!(matches!(status, LayoutStatus::Aborted));
    // An aborted layout has nothing to draw.
    assert!(matches!(
        draw(
            status,
            &RenderResources::new(),
            &Config::default(),
            &bookmarks::BookmarkCollector::default(),
            &url::Url::from_file_path(path.canonicalize().unwrap()).unwrap(),
            &RenderOptions::default(),
        ),
        Err(Error::Layout(_))
    ));
}

#[test]
fn render_writes_one_pdf_page_per_layout_page() {
    let (_dir, path) = input(&format!(
        "{CSS}<p style=\"background-color: rgb(0, 128, 0); border: 2px solid red\">one</p>\
         <p style=\"break-before: page\">two</p>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    let pages = pdf.get_pages();
    assert_eq!(pages.len(), 2);
    // 300 x 200 CSS px is 225 x 150 pt.
    let first = pdf.get_object(*pages.get(&1).unwrap()).unwrap();
    let media_box = first.as_dict().unwrap().get(b"MediaBox").unwrap();
    let media_box: Vec<f32> = media_box
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_float().unwrap())
        .collect();
    assert_eq!(media_box, vec![0.0, 0.0, 225.0, 150.0]);
}

#[test]
fn render_preserves_input_io_error() {
    let dir = tempfile::tempdir().unwrap();
    match render(&dir.path().join("missing.html"), &Config::default()) {
        Err(Error::Io(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected IO error, got {other:?}"),
    }
}

// macOS file systems (APFS) only accept UTF-8 file names, so a path with a
// non-UTF-8 byte cannot be created there; other Unix systems store raw bytes.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_input_filename_is_accepted() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"input-\xff.html".to_vec()));
    std::fs::write(&path, "<p>Hello</p>").unwrap();
    assert_eq!(completed(&path).page_count(), 1);
    assert!(render(&path, &Config::default()).is_ok_and(|bytes| bytes.starts_with(b"%PDF")));
}

#[test]
fn render_draws_extractable_text_with_one_font_per_face() {
    let (_dir, path) = input(&format!(
        "{CSS}<p>Hello <b>bold</b> world</p><p style=\"color: rgb(200, 0, 0)\">second line</p>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    let text = pdf.extract_text(&[1]).expect("extractable text");
    let words: Vec<&str> = text.split_whitespace().collect();
    assert_eq!(words, ["Hello", "bold", "world", "second", "line"]);
    // The regular face is shared by both paragraphs; bold is a second face.
    let fonts = pdf
        .objects
        .values()
        .filter(|object| {
            object.as_dict().is_ok_and(|dict| {
                dict.get(b"Type")
                    .is_ok_and(|t| t.as_name().is_ok_and(|n| n == b"Font"))
            }) && object.as_dict().is_ok_and(|dict| {
                dict.get(b"Subtype")
                    .is_ok_and(|t| t.as_name().is_ok_and(|n| n == b"Type0"))
            })
        })
        .count();
    assert_eq!(fonts, 2);
}

#[test]
fn hidden_boxes_and_transparent_text_draw_nothing() {
    let (_dir, path) = input(&format!(
        "{CSS}<p>kept</p>\
         <p style=\"visibility: hidden; background-color: red; border: 3px solid red\">hidden</p>\
         <p style=\"color: transparent\">clear</p>"
    ));
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    let text = pdf.extract_text(&[1]).expect("extractable text");
    assert_eq!(text.split_whitespace().collect::<Vec<_>>(), ["kept"]);
    // Nothing red is filled: the hidden box draws no background or border.
    let content = pdf
        .get_page_content(*pdf.get_pages().get(&1).unwrap())
        .unwrap();
    let content = String::from_utf8_lossy(&content);
    assert!(!content.contains("1 0 0 rg"), "{content}");
}

#[test]
fn margin_boxes_draw_the_running_element_of_each_page() {
    let (_dir, path) = input(
        "<style>@page { size: 300px 200px; margin: 40px; @top-center { content: element(hdr) } } \
         body { margin: 0 } p { margin: 0 } \
         .hdr { position: running(hdr); background: rgb(0, 128, 0) }</style>\
         <div class=\"hdr\">Alpha</div><p>one</p>\
         <div class=\"hdr\">Beta</div><p style=\"break-before: page\">two</p>",
    );
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    let words = |page: u32| {
        let text = pdf.extract_text(&[page]).expect("extractable text");
        text.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    // Each page shows its own header, drawn before the page body, instead of
    // the document-wide text of the last header.
    assert_eq!(words(1), ["Alpha", "one"]);
    assert_eq!(words(2), ["Beta", "two"]);
    // The element is drawn as laid out, with its background.
    let content = pdf
        .get_page_content(*pdf.get_pages().get(&1).unwrap())
        .unwrap();
    let operations = lopdf::content::Content::decode(&content)
        .unwrap()
        .operations;
    assert!(fill_colors(&operations).contains(&[0, 128, 0]));
}

/// Render `html` and decode the drawing operations of every content stream:
/// page contents and Form XObjects (Krilla draws some fills, such as
/// translucent ones, through those).
fn operations(html: &str) -> (lopdf::Document, Vec<lopdf::content::Operation>) {
    let (_dir, path) = input(html);
    let bytes = render(&path, &Config::default()).expect("PDF bytes");
    let pdf = lopdf::Document::load_mem(&bytes).expect("a readable PDF");
    let mut operations = Vec::new();
    for page in pdf.get_pages().values() {
        let content = pdf.get_page_content(*page).unwrap();
        operations.extend(
            lopdf::content::Content::decode(&content)
                .unwrap()
                .operations,
        );
    }
    for object in pdf.objects.values() {
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        let is_form = stream
            .dict
            .get(b"Subtype")
            .is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Form"));
        if is_form {
            let content = stream.get_plain_content().unwrap();
            operations.extend(
                lopdf::content::Content::decode(&content)
                    .unwrap()
                    .operations,
            );
        }
    }
    (pdf, operations)
}

fn count(operations: &[lopdf::content::Operation], operator: &str) -> usize {
    operations
        .iter()
        .filter(|operation| operation.operator == operator)
        .count()
}

/// Whether any dictionary in the document, at any depth, has
/// `/ShadingType shading_type`.
fn has_shading(pdf: &lopdf::Document, shading_type: i64) -> bool {
    fn in_dict(dict: &lopdf::Dictionary, shading_type: i64) -> bool {
        dict.iter().any(|(key, value)| {
            (key == b"ShadingType" && value.as_i64().is_ok_and(|t| t == shading_type))
                || in_object(value, shading_type)
        })
    }
    fn in_object(object: &lopdf::Object, shading_type: i64) -> bool {
        match object {
            lopdf::Object::Dictionary(dict) => in_dict(dict, shading_type),
            lopdf::Object::Stream(stream) => in_dict(&stream.dict, shading_type),
            lopdf::Object::Array(items) => items.iter().any(|item| in_object(item, shading_type)),
            _ => false,
        }
    }
    pdf.objects
        .values()
        .any(|object| in_object(object, shading_type))
}

/// Bounding box `[x0, y0, x1, y1]` in CSS px of the path each `W` clips
/// to. Krilla writes clip paths in PDF page space (pt, y up), so they are
/// mapped back for the 300 x 200 px pages of [`CSS`].
fn clip_bounds(operations: &[lopdf::content::Operation]) -> Vec<[f32; 4]> {
    let mut bounds = Vec::new();
    for (index, operation) in operations.iter().enumerate() {
        if operation.operator != "W" {
            continue;
        }
        let start = operations[..index]
            .iter()
            .rposition(|op| op.operator == "m")
            .unwrap();
        let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for op in &operations[start..index] {
            let values: Vec<f32> = op
                .operands
                .iter()
                .filter_map(|value| value.as_float().ok())
                .collect();
            for point in values.as_chunks::<2>().0 {
                bbox = [
                    bbox[0].min(point[0]),
                    bbox[1].min(point[1]),
                    bbox[2].max(point[0]),
                    bbox[3].max(point[1]),
                ];
            }
        }
        let px = |pt: f32| pt / 0.75;
        bounds.push([
            px(bbox[0]),
            px(150.0 - bbox[3]),
            px(bbox[2]),
            px(150.0 - bbox[1]),
        ]);
    }
    bounds
}

/// RGB fill colors set with `rg`, in 0-255.
fn fill_colors(operations: &[lopdf::content::Operation]) -> Vec<[u8; 3]> {
    operations
        .iter()
        .filter(|operation| operation.operator == "rg")
        .map(|operation| {
            let channel = |index: usize| {
                (operation.operands[index].as_float().unwrap() * 255.0).round() as u8
            };
            [channel(0), channel(1), channel(2)]
        })
        .collect()
}

#[test]
fn rounded_corners_draw_curves_and_square_ones_do_not() {
    let (_, square) = operations(&format!(
        "{CSS}<p style=\"background-color: rgb(0, 128, 0); border: 2px solid red\">x</p>"
    ));
    assert_eq!(count(&square, "c"), 0);
    let (_, rounded) = operations(&format!(
        "{CSS}<p style=\"background-color: rgb(0, 128, 0); border: 2px solid red; \
         border-radius: 10px\">x</p>"
    ));
    // The background and the outer and inner outline of the border ring
    // each have four corners.
    assert_eq!(count(&rounded, "c"), 12, "{rounded:?}");
    // The ring is one even-odd fill.
    assert_eq!(count(&rounded, "f*"), 1);
}

#[test]
fn percentage_radius_draws_an_ellipse() {
    let (_, operations) = operations(&format!(
        "{CSS}<p style=\"background-color: rgb(0, 128, 0); width: 80px; border-radius: 50%\"></p>"
    ));
    // 80 x 30 with 50%: radii of 40 x 15, so the curve from the top edge
    // to the right edge spans that much.
    let curve = operations.iter().find(|op| op.operator == "c").unwrap();
    let end: Vec<f32> = curve.operands[4..6]
        .iter()
        .map(|value| value.as_float().unwrap())
        .collect();
    let start = operations
        .iter()
        .find(|op| op.operator == "m")
        .map(|op| op.operands[0].as_float().unwrap())
        .unwrap();
    assert!((start - 60.0).abs() < 0.01, "{start}");
    assert_eq!(end, [100.0, 35.0]);
}

#[test]
fn gradients_produce_axial_and_radial_shadings() {
    let (pdf, _) = operations(&format!(
        "{CSS}<p style=\"background-image: linear-gradient(45deg, red, blue)\"></p>"
    ));
    assert!(has_shading(&pdf, 2));
    assert!(!has_shading(&pdf, 3));
    let (pdf, _) = operations(&format!(
        "{CSS}<p style=\"background-image: radial-gradient(circle, red, blue)\"></p>"
    ));
    assert!(has_shading(&pdf, 3));
    // A repeating gradient expands to an ordinary shading, without a
    // PostScript function (type 4).
    let (pdf, _) = operations(&format!(
        "{CSS}<p style=\"background-image: \
         repeating-linear-gradient(red 0px, red 5px, blue 5px, blue 10px)\"></p>"
    ));
    assert!(has_shading(&pdf, 2));
    let has_postscript = pdf.objects.values().any(|object| {
        object.as_stream().is_ok_and(|stream| {
            stream
                .dict
                .get(b"FunctionType")
                .is_ok_and(|kind| kind.as_i64().is_ok_and(|kind| kind == 4))
        })
    });
    assert!(!has_postscript);
}

#[test]
fn overflow_hidden_clips_descendants_to_the_padding_box() {
    let child = "<div style=\"height: 100px; background-color: blue\">text</div>";
    let (_, visible) = operations(&format!(
        "{CSS}<div style=\"height: 40px; border: 5px solid black\">{child}</div>"
    ));
    assert_eq!(count(&visible, "W"), 0);
    let (_, hidden) = operations(&format!(
        "{CSS}<div style=\"height: 40px; border: 5px solid black; overflow: hidden\">{child}</div>"
    ));
    // One clip, kept for the child's background and its text, at the
    // padding box: the border box at (20, 20) inset by the 5px border, 40px
    // high.
    assert_eq!(count(&hidden, "W"), 1, "{hidden:?}");
    assert_eq!(clip_bounds(&hidden), [[25.0, 25.0, 275.0, 65.0]]);
}

#[test]
fn overflow_clip_on_one_axis_clips_only_that_axis() {
    let html = |overflow: &str| {
        format!(
            "{CSS}<div style=\"width: 50px; height: 40px; {overflow}\">\
             <div style=\"width: 200px; height: 100px; background-color: blue\"></div></div>"
        )
    };
    // `clip` beside `visible`: clipped to the 50px width, open over the
    // whole page height.
    let (_, clip) = operations(&html("overflow-x: clip"));
    assert_eq!(clip_bounds(&clip), [[20.0, 0.0, 70.0, 200.0]]);
    // `hidden` beside `visible`: the visible axis computes to `auto`
    // (CSS Overflow 3 §3.1), so both axes clip.
    let (_, hidden) = operations(&html("overflow-x: hidden; overflow-y: visible"));
    assert_eq!(clip_bounds(&hidden), [[20.0, 20.0, 70.0, 60.0]]);
}

#[test]
fn rounded_overflow_clip_follows_the_padding_curve() {
    let (_, operations) = operations(&format!(
        "{CSS}<div style=\"height: 40px; border-radius: 10px; overflow: hidden\">\
         <div style=\"height: 100px; background-color: blue\"></div></div>"
    ));
    let clip_index = operations.iter().position(|op| op.operator == "W").unwrap();
    let clip_path = &operations[..clip_index];
    let last_move = clip_path.iter().rposition(|op| op.operator == "m").unwrap();
    assert_eq!(count(&clip_path[last_move..], "c"), 4);
}

#[test]
fn body_overflow_does_not_clip_the_document() {
    let (_, operations) = operations(
        "<style>@page { size: 300px 200px; margin: 20px } body { margin: 0; overflow: hidden; \
         height: 10px }</style><p style=\"background-color: blue; height: 50px\"></p>",
    );
    assert_eq!(count(&operations, "W"), 0);
}

#[test]
fn dashed_and_dotted_borders_set_dash_patterns() {
    let dash_arrays = |style: &str| {
        let (_, operations) = operations(&format!(
            "{CSS}<p style=\"width: 100px; border: 4px {style} rgb(0, 0, 255)\"></p>"
        ));
        operations
            .iter()
            .filter(|op| op.operator == "d")
            .map(|op| {
                op.operands[0]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_float().unwrap())
                    .collect::<Vec<f32>>()
            })
            .collect::<Vec<_>>()
    };
    let dashed = dash_arrays("dashed");
    // One pattern per side; each is a dash of three widths and a gap.
    assert_eq!(dashed.len(), 4, "{dashed:?}");
    assert!(
        dashed
            .iter()
            .all(|array| array.len() == 2 && array[0] == 12.0)
    );
    let dotted = dash_arrays("dotted");
    assert_eq!(dotted.len(), 4, "{dotted:?}");
    // Zero-length dashes with round caps make dots.
    assert!(dotted.iter().all(|array| array[0] == 0.0 && array[1] > 0.0));
    assert!(dash_arrays("solid").is_empty());
}

#[test]
fn double_and_three_d_borders_split_the_border_area() {
    let (_, double) = operations(&format!(
        "{CSS}<p style=\"border: 9px double rgb(0, 0, 255)\"></p>"
    ));
    // Two lines, each an even-odd ring.
    assert_eq!(count(&double, "f*"), 2);
    let (_, inset) = operations(&format!(
        "{CSS}<p style=\"border: 4px inset rgb(200, 200, 200)\"></p>"
    ));
    let colors = fill_colors(&inset);
    // Top and left are shaded, bottom and right lit.
    assert!(colors.contains(&[100, 100, 100]), "{colors:?}");
    assert!(colors.contains(&[227, 227, 227]), "{colors:?}");
    let (_, groove) = operations(&format!(
        "{CSS}<p style=\"border: 4px groove rgb(200, 200, 200)\"></p>"
    ));
    // Each side has an outer and an inner half.
    assert_eq!(count(&groove, "f*"), 8);
}

/// Every shading dictionary in the document.
fn shadings(pdf: &lopdf::Document) -> Vec<&lopdf::Dictionary> {
    pdf.objects
        .values()
        .filter_map(|object| match object {
            lopdf::Object::Dictionary(dict) => Some(dict),
            lopdf::Object::Stream(stream) => Some(&stream.dict),
            _ => None,
        })
        .filter(|dict| dict.has(b"ShadingType"))
        .collect()
}

fn floats(object: &lopdf::Object) -> Vec<f32> {
    object
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_float().unwrap())
        .collect()
}

fn assert_close(got: &[f32], want: &[f32]) {
    assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 0.01, "{got:?} vs {want:?}");
    }
}

/// Render one 260 x 30 px paragraph at (20, 20) with `style` and return the
/// document and the `/Coords` of its only shading, if any.
fn gradient_box(
    style: &str,
) -> (
    lopdf::Document,
    Vec<lopdf::content::Operation>,
    Option<Vec<f32>>,
) {
    let (pdf, operations) = operations(&format!("{CSS}<p style=\"{style}\"></p>"));
    let all = shadings(&pdf);
    assert!(all.len() <= 1, "{all:?}");
    let coords = all
        .first()
        .and_then(|dict| dict.get(b"Coords").ok())
        .map(floats);
    (pdf, operations, coords)
}

/// The stop colors of a shading's function, in 0-255: the start color of
/// each stitched segment and the end color of the last one.
fn stop_colors(pdf: &lopdf::Document, shading: &lopdf::Dictionary) -> Vec<[u8; 3]> {
    let resolve = |object: &lopdf::Object| -> lopdf::Dictionary {
        match object {
            lopdf::Object::Reference(id) => match pdf.get_object(*id).unwrap() {
                lopdf::Object::Stream(stream) => stream.dict.clone(),
                other => other.as_dict().unwrap().clone(),
            },
            other => other.as_dict().unwrap().clone(),
        }
    };
    let color = |dict: &lopdf::Dictionary, key: &[u8]| {
        let values = floats(dict.get(key).unwrap());
        [0, 1, 2].map(|index| (values[index] * 255.0).round() as u8)
    };
    let function = resolve(shading.get(b"Function").unwrap());
    let parts: Vec<lopdf::Dictionary> = match function.get(b"Functions") {
        Ok(functions) => functions.as_array().unwrap().iter().map(resolve).collect(),
        Err(_) => vec![function],
    };
    let mut colors: Vec<[u8; 3]> = parts.iter().map(|part| color(part, b"C0")).collect();
    colors.push(color(parts.last().unwrap(), b"C1"));
    colors
}

/// The source of every PostScript (type 4) function in the document.
fn postscript(pdf: &lopdf::Document) -> String {
    pdf.objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter(|stream| {
            stream
                .dict
                .get(b"FunctionType")
                .is_ok_and(|kind| kind.as_i64().is_ok_and(|kind| kind == 4))
        })
        .map(|stream| String::from_utf8_lossy(&stream.get_plain_content().unwrap()).into_owned())
        .collect()
}

/// Position of `point` along the gradient line `[x1, y1, x2, y2]`, as a
/// fraction of the line.
fn along(line: &[f32], point: (f32, f32)) -> f32 {
    let (dx, dy) = (line[2] - line[0], line[3] - line[1]);
    ((point.0 - line[0]) * dx + (point.1 - line[1]) * dy) / (dx * dx + dy * dy)
}

#[test]
fn linear_gradient_lines_follow_sides_angles_and_corners() {
    // Sides: the line spans the box from edge to edge through the center.
    for (direction, want) in [
        ("to right", [20.0, 35.0, 280.0, 35.0]),
        ("to left", [280.0, 35.0, 20.0, 35.0]),
        ("to top", [150.0, 50.0, 150.0, 20.0]),
        ("180deg", [150.0, 20.0, 150.0, 50.0]),
    ] {
        let (_, _, coords) = gradient_box(&format!(
            "background-image: linear-gradient({direction}, red, blue)"
        ));
        assert_close(&coords.unwrap(), &want);
    }
    // Corners and angles (CSS Images 3 §3.1.1): the corner the line points
    // to is at 100% and the opposite one at 0%.
    for (direction, start, end) in [
        ("to top right", (20.0, 50.0), (280.0, 20.0)),
        ("to bottom left", (280.0, 20.0), (20.0, 50.0)),
        ("45deg", (20.0, 50.0), (280.0, 20.0)),
        ("135deg", (20.0, 20.0), (280.0, 50.0)),
    ] {
        let (_, _, coords) = gradient_box(&format!(
            "background-image: linear-gradient({direction}, red, blue)"
        ));
        let line = coords.unwrap();
        assert!(along(&line, start).abs() < 1e-3, "{direction}: {line:?}");
        assert!(
            (along(&line, end) - 1.0).abs() < 1e-3,
            "{direction}: {line:?}"
        );
    }
    // A magic corner makes the 50% line join the two other corners, so the
    // line is perpendicular to that diagonal (it is not 45 degrees here).
    let (_, _, coords) = gradient_box("background-image: linear-gradient(to top right, red, blue)");
    let line = coords.unwrap();
    let (dx, dy) = (line[2] - line[0], line[3] - line[1]);
    assert!((dx * 260.0 + dy * 30.0).abs() < 1e-2, "{line:?}");
}

#[test]
fn linear_gradient_stop_positions_move_the_shading_ends() {
    // 25% and 75% of the 260px line.
    let (pdf, _, coords) =
        gradient_box("background-image: linear-gradient(to right, red 25%, blue 75%)");
    assert_close(&coords.unwrap(), &[85.0, 35.0, 215.0, 35.0]);
    assert_eq!(
        stop_colors(&pdf, shadings(&pdf)[0]),
        [[255, 0, 0], [0, 0, 255]]
    );
    // Both stops at one point: a hard edge there, red before and blue after.
    let (pdf, _, coords) =
        gradient_box("background-image: linear-gradient(to right, red 50%, blue 50%)");
    let coords = coords.unwrap();
    assert!(coords[0] < 150.0 && coords[2] > 150.0, "{coords:?}");
    assert!(coords[2] - coords[0] < 0.1, "{coords:?}");
    let colors = stop_colors(&pdf, shadings(&pdf)[0]);
    assert_eq!(colors.first(), Some(&[255, 0, 0]));
    assert_eq!(colors.last(), Some(&[0, 0, 255]));
}

#[test]
fn gradient_stops_resolve_currentcolor() {
    let (pdf, _, _) = gradient_box(
        "color: rgb(0, 128, 0); background-image: linear-gradient(currentcolor, blue)",
    );
    assert_eq!(
        stop_colors(&pdf, shadings(&pdf)[0]),
        [[0, 128, 0], [0, 0, 255]]
    );
}

#[test]
fn zero_length_repeating_gradient_draws_its_last_color() {
    let (pdf, _, _) =
        gradient_box("background-image: repeating-linear-gradient(red 10px, blue 10px)");
    let colors = stop_colors(&pdf, shadings(&pdf)[0]);
    assert!(
        colors.iter().all(|color| *color == [0, 0, 255]),
        "{colors:?}"
    );
}

#[test]
fn gradient_on_an_empty_box_draws_nothing() {
    let (pdf, operations, _) =
        gradient_box("height: 0; background-image: linear-gradient(red, blue)");
    assert!(shadings(&pdf).is_empty());
    assert_eq!(count(&operations, "f"), 0);
    // A painting area with an empty positioning area: no gradient box.
    let (pdf, _, _) = gradient_box(
        "height: 0; padding: 10px; background-origin: content-box; \
         background-image: linear-gradient(red, blue)",
    );
    assert!(shadings(&pdf).is_empty());
}

/// `/Coords` of a radial gradient on the test paragraph and the vertical
/// scale its ellipse applies (`ry / rx`).
fn radial(gradient: &str) -> (Vec<f32>, f32) {
    let (pdf, _, coords) = gradient_box(&format!("background-image: {gradient}"));
    let coords = coords.unwrap_or_else(|| panic!("no shading for {gradient}"));
    assert_eq!(coords.len(), 6, "{coords:?}");
    // The pattern matrix is the page transform (0.75, -0.75) times the
    // ellipse scale.
    let matrix = pdf
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .find(|dict| dict.has(b"PatternType"))
        .map(|dict| floats(dict.get(b"Matrix").unwrap()))
        .unwrap();
    (coords, matrix[3] / -matrix[0])
}

#[test]
fn radial_gradient_sizes_follow_the_extent_keywords() {
    // Center at (30, 10) in the 260 x 30 box: sides 30 / 230 px away
    // horizontally and 10 / 20 px vertically.
    let at = "at 30px 10px";
    let center = [50.0, 30.0];
    let corner = 30f32.hypot(10.0);
    let far_corner = 230f32.hypot(20.0);
    let root2 = std::f32::consts::SQRT_2;
    for (shape, radius, scale) in [
        ("circle closest-side", 10.0, 1.0),
        ("circle farthest-side", 230.0, 1.0),
        ("circle closest-corner", corner, 1.0),
        ("circle farthest-corner", far_corner, 1.0),
        ("closest-side", 30.0, 10.0 / 30.0),
        ("farthest-side", 230.0, 20.0 / 230.0),
        ("closest-corner", 30.0 * root2, 10.0 / 30.0),
        ("farthest-corner", 230.0 * root2, 20.0 / 230.0),
    ] {
        let (coords, got_scale) = radial(&format!("radial-gradient({shape} {at}, red, blue)"));
        assert_close(
            &coords,
            &[center[0], center[1], 0.0, center[0], center[1], radius],
        );
        assert!((got_scale - scale).abs() < 1e-3, "{shape}: {got_scale}");
    }
}

#[test]
fn radial_gradient_explicit_sizes_and_positions() {
    let (coords, scale) = radial("radial-gradient(circle 10px, red, blue)");
    assert_close(&coords, &[150.0, 35.0, 0.0, 150.0, 35.0, 10.0]);
    assert_eq!(scale, 1.0);
    let (coords, scale) = radial("radial-gradient(20px 10px, red, blue)");
    assert_eq!(coords[5], 20.0);
    assert!((scale - 0.5).abs() < 1e-4);
    // Percentages refer to the box: 50% of 260 and 25% of 30.
    let (coords, scale) = radial("radial-gradient(50% 25%, red, blue)");
    assert_eq!(coords[5], 130.0);
    assert!((scale - 7.5 / 130.0).abs() < 1e-4);
    // Offsets from the right and bottom edges.
    let (coords, _) = radial("radial-gradient(at right 10px bottom 5px, red, blue)");
    assert_close(&coords[..2], &[270.0, 45.0]);
}

#[test]
fn degenerate_radial_gradients_draw_their_last_color() {
    // A zero-size ending shape (CSS Images 3 §3.2.3): flat stops of the
    // last color.
    let (pdf, _, _) = gradient_box("background-image: radial-gradient(circle 0px, red, blue)");
    let colors = stop_colors(&pdf, shadings(&pdf)[0]);
    assert!(
        colors.iter().all(|color| *color == [0, 0, 255]),
        "{colors:?}"
    );
    // Its alpha is kept: half-transparent blue gets an opacity shading;
    // a transparent last color draws nothing.
    let (pdf, _) = operations(&format!(
        "{CSS}<p style=\"background-image: \
         radial-gradient(circle 0px, red, rgba(0, 0, 255, 0.5))\"></p>"
    ));
    assert_eq!(
        stop_colors(&pdf, color_shading(&pdf)),
        [[0, 0, 255], [0, 0, 255]]
    );
    assert!(shadings(&pdf).len() > 1, "an opacity shading");
    let (pdf, operations, _) =
        gradient_box("background-image: radial-gradient(circle 0px, red, transparent)");
    assert!(shadings(&pdf).is_empty());
    assert_eq!(count(&operations, "f"), 0);
    // Every stop before the center, where no radius reaches: one stop,
    // which Krilla draws as a plain color.
    let (pdf, operations, _) =
        gradient_box("background-image: radial-gradient(red -20px, blue -10px)");
    assert!(shadings(&pdf).is_empty());
    assert!(fill_colors(&operations).contains(&[0, 0, 255]));
}

#[test]
fn repeating_radial_gradient_repeats_to_the_farthest_corner() {
    let (pdf, _, coords) =
        gradient_box("background-image: repeating-radial-gradient(circle, red 0px, blue 10px)");
    let coords = coords.unwrap();
    // The farthest corner is hypot(130, 15) px from the center; the stops
    // repeat every 10px up to it.
    assert!(coords[5] >= 130f32.hypot(15.0), "{coords:?}");
    let colors = stop_colors(&pdf, shadings(&pdf)[0]);
    assert!(colors.len() >= 2 * 14, "{}", colors.len());
}

#[test]
fn conic_gradients_sweep_from_the_start_angle_around_the_center() {
    // The rotation, in degrees clockwise, that the pattern applies on top of
    // the page transform (0.75, flipped).
    let rotation = |pdf: &lopdf::Document| {
        let matrix = pdf
            .objects
            .values()
            .filter_map(|object| object.as_dict().ok())
            .find(|dict| dict.has(b"PatternType"))
            .map(|dict| floats(dict.get(b"Matrix").unwrap()))
            .unwrap();
        (-matrix[1]).atan2(matrix[0]).to_degrees()
    };
    let (pdf, _, _) = gradient_box(
        "background-image: conic-gradient(from 45deg at 25% 50%, red 10%, blue 90deg)",
    );
    assert!(has_shading(&pdf, 1));
    let code = postscript(&pdf);
    // Center (20 + 65, 20 + 15). The sweep is rotated to start at the
    // `from` angle (0deg points up, a quarter turn before the sweep's
    // positive x axis), so its domain is the stops' own angles: red at 36
    // and blue at 90 degrees, inside [0, 360) without crossing the wrap.
    assert!(code.contains("35.0 sub exch 85.0 sub"), "{code}");
    assert!(code.contains("dup 36.0 le"), "{code}");
    assert!(code.contains("dup 90.0 le"), "{code}");
    assert!(!code.contains("floor"), "{code}");
    assert!((rotation(&pdf) + 45.0).abs() < 1e-3, "{}", rotation(&pdf));
    // From 0deg: one full turn from the top.
    let (pdf, _, _) = gradient_box("background-image: conic-gradient(red, blue)");
    let code = postscript(&pdf);
    assert!(code.contains("dup 0.0 le"), "{code}");
    assert!(code.contains("dup 360.0 le"), "{code}");
    assert!((rotation(&pdf) + 90.0).abs() < 1e-3, "{}", rotation(&pdf));
    // Stops beyond the turn are cut to it.
    let (pdf, _, _) = gradient_box("background-image: conic-gradient(red -90deg, blue 450deg)");
    let code = postscript(&pdf);
    assert!(code.contains("dup 0.0 le"), "{code}");
    assert!(code.contains("dup 360.0 le"), "{code}");
    assert!(!code.contains("-90.0") && !code.contains("450.0"), "{code}");
    // A repeating conic repeats its 30 degree period in the function.
    let (pdf, _, _) =
        gradient_box("background-image: repeating-conic-gradient(red 0deg, blue 30deg)");
    let code = postscript(&pdf);
    assert!(code.contains("30.0 0.0"), "{code}");
    assert!(code.contains("floor"), "{code}");
}

#[test]
fn background_clip_selects_the_painting_area() {
    let fills = |clip: &str| {
        let (_, operations) = operations(&format!(
            "{CSS}<p style=\"border: 5px solid transparent; padding: 10px; \
             background-color: rgb(0, 128, 0); background-clip: {clip}\"></p>"
        ));
        // The background is the only fill: its path is the clip box.
        let fill = operations.iter().position(|op| op.operator == "f").unwrap();
        let start = operations[..fill]
            .iter()
            .rposition(|op| op.operator == "m")
            .unwrap();
        floats(&lopdf::Object::Array(operations[start].operands.clone()))
    };
    // The border box starts at (20, 20).
    assert_eq!(fills("border-box"), [20.0, 20.0]);
    assert_eq!(fills("padding-box"), [25.0, 25.0]);
    assert_eq!(fills("content-box"), [35.0, 35.0]);
    // A percentage padding has no containing block width here and counts as
    // zero, leaving the padding box.
    let (_, operations) = operations(&format!(
        "{CSS}<p style=\"border: 5px solid transparent; padding: 10%; \
         background-color: rgb(0, 128, 0); background-clip: content-box\"></p>"
    ));
    let first_move = operations.iter().find(|op| op.operator == "m").unwrap();
    assert_eq!(
        floats(&lopdf::Object::Array(first_move.operands.clone())),
        [25.0, 25.0]
    );
}

/// Dash arrays of every `d` operator.
fn dash_patterns(operations: &[lopdf::content::Operation]) -> Vec<Vec<f32>> {
    operations
        .iter()
        .filter(|op| op.operator == "d")
        .map(|op| floats(&op.operands[0]))
        .collect()
}

#[test]
fn rounded_dashed_and_dotted_borders_fit_the_outline() {
    for style in ["dotted", "dashed"] {
        let (_, operations) = operations(&format!(
            "{CSS}<p style=\"border: 4px {style} rgb(0, 0, 255); border-radius: 10px\"></p>"
        ));
        // Four identical sides: one stroke of the whole center line.
        let patterns = dash_patterns(&operations);
        assert_eq!(patterns.len(), 1, "{style}: {patterns:?}");
        assert_eq!(count(&operations, "S"), 1);
        assert_eq!(count(&operations, "c"), 4);
        // The center line is the 260 x 38 border box inset by 2px, with 8px
        // corners: 2 * (256 + 34) - 8 * 8 + 2 * pi * 8 px around.
        let perimeter = 2.0 * (256.0 + 34.0) - 64.0 + 2.0 * std::f32::consts::PI * 8.0;
        let period = patterns[0][0] + patterns[0][1];
        let repeats = perimeter / period;
        assert!(
            (repeats - repeats.round()).abs() < 1e-2,
            "{style}: {repeats}"
        );
        if style == "dotted" {
            assert_eq!(patterns[0][0], 0.0);
        } else {
            assert_eq!(patterns[0][0], patterns[0][1]);
        }
    }
    // Sides that differ are each stroked inside their own region and the
    // border ring.
    let (_, operations) = operations(&format!(
        "{CSS}<p style=\"border: 4px dashed rgb(0, 0, 255); border-left-color: red; \
         border-radius: 10px\"></p>"
    ));
    assert_eq!(dash_patterns(&operations).len(), 4);
    assert_eq!(count(&operations, "W*"), 4);
}

#[test]
fn a_dashed_side_too_short_for_two_dashes_is_one_dash() {
    let (_, operations) = operations(&format!(
        "{CSS}<p style=\"width: 4px; height: 4px; border: 2px dashed rgb(0, 0, 255)\"></p>"
    ));
    // Each side is 8px long and a dash is 6px: one dash over the whole side.
    let patterns = dash_patterns(&operations);
    assert_eq!(patterns.len(), 4);
    assert!(
        patterns.iter().all(|array| array == &[8.0, 0.0]),
        "{patterns:?}"
    );
}

#[test]
fn sides_with_different_colors_are_painted_in_their_own_regions() {
    let (_, operations) = operations(&format!(
        "{CSS}<p style=\"border: 4px solid rgb(255, 0, 0); border-top-color: rgb(0, 0, 255); \
         border-bottom-style: none\"></p>"
    ));
    // Three visible sides, each clipped to its corner-mitred region.
    assert_eq!(count(&operations, "W"), 3);
    assert_eq!(count(&operations, "f*"), 3);
    let colors = fill_colors(&operations);
    assert_eq!(colors.iter().filter(|c| **c == [0, 0, 255]).count(), 1);
    assert_eq!(colors.iter().filter(|c| **c == [255, 0, 0]).count(), 2);
    // The top region runs from the outer corners to the inner ones.
    assert_eq!(clip_bounds(&operations)[0], [20.0, 20.0, 280.0, 24.0]);
}

#[test]
fn double_borders_are_solid_below_three_pixels_and_split_per_side() {
    let (_, thin) = operations(&format!(
        "{CSS}<p style=\"border: 2px double rgb(0, 0, 255)\"></p>"
    ));
    assert_eq!(count(&thin, "f*"), 1);
    // Sides that differ in color: two lines per side, in four regions.
    let (_, mixed) = operations(&format!(
        "{CSS}<p style=\"border: 9px double rgb(0, 0, 255); border-top-color: red\"></p>"
    ));
    assert_eq!(count(&mixed, "W"), 4);
    assert_eq!(count(&mixed, "f*"), 8);
}

#[test]
fn three_d_borders_shade_by_side() {
    let colors = |style: &str| {
        let (_, operations) = operations(&format!(
            "{CSS}<p style=\"border: 4px {style} rgb(200, 200, 200)\"></p>"
        ));
        fill_colors(&operations)
    };
    let (dark, light) = ([100, 100, 100], [227, 227, 227]);
    // Sides are painted top, right, bottom, left.
    assert_eq!(colors("outset"), [light, dark, dark, light]);
    assert_eq!(colors("inset"), [dark, light, light, dark]);
    // Groove: outer half then inner half of each side.
    assert_eq!(
        colors("groove"),
        [dark, light, light, dark, light, dark, dark, light]
    );
    assert_eq!(
        colors("ridge"),
        [light, dark, dark, light, dark, light, light, dark]
    );
}

#[test]
fn clip_switches_between_sibling_overflow_boxes() {
    let child = "<div style=\"height: 60px; background-color: blue\">x</div>";
    let (_, operations) = operations(&format!(
        "{CSS}<div style=\"height: 20px; overflow: hidden\">{child}</div>\
         <div style=\"height: 20px; overflow: hidden\">{child}</div>\
         <p style=\"background-color: green\">after</p>"
    ));
    let bounds = clip_bounds(&operations);
    // Each subtree draws its box and text together, then switches to the
    // second clip after popping the first.
    assert_eq!(
        bounds,
        [[20.0, 20.0, 280.0, 40.0], [20.0, 40.0, 280.0, 60.0],]
    );
    // Every pushed clip is popped: the graphics states balance.
    assert_eq!(count(&operations, "q"), count(&operations, "Q"));
}

#[test]
fn overflow_clip_on_the_vertical_axis_leaves_the_horizontal_open() {
    let (_, operations) = operations(&format!(
        "{CSS}<div style=\"width: 50px; height: 40px; overflow-y: clip\">\
         <div style=\"width: 200px; height: 100px; background-color: blue\"></div></div>"
    ));
    assert_eq!(clip_bounds(&operations), [[0.0, 20.0, 300.0, 60.0]]);
}

#[test]
fn an_empty_overflow_box_hides_its_content() {
    let (_, operations) = operations(&format!(
        "{CSS}<div style=\"height: 0; overflow: hidden\">\
         <div style=\"height: 50px; background-color: blue\"></div></div>"
    ));
    // The clip still exists, around no area at all.
    let bounds = clip_bounds(&operations);
    assert_eq!(bounds.len(), 1);
    assert!(bounds[0][2] - bounds[0][0] <= 0.0 || bounds[0][3] - bounds[0][1] <= 0.0);
}

#[test]
fn generated_content_is_clipped_by_its_element() {
    let (pdf, operations) = operations(
        "<style>@page { size: 300px 200px; margin: 20px } body { margin: 0 } \
         div::before { content: 'gen' }</style>\
         <div style=\"height: 20px; overflow: hidden\"></div>",
    );
    let text = pdf.extract_text(&[1]).unwrap();
    assert_eq!(text.split_whitespace().collect::<Vec<_>>(), ["gen"]);
    let clip = operations.iter().position(|op| op.operator == "W").unwrap();
    let text_start = operations
        .iter()
        .position(|op| op.operator == "BT")
        .unwrap();
    assert!(clip < text_start);
}

/// The `/ColorSpace /DeviceRGB` shading of a translucent gradient (the
/// opacity goes to a separate gray shading in a soft mask).
fn color_shading(pdf: &lopdf::Document) -> &lopdf::Dictionary {
    let color: Vec<_> = shadings(pdf)
        .into_iter()
        .filter(|dict| {
            dict.get(b"ColorSpace")
                .is_ok_and(|space| space.as_name().is_ok_and(|name| name == b"DeviceRGB"))
        })
        .collect();
    assert_eq!(color.len(), 1);
    color[0]
}

#[test]
fn translucent_gradients_interpolate_in_premultiplied_alpha() {
    // Toward `transparent` (transparent black) the color stays red while
    // only the opacity fades: no dark fringe.
    let (pdf, _) = operations(&format!(
        "{CSS}<p style=\"background-image: linear-gradient(to right, red, transparent)\"></p>"
    ));
    assert_eq!(
        stop_colors(&pdf, color_shading(&pdf)),
        [[255, 0, 0], [255, 0, 0]]
    );
    assert!(shadings(&pdf).len() > 1, "an opacity shading");
    // A transparent stop between two colors takes each neighbor's color on
    // its side.
    let (pdf, _) = operations(&format!(
        "{CSS}<p style=\"background-image: \
         linear-gradient(to right, red, transparent, blue)\"></p>"
    ));
    assert_eq!(
        stop_colors(&pdf, color_shading(&pdf)),
        [[255, 0, 0], [255, 0, 0], [0, 0, 255], [0, 0, 255]]
    );
    // Two partial opacities: the color between them is sampled.
    let (pdf, _) = operations(&format!(
        "{CSS}<p style=\"background-image: linear-gradient(to right, \
         rgba(255, 0, 0, 1), rgba(0, 0, 255, 0.2))\"></p>"
    ));
    let colors = stop_colors(&pdf, color_shading(&pdf));
    assert_eq!(colors.len(), 9, "{colors:?}");
    // Halfway, red weighs 1 against blue's 0.2: 255 * 0.5 / 0.6 = 212.5
    // and 255 * 0.1 / 0.6 = 42.5.
    let [r, g, b] = colors[4];
    assert!(
        r.abs_diff(212) <= 1 && g == 0 && b.abs_diff(42) <= 1,
        "{colors:?}"
    );
}

#[test]
fn double_border_fallback_uses_each_side_width() {
    let (_, operations) = operations(&format!(
        "{CSS}<p style=\"border-style: double; border-width: 2px 6px 6px 6px; \
         border-color: rgb(0, 0, 255)\"></p>"
    ));
    // The 2px top side is one solid ring; the 6px sides two lines each.
    assert_eq!(count(&operations, "W"), 4);
    assert_eq!(count(&operations, "f*"), 1 + 3 * 2);
}

#[test]
fn body_overflow_clips_when_the_root_overflow_is_not_visible() {
    let html = |root: &str| {
        format!(
            "<style>@page {{ size: 300px 200px; margin: 20px }} html {{ {root} }} \
             body {{ margin: 0; border: 5px solid black; overflow: hidden }}</style>\
             <p style=\"background-color: blue; height: 100px\"></p>"
        )
    };
    // With a visible root, the body's overflow goes to the page.
    let (_, propagated) = operations(&html(""));
    assert_eq!(count(&propagated, "W"), 0);
    // Otherwise the root's goes to the page and the body clips its own
    // content to its padding box: its border box inset by the border.
    let (_, clipped) = operations(&html("overflow: hidden"));
    let (_dir, path) = input(&html("overflow: hidden"));
    let layout = completed(&path);
    let page = layout.page(0).unwrap();
    let body = page
        .fragments()
        .find(|f| page.dom().local_name(f.node()) == Some("body"))
        .unwrap()
        .paint_rect();
    assert_eq!(body, raikiri_html::PaintRect::new(20.0, 20.0, 260.0, 160.0));
    // The body's content-box minimum is 160px; its two 5px borders make
    // the whole box 170px tall. The page cuts the visible fragment, while
    // the overflow clip keeps the whole padding box below the page cut.
    assert_eq!(clip_bounds(&clipped), [[25.0, 25.0, 275.0, 185.0]]);
}

/// Decoded operations of one page's content stream.
fn page_operations(pdf: &lopdf::Document, page: u32) -> Vec<lopdf::content::Operation> {
    let id = *pdf.get_pages().get(&page).unwrap();
    lopdf::content::Content::decode(&pdf.get_page_content(id).unwrap())
        .unwrap()
        .operations
}

/// The y coordinates of every curve point.
fn curve_ys(operations: &[lopdf::content::Operation]) -> Vec<f32> {
    operations
        .iter()
        .filter(|op| op.operator == "c")
        .flat_map(|op| {
            floats(&lopdf::Object::Array(op.operands.clone()))
                .into_iter()
                .skip(1)
                .step_by(2)
        })
        .collect()
}

#[test]
fn a_box_broken_across_pages_has_no_corners_or_borders_at_the_break() {
    let (_dir, path) = input(&format!(
        "{CSS}<div style=\"height: 250px; border: 6px solid black; border-radius: 20px; \
         background-color: rgb(0, 128, 0)\"></div>"
    ));
    let pdf = lopdf::Document::load_mem(&render(&path, &Config::default()).unwrap()).unwrap();
    assert_eq!(pdf.get_pages().len(), 2);
    let first = page_operations(&pdf, 1);
    let second = page_operations(&pdf, 2);
    // The first fragment fills the page area (y 20 to 180) and keeps only
    // its top corners; the second (from y 20) only its bottom ones.
    let ys = curve_ys(&first);
    assert!(!ys.is_empty() && ys.iter().all(|y| *y <= 40.0), "{ys:?}");
    let ys = curve_ys(&second);
    assert!(!ys.is_empty() && ys.iter().all(|y| *y >= 60.0), "{ys:?}");
    // Three bordered sides on each page: no border along the break.
    for operations in [&first, &second] {
        assert_eq!(count(operations, "W"), 3);
        assert_eq!(count(operations, "f*"), 3);
    }
}

fn first_page_geometry(path: &Path, config: &Config) -> (f32, f32, [f32; 4]) {
    let LayoutStatus::Completed(result) =
        layout_file(path, config, LayoutConfig::default()).unwrap()
    else {
        panic!("expected a completed layout");
    };
    let geometry = result.page(0).unwrap().geometry();
    let m = geometry.margins;
    (
        geometry.page_box.width,
        geometry.page_box.height,
        [m.top, m.right, m.bottom, m.left],
    )
}

fn assert_near(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.01,
        "expected {expected}, got {actual}"
    );
}

fn assert_margins(actual: [f32; 4], expected: [f32; 4]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert_near(actual, expected);
    }
}

/// 20mm, Fulgur's default margin, in CSS px.
const DEFAULT_MARGIN_PX: f32 = 20.0 * 96.0 / 25.4;

#[test]
fn config_supplies_page_size_and_margins_without_page_rules() {
    let (_dir, path) = input("<p>Hello</p>");
    let (width, height, margins) = first_page_geometry(&path, &Config::default());
    assert_near(width, 210.0 * 96.0 / 25.4);
    assert_near(height, 297.0 * 96.0 / 25.4);
    assert_margins(margins, [DEFAULT_MARGIN_PX; 4]);

    let config = Config {
        landscape: true,
        margin: Margin::symmetric(36.0, 18.0),
        ..Config::default()
    };
    let (width, height, margins) = first_page_geometry(&path, &config);
    assert_near(width, 297.0 * 96.0 / 25.4);
    assert_near(height, 210.0 * 96.0 / 25.4);
    assert_margins(margins, [48.0, 24.0, 48.0, 24.0]);
}

#[test]
fn document_page_rules_win_over_config_defaults() {
    let (_dir, path) = input(&format!("{CSS}<p>Hello</p>"));
    let (width, height, margins) = first_page_geometry(&path, &Config::default());
    assert_eq!((width, height), (300.0, 200.0));
    assert_margins(margins, [20.0; 4]);
}

#[test]
fn explicitly_set_config_fields_win_over_document_page_rules() {
    let (_dir, path) = input(&format!("{CSS}<p>Hello</p>"));
    let config = Config::builder()
        .page_size(PageSize::A5)
        .margin(Margin::uniform(30.0))
        .build();
    let (width, height, margins) = first_page_geometry(&path, &config);
    assert_near(width, 148.0 * 96.0 / 25.4);
    assert_near(height, 210.0 * 96.0 / 25.4);
    assert_margins(margins, [40.0; 4]);

    // Only the margin is set: the document's size still applies.
    let config = Config::builder().margin(Margin::uniform(30.0)).build();
    let (width, height, margins) = first_page_geometry(&path, &config);
    assert_eq!((width, height), (300.0, 200.0));
    assert_margins(margins, [40.0; 4]);
}

fn paragraph_height(path: &Path) -> f32 {
    let result = completed(path);
    let page = result.page(0).unwrap();
    page.fragments()
        .find(|f| page.dom().local_name(f.node()) == Some("p"))
        .unwrap()
        .rect()
        .height
}

#[test]
fn linked_and_imported_stylesheets_next_to_the_input_apply() {
    let (dir, path) = input(
        "<link rel=stylesheet href=style.css><link rel=stylesheet href='nested/more.css'><p>Hello</p>",
    );
    std::fs::write(dir.path().join("style.css"), "p { height: 41px }").unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    std::fs::write(
        dir.path().join("nested/more.css"),
        "@import url('../late.css');",
    )
    .unwrap();
    std::fs::write(dir.path().join("late.css"), "p { padding-top: 2px }").unwrap();
    let result = completed(&path);
    let page = result.page(0).unwrap();
    let paragraph = page
        .fragments()
        .find(|f| page.dom().local_name(f.node()) == Some("p"))
        .unwrap();
    assert_eq!(paragraph.rect().height, 43.0);
}

#[test]
fn stylesheets_outside_the_input_directory_are_not_read() {
    let outer = tempfile::tempdir().unwrap();
    std::fs::write(outer.path().join("secret.css"), "p { height: 77px }").unwrap();
    let inner = outer.path().join("doc");
    std::fs::create_dir(&inner).unwrap();
    let path = inner.join("input.html");
    let absolute = url::Url::from_file_path(outer.path().join("secret.css")).unwrap();
    std::fs::write(
        &path,
        format!(
            "<link rel=stylesheet href=../secret.css><link rel=stylesheet href='{absolute}'><p style='margin:0'>Hello</p>"
        ),
    )
    .unwrap();
    assert_ne!(paragraph_height(&path), 77.0);
}

#[test]
fn local_files_get_a_content_type_from_their_extension() {
    assert_eq!(files::content_type(Path::new("a/b.CSS")), Some("text/css"));
    assert_eq!(
        files::content_type(Path::new("x.woff2")),
        Some("font/woff2")
    );
    assert_eq!(
        files::content_type(Path::new("photo.jpeg")),
        Some("image/jpeg")
    );
    assert_eq!(files::content_type(Path::new("README")), None);
    assert_eq!(files::content_type(Path::new("data.bin")), None);
}

fn get(provider: &files::BaseDirectoryProvider, url: &str) -> std::result::Result<(), String> {
    use raikiri_traits::ResourceKind;
    use raikiri_traits::net::{Body, Method, NetworkProvider, Request};
    provider
        .fetch_one_hop(Request {
            url: url::Url::parse(url).unwrap(),
            method: Method::Get,
            content_type: None,
            headers: Vec::new(),
            body: Body::Empty,
            signal: None,
            kind: ResourceKind::ExternalStylesheet,
        })
        .map(drop)
        .map_err(|error| error.to_string())
}

#[test]
fn provider_refuses_other_schemes_and_hosted_file_urls() {
    let (_dir, path) = input("<p>Hello</p>");
    let provider = files::BaseDirectoryProvider::for_input(&path).unwrap();
    let document = provider.document_url(&path).unwrap();
    assert!(get(&provider, document.as_str()).is_ok());
    let error = get(&provider, "https://example.com/style.css").unwrap_err();
    assert!(error.contains("only file://"), "{error}");
    let error = get(&provider, "file://example.com/style.css").unwrap_err();
    assert!(error.contains("invalid file URL"), "{error}");
    let localhost = document.as_str().replacen("file://", "file://localhost", 1);
    assert!(get(&provider, &localhost).is_ok());
}

#[test]
fn provider_refuses_files_over_the_resource_limit() {
    let (dir, path) = input("<p>Hello</p>");
    let provider = files::BaseDirectoryProvider::for_input(&path)
        .unwrap()
        .with_max_bytes(4);
    std::fs::write(dir.path().join("small.css"), "p{}").unwrap();
    std::fs::write(dir.path().join("large.css"), "p { }").unwrap();
    let url = |name: &str| {
        url::Url::from_file_path(dir.path().canonicalize().unwrap().join(name)).unwrap()
    };
    assert!(get(&provider, url("small.css").as_str()).is_ok());
    let error = get(&provider, url("large.css").as_str()).unwrap_err();
    assert!(error.contains("exceeds 4 bytes"), "{error}");
}

#[test]
fn invalid_config_is_rejected_before_layout() {
    let (_dir, path) = input("<p>Hello</p>");
    let config = Config {
        margin: Margin::uniform(f32::NAN),
        ..Config::default()
    };
    assert!(render(&path, &config).is_err());
}

#[test]
fn landscape_only_override_keeps_the_document_page_size() {
    let (_dir, path) = input(&format!("{CSS}<p>Hello</p>"));
    let config = Config::builder().landscape(true).build();
    let (width, height, _) = first_page_geometry(&path, &config);
    assert_eq!((width, height), (300.0, 200.0));
}

#[test]
fn input_path_without_a_file_name_is_a_layout_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("..");
    let provider = files::BaseDirectoryProvider::for_input(&dir.path().join("x.html")).unwrap();
    assert!(matches!(
        provider.document_url(&path),
        Err(Error::Layout(_))
    ));
}

/// Whether every straight segment of every filled path is horizontal or
/// vertical (clip paths, such as the mitred side regions, are skipped).
fn filled_segments_are_axis_aligned(operations: &[lopdf::content::Operation]) -> bool {
    let mut current = (0.0, 0.0);
    let mut diagonal = false;
    for op in operations {
        let values: Vec<f32> = op
            .operands
            .iter()
            .filter_map(|value| value.as_float().ok())
            .collect();
        match op.operator.as_str() {
            "m" => current = (values[0], values[1]),
            "l" => {
                let next = (values[0], values[1]);
                if (next.0 - current.0).abs() > 1e-3 && (next.1 - current.1).abs() > 1e-3 {
                    diagonal = true;
                }
                current = next;
            }
            "c" => current = (values[4], values[5]),
            "f" | "f*" if diagonal => return false,
            "f" | "f*" | "n" => diagonal = false,
            _ => {}
        }
    }
    true
}

#[test]
fn a_corner_with_one_zero_inner_radius_is_square() {
    // The 12px top border exceeds the 10px radius, so the inner top
    // corners have a zero vertical radius: square (CSS Backgrounds 3 §5.1),
    // not a diagonal cut across the padding area.
    let (_, operations) = operations(&format!(
        "{CSS}<p style=\"border-style: solid; border-width: 12px 2px 2px 2px; \
         border-color: rgb(0, 0, 255); border-radius: 10px; \
         background-color: rgb(0, 128, 0); background-clip: padding-box\"></p>"
    ));
    assert!(count(&operations, "c") > 0);
    assert!(
        filled_segments_are_axis_aligned(&operations),
        "{operations:?}"
    );
}

#[test]
fn background_clip_text_paints_inside_the_glyphs() {
    let (pdf, text_ops) = operations(&format!(
        "{CSS}<p style=\"background-image: linear-gradient(red, blue); \
         background-clip: text; color: transparent\">Hi</p>"
    ));
    assert!(has_shading(&pdf, 2));
    // The gradient fills glyph outlines (curves), not the 260 x 30 box.
    let fill = text_ops.iter().position(|op| op.operator == "f").unwrap();
    let path = &text_ops[..fill];
    let start = path.iter().position(|op| op.operator == "m").unwrap();
    assert!(count(&path[start..], "c") > 4, "{path:?}");
    let xs: Vec<f32> = path[start..]
        .iter()
        .flat_map(|op| {
            floats(&lopdf::Object::Array(op.operands.clone()))
                .into_iter()
                .step_by(2)
        })
        .collect();
    let right = xs.iter().copied().fold(f32::MIN, f32::max);
    assert!(right < 60.0, "{right}");
    // Without text there is nothing to paint.
    let (_, empty) = operations(&format!(
        "{CSS}<p style=\"background-color: rgb(255, 0, 0); background-clip: text\"></p>"
    ));
    assert!(!fill_colors(&empty).contains(&[255, 0, 0]));
}

#[test]
fn background_clip_border_area_paints_under_the_border() {
    let (_, operations) = operations(&format!(
        "{CSS}<p style=\"border: 5px solid transparent; background-color: rgb(0, 128, 0); \
         background-clip: border-area\"></p>"
    ));
    assert!(fill_colors(&operations).contains(&[0, 128, 0]));
    // The ring between the border and padding edges, filled even-odd.
    assert_eq!(count(&operations, "f*"), 1);
    assert_eq!(count(&operations, "f"), 0);
}

mod assets_tests;

mod metadata_tests;

mod navigation_tests;

mod bookmark_tests;

mod order_tests;

mod decoration_tests;

#[test]
fn margin_boxes_draw_their_backgrounds_borders_and_text_on_each_page() {
    let (pdf, operations) = operations(
        "<style>@page { size: 300px 200px; margin: 40px; \
           @bottom-center { content: 'p.' counter(page) } } \
         @page :first { @top-center { content: 'FIRST'; background-color: rgb(0, 0, 255); \
           border-bottom: 2px solid rgb(0, 255, 0) } @bottom-center { content: none } \
           @left-middle { content: ''; border: 1px solid rgb(255, 0, 0) } } \
         body { margin: 0 } div { height: 120px }</style>\
         <div>one</div><div>two</div>",
    );
    assert_eq!(pdf.get_pages().len(), 2);
    let colors = fill_colors(&operations);
    assert!(colors.contains(&[0, 0, 255]), "{colors:?}");
    assert!(colors.contains(&[0, 255, 0]), "{colors:?}");
    // The empty left-middle box still draws its four border sides.
    assert_eq!(
        colors.iter().filter(|color| **color == [255, 0, 0]).count(),
        4,
        "{colors:?}"
    );
    let first = pdf.extract_text(&[1]).expect("extractable text");
    assert_eq!(
        first.split_whitespace().collect::<Vec<_>>(),
        ["FIRST", "one"]
    );
    let second = pdf.extract_text(&[2]).expect("extractable text");
    assert_eq!(
        second.split_whitespace().collect::<Vec<_>>(),
        ["p.2", "two"]
    );
    // The margin box text is clipped to its border box (the 220 x 40 px
    // top-center strip of the first page).
    assert!(
        clip_bounds(&operations).iter().any(|bounds| bounds
            .iter()
            .zip([40.0, 0.0, 260.0, 40.0])
            .all(|(value, expected)| (value - expected).abs() < 0.01)),
        "{:?}",
        clip_bounds(&operations)
    );
}
