use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

const HTML: &str = "<!doctype html><html><body><p>Hello</p></body></html>";

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fulgur-dev"))
}

fn run(input: &Path, output: &Path, engine: Option<&str>, cwd: &Path) -> Output {
    run_with_args(input, output, engine, cwd, &[])
}

fn run_with_args(
    input: &Path,
    output: &Path,
    engine: Option<&str>,
    cwd: &Path,
    args: &[&OsStr],
) -> Output {
    let mut cmd = command();
    cmd.current_dir(cwd)
        .arg("render")
        .arg(input)
        .arg("-o")
        .arg(output);
    if let Some(engine) = engine {
        cmd.arg("--engine").arg(engine);
    }
    cmd.args(args).output().unwrap()
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
fn raikiri_selection_writes_pdf() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    let result = run(&input, &output, Some("raikiri"), dir.path());
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(load_pdf(&output).get_pages().len(), 1);
}

#[test]
fn backend_failure_preserves_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    // A directory cannot be read as an HTML file, so both backends fail.
    let input = dir.path().join("input-dir");
    std::fs::create_dir(&input).unwrap();
    let output = dir.path().join("output.pdf");
    std::fs::write(&output, b"keep me").unwrap();
    for engine in ["blitz", "raikiri"] {
        let result = run(&input, &output, Some(engine), dir.path());
        assert!(!result.status.success(), "{engine}");
        assert_eq!(std::fs::read(&output).unwrap(), b"keep me", "{engine}");
    }
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

// macOS file systems (APFS) only accept UTF-8 file names, so a path with a
// non-UTF-8 byte cannot be created there; other Unix systems store raw bytes.
#[cfg(all(unix, not(target_os = "macos")))]
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

fn media_box(pdf: &lopdf::Document, page: u32) -> [f32; 4] {
    let id = pdf.get_pages()[&page];
    let rect = pdf
        .get_dictionary(id)
        .unwrap()
        .get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap();
    std::array::from_fn(|i| rect[i].as_float().unwrap())
}

const PAGE_HTML: &str =
    "<!doctype html><style>@page { size:100pt 100pt; margin:0 }</style><p>Hello</p>";

#[test]
fn dev_config_explicit_size_beats_author_page() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, PAGE_HTML).unwrap();
    for engine in ["blitz", "raikiri"] {
        let result = run_with_args(
            &input,
            &output,
            Some(engine),
            dir.path(),
            &[
                OsStr::new("--size"),
                OsStr::new("200x300pt"),
                OsStr::new("--margin"),
                OsStr::new("0"),
            ],
        );
        assert!(
            result.status.success(),
            "{engine}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(media_box(&load_pdf(&output), 1), [0.0, 0.0, 200.0, 300.0]);
    }
}

#[test]
fn dev_config_omitted_size_preserves_author_page() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, PAGE_HTML).unwrap();
    for engine in ["blitz", "raikiri"] {
        let result = run(&input, &output, Some(engine), dir.path());
        assert!(
            result.status.success(),
            "{engine}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(media_box(&load_pdf(&output), 1), [0.0, 0.0, 100.0, 100.0]);
    }
}

#[test]
fn dev_config_invalid_geometry_preserves_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    for engine in ["blitz", "raikiri"] {
        for (flag, value) in [
            ("--size", "bogus"),
            ("--size", "nanx100pt"),
            ("--size", "-20x100pt"),
            ("--size", "0x100pt"),
            ("--margin", "nan"),
            ("--margin", "-5"),
            ("--margin", "1 2 3 4 5"),
            ("--landscape", ""),
        ] {
            std::fs::write(&output, b"keep me").unwrap();
            let option = format!("{flag}={value}");
            let result = run_with_args(
                &input,
                &output,
                Some(engine),
                dir.path(),
                &[OsStr::new(&option)],
            );
            assert!(!result.status.success(), "{engine}: {option}");
            assert_eq!(std::fs::read(&output).unwrap(), b"keep me");
        }
        let result = run_with_args(
            &input,
            &output,
            Some(engine),
            dir.path(),
            &[OsStr::new("--landscape")],
        );
        assert!(!result.status.success());
    }
}

#[test]
fn dev_config_landscape_rotates_explicit_size() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, PAGE_HTML).unwrap();
    for engine in ["blitz", "raikiri"] {
        let result = run_with_args(
            &input,
            &output,
            Some(engine),
            dir.path(),
            &[
                OsStr::new("--size"),
                OsStr::new("200x300pt"),
                OsStr::new("--margin"),
                OsStr::new("0"),
                OsStr::new("--landscape"),
            ],
        );
        assert!(
            result.status.success(),
            "{engine}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(media_box(&load_pdf(&output), 1), [0.0, 0.0, 300.0, 200.0]);
    }
}

#[test]
fn dev_assets_css_and_fonts() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    let css = dir.path().join("page.css");
    std::fs::write(&input, "<p>Hello bundled font</p>").unwrap();
    std::fs::write(
        &css,
        "@page {size:250pt 350pt; margin:0} body {font-family:'Noto Sans'}",
    )
    .unwrap();
    for engine in ["blitz", "raikiri"] {
        for font in [
            "../fulgur-ruby/spec/fixtures/noto_sans.ttf",
            "../fulgur-blitz/tests/fixtures/fonts/NotoSans-Regular.woff2",
        ] {
            let font = Path::new(env!("CARGO_MANIFEST_DIR")).join(font);
            let family = if font.extension() == Some(OsStr::new("ttf")) {
                "Noto Sans Mono"
            } else {
                "Noto Sans"
            };
            std::fs::write(
                &css,
                format!("@page {{size:250pt 350pt; margin:0}} body {{font-family:'{family}'}}"),
            )
            .unwrap();
            let cwd = tempfile::tempdir().unwrap();
            let result = run_with_args(
                &input,
                &output,
                Some(engine),
                cwd.path(),
                &[
                    OsStr::new("--css"),
                    css.as_os_str(),
                    OsStr::new("--font"),
                    font.as_os_str(),
                    OsStr::new("--no-system-fonts"),
                ],
            );
            assert!(
                result.status.success(),
                "{engine}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let pdf = load_pdf(&output);
            assert_eq!(media_box(&pdf, 1), [0.0, 0.0, 250.0, 350.0]);
            let text = pdf.extract_text(&[1]).unwrap();
            assert!(
                text.contains("Hello bundled font"),
                "{engine}, {font:?}: {text:?}"
            );
        }
    }
}

#[test]
fn dev_assets_invalid_font_does_not_write_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    let font = dir.path().join("bad.ttf");
    std::fs::write(&input, HTML).unwrap();
    std::fs::write(&font, b"not a font").unwrap();
    for engine in ["blitz", "raikiri"] {
        for flag in ["--font", "--css"] {
            std::fs::write(&output, b"keep me").unwrap();
            let asset = if flag == "--css" {
                dir.path().join("missing.css")
            } else {
                font.clone()
            };
            let result = run_with_args(
                &input,
                &output,
                Some(engine),
                dir.path(),
                &[OsStr::new(flag), asset.as_os_str()],
            );
            assert!(!result.status.success());
            assert_eq!(std::fs::read(&output).unwrap(), b"keep me");
        }
    }
}

#[test]
fn dev_metadata_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, HTML).unwrap();
    let args: Vec<&OsStr> = [
        "--title",
        "Report",
        "--author",
        "Alice",
        "--author",
        "Bob",
        "--description",
        "Quarterly",
        "--keyword",
        "one",
        "--keywords",
        "two",
        "--language",
        "ja",
        "--creator",
        "tool",
        "--producer",
        "fulgur-dev",
        "--creation-date",
        "2026-10-07T01:02:03Z",
    ]
    .into_iter()
    .map(OsStr::new)
    .collect();
    for engine in ["blitz", "raikiri"] {
        let result = run_with_args(&input, &output, Some(engine), dir.path(), &args);
        assert!(
            result.status.success(),
            "{engine}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let pdf = load_pdf(&output);
        let info = pdf
            .get_dictionary(pdf.trailer.get(b"Info").unwrap().as_reference().unwrap())
            .unwrap();
        for (key, expected) in [
            ("Title", "Report"),
            ("Author", "Alice, Bob"),
            ("Subject", "Quarterly"),
            ("Keywords", "one, two"),
            ("Creator", "tool"),
            ("Producer", "fulgur-dev"),
        ] {
            assert_eq!(
                lopdf::decode_text_string(info.get(key.as_bytes()).unwrap()).unwrap(),
                expected,
                "{engine}: {key}"
            );
        }
        let date = lopdf::decode_text_string(info.get(b"CreationDate").unwrap()).unwrap();
        assert!(date.starts_with("D:20261007010203"), "{engine}: {date}");
        assert_eq!(
            lopdf::decode_text_string(pdf.catalog().unwrap().get(b"Lang").unwrap()).unwrap(),
            "ja"
        );
    }
}

#[test]
fn dev_bookmark_opt_in_creates_outline() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.html");
    let output = dir.path().join("output.pdf");
    std::fs::write(&input, "<h1>Heading</h1>").unwrap();
    for engine in ["blitz", "raikiri"] {
        let result = run_with_args(
            &input,
            &output,
            Some(engine),
            dir.path(),
            &[OsStr::new("--bookmarks")],
        );
        assert!(
            result.status.success(),
            "{engine}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            load_pdf(&output).catalog().unwrap().has(b"Outlines"),
            "{engine}"
        );
    }
}
