# FULGUR-19 Engine Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the `fulgur` library and the `fulgur render` CLI render with Raikiri behind an off-by-default `raikiri` Cargo feature, with Blitz the default.

**Architecture:** `fulgur-blitz` gains three read-only accessors on `Engine`. The `fulgur` facade gains an optional `fulgur-raikiri` dependency, a `Backend` enum and an extension trait that renders a built `Engine` with a chosen backend. The CLI adds `--engine` and calls that trait. CI coverage for the feature goes into `raikiri.yml`.

**Tech Stack:** Rust 2024, clap 4 (derive, ValueEnum), Cargo features, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-02-fulgur-19-engine-selection-design.md`

## Global Constraints

- The `raikiri` feature is never in `default` (`fulgur`, `fulgur-cli`). With it off, `cargo tree -p fulgur-cli` lists no `raikiri-*` or `fulgur-raikiri` crate.
- No existing public signature changes. `fulgur-blitz` changes are additive read-only accessors plus one private field.
- `fulgur_core::Error` is not `#[non_exhaustive]`: add no variant, use `Error::Other(String)` for new errors.
- `fulgur-blitz` must not depend on `fulgur-raikiri`; `fulgur-core` stays backend-independent.
- In-group dependency declarations carry both `version` and `path`: `fulgur-raikiri = { version = "0.41.0", path = "../fulgur-raikiri", optional = true }`.
- Without `--engine`, or with `--engine blitz`, the CLI behaves exactly as before.
- Raikiri never ignores a request it cannot honour silently: templates, and custom serialize settings, are refused with an error.
- Not in scope: distribution bundling (FULGUR-20), template/batch/layout on Raikiri (FULGUR-22), bindings (FULGUR-23), user docs (FULGUR-21), `fulgur-dev`-only options (`--stream`, `--svg-rendering`, `--svg-raster-scale`).
- Error copy (exact):
  - facade template: `the Raikiri backend does not render templates yet; use Backend::Blitz`
  - facade serialize: `the Raikiri backend does not accept custom serialize settings; use Backend::Blitz`
  - CLI feature off: `--engine raikiri is not available in this build; install with \`cargo install fulgur-cli --features raikiri\``
  - CLI data: `--data (template rendering) requires --engine blitz for now`

## Review Focus

1. `--stdin` with `--engine raikiri`: the base path is the current directory, so a relative `<img src>` next to the cwd still resolves. Test in Task 3.
2. `-o -` with `--engine raikiri`: stdout carries only the PDF bytes (starts with `%PDF-`), as for Blitz. Test in Task 3.
3. A build with the feature **on** but `--engine` omitted must still use Blitz, not Raikiri. Test in Task 3 (feature-on build, default engine, output equals `--engine blitz`).
4. `--engine raikiri` together with metadata flags: `--title` reaches the PDF `/Title`. Test in Task 3.
5. A library user who builds `Engine` with `.template(...)` and calls `render_html_with(Backend::Raikiri, ...)` gets the error, never a PDF of the raw template. Test in Task 2.

---

### Task 1: Read-only accessors on `Engine` (fulgur-blitz)

**Files:**
- Modify: `crates/fulgur-blitz/src/engine.rs` (`Engine` struct ~L12, accessors ~L88-L100, `EngineBuilder` ~L1330, `serialize_settings()` ~L1441, `build()` ~L1446)
- Test: `crates/fulgur-blitz/src/engine.rs` existing `#[cfg(test)] mod tests`

**Interfaces:**
- Produces on `Engine`:
  - `pub fn system_fonts(&self) -> bool`
  - `pub fn has_template(&self) -> bool`: true iff a template was set via `EngineBuilder::template`
  - `pub fn has_custom_serialize_settings(&self) -> bool`: true iff `EngineBuilder::serialize_settings` was called
- Adds a private `custom_serialize_settings: bool` to `Engine` and `EngineBuilder`, default `false`, set to `true` in `EngineBuilder::serialize_settings`, copied in `build()`.

- [ ] **Step 1: Write failing tests** in the existing tests module:

```rust
#[test]
fn accessors_report_builder_defaults() {
    let engine = Engine::builder().build();
    assert!(engine.system_fonts());
    assert!(!engine.has_template());
    assert!(!engine.has_custom_serialize_settings());
}

#[test]
fn accessors_report_builder_settings() {
    let engine = Engine::builder()
        .system_fonts(false)
        .template("t.html", "<p>{{ x }}</p>")
        .serialize_settings(SerializeSettings::default())
        .build();
    assert!(!engine.system_fonts());
    assert!(engine.has_template());
    assert!(engine.has_custom_serialize_settings());
}
```

Check the builder's actual default for `system_fonts` before asserting `true`. If the default is different, assert that default instead.

- [ ] **Step 2: Run** `cargo test -p fulgur-blitz --lib accessors_report`. Expected: FAIL, compile error (no method `system_fonts`).
- [ ] **Step 3: Implement** the three accessors and the private flag as described in Interfaces. Add a one-line rustdoc to each accessor.
- [ ] **Step 4: Run** `cargo test -p fulgur-blitz --lib accessors_report`. Expected: PASS. Then run `cargo test -p fulgur-blitz --lib`. Expected: all pass.
- [ ] **Step 5: Commit** `feat(blitz): expose system_fonts, has_template and custom serialize settings on Engine`

### Task 2: `Backend` and `render_html_with` in the facade

**Files:**
- Modify: `crates/fulgur/Cargo.toml` (optional dep + `[features] raikiri = ["dep:fulgur-raikiri"]`)
- Modify: `crates/fulgur/src/lib.rs` (keep `pub use fulgur_blitz::*;`, add `mod backend; pub use backend::{Backend, EngineExt};`)
- Create: `crates/fulgur/src/backend.rs`
- Test: `crates/fulgur/tests/backend_selection.rs`

**Interfaces:**
- Consumes: Task 1 accessors.
- Produces:
  - `#[non_exhaustive] #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)] pub enum Backend { #[default] Blitz, #[cfg(feature = "raikiri")] Raikiri }`
  - `pub trait EngineExt { fn render_html_with(&self, backend: Backend, html: &str) -> fulgur_core::Result<Vec<u8>>; }`
  - `impl EngineExt for Engine`. `Blitz` gives `self.render(html)`. `Raikiri` checks `has_template()` and then `has_custom_serialize_settings()`, returning `Err(Error::Other(<exact copy>))` in that order. Otherwise it calls `fulgur_raikiri::render_html_with_options(html, self.base_path(), self.config(), &fulgur_raikiri::RenderOptions { assets: self.assets(), system_fonts: self.system_fonts(), ..Default::default() })`.
- Use the `Error` / `Result` paths the facade already re-exports (`fulgur::Error`). The trait is sealed (private supertrait) so it can grow methods later without breaking implementors.

- [ ] **Step 1: Write failing tests** in `crates/fulgur/tests/backend_selection.rs`:

```rust
use fulgur::{Backend, Engine, EngineExt};

const HTML: &str = "<html><body><p>Hello</p></body></html>";

#[test]
fn default_backend_is_blitz() {
    assert_eq!(Backend::default(), Backend::Blitz);
}

#[test]
fn blitz_backend_matches_render() {
    let engine = Engine::builder().build();
    assert_eq!(
        engine.render_html_with(Backend::Blitz, HTML).unwrap(),
        engine.render(HTML).unwrap()
    );
}

#[cfg(feature = "raikiri")]
mod raikiri {
    use super::*;

    #[test]
    fn raikiri_renders_a_pdf_with_the_title() {
        let engine = Engine::builder().title("Backend test").build();
        let pdf = engine.render_html_with(Backend::Raikiri, HTML).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(String::from_utf8_lossy(&pdf).contains("Backend test"));
    }

    #[test]
    fn raikiri_refuses_a_template() {
        let engine = Engine::builder().template("t.html", "<p>{{ x }}</p>").build();
        let err = engine.render_html_with(Backend::Raikiri, HTML).unwrap_err();
        assert_eq!(err.to_string(), "the Raikiri backend does not render templates yet; use Backend::Blitz");
    }

    #[test]
    fn raikiri_refuses_custom_serialize_settings() {
        let engine = Engine::builder().serialize_settings(Default::default()).build();
        let err = engine.render_html_with(Backend::Raikiri, HTML).unwrap_err();
        assert_eq!(err.to_string(), "the Raikiri backend does not accept custom serialize settings; use Backend::Blitz");
    }
}
```

`blitz_backend_matches_render` relies on Blitz being deterministic (`crates/fulgur-cli/tests/examples_determinism.rs` holds that). If the title text is compressed or hex-encoded in the PDF, assert on `/Title` through `lopdf` (already a dev-dependency) instead of a substring.

- [ ] **Step 2: Run** `cargo test -p fulgur --test backend_selection` (feature off). Expected: FAIL, compile error (`Backend` not found).
- [ ] **Step 3: Implement** `crates/fulgur/src/backend.rs` and the Cargo/lib changes per Interfaces. Update the crate doc in `lib.rs` to say the facade can also render with Raikiri behind the `raikiri` feature.
- [ ] **Step 4: Run** `cargo test -p fulgur --test backend_selection`. Expected: 2 passed. Then run `cargo test -p fulgur --test backend_selection --features raikiri`. Expected: 5 passed. Then run `cargo tree -p fulgur -e normal | grep -c raikiri`. Expected: `0`.
- [ ] **Step 5: Commit** `feat(fulgur): select the layout backend with Backend and EngineExt::render_html_with`

### Task 3: `fulgur render --engine`

**Files:**
- Modify: `crates/fulgur-cli/Cargo.toml` (`[features] raikiri = ["fulgur/raikiri"]`)
- Modify: `crates/fulgur-cli/src/main.rs` (the `Render` variant ~L189-L281, render flow ~L520-L700)
- Test: `crates/fulgur-cli/tests/engine_cli.rs`

**Interfaces:**
- Consumes: `fulgur::{Backend, EngineExt}` from Task 2.
- Produces: a CLI flag `--engine <blitz|raikiri>` (clap `ValueEnum` `EngineChoice { Blitz, Raikiri }`, `default_value = "blitz"`). Help text: `Layout engine (raikiri requires a build with the raikiri feature)`.

Behaviour:
- Validate before reading input or rendering. With `--engine raikiri`, a build without the feature prints `Error: <CLI feature-off copy>` and exits 1, and `--data` prints `Error: <CLI data copy>` and exits 1.
- Otherwise build the `Engine` exactly as today. Map `EngineChoice` to `Backend` (`Raikiri` only under `#[cfg(feature = "raikiri")]`). Keep `engine.render_template()` for `--data` (Blitz only), and call `engine.render_html_with(backend, &input_content)` instead of `engine.render(&input_content)`. The stdout isolator and the output handling stay as they are.

- [ ] **Step 1: Write failing tests** in `crates/fulgur-cli/tests/engine_cli.rs`. Use the `run_cli` helper pattern from `bookmarks_cli.rs` and `tempfile::TempDir`.
  - `engine_blitz_is_the_default`: render the same HTML with no `--engine` and with `--engine blitz`; both succeed and the PDF bytes are equal. Runs in both builds, which covers Review Focus 3.
  - `#[cfg(not(feature = "raikiri"))] engine_raikiri_without_feature_fails`: exit is not success, and stderr contains `--engine raikiri is not available in this build`.
  - `#[cfg(feature = "raikiri")]` tests:
    - `engine_raikiri_renders`: success, the output starts with `%PDF-`, and the `--title "Raikiri CLI"` text is in the PDF (Review Focus 4).
    - `engine_raikiri_with_data_fails`: also pass `--data data.json`; exit is not success, stderr contains `--data (template rendering) requires --engine blitz for now`, and no output file is created.
    - `engine_raikiri_to_stdout`: `-o -`; stdout starts with `%PDF-` (Review Focus 2).
    - `engine_raikiri_stdin_resolves_relative_image`: the HTML from stdin has `<img src="dot.png">`, with `dot.png`, a tiny PNG written by the test, in the cwd. Run with `.current_dir(dir)` and `--stdin`; succeed, and the PDF contains an `/Image` XObject (Review Focus 1).
- [ ] **Step 2: Run** `cargo test -p fulgur-cli --test engine_cli`. Expected: FAIL, because clap rejects the unknown `--engine`.
- [ ] **Step 3: Implement** the flag, the feature passthrough, the early validation and the backend dispatch, as described above.
- [ ] **Step 4: Run** `cargo test -p fulgur-cli --test engine_cli`. Expected: 2 passed. Then run `cargo test -p fulgur-cli --test engine_cli --features raikiri`. Expected: 5 passed. Then run `cargo test -p fulgur-cli`. Expected: all existing CLI tests pass.
- [ ] **Step 5: Manual check** with the blitz#386 fixture: `cargo run -p fulgur-cli --features raikiri -- render border-collapse.html --engine raikiri -o out.pdf` draws collapsed borders, the same as `fulgur-dev render --engine raikiri`. Get the HTML from DioxusLabs/blitz#386, and keep it out of the repo.
- [ ] **Step 6: Commit** `feat(cli): add fulgur render --engine blitz|raikiri`

### Task 4: Exercise the feature in `raikiri.yml`

**Files:**
- Modify: `.github/workflows/raikiri.yml` (`paths:` filters under `push` and `pull_request`, plus the Clippy step and the "Run tests with coverage" step)

**Interfaces:**
- Consumes: features `fulgur/raikiri` and `fulgur-cli/raikiri` from Tasks 2 and 3.

- [ ] **Step 1: Add paths** to both `paths:` lists:
  - `crates/fulgur/Cargo.toml`
  - `crates/fulgur/src/backend.rs`
  - `crates/fulgur/tests/backend_selection.rs`
  - `crates/fulgur-cli/**`
  - `crates/fulgur-blitz/src/engine.rs`
- [ ] **Step 2: Clippy step.** Append `-p fulgur -p fulgur-cli` to the package list and `fulgur/raikiri,fulgur-cli/raikiri` to `--features`.
- [ ] **Step 3: Test step.** Add one line before `cargo llvm-cov report`:
  `cargo llvm-cov nextest -p fulgur -p fulgur-cli --features fulgur/raikiri,fulgur-cli/raikiri --test backend_selection --test engine_cli --profile ci --no-report`
- [ ] **Step 4: Run** the same commands locally:
  - `cargo clippy -p fulgur-raikiri -p fulgur-dev -p fulgur-wpt -p fulgur-vrt -p fulgur -p fulgur-cli --features fulgur-wpt/raikiri,fulgur-vrt/raikiri,fulgur/raikiri,fulgur-cli/raikiri --all-targets -- -D warnings` on stable
  - `cargo test -p fulgur -p fulgur-cli --features fulgur/raikiri,fulgur-cli/raikiri --test backend_selection --test engine_cli`

  Expected: clean, then 10 passed. Also run `cargo clippy --workspace --all-targets -- -D warnings` with the feature off. Expected: clean.
- [ ] **Step 5: Commit** `ci(raikiri): build and test the raikiri feature of fulgur and fulgur-cli`
