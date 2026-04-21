# WPT rel=mismatch (negative reftest) support — design

Beads: fulgur-rx3f  
Epic: fulgur-2foo (WPT reftest runner)  
Date: 2026-04-21

## Problem

`crates/fulgur-wpt/src/reftest.rs::classify()` treats `<link rel="mismatch">`
as `ReftestKind::Skip { reason: SkipReason::Mismatch }`. The harness never
runs these tests, so 4 entries in `expectations/css-page.txt` sit permanently
as `SKIP  ... # Mismatch` and cannot contribute to coverage even if fulgur's
output would be correct.

Target subset inventory:

- css-page: 4 tests (basic-pagination-004/005, page-orientation-on-portrait-002/003)
- css-break: 2 tests (out of scope — picked up in Phase 4)
- None use fuzzy metadata; none combine `rel=match` with `rel=mismatch`.

## Non-goals

- No change to the rendering pipeline.
- No asset-URL resolution work (tracked separately in fulgur-6cy5).
- css-break mismatches not promoted in this change — expectations for that
  subdir do not exist yet.

## Design

### Reftest classification (`reftest.rs`)

`ReftestKind` grows a `Mismatch` variant that mirrors `Match`:

```rust
pub enum ReftestKind {
    Match    { ref_path: PathBuf, fuzzy: FuzzyTolerance },
    Mismatch { ref_path: PathBuf, fuzzy: FuzzyTolerance },
    Skip     { reason: SkipReason },
}
```

`SkipReason` gains two variants to distinguish the remaining skip cases from
the now-implemented single-mismatch path:

- `MultipleMismatches` — 2+ `rel=mismatch` links
- `MixedMatchAndMismatch` — one of each (WPT spec allows, but out of subset)

The existing `SkipReason::Mismatch` is retained so older expectations files
(`# Mismatch` comments) remain parseable during the transition.

`classify()` collects `rel=match` and `rel=mismatch` hrefs independently,
then picks an outcome by count:

| match count | mismatch count | result |
|---|---|---|
| ≥2 | any | `Skip(MultipleMatches)` |
| any | ≥2 | `Skip(MultipleMismatches)` |
| 1 | ≥1 | `Skip(MixedMatchAndMismatch)` |
| 1 | 0 | `Match { ... }` |
| 0 | 1 | `Mismatch { ... }` *(new)* |
| 0 | 0 | `Skip(NoMatch)` |

The `meta[name=fuzzy]` selection logic (scope-matched meta wins, otherwise
last unscoped wins) is shared between match and mismatch — it operates on
`ref_path` regardless of which variant resolves it.

### Harness (`harness.rs`)

An internal `Kind { Match, Mismatch }` drives the comparison phase. Rendering
and page-count check are shared. Per-page comparison and final verdict flip:

- **Match** (existing): each page must be within fuzzy tolerance; any
  page-diff exceeding tolerance → `Fail`. Page-count mismatch → `Fail`.
- **Mismatch**: *any* page exceeding fuzzy tolerance OR page-count mismatch
  → `Pass` (the test demonstrated a visible difference, which is exactly
  what the test expected). All pages within tolerance → `Fail` with reason
  `"mismatch expected but test matches ref within tolerance"`.

On mismatch-FAIL no diff image is written (a passing match would be
indistinguishable, and there is no "diff" worth preserving). On mismatch-PASS
no diff image is written either (the difference is expected).

### Tests (TDD order)

Written first, failing against current `main`:

**Unit (`reftest.rs`)**:

- `single_mismatch_classified_as_mismatch`
- `mismatch_with_fuzzy_meta`
- `multiple_mismatches_skip`
- `mixed_match_and_mismatch_skip`
- Replace existing `mismatch_skip` (which asserts the current Skip behavior).

**Harness integration (`tests/harness_smoke.rs`)**:

- `mismatch_test_with_identical_ref_is_fail`
- `mismatch_test_with_different_ref_is_pass`
- `mismatch_test_different_page_count_is_pass`

Uses small tempdir HTML pairs so the PDF pipeline runs in under a second.

### Expectations rollout

After implementation is green, run each of the 4 tests through `run_one` and
update `expectations/css-page.txt`:

```text
# Before
SKIP  css/css-page/basic-pagination-004-print.html  # Mismatch

# After (observed-dependent; FAIL expected given missing break-after support)
FAIL  css/css-page/basic-pagination-004-print.html  # mismatch expected but test matches ref within tolerance
```

The header summary line (`# Summary: N PASS, N FAIL, N SKIP (total 257)`)
is recomputed to match.

## Predicted outcomes

- `basic-pagination-004/005-print.html` → **FAIL** expected. fulgur currently
  does not honor `break-after: page` (see pseudo-first-margin investigation
  on 2026-04-21), so test and ref collapse to a single page and match.
- `page-orientation-on-portrait-002/003-print.html` → **FAIL or PASS**
  depending on whether `page-orientation: rotate-left` is implemented in
  `gcpm/`. Either outcome is a useful expectation anchor.

Even a FAIL baseline is valuable: expectations now track the inverse-reftest
dimension, so a future fulgur fix converts declared=FAIL to declared=PASS
without further harness work.

## Rollout

1. beads fulgur-rx3f open.
2. Implement in worktree on branch `feature/wpt-rel-mismatch`.
3. TDD cycle: unit tests → reftest.rs change → harness tests → harness.rs change.
4. Run all 4 mismatch tests via `run_one`, capture observed verdict.
5. Update expectations, README, close fulgur-rx3f.

## Out of scope / follow-up

- css-break subdir Phase 4 (future epic child issue).
- Chained references (`ChainedReference` variant unused today).
- Absolute-URL ref href resolution (fulgur-6cy5).
