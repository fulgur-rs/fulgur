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
    let bytes = crate::paint::paint_document(
        &document,
        &raikiri_html::RenderResources::new(),
        &fulgur_core::Config::default(),
        Some(outline(&document, collector)),
        &url::Url::from_file_path(path.canonicalize().unwrap()).unwrap(),
        &crate::RenderOptions::default(),
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
