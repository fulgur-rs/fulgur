//! Phase 1 entry: run all css-page reftests via the shared phase runner.
//! Never panics on regressions — emits artifacts under target/wpt-report/css-page/.

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
fn wpt_css_page() {
    let outcome = run_phase(&workspace_root(), "css-page", 96).expect("runner should not error");
    if let Some(o) = outcome {
        eprintln!("css-page report at {}", o.report_dir.display());
    }
}
