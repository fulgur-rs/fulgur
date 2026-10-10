use super::*;

const LINKED: &str = "<link rel=stylesheet href=page.css><p>Hello</p>";
const PAGE_CSS: &str = "@page { size: 300px 200px; margin: 20px }";

fn media_box(bytes: &[u8]) -> Vec<f32> {
    let pdf = lopdf::Document::load_mem(bytes).unwrap();
    let page = pdf.get_dictionary(pdf.get_pages()[&1]).unwrap();
    page.get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_float().unwrap())
        .collect()
}

fn page_count(bytes: &[u8]) -> usize {
    lopdf::Document::load_mem(bytes).unwrap().get_pages().len()
}

#[test]
fn html_string_matches_the_same_file() {
    let html = format!("{CSS}<p>A</p><p style='break-before:page'>B</p>");
    let (_dir, path) = input(&html);
    let from_file = render(&path, &Config::default()).unwrap();
    let from_string = render_html(&html, None, &Config::default()).unwrap();
    assert_eq!(page_count(&from_string), 2);
    assert_eq!(page_count(&from_string), page_count(&from_file));
    assert_eq!(media_box(&from_string), media_box(&from_file));
}

#[test]
fn html_string_resolves_relative_urls_against_the_base_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("page.css"), PAGE_CSS).unwrap();
    let bytes = render_html(LINKED, Some(dir.path()), &Config::default()).unwrap();
    assert_eq!(media_box(&bytes), [0.0, 0.0, 225.0, 150.0]);
}

#[test]
fn html_string_without_a_base_directory_reads_no_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("page.css"), PAGE_CSS).unwrap();
    let absolute =
        url::Url::from_file_path(dir.path().canonicalize().unwrap().join("page.css")).unwrap();
    let with_base = render_html(LINKED, Some(dir.path()), &Config::default()).unwrap();
    for html in [
        LINKED.to_string(),
        format!("<link rel=stylesheet href='{absolute}'><p>Hello</p>"),
    ] {
        let bytes = render_html(&html, None, &Config::default()).unwrap();
        assert_ne!(media_box(&bytes), media_box(&with_base), "{html}");
    }
}

#[test]
fn html_string_with_a_missing_base_directory_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    assert!(render_html("<p>Hello</p>", Some(&missing), &Config::default()).is_err());
}

#[test]
fn html_string_fragment_links_stay_internal_without_a_base_directory() {
    let html = "<style>@page {size:300px 200px; margin:0} body {margin:0} a {display:block;width:40px;height:10px}</style><div id=target>T</div><a href='#target'></a>";
    let bytes = render_html(html, None, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let page = pdf.get_dictionary(pdf.get_pages()[&1]).unwrap();
    let annots = page.get(b"Annots").unwrap().as_array().unwrap();
    assert_eq!(annots.len(), 1);
    let link = pdf
        .get_dictionary(annots[0].as_reference().unwrap())
        .unwrap();
    let action = link.get(b"A").unwrap().as_dict().unwrap();
    assert_eq!(action.get(b"S").unwrap().as_name().unwrap(), b"GoTo");
}
