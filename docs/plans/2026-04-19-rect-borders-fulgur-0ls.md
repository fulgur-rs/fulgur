# Rect Borders (fulgur-0ls) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Consolidate 4-line per-cell border strokes into a single stroked rectangle when borders are uniform and there is no `border-radius`. Target: `m + l` 1,492 → ~680 on `examples/table-header/` (54% reduction). krilla 0.7 does not emit the PDF `re` operator from `PathBuilder::push_rect`; the win comes from fewer strokes per cell, not from rect-op emission. (A krilla upstream PR to expose `Content::rect` would push this further — tracked separately.)

**Architecture:** Add a new branch in `pageable.rs::draw_block_border` that triggers on `uniform_width && uniform_style && !has_radius() && style == Solid`. Use krilla 0.7's `PathBuilder::push_rect` to emit a single closed-rectangle subpath, stroked once. **Only `Solid` collapses to a single rect stroke.** `Dashed`/`Dotted` stay on the 4-line fallback to preserve per-edge dash phase (browsers stroke each edge separately, so phase restarts at each corner — collapsing to one closed path would run dash phase continuously around the perimeter and break corner symmetry; see Task 4 course correction). `Double` collapses to two concentric rect strokes (no dash phase involved). 3D styles (Groove/Ridge/Inset/Outset) keep the 4-line path because their colors vary per side.

**Tech Stack:** Rust, krilla 0.7 (`geom::PathBuilder::push_rect`, `geom::Rect::from_xywh`), qpdf (content-stream introspection in tests), cargo.

**Baseline (2026-04-19, `examples/table-header/` → `/tmp/table-header-before.pdf`, 37,640 bytes):**

| op  | count | meaning                        |
|-----|------:|--------------------------------|
| m   |   822 | moveto                         |
| l   |   670 | lineto                         |
| re  |     0 | rectangle                      |
| S   |   783 | stroke                         |
| q   |   826 | save graphics state            |
| BT  |   170 | begin text                     |
| RG  |   646 | set stroke color RGB           |

---

## Task 0: Add a content-stream introspection helper

**Goal:** Let integration tests assert operator counts from a rendered PDF. This is the only reliable way to verify our change end-to-end, because krilla's `Path` is opaque to callers.

**Files:**

- Create: `crates/fulgur/tests/support/content_stream.rs`
- Modify: `crates/fulgur/tests/support/mod.rs` (create if missing) to re-export it
- Reference: `crates/fulgur/tests/background_test.rs` for how existing integration tests are structured

**Step 1: Write the helper**

```rust
// crates/fulgur/tests/support/content_stream.rs
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
/// PDF operators. Returns `None` only when qpdf is not installed (tests
/// should skip — CI always has it, local devs may not). Any other
/// failure panics so that bugs don't silently appear as skipped tests.
pub fn count_ops(pdf_bytes: &[u8]) -> Option<OpCounts> {
    // Probe: qpdf binary present? If not, return None (skip). If present,
    // any subsequent failure is a real bug and should panic rather than
    // silently skip, so tests don't pretend to pass.
    let probe = Command::new("qpdf").arg("--version").status();
    if probe.map(|s| !s.success()).unwrap_or(true) {
        return None;
    }

    let tmp = tempfile::NamedTempFile::new().expect("create tempfile");
    let out = tempfile::NamedTempFile::new().expect("create tempfile");
    std::fs::write(tmp.path(), pdf_bytes).expect("write tmp pdf");

    let status = Command::new("qpdf")
        .args(["--qdf", "--object-streams=disable"])
        .arg(tmp.path())
        .arg(out.path())
        .status()
        .expect("spawn qpdf");
    assert!(status.success(), "qpdf --qdf failed: {:?}", status);

    // `qpdf --qdf` does NOT strip binary streams (embedded fonts, inline
    // images, etc.), so the output is not valid UTF-8. Scan bytes
    // directly — PDF operators we care about are ASCII-only and sit at
    // the end of a line, so suffix matching on byte slices works.
    let qdf = std::fs::read(out.path()).expect("read qdf output");
    let mut c = OpCounts::default();
    for raw in qdf.split(|&b| b == b'\n') {
        // Strip trailing \r on CRLF lines.
        let line: &[u8] = if raw.last() == Some(&b'\r') {
            &raw[..raw.len() - 1]
        } else {
            raw
        };
        if line.ends_with(b" m") || line == b"m" {
            c.m += 1;
        } else if line.ends_with(b" l") || line == b"l" {
            c.l += 1;
        } else if line.ends_with(b" re") {
            c.re += 1;
        } else if line == b"S" || line.ends_with(b" S") {
            c.s_stroke += 1;
        } else if line == b"q" {
            c.q += 1;
        } else if line == b"BT" {
            c.bt += 1;
        } else if line.ends_with(b" RG") {
            c.rg_stroke += 1;
        }
    }
    Some(c)
}
```

**Step 2: Wire it into the test support module**

```rust
// crates/fulgur/tests/support/mod.rs
pub mod content_stream;
```

If `mod.rs` doesn't exist, create the file. If it already has other helpers, append the `pub mod content_stream;` line.

**Step 3: Add `tempfile` to `[dev-dependencies]` if missing**

Inspect `crates/fulgur/Cargo.toml`. If `tempfile` is not already under `[dev-dependencies]`, add:

```toml
[dev-dependencies]
tempfile = "3"
```

**Step 4: Verify it compiles**

Run: `cargo test -p fulgur --lib --tests --no-run`
Expected: clean build, no errors.

**Step 5: Commit**

```bash
git add crates/fulgur/tests/support/content_stream.rs \
        crates/fulgur/tests/support/mod.rs \
        crates/fulgur/Cargo.toml
git commit -m "test(fulgur): add content-stream operator count helper for fulgur-0ls"
```

---

## Task 1: Pin current behavior with a failing regression test

**Goal:** Lock in the intended outcome before writing production code. The test will render `examples/table-header/index.html`, count operators, and assert `re > 0` (we expect rect strokes) and `m < 300` (we expect a large drop). It should fail on current code.

**Files:**

- Create: `crates/fulgur/tests/rect_borders_test.rs`

**Step 1: Write the failing test**

```rust
// crates/fulgur/tests/rect_borders_test.rs
mod support;
use support::content_stream::count_ops;

use fulgur::asset::AssetBundle;
use fulgur::config::PageSize;
use fulgur::engine::Engine;
use std::path::PathBuf;

fn render_example(name: &str) -> Vec<u8> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(name);
    let html = std::fs::read_to_string(root.join("index.html")).unwrap();

    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .base_path(root)
        .build();

    engine
        .render_html(&html)
        .expect("render_html should succeed")
}

#[test]
fn table_header_uses_rect_for_uniform_borders() {
    let pdf = render_example("table-header");
    let Some(counts) = count_ops(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };

    // Task 3 collapses 4 abutting strokes per cell into one rect path.
    // krilla 0.7 does not emit the PDF `re` operator — `PathBuilder::push_rect`
    // decomposes to `m + 3l + h`. We measure the real win via combined
    // line-segment count rather than rect count.
    // Baseline (pre-Task-3): m=822, l=670 (total 1492).
    // After Task 3: m≈170, l≈510 (total ≈680).
    assert!(
        counts.m < 300,
        "expected m < 300 (moveto collapsed into single rect paths), got m={} l={}",
        counts.m, counts.l,
    );
    assert!(
        counts.m + counts.l < 900,
        "expected m+l < 900 (rect paths share a single subpath), got m={} l={}",
        counts.m, counts.l,
    );
}
```

Adjust the `Engine::builder()` call if the real API surface differs — verify against `crates/fulgur/src/engine.rs` before writing. (CLAUDE.md confirms the builder signature: `Engine::builder().page_size(...).base_path(...).build()` + `render_html(html)`.)

**Step 2: Run the test — expect FAIL**

Run: `cargo test -p fulgur --test rect_borders_test -- --nocapture`
Expected: FAIL with `expected m < 300, got m=822 l=670` on unmodified code. Task 3 makes this pass.

If qpdf is missing locally the test prints a skip message and passes — that's fine; CI has qpdf.

**Step 3: Commit**

```bash
git add crates/fulgur/tests/rect_borders_test.rs
git commit -m "test(fulgur): add failing regression test for rect-based borders (fulgur-0ls)"
```

---

## Task 2: Add `build_rect_path` + `stroke_rect` helpers

**Goal:** Introduce the two helpers the new branch will use. No behavior change yet.

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (insert near `stroke_line` around line 1237-1253)

**Step 1: Add helpers immediately after `stroke_line`**

```rust
/// Build a krilla `Path` for the axis-aligned rectangle at (x,y) with
/// size (w,h). Returns `None` if `w <= 0` or `h <= 0` (krilla rejects
/// degenerate rects).
fn build_rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<krilla::geom::Path> {
    let rect = krilla::geom::Rect::from_xywh(x, y, w, h)?;
    let mut pb = krilla::geom::PathBuilder::new();
    pb.push_rect(rect);
    pb.finish()
}

/// Helper to stroke an axis-aligned rectangle with a given stroke.
/// Emits a single `re + S` in the PDF content stream (versus 4 × `m/l/S`
/// from abutting `stroke_line` calls).
fn stroke_rect(
    canvas: &mut Canvas<'_, '_>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    stroke: krilla::paint::Stroke,
) {
    if let Some(path) = build_rect_path(x, y, w, h) {
        canvas.surface.set_stroke(Some(stroke));
        canvas.surface.draw_path(&path);
    }
}
```

**Step 2: Verify it compiles**

Run: `cargo build -p fulgur`
Expected: clean.

**Step 3: Run all existing tests — must still pass (no wiring yet)**

Run: `cargo test -p fulgur --lib`
Expected: 445 passed, 0 failed.

**Step 4: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "refactor(fulgur): add build_rect_path and stroke_rect helpers (fulgur-0ls)"
```

---

## Task 3: Integrate new branch into `draw_block_border` for Solid

**Goal:** Add the new uniform/no-radius branch, handle `Solid` only. Make the Task 1 regression test pass.

**Files:**

- Modify: `crates/fulgur/src/pageable.rs::draw_block_border` (currently at lines 1357-1464)

**Step 1: Add the new branch**

Current shape:

```rust
if style.has_radius() && uniform_width && uniform_style && st != BorderStyleValue::None {
    // single rounded-rect stroke — existing
} else {
    // 4 × draw_border_line — existing
}
```

New shape:

```rust
if style.has_radius() && uniform_width && uniform_style && st != BorderStyleValue::None {
    // single rounded-rect stroke — unchanged
} else if !style.has_radius()
    && uniform_width
    && uniform_style
    && matches!(st, BorderStyleValue::Solid)
{
    // Single rect stroke (re + S). inset by width/2 so the stroked
    // rectangle's outer edge sits at (x, y, w, h).
    let opacity = krilla::num::NormalizedF32::new(bc[3] as f32 / 255.0)
        .unwrap_or(krilla::num::NormalizedF32::ONE);
    let inset = bt / 2.0;
    let base = colored_stroke(bc, bt, opacity);
    if let Some(styled) = apply_border_style(base, st, bt) {
        canvas.surface.set_fill(None);
        stroke_rect(
            canvas,
            x + inset,
            y + inset,
            (w - inset * 2.0).max(0.0),
            (h - inset * 2.0).max(0.0),
            styled,
        );
        canvas.surface.set_stroke(None);
    }
} else {
    // 4 × draw_border_line — unchanged
}
```

Only `Solid` is handled in this branch. `Dashed`/`Dotted` remain on the 4-line fallback (see Task 4 for the rationale — browser-compatible per-edge dash phase); `Double` is handled by its own sub-branch added in Task 5.

**Step 2: Run the regression test — expect PASS**

Run: `cargo test -p fulgur --test rect_borders_test -- --nocapture`
Expected: PASS — `m < 300` and `m + l < 900` once table-header cells collapse
(measured: `m=170`, `l=510`, `m+l=680`, 54% drop from 1,492). krilla 0.7
emits rect paths as `m + 3l + h` rather than the PDF `re` operator, so we do
not assert on `re`; the win is still real.

**Step 3: Run the full unit-test suite**

Run: `cargo test -p fulgur --lib`
Expected: 445 passed, 0 failed.

**Step 4: Run integration tests**

Run: `cargo test -p fulgur --tests`
Expected: all pass. If `border_radius_test` or `background_test` fail, investigate — we should not have touched those code paths.

**Step 5: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "perf(fulgur): stroke uniform borders as a single rect (fulgur-0ls)

Collapse 4-line border strokes into one re+S for cells with
uniform width, uniform style (Solid), and no border-radius."
```

---

## Task 4: Course correction — keep Dashed/Dotted on 4-line fallback

**History:**

- 2026-04-19: commit `6cfc2f8` broadened the Task 3 `matches!` guard to
  `Solid | Dashed | Dotted`, betting that collapsing dashed/dotted
  uniform borders into a single closed rect path would reduce `m + l`
  further. A conformance test (`dashed_uniform_border_uses_rect`) pinned
  the collapsed behavior (`m == 1`, `l == 3`).
- VRT re-run revealed a CSS conformance regression on
  `crates/fulgur-vrt/goldens/basic/borders.html`: browsers stroke each
  edge independently, so dash phase restarts at every corner. Folding
  the perimeter into one closed rect path (`m + 3l + h + S`) lets dash
  phase run continuously around the box, shifting the phase of the
  right, bottom, and left edges relative to the top. Corner symmetry
  the golden encoded was broken.
- User decision (Option B): revert to Solid-only. Dashed/Dotted stay on
  the 4-line fallback. Fulgur's positioning is "CSS = WeasyPrint超え
  目標 (Prince肉薄)", and per-edge dash phase is a visible, user-facing
  conformance property — perf wins cannot compromise it.

**Action taken:**

1. Narrowed the guard in `crates/fulgur/src/pageable.rs::draw_block_border`
   back to `matches!(st, BorderStyleValue::Solid)`.
2. Replaced `dashed_uniform_border_uses_rect` in
   `crates/fulgur/tests/rect_borders_test.rs` with
   `dashed_uniform_border_keeps_per_edge_phase`, which now asserts
   `m >= 4 && l >= 4`. This pins the conformance property so a future
   drive-by "perf fix" cannot silently regress it again.
3. Re-ran VRT; `basic/borders.html` matches its pre-Task-4 golden.

**Deferred work (not scheduled):**

A future task could batch the 4 edges into one `PathBuilder` with
explicit per-edge `move_to` + `line_to` (no `close_path` /
auto-connect), so each edge keeps its own dash origin while collapsing
to a single `draw_path` call. That would save `set_stroke`/draw-path
overhead without changing phase. Tracked informally — not part of this
plan.

**Files touched:**

- `crates/fulgur/src/pageable.rs` (guard narrowed)
- `crates/fulgur/tests/rect_borders_test.rs` (test renamed + flipped)
- `docs/plans/2026-04-19-rect-borders-fulgur-0ls.md` (this section)

**Commit:**

```bash
git add crates/fulgur/src/pageable.rs crates/fulgur/tests/rect_borders_test.rs docs/plans/2026-04-19-rect-borders-fulgur-0ls.md
git commit -m "revert(fulgur): keep dashed/dotted on per-edge path for CSS conformance (fulgur-0ls)"
```

---

## Task 5: Handle Double style with 2 concentric rects

**Goal:** `border-style: double` should emit 2 stroked rectangles (outer + inner ring) instead of 8 line strokes. Each rect is one `m + 3l + h` subpath (krilla 0.7 does not emit the PDF `re` operator — see Task 3 note), so assert `counts.m == 2` and `counts.l == 6` rather than `counts.re == 2`.

**Note:** Double is static concentric rings with no dash phase, so the Task 4 per-edge-phase concern does not apply. Rect-collapse is safe here.

**Files:**

- Modify: `crates/fulgur/src/pageable.rs::draw_block_border`

**Step 1: Write a failing test**

```rust
#[test]
fn double_uniform_border_uses_two_rects() {
    use fulgur::config::PageSize;
    use fulgur::engine::Engine;

    let html = r#"
        <html><head><style>
            .b { width: 200px; height: 100px; border: 9px double #444; }
        </style></head><body><div class="b"></div></body></html>
    "#;

    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).unwrap();

    let Some(counts) = count_ops(&pdf) else { return; };
    assert_eq!(counts.re, 2, "expected re == 2 (outer + inner), got {}", counts.re);
    assert_eq!(counts.l, 0, "expected l == 0, got {}", counts.l);
}
```

Run: FAIL on current code.

**Step 2: Extend the uniform branch**

Add a sub-match on `Double`:

```rust
} else if !style.has_radius()
    && uniform_width
    && uniform_style
    && matches!(
        st,
        BorderStyleValue::Solid | BorderStyleValue::Double
    )
{
    let opacity = krilla::num::NormalizedF32::new(bc[3] as f32 / 255.0)
        .unwrap_or(krilla::num::NormalizedF32::ONE);
    canvas.surface.set_fill(None);
    if st == BorderStyleValue::Double {
        // Two concentric strokes, each (bt/3) wide.
        // Outer ring: inset by (bt/3)/2 = bt/6.
        // Inner ring: inset by bt - (bt/3)/2 = bt * 5/6.
        let thin_w = bt / 3.0;
        let outer_inset = thin_w / 2.0;
        let inner_inset = bt - thin_w / 2.0;
        let stroke_thin = colored_stroke(bc, thin_w, opacity);
        stroke_rect(
            canvas,
            x + outer_inset,
            y + outer_inset,
            (w - outer_inset * 2.0).max(0.0),
            (h - outer_inset * 2.0).max(0.0),
            stroke_thin.clone(),
        );
        stroke_rect(
            canvas,
            x + inner_inset,
            y + inner_inset,
            (w - inner_inset * 2.0).max(0.0),
            (h - inner_inset * 2.0).max(0.0),
            stroke_thin,
        );
    } else {
        let inset = bt / 2.0;
        let base = colored_stroke(bc, bt, opacity);
        if let Some(styled) = apply_border_style(base, st, bt) {
            stroke_rect(
                canvas,
                x + inset,
                y + inset,
                (w - inset * 2.0).max(0.0),
                (h - inset * 2.0).max(0.0),
                styled,
            );
        }
    }
    canvas.surface.set_stroke(None);
}
```

**Step 3: Run the new test — expect PASS**

Run: `cargo test -p fulgur --test rect_borders_test double_uniform_border_uses_two_rects -- --nocapture`
Expected: PASS.

**Step 4: Re-run the full suite**

Run: `cargo test -p fulgur`
Expected: all pass.

**Step 5: Commit**

```bash
git add crates/fulgur/src/pageable.rs crates/fulgur/tests/rect_borders_test.rs
git commit -m "perf(fulgur): consolidate double borders into 2 rect strokes (fulgur-0ls)"
```

---

## Task 6: VRT full run + update goldens if intentionally changed

**Goal:** Confirm no visual regression across the full VRT suite. After the Task 4 revert, dashed/dotted borders take the 4-line fallback, so per-edge dash phase is preserved and no golden change is expected for those fixtures. Only Solid (and Double from Task 5) change code path.

**Files:**

- Update (if needed): `crates/fulgur-vrt/goldens/*.png`

**Step 1: Run the VRT suite**

Run: `cargo test -p fulgur-vrt`
Expected: 0 failed. If any fixture fails, inspect the diff image.

**Step 2: If visual diffs appear**

Likely suspects (Solid / Double only — dashed/dotted are no longer in the rect branch):

- `border-*` fixtures with solid borders: check stroke join at corners (miter vs. abutting line-cap).
- `basic/borders.html`: the fixture that caught the Task 4 regression; after the revert it should match its pre-Task-4 golden with no diff.

For each affected fixture:

1. Visually inspect `crates/fulgur-vrt/diff_out/*.png` (the rendered diff)
1. If the new rendering is correct (e.g. cleaner solid corners), regenerate the golden:

```bash
UPDATE_GOLDENS=1 cargo test -p fulgur-vrt <fixture_name>
```

1. If the new rendering is *wrong* (e.g. miter spike at corner when it should be square, or any dashed/dotted phase shift), revert: narrow the `matches!` guard to exclude the problematic style for now, and file a follow-up issue. (Task 4 is the template: conformance first, perf second.)

**Step 3: Commit golden updates (only if changes were intentional)**

```bash
git add crates/fulgur-vrt/goldens/
git commit -m "test(fulgur-vrt): refresh goldens for rect-border stroke change (fulgur-0ls)"
```

---

## Task 7: Lock in the improvement with tight thresholds

**Goal:** Replace the loose Task 1 thresholds with counts that reflect the actual post-change state, so any regression is caught.

**Files:**

- Modify: `crates/fulgur/tests/rect_borders_test.rs::table_header_uses_rect_for_uniform_borders`

**Note:** krilla 0.7 does not emit the PDF `re` operator, so we measure
the improvement via `m` (moveto count), `l` (lineto count), combined
`m + l` (total line-segment ops), and raw PDF byte size. Do not assert
on `counts.re` — it will be 0 regardless of how many rect paths we
emit.

**Scope of the improvement:** numbers reported here reflect Solid
uniform borders only (Task 3) plus Double (Task 5). `examples/table-header/`
is all-Solid, so the table-header `m`/`l` drop captures the full win
available from this plan. Dashed/Dotted borders remain on the 4-line
fallback (Task 4) and must not be claimed as part of the metric
improvement — document-average reductions on content heavy in
dashed/dotted borders will be proportionally smaller than table-header
shows.

**Step 1: Capture the new numbers**

```bash
cargo run --release -q -p fulgur-cli -- \
  render examples/table-header/index.html -o /tmp/table-header-after.pdf
qpdf --qdf --object-streams=disable /tmp/table-header-after.pdf /tmp/table-header-after.qdf
grep -cE ' m$|^m$' /tmp/table-header-after.qdf      # expect ≈ 170 (was 822)
grep -cE ' l$|^l$' /tmp/table-header-after.qdf      # expect ≈ 510 (was 670)
ls -la /tmp/table-header-after.pdf                    # size should shrink from 37,640B
```

Record the numbers: `m`, `l`, `m + l`, and PDF byte size.

**Step 2: Update the regression test with tighter thresholds**

In `crates/fulgur/tests/rect_borders_test.rs`, replace the Task 1
assertions with values tied to the measurement. Add ~10% headroom
for per-environment variance (font fallback, etc.):

```rust
assert!(
    counts.m <= <measured_m * 1.1>,
    "expected m <= X, got m={} l={}", counts.m, counts.l
);
assert!(
    counts.l <= <measured_l * 1.1>,
    "expected l <= X, got m={} l={}", counts.m, counts.l
);
assert!(
    counts.m + counts.l <= <measured_m+l * 1.1>,
    "expected m+l <= X, got m={} l={}", counts.m, counts.l
);
```

Optionally record the PDF byte size before/after as a comment in the
test (not an assertion — byte counts are too environment-sensitive for
CI).

**Step 3: Run the test — expect PASS**

Run: `cargo test -p fulgur --test rect_borders_test -- --nocapture`
Expected: PASS.

**Step 4: Commit**

```bash
git add crates/fulgur/tests/rect_borders_test.rs
git commit -m "test(fulgur): tighten rect-border regression thresholds (fulgur-0ls)"
```

---

## Task 8: Final verification + record metrics on the beads issue

**Goal:** Final full-suite run and write the measured improvement onto the beads issue for future reference.

**Step 1: Full-suite run**

Run (from worktree root):

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test -p fulgur
cargo test -p fulgur-vrt
```

Expected: all clean.

**Step 2: Record metrics**

Append a note to the beads issue with before/after counts:

```bash
bd update fulgur-0ls --append-notes "## Result (table-header)

| op    | before | after | delta |
|-------|-------:|------:|------:|
| m     |    822 |  <M>  | ...   |
| l     |    670 |  <L>  | ...   |
| m + l |   1492 | <M+L> | ...   |
| S     |    783 |  <S>  | ...   |
| size  | 37640B | <B>B  | ...   |

Note: krilla 0.7 does not emit the PDF \`re\` operator from
\`PathBuilder::push_rect\` — it decomposes to \`m + 3l + h\`. The
saving comes from fewer strokes per cell (one \`draw_path\` replaces
4 abutting \`stroke_line\` calls), not from rect-op emission. A krilla
upstream PR exposing \`Content::rect\` would unlock further savings.

Scope: figures cover Solid (Task 3) + Double (Task 5). Dashed/Dotted
borders retain the 4-line fallback per Task 4 (per-edge dash phase
conformance) and are not represented in these numbers.
"
```

**Step 3: Flush beads**

```bash
bd sync --flush-only
```

**Step 4: Done**

Hand back to `superpowers:verification-before-completion` and `superpowers:finishing-a-development-branch` per the Impl skill's Step 6.
