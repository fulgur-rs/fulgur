# Tagged PDF StructTree Builder — Design

Issue: `fulgur-izp.5`
Depends on: `fulgur-izp.4` (TagCollector + start_tagged/end_tagged wiring)

## Context

`fulgur-izp.4` implements a **flat** TagTree: each tagged NodeId becomes a
top-level `TagGroup` in the tree, with its `Identifier` leaves as direct
children. The DOM parent-child hierarchy in `Drawables.semantics` is
ignored. This issue replaces that flat builder with a hierarchical one that
reflects HTML element nesting.

## Scope

In:
1. Extend `pdf_tag_to_krilla_tag` in `tagging.rs` to cover all `PdfTag`
   variants (currently only P/H/Span are handled; the rest fall back to P).
2. Replace the flat TagTree builder in `render.rs` with a hierarchical
   builder that uses `SemanticEntry.parent` to nest `TagGroup`s.
3. PDF byte-snapshot tests asserting the stable StructTree output.

Out (deferred to later issues):
- `ListNumbering` detection from CSS `list-style-type` (izp.7).
- `TableHeaderScope` detection from `<th scope="...">` (izp.8).
- `<thead>` / `<tfoot>` distinction for `TRowGroup` (izp.8).
- `Figure` alt text (izp.6).
- Link annotation wiring (izp.9).

## Part A: `pdf_tag_to_krilla_tag` extension

Full mapping for every `PdfTag` variant:

| `PdfTag`     | Krilla `TagKind`                            | Notes                         |
|-------------|----------------------------------------------|-------------------------------|
| `P`          | `Tag::<kind::P>::P`                          | unchanged                     |
| `H { level }`| `Tag::Hn(level, heading_title)`              | unchanged                     |
| `Span`       | `Tag::<kind::Span>::Span`                    | unchanged                     |
| `Div`        | `Tag::<kind::Div>::Div`                      | covers section/article etc.   |
| `Figure`     | `Tag::<kind::Figure>::Figure`                | alt text deferred to izp.6    |
| `L`          | `Tag::L(ListNumbering::None)`                | numbering deferred to izp.7   |
| `Li`         | `Tag::<kind::LI>::LI`                        | Lbl/LBody split deferred      |
| `Table`      | `Tag::<kind::Table>::Table`                  |                               |
| `TRowGroup`  | `Tag::<kind::TBody>::TBody`                  | thead/tfoot deferred to izp.8 |
| `Tr`         | `Tag::<kind::TR>::TR`                        |                               |
| `Th`         | `Tag::TH(TableHeaderScope::Both)`            | scope deferred to izp.8       |
| `Td`         | `Tag::<kind::TD>::TD`                        |                               |

## Part B: Hierarchical TagTree builder

### Algorithm

```
Inputs:
  drawables.semantics: BTreeMap<NodeId, SemanticEntry>
  TagCollector.entries: Vec<(NodeId, PdfTag, Identifier, Option<heading_title>)>

Step 1 — Group identifiers by NodeId
  identifiers_map: BTreeMap<NodeId, Vec<Identifier>>
  heading_titles:  BTreeMap<NodeId, String>        // first heading_title wins

Step 2 — Build parent→children map
  children_map: BTreeMap<NodeId, Vec<NodeId>>
  For (node_id, entry) in &semantics:
    if let Some(p) = entry.parent:
      children_map[p].push(node_id)
  // BTreeMap insertion order = NodeId ascending = DOM parse order = reading order

Step 3 — Collect roots
  roots: Vec<NodeId> = semantics keys where entry.parent == None (ascending)

Step 4 — Recursive build
  fn build_group(node_id) -> TagGroup:
    let tag  = &semantics[node_id].tag
    let title = heading_titles.get(node_id).cloned()
    let mut g = TagGroup::new(pdf_tag_to_krilla_tag(tag, title))
    for id in identifiers_map.get(node_id).iter().flatten():
      g.push(Node::Leaf(*id))          // leaves first (content identifiers)
    for child_id in children_map.get(node_id).iter().flatten():
      g.push(Node::Group(build_group(child_id)))   // then child groups
    g

Step 5 — Assemble tree
  let mut tree = TagTree::new().with_lang(config.lang.clone())
  for root_id in roots:
    tree.push(Node::Group(build_group(root_id)))
  document.set_tag_tree(tree)
```

### Key design decisions

**Leaves before child Groups**: For a given NodeId, its own `Identifier`
leaves appear before child `TagGroup`s. In the current scope (P/H nodes do
not contain child P/H nodes), this ordering is correct. The edge case where
a `<p>` directly wraps a `<span>` (Span is a child group) is acceptable for
now; reading-order interleaving of leaves and groups is deferred.

**BTreeMap for determinism**: All traversal order depends on `NodeId`
comparison via `BTreeMap`. NodeIds are allocated by Blitz during HTML
parsing in document order, so BTreeMap order ≈ DOM order.

**Empty groups**: Container nodes (div/section) that have no Identifier
leaves and no semantic children produce empty `TagGroup`s. Krilla discards
these silently per the PDF spec, so no pre-pruning is needed.

**Container-only nodes in the tree**: Nodes that appear in `semantics` but
have no `TagCollector` entries (e.g. `<div>`) still get a `TagGroup` —
built from `semantics` alone. This is the main structural addition over
the izp.4 flat builder.

## Part C: PDF byte-snapshot tests

### Snapshot infrastructure

```
crates/fulgur/tests/snapshots/
  tagged_struct_tree_nested.pdf
  tagged_struct_tree_deterministic.pdf   (optional second render comparison)
```

Helper added to the smoke test file:

```rust
fn check_pdf_snapshot(name: &str, pdf: &[u8]) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.pdf"));
    if std::env::var("FULGUR_UPDATE_SNAPSHOTS").is_ok() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, pdf).unwrap();
        if std::env::var("FULGUR_UPDATE_SNAPSHOTS").is_err() {
            panic!("new snapshot created: {name} — review and re-run");
        }
        return;
    }
    let expected = std::fs::read(&path).unwrap();
    assert_eq!(
        pdf, expected.as_slice(),
        "PDF snapshot mismatch: {name}\nRun with FULGUR_UPDATE_SNAPSHOTS=1 to update"
    );
}
```

### Determinism requirements

- Fixed font: `examples/.fonts/NotoSans-Regular.ttf` supplied via
  `AssetBundle` so font subsetting is machine-independent.
- `font-family: 'Noto Sans', sans-serif` in inline CSS to ensure only
  NotoSans is selected.
- No `creation_date` set in `Config` (avoids timestamp variation).
- `document_id` in krilla is `stable_hash_base64(pdf_bytes)` — fully
  content-addressed, so same input → same bytes → same ID.

### Test cases

```rust
// 1. Hierarchical structure smoke: section > h1 + p
#[test]
fn snapshot_tagged_struct_tree_nested() { ... }

// 2. Determinism: two renders of the same HTML produce identical bytes
#[test]
fn tagged_pdf_is_deterministic() { ... }
```

## Files touched

- `crates/fulgur/src/tagging.rs` — extend `pdf_tag_to_krilla_tag`
- `crates/fulgur/src/render.rs` — replace flat builder with hierarchical builder
- `crates/fulgur/tests/render_smoke.rs` — `check_pdf_snapshot` helper + snapshot tests
- `crates/fulgur/tests/snapshots/` — new directory (generated on first run)
