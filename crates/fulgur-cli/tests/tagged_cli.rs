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
fn cli_pdf_ua_flag_succeeds() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<html><head><title>Test Document</title></head><body><h1>Hello</h1><p>Hello PDF/UA</p></body></html>",
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--pdf-ua",
        "--title",
        "Test Document",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI --pdf-ua failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/StructTreeRoot"),
        "pdf-ua PDF must contain /StructTreeRoot"
    );
}

#[test]
fn tagged_pdf_ul_has_lbl_lbody_tags() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<!DOCTYPE html><html><body><ul><li>first</li><li>second</li></ul></body></html>",
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
    // LI/Lbl/LBody はいずれも PDF 名前空間内で一意なため安全にバイト検索できる
    assert!(
        s.contains("/LI"),
        "tagged ul must contain /LI struct element"
    );
    assert!(
        s.contains("/Lbl"),
        "tagged ul must contain /Lbl (marker label)"
    );
    assert!(
        s.contains("/LBody"),
        "tagged ul must contain /LBody (list body)"
    );
}

#[test]
fn tagged_pdf_ol_has_lbl_lbody_tags() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<!DOCTYPE html><html><body><ol><li>first</li><li>second</li></ol></body></html>",
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
    assert!(s.contains("/LI"), "tagged ol must contain /LI");
    assert!(s.contains("/Lbl"), "tagged ol must contain /Lbl");
    assert!(s.contains("/LBody"), "tagged ol must contain /LBody");
}

#[test]
fn tagged_pdf_nested_list_does_not_panic() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<!DOCTYPE html><html><body><ul><li>outer<ol><li>inner</li></ol></li></ul></body></html>",
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
    assert!(
        out.status.success(),
        "nested list CLI must not panic: {stderr}"
    );

    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    // ネストリストで LI が 2 つ（outer + inner）以上あること
    let li_count = s.match_indices("/LI").count();
    assert!(
        li_count >= 2,
        "nested list must have at least 2 /LI tags, got {li_count}"
    );
}
