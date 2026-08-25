# fulgur-pgbrk: engineering review and roadmap

**Goal:** Consolidate the fulgur-pgbrk campaign into a verified birdseye review,
name the structural root cause of its recurring defect pattern, and lay out the
remaining roadmap under the project's standing constraints: **single-pass
layout, byte-determinism, and un-forked upstream dependencies** — all three
hard requirements per project direction (2026-08-18).

**Predecessors:**
[page-fragmentation review](./2026-08-16-fulgur-pgbrk-page-fragmentation-review.md),
[R1 border-box design](./2026-08-16-fulgur-pgbrk-r1-border-box-design.md),
[R3 overflow-detection design](./2026-08-16-fulgur-pgbrk-r3-overflow-detection-design.md),
[break-decision extraction design](./2026-08-17-fulgur-pgbrk-break-decision-extraction-design.md),
[break-decision extraction plan](./2026-08-17-fulgur-pgbrk-break-decision-extraction-plan.md).

**Successor design:** [2026-08-18-fulgur-single-pass-fragmentation-design.md](./2026-08-18-fulgur-single-pass-fragmentation-design.md).

**Spec baseline:** [CSS Fragmentation Module Level 3](https://www.w3.org/TR/css-break-3/).

**Tech stack:** Rust, Blitz/Taffy/Parley layout (crates.io deps: `blitz-dom 0.2`,
`taffy 0.9`, `parley 0.6`, `stylo 0.8`), krilla PDF output.

---

## Verified architecture map

All numbers below were measured against the tree at the time this review was
written, not copied from prior docs. **Historical snapshot:** the campaign
has since closed (2026-08-19, see the predecessor review's closing note) —
R7 shipped and `pagination_layout.rs` now has zero `#[ignore]`d tests; the
counts below are the pre-R7 baseline this review reasoned from, not current
state.

- `crates/fulgur/src/pagination_layout.rs`: **9,544 lines**, ~4,730 of them the
  `#[cfg(test)] mod tests` block. **139 tests**, **11 `#[ignore]`d** (the R7
  remainder: 7 flex/grid co-split, 4 nested-monolithic).
- The walk:

```text
run_pass_inner
└─ fragment_pagination_root      (body's direct children — 781 lines)
   ├─ fragment_inline_root       (line-level splits — 76 lines, 12 params)
   └─ fragment_block_subtree     (nested containers — 1,037 lines, 12 params; recurses)
      └─ fragment_inline_root    (nested paragraphs)
```

- `would_split_block_subtree` (69 lines) is an emission-free **simulator** of
  the child walk, used to gate recursion.
- `SubtreeResult` (`Placed` / `RequestBreakBefore`) hands break decisions up to
  the caller (css-break-3 §3.1.1 / §4.4 rule 2).
- Single-source extractions landed in this campaign: `break_decision` (one
  break predicate, 3 consumers), `ParentSlice` (one parent-slice emission
  idiom, 9 consumers), `parent_slice_height`, `inline_root_box_metrics`,
  `scan_split_points` (returns a discardable plan for the §4.4 relaxation
  re-run), `resolved_line_constraints` (orphans/widows by ancestor walk).
- Output: `PaginationGeometryTable = BTreeMap<usize, PaginationGeometry>`,
  consumed by `render.rs` (`paragraph_lines_for_page`, fragment dispatch) and
  by `find_overflowing_fragments` / `report_fragment_overflow` — warn in
  production, **panic in every test build**, making "no fragment escapes the
  strip" a blanket invariant.
- Test totals at the last full-suite run documented in this campaign:
  `cargo test -p fulgur` → 2481 passed, 0 failed, 18 ignored.

### WPT infrastructure (verified)

- `crates/fulgur-wpt` is a working reftest runner: fetches the W3C
  web-platform-tests corpus into `target/wpt` (sparse checkout via
  `scripts/wpt/fetch.sh` + `subset.txt`), renders tests and references with the
  production engine, and compares with fuzzy tolerance per WPT reftest rules.
- Expectations language: `PASS` / `FAIL` / `SKIP` per test per phase file;
  promotion = flip `FAIL`→`PASS` after `run_one` confirms (documented in
  `crates/fulgur-wpt/README.md`; the `wpt-promote` skill encodes the flow).
- Phases today: `css-page` (**84 PASS / 139 FAIL / 34 SKIP**, 261 entries),
  `css-multicol` split into list shards, plus cherry-pick lists.
- `css/css-break` is **fetched on disk** (638 files in `target/wpt/css/css-break`,
  roughly half of them reftest references) but has **no expectations phase yet**.

---

## Campaign status

R1 (border-box inline roots), R2 (widow relaxation), R3 (overflow detection),
R4 (`break-inside: avoid` on blocks), R5 (forced-break propagation), R6 (author
orphans/widows), R8 (forced-break parent dedup) and the Risk-1 extraction
(break_decision + ParentSlice) are shipped. **Only R7 remains**, in two
bounded clusters, each pinned by failing `#[ignore]`d repros:

1. **Nested-monolithic asymmetry (4 tests):** the body-direct path slices
   oversized leaves per strip; the nested path emits them whole. §4.1 permits
   either; fulgur's own inconsistency.
2. **Flex/grid internal fragmentation (7 tests):** rows co-split in place; a
   row taller than the strip overflows. Requires slicing cells at computed
   offsets rather than re-running layout.

## Root-cause diagnosis: the fork is the defect multiplier

Every rule must be correct in the **body path** *and* the **nested path**, and
the **simulator** must agree with both while emitting nothing. Nearly every
defect this campaign fixed was a *fork disagreement*, not a spec
misunderstanding:

- **R8:** nine parent-slice emission sites split 7-forced/2-unforced on dedup;
  only the two unforced sites consulted `row_state.emitted_parent_pages`.
- **R3 defect A/B:** two parent-slice height computations disagreed; one site
  had the right answer and fed it wrong anyway.
- **Vacuous tests** (three in this campaign): a new test passed because it
  never reached the code under test — the cost of invariants being implicit
  rather than structural.

The Risk-1 extraction unified the **predicate** and the **emission**; the
**structure** — two near-1,000-line recursive walkers plus a simulator — is
untouched. That fork, plus 12-parameter functions and 9-level nesting, is the
concrete reason the work "feels like drowning in details". Cognitive load
scales with the number of sites that must agree, not with spec size.

## The invariant that licenses single-pass

Pagination fragments **vertically only**. Horizontal geometry — and therefore
every width-dependent layout decision, including line wrapping — is
page-invariant. Anything in css-break-3 whose geometry is horizontal is decided
once and reused; anything vertical is composition, not re-layout. This is not
an approximation hack: it is the theorem that makes the architecture
sound-by-construction. The design doc promotes it from folk justification to a
stated invariant, and uses it to enumerate css-break-3's rules as
reachable-by-construction vs documented-limitation, so the conformance ceiling
is *enumerated* rather than repeatedly re-discovered.

## Findings on the conformance oracle

Hand-written unit tests first made sense when there was no corpus infrastructure,
but `fulgur-wpt` now exists. Today the nine hand-authored `css_break3_*` tests are
the de-facto conformance map while an upstream corpus (`css/css-break`, plus
`css/css-page` and `css/css-multicol` already on disk) sits unused for this
area. Hand-written spec tests are the wheel-reinvention happening here; the
upstream corpus is the battle-tested one browsers are held against. The
roadmap makes WPT primary and the hand block supplemental. Note the one caveat
the harness itself documents: chained references and root-relative `href`s are
SKIPPED by `classify()` — css-break promotion inherits that limitation.

## Roadmap

> Standing constraints (decisions of record, 2026-08-18): single-pass layout;
> byte-determinism; upstream crates.io deps. The roadmap is closed under all
> three.

- **P0 — Finish R7. SHIPPED 2026-08-19.** `330274b9` (nested monolithic
  slicing, 4 tests) and `fc2cd156` (flex/grid internal fragmentation, 7
  tests). `pagination_layout.rs` `#[ignore]` count: 11 → 0.
- **P1 — WPT `css/css-break` phase. SHIPPED 2026-08-19.** `185ed3a4`:
  `expectations/css-break.txt` seeded (36 PASS / 969 FAIL / 163 SKIP),
  phase runner and CI/nightly matrix entries wired. Promotion ledger is live.
- **P2 — Converge the walker fork. SHIPPED 2026-08-19.** Five commits
  (`1dd9473e` Ctx/Frame, `a12f6a02` enumeration, `2cf9eb2d` zero-height,
  `f03fba9b` inline-root, `a91855c5` recursion gate) plus `3555418e`
  (simulator assimilation). Behavior-preserving throughout; suite green with
  no re-blessing; VRT fixture-level byte-identical each phase.
- **P3 — Adopt the converged design. DONE.** This doc and the design doc are
  the landing artifacts.

## Non-goals

- Any re-layout-driven architecture. Production skips the Taffy re-drive by
  construction (`run_pass_inner`); the wrapper traits are preserved as
  test-only scaffolding, and P2/P3 keep that property.
- Forking or vendoring Blitz/Taffy/Parley/Stylo. They stay crates.io deps.
- Thinning byte-determinism (`examples_determinism`, VRT byte compares).

## Verification commands

```bash
npx markdownlint-cli2 '**/*.md'
cargo test -p fulgur                       # baseline preserved by P2
cargo test -p fulgur --lib -- --ignored    # open gaps: currently 11, all R7
cargo test -p fulgur-wpt                   # css-page phase baseline
```
