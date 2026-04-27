use fulgur_vrt::runner::{self, RunnerContext};
use std::fmt::Write;

#[test]
fn run_fulgur_vrt() {
    let ctx = RunnerContext::discover().expect("discover context");
    let result = runner::run(&ctx).expect("runner execution failed");

    if !result.updated.is_empty() {
        eprintln!("updated {} goldens", result.updated.len());
        return;
    }

    if !result.failed.is_empty() {
        let mut msg = format!(
            "VRT failed: {} of {} fixtures differ (PDF byte-wise)\n",
            result.failed.len(),
            result.total
        );
        for f in &result.failed {
            let _ = writeln!(
                msg,
                "  - {} (reference={} bytes, actual={} bytes)",
                f.path.display(),
                f.reference_size,
                f.actual_size,
            );
        }
        msg.push_str("\nTo update all goldens:    FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt\n");
        msg.push_str(
            "To update failing only:   FULGUR_VRT_UPDATE=failing cargo test -p fulgur-vrt\n",
        );
        msg.push_str("Inspect diff images:      ls target/vrt-diff/\n");
        panic!("{msg}");
    }

    assert_eq!(result.passed, result.total);
}
