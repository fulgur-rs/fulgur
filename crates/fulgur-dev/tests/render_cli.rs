use std::path::Path;
use std::process::{Command, Output};

const HTML: &str = "<!doctype html><html><body><p>Hello</p></body></html>";

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fulgur-dev"))
}

fn run(input: &Path, output: &Path, engine: Option<&str>, cwd: &Path) -> Output {
    let mut cmd = command();
    cmd.current_dir(cwd)
        .arg("render")
        .arg(input)
        .arg("-o")
        .arg(output);
    if let Some(engine) = engine {
        cmd.arg("--engine").arg(engine);
    }
    cmd.output().unwrap()
}

fn load_pdf(path: &Path) -> lopdf::Document {
    let bytes = std::fs::read(path).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    lopdf::Document::load_mem(&bytes).unwrap()
}

#[test]
fn blitz_selection_writes_pdf() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    let result = run(&input, &output, Some("blitz"), dir.path());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(load_pdf(&output).get_pages().len(), 1);
}

#[test]
fn default_engine_is_blitz() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    let result = run(&input, &output, None, dir.path());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(load_pdf(&output).get_pages().len(), 1);
}

#[test]
fn blitz_resolves_stylesheet_relative_to_input() {
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(child.join("input.html"), "<!doctype html><html><head><link rel=stylesheet href=page.css></head><body><p>Hello</p></body></html>").unwrap();
    std::fs::write(
        child.join("page.css"),
        "@page { size: 200pt 300pt; margin:0 }",
    )
    .unwrap();
    let output = dir.path().join("output.pdf");
    let result = run(
        Path::new("child/input.html"),
        &output,
        Some("blitz"),
        dir.path(),
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let pdf = load_pdf(&output);
    let page_id = *pdf.get_pages().values().next().unwrap();
    let rect = pdf
        .get_dictionary(page_id)
        .unwrap()
        .get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap();
    let number = |value: &lopdf::Object| match value {
        lopdf::Object::Integer(n) => *n as f64,
        lopdf::Object::Real(n) => f64::from(*n),
        other => panic!("expected PDF number, got {other:?}"),
    };
    assert!((number(&rect[2]) - number(&rect[0]) - 200.0).abs() < 0.1);
    assert!((number(&rect[3]) - number(&rect[1]) - 300.0).abs() < 0.1);
}

#[test]
fn raikiri_selection_reports_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    let result = run(&input, &output, Some("raikiri"), dir.path());
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("Raikiri PDF drawing is not implemented")
    );
    assert!(!output.exists());
}

#[test]
fn backend_failure_preserves_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    std::fs::write(&output, b"keep me").unwrap();
    let result = run(&input, &output, Some("raikiri"), dir.path());
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&output).unwrap(), b"keep me");
}

#[test]
fn missing_input_does_not_create_or_overwrite_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("missing.html");
    for engine in ["blitz", "raikiri"] {
        let new_output = dir.path().join(format!("new-{engine}.pdf"));
        let result = run(&input, &new_output, Some(engine), dir.path());
        assert!(!result.status.success());
        assert!(!new_output.exists());
        let existing_output = dir.path().join(format!("existing-{engine}.pdf"));
        std::fs::write(&existing_output, b"keep me").unwrap();
        let result = run(&input, &existing_output, Some(engine), dir.path());
        assert!(!result.status.success());
        assert_eq!(std::fs::read(&existing_output).unwrap(), b"keep me");
    }
}

#[test]
fn unknown_engine_is_argument_error() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    std::fs::write(&output, b"keep me").unwrap();
    let result = run(&input, &output, Some("unknown"), dir.path());
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("blitz") && stderr.contains("raikiri"));
    assert_eq!(std::fs::read(&output).unwrap(), b"keep me");
}

#[test]
fn required_arguments_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    let missing_input = command()
        .arg("render")
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!missing_input.status.success());
    assert!(!output.exists());
    let missing_output = command().arg("render").arg(&input).output().unwrap();
    assert!(!missing_output.status.success());
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_work() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let input = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"input-\xff.html".to_vec()));
    let output = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"output-\xff.pdf".to_vec()));
    std::fs::write(&input, HTML).unwrap();
    let result = run(&input, &output, Some("blitz"), dir.path());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(load_pdf(&output).get_pages().len(), 1);
}
