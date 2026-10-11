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

#[test]
fn invalid_opacity_is_rejected_before_surface_stack_changes() {
    for alpha in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert!(!supported(
            &[PaintEvent::PushOpacity(alpha), PaintEvent::PopOpacity],
            &[]
        ));
    }
    for alpha in [0.0, 1.0] {
        assert!(supported(
            &[PaintEvent::PushOpacity(alpha), PaintEvent::PopOpacity],
            &[]
        ));
    }
}

#[test]
fn line_events_include_generated_and_ellipsis_runs() {
    let document = document();
    let page = document.page(0).unwrap();
    let mut runs = page.text_runs();
    let RunSource::Text(node) = runs[0].source else {
        panic!("text run")
    };
    let events = page.paint_order_for_text_runs(&runs);
    assert!(supported(&events, &runs));
    runs[0].source = RunSource::Generated(node, GeneratedKind::Before);
    assert!(supported(&events, &runs));
    runs[0].source = RunSource::Ellipsis(node);
    assert!(supported(&events, &runs));
}

#[test]
fn duplicate_missing_and_unknown_lines_are_rejected() {
    let document = document();
    let page = document.page(0).unwrap();
    let runs = page.text_runs();
    let events = page.paint_order_for_text_runs(&runs);
    let line = events
        .iter()
        .find(|event| matches!(event, PaintEvent::TextLine(_)))
        .copied()
        .unwrap();
    let mut duplicate = events.clone();
    duplicate.push(line);
    assert!(!supported(&duplicate, &runs));
    let mut missing = events.clone();
    missing.retain(|event| !matches!(event, PaintEvent::TextLine(_)));
    assert!(!supported(&missing, &runs));
    let mut unknown_line = runs[0].line;
    unknown_line.index += 100;
    let mut unknown = events;
    unknown.push(PaintEvent::TextLine(unknown_line));
    assert!(!supported(&unknown, &runs));
}

#[test]
fn mixed_fragment_and_line_events_are_rejected() {
    let document = document();
    let page = document.page(0).unwrap();
    let runs = page.text_runs();
    let mut events = page.paint_order_for_text_runs(&runs);
    events.extend(
        page.paint_order()
            .into_iter()
            .filter(|event| matches!(event, PaintEvent::Text(_))),
    );
    assert!(!supported(&events, &runs));
}
