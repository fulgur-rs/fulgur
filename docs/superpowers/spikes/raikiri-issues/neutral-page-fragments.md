# Neutral page fragments for render consumers

## Context

Observed at raikiri revision `58833283a9eb7fa8c3885a194d364740dce0244f`.

Reproduction from the fulgur spike checkout:

```bash
CARGO_HOME=/tmp/fulgur-raikiri-cargo \
  cargo run --manifest-path spikes/raikiri-consumer/Cargo.toml --locked
```

`raikiri_dom::layout_pages` successfully returns ordered `PageSlice` values
for a short document and a forced page break. `PageSlice` only contains
`page_index`, `content_origin_y`, and `page_name`. The public
`raikiri_traits::PageFragment` is still an empty placeholder, so a consumer
cannot obtain per-node page fragments, paint order, line ranges, or split/repeat
state without reaching into the DOM internals or using the dogfooding
`PageScene`/`PageDrawables` surface.

## Proposed surface

Promote `PageFragment` into a neutral immutable page snapshot emitted by the
pagination driver. It should provide:

- resolved page metadata, including page box, margins, orientation, name, and
  page index;
- ordered fragment/item records with opaque `NodeId`, rect, and fragment kind;
- text line/glyph placement or another neutral text payload that a renderer can
  consume without depending on Parley;
- split/repeat state and source-node identity;
- page-local consumer events needed for links, annotations, and other sinks.

The existing `RenderSink::accept_page(PageFragment)` should receive this
snapshot once per committed page. The public contract must not expose Taffy,
Parley, `PageScene`, `PageDrawables`, Krilla, or PDF-specific types.

## Acceptance criteria

- Basic one-page and forced-break documents emit non-empty page fragments.
- Each emitted fragment carries stable node identity and page-local geometry.
- A long text block exposes enough line/fragment information for a consumer to
  render continuation pages without re-running pagination.
- The API has a unit test proving deterministic item order.
- Existing `html_to_png` and current single-page callers remain source-compatible.
