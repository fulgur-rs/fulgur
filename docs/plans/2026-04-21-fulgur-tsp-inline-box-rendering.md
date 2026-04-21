# fulgur-tsp inline-box rendering Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `display: inline-block` / `inline-flex` / `inline-grid` / `inline-table` を PDF に正しく描画できるようにする。現状 `convert.rs::extract_paragraph` が Parley の `PositionedLayoutItem::InlineBox` を無視して silently drop している問題を修正する。

**Architecture:** `LineItem` に `InlineBox(InlineBoxItem)` variant を追加。`InlineBoxItem` は内部 Pageable (`Block(BlockPageable)` / `Paragraph(ParagraphPageable)` の enum) と Parley の `PositionedInlineBox` から取った位置情報を保持。`extract_paragraph` は InlineBox variant に対して既存の `convert_node_inner` を再帰呼び出しして子ツリーを構築。`draw_shaped_lines` は transform push/pop で位置合わせして内部 Pageable の `draw()` を呼ぶ。`recalculate_line_box` の match を網羅にして InlineBox は既存 `computed_y`（extract 時に line-relative に変換済み）をそのまま使う。

**Tech Stack:** Rust / Blitz 0.2.4 / Parley 0.6 / Krilla。既存の `ConvertContext` / `AssetBundle` / `LinkCache` をそのまま共有。

**Related issue:** `bd show fulgur-tsp`（design フィールドに詳細）

---

## Task 1: Add `InlineBoxContent` / `InlineBoxItem` types + `LineItem::InlineBox` variant

**Files:**

- Modify: `crates/fulgur/src/paragraph.rs` (enum LineItem 周辺 ~L132)

**Step 1: Write the failing test (paragraph.rs unit tests 末尾)**

`crates/fulgur/src/paragraph.rs` の `#[cfg(test)] mod tests` に追加:

```rust
#[test]
fn line_item_inline_box_variant_can_be_constructed() {
    use crate::pageable::BlockPageable;
    let block = BlockPageable::new(50.0, 20.0);
    let item = LineItem::InlineBox(InlineBoxItem {
        content: InlineBoxContent::Block(block),
        width: 50.0,
        height: 20.0,
        x_offset: 10.0,
        computed_y: 0.0,
        link: None,
        opacity: 1.0,
        visible: true,
    });
    match item {
        LineItem::InlineBox(ib) => {
            assert_eq!(ib.width, 50.0);
            assert_eq!(ib.height, 20.0);
            assert!(matches!(ib.content, InlineBoxContent::Block(_)));
        }
        _ => panic!("expected InlineBox variant"),
    }
}
```

**Step 2: Run the test to verify it fails**

```bash
cargo test -p fulgur --lib line_item_inline_box_variant_can_be_constructed 2>&1 | tail
```

Expected: FAIL — unknown variant `InlineBox`.

**Step 3: Implement types**

`paragraph.rs` の `LineItem` enum 直上にデータ型を追加し、enum に variant を加える:

```rust
/// Content of an atomic inline box. Concrete enum (not `Box<dyn Pageable>`)
/// so `LineItem` can derive Clone without `dyn_clone`.
#[derive(Clone)]
pub enum InlineBoxContent {
    Block(crate::pageable::BlockPageable),
    Paragraph(ParagraphPageable),
}

/// An atomic inline box (display: inline-block / inline-flex / inline-grid /
/// inline-table) within a shaped line.
#[derive(Clone)]
pub struct InlineBoxItem {
    pub content: InlineBoxContent,
    /// Width / height of the box in pt (from Parley, px→pt converted).
    pub width: f32,
    pub height: f32,
    /// X offset from paragraph left edge, in pt.
    pub x_offset: f32,
    /// Y offset from the line top, in pt (extract_paragraph converts
    /// Parley's paragraph-relative `y` to line-relative by subtracting
    /// the accumulated line_top).
    pub computed_y: f32,
    pub link: Option<Arc<LinkSpan>>,
    pub opacity: f32,
    pub visible: bool,
}

#[derive(Clone, Debug)]
pub enum LineItem {
    Text(ShapedGlyphRun),
    Image(InlineImage),
    InlineBox(InlineBoxItem),
}
```

注記: `InlineBoxContent` と `InlineBoxItem` は `#[derive(Clone)]` のみ。`Debug` は
`BlockPageable`/`ParagraphPageable` に derive が無いので **付けない**。`LineItem` の既存 `Debug` derive は残せないので、`#[derive(Clone, Debug)]` を `#[derive(Clone)]` に変更し、必要なら手書きの `Debug` impl は後続 task で対応（現状テストで Debug を使っている箇所がないか Step 4 で確認）。

**Step 4: Fix compile errors from non-exhaustive matches**

`cargo build -p fulgur 2>&1 | tail -40` で match の網羅性エラーを確認。以下 3 箇所に `LineItem::InlineBox(_) => { /* handled later */ }` 相当のアームを追加（暫定、後続 task で本実装）:

- `paragraph.rs::draw_line_decorations` (L408〜) — `LineItem::InlineBox(_) => continue` を追加
- `paragraph.rs::draw_shaped_lines` (L502〜) — `LineItem::InlineBox(_) => { /* TODO: Task 3 */ }` を追加
- `paragraph.rs::recalculate_line_box` の 2 箇所 (L653, L684) — `LineItem::InlineBox(_) => continue` を追加
- `paragraph.rs` の L709, L780 付近 — `if let LineItem::Image(...)` はそのままで OK（分岐のない if let なら網羅性エラーは出ない）

`Debug` に関しては、`LineItem` の `#[derive(Debug)]` を外し、必要箇所で `Debug` を要求するテストがないか確認。なければそのまま進む。

**Step 5: Run the test to verify it passes**

```bash
cargo build -p fulgur 2>&1 | tail -5
cargo test -p fulgur --lib line_item_inline_box_variant_can_be_constructed 2>&1 | tail
```

Expected: build OK, test PASS.

**Step 6: Run full lib suite to confirm no regression**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 494 passed (493 baseline + 1 new).

**Step 7: Commit**

```bash
git add crates/fulgur/src/paragraph.rs
git commit -m "feat(fulgur-tsp): add LineItem::InlineBox variant and data types

Introduces InlineBoxContent (Block/Paragraph) enum and InlineBoxItem
struct to carry atomic inline-box data. Match arms in draw_line_decorations,
draw_shaped_lines, and recalculate_line_box get placeholder handling —
real draw logic lands in Task 3.

fulgur-tsp"
```

---

## Task 2: extract_paragraph handles `PositionedLayoutItem::InlineBox`

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (extract_paragraph L1955〜 and helper)
- Test: `crates/fulgur/src/convert.rs` の `#[cfg(test)] mod tests` または新規 unit 場所

**Step 1: Write the failing test**

`crates/fulgur/src/convert.rs` 末尾に `#[cfg(test)] mod inline_box_tests { ... }` を追加（既存テストモジュールの有無確認後）:

```rust
#[cfg(test)]
mod inline_box_extraction_tests {
    use crate::engine::Engine;
    use crate::paragraph::{InlineBoxContent, LineItem, ParagraphPageable};
    use crate::pageable::{BlockPageable, Pageable, PositionedChild};

    /// Walk the Pageable tree and return the first ParagraphPageable found.
    fn find_paragraph(root: &dyn Pageable) -> Option<&ParagraphPageable> {
        if let Some(p) = root.as_any().downcast_ref::<ParagraphPageable>() {
            return Some(p);
        }
        if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
            for PositionedChild { child, .. } in &block.children {
                if let Some(p) = find_paragraph(child.as_ref()) {
                    return Some(p);
                }
            }
        }
        None
    }

    fn build_tree(html: &str) -> Box<dyn Pageable> {
        Engine::builder()
            .build()
            .build_pageable_for_testing_no_gcpm(html)
    }

    #[test]
    fn inline_block_becomes_line_item_inline_box() {
        let html = r#"<!DOCTYPE html><html><body><p>before <span style="display:inline-block;width:40px;height:20px;background:red"></span> after</p></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");

        let found = para
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .find(|it| matches!(it, LineItem::InlineBox(_)));
        assert!(found.is_some(), "inline-block should appear as LineItem::InlineBox");
    }

    #[test]
    fn inline_block_with_block_child_has_block_content() {
        let html = r#"<!DOCTYPE html><html><body><p><span style="display:inline-block;width:40px;height:20px"><div>inner</div></span></p></body></html>"#;
        let tree = build_tree(html);
        let para = find_paragraph(tree.as_ref()).expect("paragraph expected");
        let ib = para
            .lines
            .iter()
            .flat_map(|l| l.items.iter())
            .find_map(|it| match it {
                LineItem::InlineBox(ib) => Some(ib),
                _ => None,
            })
            .expect("InlineBox expected");
        // 内部に <div> を持つので Block content 期待 (具象 display の決定は
        // convert_node_inner の通常パスに任せる)
        assert!(matches!(&ib.content, InlineBoxContent::Block(_)));
    }
}
```

**Step 2: Run the test to verify it fails**

```bash
cargo test -p fulgur --lib inline_block_becomes_line_item_inline_box 2>&1 | tail
```

Expected: FAIL — `LineItem::InlineBox` not found in items.

**Step 3: Add `convert_inline_box_node` helper and wire in extract_paragraph**

`convert.rs` の `extract_paragraph` 近辺にヘルパーを追加:

```rust
/// Convert a Blitz node referenced by a Parley InlineBox into an
/// InlineBoxContent. Uses the existing top-level converter so that border /
/// background / padding / inner layout all work via the usual paths.
fn convert_inline_box_node(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Option<crate::paragraph::InlineBoxContent> {
    use crate::paragraph::InlineBoxContent;
    use crate::pageable::{BlockPageable, ParagraphPageable as _};
    // Re-enter the normal converter. The boxed dyn trait comes back and we
    // downcast to the two concrete types we expect. Anything else (rare) is
    // ignored to avoid smuggling Spacer / Image / wrapper Pageables into a
    // line item, since those were never meant to be atomic inline content.
    let pageable = convert_node(doc, node_id, ctx, depth + 1);
    let any = pageable.as_any();
    if let Some(block) = any.downcast_ref::<BlockPageable>() {
        return Some(InlineBoxContent::Block(block.clone()));
    }
    if let Some(para) = any.downcast_ref::<crate::paragraph::ParagraphPageable>() {
        return Some(InlineBoxContent::Paragraph(para.clone()));
    }
    None
}
```

注記: `as_any()` は既存の `Pageable` trait 経由で利用可。downcast 失敗時は `None` を返して当該 InlineBox を silently drop（現状と同じ挙動、ただし通常経路では Block か Paragraph のどちらかになる前提）。

`extract_paragraph` のループ内で `PositionedLayoutItem::InlineBox(positioned)` を処理:

```rust
// 既存の let mut shaped_lines = Vec::new(); の近くに追加:
let mut accumulated_line_top: f32 = 0.0;

for line in parley_layout.lines() {
    let metrics = line.metrics();
    let mut items = Vec::new();

    for item in line.items() {
        match item {
            parley::PositionedLayoutItem::GlyphRun(glyph_run) => {
                // 既存ロジックそのまま
            }
            parley::PositionedLayoutItem::InlineBox(positioned) => {
                let node_id = positioned.id as usize;
                // ConvertContext には depth を持たないので、呼び出し元から
                // 引き継げるように extract_paragraph の signature に depth を追加。
                // (呼び出し元 convert_node_inner 側で depth + 1 を渡す)
                let Some(content) =
                    convert_inline_box_node(doc, node_id, ctx, _depth)
                else {
                    continue;
                };
                let link = link_cache.lookup(doc, node_id);
                items.push(LineItem::InlineBox(InlineBoxItem {
                    content,
                    width: px_to_pt(positioned.width),
                    height: px_to_pt(positioned.height),
                    x_offset: px_to_pt(positioned.x),
                    computed_y: px_to_pt(positioned.y) - accumulated_line_top,
                    link,
                    opacity: 1.0,
                    visible: true,
                }));
            }
        }
    }

    shaped_lines.push(ShapedLine {
        height: px_to_pt(metrics.line_height),
        baseline: px_to_pt(metrics.baseline),
        items,
    });
    accumulated_line_top += px_to_pt(metrics.line_height);
}
```

`extract_paragraph` の signature に `depth: usize` を加え、呼び出し側（`convert_node_inner`）で `depth + 1` を渡すように更新。再帰無限ループ防止のため `depth >= MAX_DOM_DEPTH` ガードは既存 `convert_node` 冒頭で既に効く。

**Step 4: Run the extraction tests**

```bash
cargo test -p fulgur --lib inline_block_becomes_line_item_inline_box 2>&1 | tail
cargo test -p fulgur --lib inline_block_with_block_child_has_block_content 2>&1 | tail
```

Expected: PASS. 描画自体は未実装でも、tree 構築は成功する。

**Step 5: Run full lib suite**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: regression ゼロ。

**Step 6: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(fulgur-tsp): extract PositionedLayoutItem::InlineBox in extract_paragraph

Adds convert_inline_box_node helper that recursively invokes convert_node
on the Blitz node referenced by Parley's InlineBox id, downcasts to
Block/Paragraph content, and pushes a LineItem::InlineBox with px→pt
converted geometry.

fulgur-tsp"
```

---

## Task 3: `draw_shaped_lines` renders `LineItem::InlineBox`

**Files:**

- Modify: `crates/fulgur/src/paragraph.rs::draw_shaped_lines` (L502〜)

**Step 1: Write the failing test (VRT の代わりに unit で最小検証)**

`crates/fulgur/src/paragraph.rs` の unit tests に追加。mock canvas を使わず、PDF バイト列出力の smoke として既存の link_collect_tests と同じ手法で engine 経由の test を書く:

`crates/fulgur/tests/` に新規ファイル `inline_box_render_test.rs` を作成:

```rust
//! Integration: inline-box is actually rendered (non-empty PDF bytes).

use fulgur::config::RenderConfig;
use fulgur::engine::Engine;

fn render(html: &str) -> Vec<u8> {
    let engine = Engine::builder().build();
    engine.render_html(html).expect("render ok")
}

#[test]
fn inline_block_with_background_produces_output() {
    // ベースライン: inline-block 無しと有りで PDF バイト数に差が出ること。
    // 現状 (inline-box 未対応) は両者ほぼ同じバイト数 → 差が出れば描画されている証拠。
    let without = render(
        r#"<!DOCTYPE html><html><body><p>hello world</p></body></html>"#,
    );
    let with_ib = render(
        r#"<!DOCTYPE html><html><body><p>hello <span style="display:inline-block;width:40px;height:20px;background:red"></span> world</p></body></html>"#,
    );
    // inline-block の background が描画されていれば PDF サイズが増える
    assert!(
        with_ib.len() > without.len() + 50,
        "inline-block with background should add to PDF size: without={}, with={}",
        without.len(),
        with_ib.len()
    );
}
```

**Step 2: Run to verify it fails**

```bash
cargo test -p fulgur --test inline_box_render_test 2>&1 | tail
```

Expected: FAIL — サイズ差が出ない（inline-box 未描画）。

**Step 3: Implement InlineBox drawing in `draw_shaped_lines`**

`paragraph.rs::draw_shaped_lines` の match に本実装を入れる:

```rust
LineItem::InlineBox(ib) => {
    if !ib.visible { continue; }
    crate::pageable::draw_with_opacity(canvas, ib.opacity, |canvas| {
        let ox = x + ib.x_offset;
        let oy = line_top_abs + ib.computed_y;
        let transform = krilla::geom::Transform::from_translate(ox, oy);
        canvas.surface.push_transform(&transform);
        // 内部 Pageable を (0, 0) 起点で描画。avail_width/height は自身のサイズ。
        match &ib.content {
            InlineBoxContent::Block(b) => {
                b.draw(canvas, 0.0, 0.0, ib.width, ib.height);
            }
            InlineBoxContent::Paragraph(p) => {
                p.draw(canvas, 0.0, 0.0, ib.width, ib.height);
            }
        }
        canvas.surface.pop();
    });

    // link rect 登録 (InlineBox 全体を 1 矩形として)
    if let Some(link_span) = ib.link.as_ref() {
        let rect = crate::pageable::Rect {
            x: x + ib.x_offset,
            y: line_top_abs + ib.computed_y,
            width: ib.width.max(0.0),
            height: ib.height.max(0.0),
        };
        if let Some(collector) = canvas.link_collector.as_deref_mut() {
            collector.push_rect(link_span, rect);
        }
    }
}
```

`use` 文に `InlineBoxContent` を追加（同ファイル内なので不要だが、パスを調整）。

**Step 4: Run the render test**

```bash
cargo test -p fulgur --test inline_box_render_test 2>&1 | tail
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: render test PASS。lib suite 全 pass。

**Step 5: Commit**

```bash
git add crates/fulgur/src/paragraph.rs crates/fulgur/tests/inline_box_render_test.rs
git commit -m "feat(fulgur-tsp): draw LineItem::InlineBox via transform push/pop

draw_shaped_lines now positions atomic inline boxes via a translate
transform and dispatches to the inner Block/Paragraph draw(). Link rects
are recorded for the whole box so <a> around inline-block works.

fulgur-tsp"
```

---

## Task 4: Link rect coverage test

**Files:**

- Test: `crates/fulgur/tests/inline_box_render_test.rs`

**Step 1: Add link-coverage test**

既存 `inline_box_render_test.rs` に追加:

```rust
#[test]
fn inline_block_inside_anchor_gets_link_rect() {
    let html = r#"<!DOCTYPE html><html><body><p><a href="https://example.com"><span style="display:inline-block;width:40px;height:20px;background:red"></span></a></p></body></html>"#;
    let bytes = render(html);
    // krilla emits /Link annotations as "/Annot" objects in the PDF stream.
    let s = String::from_utf8_lossy(&bytes);
    assert!(
        s.contains("/Link") || s.contains("/URI"),
        "expected a link annotation in the PDF"
    );
}
```

**Step 2: Run**

```bash
cargo test -p fulgur --test inline_box_render_test inline_block_inside_anchor_gets_link_rect 2>&1 | tail
```

Expected: PASS (link_cache.lookup が node_id を引き当て、draw_shaped_lines が rect を登録する経路が成立)。

**Step 3: Commit**

```bash
git add crates/fulgur/tests/inline_box_render_test.rs
git commit -m "test(fulgur-tsp): inline-block inside <a> emits a link annotation

fulgur-tsp"
```

---

## Task 5: Overflow-clip integration test (fulgur-i5a prerequisite green)

**Background:** fulgur-i5a の ignored テスト `inline_block_with_overflow_hidden_becomes_clipped_block` は fulgur-i5a ブランチ内にあり main にまだ来ていない。fulgur-tsp のスコープでは、そのテストが **将来 green にできる前提が整っている** ことを確認したい。

**Approach:** fulgur-tsp 側で同等のテストを `crates/fulgur/tests/inline_box_overflow_test.rs` に新規作成し、(a) inline-block + overflow:hidden が BlockPageable に wrap され、(b) その BlockPageable が ParagraphPageable の `LineItem::InlineBox` に積まれることを確認する。fulgur-i5a 側の overflow-clip 実装が main にマージされているかは `fulgur-i5a` 側の別 issue なので、ここでは **BlockPageable wrap 部分は書かず、既存の convert 挙動をそのまま検証する。**

実際には、fulgur-i5a の修正（inline-block + overflow:hidden を BlockPageable に wrap する）はすでに `convert_node_inner` に入っている可能性がある。main の `convert.rs:L2213` 近辺に overflow スタイル拾いがある旨 fulgur-i5a の design に書かれているので、まずはそれを確認してから test を書く。

**Files:**

- Create: `crates/fulgur/tests/inline_box_overflow_test.rs`

**Step 1: Verify current main state**

```bash
grep -n "has_overflow_clip\|needs_block_wrapper" crates/fulgur/src/pageable.rs | head
grep -n "overflow\|Overflow::Clip" crates/fulgur/src/convert.rs | head
```

overflow-clip ロジックが main にあり、inline-block + overflow:hidden が BlockPageable に wrap される挙動になっているかを確認。なっていない場合、このタスクは「fulgur-i5a 側で wrap 実装が入った後に enable」と書き換える。なっていれば Step 2 へ。

**Step 2: Write integration test**

```rust
//! Integration: inline-block with overflow:hidden becomes a clipped
//! BlockPageable, reachable via a ParagraphPageable's LineItem::InlineBox.

use fulgur::engine::Engine;
use fulgur::paragraph::{InlineBoxContent, LineItem, ParagraphPageable};
use fulgur::pageable::{BlockPageable, Pageable, PositionedChild};

fn build_tree(html: &str) -> Box<dyn Pageable> {
    Engine::builder()
        .build()
        .build_pageable_for_testing_no_gcpm(html)
}

fn walk_paragraphs<'a>(root: &'a dyn Pageable, out: &mut Vec<&'a ParagraphPageable>) {
    if let Some(p) = root.as_any().downcast_ref::<ParagraphPageable>() {
        out.push(p);
        return;
    }
    if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
        for PositionedChild { child, .. } in &block.children {
            walk_paragraphs(child.as_ref(), out);
        }
    }
}

#[test]
fn inline_block_with_overflow_hidden_is_reachable_as_clipped_block() {
    let html = r#"<!DOCTYPE html><html><head><style>
        .ib {
            display: inline-block;
            width: 100px;
            height: 50px;
            overflow: hidden;
            background: #eee;
        }
    </style></head><body><p><span class="ib"><span style="display:inline-block;width:200px;height:200px;background:red"></span></span></p></body></html>"#;
    let tree = build_tree(html);
    let mut paras = Vec::new();
    walk_paragraphs(tree.as_ref(), &mut paras);

    let clipped = paras.iter().flat_map(|p| p.lines.iter()).flat_map(|l| l.items.iter())
        .find_map(|it| match it {
            LineItem::InlineBox(ib) => match &ib.content {
                InlineBoxContent::Block(b) if b.style.has_overflow_clip() => Some(b),
                _ => None,
            },
            _ => None,
        });
    assert!(
        clipped.is_some(),
        "expected an inline-block with overflow clip to be reachable via LineItem::InlineBox"
    );
}
```

**Step 3: Run**

```bash
cargo test -p fulgur --test inline_box_overflow_test 2>&1 | tail
```

Expected: PASS if overflow-clip wrap logic is on main; otherwise FAIL — 判定の結果を見て対処（fulgur-i5a 未マージなら `#[ignore]` で保留 + コメントで理由を記述）。

**Step 4: Commit (PASS の場合)**

```bash
git add crates/fulgur/tests/inline_box_overflow_test.rs
git commit -m "test(fulgur-tsp): inline-block + overflow:hidden reachable as clipped BlockPageable

Verifies that fulgur-tsp's inline-box rendering path makes the
overflow-clipped BlockPageable discoverable through a ParagraphPageable's
LineItem::InlineBox. Once this and fulgur-i5a both land, the ignored test
in overflow_integration.rs can be unignored.

fulgur-tsp"
```

---

## Task 6: VRT fixtures

**Files:**

- Create: `crates/fulgur-vrt/fixtures/layout/inline-block-basic.html`
- Create: `crates/fulgur-vrt/fixtures/layout/inline-block-overflow-hidden.html`
- Create: `crates/fulgur-vrt/fixtures/layout/inline-block-nested.html`
- Create: `crates/fulgur-vrt/fixtures/layout/inline-flex-smoke.html`
- Create: `crates/fulgur-vrt/fixtures/layout/inline-grid-smoke.html`
- Create (goldens): `crates/fulgur-vrt/goldens/fulgur/layout/inline-block-*.png` など (`FULGUR_VRT_UPDATE=1` で自動生成)

**Step 1: Author fixtures**

5 本の HTML を作成。例:

```html
<!-- inline-block-basic.html -->
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>VRT: layout/inline-block-basic</title>
<style>
  html,body{margin:0;padding:0;font-family:sans-serif}
  .wrap{padding:40px;font-size:16px}
  .ib{display:inline-block;width:80px;height:40px;border:2px solid #333;background:#def;padding:4px}
</style>
</head><body>
<div class="wrap">before <span class="ib">ib</span> after</div>
</body></html>
```

各 fixture で検証したい観点:

- `inline-block-basic`: border + background + padding + 内部テキストがそろって描画される
- `inline-block-overflow-hidden`: 100×50 親 + 200×200 子の clip (fulgur-i5a の視覚版)
- `inline-block-nested`: `<span inline-block><span inline-block></span></span>`
- `inline-flex-smoke`: `<span style="display:inline-flex">` + 子 2 個で横並び
- `inline-grid-smoke`: `<span style="display:inline-grid;grid-template-columns:30px 30px">` + 子 2 個

**Step 2: Generate goldens**

```bash
FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt 2>&1 | tail
```

**Step 3: Inspect goldens visually**

```bash
ls crates/fulgur-vrt/goldens/fulgur/layout/inline-*
```

PNG を開いて意図通りの描画か確認（border / background / clip / 内部レイアウト）。意図と異なれば fixture を調整 → goldens 再生成。

**Step 4: Run VRT diff clean**

```bash
cargo test -p fulgur-vrt 2>&1 | tail
```

Expected: all fixtures pass, 0 diff.

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/fixtures/layout/inline-*.html \
        crates/fulgur-vrt/goldens/fulgur/layout/inline-*.png
git commit -m "test(fulgur-tsp): VRT fixtures for inline-box rendering

Adds 5 fixtures covering inline-block basics, overflow clip, nesting,
inline-flex/inline-grid smoke.

fulgur-tsp"
```

---

## Task 7: Verification sweep & format/lint

**Step 1: Clippy**

```bash
cargo clippy --all-targets -- -D warnings 2>&1 | tail
```

Expected: 0 warnings. Fix any new warnings introduced.

**Step 2: Format**

```bash
cargo fmt --check 2>&1 | tail
```

Expected: clean. If diff, run `cargo fmt` and include in a cleanup commit.

**Step 3: Full test suite**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur 2>&1 | tail -5
cargo test -p fulgur-vrt 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -10
```

Expected: all pass.

**Step 4: Markdownlint on plan file**

```bash
npx markdownlint-cli2 '**/*.md' 2>&1 | tail
```

Expected: clean.

**Step 5: If any fmt changes are needed, commit**

```bash
git diff
git add -u
git commit -m "style(fulgur-tsp): cargo fmt cleanup

fulgur-tsp"
```

---

## Out of scope (tracked elsewhere)

- inline-block 内部でのページまたぎ (atomic 扱いで当面固定)
- `vertical-align` の非 baseline 値の厳密対応 (Parley の `y` そのまま採用)
- `<img>` / form controls を `LineItem::InlineBox` 経路に統一すること (現行 `LineItem::Image` 維持)
- fulgur-i5a の `overflow_integration.rs` 側 `#[ignore]` 解除 (fulgur-i5a 側 PR で実施)

## Success criteria (A.C. from issue)

1. [ ] Task 2, 3 完了: `display: inline-block / inline-flex / inline-grid / inline-table` が PDF に描画される
2. [ ] Task 3, 6 完了: border / background / padding / overflow:hidden が効く
3. [ ] Task 5 完了: `inline_block_with_overflow_hidden_is_reachable_as_clipped_block` が green（fulgur-i5a 側の ignored テストの前提条件が整う）
4. [ ] Task 6, 7 完了: VRT fixture 5 本 + regression なし
5. [ ] Task 7 完了: `cargo test -p fulgur{,-vrt} --lib` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --check` 全 pass
