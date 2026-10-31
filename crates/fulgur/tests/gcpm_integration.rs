use fulgur::asset::AssetBundle;
use fulgur::engine::Engine;

#[test]
fn test_counter_margin_box_values_correct() {
    // A document counter referenced from a margin box must not break the
    // render. Its value there is 0 (the page context does not see document
    // counters); `margin_box_document_counter_resolves_to_zero` pins that.
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
    let result = engine.render(html);
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
    let pdf = engine.render(html).unwrap();

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
    let pdf1 = engine.render(html).unwrap();

    let mut assets2 = AssetBundle::new();
    assets2.add_css(css);
    let engine2 = Engine::builder().assets(assets2).build();
    let pdf2 = engine2.render(html).unwrap();

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
        .render(html)
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
        .render(html)
        .expect("render should succeed with element(title, first-except)");

    assert!(
        pdf.len() > 1000,
        "PDF seems empty or too small: {} bytes",
        pdf.len()
    );
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn test_element_policy_start() {
    let css = r#"
        @page {
            size: 400pt 300pt;
            margin: 40pt;
            @top-center { content: element(title, start); }
        }
        .title { position: running(title); }
        .spacer { height: 220pt; }
    "#;

    let html = r#"<!DOCTYPE html>
    <html>
    <head></head>
    <body>
      <h1 class="title">Seed Title</h1>
      <div class="spacer"></div>
      <div style="page-break-before: always">
        <p>Page two has no running title and should keep the previous value.</p>
      </div>
      <div class="spacer"></div>
      <div style="page-break-before: always">
        <h1 class="title">New Seed Title</h1>
        <p>Page three introduces a new running element, but start should ignore the local value.</p>
      </div>
    </body>
    </html>"#;

    let mut assets = AssetBundle::new();
    assets.add_css(css);

    let engine = Engine::builder().assets(assets).build();
    let pdf = engine
        .render(html)
        .expect("render should succeed with element(title, start)");

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
        .render(html)
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
    let result = engine.render(html);
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
    let result = engine.render(html);
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
    let result = engine.render(html);
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
    let result = engine.render(html);
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
    let result = engine.render(html);
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
    let result = engine.render(html);
    assert!(
        result.is_ok(),
        "counter(page) should still work: {:?}",
        result.err()
    );
}

// Snapshot tests (inline <style> variants + migrated integration tests): see crates/fulgur/tests/gcpm_snapshot.rs

#[test]
fn counter_pass_survives_element_names_with_css_metacharacters() {
    // fulgur-ka6c: CounterPass reconstructs a specificity prefix from the
    // matched element's tag/id/classes. The tag prefix must not carry raw
    // characters through to the generated stylesheet — otherwise a class
    // pseudo-content rule matching an element with a crafted tag name
    // could seed unrelated declarations into the Stylo cascade.
    //
    // End-to-end guard for the escape contract: rendering must succeed and
    // produce a well-formed PDF even when html5ever preserves an odd tag
    // name verbatim.
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        body { counter-reset: c 0; }
        .x::before { content: counter(c); }
        "#,
    );
    let engine = Engine::builder().assets(assets).build();
    let html = concat!(
        "<html><body>",
        r#"<x{}body{color:red} class="x">payload</x{}body{color:red}>"#,
        "</body></html>",
    );
    let pdf = engine
        .render(html)
        .expect("render must succeed even with crafted tag names");
    assert!(pdf.starts_with(b"%PDF-"), "output should be a PDF");
}

/// Running elements are serialized back to HTML and reparsed for the margin
/// box, so the serializer has to be the parser's inverse. It used not to be
/// for text nodes: the parser decodes character references on the way in and
/// nothing re-encoded them on the way out, so text that arrived escaped came
/// back as live markup in the header/footer.
///
/// That defeats `template::render_template`'s forced `AutoEscape::Html`,
/// which `docs/security/threat-model.md` V1 lists as an existing mitigation
/// for the trusted-template / untrusted-data shape.
fn render_with_running_header(header_body: &str) -> Vec<u8> {
    let mut assets = AssetBundle::new();
    assets.add_css(
        r#"
        .header { position: running(pageHeader); }
        @page { margin: 2cm; @top-center { content: element(pageHeader); } }
    "#,
    );
    let engine = Engine::builder().assets(assets).build();
    engine
        .render(&format!(
            r#"<!DOCTYPE html><html><body>
  <div class="header">{header_body}</div>
  <p>body</p>
</body></html>"#
        ))
        .expect("render must succeed")
}

#[test]
fn running_element_escaped_text_does_not_become_a_margin_box_link() {
    // `&lt;a href=…&gt;` is what MiniJinja's HTML autoescape emits for a
    // `{{ variable }}` holding an anchor — it must stay text.
    let pdf = render_with_running_header(
        "Report for &lt;a href=&quot;javascript:alert(1)&quot;&gt;click&lt;/a&gt;",
    );
    let bytes = String::from_utf8_lossy(&pdf);
    assert!(
        !bytes.contains("javascript:alert(1)"),
        "escaped anchor became a live PDF URI action"
    );
    assert!(
        !bytes.contains("/URI"),
        "escaped anchor produced a link annotation"
    );
}

#[test]
fn running_element_escaped_text_does_not_become_margin_box_markup() {
    // Beyond links: escaped text must not render as the markup it spells.
    // Comparing against the same markup *authored* as elements is the check
    // — before the fix both serialized to the same string, so the two PDFs
    // were byte-identical.
    //
    // Asserting on the drawn text directly would be sharper, but `inspect`
    // cannot recover it: lopdf 0.40 does not read krilla's `ToUnicode` CMap
    // (no `/CMapVersion` / `/WMode` support), so extracted text comes back as
    // raw glyph ids. Byte comparison against a known-good control is the
    // available signal until that lands.
    let injected = render_with_running_header("Status: &lt;b&gt;APPROVED&lt;/b&gt;");
    let authored = render_with_running_header("Status: <b>APPROVED</b>");
    assert_ne!(
        injected, authored,
        "escaped markup still rendered as authored markup in the margin box"
    );
}

/// `<style />` reparses as an *open* `<style>`, so an empty `<style>` in a
/// running element used to turn the rest of its margin box into CSS text —
/// the header simply vanished. Same round-trip requirement as the escaping
/// above: the serializer has to be the parser's inverse.
#[test]
fn running_element_empty_style_does_not_erase_the_margin_box() {
    let with_empty_style = render_with_running_header("<style></style><b>X</b>");
    let without = render_with_running_header("<b>X</b>");
    assert_eq!(
        with_empty_style, without,
        "an empty <style> swallowed the rest of the margin box"
    );
}

/// Over-escaping tripwire at the render level: `<style>` is a raw text
/// element, so its CSS must survive serialization unescaped.
#[test]
fn running_element_style_child_survives_serialization() {
    let with_style =
        render_with_running_header("<style>.hl { color: red }</style><span class=\"hl\">X</span>");
    let without_style = render_with_running_header("<span class=\"hl\">X</span>");
    assert_ne!(
        with_style, without_style,
        "<style> inside a running element stopped applying — CSS was corrupted by escaping"
    );
}

/// Render a two-chapter, single-page document whose only variable part is
/// the `@top-right` margin-box `content` value.
fn render_with_top_right(content: &str) -> Vec<u8> {
    let mut assets = AssetBundle::new();
    assets.add_css(format!(
        r#"
        body {{ counter-reset: chapter; }}
        h2 {{ counter-increment: chapter; }}
        @page {{ @top-right {{ content: {content}; }} }}
    "#
    ));
    Engine::builder()
        .assets(assets)
        .build()
        .render("<h2>One</h2><p>text</p><h2>Two</h2><p>text</p>")
        .expect("render must succeed")
}

/// CSS Paged Media 3: margin boxes belong to the page context, which does
/// not see document counters, so `counter(chapter)` there is 0 however many
/// `h2`s precede the page. Text extraction cannot recover margin-box glyphs
/// (see `running_element_escaped_text_does_not_become_margin_box_markup`),
/// so the check compares against the same document with the expected text
/// written as a literal.
#[test]
fn margin_box_document_counter_resolves_to_zero() {
    let via_counter = render_with_top_right(r#""Ch. " counter(chapter)"#);
    let via_counters = render_with_top_right(r#""Ch. " counters(chapter, ".")"#);
    let literal_zero = render_with_top_right(r#""Ch. 0""#);
    let literal_two = render_with_top_right(r#""Ch. 2""#);
    assert_eq!(
        via_counter, literal_zero,
        "margin-box counter(chapter) must render as 0"
    );
    assert_eq!(
        via_counters, literal_zero,
        "margin-box counters(chapter) must render as 0"
    );
    assert_ne!(
        via_counter, literal_two,
        "margin-box counter(chapter) must not see the document value"
    );
}

/// The page context keeps `page` / `pages`, also next to a document counter
/// in the same content list.
#[test]
fn margin_box_page_counters_resolve_next_to_document_counter() {
    let via_counter = render_with_top_right(
        r#""Page " counter(page) " of " counter(pages) " Ch. " counter(chapter)"#,
    );
    let literal = render_with_top_right(r#""Page 1 of 1 Ch. 0""#);
    let wrong_page = render_with_top_right(r#""Page 0 of 0 Ch. 0""#);
    assert_eq!(via_counter, literal, "page / pages must still resolve");
    assert_ne!(via_counter, wrong_page);
}

/// The documented way to show a chapter number in a page header: carry the
/// number into a named string and reference it with `string()`.
#[test]
fn margin_box_chapter_number_via_string_set() {
    let render = |top_right: &str| {
        let mut assets = AssetBundle::new();
        assets.add_css(format!(
            r#"
            h2 {{ string-set: chapter attr(data-chapter); }}
            @page {{ @top-right {{ content: {top_right}; }} }}
        "#
        ));
        Engine::builder()
            .assets(assets)
            .build()
            .render(r#"<h2 data-chapter="5">Five</h2><p>text</p>"#)
            .expect("render must succeed")
    };
    let via_string = render(r#""Ch. " string(chapter)"#);
    let literal = render(r#""Ch. 5""#);
    let literal_zero = render(r#""Ch. 0""#);
    assert_eq!(
        via_string, literal,
        "string(chapter) must carry the chapter number"
    );
    assert_ne!(via_string, literal_zero);
}
