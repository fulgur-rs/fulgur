# Figure Alt Text Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `<img alt="...">` の代替テキストを Tagged PDF の Figure タグの `/Alt` エントリとして PDF に埋め込む。

**Architecture:** `SemanticEntry` に `alt_text: Option<Arc<str>>` を追加し、`walk_semantics` で DOM の `alt` 属性を読んで格納する。`pdf_tag_to_krilla_tag` にも `alt_text` パラメータを追加し、`build_tag_group` から渡す。

**Tech Stack:** Rust, Krilla PDF library, Blitz DOM (`blitz_dom::node::ElementData`)

---

## 背景知識

- `SemanticEntry` は `crates/fulgur/src/tagging.rs:42-45`
- `walk_semantics` は `crates/fulgur/src/convert/mod.rs:373-410`
  - `get_attr` は同ファイル:821 でモジュールレベル `use` 済み。`get_attr(elem, "alt")` で `<img>` の alt 属性を取得できる
- `pdf_tag_to_krilla_tag` は `crates/fulgur/src/tagging.rs:87-116`
  - 現在のシグネチャ: `fn pdf_tag_to_krilla_tag(tag: &PdfTag, heading_title: Option<String>) -> TagKind`
  - Krilla Figure API: `Tag::<kind::Figure>::Figure(alt_text: Option<String>)`
- `build_tag_group` は `crates/fulgur/src/render.rs:3566-3595`
  - `pdf_tag_to_krilla_tag` の唯一の呼び出し箇所（テスト除く）は行 3575
- `pdf_tag_to_krilla_tag` のテスト内呼び出しは `crates/fulgur/src/tagging.rs:169-228`（`None` を渡している）

---

### Task 1: SemanticEntry に alt_text を追加し、convert 側で populate する

**Files:**
- Modify: `crates/fulgur/src/tagging.rs:41-45` — `SemanticEntry` に `alt_text` 追加
- Modify: `crates/fulgur/src/convert/mod.rs:398-404` — `walk_semantics` で alt 属性を読む
- Test: `crates/fulgur/src/convert/mod.rs` — 既存テスト更新 + 新テスト追加

**Step 1: 失敗するテストを書く**

`crates/fulgur/src/convert/mod.rs` の `#[cfg(test)]` ブロック内、既存テストの末尾に追加:

```rust
#[test]
fn dom_to_drawables_records_alt_text_on_figure() {
    // alt あり
    let d = build_drawables(
        "<!DOCTYPE html><html><body><img src='a.png' alt='photo of cat'></body></html>",
    );
    let figures: Vec<_> = d
        .semantics
        .values()
        .filter(|e| e.tag == PdfTag::Figure)
        .collect();
    assert_eq!(figures.len(), 1);
    assert_eq!(
        figures[0].alt_text.as_deref(),
        Some("photo of cat"),
        "alt text should be captured"
    );

    // alt="" decorative
    let d2 = build_drawables(
        "<!DOCTYPE html><html><body><img src='a.png' alt=''></body></html>",
    );
    let figs2: Vec<_> = d2
        .semantics
        .values()
        .filter(|e| e.tag == PdfTag::Figure)
        .collect();
    assert_eq!(figs2[0].alt_text.as_deref(), Some(""), "empty alt should be Some(\"\")");

    // alt 未指定
    let d3 = build_drawables(
        "<!DOCTYPE html><html><body><img src='a.png'></body></html>",
    );
    let figs3: Vec<_> = d3
        .semantics
        .values()
        .filter(|e| e.tag == PdfTag::Figure)
        .collect();
    assert_eq!(figs3[0].alt_text, None, "missing alt should be None");
}
```

**Step 2: テストが失敗することを確認**

```bash
cargo test -p fulgur --lib dom_to_drawables_records_alt_text_on_figure 2>&1 | tail -15
```

Expected: コンパイルエラー（`alt_text` フィールドが存在しない）

**Step 3: SemanticEntry に alt_text を追加**

`crates/fulgur/src/tagging.rs:41-45` を変更:

```rust
#[derive(Debug, Clone)]
pub struct SemanticEntry {
    pub tag: PdfTag,
    pub parent: Option<NodeId>,
    /// Alt text for `Figure` nodes (`<img alt="...">`).
    /// `Some("")` = decorative image; `None` = alt attribute absent.
    pub alt_text: Option<std::sync::Arc<str>>,
}
```

**Step 4: walk_semantics の SemanticEntry 構築を更新**

`crates/fulgur/src/convert/mod.rs:398-404` を変更:

```rust
let alt_text = if matches!(tag, crate::tagging::PdfTag::Figure) {
    get_attr(elem, "alt").map(|v| std::sync::Arc::from(v))
} else {
    None
};
out.semantics.insert(
    node_id,
    crate::tagging::SemanticEntry {
        tag,
        parent: parent_node_id,
        alt_text,
    },
);
```

**Step 5: テストが通ることを確認**

```bash
cargo test -p fulgur --lib dom_to_drawables_records_alt_text_on_figure 2>&1 | tail -5
```

Expected: `test result: ok. 1 passed`

**Step 6: 既存テストがすべて通ることを確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: `test result: ok. N passed; 0 failed`

**Step 7: コミット**

```bash
git add crates/fulgur/src/tagging.rs crates/fulgur/src/convert/mod.rs
git commit -m "feat(tagging): add alt_text to SemanticEntry and populate from img alt attribute (fulgur-izp.6)"
```

---

### Task 2: pdf_tag_to_krilla_tag に alt_text を追加し、render 側で Figure に渡す

**Files:**
- Modify: `crates/fulgur/src/tagging.rs:87-116` — `pdf_tag_to_krilla_tag` のシグネチャ拡張
- Modify: `crates/fulgur/src/render.rs:3575` — `build_tag_group` で alt_text を渡す
- Test: `crates/fulgur/tests/render_smoke.rs` — render 統合テスト追加

**Step 1: 失敗する統合テストを書く**

`crates/fulgur/tests/render_smoke.rs` の末尾に追加:

```rust
#[test]
fn tagged_figure_alt_text_appears_in_pdf() {
    let html = r#"<!DOCTYPE html><html lang="en">
<head><style>body{margin:0}</style></head>
<body><img src="data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==" alt="fulgur logo"></body>
</html>"#;

    let pdf = Engine::builder()
        .tagged(true)
        .lang("en")
        .build()
        .render_html(html)
        .expect("render");

    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/Alt"),
        "PDF StructTree must contain /Alt for <img alt=...>"
    );
    assert!(
        s.contains("fulgur logo"),
        "PDF must contain the alt text string 'fulgur logo'"
    );
}
```

**Step 2: テストが失敗することを確認**

```bash
cargo test -p fulgur --test render_smoke tagged_figure_alt_text_appears_in_pdf 2>&1 | tail -15
```

Expected: FAIL（`/Alt` が PDF に含まれない）

**Step 3: pdf_tag_to_krilla_tag のシグネチャを拡張**

`crates/fulgur/src/tagging.rs:87-116` を変更（`alt_text` パラメータ追加と Figure 分岐の更新）:

```rust
/// Map a fulgur-internal [`PdfTag`] to the Krilla [`TagKind`] used when
/// building the PDF StructTree.
///
/// `heading_title` is forwarded to [`krilla::tagging::Tag::Hn`] as the
/// `/T` (Title) attribute required by PDF/UA-1. Pass `None` for non-heading
/// tags or when the text is unavailable.
///
/// `alt_text` is forwarded to [`krilla::tagging::Tag::Figure`] as the
/// `/Alt` attribute. `Some("")` marks a decorative image; `None` omits `/Alt`.
pub fn pdf_tag_to_krilla_tag(
    tag: &PdfTag,
    heading_title: Option<String>,
    alt_text: Option<std::sync::Arc<str>>,
) -> krilla::tagging::TagKind {
    use std::num::NonZeroU16;
    match tag {
        PdfTag::P => krilla::tagging::Tag::<krilla::tagging::kind::P>::P.into(),
        PdfTag::H { level } => {
            let level = NonZeroU16::new((*level).clamp(1, 6) as u16).unwrap();
            krilla::tagging::Tag::Hn(level, heading_title).into()
        }
        PdfTag::Span => krilla::tagging::Tag::<krilla::tagging::kind::Span>::Span.into(),
        PdfTag::Div => krilla::tagging::Tag::<krilla::tagging::kind::Div>::Div.into(),
        PdfTag::Figure => {
            krilla::tagging::Tag::<krilla::tagging::kind::Figure>::Figure(
                alt_text.map(|s| s.to_string()),
            )
            .into()
        }
        PdfTag::L => {
            krilla::tagging::Tag::L(krilla::tagging::ListNumbering::None).into() // numbering: fulgur-izp.7
        }
        PdfTag::Li => krilla::tagging::Tag::<krilla::tagging::kind::LI>::LI.into(),
        PdfTag::Table => krilla::tagging::Tag::<krilla::tagging::kind::Table>::Table.into(),
        PdfTag::TRowGroup => {
            krilla::tagging::Tag::<krilla::tagging::kind::TBody>::TBody.into()
        }
        PdfTag::Tr => krilla::tagging::Tag::<krilla::tagging::kind::TR>::TR.into(),
        PdfTag::Th => {
            krilla::tagging::Tag::TH(krilla::tagging::TableHeaderScope::Both).into() // scope attr: fulgur-izp.8
        }
        PdfTag::Td => krilla::tagging::Tag::<krilla::tagging::kind::TD>::TD.into(),
    }
}
```

**Step 4: tagging.rs テスト内の呼び出しを更新**

`tagging.rs` の `#[cfg(test)]` ブロック内の `pdf_tag_to_krilla_tag` 呼び出しはすべて `None` (heading_title) と `None` (alt_text) に更新する。例:

```rust
// 変更前
pdf_tag_to_krilla_tag(&PdfTag::P, None)
// 変更後
pdf_tag_to_krilla_tag(&PdfTag::P, None, None)
```

テスト内の Figure テストに alt_text の確認を追加:

```rust
// pdf_tag_to_krilla_tag_covers_all_variants テスト内
assert!(matches!(
    pdf_tag_to_krilla_tag(&PdfTag::Figure, None, Some(std::sync::Arc::from("logo"))),
    TagKind::Figure(_)
));
```

**Step 5: build_tag_group で alt_text を渡す**

`crates/fulgur/src/render.rs:3574-3575` を変更:

```rust
let title = heading_titles.get(&node_id).cloned();
let alt = entry.alt_text.clone();
let mut group = TagGroup::new(crate::tagging::pdf_tag_to_krilla_tag(&entry.tag, title, alt));
```

**Step 6: コンパイルが通ることを確認**

```bash
cargo build -p fulgur 2>&1 | tail -10
```

Expected: コンパイル成功

**Step 7: テストが通ることを確認**

```bash
cargo test -p fulgur --test render_smoke tagged_figure_alt_text_appears_in_pdf 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed`

**Step 8: 全テストパス確認**

```bash
cargo test -p fulgur 2>&1 | grep "^test result" | head -10
```

Expected: すべて `ok. N passed; 0 failed`

**Step 9: コミット**

```bash
git add crates/fulgur/src/tagging.rs crates/fulgur/src/render.rs crates/fulgur/tests/render_smoke.rs
git commit -m "feat(tagging): wire img alt text through to Krilla Figure /Alt entry (fulgur-izp.6)"
```
