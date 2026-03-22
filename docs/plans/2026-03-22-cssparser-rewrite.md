# GCPM パーサー cssparser 化 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 手書き文字ベースの `parse_gcpm()` を cssparser クレートベースに書き直す。機能変更なし（純粋リファクタ）。

**Architecture:** cssparser の `StyleSheetParser` でトップレベルルールを走査し、`AtRuleParser` で `@page` とマージンボックス at-rule を、`QualifiedRuleParser` + `DeclarationParser` で通常ルール内の `position: running(name)` を検出する。出力する `GcpmContext` の構造は変更しない。

**Tech Stack:** cssparser 0.35.0（Blitz 経由で依存ツリーに存在、直接依存として追加）

---

### Task 1: cssparser を直接依存に追加

**Files:**

- Modify: `crates/fulgur/Cargo.toml`

**Step 1: Cargo.toml に cssparser を追加**

`[dependencies]` セクションに追加:

```toml
cssparser = "0.35"
```

**Step 2: ビルド確認**

Run: `cargo build -p fulgur`
Expected: PASS（バージョンは Cargo.lock で既に 0.35.0 に固定されているので新規ダウンロードなし）

**Step 3: Commit**

```bash
git add crates/fulgur/Cargo.toml
git commit -m "chore: add cssparser as direct dependency for GCPM parser"
```

---

### Task 2: cssparser ベースの parse_gcpm() を実装（テストは既存を流用）

**Files:**

- Modify: `crates/fulgur/src/gcpm/parser.rs`

**注意:** 既存の `parser.rs` のテストモジュール（`mod tests`）は変更しない。このタスクでは `parse_gcpm()` 関数と内部ヘルパーの実装を書き直し、既存テストがそのまま通ることを確認する。

**Step 1: 新しい parser.rs を実装**

cssparser のトレイトを使った実装構造:

1. **トップレベルパーサー** (`GcpmSheetParser`):
   - `AtRuleParser`: `@page` を検出 → `parse_block` で内部をパース
   - `QualifiedRuleParser`: セレクタ部分を読み飛ばし、`parse_block` で宣言をパース
   - `DeclarationParser`: トップレベルでは不要（空実装）

2. **@page ブロック内パーサー** (`PageRuleParser`):
   - `AtRuleParser`: `@top-center` 等のマージンボックス at-rule を検出
   - `DeclarationParser`: `@page` 直下の宣言は読み飛ばし
   - `QualifiedRuleParser`: 不要（空実装）

3. **マージンボックスブロック内パーサー** (`MarginBoxParser`):
   - `DeclarationParser`: `content` プロパティを解析して `ContentItem` に変換、それ以外の宣言は `declarations` 文字列に蓄積
   - `AtRuleParser` / `QualifiedRuleParser`: 不要（空実装）

4. **通常ルールブロック内パーサー** (`StyleRuleParser`):
   - `DeclarationParser`: `position: running(name)` を検出
   - `AtRuleParser` / `QualifiedRuleParser`: 不要（空実装）

5. **`cleaned_css` の生成**:
   - cssparser はトークンを消費するため、元の CSS 文字列を再構築する必要がある
   - アプローチ: `Parser::position()` と `Parser::slice_from()` を活用
   - `position: running(...)` を含むルールは `display: none` に置き換え、`@page` ブロックは丸ごと除去
   - 具体的には: トップレベルパースで各ルールの開始・終了位置を記録し、非 GCPM 部分はそのまま `cleaned_css` にコピー、GCPM ルールは変換済みテキストで置換

6. **`parse_content_value()` 関数**:
   - マージンボックス内の `content` プロパティ値をパースして `Vec<ContentItem>` を返す
   - cssparser の `Parser` を使って `element(name)` / `counter(page)` / `counter(pages)` / 文字列リテラルを検出
   - 既存の手書き実装と同等の動作

公開 API:

```rust
pub fn parse_gcpm(css: &str) -> GcpmContext
```

シグネチャは変更しない。内部実装のみ cssparser 化。

**Step 2: 既存テストを実行して全パス確認**

Run: `cargo test --lib -p fulgur gcpm::parser`
Expected: 既存の 7 テスト全パス

**Step 3: Commit**

```bash
git add crates/fulgur/src/gcpm/parser.rs
git commit -m "refactor: rewrite parse_gcpm() using cssparser crate"
```

---

### Task 3: エッジケーステストの追加

**Files:**

- Modify: `crates/fulgur/src/gcpm/parser.rs`（テストモジュール）

**Step 1: cssparser 化で正しく処理されることを確認するテストを追加**

以下のケースを追加:

```rust
#[test]
fn test_running_name_case_insensitive_property() {
    // POSITION: Running(name) — プロパティ名の大文字小文字
    let css = ".header { POSITION: running(pageHeader); }";
    let ctx = parse_gcpm(css);
    assert!(ctx.running_names.contains("pageHeader"));
    assert!(ctx.cleaned_css.contains("display: none"));
}

#[test]
fn test_multiple_running_names() {
    let css = ".h { position: running(hdr); } .f { position: running(ftr); }";
    let ctx = parse_gcpm(css);
    assert!(ctx.running_names.contains("hdr"));
    assert!(ctx.running_names.contains("ftr"));
}

#[test]
fn test_running_with_other_declarations() {
    // running() 以外の宣言が cleaned_css に残ること
    let css = ".header { color: red; position: running(hdr); font-size: 14px; }";
    let ctx = parse_gcpm(css);
    assert!(ctx.running_names.contains("hdr"));
    assert!(ctx.cleaned_css.contains("color: red"));
    assert!(ctx.cleaned_css.contains("font-size: 14px"));
}

#[test]
fn test_page_with_multiple_margin_boxes() {
    let css = "@page { @top-left { content: \"Left\"; } @top-center { content: element(hdr); } @top-right { content: counter(page); } }";
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.margin_boxes.len(), 3);
}

#[test]
fn test_margin_box_with_extra_declarations() {
    let css = "@page { @top-center { content: element(hdr); font-size: 10pt; color: gray; } }";
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.margin_boxes.len(), 1);
    let mb = &ctx.margin_boxes[0];
    assert_eq!(mb.content, vec![ContentItem::Element("hdr".to_string())]);
    assert!(mb.declarations.contains("font-size"));
    assert!(mb.declarations.contains("color"));
}

#[test]
fn test_page_left_right_selectors() {
    let css = r#"
        @page :left { @bottom-left { content: counter(page); } }
        @page :right { @bottom-right { content: counter(page); } }
    "#;
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.margin_boxes.len(), 2);
    assert_eq!(ctx.margin_boxes[0].page_selector, Some(":left".to_string()));
    assert_eq!(ctx.margin_boxes[1].page_selector, Some(":right".to_string()));
}
```

**Step 2: テスト実行**

Run: `cargo test --lib -p fulgur gcpm::parser`
Expected: 全テストパス

**Step 3: Commit**

```bash
git add crates/fulgur/src/gcpm/parser.rs
git commit -m "test: add edge case tests for cssparser-based GCPM parser"
```

---

### Task 4: 統合テストの実行確認

**Files:**

- なし（既存テストの実行のみ）

**Step 1: ユニットテスト全体を実行**

Run: `cargo test --lib -p fulgur`
Expected: 全テストパス（46+追加分）

**Step 2: 統合テストを実行**

Run: `cargo test -p fulgur --test gcpm_integration -- --test-threads=1`
Expected: 全テストパス

**Step 3: clippy と fmt を確認**

Run: `cargo clippy -p fulgur && cargo fmt -p fulgur --check`
Expected: 警告・エラーなし

---

### Task 5: 旧パーサーのコード削除確認

**Step 1: parser.rs に旧コードが残っていないことを確認**

`try_parse_running()`, `find_matching_brace()`, `parse_page_rule()`, `parse_margin_boxes()`, `parse_content_property()`, `parse_content_value()` — これらの手書き関数が Task 2 で全て置き換えられていることを確認。

もし `parse_content_value()` 等を cssparser 化せず内部で流用している場合は、そのまま残して良い（`@page` 内の `content` プロパティパースは cssparser のトークナイザで十分だが、既存関数の再利用も許容）。

**Step 2: 最終ビルド確認**

Run: `cargo build -p fulgur`
Expected: 警告なしでビルド成功
