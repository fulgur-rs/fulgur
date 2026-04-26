# Render Smoke Tests Extraction Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Move 10 E2E render smoke tests out of `crates/fulgur/src/engine.rs::tests` into a dedicated integration test file `crates/fulgur/tests/render_smoke.rs`, and update CLAUDE.md guidance accordingly (beads `fulgur-181g`).

**Architecture:** Cargo's standard integration test layout (`<crate>/tests/*.rs`). Use the public API (`fulgur::{Engine, AssetBundle}`) so the moved tests are not coupled to `engine.rs` private items. The 7 builder/template/config unit tests remain in `engine.rs::tests`.

**Tech Stack:** Rust, cargo, integration tests, `tempfile` dev-dependency.

---

## Pre-flight notes

- `super::*` in `engine.rs::tests` must stay after deletion — the 7 remaining builder/template tests still need `Engine` in scope.
- Japanese `///` doc comments above the 4 gradient tests (engine.rs lines 626-629, 643-644, 658-661, 675-677) **must travel with their tests** — they record *why* the smoke tests exist for coverage.
- The PNG byte literal in `test_render_html_marker_content_url_with_image` is 64 bytes — copy it intact.
- `tempfile::tempdir()` callers: 4 link-stylesheet tests use it; mirror the existing integration test idiom (`use tempfile::tempdir;`).
- Skip `cargo llvm-cov` locally — integration tests are in scope of the workspace coverage command, so coverage is preserved by construction. CI will verify.

---

## Task 1: Create `tests/render_smoke.rs` with the 10 E2E tests

**Files:**

- Create: `crates/fulgur/tests/render_smoke.rs`

**Step 1: Write the new integration test file**

Copy these 10 tests from `crates/fulgur/src/engine.rs` (lines 484-689 in current state) into the new file. Replace `super::*` with `use fulgur::{Engine, AssetBundle};` and `use tempfile::tempdir;` (where needed). Preserve all `///` doc comments verbatim, including Japanese.

Tests to move (in order):

1. `test_render_html_resolves_link_stylesheet`
2. `test_render_html_link_stylesheet_with_gcpm`
3. `test_render_html_link_stylesheet_with_import`
4. `test_render_html_link_stylesheet_rejects_path_traversal`
5. `test_render_html_marker_content_url_does_not_panic`
6. `test_render_html_marker_content_url_with_image`
7. `test_render_repeating_linear_gradient_smoke`
8. `test_render_repeating_radial_gradient_smoke`
9. `test_render_linear_gradient_corner_direction_smoke`
10. `test_render_linear_gradient_tiled_smoke`

Add a top-of-file `//!` module doc comment summarizing why this file exists (E2E smoke tests excluded from VRT but required for codecov patch coverage, mirroring the existing `gradient_test.rs` style).

Use `tempfile::tempdir` directly (not `tempfile::tempdir()` namespaced) to match the convention in `link_stylesheet_url_resolution.rs`.

**Step 2: Run new integration test, expect PASS**

Run: `cargo test -p fulgur --test render_smoke`

Expected: `10 passed; 0 failed`. If any test fails (e.g. relied on a private item via `super::*`), surface it before proceeding.

**Step 3: Confirm full lib still passes (duplicate tests are fine)**

Run: `cargo test -p fulgur --lib`

Expected: All `#[cfg(test)] mod tests` in engine.rs still pass — duplicates between integration test and unit module are allowed at this checkpoint; we delete the engine.rs copies in Task 2.

---

## Task 2: Delete the moved tests from `engine.rs`

**Files:**

- Modify: `crates/fulgur/src/engine.rs` (lines 484-689 — the 10 E2E test fns and their doc comments)

**Step 1: Remove the 10 functions from `engine.rs::tests`**

Keep:

- `use super::*;`
- `builder_bookmarks_defaults_to_false`
- `builder_bookmarks_opt_in`
- `test_engine_builder_base_path`
- `test_engine_builder_no_base_path`
- `test_engine_render_template`
- `test_engine_render_without_template_errors`
- `test_engine_render_without_data_uses_empty_object`

Delete:

- All 10 tests listed in Task 1 (functions + their `///` doc comments).

**Step 2: Run lib tests, expect PASS**

Run: `cargo test -p fulgur --lib`

Expected: `7 passed` from `engine::tests::` (no E2E tests left). Total lib count drops by 10. No compile errors.

**Step 3: Run full fulgur tests**

Run: `cargo test -p fulgur`

Expected: All tests pass, including `render_smoke` (10 tests) and existing integration tests.

---

## Task 3: Update CLAUDE.md guidance

**Files:**

- Modify: `CLAUDE.md` (line ~141)

**Step 1: Replace the `engine.rs::tests` reference with `tests/render_smoke.rs`**

Find:

```text
レンダリング経路 (`draw_background_layer` の match arm 等、`Engine::render_html` を通って初めて
    叩かれる箇所) → `crates/fulgur/src/engine.rs` の `tests` モジュールに end-to-end smoke test
    (`Engine::builder().build().render_html(html)` で `assert!(!pdf.is_empty())`)
```

Replace with:

```text
レンダリング経路 (`draw_background_layer` の match arm 等、`Engine::render_html` を通って初めて
    叩かれる箇所) → `crates/fulgur/tests/render_smoke.rs` に end-to-end smoke test
    (`Engine::builder().build().render_html(html)` で `assert!(!pdf.is_empty())`)
```

**Step 2: Verify markdownlint passes**

Run: `npx markdownlint-cli2 '**/*.md'`

Expected: No new violations reported on CLAUDE.md or the new plan file.

---

## Task 4: Lint, fmt, commit

**Step 1: `cargo fmt --check`**

Run: `cargo fmt --check`

Expected: clean. If not, run `cargo fmt` and re-check.

**Step 2: `cargo clippy --all-targets`**

Run: `cargo clippy --all-targets -- -D warnings`

Expected: clean.

**Step 3: Commit**

```bash
git add crates/fulgur/tests/render_smoke.rs crates/fulgur/src/engine.rs CLAUDE.md docs/plans/2026-04-27-render-smoke-tests-extraction.md
git commit -m "test(fulgur): move E2E render smoke tests to tests/render_smoke.rs

Extract 10 end-to-end smoke tests from engine.rs::tests into a dedicated
integration test file. engine.rs::tests now holds only Engine builder /
template unit tests. Update CLAUDE.md Gotchas to point at the new home so
future draw paths land in the right file by default.

Closes fulgur-181g"
```

---

## Verification summary

- `cargo test -p fulgur --lib` — 7 builder/template unit tests in engine.rs (down from 17), total ~772 lib tests
- `cargo test -p fulgur --test render_smoke` — 10 E2E smoke tests
- `cargo test -p fulgur` — full suite green
- `cargo fmt --check` — clean
- `cargo clippy --all-targets -- -D warnings` — clean
- `npx markdownlint-cli2 '**/*.md'` — clean

CI (codecov patch coverage) will confirm coverage is preserved.
