use std::process::Command;

fn fulgur_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_fulgur"))
}

/// Render trivial HTML with `--size` and return the raw PDF bytes.
fn render_with_size(size: &str) -> Vec<u8> {
    use std::io::Write;
    let bin = fulgur_bin();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    let mut child = Command::new(&bin)
        .args([
            "render",
            "--stdin",
            "--size",
            size,
            "-o",
            out.to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn fulgur render");
    // Take stdin so it is dropped (closed) after writing, sending EOF to the
    // `--stdin` reader. `Child::wait` also closes stdin, but taking it here
    // makes the EOF explicit and independent of that detail.
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(b"<html><body><p>x</p></body></html>")
            .unwrap();
    }
    assert!(
        child.wait().unwrap().success(),
        "render failed for --size {size}"
    );
    std::fs::read(&out).unwrap()
}

fn media_box(pdf: &[u8]) -> String {
    let text = String::from_utf8_lossy(pdf);
    let idx = text.find("/MediaBox").expect("no MediaBox");
    // `/MediaBox` is immediately followed by `[ ... ]`; search the remainder
    // for the bracket pair rather than slicing a fixed-size window (which
    // could split a multi-byte U+FFFD replacement char at its boundary).
    let rest = &text[idx..];
    let start = rest.find('[').expect("no '[' after MediaBox");
    let end = rest.find(']').expect("no ']' after MediaBox");
    rest[start + 1..end].trim().to_string()
}

#[test]
fn custom_pt_size_sets_media_box() {
    let pdf = render_with_size("200ptx400pt");
    assert_eq!(media_box(&pdf), "0 0 200 400");
}

#[test]
fn custom_mm_size_sets_media_box() {
    // 100mm x 200mm = 283.46 x 566.93 pt (distinct from the A4 fallback)
    let pdf = render_with_size("100x200mm");
    let mb = media_box(&pdf);
    assert!(mb.starts_with("0 0 283.4"), "got {mb}");
}

#[test]
fn keyword_size_still_works() {
    let pdf = render_with_size("A4");
    assert!(media_box(&pdf).starts_with("0 0 595.2"));
}

/// fulgur-5oav: `fulgur` core logs a warning (via the `log` facade) when a
/// `@page { size }` keyword is unrecognised. `fulgur-cli` has to install a
/// logger for that to reach the user at all — a bare `log::warn!` with no
/// logger installed is silently discarded.
#[test]
fn unknown_css_page_size_keyword_warns_on_stderr() {
    use std::io::Write;
    let bin = fulgur_bin();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.pdf");
    let mut child = Command::new(&bin)
        .args(["render", "--stdin", "-o", out.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn fulgur render");
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(
                b"<html><head><style>@page { size: banana; }</style></head>\
                  <body><p>x</p></body></html>",
            )
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "render should still succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("banana") && stderr.to_ascii_lowercase().contains("unknown"),
        "expected an unknown-page-size warning on stderr, got: {stderr}"
    );
    // The document must still render, falling back to A4.
    assert!(media_box(&std::fs::read(&out).unwrap()).starts_with("0 0 595.2"));
}

#[test]
fn help_documents_custom_and_priority() {
    let out = Command::new(fulgur_bin())
        .args(["render", "--help"])
        .output()
        .expect("run --help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("WxH"), "help missing WxH: {help}");
    assert!(
        help.contains("@page"),
        "help missing @page priority note: {help}"
    );
}
