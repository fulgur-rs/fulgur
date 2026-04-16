# GCPM bookmark-* 対応 Implementation Plan (fulgur-yqi)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS GCPM `bookmark-level` / `bookmark-label` を実装し、fulgur の h1-h6 ハードコード自動 outline を UA stylesheet 駆動の CSS パスへ移行する。

**Architecture:**
- 既存 `HeadingEntry` / `HeadingCollector` / `HeadingMarker*Pageable` を `Bookmark*` に改名して一般化
- GCPM パーサに `bookmark-level` / `bookmark-label` パースを追加、`BookmarkPass`（blitz_adapter）で DOM ノード→ラベル/レベル解決
- fulgur 専用 UA CSS (`FULGUR_UA_CSS`) を GCPM パーサのみに食わせ、h1-h6 を CSS 駆動化（blitz 側は非関与）
- krilla 0.7.0 の現行 API（`OutlineNode::new` + `push_child`）で level/label は完結。`bookmark-state` は別 issue (fulgur-ta4) で krilla PR #343 待ち

**Tech Stack:** Rust, blitz-dom 0.2.4, krilla 0.7.0, cssparser, GCPM parser (既存)

**Reference:** beads issue fulgur-yqi、fulgur-ta4（bookmark-state 後続）、fulgur-yfx（counter/string 後続）

---

## Phase 1: 型の改名（Heading* → Bookmark*）

改名のみ。機能変更なし。全 372 テストが通ること、既存 examples の PDF 出力がバイト一致すること（厳密比較不要だが outline が同じであること）。

### Task 1.1: `HeadingEntry` → `BookmarkEntry`

**Files:**
- Modify: `crates/fulgur/src/pageable.rs`
- Modify: `crates/fulgur/src/outline.rs`
- Modify: `crates/fulgur/src/render.rs`
- Modify: `crates/fulgur/src/paginate.rs`

**Step 1.1.1: 型とフィールドを rename**

- `pub struct HeadingEntry { page_idx, y_pt, level, text }` → `BookmarkEntry { page_idx, y_pt, level, label }`
- `pub struct HeadingCollector { current_page_idx, entries: Vec<HeadingEntry> }` → `BookmarkCollector { ... entries: Vec<BookmarkEntry> }`
- `HeadingMarkerPageable { level, text }` → `BookmarkMarkerPageable { level, label }`
- `HeadingMarkerWrapperPageable` → `BookmarkMarkerWrapperPageable`
- `Canvas::heading_collector` → `Canvas::bookmark_collector`
- `HeadingCollector::record(level, text, y)` → `BookmarkCollector::record(level, label, y)`

すべての宣言サイトとusageを rename。内部 docstring コメントも追従。

**Step 1.1.2: ビルド＆テスト**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-yqi-bookmark-css
cargo build -p fulgur 2>&1 | tail -5
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 372 tests pass.

**Step 1.1.3: Commit**

```bash
git add -u crates/fulgur/src/
git commit -m "refactor(bookmark): rename HeadingEntry/Collector/Marker to Bookmark*"
```

---

## Phase 2: GCPM パーサで bookmark-* を認識

### Task 2.1: `gcpm/bookmark.rs` モジュール新規作成

**Files:**
- Create: `crates/fulgur/src/gcpm/bookmark.rs`
- Modify: `crates/fulgur/src/gcpm/mod.rs`

**Step 2.1.1: 型定義を書く**

```rust
// crates/fulgur/src/gcpm/bookmark.rs
use crate::gcpm::{ContentItem, ParsedSelector};

/// `bookmark-level` の値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkLevel {
    /// 明示的整数レベル (1 以上)。
    Integer(u8),
    /// `bookmark-level: none` — その要素の bookmark を抑制。
    None_,
}

/// CSS ルール 1 件から抽出した bookmark 宣言のマッピング。
#[derive(Debug, Clone, PartialEq)]
pub struct BookmarkMapping {
    pub selector: ParsedSelector,
    pub level: Option<BookmarkLevel>,
    pub label: Option<Vec<ContentItem>>,
}
```

**Step 2.1.2: `gcpm/mod.rs` に `pub mod bookmark;` を追加**

**Step 2.1.3: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | tail -3
```

Expected: 通る（まだ参照なし）。

**Step 2.1.4: Commit**

```bash
git add crates/fulgur/src/gcpm/bookmark.rs crates/fulgur/src/gcpm/mod.rs
git commit -m "feat(bookmark): add BookmarkLevel / BookmarkMapping types"
```

---

### Task 2.2: `parse_bookmark_level` — TDD

**Files:**
- Modify: `crates/fulgur/src/gcpm/parser.rs`

**Step 2.2.1: 失敗するテストを書く**

`gcpm/parser.rs` の `#[cfg(test)] mod tests` に追加:

```rust
#[test]
fn test_parse_bookmark_level_integer() {
    let css = "h1 { bookmark-level: 1; }";
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.bookmark_mappings.len(), 1);
    assert_eq!(
        ctx.bookmark_mappings[0].level,
        Some(BookmarkLevel::Integer(1))
    );
}

#[test]
fn test_parse_bookmark_level_none() {
    let css = "h1 { bookmark-level: none; }";
    let ctx = parse_gcpm(css);
    assert_eq!(
        ctx.bookmark_mappings[0].level,
        Some(BookmarkLevel::None_)
    );
}
```

**Step 2.2.2: テストが failed することを確認**

```bash
cargo test -p fulgur --lib gcpm::parser::tests::test_parse_bookmark_level 2>&1 | tail -5
```

Expected: `bookmark_mappings` フィールド不在でコンパイル失敗、または rule が無視されて空リストで assert 失敗。

**Step 2.2.3: `GcpmContext` に `bookmark_mappings: Vec<BookmarkMapping>` を追加**

`gcpm/parser.rs` の `GcpmContext` 構造体（または equivalent）に追加:

```rust
pub bookmark_mappings: Vec<BookmarkMapping>,
```

`GcpmContext::is_empty()` の判定にも追加 (`&& self.bookmark_mappings.is_empty()`).

**Step 2.2.4: パーサのルール認識を書く**

`parse_declaration` 相当（`string-set` / `content` 等を認識している箇所を探す）に bookmark 対応を追加:

```rust
"bookmark-level" => {
    if let Ok(level) = parse_bookmark_level_value(input) {
        *self.bookmark_level = Some(level);
    }
}
```

新規ヘルパー関数:

```rust
fn parse_bookmark_level_value<'i, 't>(
    input: &mut Parser<'i, 't>,
) -> Result<BookmarkLevel, ParseError<'i, ()>> {
    let token = input.next()?;
    match token {
        Token::Number { int_value: Some(n), .. } if *n >= 1 => {
            Ok(BookmarkLevel::Integer(*n as u8))
        }
        Token::Ident(ref ident) if ident.eq_ignore_ascii_case("none") => {
            Ok(BookmarkLevel::None_)
        }
        _ => Err(input.new_custom_error(())),
    }
}
```

ルール完了時に `BookmarkMapping { selector, level: bookmark_level, label: bookmark_label }` を `ctx.bookmark_mappings` に push。`string-set` / `running` の処理を参考に同じ流儀で書く。

**Step 2.2.5: テストが pass することを確認**

```bash
cargo test -p fulgur --lib gcpm::parser::tests::test_parse_bookmark_level 2>&1 | tail -5
```

Expected: 2 tests pass.

**Step 2.2.6: Commit**

```bash
git add -u crates/fulgur/src/gcpm/parser.rs
git commit -m "feat(bookmark): parse bookmark-level (integer | none)"
```

---

### Task 2.3: `parse_bookmark_label` — TDD

**Files:**
- Modify: `crates/fulgur/src/gcpm/parser.rs`

**Step 2.3.1: 失敗するテストを書く**

```rust
#[test]
fn test_parse_bookmark_label_content() {
    let css = "h1 { bookmark-label: content(); }";
    let ctx = parse_gcpm(css);
    let label = ctx.bookmark_mappings[0].label.as_ref().unwrap();
    assert_eq!(
        label,
        &vec![ContentItem::Element {
            name: String::new(),
            policy: ElementPolicy::default(),
        }]
        // もしくは content() が spec 上 content(text) と等価なら:
        // &vec![ContentItem::ContentText] — 既存 ContentItem variants に合わせる
    );
}

#[test]
fn test_parse_bookmark_label_literal_and_attr() {
    let css = r#".c { bookmark-label: "Ch. " attr(data-num) " - " content(text); }"#;
    let ctx = parse_gcpm(css);
    let label = ctx.bookmark_mappings[0].label.as_ref().unwrap();
    assert_eq!(label.len(), 4);
    // 詳細 assert は ContentItem の現状 enum 形状に合わせて書く
}
```

注: 既存 `ContentItem` は `StringSetValue` と似た形で定義されている。`parse_content_value` が返す enum と同じ型を再利用するので、`content()` / `content(text)` / `attr()` / literal が既存形状で取れる。実装時に enum バリアントを確認し assert の右辺を調整すること。

**Step 2.3.2: テストが failed することを確認**

```bash
cargo test -p fulgur --lib gcpm::parser::tests::test_parse_bookmark_label 2>&1 | tail -5
```

Expected: fail（パーサが bookmark-label を読まない）。

**Step 2.3.3: パーサに `bookmark-label` 認識を追加**

`parse_declaration` 相当に追加:

```rust
"bookmark-label" => {
    *self.bookmark_label = Some(parse_content_value(input));
}
```

`parse_content_value` は既存関数。そのまま使う。

**Step 2.3.4: テストが pass することを確認**

```bash
cargo test -p fulgur --lib gcpm::parser::tests::test_parse_bookmark_label 2>&1 | tail -5
```

Expected: 2 tests pass.

**Step 2.3.5: Commit**

```bash
git add -u crates/fulgur/src/gcpm/parser.rs
git commit -m "feat(bookmark): parse bookmark-label (content-list)"
```

---

### Task 2.4: `bookmark-level` と `bookmark-label` の組み合わせテスト

**Files:**
- Modify: `crates/fulgur/src/gcpm/parser.rs`

**Step 2.4.1: テストを書く**

```rust
#[test]
fn test_parse_bookmark_combined() {
    let css = "h1 { bookmark-level: 1; bookmark-label: content(); }";
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.bookmark_mappings.len(), 1);
    let m = &ctx.bookmark_mappings[0];
    assert_eq!(m.level, Some(BookmarkLevel::Integer(1)));
    assert!(m.label.is_some());
}

#[test]
fn test_parse_bookmark_only_level_produces_mapping() {
    // level だけで label なし → mapping は作る（label は None）
    let css = ".aside { bookmark-level: 2; }";
    let ctx = parse_gcpm(css);
    assert_eq!(ctx.bookmark_mappings.len(), 1);
    let m = &ctx.bookmark_mappings[0];
    assert_eq!(m.level, Some(BookmarkLevel::Integer(2)));
    assert!(m.label.is_none());
}

#[test]
fn test_parse_no_bookmark_no_mapping() {
    let css = "p { color: red; }";
    let ctx = parse_gcpm(css);
    assert!(ctx.bookmark_mappings.is_empty());
}
```

**Step 2.4.2: テスト実行**

```bash
cargo test -p fulgur --lib test_parse_bookmark 2>&1 | tail -10
```

Expected: 全て pass（パース側は揃っているはず。ルール終了時の push 判定が「level か label のどちらかがあれば push」になっていることを確認）。

必要なら parser.rs の「ルール終了時に push」ロジックを `if bookmark_level.is_some() || bookmark_label.is_some() { push }` に調整。

**Step 2.4.3: Commit**

```bash
git add -u crates/fulgur/src/gcpm/parser.rs
git commit -m "test(bookmark): cover combined / level-only / absent rule cases"
```

---

### Task 2.5: FULGUR_UA_CSS 定義と GCPM パーサでの確認

**Files:**
- Create: `crates/fulgur/src/gcpm/ua_css.rs`
- Modify: `crates/fulgur/src/gcpm/mod.rs`

**Step 2.5.1: UA CSS 定数を作る**

```rust
// crates/fulgur/src/gcpm/ua_css.rs
/// fulgur 専用 UA stylesheet (GCPM 固有プロパティのみ)。
/// blitz には渡さず、GCPM パーサが作者 CSS より先に食う。
pub const FULGUR_UA_CSS: &str = r#"
h1 { bookmark-level: 1; bookmark-label: content(); }
h2 { bookmark-level: 2; bookmark-label: content(); }
h3 { bookmark-level: 3; bookmark-label: content(); }
h4 { bookmark-level: 4; bookmark-label: content(); }
h5 { bookmark-level: 5; bookmark-label: content(); }
h6 { bookmark-level: 6; bookmark-label: content(); }
"#;
```

**Step 2.5.2: `gcpm/mod.rs` に `pub mod ua_css;` を追加**

**Step 2.5.3: テストを書く（UA CSS がパーサを通ること）**

`gcpm/ua_css.rs` に追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcpm::bookmark::BookmarkLevel;
    use crate::gcpm::parser::parse_gcpm;

    #[test]
    fn fulgur_ua_css_produces_h1_to_h6_mappings() {
        let ctx = parse_gcpm(FULGUR_UA_CSS);
        assert_eq!(ctx.bookmark_mappings.len(), 6);
        for (i, m) in ctx.bookmark_mappings.iter().enumerate() {
            assert_eq!(m.level, Some(BookmarkLevel::Integer((i + 1) as u8)));
            assert!(m.label.is_some());
        }
    }
}
```

**Step 2.5.4: テスト実行**

```bash
cargo test -p fulgur --lib fulgur_ua_css 2>&1 | tail -5
```

Expected: 1 test pass.

**Step 2.5.5: Commit**

```bash
git add crates/fulgur/src/gcpm/ua_css.rs
git add -u crates/fulgur/src/gcpm/mod.rs
git commit -m "feat(bookmark): add FULGUR_UA_CSS for h1-h6 default bookmarks"
```

---

## Phase 3: `BookmarkPass` — DOM ノードに bookmark info を解決

### Task 3.1: `BookmarkPass` スケルトン + selector マッチ

**Files:**
- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 3.1.1: 既存 `StringSetPass` の構造を把握**

`blitz_adapter.rs` で `StringSetPass` の実装を読む（selector マッチング、DOM walk の流儀を真似る）。

**Step 3.1.2: `BookmarkPass` 型を追加**

```rust
pub struct BookmarkPass {
    mappings: Vec<BookmarkMapping>,
    results: std::cell::RefCell<Vec<(usize, BookmarkInfo)>>,
}

#[derive(Debug, Clone)]
pub struct BookmarkInfo {
    pub level: u8,
    pub label: String,
}

impl BookmarkPass {
    pub fn new(mappings: Vec<BookmarkMapping>) -> Self { ... }
    pub fn into_results(self) -> Vec<(usize, BookmarkInfo)> { self.results.into_inner() }
}
```

**Step 3.1.3: selector マッチング + cascade 解決の `Pass` trait 実装**

ノード毎に走査、`mappings` 全部に対し selector マッチを試み、`Option<(BookmarkLevel, Option<Vec<ContentItem>>)>` を cascade で解決。後勝ち（ソース順で後のルールが上書き）。

- `BookmarkLevel::None_` → entry 生成せずスキップ
- `BookmarkLevel::Integer(n)` → `level=n`、label の解決へ進む
- label が `None` の場合 → 空文字 fallback（spec 上は `content()` 同等だが実装簡潔化のため今回は空文字で出す）
  - 実際にはすべての UA stylesheet entry が `bookmark-label: content();` を含むため、fallback が問題になるのは「作者が bookmark-level だけ指定して bookmark-label を書き忘れた」ケースのみ
  - 暫定: `Vec<ContentItem>` の未指定は `content()` 相当として扱う（即 `extract_text_content` を使う）

**Step 3.1.4: ユニットテストを書く**

`blitz_adapter.rs` の `#[cfg(test)] mod tests` に追加:

```rust
#[test]
fn bookmark_pass_matches_class_selector() {
    let html = r#"<html><body>
        <div class="ch" data-title="Intro">X</div>
    </body></html>"#;
    let mappings = vec![BookmarkMapping {
        selector: ParsedSelector::Class("ch".into()),
        level: Some(BookmarkLevel::Integer(1)),
        label: Some(vec![ContentItem::Attr("data-title".into())]),
    }];
    let mut doc = parse(html, ...);
    let pass = BookmarkPass::new(mappings);
    apply_single_pass(&pass, &mut doc, &...);
    let results = pass.into_results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.level, 1);
    assert_eq!(results[0].1.label, "Intro");
}
```

**Step 3.1.5: 実装 & テスト pass**

```bash
cargo test -p fulgur --lib bookmark_pass 2>&1 | tail -5
```

Expected: 1 test pass.

**Step 3.1.6: Commit**

```bash
git add -u crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(bookmark): add BookmarkPass with selector matching"
```

---

### Task 3.2: label 解決 — `content()` / `content(text)`

**Files:**
- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 3.2.1: テストを書く**

```rust
#[test]
fn bookmark_pass_resolves_content_text() {
    let html = r#"<html><body><h2>Hello World</h2></body></html>"#;
    let mappings = vec![BookmarkMapping {
        selector: ParsedSelector::Tag("h2".into()),
        level: Some(BookmarkLevel::Integer(2)),
        label: Some(vec![ContentItem::Element {
            name: String::new(), // content() = text content of element
            policy: ElementPolicy::default(),
        }]),
    }];
    // ... 実行 ...
    assert_eq!(results[0].1.label, "Hello World");
}
```

実際の `ContentItem` variants は `parse_content_value` の出力形状に合わせること。実装時に確認。

**Step 3.2.2: 解決ロジック**

```rust
fn resolve_label(
    items: &[ContentItem],
    doc: &BaseDocument,
    node_id: usize,
) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            ContentItem::String(s) => out.push_str(s),
            ContentItem::Element { name, .. } if name.is_empty() || name == "text" => {
                out.push_str(&crate::gcpm::string_set::extract_text_content(doc, node_id));
            }
            ContentItem::Attr(name) => {
                if let Some(val) = get_attribute(doc, node_id, name) {
                    out.push_str(&val);
                }
            }
            // counter() / string() / etc. — 今回は TODO で空文字扱い
            _ => {
                // TODO(fulgur-yfx): support counter() / string() in bookmark-label
            }
        }
    }
    out
}
```

`ContentItem` の具体 variants は既存コードから確認。

**Step 3.2.3: テスト pass**

```bash
cargo test -p fulgur --lib bookmark_pass_resolves 2>&1 | tail -5
```

Expected: 1 test pass.

**Step 3.2.4: Commit**

```bash
git add -u crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(bookmark): resolve content() and content(text) in bookmark-label"
```

---

### Task 3.3: label 解決 — `attr()`

**Files:**
- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 3.3.1: テスト**

```rust
#[test]
fn bookmark_pass_resolves_attr() {
    let html = r#"<html><body>
        <div class="ch" data-title="Intro">x</div>
    </body></html>"#;
    // mappings: ContentItem::Attr("data-title")
    // expected: label == "Intro"
}
```

**Step 3.3.2: 実装 (`resolve_label` の `ContentItem::Attr` アーム)**

Task 3.2 と同時に実装済みなら既に pass するはず。

**Step 3.3.3: テスト & commit**

---

### Task 3.4: label 解決 — literal + 複数要素の連結

**Files:**
- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 3.4.1: テスト**

```rust
#[test]
fn bookmark_pass_resolves_literal_and_mixed() {
    let html = r#"<html><body>
        <div class="ch" data-num="1">Intro</div>
    </body></html>"#;
    let mappings = vec![BookmarkMapping {
        selector: ParsedSelector::Class("ch".into()),
        level: Some(BookmarkLevel::Integer(1)),
        label: Some(vec![
            ContentItem::String("Ch. ".into()),
            ContentItem::Attr("data-num".into()),
            ContentItem::String(": ".into()),
            ContentItem::Element { name: String::new(), policy: Default::default() },
        ]),
    }];
    // expected: "Ch. 1: Intro"
}
```

**Step 3.4.2: 実装 & テスト pass & commit**

---

### Task 3.5: counter() / string() は空文字で panic せず

**Files:**
- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 3.5.1: テスト**

```rust
#[test]
fn bookmark_pass_skips_counter_and_string_gracefully() {
    let html = r#"<html><body><h1>Title</h1></body></html>"#;
    let mappings = vec![BookmarkMapping {
        selector: ParsedSelector::Tag("h1".into()),
        level: Some(BookmarkLevel::Integer(1)),
        label: Some(vec![
            ContentItem::Counter { name: "chapter".into(), style: CounterStyle::Decimal },
            ContentItem::String(" ".into()),
            ContentItem::Element { name: "text".into(), policy: Default::default() },
        ]),
    }];
    // expected: label == " Title" (counter 部分が空でスキップ)
    // 重要: panic しない
}
```

**Step 3.5.2: 既に `_ => {}` で暗黙 no-op なら pass するはず。確認のみ**

**Step 3.5.3: Commit**

```bash
git add -u crates/fulgur/src/blitz_adapter.rs
git commit -m "test(bookmark): cover unsupported counter/string in bookmark-label"
```

---

### Task 3.6: bookmark-level: none で抑制

**Files:**
- Modify: `crates/fulgur/src/blitz_adapter.rs`

**Step 3.6.1: テスト**

```rust
#[test]
fn bookmark_pass_none_suppresses_entry() {
    let html = r#"<html><body><h1>X</h1></body></html>"#;
    let mappings = vec![
        // UA: h1 → level 1
        BookmarkMapping {
            selector: ParsedSelector::Tag("h1".into()),
            level: Some(BookmarkLevel::Integer(1)),
            label: Some(vec![ContentItem::Element {
                name: String::new(), policy: Default::default(),
            }]),
        },
        // author: h1 → none (後勝ち cascade で抑制)
        BookmarkMapping {
            selector: ParsedSelector::Tag("h1".into()),
            level: Some(BookmarkLevel::None_),
            label: None,
        },
    ];
    // expected: results.is_empty()
}
```

**Step 3.6.2: 実装（cascade 時に level が最終的に `None_` なら skip）**

**Step 3.6.3: テスト pass & commit**

---

## Phase 4: Convert & Engine 配線

### Task 4.1: `ConvertContext` に `bookmark_by_node` を追加

**Files:**
- Modify: `crates/fulgur/src/convert.rs`
- Modify: `crates/fulgur/src/render.rs`（convert ctx 初期化箇所）

**Step 4.1.1: フィールド追加**

```rust
pub struct ConvertContext<'a> {
    // 既存...
    pub bookmark_by_node: HashMap<usize, crate::blitz_adapter::BookmarkInfo>,
}
```

**Step 4.1.2: 既存の `ConvertContext { ... }` リテラル全箇所に `bookmark_by_node: HashMap::new()` を追加**

`grep -n "ConvertContext {" crates/fulgur/src/` で洗い出し、`string_set_by_node: HashMap::new()` と同じ場所に追加。

**Step 4.1.3: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | tail -5
```

Expected: 通る。

**Step 4.1.4: Commit**

```bash
git add -u crates/fulgur/src/convert.rs crates/fulgur/src/render.rs
git commit -m "feat(bookmark): plumb bookmark_by_node through ConvertContext"
```

---

### Task 4.2: `maybe_wrap_bookmark` を追加（既存 `maybe_wrap_heading` と併存）

**Files:**
- Modify: `crates/fulgur/src/convert.rs`

**Step 4.2.1: 関数追加**

```rust
fn maybe_wrap_bookmark(
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    result: Box<dyn Pageable>,
) -> Box<dyn Pageable> {
    use crate::pageable::{BookmarkMarkerPageable, BookmarkMarkerWrapperPageable};
    let Some(info) = ctx.bookmark_by_node.remove(&node_id) else {
        return result;
    };
    Box::new(BookmarkMarkerWrapperPageable::new(
        BookmarkMarkerPageable::new(info.level, info.label),
        result,
    ))
}
```

**Step 4.2.2: `convert_node` から呼ぶ（既存 `maybe_wrap_heading` の前に）**

```rust
let result = maybe_wrap_bookmark(node_id, ctx, result);
let result = maybe_wrap_heading(doc, node_id, result);  // 既存
```

**注**: このタスクでは `maybe_wrap_heading` は削除しない。Task 5.x で CSS 駆動が動くのを確認してから削除する。ただし両方が同じ h1 で動くと二重ラップになるため、`maybe_wrap_heading` に「`bookmark_by_node` を既に消費していればスキップ」の判定を追加するか、もしくは「既に `BookmarkMarkerWrapperPageable` でラップされていれば skip」をチェックする。

**最も安全策**: `maybe_wrap_bookmark` で消費したかどうかを Return 型で返し、呼び出し側で `else` ブランチとして hardcode を走らせる:

```rust
fn convert_node(...) -> Box<dyn Pageable> {
    // ...
    let result = maybe_prepend_counter_ops(node_id, result, ctx);
    let result = maybe_wrap_transform(doc, node_id, result);
    // CSS 駆動の bookmark を最優先、なければ hardcode（h1-h6）を fallback
    if let Some(info) = ctx.bookmark_by_node.remove(&node_id) {
        Box::new(BookmarkMarkerWrapperPageable::new(
            BookmarkMarkerPageable::new(info.level, info.label),
            result,
        ))
    } else {
        maybe_wrap_heading(doc, node_id, result)
    }
}
```

**Step 4.2.3: テスト（手動で `bookmark_by_node` を埋めて convert するユニットテスト）**

```rust
#[test]
fn convert_wraps_with_bookmark_when_css_driven() {
    // html: <div class="x">T</div>
    // ctx.bookmark_by_node: { div_id -> BookmarkInfo { level: 1, label: "T" } }
    // 結果の Pageable ツリーをwalkし、BookmarkMarkerWrapperPageable が見つかる
}
```

**Step 4.2.4: Commit**

```bash
git add -u crates/fulgur/src/convert.rs
git commit -m "feat(bookmark): wrap CSS-driven elements with BookmarkMarkerWrapper"
```

---

### Task 4.3: `engine.rs` で UA CSS + BookmarkPass を配線

**Files:**
- Modify: `crates/fulgur/src/engine.rs`

**Step 4.3.1: GCPM パーサに UA CSS を先に食わせる**

`engine.rs` で現在 `gcpm::parser::parse_gcpm(...)` を呼んでいる箇所（`<style>` の中身を parse している箇所）を探し、`FULGUR_UA_CSS` を最初に concat する形にする。

または、`parse_gcpm` を2回呼んで結果を merge するヘルパーを追加（より綺麗）:

```rust
let ua_ctx = crate::gcpm::parser::parse_gcpm(crate::gcpm::ua_css::FULGUR_UA_CSS);
let mut gcpm = crate::gcpm::parser::parse_gcpm(&document_css);
// UA は先に、作者は後に来るよう merge（後勝ち cascade）
gcpm.bookmark_mappings.splice(0..0, ua_ctx.bookmark_mappings);
```

具体実装は既存の parse_gcpm 結合パターンに合わせる（複数 `<style>` をどう merge しているか）。

**Step 4.3.2: `BookmarkPass` を実行**

`StringSetPass` / `CounterPass` と同じブロックに追加:

```rust
let bookmark_by_node: HashMap<usize, crate::blitz_adapter::BookmarkInfo> = {
    if !gcpm.bookmark_mappings.is_empty() {
        let pass = crate::blitz_adapter::BookmarkPass::new(gcpm.bookmark_mappings.clone());
        crate::blitz_adapter::apply_single_pass(&pass, &mut doc, &ctx);
        pass.into_results().into_iter().collect()
    } else {
        HashMap::new()
    }
};
```

そして `ConvertContext` へ流し込み:

```rust
let mut convert_ctx = ConvertContext {
    // ...
    bookmark_by_node,
};
```

**Step 4.3.3: ビルド & 既存テスト確認**

```bash
cargo build -p fulgur 2>&1 | tail -5
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 全 pass（既存挙動は維持、h1-h6 は UA + hardcode の両方で処理される可能性があるが、Task 4.2 のロジックで CSS 駆動が優先されるので二重ラップは起きない）。

**Step 4.3.4: Commit**

```bash
git add -u crates/fulgur/src/engine.rs
git commit -m "feat(bookmark): wire FULGUR_UA_CSS and BookmarkPass into engine"
```

---

### Task 4.4: 統合テスト — h1 が UA CSS で自動 bookmark 化

**Files:**
- Modify: `crates/fulgur/src/engine.rs` or `crates/fulgur/tests/` 配下

**Step 4.4.1: エンドツーエンドテスト**

```rust
#[test]
fn end_to_end_h1_gets_bookmark_via_ua_css() {
    let html = r#"<html><body><h1>Title</h1></body></html>"#;
    let engine = Engine::builder().build();
    let pdf_bytes = engine.render_html(html).unwrap();
    // Parse PDF, check /Outlines contains "Title"
    assert!(contains_outline_entry(&pdf_bytes, "Title"));
}
```

PDF outline 検証ヘルパーは既存に近いものがある可能性あり（`tests/` 配下を確認）。無ければ pdf-writer / lopdf で読む簡易 helper を追加。

**Step 4.4.2: テスト pass**

```bash
cargo test -p fulgur --lib end_to_end_h1 2>&1 | tail -5
```

Expected: pass。

**Step 4.4.3: Commit**

```bash
git add -u crates/fulgur/src/
git commit -m "test(bookmark): verify h1 auto-bookmarks via UA stylesheet"
```

---

## Phase 5: hardcode パス削除 (fulgur-7r9 吸収)

### Task 5.1: `maybe_wrap_heading` と `heading_level` を削除

**Files:**
- Modify: `crates/fulgur/src/convert.rs`

**Step 5.1.1: 削除**

- `fn heading_level(node: &Node) -> Option<u8>` 削除
- `fn maybe_wrap_heading(...)` 削除
- `convert_node` の hardcode 分岐を削除し、`maybe_wrap_bookmark` 相当のロジックのみ残す:

```rust
fn convert_node(...) -> Box<dyn Pageable> {
    // ...
    let result = maybe_wrap_transform(doc, node_id, result);
    if let Some(info) = ctx.bookmark_by_node.remove(&node_id) {
        Box::new(BookmarkMarkerWrapperPageable::new(
            BookmarkMarkerPageable::new(info.level, info.label),
            result,
        ))
    } else {
        result
    }
}
```

**Step 5.1.2: ビルド & 全テスト確認**

```bash
cargo build -p fulgur 2>&1 | tail -5
cargo test -p fulgur --lib 2>&1 | tail -10
```

Expected: **既存 372 tests + 新規テストすべて pass**。もし outline 関連テストが落ちるなら、UA CSS 経路が期待通りに h1-h6 をカバーできていないサイン。

**Step 5.1.3: 関連コメントや import の整理**

`convert.rs` 先頭の use 文で `HeadingMarker*` の import が残っていれば削除。

**Step 5.1.4: Commit**

```bash
git add -u crates/fulgur/src/convert.rs
git commit -m "refactor(bookmark): remove h1-h6 hardcode, rely on UA stylesheet (fulgur-7r9)"
```

---

### Task 5.2: examples 視覚回帰確認

**Files:** なし（確認のみ）

**Step 5.2.1: examples 再生成 → outline が同じであることを確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-yqi-bookmark-css
# examples を再生成する既存コマンドがあれば実行。mise.toml / Makefile を確認
# 例: mise run update-examples
```

差分を確認。outline 関連の PDF では outline entry 位置・テキストが変化していないこと。

**Step 5.2.2: テストスイート最終確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur 2>&1 | tail -10
cargo test -p fulgur --test gcpm_integration 2>&1 | tail -5
```

Expected: 全 pass。

---

## Phase 6: 追加の受け入れ条件テスト

### Task 6.1: `bookmark-level: none` での抑制 (E2E)

**Files:**
- Modify: `crates/fulgur/tests/` 配下（統合テストファイル）

**Step 6.1.1: テスト**

```rust
#[test]
fn author_css_can_suppress_h1_bookmark() {
    let html = r#"<html><head><style>
        h1 { bookmark-level: none; }
    </style></head><body><h1>Hidden</h1></body></html>"#;
    let pdf = Engine::builder().build().render_html(html).unwrap();
    assert!(!contains_outline_entry(&pdf, "Hidden"));
}
```

**Step 6.1.2: pass & commit**

---

### Task 6.2: カスタム CSS で div を bookmark 化

```rust
#[test]
fn custom_css_bookmarks_arbitrary_element() {
    let html = r#"<html><head><style>
        .ch { bookmark-level: 1; bookmark-label: "Ch. " attr(data-num); }
    </style></head><body>
        <div class="ch" data-num="1">x</div>
        <div class="ch" data-num="2">y</div>
    </body></html>"#;
    let pdf = Engine::builder().build().render_html(html).unwrap();
    assert!(contains_outline_entry(&pdf, "Ch. 1"));
    assert!(contains_outline_entry(&pdf, "Ch. 2"));
}
```

---

### Task 6.3: h1 と div.aside を混在させてネスト確認

```rust
#[test]
fn mixed_h1_and_custom_aside() {
    let html = r#"<html><head><style>
        .aside { bookmark-level: 2; bookmark-label: attr(data-title); }
    </style></head><body>
        <h1>Chapter</h1>
        <div class="aside" data-title="Note"></div>
    </body></html>"#;
    // outline: "Chapter" (level 1) → children: "Note" (level 2)
}
```

---

### Task 6.4: 視覚回帰 / examples の最終ダブルチェック

```bash
cargo test --workspace 2>&1 | tail -10
npx markdownlint-cli2 'docs/plans/2026-04-16-bookmark-css.md'
```

Expected: すべて clean。

---

## 完了チェックリスト

- [ ] Phase 1: 型改名完了、既存 372 tests pass
- [ ] Phase 2: GCPM パーサが bookmark-level / bookmark-label を認識
- [ ] Phase 2.5: FULGUR_UA_CSS が h1-h6 を UA として宣言
- [ ] Phase 3: BookmarkPass が selector match + cascade + label 解決
- [ ] Phase 4: ConvertContext + engine 配線
- [ ] Phase 4.4: h1 auto-bookmark E2E pass
- [ ] Phase 5: hardcode path 削除、fulgur-7r9 吸収
- [ ] Phase 6: 追加 E2E（none 抑制 / カスタム CSS / 混在）
- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy` warning free
- [ ] `cargo test -p fulgur --lib` pass
- [ ] `cargo test -p fulgur` pass
- [ ] `cargo test -p fulgur --test gcpm_integration` pass
- [ ] examples の視覚回帰 clean

## 後続 issue （本プランのスコープ外）

- **fulgur-ta4**: `bookmark-state` 対応（krilla PR #343 マージ＋publish 待ち）
- **fulgur-yfx**: `bookmark-label` の `counter()` / `string()` フル対応
