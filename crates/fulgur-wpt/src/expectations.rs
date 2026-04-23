//! Expectations file parser and PASS/FAIL/SKIP judgement.
//!
//! An expectation file is a plain-text list, one entry per line:
//!
//! ```text
//! # comments start with '#'
//! PASS css/css-page/a.html
//! FAIL css/css-page/b.html  # regression since 2026-04
//! SKIP css/css-page/c.html  # manual-only test
//! ```
//!
//! Blank lines and lines starting with `#` are ignored. An inline `#`
//! starts a trailing comment that is captured but does not affect
//! parsing. Duplicate entries for the same path are rejected.
//!
//! Note: `#` anywhere inside a line begins an inline comment. WPT test
//! paths do not contain `#`, so splitting on the first `#` is safe in
//! practice.

use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expectation {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Clone, Default)]
pub struct ExpectationFile {
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone)]
struct Entry {
    expectation: Expectation,
    /// Optional trailing `# ...` comment. Exposed via
    /// [`ExpectationFile::comment`] for report output.
    comment: Option<String>,
}

impl ExpectationFile {
    pub fn parse(src: &str) -> Result<Self> {
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for (lineno, raw) in src.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (body, comment) = match line.find('#') {
                Some(idx) => (line[..idx].trim(), Some(line[idx + 1..].trim().to_string())),
                None => (line, None),
            };
            let mut parts = body.splitn(2, char::is_whitespace);
            let status = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("").trim();
            if path.is_empty() {
                bail!("line {}: missing path", lineno + 1);
            }
            let expectation = match status {
                "PASS" => Expectation::Pass,
                "FAIL" => Expectation::Fail,
                "SKIP" => Expectation::Skip,
                other => bail!("line {}: unknown status {other}", lineno + 1),
            };
            if entries.contains_key(path) {
                bail!("line {}: duplicate entry for {path}", lineno + 1);
            }
            entries.insert(
                path.to_string(),
                Entry {
                    expectation,
                    comment,
                },
            );
        }
        Ok(Self { entries })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::parse(&s)
    }

    pub fn get(&self, test_path: &str) -> Option<Expectation> {
        self.entries.get(test_path).map(|e| e.expectation)
    }

    /// Return the comment associated with an entry, if any.
    /// Useful for human-readable report output explaining why a test is
    /// marked FAIL or SKIP.
    pub fn comment(&self, test_path: &str) -> Option<&str> {
        self.entries
            .get(test_path)
            .and_then(|e| e.comment.as_deref())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate all test paths registered in the file (sorted).
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Regression,
    Promotion,
    Skipped,
    UnknownTest,
}

/// Compare the declared expectation with the observed result.
///
/// `observed` is not optional because the harness always produces a
/// concrete outcome (or errors before calling `judge`). Keeping it
/// non-Optional makes the match exhaustive and prevents an accidental
/// `None` from collapsing into the catch-all `Ok` arm.
pub fn judge(declared: Option<Expectation>, observed: Expectation) -> Verdict {
    use Expectation::{Fail, Pass, Skip};
    match (declared, observed) {
        // Declared SKIP wins over whatever we observed.
        (Some(Skip), _) => Verdict::Skipped,
        // No entry in the expectations file → unknown test.
        (None, _) => Verdict::UnknownTest,
        // Matches.
        (Some(Pass), Pass) => Verdict::Ok,
        (Some(Fail), Fail) => Verdict::Ok,
        // Mismatches.
        (Some(Pass), Fail) => Verdict::Regression,
        (Some(Fail), Pass) => Verdict::Promotion,
        // Declared PASS/FAIL + observed SKIP: treated as Ok. The harness
        // opted out of running the test for some reason; not a regression.
        (Some(Pass), Skip) => Verdict::Ok,
        (Some(Fail), Skip) => Verdict::Ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_three_statuses() {
        let src = "\
# comment line
PASS css/css-page/a.html
FAIL css/css-page/b.html  # reason
SKIP css/css-page/c.html  # manual

";
        let f = ExpectationFile::parse(src).unwrap();
        assert_eq!(f.get("css/css-page/a.html"), Some(Expectation::Pass));
        assert_eq!(f.get("css/css-page/b.html"), Some(Expectation::Fail));
        assert_eq!(f.get("css/css-page/c.html"), Some(Expectation::Skip));
        assert_eq!(f.len(), 3);
    }

    #[test]
    fn ignores_blank_and_comment_lines() {
        let src = "\n  \n# foo\nPASS x\n";
        let f = ExpectationFile::parse(src).unwrap();
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn rejects_unknown_status() {
        assert!(ExpectationFile::parse("XYZZY a.html\n").is_err());
    }

    #[test]
    fn rejects_duplicate_entry() {
        let src = "PASS a.html\nFAIL a.html\n";
        assert!(ExpectationFile::parse(src).is_err());
    }

    #[test]
    fn judge_pass_pass_is_ok() {
        assert_eq!(
            judge(Some(Expectation::Pass), Expectation::Pass),
            Verdict::Ok
        );
    }

    #[test]
    fn judge_pass_fail_is_regression() {
        assert_eq!(
            judge(Some(Expectation::Pass), Expectation::Fail),
            Verdict::Regression
        );
    }

    #[test]
    fn judge_fail_pass_is_promotion() {
        assert_eq!(
            judge(Some(Expectation::Fail), Expectation::Pass),
            Verdict::Promotion
        );
    }

    #[test]
    fn judge_skip_wins() {
        assert_eq!(
            judge(Some(Expectation::Skip), Expectation::Pass),
            Verdict::Skipped
        );
    }

    #[test]
    fn judge_unknown_declared() {
        assert_eq!(judge(None, Expectation::Pass), Verdict::UnknownTest);
    }

    // ---- Extra coverage ----------------------------------------------------

    #[test]
    fn parses_tab_separated_fields() {
        let src = "PASS\tcss/css-page/tabbed.html\n";
        let f = ExpectationFile::parse(src).unwrap();
        assert_eq!(f.get("css/css-page/tabbed.html"), Some(Expectation::Pass));
    }

    #[test]
    fn parses_crlf_line_endings() {
        let src = "PASS a.html\r\nFAIL b.html  # reason\r\n";
        let f = ExpectationFile::parse(src).unwrap();
        assert_eq!(f.get("a.html"), Some(Expectation::Pass));
        assert_eq!(f.get("b.html"), Some(Expectation::Fail));
    }

    #[test]
    fn empty_file_yields_empty() {
        let f = ExpectationFile::parse("").unwrap();
        assert!(f.is_empty());
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn rejects_missing_path() {
        // Bare status with no path.
        assert!(ExpectationFile::parse("PASS\n").is_err());
    }

    // Current contract: "declared PASS, observed SKIP" is not a regression.
    // It is pinned explicitly by the match so any future tightening of
    // the rule is deliberate rather than accidental.
    #[test]
    fn judge_pass_declared_skip_observed_is_ok() {
        assert_eq!(
            judge(Some(Expectation::Pass), Expectation::Skip),
            Verdict::Ok
        );
    }

    #[test]
    fn comment_is_exposed_per_entry() {
        let src =
            "PASS a.html  # all good\nFAIL b.html\nSKIP c.html  # manual interaction needed\n";
        let f = ExpectationFile::parse(src).unwrap();
        assert_eq!(f.comment("a.html"), Some("all good"));
        assert_eq!(f.comment("b.html"), None);
        assert_eq!(f.comment("c.html"), Some("manual interaction needed"));
        assert_eq!(f.comment("nonexistent.html"), None);
    }

    #[test]
    fn paths_iterates_sorted() {
        // BTreeMap-backed, so insertion order differs from iteration order.
        let src = "PASS z.html\nPASS a.html\nFAIL m.html\n";
        let f = ExpectationFile::parse(src).unwrap();
        let collected: Vec<&str> = f.paths().collect();
        assert_eq!(collected, vec!["a.html", "m.html", "z.html"]);
    }

    #[test]
    fn paths_on_empty_file_yields_nothing() {
        let f = ExpectationFile::default();
        assert_eq!(f.paths().count(), 0);
    }
}
