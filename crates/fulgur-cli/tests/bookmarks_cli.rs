use std::process::Command;

use tempfile::TempDir;

fn run_cli(args: &[&str]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_fulgur");
    Command::new(bin).args(args).output().expect("spawn fulgur")
}

#[test]
fn cli_bookmarks_flag_produces_outline() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<html><body><h1>Title</h1><h2>Sub</h2></body></html>",
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--bookmarks",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/Outlines"));
}

#[test]
fn cli_without_flag_produces_no_outline() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(&html_path, "<html><body><h1>Title</h1></body></html>").unwrap();

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
    assert!(!s.contains("/Outlines"));
}
