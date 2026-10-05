#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTML parse error: {0}")]
    HtmlParse(String),

    #[error("Layout error: {0}")]
    Layout(String),

    #[error("PDF generation error: {0}")]
    PdfGeneration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Asset error: {0}")]
    Asset(String),

    #[error("Template error: {0}")]
    Template(String),

    #[error("WOFF decode error: {0}")]
    WoffDecode(String),

    #[error("Unsupported font format: {0}")]
    UnsupportedFontFormat(String),

    #[error("{0}")]
    Other(String),
}

impl From<minijinja::Error> for Error {
    fn from(e: minijinja::Error) -> Self {
        Error::Template(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_error_display() {
        let err = Error::Template("syntax error at line 3".into());
        assert!(err.to_string().contains("syntax error at line 3"));
    }

    #[test]
    fn from_minijinja_error_becomes_template_variant() {
        let env = minijinja::Environment::new();
        let jinja_err = env.get_template("nonexistent_template").unwrap_err();
        let err: Error = jinja_err.into();
        assert!(
            matches!(err, Error::Template(_)),
            "minijinja::Error should convert to Error::Template, got: {err:?}"
        );
        assert!(
            err.to_string().starts_with("Template error:"),
            "display should begin with 'Template error:', got: {err}"
        );
    }

    #[test]
    fn from_io_error_becomes_io_variant() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();
        assert!(
            matches!(err, Error::Io(_)),
            "io::Error should convert to Error::Io, got: {err:?}"
        );
        assert!(
            err.to_string().starts_with("IO error:"),
            "display should begin with 'IO error:', got: {err}"
        );
    }

    #[test]
    fn all_variant_displays_include_the_message() {
        let cases: &[(&str, Error)] = &[
            ("HTML parse error:", Error::HtmlParse("bad tag".into())),
            ("Layout error:", Error::Layout("overflow".into())),
            (
                "PDF generation error:",
                Error::PdfGeneration("krilla fail".into()),
            ),
            ("Asset error:", Error::Asset("missing font".into())),
            ("Template error:", Error::Template("bad block".into())),
            (
                "WOFF decode error:",
                Error::WoffDecode("corrupt header".into()),
            ),
            (
                "Unsupported font format:",
                Error::UnsupportedFontFormat("eot".into()),
            ),
        ];
        for (prefix, err) in cases {
            let s = err.to_string();
            assert!(
                s.starts_with(prefix),
                "expected display to start with {prefix:?}, got: {s:?}"
            );
        }
    }

    #[test]
    fn other_variant_display_shows_message_directly() {
        let err = Error::Other("something unexpected".into());
        assert_eq!(err.to_string(), "something unexpected");
    }

    #[test]
    fn result_type_alias_is_result_of_error() {
        fn returns_ok() -> Result<u32> {
            Ok(42)
        }
        fn returns_err() -> Result<u32> {
            Err(Error::Other("oops".into()))
        }
        assert_eq!(returns_ok().unwrap(), 42);
        assert!(returns_err().is_err());
    }
}
