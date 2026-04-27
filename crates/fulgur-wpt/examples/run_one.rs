//! Run a single WPT reftest end-to-end and print the verdict.
//!
//! Usage:
//!     cargo run -p fulgur-wpt --example run_one -- <test.html> [--dpi N]
//!
//! Outputs are written under `target/wpt-run/<test-stem>/` (test/, ref/, diff/).

use anyhow::{Context, Result, bail};
use fulgur_wpt::expectations::Expectation;
use fulgur_wpt::harness::run_one;
use std::path::{Path, PathBuf};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let test_arg = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: run_one <test.html> [--dpi N]"))?;
    let mut dpi: u32 = 96;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--dpi" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--dpi needs a value"))?;
                dpi = v.parse().context("parse --dpi value")?;
                if dpi == 0 {
                    bail!("--dpi must be greater than 0");
                }
            }
            other => bail!("unknown flag: {other}"),
        }
    }

    let test_path = PathBuf::from(&test_arg);
    let stem = Path::new(&test_arg)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "test".to_string());
    let out_root = PathBuf::from("target/wpt-run").join(&stem);
    let work_dir = out_root.join("work");
    let diff_dir = out_root.join("diff");

    println!("test:    {}", test_path.display());
    println!("dpi:     {dpi}");
    println!("work:    {}", work_dir.display());
    println!("diff:    {}", diff_dir.display());

    let outcome = run_one(&test_path, &work_dir, &diff_dir, dpi, None)?;
    let verdict = match outcome.observed {
        Expectation::Pass => "PASS",
        Expectation::Fail => "FAIL",
        Expectation::Skip => "SKIP",
    };
    println!("\nverdict: {verdict}");
    if let Some(reason) = &outcome.reason {
        println!("reason:  {reason}");
    }
    if let Some(diff) = &outcome.diff_dir {
        println!("diffs:   {}", diff.display());
    }
    Ok(())
}
