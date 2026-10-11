# fulgur-smlr: GCPM selector cascade — specificity-aware, document-order-consistent resolution

## Problem

`parse_gcpm` and its consumers (`RunningElementPass`, `BookmarkPass`) do not
implement real CSS cascade semantics when multiple mapping rules match the
same element:

- `RunningElementPass::find_running_name` (`blitz_adapter.rs`) uses
  `Vec::find` — the **first** matching mapping in source order wins.
- `BookmarkPass::resolve_node` (`blitz_adapter.rs`) overlays fields forward —
  the **last** matching mapping wins (documented as intentional "last-match
  cascade").

These two passes pick winners in opposite directions, and neither considers
selector specificity (`ParsedSelector` is `Tag` / `Class` / `Id`, no
compounds — that gap is tracked separately by fulgur-j63u / fulgur-nmo).

A second, independent gap surfaced during design review: the mapping `Vec`s
these passes consume are **not in true document order**. Mappings are
concatenated as `UA → AssetBundle CSS → <link>/@import CSS → inline
<style>`, a fixed pipeline order that does not reflect where these
stylesheets actually land in the DOM. In particular, `InjectCssPass` appends
the AssetBundle-derived `<style>` as the **last** child of `<head>` — after
any author `<link>`/`<style>` — so for ordinary CSS properties AssetBundle
CSS already wins ties via Stylo's real cascade, while GCPM's own resolution
treats it as applied *first* (lowest priority) or *last* depending on which
of the two contradictory passes is asked. A specificity fix layered on top
of the current concatenation order would not resolve this — it would just
change which direction the inconsistency points.

## Non-goals

- Full CSS specificity `(a, b, c, d)` tuples, `!important`, cascade layers,
  compound/combinator selectors — out of scope while `ParsedSelector` only
  represents a single simple selector (fulgur-j63u / fulgur-nmo track that
  separately).
- Integrating GCPM resolution with Blitz/Stylo's real cascade engine
  (custom-property-based cascade delegation). Investigated as "Approach B"
  during design; rejected for this issue because GCPM properties
  (`bookmark-level`, `string-set`, `position: running()`) have no Stylo
  representation to cascade over, so the only viable path is re-encoding
  every GCPM declaration as a CSS custom property and reading back computed
  values post-cascade — a much larger redesign (also requires solving
  custom-property inheritance semantics that today's flat, non-inheriting
  selector matching does not have). Worth a future design doc of its own if
  `!important`/cascade-layer support is ever required; not needed to fix the
  bug this issue describes.
- `StringSetPass` and `CounterPass`. Both were found to have related but
  differently-shaped cascade gaps during this investigation (see
  "Related, deferred" below) — left for follow-up issues.

## Part A — document-order-correct mapping extraction

The mapping `Vec`s already double as the source-order signal once correctly
built — no new `source_index` field is needed on any mapping struct. The fix
is entirely in *how* the `Vec`s are assembled.

### Current state

- `FulgurNetProvider::Inner::gcpm_contexts: Vec<GcpmContext>` (`net.rs`)
  accumulates one `GcpmContext` per fetched `<link>`/`@import` stylesheet,
  discarding the triggering node's id.
- `walk_for_inline_styles` (`blitz_adapter.rs`) walks the DOM in order and
  `extend_from`s each `<style>` block's context into one aggregate
  immediately, also discarding per-node identity.
- `engine.rs`'s `layout_to_drawables` concatenates
  `AssetBundle ctx → link ctx → inline ctx` via two `extend_from` calls, a
  fixed order unrelated to real DOM position.

### Change

1. `net.rs`: `gcpm_contexts` becomes `Vec<(usize, GcpmContext)>`, paired with
   the triggering node id the same way `column_css_texts: Vec<(usize,
   String)>` already is (`fulgur-s5ro` precedent). The node id is already
   available inside the `fetch()` callback that currently pushes
   `column_css_texts`; capture it for the GCPM push too.
2. `parse_html_with_local_resources` returns `Vec<(usize, GcpmContext)>` for
   link-derived contexts instead of one flattened `GcpmContext`.
3. `walk_for_inline_styles` accumulates `Vec<(usize, GcpmContext)>` instead
   of `extend_from`-ing immediately.
4. A single unifying DOM walk — structurally identical to
   `walk_for_column_styles` (`blitz_adapter.rs:1016`), and worth merging
   into that same traversal rather than adding a third one (see
   "Performance" below) — runs **after** the `apply_passes` call that
   includes the AssetBundle `InjectCssPass` and **before** the first mapping
   consumer (`RunningElementPass::new`, `engine.rs:310`). It visits
   `<link rel=stylesheet>` / `<style>` nodes in true DOM order (including
   the now-materialized AssetBundle `<style>`, which naturally lands last
   since `InjectCssPass` appends it as `<head>`'s final child) and
   `extend_from`s each node's pre-parsed `GcpmContext`, looked up by node id.
5. UA CSS is `extend_from`'d in front of this result, as today — it has no
   DOM anchor and stays the unconditional floor.

Verified pass ordering (`engine.rs`, `layout_to_drawables`): `apply_passes`
(→ `InjectCssPass` for AssetBundle CSS, → `CaptionRestructurePass`, neither
of which touches `<head>`/`<link>`/`<style>` nodes after the first) runs
before `RunningElementPass`, `StringSetPass`, `CounterPass`, and
`BookmarkPass` all consume the mapping `Vec`s. The two *later*
`InjectCssPass` calls (counter_css, static_css) inject already-resolved
`content` values keyed by `[data-fulgur-cid]` — they don't add new
`position: running()` / `bookmark-level` declarations, so they don't need to
participate in this ordering walk.

### Performance

Net new cost is small relative to the render pipeline's existing DOM-walk
count (`RunningElementPass`, `BookmarkPass`, `StringSetPass`, `CounterPass`'s
two phases, `walk_for_column_styles`, `walk_for_inline_styles`,
`CaptionRestructurePass`, caption collection — already 6-8 full traversals
per render). Recommend folding the new traversal into the existing
`walk_for_column_styles` call (same node-matching predicate, same
document-order requirement) rather than adding a fourth/fifth walk — zero
net new traversals. The per-match lookup (`BTreeMap<usize, GcpmContext>` or
similar, keyed by node id) mirrors `extract_column_style_table`'s existing
`BTreeMap<usize, &str>` pattern; key count is bounded by the number of
`<link>`/`<style>` tags in the document (typically single digits), so lookup
and `extend_from` cost is negligible next to CSS parsing itself. No change
to peak-memory profile (`project_memory_peak_topology` territory) — no new
large structures, just node-id tags on data already being built.

## Part B — specificity-aware tie-break

### Specificity model

```rust
/// CSS specificity for the flat (Tag | Class | Id) grammar `ParsedSelector`
/// currently supports. Each mapping's selector is a single simple selector
/// (no compounds — fulgur-j63u/nmo), so specificity collapses to one tier
/// instead of the full (id, class, type) triple real CSS uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SelectorSpecificity {
    Tag,
    Class,
    Id,
}

pub(crate) fn specificity(selector: &ParsedSelector) -> SelectorSpecificity {
    match selector {
        ParsedSelector::Tag(_) => SelectorSpecificity::Tag,
        ParsedSelector::Class(_) => SelectorSpecificity::Class,
        ParsedSelector::Id(_) => SelectorSpecificity::Id,
    }
}
```

Lives in `gcpm/mod.rs`, next to `ParsedSelector` — it's a pure property of
the selector, no DOM dependency.

### `RunningElementPass::find_running_name`

```rust
fn find_running_name(&self, elem: &blitz_dom::node::ElementData) -> Option<String> {
    self.mappings
        .iter()
        .enumerate()
        .filter(|(_, m)| selector_matches(&m.parsed, elem))
        .max_by_key(|(i, m)| (crate::gcpm::specificity(&m.parsed), *i))
        .map(|(_, m)| m.running_name.clone())
}
```

`(specificity, index)` tuple ordering picks highest specificity, breaking
ties toward the highest index — i.e. latest in (now-correct) document order,
matching real CSS's "higher specificity wins; equal specificity, later
declaration wins." Single pass, no buffering.

### `BookmarkPass::resolve_node`

`level` and `label` are independent CSS properties and must cascade
independently, so this keeps the existing single-pass field overlay instead
of collecting all matches:

```rust
let mut level: Option<BookmarkLevel> = None;
let mut level_specificity: Option<gcpm::SelectorSpecificity> = None;
let mut label: Option<Vec<ContentItem>> = None;
let mut label_specificity: Option<gcpm::SelectorSpecificity> = None;
let mut any_match = false;

for mapping in &self.mappings {
    if !selector_matches(&mapping.selector, elem) {
        continue;
    }
    any_match = true;
    let spec = gcpm::specificity(&mapping.selector);
    if let Some(l) = &mapping.level
        && level_specificity.is_none_or(|cur| spec >= cur)
    {
        level = Some(l.clone());
        level_specificity = Some(spec);
    }
    if let Some(lbl) = &mapping.label
        && label_specificity.is_none_or(|cur| spec >= cur)
    {
        label = Some(lbl.clone());
        label_specificity = Some(spec);
    }
}
```

The `>=` guard makes forward iteration alone sufficient: equal-specificity
matches always overwrite (preserving last-wins on ties), and a
lower-specificity match appearing later never overwrites an earlier
higher-specificity winner.

### UA interaction

UA CSS (`h1`-`h6`) is `Tag`-specificity — the lowest tier — so any author
`Class`/`Id` rule outranks it regardless of position, and an author `Tag`
rule (e.g. another `h1` rule) beats it via the same-tier "last wins" path
because UA mappings are always prepended first. This generalizes the
existing documented intent ("author rules override UA via last-match
cascade") rather than replacing it.

## Testing plan

- `gcpm::specificity` unit tests: `Tag < Class < Id` ordering.
- `RunningElementPass` unit tests: mappings constructed out of specificity
  order (e.g. `Class` mapping before `Id` mapping in the `Vec`) — assert the
  `Id` mapping wins despite being later; equal-specificity tie — assert the
  later mapping wins.
- `BookmarkPass` unit tests: `level` and `label` sourced from different
  mappings with different specificities — assert independent per-field
  resolution.
- Extraction-order unit tests (Part A): `<style>` before `<link>` in markup
  with equal-specificity competing rules — assert `<link>` (later in
  document order) wins; reverse markup order — assert the reverse. A
  dedicated test locking down the non-obvious AssetBundle-wins-ties case
  (AssetBundle CSS vs. `<link>`/inline `<style>`, equal specificity) since it
  contradicts naive intuition and is easy to regress silently.
- Regression: existing `@import` ordering test
  (`parse_html_with_local_resources_orders_imports_before_parent`) and every
  `gcpm_snapshot` test (`gcpm_running_element_via_inline_style`,
  `gcpm_element_policy_first/last`, etc.) must stay green — these are the
  fulgur-da3u tripwires for the AssetBundle/inline-`<style>` DOM-presence
  asymmetry that Part A must not disturb.
- Per CLAUDE.md's coverage-scope rule: a `render_smoke`-style end-to-end
  test (`Engine::builder().build().render(html)`) with competing
  bookmark-level rules of different specificity, asserting the PDF outline
  reflects the higher-specificity rule — this path is only reachable
  through the full `Engine::render` pipeline, not through the DOM-pass unit
  tests alone.

No new fallible paths are introduced — this is a pure selection-algorithm
change over already-infallible data.

## Related, deferred (not in this issue's scope)

- **fulgur-j63u / fulgur-nmo**: compound/descendant selector parsing. Once
  landed, `specificity()` needs to sum tiers across a compound instead of
  returning a single enum variant — noted as a compatible future extension,
  not a redesign.
- **`StringSetPass`**: applies every matching mapping (not a single
  winner), so same-named `string-set` assignments from competing selectors
  resolve by source order only, no specificity. Different shape of bug from
  the running/bookmark contradiction this issue fixes.
- **`CounterPass`**: `counter-increment`/`counter-reset` ops from every
  matching mapping are applied cumulatively rather than cascade-resolved to
  one winning declaration. Different shape again (accumulation vs.
  single-winner selection) — worth its own investigation.
- **Approach B (Stylo cascade integration)**: see "Non-goals" above.
