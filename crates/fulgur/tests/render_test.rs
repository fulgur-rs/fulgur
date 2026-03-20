use fulgur::config::{Config, Margin, PageSize};
use fulgur::pageable::{BlockPageable, Pageable, SpacerPageable};
use fulgur::render::render_to_pdf;

#[test]
fn test_render_empty_pdf() {
    let config = Config::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build();

    let root = BlockPageable::new(vec![]);
    let pdf = render_to_pdf(Box::new(root), &config).unwrap();

    // PDF should start with %PDF header
    assert!(pdf.starts_with(b"%PDF"));
    // Should be non-trivially sized
    assert!(pdf.len() > 100);
}

#[test]
fn test_render_multipage_pdf() {
    let config = Config::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build();

    let content_height = config.content_height();

    // Create content taller than one page
    let mut spacers: Vec<Box<dyn fulgur::pageable::Pageable>> = Vec::new();
    let spacer_height = content_height / 3.0;
    for _ in 0..7 {
        let mut s = SpacerPageable::new(spacer_height);
        s.wrap(100.0, 1000.0);
        spacers.push(Box::new(s));
    }

    let root = BlockPageable::new(spacers);
    let pdf = render_to_pdf(Box::new(root), &config).unwrap();

    assert!(pdf.starts_with(b"%PDF"));
}
