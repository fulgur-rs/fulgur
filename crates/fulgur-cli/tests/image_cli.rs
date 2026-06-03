use std::process::Command;
use tempfile::TempDir;

fn run_cli(args: &[&str]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_fulgur");
    Command::new(bin).args(args).output().expect("spawn fulgur")
}

#[test]
fn cli_writes_png_of_requested_size() {
    let dir = TempDir::new().unwrap();
    let html = dir.path().join("c.html");
    let png = dir.path().join("c.png");
    std::fs::write(
        &html,
        r#"<html><body style="margin:0"><div style="width:80px;height:40px;background:#ff0000"></div></body></html>"#,
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html.to_str().unwrap(),
        "-o",
        png.to_str().unwrap(),
        "--image-size",
        "80x40",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&png).unwrap();
    assert_eq!(&bytes[..4], &[0x89, b'P', b'N', b'G']);
    let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (80, 40));
}

#[test]
fn cli_errors_without_image_size() {
    let dir = TempDir::new().unwrap();
    let html = dir.path().join("c.html");
    let png = dir.path().join("c.png");
    std::fs::write(&html, "<html><body>hi</body></html>").unwrap();
    let out = run_cli(&[
        "render",
        html.to_str().unwrap(),
        "-o",
        png.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--image-size"));
}
