//! Over the VRT fixture corpus, the page layout each backend reports matches
//! the PDF that backend renders. Differences between the backends are
//! printed as a summary, not asserted: `fulgur-vrt`'s engine comparison
//! measures cross-engine parity.
#![cfg(feature = "raikiri")]

use fulgur::{Backend, Engine, EngineExt, PageLayout};
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fulgur-vrt/fixtures")
}

/// Every `.html` file under `dir`, sorted.
fn html_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read fixture dir") {
            let path = entry.expect("fixture dir entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "html") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The `MediaBox` width and height of every page of `pdf`, in order.
fn pdf_page_sizes(pdf: &[u8]) -> Vec<(f32, f32)> {
    let doc = lopdf::Document::load_mem(pdf).expect("parse PDF");
    doc.get_pages()
        .values()
        .map(|&id| {
            let page = doc.get_dictionary(id).expect("page dictionary");
            let media_box = page
                .get(b"MediaBox")
                .and_then(lopdf::Object::as_array)
                .expect("MediaBox");
            let n = |i: usize| media_box[i].as_float().expect("number");
            (n(2) - n(0), n(3) - n(1))
        })
        .collect()
}

fn assert_layout_matches_pdf(name: &str, backend: Backend, layout: &PageLayout, pdf: &[u8]) {
    let sizes = pdf_page_sizes(pdf);
    assert_eq!(
        layout.page_count(),
        sizes.len(),
        "{name} ({backend:?}): page count"
    );
    for (index, (page, &(width, height))) in layout.pages.iter().zip(&sizes).enumerate() {
        assert!(
            (page.size.width - width).abs() < 0.01 && (page.size.height - height).abs() < 0.01,
            "{name} ({backend:?}) page {index}: layout {:?} vs PDF {width}x{height}",
            page.size
        );
    }
}

#[test]
fn page_layout_matches_the_rendered_pdf_over_the_vrt_corpus() {
    let files = html_files(&fixtures_dir());
    assert!(!files.is_empty(), "no fixtures found");
    let mut checked = 0;
    let mut differing = Vec::new();
    for path in &files {
        let name = path
            .strip_prefix(fixtures_dir())
            .unwrap_or(path)
            .display()
            .to_string();
        let html = std::fs::read_to_string(path).expect("read fixture");
        let engine = Engine::builder()
            .base_path(path.parent().expect("fixture dir"))
            .build();
        let mut counts = Vec::new();
        for backend in [Backend::Blitz, Backend::Raikiri] {
            // A fixture a backend cannot render, for example because of the
            // fonts of the host, says nothing about its page layout.
            let Ok(pdf) = engine.render_html_with(backend, &html) else {
                continue;
            };
            let layout = engine
                .page_layout_with(backend, &html)
                .unwrap_or_else(|e| panic!("{name} ({backend:?}): page_layout failed: {e}"));
            assert_layout_matches_pdf(&name, backend, &layout, &pdf);
            counts.push(layout.page_count());
            checked += 1;
        }
        if let [blitz, raikiri] = counts[..]
            && blitz != raikiri
        {
            differing.push(format!("{name}: blitz {blitz}, raikiri {raikiri}"));
        }
    }
    eprintln!(
        "checked {checked} renders over {} fixtures; page counts differ between backends on {}:",
        files.len(),
        differing.len()
    );
    for line in &differing {
        eprintln!("  {line}");
    }
    assert!(checked > 0);
}
