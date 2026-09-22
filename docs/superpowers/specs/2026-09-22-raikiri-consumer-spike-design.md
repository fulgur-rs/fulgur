# Raikiri Consumer Boundary Spike Design

**Date:** 2026-09-22

**Beads:** `fulgur-3ylj`

## Goal

Run a small, reproducible integration spike from the `fulgur` repository against
the pinned `raikiri` main commit to identify the public surface required for
fulgur to consume raikiri-owned parsing, GCPM, layout, and pagination. Convert
each concrete missing surface into a proposed raikiri GitHub issue without
adding PDF-specific semantics to raikiri.

## Architecture

The intended production boundary is:

```text
HTML/CSS
  -> raikiri parse/cascade/GCPM/layout/pagination
  -> neutral page-fragment and consumer-property events
  -> fulgur PDF consumer / Krilla serialization
```

The spike will exercise the currently public raikiri parse/cascade/page-layout
path from a standalone private crate under `spikes/raikiri-consumer`. It will
not route the default fulgur `Engine::render` path, use `PageScene` or
`PageDrawables` as the integration contract, or duplicate pagination inside
fulgur. The probe will capture the successful `PageSlice` behavior and the
blocking gaps at the `PageFragment`/`RenderSink` and generic consumer-property
callback boundary.

Bookmark handling remains a fulgur consumer concern. Fulgur will eventually
register generic consumer properties such as `bookmark-level` and
`bookmark-label`; raikiri may parse/cascade/resolve their neutral values and
notify a generic observer, but raikiri will not construct a PDF outline.

## Spike components

### 1. Standalone probe

Create `spikes/raikiri-consumer/Cargo.toml` and `spikes/raikiri-consumer/src/main.rs`.
The crate is a separate Cargo workspace so adding the probe does not alter the
default fulgur workspace dependency graph or production feature set. It pins
raikiri to the observed commit `58833283a9eb7fa8c3885a194d364740dce0244f`.

The probe will:

1. parse a basic document and a forced-break document through `raikiri::parse_html`;
2. build the first-page cascade;
3. call `raikiri_dom::layout_pages` with an explicit `PageBox` and `FontContext`;
4. print a deterministic JSON-like report containing page count, page indices,
   content origins, and named-page selections;
5. call the public `raikiri::render_streaming` entry point through a recording
   sink and report its current `RenderError::Unimplemented` result.

The probe deliberately does not inspect `PageDrawables`. Direct DOM access is
allowed only inside the diagnostic probe to show what information is currently
available and must not become the proposed fulgur integration contract.

### 2. Evidence report

Create `docs/superpowers/spikes/2026-09-22-raikiri-consumer-spike.md` with:

- the exact raikiri commit and probe command;
- observed successful behavior;
- observed unavailable behavior;
- a table mapping each fulgur requirement to current raikiri evidence;
- issue-ready proposals for page fragments, generic consumer properties,
  finalization, and resource/font plumbing.

The report will distinguish facts observed at the pinned commit from proposed
API shape. It will not claim PDF parity from a page-count probe.

### 3. Raikiri issue proposals

After the probe and report are verified, create one issue per independent missing
surface in `mitsuru/raikiri` with:

- pinned commit and reproduction command;
- the current public API and why it blocks a consumer;
- a neutral API proposal;
- explicit non-goals that keep PDF Outline/Krilla types out of raikiri;
- acceptance criteria for the raikiri-side change.

The issue set will cover only gaps demonstrated by the probe. Duplicate or
overlapping proposals will be consolidated before creation, and every created
issue will be read back and linked in the evidence report.

## Data and ownership rules

- raikiri owns parsing, cascading, GCPM state, page layout, pagination, and
  resolved page-fragment production.
- fulgur owns PDF policy, including whether to collect bookmarks, Outline tree
  construction, resource limits for Outline labels, and Krilla serialization.
- The future callback must be generic: property name plus neutral resolved value
  and node identity, not a `BookmarkCallback` or a PDF-specific enum.
- Page placement must be delivered separately or alongside a neutral fragment
  event, because fulgur needs the first fragment's page and coordinates for an
  Outline destination.
- Final document-level consumer output belongs at render completion, not in a
  per-page Outline-specific payload. The existing `RenderSink::finish_render`
  protocol is the candidate handoff point, but the spike will not invent a
  production API before the missing evidence is recorded.

## Testing and verification

The spike will run:

```text
cargo test -p fulgur --lib --locked
cargo fmt --all -- --check
cargo run --manifest-path spikes/raikiri-consumer/Cargo.toml --locked
```

The first command establishes the clean fulgur baseline. The standalone probe
must show:

- one page for a short document;
- two ordered pages for a forced page break;
- monotonically increasing continuation origins;
- the current `render_streaming` unimplemented result, if unchanged at the
  pinned commit.

The final verification will also check that the default fulgur workspace does
not acquire the spike dependency and that the main checkout's pre-existing
untracked files are unchanged.

## Non-goals

- No default fulgur renderer switch in this spike.
- No `PageDrawables` adapter.
- No PDF-specific types or Outline implementation in raikiri.
- No speculative raikiri source patch before the missing API is evidenced.
- No merge or deletion of the existing fulgur Blitz path.
