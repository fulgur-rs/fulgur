use super::*;

/// Enough input for the streaming layout to deliver pages before the end:
/// `pages` paragraphs, one per page, then a comment that pushes the input
/// past the first layout checkpoint, then one more paragraph.
fn long_input(css: &str, pages: usize, body: impl Fn(usize) -> String) -> String {
    let mut html = format!("<html><head>{css}</head><body>");
    for index in 0..pages {
        html.push_str(&body(index));
    }
    html.push_str("<!--");
    html.push_str(&"x".repeat(70 * 1024));
    html.push_str("-->");
    html.push_str(&body(pages));
    html.push_str("</body></html>");
    html
}

const PAGE_CSS: &str = "<style>@page { size: 300px 200px; margin: 40px } \
    body { margin: 0 } p { margin: 0; break-after: page }</style>";

fn both(html: &str, config: &Config) -> (Vec<u8>, Vec<u8>) {
    let (_dir, path) = input(html);
    let batch = render(&path, config).expect("batch PDF");
    let streamed =
        render_streaming(&path, config, &RenderOptions::default()).expect("streamed PDF");
    (batch, streamed)
}

/// The text of the form XObject that a merged page draws below its body.
fn margin_text(pdf: &lopdf::Document, page_number: u32) -> String {
    let page_id = pdf.get_pages()[&page_number];
    let page = pdf.get_dictionary(page_id).unwrap();
    let resources = match page.get(b"Resources").unwrap() {
        lopdf::Object::Reference(id) => pdf.get_dictionary(*id).unwrap(),
        object => object.as_dict().unwrap(),
    };
    let form_id = resources
        .get(b"XObject")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"FulgurMarginBoxes")
        .unwrap()
        .as_reference()
        .unwrap();
    let form = pdf.get_object(form_id).unwrap().as_stream().unwrap();
    // Show the form as the page's own content so the text extractor reads it.
    let mut copy = pdf.clone();
    let content = copy.add_object(lopdf::Stream::new(
        lopdf::Dictionary::new(),
        form.decompressed_content().unwrap(),
    ));
    let copied = copy.get_dictionary_mut(page_id).unwrap();
    copied.set("Contents", lopdf::Object::Reference(content));
    copied.set("Resources", form.dict.get(b"Resources").unwrap().clone());
    copy.extract_text(&[page_number]).unwrap()
}

#[test]
fn streaming_a_short_document_matches_the_batch_pdf() {
    let html = format!("{PAGE_CSS}<p>one <a href='https://example.com/'>link</a></p><p>two</p>");
    let (batch, streamed) = both(&html, &Config::default());
    assert_eq!(batch, streamed);
}

#[test]
fn pages_delivered_early_match_the_batch_pdf() {
    let html = long_input(PAGE_CSS, 4, |index| {
        format!("<p>page {index} <a href='https://example.com/{index}'>link</a></p>")
    });
    let (batch, streamed) = both(&html, &Config::default());
    assert_eq!(batch, streamed);
}

#[test]
fn early_pages_show_the_real_page_count() {
    let css = "<style>@page { size: 300px 200px; margin: 40px; \
        @bottom-center { content: counter(page) ' of ' counter(pages) } } \
        body { margin: 0 } p { margin: 0; break-after: page }</style>";
    let html = long_input(css, 4, |index| format!("<p>page {index}</p>"));
    let (batch, streamed) = both(&html, &Config::default());
    let batch = lopdf::Document::load_mem(&batch).unwrap();
    let pdf = lopdf::Document::load_mem(&streamed).unwrap();
    let page_count = batch.get_pages().len();
    assert_eq!(pdf.get_pages().len(), page_count);
    let mut merged = 0;
    for number in 1..=page_count as u32 {
        let footer = format!("{number} of {page_count}");
        let body = pdf.extract_text(&[number]).unwrap();
        assert!(!body.contains("99999"), "{body:?}");
        if body.contains(&footer) {
            continue;
        }
        merged += 1;
        let text = margin_text(&pdf, number);
        assert_eq!(
            text.split_whitespace().collect::<Vec<_>>().join(" "),
            footer
        );
    }
    assert!(merged > 0, "some pages were delivered early");
    let npages = format!("<xmpTPg:NPages>{page_count}</xmpTPg:NPages>");
    assert!(
        streamed
            .windows(npages.len())
            .any(|window| window == npages.as_bytes()),
        "the metadata counts the merged pages only"
    );
}

/// The pages that the link annotations of page `number` go to.
fn destinations(pdf: &lopdf::Document, number: u32) -> Vec<lopdf::ObjectId> {
    let page = pdf.get_dictionary(pdf.get_pages()[&number]).unwrap();
    let Ok(annotations) = page.get(b"Annots") else {
        return Vec::new();
    };
    annotations
        .as_array()
        .unwrap()
        .iter()
        .map(|annotation| {
            let annotation = pdf
                .get_dictionary(annotation.as_reference().unwrap())
                .unwrap();
            let action = annotation.get(b"A").unwrap().as_dict().unwrap();
            let destination = pdf
                .get_object(action.get(b"D").unwrap().as_reference().unwrap())
                .unwrap()
                .as_array()
                .unwrap();
            destination[0].as_reference().unwrap()
        })
        .collect()
}

#[test]
fn links_to_later_anchors_resolve_on_early_pages() {
    let html = long_input(PAGE_CSS, 4, |index| match index {
        0 => "<p><a href='#end'>to the end</a></p>".to_owned(),
        4 => "<p id=end><a href='#start'>back</a></p>".to_owned(),
        _ => format!(
            "<p id={}>page {index}</p>",
            if index == 1 { "start" } else { "x" }
        ),
    });
    let (batch, streamed) = both(&html, &Config::default());
    let batch = lopdf::Document::load_mem(&batch).unwrap();
    let pdf = lopdf::Document::load_mem(&streamed).unwrap();
    let pages = pdf.get_pages();
    assert_eq!(pages.len(), batch.get_pages().len());
    let last = pages.len() as u32;
    assert_eq!(destinations(&pdf, 1), [pages[&last]]);
    assert_eq!(destinations(&pdf, last), [pages[&2]]);
    // The batch PDF links the same pages.
    let batch_pages = batch.get_pages();
    assert_eq!(destinations(&batch, 1), [batch_pages[&last]]);
}

#[test]
fn streamed_bookmarks_match_the_batch_outline() {
    let html = long_input(PAGE_CSS, 4, |index| {
        format!(
            "<h{} style='break-after: page'>Heading {index}</h{0}>",
            1 + index % 2
        )
    });
    let config = Config::builder().bookmarks(true).build();
    let (batch, streamed) = both(&html, &config);
    assert_eq!(batch, streamed);
}

#[test]
fn streaming_reports_a_missing_input() {
    let dir = tempfile::tempdir().unwrap();
    let result = render_streaming(
        &dir.path().join("missing.html"),
        &Config::default(),
        &RenderOptions::default(),
    );
    assert!(result.is_err());
}

#[test]
fn streaming_rejects_pdf_ua() {
    let (_dir, path) = input("<p>Body</p>");
    let config = Config::builder().pdf_ua(true).build();
    let error = render_streaming(&path, &config, &RenderOptions::default())
        .expect_err("PDF/UA needs the whole document");
    assert!(error.to_string().contains("streaming"), "{error}");
}

#[test]
fn links_in_running_elements_to_later_anchors_resolve_on_early_pages() {
    let css = "<style>@page { size: 300px 200px; margin: 40px; \
        @top-center { content: element(hdr) } } \
        body { margin: 0 } p { margin: 0; break-after: page } \
        .hdr { position: running(hdr) } \
        .hdr a { display: block; width: 20px; height: 10px }</style>";
    let html = long_input(css, 3, |index| match index {
        0 => "<div class=hdr><a href='#end'></a></div><p>page 0</p>".to_owned(),
        3 => "<p id=end>end</p>".to_owned(),
        _ => format!("<p>page {index}</p>"),
    });
    let (_dir, path) = input(&html);
    let streamed = render_streaming(&path, &Config::default(), &RenderOptions::default())
        .expect("streamed PDF");
    let pdf = lopdf::Document::load_mem(&streamed).unwrap();
    let pages = pdf.get_pages();
    let last = pages[&(pages.len() as u32)];
    assert_eq!(destinations(&pdf, 1), [last]);
}

#[test]
fn streaming_reports_layout_errors() {
    let deep = "<div>".repeat(300);
    let filler = format!("<!--{}-->", "x".repeat(70 * 1024));
    // Too deep from the start, and too deep only after a checkpoint has
    // passed, so that a later chunk fails while being fed.
    for html in [deep.clone(), format!("{filler}{deep}{filler}{filler}")] {
        let (_dir, path) = input(&html);
        let batch = render(&path, &Config::default()).expect_err("too deep for the batch layout");
        let streamed = render_streaming(&path, &Config::default(), &RenderOptions::default())
            .expect_err("too deep for the streaming layout");
        assert_eq!(streamed.to_string(), batch.to_string());
    }
}
