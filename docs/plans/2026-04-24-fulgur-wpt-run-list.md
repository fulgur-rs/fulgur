# fulgur-wpt: cross-subdir cherry-pick runner (`run_list`) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Let `fulgur-wpt` run an arbitrary set of WPT reftests enumerated in an expectations-file list, so bugs can cherry-pick tests across WPT subdirs and heavy phases (css-multicol) can be sharded. Adding a new `expectations/lists/<name>.txt` automatically produces a `cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_<name>` entry with no Rust edits.

**Architecture:** (1) Refactor `runner::run_phase` to split out its "given a set of test paths + expectations, run them and emit artifacts" inner loop into a private helper. (2) Add a new public `run_list(workspace, list_name, expectations_path, dpi)` that derives the test set from the expectations file and calls the shared helper. (3) Add `crates/fulgur-wpt/build.rs` that scans `expectations/lists/*.txt` and emits one `#[test] fn wpt_list_<stem>()` per file into `$OUT_DIR/wpt_lists_generated.rs`. (4) Add `crates/fulgur-wpt/tests/wpt_lists.rs` that defines a shared `run(list_name: &str)` helper and `include!`s the generated code. (5) Ship a `smoke.txt` list with one known-PASS test to prove the pipeline end-to-end.

**Tech Stack:** Rust 2021, existing `fulgur-wpt` harness (`anyhow`, `serde`, `tempfile` in dev-deps). Cargo `build.rs` auto-detected, no `syn`/`quote` — we emit raw strings.

---

## Task 1: Extract `execute_and_report` helper from `run_phase`

**Files:**

- Modify: `crates/fulgur-wpt/src/runner.rs` (lines 55-211)

**Goal:** Pull the "iterate tests → judge → accumulate → write artifacts → return PhaseOutcome" block out of `run_phase` into a private helper that takes `(workspace_root, label, tests: Vec<PathBuf>, declared, dpi)`. No behavior change — `run_phase` keeps doing `is_dir`, `poppler_available`, `ExpectationFile::load`, `collect_reftest_files`, then delegates.

This is a pure refactor. The test is: the existing `wpt_css_page` / `wpt_css_multicol` integration tests must still behave the same (same counts, same artifacts). We don't add a unit test; we rely on compile + existing integration.

**Step 1: Sketch the helper signature**

Inside `crates/fulgur-wpt/src/runner.rs`, below `run_phase`, add a private function:

```rust
fn execute_and_report(
    workspace_root: &Path,
    label: &str,
    tests: Vec<PathBuf>,
    declared: ExpectationFile,
    dpi: u32,
) -> Result<PhaseOutcome> {
    // Body: lines currently in run_phase from `let total = tests.len();`
    // through the final `Ok(Some(PhaseOutcome { ... }))`, with the
    // wrapping Option<> unwrapped (it was None only on early returns).
    //
    // - replace `subdir` → `label` everywhere in the body (variable names,
    //   report_dir = workspace_root.join("target/wpt-report").join(label),
    //   PhaseOutcome.subdir = label.to_string())
    // - `wpt_root` used for strip_prefix — recompute as
    //   `workspace_root.join("target/wpt")` inside the helper
}
```

**Step 2: Refactor `run_phase` to call the helper**

Replace the body of `run_phase` (lines 55-211) so that after the existing early-return guards it becomes:

```rust
pub fn run_phase(workspace_root: &Path, subdir: &str, dpi: u32) -> Result<Option<PhaseOutcome>> {
    let wpt_root = workspace_root.join("target/wpt");
    let dir = wpt_root.join("css").join(subdir);
    if !dir.is_dir() {
        eprintln!("skip: {} missing (run scripts/wpt/fetch.sh first)", dir.display());
        return Ok(None);
    }
    if !poppler_available() {
        eprintln!("skip: pdftocairo not available on PATH");
        return Ok(None);
    }
    let expect_path = workspace_root
        .join("crates/fulgur-wpt/expectations")
        .join(format!("{subdir}.txt"));
    let declared = if expect_path.exists() {
        ExpectationFile::load(&expect_path)
            .with_context(|| format!("load {}", expect_path.display()))?
    } else {
        eprintln!("note: {} missing, treating every test as unknown", expect_path.display());
        ExpectationFile::default()
    };
    let tests = collect_reftest_files(&dir)
        .with_context(|| format!("collect reftest files in {}", dir.display()))?;
    Ok(Some(execute_and_report(workspace_root, subdir, tests, declared, dpi)?))
}
```

Ensure `PhaseOutcome.subdir` is still populated correctly — the field name stays `subdir` even though it's really a label now, to avoid breaking the public struct.

**Step 3: Verify compile**

Run: `cargo build -p fulgur-wpt --tests`
Expected: clean build, no warnings.

**Step 4: Verify existing behavior unchanged**

Run: `cargo test -p fulgur-wpt --lib`
Expected: `test result: ok. 55 passed; 0 failed` (the lib unit tests for expectations/report/reftest modules).

Run: `cargo test -p fulgur-wpt --test wpt_css_page 2>&1 | tail -3`
Expected: `test result: ok. 1 passed` OR `skip: target/wpt/css/css-page missing` depending on whether WPT was fetched locally. Either is acceptable; the test doesn't panic.

**Step 5: Commit**

```bash
git add crates/fulgur-wpt/src/runner.rs
git commit -m "refactor(fulgur-wpt): extract execute_and_report from run_phase

Pure refactor. Splits the iterate-judge-report inner loop so the
upcoming run_list entry point can reuse it without duplicating the
artifact-generation pipeline."
```

---

## Task 2: Add `run_list` public API

**Files:**

- Modify: `crates/fulgur-wpt/src/runner.rs`

**Goal:** Add `pub fn run_list(workspace_root, list_name, expectations_path, dpi)` that loads the expectations file, builds the test-path list from its keys, and delegates to `execute_and_report`. Missing-WPT / missing-poppler guards behave the same way as `run_phase` (returns `Ok(None)`).

**Step 1: Write the doc + signature**

Append to `crates/fulgur-wpt/src/runner.rs` after `run_phase` (and above `execute_and_report`):

```rust
/// Run exactly the reftests enumerated in `expectations_path` — keys of
/// the expectations file are interpreted as WPT-root-relative paths
/// (e.g. `css/css-page/foo.html`). Cross-subdir: paths may live under
/// any `css/<subdir>` as long as they exist in the fetched WPT snapshot.
///
/// Writes artifacts under `target/wpt-report/<list_name>/`.
///
/// Returns `Ok(None)` when prerequisites are missing (no WPT checkout,
/// no pdftocairo, or the expectations file doesn't exist).
pub fn run_list(
    workspace_root: &Path,
    list_name: &str,
    expectations_path: &Path,
    dpi: u32,
) -> Result<Option<PhaseOutcome>> {
    let wpt_root = workspace_root.join("target/wpt");
    if !wpt_root.is_dir() {
        eprintln!(
            "skip: {} missing (run scripts/wpt/fetch.sh first)",
            wpt_root.display()
        );
        return Ok(None);
    }
    if !poppler_available() {
        eprintln!("skip: pdftocairo not available on PATH");
        return Ok(None);
    }
    if !expectations_path.exists() {
        eprintln!(
            "skip: {} missing (list has no expectations file)",
            expectations_path.display()
        );
        return Ok(None);
    }

    let declared = ExpectationFile::load(expectations_path)
        .with_context(|| format!("load {}", expectations_path.display()))?;

    // Test paths are the keys of the expectations file. Skip entries
    // whose file is missing from the WPT snapshot (surface them as
    // "missing" via eprintln, but don't fail the whole run).
    let mut tests: Vec<PathBuf> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for rel in declared.paths() {
        let abs = wpt_root.join(rel);
        if abs.is_file() {
            tests.push(abs);
        } else {
            missing.push(rel.to_string());
        }
    }
    tests.sort();

    if !missing.is_empty() {
        eprintln!(
            "warning: {} test(s) in {} missing from WPT snapshot; update subset.txt:",
            missing.len(),
            expectations_path.display()
        );
        for m in &missing {
            eprintln!("  - {m}");
        }
    }

    Ok(Some(execute_and_report(
        workspace_root,
        list_name,
        tests,
        declared,
        dpi,
    )?))
}
```

**Step 2: Expose `paths()` on `ExpectationFile`**

Modify `crates/fulgur-wpt/src/expectations.rs` to add a helper that iterates registered paths in sorted order (BTreeMap already sorts):

```rust
impl ExpectationFile {
    /// Iterate all test paths registered in the file (sorted).
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}
```

Place after the existing `is_empty` method.

**Step 3: Verify compile + lib tests**

Run: `cargo test -p fulgur-wpt --lib 2>&1 | tail -3`
Expected: `test result: ok. 55 passed; 0 failed` (lib tests unchanged).

Run: `cargo clippy -p fulgur-wpt --all-targets -- -D warnings 2>&1 | tail -5`
Expected: no warnings / errors.

**Step 4: Commit**

```bash
git add crates/fulgur-wpt/src/runner.rs crates/fulgur-wpt/src/expectations.rs
git commit -m "feat(fulgur-wpt): run_list entry for cross-subdir test cherry-pick

run_list loads an expectations file, treats its keys as WPT-root-relative
test paths, and hands off to the shared execute_and_report helper.
Missing tests are logged as warnings rather than failing the phase,
so subset.txt and expectations/lists/*.txt can be edited independently."
```

---

## Task 3: Add `build.rs` to generate `#[test]` per list

**Files:**

- Create: `crates/fulgur-wpt/build.rs`

**Goal:** `build.rs` scans `expectations/lists/*.txt` at build time and writes one `#[test] fn wpt_list_<stem>()` per file into `$OUT_DIR/wpt_lists_generated.rs`. Hyphens in stems are sanitized to underscores for Rust identifiers. `cargo:rerun-if-changed=expectations/lists` catches new/deleted files.

**Step 1: Write the build.rs**

Create `crates/fulgur-wpt/build.rs`:

```rust
//! Generate one #[test] fn per file in expectations/lists/.
//!
//! The generated file is pulled in by tests/wpt_lists.rs via
//! `include!(concat!(env!("OUT_DIR"), "/wpt_lists_generated.rs"))`.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=expectations/lists");
    println!("cargo:rerun-if-changed=build.rs");

    let lists_dir = Path::new("expectations/lists");
    let mut stems: Vec<String> = Vec::new();
    if lists_dir.is_dir() {
        for entry in fs::read_dir(lists_dir).expect("read expectations/lists") {
            let entry = entry.expect("read entry");
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("txt") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("utf-8 stem")
                .to_string();
            stems.push(stem);
        }
    }
    stems.sort();

    let mut out = String::new();
    out.push_str("// @generated by build.rs — do not edit.\n\n");
    for stem in &stems {
        let ident = sanitize_ident(stem);
        out.push_str(&format!(
            "#[test] fn wpt_list_{ident}() {{ run(\"{stem}\"); }}\n"
        ));
    }

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let out_path = Path::new(&out_dir).join("wpt_lists_generated.rs");
    fs::write(&out_path, out).expect("write generated tests");
}

/// Turn a filename stem into a valid Rust identifier: replace every
/// non-alphanumeric char with '_'. Leading digits get a `_` prefix.
fn sanitize_ident(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            out.push('_');
        }
        if i == 0 && ch.is_ascii_digit() {
            let fixed = format!("_{out}");
            return fixed;
        }
    }
    out
}
```

**Step 2: Verify build.rs is picked up by cargo**

Cargo auto-detects `build.rs` at the crate root; no Cargo.toml change needed. Confirm:

Run: `cargo build -p fulgur-wpt --tests 2>&1 | tail -5`
Expected: clean build. `build.rs` runs but produces an empty generated file (since `expectations/lists/` doesn't exist yet).

Run: `ls target/debug/build/fulgur-wpt-*/out/wpt_lists_generated.rs`
Expected: file exists and contains only the `// @generated` header line.

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/build.rs
git commit -m "feat(fulgur-wpt): build.rs generates #[test] per lists/*.txt

Walks expectations/lists/*.txt at build time and emits one
#[test] fn wpt_list_<stem>() per file into \$OUT_DIR/wpt_lists_generated.rs.
cargo:rerun-if-changed on the dir catches new/removed files so adding
a list is a pure .txt edit."
```

---

## Task 4: Add `tests/wpt_lists.rs` stub

**Files:**

- Create: `crates/fulgur-wpt/tests/wpt_lists.rs`

**Goal:** One hand-written test-binary file that defines the `run(list_name: &str)` helper and includes the generated `#[test]` fns. Pattern follows the existing `wpt_css_page.rs` and `wpt_css_multicol.rs` skip-gating.

**Step 1: Write the stub**

Create `crates/fulgur-wpt/tests/wpt_lists.rs`:

```rust
//! Cherry-pick WPT reftest entry: runs arbitrary lists of tests
//! enumerated in crates/fulgur-wpt/expectations/lists/<name>.txt.
//!
//! Each `.txt` in that directory becomes one #[test] fn, generated by
//! build.rs at compile time. No edits here are required when adding a
//! new list — just drop a .txt file.

use fulgur_wpt::runner::run_list;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn run(list_name: &str) {
    let expectations = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("expectations/lists")
        .join(format!("{list_name}.txt"));
    let outcome =
        run_list(&workspace_root(), list_name, &expectations, 96).expect("runner should not error");
    match outcome {
        Some(o) => eprintln!("list {list_name} report at {}", o.report_dir.display()),
        // `FULGUR_WPT_REQUIRED=1` is set only by the dedicated `wpt`
        // matrix job. Other CI cells and local dev without WPT fetched
        // must skip silently.
        None if std::env::var_os("FULGUR_WPT_REQUIRED").is_some() => {
            panic!("wpt_list {list_name} prerequisites missing (run scripts/wpt/fetch.sh + install poppler-utils)");
        }
        None => {}
    }
}

// Generated by build.rs: one `#[test] fn wpt_list_<stem>()` per file in
// expectations/lists/*.txt. The file may be empty if no lists exist yet.
include!(concat!(env!("OUT_DIR"), "/wpt_lists_generated.rs"));
```

**Step 2: Verify compile**

Run: `cargo test -p fulgur-wpt --test wpt_lists -- --list 2>&1 | tail -10`
Expected: build succeeds, output lists 0 tests (no list files yet):

```text
0 tests, 0 benchmarks
```

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/tests/wpt_lists.rs
git commit -m "feat(fulgur-wpt): wpt_lists test binary wraps build.rs-generated tests

Hand-written stub that include!s the generated #[test] fns and defines
the shared run() helper. Adding a new expectations/lists/<name>.txt
automatically grows a wpt_list_<name> test with no edits here."
```

---

## Task 5: Add `smoke.txt` and verify end-to-end

**Files:**

- Create: `crates/fulgur-wpt/expectations/lists/smoke.txt`

**Goal:** One minimal list that references a known-PASS test already in the fetched WPT subset. Running `cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_smoke` proves the pipeline end-to-end (when WPT + poppler are present).

**Step 1: Pick a known-PASS test from css-page expectations**

Run: `grep "^PASS " crates/fulgur-wpt/expectations/css-page.txt | head -3`
Expected: at least one line like `PASS  css/css-page/basic-pagination-002-print.html`. Pick the first one.

**Step 2: Write the smoke list**

Create `crates/fulgur-wpt/expectations/lists/smoke.txt`:

```text
# Sanity check for the lists/ runner — one known-PASS test.
# If this fails, wpt_lists infrastructure is broken (not the test).
PASS  css/css-page/basic-pagination-002-print.html
```

(Replace the path with whatever `grep` returned in Step 1 if basic-pagination-002 wasn't on the PASS list.)

**Step 3: Verify the generated test appears**

Run: `cargo test -p fulgur-wpt --test wpt_lists -- --list 2>&1 | tail -5`
Expected: one line `wpt_list_smoke: test` plus a `1 tests, 0 benchmarks` summary.

**Step 4: Verify the test runs (or skips cleanly when WPT absent)**

Run: `cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_smoke 2>&1 | tail -10`

Two acceptable outcomes:

1. WPT fetched + pdftocairo installed → test passes, `target/wpt-report/smoke/report.json` exists.
2. WPT not fetched or pdftocairo missing → test prints `skip: ...` and passes silently.

Run: `ls target/wpt-report/smoke/ 2>&1`
Expected (case 1): `report.json  regressions.json  summary.md`
Expected (case 2): directory doesn't exist — acceptable.

**Step 5: Verify rerun-if-changed detects new lists**

Run:

```bash
touch crates/fulgur-wpt/expectations/lists/dummy.txt
cargo build -p fulgur-wpt --tests 2>&1 | tail -3
cargo test -p fulgur-wpt --test wpt_lists -- --list 2>&1 | grep wpt_list_
rm crates/fulgur-wpt/expectations/lists/dummy.txt
cargo build -p fulgur-wpt --tests 2>&1 | tail -3
```

Expected: after `touch`, `--list` shows both `wpt_list_smoke` and `wpt_list_dummy`. After `rm`, only `wpt_list_smoke` remains. This confirms build.rs's `rerun-if-changed` directive fires on directory mutations.

**Step 6: Commit**

```bash
git add crates/fulgur-wpt/expectations/lists/smoke.txt
git commit -m "test(fulgur-wpt): add lists/smoke.txt sanity test

Minimal cherry-pick list that references one known-PASS css-page
reftest. Proves the build.rs → wpt_lists generation pipeline
end-to-end and catches breakage before bug-specific lists are added."
```

---

## Task 6: Update `crates/fulgur-wpt/README.md`

**Files:**

- Modify: `crates/fulgur-wpt/README.md`

**Goal:** Document the new `lists/` mechanism so future contributors can add their own cross-subdir lists without spelunking the source.

**Step 1: Append a new section**

Append to `crates/fulgur-wpt/README.md`:

````markdown
## Cross-subdir cherry-pick lists (`expectations/lists/`)

Large, heterogeneous WPT test sets are expensive to run. For bugs that
need a handful of tests across multiple WPT subdirs, drop an
`expectations/lists/<name>.txt` file using the same `PASS | FAIL | SKIP`
format as the phase files — paths may reference **any** subdir under
`css/` as long as they exist in the fetched WPT snapshot.

```text
# crates/fulgur-wpt/expectations/lists/my-list.txt
FAIL  css/css-grid/grid-items-backgrounds-001.html  # fulgur-XXXX
FAIL  css/css-images/linear-gradient-004.html       # fulgur-YYYY
```

Adding the file automatically produces a new cargo test:

```bash
cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_my_list
```

Under the hood `build.rs` scans `expectations/lists/*.txt` at compile
time and emits `#[test] fn wpt_list_<stem>()` for each file (hyphens
become underscores). Adding, removing, or renaming a file is a pure
`.txt` edit — no Rust changes required.

Every test path listed must also be added to `scripts/wpt/subset.txt`
(test file + `-ref.html` both) so the sparse-checkout fetches them.

Artifacts land at `target/wpt-report/<name>/` with the same
`report.json` / `regressions.json` / `summary.md` structure as the
phase runners.

### CI sharding use case

A big phase like `css-multicol` can be split into
`lists/multicol-1.txt`, `multicol-2.txt`, etc., each running as its own
test binary filter. CI matrix jobs invoke
`cargo test ... -- wpt_list_multicol_1` and run in parallel.
````

**Step 2: Lint markdown**

Run: `npx markdownlint-cli2 crates/fulgur-wpt/README.md 2>&1 | tail -5`
Expected: no violations. If any, fix (typically MD032 blank lines around lists).

**Step 3: Commit**

```bash
git add crates/fulgur-wpt/README.md
git commit -m "docs(fulgur-wpt): document expectations/lists/ cherry-pick mechanism"
```

---

## Task 7: Final verification + push

**Goal:** Run the full quality gates to confirm nothing regressed.

**Step 1: Format + clippy**

```bash
cargo fmt --check
cargo clippy -p fulgur-wpt --all-targets -- -D warnings
```

Expected: clean.

**Step 2: Full fulgur-wpt tests**

```bash
cargo test -p fulgur-wpt 2>&1 | tail -20
```

Expected: existing tests unchanged (55 lib + `wpt_css_page` + `wpt_css_multicol` + new `wpt_list_smoke`). Integration tests may skip silently if WPT not fetched locally — that's fine.

**Step 3: Workspace build sanity**

```bash
cargo build --workspace 2>&1 | tail -3
```

Expected: clean build.

**Step 4: Push branch (don't open PR yet — downstream issues fulgur-eye4 and fulgur-645s will consume this)**

```bash
git push -u origin feature/fulgur-6nf0
```

No user-visible behavior change; downstream bug work depends on this landing first.

---

## Summary of files touched

- **Created**: `crates/fulgur-wpt/build.rs`, `crates/fulgur-wpt/tests/wpt_lists.rs`, `crates/fulgur-wpt/expectations/lists/smoke.txt`, `docs/plans/2026-04-24-fulgur-wpt-run-list.md`
- **Modified**: `crates/fulgur-wpt/src/runner.rs`, `crates/fulgur-wpt/src/expectations.rs`, `crates/fulgur-wpt/README.md`
- **Untouched**: `crates/fulgur-wpt/src/harness.rs`, `reftest.rs`, `report.rs`, `tests/wpt_css_page.rs`, `tests/wpt_css_multicol.rs`, `expectations/css-page.txt`, `expectations/css-multicol.txt`, `scripts/wpt/*`, `.github/workflows/*`

## Acceptance checklist (from fulgur-6nf0)

- [x] `expectations/lists/smoke.txt` added referencing a known-PASS WPT test
- [x] `cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_smoke` runs (PASS or silent skip)
- [x] Touching `expectations/lists/foo.txt` and rebuilding generates `wpt_list_foo`
- [x] `target/wpt-report/smoke/report.json` etc. produced when WPT is present
- [x] Existing `wpt_css_page` / `wpt_css_multicol` unchanged (no refactor regressions)
