use fulgur::{Backend, Engine, EngineExt};

const HTML: &str = "<html><body><p>Hello</p></body></html>";

#[test]
fn default_backend_is_blitz() {
    assert_eq!(Backend::default(), Backend::Blitz);
}

#[test]
fn blitz_backend_matches_render() {
    let engine = Engine::builder().build();
    assert_eq!(
        engine.render_html_with(Backend::Blitz, HTML).unwrap(),
        engine.render(HTML).unwrap()
    );
}

#[test]
fn blitz_backend_matches_render_template() {
    let engine = Engine::builder()
        .creation_date("2020-01-01T00:00:00Z")
        .template("t.html", "<p>{{ name }}</p>")
        .data(serde_json::json!({ "name": "Ada" }))
        .build();
    assert_eq!(
        engine.render_template_with(Backend::Blitz).unwrap(),
        engine.render_template().unwrap()
    );
}

#[test]
fn blitz_backend_matches_render_batch() {
    let engine = Engine::builder()
        .creation_date("2020-01-01T00:00:00Z")
        .build();
    let htmls = [HTML, "<p>Second</p>"];
    let with: Vec<Vec<u8>> = engine
        .render_batch_with(Backend::Blitz, &htmls)
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let plain: Vec<Vec<u8>> = engine
        .render_batch(&htmls)
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(with, plain);
}

#[test]
fn blitz_backend_matches_page_layout() {
    let engine = Engine::builder().build();
    let layout = engine.page_layout_with(Backend::Blitz, HTML).unwrap();
    assert_eq!(layout, engine.page_layout(HTML).unwrap());
    assert_eq!(layout.page_count(), 1);
}

#[cfg(feature = "raikiri")]
mod raikiri {
    use super::*;

    #[test]
    fn raikiri_renders_a_pdf_with_the_title() {
        let engine = Engine::builder().title("Backend test").build();
        let pdf = engine.render_html_with(Backend::Raikiri, HTML).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(String::from_utf8_lossy(&pdf).contains("Backend test"));
    }

    #[test]
    fn raikiri_refuses_custom_serialize_settings() {
        let engine = Engine::builder()
            .serialize_settings(Default::default())
            .build();
        let err = engine.render_html_with(Backend::Raikiri, HTML).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the Raikiri backend does not accept custom serialize settings; use Backend::Blitz"
        );
    }

    /// An engine whose PDFs do not depend on the time they were made.
    fn pinned() -> fulgur::EngineBuilder {
        Engine::builder().creation_date("2020-01-01T00:00:00Z")
    }

    #[test]
    fn raikiri_render_html_ignores_a_registered_template() {
        let engine = pinned().template("t.html", "<p>{{ x }}</p>").build();
        assert_eq!(
            engine.render_html_with(Backend::Raikiri, HTML).unwrap(),
            pinned()
                .build()
                .render_html_with(Backend::Raikiri, HTML)
                .unwrap()
        );
    }

    #[test]
    fn raikiri_render_template_renders_the_expanded_html() {
        let engine = pinned()
            .template("t.html", "<p>Hello {{ name }}</p>")
            .data(serde_json::json!({ "name": "Ada" }))
            .build();
        let pdf = engine.render_template_with(Backend::Raikiri).unwrap();
        assert_eq!(
            pdf,
            engine
                .render_html_with(Backend::Raikiri, "<p>Hello Ada</p>")
                .unwrap()
        );
    }

    #[test]
    fn raikiri_render_template_without_template_errors() {
        let err = Engine::builder()
            .build()
            .render_template_with(Backend::Raikiri)
            .unwrap_err();
        assert!(matches!(err, fulgur::Error::Template(_)), "{err:?}");
    }

    #[test]
    fn raikiri_render_template_refuses_custom_serialize_settings() {
        let engine = Engine::builder()
            .template("t.html", "<p>x</p>")
            .serialize_settings(Default::default())
            .build();
        assert!(engine.render_template_with(Backend::Raikiri).is_err());
    }

    #[test]
    fn raikiri_render_batch_matches_single_renders() {
        // A bundled font makes every item build its own font collection,
        // which is the state concurrent items must not share.
        let mut assets = fulgur::AssetBundle::new();
        assets
            .add_font_file(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../examples/.fonts/NotoSans-Regular.ttf"
            ))
            .unwrap();
        let engine = pinned().assets(assets).system_fonts(false).build();
        let htmls: Vec<String> = (0..8)
            .map(|i| format!("<h1>Document {i}</h1>{}", "<p>Text</p>".repeat(i * 20)))
            .collect();
        let batch = engine.render_batch_with(Backend::Raikiri, &htmls);
        assert_eq!(batch.len(), htmls.len());
        for (html, pdf) in htmls.iter().zip(batch) {
            assert_eq!(
                pdf.unwrap(),
                engine.render_html_with(Backend::Raikiri, html).unwrap()
            );
        }
    }

    #[test]
    fn raikiri_render_batch_isolates_a_failing_item() {
        // Without system fonts or a bundled font every Raikiri item fails,
        // and each failure is reported in place.
        let engine = Engine::builder().system_fonts(false).build();
        let batch = engine.render_batch_with(Backend::Raikiri, &[HTML, HTML]);
        assert_eq!(batch.len(), 2);
        assert!(batch.iter().all(Result::is_err));
        let mixed = pinned()
            .build()
            .render_batch_with(Backend::Raikiri, &[HTML, HTML]);
        assert!(mixed.iter().all(Result::is_ok));
    }

    #[test]
    fn raikiri_page_layout_agrees_with_blitz_on_a_simple_page() {
        let engine = Engine::builder()
            .page_size(fulgur::PageSize::A5)
            .landscape(true)
            .build();
        let raikiri = engine.page_layout_with(Backend::Raikiri, HTML).unwrap();
        let blitz = engine.page_layout_with(Backend::Blitz, HTML).unwrap();
        assert_eq!(raikiri.page_count(), 1);
        assert_eq!(blitz.page_count(), 1);
        let (r, b) = (raikiri.pages[0], blitz.pages[0]);
        assert!((r.size.width - b.size.width).abs() < 0.01, "{r:?} vs {b:?}");
        assert!(
            (r.size.height - b.size.height).abs() < 0.01,
            "{r:?} vs {b:?}"
        );
        assert!((r.margin.top - b.margin.top).abs() < 0.01, "{r:?} vs {b:?}");
        assert!(
            (r.margin.left - b.margin.left).abs() < 0.01,
            "{r:?} vs {b:?}"
        );
    }

    #[test]
    fn raikiri_page_layout_refuses_custom_serialize_settings() {
        let engine = Engine::builder()
            .serialize_settings(Default::default())
            .build();
        assert!(engine.page_layout_with(Backend::Raikiri, HTML).is_err());
    }
}
