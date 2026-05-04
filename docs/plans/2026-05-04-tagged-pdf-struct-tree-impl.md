# Tagged PDF StructTree Builder Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** HTML の親子関係を反映した階層的 TagTree を構築し `document.set_tag_tree` に渡す。

**Architecture:** `tagging.rs` の `pdf_tag_to_krilla_tag` を全 PdfTag バリアント対応に拡張し、`render.rs` のフラット TagTree ビルダーを `drawables.semantics` の `parent` フィールドを使った再帰的ビルダーに置き換える。スナップショットテストで PDF バイト列の安定性を保証する。

**Tech Stack:** Rust, krilla 0.7 (`TagGroup`, `TagTree`, `TagKind`), lopdf (テスト用 PDF 解析)

---

### Task 1: `pdf_tag_to_krilla_tag` を全バリアント対応に拡張

**Files:**
- Modify: `crates/fulgur/src/tagging.rs`

**Step 1: 失敗するテストを書く**

`crates/fulgur/src/tagging.rs` の `#[cfg(test)]` ブロック末尾に追記:

```rust
#[test]
fn pdf_tag_to_krilla_tag_covers_all_variants() {
    use krilla::tagging::TagKind;
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::Div, None), TagKind::Div(_)));
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::Figure, None), TagKind::Figure(_)));
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::L, None), TagKind::L(_)));
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::Li, None), TagKind::LI(_)));
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::Table, None), TagKind::Table(_)));
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::TRowGroup, None), TagKind::TBody(_)));
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::Tr, None), TagKind::TR(_)));
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::Th, None), TagKind::TH(_)));
    assert!(matches!(pdf_tag_to_krilla_tag(&PdfTag::Td, None), TagKind::TD(_)));
}
```

**Step 2: テストが失敗することを確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-izp.5
cargo test -p fulgur --lib tagging::tests::pdf_tag_to_krilla_tag_covers_all_variants 2>&1 | tail -5
```

期待結果: `FAILED` (Div/Figure 等が TagKind::P にマップされるため)

**Step 3: 実装**

`crates/fulgur/src/tagging.rs` の `pdf_tag_to_krilla_tag` 関数を以下で置き換える:

```rust
pub fn pdf_tag_to_krilla_tag(
    tag: &PdfTag,
    heading_title: Option<String>,
) -> krilla::tagging::TagKind {
    use std::num::NonZeroU16;
    match tag {
        PdfTag::P => krilla::tagging::Tag::<krilla::tagging::kind::P>::P.into(),
        PdfTag::H { level } => {
            let level = NonZeroU16::new((*level).max(1) as u16)
                .unwrap_or_else(|| NonZeroU16::new(1).unwrap());
            krilla::tagging::Tag::Hn(level, heading_title).into()
        }
        PdfTag::Span => krilla::tagging::Tag::<krilla::tagging::kind::Span>::Span.into(),
        PdfTag::Div => krilla::tagging::Tag::<krilla::tagging::kind::Div>::Div.into(),
        PdfTag::Figure => krilla::tagging::Tag::<krilla::tagging::kind::Figure>::Figure.into(),
        PdfTag::L => {
            krilla::tagging::Tag::L(krilla::tagging::ListNumbering::None).into()
        }
        PdfTag::Li => krilla::tagging::Tag::<krilla::tagging::kind::LI>::LI.into(),
        PdfTag::Table => krilla::tagging::Tag::<krilla::tagging::kind::Table>::Table.into(),
        PdfTag::TRowGroup => {
            krilla::tagging::Tag::<krilla::tagging::kind::TBody>::TBody.into()
        }
        PdfTag::Tr => krilla::tagging::Tag::<krilla::tagging::kind::TR>::TR.into(),
        PdfTag::Th => {
            krilla::tagging::Tag::TH(krilla::tagging::TableHeaderScope::Both).into()
        }
        PdfTag::Td => krilla::tagging::Tag::<krilla::tagging::kind::TD>::TD.into(),
    }
}
```

関数の docstring も更新する（"Only P / H{n} / Span are fully wired; all other variants fall back to `Tag::P`..." の行を削除）:

```rust
/// Map a fulgur-internal [`PdfTag`] to the Krilla [`TagKind`] used when
/// building the PDF StructTree.
///
/// `heading_title` is forwarded to [`krilla::tagging::Tag::Hn`] as the
/// `/T` (Title) attribute required by PDF/UA-1. Pass `None` for non-heading
/// tags or when the text is unavailable.
pub fn pdf_tag_to_krilla_tag(
```

**Step 4: テストが通ることを確認**

```bash
cargo test -p fulgur --lib tagging 2>&1 | tail -5
```

期待結果: `test result: ok. N passed`

**Step 5: コミット**

```bash
git add crates/fulgur/src/tagging.rs
git commit -m "feat(tagging): extend pdf_tag_to_krilla_tag to cover all PdfTag variants (fulgur-izp.5)"
```

---

### Task 2: 階層的 TagTree ビルダーを実装

**Files:**
- Modify: `crates/fulgur/src/render.rs:241-263`

**Step 1: 失敗するテストを書く**

`crates/fulgur/tests/render_smoke.rs` の末尾に追記（インポートは既存の `use fulgur::{AssetBundle, Engine};` で済む）:

```rust
#[test]
fn tagged_struct_tree_reflects_dom_nesting() {
    // section > h1 + p の2階層構造が StructTree に反映されることを確認。
    // 現在の flat ビルダーでは section(Div)が top-level に出ず失敗する。
    let html = r#"<!DOCTYPE html><html lang="en">
<head><style>body{font-family:'Noto Sans',sans-serif;margin:0}</style></head>
<body><section><h1>Title</h1><p>Body.</p></section></body></html>"#;

    let mut assets = AssetBundle::default();
    assets.add_font_file("examples/.fonts/NotoSans-Regular.ttf").unwrap();
    assets.add_css("body { font-family: 'Noto Sans', sans-serif; }");

    let pdf = Engine::builder()
        .tagged(true)
        .lang("en")
        .assets(assets)
        .build()
        .render_html(html)
        .expect("render");

    let s = String::from_utf8_lossy(&pdf);
    // Div タグが StructTree に存在するかを確認
    // (現 flat ビルダーでは Div は出力されない)
    assert!(s.contains("/Div"), "StructTree must contain /Div for <section>");
}
```

**Step 2: テストが失敗することを確認**

```bash
cargo test -p fulgur --test render_smoke tagged_struct_tree_reflects_dom_nesting 2>&1 | tail -10
```

期待結果: `FAILED` — flat ビルダーは Div コンテナを含まない

**Step 3: 階層的ビルダーを実装**

`crates/fulgur/src/render.rs` の `if let Some(tc) = tag_collector {` ブロック（現在 ~241-263 行）を以下で置き換える:

```rust
    if let Some(tc) = tag_collector {
        let mut tree = TagTree::new().with_lang(config.lang.clone());
        build_struct_tree(tc, drawables, &mut tree);
        document.set_tag_tree(tree);
    }
```

同ファイルに以下のヘルパー関数を追加する（`render_v2` 関数の **外側**、ファイル末尾付近に追加）:

```rust
/// Build a hierarchical [`TagTree`] from [`TagCollector`] entries and
/// the `semantics` map in [`Drawables`].
///
/// The flat approach used before fulgur-izp.5 created one top-level
/// [`TagGroup`] per tagged NodeId. This function instead uses
/// [`crate::tagging::SemanticEntry::parent`] to nest groups, so that
/// `<section><h1>…</h1></section>` produces a Div group containing an
/// Hn group rather than two sibling groups.
fn build_struct_tree(
    tc: crate::draw_primitives::TagCollector,
    drawables: &Drawables,
    tree: &mut TagTree,
) {
    // Step 1: group Identifiers and heading titles by NodeId.
    let mut identifiers: BTreeMap<crate::drawables::NodeId, Vec<Identifier>> = BTreeMap::new();
    let mut heading_titles: BTreeMap<crate::drawables::NodeId, String> = BTreeMap::new();
    for (node_id, _tag, id, heading_title) in tc.into_entries() {
        identifiers.entry(node_id).or_default().push(id);
        if let Some(title) = heading_title {
            heading_titles.entry(node_id).or_insert(title);
        }
    }

    // Step 2: build parent→children map from semantics.
    // BTreeMap preserves NodeId insertion order which equals DOM parse order.
    let mut children_map: BTreeMap<crate::drawables::NodeId, Vec<crate::drawables::NodeId>> =
        BTreeMap::new();
    for (&node_id, entry) in &drawables.semantics {
        if let Some(parent_id) = entry.parent {
            children_map.entry(parent_id).or_default().push(node_id);
        }
    }

    // Step 3: collect root NodeIds (those with parent == None) in ascending order.
    let roots: Vec<crate::drawables::NodeId> = drawables
        .semantics
        .iter()
        .filter(|(_, e)| e.parent.is_none())
        .map(|(&id, _)| id)
        .collect();

    // Step 4: recursively build TagGroups and push roots onto the tree.
    for root_id in roots {
        let group = build_tag_group(root_id, drawables, &identifiers, &heading_titles, &children_map);
        tree.push(Node::Group(group));
    }
}

fn build_tag_group(
    node_id: crate::drawables::NodeId,
    drawables: &Drawables,
    identifiers: &BTreeMap<crate::drawables::NodeId, Vec<Identifier>>,
    heading_titles: &BTreeMap<crate::drawables::NodeId, String>,
    children_map: &BTreeMap<crate::drawables::NodeId, Vec<crate::drawables::NodeId>>,
) -> TagGroup {
    let entry = &drawables.semantics[&node_id];
    let title = heading_titles.get(&node_id).cloned();
    let mut group = TagGroup::new(crate::tagging::pdf_tag_to_krilla_tag(&entry.tag, title));

    // Leaves first: Identifier(s) for content drawn at this node.
    if let Some(ids) = identifiers.get(&node_id) {
        for &id in ids {
            group.push(Node::Leaf(id));
        }
    }

    // Then child TagGroups in NodeId (DOM) order.
    if let Some(children) = children_map.get(&node_id) {
        for &child_id in children {
            let child = build_tag_group(child_id, drawables, identifiers, heading_titles, children_map);
            group.push(Node::Group(child));
        }
    }

    group
}
```

**Step 4: テストが通ることを確認**

```bash
cargo test -p fulgur --test render_smoke tagged_struct_tree_reflects_dom_nesting 2>&1 | tail -5
```

期待結果: `test result: ok. 1 passed`

**Step 5: 既存タグ付きテストも全パスを確認**

```bash
cargo test -p fulgur --test render_smoke tagged 2>&1 | tail -10
```

期待結果: `ok. N passed; 0 failed`

**Step 6: コミット**

```bash
git add crates/fulgur/src/render.rs
git commit -m "feat(render): build hierarchical TagTree from semantics.parent (fulgur-izp.5)"
```

---

### Task 3: PDF バイト列スナップショットテスト

**Files:**
- Modify: `crates/fulgur/tests/render_smoke.rs`
- Create: `crates/fulgur/tests/snapshots/` (初回実行時に自動生成)

**Step 1: `check_pdf_snapshot` ヘルパーを追加**

`crates/fulgur/tests/render_smoke.rs` の `use` ブロック直下（最初のテスト関数の前）に追加:

```rust
use std::path::PathBuf;

/// PDF バイト列スナップショット比較。
///
/// `crates/fulgur/tests/snapshots/{name}.pdf` が存在しなければ生成して
/// panic する（初回は生成されたファイルを目視確認し再実行する）。
/// `FULGUR_UPDATE_SNAPSHOTS=1` 環境変数でスナップショットを強制更新。
fn check_pdf_snapshot(name: &str, pdf: &[u8]) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.pdf"));

    if std::env::var("FULGUR_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, pdf).unwrap();
        return;
    }

    if !path.exists() {
        std::fs::write(&path, pdf).unwrap();
        panic!(
            "new snapshot created: {name}.pdf — review the file, then re-run the test"
        );
    }

    let expected = std::fs::read(&path).unwrap();
    if pdf != expected.as_slice() {
        panic!(
            "PDF snapshot mismatch: {name}\n\
             Run with FULGUR_UPDATE_SNAPSHOTS=1 to update the snapshot."
        );
    }
}

fn tagged_render_with_noto(html: &str) -> Vec<u8> {
    let mut assets = AssetBundle::default();
    assets.add_font_file("examples/.fonts/NotoSans-Regular.ttf").unwrap();
    assets.add_css("body { font-family: 'Noto Sans', sans-serif; }");
    Engine::builder()
        .tagged(true)
        .lang("en")
        .assets(assets)
        .build()
        .render_html(html)
        .expect("tagged render")
}
```

**Step 2: スナップショットテストを追加**

`crates/fulgur/tests/render_smoke.rs` 末尾に追記:

```rust
#[test]
fn snapshot_tagged_struct_tree_nested() {
    let html = r#"<!DOCTYPE html><html lang="en">
<head><style>body{font-family:'Noto Sans',sans-serif;margin:0}</style></head>
<body><section><h1>Title</h1><p>Body text.</p></section></body></html>"#;
    let pdf = tagged_render_with_noto(html);
    check_pdf_snapshot("tagged_struct_tree_nested", &pdf);
}

#[test]
fn tagged_pdf_is_deterministic() {
    let html = r#"<!DOCTYPE html><html lang="en">
<head><style>body{font-family:'Noto Sans',sans-serif;margin:0}</style></head>
<body><section><h1>Title</h1><p>Body text.</p></section></body></html>"#;
    let pdf1 = tagged_render_with_noto(html);
    let pdf2 = tagged_render_with_noto(html);
    assert_eq!(pdf1, pdf2, "tagged PDF must be byte-identical across renders");
}
```

**Step 3: 初回実行（スナップショット生成）**

```bash
cargo test -p fulgur --test render_smoke snapshot_tagged_struct_tree_nested 2>&1 | tail -10
```

期待結果: `panicked at ... new snapshot created: tagged_struct_tree_nested.pdf`
→ `crates/fulgur/tests/snapshots/tagged_struct_tree_nested.pdf` が生成される

**Step 4: 生成された PDF を目視確認**

lopdf で StructTree を確認する:

```bash
python3 -c "
import subprocess, re
result = subprocess.run(['strings', 'crates/fulgur/tests/snapshots/tagged_struct_tree_nested.pdf'],
    capture_output=True, text=True)
for line in result.stdout.splitlines():
    if any(k in line for k in ['/Div', '/Hn', '/P', '/Document', '/StructElem', '/StructTreeRoot']):
        print(line)
" 2>/dev/null | head -20
```

`/Div`（section）と `/Hn`（h1）と `/P`（p）が存在することを確認する。

**Step 5: 再実行でテストが通ることを確認**

```bash
cargo test -p fulgur --test render_smoke snapshot_tagged_struct_tree_nested tagged_pdf_is_deterministic 2>&1 | tail -5
```

期待結果: `test result: ok. 2 passed`

**Step 6: コミット**

```bash
git add crates/fulgur/tests/render_smoke.rs crates/fulgur/tests/snapshots/
git commit -m "test(render): add PDF snapshot tests for hierarchical TagTree (fulgur-izp.5)"
```

---

### Task 4: lib テスト全パスと既存 VRT 影響確認

**Step 1: lib テスト全パス確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

期待結果: `test result: ok. N passed; 0 failed`

**Step 2: integration テスト全パス確認**

```bash
cargo test -p fulgur --tests 2>&1 | tail -10
```

期待結果: `test result: ok. N passed; 0 failed`

**Step 3: タグなし PDF がバイト変化しないことを確認**

```bash
cargo test -p fulgur --test render_smoke untagged 2>&1 | tail -5
```

期待結果: `ok. N passed` (タグなしパスは `tag_collector = None` のまま、ビルダーは呼ばれない)

**Step 4: スナップショットファイルを .gitignore から除外されていないことを確認**

```bash
git check-ignore crates/fulgur/tests/snapshots/tagged_struct_tree_nested.pdf && echo "IGNORED (bad)" || echo "tracked (ok)"
```

期待結果: `tracked (ok)` — スナップショットは git に追跡される

**Step 5: 最終コミット（必要があれば）**

変更があれば:
```bash
git add -A
git commit -m "chore(tagged-pdf): ensure snapshot files are tracked (fulgur-izp.5)"
```
