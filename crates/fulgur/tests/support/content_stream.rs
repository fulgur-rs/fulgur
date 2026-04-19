use std::process::Command;

/// Counts of PDF content-stream operators after a qpdf `--qdf` expansion.
/// Only tracks the operators we care about in border/text optimization work.
#[derive(Debug, Default, Clone)]
pub struct OpCounts {
    pub m: usize,
    pub l: usize,
    pub re: usize,
    pub s_stroke: usize,
    pub q: usize,
    pub bt: usize,
    pub rg_stroke: usize,
}

/// Run `qpdf --qdf --object-streams=disable` on `pdf_bytes` and count
/// PDF operators. Returns `None` if qpdf is not installed (tests should
/// skip instead of fail — CI always has it, local devs may not).
pub fn count_ops(pdf_bytes: &[u8]) -> Option<OpCounts> {
    let tmp = tempfile::NamedTempFile::new().ok()?;
    let out = tempfile::NamedTempFile::new().ok()?;
    std::fs::write(tmp.path(), pdf_bytes).ok()?;

    let status = Command::new("qpdf")
        .args(["--qdf", "--object-streams=disable"])
        .arg(tmp.path())
        .arg(out.path())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }

    let qdf = std::fs::read_to_string(out.path()).ok()?;
    let mut c = OpCounts::default();
    for line in qdf.lines() {
        let l = line.trim_end();
        if l.ends_with(" m") || l == "m" {
            c.m += 1;
        } else if l.ends_with(" l") || l == "l" {
            c.l += 1;
        } else if l.ends_with(" re") {
            c.re += 1;
        } else if l == "S" || l.ends_with(" S") {
            c.s_stroke += 1;
        } else if l == "q" {
            c.q += 1;
        } else if l == "BT" {
            c.bt += 1;
        } else if l.ends_with(" RG") {
            c.rg_stroke += 1;
        }
    }
    Some(c)
}
