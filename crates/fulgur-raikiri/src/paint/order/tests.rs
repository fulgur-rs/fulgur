use super::*;
use raikiri_html::{GeneratedKind, LayoutConfig, LayoutStatus, PaintEvent, RunSource};

fn document() -> raikiri_html::DocumentLayout {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.html");
    std::fs::write(&path, "<p>Hello</p>").unwrap();
    let LayoutStatus::Completed(document) = crate::layout_file(
        &path,
        &fulgur_core::Config::default(),
        LayoutConfig::default(),
    )
    .unwrap() else {
        panic!("completed layout");
    };
    document
}

#[test]
fn ordinary_text_is_supported() {
    let document = document();
    let page = document.page(0).unwrap();
    assert!(supported(&page.paint_order(), &page.text_runs()));
}

#[test]
fn duplicate_text_events_are_not_drawn_twice() {
    let document = document();
    let page = document.page(0).unwrap();
    let mut events = page.paint_order();
    let text = events
        .iter()
        .find(|event| matches!(event, PaintEvent::Text(_)))
        .copied()
        .unwrap();
    events.push(text);
    assert!(!supported(&events, &page.text_runs()));
}

#[test]
fn missing_text_event_uses_legacy_page() {
    let document = document();
    let page = document.page(0).unwrap();
    let events: Vec<_> = page
        .paint_order()
        .into_iter()
        .filter(|event| !matches!(event, PaintEvent::Text(_)))
        .collect();
    assert!(!supported(&events, &page.text_runs()));
}

#[test]
fn generated_run_uses_legacy_page() {
    let document = document();
    let page = document.page(0).unwrap();
    let mut runs = page.text_runs();
    let RunSource::Text(node) = runs[0].source else {
        panic!("text run");
    };
    runs[0].source = RunSource::Generated(node, GeneratedKind::Before);
    assert!(!supported(&page.paint_order(), &runs));
}
