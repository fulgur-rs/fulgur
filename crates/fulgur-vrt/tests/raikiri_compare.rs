//! Render every fixture through the Raikiri backend and compare it against
//! the committed Blitz goldens. Measurement only: the report lands in
//! `target/vrt-report/raikiri/` and differences never fail the test.
#![cfg(feature = "raikiri")]

use fulgur_vrt::engine_compare::{self, Outcome};
use fulgur_vrt::pdf_render;
use fulgur_vrt::runner::RunnerContext;

#[test]
fn compare_raikiri_with_blitz_goldens() {
    let ctx = RunnerContext::discover().expect("discover context");
    let report_dir = engine_compare::report_dir(&ctx, "raikiri");
    if report_dir.exists() {
        std::fs::remove_dir_all(&report_dir).expect("clear previous report");
    }
    let report = engine_compare::compare_with_goldens(
        &ctx,
        "raikiri",
        &pdf_render::render_html_to_pdf_raikiri,
        &report_dir,
    )
    .expect("comparison run failed");
    engine_compare::write_report(&report, &report_dir).expect("write report");

    eprintln!(
        "raikiri vs blitz goldens: {} fixtures, {} match, {} differ, {} page count mismatch, {} render error",
        report.fixtures.len(),
        report.count(Outcome::Match),
        report.count(Outcome::Differs),
        report.count(Outcome::PageCountMismatch),
        report.count(Outcome::RenderError),
    );
    eprintln!("report: {}", report_dir.join("summary.md").display());
}
