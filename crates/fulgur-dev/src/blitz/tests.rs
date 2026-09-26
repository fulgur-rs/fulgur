use super::*;
use fulgur_core::Error;

#[test]
fn file_input_returns_pdf_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    std::fs::write(
        &input,
        "<!doctype html><html><body><p>Hello</p></body></html>",
    )
    .unwrap();
    let bytes = render(&input).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert_eq!(pdf.get_pages().len(), 1);
}

#[test]
fn missing_input_returns_io_error() {
    let dir = tempfile::tempdir().unwrap();
    match render(&dir.path().join("missing.html")) {
        Err(Error::Io(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!("expected IO error, got {other:?}"),
    }
}
