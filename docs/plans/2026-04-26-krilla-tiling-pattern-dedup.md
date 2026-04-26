# Krilla Tiling Pattern Dedup for Gradient Layers Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** uniform-grid な gradient タイル列を krilla の Tiling Pattern (PatternType 1) として 1 個の Pattern resource にまとめ、PDF サイズと PDF オブジェクト数を削減する (例: `gradient-repeat-round.pdf` で 48 個の Function/Shading triplet を 1 個に圧縮)。

**Architecture:** `crates/fulgur/src/background.rs` の `BgImageContent::LinearGradient` / `RadialGradient` ブランチに「全タイル `(tw, th)` 一致 + ステップ一様」の検出を追加し、該当時のみ `surface.stream_builder().surface()` で gradient を `(0,0,tile_w,tile_h)` に 1 回描画したパターン stream を作成、`krilla::paint::Pattern { stream, transform: Translate(first_tile), width: step_x, height: step_y }` で fill する。条件外は既存 per-tile ループへフォールバック。

**Tech Stack:** Rust, krilla 0.7 (`paint::Pattern`, `surface::Surface::stream_builder`), fulgur internal types (`Canvas`, `BackgroundLayer`).

---

### Task 1: `draw_linear_gradient` / `draw_radial_gradient` の `&mut Surface` 化

**Files:**

- Modify: `crates/fulgur/src/background.rs:343-419` (`draw_linear_gradient`)
- Modify: `crates/fulgur/src/background.rs:426-560` 付近 (`draw_radial_gradient`)
- Modify: `crates/fulgur/src/background.rs:238-273` (`draw_background_layer` の per-tile 呼び出し箇所)

**Why:** Tiling Pattern の sub-stream surface は `krilla::surface::Surface<'_>` を返すが、`Canvas` には包めない (bookmark/link collector が必要)。両関数は `canvas.surface.*` しか使っていないので、シグネチャを `&mut krilla::surface::Surface<'_>` に変更すれば pattern stream surface もそのまま渡せる。リファクタのみで挙動変更なし。

**Step 1: シグネチャ変更**

`draw_linear_gradient` と `draw_radial_gradient` の第 1 引数を `canvas: &mut Canvas<'_, '_>` から `surface: &mut krilla::surface::Surface<'_>` に変更し、関数本体の `canvas.surface.` を `surface.` に置換する。

**Step 2: 呼び出し箇所更新**

`draw_background_layer` 内の 3 箇所 (Linear/Angle, Linear/Corner, Radial) で `draw_linear_gradient(canvas, ...)` を `draw_linear_gradient(canvas.surface, ...)` に置換。

**Step 3: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | tail -5
```

Expected: 警告 / エラーなし。

**Step 4: 既存テスト全 PASS 確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 729 passed (baseline と同じ).

**Step 5: VRT byte 一致確認 (リファクタは byte 変化を起こさない)**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -5
```

Expected: 全 PASS (byte 変化なし)。

**Step 6: コミット**

```bash
git add crates/fulgur/src/background.rs
git commit -m "refactor(gradient): take &mut Surface in draw_*_gradient helpers"
```

---

### Task 2: `try_uniform_grid` ヘルパー追加 + 単体テスト

**Files:**

- Modify: `crates/fulgur/src/background.rs` (private fn を追加、近隣 `compute_tile_positions` の直下を想定)
- Test: `crates/fulgur/src/background.rs` の `#[cfg(test)] mod tests` 内

**Why:** 一様グリッド検出ロジックを単独 fn に切り出し、ユニットテストで挙動を固定する。検出失敗 (irregular grid) 時は `None` を返し fallback を選ばせる。

**Step 1: 失敗するテストを書く**

`crates/fulgur/src/background.rs` の `#[cfg(test)] mod tests` 末尾に以下を追加:

```rust
// ─── try_uniform_grid ─────────────────────────────────────────────────────

#[test]
fn uniform_grid_single_tile_returns_none() {
    // Single tile (no-repeat) is not worth the Pattern overhead.
    let tiles = vec![(10.0, 20.0, 30.0, 40.0)];
    assert!(try_uniform_grid(&tiles).is_none());
}

#[test]
fn uniform_grid_repeat_round_8x6_detected() {
    // 8 columns × 6 rows, cell = step = (10, 15)
    let mut tiles = Vec::new();
    for j in 0..6 {
        for i in 0..8 {
            tiles.push((i as f32 * 10.0, j as f32 * 15.0, 10.0, 15.0));
        }
    }
    let g = try_uniform_grid(&tiles).expect("detect 8×6 grid");
    assert_eq!(g.count, (8, 6));
    assert!((g.cell.0 - 10.0).abs() < 1e-3);
    assert!((g.cell.1 - 15.0).abs() < 1e-3);
    assert!((g.step.0 - 10.0).abs() < 1e-3);
    assert!((g.step.1 - 15.0).abs() < 1e-3);
    assert!((g.origin.0 - 0.0).abs() < 1e-3);
    assert!((g.origin.1 - 0.0).abs() < 1e-3);
}

#[test]
fn uniform_grid_space_with_gaps_detected() {
    // 3 columns × 2 rows, cell = (10, 10), step = (15, 20)
    let tiles = vec![
        (0.0, 0.0, 10.0, 10.0),
        (15.0, 0.0, 10.0, 10.0),
        (30.0, 0.0, 10.0, 10.0),
        (0.0, 20.0, 10.0, 10.0),
        (15.0, 20.0, 10.0, 10.0),
        (30.0, 20.0, 10.0, 10.0),
    ];
    let g = try_uniform_grid(&tiles).expect("detect 3×2 spaced grid");
    assert_eq!(g.count, (3, 2));
    assert!((g.cell.0 - 10.0).abs() < 1e-3);
    assert!((g.step.0 - 15.0).abs() < 1e-3);
    assert!((g.step.1 - 20.0).abs() < 1e-3);
}

#[test]
fn uniform_grid_mismatched_cell_size_returns_none() {
    // Second tile has different height → not uniform.
    let tiles = vec![
        (0.0, 0.0, 10.0, 10.0),
        (10.0, 0.0, 10.0, 12.0),
    ];
    assert!(try_uniform_grid(&tiles).is_none());
}

#[test]
fn uniform_grid_irregular_step_returns_none() {
    // X steps: 10, 11 → not uniform.
    let tiles = vec![
        (0.0, 0.0, 5.0, 5.0),
        (10.0, 0.0, 5.0, 5.0),
        (21.0, 0.0, 5.0, 5.0),
    ];
    assert!(try_uniform_grid(&tiles).is_none());
}

#[test]
fn uniform_grid_single_row_repeat_x() {
    // 4 tiles in a single row → 4×1 grid.
    let tiles = vec![
        (0.0, 5.0, 10.0, 10.0),
        (10.0, 5.0, 10.0, 10.0),
        (20.0, 5.0, 10.0, 10.0),
        (30.0, 5.0, 10.0, 10.0),
    ];
    let g = try_uniform_grid(&tiles).expect("detect 4×1 grid");
    assert_eq!(g.count, (4, 1));
}

#[test]
fn uniform_grid_count_mismatch_returns_none() {
    // 3 tiles claimed but only 2 unique x positions × 2 unique y → 4 expected.
    let tiles = vec![
        (0.0, 0.0, 5.0, 5.0),
        (10.0, 0.0, 5.0, 5.0),
        (0.0, 10.0, 5.0, 5.0),
    ];
    assert!(try_uniform_grid(&tiles).is_none());
}
```

**Step 2: テストが落ちることを確認**

```bash
cargo test -p fulgur --lib uniform_grid 2>&1 | tail -10
```

Expected: コンパイルエラー (`try_uniform_grid` / `UniformGrid` 未定義)。

**Step 3: 最小実装**

`compute_tile_positions_slow` の直下に追加:

```rust
/// 一様タイルグリッドのジオメトリ。
#[derive(Debug, Clone, Copy, PartialEq)]
struct UniformGrid {
    /// 最初のタイルの (x, y) — グリッド原点。
    origin: (f32, f32),
    /// セル (タイル) のサイズ。
    cell: (f32, f32),
    /// グリッドのステップ (cell + 任意の gap)。`cell == step` で repeat / round、
    /// `step > cell` で space (タイル間 gap あり)。
    step: (f32, f32),
    /// X 方向のタイル数, Y 方向のタイル数。`count.0 * count.1 == tiles.len()`。
    count: (usize, usize),
}

/// 全タイルが (cell サイズ一致 + ステップ一様 + count.x×count.y == tiles.len()) を
/// 満たすなら `UniformGrid` を返す。Pattern dedup 経路の適用判定に使う。
/// `tiles.len() < 2` のときは Pattern 構築コストが無駄なので `None`。
fn try_uniform_grid(tiles: &[(f32, f32, f32, f32)]) -> Option<UniformGrid> {
    if tiles.len() < 2 {
        return None;
    }
    let eps = 1e-3_f32;

    // セルサイズ一致チェック
    let (tw0, th0) = (tiles[0].2, tiles[0].3);
    if !tiles.iter().all(|t| (t.2 - tw0).abs() < eps && (t.3 - th0).abs() < eps) {
        return None;
    }

    // ユニークな x / y 座標を昇順で収集 (epsilon マージ)
    let mut xs: Vec<f32> = Vec::new();
    for t in tiles {
        if !xs.iter().any(|&x| (x - t.0).abs() < eps) {
            xs.push(t.0);
        }
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut ys: Vec<f32> = Vec::new();
    for t in tiles {
        if !ys.iter().any(|&y| (y - t.1).abs() < eps) {
            ys.push(t.1);
        }
    }
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    // count.x × count.y がタイル数と一致 (=完全グリッド) であること
    if xs.len() * ys.len() != tiles.len() {
        return None;
    }

    // X / Y 方向のステップ一様性をチェック
    let step_x = if xs.len() >= 2 {
        let s = xs[1] - xs[0];
        for w in xs.windows(2) {
            if (w[1] - w[0] - s).abs() > eps {
                return None;
            }
        }
        s
    } else {
        // 単行 (count.x == 1): ステップは未定義だが、cell サイズで埋める
        tw0
    };
    let step_y = if ys.len() >= 2 {
        let s = ys[1] - ys[0];
        for w in ys.windows(2) {
            if (w[1] - w[0] - s).abs() > eps {
                return None;
            }
        }
        s
    } else {
        th0
    };

    Some(UniformGrid {
        origin: (xs[0], ys[0]),
        cell: (tw0, th0),
        step: (step_x, step_y),
        count: (xs.len(), ys.len()),
    })
}
```

**Step 4: テスト PASS 確認**

```bash
cargo test -p fulgur --lib uniform_grid 2>&1 | tail -10
```

Expected: 7 passed.

**Step 5: 全テスト PASS 確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 736 passed (729 + 7 新規).

**Step 6: コミット**

```bash
git add crates/fulgur/src/background.rs
git commit -m "feat(gradient): add try_uniform_grid helper for tile-grid detection"
```

---

### Task 3: Tiling Pattern 経路の実装 + uniform-grid 時のディスパッチ

**Files:**

- Modify: `crates/fulgur/src/background.rs:238-273` (`draw_background_layer` の gradient 分岐)

**Why:** Task 1 で `draw_linear_gradient` / `draw_radial_gradient` が `&mut Surface` ベースになったので、stream_builder の sub-surface にそのまま渡せる。Task 2 の `try_uniform_grid` で uniform-grid を検出した場合は、Pattern stream を 1 度だけ作成して `set_fill(Pattern)` + `draw_path(union_rect)` で塗る。

**Step 1: 実装**

`crates/fulgur/src/background.rs:238` の `match &layer.content { ... }` ブロックを以下に書き換え。`Raster` / `Svg` 分岐は既存のまま残す。

```rust
match &layer.content {
    BgImageContent::LinearGradient { direction, stops } => {
        if let Some(grid) = try_uniform_grid(&tiles) {
            let angle = match direction {
                crate::pageable::LinearGradientDirection::Angle(a) => *a,
                crate::pageable::LinearGradientDirection::Corner(corner) => {
                    // uniform grid なので tile aspect は全タイル共通 → 1 つの angle で OK。
                    corner_to_angle_rad(*corner, grid.cell.0, grid.cell.1)
                }
            };
            draw_gradient_tiling_pattern(canvas, grid, |surface, _tw, _th| {
                draw_linear_gradient(surface, angle, stops, 0.0, 0.0, grid.cell.0, grid.cell.1);
            });
        } else {
            // Fallback: per-tile loop (irregular tile geometry).
            match direction {
                crate::pageable::LinearGradientDirection::Angle(a) => {
                    let angle = *a;
                    for (tx, ty, tw, th) in &tiles {
                        draw_linear_gradient(canvas.surface, angle, stops, *tx, *ty, *tw, *th);
                    }
                }
                crate::pageable::LinearGradientDirection::Corner(corner) => {
                    for (tx, ty, tw, th) in &tiles {
                        let angle = corner_to_angle_rad(*corner, *tw, *th);
                        draw_linear_gradient(canvas.surface, angle, stops, *tx, *ty, *tw, *th);
                    }
                }
            }
        }
    }
    BgImageContent::RadialGradient {
        shape,
        size,
        position_x,
        position_y,
        stops,
    } => {
        if let Some(grid) = try_uniform_grid(&tiles) {
            draw_gradient_tiling_pattern(canvas, grid, |surface, tw, th| {
                draw_radial_gradient(
                    surface, *shape, size, position_x, position_y, stops, 0.0, 0.0, tw, th,
                );
            });
        } else {
            for (tx, ty, tw, th) in &tiles {
                draw_radial_gradient(
                    canvas.surface, *shape, size, position_x, position_y, stops, *tx, *ty, *tw, *th,
                );
            }
        }
    }
    BgImageContent::Raster { data, format } => {
        // (既存実装をそのまま残す)
        ...
    }
    BgImageContent::Svg { tree } => {
        // (既存実装をそのまま残す)
        ...
    }
}
```

`draw_gradient_tiling_pattern` を `draw_radial_gradient` の下に追加:

```rust
/// uniform-grid 検出時の Tiling Pattern 描画ヘルパー。
///
/// 1. `surface.stream_builder().surface()` で sub-surface を取得し、
///    `paint_in_cell` クロージャで gradient を `(0, 0, tile_w, tile_h)` に描画。
/// 2. `Pattern { stream, transform: Translate(origin), width: step_x, height: step_y }`
///    を構築 (PDF /Matrix · /XStep · /YStep に対応)。
/// 3. `set_fill(pattern)` + `draw_path(union_rect)` で塗りつぶし。
///    既存の `clip_path` がレイヤーの可視領域を bound する。
fn draw_gradient_tiling_pattern(
    canvas: &mut Canvas<'_, '_>,
    grid: UniformGrid,
    paint_in_cell: impl FnOnce(&mut krilla::surface::Surface<'_>, f32, f32),
) {
    // パターン stream の構築 (`stream_builder` は親 surface の sc を借用)
    let mut sb = canvas.surface.stream_builder();
    {
        let mut ps = sb.surface();
        paint_in_cell(&mut ps, grid.cell.0, grid.cell.1);
        ps.finish();
    }
    let stream = sb.finish();

    let pattern = krilla::paint::Pattern {
        stream,
        transform: krilla::geom::Transform::from_translate(grid.origin.0, grid.origin.1),
        width: grid.step.0,
        height: grid.step.1,
    };

    canvas.surface.set_fill(Some(krilla::paint::Fill {
        paint: pattern.into(),
        rule: Default::default(),
        opacity: krilla::num::NormalizedF32::ONE,
    }));
    canvas.surface.set_stroke(None);

    let total_w = grid.step.0 * grid.count.0 as f32;
    let total_h = grid.step.1 * grid.count.1 as f32;
    let Some(rect_path) =
        build_rect_path(grid.origin.0, grid.origin.1, total_w, total_h)
    else {
        canvas.surface.set_fill(None);
        return;
    };
    canvas.surface.draw_path(&rect_path);
    canvas.surface.set_fill(None);
}
```

**Step 2: ビルド + 既存テスト全 PASS**

```bash
cargo build -p fulgur 2>&1 | tail -5
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 736 passed (Task 2 までと同数、新規ロジックは VRT で検証).

**Step 3: VRT 出力差分の確認 (golden 更新前)**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -20
```

Expected: gradient 系 fixture (linear-gradient-*, radial-gradient-*, gradient-repeat-*) で byte 不一致 (= 期待される挙動変化)。失敗一覧を確認し「想定外の fixture が変わっていない」ことを目視チェック。

**Step 4: golden 再生成**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt 2>&1 | tail -5
```

Expected: 全 fixture が更新されて PASS。

**Step 5: 視覚的 regression チェック**

```bash
git diff --stat crates/fulgur-vrt/goldens/ | head -30
```

更新されたファイル名を確認。代表 fixture (`gradient-repeat-round.pdf`, `gradient-radial-*.pdf`, `gradient-linear-*.pdf`) について、git stash で更新前の golden を取り出し pdftocairo で diff 画像を生成する手順を提示:

```bash
# 1. 新 PDF と旧 PDF をそれぞれ PNG 化して比較
mkdir -p /tmp/vrt-diff
for f in $(git diff --name-only crates/fulgur-vrt/goldens/ | grep -E "gradient.*\.pdf$" | head -5); do
  base=$(basename "$f" .pdf)
  pdftocairo -png -r 144 "$f" "/tmp/vrt-diff/${base}-new"
  git show HEAD:"$f" > /tmp/vrt-diff/old.pdf
  pdftocairo -png -r 144 /tmp/vrt-diff/old.pdf "/tmp/vrt-diff/${base}-old"
  # ImageMagick compare: 視覚差分を /tmp/vrt-diff/${base}-diff.png に出力
  compare -metric AE "/tmp/vrt-diff/${base}-old-1.png" \
    "/tmp/vrt-diff/${base}-new-1.png" "/tmp/vrt-diff/${base}-diff.png" 2>&1 || true
done
```

(※ `compare` が無ければ `pdftoppm` + `python` の差分計算でも可。視覚同一であることを目視確認.)

**Step 6: 効果測定**

`gradient-repeat-round.pdf` の (a) ファイルサイズ、(b) Function/Shading オブジェクト数を before/after で比較:

```bash
echo "=== After ==="
wc -c crates/fulgur-vrt/goldens/fulgur/paint/gradient-repeat-round.pdf
grep -c "/FunctionType 2" crates/fulgur-vrt/goldens/fulgur/paint/gradient-repeat-round.pdf || true

echo "=== Before ==="
git show HEAD:crates/fulgur-vrt/goldens/fulgur/paint/gradient-repeat-round.pdf | wc -c
git show HEAD:crates/fulgur-vrt/goldens/fulgur/paint/gradient-repeat-round.pdf | grep -c "/FunctionType 2" || true
```

Expected: After のサイズ < Before、Function 数 1 (After) vs 48 (Before)。サイズが期待通り縮小していなければ Pattern dedup が効いていない兆候 → 中断して原因調査。

**Step 7: コミット**

```bash
git add crates/fulgur/src/background.rs crates/fulgur-vrt/goldens/
git commit -m "perf(gradient): emit one Tiling Pattern per uniform-grid layer

uniform tile grids (repeat / repeat-round / 1-axis repeat / no-repeat
single-tile excluded) now produce a single PDF Tiling Pattern resource
instead of N separate Function 2 + Shading 2 + Pattern triplets.

gradient-repeat-round.pdf: 48 Function/Shading triplets -> 1
File size: ~24KB -> ~XKB (measured)

Irregular tile geometries (e.g. uneven space repeat) fall back to the
existing per-tile loop unchanged.

Closes fulgur-1bmx"
```

---

### Task 4: clippy + fmt + 全 VRT パス + ドキュメント

**Files:**

- Verify: 全 fulgur ソース、VRT goldens

**Step 1: clippy 警告 0**

```bash
cargo clippy -p fulgur --all-targets 2>&1 | tail -10
```

Expected: warning 0、error 0。

**Step 2: cargo fmt**

```bash
cargo fmt --check 2>&1 | tail -5
```

Expected: 出力なし。差分があれば `cargo fmt` で整形してから再度 commit。

**Step 3: 全 ws テスト**

```bash
cargo test 2>&1 | tail -10
```

Expected: 全 crate PASS。

**Step 4: markdownlint**

```bash
npx markdownlint-cli2 'docs/plans/2026-04-26-krilla-tiling-pattern-dedup.md' 2>&1 | tail -10
```

Expected: violations 0。

**Step 5: PR 用効果測定の最終整理**

`gradient-repeat-round.pdf` だけでなく、`gradient-radial-repeat-round.pdf` 等の代表的 multi-tile gradient fixture について before/after サイズ表を作成し、PR description に貼る:

```bash
for f in $(git diff --name-only HEAD~3 -- crates/fulgur-vrt/goldens/ | grep -E "gradient.*\.pdf$"); do
  new=$(wc -c < "$f")
  old=$(git show HEAD~3:"$f" 2>/dev/null | wc -c)
  printf "%-60s old=%6d  new=%6d  delta=%+d\n" "$f" "$old" "$new" "$((new - old))"
done
```

(差分が `feat:` コミットしか含まないようにコミット粒度を保つ。`HEAD~3` は実コミット数で調整。)

**Step 6: 最終コミット (必要なら)**

`cargo fmt` 由来の整形差分や markdownlint 修正があれば squash せず別コミットで:

```bash
git add -u
git commit -m "style: cargo fmt"
```

---

## 受け入れ基準 (issue fulgur-1bmx より)

- `gradient-repeat-round.pdf`: Function/Shading オブジェクト数 48 → 1 へ削減
- ファイルサイズ ~24KB → ~1-2KB へ縮小
- VRT golden 全更新 (visual identical, byte 変化のみ) を pdftocairo diff で目視 PASS
- `cargo test -p fulgur`, `-p fulgur-vrt` 全 PASS
- `cargo clippy` warning 0
- uniform でないタイル列 (space 等) では現状の per-tile ループにフォールバック

## 実装後の確認 (executing-plans / subagent-driven-development の最後で)

REQUIRED SUB-SKILL: `superpowers:verification-before-completion` で Step 1-3 (clippy / fmt / 全 test) を再実行し、PASS を確認してから完了宣言する。
