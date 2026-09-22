# Raikiri consumer boundary spike report

**Repository:** `fulgur`

**Beads:** `fulgur-3ylj`

**Raikiri revision:** `58833283a9eb7fa8c3885a194d364740dce0244f`

**Probe:** `spikes/raikiri-consumer`

## Reproduction

The probe was run from the `.worktrees/raikiri-consumer-spike` checkout. Cargo's
git and registry cache had to be redirected because `/home/ubuntu/.cargo` is
read-only in this environment; this does not change the probe's source or
dependency revision.

```bash
CARGO_HOME=/tmp/fulgur-raikiri-cargo \
  cargo test --manifest-path spikes/raikiri-consumer/Cargo.toml --locked
CARGO_HOME=/tmp/fulgur-raikiri-cargo \
  cargo run --manifest-path spikes/raikiri-consumer/Cargo.toml --locked
```

The probe intentionally does not use `PageScene` or `PageDrawables`.

## Observed output

The short document produced one page:

```text
page_index=0, content_origin_y=0.0, page_name=None
```

The forced-break document produced two ordered pages:

```text
page_index=0, content_origin_y=0.0, page_name=None
page_index=1, content_origin_y=1122.5196533203125, page_name=None
```

The streaming probe returned:

```text
status=unimplemented
feature=render_streaming
pages_emitted=0
finished=false
migration_hint="render_streaming is a non-goal for now. Single-page raster is implemented via `html_to_png`; multi-page streaming lands once the pagestream state machine is implemented"
```

This is layout/page-count evidence only. It is not PDF or pixel parity
evidence.

## Current public surface

### What works

`raikiri::parse` produces an `UncascadedDocument` with a public DOM, and
`raikiri::build_cascaded` produces the first-page cascade. The public
`raikiri_dom::layout_pages` entry point accepts the mutable DOM, cascade,
`PageBox`, and `FontContext` and returns ordered page slices.

The current `PageSlice` contains:

- `page_index`;
- `content_origin_y`;
- `page_name`.

Source: `raikiri-dom/src/layout/mod.rs`, `PageSlice` and `layout_pages`.

### What is unavailable

`raikiri_traits::PageFragment` is still an empty placeholder. Its source only
documents future fields such as `page_index`, `page_box`, and `items`.

`raikiri::render_streaming` has the intended `RenderSink` signature but always
returns `RenderError::Unimplemented`. It does not call `accept_page` or
`finish_render`.

`RenderSink` and `RenderSummary` already show the intended completion shape:
page emission followed by a final summary containing page count, target state,
unresolved slots, discrepancies, and warnings. The implementation that would
populate the page payload is not present at this revision.

## Fulgur consumer requirements versus evidence

| Required consumer surface | Current evidence | Gap |
|---|---|---|
| Page metadata | `PageSlice` exposes page index, origin, and name | Resolved page box, margins, orientation, and page-local paint area are not delivered as a single consumer snapshot |
| Per-node page fragments | `PageSlice` is page-level only; `Document` layout fields are not a page-fragment contract | Need neutral node identity, rect, line range, split/repeat state, and paint item ordering |
| Generic `bookmark-level` / `bookmark-label` handoff | No generic resolved consumer-property observer exists | Need registration plus neutral resolved values and node identity; no PDF Outline type belongs in raikiri |
| Page emission | `RenderSink::accept_page(PageFragment)` exists | `PageFragment` is empty and `render_streaming` is unimplemented |
| Finalization | `RenderSink::finish_render(RenderSummary)` exists | No production path emits pages or gives the consumer a final page/document snapshot |
| Resources and fonts | Probe uses `FontContext::new()` and no resolver | Fulgur needs one contract for asset fonts, base path, network policy, and replaced-element resolution |

## Proposed raikiri changes

The following proposals are intentionally neutral and do not introduce PDF
Outline, Krilla, or other PDF-specific types into raikiri.

### Neutral page fragments

Promote the page output contract from the empty `PageFragment` placeholder to a
neutral immutable page snapshot. It needs page metadata plus ordered items or
fragment records carrying node identity, geometry, text line/glyph information,
and split/repeat state. The contract must not expose Taffy, Parley, or
`PageDrawables` types directly to consumers.

### Generic consumer-property observer

Allow a consumer to register property names and receive a generic resolved
event containing:

- opaque node identity and parent/source order;
- property name;
- neutral resolved value;
- a separate page-fragment event for destination geometry.

Fulgur can register and interpret `bookmark-level` and `bookmark-label`, then
build the Outline tree itself. Raikiri only performs parsing, cascade, and
generic value resolution.

### Render completion

Implement the existing page-stream driver so `RenderSink::accept_page` receives
real page fragments and `finish_render` receives the final summary. The target
registry and convergence state remain generic; the consumer decides how to
serialize its document output.

### Resource/font handoff

Define a single consumer-facing configuration path that maps font bytes,
stylesheet sources, base URL/path, network policy, and replaced-element
resolver into parse/cascade/layout. The probe's default system `FontContext`
is sufficient for characterization but not for deterministic fulgur output.

## Ownership conclusion

Raikiri should own HTML/CSS parsing, GCPM, layout, pagination, and neutral page
output. Fulgur should own PDF policy, including whether to collect bookmarks,
Outline hierarchy construction, Outline label budgets, and Krilla
serialization. The missing raikiri work is an output/observer contract, not a
request to move PDF semantics into raikiri.

## Filed raikiri proposals

All four issues were read back after creation and are open in
`mitsuru/raikiri`:

- [#190: expose neutral page fragments to render consumers](https://github.com/mitsuru/raikiri/issues/190)
- [#191: add generic resolved consumer-property observer](https://github.com/mitsuru/raikiri/issues/191)
- [#192: implement render completion for page consumers](https://github.com/mitsuru/raikiri/issues/192)
- [#193: define consumer resource and font handoff](https://github.com/mitsuru/raikiri/issues/193)
