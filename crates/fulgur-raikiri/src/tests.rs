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
    assert!(matches!(draw(status), Err(Error::Layout(_))));
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

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.01,
        "expected {expected}, got {actual}"
    );
}

fn assert_margins(actual: [f32; 4], expected: [f32; 4]) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert_close(actual, expected);
    }
}

/// 20mm, Fulgur's default margin, in CSS px.
const DEFAULT_MARGIN_PX: f32 = 20.0 * 96.0 / 25.4;

#[test]
fn config_supplies_page_size_and_margins_without_page_rules() {
    let (_dir, path) = input("<p>Hello</p>");
    let (width, height, margins) = first_page_geometry(&path, &Config::default());
    assert_close(width, 210.0 * 96.0 / 25.4);
    assert_close(height, 297.0 * 96.0 / 25.4);
    assert_margins(margins, [DEFAULT_MARGIN_PX; 4]);

    let config = Config {
        landscape: true,
        margin: Margin::symmetric(36.0, 18.0),
        ..Config::default()
    };
    let (width, height, margins) = first_page_geometry(&path, &config);
    assert_close(width, 297.0 * 96.0 / 25.4);
    assert_close(height, 210.0 * 96.0 / 25.4);
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
    assert_close(width, 148.0 * 96.0 / 25.4);
    assert_close(height, 210.0 * 96.0 / 25.4);
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
