use fulgur::asset::AssetBundle;
use fulgur::engine::Engine;

#[test]
fn test_gcpm_header_footer_generates_pdf() {
    let css = r#"
        .header { position: running(pageHeader); }
        .footer { position: running(pageFooter); }
        @page {
            @top-center { content: element(pageHeader); }
            @bottom-center { content: element(pageFooter) " - " counter(page) " / " counter(pages); }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <div class="header">Document Header</div>
  <div class="footer">Document Footer</div>
  <p>Body content for the document.</p>
  <p>Second paragraph of content.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(html).unwrap();

    assert!(!pdf.is_empty(), "PDF output should not be empty");
    assert!(
        pdf.starts_with(b"%PDF-"),
        "PDF output should start with %PDF-"
    );
}

#[test]
fn test_gcpm_no_gcpm_css_works_as_before() {
    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <h1>Simple Document</h1>
  <p>This is a simple document with no GCPM CSS.</p>
</body>
</html>"#;

    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).unwrap();

    assert!(!pdf.is_empty(), "PDF output should not be empty");
    assert!(
        pdf.starts_with(b"%PDF-"),
        "PDF output should start with %PDF-"
    );
}

#[test]
fn test_gcpm_multipage_counter() {
    let css = r#"
        @page {
            @bottom-center { content: "Page " counter(page) " of " counter(pages); }
        }
    "#;

    let mut paragraphs = String::new();
    for i in 0..100 {
        paragraphs.push_str(&format!(
            "<p>Paragraph {} with enough text to take up space on the page.</p>\n",
            i + 1
        ));
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head></head>
<body>
{}
</body>
</html>"#,
        paragraphs
    );

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(&html).unwrap();

    assert!(!pdf.is_empty(), "PDF output should not be empty");
    assert!(
        pdf.starts_with(b"%PDF-"),
        "PDF output should start with %PDF-"
    );
}

#[test]
fn test_gcpm_counter_only_no_running() {
    let css = r#"
        @page {
            @bottom-center { content: "Page " counter(page); }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <p>Simple body text with page counter only, no running elements.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine.render_html(html).unwrap();

    assert!(!pdf.is_empty(), "PDF output should not be empty");
    assert!(
        pdf.starts_with(b"%PDF-"),
        "PDF output should start with %PDF-"
    );
}

#[test]
fn test_deterministic_output() {
    let css = r#"
        .header { position: running(pageHeader); }
        @page {
            @top-left { content: element(pageHeader); }
            @top-right { content: "Page " counter(page) " / " counter(pages); font-size: 8px; }
            @bottom-center { content: "Footer"; }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html><body>
  <div class="header">Title</div>
  <p>Content.</p>
</body></html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);
    let engine = Engine::builder().assets(assets).build();
    let pdf1 = engine.render_html(html).unwrap();

    let mut assets2 = AssetBundle::new();
    assets2.add_css(css);
    let engine2 = Engine::builder().assets(assets2).build();
    let pdf2 = engine2.render_html(html).unwrap();

    assert_eq!(pdf1, pdf2, "Same input must produce identical PDF output");
}

#[test]
fn test_gcpm_id_selector_running_element() {
    let css = r#"
        #doc-title { position: running(pageTitle); }
        @page { @top-center { content: element(pageTitle); } }
    "#;
    let html = r#"<!DOCTYPE html>
    <html><body>
      <div id="doc-title">My Document</div>
      <p>Body content</p>
    </body></html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("should render with ID selector running element");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_gcpm_tag_selector_running_element() {
    let css = r#"
        header { position: running(pageHeader); }
        @page { @top-center { content: element(pageHeader); } }
    "#;
    let html = r#"<!DOCTYPE html>
    <html><body>
      <header>Document Header</header>
      <p>Body content</p>
    </body></html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("should render with tag selector running element");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}
