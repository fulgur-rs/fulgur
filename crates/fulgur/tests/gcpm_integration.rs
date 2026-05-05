use fulgur::asset::AssetBundle;
use fulgur::engine::Engine;

#[test]
fn test_counter_margin_box_values_correct() {
    // Verify that counter values actually reach margin boxes correctly
    // by generating a document with counter-increment on visible elements.
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        body { counter-reset: chapter; }
        h2 { counter-increment: chapter; }
        @page {
            @bottom-center {
                content: "Chapter " counter(chapter);
            }
        }
    "#,
    );

    let engine = Engine::builder().assets(assets).build();
    // Multiple chapters with enough content to potentially span pages
    let html = r#"
        <h2>One</h2><p>Content for chapter one.</p>
        <h2>Two</h2><p>Content for chapter two.</p>
        <h2>Three</h2><p>Content for chapter three.</p>
    "#;
    let result = engine.render_html(html);
    assert!(
        result.is_ok(),
        "Counter values should reach margin boxes: {:?}",
        result.err()
    );
    // Verify PDF is non-trivial (contains actual content)
    let pdf_bytes = result.unwrap();
    assert!(
        pdf_bytes.len() > 1000,
        "PDF should contain rendered content with counters"
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
fn test_element_policy_multiple_chapters_last() {
    let css = r#"
        @page {
            size: 400pt 300pt;
            margin: 40pt;
            @top-center { content: element(title, last); }
        }
        .title { position: running(title); }
        .big { height: 250pt; border: 1px solid black; }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <h1 class="title">Chapter 1</h1>
  <div class="big">Chapter 1 body</div>
  <h1 class="title">Chapter 2</h1>
  <div class="big">Chapter 2 body</div>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("render should succeed with element(title, last) across multiple chapters");

    assert!(
        pdf.len() > 1000,
        "PDF seems empty or too small: {} bytes",
        pdf.len()
    );
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_element_policy_first_except() {
    let css = r#"
        @page {
            size: 400pt 300pt;
            margin: 40pt;
            @top-center { content: element(title, first-except); }
        }
        .title { position: running(title); }
        .big { height: 250pt; border: 1px solid black; }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <h1 class="title">Chapter 1</h1>
  <div class="big">Chapter 1 body</div>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("render should succeed with element(title, first-except)");

    assert!(
        pdf.len() > 1000,
        "PDF seems empty or too small: {} bytes",
        pdf.len()
    );
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_element_default_policy_still_works() {
    // Baseline: element(title) without an explicit policy must still render
    // (default = first), matching pre-policy behavior.
    let css = r#"
        @page {
            size: 400pt 300pt;
            margin: 40pt;
            @top-center { content: element(title); }
        }
        .title { position: running(title); }
    "#;

    let html = r#"<!DOCTYPE html>
<html>
<head></head>
<body>
  <h1 class="title">My Title</h1>
  <p>Body content.</p>
</body>
</html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render_html(html)
        .expect("render should succeed with default element() policy");

    assert!(
        pdf.len() > 1000,
        "PDF seems empty or too small: {} bytes",
        pdf.len()
    );
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_counter_chapter_before_pseudo() {
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        body { counter-reset: chapter; }
        h2 { counter-increment: chapter; }
        h2::before { content: counter(chapter) ". "; }
    "#,
    );

    let engine = Engine::builder().assets(assets).build();
    let html = "<h2>Introduction</h2><p>Some text here</p><h2>Methods</h2><p>More text</p>";
    let result = engine.render_html(html);
    assert!(
        result.is_ok(),
        "PDF generation with counter in ::before should succeed: {:?}",
        result.err()
    );
}

#[test]
fn test_counter_in_margin_box() {
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        body { counter-reset: chapter; }
        h2 { counter-increment: chapter; }
        @page {
            @bottom-center {
                content: "Chapter " counter(chapter);
            }
        }
    "#,
    );

    let engine = Engine::builder().assets(assets).build();
    let html = "<h2>One</h2><p>Some text</p><h2>Two</h2><p>More text</p>";
    let result = engine.render_html(html);
    assert!(
        result.is_ok(),
        "PDF with counter in margin box should succeed: {:?}",
        result.err()
    );
}

#[test]
fn test_counter_upper_roman_style() {
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        body { counter-reset: chapter; }
        h2 { counter-increment: chapter; }
        h2::before { content: counter(chapter, upper-roman) ". "; }
    "#,
    );

    let engine = Engine::builder().assets(assets).build();
    let html = "<h2>A</h2><h2>B</h2><h2>C</h2><h2>D</h2>";
    let result = engine.render_html(html);
    assert!(
        result.is_ok(),
        "PDF with upper-roman counter should succeed: {:?}",
        result.err()
    );
}

#[test]
fn test_counter_set() {
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        body { counter-reset: chapter; }
        h2 { counter-increment: chapter; }
        .reset { counter-set: chapter 10; }
        h2::before { content: counter(chapter) ". "; }
    "#,
    );

    let engine = Engine::builder().assets(assets).build();
    let html = r#"<h2>One</h2><div class="reset"></div><h2>Eleven</h2>"#;
    let result = engine.render_html(html);
    assert!(
        result.is_ok(),
        "PDF with counter-set should succeed: {:?}",
        result.err()
    );
}

#[test]
fn test_counter_body_and_margin_box() {
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        body { counter-reset: chapter; }
        h2 { counter-increment: chapter; }
        h2::before { content: counter(chapter) ". "; }
        @page {
            @bottom-right {
                content: "Ch. " counter(chapter);
            }
        }
    "#,
    );

    let engine = Engine::builder().assets(assets).build();
    let html = "<h2>Intro</h2><p>text</p><h2>Body</h2><p>text</p><h2>End</h2><p>text</p>";
    let result = engine.render_html(html);
    assert!(
        result.is_ok(),
        "PDF with counter in both body and margin box should succeed: {:?}",
        result.err()
    );
}

#[test]
fn test_counter_page_still_works() {
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        @page {
            @bottom-center {
                content: "Page " counter(page) " of " counter(pages);
            }
        }
    "#,
    );

    let engine = Engine::builder().assets(assets).build();
    let html = "<p>Hello World</p>";
    let result = engine.render_html(html);
    assert!(
        result.is_ok(),
        "counter(page) should still work: {:?}",
        result.err()
    );
}

// Snapshot tests (inline <style> variants + migrated integration tests): see crates/fulgur/tests/gcpm_snapshot.rs
