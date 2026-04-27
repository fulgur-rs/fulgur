//! Split one or more `expectations/<name>.txt` files into N round-balanced
//! shards under `expectations/lists/<prefix><i>.txt`, so the WPT CI matrix
//! can run them in parallel through the `wpt_lists` cherry-pick runner.
//!
//! Usage (initial split from a fresh seed; `seed.rs` writes the
//! intermediate file to `target/wpt-seed/<name>.all.txt`):
//!
//!     cargo run -p fulgur-wpt --example shard_list -- \
//!         --from target/wpt-seed/css-multicol.all.txt \
//!         --shards 3 \
//!         --output-prefix crates/fulgur-wpt/expectations/lists/multicol-
//!
//! Re-balance an already-sharded set:
//!     cargo run -p fulgur-wpt --example shard_list -- \
//!         --from crates/fulgur-wpt/expectations/lists/multicol-1.txt \
//!         --from crates/fulgur-wpt/expectations/lists/multicol-2.txt \
//!         --from crates/fulgur-wpt/expectations/lists/multicol-3.txt \
//!         --shards 3 \
//!         --output-prefix crates/fulgur-wpt/expectations/lists/multicol-
//!
//! Filter to a subdirectory and exclude SKIP entries:
//!     cargo run -p fulgur-wpt --example shard_list -- \
//!         --from crates/fulgur-wpt/expectations/lists/bugs.txt \
//!         --filter css/css-grid/ \
//!         --status PASS,FAIL \
//!         --shards 2 \
//!         --output-prefix crates/fulgur-wpt/expectations/lists/grid-bug-
//!
//! Splitting strategy: contiguous chunks preserving input order. Each shard
//! gets `total / shards` entries; the remainder is appended to the last
//! shard. Adding a single new entry post-split is therefore safe — append
//! it to the highest-numbered shard, and the next full re-balance will
//! redistribute when drift becomes meaningful.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Status {
    Pass,
    Fail,
    Skip,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }

    fn parse(s: &str) -> Option<Status> {
        match s {
            "PASS" => Some(Status::Pass),
            "FAIL" => Some(Status::Fail),
            "SKIP" => Some(Status::Skip),
            _ => None,
        }
    }
}

struct Entry {
    status: Status,
    path: String,
    note: Option<String>,
}

impl Entry {
    fn render(&self) -> String {
        // Match the seed.rs format: `STATUS<4-char field>  path[  # note]`.
        match &self.note {
            Some(n) => format!("{:4}  {}  # {}", self.status.as_str(), self.path, n),
            None => format!("{:4}  {}", self.status.as_str(), self.path),
        }
    }
}

fn main() -> Result<()> {
    let mut from: Vec<PathBuf> = Vec::new();
    let mut shards: Option<usize> = None;
    let mut output_prefix: Option<String> = None;
    let mut filter: Option<String> = None;
    let mut status_filter: Option<BTreeSet<Status>> = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--from" => from.push(PathBuf::from(args.next().context("--from needs value")?)),
            "--shards" => {
                shards = Some(
                    args.next()
                        .context("--shards needs value")?
                        .parse()
                        .context("--shards must be a positive integer")?,
                )
            }
            "--output-prefix" => {
                output_prefix = Some(args.next().context("--output-prefix needs value")?)
            }
            "--filter" => filter = Some(args.next().context("--filter needs value")?),
            "--status" => {
                let raw = args.next().context("--status needs value")?;
                let mut set = BTreeSet::new();
                for tok in raw.split(',') {
                    let s = Status::parse(tok.trim())
                        .with_context(|| format!("--status: unknown token `{tok}`"))?;
                    set.insert(s);
                }
                status_filter = Some(set);
            }
            other => bail!("unknown flag: {other}"),
        }
    }

    if from.is_empty() {
        bail!("--from required (repeat to combine multiple inputs)");
    }
    let shards = shards.context("--shards required")?;
    if shards == 0 {
        bail!("--shards must be > 0");
    }
    let output_prefix = output_prefix.context("--output-prefix required")?;

    // Load and parse all inputs while preserving relative order.
    let mut entries: Vec<Entry> = Vec::new();
    let mut input_data_lines = 0usize;
    for path in &from {
        let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        for (lineno, line) in body.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            input_data_lines += 1;
            let entry = parse_entry(line)
                .with_context(|| format!("parse error at {}:{}", path.display(), lineno + 1))?;
            if let Some(prefix) = &filter {
                if !entry.path.starts_with(prefix) {
                    continue;
                }
            }
            if let Some(set) = &status_filter {
                if !set.contains(&entry.status) {
                    continue;
                }
            }
            entries.push(entry);
        }
    }

    let kept = entries.len();
    if kept == 0 {
        bail!("no entries matched the filters — refusing to write empty shards");
    }

    // Contiguous chunks preserving input order. The remainder is appended
    // to the last shard so that "append new tests to the highest shard"
    // remains the natural workflow between rebalances.
    let base = kept / shards;
    let mut buckets: Vec<Vec<&Entry>> = Vec::with_capacity(shards);
    let mut cursor = 0;
    for i in 0..shards {
        let len = if i + 1 == shards { kept - cursor } else { base };
        let bucket: Vec<&Entry> = entries[cursor..cursor + len].iter().collect();
        cursor += len;
        buckets.push(bucket);
    }
    debug_assert_eq!(cursor, kept);

    // Build the rebalance command line that gets embedded in each
    // shard's header. We list every `--from` explicitly because the
    // parser consumes exactly one path per `--from` flag — a shell glob
    // (`--from prefix*.txt`) would expand to multiple bare arguments and
    // trip the "unknown flag" error.
    let rebalance_from_args = (1..=shards)
        .map(|n| format!("--from {output_prefix}{n}.txt"))
        .collect::<Vec<_>>()
        .join(" ");

    let mut total_written = 0usize;
    for (i, bucket) in buckets.iter().enumerate() {
        let shard_no = i + 1;
        let path = PathBuf::from(format!("{output_prefix}{shard_no}.txt"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
        let mut counts = [0u32; 3];
        for e in bucket {
            match e.status {
                Status::Pass => counts[0] += 1,
                Status::Fail => counts[1] += 1,
                Status::Skip => counts[2] += 1,
            }
        }
        let mut f =
            fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
        writeln!(
            f,
            "# Generated by `cargo run -p fulgur-wpt --example shard_list`."
        )?;
        writeln!(
            f,
            "# Shard {shard_no}/{shards}: {len} entries ({pass} PASS, {fail} FAIL, {skip} SKIP).",
            len = bucket.len(),
            pass = counts[0],
            fail = counts[1],
            skip = counts[2],
        )?;
        writeln!(
            f,
            "# Re-balance with `cargo run -p fulgur-wpt --example shard_list -- {rebalance_from_args} --shards {shards} --output-prefix {output_prefix}`."
        )?;
        writeln!(f)?;
        for e in bucket {
            writeln!(f, "{}", e.render())?;
        }
        total_written += bucket.len();
        println!(
            "wrote {} ({} entries: {} PASS, {} FAIL, {} SKIP)",
            path.display(),
            bucket.len(),
            counts[0],
            counts[1],
            counts[2],
        );
    }

    // Conservation check: every input data line either matched the
    // filters and got written, or was filtered out — never silently lost.
    let filtered_out = input_data_lines - kept;
    assert_eq!(
        total_written, kept,
        "BUG: bucket sum {total_written} != kept {kept}"
    );
    println!(
        "summary: read {input_data_lines} data lines, kept {kept}, filtered out {filtered_out}, wrote {total_written} across {shards} shards"
    );
    Ok(())
}

fn parse_entry(line: &str) -> Result<Entry> {
    // Format: `STATUS  path  # optional note`. The seed.rs writer pads
    // STATUS to width 4 and uses two spaces as the separator, but we
    // accept any whitespace to stay tolerant of hand edits.
    let mut chars = line.char_indices();
    let mut status_end = 0;
    for (i, ch) in chars.by_ref() {
        if ch.is_whitespace() {
            status_end = i;
            break;
        }
        status_end = i + ch.len_utf8();
    }
    let status_str = &line[..status_end];
    let status =
        Status::parse(status_str).with_context(|| format!("unknown status `{status_str}`"))?;

    let rest = line[status_end..].trim_start();
    // Split off the `# note` suffix if present.
    let (path, note) = match rest.find("  #").or_else(|| rest.find(" #")) {
        Some(hash_at) => {
            let path = rest[..hash_at].trim().to_string();
            let note = rest[hash_at..]
                .trim_start_matches([' ', '#'])
                .trim()
                .to_string();
            (path, if note.is_empty() { None } else { Some(note) })
        }
        None => (rest.trim().to_string(), None),
    };
    if path.is_empty() {
        bail!("empty path");
    }
    Ok(Entry { status, path, note })
}
