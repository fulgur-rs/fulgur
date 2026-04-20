# border-style: double hairline fallback (fulgur-xca) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS Backgrounds Level 3 準拠で `border-style: double` かつ `border-width < 3px` のとき solid として描画する。

**Architecture:** `pageable.rs` の 2 箇所（`draw_border_line` の `Double` arm と `draw_block_border` の `Double` uniform rect branch）でガードを追加し、`width < 3.0` のときは solid と同じ描画経路へフォールバックする。

**Tech Stack:** Rust / Krilla / fulgur `pageable.rs`

**Spec reference:** [CSS Backgrounds Level 3 §4.2](https://www.w3.org/TR/css-backgrounds-3/#border-style) — *If the border width is less than 3 CSS pixels, the double border should be drawn as a solid border.*

**Issue:** [fulgur-xca](../../.beads/) — per-edge fallback (`pageable.rs` の `draw_border_line::Double`) と rect fast path (`draw_block_border` の `st == Double` 分岐) の双方で現在 `bt < 3` 判定がなく、`border: 2px double` が 0.67px x 2 本の hairline になる。

---

## Task 1: Failing test for rect fast path

**Files:**

- Test: `crates/fulgur/tests/rect_borders_test.rs`

**Step 1: Write the failing test**

`crates/fulgur/tests/rect_borders_test.rs` の末尾に追加（既存 `double_uniform_border_uses_two_rects` の下）:

```rust
#[test]
fn double_uniform_border_below_3px_falls_back_to_solid() {
    // CSS Backgrounds L3: computed border-width < 3 の double は solid で描く。
    // rect fast path (draw_block_border の Double 分岐) が < 3px のとき
    // 2 本の hairline を emit せず、solid と同じ 1 本の rect 経路になるかを確認する。
    let html = r#"
        <html><head><style>
            .b { width: 200px; height: 100px; border: 2px double #444; }
        </style></head><body><div class="b"></div></body></html>
    "#;

    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).unwrap();

    let Some(counts) = count_ops(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };

    // Solid と同じく 1 本の rect subpath のみ許容。2 本なら未修正。
    assert!(
        counts.m + counts.re <= 1,
        "double < 3px should collapse to single solid rect, got m={} re={}",
        counts.m,
        counts.re,
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p fulgur --test rect_borders_test double_uniform_border_below_3px_falls_back_to_solid -- --nocapture`

Expected: FAIL — `m + re = 2` で "should collapse to single solid rect" assertion が落ちる。

**Step 3: Commit failing test**

```bash
git add crates/fulgur/tests/rect_borders_test.rs
git commit -m "test(borders): failing test for double < 3px fast path fallback"
```

---

## Task 2: Fix rect fast path

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (`draw_block_border` の Double 分岐)

**Step 1: Locate the Double branch**

`draw_block_border` 内、`matches!(st, BorderStyleValue::Solid | BorderStyleValue::Double)` の分岐（現行 `pageable.rs:1438` 周辺）。

**Step 2: Add `< 3` guard**

`if st == BorderStyleValue::Double` ブロックの先頭でガードし、solid 経路へ落とす。現行コード:

```rust
if st == BorderStyleValue::Double {
    // Double = 3 equal bands (border/gap/border): thin_w = bt/3.
    // Stroke centerlines: outer at bt/6, inner at bt*5/6.
    let thin_w = bt / 3.0;
    let stroke_thin = colored_stroke(bc, thin_w, opacity);
    stroke_inset_rect(canvas, x, y, w, h, thin_w / 2.0, stroke_thin.clone());
    stroke_inset_rect(canvas, x, y, w, h, bt - thin_w / 2.0, stroke_thin);
} else {
    let base = colored_stroke(bc, bt, opacity);
    if let Some(styled) = apply_border_style(base, st, bt) {
        stroke_inset_rect(canvas, x, y, w, h, bt / 2.0, styled);
    }
}
```

改修後:

```rust
// CSS Backgrounds L3: border-width < 3 の double は solid として描画。
if st == BorderStyleValue::Double && bt >= 3.0 {
    // Double = 3 equal bands (border/gap/border): thin_w = bt/3.
    // Stroke centerlines: outer at bt/6, inner at bt*5/6.
    let thin_w = bt / 3.0;
    let stroke_thin = colored_stroke(bc, thin_w, opacity);
    stroke_inset_rect(canvas, x, y, w, h, thin_w / 2.0, stroke_thin.clone());
    stroke_inset_rect(canvas, x, y, w, h, bt - thin_w / 2.0, stroke_thin);
} else {
    let base = colored_stroke(bc, bt, opacity);
    if let Some(styled) = apply_border_style(base, st, bt) {
        stroke_inset_rect(canvas, x, y, w, h, bt / 2.0, styled);
    }
}
```

注: `apply_border_style` は `Double` を `Some(stroke)` で返すため（pageable.rs:1229-1233）、else 側に落ちても solid 相当で 1 本の rect stroke になる。

**Step 3: Run test to verify it passes**

Run: `cargo test -p fulgur --test rect_borders_test double_uniform_border_below_3px_falls_back_to_solid -- --nocapture`

Expected: PASS

**Step 4: Verify no regressions**

Run: `cargo test -p fulgur --test rect_borders_test`

Expected: all 4 tests pass (`table_header_uses_rect_for_uniform_borders`, `dashed_uniform_border_keeps_per_edge_phase`, `double_uniform_border_uses_two_rects`, `double_uniform_border_below_3px_falls_back_to_solid`)。

**Step 5: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "fix(borders): double < 3px falls back to solid in rect fast path"
```

---

## Task 3: Failing test for per-edge fallback

**Files:**

- Test: `crates/fulgur/tests/rect_borders_test.rs`

**Step 1: Write the failing test**

non-uniform style または `has_radius()` を使い、rect fast path ではなく `draw_border_line` を走らせる。最もシンプルなのは `border-radius` を追加して rounded path 分岐を避け、かつ uniform style を崩して per-edge へ落とす。簡易なやり方は non-uniform width:

```rust
#[test]
fn double_per_edge_below_3px_falls_back_to_solid() {
    // non-uniform 境界（幅不一致）で rect fast path を避け、
    // draw_border_line の Double arm を通す経路。
    // < 3px のとき hairline 2 本ではなく単一 solid stroke になるか検証する。
    let html = r#"
        <html><head><style>
            .b {
                width: 200px;
                height: 100px;
                border-style: double;
                border-top-width: 2px;
                border-right-width: 4px;
                border-bottom-width: 2px;
                border-left-width: 4px;
                border-color: #444;
            }
        </style></head><body><div class="b"></div></body></html>
    "#;

    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).unwrap();

    let Some(counts) = count_ops(&pdf) else {
        eprintln!("qpdf not installed — skipping");
        return;
    };

    // top/bottom 2px double → solid fallback で 1 本ずつ = 2 本
    // left/right 4px double → 3 本 stroke 扱いで 2 本ずつ = 4 本
    // 合計 <= 6 本の stroke。fallback 無し (< 3px で 2 本ずつ emit) だと
    // top/bottom が 2 本ずつ = 4、left/right が 2 本ずつ = 4 で合計 8 本になる。
    // 線ごとに `m` が 1 個出るので m をしきい値にする。
    assert!(
        counts.m <= 6,
        "double < 3px per-edge should reduce stroke count, got m={} l={}",
        counts.m,
        counts.l,
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p fulgur --test rect_borders_test double_per_edge_below_3px_falls_back_to_solid -- --nocapture`

Expected: FAIL — 修正前は top/bottom も double で 2 本 × 2 辺 = 4、left/right も double で 2 本 × 2 辺 = 4、合計 `m=8`。

注: しきい値は実測で確認する。最初の実行で actual `m` を控え、solid fallback 後の actual `m` も測って、fallback 有無を区別できる境界値に置き換える。

**Step 3: Measure actual counts**

失敗出力（`--nocapture` の eprintln で実数を出してもよい）で修正前 `m` と修正後 `m` を確認し、閾値を両者の中間に調整。

**Step 4: Commit failing test**

```bash
git add crates/fulgur/tests/rect_borders_test.rs
git commit -m "test(borders): failing test for double < 3px per-edge fallback"
```

---

## Task 4: Fix per-edge fallback

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (`draw_border_line` の Double arm)

**Step 1: Add `< 3` guard in `draw_border_line`**

現行 (pageable.rs:1327-1341):

```rust
match style {
    BorderStyleValue::Double => {
        let gap = width / 3.0;
        let dx = x2 - x1;
        let dy = y2 - y1;
        let len = (dx * dx + dy * dy).sqrt();
        if len == 0.0 {
            return;
        }
        let nx = -dy / len * gap;
        let ny = dx / len * gap;
        let thin = colored_stroke(base_color, width / 3.0, opacity);
        stroke_line(canvas, x1 + nx, y1 + ny, x2 + nx, y2 + ny, thin.clone());
        stroke_line(canvas, x1 - nx, y1 - ny, x2 - nx, y2 - ny, thin);
    }
```

改修後（`width < 3.0` のとき match の末尾 `_` アームと同じ solid 経路へ落とす）:

```rust
match style {
    BorderStyleValue::Double if width >= 3.0 => {
        let gap = width / 3.0;
        let dx = x2 - x1;
        let dy = y2 - y1;
        let len = (dx * dx + dy * dy).sqrt();
        if len == 0.0 {
            return;
        }
        let nx = -dy / len * gap;
        let ny = dx / len * gap;
        let thin = colored_stroke(base_color, width / 3.0, opacity);
        stroke_line(canvas, x1 + nx, y1 + ny, x2 + nx, y2 + ny, thin.clone());
        stroke_line(canvas, x1 - nx, y1 - ny, x2 - nx, y2 - ny, thin);
    }
```

理由: match guard `if width >= 3.0` を付けることで、`width < 3.0` の Double は最終 `_` arm（`apply_border_style` 経由の solid）に流れる。`apply_border_style` は Double を `Some(stroke)` で返すため、solid と同じ 1 本 stroke を emit する。

**Step 2: Run test to verify it passes**

Run: `cargo test -p fulgur --test rect_borders_test double_per_edge_below_3px_falls_back_to_solid -- --nocapture`

Expected: PASS

**Step 3: Verify no regressions**

Run: `cargo test -p fulgur --test rect_borders_test`

Expected: 全 pass。

**Step 4: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "fix(borders): double < 3px falls back to solid in per-edge path"
```

---

## Task 5: Full-suite verification

**Step 1: Run all fulgur tests**

Run: `cargo test -p fulgur`

Expected: 全 pass（unit ~497 + integration）。

**Step 2: Lint**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

Expected: どちらも成功。

**Step 3: If any fail, fix and re-run**

lint 指摘が出たら fix、`git add` して amend ではなく新規コミット（CLAUDE.md: "Always create NEW commits rather than amending"）。

---

## Out of scope

- `border-style: double` で `width >= 3` のときの描画ロジックの改善。
- 3D styles (`groove`, `ridge`, `inset`, `outset`) の < 3px 挙動（別仕様）。
- rect fast path で `has_radius()` かつ `Double` の fallback（既存は rounded 分岐へ、今回の問題は発生しない）。

## Notes

- テスト戦略: `count_ops` で PDF operator 数を境界に取る。既存の `double_uniform_border_uses_two_rects` と同じ手法。
- spec 境界値（3px 丁度のとき）は double として描画。`bt >= 3.0` と書く。
- `apply_border_style` の Double → `Some(stroke)` 挙動（pageable.rs:1229-1233）が fallback 経路の正しさを担保する。
