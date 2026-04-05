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

#[test]
fn test_gcpm_left_right_margin_boxes() {
    let css = r#"
        @page {
            margin: 72pt;
            @left-middle { content: "Left Side"; font-size: 8px; }
            @right-middle { content: "Page " counter(page); font-size: 8px; }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <p>Body content with left and right margin boxes.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("should render with left/right margin boxes");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_gcpm_all_side_margin_boxes() {
    let css = r#"
        @page {
            margin: 72pt;
            @left-top { content: "LT"; }
            @left-middle { content: "LM"; }
            @left-bottom { content: "LB"; }
            @right-top { content: "RT"; }
            @right-middle { content: "RM"; }
            @right-bottom { content: "RB"; }
            @top-center { content: "Page " counter(page); }
            @bottom-center { content: "Footer"; }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <p>Body content with all margin box positions.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("should render with all side margin boxes");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_gcpm_left_right_with_running_element() {
    let css = r#"
        .sidebar-label { position: running(sideLabel); }
        @page {
            margin: 72pt;
            @left-top { content: element(sideLabel); }
            @right-bottom { content: "Page " counter(page) " / " counter(pages); font-size: 8px; }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <div class="sidebar-label">Chapter 1</div>
  <p>Content of chapter 1.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("should render left/right with running elements");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}

/// Regression: same running element on both sides with asymmetric margins
/// exercises the height_cache width-dependent key and per-side measurement.
#[test]
fn test_gcpm_side_boxes_asymmetric_margins() {
    let css = r#"
        .sidebar-label { position: running(sideLabel); }
        @page {
            margin-top: 72pt;
            margin-right: 144pt;
            margin-bottom: 72pt;
            margin-left: 36pt;
            @left-middle { content: element(sideLabel) " - " counter(page); font-size: 8px; }
            @right-middle { content: element(sideLabel) " - " counter(page); font-size: 8px; }
        }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <div class="sidebar-label">A very long chapter label that should wrap differently on each side</div>
  <p>Body content with asymmetric side margins.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("should render side boxes with asymmetric widths and mixed content");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF-"));
}
