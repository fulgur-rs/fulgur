# セレクタ→Running名マッピング 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Running要素の検出をクラス名一致からCSSセレクタベースのマッチングに拡張し、`.my-header { position: running(pageHeader); }` で `class="my-header"` の要素を `pageHeader` として認識できるようにする。

**Architecture:** `GcpmContext.running_names: HashSet<String>` を `running_mappings: Vec<RunningMapping>` に置換。パーサーでセレクタを抽出し `ParsedSelector` に変換。DOM照合を `ParsedSelector` variant分岐に変更。

**Tech Stack:** cssparser 0.35（セレクタ抽出に使用）

---

### Task 1: データ構造の追加と GcpmContext の変更

**Files:**
- Modify: `crates/fulgur/src/gcpm/mod.rs`

**Step 1: `ParsedSelector` と `RunningMapping` を追加し、`GcpmContext` を変更**

`gcpm/mod.rs` に以下を追加:

```rust
/// A simple CSS selector parsed from a style rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSelector {
    /// A class selector, e.g. `.header`
    Class(String),
    /// An ID selector, e.g. `#title`
    Id(String),
    /// A tag name selector, e.g. `header`
    Tag(String),
}

/// Maps a CSS selector to a running element name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningMapping {
    /// The parsed CSS selector.
    pub parsed: ParsedSelector,
    /// The name from `position: running(name)`.
    pub running_name: String,
}
```

`GcpmContext` を変更:
- `running_names: HashSet<String>` → `running_mappings: Vec<RunningMapping>`
- `is_empty()` の条件を `running_mappings.is_empty()` に変更
- `use std::collections::HashSet;` が不要になったら削除

`mod.rs` の既存テストも `running_names` → `running_mappings` に更新。

**Step 2: ビルド確認（コンパイルエラーが出ることを確認 — parser.rs と convert.rs がまだ旧APIを参照）**

Run: `cargo build -p fulgur 2>&1 | head -20`
Expected: コンパイルエラー（`running_names` フィールドが見つからない）

**Step 3: Commit**

```bash
git add crates/fulgur/src/gcpm/mod.rs
git commit -m "refactor: replace running_names with running_mappings in GcpmContext"
```

---

### Task 2: パーサーでセレクタを抽出して RunningMapping を生成

**Files:**
- Modify: `crates/fulgur/src/gcpm/parser.rs`

**Step 1: パーサーを更新**

変更ポイント:

1. **`GcpmSheetParser` の `running_names` フィールドを `running_mappings: &'a mut Vec<RunningMapping>` に変更**

2. **`QualifiedRuleParser::parse_prelude` でセレクタを抽出**:
   - `type Prelude = ()` → `type Prelude = Option<ParsedSelector>`
   - cssparser の `input` からトークンを読み、最初のトークンで判定:
     - `Token::Delim('.')` + `Token::Ident(name)` → `ParsedSelector::Class(name)`
     - `Token::IDHash(name)` → `ParsedSelector::Id(name)`
     - `Token::Ident(name)` → `ParsedSelector::Tag(name)`
   - 残りのトークンを消費（複合セレクタの後続部分は無視）
   - パース不能なら `None` を返す

3. **`QualifiedRuleParser::parse_block` で `RunningMapping` を生成**:
   - `running_name` が `Some` で `prelude` が `Some(selector)` なら、`RunningMapping { parsed: selector, running_name }` を `running_mappings` に追加

4. **`parse_gcpm()` 関数の出力を更新**: `running_names` → `running_mappings`

5. **テストモジュールの更新**:
   - `ctx.running_names.contains("pageHeader")` → `ctx.running_mappings.iter().any(|m| m.running_name == "pageHeader")` 等に変更
   - セレクタが正しくパースされることを検証するテストを追加:
     ```rust
     #[test]
     fn test_class_selector_extraction() {
         let css = ".my-header { position: running(pageHeader); }";
         let ctx = parse_gcpm(css);
         assert_eq!(ctx.running_mappings.len(), 1);
         assert_eq!(ctx.running_mappings[0].parsed, ParsedSelector::Class("my-header".to_string()));
         assert_eq!(ctx.running_mappings[0].running_name, "pageHeader");
     }

     #[test]
     fn test_id_selector_extraction() {
         let css = "#main-title { position: running(docTitle); }";
         let ctx = parse_gcpm(css);
         assert_eq!(ctx.running_mappings.len(), 1);
         assert_eq!(ctx.running_mappings[0].parsed, ParsedSelector::Id("main-title".to_string()));
         assert_eq!(ctx.running_mappings[0].running_name, "docTitle");
     }

     #[test]
     fn test_tag_selector_extraction() {
         let css = "header { position: running(pageHeader); }";
         let ctx = parse_gcpm(css);
         assert_eq!(ctx.running_mappings.len(), 1);
         assert_eq!(ctx.running_mappings[0].parsed, ParsedSelector::Tag("header".to_string()));
         assert_eq!(ctx.running_mappings[0].running_name, "pageHeader");
     }
     ```

**Step 2: テスト実行**

Run: `cargo test --lib -p fulgur gcpm::parser`
Expected: 全テストパス

**Step 3: Commit**

```bash
git add crates/fulgur/src/gcpm/parser.rs
git commit -m "feat: extract CSS selectors for running element mappings"
```

---

### Task 3: convert.rs の DOM照合を ParsedSelector ベースに変更

**Files:**
- Modify: `crates/fulgur/src/convert.rs`

**Step 1: ヘルパー関数を更新**

1. **`get_id_attr` 関数を追加**（`get_class_attr` と同様）:
   ```rust
   fn get_id_attr(elem: &blitz_dom::node::ElementData) -> Option<&str> {
       elem.attrs()
           .iter()
           .find(|a| a.name.local.as_ref() == "id")
           .map(|a| a.value.as_ref())
   }
   ```

2. **`get_tag_name` 関数を追加**:
   ```rust
   fn get_tag_name(elem: &blitz_dom::node::ElementData) -> &str {
       elem.name.local.as_ref()
   }
   ```

3. **`matches_selector` 関数を追加**:
   ```rust
   fn matches_selector(selector: &ParsedSelector, elem: &blitz_dom::node::ElementData) -> bool {
       match selector {
           ParsedSelector::Class(name) => {
               get_class_attr(elem)
                   .map(|cls| cls.split_whitespace().any(|c| c == name))
                   .unwrap_or(false)
           }
           ParsedSelector::Id(name) => {
               get_id_attr(elem)
                   .map(|id| id == name)
                   .unwrap_or(false)
           }
           ParsedSelector::Tag(name) => {
               get_tag_name(elem).eq_ignore_ascii_case(name)
           }
       }
   }
   ```

4. **`is_running_element` を更新**:
   ```rust
   fn is_running_element(node: &Node, ctx: &GcpmContext) -> bool {
       if ctx.running_mappings.is_empty() {
           return false;
       }
       let Some(elem) = node.element_data() else {
           return false;
       };
       ctx.running_mappings.iter().any(|m| matches_selector(&m.parsed, elem))
   }
   ```

5. **`get_running_name` を更新**:
   ```rust
   fn get_running_name(node: &Node, ctx: &GcpmContext) -> Option<String> {
       let elem = node.element_data()?;
       ctx.running_mappings
           .iter()
           .find(|m| matches_selector(&m.parsed, elem))
           .map(|m| m.running_name.clone())
   }
   ```

6. **`has_matching_running_name` を削除**（`matches_selector` に置換されたため不要）

**Step 2: ビルドとテスト実行**

Run: `cargo test --lib -p fulgur`
Expected: 全テストパス

Run: `cargo test -p fulgur --test gcpm_integration -- --test-threads=1`
Expected: 全テストパス

**Step 3: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat: match running elements by CSS selector instead of class name"
```

---

### Task 4: 統合テストの追加 — セレクタ≠running名のケース

**Files:**
- Modify: `crates/fulgur/tests/gcpm_integration.rs`

**Step 1: 既存テストの確認**

既存の統合テストでは `class="header"` と `running(pageHeader)` のように、クラス名≠running名のケースがある。旧実装ではこれが動作しなかったが、新実装ではセレクタ `.header` でマッチするので動作するはず。

**Step 2: セレクタ種別ごとの統合テストを追加**

```rust
#[test]
fn test_gcpm_id_selector_running_element() {
    let css = r#"
        #doc-title { position: running(pageTitle); }
        @page { @top-center { content: element(pageTitle); } }
    "#;
    let html = r#"<!DOCTYPE html>
    <html><body>
      <div id="doc-title">My Document</div>
      <p>Body content</p>
    </body></html>"#;

    // Should not panic and should produce valid PDF
    let pdf = Engine::new()
        .css(css)
        .render_html(html)
        .expect("should render with ID selector running element");
    assert!(!pdf.is_empty());
}

#[test]
fn test_gcpm_tag_selector_running_element() {
    let css = r#"
        header { position: running(pageHeader); }
        @page { @top-center { content: element(pageHeader); } }
    "#;
    let html = r#"<!DOCTYPE html>
    <html><body>
      <header>Document Header</header>
      <p>Body content</p>
    </body></html>"#;

    let pdf = Engine::new()
        .css(css)
        .render_html(html)
        .expect("should render with tag selector running element");
    assert!(!pdf.is_empty());
}
```

**Step 3: テスト実行**

Run: `cargo test -p fulgur --test gcpm_integration -- --test-threads=1`
Expected: 全テストパス

**Step 4: Commit**

```bash
git add crates/fulgur/tests/gcpm_integration.rs
git commit -m "test: add integration tests for ID and tag selector running elements"
```

---

### Task 5: 最終検証

**Step 1: 全テスト実行**

Run: `cargo test --lib -p fulgur`
Run: `cargo test -p fulgur --test gcpm_integration -- --test-threads=1`

**Step 2: clippy と fmt**

Run: `cargo clippy -p fulgur && cargo fmt -p fulgur --check`

**Step 3: 既存の統合テストが引き続きパスすることを確認**

特に `test_gcpm_header_footer_generates_pdf` — このテストでは `.header { position: running(pageHeader); }` と `class="header"` を使用しており、セレクタは `.header`、running名は `pageHeader`。新実装ではセレクタ `.header` → `ParsedSelector::Class("header")` でマッチし、running名 `pageHeader` を返す。旧実装（クラス名 = running名の一致）では動作しなかったケースが、新実装で初めて正しく動作する。
