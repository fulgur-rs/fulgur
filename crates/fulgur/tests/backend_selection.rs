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
    fn raikiri_refuses_a_template() {
        let engine = Engine::builder()
            .template("t.html", "<p>{{ x }}</p>")
            .build();
        let err = engine.render_html_with(Backend::Raikiri, HTML).unwrap_err();
        assert_eq!(
            err.to_string(),
            "the Raikiri backend does not render templates yet; use Backend::Blitz"
        );
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
}
