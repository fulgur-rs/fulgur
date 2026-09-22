# Define the consumer resource and font handoff

## Context

The fulgur probe can drive raikiri with `FontContext::new()`, empty extra
stylesheets, no base URL, no network provider, and a no-op replaced-element
resolver. That is enough to characterize page counting but not enough to
replace fulgur's layout engine: fulgur supplies bundled font bytes, CSS and
`@import` sources, a base path, network policy, and image intrinsic-size
resolution.

The current public pieces are split between `ParseOptions`, `FontContext`,
`layout_pages_with_resolver`, and separate font-face helpers. There is no single
consumer-facing handoff that lets a renderer provide its resource policy and
retain deterministic font behavior across parse, cascade, layout, and page
emission.

## Proposed surface

Define a renderer-neutral input/resource configuration that can carry:

- stylesheet sources and base URL/path;
- network provider and policy;
- font bytes or a font-context builder/registration hook;
- replaced-element intrinsic resolver;
- resource and aggregate-size limits;
- deterministic fallback behavior and warnings.

The configuration should be usable by both batch/page-plan and streaming paths,
avoid requiring consumers to depend on raikiri implementation crates, and keep
PDF or image formats out of the core interface.

## Acceptance criteria

- A consumer can provide bundled fonts and obtain the same font context used by
  layout and paint.
- A consumer can provide one replaced-element resolver for both layout and the
  emitted page fragments.
- Base URL, stylesheet import, and network policy are applied consistently.
- Resource-limit and fallback diagnostics reach `RenderSummary`.
- A no-network, explicitly bundled-input test proves deterministic behavior.
