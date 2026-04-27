# Gradient Interpolation Hint Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS Images 3 §3.5.3 の interpolation hint (`linear-gradient(red, 30%, blue)` の `30%`) を実装し、Phase 1 の Layer drop 動作を spec 通りの累乗カーブ補間に置き換える。

**Architecture:** `pageable::GradientStop` に `is_hint: bool` フィールドを追加し、convert 段で hint を carrier として保持、draw 段の `resolve_gradient_stops` パイプラインで pure helper `expand_interpolation_hints` により 8 個の中間 stop に展開する。`resolve_color_stops` は linear / radial 共有なので両方で動作する。conic は別 issue。

**Tech Stack:** Rust, Krilla (axial / radial shading), Stylo (CSS computed values), fulgur-vrt (PDF byte-diff harness)

**Beads Issue:** fulgur-2zam

**Reference:**

- CSS Images 3 §3.5.3 — <https://drafts.csswg.org/css-images-3/#color-stop-syntax>
- Phase 1 (fulgur-yax4) — `crates/fulgur/src/convert/style/background.rs::resolve_color_stops`

---

## Task 1: Add `is_hint: bool` to `GradientStop`

データモデル拡張。挙動変更なし、既存テスト全 pass を確認するメカニカルな refactor。

**Files:**

- Modify: `crates/fulgur/src/pageable.rs:755-761` (struct 定義 + コメント)
- Modify: `crates/fulgur/src/background.rs` (15 箇所の `GradientStop {` literal、`stop()` test helper)
- Modify: `crates/fulgur/src/convert/style/background.rs` (convert 内の literal: `SimpleColorStop` / `ComplexColorStop` arm、conic ループ内 2 箇所)

**Step 1: `pageable.rs` の `GradientStop` 構造体を拡張**

```rust
/// A single color stop in a CSS gradient. Position は `GradientStopPosition`
/// で保持され、draw 時に gradient line 長さで fraction に解決される。
///
/// `is_hint=true` のときは CSS interpolation hint marker。`rgba` は無効値で
/// あり読まない契約。draw 段の `resolve_gradient_stops` が `expand_interpolation_hints`
/// を介して隣接 stop の色から N 個の中間 stop に展開する (CSS Images 3 §3.5.3)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub position: GradientStopPosition,
    pub rgba: [u8; 4],
    pub is_hint: bool,
}
```

**Step 2: 既存 `GradientStop {` literal にすべて `is_hint: false` を追加**

`background.rs` の test helper を含む 15 箇所と、`convert/style/background.rs` の `SimpleColorStop` / `ComplexColorStop` arm 2 箇所、`resolve_conic_gradient` 内 2 箇所、計 19 箇所。

例 (`background.rs:3013` の `stop` helper):

```rust
fn stop(p: GradientStopPosition, rgba: [u8; 4]) -> GradientStop {
    GradientStop { position: p, rgba, is_hint: false }
}
```

**Step 3: lib テストで挙動不変を確認**

Run: `cargo test -p fulgur --lib`
Expected: 815 passed (baseline と同数), 0 failed

**Step 4: clippy / fmt**

Run: `cargo fmt && cargo clippy -p fulgur --lib --tests -- -D warnings`
Expected: No errors

**Step 5: Commit**

```bash
git add -A
git commit -m "refactor(gradient): add is_hint flag to GradientStop"
```

---

## Task 2: `expand_interpolation_hints` pure helper (TDD)

CSS Images 3 §3.5.3 の累乗カーブ展開を pure function として実装。codecov patch coverage に乗せるため lib 側に unit test。

**Files:**

- Modify: `crates/fulgur/src/background.rs` (新規 helper 関数 + `#[cfg(test)] mod expand_interpolation_hints_tests`)

**Step 1: failing tests を先に書く**

`crates/fulgur/src/background.rs` の `mod resolve_gradient_stops_tests` の隣に新規 mod を追加:

```rust
#[cfg(test)]
mod expand_interpolation_hints_tests {
    use super::*;

    /// 内部表現: position 解決済みの stop 列。`is_hint=true` は CSS hint marker。
    fn s(pos: f32, r: u8, g: u8, b: u8) -> ResolvedStop {
        ResolvedStop { pos, rgba: [r, g, b, 255], is_hint: false }
    }
    fn h(pos: f32) -> ResolvedStop {
        ResolvedStop { pos, rgba: [0, 0, 0, 0], is_hint: true }
    }

    #[test]
    fn no_hints_passthrough() {
        let input = vec![s(0.0, 255, 0, 0), s(1.0, 0, 0, 255)];
        let out = expand_interpolation_hints(input);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], (0.0, [255, 0, 0, 255]));
        assert_eq!(out[1], (1.0, [0, 0, 255, 255]));
    }

    #[test]
    fn midpoint_hint_yields_linear_interpolation() {
        // H=0.5 → exponent=1.0 → 8 個の中間 stop は線形補間と一致 (許容誤差 1)
        let input = vec![s(0.0, 255, 0, 0), h(0.5), s(1.0, 0, 0, 255)];
        let out = expand_interpolation_hints(input);
        assert_eq!(out.len(), 2 + 8, "2 endpoints + 8 samples");
        // 中央サンプル (i=4, t=4/9) の R は線形補間で 255 * (1 - 4/9) = 141.67 → 142
        let mid = out.iter().find(|(p, _)| (p - 4.0 / 9.0).abs() < 1e-3).unwrap();
        assert!((mid.1[0] as i16 - 142).abs() <= 1, "R channel near 142, got {}", mid.1[0]);
        assert!((mid.1[2] as i16 - 113).abs() <= 1, "B channel near 113, got {}", mid.1[2]);
    }

    #[test]
    fn early_hint_biases_toward_end_color() {
        // H=0.2 → exponent = log(0.5)/log(0.2) ≈ 0.43 → 早期に end 色に寄る
        let input = vec![s(0.0, 255, 0, 0), h(0.2), s(1.0, 0, 0, 255)];
        let out = expand_interpolation_hints(input);
        // 中央サンプル (t=0.5 相当 = i=4 / 9) の B は H=0.5 (線形 113) より高い
        let mid = out.iter().find(|(p, _)| (p - 4.0 / 9.0).abs() < 1e-3).unwrap();
        assert!(mid.1[2] > 113, "B should be biased above 113, got {}", mid.1[2]);
    }

    #[test]
    fn late_hint_biases_toward_start_color() {
        // H=0.8 → exponent ≈ 3.1 → 後期まで start 色に近い
        let input = vec![s(0.0, 255, 0, 0), h(0.8), s(1.0, 0, 0, 255)];
        let out = expand_interpolation_hints(input);
        let mid = out.iter().find(|(p, _)| (p - 4.0 / 9.0).abs() < 1e-3).unwrap();
        assert!(mid.1[0] > 142, "R should remain above 142, got {}", mid.1[0]);
    }

    #[test]
    fn degenerate_zero_span_drops_hint() {
        // p_a == p_b → hint は無意味、output から hint を drop
        let input = vec![s(0.5, 255, 0, 0), h(0.5), s(0.5, 0, 0, 255)];
        let out = expand_interpolation_hints(input);
        // hint が drop され、両端 color stop だけが残る
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|(_, c)| c[3] == 255), "no transparent hint leak");
    }

    #[test]
    fn hint_at_extreme_position_clamps() {
        // H=0 (start に張り付き) は EPS で吸収、panic しない
        let input = vec![s(0.0, 255, 0, 0), h(0.0), s(1.0, 0, 0, 255)];
        let out = expand_interpolation_hints(input);
        assert_eq!(out.len(), 2 + 8);
        // すべて step 関数化、ほぼ end 色 (B≈255)
        let last_sample = out.iter().filter(|(p, _)| *p > 0.0 && *p < 1.0).last().unwrap();
        assert!(last_sample.1[2] > 200, "B should be near 255 (step), got {}", last_sample.1[2]);
    }

    #[test]
    fn hint_within_segment_geometry() {
        // span 0.4..0.8 内の hint。位置は span 等間隔で 8 サンプル。
        let input = vec![s(0.4, 255, 0, 0), h(0.6), s(0.8, 0, 0, 255)];
        let out = expand_interpolation_hints(input);
        assert_eq!(out.len(), 2 + 8);
        // 中間 sample 位置は p_a + (p_b - p_a) * i/9 で 0.4..0.8 内
        for (p, _) in out.iter().filter(|(p, _)| *p > 0.4 && *p < 0.8) {
            assert!((0.4..=0.8).contains(p));
        }
    }

    #[test]
    fn multiple_hints_in_separate_segments() {
        // [red, h, blue, h, green]: 各 segment 独立に展開
        let input = vec![
            s(0.0, 255, 0, 0),
            h(0.25),
            s(0.5, 0, 0, 255),
            h(0.75),
            s(1.0, 0, 255, 0),
        ];
        let out = expand_interpolation_hints(input);
        assert_eq!(out.len(), 3 + 8 + 8, "3 endpoints + 8 + 8 samples");
    }
}
```

**Step 2: テストを走らせて全部 fail することを確認**

Run: `cargo test -p fulgur --lib expand_interpolation_hints`
Expected: コンパイルエラー (helper / `ResolvedStop` 未定義)

**Step 3: `ResolvedStop` 構造体と `expand_interpolation_hints` 実装**

`background.rs` の `resolve_gradient_stops` の上方 (or 適切な位置) に追加:

```rust
/// `resolve_gradient_stops` 内の中間表現。position / Auto fixup 完了後に
/// `is_hint` を保持して `expand_interpolation_hints` に渡される。
#[derive(Clone, Copy, Debug, PartialEq)]
struct ResolvedStop {
    pos: f32,
    rgba: [u8; 4],
    is_hint: bool,
}

/// CSS Images 3 §3.5.3 の interpolation hint を 8 個の中間 stop に展開する。
///
/// 入力契約:
/// - 先頭末尾は必ず `is_hint=false` (convert 時に validate 済み)
/// - 各 hint は前後を `is_hint=false` stop に挟まれる (連続 hint は convert で drop)
/// - position は monotonic (resolve_gradient_stops の clamp 通過後)
///
/// アルゴリズム: hint 位置 H_norm に対し exponent = log(0.5) / log(H_norm)、
/// 隣接 stop 間で t 等間隔の N=8 サンプル位置 / t.powf(exponent) で色補間。
/// `H_norm == 0.5` のとき exponent=1.0 で線形 (no-op、8 stop は冗長だが正しい)。
fn expand_interpolation_hints(stops: Vec<ResolvedStop>) -> Vec<(f32, [u8; 4])> {
    const N_SAMPLES: usize = 8;
    const EPS: f32 = 1e-4;

    let mut out: Vec<(f32, [u8; 4])> = Vec::with_capacity(stops.len() + N_SAMPLES);

    let mut i = 0;
    while i < stops.len() {
        let s = stops[i];
        if !s.is_hint {
            out.push((s.pos, s.rgba));
            i += 1;
            continue;
        }
        // hint: 前後の color stop を取得 (型不変条件で必ず存在)
        debug_assert!(i > 0 && i + 1 < stops.len(), "hint must be flanked by color stops");
        let a = stops[i - 1];
        let b = stops[i + 1];
        let span = b.pos - a.pos;

        if span <= EPS {
            // 縮退: a と b が同位置 → hint を drop して continue
            i += 1;
            continue;
        }

        let h_norm = ((s.pos - a.pos) / span).clamp(EPS, 1.0 - EPS);
        let exponent = 0.5_f32.ln() / h_norm.ln();

        for k in 1..=N_SAMPLES {
            let t = (k as f32) / (N_SAMPLES as f32 + 1.0);
            let p = t.powf(exponent);
            let pos = a.pos + span * t;
            let rgba = [
                lerp_u8(a.rgba[0], b.rgba[0], p),
                lerp_u8(a.rgba[1], b.rgba[1], p),
                lerp_u8(a.rgba[2], b.rgba[2], p),
                lerp_u8(a.rgba[3], b.rgba[3], p),
            ];
            out.push((pos, rgba));
        }
        i += 1;
    }
    out
}
```

**Step 4: テスト全 pass を確認**

Run: `cargo test -p fulgur --lib expand_interpolation_hints`
Expected: 8 passed, 0 failed

**Step 5: clippy / fmt**

Run: `cargo fmt && cargo clippy -p fulgur --lib --tests -- -D warnings`
Expected: No errors

**Step 6: Commit**

```bash
git add -A
git commit -m "feat(gradient): add expand_interpolation_hints helper"
```

---

## Task 3: Wire `expand_interpolation_hints` into `resolve_gradient_stops`

draw パイプラインに hint expansion を組み込む。convert 側はまだ hint を drop しているので、この時点では hint stop は通らず実質 no-op。`ResolvedStop` 経由のリファクタが主目的。

**Files:**

- Modify: `crates/fulgur/src/background.rs::resolve_gradient_stops` (内部表現を `ResolvedStop` に統一、`expand_interpolation_hints` 挿入)

**Step 1: `resolve_gradient_stops` を `ResolvedStop` ベースに書き換え**

現状の position 解決後の `Vec<(f32, [u8; 4])>` を `Vec<ResolvedStop>` に変更し、repeating の前に `expand_interpolation_hints` を挟む:

```rust
fn resolve_gradient_stops(
    stops: &[crate::pageable::GradientStop],
    line_length: f32,
    repeating: bool,
) -> Option<Vec<krilla::paint::Stop>> {
    use crate::pageable::GradientStopPosition;

    if stops.len() < 2 { return None; }
    if line_length <= 0.0 { return None; }

    // ... (既存の positions 解決ロジックそのまま)

    // 既存: let resolved: Vec<(f32, [u8; 4])> = ...
    // 変更: ResolvedStop に統合 (is_hint 持ち越し)
    let resolved: Vec<ResolvedStop> = stops
        .iter()
        .zip(positions)
        .map(|(s, p)| ResolvedStop {
            pos: p.expect("all slots resolved"),
            rgba: s.rgba,
            is_hint: s.is_hint,
        })
        .collect();

    // ★ 新規: hint expansion (CSS Images 3 §3.5.3)
    // hint がない場合は実質 passthrough
    let after_hints: Vec<(f32, [u8; 4])> = expand_interpolation_hints(resolved);

    let expanded = if repeating {
        expand_repeating_stops(after_hints)?
    } else {
        after_hints
    };

    let renormalized = renormalize_stops_to_unit_range(expanded);
    // ... (既存の krilla::Stop emit ロジックそのまま)
}
```

**Step 2: 既存テスト全 pass 確認**

Run: `cargo test -p fulgur --lib resolve_gradient_stops`
Expected: All resolve_gradient_stops tests pass (no behavior change because no hints flow through)

**Step 3: lib テスト全 pass 確認**

Run: `cargo test -p fulgur --lib`
Expected: 815 + 8 (Task 2 で追加) = 823 passed

**Step 4: clippy / fmt**

Run: `cargo fmt && cargo clippy -p fulgur --lib --tests -- -D warnings`
Expected: No errors

**Step 5: Commit**

```bash
git add -A
git commit -m "refactor(gradient): wire hint expansion into resolve_gradient_stops"
```

---

## Task 4: Convert: accept hints in `resolve_color_stops` (linear + radial)

convert 段で hint を carrier に変換する。先頭/末尾/連続/calc() 位置は従来通り Layer drop。

**Files:**

- Modify: `crates/fulgur/src/convert/style/background.rs::resolve_color_stops` (`InterpolationHint` arm の置き換え)
- Modify: `crates/fulgur/tests/gradient_test.rs` (既存 `linear_gradient_interpolation_hint_drops_layer` テストを更新 + 新規バリデーションケース追加)

**Step 1: `gradient_test.rs` の既存テストを更新 / 失敗させる**

```rust
// 旧:
// #[test]
// fn linear_gradient_interpolation_hint_drops_layer() {
//     // InterpolationHint arm → log::warn + None. Layer is dropped, but
//     // overall render still succeeds (background just becomes solid white).
//     assert_pdf(&render("linear-gradient(red, 30%, blue)"));
// }

// 新:
#[test]
fn linear_gradient_with_interpolation_hint_renders() {
    // CSS Images 3 §3.5.3: 30% hint で red→blue を累乗カーブ補間。
    // Layer drop ではなく実際に gradient が描画される (fulgur-2zam)。
    assert_pdf(&render("linear-gradient(red, 30%, blue)"));
}

#[test]
fn linear_gradient_leading_hint_drops_layer() {
    // 先頭 hint は CSS syntax 上不正、Layer drop.
    assert_pdf(&render("linear-gradient(30%, red, blue)"));
}

#[test]
fn linear_gradient_trailing_hint_drops_layer() {
    // 末尾 hint は不正、Layer drop.
    assert_pdf(&render("linear-gradient(red, blue, 70%)"));
}

#[test]
fn linear_gradient_consecutive_hints_drop_layer() {
    // 連続 hint は不正、Layer drop.
    assert_pdf(&render("linear-gradient(red, 30%, 60%, blue)"));
}

#[test]
fn radial_gradient_with_interpolation_hint_renders() {
    // 共有 resolve_color_stops 経由で radial も hint 対応 (fulgur-2zam スコープ).
    assert_pdf(&render("radial-gradient(red, 30%, blue)"));
}

#[test]
fn repeating_linear_gradient_with_interpolation_hint_renders() {
    // 周期内 hint: 各周期に hint 展開済み stop 列が平行コピーされる.
    assert_pdf(&render(
        "repeating-linear-gradient(red, 30%, blue 50%, red 100%)",
    ));
}
```

Run: `cargo test -p fulgur --test gradient_test`
Expected: コンパイル成功 (assert_pdf は単に PDF header をチェックするだけなので、既存挙動でも全 pass する。意味のある検証は VRT で行う)

**Step 2: `resolve_color_stops` で hint を受理**

`crates/fulgur/src/convert/style/background.rs:259-265` の `InterpolationHint` arm を以下に置き換え:

```rust
GradientItem::InterpolationHint(lp) => {
    // 先頭/連続 hint は CSS Images 3 syntax 上不正、Layer drop.
    if out.is_empty() || out.last().is_some_and(|s| s.is_hint) {
        log::warn!("{gradient_kind}: leading or consecutive interpolation hint. Layer dropped.");
        return None;
    }
    let pos = if let Some(pct) = lp.to_percentage() {
        GradientStopPosition::Fraction(pct.0)
    } else if let Some(len) = lp.to_length() {
        GradientStopPosition::LengthPx(len.px())
    } else {
        // calc() etc. unsupported (Phase 2 別 issue) — Layer drop.
        log::warn!("{gradient_kind}: hint position is neither percentage nor length (calc() etc.). Layer dropped.");
        return None;
    };
    out.push(GradientStop {
        position: pos,
        rgba: [0; 4],   // is_hint=true のとき意味なし
        is_hint: true,
    });
}
```

ループ末尾の検証も追加 (既存の `if out.len() < 2 { return None; }` の前):

```rust
// 末尾 hint は不正
if out.last().is_some_and(|s| s.is_hint) {
    log::warn!("{gradient_kind}: trailing interpolation hint. Layer dropped.");
    return None;
}
if out.len() < 2 { return None; }
```

**Step 3: 全テスト pass 確認**

Run: `cargo test -p fulgur`
Expected: 全 pass (lib 823 + integration 増分)

**Step 4: clippy / fmt**

Run: `cargo fmt && cargo clippy -p fulgur -- -D warnings`
Expected: No errors

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(gradient): implement CSS interpolation hint for linear/radial"
```

---

## Task 5: VRT fixtures + goldens

PDF byte-diff で視覚的回帰を担保する。

**Files:**

- Create: `crates/fulgur-vrt/fixtures/paint/linear-gradient-hint-30pct.html`
- Create: `crates/fulgur-vrt/fixtures/paint/linear-gradient-hint-70pct.html`
- Create: `crates/fulgur-vrt/fixtures/paint/radial-gradient-hint.html`
- Create: `crates/fulgur-vrt/fixtures/paint/repeating-linear-gradient-hint.html`
- Modify: `crates/fulgur-vrt/manifest.toml` (4 fixtures 追加)
- Create: `crates/fulgur-vrt/goldens/fulgur/paint/linear-gradient-hint-30pct.pdf` (生成)
- Create: `crates/fulgur-vrt/goldens/fulgur/paint/linear-gradient-hint-70pct.pdf` (生成)
- Create: `crates/fulgur-vrt/goldens/fulgur/paint/radial-gradient-hint.pdf` (生成)
- Create: `crates/fulgur-vrt/goldens/fulgur/paint/repeating-linear-gradient-hint.pdf` (生成)

**Step 1: fixtures を作成**

すべて既存 `linear-gradient-horizontal.html` と同じレイアウト規約 (margin 32, width 400, height 192) を踏襲する。

`linear-gradient-hint-30pct.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT: linear-gradient with 30% interpolation hint</title>
<style>
  html, body { margin: 0; padding: 0; background: white; }
  .g {
    width: 400px;
    height: 192px;
    margin: 32px;
    background: linear-gradient(90deg, #e53935 0%, 30%, #1e88e5 100%);
  }
</style>
</head>
<body>
  <div class="g"></div>
</body>
</html>
```

`linear-gradient-hint-70pct.html`: 同形式で `30%` を `70%` に変更。

`radial-gradient-hint.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT: radial-gradient with interpolation hint</title>
<style>
  html, body { margin: 0; padding: 0; background: white; }
  .g {
    width: 400px;
    height: 192px;
    margin: 32px;
    background: radial-gradient(circle at center, #e53935 0%, 30%, #1e88e5 100%);
  }
</style>
</head>
<body>
  <div class="g"></div>
</body>
</html>
```

`repeating-linear-gradient-hint.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT: repeating-linear-gradient with hint</title>
<style>
  html, body { margin: 0; padding: 0; background: white; }
  .g {
    width: 400px;
    height: 192px;
    margin: 32px;
    background: repeating-linear-gradient(90deg, #e53935 0%, 30%, #1e88e5 50%, #e53935 100%);
  }
</style>
</head>
<body>
  <div class="g"></div>
</body>
</html>
```

**Step 2: `manifest.toml` に 4 entries 追加**

既存 `paint/repeating-radial-gradient.html` 等の隣に:

```toml
[[fixture]]
path = "paint/linear-gradient-hint-30pct.html"
margin_pt = 0.0

[[fixture]]
path = "paint/linear-gradient-hint-70pct.html"
margin_pt = 0.0

[[fixture]]
path = "paint/radial-gradient-hint.html"
margin_pt = 0.0

[[fixture]]
path = "paint/repeating-linear-gradient-hint.html"
margin_pt = 0.0
```

**Step 3: goldens を生成**

CLAUDE.md の手順に従い、bundled font 環境で生成:

Run:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt
```

Expected: 4 つの新規 golden PDF が `crates/fulgur-vrt/goldens/fulgur/paint/` に作成される

**Step 4: VRT 全 pass 確認 (golden 比較モード)**

Run:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt
```

Expected: 全 fixture pass (新規 4 件含む)

**Step 5: 視覚的サニティチェック**

`pdftocairo` で新規 golden を画像化 (失敗時用ではなく確認用):

```bash
for f in linear-gradient-hint-30pct linear-gradient-hint-70pct radial-gradient-hint repeating-linear-gradient-hint; do
  pdftocairo -r 150 -png crates/fulgur-vrt/goldens/fulgur/paint/${f}.pdf /tmp/check-${f}
done
ls -la /tmp/check-*.png
```

期待値:

- `linear-gradient-hint-30pct`: 横方向 30% 地点が紫 (中間色) に来る。30% より前に紫寄り、30% より後に青寄りのカーブ。
- `linear-gradient-hint-70pct`: 70% 地点に紫、左寄りに紫、右側に青寄り。
- `radial-gradient-hint`: 中心から半径 30% 付近で紫。
- `repeating-linear-gradient-hint`: 1 周期 (50%) 内に 30% hint が効き、4 stripe 構造 (≒ 2 周期) で繰り返す。

**Step 6: Commit**

```bash
git add -A
git commit -m "test(vrt): add interpolation hint goldens for linear/radial/repeating"
```

---

## Task 6: render-smoke test for the new draw branch

CLAUDE.md "Coverage scope" Gotcha に従い、`expand_interpolation_hints` の draw 経路を `Engine::render_html` 経由で叩く end-to-end smoke を追加。VRT は codecov 対象外なので lib 側にも置く必要あり。

**Files:**

- Modify: `crates/fulgur/tests/render_smoke.rs`

**Step 1: smoke test 追加**

`render_smoke.rs` の末尾に:

```rust
#[test]
fn linear_gradient_with_interpolation_hint_renders_via_engine() {
    // CSS Images 3 §3.5.3 hint expansion を `Engine::render_html` 経由で叩く
    // (fulgur-2zam). VRT は codecov 対象外なので draw branch 起動の証拠を
    // ここに残す (CLAUDE.md "Coverage scope" Gotcha).
    let html = r#"<html><body><div style="width:200px;height:100px;background:linear-gradient(red, 30%, blue)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn radial_gradient_with_interpolation_hint_renders_via_engine() {
    let html = r#"<html><body><div style="width:200px;height:100px;background:radial-gradient(red, 30%, blue)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}

#[test]
fn repeating_linear_gradient_with_hint_renders_via_engine() {
    // hint expansion + repeating 周期展開の組み合わせ経路.
    let html = r#"<html><body><div style="width:200px;height:100px;background:repeating-linear-gradient(red, 30%, blue 50%, red 100%)">x</div></body></html>"#;
    let pdf = Engine::builder().build().render_html(html).expect("render");
    assert!(!pdf.is_empty());
}
```

**Step 2: 全テスト pass 確認**

Run: `cargo test -p fulgur --test render_smoke`
Expected: 既存 smoke + 3 件追加 = 全 pass

**Step 3: 全体 sanity**

Run: `cargo test -p fulgur && cargo fmt --check && cargo clippy -p fulgur -- -D warnings`
Expected: 全 pass, clippy clean

**Step 4: Commit**

```bash
git add -A
git commit -m "test(smoke): add render-smoke for gradient interpolation hint"
```

---

## Task 7: Final verification & cleanup

**Files:**

- (なし — 検証と markdown lint のみ)

**Step 1: workspace 全体テスト**

Run: `cargo test --workspace`
Expected: 全 pass

**Step 2: fmt / clippy**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean

**Step 3: VRT 最終確認**

Run: `FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt`
Expected: 全 pass

**Step 4: markdown lint**

Run: `npx markdownlint-cli2 'docs/plans/2026-04-28-gradient-interpolation-hint.md'`
Expected: clean

**Step 5: 最終 commit (なくてもよい — 全タスクのまとめ commit)**

不要であれば skip。Task 1-6 で個別 commit 済み。

---

## ロールバック計画

各タスクは独立 commit なので、問題発生時は `git revert <task-N-commit>` で個別 rollback 可能。特に Task 1 (struct field 追加) は他のすべての変更の前提なので、これを revert すると Task 2 以降もコンパイル不能になる点に注意。
