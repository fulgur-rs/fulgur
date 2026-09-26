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
    assert!(matches!(
        layout_file(&path, config).unwrap(),
        LayoutStatus::Aborted
    ));
}

#[test]
fn render_reports_pdf_drawing_unavailable() {
    let (_dir, path) = input("<p>Hello</p>");
    match render(&path) {
        Err(Error::PdfGeneration(message)) => {
            assert_eq!(message, "Raikiri PDF drawing is not implemented")
        }
        other => panic!("expected PDF drawing error, got {other:?}"),
    }
}

#[test]
fn render_preserves_input_io_error() {
    let dir = tempfile::tempdir().unwrap();
    match render(&dir.path().join("missing.html")) {
        Err(Error::Io(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected IO error, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_input_filename_is_accepted() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"input-\xff.html".to_vec()));
    std::fs::write(&path, "<p>Hello</p>").unwrap();
    assert_eq!(completed(&path).page_count(), 1);
    assert!(matches!(render(&path), Err(Error::PdfGeneration(_))));
}
