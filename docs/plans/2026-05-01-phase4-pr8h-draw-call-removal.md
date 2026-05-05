# Phase 4 PR 8h: 残 Pageable::draw 呼び出しの除去 — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** render.rs に残る 3 箇所の `Pageable::draw()` 呼び出しを v2 draw ヘルパーに置き換え、`RenderCache` 型を `(Drawables, PaginationGeometryTable)` に変える。

**Architecture:** (1) `draw_list_item_marker` の Raster/Svg 分岐を `draw_image_v2` / `draw_svg_v2` に切り替える。(2) `MarginBoxRenderer` の cache miss パスで `run_pass` + `dom_to_drawables` を呼び、`draw_v2_page` で描画する。`RenderCache` 型エイリアスも合わせて変更する。

**Tech Stack:** Rust, `crates/fulgur/src/render.rs`, `crates/fulgur/tests/render_smoke.rs`

---

## Task 1: SVG リストマーカーの smoke test を追加

`ImageMarker::Svg` 分岐 (render.rs:2573) のカバレッジがない。テストを先に書いておく。

**Files:**

- Modify: `crates/fulgur/tests/render_smoke.rs`

**Step 1: テストを追加**

`render_smoke.rs` の末尾に追加する。

```rust
#[test]
fn render_v2_smoke_list_item_svg_marker() {
    // Exercises `draw_list_item_marker`'s `ImageMarker::Svg` branch
    // (render.rs: svg.draw call) — a `<li>` with an SVG list-style-image.
    let svg_data = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><circle cx="5" cy="5" r="4" fill="blue"/></svg>"#;
    let mut bundle = AssetBundle::default();
    bundle.add_css(r#"li { list-style-image: url("bullet.svg"); }"#);
    bundle.add_image("bullet.svg", svg_data.to_vec());
    let html = r##"<!doctype html><html><body><ul><li>Alpha</li><li>Beta</li></ul></body></html>"##;
    let engine = Engine::builder().assets(bundle).build();
    let pdf = engine.render_html(html).expect("v2 render");
    assert!(!pdf.is_empty());
}
```

**Step 2: テストが通ることを確認（既存コードで pass する）**

```bash
cd /home/ubuntu/fulgur/.worktrees/feat-phase4-pr8h
cargo test -p fulgur --test render_smoke render_v2_smoke_list_item_svg_marker 2>&1 | tail -5
```

Expected: `test render_v2_smoke_list_item_svg_marker ... ok`

**Step 3: MarginBoxRenderer 用 smoke test を追加**

`render_smoke.rs` の末尾に続けて追加する。

```rust
#[test]
fn render_v2_smoke_margin_box_renderer() {
    // Exercises `MarginBoxRenderer`'s Stage 3 draw path
    // (render.rs: pageable.draw call) — a simple @top-center counter.
    let html = r##"<!DOCTYPE html><html><head><style>
        @page { margin: 36pt; @top-center { content: counter(page); } }
        body { margin: 0; }
        p { height: 500pt; background: #eee; }
    </style></head><body><p>Page 1</p><p>Page 2</p></body></html>"##;
    let engine = Engine::builder().build();
    let pdf = engine.render_html(html).expect("v2 render");
    assert!(!pdf.is_empty());
}
```

**Step 4: テストが通ることを確認（既存コードで pass する）**

```bash
cargo test -p fulgur --test render_smoke render_v2_smoke_margin_box_renderer 2>&1 | tail -5
```

Expected: `test render_v2_smoke_margin_box_renderer ... ok`

**Step 5: コミット**

```bash
git add crates/fulgur/tests/render_smoke.rs
git commit -m "test(render_smoke): add smoke tests for svg list marker and margin box renderer"
```

---

## Task 2: ListItemMarker — img.draw / svg.draw を v2 ヘルパーに置き換え

`draw_list_item_marker` (render.rs:2549) の `img.draw()` (行 2570) と `svg.draw()` (行 2573) を `draw_image_v2` / `draw_svg_v2` に置き換える。

**Files:**

- Modify: `crates/fulgur/src/render.rs:2565-2577`

**Step 1: 実装を変更**

`render.rs` の `draw_list_item_marker` 内、`match marker {` ブロックを以下に置き換える。

変更前 (行 2568〜2576):

```rust
            match marker {
                ImageMarker::Raster(img) => {
                    img.draw(canvas, marker_x, marker_y, *width, *height);
                }
                ImageMarker::Svg(svg) => {
                    svg.draw(canvas, marker_x, marker_y, *width, *height);
                }
            }
```

変更後:

```rust
            match marker {
                ImageMarker::Raster(img) => {
                    let entry = crate::drawables::ImageEntry {
                        image_data: img.image_data.clone(),
                        format: img.format,
                        width: img.width,
                        height: img.height,
                        opacity: img.opacity,
                        visible: img.visible,
                    };
                    draw_image_v2(canvas, &entry, marker_x, marker_y);
                }
                ImageMarker::Svg(svg) => {
                    let entry = crate::drawables::SvgEntry {
                        tree: svg.tree.clone(),
                        width: svg.width,
                        height: svg.height,
                        opacity: svg.opacity,
                        visible: svg.visible,
                    };
                    draw_svg_v2(canvas, &entry, marker_x, marker_y);
                }
            }
```

`img.width == *width` かつ `img.height == *height`（`list_marker.rs` で同じ `size_raster_marker()` 戻り値から設定）なので avail 引数を無視しても出力は変わらない。

**Step 2: コンパイル確認**

```bash
cargo build -p fulgur 2>&1 | grep -E "^error" | head -10
```

Expected: エラーなし

**Step 3: テストで動作確認**

```bash
cargo test -p fulgur --test render_smoke render_v2_smoke_list_item_image_marker 2>&1 | tail -5
cargo test -p fulgur --test render_smoke render_v2_smoke_list_item_svg_marker 2>&1 | tail -5
```

Expected: 両方 ok

**Step 4: lib テスト全通過確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: `test result: ok. 811 passed; 0 failed`

**Step 5: コミット**

```bash
git add crates/fulgur/src/render.rs
git commit -m "refactor(render): replace img/svg .draw() in draw_list_item_marker with v2 helpers (Phase 4 PR 8h)"
```

---

## Task 3: MarginBoxRenderer — RenderCache 型変更と Stage 3 を v2 描画に切り替え

`RenderCache` 型エイリアスを `(Drawables, PaginationGeometryTable)` に変え、Stage 3 の cache miss / draw ロジックを `dom_to_drawables` + `draw_v2_page` に置き換える。

**Files:**

- Modify: `crates/fulgur/src/render.rs:2811, 3090-3125`

**Step 1: RenderCache 型エイリアスを変更**

行 2811:

```rust
// 変更前:
type RenderCache = HashMap<(String, u32, u32), Box<dyn Pageable>>;

// 変更後:
type RenderCache = HashMap<
    (String, u32, u32),
    (crate::drawables::Drawables, crate::pagination_layout::PaginationGeometryTable),
>;
```

**Step 2: cache miss パスを変更**

render.rs の `if !self.render_cache.contains_key(&cache_key)` ブロック（行 3092〜3119）を以下に置き換える。

変更前（行 3092〜3119）:

```rust
            if !self.render_cache.contains_key(&cache_key) {
                let render_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\">{}</body></html>",
                    self.margin_css, html
                );
                let render_doc = crate::blitz_adapter::parse_and_layout(
                    &render_html,
                    crate::convert::pt_to_px(rect.width),
                    crate::convert::pt_to_px(rect.height),
                    self.font_data,
                );
                let dummy_store = RunningElementStore::new();
                let mut dummy_ctx = crate::convert::ConvertContext {
                    running_store: &dummy_store,
                    assets: None,
                    font_cache: HashMap::new(),
                    string_set_by_node: HashMap::new(),
                    counter_ops_by_node: HashMap::new(),
                    bookmark_by_node: HashMap::new(),
                    column_styles: crate::column_css::ColumnStyleTable::new(),
                    multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
                    pagination_geometry: crate::pagination_layout::PaginationGeometryTable::new(),
                    link_cache: Default::default(),
                    viewport_size_px: None,
                };
                let pageable = crate::convert::dom_to_pageable(&render_doc, &mut dummy_ctx);
                self.render_cache.insert(cache_key.clone(), pageable);
            }
```

変更後:

```rust
            if !self.render_cache.contains_key(&cache_key) {
                let render_html = format!(
                    "<html><head><style>{}</style></head><body style=\"margin:0;padding:0;\">{}</body></html>",
                    self.margin_css, html
                );
                let mut render_doc = crate::blitz_adapter::parse_and_layout(
                    &render_html,
                    crate::convert::pt_to_px(rect.width),
                    crate::convert::pt_to_px(rect.height),
                    self.font_data,
                );
                let geometry = crate::pagination_layout::run_pass(
                    render_doc.deref_mut(),
                    crate::convert::pt_to_px(rect.height),
                );
                let dummy_store = RunningElementStore::new();
                let mut dummy_ctx = crate::convert::ConvertContext {
                    running_store: &dummy_store,
                    assets: None,
                    font_cache: HashMap::new(),
                    string_set_by_node: HashMap::new(),
                    counter_ops_by_node: HashMap::new(),
                    bookmark_by_node: HashMap::new(),
                    column_styles: crate::column_css::ColumnStyleTable::new(),
                    multicol_geometry: crate::multicol_layout::MulticolGeometryTable::new(),
                    pagination_geometry: geometry,
                    link_cache: Default::default(),
                    viewport_size_px: None,
                };
                let drawables = crate::convert::dom_to_drawables(&render_doc, &mut dummy_ctx);
                let geometry = dummy_ctx.pagination_geometry;
                self.render_cache.insert(cache_key.clone(), (drawables, geometry));
            }
```

**Step 3: draw パスを変更**

続く `if let Some(pageable) = ...` ブロック（行 3121〜3123）を以下に置き換える。

変更前:

```rust
            if let Some(pageable) = self.render_cache.get(&cache_key) {
                pageable.draw(canvas, rect.x, rect.y, rect.width, rect.height);
            }
```

変更後:

```rust
            if let Some((drawables, geometry)) = self.render_cache.get(&cache_key) {
                if let Some(root_id) = drawables.root_id
                    && let Some(root_block) = drawables.block_styles.get(&root_id)
                {
                    paint_root_block_v2(canvas, root_block, rect.x, rect.y);
                }
                draw_v2_page(canvas, 0, rect.x, rect.y, geometry, drawables);
            }
```

**Step 4: コンパイル確認**

```bash
cargo build -p fulgur 2>&1 | grep -E "^error" | head -10
```

Expected: エラーなし。`Box<dyn Pageable>` に関する unused import 警告が出る場合は次のステップで除去する。

**Step 5: 不要になった import / use を確認・除去**

`render.rs` 先頭付近で `dom_to_pageable` または `Pageable` トレイトへの use 参照が残っていれば除去する。

```bash
cargo clippy -p fulgur 2>&1 | grep "unused import\|never used" | grep -i "pageable" | head -10
```

警告が出ていれば該当行を削除する。

**Step 6: テストで動作確認**

```bash
cargo test -p fulgur --test render_smoke render_v2_smoke_margin_box_renderer 2>&1 | tail -5
cargo test -p fulgur --test render_smoke test_render_html_link_stylesheet_with_gcpm 2>&1 | tail -5
```

Expected: 両方 ok

**Step 7: lib テスト全通過確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: `test result: ok. 811 passed; 0 failed`

**Step 8: コミット**

```bash
git add crates/fulgur/src/render.rs
git commit -m "refactor(render): migrate MarginBoxRenderer to Drawables/draw_v2_page (Phase 4 PR 8h)"
```

---

## Task 4: `Pageable::draw` の呼び出しがゼロになったことを確認 & 全テスト通過

`render.rs` に `.draw(` が残っていないことを確認し、clippy / fmt も通すことを確認する。

**Files:** なし（確認のみ）

**Step 1: .draw( の残存確認**

```bash
grep -n "\.draw(" /home/ubuntu/fulgur/.worktrees/feat-phase4-pr8h/crates/fulgur/src/render.rs
```

Expected: `draw_with_opacity`, `draw_block_border`, `draw_list_item_with_block` など内部ヘルパー名のみ表示される。`img.draw` / `svg.draw` / `pageable.draw` の行が **ゼロ** であること。

**Step 2: clippy 確認**

```bash
cargo clippy -p fulgur 2>&1 | grep "^error" | head -10
```

Expected: エラーなし

**Step 3: fmt 確認**

```bash
cargo fmt -p fulgur --check 2>&1 | head -10
```

Expected: エラーなし（差分があれば `cargo fmt -p fulgur` で整形してコミット）

**Step 4: 全 lib テスト確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: `test result: ok. 811 passed; 0 failed`

**Step 5: integration テスト確認**

```bash
cargo test -p fulgur --test render_smoke 2>&1 | tail -10
cargo test -p fulgur --test gcpm_integration 2>&1 | tail -5
```

Expected: 全テスト ok
