# inline-block y座標バグ修正実装プラン

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `display:inline-block` な要素がブロックコンテナの直接子として末尾に来たとき、y座標が 0（カード先頭）になるバグを修正する。

**Architecture:** まずデバッグログで実際のTaffy座標を確認して根本原因を特定し、その後最小限の修正を加える。VRTフィクスチャで回帰を防ぐ。

**Tech Stack:** Rust, `crates/fulgur/src/convert.rs`, `crates/fulgur/src/pageable.rs`, `crates/fulgur-vrt/`

---

### Task 1: 最小再現フィクスチャを作成する

**Files:**
- Create: `crates/fulgur-vrt/fixtures/layout/review_card_inline_block.html`

**Step 1: フィクスチャファイルを作成する**

```html
<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  body { font-family: sans-serif; font-size: 10pt; margin: 0; padding: 20pt; }
</style>
</head>
<body>
<div class="review-card" style="background:#f9f9f9;border-left:4pt solid #e94560;padding:12pt;margin-bottom:12pt;">
  <div class="review-header" style="display:flex;justify-content:space-between;margin-bottom:6pt;">
    <span style="font-weight:700;color:#0f3460;">enricoschaaf</span>
    <span style="font-size:8pt;color:#999;">GitHub Issue #88</span>
  </div>
  <div style="font-size:9.5pt;color:#333;font-style:italic;">
    "Really cool project! I'm trying to use it in existing repository as a library..."
  </div>
  <span style="display:inline-block;background:#d4edda;color:#155724;padding:1pt 7pt;border-radius:10pt;margin-top:6pt;font-size:7.5pt;">
    ✓ 高評価
  </span>
</div>
</body>
</html>
```

保存先: `crates/fulgur-vrt/fixtures/layout/review_card_inline_block.html`

**Step 2: ファイルが存在することを確認する**

```bash
ls crates/fulgur-vrt/fixtures/review_card_inline_block.html
```

Expected: ファイルが表示される

---

### Task 2: デバッグログを追加する

**Files:**
- Modify: `crates/fulgur/src/convert.rs` (collect_positioned_children 内, line ~1150付近)

**Step 1: `FULGUR_DEBUG_LAYOUT` 環境変数でログを有効化するコードを追加する**

`collect_positioned_children` 関数内、line 1150 の `layout_in_pt` の呼び出し直後に追加:

```rust
        let (cx, cy, cw, ch) = layout_in_pt(&child_node.final_layout);

        // Temporary debug: FULGUR_DEBUG_LAYOUT=1 で座標を出力
        if std::env::var("FULGUR_DEBUG_LAYOUT").is_ok() {
            let tag = child_node
                .element_data()
                .map(|ed| ed.name.local.as_ref().to_string())
                .unwrap_or_else(|| "(text)".to_string());
            eprintln!(
                "[layout] id={} <{}> cx={:.1} cy={:.1} cw={:.1} ch={:.1} inline_root={} children={}",
                child_id,
                tag,
                cx, cy, cw, ch,
                child_node.flags.is_inline_root(),
                child_node.children.len()
            );
        }
```

また、zero-size フラット化ブランチの先頭にもログを追加する (line ~1179付近):

```rust
        if ch == 0.0 && cw == 0.0 && !child_node.children.is_empty() {
            // Temporary debug
            if std::env::var("FULGUR_DEBUG_LAYOUT").is_ok() {
                let tag = child_node
                    .element_data()
                    .map(|ed| ed.name.local.as_ref().to_string())
                    .unwrap_or_else(|| "(text)".to_string());
                eprintln!(
                    "[layout] FLATTEN id={} <{}> at cx={:.1} cy={:.1} (zero-size container)",
                    child_id, tag, cx, cy
                );
            }
```

**Step 2: コンパイルが通ることを確認する**

```bash
cd crates/fulgur && cargo build 2>&1 | grep -E "error|warning: unused" | head -20
```

Expected: エラーなし

---

### Task 3: デバッグログを実行して根本原因を確認する

**Step 1: フィクスチャをPDFに変換してログを取得する**

```bash
FULGUR_DEBUG_LAYOUT=1 cargo run --bin fulgur -- render \
  crates/fulgur-vrt/fixtures/review_card_inline_block.html \
  -o /tmp/review_debug.pdf 2>&1 | grep "\[layout\]"
```

**Step 2: ログを解析して根本原因を判断する**

以下のいずれかのパターンを確認:

**Pattern A (zero-size フラット化バグ):**
```
[layout] FLATTEN id=NNN <span> at cx=12.0 cy=60.0 (zero-size container)
[layout] id=NNN <span> cx=0.0 cy=0.0 ...   ← ★ フラット化後に y=0 になっている
```
→ Hypothesis A が正しい。Task 4A を実装する。

**Pattern B (Taffy が y=0 を返す):**
```
[layout] id=NNN <span> cx=12.0 cy=0.0 cw=... ch=... ← ★ Taffy 自体が y=0
```
→ Hypothesis B が正しい。Task 4B を検討する。

**Pattern C (Split によるリベース問題):**
ログでは cy が正しいが PDF 上で y=0 → Task 4C を検討する。

---

### Task 4A: 修正 — zero-size フラット化時に親の y オフセットを引き継ぐ

*(Task 3 で Pattern A が確認された場合のみ実施)*

**Files:**
- Modify: `crates/fulgur/src/convert.rs` (collect_positioned_children, line ~1179)

**Context:** `thead`, `tbody`, `tr` などのテーブル構造要素は Taffy がその子の座標を grandparent 相対で直接計算するため、これらのフラット化時にオフセットを加算すると二重計算になる。テーブル要素を除外して inline-block の anonymous block にのみオフセットを適用する。

**Step 1: フラット化ブランチでオフセットを伝播する修正を実装する**

`collect_positioned_children` の zero-size フラット化部分 (line ~1179) を以下のように修正する:

```rust
        if ch == 0.0 && cw == 0.0 && !child_node.children.is_empty() {
            // ... (existing string-set, counter-op, bookmark harvesting code) ...

            let mut nested = collect_positioned_children(doc, &child_node.children, ctx, depth + 1);

            // Table structural elements (thead/tbody/tfoot/tr/col/colgroup) are
            // flattened without offset because Taffy already positions their
            // children relative to the grandparent in its table layout algorithm.
            // For all other zero-size containers (e.g. anonymous block wrappers
            // around inline-block elements), the children's final_layout is
            // relative to the container, so we must propagate the container's
            // own offset.
            let is_table_structural = child_node.element_data().is_some_and(|ed| {
                matches!(
                    ed.name.local.as_ref(),
                    "thead" | "tbody" | "tfoot" | "tr" | "col" | "colgroup"
                )
            });
            if !is_table_structural && (cx != 0.0 || cy != 0.0) {
                for pc in &mut nested {
                    pc.x += cx;
                    pc.y += cy;
                }
            }

            result.extend(nested);
            continue;
        }
```

**Step 2: コンパイルが通ることを確認する**

```bash
cargo build -p fulgur 2>&1 | grep "^error" | head -10
```

Expected: エラーなし

**Step 3: デバッグ付きで再実行して修正を確認する**

```bash
FULGUR_DEBUG_LAYOUT=1 cargo run --bin fulgur -- render \
  crates/fulgur-vrt/fixtures/review_card_inline_block.html \
  -o /tmp/review_fixed.pdf 2>&1 | grep "\[layout\]"
```

Expected: span が正しい cy (>0) で登録されていることを確認

**Step 4: 既存ユニットテストが全通過することを確認する**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: `0 failed`

**Step 5: テーブルレイアウトの回帰がないことを確認する**

```bash
cargo test -p fulgur 2>&1 | tail -10
```

Expected: 全テスト通過

**Step 6: コミットする**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "fix(convert): propagate anonymous block offset when flattening zero-size containers

Inline-block spans that are direct children of block containers may be
wrapped in an anonymous block that Taffy reports as zero-size. The
flattening path in collect_positioned_children discarded the parent
container's cx/cy, placing inline-block content at y=0 (card top).

Table structural elements (thead/tbody/tr etc.) are exempt because Taffy
already positions their children relative to the grandparent in its table
layout algorithm.

Fixes fulgur-z2ho."
```

---

### Task 4B: 修正 — Taffy が inline-block に y=0 を返す場合の調査

*(Task 3 で Pattern B が確認された場合のみ実施)*

**Step 1: Blitz の DOM 構造を `debug_print_tree` で確認する**

`convert.rs` の `render_pageable` または `render_html` 呼び出し直前に一時的に追加:

```rust
debug_print_tree(doc, root_id, 0);
```

`debug_print_tree` (convert.rs:156 付近) で inline_root と各ノードの座標を確認する。

**Step 2: inline-block span の扱いを Blitz ソースで確認する**

```bash
grep -rn "inline.block\|InlineBlock" ~/.cargo/registry/src/*/blitz-dom-*/src/ | head -20
```

**Step 3:** 確認した結果に基づいて Taffy/Blitz のレイアウト結果を修正するアプローチを設計する（別途 advisor を呼んで方針を確認してから実装する）。

---

### Task 5: デバッグログを削除する

**Files:**
- Modify: `crates/fulgur/src/convert.rs`

**Step 1: Task 2 で追加したデバッグログをすべて削除する**

`FULGUR_DEBUG_LAYOUT` に関するすべての `if std::env::var(...)` ブロックを削除する。

**Step 2: コンパイル確認**

```bash
cargo build -p fulgur 2>&1 | grep "^error" | head -10
```

Expected: エラーなし

**Step 3: コミットする**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "chore: remove debug layout logging"
```

---

### Task 6: VRT フィクスチャとしてゴールデンを生成する

**Files:**
- Modify: `crates/fulgur-vrt/src/` (既存の VRT テスト追加パターンに従う)

**Step 1: 既存 VRT テストのパターンを確認する**

```bash
grep -rn "fixtures\|fixture_path\|render_html_fixture" crates/fulgur-vrt/src/ | head -20
```

**Step 2: フィクスチャの VRT テスト関数を追加する**

既存パターンに合わせて `crates/fulgur-vrt/src/` の適切なファイルに追加する:

```rust
#[test]
fn review_card_inline_block_tag_position() {
    // inline-block span at end of block container must appear at card bottom,
    // not at card top (fulgur-z2ho regression test)
    render_html_fixture("review_card_inline_block");
}
```

**Step 3: ゴールデンを生成する**

```bash
UPDATE_SNAPSHOTS=1 cargo test -p fulgur-vrt review_card_inline_block 2>&1 | tail -10
```

Expected: ゴールデン PNG が生成される

**Step 4: ゴールデン PNG を目視確認する**

```bash
ls crates/fulgur-vrt/snapshots/review_card_inline_block*.png
```

生成された PNG を確認して、span がカード下部に正しく配置されていることを確認する。

**Step 5: テストが通ることを確認する**

```bash
cargo test -p fulgur-vrt review_card_inline_block 2>&1 | tail -5
```

Expected: `test ... ok`

**Step 6: コミットする**

```bash
git add crates/fulgur-vrt/fixtures/review_card_inline_block.html \
        crates/fulgur-vrt/src/ \
        crates/fulgur-vrt/snapshots/
git commit -m "test(vrt): add regression fixture for inline-block tag position (fulgur-z2ho)"
```

---

### Task 7: 最終検証

**Step 1: 全ユニットテスト通過確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur 2>&1 | tail -5
```

Expected: 全テスト通過、`0 failed`

**Step 2: Clippy 確認**

```bash
cargo clippy -p fulgur 2>&1 | grep "^error" | head -10
```

Expected: エラーなし

**Step 3: fmt 確認**

```bash
cargo fmt --check 2>&1
```

Expected: 差分なし

**Step 4: 最終コミット確認**

```bash
git log --oneline -5
```

Expected: Task 4x, 5, 6 のコミットが積まれている
