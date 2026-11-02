use std::process::Command;

use tempfile::TempDir;

const HTML: &str = "<html><body><h1>Engine</h1><p>Hello</p></body></html>";

fn run_cli(args: &[&str]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_fulgur");
    Command::new(bin).args(args).output().expect("spawn fulgur")
}

#[test]
fn engine_blitz_is_the_default() {
    let dir = TempDir::new().expect("create temp dir");
    let html = dir.path().join("doc.html");
    let default_pdf = dir.path().join("default.pdf");
    let blitz_pdf = dir.path().join("blitz.pdf");
    std::fs::write(&html, HTML).unwrap();

    for (pdf, extra) in [(&default_pdf, None), (&blitz_pdf, Some("blitz"))] {
        let mut args = vec![
            "render",
            html.to_str().unwrap(),
            "-o",
            pdf.to_str().unwrap(),
        ];
        // Pin metadata that would otherwise vary between runs.
        args.extend(["--creation-date", "2020-01-01T00:00:00Z"]);
        if let Some(e) = extra {
            args.extend(["--engine", e]);
        }
        let out = run_cli(&args);
        assert!(
            out.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(
        std::fs::read(&default_pdf).unwrap(),
        std::fs::read(&blitz_pdf).unwrap()
    );
}

#[cfg(not(feature = "raikiri"))]
#[test]
fn engine_raikiri_without_feature_fails() {
    let dir = TempDir::new().expect("create temp dir");
    let html = dir.path().join("doc.html");
    let pdf = dir.path().join("doc.pdf");
    std::fs::write(&html, HTML).unwrap();

    let out = run_cli(&[
        "render",
        html.to_str().unwrap(),
        "-o",
        pdf.to_str().unwrap(),
        "--engine",
        "raikiri",
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--engine raikiri is not available in this build"),
        "stderr: {stderr}"
    );
    assert!(!pdf.exists());
}

#[cfg(feature = "raikiri")]
mod raikiri {
    use super::*;

    #[test]
    fn engine_raikiri_renders() {
        let dir = TempDir::new().expect("create temp dir");
        let html = dir.path().join("doc.html");
        let pdf = dir.path().join("doc.pdf");
        std::fs::write(&html, HTML).unwrap();

        let out = run_cli(&[
            "render",
            html.to_str().unwrap(),
            "-o",
            pdf.to_str().unwrap(),
            "--engine",
            "raikiri",
            "--title",
            "Raikiri CLI",
        ]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "CLI failed: {stderr}");
        let bytes = std::fs::read(&pdf).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("Raikiri CLI"), "title missing from PDF");
    }

    #[test]
    fn engine_raikiri_with_data_fails() {
        let dir = TempDir::new().expect("create temp dir");
        let html = dir.path().join("doc.html");
        let data = dir.path().join("data.json");
        let pdf = dir.path().join("doc.pdf");
        std::fs::write(&html, HTML).unwrap();
        std::fs::write(&data, "{}").unwrap();

        let out = run_cli(&[
            "render",
            html.to_str().unwrap(),
            "-o",
            pdf.to_str().unwrap(),
            "--engine",
            "raikiri",
            "--data",
            data.to_str().unwrap(),
        ]);
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("--data (template rendering) requires --engine blitz for now"),
            "stderr: {stderr}"
        );
        assert!(!pdf.exists());
    }

    #[test]
    fn engine_raikiri_to_stdout() {
        let dir = TempDir::new().expect("create temp dir");
        let html = dir.path().join("doc.html");
        std::fs::write(&html, HTML).unwrap();

        let out = run_cli(&[
            "render",
            html.to_str().unwrap(),
            "-o",
            "-",
            "--engine",
            "raikiri",
        ]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "CLI failed: {stderr}");
        assert!(out.stdout.starts_with(b"%PDF-"));
    }

    /// Smallest valid 1x1 RGBA PNG.
    const DOT_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0xF0, 0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99, 0x3D, 0x1D, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn engine_raikiri_stdin_resolves_relative_image() {
        use std::io::Write;
        use std::process::Stdio;

        let dir = TempDir::new().expect("create temp dir");
        std::fs::write(dir.path().join("dot.png"), DOT_PNG).unwrap();
        let pdf = dir.path().join("out.pdf");

        let mut child = Command::new(env!("CARGO_BIN_EXE_fulgur"))
            .current_dir(dir.path())
            .args([
                "render",
                "--stdin",
                "-o",
                pdf.to_str().unwrap(),
                "--engine",
                "raikiri",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fulgur");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                b"<html><body><img src=\"dot.png\" width=\"20\" height=\"20\"></body></html>",
            )
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "CLI failed: {stderr}");
        let bytes = std::fs::read(&pdf).unwrap();
        assert!(
            String::from_utf8_lossy(&bytes).contains("/Image"),
            "no image XObject in PDF"
        );
    }
}
