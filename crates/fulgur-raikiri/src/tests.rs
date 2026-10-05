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
