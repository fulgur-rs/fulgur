# counters() 関数対応 (fulgur-vsv) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS Lists 3 の `counters(name, separator, style?)` を実装し、ネストした `<ol counter-reset:item>` 等で全インスタンスをセパレータで連結した文字列が生成できるようにする。同時に flat-map ベースの `CounterState` を spec 準拠の stack-of-instances モデルへ置き換え、`counter()` のネスト挙動も spec 通り innermost を返すよう正す。

**Architecture:** `CounterState` の内部表現を `BTreeMap<String, Vec<CounterInstance>>`（インスタンスは originator の親 node_id を保持）に置き換える。`CounterPass` の DFS は post-order で `leave_element(node_id)` を呼び、`stack.last().parent_id == node_id` のあいだ pop することで CSS scope (element + descendants + following siblings + their descendants) を表現する。`ContentItem` に `Counters { name, separator, style }` を追加し、resolver で stack の全要素を separator で join する。

**Tech Stack:** Rust, cssparser, blitz-dom, GCPM (CSS Generated Content for Paged Media)

**Scope notes:**
- `CounterState` の既存 API (`reset/increment/set/get/snapshot`) はシグネチャを変えず残す（`pagination_layout::collect_counter_states` が flat 動作のまま使う）。新たに `reset_in_scope` / `increment_in_scope` / `set_in_scope` / `leave_element` / `chain` / `chain_snapshot` を追加し、`CounterPass` だけがそれらを使う。
- 結果: margin box 内の `counters()` は単一値 chain にフォールバック、`<ol>` ネストの ::before/::after に書いた `counters()` は spec 通り全 chain を join する（本 issue の主目的）。
- BookmarkPass は `chain_snapshot()` の結果を保持し、`counter()` は `last()` を取り、`counters()` は join する。

---

## Task 1: `ContentItem::Counters` enum variant + parser

新しい `Counters` ContentItem を追加し、`counters(name, "sep", style?)` をパースする。resolver はまだ no-op（後続タスクで実装）。

**Files:**
- Modify: `crates/fulgur/src/gcpm/mod.rs` (around `enum ContentItem`, line ~245)
- Modify: `crates/fulgur/src/gcpm/parser.rs` (around `parse_content_value`, line ~1058)
- Test: `crates/fulgur/src/gcpm/parser.rs` (`#[cfg(test)] mod tests`, append at end of `mod tests`)

**Step 1: Write failing parser tests**

`crates/fulgur/src/gcpm/parser.rs` の `mod tests` に以下を追加:

```rust
#[test]
fn test_parse_counters_with_separator_only() {
    let css = r#"li::before { content: counters(item, "."); }"#;
    let ctx = parse_gcpm(css);
    let mapping = &ctx.content_counter_mappings[0];
    assert_eq!(
        mapping.content,
        vec![ContentItem::Counters {
            name: "item".into(),
            separator: ".".into(),
            style: CounterStyle::Decimal,
        }]
    );
}

#[test]
fn test_parse_counters_with_style() {
    let css = r#"li::before { content: counters(item, "-", upper-roman); }"#;
    let ctx = parse_gcpm(css);
    let mapping = &ctx.content_counter_mappings[0];
    assert_eq!(
        mapping.content,
        vec![ContentItem::Counters {
            name: "item".into(),
            separator: "-".into(),
            style: CounterStyle::UpperRoman,
        }]
    );
}

#[test]
fn test_parse_counters_missing_separator_drops_item() {
    // counters() with only a name is invalid per spec — drop silently.
    let css = r#"li::before { content: counters(item); }"#;
    let ctx = parse_gcpm(css);
    // The mapping is still created (the rule matched a content-counter
    // selector via the strip-and-inject machinery), but its items list
    // contains no Counters entry.
    let any_counters = ctx
        .content_counter_mappings
        .iter()
        .flat_map(|m| m.content.iter())
        .any(|i| matches!(i, ContentItem::Counters { .. }));
    assert!(!any_counters, "invalid counters() should produce no item");
}
```

**Step 2: Verify tests fail**

```bash
cargo test -p fulgur --lib gcpm::parser::tests::test_parse_counters 2>&1 | tail -20
```

Expected: compilation error (`ContentItem::Counters` not found).

**Step 3: Add `Counters` variant to `ContentItem`**

`crates/fulgur/src/gcpm/mod.rs` で `ContentItem::Counter { name, style }` のすぐ下に追加:

```rust
/// A nested counter reference, e.g. `counters(section, ".")` or
/// `counters(section, ".", upper-roman)`. Resolves to all active
/// counter instances joined by the separator (CSS Lists 3 §4.5).
Counters {
    /// Counter name. Built-ins (`page`, `pages`) fall back to a
    /// single-value chain at margin-box resolve time.
    name: String,
    /// Separator string inserted between consecutive instance values.
    separator: String,
    /// Display style applied to every value in the chain.
    style: CounterStyle,
},
```

**Step 4: Wire parser to recognise `counters(...)`**

`crates/fulgur/src/gcpm/parser.rs` の `parse_content_value` 内、`fn_name.eq_ignore_ascii_case("counter")` の `else if` ブロックの直後に追加:

```rust
} else if fn_name.eq_ignore_ascii_case("counters") {
    let name = arg.to_string();
    // separator is required per spec.
    if input.try_parse(|input| input.expect_comma()).is_err() {
        // Drop item silently — invalid counters() with no separator.
        return Ok(());
    }
    let separator = match input.try_parse(|input| {
        input.expect_string().map(|s| s.to_string())
    }) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let style = input
        .try_parse(|input| {
            input.expect_comma()?;
            parse_counter_style(input)
        })
        .unwrap_or(CounterStyle::Decimal);
    items.push(ContentItem::Counters { name, separator, style });
}
```

**Step 4b: Extend the strip-and-inject predicate to recognise Counters**

`crates/fulgur/src/gcpm/parser.rs` line 717 の `has_counter` を `Counters` も拾うように拡張:

```rust
let has_counter = items.iter().any(|item|
    matches!(
        item,
        ContentItem::Counter { .. } | ContentItem::Counters { .. }
    )
);
```

ここで grep して同種の述語が他にもないか確認:

```bash
grep -n "matches!(.*ContentItem::Counter " crates/fulgur/src/gcpm/parser.rs crates/fulgur/src/blitz_adapter.rs
```

ヒットしたら全箇所同じ拡張を適用する（Task 1 の単体テスト `test_parse_counters_with_separator_only` が
`ctx.content_counter_mappings[0]` を読むので、ここを拡張しないと Vec が空のままで panic する）。

**Step 5: Add inert match arms for `Counters`**

This task does not implement resolution. To keep `cargo build` green, add `ContentItem::Counters { .. } => {}` arms (or extend existing catch-alls) in:

- `crates/fulgur/src/gcpm/counter.rs` `resolve_content_to_string` (line ~41)
- `crates/fulgur/src/gcpm/counter.rs` `resolve_content_to_html` flat mode (line ~105) and flex mode `_ => {}` (line ~162) — the existing `_` already covers it, but verify via build
- `crates/fulgur/src/blitz_adapter.rs` `CounterPass::resolve_content` (line ~1861) — the existing `_ => {}` covers it
- `crates/fulgur/src/blitz_adapter.rs` `resolve_label` (line ~2092) add explicit drop arm

For `resolve_content_to_string` add to the catch-all group:

```rust
ContentItem::ContentText
| ContentItem::ContentBefore
| ContentItem::ContentAfter
| ContentItem::Attr(_)
| ContentItem::Leader { .. }
| ContentItem::Counters { .. } => {}
```

For `resolve_label` similarly extend:

```rust
ContentItem::ContentBefore
| ContentItem::ContentAfter
| ContentItem::Element { .. }
| ContentItem::Leader { .. }
| ContentItem::Counters { .. } => {}
```

**Step 6: Run tests**

```bash
cargo test -p fulgur --lib gcpm::parser::tests::test_parse_counters 2>&1 | tail -20
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 3 new tests pass; total lib tests pass.

**Step 7: Commit**

```bash
git add crates/fulgur/src/gcpm/mod.rs crates/fulgur/src/gcpm/parser.rs crates/fulgur/src/gcpm/counter.rs crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(gcpm): add ContentItem::Counters variant + counters() parser"
```

---

## Task 2: `CounterState` stack-of-instances refactor (preserve existing API)

`CounterState` の内部を `BTreeMap<String, Vec<CounterInstance>>` に変更。既存 API は flat-map と同等の動作を維持しつつ、新 API として `*_in_scope` / `leave_element` / `chain` / `chain_snapshot` を追加する。

**Files:**
- Modify: `crates/fulgur/src/gcpm/counter.rs` (`CounterState` and `impl CounterState`, line 328-363)
- Test: `crates/fulgur/src/gcpm/counter.rs` (`#[cfg(test)] mod tests`, append after existing tests)

**Step 1: Write failing unit tests**

`crates/fulgur/src/gcpm/counter.rs` の `mod tests` の末尾に追加:

```rust
#[test]
fn test_counter_state_in_scope_basic() {
    let mut s = CounterState::new();
    s.reset_in_scope("section", 0, 1); // E (parent=1) creates instance
    s.increment_in_scope("section", 1, 1);
    assert_eq!(s.get("section"), 1);
    assert_eq!(s.chain("section"), vec![1]);
}

#[test]
fn test_counter_state_nested_chain() {
    // <root id=1> <ol id=2 reset:item> <li id=3 inc:item> <ol id=4 reset:item> <li id=5 inc:item>
    // Stack model: parent_id is the parent of the resetting element.
    let mut s = CounterState::new();
    s.reset_in_scope("item", 0, 1); // ol#2's reset: parent=1
    s.increment_in_scope("item", 1, 2); // li#3's increment, target=top
    s.reset_in_scope("item", 0, 3); // ol#4's reset: parent=3
    s.increment_in_scope("item", 1, 4); // li#5's increment, target=top
    assert_eq!(s.get("item"), 1);
    assert_eq!(s.chain("item"), vec![1, 1]);
}

#[test]
fn test_counter_state_leave_element_pops_children() {
    let mut s = CounterState::new();
    s.reset_in_scope("item", 0, 1); // pushed by something whose parent is 1
    s.reset_in_scope("item", 0, 2); // pushed by something whose parent is 2
    assert_eq!(s.chain("item"), vec![0, 0]);
    s.leave_element(2); // children of 2 popped
    assert_eq!(s.chain("item"), vec![0]);
    s.leave_element(1); // children of 1 popped
    assert_eq!(s.chain("item"), Vec::<i32>::new());
}

#[test]
fn test_counter_state_increment_without_reset_implicit_root() {
    // CSS spec: counter-increment on a counter not in scope creates an
    // implicit root counter at value 0 then applies the increment.
    let mut s = CounterState::new();
    s.increment_in_scope("foo", 5, 42);
    assert_eq!(s.get("foo"), 5);
    assert_eq!(s.chain("foo"), vec![5]);
    // The implicit root instance lives forever — leave_element on the
    // recorded parent never pops it (sentinel value differs).
    s.leave_element(42);
    assert_eq!(s.chain("foo"), vec![5]);
}

#[test]
fn test_counter_state_existing_api_backwards_compatible() {
    // Public flat API (used by collect_counter_states) must keep
    // working: reset → increment → get returns the latest reset+inc.
    let mut s = CounterState::new();
    s.reset("page", 0);
    s.increment("page", 1);
    assert_eq!(s.get("page"), 1);
    let snap = s.snapshot();
    assert_eq!(snap.get("page"), Some(&1));
}

#[test]
fn test_counter_state_legacy_reset_does_not_grow_stack() {
    // Regression: collect_counter_states reuses one CounterState
    // across all pages and applies counter-reset on every page that
    // declares it. The legacy `reset` API must overwrite the existing
    // root instance, not push a new one — otherwise the stack (and
    // chain_snapshot) grows O(pages).
    let mut s = CounterState::new();
    s.reset("page", 0);
    s.reset("page", 0);
    s.reset("page", 0);
    assert_eq!(s.chain("page"), vec![0]);
}

#[test]
fn test_counter_state_chain_snapshot() {
    let mut s = CounterState::new();
    s.reset_in_scope("item", 0, 1);
    s.increment_in_scope("item", 1, 1);
    s.reset_in_scope("item", 0, 2);
    s.increment_in_scope("item", 2, 2);
    let chain_snap = s.chain_snapshot();
    assert_eq!(chain_snap.get("item"), Some(&vec![1, 2]));
}
```

**Step 2: Verify tests fail**

```bash
cargo test -p fulgur --lib gcpm::counter::tests::test_counter_state 2>&1 | tail -10
```

Expected: compile errors (`reset_in_scope`, `chain`, `leave_element`, `chain_snapshot` missing).

**Step 3: Replace `CounterState` internals**

`crates/fulgur/src/gcpm/counter.rs` の line 328 周辺 (`CounterState` の定義と `impl`) を以下に置き換え:

```rust
/// Sentinel `parent_id` for instances created via the legacy flat API
/// (`reset` / `increment` / `set`) and for implicit-root instances
/// created when `*_in_scope` is called without a prior reset.
/// `usize::MAX` is unreachable as a real DOM node id because Blitz's
/// node ids are dense low integers, so `leave_element(usize::MAX)`
/// will never accidentally match.
pub const COUNTER_ROOT_PARENT: usize = usize::MAX;

/// A single counter instance: a value scoped to the originating
/// element's parent's subtree (CSS Lists 3 §4.5).
#[derive(Debug, Clone)]
struct CounterInstance {
    /// Parent node id of the element that pushed this instance.
    /// `leave_element(X)` pops while top.parent_id == X.
    parent_id: usize,
    value: i32,
}

/// Tracks CSS counter values during DOM traversal.
///
/// Internally stores a stack of instances per name (CSS Lists 3 scope:
/// element + descendants + following siblings + their descendants).
/// `counter()` returns the innermost (top of stack) value;
/// `counters()` joins all values from outermost to innermost.
#[derive(Debug, Clone, Default)]
pub struct CounterState {
    stacks: BTreeMap<String, Vec<CounterInstance>>,
}

impl CounterState {
    pub fn new() -> Self {
        Self::default()
    }

    // ----- Legacy flat API (no scope tracking) -----

    /// Reset (or create) a counter at the implicit-root scope.
    /// Used by `pagination_layout::collect_counter_states` which
    /// reuses one `CounterState` across all pages and never calls
    /// `leave_element`. Therefore this overwrites the existing root
    /// instance rather than pushing — preserving the old flat-map
    /// `BTreeMap::insert` semantics and preventing unbounded stack
    /// growth across pages.
    pub fn reset(&mut self, name: &str, value: i32) {
        let stack = self.stacks.entry(name.to_string()).or_default();
        if let Some(top) = stack.last_mut() {
            if top.parent_id == COUNTER_ROOT_PARENT {
                top.value = value;
                return;
            }
        }
        stack.push(CounterInstance {
            parent_id: COUNTER_ROOT_PARENT,
            value,
        });
    }

    pub fn increment(&mut self, name: &str, value: i32) {
        self.increment_in_scope(name, value, COUNTER_ROOT_PARENT);
    }

    pub fn set(&mut self, name: &str, value: i32) {
        self.set_in_scope(name, value, COUNTER_ROOT_PARENT);
    }

    /// Innermost active value for `name`, or 0 if none.
    pub fn get(&self, name: &str) -> i32 {
        self.stacks
            .get(name)
            .and_then(|s| s.last())
            .map(|i| i.value)
            .unwrap_or(0)
    }

    /// Flat snapshot mapping each name to its innermost value.
    /// Used by margin-box rendering via `collect_counter_states`.
    pub fn snapshot(&self) -> BTreeMap<String, i32> {
        self.stacks
            .iter()
            .filter_map(|(k, v)| v.last().map(|i| (k.clone(), i.value)))
            .collect()
    }

    // ----- Scope-aware API (used by CounterPass) -----

    /// Push a new counter instance scoped to `parent_id`'s subtree.
    pub fn reset_in_scope(&mut self, name: &str, value: i32, parent_id: usize) {
        self.stacks
            .entry(name.to_string())
            .or_default()
            .push(CounterInstance { parent_id, value });
    }

    /// Increment the innermost active instance of `name`. If no
    /// instance exists, create one at `parent_id`'s scope at value 0,
    /// then apply the increment (CSS Lists 3: implicit root reset).
    pub fn increment_in_scope(&mut self, name: &str, value: i32, parent_id: usize) {
        let stack = self.stacks.entry(name.to_string()).or_default();
        if let Some(top) = stack.last_mut() {
            top.value += value;
        } else {
            stack.push(CounterInstance {
                parent_id: COUNTER_ROOT_PARENT,
                value,
            });
            // The recorded parent_id is intentionally COUNTER_ROOT_PARENT
            // — implicit-root instances must not be popped by
            // `leave_element(parent_id)`.
            let _ = parent_id;
        }
    }

    /// Set the innermost active instance of `name` to `value`. If no
    /// instance exists, create an implicit-root one at `value`.
    pub fn set_in_scope(&mut self, name: &str, value: i32, parent_id: usize) {
        let stack = self.stacks.entry(name.to_string()).or_default();
        if let Some(top) = stack.last_mut() {
            top.value = value;
        } else {
            stack.push(CounterInstance {
                parent_id: COUNTER_ROOT_PARENT,
                value,
            });
            let _ = parent_id;
        }
    }

    /// Pop instances created by direct children of `node_id`. Call in
    /// post-order (after recursing into a node's children, when the
    /// node itself is about to return to its parent).
    pub fn leave_element(&mut self, node_id: usize) {
        for stack in self.stacks.values_mut() {
            while stack.last().map_or(false, |i| i.parent_id == node_id) {
                stack.pop();
            }
        }
    }

    /// Chain of values for `name`, from outermost to innermost.
    /// Empty if no instance exists.
    pub fn chain(&self, name: &str) -> Vec<i32> {
        self.stacks
            .get(name)
            .map(|s| s.iter().map(|i| i.value).collect())
            .unwrap_or_default()
    }

    /// Snapshot of every counter's full chain (outer→inner). Used by
    /// `CounterPass::take_node_snapshots` for `BookmarkPass`.
    pub fn chain_snapshot(&self) -> BTreeMap<String, Vec<i32>> {
        self.stacks
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|i| i.value).collect()))
            .collect()
    }
}
```

**Step 4: Verify all tests pass**

```bash
cargo test -p fulgur --lib gcpm::counter 2>&1 | tail -10
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 6 new tests pass; existing `test_counter_state_snapshot` and other tests pass.

**Step 5: Commit**

```bash
git add crates/fulgur/src/gcpm/counter.rs
git commit -m "refactor(gcpm): replace CounterState flat map with stack-of-instances"
```

---

## Task 3: Wire `CounterPass` to scope-aware API + chain snapshots

`CounterPass` の DFS で `*_in_scope` を使い、post-order で `leave_element` を呼ぶ。`node_snapshots` を `BTreeMap<usize, BTreeMap<String, Vec<i32>>>` 型に変える。

**Files:**
- Modify: `crates/fulgur/src/blitz_adapter.rs` (`CounterPass`, lines 1629-1820)
- Test: `crates/fulgur/src/blitz_adapter.rs` (`#[cfg(test)] mod tests`, existing `test_counter_pass*` tests)

**Step 1: Write failing test for nested scope**

既存の `counter_pass_records_per_node_snapshot` テスト (line ~3149) と同じ
`parse(html, 400.0, &[])` + `PassContext { font_data: &[] }` パターンを使う。
`crates/fulgur/src/blitz_adapter.rs` の counter_pass 関連テスト群の末尾に追加:

```rust
#[test]
fn counter_pass_nested_reset_records_chain_snapshot() {
    use crate::gcpm::{CounterMapping, CounterOp, ParsedSelector};

    // Outer ol resets `item`. Inner ol nested under outer's first li
    // resets `item` again — at that scope chain is length 2.
    let html = r#"<html><body>
        <ol><li><ol><li id="inner">Inner</li></ol></li></ol>
    </body></html>"#;
    let mappings = vec![
        CounterMapping {
            parsed: ParsedSelector::Tag("ol".into()),
            ops: vec![CounterOp::Reset {
                name: "item".into(),
                value: 0,
            }],
        },
        CounterMapping {
            parsed: ParsedSelector::Tag("li".into()),
            ops: vec![CounterOp::Increment {
                name: "item".into(),
                value: 1,
            }],
        },
    ];
    let mut doc = parse(html, 400.0, &[]);
    let pass =
        CounterPass::new(mappings, Vec::new()).with_snapshot_recording();
    let ctx = PassContext { font_data: &[] };
    pass.apply(&mut doc, &ctx);
    let snapshots = pass.take_node_snapshots();

    // Find the inner li by walking the DOM and looking for an li
    // whose ancestor chain has two ols.
    fn find_inner_li(doc: &HtmlDocument, id: usize, depth: usize) -> Option<usize> {
        if let Some(node) = doc.get_node(id) {
            if let Some(el) = node.element_data() {
                if el.name.local.as_ref() == "li" && depth >= 2 {
                    return Some(id);
                }
            }
            let next_depth = if let Some(el) = node.element_data() {
                if el.name.local.as_ref() == "ol" { depth + 1 } else { depth }
            } else { depth };
            for &c in &node.children {
                if let Some(found) = find_inner_li(doc, c, next_depth) {
                    return Some(found);
                }
            }
        }
        None
    }
    let inner_id =
        find_inner_li(&doc, doc.root_element().id, 0).expect("inner li");
    let snap = snapshots.get(&inner_id).expect("snapshot at inner li");
    let chain = snap.get("item").expect("item chain at inner li");
    assert_eq!(
        chain.len(),
        2,
        "nested counter-reset must yield chain of length 2, got {chain:?}"
    );
    // Both inner and outer li have been incremented once before the
    // snapshot is taken (inner snapshot is taken after own ops).
    assert_eq!(chain, &vec![1, 1]);
}
```

**Step 2: Verify test fails**

```bash
cargo test -p fulgur --lib counter_pass_nested_reset 2>&1 | tail -20
```

Expected: snapshot is flat (length 1) — assertion fails.

**Step 3: Update `CounterPass` field types**

In `crates/fulgur/src/blitz_adapter.rs` around line 1643:

```rust
node_snapshots: RefCell<BTreeMap<usize, BTreeMap<String, Vec<i32>>>>,
```

In `take_node_snapshots` signature (line ~1691):

```rust
pub fn take_node_snapshots(&self) -> BTreeMap<usize, BTreeMap<String, Vec<i32>>> {
    std::mem::take(&mut *self.node_snapshots.borrow_mut())
}
```

**Step 4: Update `CounterPass::walk_tree` Phase 2 + add post-order leave**

In `crates/fulgur/src/blitz_adapter.rs` `walk_tree` (line ~1716), find the parent_id for the current node by reading `doc.get_node(node_id).map(|n| n.parent)`. Use it for the `*_in_scope` calls. Snippet of the modified Phase 2 block:

```rust
// Phase 2: Apply counter state changes (no doc borrow needed)
let parent_id = doc
    .get_node(node_id)
    .and_then(|n| n.parent)
    .unwrap_or(crate::gcpm::counter::COUNTER_ROOT_PARENT);

if !matched_ops.is_empty() {
    let mut state = self.state.borrow_mut();
    for op in &matched_ops {
        match op {
            CounterOp::Reset { name, value } => {
                state.reset_in_scope(name, *value, parent_id)
            }
            CounterOp::Increment { name, value } => {
                state.increment_in_scope(name, *value, parent_id)
            }
            CounterOp::Set { name, value } => {
                state.set_in_scope(name, *value, parent_id)
            }
        }
    }
    drop(state);
    self.ops_by_node.borrow_mut().push((node_id, matched_ops));
}
```

Snapshot-recording change (line ~1787):

```rust
if self.record_node_snapshots {
    self.node_snapshots
        .borrow_mut()
        .insert(node_id, self.state.borrow().chain_snapshot());
}
```

After the `for child_id in children { walk_tree ... }` loop near line 1840, add (still inside `walk_tree`):

```rust
// Post-order: pop instances created by this node's children. Per
// CSS Lists 3 §4.5, an instance is in scope through its
// originating element + descendants + following siblings, which
// means it must die when we return to the originating element's
// parent — which is now (we are about to return from `node_id`,
// and `node_id` was the parent of any instance pushed by its
// children).
self.state.borrow_mut().leave_element(node_id);
```

**Step 5: Update `CounterPass::resolve_content` (no behavior change yet)**

Line ~1861. The existing `state.get(name)` path keeps working — it now returns the innermost. No code change needed here, but verify the test compilations don't break.

**Step 6: Update existing CounterPass tests**

Tests like `counter_pass_records_per_node_snapshot` (line ~3149) and similar previously asserted on `BTreeMap<String, i32>` snapshots. Update them to:

```rust
let snaps: BTreeMap<usize, BTreeMap<String, Vec<i32>>> = pass.take_node_snapshots();
// existing assertions on values: `Some(&3)` becomes `Some(&vec![3])`.
```

Find every assertion of the form `assert_eq!(snap.get("X"), Some(&N))` in this file and convert to `Some(&vec![N])`.

**Step 7: Verify all tests pass**

```bash
cargo test -p fulgur --lib counter_pass 2>&1 | tail -20
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: new test passes; all existing CounterPass tests pass with updated assertions.

**Step 8: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(gcpm): scope counters per CSS Lists 3 in CounterPass DFS"
```

---

## Task 4: Wire `BookmarkPass` to consume `Vec<i32>` snapshots

`counter_snapshots` を `BTreeMap<usize, BTreeMap<String, Vec<i32>>>` に変え、`resolve_label` の `ContentItem::Counter` 解決を `last()` で行う。

**Files:**
- Modify: `crates/fulgur/src/blitz_adapter.rs` `BookmarkPass` (lines 1915-1955) and `resolve_label` (line ~2092)
- Modify: `crates/fulgur/src/engine.rs` (line ~244, where `counter_snapshots` is forwarded)
- Test: `crates/fulgur/src/blitz_adapter.rs` existing BookmarkPass tests

**Step 1: Update `BookmarkPass` field + ctor type**

```rust
counter_snapshots: BTreeMap<usize, BTreeMap<String, Vec<i32>>>,
```

```rust
pub fn new_with_snapshots(
    mappings: Vec<BookmarkMapping>,
    counter_snapshots: BTreeMap<usize, BTreeMap<String, Vec<i32>>>,
    string_snapshots: BTreeMap<usize, BTreeMap<String, String>>,
) -> Self { ... }
```

**Step 2: Update `resolve_label` Counter arm**

`crates/fulgur/src/blitz_adapter.rs` line 2112. Replace:

```rust
ContentItem::Counter { name, style } => {
    let value = counter_snapshot
        .and_then(|s| s.get(name))
        .and_then(|chain| chain.last().copied())
        .unwrap_or(0);
    out.push_str(&format_counter(value, *style));
}
```

The function signature also needs:

```rust
fn resolve_label(
    items: &[ContentItem],
    doc: &HtmlDocument,
    node_id: usize,
    elem: &blitz_dom::node::ElementData,
    counter_snapshot: Option<&BTreeMap<String, Vec<i32>>>,
    string_snapshot: Option<&BTreeMap<String, String>>,
) -> String { ... }
```

**Step 3: Fix call site in engine.rs**

`crates/fulgur/src/engine.rs` line 244 area: `counter_snapshots` is passed from `CounterPass::take_node_snapshots()` (now `Vec<i32>` typed) into `BookmarkPass::new_with_snapshots`. Type chain should be unbroken — confirm `cargo check`.

**Step 4: Update existing bookmark tests**

Any test in `blitz_adapter.rs` that constructed a `counter_snapshots: BTreeMap<usize, BTreeMap<String, i32>>` for `BookmarkPass::new_with_snapshots` needs to wrap inner values in `vec![...]`. Find with:

```bash
grep -n "BTreeMap::<String, i32>\|insert(\".*\".to_string(), [0-9]" crates/fulgur/src/blitz_adapter.rs | head -20
```

Convert each value to `vec![v]`.

**Step 5: Verify**

```bash
cargo test -p fulgur --lib bookmark 2>&1 | tail -10
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: all pass.

**Step 6: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs crates/fulgur/src/engine.rs
git commit -m "feat(gcpm): consume Vec<i32> chain snapshots in BookmarkPass"
```

---

## Task 5: Resolve `ContentItem::Counters` everywhere

`format_counter_chain` ヘルパを追加し、全 resolver で `Counters` を実装する。

**Files:**
- Modify: `crates/fulgur/src/gcpm/counter.rs` (new helper, all resolvers)
- Modify: `crates/fulgur/src/blitz_adapter.rs` (`CounterPass::resolve_content`, `resolve_label`)
- Test: `crates/fulgur/src/gcpm/counter.rs` mod tests

**Step 1: Write failing tests**

`crates/fulgur/src/gcpm/counter.rs` `mod tests`:

```rust
#[test]
fn test_format_counter_chain_basic() {
    assert_eq!(
        format_counter_chain(&[1, 2, 3], ".", CounterStyle::Decimal),
        "1.2.3"
    );
    assert_eq!(
        format_counter_chain(&[], ".", CounterStyle::Decimal),
        ""
    );
    assert_eq!(
        format_counter_chain(&[5], ".", CounterStyle::Decimal),
        "5"
    );
}

#[test]
fn test_format_counter_chain_with_style() {
    assert_eq!(
        format_counter_chain(&[1, 4, 9], "-", CounterStyle::UpperRoman),
        "I-IV-IX"
    );
}

#[test]
fn test_resolve_counters_in_margin_box_falls_back_to_single() {
    // Margin-box `counters()` only sees flat custom_counters; the
    // resolver degrades to single-value (equivalent to counter()).
    let items = vec![ContentItem::Counters {
        name: "chapter".into(),
        separator: ".".into(),
        style: CounterStyle::Decimal,
    }];
    let mut custom = BTreeMap::new();
    custom.insert("chapter".to_string(), 7);
    assert_eq!(
        resolve_content_to_string(&items, &BTreeMap::new(), 1, 1, &custom),
        "7"
    );
}
```

**Step 2: Verify tests fail**

```bash
cargo test -p fulgur --lib gcpm::counter::tests 2>&1 | tail -10
```

Expected: `format_counter_chain` not found, third test fails because Counters is no-op.

**Step 3: Add helper**

`crates/fulgur/src/gcpm/counter.rs` next to `format_counter` (around line 268):

```rust
/// Format a list of counter values according to the given
/// [`CounterStyle`] and join them by `separator`. Returns an empty
/// string when `values` is empty.
pub fn format_counter_chain(
    values: &[i32],
    separator: &str,
    style: CounterStyle,
) -> String {
    let mut out = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(separator);
        }
        out.push_str(&format_counter(*v, style));
    }
    out
}
```

**Step 4: Implement margin-box `Counters` arm in `counter.rs`**

In `resolve_content_to_string` (line ~21) — replace the catch-all you added in Task 1 with:

```rust
ContentItem::Counters { name, separator, style } => match name.as_str() {
    "page" => out.push_str(&format_counter(page as i32, *style)),
    "pages" => out.push_str(&format_counter(total_pages as i32, *style)),
    _ => {
        // Margin-box has no DOM tree, only flat custom_counters. Degrade
        // to single-value chain (equivalent to counter()). When the
        // body's CounterPass tracked chains correctly, the innermost
        // value is what made it into custom_counters.
        let value = custom_counters.get(name.as_str()).copied().unwrap_or(0);
        let chain = if value != 0 || custom_counters.contains_key(name.as_str()) {
            vec![value]
        } else {
            vec![]
        };
        out.push_str(&format_counter_chain(&chain, separator, *style));
    }
},
```

Apply equivalent change to `resolve_content_to_html` flat mode (line ~85) and flex mode (line ~139).

**Step 5: Implement `Counters` arm in `CounterPass::resolve_content`**

`crates/fulgur/src/blitz_adapter.rs` line 1861:

```rust
ContentItem::Counters { name, separator, style } => {
    let chain = state.chain(name);
    out.push_str(&format_counter_chain(&chain, separator, *style));
}
```

Add a `use crate::gcpm::counter::{format_counter, format_counter_chain};` near line 1414 if not already.

**Step 6: Implement `Counters` arm in `resolve_label`**

`crates/fulgur/src/blitz_adapter.rs` `resolve_label` (line ~2092):

```rust
ContentItem::Counters { name, separator, style } => {
    let chain: Vec<i32> = counter_snapshot
        .and_then(|s| s.get(name))
        .cloned()
        .unwrap_or_default();
    out.push_str(&format_counter_chain(&chain, separator, *style));
}
```

Remove `Counters { .. }` from the catch-all you added in Task 1.

**Step 7: Verify**

```bash
cargo test -p fulgur --lib gcpm::counter 2>&1 | tail -10
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: all 3 new tests pass; lib tests stay green.

**Step 8: Commit**

```bash
git add crates/fulgur/src/gcpm/counter.rs crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(gcpm): resolve counters() in margin boxes, ::before/::after, bookmark-label"
```

---

## Task 6: End-to-end smoke test

ネスト ol で `counters(item, ".")` を使った HTML を `Engine::render_html` に通し、PDF が生成されることと、可能ならテキスト抽出で `1.`, `1.1.`, `1.2.`, `2.` が含まれることを確認する。

**Files:**
- Create or modify: `crates/fulgur/tests/render_smoke.rs` (existing file — append a new test)

**Step 1: Inspect existing pattern**

```bash
head -80 crates/fulgur/tests/render_smoke.rs
```

Use the same `Engine::builder().build().render_html(html)` pattern + `assert!(!pdf.is_empty())`.

**Step 2: Write the test**

Append to `crates/fulgur/tests/render_smoke.rs`:

```rust
#[test]
fn smoke_nested_counters_function() {
    // Nested <ol> with custom counter `item` and a `counters(item, ".")`
    // marker. Verifies the new stack-of-instances scope model emits
    // PDF bytes; the actual rendered text is verified via lopdf when
    // the visible glyph extraction is robust enough.
    let html = r#"<!doctype html>
<html>
<head><style>
body { counter-reset: item -1; }
ol { counter-reset: item; padding: 0; margin: 0; list-style: none; }
li { counter-increment: item; }
li::before { content: counters(item, ".") ". "; }
</style></head>
<body>
<ol>
  <li>Alpha</li>
  <li>Beta
    <ol>
      <li>Beta-one</li>
      <li>Beta-two</li>
    </ol>
  </li>
  <li>Gamma</li>
</ol>
</body>
</html>"#;

    let pdf = fulgur::Engine::builder()
        .build()
        .render_html(html)
        .expect("render");
    assert!(!pdf.is_empty(), "rendered PDF must not be empty");

    // Best-effort text extraction. If lopdf can read the ToUnicode
    // CMap (krilla emits one), we expect the markers in the page text.
    if let Ok(doc) = lopdf::Document::load_mem(&pdf) {
        let page_ids: Vec<u32> = doc.get_pages().keys().copied().collect();
        let mut all_text = String::new();
        for pid in page_ids {
            if let Ok(t) = doc.extract_text(&[pid]) {
                all_text.push_str(&t);
            }
        }
        if !all_text.is_empty() {
            for needle in ["1.", "2.1.", "2.2.", "3."] {
                assert!(
                    all_text.contains(needle),
                    "expected {needle:?} in extracted text, got {all_text:?}"
                );
            }
        }
    }
}
```

(Note: with `body { counter-reset: item -1 }` and `li { counter-increment: item }`, first li gets value 0 (-1 + 1) — actually wait, `counter-reset: item -1` sets the body's item to -1, but `<ol>` then resets item to 0 again, so first li → 1. Adjust the assertion if mid-build numbering differs. The plan-execution subagent should run the test once and tune marker strings to match.)

**Step 3: Add lopdf dev-dep if missing**

Check `crates/fulgur/Cargo.toml` `[dev-dependencies]`:

```bash
grep -A3 "dev-dependencies" crates/fulgur/Cargo.toml
```

If `lopdf` is not listed, add it. (It is already a direct dependency of fulgur for inspect.rs, so it should also be available in dev tests via the same workspace lockfile — verify by grep.)

**Step 4: Run the test**

```bash
cargo test -p fulgur --test render_smoke smoke_nested_counters_function 2>&1 | tail -30
```

Expected: pass. If `extract_text` returns empty (lopdf 0.40 + krilla CMap incompatibility — see project memory `project_lopdf_text_extraction`), the test falls through to PDF-bytes check only, which is still meaningful.

**Step 5: Run full test suite + lints**

```bash
cargo test -p fulgur 2>&1 | tail -10
cargo clippy -p fulgur --all-targets -- -D warnings 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -10
```

Expected: all green.

**Step 6: Commit**

```bash
git add crates/fulgur/tests/render_smoke.rs crates/fulgur/Cargo.toml
git commit -m "test(gcpm): smoke test for counters() with nested <ol>"
```

---

## Final verification

Before declaring done:

```bash
cargo test -p fulgur 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
cargo fmt --check
```

If anything fails, fix and recommit. Then run `bd close fulgur-vsv` per blueprint:impl Step 6.
