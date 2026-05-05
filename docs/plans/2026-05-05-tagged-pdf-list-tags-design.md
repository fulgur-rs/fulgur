# Tagged PDF List / List Item Structure Tags Design

**Issue:** fulgur-izp.7 — tagged pdf: list / list item の構造タグを実装する

## Goal

`ul`/`ol`/`li` 要素を PDF StructTree で L / LI / Lbl / LBody へ対応させる。
マーカー（bullet/number）は `Lbl` グループへ、本文コンテンツは `LBody` グループへ入れる。

期待する StructTree:

```text
L (ul, ListNumbering::Disc)
└── LI (li)
    ├── Lbl
    │   └── [marker span content: "•"]
    └── LBody
        └── P
            └── [text span content: "item text"]
```

ネストリスト例:

```text
L (outer ul)
└── LI
    ├── Lbl → [•]
    └── LBody
        └── L (inner ol, ListNumbering::Decimal)
            └── LI
                ├── Lbl → [1.]
                └── LBody → P → [nested text]
```

## Architecture

### 1. `tagging.rs` — PdfTag 拡張

```rust
pub enum PdfTag {
    P,
    H { level: u8 },
    Div,
    Span,
    Figure,
    L { numbering: krilla::tagging::ListNumbering },  // ListNumbering 追加
    Lbl,    // 新規: マーカーラベルコンテナ
    LBody,  // 新規: 本文コンテナ
    Li,
    Table, TRowGroup, Tr, Th, Td,
}
```

**`classify_element` の変更:**

```rust
"ul" => Some(PdfTag::L { numbering: ListNumbering::Disc }),    // デフォルト
"ol" => Some(PdfTag::L { numbering: ListNumbering::Decimal }),  // デフォルト
```

CSS `list-style-type` の詳細マッピングは `walk_semantics` でオーバーライド。

**`pdf_tag_to_krilla_tag` の変更:**

```rust
PdfTag::L { numbering } => Tag::L(*numbering).into(),
PdfTag::Lbl => Tag::<kind::Lbl>::Lbl.into(),
PdfTag::LBody => Tag::<kind::LBody>::LBody.into(),
```

**CSS `list-style-type` → `ListNumbering` マッピング:**

```text
Disc          → ListNumbering::Disc
Circle        → ListNumbering::Circle
Square        → ListNumbering::Square
Decimal       → ListNumbering::Decimal
LowerAlpha    → ListNumbering::LowerAlpha
UpperAlpha    → ListNumbering::UpperAlpha
LowerRoman    → ListNumbering::LowerRoman
UpperRoman    → ListNumbering::UpperRoman
None / other  → ListNumbering::None
```

### 2. `drawables.rs` — 合成 NodeId サポート

```rust
pub struct Drawables {
    // ... 既存フィールド
    /// 合成 NodeId カウンタ。usize::MAX / 2 から降順。
    /// DOM NodeId（通常 < 100_000）と衝突しない領域を使う。
    pub synthetic_id_counter: usize,
    /// li NodeId → Lbl 合成 NodeId（render pass でのマーカータグ付け用）
    pub li_lbl_ids: BTreeMap<NodeId, NodeId>,
    /// li NodeId → LBody 合成 NodeId（inline-root li の body タグ付け用）
    pub li_lbody_ids: BTreeMap<NodeId, NodeId>,
}

impl Drawables {
    pub fn alloc_synthetic_id(&mut self) -> NodeId {
        let id = self.synthetic_id_counter;
        self.synthetic_id_counter -= 1;
        id
    }
}
```

### 3. `convert/mod.rs` — `walk_semantics` 再設計

新しいシグネチャ:

```rust
fn walk_semantics(
    doc: &BaseDocument,
    node_id: usize,
    depth: usize,
    parent_override: Option<NodeId>,  // 追加: li の子が lbody を親に使うため
    out: &mut Drawables,
)
```

**ロジック:**

1. 分類された場合: `parent = parent_override.or_else(|| DOM walk-up)`
2. `PdfTag::L { .. }` の場合: `node.primary_styles().clone_list_style_type()` でオーバーライド
3. **`PdfTag::Li` の場合（特別処理）:**
   - `lbl_id = alloc_synthetic_id()`
   - `lbody_id = alloc_synthetic_id()`
   - `semantics[lbl_id] = SemanticEntry { Lbl, parent: Some(li_id) }`
   - `semantics[lbody_id] = SemanticEntry { LBody, parent: Some(li_id) }`
   - `li_lbl_ids[li_id] = lbl_id`
   - `li_lbody_ids[li_id] = lbody_id`
   - 子ノードを `parent_override = Some(lbody_id)` で再帰
   - `return`（通常の再帰をスキップ）
4. 非分類の要素: `parent_override` をそのまま子に引き継ぐ
5. 分類済みの要素: `parent_override = None` で子を再帰（リセット）

**parent_override の伝搬ルール:**

| 状況 | このノードの parent | 子への override |
|------|-------------------|----------------|
| 分類あり (Li 以外) | override or DOM walk | None |
| 分類あり (Li) | override or DOM walk | Some(lbody_id) |
| 分類なし | — | 引き継ぎ |

**ネストリスト対応例:**

```text
<li>                 → Li, parent=ul_id, override→children=lbody1_id
  <ul>               → L,  parent=lbody1_id (override使用), override→children=None
    <li>             → Li, DOM walk→ul2_id, override→children=lbody2_id
      <p>text</p>    → P,  parent=lbody2_id (override使用)
```

### 4. `render.rs` — マーカーと本文のタグ付け

**`try_start_tagged` の拡張:**

```rust
fn try_start_tagged(canvas, node_id, drawables) -> Option<(NodeId, PdfTag, Identifier, Option<String>)> {
    // record_id: TagCollector.record() に渡す NodeId
    match semantic.tag {
        PdfTag::P | PdfTag::H { .. } | PdfTag::Span => {
            let record_id = node_id;
            // ...
            Some((record_id, tag.clone(), id, heading_title))
        }
        PdfTag::Li => {
            // inline-root li (テキスト直書き): lbody_id で記録
            let &lbody_id = drawables.li_lbody_ids.get(&node_id)?;
            let id = canvas.surface.start_tagged(ContentTag::Span(SpanTag::empty()));
            Some((lbody_id, PdfTag::LBody, id, None))
        }
        _ => None,
    }
}
```

**`finish_tagged` の変更:** `node_id` 引数を `record_id` に替え、tuple から取得。

**`draw_list_item_with_block` でのマーカータグ付け:**

```rust
// outside marker のタグ付け
let lbl_id = drawables.li_lbl_ids.get(&node_id).copied();
let marker_tag_id = if lbl_id.is_some() {
    canvas.tag_collector.as_ref()?;
    Some(canvas.surface.start_tagged(ContentTag::Span(SpanTag::empty())))
} else { None };

draw_list_item_marker(canvas, list_item, x, y);

if let (Some(lbl_id), Some(id)) = (lbl_id, marker_tag_id) {
    canvas.surface.end_tagged();
    canvas.tag_collector.as_mut().unwrap()
        .record(lbl_id, PdfTag::Lbl, id, None);
}
```

**注意:** inside marker は現状タグ付けなし（Lbl グループは空になる）。inside marker のタグ付けは将来の拡張課題。

### 5. テスト

**Unit tests (`convert/mod.rs`):**

- `ul`/`ol` で `L { numbering: Disc/Decimal }` が記録されること
- `li` ごとに `Lbl`/`LBody` 合成エントリが作られること
- ネストリストで親子関係が正しいこと

**Integration test (`tests/render_smoke.rs` or `tagged_cli.rs`):**

- HTML: `<ul><li>item</li><li>item2</li></ul>` でタグ付き PDF を生成
- `<ol><li>first</li><li>second</li></ol>` での `ListNumbering::Decimal`
- ネストリスト `<ul><li><ol><li>nested</li></ol></li></ul>` で構造破綻なし

## Acceptance Criteria 対応

| AC | 対応 |
|----|------|
| ul/ol/li が list 構造としてタグ付けされる | L { numbering } / LI |
| marker と本文の関係が保持される | Lbl (marker) / LBody (body) under LI |
| ネストしたリストで構造が破綻しない | parent_override 伝搬ロジック |
| list integration test がある | render/tagged_cli テスト |

## 非対応事項

- inside marker の Lbl タグ付け（Lbl グループは空になる）
- CSS `list-style-type: disc` など詳細な確認は CSS 読み取りで対応

## ファイル変更一覧

| ファイル | 変更内容 |
|---------|---------|
| `src/tagging.rs` | PdfTag::L に numbering, Lbl/LBody 追加, classify_element, pdf_tag_to_krilla_tag |
| `src/drawables.rs` | synthetic_id_counter, li_lbl_ids, li_lbody_ids フィールド追加 |
| `src/convert/mod.rs` | walk_semantics に parent_override, li 特別処理 |
| `src/render.rs` | try_start_tagged/finish_tagged 拡張, draw_list_item_with_block マーカータグ |
| `tests/tagged_cli.rs` / `tests/render_smoke.rs` | integration test 追加 |
