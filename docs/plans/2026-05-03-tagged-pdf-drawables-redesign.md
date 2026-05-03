# Tagged PDF Semantic Layer — Drawables Redesign Δ

Issue: `fulgur-izp.3`
Supersedes the "Pageable / Canvas wrapper / TagCollector" portion of
`docs/plans/2026-04-22-tagged-pdf-krilla-api-design.md` (sections
"Existing Fulgur Pipeline", "Draw-Time Collector", "DOM / Pageable
Connection"). The Krilla API analysis and the PDF/UA-first product
direction in the original memo remain valid.

## Why this Δ

`fulgur-izp.1` was written when the render pipeline still walked a
`Box<dyn Pageable>` tree built by `convert.rs`. Phase 4 (PR 8i,
fulgur-9t3z) replaced that tree with `convert::dom_to_drawables`
producing a flat node-keyed `Drawables` struct that
`render::render_v2` consumes by walking `PaginationGeometryTable`
per page. There is no Pageable trait, no `Canvas`-attached
collector chain to mirror, and no central place to wrap a
"tagging wrapper" around an inner pageable. The semantic layer
must follow the new shape:

- **Convert side**: write per-NodeId entries into a new
  `Drawables.semantics` map at the same DOM walk that already
  populates `block_styles`, `paragraphs`, etc.
- **Render side (later issue)**: `render_v2` looks up
  `drawables.semantics.get(&node_id)` per fragment and calls
  `surface.start_tagged(...) / end_tagged()` around the existing
  per-type draw functions. No `Canvas.tag_collector` field, no
  trait wrappers.

This issue delivers the convert side only. Render integration
ships in `fulgur-izp.4` (basic block/inline tagging) and
`fulgur-izp.5` (TagTree assembly + `set_tag_tree`).

## Scope of fulgur-izp.3

In:

1. New module `crates/fulgur/src/tagging.rs` exposing a fulgur-internal
   `PdfTag` enum that mirrors the subset of Krilla `Tag` variants we
   intend to support.
2. New `Drawables` field `semantics: BTreeMap<NodeId, SemanticEntry>`
   carrying `(tag, parent: Option<NodeId>)`.
3. A standalone `record_semantics_pass` invoked from
   `dom_to_drawables` that classifies elements and inserts
   `SemanticEntry` records for recognised tags.
4. `Drawables::is_empty()` updated to include the new map (mirrors
   the existing `bookmark_anchors` precedent).
5. Convert-layer unit tests asserting the map content for an HTML
   fixture spanning every recognised element class.

Out (handled by later issues):

- Krilla `start_tagged` / `end_tagged` calls.
- TagTree construction and `Document::set_tag_tree`.
- Config / Engine / CLI flags (`fulgur-izp.2`).
- Img alt text propagation as `Tag::Figure(Some(alt))`
  (`fulgur-izp.6`).
- List, table, link structure tags (`fulgur-izp.7..9`).
- PDF/UA validator and metadata (`fulgur-izp.10`).

The acceptance criteria reduce to:

- `dom_to_drawables` emits a recognisable `SemanticEntry` for h1-h6,
  p, span, div, img, ul, ol, li, table-family elements.
- Tagging-disabled draw output is byte-identical (we add data only;
  `render_v2` does not read it yet).
- A `convert` test asserts the recorded entries for a fixture HTML.

## Data shape

```rust
// crates/fulgur/src/tagging.rs

/// Subset of Krilla tag variants we intend to map HTML semantics to.
/// Carried per-NodeId in `Drawables.semantics`. Render-side translation
/// to `krilla::tagging::Tag` happens in fulgur-izp.5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfTag {
    /// `<p>` and other generic paragraph-like blocks.
    P,
    /// `<h1>`..`<h6>` — `level` is `1..=6`.
    H { level: u8 },
    /// Generic block container (`<div>`, `<section>`, `<article>`,
    /// `<main>`, `<aside>`, `<nav>`, `<header>`, `<footer>`).
    /// Renders as `Tag::NonStruct` in the simplest mapping; a future
    /// pass may promote some of these to `Sect` / `Art` etc.
    Div,
    /// `<span>` — inline grouping. Maps to `Tag::Span`.
    Span,
    /// `<img>` — alt text is read from the DOM at render time
    /// (fulgur-izp.6); not stored here.
    Figure,
    /// `<ul>` / `<ol>` — list container. Numbering style stays at the
    /// DOM until fulgur-izp.7 wires `ListNumbering`.
    L,
    /// `<li>` — list item.
    Li,
    /// `<table>`.
    Table,
    /// `<thead>` / `<tbody>` / `<tfoot>` — table row group. Optional
    /// in PDF/UA but useful for downstream consumers.
    TRowGroup,
    /// `<tr>`.
    Tr,
    /// `<th>`. Header scope is read at render time (fulgur-izp.8).
    Th,
    /// `<td>`.
    Td,
}

/// Per-NodeId semantic record. `parent` lets a render-time pass
/// reconstruct the StructTree without re-walking the DOM. `None`
/// marks roots of the recorded subtree (e.g. `<body>` itself when
/// it gains a tag, or any element whose ancestors carry no
/// recognised tag).
#[derive(Debug, Clone)]
pub struct SemanticEntry {
    pub tag: PdfTag,
    pub parent: Option<NodeId>,
}
```

`Drawables` gains:

```rust
pub semantics: BTreeMap<NodeId, SemanticEntry>,
```

`Drawables::is_empty()` adds `&& self.semantics.is_empty()` (parallel
to `bookmark_anchors`).

## Convert-side wiring

Semantic classification runs as a separate top-down DOM pass invoked
from `dom_to_drawables` after the main `convert_node` walk:

```rust
// crates/fulgur/src/convert/mod.rs

pub fn dom_to_drawables(...) -> Drawables {
    ...
    convert_node(doc.deref(), root.id, ctx, 0, &mut drawables);
    drawables.bookmark_anchors = extract_bookmark_anchors(...);
    drawables.body_offset_pt = extract_body_offset_pt(doc);
    drawables.root_id = Some(root.id);
    drawables.body_id = find_body_id_in_dom(doc);
    record_semantics_pass(doc, &mut drawables);   // ← new
    drawables
}
```

`record_semantics_pass`:

1. Start from `drawables.body_id`. `<head>` and its descendants are
   intentionally skipped — none participate in the StructTree, and
   confining the walk to body keeps later expansions of
   `classify_element` (e.g. a future promotion of `<header>` /
   `<footer>` to dedicated tags) from accidentally classifying
   `<head>`'s `<title>` / `<style>` etc.
2. For each element node, call `tagging::classify_element` on the
   local name. Skip when it returns `None`.
3. Walk ancestors via `node.parent` until an already-recorded ancestor
   is found; that NodeId becomes `parent`. The walk is top-down, so
   classified ancestors are guaranteed to be present.
4. Insert the `SemanticEntry` into `out.semantics`.

The classifier intentionally stays element-local. `<a>` is
deliberately deferred — inline link spans interact with
`paragraph::LinkSpan` and must be coordinated with `link_spans` in
`fulgur-izp.9`.

### Why a standalone pass instead of a `convert_node` post-step

The first iteration of this design hooked classification into
`convert_node` directly. That broke for table rows and sections
because `convert::table::convert_table` walks `<thead>` / `<tbody>`
through a custom `collect_table_cells` helper that does **not**
recurse via `convert_node` — only the cell elements re-enter the
main dispatch. Any tag classification anchored to `convert_node`
silently misses the entire row-group / row layer.

A standalone DOM walk decouples semantic classification from each
per-type converter's child-traversal contract. It also keeps the
draw-payload converters from having to know about the StructTree at
all, which mirrors the way bookmark anchors are projected into
Drawables from a separate snapshot rather than threaded through the
draw walk.

## Why parent: Option<NodeId> rather than a separate tree

The advisor reviewed three options:

- **A: `parent` per entry, flat map.** Matches every other entry
  shape in `Drawables`; lets render assemble the tree on demand by
  `O(N)` traversal. Chosen.
- **B: Separate `Vec<TagTreeNode>` with explicit children.** Diverges
  from the flat per-NodeId pattern, requires a second invariant
  ("entries match the map") to maintain.
- **C: No parent in convert, re-walk the DOM at render time.** Forces
  the render path to keep the `BaseDocument` alive past `convert` and
  re-do classification, doubling the source of truth.

A is the lowest-friction fit for the existing Drawables shape.

## Tests

`crates/fulgur/src/convert/mod.rs` already contains a
`dom_to_drawables_preserves_bookmark_anchors_for_outline` test. Add
siblings:

- `dom_to_drawables_records_semantic_entries_for_block_elements`:
  fixture HTML containing `<h1>..</h1><p>..</p><div><img src=...></div>`
  asserts `semantics` map contents and `parent` chain.
- `dom_to_drawables_records_semantic_entries_for_lists`: `<ul><li>..</li></ul>`
  asserts `L` parent of `Li`.
- `dom_to_drawables_records_semantic_entries_for_tables`: `<table><thead><tr><th>..`
  full table fixture asserts the row-group / row / cell parent chain.
- `dom_to_drawables_skips_unrecognised_elements`: `<custom-tag>` and
  `<script>` produce no semantic entry.

Verification beyond unit tests:

- `cargo test -p fulgur --lib` continues to pass.
- VRT (`crates/fulgur-vrt`) byte-identical: render path is
  untouched, so all goldens hold without regeneration.

## Files touched

- `crates/fulgur/src/tagging.rs` — new.
- `crates/fulgur/src/lib.rs` — `pub mod tagging;`.
- `crates/fulgur/src/drawables.rs` — `semantics` field, `is_empty()`.
- `crates/fulgur/src/convert/mod.rs` — `record_semantics`, test sibs.

No other module changes; render, paginate, link, paragraph, image,
gcpm all stay byte-equivalent.
