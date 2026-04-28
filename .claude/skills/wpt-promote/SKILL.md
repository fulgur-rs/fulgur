---
name: wpt-promote
description: Promote one or more individual WPT tests from FAIL to PASS in fulgur-wpt expectations. Use when the user asks to "make WPT X pass", "fix WPT regression", "WPT を PASS に", references a specific test like `css/css-page/<name>-print.html`, or hands off a beads issue whose acceptance criteria includes flipping a WPT expectation. Excludes blanket "fix all failing WPT" sweeps and pure expectation-file edits with no underlying fix.
---

# wpt-promote

Repeatable workflow for taking one (or a small set) of failing WPT tests from
FAIL to PASS in `crates/fulgur-wpt/expectations/`. Captures the path that
worked for `fulgur-aijf` (PR #260, `page-background-002/003` + `fixedpos-001`):
reproduce → root-cause → fix with TDD → verify net WPT delta → promote
expectations + downgrade unavoidable regressions → ship.

**Announce at start:** "Using wpt-promote to flip <test name> from FAIL to PASS."

## Usage

```text
/wpt-promote <target> [<target>...]
```

`<target>` accepts any of these forms (mix-and-match across positional args):

| Form | Example | Resolution |
|---|---|---|
| Full WPT path | `css/css-page/page-background-002-print.html` | Used verbatim |
| Phase-relative path | `css-page/page-background-002-print.html` | Prefix `css/` |
| Test stem | `page-background-002` | Search expectations across all `<phase>.txt` for the matching FAIL line; auto-append `-print.html` if needed |
| Beads issue ID | `fulgur-aijf` | `bd show <id>` then extract every WPT test path mentioned in description / acceptance criteria |
| `--issue <id>` | `--issue fulgur-aijf` | Explicit beads issue form (use when ambiguous with stems) |
| `--phase <name>` | `--phase css-page page-background-002` | Disambiguate stems when the same name exists in multiple phases |

Multiple targets are allowed and **encouraged** when they share a root
cause — they'll be promoted together by one underlying fix.

If no argument is given, list `bd ready` issues whose acceptance
criteria mention WPT and ask which to target.

### Argument resolution

Resolve targets to full WPT paths before any other work:

```bash
# 1. Direct path (already full)
case "$arg" in
  css/*) target_path="$arg" ;;
  css-*/*) target_path="css/$arg" ;;
esac

# 2. Beads issue ID (matches fulgur-XXXX)
if [[ "$arg" =~ ^fulgur-[a-z0-9]+$ ]] || [ "$arg" = "--issue" ]; then
  bd show <id> | grep -oE "css/[^ ]+-print\.html"   # extract paths
fi

# 3. Stem (e.g. page-background-002): grep across expectations
if [ -z "$target_path" ]; then
  grep -lE "^FAIL\s+css/.+/${arg}(-print)?\.html" \
    crates/fulgur-wpt/expectations/*.txt
fi
```

Confirm each resolved path exists under `target/wpt/` before proceeding.
If a stem matches more than one phase, ask the user (or take `--phase`).

## Inputs (after resolution)

The agent now has:

- One or more **WPT test paths** (e.g.
  `css/css-page/page-background-002-print.html`)
- Optionally an associated **beads issue** for tracking design/notes
- The matching **`<phase>.txt`** file in
  `crates/fulgur-wpt/expectations/` (or `expectations/lists/<name>.txt`
  for cherry-picked lists)

## Workflow

```dot
digraph wpt_promote {
  rankdir=TB;
  node [shape=box];

  start [label="1. Identify target test(s)" shape=doublecircle];
  repro [label="2. Reproduce locally\n(render test/ref through fulgur)"];
  rc [label="3. Root-cause\n(read test/ref HTML, locate fulgur source)"];
  design [label="4. Design + advisor\n(save to beads design field if non-trivial)"];
  tdd [label="5. TDD: failing regression test\n→ implement fix → green"];
  wpt [label="6. Run WPT runner\nparse summary.md / regressions.json"];
  has_reg [label="New regressions\nintroduced?" shape=diamond];
  triage [label="Triage: in-scope vs follow-up\nfile beads issue for follow-up"];
  flip [label="7. Update expectations\n(promote ALL promotion candidates)"];
  reverify [label="Re-run WPT\n0 promotions / 0 regressions delta?" shape=diamond];
  lint [label="8. fmt + clippy + full test suite + VRT + cli"];
  done [label="9. Commit + push + PR\nthen resolving-ai-review" shape=doublecircle];

  start -> repro;
  repro -> rc;
  rc -> design;
  design -> tdd;
  tdd -> wpt;
  wpt -> has_reg;
  has_reg -> triage [label="yes"];
  triage -> flip;
  has_reg -> flip [label="no"];
  flip -> reverify;
  reverify -> flip [label="no, fix expectations"];
  reverify -> lint [label="yes"];
  lint -> done;
}
```

## Step 1 — Identify the target test(s)

The argument resolution above already produced one or more WPT test
paths. For each, extract:

- The matching **`ref` file** (usually `<name>-print-ref.html` next to
  the test under `target/wpt/`)
- The **current FAIL reason** from `crates/fulgur-wpt/expectations/<phase>.txt`
  (`page count mismatch: test=N ref=M`, `page 1 diff: N pixels...`,
  etc.) — the comment after `#` carries this
- If a beads issue is the entry point, also note its **acceptance
  criteria** for any extra hints on related tests sharing the root
  cause

```bash
# Quick lookup of FAIL reason for a target
grep "<test-path>" crates/fulgur-wpt/expectations/<phase>.txt
```

If multiple targets share the same root cause, **list them all** —
expectations promote together when the underlying fix lands. The
"promotion candidates" line in the WPT summary later confirms which
tests went green from a single fix.

## Step 2 — Reproduce locally

Render both files through fulgur and confirm the failure mode:

```bash
# Render test
cargo run --bin fulgur --release -- \
  render target/wpt/css/<phase>/<name>-print.html -o /tmp/test.pdf
pdfinfo /tmp/test.pdf | grep Pages

# Render ref
cargo run --bin fulgur --release -- \
  render target/wpt/css/<phase>/<name>-print-ref.html -o /tmp/ref.pdf
pdfinfo /tmp/ref.pdf | grep Pages
```

If page counts differ, the failure is "page count mismatch". If they match
but pixels differ, it's a visual regression — use the WPT runner's diff
images under `target/wpt-report/<phase>/diffs/<name>/` to inspect.

**WPT data path**: tests live under `target/wpt/`. If missing, the WPT
runner reports "skip" — symlink from main worktree
(`ln -s /home/ubuntu/fulgur/target/wpt .worktrees/<name>/target/wpt`)
or run `scripts/wpt/fetch.sh` from a worktree where it exists.

## Step 3 — Root-cause

Open both HTML files. Identify which CSS feature/property is the test's
focus — usually visible in `link rel="help"` or the inline comment.

Locate the fulgur source for that feature:

```bash
# Find the relevant convert path
grep -rn "<feature>" crates/fulgur/src/convert/ | head
```

For non-trivial root causes, **call `advisor()` before committing to a
fix approach** — the advisor sees the conversation and can flag missing
constraints (height-fold, paint-order, OOF-aware split logic, etc. —
this is exactly how PR #260's `out_of_flow` flag was identified).

## Step 4 — Design

For substantial work (anything beyond a one-line fix), save the design
to the beads issue's `design` field:

```bash
bd update <issue-id> --design="$(cat /tmp/design.md)"
```

Lead the design with the **two-piece rule** when applicable: many "promote
this WPT" fixes split into (a) a structural flag/state change in
`pageable.rs` and (b) a hoisting/conversion change in `convert/`.
PR #260's `out_of_flow` flag + `build_absolute_non_pseudo_children` was
exactly this shape.

## Step 5 — TDD

Create a worktree (`superpowers:using-git-worktrees`) and:

1. **Write a failing regression test first**, in
   `crates/fulgur/tests/<feature>_test.rs`. Use the existing `page_count`
   helper (`tests/page_break_wiring.rs:11`) for byte-count tests, or
   `Engine::build_pageable_for_testing_no_gcpm()` for tree-inspection
   tests when the oracle needs to detect dropped/misplaced nodes
   (`!pdf.is_empty()` is **never** a sufficient oracle — krilla always
   serialises a complete PDF).
2. **Implement** the fix.
3. Verify the regression test goes green.
4. Run the full suite locally:

   ```bash
   cargo test -p fulgur --lib --quiet | tail -3
   cargo test -p fulgur 2>&1 | grep "test result:"
   ```

If you added pure helpers in `pageable.rs`, also add unit tests in
`#[cfg(test)] mod tests` per CLAUDE.md's "Coverage scope" rule —
codecov's patch coverage doesn't count VRT-only paths.

## Step 6 — Run the WPT runner

```bash
ln -sf /home/ubuntu/fulgur/target/wpt target/wpt  # if not present
FULGUR_WPT_REQUIRED=1 cargo test -p fulgur-wpt --test wpt_<phase> --release \
  -- --nocapture 2>&1 | grep -E "regressions=|promotions=|<phase> report"
```

Phases: `wpt_css_page`, `wpt_lists`, `wpt_smoke`. The phase name maps to
`target/wpt/css/<subdir>/`.

After it runs, parse the report:

```bash
cat target/wpt-report/<phase>/summary.md | grep -A30 "Promotion candidates"
cat target/wpt-report/<phase>/regressions.json | python3 -c "
import json, sys
d = json.load(sys.stdin)
for r in d:
    print(r['test'].split('/')[-1].replace('-print.html', ''), '→', r['message'])"
```

**Critical: distinguish your fix's deltas from pre-existing flakiness.** Run
the same WPT phase against `main` and compare regression/promotion lists.
The numbers that matter for your fix:

- *Net new promotions*: tests now PASS that were FAIL on main → these are
  your fix's wins.
- *Net new regressions*: tests now FAIL that were PASS on main → these are
  your fix's costs.

Pre-existing failures (same regressions on main and your branch) are
**not your responsibility** in this PR.

## Step 7 — Update expectations

Edit `crates/fulgur-wpt/expectations/<phase>.txt`:

- **Net new promotions**: flip `FAIL → PASS` and remove the comment.
  *Promote ALL of them*, not just the target — leaving promotion
  candidates as `FAIL` is "leaking correctness" and the next reviewer
  will flag it.
- **Net new regressions**:
  1. Open a follow-up beads issue (`bd create --type=bug --priority=3`)
     describing the regression, the test that failed, and a direction
     for the deeper fix.
  2. Flip the expectation comment to reference the new issue:

     ```text
     FAIL  css/...  # fulgur-XXXX: <one-line reason>
     ```

- **Pre-existing regressions** (same on main): leave untouched.

Then **re-run the WPT runner** and confirm:

```text
regressions=<same number as main>
promotions=0
```

`promotions=0` means expectations are in sync — no candidates left
unflipped.

## Step 8 — Lint + final verification

```bash
cargo fmt
cargo fmt --check         # must be clean
cargo clippy --all-targets 2>&1 | grep "^warning:"  # must be empty
cargo test -p fulgur 2>&1 | grep -c "FAILED"        # must be 0
cargo test -p fulgur-vrt --release 2>&1 | grep -c "FAILED"  # must be 0
cargo test -p fulgur-cli 2>&1 | grep -c "FAILED"    # must be 0
```

`cargo fmt --check` is CI-enforced (CLAUDE.md). VRT goldens may need
regenerating with `FONTCONFIG_FILE=$PWD/examples/.fontconfig/fonts.conf
FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt` if rendering changed
intentionally — but treat any unintended VRT shift as a stop signal,
not a regenerate-and-go.

## Step 9 — Commit, push, PR

Per project memory:

- **Commit message**: English. Lead with `fix(<area>):` or `feat(<area>):`
  describing the structural change. Body explains *why* and lists the
  WPT delta concisely. Reference the beads issue ID at the bottom (e.g.
  `fulgur-aijf`).
- **PR title**: English (under 70 chars).
- **PR body**: Japanese. Include "概要", "設計", "検証", "WPT delta" with
  a small table for promotions/regressions.

Push and create the PR via `gh pr create`. Do **not** push to `main`.

## Step 10 — AI review iteration

After the PR is up, hand off to **`resolving-ai-review`** for coderabbit
/ Devin Review iteration. That skill polls for re-reviews, triages
findings, and cycles fix → push → poll until coderabbit no longer
requests changes. Don't manually re-poll.

## Examples

```text
# Beads issue with WPT in acceptance criteria
/wpt-promote fulgur-puml

# Single test, full path
/wpt-promote css/css-page/fixedpos-002-print.html

# Single test, stem (skill grep's expectations to find phase)
/wpt-promote fixedpos-002

# Multiple tests sharing a root cause (same fix promotes both)
/wpt-promote fixedpos-002 fixedpos-004 fixedpos-005 fixedpos-006

# Phase-relative shorthand (skill prefixes css/)
/wpt-promote css-page/page-background-002-print.html

# Disambiguate stems that match multiple phases
/wpt-promote --phase css-page fixedpos-002

# Explicit beads issue form when stem is ambiguous with an issue ID
/wpt-promote --issue fulgur-aijf
```

## Common pitfalls

| Pitfall | Why it happens | How to avoid |
|---|---|---|
| Test passes but oracle is too weak | `assert!(!pdf.is_empty())` always holds because krilla serialises a complete PDF on every render | For "X is in the tree" oracles, walk the Pageable tree via `Engine::build_pageable_for_testing_no_gcpm` and assert structural properties (size, `out_of_flow`, etc.) |
| "Why did test=N ref=M when same engine renders both?" | The `<test>` and `<ref>` files use different CSS features. Engine bugs may surface only in one path. | Always render BOTH through fulgur and inspect each independently. |
| Flipped one promotion, missed others | Single-test focus | After re-running WPT, the report's `promotions=0` line is the gate. Anything > 0 means an unflipped candidate. |
| New regressions look like pre-existing flakiness | Both report lines say `FAIL ... declared=PASS` | Run the same WPT phase on `main` to baseline, then diff the regression lists. Net-new regressions are yours. |
| `display:contents` zero-size wrapper test | Blitz drops abs descendants of contents-display wrappers entirely (separate issue). Tests using it as the zero-size container won't exercise the flatten path you're guarding. | Use a real `width:0; height:0; overflow:visible` `<div>`. |
| Worktree sparse-checkout blocks `git add` | `git worktree add` inherits a `.beads/`-only sparse-checkout pattern | Right after creating the worktree: `git -C <path> sparse-checkout disable`. Documented in CLAUDE.md. |
| Forgot to symlink `target/wpt` in worktree | WPT runner reports "skip: ... missing" | `ln -s /home/ubuntu/fulgur/target/wpt .worktrees/<name>/target/wpt` once per worktree. |
| Tried to push without authorization | Auto mode ≠ push permission | `git push` only after explicit user "yes" / "push it" / "make a PR". CLAUDE.md is explicit. |

## Red flags — STOP

- Editing `crates/fulgur-wpt/expectations/<phase>.txt` to flip a test to
  PASS without an underlying source change. That's gaming the oracle.
- Running the WPT runner with `FULGUR_VRT_UPDATE=1` to "make tests pass"
  — that flag is for VRT goldens, not WPT. Different test plumbing.
- `cargo fmt` modifying files you haven't read in this session — Edit
  tool will refuse. Read the file first.
- Net regressions count goes UP. Investigate before promoting anything.

## Integration

**Pairs with:**

- `superpowers:using-git-worktrees` — creates the isolated workspace.
- `blueprint:impl` — for running the full design → plan → execute
  pipeline if the user starts from `bd ready` rather than a specific
  WPT test.
- `resolving-ai-review` — handles coderabbit / Devin iteration after the
  PR is up.

**Calls these sub-skills:**

- `superpowers:verification-before-completion` — before declaring done.
- `superpowers:finishing-a-development-branch` — for cleanup after merge.

**Project context** loaded automatically:

- `CLAUDE.md` — coverage scope, fmt/clippy CI rules, sparse-checkout
  gotcha, font determinism, "Engine is a builder" reminder.
- `.claude/rules/coordinate-system.md` — px↔pt conversion, Krilla Y-down,
  Stylo length-percentage basis. Consult when the fix touches drawing
  code.
- `.claude/rules/markdownlint.md` — for any docs touched.

## Reference path: `fulgur-aijf` / PR #260

Concrete example of the full workflow for posterity:

- **Target**: `css/css-page/page-background-002/003-print.html` (abs `<img>`
  consumes a page).
- **Root cause**: `convert/positioned.rs` had a re-emit path for abs
  pseudos but not non-pseudo abs DOM children. Identified via advisor
  (which flagged the missing `BlockPageable::wrap` height-fold concern
  the description had glossed over).
- **Two-piece fix**: (a) `PositionedChild::out_of_flow` flag with
  height-fold / find_split_point / clone_pc_with_offset OOF-aware logic;
  (b) `build_absolute_non_pseudo_children` mirroring the pseudo path,
  wired via a `build_absolute_children` source-order wrapper.
- **WPT delta**: +3 net promotions (`page-background-002/003`,
  `fixedpos-001`), -2 net regressions (`monolithic-overflow-013`,
  `page-name-propagated-003`) tracked in `fulgur-puml`.
- **AI review rounds**: 2. Round 1: 3 findings (flatten guard, OOF-aware
  index/length checks, has_forced_break_below skip). Round 2: 3 findings
  (paint order, within-split sibling reorder, weak test oracle).
- Closed with merge after round 2 cleared.
