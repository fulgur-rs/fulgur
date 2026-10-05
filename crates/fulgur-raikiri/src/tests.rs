use super::*;
use fulgur_core::Error;
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
    match layout_file(path, LayoutConfig::default()).unwrap() {
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
    let status = layout_file(&path, config).unwrap();
    assert!(matches!(status, LayoutStatus::Aborted));
    // An aborted layout has nothing to draw.
    assert!(matches!(draw(status), Err(Error::Layout(_))));
}

#[test]
fn render_writes_one_pdf_page_per_layout_page() {
    let (_dir, path) = input(&format!(
        "{CSS}<p style=\"background-color: rgb(0, 128, 0); border: 2px solid red\">one</p>\
         <p style=\"break-before: page\">two</p>"
    ));
    let bytes = render(&path).expect("PDF bytes");
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
    match render(&dir.path().join("missing.html")) {
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
    assert!(render(&path).is_ok_and(|bytes| bytes.starts_with(b"%PDF")));
}

#[test]
fn render_draws_extractable_text_with_one_font_per_face() {
    let (_dir, path) = input(&format!(
        "{CSS}<p>Hello <b>bold</b> world</p><p style=\"color: rgb(200, 0, 0)\">second line</p>"
    ));
    let bytes = render(&path).expect("PDF bytes");
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
    let bytes = render(&path).expect("PDF bytes");
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

/// Render `html` and decode the drawing operations of every content stream:
/// page contents and Form XObjects (Krilla draws some fills, such as
/// translucent ones, through those).
fn operations(html: &str) -> (lopdf::Document, Vec<lopdf::content::Operation>) {
    let (_dir, path) = input(html);
    let bytes = render(&path).expect("PDF bytes");
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
