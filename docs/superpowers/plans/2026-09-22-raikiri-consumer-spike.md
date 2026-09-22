# Raikiri Consumer Boundary Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a reproducible standalone raikiri consumer probe, document the
verified missing surfaces, and file focused change proposals against
`mitsuru/raikiri` without changing fulgur's default renderer.

**Architecture:** A private Cargo workspace under `spikes/raikiri-consumer`
will call raikiri's public parse/cascade/layout APIs and characterize the
unimplemented streaming sink. The probe will expose only evidence: page count,
page origins, page names, and streaming status. Fulgur's future bookmark
collector remains the consumer of generic resolved property events; the spike
will not add PDF semantics to raikiri or use PageDrawables as a bridge.

**Tech Stack:** Rust 2024, Cargo, raikiri git dependencies pinned to
`58833283a9eb7fa8c3885a194d364740dce0244f`, `serde_json`, Beads, GitHub CLI.

---

### Task 1: Create the standalone probe workspace

**Files:**
- Create: `spikes/raikiri-consumer/Cargo.toml`
- Create: `spikes/raikiri-consumer/Cargo.lock` (generated and committed)
- Create: `spikes/raikiri-consumer/src/lib.rs`
- Create: `spikes/raikiri-consumer/src/main.rs`

- [ ] **Step 1: Add the probe manifest and write the failing characterization tests**

Create `Cargo.toml` first so Cargo can compile the test harness. The manifest
must contain the pinned dependencies shown below and an empty `[workspace]`
table so Cargo does not discover the parent fulgur workspace. Then create
`src/lib.rs` with tests that require these not-yet-defined functions:

```rust
pub fn probe_layout(html: &str) -> Result<LayoutReport, ProbeError>;
pub fn probe_streaming(html: &str) -> Result<StreamingReport, ProbeError>;
```

The first test must use a short HTML document and assert one page with page
index `0` and origin `0.0`. The second must use a forced `break-after: page`
document and assert two ordered page records with a strictly increasing second
origin. The third must call `probe_streaming` and assert that the pinned
raikiri commit returns `RenderError::Unimplemented` with feature
`"render_streaming"` and emits zero pages.

- [ ] **Step 2: Run the probe tests to verify the expected compile failure**

Run:

```bash
cargo test --manifest-path spikes/raikiri-consumer/Cargo.toml
```

Expected: fail because the probe workspace and `probe_layout` /
`probe_streaming` implementation do not exist yet. Do not add implementation
before this failure is observed.

- [ ] **Step 3: Generate the standalone lockfile**

Run `cargo generate-lockfile --manifest-path spikes/raikiri-consumer/Cargo.toml`
after creating the manifest and commit the generated standalone lockfile with
the probe so the later `--locked` commands use the exact resolved graph.

- [ ] **Step 4: Implement the minimal layout probe**

Implement `probe_layout` using only the public APIs already verified in the
raikiri source:

```rust
let options = ParseOptions {
    extra_stylesheets: &[],
    network: None,
    base_url: None,
};
let mut uncascaded = raikiri::parse(html.as_bytes(), &options)?;
let cascade = raikiri::build_cascaded(&uncascaded);
let pages = raikiri_dom::layout_pages(
    &mut uncascaded.dom,
    &cascade,
    PageBox::A4,
    FontContext::new(),
)?;
```

Map each `PageSlice` to a serializable record containing only
`page_index`, `content_origin_y`, and `page_name`. Convert parse and layout
errors to a small probe error type that preserves the formatted source error.

- [ ] **Step 5: Implement the streaming characterization probe**

Implement a no-op `ReplacedResolver` returning a zero intrinsic fallback and a
recording `RenderSink` whose `accept_page` increments a counter and whose
`finish_render` marks completion. Parse a separate `HtmlDocument` through
`raikiri::parse_html`, then call:

```rust
raikiri::render_streaming(
    &document,
    PageDefaults::default(),
    &resolver,
    StreamingConfig::default(),
    &mut sink,
)
```

Record `status`, `pages_emitted`, and `finished`. Preserve the exact
`Unimplemented { feature, migration_hint }` values in the report rather than
swallowing the error.

- [ ] **Step 6: Add the deterministic CLI report**

`src/main.rs` should run the two fixtures used by the tests and print one
`serde_json::to_string_pretty` object containing `layout` and `streaming`
sections. The CLI must not access `PageScene` or `PageDrawables`.

- [ ] **Step 7: Run the focused probe tests and CLI**

Run:

```bash
cargo test --manifest-path spikes/raikiri-consumer/Cargo.toml
cargo run --manifest-path spikes/raikiri-consumer/Cargo.toml --locked
```

Expected: all probe tests pass; the report shows one page for the basic case,
two pages for the forced break, increasing origins, and
`render_streaming` as unimplemented with zero emitted pages.

- [ ] **Step 8: Commit the probe**

```bash
git add spikes/raikiri-consumer
git commit -m "spike: probe raikiri consumer surface"
```

### Task 2: Write the evidence report

**Files:**
- Create: `docs/superpowers/spikes/2026-09-22-raikiri-consumer-spike.md`

- [ ] **Step 1: Record the exact probe evidence**

Include the raikiri revision, commands, and the actual CLI output. State that
`layout_pages` and `PageSlice` work at the pinned revision, while
`PageFragment` remains empty and `render_streaming` returns the observed
`Unimplemented` error.

- [ ] **Step 2: Classify the missing surfaces**

Use these independently actionable categories:

1. **Neutral page-fragment emission:** page-local item/rect/line-range data and
   page metadata are missing from the public consumer surface.
2. **Generic consumer-property observer:** no registration/notification path
   exists for resolved properties such as fulgur's `bookmark-level` and
   `bookmark-label` without adding PDF concepts to raikiri.
3. **Render completion contract:** `RenderSink` and `RenderSummary` shapes
   exist, but the page emission implementation is unavailable and the final
   consumer handoff cannot be exercised.
4. **Fulgur resource/font handoff:** the probe can use default
   `FontContext`, but the production mapping from `AssetBundle`, base path,
   font bytes, and replaced-element resolver into raikiri is not yet a single
   consumer-facing contract.

For each category, cite the exact raikiri source path and distinguish the
observed fact from the proposed API.

- [ ] **Step 3: Run documentation checks**

Run:

```bash
cargo fmt --all -- --check
```

Then scan the report for unsupported claims, `TODO`/`TBD` placeholders, and
any accidental PDF-specific API proposal in the raikiri section.

- [ ] **Step 4: Commit the evidence report**

```bash
git add docs/superpowers/spikes/2026-09-22-raikiri-consumer-spike.md
git commit -m "docs: record raikiri consumer surface gaps"
```

### Task 3: Prepare and file raikiri issue proposals

**Files:**
- Create: `docs/superpowers/spikes/raikiri-issues/neutral-page-fragments.md`
- Create: `docs/superpowers/spikes/raikiri-issues/consumer-property-observer.md`
- Create: `docs/superpowers/spikes/raikiri-issues/render-completion-sink.md`
- Create: `docs/superpowers/spikes/raikiri-issues/resource-font-handoff.md`

- [ ] **Step 1: Write issue bodies from the evidence report**

Each body must contain:

- title and pinned revision;
- reproduction command and observed output;
- current public API limitation;
- neutral proposed surface;
- explicit non-goals: no `Bookmark`, `Outline`, Krilla, or PDF type in raikiri;
- acceptance criteria with at least one raikiri-side test.

The consumer-property issue must propose a generic property name plus neutral
resolved value and node identity, with page-fragment notification kept
separate so fulgur can join the first fragment for an Outline destination.

- [ ] **Step 2: Review issue bodies locally**

Run:

```bash
rg -n "TODO|TBD|FIXME|Bookmark|Outline|Krilla|58833283" \
  docs/superpowers/spikes/raikiri-issues
```

`Bookmark` and `Outline` may appear only in the explanation of the non-goal and
consumer example, never as a proposed raikiri type.

- [ ] **Step 3: Create the GitHub issues**

After confirming authentication and repository identity:

```bash
gh auth status
gh issue create --repo mitsuru/raikiri \
  --title "feat: expose neutral page fragments to render consumers" \
  --body-file docs/superpowers/spikes/raikiri-issues/neutral-page-fragments.md
```

Then create the remaining three focused proposals with their exact bodies:

```bash
gh issue create --repo mitsuru/raikiri \
  --title "feat: add generic resolved consumer-property observer" \
  --body-file docs/superpowers/spikes/raikiri-issues/consumer-property-observer.md
gh issue create --repo mitsuru/raikiri \
  --title "feat: implement render completion for page consumers" \
  --body-file docs/superpowers/spikes/raikiri-issues/render-completion-sink.md
gh issue create --repo mitsuru/raikiri \
  --title "feat: define consumer resource and font handoff" \
  --body-file docs/superpowers/spikes/raikiri-issues/resource-font-handoff.md
```

Use the actual returned issue URLs in the evidence report. This is an
explicitly requested external write; do not create issues in any other
repository.

- [ ] **Step 4: Read back every created issue**

For every returned issue number, run:

```bash
gh issue view <number> --repo mitsuru/raikiri --json number,title,url,state
```

Verify title, repository, and open state before recording the URL.

- [ ] **Step 5: Commit local proposal bodies and issue links**

```bash
git add docs/superpowers/spikes
git commit -m "docs: propose raikiri consumer surface changes"
```

### Task 4: Final verification and handoff

**Files:**
- Modify: `docs/superpowers/spikes/2026-09-22-raikiri-consumer-spike.md`

- [ ] **Step 1: Run the full scoped verification**

Run:

```bash
cargo test -p fulgur --lib --locked
cargo fmt --all -- --check
cargo test --manifest-path spikes/raikiri-consumer/Cargo.toml --locked
cargo run --manifest-path spikes/raikiri-consumer/Cargo.toml --locked
git status --short --branch
```

Expected: the fulgur baseline remains green, the standalone probe tests pass,
the CLI output remains deterministic, and only intended spike files are dirty
before the final documentation commit.

- [ ] **Step 2: Add issue URLs and final gap summary**

Update the evidence report with every read-back issue URL, the exact observed
limitations, and a concise statement that no default fulgur render path was
changed.

- [ ] **Step 3: Commit the final report**

```bash
git add docs/superpowers/spikes/2026-09-22-raikiri-consumer-spike.md
git commit -m "docs: link raikiri consumer proposals"
```

- [ ] **Step 4: Push the feature branch and sync Beads**

Because the project instructions require a pushed handoff, run from the
`.worktrees` checkout:

```bash
git pull --rebase
bd dolt push
git push -u origin spike/raikiri-consumer-spike
git status --short --branch
```

The final status must show the branch tracking its origin and no uncommitted
intended changes. Do not merge or mark the spike complete until the GitHub
issue readbacks and push both succeed.
