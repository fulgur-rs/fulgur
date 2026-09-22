# Implement page emission and render completion for consumers

## Context

At revision `58833283a9eb7fa8c3885a194d364740dce0244f`, the public
`raikiri::render_streaming` entry point always returns
`RenderError::Unimplemented { feature: "render_streaming", .. }`. A recording
sink receives zero `accept_page` calls and no `finish_render` call. The public
`RenderSink` and `RenderSummary` shapes exist, but no production page-stream
driver connects them to the pagination result.

Reproduction:

```bash
CARGO_HOME=/tmp/fulgur-raikiri-cargo \
  cargo run --manifest-path spikes/raikiri-consumer/Cargo.toml --locked
```

## Proposed change

Implement the page-stream driver behind the existing `render_streaming` API:

1. parse/cascade/layout/paginate through the raikiri-owned pipeline;
2. emit each committed neutral `PageFragment` exactly once through
   `RenderSink::accept_page`;
3. invoke `finish_render(RenderSummary)` exactly once after successful page
   emission;
4. preserve the existing abort/error contract, including no completion callback
   after an aborted render;
5. make `RenderSummary.total_pages` agree with the emitted page count and retain
   target-resolution diagnostics for consumers.

The driver must remain renderer-neutral. PDF serialization, Outline creation,
image encoding, and Krilla integration stay outside raikiri.

## Acceptance criteria

- Basic and forced-break documents emit the expected number of pages to a test
  sink.
- `finish_render` receives the final page count and target summary.
- Sink I/O failure stops the render and returns `RenderError::Sink`.
- Abort behavior does not call `finish_render`.
- Existing single-page PNG APIs continue to pass their tests.
