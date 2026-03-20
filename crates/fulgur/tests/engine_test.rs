use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;
use fulgur::pageable::{BlockPageable, Pageable, SpacerPageable};

#[test]
fn test_engine_render_pageable() {
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .title("Test Document")
        .build();

    let mut s = SpacerPageable::new(100.0);
    s.wrap(100.0, 1000.0);
    let root = BlockPageable::new(vec![Box::new(s)]);

    let pdf = engine.render_pageable(Box::new(root)).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
