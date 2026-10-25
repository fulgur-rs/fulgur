use super::*;

#[test]
fn collector_clears_none_labels_and_ignores_unregistered_properties() {
    let mut collector = BookmarkCollector::default();
    let node = NodeId(42);
    for (property, value) in [
        ("bookmark-level", ConsumerPropertyValue::Integer(1)),
        (
            "bookmark-label",
            ConsumerPropertyValue::Text("Title".into()),
        ),
        (
            "unregistered",
            ConsumerPropertyValue::Text("Unrelated".into()),
        ),
    ] {
        collector
            .observe_event(ConsumerPropertyEvent::new(node, None, 0, property, value))
            .unwrap();
    }
    assert_eq!(collector.headings[&node].level, Some(1));
    assert_eq!(collector.headings[&node].label.as_deref(), Some("Title"));
    collector
        .observe_event(ConsumerPropertyEvent::new(
            node,
            None,
            0,
            "bookmark-label",
            ConsumerPropertyValue::None,
        ))
        .unwrap();
    assert_eq!(collector.headings[&node].label, None);
    assert_eq!(collector.headings[&node].level, Some(1));
}

#[test]
fn missing_resolved_label_omits_outline_without_losing_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.html");
    std::fs::write(&path, "<h1>Title</h1>").unwrap();
    let raikiri_html::LayoutStatus::Completed(document) = crate::layout_file(
        &path,
        &fulgur_core::Config::default(),
        raikiri_html::LayoutConfig::default(),
    )
    .unwrap() else {
        panic!("completed layout");
    };
    let page = document.page(0).unwrap();
    let node = page
        .fragments()
        .find(|fragment| page.dom().local_name(fragment.node()) == Some("h1"))
        .unwrap()
        .node();
    let mut collector = BookmarkCollector::default();
    collector
        .observe_event(ConsumerPropertyEvent::new(
            node,
            None,
            0,
            "bookmark-level",
            ConsumerPropertyValue::Integer(1),
        ))
        .unwrap();
    let (bytes, _) = crate::paint::paint_document(
        &document,
        &raikiri_html::RenderResources::new(),
        &fulgur_core::Config::default(),
        Some(outline(&document, collector).outline),
        &url::Url::from_file_path(path.canonicalize().unwrap()).unwrap(),
    )
    .unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(!pdf.catalog().unwrap().has(b"Outlines"));
    assert_eq!(
        pdf.extract_text(&[1])
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["Title"]
    );
}

#[test]
fn open_state_rejects_unexpected_pdf_structure() {
    let pdf = |objects: &[&str]| raw::tests::pdf_with(objects, "/Root 1 0 R");
    let outline = [
        "<</Type/Catalog/Outlines 2 0 R>>",
        "<</First 3 0 R/Last 3 0 R/Count 1>>",
        "<</Title(x)/Parent 2 0 R/First 4 0 R/Last 4 0 R/Count -1>>",
        "<</Title(y)/Parent 3 0 R>>",
    ];
    // Opening the parent rewrites both counts.
    let patched = apply_open_state(pdf(&outline), &[true, false]).unwrap();
    let raw = raw::RawPdf::new(&patched).unwrap();
    assert_eq!(
        raw::get(&raw.dictionary(2).unwrap(), b"Count"),
        Some(&raw::Value::Integer(2))
    );
    assert_eq!(
        raw::get(&raw.dictionary(3).unwrap(), b"Count"),
        Some(&raw::Value::Integer(1))
    );
    // Counts already right leave the bytes alone.
    assert_eq!(
        apply_open_state(patched.clone(), &[true, false]).unwrap(),
        patched
    );

    let mut no_outline = outline;
    no_outline[0] = "<</Type/Catalog>>";
    let mut no_count = outline;
    no_count[2] = "<</Title(x)/Parent 2 0 R/First 4 0 R/Last 4 0 R>>";
    let mut dangling = outline;
    dangling[3] = "<</Title(y)/Parent 3 0 R/Next 9 0 R>>";
    for (bytes, open) in [
        (b"not a pdf".to_vec(), &[true, false][..]),
        (pdf(&no_outline), &[true, false]),
        (pdf(&no_count), &[true, false]),
        (pdf(&dangling), &[true, false, true]),
        // Fewer states than entries: not the outline that was written.
        (pdf(&outline), &[true]),
    ] {
        assert!(apply_open_state(bytes, open).is_err());
    }
}
