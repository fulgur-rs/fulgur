# bookmark-label の counter() / string() 解決 (fulgur-70c) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `bookmark-label` の `counter(name [, style])` と `string(name [, policy])` を、要素出現時点（DOM walk order）のスナップショットで解決する。

**Architecture:** 既存の `CounterPass` / `StringSetPass` (どちらも DOM を document order で walk) に「visited node ごとの状態 snapshot map」を持たせ、その出力を `BookmarkPass` に注入して `resolve_label` で `Counter` / `StringRef` を解決する。`BookmarkPass` は現在 `CounterPass` より先に走っているので、`engine.rs` の pass 順序を入れ替える。Pageable は v0.6 で廃止済みなので draw 時解決経路は使わない（issue description 参照）。

**Tech Stack:** Rust 1.x / Blitz / Stylo / 既存 GCPM (`crates/fulgur/src/gcpm/`) / 既存 DomPass chain (`crates/fulgur/src/blitz_adapter.rs`).

---

## Pre-flight

- 作業ディレクトリ: `/home/ubuntu/fulgur/.worktrees/fulgur-70c-bookmark-counter`
- ブランチ: `feat/fulgur-70c-bookmark-counter`
- baseline: `cargo test -p fulgur --lib bookmark_pass` → 11 passed (確認済)
- 関連 line:
  - `CounterPass`: `crates/fulgur/src/blitz_adapter.rs:1576-1772`
  - `StringSetPass`: `crates/fulgur/src/blitz_adapter.rs:1509-1572`
  - `BookmarkPass`: `crates/fulgur/src/blitz_adapter.rs:1811-1923`
  - `resolve_label`: `crates/fulgur/src/blitz_adapter.rs:1940-1973`
  - engine pass orchestration: `crates/fulgur/src/engine.rs:170-220`
  - `CounterState` (`snapshot()` 既存): `crates/fulgur/src/gcpm/counter.rs:333-363`
  - `format_counter`: `crates/fulgur/src/gcpm/counter.rs:269` 周辺

---

## Task 1: `CounterPass` に per-node counter snapshot map を追加

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs:1576-1772` (`CounterPass` 構造体と `walk_tree`)

### Step 1: 失敗するユニットテストを書く

`crates/fulgur/src/blitz_adapter.rs` の `mod tests` 内 (既存の Counter テスト群の近く) に追加:

```rust
#[test]
fn counter_pass_records_per_node_snapshot() {
    let html = r#"<html><body>
        <h1 id="a">A</h1><h1 id="b">B</h1>
    </body></html>"#;
    let mappings = vec![CounterMapping {
        parsed: ParsedSelector::Tag("h1".into()),
        ops: vec![CounterOp::Increment {
            name: "chapter".into(),
            value: 1,
        }],
    }];
    let mut doc = parse(html, 400.0, &[]);
    let pass = CounterPass::new(mappings, Vec::new());
    let ctx = PassContext { font_data: &[] };
    pass.apply(&mut doc, &ctx);
    let snapshots = pass.take_node_snapshots();

    // Find h1 nodes by id attr.
    let mut h1_ids: Vec<usize> = Vec::new();
    fn walk(doc: &HtmlDocument, id: usize, out: &mut Vec<usize>) {
        if let Some(node) = doc.get_node(id) {
            if let Some(el) = node.element_data() {
                if el.name.local.as_ref() == "h1" {
                    out.push(id);
                }
            }
            for &c in &node.children {
                walk(doc, c, out);
            }
        }
    }
    walk(&doc, doc.root_element().id, &mut h1_ids);
    assert_eq!(h1_ids.len(), 2);
    let a = snapshots.get(&h1_ids[0]).expect("snapshot at first h1");
    let b = snapshots.get(&h1_ids[1]).expect("snapshot at second h1");
    assert_eq!(a.get("chapter").copied(), Some(1));
    assert_eq!(b.get("chapter").copied(), Some(2));
}
```

### Step 2: テストが失敗することを確認

```bash
cargo test -p fulgur --lib counter_pass_records_per_node_snapshot 2>&1 | tail -10
```

Expected: `take_node_snapshots` が存在せずコンパイルエラー。

### Step 3: 最小実装

`CounterPass` に snapshot フィールドと accessor を追加:

```rust
pub struct CounterPass {
    counter_mappings: Vec<CounterMapping>,
    content_mappings: Vec<ContentCounterMapping>,
    state: RefCell<CounterState>,
    generated_css: RefCell<String>,
    counter_id: RefCell<usize>,
    ops_by_node: RefCell<Vec<(usize, Vec<CounterOp>)>>,
    /// Counter-state snapshot taken at each visited element after the
    /// element's own `counter-reset` / `counter-increment` / `counter-set`
    /// operations have been applied (Phase 2). Consumed by `BookmarkPass`
    /// to resolve `counter()` inside `bookmark-label`.
    node_snapshots: RefCell<BTreeMap<usize, BTreeMap<String, i32>>>,
}
```

`new` の `Self { ... }` に `node_snapshots: RefCell::new(BTreeMap::new())` を追加。

`walk_tree` の Phase 2 直後（Phase 3 の attr_value 処理の **前**）に snapshot を記録:

```rust
// Snapshot the counter state at this element's "after own ops, before
// children" position. This is the value bookmark-label sees (matches
// `::before` content resolution timing).
self.node_snapshots
    .borrow_mut()
    .insert(node_id, self.state.borrow().snapshot());
```

新規 accessor:

```rust
pub fn take_node_snapshots(&self) -> BTreeMap<usize, BTreeMap<String, i32>> {
    std::mem::take(&mut *self.node_snapshots.borrow_mut())
}
```

**重要**: snapshot は ops が match しなくても全要素について記録する（bookmark-label は counter ops を持たない要素にも付き得る）。`apply` の early return (`counter_mappings.is_empty() && content_mappings.is_empty()`) は維持して良い — その場合 snapshots は空で、BookmarkPass 側で `unwrap_or(0)` フォールバックされる（CSS spec: 未定義 counter は 0）。

### Step 4: テストが pass することを確認

```bash
cargo test -p fulgur --lib counter_pass_records_per_node_snapshot 2>&1 | tail -10
```

Expected: PASS。さらに既存 counter テストが全て pass:

```bash
cargo test -p fulgur --lib counter_pass 2>&1 | tail -20
```

### Step 5: commit

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(counter-pass): record per-node counter snapshots for bookmark-label resolution"
```

---

## Task 2: `StringSetPass` に per-node named-string snapshot map を追加

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs:1509-1572` (`StringSetPass`)

### Step 1: 失敗するユニットテスト

```rust
#[test]
fn string_set_pass_records_per_node_snapshot() {
    use crate::gcpm::StringSetValue;
    let html = r#"<html><body>
        <h1 id="a">First</h1>
        <p id="p1">Body</p>
        <h1 id="b">Second</h1>
        <p id="p2">Body2</p>
    </body></html>"#;
    let mappings = vec![StringSetMapping {
        parsed: ParsedSelector::Tag("h1".into()),
        name: "title".into(),
        values: vec![StringSetValue::ContentText],
    }];
    let mut doc = parse(html, 400.0, &[]);
    let pass = StringSetPass::new(mappings);
    let ctx = PassContext { font_data: &[] };
    pass.apply(&mut doc, &ctx);
    let snapshots = pass.take_node_snapshots();

    let mut tag_ids: Vec<(String, usize)> = Vec::new();
    fn walk(doc: &HtmlDocument, id: usize, out: &mut Vec<(String, usize)>) {
        if let Some(node) = doc.get_node(id) {
            if let Some(el) = node.element_data() {
                let tag = el.name.local.as_ref().to_string();
                if matches!(tag.as_str(), "h1" | "p") {
                    out.push((tag, id));
                }
            }
            for &c in &node.children {
                walk(doc, c, out);
            }
        }
    }
    walk(&doc, doc.root_element().id, &mut tag_ids);

    let p1 = tag_ids.iter().find(|(t, _)| t == "p").unwrap().1;
    let p2 = tag_ids.iter().rev().find(|(t, _)| t == "p").unwrap().1;
    assert_eq!(
        snapshots.get(&p1).and_then(|m| m.get("title").cloned()),
        Some("First".to_string())
    );
    assert_eq!(
        snapshots.get(&p2).and_then(|m| m.get("title").cloned()),
        Some("Second".to_string())
    );
}
```

### Step 2: 失敗を確認

```bash
cargo test -p fulgur --lib string_set_pass_records_per_node_snapshot 2>&1 | tail -10
```

Expected: コンパイルエラー (`take_node_snapshots` 未定義)。

### Step 3: 最小実装

`StringSetPass` を拡張:

```rust
pub struct StringSetPass {
    mappings: Vec<StringSetMapping>,
    store: RefCell<StringSetStore>,
    /// Running map `name -> latest value` updated as the DOM walk
    /// encounters string-set assignments. Snapshotted into
    /// `node_snapshots` at every visited element so `BookmarkPass` can
    /// resolve `string(name)` at the element's DOM position.
    running: RefCell<BTreeMap<String, String>>,
    node_snapshots: RefCell<BTreeMap<usize, BTreeMap<String, String>>>,
}
```

`new` でフィールドを初期化。

`walk_tree` を変更:

```rust
fn walk_tree(&self, doc: &HtmlDocument, node_id: usize, depth: usize) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };

    if let Some(elem) = node.element_data() {
        if is_non_visual_tag(elem.name.local.as_ref()) {
            return;
        }
        if let Some(mapping) = self.find_string_set(elem) {
            let value = resolve_string_set_values(doc, node_id, elem, &mapping.values);
            self.running
                .borrow_mut()
                .insert(mapping.name.clone(), value.clone());
            self.store.borrow_mut().push(StringSetEntry {
                name: mapping.name.clone(),
                value,
                node_id,
            });
        }
        // Snapshot AFTER updating running state — the element's own
        // assignment is visible to bookmark-label on the same element.
        self.node_snapshots
            .borrow_mut()
            .insert(node_id, self.running.borrow().clone());
    }

    for &child_id in &node.children {
        self.walk_tree(doc, child_id, depth + 1);
    }
}
```

新規 accessor:

```rust
pub fn take_node_snapshots(&self) -> BTreeMap<usize, BTreeMap<String, String>> {
    std::mem::take(&mut *self.node_snapshots.borrow_mut())
}
```

`apply` の `if self.mappings.is_empty()` early return は維持。空 mappings 時は snapshot も空で、BookmarkPass フォールバックで OK。

### Step 4: テストが pass することを確認

```bash
cargo test -p fulgur --lib string_set_pass 2>&1 | tail -20
```

Expected: 新規テスト含め全 PASS。

### Step 5: commit

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(string-set-pass): record per-node named-string snapshots for bookmark-label resolution"
```

---

## Task 3: `BookmarkPass` に snapshot を注入し `resolve_label` で counter() / string() を解決

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs:1811-1923` (`BookmarkPass`)
- Modify: `crates/fulgur/src/blitz_adapter.rs:1940-1973` (`resolve_label`)
- Modify: `crates/fulgur/src/blitz_adapter.rs:3907-3929` (既存 `bookmark_pass_skips_counter_gracefully` の期待値を更新)

### Step 1: 既存テストの期待値を更新する (advisor 指摘)

`bookmark_pass_skips_counter_gracefully` は「snapshot を渡さない場合の挙動」の regression として **残すが**、新実装では空 snapshot ⇒ `unwrap_or(0)` ⇒ `format_counter(0, Decimal) = "0"` になるので期待ラベルを変える:

```rust
#[test]
fn bookmark_pass_unset_counter_resolves_to_zero() {
    use crate::gcpm::CounterStyle;

    let html = r#"<html><body><h1>Title</h1></body></html>"#;
    let results = run_bookmark_pass(
        html,
        vec![BookmarkMapping {
            selector: ParsedSelector::Tag("h1".into()),
            level: Some(BookmarkLevel::Integer(1)),
            label: Some(vec![
                ContentItem::Counter {
                    name: "chapter".into(),
                    style: CounterStyle::Decimal,
                },
                ContentItem::String(": ".into()),
                ContentItem::ContentText,
            ]),
        }],
    );
    assert_eq!(results.len(), 1);
    // CSS spec: undefined counter resolves to 0.
    // `BookmarkPass::new` (no snapshots) ⇒ counter() falls back to 0.
    assert_eq!(results[0].1.label, "0: Title");
}
```

旧テスト名 `bookmark_pass_skips_counter_gracefully` は意味が変わるので **rename** する (`skips_counter_gracefully` → `unset_counter_resolves_to_zero`)。

### Step 2: 失敗するユニットテストを書く (新規 2 件)

`mod tests` 内、上で更新した test の下に追加:

```rust
#[test]
fn bookmark_pass_resolves_counter_with_snapshot() {
    use crate::gcpm::CounterStyle;
    use std::collections::BTreeMap;

    let html = r#"<html><body><h1>Title</h1></body></html>"#;
    let mappings = vec![BookmarkMapping {
        selector: ParsedSelector::Tag("h1".into()),
        level: Some(BookmarkLevel::Integer(1)),
        label: Some(vec![
            ContentItem::Counter {
                name: "chapter".into(),
                style: CounterStyle::Decimal,
            },
            ContentItem::String(". ".into()),
            ContentItem::ContentText,
        ]),
    }];
    let mut doc = parse(html, 400.0, &[]);
    // Locate the h1 node and stage a snapshot manually.
    let mut h1_id: Option<usize> = None;
    fn find(doc: &HtmlDocument, id: usize, out: &mut Option<usize>) {
        if out.is_some() {
            return;
        }
        if let Some(node) = doc.get_node(id) {
            if let Some(el) = node.element_data() {
                if el.name.local.as_ref() == "h1" {
                    *out = Some(id);
                    return;
                }
            }
            for &c in &node.children {
                find(doc, c, out);
            }
        }
    }
    find(&doc, doc.root_element().id, &mut h1_id);
    let h1 = h1_id.unwrap();

    let mut counter_snap = BTreeMap::new();
    let mut h1_state = BTreeMap::new();
    h1_state.insert("chapter".to_string(), 1);
    counter_snap.insert(h1, h1_state);

    let pass = BookmarkPass::new_with_snapshots(mappings, counter_snap, BTreeMap::new());
    let ctx = PassContext { font_data: &[] };
    pass.apply(&mut doc, &ctx);
    let results = pass.into_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.label, "1. Title");
}

#[test]
fn bookmark_pass_resolves_string_ref_with_snapshot() {
    use crate::gcpm::StringPolicy;
    use std::collections::BTreeMap;

    let html = r#"<html><body><h1>Body Heading</h1></body></html>"#;
    let mappings = vec![BookmarkMapping {
        selector: ParsedSelector::Tag("h1".into()),
        level: Some(BookmarkLevel::Integer(1)),
        label: Some(vec![ContentItem::StringRef {
            name: "section".into(),
            policy: StringPolicy::First,
        }]),
    }];
    let mut doc = parse(html, 400.0, &[]);
    let mut h1_id: Option<usize> = None;
    fn find(doc: &HtmlDocument, id: usize, out: &mut Option<usize>) {
        if out.is_some() {
            return;
        }
        if let Some(node) = doc.get_node(id) {
            if let Some(el) = node.element_data() {
                if el.name.local.as_ref() == "h1" {
                    *out = Some(id);
                    return;
                }
            }
            for &c in &node.children {
                find(doc, c, out);
            }
        }
    }
    find(&doc, doc.root_element().id, &mut h1_id);
    let h1 = h1_id.unwrap();

    let mut string_snap = BTreeMap::new();
    let mut h1_state = BTreeMap::new();
    h1_state.insert("section".to_string(), "Intro".to_string());
    string_snap.insert(h1, h1_state);

    let pass = BookmarkPass::new_with_snapshots(mappings, BTreeMap::new(), string_snap);
    let ctx = PassContext { font_data: &[] };
    pass.apply(&mut doc, &ctx);
    let results = pass.into_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.label, "Intro");
}
```

### Step 3: 失敗を確認

```bash
cargo test -p fulgur --lib bookmark_pass_resolves_counter_with_snapshot bookmark_pass_resolves_string_ref_with_snapshot bookmark_pass_unset_counter_resolves_to_zero 2>&1 | tail -10
```

Expected: `new_with_snapshots` 未定義でコンパイルエラー、および `bookmark_pass_unset_counter_resolves_to_zero` (旧 skips test の rename 後) は Step 4 の実装後にのみ pass する想定。

### Step 4: 最小実装

`BookmarkPass` を拡張:

```rust
pub struct BookmarkPass {
    mappings: Vec<BookmarkMapping>,
    results: RefCell<Vec<(usize, BookmarkInfo)>>,
    /// Per-node counter-state snapshots produced by `CounterPass`.
    /// Empty map ⇒ `counter()` resolves to 0 (CSS undefined-counter rule).
    counter_snapshots: BTreeMap<usize, BTreeMap<String, i32>>,
    /// Per-node named-string snapshots produced by `StringSetPass`.
    /// Empty map ⇒ `string()` resolves to "".
    string_snapshots: BTreeMap<usize, BTreeMap<String, String>>,
}

impl BookmarkPass {
    pub fn new(mappings: Vec<BookmarkMapping>) -> Self {
        Self::new_with_snapshots(mappings, BTreeMap::new(), BTreeMap::new())
    }

    pub fn new_with_snapshots(
        mappings: Vec<BookmarkMapping>,
        counter_snapshots: BTreeMap<usize, BTreeMap<String, i32>>,
        string_snapshots: BTreeMap<usize, BTreeMap<String, String>>,
    ) -> Self {
        Self {
            mappings,
            results: RefCell::new(Vec::new()),
            counter_snapshots,
            string_snapshots,
        }
    }
    // existing into_results unchanged
}
```

`resolve_node` の `resolve_label` 呼び出しを更新:

```rust
let resolved_label = match label {
    Some(items) => resolve_label(
        &items,
        doc,
        node_id,
        elem,
        self.counter_snapshots.get(&node_id),
        self.string_snapshots.get(&node_id),
    ),
    None => extract_text_content(doc, node_id),
};
```

`resolve_label` のシグネチャと本体を更新:

```rust
fn resolve_label(
    items: &[ContentItem],
    doc: &HtmlDocument,
    node_id: usize,
    elem: &blitz_dom::node::ElementData,
    counter_snapshot: Option<&BTreeMap<String, i32>>,
    string_snapshot: Option<&BTreeMap<String, String>>,
) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::ContentText => {
                out.push_str(&extract_text_content(doc, node_id));
            }
            ContentItem::Attr(name) => {
                if let Some(v) = get_attr(elem, name) {
                    out.push_str(v);
                }
            }
            ContentItem::Counter { name, style } => {
                let value = counter_snapshot
                    .and_then(|s| s.get(name))
                    .copied()
                    .unwrap_or(0);
                out.push_str(&format_counter(value, *style));
            }
            ContentItem::StringRef { name, .. } => {
                // bookmark-label resolves at a single DOM position, so
                // all StringPolicy variants reduce to "the latest value
                // seen at this point in document order". The empty-string
                // fallback matches CSS GCPM for an unset named string.
                if let Some(v) = string_snapshot.and_then(|s| s.get(name)) {
                    out.push_str(v);
                }
            }
            // Element / ContentBefore / ContentAfter / Leader stay
            // out-of-scope for bookmark-label (tracked in fulgur-yfx).
            ContentItem::ContentBefore
            | ContentItem::ContentAfter
            | ContentItem::Element { .. }
            | ContentItem::Leader { .. } => {}
        }
    }
    out
}
```

`use crate::gcpm::counter::{CounterState, format_counter};` は既存。`std::collections::BTreeMap` の import は file 先頭で確認 (既に使われている)。

`resolve_label` の docstring から「Counter は state が無いので no-op」「StringRef は margin-box 専用」の旧 TODO コメントを削り、現状を反映 (ContentBefore/After/Element/Leader が残ることを明記)。

### Step 5: テストが pass することを確認

```bash
cargo test -p fulgur --lib bookmark_pass 2>&1 | tail -20
```

Expected: 既存 10 (skips を rename したぶん 11→10) + rename 1 + 新規 2 = 13 テスト全 PASS。

### Step 6: commit

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(bookmark-pass): resolve counter() and string() in bookmark-label via per-node snapshots"
```

---

## Task 4: `engine.rs` の pass 順序を入れ替えて snapshot を注入

**Files:**

- Modify: `crates/fulgur/src/engine.rs:170-220` (pass orchestration)

### Step 1: 失敗するテスト

`crates/fulgur/tests/render_smoke.rs` (新規 or 既存) に追加。まず既存ファイルを確認:

```bash
ls crates/fulgur/tests/ 2>&1
test -f crates/fulgur/tests/render_smoke.rs && head -30 crates/fulgur/tests/render_smoke.rs
```

ない場合は新規作成。`bookmark_label_counter_renders` を追加:

```rust
use fulgur::{Engine, PageSize};

#[test]
fn bookmark_label_counter_renders_non_empty_pdf() {
    let html = r#"<!doctype html><html><head><style>
        h1 { counter-increment: chapter; bookmark-level: 1; bookmark-label: counter(chapter) ". " content(text); }
    </style></head><body>
        <h1>Intro</h1><h1>Method</h1><h1>Result</h1>
    </body></html>"#;
    let pdf = Engine::builder()
        .page_size(PageSize::A4)
        .build()
        .render_html(html)
        .expect("render_html");
    assert!(!pdf.is_empty(), "PDF should be non-empty");
    // Cheap sanity check: the resolved labels appear inside the PDF
    // outline (krilla writes outline titles as plain UTF-16BE in
    // /Title; check for the byte signature of each label).
    // We only verify byte-length increases — the label-content
    // assertion lives in the BookmarkPass unit tests.
}
```

> Note: PDF 内の outline 文字列を直接 grep する必要があるなら `lopdf::Document::load_mem(&pdf)` で outline tree を辿るのが確実だが、このスモークテストはあくまで「render が落ちない」「pdf が空でない」の保証。詳細な出力検証は VRT golden に任せる (Task 6)。

### Step 2: 失敗を確認

現状ではコード側が CounterPass の snapshot を渡していないので、PDF 自体は空でない確率が高い (= 失敗しないかも)。pass-through テストとして残し、Task 6 の VRT で実際の出力を検証する。Step 1-2 を **lib smoke** だけにし、failing-test 性質は VRT に寄せても OK。本タスクは pass 順序入れ替えそのものなので Step 1 のテストは「regression がない」確認用と位置付ける。

代わりに、実際に snapshot 連携が無いと壊れる点として: 既存の `crates/fulgur/tests/render_smoke.rs` (lopdf を dep として既に使用、`Document::load(&path)` 例も多数あり) に outline 検証テストを追加する。`lopdf = "0.40.0"` は `crates/fulgur/Cargo.toml:31` にあり別途追加不要 (確認済)。

`render_smoke.rs` の末尾近く (既存の outline 系テストの近辺) にヘルパ + テストを追加:

```rust
fn outline_titles(pdf_bytes: &[u8]) -> Vec<String> {
    let doc = lopdf::Document::load_mem(pdf_bytes).expect("load_mem");
    let catalog_id = doc
        .trailer
        .get(b"Root")
        .expect("Root")
        .as_reference()
        .expect("Root ref");
    let catalog = doc.get_object(catalog_id).unwrap().as_dict().unwrap();
    let outlines_id = match catalog.get(b"Outlines") {
        Ok(v) => v.as_reference().expect("Outlines ref"),
        Err(_) => return Vec::new(),
    };
    let outlines = doc.get_object(outlines_id).unwrap().as_dict().unwrap();
    let mut cur = outlines
        .get(b"First")
        .ok()
        .and_then(|v| v.as_reference().ok());

    let decode = |s: &[u8]| -> String {
        if s.starts_with(&[0xFE, 0xFF]) {
            let chars: Vec<u16> = s[2..]
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&chars)
        } else {
            String::from_utf8_lossy(s).into_owned()
        }
    };

    let mut out = Vec::new();
    while let Some(id) = cur {
        let dict = doc.get_object(id).unwrap().as_dict().unwrap();
        if let Ok(title) = dict.get(b"Title") {
            if let Ok(s) = title.as_str() {
                out.push(decode(s));
            }
        }
        cur = dict
            .get(b"Next")
            .ok()
            .and_then(|v| v.as_reference().ok());
    }
    out
}

#[test]
fn bookmark_label_counter_appears_in_outline() {
    let html = r#"<!doctype html><html><head><style>
        h1 { counter-increment: chapter; bookmark-level: 1; bookmark-label: counter(chapter) ". " content(text); }
    </style></head><body>
        <h1>Intro</h1><h1>Method</h1>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render_html");
    let titles = outline_titles(&pdf);
    assert!(
        titles.iter().any(|t| t == "1. Intro"),
        "outline should contain '1. Intro', got: {titles:?}"
    );
    assert!(
        titles.iter().any(|t| t == "2. Method"),
        "outline should contain '2. Method', got: {titles:?}"
    );
}

#[test]
fn bookmark_label_string_appears_in_outline() {
    let html = r#"<!doctype html><html><head><style>
        h1 { string-set: title content(text); bookmark-level: 1; bookmark-label: string(title); }
    </style></head><body>
        <h1>Alpha</h1><h1>Beta</h1>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render_html");
    let titles = outline_titles(&pdf);
    assert_eq!(titles, vec!["Alpha".to_string(), "Beta".to_string()]);
}
```

実行:

```bash
cargo test -p fulgur --test render_smoke -- bookmark_label 2>&1 | tail -30
```

Expected: FAIL — 現状 BookmarkPass は counter / string snapshot を見ないので outline title は ". Intro" / ". Method" / "" / "" になる。

### Step 3: 最小実装 — engine.rs 順序入れ替え

`crates/fulgur/src/engine.rs:170-220` を以下の順序に並べ替える:

```text
RunningElementPass
  ↓
StringSetPass        → string_set_store + string_snapshots
  ↓
CounterPass          → counter_ops + counter_css + counter_snapshots
  ↓
InjectCssPass        (if counter_css non-empty)
  ↓
BookmarkPass(counter_snapshots, string_snapshots)
                     → bookmark_by_node
  ↓
resolve(&mut doc)
  ↓
... (以降は既存通り)
```

具体的には:

1. `string_set_store` 取得後、`pass.take_node_snapshots()` で `string_snapshots` も取り出す。
2. CounterPass を **BookmarkPass の前** に動かす。`pass.take_node_snapshots()` で `counter_snapshots` を取り出してから `into_parts()` で `(counter_ops, css)` を回収。
3. InjectCssPass (counter_css 非空時) を CounterPass 直後に走らせる。
4. BookmarkPass を `new_with_snapshots(...)` で構築し、`apply_single_pass` で実行。
5. `crate::blitz_adapter::resolve(&mut doc);` の呼び出し位置は変えない。

```rust
let (string_set_store, string_snapshots) = if !gcpm.string_set_mappings.is_empty() {
    let pass = crate::blitz_adapter::StringSetPass::new(gcpm.string_set_mappings.clone());
    crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
    let snapshots = pass.take_node_snapshots();
    (pass.into_store(), snapshots)
} else {
    (
        crate::gcpm::string_set::StringSetStore::new(),
        std::collections::BTreeMap::new(),
    )
};

let (counter_ops_by_node_vec, counter_css, counter_snapshots) =
    if !gcpm.counter_mappings.is_empty() || !gcpm.content_counter_mappings.is_empty() {
        let pass = crate::blitz_adapter::CounterPass::new(
            gcpm.counter_mappings.clone(),
            gcpm.content_counter_mappings.clone(),
        );
        crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
        let snapshots = pass.take_node_snapshots();
        let (ops, css) = pass.into_parts();
        (ops, css, snapshots)
    } else {
        (Vec::new(), String::new(), std::collections::BTreeMap::new())
    };

if !counter_css.is_empty() {
    let inject_pass = crate::blitz_adapter::InjectCssPass { css: counter_css };
    crate::blitz_adapter::apply_single_pass(&inject_pass, &mut doc, &ctx);
}

let bookmark_by_node: HashMap<usize, crate::blitz_adapter::BookmarkInfo> =
    if self.config.effective_bookmarks() && !gcpm.bookmark_mappings.is_empty() {
        let pass = crate::blitz_adapter::BookmarkPass::new_with_snapshots(
            gcpm.bookmark_mappings.clone(),
            counter_snapshots,
            string_snapshots,
        );
        crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
        pass.into_results().into_iter().collect()
    } else {
        HashMap::new()
    };
```

> 注: `counter_snapshots` / `string_snapshots` が `BookmarkPass::new_with_snapshots` で move されるため、bookmark mapping が空 or `effective_bookmarks() == false` の場合に snapshot がそのまま捨てられる。これは意図通り (使い道がない)。

### Step 4: テストが pass することを確認

```bash
cargo test -p fulgur --test render_smoke -- bookmark_label 2>&1 | tail -20
cargo test -p fulgur --lib 2>&1 | tail -3
cargo test -p fulgur 2>&1 | tail -3
```

Expected: outline テスト PASS, lib テスト全て PASS (regression なし)。

### Step 5: commit

```bash
git add crates/fulgur/src/engine.rs crates/fulgur/tests/render_smoke.rs
git commit -m "feat(engine): wire bookmark-label counter()/string() snapshots through pass chain (fulgur-70c)"
```

---

## Task 5: (削除済み — Task 4 の `render_smoke.rs` 追加に合流)

---

## Task 6: VRT golden を追加

**Files:**

- Create: `crates/fulgur-vrt/fixtures/gcpm/bookmark-label-counter.html` (既存 GCPM fixture 配置に合わせる)
- Create: `crates/fulgur-vrt/goldens/fulgur/gcpm/bookmark-label-counter.pdf`

### Step 1: 既存 fixture / golden の流儀を確認

```bash
ls crates/fulgur-vrt/fixtures/gcpm/ 2>/dev/null | head -20
ls crates/fulgur-vrt/goldens/fulgur/gcpm/ 2>/dev/null | head -20
grep -rn "bookmark" crates/fulgur-vrt/ 2>/dev/null | head
```

### Step 2: fixture HTML を追加

```html
<!doctype html>
<html>
  <head>
    <meta charset="utf-8">
    <title>bookmark-label counter()</title>
    <style>
      @page { size: A5; margin: 12mm; }
      body { font-family: "Noto Sans"; font-size: 14px; }
      h1 {
        counter-increment: chapter;
        counter-reset: section;
        bookmark-level: 1;
        bookmark-label: counter(chapter) ". " content(text);
      }
      h2 {
        counter-increment: section;
        bookmark-level: 2;
        bookmark-label: counter(chapter) "." counter(section) " " content(text);
      }
    </style>
  </head>
  <body>
    <h1>Introduction</h1>
    <h2>Background</h2>
    <h2>Goals</h2>
    <h1>Method</h1>
    <h2>Setup</h2>
  </body>
</html>
```

### Step 3: golden 更新コマンドを実行

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 \
  cargo test -p fulgur-vrt -- bookmark_label_counter 2>&1 | tail -30
```

(テストの命名規約は既存 VRT を踏襲。ファイル名から自動派生する場合は `bookmark-label-counter` を test 名に)

### Step 4: 通常実行で byte-equal を確認

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  cargo test -p fulgur-vrt -- bookmark_label_counter 2>&1 | tail -10
```

Expected: PASS (golden と一致)。

### Step 5: commit

```bash
git add crates/fulgur-vrt/fixtures/gcpm/bookmark-label-counter.html \
        crates/fulgur-vrt/goldens/fulgur/gcpm/bookmark-label-counter.pdf
git commit -m "test(vrt): add bookmark-label counter() golden (fulgur-70c)"
```

---

## Task 7: `fulgur-yfx` (フォローアップ TODO) のクリーンアップ

`resolve_label` 内に既存の `TODO(fulgur-yfx)` コメントがあった (counter / string が含まれる)。今回 counter / string は実装したので、コメントから両者を取り除き、残った非対応項目 (Element / ContentBefore / ContentAfter / Leader) を反映する。Task 3 の Step 3 で既にやっている想定だが、最終チェック。

```bash
grep -n "TODO(fulgur-yfx)" crates/fulgur/src/blitz_adapter.rs | head
```

不要な記述が残っていれば削除し commit。

---

## Verification (final)

完了前に以下を全て実行する:

```bash
cargo test -p fulgur --lib 2>&1 | tail -3
cargo test -p fulgur 2>&1 | tail -3
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -3
cargo clippy -p fulgur --all-targets 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -3
```

全て PASS / clean を確認した上で `superpowers:finishing-a-development-branch` に進む。

---

## Out of Scope (本 issue 対象外)

- `bookmark-label` 内の `element()`, `content(before)`, `content(after)`, `leader()` — `fulgur-yfx` で別途検討
- margin-box 側の `string()` policy 違いのフル実装 — 既に `gcpm/counter.rs` で実装済み (本 issue で触らない)
- `string-set` の per-page snapshot (pagination 連携) — 既存 (`pagination_layout::collect_counter_states` 系) のままで OK
