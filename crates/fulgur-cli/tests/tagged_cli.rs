use std::process::Command;
use tempfile::TempDir;

fn run_cli(args: &[&str]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_fulgur");
    Command::new(bin).args(args).output().expect("spawn fulgur")
}

#[test]
fn cli_tagged_flag_produces_struct_tree_root() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<html><body><p>Hello tagged world</p></body></html>",
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--tagged",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/StructTreeRoot"),
        "tagged PDF must contain /StructTreeRoot"
    );
}

#[test]
fn cli_without_tagged_flag_has_no_struct_tree_root() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(&html_path, "<html><body><p>Hello</p></body></html>").unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        !s.contains("/StructTreeRoot"),
        "untagged PDF must not contain /StructTreeRoot"
    );
}

#[test]
fn cli_pdf_ua_flag_fails_with_validation_errors() {
    // PDF/UA (Validator::UA1) is strict and rejects documents that lack required
    // metadata such as a document title and outline. The CLI propagates the
    // validation error and exits with a non-zero status.
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(&html_path, "<html><body><p>Hello PDF/UA</p></body></html>").unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--pdf-ua",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "CLI should fail PDF/UA validation: {stderr}"
    );
    assert!(
        stderr.contains("Validation"),
        "stderr must mention Validation: {stderr}"
    );
}
