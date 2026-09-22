# Generic resolved consumer-property observer

## Context

Fulgur needs to collect consumer-owned semantics such as `bookmark-level` and
`bookmark-label`, but those semantics should not become PDF concepts inside
raikiri. At revision `58833283a9eb7fa8c3885a194d364740dce0244f`, raikiri has no
public registration or notification path for a consumer-defined resolved
property. The existing GCPM directives cover raikiri-owned counter, string,
running-element, and target state, not consumer extensions.

The consumer needs values after cascade and generic content resolution, while
page destination geometry is only known after pagination.

## Proposed surface

Add a generic consumer-property registration and observer surface at the
render/layout boundary:

- registration identifies a property name and a neutral value grammar;
- the resolved-property event contains opaque node identity, parent/source
  order, property name, and a neutral owned value;
- a separate fragment event carries page index and geometry so the consumer can
  join a node property with its first page fragment;
- observer errors propagate through the existing render error path;
- registration is optional and absent observers have no output or traversal
  cost beyond the normal layout path.

Fulgur can register and interpret `bookmark-level` and `bookmark-label` and
construct its own Outline tree. Raikiri only parses/cascades/resolves the
neutral value and never creates an Outline or any other PDF object.

## Acceptance criteria

- A test consumer registers an integer and a resolved-text property and receives
  deterministic events in document order.
- The event exposes no `raikiri_style::PropertyValue`, Taffy, Parley, Krilla, or
  PDF-specific type.
- The event can be correlated with a later page fragment by opaque `NodeId`.
- A failing observer returns a structured render error and does not silently
  drop the failure.
- The default path with no consumer registration remains behaviorally unchanged.
