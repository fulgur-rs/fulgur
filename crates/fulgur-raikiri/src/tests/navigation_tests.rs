use super::*;

fn annotations(pdf: &lopdf::Document, page: u32) -> Vec<&lopdf::Dictionary> {
    let page = pdf.get_dictionary(pdf.get_pages()[&page]).unwrap();
    page.get(b"Annots")
        .ok()
        .map(|value| {
            value
                .as_array()
                .unwrap()
                .iter()
                .map(|object| pdf.get_dictionary(object.as_reference().unwrap()).unwrap())
                .collect()
        })
        .unwrap_or_default()
}

fn destination<'a>(
    pdf: &'a lopdf::Document,
    annotation: &lopdf::Dictionary,
) -> &'a Vec<lopdf::Object> {
    let action = annotation.get(b"A").unwrap().as_dict().unwrap();
    assert_eq!(action.get(b"S").unwrap().as_name().unwrap(), b"GoTo");
    pdf.get_object(action.get(b"D").unwrap().as_reference().unwrap())
        .unwrap()
        .as_array()
        .unwrap()
}

const NAV_CSS: &str = "<style>@page {size:300px 200px; margin:0} body {margin:0} a {display:block;position:absolute;left:20px;top:40px;width:40px;height:10px} #target {position:absolute;left:20px;top:40px;width:40px;height:10px}</style>";

#[test]
fn navigation_external_and_internal_links() {
    let (_dir, path) = input(&format!(
        "{NAV_CSS}<div id=target></div><a href='https://example.com/report'></a><a href='#target'></a>"
    ));
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let links = annotations(&pdf, 1);
    assert_eq!(links.len(), 2);
    let external = links
        .iter()
        .find(|a| {
            a.get(b"A")
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"S")
                .unwrap()
                .as_name()
                .unwrap()
                == b"URI"
        })
        .unwrap();
    assert_eq!(
        external
            .get(b"A")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"URI")
            .unwrap()
            .as_str()
            .unwrap(),
        b"https://example.com/report"
    );
    let rect = external.get(b"Rect").unwrap().as_array().unwrap();
    let values: [f32; 4] = std::array::from_fn(|i| rect[i].as_float().unwrap());
    assert_eq!(values, [15.0, 112.5, 45.0, 120.0]);
    let internal = links
        .iter()
        .find(|a| {
            a.get(b"A")
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"S")
                .unwrap()
                .as_name()
                .unwrap()
                == b"GoTo"
        })
        .unwrap();
    let dest = destination(&pdf, internal);
    assert_eq!(dest[0].as_reference().unwrap(), pdf.get_pages()[&1]);
    assert_eq!(dest[1].as_name().unwrap(), b"XYZ");
    assert_eq!(dest[2].as_float().unwrap(), 15.0);
    assert_eq!(dest[3].as_float().unwrap(), 120.0);
}

#[test]
fn navigation_multiline_and_escaped_anchor() {
    let (_dir, path) = input(
        "<style>@page {size:300px 200px;margin:0} body {margin:0} p {margin:0; font-size:10px;line-height:10px}</style><p><a href='#section%20name'>One<br>Two</a></p><div id='section name' style='break-before:page;height:20px'></div><div id='section name' style='margin-left:50px'>Duplicate</div>",
    );
    let document = completed(&path);
    let page = document.page(0).unwrap();
    let quads: usize = page.links().map(|link| link.quads.len()).sum();
    assert_eq!(quads, 2);
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert_eq!(pdf.get_pages().len(), 2);
    let links = annotations(&pdf, 1);
    assert_eq!(links.len(), 1);
    assert_eq!(
        links[0]
            .get(b"QuadPoints")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        16
    );
    for link in links {
        let dest = destination(&pdf, link);
        assert_eq!(dest[0].as_reference().unwrap(), pdf.get_pages()[&2]);
        assert_eq!(dest[2].as_float().unwrap(), 0.0);
        assert_eq!(dest[3].as_float().unwrap(), 150.0);
    }
}

#[test]
fn navigation_relative_uri_uses_document_base() {
    let (dir, path) = input(&format!(
        "{NAV_CSS}<a href='reports/next.html?x=1#part'></a>"
    ));
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let links = annotations(&pdf, 1);
    assert_eq!(links.len(), 1);
    let uri = links[0]
        .get(b"A")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"URI")
        .unwrap()
        .as_str()
        .unwrap();
    let expected =
        url::Url::from_file_path(dir.path().canonicalize().unwrap().join("reports/next.html"))
            .unwrap();
    assert_eq!(String::from_utf8_lossy(uri), format!("{expected}?x=1#part"));
}

#[test]
fn navigation_missing_anchor_and_empty_quads_are_omitted() {
    let (_dir, path) = input(&format!(
        "{NAV_CSS}<a href='#missing'></a><a href='https://example.com' style='width:0;height:0'></a><a href='#%FF'></a>"
    ));
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(annotations(&pdf, 1).is_empty());
}

#[test]
fn navigation_same_document_url_forms_use_internal_destinations() {
    let (_dir, path) = input("");
    let absolute = url::Url::from_file_path(path.canonicalize().unwrap()).unwrap();
    let filename = path.file_name().unwrap().to_str().unwrap();
    let html = format!(
        "<style>@page {{size:300px 200px;margin:0}} body {{margin:0}} a {{display:block;width:40px;height:10px}}</style><a href='{filename}#section%20name'></a><a href='./{filename}#section%20name'></a><a href='{absolute}#section%20name'></a><div id='section name' style='break-before:page;height:20px'></div>"
    );
    std::fs::write(&path, html).unwrap();
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert_eq!(pdf.get_pages().len(), 2);
    let links = annotations(&pdf, 1);
    assert_eq!(links.len(), 3);
    for link in links {
        let dest = destination(&pdf, link);
        assert_eq!(dest[0].as_reference().unwrap(), pdf.get_pages()[&2]);
        assert_eq!(dest[2].as_float().unwrap(), 0.0);
        assert_eq!(dest[3].as_float().unwrap(), 150.0);
    }
}

#[test]
fn navigation_missing_same_document_anchor_is_omitted() {
    let (_dir, path) = input("");
    let filename = path.file_name().unwrap().to_str().unwrap();
    std::fs::write(&path, format!("{NAV_CSS}<a href='{filename}#missing'></a><a href='{filename}?other=1#target'></a><div id='target'></div>")).unwrap();
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let links = annotations(&pdf, 1);
    assert_eq!(links.len(), 1);
    let action = links[0].get(b"A").unwrap().as_dict().unwrap();
    assert_eq!(action.get(b"S").unwrap().as_name().unwrap(), b"URI");
    let uri = String::from_utf8_lossy(action.get(b"URI").unwrap().as_str().unwrap());
    assert!(uri.ends_with("?other=1#target"));
}

#[test]
fn navigation_base_href_resolves_links_before_classification() {
    for base in ["other.html", "https://example.com/other.html"] {
        let (_dir, path) = input("");
        let original = url::Url::from_file_path(path.canonicalize().unwrap()).unwrap();
        std::fs::write(&path, format!("<head><base href='{base}'></head>{NAV_CSS}<div id=target></div><a href='#target'></a><a href='other.html#target'></a><a href='{original}#target'></a>")).unwrap();
        let bytes = render(&path, &Config::default()).unwrap();
        let pdf = lopdf::Document::load_mem(&bytes).unwrap();
        let links = annotations(&pdf, 1);
        assert_eq!(links.len(), 3);
        let expected = original.join(base).unwrap().join("#target").unwrap();
        for link in &links[..2] {
            let action = link.get(b"A").unwrap().as_dict().unwrap();
            assert_eq!(action.get(b"S").unwrap().as_name().unwrap(), b"URI");
            assert_eq!(
                String::from_utf8_lossy(action.get(b"URI").unwrap().as_str().unwrap()),
                expected.as_str()
            );
        }
        let dest = destination(&pdf, links[2]);
        assert_eq!(dest[0].as_reference().unwrap(), pdf.get_pages()[&1]);
        assert_eq!(dest[2].as_float().unwrap(), 15.0);
        assert_eq!(dest[3].as_float().unwrap(), 120.0);
    }
}

#[test]
fn navigation_multiline_uses_one_annotation_with_all_quads() {
    let (_dir, path) = input(
        "<style>@page {size:300px 200px;margin:0} body {margin:0} p {margin:0;font-size:10px;line-height:10px}</style><p><a href='https://example.com/'>One<br>Two</a></p>",
    );
    let document = completed(&path);
    let count: usize = document
        .page(0)
        .unwrap()
        .links()
        .map(|link| link.quads.len())
        .sum();
    assert_eq!(count, 2);
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let links = annotations(&pdf, 1);
    assert_eq!(links.len(), 1);
    let points = links[0].get(b"QuadPoints").unwrap().as_array().unwrap();
    assert_eq!(points.len(), 16);
    assert!(
        points
            .iter()
            .all(|point| point.as_float().unwrap().is_finite())
    );
}

#[test]
fn links_in_running_elements_are_placed_in_their_margin_box() {
    let (_dir, path) = input(
        "<style>@page {size:300px 200px; margin:40px; @top-center {content: element(hdr)}} \
         body {margin:0} .hdr {position: running(hdr)} \
         .hdr a {display:block; width:20px; height:10px}</style>\
         <div class='hdr'><a href='https://example.com/header'></a></div><p>Body</p>",
    );
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let links = annotations(&pdf, 1);
    assert_eq!(links.len(), 1);
    let action = links[0].get(b"A").unwrap().as_dict().unwrap();
    assert_eq!(
        action.get(b"URI").unwrap().as_str().unwrap(),
        b"https://example.com/header"
    );
    let rect = links[0].get(b"Rect").unwrap().as_array().unwrap();
    let [x0, y0, x1, y1]: [f32; 4] = std::array::from_fn(|i| rect[i].as_float().unwrap());
    // 20px x 10px, inside the top margin (PDF y from 120pt to 150pt).
    assert_eq!((x1 - x0, y1 - y0), (15.0, 7.5));
    assert!(x0 >= 0.0 && x1 <= 225.0);
    assert!(y0 >= 120.0 && y1 <= 150.0, "{y0}..{y1}");
}

#[test]
fn links_of_running_elements_outside_their_margin_box_are_dropped() {
    let (_dir, path) = input(
        "<style>@page {size:300px 200px; margin:40px; @top-center {content: element(hdr)}} \
         body {margin:0} .hdr {position: running(hdr)} \
         .hdr a {display:block; width:20px; height:10px; margin-top:500px}</style>\
         <div class='hdr'><a href='https://example.com/header'></a></div><p>Body</p>",
    );
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    // The link lies below the margin box, where its content is clipped away.
    assert!(annotations(&pdf, 1).is_empty());
}

#[test]
fn links_of_running_elements_to_missing_anchors_are_dropped() {
    let (_dir, path) = input(
        "<style>@page {size:300px 200px; margin:40px; @top-center {content: element(hdr)}} \
         body {margin:0} .hdr {position: running(hdr)} \
         .hdr a {display:block; width:20px; height:10px}</style>\
         <div class='hdr'><a href='#nowhere'></a></div><p>Body</p>",
    );
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(annotations(&pdf, 1).is_empty());
}
