use fulgur_wpt::expectations::Expectation;
use fulgur_wpt::harness::run_one;
use std::io::Write;

fn poppler_available() -> bool {
    std::process::Command::new("pdftocairo")
        .arg("-v")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn write(path: &std::path::Path, body: &str) {
    std::fs::File::create(path)
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
}

#[test]
fn identical_test_and_ref_pass() {
    if !poppler_available() {
        eprintln!("skip: no pdftocairo");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let test = dir.path().join("t.html");
    let refh = dir.path().join("t-ref.html");

    // A small multi-page fixture via natural overflow (page-break-after
    // is not wired through fulgur yet — see Task 5 note).
    let common_style = r#"<style>
  @page { size: 300pt 200pt; margin: 0; }
  p { font-size: 14pt; line-height: 18pt; margin: 0; }
</style>"#;
    let common_body: String = (0..40)
        .map(|i| format!("<p>paragraph {i}</p>"))
        .collect::<Vec<_>>()
        .join("\n");

    let test_body = format!(
        r#"<!DOCTYPE html><link rel="match" href="t-ref.html"><meta name="fuzzy" content="0-2;0-1000000">{common_style}<body>{common_body}</body>"#
    );
    let ref_body = format!(r#"<!DOCTYPE html>{common_style}<body>{common_body}</body>"#);
    write(&test, &test_body);
    write(&refh, &ref_body);

    let work = dir.path().join("work");
    let diff = dir.path().join("diff");
    let out = run_one(&test, &work, &diff, 96).unwrap();
    assert_eq!(out.observed, Expectation::Pass, "reason: {:?}", out.reason);
}

#[test]
fn page_count_mismatch_fails() {
    if !poppler_available() {
        eprintln!("skip: no pdftocairo");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let test = dir.path().join("t.html");
    let refh = dir.path().join("t-ref.html");

    // Test body overflows MUCH more than ref body → different page counts.
    let style = r#"<style>
  @page { size: 300pt 200pt; margin: 0; }
  p { font-size: 14pt; line-height: 18pt; margin: 0; }
</style>"#;
    let many_paras: String = (0..80)
        .map(|i| format!("<p>t{i}</p>"))
        .collect::<Vec<_>>()
        .join("\n");
    let few_paras: String = (0..10)
        .map(|i| format!("<p>r{i}</p>"))
        .collect::<Vec<_>>()
        .join("\n");

    let test_body = format!(
        r#"<!DOCTYPE html><link rel="match" href="t-ref.html">{style}<body>{many_paras}</body>"#
    );
    let ref_body = format!(r#"<!DOCTYPE html>{style}<body>{few_paras}</body>"#);
    write(&test, &test_body);
    write(&refh, &ref_body);

    let work = dir.path().join("work");
    let diff = dir.path().join("diff");
    let out = run_one(&test, &work, &diff, 96).unwrap();
    assert_eq!(out.observed, Expectation::Fail);
    assert!(
        out.reason.as_deref().unwrap_or("").contains("page count"),
        "unexpected reason: {:?}",
        out.reason
    );
}

#[test]
fn skipped_reftest_reports_skip() {
    // No poppler needed: classify() decides before render is invoked.
    let dir = tempfile::tempdir().unwrap();
    let test = dir.path().join("t.html");
    write(
        &test,
        r#"<!DOCTYPE html><link rel="mismatch" href="x.html"><body></body>"#,
    );

    let work = dir.path().join("work");
    let diff = dir.path().join("diff");
    let out = run_one(&test, &work, &diff, 96).unwrap();
    assert_eq!(out.observed, Expectation::Skip);
}
