//! fulgur-rpvu: v2 path support for `position: fixed` per-page repetition.

use fulgur::{Engine, PageSize};

#[test]
fn v2_fixed_repeats_on_every_page() {
    let html = r#"<html><body>
          <div style="height: 600px"></div>
          <div style="height: 600px"></div>
          <div style="position: fixed; top: 10px; left: 20px;
                      width: 200px; height: 50px">FXFXFX</div>
        </body></html>"#;
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html_v2(html).expect("render v2");

    let dir = tempfile::tempdir().expect("tempdir");
    let pdf_path = dir.path().join("out.pdf");
    std::fs::write(&pdf_path, &pdf).unwrap();
    if std::process::Command::new("pdftotext")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("pdftotext not available; skipping per-page text assertion");
        return;
    }

    let extract = |page: u32| {
        std::process::Command::new("pdftotext")
            .args(["-f", &page.to_string(), "-l", &page.to_string(), "-layout"])
            .arg(&pdf_path)
            .arg("-")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default()
    };

    assert!(
        extract(1).contains("FXFXFX"),
        "page 1 should contain FXFXFX (v2 fixed repetition)"
    );
    assert!(
        extract(2).contains("FXFXFX"),
        "page 2 should also contain FXFXFX (v2 per-page fixed repetition)"
    );
}
