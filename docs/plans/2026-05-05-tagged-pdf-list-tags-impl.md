# Tagged PDF List Structure Tags Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `ul`/`ol`/`li` を PDF StructTree で L / LI / Lbl / LBody へ対応させ、マーカーと本文を正しくタグ付けする。

**Architecture:** `PdfTag::L` に `ListNumbering` を追加。`walk_semantics` に `parent_override` を導入し `li` ごとに合成 `Lbl`/`LBody` エントリを作成。render 側でマーカー描画を `Lbl` タグコンテンツで囲み、inline-root `li` の body を `LBody` 配下に記録する。

**Tech Stack:** Rust, krilla (PDF/Tagged), Stylo (CSS computed values), Blitz DOM, BTreeMap for semantics

---

### Task 1: `PdfTag` 拡張 — `Lbl`/`LBody` 追加と `L` への `ListNumbering`

**Files:**
- Modify: `crates/fulgur/src/tagging.rs`

**Step 1: 既存テストを確認し変更対象を把握**

```bash
cargo test --package fulgur -- tagging 2>&1 | tail -10
```

期待: 全テスト pass（現状確認）

**Step 2: `PdfTag` に `Lbl`/`LBody` を追加し `L` に `numbering` フィールドを追加**

`crates/fulgur/src/tagging.rs` の `PdfTag` enum を変更:

```rust
pub enum PdfTag {
    P,
    H { level: u8 },
    Div,
    Span,
    Figure,
    L { numbering: krilla::tagging::ListNumbering },
    Lbl,
    LBody,
    Li,
    Table,
    TRowGroup,
    Tr,
    Th,
    Td,
}
```

**Step 3: `classify_element` を更新**

```rust
"ul" => Some(PdfTag::L { numbering: krilla::tagging::ListNumbering::Disc }),
"ol" => Some(PdfTag::L { numbering: krilla::tagging::ListNumbering::Decimal }),
```

**Step 4: `pdf_tag_to_krilla_tag` を更新**

```rust
PdfTag::L { numbering } => {
    krilla::tagging::Tag::L(*numbering).into()
}
PdfTag::Lbl => krilla::tagging::Tag::<krilla::tagging::kind::Lbl>::Lbl.into(),
PdfTag::LBody => krilla::tagging::Tag::<krilla::tagging::kind::LBody>::LBody.into(),
```

**Step 5: `tagging.rs` 内テストを更新**

- `classify_element_recognises_lists_and_tables` で `L { numbering: Disc/Decimal }` を確認するよう修正
- `pdf_tag_to_krilla_tag_covers_all_variants` に `Lbl`/`LBody` ケースを追加

```rust
assert!(matches!(
    pdf_tag_to_krilla_tag(&PdfTag::L { numbering: ListNumbering::Disc }, None, None),
    TagKind::L(_)
));
assert!(matches!(
    pdf_tag_to_krilla_tag(&PdfTag::Lbl, None, None),
    TagKind::Lbl(_)
));
assert!(matches!(
    pdf_tag_to_krilla_tag(&PdfTag::LBody, None, None),
    TagKind::LBody(_)
));
```

**Step 6: コンパイルエラーを修正してテストを通す**

`PdfTag::L` に `numbering` フィールドを追加すると、`convert/mod.rs:1106` にコンパイルエラーが発生する。
render.rs には `PdfTag::L` のパターンマッチがないためそこは影響なし。

`crates/fulgur/src/convert/mod.rs:1106` を修正:
```rust
// Before:
let lists = entries_by_tag(&d, &PdfTag::L);
// After:
let lists = entries_by_tag(&d, &PdfTag::L { numbering: krilla::tagging::ListNumbering::Disc });
```

コンパイル確認:
```bash
cargo build --package fulgur 2>&1 | grep "^error" | head -10
```

期待: エラー 0 件

テスト確認:
```bash
cargo test --package fulgur -- tagging 2>&1 | tail -10
```

期待: 全テスト pass

**Step 7: コミット**

```bash
git add crates/fulgur/src/tagging.rs
git commit -m "feat(tagging): add Lbl/LBody variants and ListNumbering to PdfTag::L (fulgur-izp.7)"
```

---

### Task 2: `Drawables` に合成 NodeId サポートを追加

**Files:**
- Modify: `crates/fulgur/src/drawables.rs`

**Step 1: 既存テストを確認**

```bash
cargo test --package fulgur 2>&1 | tail -5
```

**Step 2: `Drawables` にフィールドを追加**

`crates/fulgur/src/drawables.rs` の `Drawables` struct に追加:

```rust
/// 合成 NodeId カウンタ。usize::MAX / 2 から降順に割り当て。
/// DOM NodeId（通常 < 100_000）との衝突を避けるため大きな値から開始する。
pub synthetic_id_counter: usize,
/// li NodeId → Lbl 合成 NodeId（render pass のマーカータグ付け用）
pub li_lbl_ids: BTreeMap<NodeId, NodeId>,
/// li NodeId → LBody 合成 NodeId（inline-root li の body タグ付け用）
pub li_lbody_ids: BTreeMap<NodeId, NodeId>,
```

**Step 3: `Default` derivation を確認し、手動 impl が必要なら追加**

`Drawables` が `#[derive(Default)]` を使っているなら `synthetic_id_counter` の初期値が `0` になるため、手動 impl が必要。

`Drawables` の `Default` を確認:
```bash
grep -n "derive.*Default\|impl Default for Drawables" crates/fulgur/src/drawables.rs
```

`#[derive(Default)]` ならば `Default` を手動に切り替え:

```rust
impl Default for Drawables {
    fn default() -> Self {
        Self {
            body_id: None,
            body_offset_pt: (0.0, 0.0),
            block_styles: BTreeMap::new(),
            paragraphs: BTreeMap::new(),
            paragraph_slices: BTreeMap::new(),
            images: BTreeMap::new(),
            svgs: BTreeMap::new(),
            tables: BTreeMap::new(),
            list_items: BTreeMap::new(),
            multicol_rules: BTreeMap::new(),
            transforms: BTreeMap::new(),
            bookmark_anchors: BTreeMap::new(),
            link_spans: Vec::new(),
            semantics: BTreeMap::new(),
            inline_box_subtree_skip: std::collections::BTreeSet::new(),
            inline_box_subtree_descendants: BTreeMap::new(),
            synthetic_id_counter: usize::MAX / 2,
            li_lbl_ids: BTreeMap::new(),
            li_lbody_ids: BTreeMap::new(),
        }
    }
}
```

**Step 4: `alloc_synthetic_id` メソッドを追加**

`Drawables` impl ブロックに:

```rust
/// 合成 NodeId を 1 つ割り当てる。
/// `usize::MAX / 2` から始まり降順に割り当てるので DOM NodeId と衝突しない。
pub fn alloc_synthetic_id(&mut self) -> NodeId {
    let id = self.synthetic_id_counter;
    self.synthetic_id_counter = self.synthetic_id_counter.saturating_sub(1);
    id
}
```

**Step 5: `is_empty` メソッドを更新**

`li_lbl_ids` / `li_lbody_ids` は semantics ベースのメタデータなので `is_empty` には含めない（データペイロードではないため）。

**Step 6: テスト通過確認**

```bash
cargo test --package fulgur 2>&1 | tail -5
```

**Step 7: コミット**

```bash
git add crates/fulgur/src/drawables.rs
git commit -m "feat(drawables): add synthetic_id_counter, li_lbl_ids, li_lbody_ids for Lbl/LBody (fulgur-izp.7)"
```

---

### Task 3: `walk_semantics` に `parent_override` と li 特別処理を追加

**Files:**
- Modify: `crates/fulgur/src/convert/mod.rs`

**Step 1: テスト用ヘルパーを確認**

```bash
grep -n "dom_to_drawables_records_semantic\|make_drawables\|entries_by_tag" crates/fulgur/src/convert/mod.rs | head -10
```

**Step 2: フェイリングテストを書く**

`crates/fulgur/src/convert/mod.rs` の `#[cfg(test)]` セクションに追加:

```rust
#[test]
fn walk_semantics_li_creates_lbl_and_lbody_synthetic_entries() {
    let html = "<!DOCTYPE html><html><body><ul><li>item</li></ul></body></html>";
    let d = make_drawables(html);

    // li_lbl_ids と li_lbody_ids に 1 エントリずつあること
    assert_eq!(d.li_lbl_ids.len(), 1, "one li_lbl_ids entry");
    assert_eq!(d.li_lbody_ids.len(), 1, "one li_lbody_ids entry");

    // li_lbl_ids のキー = li NodeId、値 = Lbl 合成 NodeId
    let (&li_id, &lbl_id) = d.li_lbl_ids.iter().next().unwrap();
    let lbody_id = d.li_lbody_ids[&li_id];

    // semantics に Lbl エントリがあり、parent = li_id
    let lbl_entry = &d.semantics[&lbl_id];
    assert_eq!(lbl_entry.tag, PdfTag::Lbl);
    assert_eq!(lbl_entry.parent, Some(li_id));

    // semantics に LBody エントリがあり、parent = li_id
    let lbody_entry = &d.semantics[&lbody_id];
    assert_eq!(lbody_entry.tag, PdfTag::LBody);
    assert_eq!(lbody_entry.parent, Some(li_id));
}

#[test]
fn walk_semantics_li_paragraph_parent_is_lbody() {
    // li 直下のテキスト（inline-root）は lbody_id を親に持つ
    let html = "<!DOCTYPE html><html><body><ul><li>item</li></ul></body></html>";
    let d = make_drawables(html);
    let (&li_id, _) = d.li_lbl_ids.iter().next().unwrap();
    let lbody_id = d.li_lbody_ids[&li_id];

    // P タグを探す
    let p_entries: Vec<_> = d.semantics.iter()
        .filter(|(_, e)| e.tag == PdfTag::P)
        .collect();
    // li 直下テキストは <p> タグがないため P がない場合もある
    // その場合は lbody_id が semantics に存在すること自体を確認
    let _ = d.semantics.get(&lbody_id).expect("lbody in semantics");
    // P がある場合は parent が lbody_id であること
    for (_, entry) in &p_entries {
        if entry.parent == Some(li_id) {
            panic!("P's parent should be lbody_id, not li_id directly");
        }
    }
}

#[test]
fn walk_semantics_nested_list_structure() {
    let html = "<!DOCTYPE html><html><body>\
        <ul><li><ol><li>nested</li></ol></li></ul>\
        </body></html>";
    let d = make_drawables(html);

    // 2 つの li があるので li_lbl_ids に 2 エントリ
    assert_eq!(d.li_lbl_ids.len(), 2);

    // inner li の Lbl の parent chain を確認
    // inner li → inner ul → outer LBody → outer LI
    let lists = entries_by_tag(&d, &PdfTag::L { numbering: krilla::tagging::ListNumbering::Decimal });
    assert_eq!(lists.len(), 1, "one ol");
    let (inner_ul_id, inner_ul_entry) = lists[0];

    // inner ul の parent は outer li の lbody_id であること
    let outer_li_ids: Vec<_> = d.li_lbl_ids.keys().copied()
        .filter(|&id| d.li_lbody_ids.get(&id).copied() == inner_ul_entry.parent)
        .collect();
    assert_eq!(outer_li_ids.len(), 1, "inner ul parent should be outer li's lbody");
}
```

**Step 3: テストが失敗することを確認**

```bash
cargo test --package fulgur -- walk_semantics_li 2>&1 | tail -20
```

期待: FAIL（`li_lbl_ids` フィールドが空など）

**Step 4: `walk_semantics` に `parent_override` を追加**

`crates/fulgur/src/convert/mod.rs` の `walk_semantics` 関数のシグネチャを変更:

```rust
fn walk_semantics(
    doc: &BaseDocument,
    node_id: usize,
    depth: usize,
    parent_override: Option<usize>,
    out: &mut crate::drawables::Drawables,
)
```

呼び出し元 (`record_semantics_pass`) を更新:
```rust
walk_semantics(base, body_id, 0, None, out);
```

**Step 5: `walk_semantics` の本体を書き直す**

```rust
fn walk_semantics(
    doc: &BaseDocument,
    node_id: usize,
    depth: usize,
    parent_override: Option<usize>,
    out: &mut crate::drawables::Drawables,
) {
    if depth >= MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };

    let (classified, child_override) = if let Some(elem) = node.element_data() {
        if let Some(mut tag) = crate::tagging::classify_element(elem.name.local.as_ref()) {
            // CSS list-style-type を読んで ListNumbering をオーバーライド
            if matches!(tag, crate::tagging::PdfTag::L { .. }) {
                if let Some(styles) = node.primary_styles() {
                    use ::style::computed_values::list_style_type::T as LST;
                    use krilla::tagging::ListNumbering;
                    let numbering = match styles.clone_list_style_type() {
                        LST::Disc => ListNumbering::Disc,
                        LST::Circle => ListNumbering::Circle,
                        LST::Square => ListNumbering::Square,
                        LST::Decimal => ListNumbering::Decimal,
                        LST::LowerAlpha => ListNumbering::LowerAlpha,
                        LST::UpperAlpha => ListNumbering::UpperAlpha,
                        _ => ListNumbering::None,
                    };
                    tag = crate::tagging::PdfTag::L { numbering };
                }
            }

            // parent を決定: override があればそれを使い、なければ DOM walk-up
            let parent_node_id = if let Some(ov) = parent_override {
                Some(ov)
            } else {
                let mut p = node.parent;
                loop {
                    let Some(pid) = p else { break None };
                    if out.semantics.contains_key(&pid) {
                        break Some(pid);
                    }
                    p = doc.get_node(pid).and_then(|n| n.parent);
                }
            };

            let alt_text = if matches!(tag, crate::tagging::PdfTag::Figure) {
                // get_attr は既存のローカル関数
                node.element_data()
                    .and_then(|e| get_attr(e, "alt"))
                    .map(|v| v.to_owned())
            } else {
                None
            };

            let is_li = matches!(tag, crate::tagging::PdfTag::Li);
            out.semantics.insert(
                node_id,
                crate::tagging::SemanticEntry {
                    tag,
                    parent: parent_node_id,
                    alt_text,
                },
            );

            if is_li {
                // Lbl / LBody 合成エントリを作成
                let lbl_id = out.alloc_synthetic_id();
                let lbody_id = out.alloc_synthetic_id();
                out.semantics.insert(
                    lbl_id,
                    crate::tagging::SemanticEntry {
                        tag: crate::tagging::PdfTag::Lbl,
                        parent: Some(node_id),
                        alt_text: None,
                    },
                );
                out.semantics.insert(
                    lbody_id,
                    crate::tagging::SemanticEntry {
                        tag: crate::tagging::PdfTag::LBody,
                        parent: Some(node_id),
                        alt_text: None,
                    },
                );
                out.li_lbl_ids.insert(node_id, lbl_id);
                out.li_lbody_ids.insert(node_id, lbody_id);
                // li の子は lbody_id を parent として再帰
                for &child_id in &node.children {
                    walk_semantics(doc, child_id, depth + 1, Some(lbody_id), out);
                }
                return; // 通常の再帰をスキップ
            }

            (true, None) // 分類済み要素: 子は override リセット
        } else {
            (false, parent_override) // 非分類要素: override を引き継ぐ
        }
    } else {
        (false, parent_override) // 要素でないノード: override を引き継ぐ
    };

    let _ = classified; // unused variable を抑制
    for &child_id in &node.children {
        walk_semantics(doc, child_id, depth + 1, child_override, out);
    }
}
```

**Step 6: 既存テスト `dom_to_drawables_records_semantic_entries_for_lists` を更新**

`PdfTag::L` → `PdfTag::L { numbering: ListNumbering::Disc }` に合わせて修正。

**Step 7: テストを通す**

```bash
cargo test --package fulgur -- semantics 2>&1 | tail -20
```

**Step 8: コミット**

```bash
git add crates/fulgur/src/convert/mod.rs
git commit -m "feat(convert): walk_semantics creates Lbl/LBody synthetic entries for li (fulgur-izp.7)"
```

---

### Task 4: `render.rs` — `try_start_tagged` / `finish_tagged` 拡張とマーカータグ付け

**Files:**
- Modify: `crates/fulgur/src/render.rs`

**Step 1: `try_start_tagged` の返り型を変更**

`record_id`（TagCollector.record に渡す NodeId）を tuple に含める:

```rust
// Before:
fn try_start_tagged(...) -> Option<(PdfTag, Identifier, Option<String>)>

// After:
fn try_start_tagged(...) -> Option<(usize, PdfTag, Identifier, Option<String>)>
```

**Step 2: `try_start_tagged` の本体を更新**

```rust
fn try_start_tagged(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    node_id: usize,
    drawables: &Drawables,
) -> Option<(usize, crate::tagging::PdfTag, krilla::tagging::Identifier, Option<String>)> {
    canvas.tag_collector.as_ref()?;
    let semantic = drawables.semantics.get(&node_id)?;
    match &semantic.tag {
        crate::tagging::PdfTag::P | crate::tagging::PdfTag::Span => {
            use krilla::tagging::{ContentTag, SpanTag};
            let id = canvas.surface.start_tagged(ContentTag::Span(SpanTag::empty()));
            Some((node_id, semantic.tag.clone(), id, None))
        }
        crate::tagging::PdfTag::H { .. } => {
            let heading_title = drawables.paragraphs.get(&node_id).map(|para| {
                para.lines
                    .iter()
                    .flat_map(|line| line.items.iter())
                    .filter_map(|item| match item {
                        crate::paragraph::LineItem::Text(run) => Some(run.text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            });
            use krilla::tagging::{ContentTag, SpanTag};
            let id = canvas.surface.start_tagged(ContentTag::Span(SpanTag::empty()));
            Some((node_id, semantic.tag.clone(), id, heading_title))
        }
        crate::tagging::PdfTag::Li => {
            // inline-root li: コンテンツを lbody_id で記録
            let &lbody_id = drawables.li_lbody_ids.get(&node_id)?;
            use krilla::tagging::{ContentTag, SpanTag};
            let id = canvas.surface.start_tagged(ContentTag::Span(SpanTag::empty()));
            Some((lbody_id, crate::tagging::PdfTag::LBody, id, None))
        }
        _ => None,
    }
}
```

**Step 3: `finish_tagged` の引数を変更**

```rust
fn finish_tagged(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    tag_info: Option<(usize, crate::tagging::PdfTag, krilla::tagging::Identifier, Option<String>)>,
) {
    if let Some((record_id, tag, id, heading_title)) = tag_info {
        canvas.surface.end_tagged();
        canvas
            .tag_collector
            .as_mut()
            .expect("tag_collector is Some when tag_info is Some")
            .record(record_id, tag, id, heading_title);
    }
}
```

**Step 4: `finish_tagged` の呼び出し側を更新**

呼び出し元:
- Line 949: `finish_tagged(canvas, node_id, tag_info)` → `finish_tagged(canvas, tag_info)`
- Line 989: `finish_tagged(canvas, node_id, tag_info)` → `finish_tagged(canvas, tag_info)`

**Step 5: `draw_list_item_with_block` に `node_id` を追加し、マーカータグ付けを実装**

```rust
fn draw_list_item_with_block(
    canvas: &mut crate::draw_primitives::Canvas<'_, '_>,
    node_id: usize,  // 追加
    list_item: &crate::drawables::ListItemEntry,
    // ... 既存パラメータ
) {
    // ...

    draw_with_opacity(canvas, list_item.opacity, |canvas| {
        if list_item.visible {
            // outside marker タグ付け
            let lbl_id = canvas.tag_collector.as_ref()
                .and_then(|_| drawables.li_lbl_ids.get(&node_id).copied());
            let marker_tag_id = lbl_id.map(|_| {
                canvas.surface.start_tagged(
                    krilla::tagging::ContentTag::Span(krilla::tagging::SpanTag::empty())
                )
            });

            draw_list_item_marker(canvas, list_item, x, y);

            if let (Some(lid), Some(id)) = (lbl_id, marker_tag_id) {
                canvas.surface.end_tagged();
                canvas.tag_collector.as_mut()
                    .unwrap()
                    .record(lid, crate::tagging::PdfTag::Lbl, id, None);
            }
        }
        // ... 残り既存コード
    });
}
```

呼び出し元 (`dispatch_fragment` の line 871 付近) に `node_id` を追加:
```rust
draw_list_item_with_block(
    canvas,
    node_id,  // 追加
    li,
    block_for_li,
    // ...
);
```

**Step 6: `draw_under_clip` 内のマーカー描画にもタグ付けを追加**

`draw_under_clip` 関数内の `draw_list_item_marker` 呼び出し（line 1531 付近）:

```rust
if let Some(li) = list_item
    && li.visible
{
    let lbl_id = canvas.tag_collector.as_ref()
        .and_then(|_| drawables.li_lbl_ids.get(&node_id).copied());
    let marker_tag_id = lbl_id.map(|_| {
        canvas.surface.start_tagged(
            krilla::tagging::ContentTag::Span(krilla::tagging::SpanTag::empty())
        )
    });

    draw_list_item_marker(canvas, li, x_pt, y_pt);

    if let (Some(lid), Some(id)) = (lbl_id, marker_tag_id) {
        canvas.surface.end_tagged();
        canvas.tag_collector.as_mut()
            .unwrap()
            .record(lid, crate::tagging::PdfTag::Lbl, id, None);
    }
}
```

**Step 7: コンパイル確認**

```bash
cargo build --package fulgur 2>&1 | tail -10
```

**Step 8: 全テスト通過確認**

```bash
cargo test --package fulgur 2>&1 | tail -10
```

**Step 9: コミット**

```bash
git add crates/fulgur/src/render.rs
git commit -m "feat(render): tag list marker under Lbl, inline-root li body under LBody (fulgur-izp.7)"
```

---

### Task 5: インテグレーションテスト

**Files:**
- Modify: `crates/fulgur-cli/tests/tagged_cli.rs`

既存のテストは `run_cli` ヘルパー経由でバイナリを起動し、生成した PDF バイト列を
`String::from_utf8_lossy` で確認するパターン。同じパターンで追加する。

**Step 1: フェイリングテストを追加**

`crates/fulgur-cli/tests/tagged_cli.rs` の末尾に追加:

```rust
#[test]
fn tagged_pdf_ul_has_lbl_lbody_tags() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<!DOCTYPE html><html><body><ul><li>first</li><li>second</li></ul></body></html>",
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--tagged",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI failed: {stderr}");

    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    // LI/Lbl/LBody はいずれも PDF 名前空間内で一意なため安全にバイト検索できる
    assert!(s.contains("/LI"), "tagged ul must contain /LI struct element");
    assert!(s.contains("/Lbl"), "tagged ul must contain /Lbl (marker label)");
    assert!(s.contains("/LBody"), "tagged ul must contain /LBody (list body)");
}

#[test]
fn tagged_pdf_ol_has_lbl_lbody_tags() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<!DOCTYPE html><html><body><ol><li>first</li><li>second</li></ol></body></html>",
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--tagged",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI failed: {stderr}");

    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/LI"), "tagged ol must contain /LI");
    assert!(s.contains("/Lbl"), "tagged ol must contain /Lbl");
    assert!(s.contains("/LBody"), "tagged ol must contain /LBody");
}

#[test]
fn tagged_pdf_nested_list_does_not_panic() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<!DOCTYPE html><html><body><ul><li>outer<ol><li>inner</li></ol></li></ul></body></html>",
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--tagged",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "nested list CLI must not panic: {stderr}");

    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    // ネストリストで LI が 2 つ（outer + inner）以上あること
    let li_count = s.match_indices("/LI").count();
    assert!(li_count >= 2, "nested list must have at least 2 /LI tags, got {li_count}");
}
```

**Step 2: テストを実行して失敗を確認**

```bash
cargo test --package fulgur-cli -- tagged_pdf_ul 2>&1 | tail -20
```

Task 1〜4 実装前なら fail（または pass でも可、CLI test はバイナリをビルドして実行するので
Task 1〜4 完了後に最終確認）。

**Step 3: Task 1〜4 完了後にフル確認**

```bash
cargo test --package fulgur-cli 2>&1 | tail -15
```

全 tagged CLI テストが pass すること。

**Step 4: 全テスト suite を通す**

```bash
cargo test --workspace 2>&1 | tail -15
```

**Step 5: コミット**

```bash
git add crates/fulgur-cli/tests/tagged_cli.rs
git commit -m "test(tagged): add CLI integration tests for L/LI/Lbl/LBody list structure (fulgur-izp.7)"
```

---

## 実行後の確認

1. `cargo test --workspace` が全 pass すること
2. `cargo clippy --workspace` でエラーがないこと
3. `cargo fmt --check --workspace` でフォーマットエラーがないこと

## 注意事項

- `PdfTag::L { numbering }` のパターンマッチが render.rs や他の箇所で必要になる（Task 1 でコンパイルエラーを修正時に対応）
- `Drawables` の `Default` が `#[derive(Default)]` の場合は手動 `impl Default` に切り替えが必要（`synthetic_id_counter = usize::MAX / 2` のため）
- inside marker（`list-style-position: inside`）は `Lbl` タグなし（`Lbl` グループが空になる）。これは既知の制限事項
