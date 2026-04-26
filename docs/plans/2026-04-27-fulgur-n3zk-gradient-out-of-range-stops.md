# Gradient Out-of-Range Stop Positions Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS Images 3 §3.5.1 準拠で、`linear-gradient(red -50%, blue 100%)` のような [0, 1] 範囲外 stop position を正しくレンダリングする。

**Architecture:** `crates/fulgur/src/convert.rs::resolve_color_stops` 内の範囲外 bail を撤去し、新規ヘルパー `renormalize_stops_to_unit_range` で範囲外 stop を `color_at(0)` / `color_at(1)` の補間合成と (0, 1) 内 stop の組み合わせに変換する。Krilla の `NormalizedF32` 制約を満たしつつ、CSS spec 通りの可視結果を保つ。`resolve_color_stops` は linear / radial 共通なので、両 gradient で動作する。

**Tech Stack:** Rust, Krilla (PDF gradients), Stylo (CSS computed values), fulgur-vrt (PDF test↔ref pixel-diff harness)

**Beads Issue:** fulgur-n3zk

---

## Task 1: `renormalize_stops_to_unit_range` ヘルパー (TDD)

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (新規関数追加 + tests module 追加)

**Step 1: Write failing tests**

`crates/fulgur/src/convert.rs` の最後の `mod tests` 内（または新規 `mod gradient_renormalize_tests`）に以下を追加。

```rust
#[cfg(test)]
mod gradient_renormalize_tests {
    use super::*;
    use crate::pageable::GradientStop;

    fn s(offset: f32, r: u8, g: u8, b: u8) -> (f32, [u8; 4]) {
        (offset, [r, g, b, 255])
    }

    fn expect(stops: &[GradientStop], expected: &[(f32, [u8; 4])]) {
        assert_eq!(stops.len(), expected.len(), "stop count mismatch");
        for (i, (got, exp)) in stops.iter().zip(expected).enumerate() {
            assert!(
                (got.offset - exp.0).abs() < 1e-5,
                "stop[{i}].offset: got {} expected {}",
                got.offset,
                exp.0
            );
            assert_eq!(got.rgba, exp.1, "stop[{i}].rgba");
        }
    }

    #[test]
    fn no_op_when_all_in_range() {
        let stops = vec![s(0.0, 255, 0, 0), s(1.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [255, 0, 0, 255]), (1.0, [0, 0, 255, 255])]);
    }

    #[test]
    fn synthesize_left_endpoint() {
        // red at -50%, blue at 100%: at offset 0, t = 0.5 / 1.5 = 1/3
        // r = 255 + 1/3 * (0 - 255) = 170
        // g = 0
        // b = 0   + 1/3 * (255 - 0) = 85
        let stops = vec![s(-0.5, 255, 0, 0), s(1.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [170, 0, 85, 255]), (1.0, [0, 0, 255, 255])]);
    }

    #[test]
    fn synthesize_right_endpoint() {
        // red at 50%, blue at 200%: at offset 1, t = 0.5 / 1.5 = 1/3
        let stops = vec![s(0.5, 255, 0, 0), s(2.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(
            &result,
            &[
                (0.0, [255, 0, 0, 255]),
                (0.5, [255, 0, 0, 255]),
                (1.0, [170, 0, 85, 255]),
            ],
        );
    }

    #[test]
    fn all_below_zero_pads_with_last_color() {
        // stops at -50%, -25%: both out of range; pad after last (blue)
        let stops = vec![s(-0.5, 255, 0, 0), s(-0.25, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [0, 0, 255, 255]), (1.0, [0, 0, 255, 255])]);
    }

    #[test]
    fn all_above_one_pads_with_first_color() {
        // stops at 150%, 200%: both out of range; pad before first (red)
        let stops = vec![s(1.5, 255, 0, 0), s(2.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [255, 0, 0, 255]), (1.0, [255, 0, 0, 255])]);
    }

    #[test]
    fn boundary_stop_at_zero_no_left_synthesis() {
        // red at -50%, blue at 0%, green at 100%
        // 0 はちょうど blue の位置なので合成不要
        let stops = vec![s(-0.5, 255, 0, 0), s(0.0, 0, 0, 255), s(1.0, 0, 255, 0)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [0, 0, 255, 255]), (1.0, [0, 255, 0, 255])]);
    }

    #[test]
    fn alpha_channel_is_interpolated() {
        // red(alpha=0) at -50%, blue(alpha=255) at 100%
        // at offset 0, t = 1/3 → alpha = 0 + 1/3 * 255 ≈ 85
        let stops = vec![(-0.5_f32, [255, 0, 0, 0]), (1.0_f32, [0, 0, 255, 255])];
        let result = renormalize_stops_to_unit_range(stops);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].rgba[3], 85, "alpha at offset 0");
        assert_eq!(result[1].rgba[3], 255, "alpha at offset 1");
    }
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur --lib gradient_renormalize 2>&1 | tail -10
```

Expected: FAIL with "cannot find function `renormalize_stops_to_unit_range`"

**Step 3: Implement the helper**

`crates/fulgur/src/convert.rs` の `resolve_color_stops` 直前 (3204行付近) に追加。

```rust
/// 範囲外 stop を CSS Images 3 §3.5.1 準拠で [0, 1] 内表現に変換する。
///
/// 入力: monotonically non-decreasing position の (pos, rgba) ベクタ。
/// pos は ℝ で、範囲外 (-0.5 や 1.5 など) も許容する。
///
/// 出力: krilla `NormalizedF32` 制約を満たす GradientStop ベクタ (offset ∈ [0, 1])。
///
/// アルゴリズム:
/// - fast path: 全 stop が [0, 1] 内なら無変換
/// - 範囲外を含む場合: `color_at(0)` / `color_at(1)` を隣接 stop 線形補間
///   (または pad 前方/後方) で合成し、(0, 1) 内既存 stop と組み合わせる
fn renormalize_stops_to_unit_range(
    stops: Vec<(f32, [u8; 4])>,
) -> Vec<crate::pageable::GradientStop> {
    use crate::pageable::GradientStop;

    debug_assert!(stops.len() >= 2, "caller guaranteed len >= 2");

    if stops.iter().all(|(p, _)| (0.0..=1.0).contains(p)) {
        return stops
            .into_iter()
            .map(|(p, rgba)| GradientStop { offset: p, rgba })
            .collect();
    }

    let last_idx = stops.len() - 1;
    let color_at = |t: f32| -> [u8; 4] {
        if t <= stops[0].0 {
            return stops[0].1;
        }
        if t >= stops[last_idx].0 {
            return stops[last_idx].1;
        }
        for w in stops.windows(2) {
            let (p0, c0) = w[0];
            let (p1, c1) = w[1];
            if p0 <= t && t <= p1 {
                let span = p1 - p0;
                if span <= 0.0 {
                    return c1;
                }
                let alpha = (t - p0) / span;
                return [
                    lerp_u8(c0[0], c1[0], alpha),
                    lerp_u8(c0[1], c1[1], alpha),
                    lerp_u8(c0[2], c1[2], alpha),
                    lerp_u8(c0[3], c1[3], alpha),
                ];
            }
        }
        unreachable!("stops are monotonic and t is in [stops[0].0, stops[last].0]")
    };

    let mut result = Vec::with_capacity(stops.len() + 2);

    if stops[0].0 != 0.0 {
        result.push(GradientStop {
            offset: 0.0,
            rgba: color_at(0.0),
        });
    }

    for (p, rgba) in stops.iter() {
        if (0.0..=1.0).contains(p) {
            result.push(GradientStop {
                offset: *p,
                rgba: *rgba,
            });
        }
    }

    if stops[last_idx].0 != 1.0 {
        result.push(GradientStop {
            offset: 1.0,
            rgba: color_at(1.0),
        });
    }

    result
}

#[inline]
fn lerp_u8(a: u8, b: u8, alpha: f32) -> u8 {
    let av = a as f32;
    let bv = b as f32;
    (av + (bv - av) * alpha).round().clamp(0.0, 255.0) as u8
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test -p fulgur --lib gradient_renormalize 2>&1 | tail -10
```

Expected: 7 tests pass

**Step 5: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(gradient): add renormalize_stops_to_unit_range helper

Convert out-of-range CSS gradient stop positions to [0,1]-range
equivalents by synthesizing boundary stops via linear interpolation
in sRGB byte space (CSS Images 3 §3.5.1, with pad mode for stops
entirely outside the visible range).

For fulgur-n3zk."
```

---

## Task 2: helper を `background.rs` に移し、`resolve_gradient_stops` で wire する

**REPLAN NOTE (2026-04-27):** PR #241 / #242 で gradient stop 解決が `convert.rs::resolve_color_stops` から `background.rs::resolve_gradient_stops` に移動済み。Task 1 で `convert.rs` に置いた helper は実際の call site (background.rs) からは layering 上望ましくない位置にある。Task 2 で背景的に正しい場所へ移動 + wire する。monotonic-clamp の `NEG_INFINITY` 修正は既に Phase 1 で適用済 (background.rs:425)。

**Files:**

- Modify: `crates/fulgur/src/convert.rs:3204-3306` (helper + lerp_u8 + tests を削除)
- Modify: `crates/fulgur/src/background.rs:380-480` (helper 移植 + wire) および `:2511-2627` (テスト追加 + 既存 None 期待 2 件を更新)

### Step 1: 現状把握

`crates/fulgur/src/background.rs::resolve_gradient_stops` の現在の構造:

1. `Vec<GradientStop>` 入力 → `Vec<Option<f32>>` positions に変換 (Auto/Fraction/LengthPx の解決)
2. 先頭/末尾 Auto を 0 / 1 に
3. monotonic clamp (`f32::NEG_INFINITY` 初期値、修正済み)
4. 中間 Auto を等間隔補間
5. **(削除対象)** `(0.0..=1.0).contains(&p)` チェックで Layer drop
6. `Vec<krilla::paint::Stop>` 構築

Step 5 を `renormalize_stops_to_unit_range` 呼び出しに置換、Step 6 を renormalize 結果から krilla::paint::Stop を構築する形に書き換える。

### Step 2: `convert.rs` から helper / lerp_u8 / tests を削除

`crates/fulgur/src/convert.rs:3204-3306` の以下を削除:

- `renormalize_stops_to_unit_range` 関数 (3220-3298 付近、`#[allow(dead_code)]` 付き)
- `lerp_u8` 関数 (3300-3306 付近、`#[allow(dead_code)]` 付き)
- ファイル末尾の `mod gradient_renormalize_tests` モジュール全体

### Step 3: `background.rs` に helper を新規追加 (signature 簡素化)

`crates/fulgur/src/background.rs` の `resolve_gradient_stops` 直前 (line 385 付近) に以下を追加。signature は `Vec<(f32, [u8; 4])>` → `Vec<(f32, [u8; 4])>` (GradientStop wrapping を撤去 — call site は krilla::paint::Stop を直接組むため)。

```rust
/// 範囲外 fraction を CSS Images 3 §3.5.1 準拠で [0, 1] 内表現に renormalize する。
///
/// 入力: monotonically non-decreasing position の (pos, rgba) ベクタ。
/// pos は ℝ で、範囲外 (-0.5 や 1.5 など) も許容する。
///
/// 出力: pos が [0, 1] に収まる (pos, rgba) ベクタ。Krilla の `NormalizedF32`
/// 制約をそのまま満たす。
///
/// アルゴリズム:
/// - fast path: 全 stop が [0, 1] 内なら無変換
/// - 範囲外を含む場合: `color_at(0)` / `color_at(1)` を隣接 stop 線形補間
///   (または pad 前方/後方) で合成し、(0, 1) 内既存 stop と組み合わせる
/// - 既に in-range stop が境界 (0.0 / 1.0) ぴったりに座っている場合は重複合成しない
fn renormalize_stops_to_unit_range(
    stops: Vec<(f32, [u8; 4])>,
) -> Vec<(f32, [u8; 4])> {
    debug_assert!(stops.len() >= 2, "caller guaranteed len >= 2");

    if stops.iter().all(|(p, _)| (0.0..=1.0).contains(p)) {
        return stops;
    }

    let last_idx = stops.len() - 1;
    let color_at = |t: f32| -> [u8; 4] {
        if t <= stops[0].0 {
            return stops[0].1;
        }
        if t >= stops[last_idx].0 {
            return stops[last_idx].1;
        }
        for w in stops.windows(2) {
            let (p0, c0) = w[0];
            let (p1, c1) = w[1];
            if p0 <= t && t <= p1 {
                let span = p1 - p0;
                if span <= 0.0 {
                    return c1;
                }
                let alpha = (t - p0) / span;
                return [
                    lerp_u8(c0[0], c1[0], alpha),
                    lerp_u8(c0[1], c1[1], alpha),
                    lerp_u8(c0[2], c1[2], alpha),
                    lerp_u8(c0[3], c1[3], alpha),
                ];
            }
        }
        unreachable!("stops are monotonic and t is in [stops[0].0, stops[last].0]")
    };

    let has_stop_at_zero = stops.iter().any(|(p, _)| *p == 0.0);
    let has_stop_at_one = stops.iter().any(|(p, _)| *p == 1.0);

    let mut result = Vec::with_capacity(stops.len() + 2);

    if stops[0].0 != 0.0 && !has_stop_at_zero {
        result.push((0.0, color_at(0.0)));
    }

    for (p, rgba) in stops.iter() {
        if (0.0..=1.0).contains(p) {
            result.push((*p, *rgba));
        }
    }

    if stops[last_idx].0 != 1.0 && !has_stop_at_one {
        result.push((1.0, color_at(1.0)));
    }

    result
}

#[inline]
fn lerp_u8(a: u8, b: u8, alpha: f32) -> u8 {
    let av = a as f32;
    let bv = b as f32;
    (av + (bv - av) * alpha).round().clamp(0.0, 255.0) as u8
}
```

`#[allow(dead_code)]` は不要 (Step 5 で wire するため call site が生まれる)。

### Step 4: `background.rs` のテストモジュールに renormalize tests を追加

`crates/fulgur/src/background.rs` の `mod resolve_gradient_stops_tests` に sibling として、または同モジュール内に新規追加。signature が `Vec<(f32, [u8;4])>` → `Vec<(f32, [u8;4])>` になったため、Task 1 commit の tests を簡素化する形で書く。

```rust
#[cfg(test)]
mod renormalize_stops_to_unit_range_tests {
    use super::renormalize_stops_to_unit_range;

    fn s(offset: f32, r: u8, g: u8, b: u8) -> (f32, [u8; 4]) {
        (offset, [r, g, b, 255])
    }

    fn expect(stops: &[(f32, [u8; 4])], expected: &[(f32, [u8; 4])]) {
        assert_eq!(stops.len(), expected.len(), "stop count mismatch");
        for (i, (got, exp)) in stops.iter().zip(expected).enumerate() {
            assert!(
                (got.0 - exp.0).abs() < 1e-5,
                "stop[{i}].offset: got {} expected {}",
                got.0,
                exp.0
            );
            assert_eq!(got.1, exp.1, "stop[{i}].rgba");
        }
    }

    #[test]
    fn no_op_when_all_in_range() {
        let stops = vec![s(0.0, 255, 0, 0), s(1.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [255, 0, 0, 255]), (1.0, [0, 0, 255, 255])]);
    }

    #[test]
    fn synthesize_left_endpoint() {
        // red at -50%, blue at 100%: at offset 0, t = 0.5 / 1.5 = 1/3
        let stops = vec![s(-0.5, 255, 0, 0), s(1.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [170, 0, 85, 255]), (1.0, [0, 0, 255, 255])]);
    }

    #[test]
    fn synthesize_right_endpoint() {
        // red at 50%, blue at 200%: at offset 1, t = 0.5 / 1.5 = 1/3
        let stops = vec![s(0.5, 255, 0, 0), s(2.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(
            &result,
            &[
                (0.0, [255, 0, 0, 255]),
                (0.5, [255, 0, 0, 255]),
                (1.0, [170, 0, 85, 255]),
            ],
        );
    }

    #[test]
    fn all_below_zero_pads_with_last_color() {
        let stops = vec![s(-0.5, 255, 0, 0), s(-0.25, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [0, 0, 255, 255]), (1.0, [0, 0, 255, 255])]);
    }

    #[test]
    fn all_above_one_pads_with_first_color() {
        let stops = vec![s(1.5, 255, 0, 0), s(2.0, 0, 0, 255)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [255, 0, 0, 255]), (1.0, [255, 0, 0, 255])]);
    }

    #[test]
    fn boundary_stop_at_zero_no_left_synthesis() {
        // -50% red, 0% blue, 100% green: 0.0 はちょうど blue で合成不要
        let stops = vec![s(-0.5, 255, 0, 0), s(0.0, 0, 0, 255), s(1.0, 0, 255, 0)];
        let result = renormalize_stops_to_unit_range(stops);
        expect(&result, &[(0.0, [0, 0, 255, 255]), (1.0, [0, 255, 0, 255])]);
    }

    #[test]
    fn alpha_channel_is_interpolated() {
        let stops = vec![(-0.5_f32, [255, 0, 0, 0]), (1.0_f32, [0, 0, 255, 255])];
        let result = renormalize_stops_to_unit_range(stops);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1[3], 85, "alpha at offset 0");
        assert_eq!(result[1].1[3], 255, "alpha at offset 1");
    }
}
```

### Step 5: `resolve_gradient_stops` の bail を renormalize 呼び出しに置換

`crates/fulgur/src/background.rs` の `resolve_gradient_stops` (line 385-480) のうち、

```rust
// 削除対象 (line 455-466)
for (idx, p_opt) in positions.iter().enumerate() {
    let p = p_opt.expect("all slots resolved");
    if !(0.0..=1.0).contains(&p) {
        log::warn!(
            "{gradient_kind}: stop {idx} resolved offset {p:.4} is outside \
             [0, 1]. Out-of-range stops require gradient-line recompute \
             (fulgur-n3zk). Layer dropped."
        );
        return None;
    }
}

// krilla::paint::Stop 構築
Some(
    stops
        .iter()
        .zip(positions)
        .map(|(s, p)| krilla::paint::Stop {
            offset: krilla::num::NormalizedF32::new(p.unwrap()).expect("offset is in [0, 1]"),
            color: krilla::color::rgb::Color::new(s.rgba[0], s.rgba[1], s.rgba[2]).into(),
            opacity: crate::pageable::alpha_to_opacity(s.rgba[3]),
        })
        .collect(),
)
```

を以下に置換:

```rust
// renormalize: 範囲外 fraction は端点合成で [0, 1] 内表現に変換 (fulgur-n3zk)
let resolved: Vec<(f32, [u8; 4])> = stops
    .iter()
    .zip(positions)
    .map(|(s, p)| (p.expect("all slots resolved"), s.rgba))
    .collect();
let renormalized = renormalize_stops_to_unit_range(resolved);

if renormalized.len() < 2 {
    // 退化ケース (理論上は起こらない: helper は最低 1 stop は返すが、
    // krilla のグラデーションは 2 stop 以上必要)
    return None;
}

Some(
    renormalized
        .into_iter()
        .map(|(p, rgba)| krilla::paint::Stop {
            offset: krilla::num::NormalizedF32::new(p).expect("renormalize guarantees [0, 1]"),
            color: krilla::color::rgb::Color::new(rgba[0], rgba[1], rgba[2]).into(),
            opacity: crate::pageable::alpha_to_opacity(rgba[3]),
        })
        .collect(),
)
```

### Step 6: `resolve_gradient_stops` の doc コメント更新

`background.rs:380-384` 付近の手順 5 (`最終 fraction が [0, 1] 外なら Layer drop`) を以下に変更:

```rust
///   5. 最終 fraction が `[0, 1]` 外なら `renormalize_stops_to_unit_range` で
///      端点合成し正規化する (CSS Images 3 §3.5.1; fulgur-n3zk)
```

### Step 7: 既存 None-期待テストを更新

`background.rs` の `mod resolve_gradient_stops_tests` 内の以下 2 件を新仕様に合わせて書き換える。

- `out_of_range_length_returns_none` (line 2586)
- `negative_fraction_returns_none` (line 2597)

```rust
#[test]
fn out_of_range_length_renormalized() {
    // 50px on line_length=30 → 50/30 ≈ 1.667
    // stops: [(0.0, red), (1.667, blue)]
    // renormalize: at offset 0 → red (boundary に既存 stop)
    //              at offset 1 → red + (1.0/1.667) * (blue - red) ≈ red + 0.6 * (blue - red)
    //              = (255, 0, 0) + 0.6 * ((0, 0, 255) - (255, 0, 0)) = (102, 0, 153)
    let stops = vec![
        stop(fr(0.0), [255, 0, 0, 255]),
        stop(px(50.0), [0, 0, 255, 255]),
    ];
    let out = resolve_gradient_stops(&stops, 30.0, "linear-gradient").unwrap();
    assert_eq!(out.len(), 2);
    assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
    assert_eq!(
        (out[0].color.clone(), out[1].offset.get()),
        // red at 0
        // synthesized at 1
        (krilla::color::rgb::Color::new(255, 0, 0).into(), 1.0),
    );
    // boundary 1 の合成色を verify (rounding tolerance ±1)
    let krilla::paint::Color::Rgb(c) = &out[1].color else {
        panic!("expected Rgb color");
    };
    assert!((c.red() as i32 - 102).abs() <= 1);
    assert_eq!(c.green(), 0);
    assert!((c.blue() as i32 - 153).abs() <= 1);
}

#[test]
fn negative_fraction_renormalized() {
    // [(fr(-0.1), red), (fr(1.0), blue)]
    // renormalize: at offset 0 → t = 0.1/1.1 ≈ 0.0909
    //   r = 255 + 0.0909 * (0 - 255) ≈ 232
    //   g = 0
    //   b = 0 + 0.0909 * 255 ≈ 23
    // at offset 1 → blue (boundary 既存)
    let stops = vec![
        stop(fr(-0.1), [255, 0, 0, 255]),
        stop(fr(1.0), [0, 0, 255, 255]),
    ];
    let out = resolve_gradient_stops(&stops, 100.0, "linear-gradient").unwrap();
    assert_eq!(out.len(), 2);
    assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
    assert!((out[1].offset.get() - 1.0).abs() < 1e-6);
    let krilla::paint::Color::Rgb(c0) = &out[0].color else {
        panic!("expected Rgb color");
    };
    assert!((c0.red() as i32 - 232).abs() <= 2);
    assert_eq!(c0.green(), 0);
    assert!((c0.blue() as i32 - 23).abs() <= 2);
}
```

注意: krilla の `Color` 内部表現 (内部 fields・accessors) は実際の API に合わせて調整する必要がある。可能であれば `out[0].color` の内部値を直接比較せず、helper を経由して RGB tuple を取得する。簡易化として「合成 stop が len() == 2 で `offset` が 0/1」だけを assert にして、合成色は VRT (Task 3, 4) で verify するという選択肢もある。

実装者は krilla 0.x の API に合わせ、最も保守的に書く (`assert_eq!(out.len(), 2)` + offset の検証のみで PASS させ、色は VRT で verify)。

### Step 8: 全 fulgur tests 実行

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```

Expected: 既存 + 新規 (renormalize 7) = pass

### Step 9: VRT 既存 regression check

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10
```

Expected: 全 pass。本変更は in-range fixture には影響しないはず。golden 差分があれば Step 10 で対応。

### Step 10: 万一 golden 差分があった場合

差分内容を確認:

```bash
git diff --stat crates/fulgur-vrt/goldens/ 2>&1 | tail -20
```

期待しない場所の差分は実装ロジックを再確認。意図通りの差分のみであれば update:

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt 2>&1 | tail -10
```

### Step 11: clippy / fmt

```bash
cargo clippy --all-targets 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -5
```

Expected: warnings なし、diff なし

### Step 12: Commit

```bash
git add crates/fulgur/src/convert.rs crates/fulgur/src/background.rs
# golden 差分があれば併せて add
git diff --stat --cached
git commit -m "feat(gradient): allow out-of-range stop positions

Move renormalize_stops_to_unit_range from convert.rs to background.rs
(near the resolve_gradient_stops call site) and wire it in: drop the
log::warn bail for out-of-range fractions, instead synthesize boundary
stops via linear interpolation per CSS Images 3 §3.5.1.

Also updates the two existing tests (out_of_range_length_returns_none,
negative_fraction_returns_none) to verify the new renormalize behavior
instead of asserting None.

For fulgur-n3zk."
```

---

## Task 3: VRT test↔ref harness で linear out-of-range 検証

**Files:**

- Create: `crates/fulgur-vrt/fixtures/paint/linear-gradient-out-of-range-low.html`
- Create: `crates/fulgur-vrt/fixtures/paint/linear-gradient-out-of-range-high.html`
- Modify: `crates/fulgur-vrt/tests/gradient_harness.rs` (新規 test 関数追加)

**Step 1: fixture HTML 作成**

`linear-gradient-out-of-range-low.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT test: linear-gradient out-of-range low stop (-50%, 100%)</title>
<style>
  html, body { margin: 0; padding: 0; background: white; }
  .g {
    width: 400px;
    height: 192px;
    margin: 32px;
    background: linear-gradient(to right, #e53935 -50%, #1e88e5 100%);
  }
</style>
</head>
<body>
  <div class="g"></div>
</body>
</html>
```

`linear-gradient-out-of-range-high.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT test: linear-gradient out-of-range high stop (0%, 200%)</title>
<style>
  html, body { margin: 0; padding: 0; background: white; }
  .g {
    width: 400px;
    height: 192px;
    margin: 32px;
    background: linear-gradient(to right, #e53935 0%, #1e88e5 200%);
  }
</style>
</head>
<body>
  <div class="g"></div>
</body>
</html>
```

box / margin 寸法は `linear_gradient_horizontal_matches_strip_reference` と一致させる必要があり、`build_strip_ref_html` の `GRADIENT_*_PX` 定数 (400 / 192 / 32) と整合する。

**Step 2: gradient_harness.rs に test 関数追加**

`crates/fulgur-vrt/tests/gradient_harness.rs` 末尾に以下を追加。先頭の `linear_gradient_horizontal_matches_strip_reference` を雛形にしているが、共通スキャフォルディング `run_gradient_px_stop_reftest` を再利用する。

```rust
/// `linear-gradient(to right, #e53935 -50%, #1e88e5 100%)` の renormalize 動作確認。
///
/// 範囲外 stop -50% は drop され、offset 0 の色は `red + (1/3) * (blue - red)` で
/// 合成される。strip 近似 ref の左端をこの合成色、右端を blue にして比較する。
#[test]
fn linear_gradient_out_of_range_low_matches_strip_reference() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_html_path =
        crate_root.join("fixtures/paint/linear-gradient-out-of-range-low.html");
    let test_html = fs::read_to_string(&test_html_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_html_path.display()));

    // synthesized color at offset 0:
    // r = 0xe5 + (1/3) * (0x1e - 0xe5) = 0xab
    // g = 0x39 + (1/3) * (0x88 - 0x39) = 0x53
    // b = 0x35 + (1/3) * (0xe5 - 0x35) = 0x82
    let ref_html = build_strip_ref_html((0xab, 0x53, 0x82), (0x1e, 0x88, 0xe5));

    let tol = Tolerance {
        max_channel_diff: 10,
        max_diff_pixels_ratio: 0.005,
    };
    run_gradient_px_stop_reftest(
        "linear-gradient out-of-range low",
        "vrt-gradient-oor-low",
        &test_html,
        &ref_html,
        tol,
    );
}

/// `linear-gradient(to right, #e53935 0%, #1e88e5 200%)` の renormalize 動作確認。
///
/// offset 1 の色は `red + (1/2) * (blue - red)` で合成される。strip 近似 ref の
/// 左端を red、右端をこの合成色にする。
#[test]
fn linear_gradient_out_of_range_high_matches_strip_reference() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_html_path =
        crate_root.join("fixtures/paint/linear-gradient-out-of-range-high.html");
    let test_html = fs::read_to_string(&test_html_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_html_path.display()));

    // synthesized color at offset 1:
    // r = 0xe5 + (1/2) * (0x1e - 0xe5) = 0x82
    // g = 0x39 + (1/2) * (0x88 - 0x39) = 0x61
    // b = 0x35 + (1/2) * (0xe5 - 0x35) = 0x8d
    let ref_html = build_strip_ref_html((0xe5, 0x39, 0x35), (0x82, 0x61, 0x8d));

    let tol = Tolerance {
        max_channel_diff: 10,
        max_diff_pixels_ratio: 0.005,
    };
    run_gradient_px_stop_reftest(
        "linear-gradient out-of-range high",
        "vrt-gradient-oor-high",
        &test_html,
        &ref_html,
        tol,
    );
}
```

**Step 3: テスト実行**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt linear_gradient_out_of_range 2>&1 | tail -15
```

Expected: 2 tests pass

**Step 4: clippy / fmt 確認**

```bash
cargo clippy -p fulgur-vrt --tests 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -5
```

Expected: warnings なし、diff なし

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/fixtures/paint/linear-gradient-out-of-range-low.html
git add crates/fulgur-vrt/fixtures/paint/linear-gradient-out-of-range-high.html
git add crates/fulgur-vrt/tests/gradient_harness.rs
git commit -m "test(gradient): add VRT test↔ref for out-of-range linear stops

Verify that linear-gradient(red -50%, blue 100%) and
linear-gradient(red 0%, blue 200%) render with synthesized boundary
colors matching CSS Images 3 §3.5.1 expectations.

For fulgur-n3zk."
```

---

## Task 4: VRT test↔ref harness で radial out-of-range 検証

**Files:**

- Create: `crates/fulgur-vrt/fixtures/paint/radial-gradient-out-of-range.html`
- Modify: `crates/fulgur-vrt/tests/radial_gradient_harness.rs` (既存 harness ファイルがあれば追加、なければ簡易 test を gradient_harness.rs に追加)

**Step 1: 既存 radial harness の確認**

```bash
cat crates/fulgur-vrt/tests/radial_gradient_harness.rs | head -50
```

存在するなら既存パターン (例: `build_radial_strip_ref_html` 等) に倣う。なければ Task 4 全体を簡易化:
PDF byte-compare のみで verify (新規 fixture + golden 生成)。

**Step 2 (radial harness あり): test 関数追加**

既存 harness の流儀で `radial_gradient_out_of_range_matches_ref` を追加。 stop は例えば
`radial-gradient(circle at center, #e53935 -50%, #1e88e5 100%)`。 中心から外周への線形補間で offset 0 の色が同じく
`(0xab, 0x53, 0x82)` になる。

**Step 2 (radial harness なし): byte-compare のみ**

`crates/fulgur-vrt/fixtures/paint/radial-gradient-out-of-range.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT test: radial-gradient out-of-range stop</title>
<style>
  html, body { margin: 0; padding: 0; background: white; }
  .g {
    width: 400px;
    height: 192px;
    margin: 32px;
    background: radial-gradient(circle at center, #e53935 -50%, #1e88e5 100%);
  }
</style>
</head>
<body>
  <div class="g"></div>
</body>
</html>
```

**Step 3: golden 生成 (byte-compare 経路の場合)**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt radial_gradient_out_of_range 2>&1 | tail -10
```

生成された PDF を視覚確認:

```bash
pdftocairo -png -r 150 crates/fulgur-vrt/goldens/fulgur/paint/radial-gradient-out-of-range.pdf /tmp/radial-oor
ls /tmp/radial-oor*
```

中心から外周にかけて `mix(red, blue, 1/3)` → blue になっているか目視確認。

**Step 4: 通常テスト実行**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt radial_gradient_out_of_range 2>&1 | tail -10
```

Expected: pass (byte-compare で同一)

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/fixtures/paint/radial-gradient-out-of-range.html
git add crates/fulgur-vrt/goldens/fulgur/paint/radial-gradient-out-of-range.pdf
# harness を変更した場合は併せて add
git commit -m "test(gradient): add VRT for radial out-of-range stop

For fulgur-n3zk."
```

---

## Task 5: 最終検証

**Step 1: フル test suite (fulgur)**

```bash
cargo test -p fulgur 2>&1 | tail -5
```

Expected: 全 pass

**Step 2: フル VRT**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -10
```

Expected: 全 pass、新規 test 含む

**Step 3: clippy + fmt (workspace 全体)**

```bash
cargo clippy --all-targets 2>&1 | tail -15
cargo fmt --check 2>&1 | tail -5
```

Expected: warnings なし、diff なし

**Step 4: markdown lint**

```bash
npx markdownlint-cli2 'docs/plans/2026-04-27-fulgur-n3zk-gradient-out-of-range-stops.md' 2>&1 | tail -5
```

Expected: pass

**Step 5: 機能確認 (任意, スモークテスト)**

簡易 HTML を CLI で render して目視確認:

```bash
cat > /tmp/oor.html <<'EOF'
<!DOCTYPE html>
<html><body>
<div style="width:400px;height:100px;background:linear-gradient(to right, red -50%, blue 100%);"></div>
<div style="width:400px;height:100px;background:linear-gradient(to right, red 0%, blue 200%);"></div>
<div style="width:400px;height:100px;background:radial-gradient(circle, red -50%, blue 100%);"></div>
</body></html>
EOF
cargo run --bin fulgur -- render /tmp/oor.html -o /tmp/oor.pdf
pdftocairo -png -r 150 /tmp/oor.pdf /tmp/oor
```

3 つの box が renormalize 通りに描画されていることを目視確認。

**Step 6: log 確認**

`linear-gradient: stop position ... is outside [0, 1].` ログが消えていることを確認:

```bash
cat > /tmp/oor.html <<'EOF'
<!DOCTYPE html>
<html><body>
<div style="width:400px;height:100px;background:linear-gradient(to right, red -50%, blue 100%);"></div>
</body></html>
EOF
RUST_LOG=warn cargo run --bin fulgur -- render /tmp/oor.html -o /tmp/oor.pdf 2>&1 | grep -i "stop position" || echo "no warning emitted (expected)"
```

Expected: `no warning emitted`

---

## Notes

- Task 1 の helper を独立完結させ、TDD 駆動で実装する。Task 2 は既存テストで regression 確認のみ
- Task 3 の VRT test↔ref で実装の visual correctness を担保する
- Task 4 の radial は `radial_gradient_harness.rs` の有無で 2 パスある (Step 1 で分岐)
- log::warn の "is outside [0, 1]" メッセージは Task 2 の bail 削除と一緒に消える
- いずれの Commit メッセージも英語 (memory: feedback_pr_body_japanese.md)
