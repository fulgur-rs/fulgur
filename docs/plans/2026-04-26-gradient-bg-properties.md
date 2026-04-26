# Gradient × background-size/position/repeat Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** linear-gradient / radial-gradient を CSS Images §3 に準拠させ、`background-size` / `background-position` / `background-repeat` と完全連携させる。

**Architecture:** gradient は intrinsic dimensions / aspect ratio を持たないため、新ヘルパー `resolve_gradient_size` で "no intrinsic dimensions" ルールを実装。`draw_background_layer` の gradient ブランチを image 経路に合流させ、`resolve_position` → `compute_tile_positions` → 各 tile に gradient を再描画する。`corner_to_angle_rad` も tile box 基準で再計算するように修正。

**Tech Stack:** Rust / krilla (LinearGradient, RadialGradient) / fulgur Pageable layer / fulgur-vrt PDF byte-wise comparison harness

**Beads Issue:** fulgur-4ono

**Worktree:** `.worktrees/fulgur-4ono-gradient-bg-props` / branch `feature/fulgur-4ono-gradient-bg-props`

**Reference files:**
- `crates/fulgur/src/background.rs:179-300` — `draw_background_layer` の gradient ブランチ
- `crates/fulgur/src/background.rs:332-408` — `draw_linear_gradient`
- `crates/fulgur/src/background.rs:415-544` — `draw_radial_gradient`
- `crates/fulgur/src/background.rs:599-642` — `resolve_size`, `resolve_lp`, `resolve_position`
- `crates/fulgur/src/background.rs:734-823` — `compute_tile_positions`, `resolve_repeat_axis`
- `crates/fulgur/src/pageable.rs:805-841` — `BgImageContent`, `BackgroundLayer`
- `crates/fulgur-vrt/tests/gradient_harness.rs` — VRT harness パターン

---

## Task 1: `resolve_gradient_size` ヘルパー (TDD)

**Files:**
- Modify: `crates/fulgur/src/background.rs` (新規ヘルパー + 単体テスト)

**Step 1: 失敗する単体テストを書く**

`crates/fulgur/src/background.rs` の `#[cfg(test)] mod tests` 内に、`make_layer` ヘルパー定義の直後あたりに 5 つのテストを追加する:

```rust
#[test]
fn resolve_gradient_size_auto_returns_origin() {
    let (w, h) = resolve_gradient_size(&BgSize::Auto, 200.0, 100.0);
    assert!((w - 200.0).abs() < 1e-6);
    assert!((h - 100.0).abs() < 1e-6);
}

#[test]
fn resolve_gradient_size_cover_returns_origin() {
    let (w, h) = resolve_gradient_size(&BgSize::Cover, 200.0, 100.0);
    assert!((w - 200.0).abs() < 1e-6);
    assert!((h - 100.0).abs() < 1e-6);
}

#[test]
fn resolve_gradient_size_contain_returns_origin() {
    let (w, h) = resolve_gradient_size(&BgSize::Contain, 200.0, 100.0);
    assert!((w - 200.0).abs() < 1e-6);
    assert!((h - 100.0).abs() < 1e-6);
}

#[test]
fn resolve_gradient_size_explicit_both_resolves() {
    let size = BgSize::Explicit(
        Some(BgLengthPercentage::Length(50.0)),
        Some(BgLengthPercentage::Percentage(0.25)),
    );
    let (w, h) = resolve_gradient_size(&size, 200.0, 100.0);
    assert!((w - 50.0).abs() < 1e-6);
    assert!((h - 25.0).abs() < 1e-6);
}

#[test]
fn resolve_gradient_size_explicit_one_auto_uses_origin() {
    // width 指定、height auto → height は origin 高さ (no aspect だから)
    let size = BgSize::Explicit(Some(BgLengthPercentage::Length(80.0)), None);
    let (w, h) = resolve_gradient_size(&size, 200.0, 100.0);
    assert!((w - 80.0).abs() < 1e-6);
    assert!((h - 100.0).abs() < 1e-6);

    // height 指定、width auto → width は origin 幅
    let size = BgSize::Explicit(None, Some(BgLengthPercentage::Percentage(0.5)));
    let (w, h) = resolve_gradient_size(&size, 200.0, 100.0);
    assert!((w - 200.0).abs() < 1e-6);
    assert!((h - 50.0).abs() < 1e-6);
}
```

**Step 2: テストが失敗することを確認**

```bash
cd .worktrees/fulgur-4ono-gradient-bg-props
cargo test -p fulgur --lib resolve_gradient_size 2>&1 | tail -20
```

Expected: コンパイルエラー (`resolve_gradient_size` は未定義)。

**Step 3: 最小実装を書く**

`crates/fulgur/src/background.rs` の `resolve_size` 関数の直前(または直後)に追加:

```rust
/// Resolve `background-size` for a gradient layer.
///
/// CSS Images §3.3 / §5.5: gradient は intrinsic dimensions / aspect ratio を
/// 持たないため、`auto` / `cover` / `contain` はすべて positioning area を埋める
/// 結果になる。`Explicit` で片方が `None` の場合も同様に positioning area の
/// 該当軸サイズを使う。
fn resolve_gradient_size(size: &BgSize, origin_w: f32, origin_h: f32) -> (f32, f32) {
    match size {
        BgSize::Auto | BgSize::Cover | BgSize::Contain => (origin_w, origin_h),
        BgSize::Explicit(w_opt, h_opt) => {
            let rw = w_opt
                .as_ref()
                .map(|v| resolve_lp(v, origin_w))
                .unwrap_or(origin_w);
            let rh = h_opt
                .as_ref()
                .map(|v| resolve_lp(v, origin_h))
                .unwrap_or(origin_h);
            (rw, rh)
        }
    }
}
```

**Step 4: テストが通ることを確認**

```bash
cargo test -p fulgur --lib resolve_gradient_size 2>&1 | tail -10
```

Expected: `5 passed` (新規 5 テスト)。

**Step 5: clippy / fmt**

```bash
cargo clippy -p fulgur --lib --all-targets 2>&1 | tail -10
cargo fmt --check
```

Expected: 警告 0、フォーマット差分なし。

**Step 6: コミット (plan ファイルも含める)**

```bash
git add crates/fulgur/src/background.rs docs/plans/2026-04-26-gradient-bg-properties.md
git commit -m "feat(gradient): add resolve_gradient_size helper for no-intrinsic sizing"
```

---

## Task 2: `draw_background_layer` を gradient 経路で統合

**事前周知 — byte-wise regression と最終形**:

- **Task 2 単独**: 新コードでは default `repeat: Repeat` / `size: auto` の gradient でも `compute_tile_positions` が **`image == clip` 縮退ケースで 4 tile** を生成する (epsilon `+ 0.01` の境界)。3 tile は clip 外で視覚的に等価だが PDF byte は変化する。Task 2 の commit ではこの byte 差分を意図的なものとして既存 gradient golden を `FULGUR_VRT_UPDATE=1` で再生成する。
- **後続の refine commit (`perf(background): collapse single-image-covers-clip case to one tile`) が compute_tile_positions に degenerate fast-path を追加** し、`image >= clip` (Round 除く) の場合は 1 tile に縮退するように改修。これにより最終的な PDF byte stream は Phase 1 と完全一致 (sha256 一致確認済) になり、Task 2 で再生成した golden は再度 main HEAD に戻る。本セクションは「Task 2 単独で push したら 4 tile になる」という中間状態を残しているが、PR レビューの過程で fast-path を追加したため、最終形は 1 tile / byte-identical with main となる。

**Files:**
- Modify: `crates/fulgur/src/background.rs:179-300` — `draw_background_layer`

**Step 1: 既存テストの状態を記録**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 既存 ~340 テストすべて pass。これを基準とする(後で regression 0 を確認するため)。

**Step 2: `draw_background_layer` の gradient ブランチを書き換える**

現状 (`background.rs:207-298`):

```rust
match &layer.content {
    BgImageContent::LinearGradient { direction, stops } => {
        let angle_rad = match direction {
            crate::pageable::LinearGradientDirection::Angle(a) => *a,
            crate::pageable::LinearGradientDirection::Corner(corner) =>
                corner_to_angle_rad(*corner, ow, oh),
        };
        draw_linear_gradient(canvas, angle_rad, stops, ox, oy, ow, oh);
    }
    BgImageContent::RadialGradient { shape, size, position_x, position_y, stops } => {
        draw_radial_gradient(canvas, *shape, size, position_x, position_y, stops, ox, oy, ow, oh);
    }
    BgImageContent::Raster { .. } | BgImageContent::Svg { .. } => {
        let (img_w, img_h) = resolve_size(layer, ow, oh);
        // ... 既存 image 経路 ...
    }
}
```

これを以下に置き換える(順序: gradient と image を同じ resolve / position / tile 流に乗せる):

```rust
let (img_w, img_h) = match &layer.content {
    BgImageContent::LinearGradient { .. } | BgImageContent::RadialGradient { .. } => {
        resolve_gradient_size(&layer.size, ow, oh)
    }
    BgImageContent::Raster { .. } | BgImageContent::Svg { .. } => {
        resolve_size(layer, ow, oh)
    }
};
if img_w <= 0.0 || img_h <= 0.0 {
    canvas.surface.pop();
    return;
}

let pos_x = ox + resolve_position(&layer.position_x, ow, img_w);
let pos_y = oy + resolve_position(&layer.position_y, oh, img_h);

let tiles = compute_tile_positions(
    layer.repeat_x,
    layer.repeat_y,
    pos_x,
    pos_y,
    img_w,
    img_h,
    cx,
    cy,
    cw,
    ch,
);
if tiles.is_empty() {
    canvas.surface.pop();
    return;
}

match &layer.content {
    BgImageContent::LinearGradient { direction, stops } => {
        for (tx, ty, tw, th) in &tiles {
            let angle_rad = match direction {
                crate::pageable::LinearGradientDirection::Angle(a) => *a,
                crate::pageable::LinearGradientDirection::Corner(corner) => {
                    corner_to_angle_rad(*corner, *tw, *th)
                }
            };
            draw_linear_gradient(canvas, angle_rad, stops, *tx, *ty, *tw, *th);
        }
    }
    BgImageContent::RadialGradient { shape, size, position_x, position_y, stops } => {
        for (tx, ty, tw, th) in &tiles {
            draw_radial_gradient(
                canvas, *shape, size, position_x, position_y, stops, *tx, *ty, *tw, *th,
            );
        }
    }
    BgImageContent::Raster { data, format } => {
        let data: krilla::Data = Arc::clone(data).into();
        let Ok(image) = format.to_krilla_image(data) else {
            canvas.surface.pop();
            return;
        };
        for (tx, ty, tw, th) in &tiles {
            let Some(size) = krilla::geom::Size::from_wh(*tw, *th) else {
                continue;
            };
            let transform = krilla::geom::Transform::from_translate(*tx, *ty);
            canvas.surface.push_transform(&transform);
            canvas.surface.draw_image(image.clone(), size);
            canvas.surface.pop();
        }
    }
    BgImageContent::Svg { tree } => {
        use krilla_svg::{SurfaceExt, SvgSettings};
        for (tx, ty, tw, th) in &tiles {
            let Some(size) = krilla::geom::Size::from_wh(*tw, *th) else {
                continue;
            };
            let transform = krilla::geom::Transform::from_translate(*tx, *ty);
            canvas.surface.push_transform(&transform);
            if canvas
                .surface
                .draw_svg(tree, size, SvgSettings::default())
                .is_none()
            {
                log::warn!("failed to draw SVG background tile");
            }
            canvas.surface.pop();
        }
    }
}
```

**Step 3: 既存テストが通ることを確認(regression 0)**

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```

Expected: 既存 ~340 + 新規 5 = ~345 テスト pass、0 fail。

**重要な確認**: Task 2 単独では `image == clip` 縮退ケースで `compute_tile_positions` が 4 tile (boundary epsilon の overshoot) を生成し、PDF byte は Phase 1 から変化する (3 tile は clip 外で視覚等価)。`background-size: auto` / `repeat: repeat (デフォルト) で position: 0 0` の場合の **最終的な byte-wise 一致** は後続の fast-path refine commit で達成される — Task 2 の VRT step (Step 4) では一旦 golden を再生成するが、refine 後に Phase 1 と同じ hash に戻る。

**Step 4: 既存 VRT を実行 → 想定通り regression なら golden 更新**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -30
```

期待: gradient 関連 golden は byte-wise 不一致になる(tile 4 個分の `set_fill`+`draw_path` が PDF stream に追加されるため)。それ以外(image / SVG / 他)は一致するはず。

差分内容を `pdftocairo` で目視 (raster diff が 0 = 視覚等価) し、視覚等価なら golden を再生成する:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt 2>&1 | tail -10
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10  # 再 PASS 確認
```

もし image / SVG 系も regression していたら停止して調査する(その場合は Step 2 のロジックに想定外の副作用がある)。

**Step 5: clippy / fmt**

```bash
cargo clippy -p fulgur --lib --all-targets 2>&1 | tail -10
cargo fmt --check
```

Expected: 警告 0、差分なし。

**Step 6: コミット (golden 更新を含む)**

```bash
git add crates/fulgur/src/background.rs crates/fulgur-vrt/goldens/
git commit -m "$(cat <<'EOF'
feat(gradient): integrate background-size/position/repeat into gradient rendering

Gradient layers now flow through the same resolve_gradient_size /
resolve_position / compute_tile_positions path as raster/SVG. Each tile
redraws the gradient inside its tile box, so background-size, -position,
and -repeat fully apply (CSS Images §3).

corner_to_angle_rad now receives the tile box instead of the origin
rect; this is the spec-correct gradient-image aspect (CSS Images §3.1.1).
Phase 1 happened to be correct only because tile == origin in degenerate
cases.

Phase 1 gradient goldens are regenerated: with the new code the default
"image == clip" repeat case emits 4 boundary tiles (clipped to identical
visual output). The byte change is intentional and visually equivalent —
verified by pdftocairo raster diff.
EOF
)"
```

---

## Task 3: VRT 追加 (gradient × bg-property の組み合わせ)

**Files:**
- Modify: `crates/fulgur-vrt/tests/gradient_harness.rs` (既存 harness にケース追加)
- Possibly create: `crates/fulgur-vrt/goldens/fulgur/gradient_bg_props/*.pdf` (golden は `FULGUR_VRT_UPDATE=1` で生成)

**Step 1: 既存 harness 構造を確認**

```bash
ls crates/fulgur-vrt/tests/
ls crates/fulgur-vrt/goldens/fulgur/ 2>/dev/null
head -100 crates/fulgur-vrt/tests/gradient_harness.rs
```

既存ケースの命名規則・HTML 構成・golden 配置を確認する。

**Step 2: 新規ケース 6 件を追加**

`gradient_harness.rs` に以下を追加(関数名は既存パターンに合わせる):

1. `linear_gradient_size_50_no_repeat_center` — `linear-gradient(red, blue)` / size `50% 50%` / no-repeat / position `center`
2. `linear_gradient_repeat_x_with_position` — size `80px 60px` / `repeat-x` / position `10px 20px`
3. `linear_gradient_repeat_round` — size `60px 40px` / `repeat round` / tile が伸縮することの検証(角度が tile aspect で再計算される)
4. `radial_gradient_explicit_size_no_repeat_corner` — `radial-gradient(circle, ...)` / size `100px 100px` / no-repeat / position `bottom right`
5. `radial_gradient_size_50_repeat` — size `50% 50%` / `repeat` (中心は各 tile 内)
6. `linear_gradient_corner_explicit_aspect` — `linear-gradient(to top right, ...)` / size `100px 50px` / no-repeat / corner 角度が tile aspect (100×50) で計算されることを検証

各ケースの HTML / CSS は既存 harness の `<div style="...">` パターンに合わせる。背景は分かりやすい `red→blue` 等の 2 色 stop。

**Step 3: golden を生成**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt 2>&1 | tail -20
```

Expected: 新規 6 件の golden PDF が `goldens/fulgur/...` に生成される。

**Step 4: 生成された PDF を目視確認**

```bash
ls -la crates/fulgur-vrt/goldens/fulgur/ | grep -i gradient
```

Optional: `pdftocairo` で PNG 化してざっと見る:

```bash
pdftocairo -png crates/fulgur-vrt/goldens/fulgur/.../*.pdf /tmp/check_
```

期待動作:
- ケース 1: 中央に小さな gradient、周囲は背景色
- ケース 2: 横に gradient タイルが並ぶ
- ケース 3: tile 数 × tile サイズが clip と一致
- ケース 4: 右下隅に円形 gradient
- ケース 5: 複数タイル、各 tile に円形 gradient
- ケース 6: 100×50 のアスペクトに合った対角線

**Step 5: 通常実行で再 PASS することを確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10
```

Expected: 全件 pass、byte-wise 一致。

**Step 6: clippy / fmt**

```bash
cargo clippy -p fulgur-vrt --all-targets 2>&1 | tail -10
cargo fmt --check
```

**Step 7: コミット**

```bash
git add crates/fulgur-vrt/tests/gradient_harness.rs crates/fulgur-vrt/goldens/
git commit -m "test(vrt): add gradient × bg-size/position/repeat integration cases"
```

---

## Task 4: WPT expectations annotation

**Files:**
- Modify: WPT runner の bugs.txt (場所は `find . -name "bugs.txt" 2>/dev/null` で確認)

**Step 1: WPT runner 構成と既存 gradient 関連 expectations を確認**

```bash
find . -name "bugs.txt" -not -path "./.worktrees/*" 2>/dev/null
find . -name "*.toml" -path "*fulgur-wpt*" 2>/dev/null
ls crates/fulgur-wpt-runner/ 2>/dev/null
```

`bugs.txt` の現在の gradient 関連 entry を確認:

```bash
grep -i "gradient" $(find . -name "bugs.txt" -not -path "./.worktrees/*" | head -1)
```

**Step 2: 関連 reftest を WPT から特定**

```bash
find . -path "*wpt/css/css-backgrounds*" -name "*background-size*" -name "*.html" | grep -i gradient | head -20
find . -path "*wpt/css/css-backgrounds*" -name "*background-position*" -name "*.html" | head -20
find . -path "*wpt/css/css-backgrounds*" -name "*background-repeat*" -name "*.html" | head -20
```

**Step 3: WPT runner で gradient 関連 reftest を実行**

```bash
# fulgur-wpt-runner の standard invocation を確認
cat crates/fulgur-wpt-runner/README.md 2>/dev/null | head -30
# 例: 個別実行
cargo run -p fulgur-wpt-runner -- --filter "background-size.*gradient" 2>&1 | tail -30
```

Expected: 該当する gradient × bg-property reftest を実行し、PASS / FAIL の現状を把握する。実在 reftest 数が少なくて 5 件に届かなくても問題ない(acceptance は "annotate that reflect reality" に緩めてある)。

**Step 4: bugs.txt を更新**

PASS したテストの `FAIL` / `SKIP` 行を削除、または `# fixed by fulgur-4ono` などのコメントとともに status 変更。書式は既存 entry に合わせる。

**Step 5: 全件再実行で diff を確認**

```bash
cargo run -p fulgur-wpt-runner -- 2>&1 | tail -30
# または既存の bugs.txt 検証コマンド (CI で使われているもの)
```

Expected: `bugs.txt` 通りに pass / fail し、不整合なし。

**Step 6: コミット**

```bash
git add path/to/bugs.txt
git commit -m "test(wpt): annotate gradient × bg-property reftests as PASS"
```

---

## Task 5: 最終検証 + design レビュー

**Files:** なし(検証のみ)

**Step 1: 全テスト実行**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur 2>&1 | tail -5
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -5
```

Expected: 全 pass、regression 0。

**Step 2: lint / fmt**

```bash
cargo clippy --all-targets 2>&1 | tail -10
cargo fmt --check
```

Expected: 警告 0、差分なし。

**Step 3: 受け入れ条件チェック (beads issue acceptance フィールドと突合)**

- [x] `cargo test -p fulgur --lib` 新規単体テスト緑
- [x] `cargo test -p fulgur-vrt` 新規 VRT 緑 (PDF byte-wise 一致)
- [x] 既存 gradient / background テスト regression 0
- [x] WPT で gradient × bg-property 関連 reftest が `bugs.txt` に正しく annotate(実在テスト数に応じて、PASS/FAIL は実態反映)
- [x] `cargo clippy` / `cargo fmt --check` clean

**Step 4: branch 状態確認**

```bash
git log --oneline main..HEAD
git status
```

Expected: 4 commits (Task 1, 2, 3, 4)、working tree clean。

---

## 終了条件

すべての Task が完了し、Step 5 の受け入れ条件 5 件すべてに green を入れた状態。

その後 `superpowers:finishing-a-development-branch` で branch 完了処理(PR 作成)へ進む。
