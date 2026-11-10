//! A box after a forced page break must start its page, not be painted at the
//! offset it would have had without the break.
//!
//! After a zero-height `break-after: page` inside a container, the walker
//! re-anchors the new page on the next child. A box-less child (`display:
//! none`, a running element) reports Taffy y=0 and used to take that
//! re-anchor, so the real child after it kept its unbroken offset. Once earlier
//! content had spanned a page that offset passes the content box: the child
//! is either pushed on to a spurious extra page or, where it is the first
//! child of a nested container, painted below the page and lost. Either way
//! the render succeeded with exit 0.
//!
//! Reference: Prince 17 opens page 3 with the heading.

use fulgur::Engine;

/// Page 400px square, 50px margins: the content box is 300px = 225pt tall and
/// ends at 262.5pt from the top of the page.
const CONTENT_BOTTOM_PT: f32 = 262.5;

const STYLE: &str = r#"<style>
@page { size: 400px 400px; margin: 50px; }
body { margin: 0; font-family: sans-serif; font-size: 16px; line-height: 20px; }
h2 { margin: 0; font-size: 16px; line-height: 20px; }
table { border-collapse: collapse; width: 100%; }
td { padding: 0; height: 40px; }
</style>"#;

struct Word {
    page: usize,
    y_max: f32,
    text: String,
}

/// Words with their page (1-based) and bbox bottom in pt, via `pdftotext
/// -bbox`. `None` when pdftotext is not installed; CI has poppler-utils.
fn words(pdf: &[u8]) -> Option<Vec<Word>> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.pdf");
    std::fs::write(&path, pdf).unwrap();
    let out = std::process::Command::new("pdftotext")
        .arg("-bbox")
        .arg(&path)
        .arg("-")
        .output()
        .ok()?;
    assert!(out.status.success(), "pdftotext failed: {out:?}");
    let html = String::from_utf8(out.stdout).unwrap();
    let mut page = 0;
    let mut words = Vec::new();
    for line in html.lines() {
        if line.contains("<page ") {
            page += 1;
        } else if let Some(rest) = line.trim().strip_prefix("<word ") {
            let attr = |name: &str| -> f32 {
                let start = rest.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
                let end = start + rest[start..].find('"').unwrap();
                rest[start..end].parse().unwrap()
            };
            let text_start = rest.find('>').unwrap() + 1;
            let text_end = rest.rfind("</word>").unwrap();
            words.push(Word {
                page,
                y_max: attr("yMax"),
                text: rest[text_start..text_end].to_string(),
            });
        }
    }
    Some(words)
}

/// A table spanning a page, a forced break, a `display: none` sibling, then a
/// heading. `main` placed the heading at its unbroken 420px offset in a 300px
/// content box and pushed it to a fourth page.
#[test]
fn heading_after_forced_break_and_hidden_sibling_starts_its_page() {
    let rows: String = (1..=10)
        .map(|i| format!("<tr><td>r{i}</td></tr>"))
        .collect();
    let html = format!(
        r#"<!DOCTYPE html><html><head>{STYLE}</head><body><section>
  <h2>ALPHA</h2>
  <table>{rows}</table>
  <div style="height:0; break-after:page"></div>
  <div style="display:none"><p>hidden</p></div>
  <h2>OMEGA</h2>
</section></body></html>"#
    );
    let pdf = Engine::builder().build().render(&html).expect("render");
    let Some(words) = words(&pdf) else {
        return;
    };
    let below: Vec<_> = words
        .iter()
        .filter(|w| w.y_max > CONTENT_BOTTOM_PT + 0.5)
        .map(|w| format!("{} on page {} at yMax {}", w.text, w.page, w.y_max))
        .collect();
    assert!(
        below.is_empty(),
        "text painted below the content box: {below:?}"
    );
    let omega: Vec<_> = words.iter().filter(|w| w.text == "OMEGA").collect();
    assert_eq!(omega.len(), 1, "OMEGA must be painted once");
    // Top margin 37.5pt plus one 15pt line box.
    assert!(
        omega[0].page == 3 && omega[0].y_max <= 37.5 + 15.0 + 0.5,
        "OMEGA must open page 3, got page {} yMax {}",
        omega[0].page,
        omega[0].y_max
    );
}
