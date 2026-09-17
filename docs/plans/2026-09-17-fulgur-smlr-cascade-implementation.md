# fulgur-smlr: GCPM Selector Cascade Implementation Plan

**Goal:** Make `RunningElementPass` and `BookmarkPass` resolve competing GCPM
mapping rules with a single, specificity-aware, document-order-consistent
algorithm instead of two contradictory ad-hoc rules (first-match vs.
last-match), across all three mapping sources (AssetBundle CSS, `<link>`
/`@import` CSS, inline `<style>`).

**Architecture:** Two independent layers, matching
`docs/plans/2026-09-17-fulgur-smlr-gcpm-cascade-design.md`:

- **Part A** — `running_mappings`/`bookmark_mappings` are rebuilt in true DOM
  document order (not today's fixed `AssetBundle → link → inline`
  concatenation) via a single post-`InjectCssPass` DOM walk, keyed by node
  id. `<link>`/`@import` mappings are sourced from a new
  node-id-tagged buffer in `FulgurNetProvider` instead of the flattened one
  `drain_gcpm_contexts()` returns today.
- **Part B** — `ParsedSelector` gets a `Tag < Class < Id` specificity tier;
  both passes pick the winning mapping by `(specificity, document_order)`
  instead of first/last-match.

**Tech Stack:** Rust, `blitz-dom`/`blitz-html` (DOM), `cssparser` (GCPM's own
mini-parser in `gcpm::parser`), existing `fulgur` test harness
(`cargo test -p fulgur --lib`).

**Reference implementation:** a throwaway perf spike already validated this
design end-to-end (worktree `worktree-smlr-cascade-spike`, kept on disk —
diff via `git -C ../smlr-cascade-spike diff 6d4d534a`). All 2191 existing
lib tests passed against it, and a 4-scenario timing comparison against
`main` showed a 1–4% wall-clock delta (within run-to-run noise). The spike
took two shortcuts this plan removes: (1) it double-parses `<link>` CSS via
`link_column_css` instead of tagging `net.rs`'s existing per-file
`parse_gcpm` result with a node id, so nested `@import` GCPM constructs were
silently dropped; (2) it added a brand-new DOM walk instead of merging into
the existing `walk_for_column_styles`. Task 4 and Task 6 below are the real
versions of those two shortcuts.

---

## Task 1: Specificity model

**Files:**

- Modify: `crates/fulgur/src/gcpm/mod.rs` (after the `ParsedSelector` enum,
  around line 22)
- Test: same file, `#[cfg(test)] mod tests` block

**Step 1: Add the type and function**

```rust
/// CSS specificity for the flat (Tag | Class | Id) grammar `ParsedSelector`
/// currently supports. Each mapping's selector is a single simple selector
/// (no compounds — fulgur-j63u/nmo track that separately), so specificity
/// collapses to one tier instead of the full `(id, class, type)` triple
/// real CSS specificity uses. Declaration order gives `Tag < Class < Id`,
/// matching the CSS specification's precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SelectorSpecificity {
    Tag,
    Class,
    Id,
}

/// The specificity tier of a single simple selector (see
/// [`SelectorSpecificity`]).
pub(crate) fn specificity(selector: &ParsedSelector) -> SelectorSpecificity {
    match selector {
        ParsedSelector::Tag(_) => SelectorSpecificity::Tag,
        ParsedSelector::Class(_) => SelectorSpecificity::Class,
        ParsedSelector::Id(_) => SelectorSpecificity::Id,
    }
}
```

**Step 2: Write the test**

```rust
#[test]
fn specificity_orders_tag_below_class_below_id() {
    assert!(specificity(&ParsedSelector::Tag("h1".into())) < specificity(&ParsedSelector::Class("x".into())));
    assert!(specificity(&ParsedSelector::Class("x".into())) < specificity(&ParsedSelector::Id("y".into())));
}
```

**Step 3: Run it**

Run: `cargo test -p fulgur --lib gcpm::tests::specificity_orders_tag_below_class_below_id`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/fulgur/src/gcpm/mod.rs
git commit -m "feat(gcpm): add selector specificity tiers"
```

---

## Task 2: `RunningElementPass` specificity + document-order tie-break

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs:2009` (`find_running_name`)
- Test: same file, `mod tests` (search for `RunningElementPass` test helpers
  around line 4000)

**Step 1: Write the failing test**

Add near the existing `RunningElementPass` tests:

```rust
#[test]
fn find_running_name_prefers_higher_specificity_regardless_of_position() {
    // Class mapping appears first in the Vec (would win under the old
    // first-match rule); Id mapping is later and must win instead.
    let mappings = vec![
        RunningMapping {
            parsed: ParsedSelector::Class("hdr".to_string()),
            running_name: "fromClass".to_string(),
        },
        RunningMapping {
            parsed: ParsedSelector::Id("main-hdr".to_string()),
            running_name: "fromId".to_string(),
        },
    ];
    let pass = RunningElementPass::new(mappings);
    let doc = parse(
        r#"<div id="main-hdr" class="hdr"></div>"#,
        400.0,
        &[],
    );
    let elem = doc.root_element().children[0]; // adjust to the actual div node lookup used by existing tests in this module
    // ... (see existing RunningElementPass tests in this file for the
    // established helper to fetch an ElementData from `doc` by tag/id —
    // reuse that helper rather than hand-rolling node traversal here)
}
```

Look at the existing tests immediately above/below line 4000 in
`blitz_adapter.rs` for the established pattern this module uses to get a
`&blitz_dom::node::ElementData` for a specific node out of a parsed `doc` —
mirror that pattern exactly instead of the sketch above.

**Step 2: Run it, confirm it fails** (or doesn't compile) against the old
`.find()` implementation.

**Step 3: Implement**

```rust
/// Picks the winning `position: running(name)` mapping for `elem` by
/// specificity, then by document order (later wins ties) — see
/// fulgur-smlr. `self.mappings` is already in document order by the
/// time this runs (Task 7), so the enumerate index doubles as the
/// source-order key without needing a separate field on `RunningMapping`.
fn find_running_name(&self, elem: &blitz_dom::node::ElementData) -> Option<String> {
    self.mappings
        .iter()
        .enumerate()
        .filter(|(_, m)| selector_matches(&m.parsed, elem))
        .max_by_key(|(i, m)| (crate::gcpm::specificity(&m.parsed), *i))
        .map(|(_, m)| m.running_name.clone())
}
```

**Step 4: Run tests, confirm pass**

Run: `cargo test -p fulgur --lib running_element`
Expected: PASS, plus all pre-existing `RunningElementPass` tests still green
(they assert on *before* this task's document-order fix lands, so mapping
order in those tests is whatever the test constructs directly — check each
still describes the scenario it means to after this change; none should
need behavior changes since single-mapping-match tests are unaffected).

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "fix(gcpm): RunningElementPass picks winner by specificity, not first-match"
```

---

## Task 3: `BookmarkPass` per-field specificity cascade

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs:2827` (`resolve_node`)
- Test: same file, existing `BookmarkPass` test block

**Step 1: Write the failing test** — `level` from a low-specificity `Tag`
mapping, `label` from a high-specificity `Id` mapping on the same element;
assert both resolve independently (this is the case the old "whole-mapping
overlay" model could get wrong if a later, lower-specificity mapping only
set one field). Follow the existing `BookmarkPass` test setup pattern in
this file (search `BookmarkPass::new_with_snapshots` usage in `mod tests`).

**Step 2: Confirm it fails against the current implementation** — actually,
because the current implementation is a whole-field overlay in forward
order already, hand-pick the test scenario so old-vs-new code paths
genuinely disagree: put the higher-specificity mapping *earlier* in the
`Vec` and a lower-specificity, same-field-setting mapping *later* — old
code (pure forward overlay) lets the later, lower-specificity mapping win;
new code must not.

**Step 3: Implement**

```rust
// Overlay accumulator — iterate forward; each field cascades
// independently by specificity, then by document order (fulgur-smlr).
// `self.mappings` is already in document order (Task 7), and the `>=`
// guard makes a single forward pass sufficient: an equal-specificity
// match always overwrites (last-wins on ties), and a lower-specificity
// match appearing later never overwrites an earlier, higher-specificity
// winner.
let mut level: Option<BookmarkLevel> = None;
let mut level_specificity: Option<crate::gcpm::SelectorSpecificity> = None;
let mut label: Option<Vec<ContentItem>> = None;
let mut label_specificity: Option<crate::gcpm::SelectorSpecificity> = None;
let mut any_match = false;
for mapping in &self.mappings {
    if !selector_matches(&mapping.selector, elem) {
        continue;
    }
    any_match = true;
    let spec = crate::gcpm::specificity(&mapping.selector);
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
if !any_match {
    return;
}
```

**Step 4: Run tests**

Run: `cargo test -p fulgur --lib bookmark`
Expected: PASS, including every pre-existing `gcpm_snapshot`/bookmark test
(these are the fulgur-da3u tripwires — do not skip re-running the full
suite here even if the targeted filter passes).

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "fix(gcpm): BookmarkPass cascades level/label independently by specificity"
```

---

## Task 4: `net.rs` — real per-`<link>` node-id-tagged GCPM contexts

This is the task the spike shortcut around. Read
`crates/fulgur/src/net.rs:1-30` (module doc) and `:60-230` (`Inner`,
`FulgurNetProvider::fetch`) fully before editing — the `@import`
post-order-push comment at the bottom of `fetch()` explains an invariant
this task must not break (there's a regression test for it:
`parse_html_with_local_resources_orders_imports_before_parent` in
`blitz_adapter.rs`).

**The problem:** `Inner.gcpm_contexts: Vec<GcpmContext>` accumulates one
entry per *fetched file* (both top-level `<link>`s and their nested
`@import`s), in post-order (children before their importing parent), with
no node id attached. `fetch()` is called by Blitz for both top-level and
nested fetches through the same trait method — fulgur has no direct
signal for "is this fetch top-level." The fix tracks recursion depth in
`Inner` to detect it.

**Files:**

- Modify: `crates/fulgur/src/net.rs`
- Test: same file, `mod tests` (around line 240)

**Step 1: Add state to `Inner`**

```rust
#[derive(Default)]
struct Inner {
    gcpm_contexts: Vec<GcpmContext>,
    /// One entry per top-level `<link rel=stylesheet>` fetch (not each
    /// nested `@import`), keyed by that `<link>` element's node id, with
    /// its entire `@import` subtree already folded in (in the same
    /// child-before-parent post-order `gcpm_contexts` uses). Built
    /// alongside `gcpm_contexts` rather than replacing it — the flat
    /// `gcpm_contexts` path is still what
    /// `parse_html_with_local_resources` folds into the single merged
    /// `GcpmContext` consumed for `cleaned_css`/`margin_boxes`/
    /// `page_settings`/etc. This new field feeds only the document-order
    /// `running_mappings`/`bookmark_mappings` recompute (fulgur-smlr).
    gcpm_by_link_node: Vec<(usize, GcpmContext)>,
    /// Recursion depth through nested `@import` fetches — 0 outside any
    /// fetch, incremented on entry to `fetch()`, decremented on exit.
    /// A fetch that finds depth 0 on entry is a top-level `<link>` fetch;
    /// this is the only way to distinguish that from a nested `@import`
    /// fetch, since Blitz calls `NetProvider::fetch` identically for both.
    import_depth: usize,
    pending_resources: Vec<Resource>,
    column_css_texts: Vec<(usize, String)>,
}
```

**Step 2: Add the drain method**

Next to `drain_gcpm_contexts` (around line 97):

```rust
/// Take the `(node_id, GcpmContext)` pairs accumulated per top-level
/// `<link>` fetch (fulgur-smlr). Unlike [`Self::drain_gcpm_contexts`],
/// each entry here already has its `@import` subtree folded in and is
/// tagged with the `<link>` element's own node id.
pub fn drain_gcpm_by_link_node(&self) -> Vec<(usize, GcpmContext)> {
    let mut inner = self.inner.lock().unwrap();
    std::mem::take(&mut inner.gcpm_by_link_node)
}
```

**Step 3: Instrument `fetch()`**

Locate the existing callback construction and the post-`handler.bytes`
`gcpm_to_push` block (around lines 168–235). Replace that whole region
with:

```rust
let is_top_level_import_root = {
    let mut inner = self.inner.lock().unwrap();
    let top = inner.import_depth == 0;
    inner.import_depth += 1;
    top
};
let gcpm_start = self.inner.lock().unwrap().gcpm_contexts.len();

// Set by the callback below when Blitz reports this fetch's own
// `Resource::Css(node_id, _)` — read back after `handler.bytes`
// returns, the same way `column_css_text`/`raw_text_fallback` are
// captured by the closure and consumed after the call.
let node_id_slot: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

let inner = self.inner.clone();
let node_id_slot_cb = node_id_slot.clone();
let callback: SharedCallback<Resource> = Arc::new(
    move |_doc_id: usize, result: Result<Resource, Option<String>>| {
        if let Ok(res) = result {
            let mut guard = inner.lock().unwrap();
            if let Resource::Css(node_id, _) = &res {
                *node_id_slot_cb.lock().unwrap() = Some(*node_id);
                if let Some(text) = column_css_text
                    .clone()
                    .or_else(|| raw_text_fallback.clone())
                {
                    guard.column_css_texts.push((*node_id, text));
                }
            }
            guard.pending_resources.push(res);
        }
    },
);

handler.bytes(doc_id, bytes_for_blitz, callback);

// Post-order push: the parent context goes into the buffer *after* its
// children (unchanged from before this task — see the `gcpm_contexts`
// consumers this must keep working for).
if let Some(gcpm) = gcpm_to_push {
    self.inner.lock().unwrap().gcpm_contexts.push(gcpm);
}

{
    let mut inner = self.inner.lock().unwrap();
    inner.import_depth -= 1;
    if is_top_level_import_root {
        let merged = inner.gcpm_contexts[gcpm_start..]
            .iter()
            .cloned()
            .fold(GcpmContext::default(), |mut acc, ctx| {
                acc.extend_from(ctx);
                acc
            });
        if let Some(node_id) = *node_id_slot.lock().unwrap() {
            inner.gcpm_by_link_node.push((node_id, merged));
        }
        // No `Resource::Css` callback fired (fetch failed, or wasn't
        // recognised as CSS) — nothing to attach a node id to; matches
        // today's behavior where a failed fetch contributes nothing.
    }
}
```

Note `gcpm_contexts` itself is **not** drained or truncated here — it
keeps accumulating exactly as it does today, so
`drain_gcpm_contexts()`/the flat merge path is untouched. `Mutex` /`Arc`
are already imported at the top of this file.

**Step 4: Write the test**

Add to `mod tests`, modeled on the existing `fetch_runs_gcpm_and_serves_cleaned_css`
test:

```rust
#[test]
fn gcpm_by_link_node_tags_top_level_fetch_with_its_node_id_and_folds_imports() {
    // parent.css: `@import "child.css"; .parent { bookmark-level: 1; bookmark-label: content(); }`
    // child.css: `.child { bookmark-level: 2; bookmark-label: content(); }`
    // Assert: drain_gcpm_by_link_node() returns exactly one entry, keyed
    // by the <link>'s node id (not child.css's, which has none), whose
    // bookmark_mappings contains BOTH the child and parent rules, child
    // first (post-order — matches the existing
    // `parse_html_with_local_resources_orders_imports_before_parent`
    // invariant in blitz_adapter.rs).
    // Also assert drain_gcpm_contexts() (the old flat path) is unaffected
    // — same two entries it already returned before this task.
}
```

Fill in file setup using the same `tempfile`/directory-writing pattern as
`fetch_runs_gcpm_and_serves_cleaned_css` just above it in this file.

**Step 5: Run**

Run: `cargo test -p fulgur --lib net::tests`
Expected: PASS, including the pre-existing `drain_gcpm_contexts` tests
unmodified.

**Step 6: Commit**

```bash
git add crates/fulgur/src/net.rs
git commit -m "feat(net): tag top-level <link> GCPM contexts with their node id"
```

---

## Task 5: Thread node-id-tagged link contexts through `parse_html_with_local_resources`

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs:235` (function signature +
  body, around line 300 where `drain_gcpm_contexts` is called today)
- Modify all 12 call sites (see below)

**Step 1: Change the return type and body**

At `crates/fulgur/src/blitz_adapter.rs:235`, change the signature from:

```rust
) -> (HtmlDocument, crate::gcpm::GcpmContext, Vec<(usize, String)>) {
```

to:

```rust
) -> (
    HtmlDocument,
    crate::gcpm::GcpmContext,
    Vec<(usize, String)>,
    Vec<(usize, crate::gcpm::GcpmContext)>,
) {
```

Around line 290 (where `gcpm.extend_from(ctx)` loops over
`net_provider.drain_gcpm_contexts()`), add, right after that loop:

```rust
let link_gcpm_by_node = net_provider.drain_gcpm_by_link_node();
```

and update the final tuple return to
`(doc, gcpm, column_css_texts, link_gcpm_by_node)`.

**Step 2: Update every call site**

Every call site destructures a 3-tuple today; add a 4th binding. Run:

```bash
grep -rn "parse_html_with_local_resources(" crates/fulgur/src/ --include=*.rs
```

At the time this plan was written that listed:

- `crates/fulgur/src/blitz_adapter.rs`: lines 3927, 6157, 6196, 6207, 6220,
  6237, 6250, 6266, 6428, 6469 — all test helpers; each destructures
  `let (doc, _, _column_css) = ...` or similar. Add a 4th `_` (or `_link_gcpm_by_node`
  if the specific test wants to assert on it — none currently need to).
- `crates/fulgur/src/engine.rs:203` — the main render path
  (`layout_to_drawables`); destructures `let (mut doc, link_gcpm, link_column_css) = ...`.
  Add a 4th binding `link_gcpm_by_node` — **this is the one call site that
  actually uses the new value**, in Task 7.
- `crates/fulgur/src/engine.rs:856`, `crates/fulgur/src/engine.rs:923` —
  `build_drawables_for_testing_no_gcpm` / `build_drawables_and_geometry_for_testing_no_gcpm`.
  These already ignore the GCPM context (`_link_gcpm`); add a 4th `_`.

Re-run the grep after editing to confirm no call site was missed (a missed
one is a compile error, not a silent bug — arity mismatch on a tuple
pattern fails to compile).

**Step 3: Build**

Run: `cargo build -p fulgur --lib`
Expected: compiles clean. Fix any missed call site the grep didn't catch.

**Step 4: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs crates/fulgur/src/engine.rs
git commit -m "refactor(blitz_adapter): thread node-id-tagged link GCPM contexts through parse_html_with_local_resources"
```

---

## Task 6: Document-order fold walk

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` — add two standalone
  walks (`collect_inline_gcpm_by_node` / `fold_gcpm_by_document_order`,
  see Steps 2-3). Earlier drafts of this plan said to extend
  `walk_for_column_styles` (line 1016) instead of adding a new
  traversal, matching the spike's known shortcut #2 in the plan header —
  that merge was not done in the actual implementation (Steps 2-3 below
  give standalone-walk code, which is what shipped) and is now tracked
  as a follow-up performance optimization rather than a Task 6
  requirement, since the spike already measured the unmerged cost as
  negligible (see the plan header's spike results).
- Test: same file, near the existing `extract_column_style_table` tests

**Step 1: Read the current `walk_for_column_styles` / `extract_column_style_table`**
(lines 659–1075) in full before editing — for context on the existing
node-visiting shape this task's new walks mirror (see Steps 2-3).

**Step 2: Add the inline-`<style>`-by-node collector**

Next to `extract_gcpm_from_inline_styles` (line 594), add:

```rust
/// Like [`extract_gcpm_from_inline_styles`], but keyed by the `<style>`
/// element's node id instead of flattened into one context — feeds the
/// document-order GCPM cascade fold (fulgur-smlr Part A).
fn collect_inline_gcpm_by_node(
    doc: &HtmlDocument,
) -> std::collections::BTreeMap<usize, crate::gcpm::GcpmContext> {
    let mut out = std::collections::BTreeMap::new();
    let root = doc.root_element();
    walk_for_inline_styles_by_node(doc, root.id, &mut out, 0);
    out
}

fn walk_for_inline_styles_by_node(
    doc: &HtmlDocument,
    node_id: usize,
    out: &mut std::collections::BTreeMap<usize, crate::gcpm::GcpmContext>,
    depth: usize,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if let Some(el) = node.element_data()
        && el.name.local.as_ref() == "style"
    {
        let mut css = String::new();
        for &child_id in &node.children {
            if let Some(child) = doc.get_node(child_id)
                && let blitz_dom::node::NodeData::Text(t) = &child.data
            {
                css.push_str(&t.content);
            }
        }
        if !css.is_empty() {
            out.insert(node_id, crate::gcpm::parser::parse_gcpm(&css));
        }
        return;
    }
    for &child_id in &node.children {
        walk_for_inline_styles_by_node(doc, child_id, out, depth + 1);
    }
}
```

(This duplicates `walk_for_inline_styles`'s shape rather than
parameterizing it — the two need different accumulator types, `String` vs.
`BTreeMap`, and this file already accepts that kind of near-duplicate walk
for `walk_for_column_styles` vs. this one. If reviewers push back, revisit
by extracting a shared "visit every `<style>` node with its text" iterator
both wrap.)

**Step 3: Add the fold + orchestrator functions**

```rust
/// Folds GCPM contexts from `<link>`/`<style>` nodes into `out` in true DOM
/// document order. Mirrors `walk_for_column_styles`'s node-matching shape.
fn fold_gcpm_by_document_order(
    doc: &HtmlDocument,
    node_id: usize,
    link_gcpm: &std::collections::BTreeMap<usize, crate::gcpm::GcpmContext>,
    style_gcpm: &std::collections::BTreeMap<usize, crate::gcpm::GcpmContext>,
    out: &mut crate::gcpm::GcpmContext,
    depth: usize,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };
    if let Some(el) = node.element_data()
        && el.name.local.as_ref() == "link"
        && el
            .attr(blitz_dom::LocalName::from("rel"))
            .is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|tok| tok.eq_ignore_ascii_case("stylesheet"))
            })
    {
        if let Some(ctx) = link_gcpm.get(&node_id) {
            out.extend_from(ctx.clone());
        }
        return;
    }
    if let Some(el) = node.element_data()
        && el.name.local.as_ref() == "style"
    {
        if let Some(ctx) = style_gcpm.get(&node_id) {
            out.extend_from(ctx.clone());
        }
        return;
    }
    for &child_id in &node.children {
        fold_gcpm_by_document_order(doc, child_id, link_gcpm, style_gcpm, out, depth + 1);
    }
}

/// Recomputes `running_mappings`/`bookmark_mappings` in true DOM document
/// order across AssetBundle CSS, `<link>`/`@import` CSS, and inline
/// `<style>` blocks (fulgur-smlr Part A). Must run after the pass that
/// injects AssetBundle's cleaned CSS as `<head>`'s last `<style>` child
/// (`InjectCssPass`), so that synthetic node is discoverable at its real
/// document position — its *text* is already GCPM-stripped by that point,
/// so `combined_css` (the pre-strip original) is re-parsed here and
/// attached to that node's id rather than relying on the walk to find
/// GCPM content in the injected node's own (cleaned) text.
///
/// `link_gcpm_by_node` comes from Task 5's
/// `parse_html_with_local_resources` return value — real per-`<link>`
/// contexts (with `@import` subtrees already folded in), not a re-parse.
pub(crate) fn document_ordered_gcpm_mappings(
    doc: &HtmlDocument,
    combined_css: &str,
    assetbundle_css_injected: bool,
    link_gcpm_by_node: &[(usize, crate::gcpm::GcpmContext)],
    ua_bookmark_mappings: Vec<crate::gcpm::bookmark::BookmarkMapping>,
) -> (
    Vec<crate::gcpm::RunningMapping>,
    Vec<crate::gcpm::bookmark::BookmarkMapping>,
) {
    let link_gcpm: std::collections::BTreeMap<usize, crate::gcpm::GcpmContext> =
        link_gcpm_by_node.iter().cloned().collect();
    let mut style_gcpm_by_node = collect_inline_gcpm_by_node(doc);
    if assetbundle_css_injected
        && let Some(head_id) = find_element_by_tag(doc, "head")
        && let Some(node) = doc.get_node(head_id)
        && let Some(&last_child) = node.children.last()
    {
        style_gcpm_by_node.insert(last_child, crate::gcpm::parser::parse_gcpm(combined_css));
    }

    let mut ordered = crate::gcpm::GcpmContext::default();
    let root_id = doc.root_element().id;
    fold_gcpm_by_document_order(doc, root_id, &link_gcpm, &style_gcpm_by_node, &mut ordered, 0);

    let mut bookmark_mappings = ua_bookmark_mappings;
    bookmark_mappings.extend(ordered.bookmark_mappings);
    (ordered.running_mappings, bookmark_mappings)
}
```

This differs from the spike in exactly one place: `link_gcpm_by_node`
is now a real parameter (from Task 5) instead of being derived by
re-parsing `link_column_css` text inside this function — no double-parse,
no dropped `@import` GCPM constructs.

**Step 4: Build**

Run: `cargo build -p fulgur --lib`
Expected: compiles (the function isn't called yet — Task 7 wires it up. It
will warn `unused` until then; that's expected and resolves in Task 7).

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(blitz_adapter): fold GCPM mappings in true document order"
```

---

## Task 7: Wire it into `engine.rs`

**Files:**

- Modify: `crates/fulgur/src/engine.rs:277-315` (the UA-prepend block and
  the `apply_passes`/`RunningElementPass::new` region)

**Step 1: Remove the old UA-prepend block**

Delete this block (originally around line 277-286):

```rust
// Prepend UA CSS bookmark mappings so author-CSS rules (appearing
// later in `bookmark_mappings`) override them via last-match
// cascade. Skipped when bookmarks are disabled to avoid unnecessary
// CSS parsing and DOM traversal.
if self.config.effective_bookmarks() {
    let ua_gcpm = crate::gcpm::parser::parse_gcpm(crate::gcpm::ua_css::FULGUR_UA_CSS);
    let mut combined_bookmarks = ua_gcpm.bookmark_mappings;
    combined_bookmarks.extend(gcpm.bookmark_mappings);
    gcpm.bookmark_mappings = combined_bookmarks;
}
```

It's superseded by Step 3 below, which does the equivalent prepend against
the freshly document-ordered `bookmark_mappings` instead of the old flat
ones.

**Step 2: Track whether AssetBundle CSS was injected**

Where `InjectCssPass` is conditionally pushed (originally around line
280-284):

```rust
let mut passes: Vec<Box<dyn crate::blitz_adapter::DomPass>> = Vec::new();

let assetbundle_css_injected = !css_to_inject.is_empty();
if assetbundle_css_injected {
    passes.push(Box::new(crate::blitz_adapter::InjectCssPass {
        css: css_to_inject,
    }));
}
```

(`css_to_inject` moves into `InjectCssPass` here, same as before — the new
`assetbundle_css_injected` bool is captured first so it survives the move.)

**Step 3: Recompute mappings after `apply_passes`, before `RunningElementPass::new`**

Insert right after the existing `crate::blitz_adapter::apply_passes(&mut doc, &passes, &ctx);`
line and before `let running_store = ...`:

```rust
// fulgur-smlr Part A: recompute running/bookmark mapping order in
// true DOM document order (AssetBundle CSS's injected <style> lands
// *last* in <head> via InjectCssPass above, not first — see
// docs/plans/2026-09-17-fulgur-smlr-gcpm-cascade-design.md). This
// must run after `apply_passes` (so the injected node exists) and
// before any mapping consumer below. Only running/bookmark mappings
// are recomputed — `gcpm`'s other fields (cleaned_css, margin_boxes,
// page_settings, counter/string-set mappings) keep today's flat
// concatenation order; that's a separate, differently-shaped gap
// (see the design doc's "Related, deferred" section).
let ua_bookmark_mappings = if self.config.effective_bookmarks() {
    crate::gcpm::parser::parse_gcpm(crate::gcpm::ua_css::FULGUR_UA_CSS).bookmark_mappings
} else {
    Vec::new()
};
let (ordered_running, ordered_bookmarks) =
    crate::blitz_adapter::document_ordered_gcpm_mappings(
        &doc,
        &combined_css,
        assetbundle_css_injected,
        &link_gcpm_by_node,
        ua_bookmark_mappings,
    );
gcpm.running_mappings = ordered_running;
gcpm.bookmark_mappings = ordered_bookmarks;
```

`link_gcpm_by_node` here is the 4th binding added to the
`parse_html_with_local_resources` destructure in Task 5.

**Step 4: Build and run the full lib test suite**

Run: `cargo build -p fulgur --lib`
Run: `cargo test -p fulgur --lib`
Expected: all 2191+ tests pass (2191 was the baseline count before this
plan's new tests; expect that plus whatever Tasks 1-6/8 added).

**Step 5: Commit**

```bash
git add crates/fulgur/src/engine.rs
git commit -m "feat(engine): wire document-order GCPM mapping recompute into the render path"
```

---

## Task 8: Extraction-order regression tests

**Files:**

- Test: `crates/fulgur/src/engine.rs` `mod tests`, or a new
  `crates/fulgur/tests/gcpm_cascade_order.rs` integration test file if the
  scenarios need real files on disk (`base_path` + `<link>` — they do)

Per the design doc's testing plan, cover:

1. `<style>` before `<link>` in markup, both matching the same element with
   equal specificity — assert the `<link>` rule wins (later in document
   order).
2. Reverse markup order (`<link>` before `<style>`) — assert the `<style>`
   rule wins.
3. **The non-obvious case**: AssetBundle CSS vs. a `<link>`/inline
   `<style>` rule, equal specificity — assert AssetBundle wins (its
   injected `<style>` lands last in `<head>`). Comment the test explaining
   why this is correct despite looking backwards (AssetBundle is not "the
   base/default" in cascade terms — see the design doc's "Discovery 1").
4. Re-run (don't just trust it still compiles) the existing
   `parse_html_with_local_resources_orders_imports_before_parent` test and
   every `gcpm_snapshot` test (`gcpm_running_element_via_inline_style`,
   `gcpm_element_policy_first`, `gcpm_element_policy_last`) — grep for
   `gcpm_snapshot` and `fulgur-da3u` in `blitz_adapter.rs` to find them all.

**Step 1-N:** one test per scenario above, following this file's existing
`Engine::builder()...build().render(html)` + PDF/outline-inspection test
pattern (search `render_with_running_elements_css` /
`render_with_string_set_css` in `engine.rs` for the idiom, and
`crates/fulgur/src/inspect.rs` for how existing tests read back bookmark
outlines / `element()` output from a rendered PDF to assert on).

**Run:** `cargo test -p fulgur --lib` and `cargo test -p fulgur` (full,
including `tests/` integration suite) after each new test.

**Commit** after each scenario lands and passes:

```bash
git add crates/fulgur/src/engine.rs # or the new integration test file
git commit -m "test(gcpm): cover AssetBundle/link/style document-order cascade wins"
```

---

## Task 9: `render_smoke` end-to-end coverage

Per `CLAUDE.md`'s coverage-scope rule — this touches a render path
(`Engine::render`), not just a pure helper, so it needs an end-to-end
smoke test in addition to the unit tests above.

**Files:**

- Test: `crates/fulgur/tests/render_smoke.rs`

**Step 1:** Add a test with competing `bookmark-level` rules of different
specificity (e.g. `.chapter { bookmark-level: 2; }` vs.
`#intro.chapter { bookmark-level: 1; }` — note: `ParsedSelector` doesn't
parse compounds yet, so express this as two *separate*, single-selector
rules of different specificity tiers instead, e.g. a `Class` rule and an
`Id` rule both matching the same element via `class="chapter" id="intro"`).
Assert via `Engine::builder().build().render(html)` that the PDF is
non-empty (`assert!(!pdf.is_empty())`) and, if
`crates/fulgur/src/inspect.rs` exposes an outline-reading helper, that the
higher-specificity rule's level won.

**Step 2:** Run: `cargo test -p fulgur --test render_smoke`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/fulgur/tests/render_smoke.rs
git commit -m "test(render_smoke): cover specificity-resolved bookmark-level end-to-end"
```

---

## Task 10: Full verification pass

**Step 1:** `cargo test -p fulgur --lib`
**Step 2:** `cargo test -p fulgur` (full crate, all integration test files)
**Step 3:** `cargo clippy -p fulgur --lib --tests -- -D warnings` (or
whatever the project's actual clippy invocation is — check
`.github/workflows/ci.yml` if unsure)
**Step 4:** `cargo fmt --check`
**Step 5:** `npx markdownlint-cli2 '**/*.md'` if any plan/design doc changed

All must pass clean before moving to Task 11.

---

## Task 11: Close out

**Step 1:** Update `fulgur-smlr`'s beads record:

```bash
bd update fulgur-smlr --status in_progress  # if not already
```

(Leave the close to the PR-merge step, per the project's usual flow — see
`bd show fulgur-smlr` for the current design/notes fields already recorded
from the brainstorming phase.)

**Step 2:** Open the PR (title in English, body in Japanese, per the
`feedback_pr_body_japanese` project convention). Reference
`docs/plans/2026-09-17-fulgur-smlr-gcpm-cascade-design.md` and this plan
in the PR body.

**Step 3:** Mention the two related-but-deferred issues found during
design (`StringSetPass`/`CounterPass`'s own, differently-shaped cascade
gaps — see the design doc's "Related, deferred" section) — file them as
separate beads issues if not already done, don't fold them into this PR.
