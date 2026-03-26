# Running要素検出のDomPass化 実装プラン

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Running要素の検出・シリアライズ・格納ロジックをconvert.rsからblitz_adapter.rsのDomPassパイプラインに移動する

**Architecture:** RunningElementPassがGcpmContextとRunningElementStoreを内包し、DomPassとしてDOM走査→Running要素検出→HTMLシリアライズ→格納を行う。Pass実行後にinto_running_store()でストアを取り出し、ConvertContextに渡す。PassContextは変更しない。

**Tech Stack:** Rust, Blitz DOM API (blitz_dom, blitz_html)

---

## Task 1: RunningElementPass構造体とDomPass実装（blitz_adapter.rs）

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs`
- Reference: `crates/fulgur/src/gcpm/mod.rs` (GcpmContext, RunningMapping, ParsedSelector)
- Reference: `crates/fulgur/src/gcpm/running.rs` (RunningElementStore, serialize_node)
- Reference: `crates/fulgur/src/convert.rs:266-309` (移植元のis_running_element, matches_selector, get_running_name)

**Step 1: テスト作成**

`blitz_adapter.rs`のtestsモジュール末尾に以下のテストを追加:

```rust
#[test]
fn test_running_element_pass_extracts_by_class() {
    let html = r#"<html><head><style>.header { display: none; }</style></head><body>
        <div class="header">Header Content</div>
        <p>Body text</p>
    </body></html>"#;
    let mut doc = parse(html, 400.0, &[]);

    let gcpm = crate::gcpm::GcpmContext {
        margin_boxes: vec![],
        running_mappings: vec![crate::gcpm::RunningMapping {
            parsed: crate::gcpm::ParsedSelector::Class("header".to_string()),
            running_name: "pageHeader".to_string(),
        }],
        cleaned_css: String::new(),
    };

    let pass = RunningElementPass::new(gcpm);
    let ctx = PassContext {
        viewport_width: 400.0,
        viewport_height: 10000.0,
        font_data: &[],
    };
    pass.apply(&mut doc, &ctx);

    let store = pass.into_running_store();
    assert!(
        store.get("pageHeader").is_some(),
        "Expected running element 'pageHeader' to be extracted"
    );
    let html_content = store.get("pageHeader").unwrap();
    assert!(
        html_content.contains("Header Content"),
        "Expected serialized HTML to contain 'Header Content', got: {html_content}"
    );
}

#[test]
fn test_running_element_pass_extracts_by_id() {
    let html = r#"<html><head><style>#title { display: none; }</style></head><body>
        <h1 id="title">Doc Title</h1>
        <p>Body text</p>
    </body></html>"#;
    let mut doc = parse(html, 400.0, &[]);

    let gcpm = crate::gcpm::GcpmContext {
        margin_boxes: vec![],
        running_mappings: vec![crate::gcpm::RunningMapping {
            parsed: crate::gcpm::ParsedSelector::Id("title".to_string()),
            running_name: "pageTitle".to_string(),
        }],
        cleaned_css: String::new(),
    };

    let pass = RunningElementPass::new(gcpm);
    let ctx = PassContext {
        viewport_width: 400.0,
        viewport_height: 10000.0,
        font_data: &[],
    };
    pass.apply(&mut doc, &ctx);

    let store = pass.into_running_store();
    assert!(store.get("pageTitle").is_some());
    assert!(store.get("pageTitle").unwrap().contains("Doc Title"));
}

#[test]
fn test_running_element_pass_no_mappings_is_noop() {
    let html = "<html><body><p>Hello</p></body></html>";
    let mut doc = parse(html, 400.0, &[]);

    let gcpm = crate::gcpm::GcpmContext {
        margin_boxes: vec![],
        running_mappings: vec![],
        cleaned_css: String::new(),
    };

    let pass = RunningElementPass::new(gcpm);
    let ctx = PassContext {
        viewport_width: 400.0,
        viewport_height: 10000.0,
        font_data: &[],
    };
    pass.apply(&mut doc, &ctx);

    let store = pass.into_running_store();
    assert!(store.get("anything").is_none());
}
```

**Step 2: テストが失敗することを確認**

Run: `cargo test --lib -p fulgur test_running_element_pass`
Expected: コンパイルエラー（RunningElementPass未定義）

**Step 3: RunningElementPass実装**

`blitz_adapter.rs`の`InjectCssPass`実装の後（196行目以降）、testsモジュールの前に以下を追加:

```rust
use std::cell::RefCell;
use crate::gcpm::GcpmContext;
use crate::gcpm::ParsedSelector;
use crate::gcpm::running::{RunningElementStore, serialize_node};

/// Extracts running elements from the DOM and stores their serialized HTML.
///
/// Running elements are identified by matching DOM nodes against parsed CSS
/// selectors in the GCPM context's `running_mappings`. Matched nodes are
/// serialized to HTML and stored in an internal `RunningElementStore`.
///
/// After applying this pass, call `into_running_store()` to retrieve the store.
pub struct RunningElementPass {
    gcpm: GcpmContext,
    store: RefCell<RunningElementStore>,
}

impl RunningElementPass {
    pub fn new(gcpm: GcpmContext) -> Self {
        Self {
            gcpm,
            store: RefCell::new(RunningElementStore::new()),
        }
    }

    /// Consume this pass and return the collected running elements.
    pub fn into_running_store(self) -> RunningElementStore {
        self.store.into_inner()
    }
}

impl DomPass for RunningElementPass {
    fn apply(&self, doc: &mut HtmlDocument, _ctx: &PassContext<'_>) {
        if self.gcpm.running_mappings.is_empty() {
            return;
        }
        let root = doc.root_element();
        self.walk_tree(doc, root.id);
    }
}

impl RunningElementPass {
    fn walk_tree(&self, doc: &HtmlDocument, node_id: usize) {
        let Some(node) = doc.get_node(node_id) else {
            return;
        };

        if let Some(elem) = node.element_data() {
            if let Some(running_name) = self.find_running_name(elem) {
                let html = serialize_node(doc, node_id);
                self.store.borrow_mut().register(running_name, html);
                return; // Don't recurse into running elements
            }
        }

        for &child_id in &node.children {
            self.walk_tree(doc, child_id);
        }
    }

    fn find_running_name(&self, elem: &blitz_dom::node::ElementData) -> Option<String> {
        self.gcpm
            .running_mappings
            .iter()
            .find(|m| self.matches_selector(&m.parsed, elem))
            .map(|m| m.running_name.clone())
    }

    fn matches_selector(
        &self,
        selector: &ParsedSelector,
        elem: &blitz_dom::node::ElementData,
    ) -> bool {
        match selector {
            ParsedSelector::Class(name) => elem
                .attrs()
                .iter()
                .find(|a| a.name.local.as_ref() == "class")
                .map(|a| a.value.as_ref())
                .map(|cls| cls.split_whitespace().any(|c| c == name))
                .unwrap_or(false),
            ParsedSelector::Id(name) => elem
                .attrs()
                .iter()
                .find(|a| a.name.local.as_ref() == "id")
                .map(|a| a.value.as_ref())
                .map(|id| id == name)
                .unwrap_or(false),
            ParsedSelector::Tag(name) => elem.name.local.as_ref().eq_ignore_ascii_case(name),
        }
    }
}
```

**Step 4: テスト実行**

Run: `cargo test --lib -p fulgur test_running_element_pass`
Expected: 3件全パス

**Step 5: コミット**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat: add RunningElementPass to DomPass pipeline"
```

---

## Task 2: engine.rsでRunningElementPassをパイプラインに組み込む

**Files:**

- Modify: `crates/fulgur/src/engine.rs:42-93`

**Step 1: engine.rsのrender_html()を変更**

`engine.rs`の`render_html()`メソッドを修正。RunningElementPassをDomPassパイプラインに追加し、実行後にRunningElementStoreを取り出す:

変更箇所は`render_html()`メソッド内（42行目〜93行目）。以下のように書き換える:

```rust
pub fn render_html(&self, html: &str) -> Result<Vec<u8>> {
    let combined_css = self
        .assets
        .as_ref()
        .map(|a| a.combined_css())
        .unwrap_or_default();

    let gcpm = crate::gcpm::parser::parse_gcpm(&combined_css);
    let css_to_inject = &gcpm.cleaned_css;

    // --- Pipeline: parse → DomPass → resolve ---
    let fonts = self
        .assets
        .as_ref()
        .map(|a| a.fonts.as_slice())
        .unwrap_or(&[]);

    let mut doc = crate::blitz_adapter::parse(html, self.config.content_width(), fonts);

    // Build and apply DOM passes
    let mut passes: Vec<Box<dyn crate::blitz_adapter::DomPass>> = Vec::new();
    if !css_to_inject.is_empty() {
        passes.push(Box::new(crate::blitz_adapter::InjectCssPass {
            css: css_to_inject.clone(),
        }));
    }

    // Running element detection pass (must run before resolve so we can walk the raw DOM)
    let running_pass = if !gcpm.is_empty() {
        let pass = crate::blitz_adapter::RunningElementPass::new(gcpm.clone());
        Some(pass)
    } else {
        None
    };
    // We can't put RunningElementPass in the passes Vec because we need to call
    // into_running_store() after apply. Apply it separately.

    let ctx = crate::blitz_adapter::PassContext {
        viewport_width: self.config.content_width(),
        viewport_height: self.config.content_height(),
        font_data: fonts,
    };
    crate::blitz_adapter::apply_passes(&mut doc, &passes, &ctx);

    // Apply running element pass separately and extract store
    let running_store = if let Some(pass) = running_pass {
        pass.apply(&mut doc, &ctx);
        pass.into_running_store()
    } else {
        crate::gcpm::running::RunningElementStore::new()
    };

    crate::blitz_adapter::resolve(&mut doc);

    // --- Convert DOM to Pageable and render ---
    let mut convert_ctx = ConvertContext {
        running_store: &mut running_store,
        assets: self.assets.as_ref(),
        font_cache: std::collections::HashMap::new(),
    };
    let root = crate::convert::dom_to_pageable(&doc, &mut convert_ctx);

    if gcpm.is_empty() {
        self.render_pageable(root)
    } else {
        crate::render::render_to_pdf_with_gcpm(
            root,
            &self.config,
            &gcpm,
            &running_store,
            fonts,
        )
    }
}
```

注意: `running_store`の所有権の扱いに注意。`running_store`は`convert_ctx`に`&mut`で貸すが、`render_to_pdf_with_gcpm`にも`&`で渡す必要がある。`let mut running_store`で宣言し、`convert_ctx`のスコープが終わった後に`&running_store`として渡す。

実際にはconvert_ctxのライフタイムはdom_to_pageableの呼び出しで終わるので、その後running_storeの不変参照を取れる。ただし、`pass.apply()`後に`pass.into_running_store()`を呼ぶと`pass`がムーブされるので、`apply`の呼び出し方に注意。DomPassトレイトは`&self`なので問題ない。

**Step 2: テスト実行**

Run: `cargo test --lib -p fulgur`
Expected: コンパイルエラー（ConvertContextからgcpmフィールドを削除していないので、まだここでは残す）

この段階ではConvertContextのgcpmフィールドにNoneを渡す:

```rust
let mut convert_ctx = ConvertContext {
    gcpm: None, // Running elements are now handled by DomPass
    running_store: &mut running_store,
    assets: self.assets.as_ref(),
    font_cache: std::collections::HashMap::new(),
};
```

Run: `cargo test --lib -p fulgur`
Expected: 全テストパス

**Step 3: コミット**

```bash
git add crates/fulgur/src/engine.rs
git commit -m "feat: integrate RunningElementPass into DomPass pipeline"
```

---

## Task 3: convert.rsからRunning要素関連コードを削除

**Files:**

- Modify: `crates/fulgur/src/convert.rs`

**Step 1: collect_positioned_children()からRunning分岐を削除**

`convert.rs:225-234`の以下のブロックを削除:

```rust
// GCPM: skip running elements and store their HTML
if let Some(gcpm_ctx) = ctx.gcpm {
    if is_running_element(child_node, gcpm_ctx) {
        let html = serialize_node(doc, child_id);
        if let Some(name) = get_running_name(child_node, gcpm_ctx) {
            ctx.running_store.register(name, html);
        }
        continue;
    }
}
```

**Step 2: collect_table_cells()からRunning分岐を削除**

`convert.rs:429-438`の同様のブロックを削除:

```rust
// GCPM: skip running elements and store their HTML
if let Some(gcpm_ctx) = ctx.gcpm {
    if is_running_element(child_node, gcpm_ctx) {
        let html = serialize_node(doc, child_id);
        if let Some(name) = get_running_name(child_node, gcpm_ctx) {
            ctx.running_store.register(name, html);
        }
        continue;
    }
}
```

**Step 3: 不要になった関数を削除**

以下の関数を`convert.rs`から削除:

- `is_running_element()` (266-280行目)
- `matches_selector()` (293-301行目)
- `get_running_name()` (303-309行目)
- `get_attr()` (282-287行目) — 他で使われていないか確認。`convert_image()`で`get_attr`を使用しているので削除しない。
- `get_tag_name()` (289-291行目) — `matches_selector`でのみ使用なので削除。

**Step 4: ConvertContextからgcpmフィールドを削除**

```rust
pub struct ConvertContext<'a> {
    pub running_store: &'a mut RunningElementStore,
    pub assets: Option<&'a AssetBundle>,
    pub(crate) font_cache: HashMap<(usize, u32), Arc<Vec<u8>>>,
}
```

不要になったimportも削除:

```rust
use crate::gcpm::GcpmContext;     // 削除
use crate::gcpm::ParsedSelector;  // 削除
use crate::gcpm::running::{RunningElementStore, serialize_node};
// serialize_nodeも不要になるので:
use crate::gcpm::running::RunningElementStore;
```

**Step 5: テスト実行**

Run: `cargo test --lib -p fulgur`
Expected: 全テストパス

**Step 6: コミット**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "refactor: remove running element detection from convert.rs"
```

---

## Task 4: 統合テスト実行と最終検証

**Files:**

- Test: `crates/fulgur/tests/gcpm_integration.rs`

**Step 1: 統合テスト実行**

Run: `cargo test -p fulgur --test gcpm_integration -- --test-threads=1`
Expected: 全テストパス（Running要素を使う統合テストが、DomPass経由で正しく動作すること）

**Step 2: 全テスト実行**

Run: `cargo test --lib -p fulgur`
Expected: 全テストパス

**Step 3: Lint実行**

Run: `cargo clippy && cargo fmt --check`
Expected: エラーなし

**Step 4: コミット（必要な修正があれば）**

```bash
git add -A
git commit -m "fix: address clippy/fmt issues"
```
