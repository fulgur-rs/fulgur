use std::process::Command;

fn fulgur_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.push("fulgur");
    p
}

#[test]
fn inspect_outputs_valid_json() {
    let bin = fulgur_bin();
    if !bin.exists() {
        eprintln!("fulgur binary not found at {:?}, skipping", bin);
        return;
    }

    let tmp_pdf = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();

    // fulgur render で一時PDFを生成
    let render_status = {
        use std::io::Write;
        let mut child = Command::new(&bin)
            .args(["render", "--stdin", "-o", tmp_pdf.path().to_str().unwrap()])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn fulgur render");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"<html><body><p>Test</p></body></html>")
            .unwrap();
        child.wait().unwrap()
    };
    assert!(render_status.success(), "fulgur render failed");

    // fulgur inspect で JSON を取得
    let output = Command::new(&bin)
        .args(["inspect", tmp_pdf.path().to_str().unwrap()])
        .stderr(std::process::Stdio::null())
        .output()
        .expect("failed to run fulgur inspect");

    assert!(
        output.status.success(),
        "fulgur inspect exited non-zero: {:?}",
        output.status
    );

    let json_str = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).expect("output is not valid JSON");

    assert!(
        parsed["pages"].as_u64().unwrap_or(0) >= 1,
        "pages must be >= 1"
    );
    assert!(parsed["metadata"].is_object(), "metadata must be an object");
    assert!(
        parsed["text_items"].is_array(),
        "text_items must be an array"
    );
    assert!(parsed["images"].is_array(), "images must be an array");
}

#[test]
fn inspect_file_output() {
    let bin = fulgur_bin();
    if !bin.exists() {
        return;
    }

    let tmp_pdf = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let tmp_json = tempfile::NamedTempFile::with_suffix(".json").unwrap();

    // render
    {
        use std::io::Write;
        let mut child = Command::new(&bin)
            .args(["render", "--stdin", "-o", tmp_pdf.path().to_str().unwrap()])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn fulgur render");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"<html><body><h1>Title</h1></body></html>")
            .unwrap();
        child.wait().unwrap();
    }

    // inspect -o <file>
    let status = Command::new(&bin)
        .args([
            "inspect",
            tmp_pdf.path().to_str().unwrap(),
            "-o",
            tmp_json.path().to_str().unwrap(),
        ])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to run fulgur inspect");

    assert!(status.success(), "fulgur inspect -o failed");

    let content = std::fs::read_to_string(tmp_json.path()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("file output is not valid JSON");
    assert!(parsed["pages"].as_u64().unwrap_or(0) >= 1);
}
