use fulgur_wpt::render::render_test;
use std::io::Write;

fn poppler_available() -> bool {
    std::process::Command::new("pdftocairo")
        .arg("-v")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn renders_all_pages_from_overflow() {
    if !poppler_available() {
        eprintln!("skip: pdftocairo not available");
        return;
    }

    // NOTE: fulgur currently paginates via natural flow overflow (not
    // `page-break-after` CSS), so we size the page small and emit more
    // paragraphs than fit on one page to force a >=2-page render.
    let mut body = String::new();
    for i in 1..=40 {
        body.push_str(&format!(
            "<p>Paragraph {i} lorem ipsum dolor sit amet.</p>\n"
        ));
    }
    let html = format!(
        r#"<!DOCTYPE html>
<html><head><style>
  @page {{ size: 300pt 200pt; margin: 0; }}
</style></head>
<body>
{body}
</body></html>"#
    );

    let dir = tempfile::tempdir().unwrap();
    let html_path = dir.path().join("t.html");
    std::fs::File::create(&html_path)
        .unwrap()
        .write_all(html.as_bytes())
        .unwrap();

    let work = dir.path().join("work");
    let out = render_test(&html_path, &work, 96, None).expect("render should succeed");
    assert!(
        out.pages.len() >= 2,
        "expected >=2 pages (got {})",
        out.pages.len()
    );
    // 300pt @ 96dpi ≈ 400px wide — sanity check the raster resolution.
    assert!(out.pages[0].width() > 100);
    assert!(out.pdf_path.exists());
}
