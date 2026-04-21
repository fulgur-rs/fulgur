//! Phase 3 entry: run all css-multicol reftests via the shared runner.
//! Never panics on regressions — emits artifacts under target/wpt-report/css-multicol/.

use fulgur_wpt::runner::run_phase;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn wpt_css_multicol() {
    let outcome =
        run_phase(&workspace_root(), "css-multicol", 96).expect("runner should not error");
    match outcome {
        Some(o) => eprintln!("css-multicol report at {}", o.report_dir.display()),
        // `FULGUR_WPT_REQUIRED=1` is set only by the dedicated `wpt` matrix
        // job. Other CI cells (the `test` matrix on macOS/Windows/arm, the
        // coverage run, local dev) don't fetch WPT and should skip silently.
        None if std::env::var_os("FULGUR_WPT_REQUIRED").is_some() => {
            panic!(
                "wpt_css_multicol prerequisites missing (run scripts/wpt/fetch.sh + install poppler-utils)"
            );
        }
        None => {}
    }
}
