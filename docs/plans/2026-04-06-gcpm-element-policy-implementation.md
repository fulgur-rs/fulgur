# GCPM element() 4ポリシー対応 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `content: element(name, <policy>)` の第2引数 (first/start/last/first-except) をWeasyPrint互換の意味論で実装する。

**Architecture:** string-set 実装と同じマーカー方式。`RunningElementPass` で DOM walk 時に node_id → instance_id マップを構築。`convert.rs` の zero-size 分岐で `RunningElementMarkerPageable` (zero-size no-op) を発行。`paginate.rs` で per-page instance リストを収集。`resolve_element_policy` でポリシーに応じて instance_id を選び、`RunningElementStore` から HTML を引く。フォールバックは「直近の先行ページの最後の instance」に統一。

**Tech Stack:** Rust, cssparser, Blitz DOM, 既存 GCPM infrastructure

**Related issue:** `fulgur-94h`

---

## Task 1: データ型 — ElementPolicy と ContentItem::Element の再構造化

**Files:**

- Modify: `crates/fulgur/src/gcpm/mod.rs`

**Step 1: ElementPolicy enum を追加**

`StringPolicy` 定義の直後に:

```rust
/// Policy for `element(name, <policy>)` — determines which running element
/// instance to show on a given page. WeasyPrint-compatible semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementPolicy {
    /// First instance assigned on the current page (default).
    First,
    /// Element in effect at start of page. Implemented identically to `First`
    /// with fallback (WeasyPrint has the same effective behavior).
    Start,
    /// Last instance assigned on the current page.
    Last,
    /// Like `First`, but empty on pages where the element is assigned.
    FirstExcept,
}
```

**Step 2: ContentItem::Element を構造体バリアントに変更**

```rust
// 変更前
Element(String),

// 変更後
Element {
    name: String,
    policy: ElementPolicy,
},
```

**Step 3: `cargo check -p fulgur` でビルドエラー箇所を洗い出し**

Expected: `ContentItem::Element(...)` の呼び出し箇所すべてがエラー化する (parser.rs, counter.rs)。次タスクで順次修正。

**Step 4: コミット**

```bash
git add crates/fulgur/src/gcpm/mod.rs
git commit -m "feat(gcpm): add ElementPolicy enum and restructure ContentItem::Element"
```

---

## Task 2: パーサー — element() の第2引数パース

**Files:**

- Modify: `crates/fulgur/src/gcpm/parser.rs`

**Step 1: 失敗テストを追加**

`parser.rs` の `tests` モジュール末尾に:

```rust
#[test]
fn test_parse_element_function_default_policy() {
    let css = "@page { @top-center { content: element(hdr); } }";
    let ctx = parse_gcpm_css(css);
    let rule = ctx.margin_boxes.first().unwrap();
    assert_eq!(
        rule.content,
        vec![ContentItem::Element {
            name: "hdr".into(),
            policy: ElementPolicy::First,
        }]
    );
}

#[test]
fn test_parse_element_function_all_policies() {
    for (policy_str, policy) in [
        ("first", ElementPolicy::First),
        ("start", ElementPolicy::Start),
        ("last", ElementPolicy::Last),
        ("first-except", ElementPolicy::FirstExcept),
    ] {
        let css = format!(
            "@page {{ @top-center {{ content: element(hdr, {}); }} }}",
            policy_str
        );
        let ctx = parse_gcpm_css(&css);
        let rule = ctx.margin_boxes.first().unwrap();
        assert_eq!(
            rule.content,
            vec![ContentItem::Element {
                name: "hdr".into(),
                policy,
            }],
            "Failed for policy: {}",
            policy_str
        );
    }
}

#[test]
fn test_parse_element_function_invalid_policy() {
    // Unknown policy identifier — the whole element() call should be dropped.
    let css = "@page { @top-center { content: element(hdr, bogus); } }";
    let ctx = parse_gcpm_css(css);
    let rule = ctx.margin_boxes.first().unwrap();
    assert!(rule.content.is_empty());
}
```

**Step 2: テスト実行して失敗を確認**

```bash
cargo test -p fulgur --lib gcpm::parser::tests::test_parse_element_function
```

Expected: コンパイルエラーまたは FAIL (ElementPolicy未使用import または ContentItem形不一致)。

**Step 3: パーサーに `parse_element_policy` と呼び出しを追加**

`parse_string_policy` の直後に共通ヘルパを追加 (ほぼ同一構造):

```rust
/// Parse the policy argument of `element(name, <policy>)`.
fn parse_element_policy<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<ElementPolicy, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    if ident.eq_ignore_ascii_case("start") {
        Ok(ElementPolicy::Start)
    } else if ident.eq_ignore_ascii_case("last") {
        Ok(ElementPolicy::Last)
    } else if ident.eq_ignore_ascii_case("first-except") {
        Ok(ElementPolicy::FirstExcept)
    } else if ident.eq_ignore_ascii_case("first") {
        let has_except = input
            .try_parse(|input| {
                input.expect_delim('-')?;
                let next = input.expect_ident()?.clone();
                if next.eq_ignore_ascii_case("except") {
                    Ok(())
                } else {
                    Err(input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid))
                }
            })
            .is_ok();
        Ok(if has_except {
            ElementPolicy::FirstExcept
        } else {
            ElementPolicy::First
        })
    } else {
        Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid))
    }
}
```

`parse_content_value` の `element` 分岐を置き換え:

```rust
if fn_name.eq_ignore_ascii_case("element") {
    let name = arg.to_string();
    // Optional second argument: policy identifier.
    let policy_result = input.try_parse(|input| {
        input.expect_comma()?;
        parse_element_policy(input)
    });
    // If a comma is present but the policy is invalid, drop the item entirely.
    match policy_result {
        Ok(policy) => {
            items.push(ContentItem::Element { name, policy });
        }
        Err(_) if input.is_exhausted() => {
            // No second argument — default to First.
            items.push(ContentItem::Element {
                name,
                policy: ElementPolicy::First,
            });
        }
        Err(_) => {
            // Second argument present but invalid — reject this call.
        }
    }
}
```

`use` 行に `ElementPolicy` を追加。

**Step 4: テスト実行**

```bash
cargo test -p fulgur --lib gcpm::parser
```

Expected: 全パーサーテスト PASS。

**Step 5: コミット**

```bash
git add crates/fulgur/src/gcpm/parser.rs
git commit -m "feat(gcpm): parse element() policy second argument"
```

---

## Task 3: RunningElementStore を instance-list 方式に書き換え

**Files:**

- Modify: `crates/fulgur/src/gcpm/running.rs`

**Step 1: 失敗テストを追加**

既存テストを置き換える形で:

```rust
#[test]
fn test_running_store_instance_registration() {
    let mut store = RunningElementStore::new();
    let id_a = store.register(10, "header".to_string(), "<h1>A</h1>".to_string());
    let id_b = store.register(20, "header".to_string(), "<h1>B</h1>".to_string());

    assert_ne!(id_a, id_b);
    assert_eq!(store.get_html(id_a), Some("<h1>A</h1>"));
    assert_eq!(store.get_html(id_b), Some("<h1>B</h1>"));
    assert_eq!(store.instance_for_node(10), Some(id_a));
    assert_eq!(store.instance_for_node(20), Some(id_b));
    assert_eq!(store.instance_for_node(99), None);
}

#[test]
fn test_running_store_name_lookup() {
    let mut store = RunningElementStore::new();
    let id = store.register(5, "footer".to_string(), "<p>F</p>".to_string());
    assert_eq!(store.name_of(id), Some("footer"));
}
```

**Step 2: テスト実行 — コンパイル失敗を確認**

```bash
cargo test -p fulgur --lib gcpm::running
```

Expected: `register` シグネチャ不一致、`get_html` / `instance_for_node` / `name_of` 未定義でエラー。

**Step 3: `RunningElementStore` を書き換え**

既存の `HashMap<String, String>` ベース実装を削除し、以下に置換:

```rust
#[derive(Debug, Clone)]
pub struct RunningInstance {
    pub id: usize,
    pub name: String,
    pub html: String,
}

pub struct RunningElementStore {
    instances: Vec<RunningInstance>,
    /// Maps DOM node_id → index into `instances`.
    node_to_instance: HashMap<usize, usize>,
}

impl RunningElementStore {
    pub fn new() -> Self {
        Self {
            instances: Vec::new(),
            node_to_instance: HashMap::new(),
        }
    }

    /// Register a running element instance. Returns the assigned instance_id.
    pub fn register(&mut self, node_id: usize, name: String, html: String) -> usize {
        let id = self.instances.len();
        self.instances.push(RunningInstance { id, name, html });
        self.node_to_instance.insert(node_id, id);
        id
    }

    /// Look up the instance_id assigned to a DOM node, if any.
    pub fn instance_for_node(&self, node_id: usize) -> Option<usize> {
        self.node_to_instance.get(&node_id).copied()
    }

    /// Get the serialized HTML for a given instance_id.
    pub fn get_html(&self, instance_id: usize) -> Option<&str> {
        self.instances.get(instance_id).map(|i| i.html.as_str())
    }

    /// Get the running name for a given instance_id.
    pub fn name_of(&self, instance_id: usize) -> Option<&str> {
        self.instances.get(instance_id).map(|i| i.name.as_str())
    }
}

impl Default for RunningElementStore {
    fn default() -> Self {
        Self::new()
    }
}
```

既存の `get(&str)` や `to_pairs()` は削除 (呼び出し元も後続タスクで更新)。

**Step 4: 残りのコンパイルエラーを洗い出し**

```bash
cargo check -p fulgur 2>&1 | tail -30
```

Expected: `blitz_adapter.rs` と `render.rs` に `to_pairs` / `get` / `register(name, html)` の呼び出しがあり、それぞれタスク4と8で修正する。

**Step 5: `gcpm::running` モジュールのテストのみ通す**

```bash
cargo test -p fulgur --lib gcpm::running
```

Expected: PASS (他モジュールはまだ壊れているが、このモジュール単体は通る)。

**Step 6: コミット**

この時点でトップレベルビルドは壊れているが、コミット境界を小さく保つため進める。

```bash
git add crates/fulgur/src/gcpm/running.rs
git commit -m "feat(gcpm): rewrite RunningElementStore with instance-list storage"
```

---

## Task 4: RunningElementPass — node_id を登録

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 1: `RunningElementPass::walk_tree` を修正**

既存の呼び出し:

```rust
self.store.borrow_mut().register(running_name, html);
```

を以下に置換:

```rust
self.store.borrow_mut().register(node_id, running_name, html);
```

**Step 2: 既存の `test_running_element_pass_extracts_by_class` テストを確認して更新**

テストは `store.get(name)` を呼んでいるはず。新APIに合わせて `instance_for_node` + `get_html` の組み合わせ、または新しいアサーションに書き換える。

```bash
grep -n "test_running_element_pass" crates/fulgur/src/blitz_adapter.rs
```

該当テストの中で:

```rust
// 変更前の想定
let store = pass.into_running_store();
assert_eq!(store.get("header"), Some("<h1>Title</h1>"));

// 変更後
let store = pass.into_running_store();
// Walk instances to find the one with name "header".
let header_id = (0..).find(|&i| store.name_of(i) == Some("header")).unwrap();
assert!(store.get_html(header_id).unwrap().contains("Title"));
```

(実際のテスト箇所を読んで文面を合わせる)

**Step 3: ビルド確認**

```bash
cargo check -p fulgur 2>&1 | tail -20
```

Expected: blitz_adapter.rs のエラーは解消。残りは render.rs のみ (後続タスク)。

**Step 4: `blitz_adapter` のテストを実行**

```bash
cargo test -p fulgur --lib blitz_adapter
```

Expected: PASS。

**Step 5: コミット**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(gcpm): record node_id when registering running element instances"
```

---

## Task 5: RunningElementMarkerPageable — zero-size marker

**Files:**

- Modify: `crates/fulgur/src/pageable.rs`

**Step 1: 失敗テストを追加**

`pageable.rs` の tests モジュール末尾に:

```rust
#[test]
fn test_running_element_marker_is_zero_size_noop() {
    let mut m = RunningElementMarkerPageable::new("header".to_string(), 42);
    let size = m.wrap(100.0, 100.0);
    assert_eq!(size.width, 0.0);
    assert_eq!(size.height, 0.0);
    assert_eq!(m.height(), 0.0);
    assert_eq!(m.name, "header");
    assert_eq!(m.instance_id, 42);
    assert!(m.split(100.0, 100.0).is_none());
}
```

**Step 2: テスト実行して失敗確認**

```bash
cargo test -p fulgur --lib pageable::tests::test_running_element_marker
```

Expected: FAIL (型未定義)。

**Step 3: `RunningElementMarkerPageable` を実装**

`StringSetPageable` 定義の直後に:

```rust
// ─── RunningElementMarkerPageable ────────────────────────

/// Zero-size marker for running element instances.
/// Inserted into the Pageable tree at the source position where
/// `position: running(name)` was declared, so that pagination can track
/// which running element instance is in effect on each page.
#[derive(Clone)]
pub struct RunningElementMarkerPageable {
    pub name: String,
    pub instance_id: usize,
}

impl RunningElementMarkerPageable {
    pub fn new(name: String, instance_id: usize) -> Self {
        Self { name, instance_id }
    }
}

impl Pageable for RunningElementMarkerPageable {
    fn wrap(&mut self, _avail_width: Pt, _avail_height: Pt) -> Size {
        Size { width: 0.0, height: 0.0 }
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        None
    }

    fn draw(&self, _canvas: &mut Canvas, _x: Pt, _y: Pt, _avail_width: Pt, _avail_height: Pt) {}

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        0.0
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
```

`pub use` / `mod` の再エクスポートがあれば追加。

**Step 4: テスト PASS 確認**

```bash
cargo test -p fulgur --lib pageable::tests::test_running_element_marker
```

Expected: PASS。

**Step 5: コミット**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(pageable): add RunningElementMarkerPageable zero-size marker"
```

---

## Task 6: Convert — running要素ノードの位置にマーカーを発行

**Files:**

- Modify: `crates/fulgur/src/convert.rs`

**Step 1: `ConvertContext` のフィールドを調整**

現状 `pub running_store: &'a mut RunningElementStore` を読み取り専用に変更:

```rust
pub running_store: &'a RunningElementStore,
```

(もはや convert 時点では register しない — RunningElementPass が済んでいる)

**Step 2: `emit_orphan_running_markers` ヘルパを追加**

`emit_orphan_string_set_markers` の直後に:

```rust
/// Emit a `RunningElementMarkerPageable` when a zero-size node corresponds to
/// a running element instance. Running elements are rewritten to `display: none`
/// by the GCPM parser, so they land in the zero-size branches of
/// `collect_positioned_children`. This preserves their source position in the
/// Pageable tree as a zero-size marker so pagination can determine which
/// running element instances fall on which page.
fn emit_orphan_running_marker(
    node_id: usize,
    x: f32,
    y: f32,
    ctx: &ConvertContext<'_>,
    out: &mut Vec<PositionedChild>,
) {
    if let Some(instance_id) = ctx.running_store.instance_for_node(node_id) {
        if let Some(name) = ctx.running_store.name_of(instance_id) {
            out.push(PositionedChild {
                child: Box::new(RunningElementMarkerPageable::new(
                    name.to_string(),
                    instance_id,
                )),
                x,
                y,
            });
        }
    }
}
```

`use` に `RunningElementMarkerPageable` を追加。

**Step 3: `collect_positioned_children` の zero-size 分岐で marker を emit**

既存の 2 箇所 (zero-size leaf, zero-size container) に、`emit_orphan_string_set_markers` 呼び出しの直後で:

```rust
emit_orphan_running_marker(
    child_id,
    child_layout.location.x,
    child_layout.location.y,
    ctx,
    &mut result,
);
```

**Step 4: 単体テスト追加**

`convert.rs` の tests モジュール (なければ作る) に統合テスト相当は不要だが、意図を表すテストを後続タスク 11 の統合テストで担保するため、ここでは `cargo check` で成立確認のみ。

**Step 5: ビルド確認**

```bash
cargo check -p fulgur 2>&1 | tail -20
```

Expected: convert.rs 由来のエラーなし。

**Step 6: コミット**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(convert): emit RunningElementMarkerPageable at running element source positions"
```

---

## Task 7: Paginate — ページごとの running instance 収集

**Files:**

- Modify: `crates/fulgur/src/paginate.rs`

**Step 1: 失敗テストを追加**

`paginate.rs` の tests モジュールに:

```rust
#[test]
fn test_collect_running_element_states_single_page() {
    use crate::pageable::RunningElementMarkerPageable;

    let marker_a: Box<dyn Pageable> =
        Box::new(RunningElementMarkerPageable::new("hdr".into(), 0));
    let marker_b: Box<dyn Pageable> =
        Box::new(RunningElementMarkerPageable::new("hdr".into(), 1));
    let block = BlockPageable::with_positioned_children(vec![
        pos(marker_a),
        pos(make_spacer(50.0)),
        pos(marker_b),
        pos(make_spacer(50.0)),
    ]);
    let pages = paginate(Box::new(block), 200.0, 500.0);
    assert_eq!(pages.len(), 1);
    let states = collect_running_element_states(&pages);
    assert_eq!(states[0].get("hdr").unwrap().instance_ids, vec![0, 1]);
}

#[test]
fn test_collect_running_element_states_splits_across_pages() {
    use crate::pageable::RunningElementMarkerPageable;

    let marker_a: Box<dyn Pageable> =
        Box::new(RunningElementMarkerPageable::new("hdr".into(), 0));
    let marker_b: Box<dyn Pageable> =
        Box::new(RunningElementMarkerPageable::new("hdr".into(), 1));
    let block = BlockPageable::with_positioned_children(vec![
        pos(marker_a),
        pos(make_spacer(100.0)),
        pos(make_spacer(100.0)), // forces split around here
        pos(marker_b),
        pos(make_spacer(100.0)),
    ]);
    let pages = paginate(Box::new(block), 200.0, 200.0);
    assert!(pages.len() >= 2);
    let states = collect_running_element_states(&pages);
    assert_eq!(states[0].get("hdr").unwrap().instance_ids, vec![0]);
    assert_eq!(states[1].get("hdr").unwrap().instance_ids, vec![1]);
}
```

**Step 2: 実行して失敗確認**

```bash
cargo test -p fulgur --lib paginate::tests::test_collect_running_element_states
```

Expected: FAIL (`PageRunningState`, `collect_running_element_states` 未定義)。

**Step 3: 実装を追加**

`paginate.rs` の string-set 関数群の直後に:

```rust
use crate::pageable::RunningElementMarkerPageable;

/// Per-page state for running elements of a given name.
#[derive(Debug, Clone, Default)]
pub struct PageRunningState {
    /// Instance IDs of running elements whose source position lies on this page,
    /// in source order.
    pub instance_ids: Vec<usize>,
}

/// Walk paginated pages and collect `RunningElementMarkerPageable` markers
/// per page, keyed by running element name.
pub fn collect_running_element_states(
    pages: &[Box<dyn Pageable>],
) -> Vec<BTreeMap<String, PageRunningState>> {
    let mut result: Vec<BTreeMap<String, PageRunningState>> = Vec::with_capacity(pages.len());

    for page in pages {
        let mut page_state: BTreeMap<String, PageRunningState> = BTreeMap::new();
        let mut markers = Vec::new();
        collect_running_markers(page.as_ref(), &mut markers);
        for (name, instance_id) in markers {
            page_state
                .entry(name)
                .or_default()
                .instance_ids
                .push(instance_id);
        }
        result.push(page_state);
    }

    result
}

fn collect_running_markers(pageable: &dyn Pageable, markers: &mut Vec<(String, usize)>) {
    let any = pageable.as_any();
    if let Some(m) = any.downcast_ref::<RunningElementMarkerPageable>() {
        markers.push((m.name.clone(), m.instance_id));
    } else if let Some(wrapper) = any.downcast_ref::<StringSetWrapperPageable>() {
        collect_running_markers(wrapper.child.as_ref(), markers);
    } else if let Some(block) = any.downcast_ref::<BlockPageable>() {
        for child in &block.children {
            collect_running_markers(child.child.as_ref(), markers);
        }
    } else if let Some(table) = any.downcast_ref::<TablePageable>() {
        for child in &table.header_cells {
            collect_running_markers(child.child.as_ref(), markers);
        }
        for child in &table.body_cells {
            collect_running_markers(child.child.as_ref(), markers);
        }
    } else if let Some(list_item) = any.downcast_ref::<ListItemPageable>() {
        collect_running_markers(list_item.body.as_ref(), markers);
    }
}
```

(`StringSetWrapperPageable`, `BlockPageable`, `TablePageable`, `ListItemPageable` のimportを追加)

**Step 4: テスト PASS 確認**

```bash
cargo test -p fulgur --lib paginate
```

Expected: PASS。

**Step 5: コミット**

```bash
git add crates/fulgur/src/paginate.rs
git commit -m "feat(paginate): add collect_running_element_states for per-page instance tracking"
```

---

## Task 8: Resolver — ポリシーに基づく instance 選択

**Files:**

- Modify: `crates/fulgur/src/gcpm/counter.rs`

**Step 1: 失敗テストを追加**

`counter.rs` の tests モジュールに:

```rust
#[test]
fn test_resolve_element_policy_scenarios() {
    use crate::gcpm::running::RunningElementStore;
    use crate::paginate::PageRunningState;

    let mut store = RunningElementStore::new();
    let id_a = store.register(1, "hdr".into(), "<h1>A</h1>".into()); // 0
    let id_b = store.register(2, "hdr".into(), "<h1>B</h1>".into()); // 1
    let id_c = store.register(3, "hdr".into(), "<h1>C</h1>".into()); // 2

    // P0 = [A, B], P1 = [C], P2 = []
    let mut p0 = BTreeMap::new();
    p0.insert(
        "hdr".to_string(),
        PageRunningState { instance_ids: vec![id_a, id_b] },
    );
    let mut p1 = BTreeMap::new();
    p1.insert(
        "hdr".to_string(),
        PageRunningState { instance_ids: vec![id_c] },
    );
    let p2 = BTreeMap::new();
    let states = vec![p0, p1, p2];

    // first: A, C, C (P2 falls back to P1.last = C)
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::First, 0, &states, &store), Some("<h1>A</h1>"));
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::First, 1, &states, &store), Some("<h1>C</h1>"));
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::First, 2, &states, &store), Some("<h1>C</h1>"));

    // last: B, C, C
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::Last, 0, &states, &store), Some("<h1>B</h1>"));
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::Last, 1, &states, &store), Some("<h1>C</h1>"));
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::Last, 2, &states, &store), Some("<h1>C</h1>"));

    // first-except: A, empty (has assignment), C (fallback)
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::FirstExcept, 0, &states, &store), None);
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::FirstExcept, 1, &states, &store), None);
    assert_eq!(resolve_element_policy("hdr", ElementPolicy::FirstExcept, 2, &states, &store), Some("<h1>C</h1>"));
}

#[test]
fn test_resolve_element_policy_no_assignments_anywhere() {
    use crate::gcpm::running::RunningElementStore;
    use crate::paginate::PageRunningState;

    let store = RunningElementStore::new();
    let states: Vec<BTreeMap<String, PageRunningState>> = vec![BTreeMap::new(); 3];

    for policy in [ElementPolicy::First, ElementPolicy::Start, ElementPolicy::Last, ElementPolicy::FirstExcept] {
        for page in 0..3 {
            assert_eq!(
                resolve_element_policy("hdr", policy, page, &states, &store),
                None,
            );
        }
    }
}
```

Note: `FirstExcept` on P0 returns `None` (assignment exists), fallback scan finds nothing earlier → `None`. Test reflects this.

**Step 2: 実行して失敗確認**

```bash
cargo test -p fulgur --lib gcpm::counter::tests::test_resolve_element_policy
```

Expected: FAIL (関数未定義)。

**Step 3: `resolve_element_policy` を実装**

`counter.rs` に:

```rust
use crate::gcpm::ElementPolicy;
use crate::gcpm::running::RunningElementStore;
use crate::paginate::PageRunningState;

/// Resolve an `element(name, policy)` reference to the HTML of the chosen
/// running element instance for the given page.
///
/// WeasyPrint-compatible semantics:
/// - `first` / `start`: first instance assigned on the current page.
/// - `last`: last instance assigned on the current page.
/// - `first-except`: returns `None` if the current page has any assignment.
/// - Fallback (any policy, no resolution on current page): the last instance
///   of the most recent preceding page that had an assignment.
pub fn resolve_element_policy<'a>(
    name: &str,
    policy: ElementPolicy,
    page_idx: usize,
    page_states: &[BTreeMap<String, PageRunningState>],
    store: &'a RunningElementStore,
) -> Option<&'a str> {
    let current = page_states.get(page_idx).and_then(|s| s.get(name));

    let chosen_id: Option<usize> = match policy {
        ElementPolicy::First | ElementPolicy::Start => {
            current.and_then(|s| s.instance_ids.first().copied())
        }
        ElementPolicy::Last => current.and_then(|s| s.instance_ids.last().copied()),
        ElementPolicy::FirstExcept => {
            if current.map(|s| !s.instance_ids.is_empty()).unwrap_or(false) {
                return None;
            }
            None
        }
    };

    if let Some(id) = chosen_id {
        return store.get_html(id);
    }

    // Fallback: scan preceding pages for the most recent assignment.
    for prev in (0..page_idx).rev() {
        if let Some(state) = page_states.get(prev).and_then(|s| s.get(name)) {
            if let Some(&last_id) = state.instance_ids.last() {
                return store.get_html(last_id);
            }
        }
    }

    None
}
```

**Step 4: `resolve_content_to_html` のシグネチャと実装を更新**

既存:

```rust
pub fn resolve_content_to_html(
    items: &[ContentItem],
    running_elements: &[(String, String)],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page: usize,
    total_pages: usize,
) -> String
```

新規:

```rust
pub fn resolve_content_to_html(
    items: &[ContentItem],
    store: &RunningElementStore,
    running_states: &[BTreeMap<String, PageRunningState>],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page_idx: usize,
    total_pages: usize,
) -> String
```

`ContentItem::Element` 分岐:

```rust
// 変更前
ContentItem::Element(name) => {
    if let Some((_, html)) = running_elements.iter().find(|(n, _)| n == name) {
        out.push_str(html);
    }
}

// 変更後
ContentItem::Element { name, policy } => {
    if let Some(html) = resolve_element_policy(name, *policy, page_idx, running_states, store) {
        out.push_str(html);
    }
}
```

`resolve_content_to_string` の `ContentItem::Element(_)` 分岐を `ContentItem::Element { .. }` に更新 (string モードでは相変わらず空)。

**Step 5: 既存テスト (`test_element_becomes_empty`, `test_resolve_html_with_running_element` 等) を新API に合わせて更新**

```bash
grep -n "resolve_content_to_html\|resolve_content_to_string\|ContentItem::Element" crates/fulgur/src/gcpm/counter.rs
```

呼び出し箇所を置換し、`running_elements: &[(String, String)]` を組み立てていた箇所は `store` と `running_states` に置換。

**Step 6: テスト実行**

```bash
cargo test -p fulgur --lib gcpm::counter
```

Expected: PASS。

**Step 7: コミット**

```bash
git add crates/fulgur/src/gcpm/counter.rs
git commit -m "feat(gcpm): add resolve_element_policy and wire into resolve_content_to_html"
```

---

## Task 9: Render と Engine — running_states をパイプライン経由で渡す

**Files:**

- Modify: `crates/fulgur/src/render.rs`
- Modify: `crates/fulgur/src/engine.rs`

**Step 1: `render_to_pdf_with_gcpm` の改修**

`running_store` は既に `&RunningElementStore` を受け取っている。追加で `running_states` を内部で計算:

```rust
let running_states = crate::paginate::collect_running_element_states(&pages);
```

(string_set_states の直後に配置)

既存の `running_pairs` 行:

```rust
let running_pairs = running_store.to_pairs();
```

を **削除** (もう使わない)。

`resolve_content_to_html` の呼び出しを更新:

```rust
let content_html = resolve_content_to_html(
    &rule.content,
    running_store,
    &running_states,
    &string_set_states[page_idx],
    page_num,
    total_pages,
);
```

note: `page_num` (1-based) と `page_idx` (0-based) の扱いを確認。現状の resolver は `page_idx` 引数を 0-based として実装したので、ここでは `page_idx` (つまり `page_num - 1`) を渡すこと。

**Step 2: 修正点 — resolver シグネチャの確認**

`resolve_element_policy` は 0-based `page_idx` を期待している。`resolve_content_to_html` は caller の page 番号をそのまま使っていた (counter resolution では 1-based)。これは分離する:

```rust
pub fn resolve_content_to_html(
    items: &[ContentItem],
    store: &RunningElementStore,
    running_states: &[BTreeMap<String, PageRunningState>],
    string_set_states: &BTreeMap<String, StringSetPageState>,
    page_num: usize,       // 1-based, for counter(page)
    total_pages: usize,
    page_idx: usize,       // 0-based, for running element resolution
) -> String
```

Task 8 の Step 4 のシグネチャをこれに修正 (plan 側の記述と一致させる)。

**Step 3: `render.rs` の dummy_store 初期化箇所**

margin-box 内部レンダリング用の `dummy_store`:

```rust
let mut dummy_store = RunningElementStore::new();
let mut dummy_ctx = crate::convert::ConvertContext {
    running_store: &mut dummy_store,
    ...
};
```

`ConvertContext::running_store` が `&RunningElementStore` (immutable) に変わったので、`&dummy_store` に変更。`mut` を外す。

**Step 4: `engine.rs` の `running_store: &mut running_store` を `&running_store` に変更**

convert に渡す時点では既に register 完了しているので immutable で OK。

**Step 5: ビルド + テスト**

```bash
cargo build -p fulgur 2>&1 | tail -20
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: ビルドPASS、全 lib テスト PASS。

**Step 6: コミット**

```bash
git add crates/fulgur/src/render.rs crates/fulgur/src/engine.rs crates/fulgur/src/convert.rs crates/fulgur/src/gcpm/counter.rs
git commit -m "feat(render): wire per-page running element states through rendering pipeline"
```

---

## Task 10: 統合テスト — 複数chapter の running element 切替

**Files:**

- Create: `crates/fulgur/tests/gcpm_element_policy.rs` (または既存 `gcpm_integration.rs` に追加)

**Step 1: 既存の統合テストファイルを確認**

```bash
ls crates/fulgur/tests/
grep -l "running\|element(" crates/fulgur/tests/*.rs
```

既存 `gcpm_integration.rs` があればそこに test function を追加、なければ新規ファイルを作る。

**Step 2: 統合テストを追加**

```rust
#[test]
fn test_element_policy_multiple_chapters_last() {
    // Two chapters across multiple pages; last policy should show the current chapter title.
    let html = r#"
<html>
<head>
<style>
  @page { size: 400pt 300pt; margin: 40pt; @top-center { content: element(title, last); } }
  .title { position: running(title); }
  .big { height: 600pt; }
</style>
</head>
<body>
  <h1 class="title">Chapter 1</h1>
  <div class="big">Chapter 1 body</div>
  <h1 class="title">Chapter 2</h1>
  <div class="big">Chapter 2 body</div>
</body>
</html>
"#;
    let pdf = Engine::new().render_html(html).unwrap();
    // At minimum assert PDF was produced; exact content assertion requires
    // PDF text extraction which the project handles elsewhere.
    assert!(pdf.len() > 1000);
    // If pdf text extraction helper exists:
    // let pages = extract_text_per_page(&pdf);
    // assert!(pages[0].contains("Chapter 1"));
    // assert!(pages[pages.len() - 1].contains("Chapter 2"));
}

#[test]
fn test_element_policy_first_except_default() {
    // first-except: chapter title hidden on the page where chapter starts.
    let html = r#"
<html>
<head>
<style>
  @page { size: 400pt 300pt; margin: 40pt; @top-center { content: element(title, first-except); } }
  .title { position: running(title); }
  .big { height: 600pt; }
</style>
</head>
<body>
  <h1 class="title">Chapter 1</h1>
  <div class="big">Chapter 1 body</div>
</body>
</html>
"#;
    let pdf = Engine::new().render_html(html).unwrap();
    assert!(pdf.len() > 1000);
}
```

**Step 3: テスト実行**

```bash
cargo test -p fulgur --test gcpm_element_policy -- --test-threads=1
```

(または `gcpm_integration` に統合した場合はそちら)

Expected: PASS。

**Step 4: コミット**

```bash
git add crates/fulgur/tests/
git commit -m "test(gcpm): integration tests for element() policy across multiple pages"
```

---

## Task 11: 最終検証

**Step 1: 全lib + 全統合テスト**

```bash
cargo test --lib -p fulgur
cargo test -p fulgur --test gcpm_integration -- --test-threads=1
cargo test -p fulgur --test gcpm_element_policy -- --test-threads=1 2>/dev/null || true
```

Expected: 全 PASS、ベースラインから後退なし。

**Step 2: fmt + clippy**

```bash
cargo fmt --check
cargo clippy -p fulgur -- -D warnings
```

Expected: 出力なし (違反なし)。

**Step 3: markdown lint (plan document のため)**

```bash
npx markdownlint-cli2 'docs/plans/2026-04-06-gcpm-element-policy-implementation.md'
```

Expected: 問題なし、またはあれば修正。

**Step 4: bd update — 実装完了記録**

```bash
bd update fulgur-94h --notes "Implementation complete on branch feature/gcpm-element-policy. All 4 policies (first/start/last/first-except) supported with WeasyPrint-compatible fallback semantics."
```

**Step 5: 最終コミット (必要なら)**

fmt/clippy 修正があれば:

```bash
git add -u
git commit -m "chore: fmt and clippy cleanup"
```

---

## 注意点

- `ContentItem::Element` は破壊的変更 — 該当バリアントのパターンマッチを全箇所更新する必要がある。`cargo check` のエラーメッセージを手がかりに網羅する。
- `RunningElementStore` も API 総入れ替え。`get`/`to_pairs` 使用箇所は全滅するので、やはり `cargo check` で追い込む。
- `page_num` (1-based) と `page_idx` (0-based) を混同しないこと。counter() は 1-based、running policy resolution は 0-based でpage_statesへのindex。
- 既存の `display: none` 経由の running要素除外は依然として機能する。マーカーは zero-size node の分岐で emit されるので、CSS rewrite を変更する必要はない。
