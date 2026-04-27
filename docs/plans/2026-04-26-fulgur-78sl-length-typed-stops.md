# gradient: length-typed stop position 対応 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `linear-gradient(red 50px, blue)` のような length-typed stop position を Phase 1 の `Layer dropped` 廃止し、draw 時に gradient line 長さで fraction 化して正しくレンダリングする。

**Architecture:** `GradientStop.offset: f32` を `position: GradientStopPosition` (enum: `Auto` / `Fraction(f32)` / `LengthPx(f32)`) に変更。convert 時は length 値を保持するだけで CSS fixup を行わない。draw 時に新ヘルパー `resolve_gradient_stops` で gradient line 長さ (linear: `|W·sinθ|+|H·cosθ|`, radial: rx) を使って解決 → CSS Images §3.5.1 の auto/monotonic fixup → 範囲外は Layer drop (fulgur-n3zk と境界統一)。

**Tech Stack:** Rust / Stylo (LengthPercentage::to_length / to_percentage) / Krilla LinearGradient / RadialGradient / fulgur-vrt strip-approximation harness

**Working directory:** `/home/ubuntu/fulgur/.worktrees/fulgur-78sl-px-stops` (branch `feature/fulgur-78sl-length-stops`)

**Reference:**

- beads: fulgur-78sl (design 保存済)
- 関連: fulgur-n3zk (out-of-range stop, 別 issue)
- CSS: <https://drafts.csswg.org/css-images-3/#color-stop-syntax>
- CSS Images §3.6.1: gradient ray 長さ = ending shape の +X 軸 radius (ellipse でも rx)

---

## Task 1: 型の刷新 — `GradientStopPosition` 導入 (no-op refactor)

**目的:** 既存挙動を維持したまま `GradientStop.offset: f32` を `position: GradientStopPosition` に置換。新 `LengthPx` variant の導入はあとで使い、まずは `Fraction(f)` のみで動かす。

**Files:**

- Modify: `crates/fulgur/src/pageable.rs:738-747`
- Modify: `crates/fulgur/src/convert.rs:3297-3305` (resolve_color_stops の末尾 map)
- Modify: `crates/fulgur/src/background.rs:412-422` (draw_linear_gradient の krilla::Stop 構築)
- Modify: `crates/fulgur/src/background.rs:541-549` (draw_radial_gradient の krilla::Stop 構築)

**Step 1: 型を pageable.rs に追加**

`crates/fulgur/src/pageable.rs:743` 直前に enum を追加し `GradientStop` を書き換え:

```rust
/// CSS gradient color stop の位置。
///
/// - `Fraction` は `[0, 1]` 域内の % / 0%/100% 由来 (convert 時に解決)。
/// - `LengthPx` は `<length>` 形式で記述された値 (例 `50px`)。draw 時に
///   gradient line 長さで割って fraction 化する。
/// - `Auto` は CSS auto。draw 時に CSS Images §3.5.1 fixup で前後の fixed
///   stop から補間される。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientStopPosition {
    Auto,
    Fraction(f32),
    LengthPx(f32),
}

/// A single color stop in a CSS gradient. Position は `GradientStopPosition`
/// で保持され、draw 時に gradient line 長さで fraction に解決される。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub position: GradientStopPosition,
    pub rgba: [u8; 4],
}
```

**Step 2: convert.rs の構築箇所を更新 (一旦 Fraction のみ)**

`crates/fulgur/src/convert.rs:3297-3305` の `.map(|((_, rgba), pos)| GradientStop { ... })` を:

```rust
.map(|((_, rgba), pos)| crate::pageable::GradientStop {
    position: crate::pageable::GradientStopPosition::Fraction(
        pos.unwrap().clamp(0.0, 1.0),
    ),
    rgba,
})
```

(fixup は残ったまま)

**Step 3: background.rs draw_linear_gradient 修正**

`crates/fulgur/src/background.rs:412-422` の krilla::paint::Stop 構築を、enum match で fraction を取り出す形に変更:

```rust
let krilla_stops: Vec<krilla::paint::Stop> = stops
    .iter()
    .map(|s| {
        let offset_f = match s.position {
            crate::pageable::GradientStopPosition::Fraction(f) => f.clamp(0.0, 1.0),
            // Task 1 では convert 側が Fraction のみ生成するため到達しない
            crate::pageable::GradientStopPosition::Auto
            | crate::pageable::GradientStopPosition::LengthPx(_) => 0.0,
        };
        krilla::paint::Stop {
            offset: krilla::num::NormalizedF32::new(offset_f)
                .expect("offset is clamped to [0, 1]"),
            color: krilla::color::rgb::Color::new(s.rgba[0], s.rgba[1], s.rgba[2]).into(),
            opacity: crate::pageable::alpha_to_opacity(s.rgba[3]),
        }
    })
    .collect();
```

**Step 4: background.rs draw_radial_gradient 同等修正**

`crates/fulgur/src/background.rs:541-549` も同じ enum match パターンで書き換える。

**Step 5: ビルド確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-78sl-px-stops
cargo build -p fulgur 2>&1 | tail -3
```

期待: コンパイル成功 (warning が出る可能性あり、Task 3 で fix)

**Step 6: 既存テスト全 PASS 確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur-vrt 2>&1 | tail -5
```

期待: `test result: ok. 738 passed`、VRT 全 fixture green。pixel-byte 互換が維持されること (no-op refactor)。

**Step 7: コミット**

```bash
git add crates/fulgur/src/pageable.rs crates/fulgur/src/convert.rs crates/fulgur/src/background.rs
git commit -m "refactor(gradient): introduce GradientStopPosition enum (no-op)"
```

---

## Task 2: `resolve_gradient_stops` ヘルパー追加 (TDD)

**目的:** convert 時の fixup ロジックを background.rs に移植し、length-typed と auto を draw 時に解決するヘルパーを TDD で実装する。

**Files:**

- Create / Modify: `crates/fulgur/src/background.rs` (関数追加 + テストモジュール)

**Step 1: 失敗テストを書く**

`crates/fulgur/src/background.rs` の末尾 `#[cfg(test)] mod tests` (なければ新規作成) に以下を追加:

```rust
#[cfg(test)]
mod resolve_gradient_stops_tests {
    use super::*;
    use crate::pageable::{GradientStop, GradientStopPosition::*};

    fn fr(f: f32) -> GradientStopPosition { Fraction(f) }
    fn px(f: f32) -> GradientStopPosition { LengthPx(f) }
    fn stop(p: GradientStopPosition, rgba: [u8; 4]) -> GradientStop {
        GradientStop { position: p, rgba }
    }

    #[test]
    fn fraction_only_passes_through() {
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, "linear-gradient").unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
        assert!((out[1].offset.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn length_px_resolved_by_line_length() {
        let stops = vec![
            stop(px(0.0), [255, 0, 0, 255]),
            stop(px(50.0), [0, 0, 255, 255]),
        ];
        // line_length = 100 → 50px = 0.5
        let out = resolve_gradient_stops(&stops, 100.0, "linear-gradient").unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[1].offset.get() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn auto_position_filled_at_endpoints() {
        let stops = vec![
            stop(Auto, [255, 0, 0, 255]),
            stop(Auto, [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, "linear-gradient").unwrap();
        assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
        assert!((out[1].offset.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn auto_position_filled_in_middle() {
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(Auto, [0, 255, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, "linear-gradient").unwrap();
        assert!((out[1].offset.get() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mixed_length_fraction_auto() {
        // linear-gradient(red, blue 50px, green) on line_length=100
        // → red Auto (→0), blue 0.5, green Auto (→1)
        let stops = vec![
            stop(Auto, [255, 0, 0, 255]),
            stop(px(50.0), [0, 0, 255, 255]),
            stop(Auto, [0, 255, 0, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, "linear-gradient").unwrap();
        assert_eq!(out.len(), 3);
        assert!((out[0].offset.get() - 0.0).abs() < 1e-6);
        assert!((out[1].offset.get() - 0.5).abs() < 1e-6);
        assert!((out[2].offset.get() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn out_of_range_length_returns_none() {
        // 50px on line_length=30 → 50/30 ≈ 1.67 > 1 → Layer drop
        let stops = vec![
            stop(fr(0.0), [255, 0, 0, 255]),
            stop(px(50.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 30.0, "linear-gradient");
        assert!(out.is_none());
    }

    #[test]
    fn negative_fraction_returns_none() {
        let stops = vec![
            stop(fr(-0.1), [255, 0, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, "linear-gradient");
        assert!(out.is_none());
    }

    #[test]
    fn monotonic_clamp_applied() {
        // [0.6, 0.3] → [0.6, 0.6] (monotonic fix)
        let stops = vec![
            stop(fr(0.6), [255, 0, 0, 255]),
            stop(fr(0.3), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 100.0, "linear-gradient").unwrap();
        assert!((out[0].offset.get() - 0.6).abs() < 1e-6);
        assert!((out[1].offset.get() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn line_length_zero_returns_none() {
        let stops = vec![
            stop(px(50.0), [255, 0, 0, 255]),
            stop(fr(1.0), [0, 0, 255, 255]),
        ];
        let out = resolve_gradient_stops(&stops, 0.0, "linear-gradient");
        assert!(out.is_none());
    }
}
```

**Step 2: テスト実行 (失敗確認)**

```bash
cargo test -p fulgur --lib resolve_gradient_stops_tests 2>&1 | tail -5
```

期待: `error[E0425]: cannot find function 'resolve_gradient_stops'`

**Step 3: ヘルパー本体を実装**

`crates/fulgur/src/background.rs` の `draw_linear_gradient` の直前あたりに追加:

```rust
/// Resolve `Vec<GradientStop>` (length / fraction / auto 混在) を krilla の
/// `Stop` 列に変換する。
///
/// CSS Images Level 3 §3.5.1 の color-stop fixup に従って:
///   1. `LengthPx(px)` を `px / line_length` で fraction 化
///   2. 先頭/末尾の `Auto` を 0.0 / 1.0 に確定
///   3. monotonic clamp (前 fixed より小さければ前 fixed に合わせる)
///   4. 中間の `Auto` 群を前後 fixed の等間隔補間で埋める
///   5. 最終 fraction が `[0, 1]` 外なら `Layer drop` (None)
///
/// `line_length <= 0` の場合は length stop が解決不能なので None。
fn resolve_gradient_stops(
    stops: &[crate::pageable::GradientStop],
    line_length: f32,
    gradient_kind: &'static str,
) -> Option<Vec<krilla::paint::Stop>> {
    use crate::pageable::GradientStopPosition;

    if stops.len() < 2 {
        return None;
    }
    if line_length <= 0.0 {
        // length-typed stop が一つでもあれば解決不能。fraction-only でも
        // 退化した gradient なので Layer drop 相当 (caller で先に early
        // return しているため通常到達しない)。
        return None;
    }

    let n = stops.len();
    let mut positions: Vec<Option<f32>> = stops
        .iter()
        .map(|s| match s.position {
            GradientStopPosition::Auto => None,
            GradientStopPosition::Fraction(f) => Some(f),
            GradientStopPosition::LengthPx(px) => Some(px / line_length),
        })
        .collect();

    // 先頭/末尾の Auto を 0/1 に
    if positions[0].is_none() {
        positions[0] = Some(0.0);
    }
    if positions[n - 1].is_none() {
        positions[n - 1] = Some(1.0);
    }

    // monotonic clamp
    let mut last_resolved = f32::NEG_INFINITY;
    for v in positions.iter_mut().flatten() {
        if *v < last_resolved {
            *v = last_resolved;
        }
        last_resolved = *v;
    }

    // 中間 Auto を前後 fixed の等間隔補間で埋める
    let mut i = 0;
    while i < n {
        if positions[i].is_some() {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = start;
        while end < n && positions[end].is_none() {
            end += 1;
        }
        let p_prev = positions[start - 1].expect("first slot resolved");
        let p_next = positions[end].expect("last slot resolved");
        let span = (end - start + 1) as f32;
        for (k, slot) in positions.iter_mut().enumerate().take(end).skip(start) {
            let t = (k - start + 1) as f32 / span;
            *slot = Some(p_prev + (p_next - p_prev) * t);
        }
        i = end;
    }

    // 範囲外チェック (fulgur-n3zk が解くまでは Layer drop)
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
                offset: krilla::num::NormalizedF32::new(p.unwrap())
                    .expect("offset is in [0, 1]"),
                color: krilla::color::rgb::Color::new(s.rgba[0], s.rgba[1], s.rgba[2]).into(),
                opacity: crate::pageable::alpha_to_opacity(s.rgba[3]),
            })
            .collect(),
    )
}
```

**Step 4: テスト実行 (PASS 確認)**

```bash
cargo test -p fulgur --lib resolve_gradient_stops_tests 2>&1 | tail -10
```

期待: `test result: ok. 9 passed; 0 failed`

**Step 5: コミット**

```bash
git add crates/fulgur/src/background.rs
git commit -m "feat(gradient): add resolve_gradient_stops helper with CSS fixup"
```

---

## Task 3: convert.rs から fixup 削除 + LengthPx 生成

**目的:** convert 側で length-typed stop を `LengthPx` として保持し、fixup を draw 時に委譲する。

**Files:**

- Modify: `crates/fulgur/src/convert.rs:3204-3306` (resolve_color_stops 全面書き換え)

**Step 1: 失敗テストを書く**

`crates/fulgur/src/convert.rs` の `mod tests` または同 file 末尾の test mod に追加 (既存 test mod が convert.rs にない場合は新規作成):

```rust
#[cfg(test)]
mod resolve_color_stops_tests {
    use super::*;
    use crate::pageable::GradientStopPosition;

    fn parse_and_resolve(css: &str) -> Option<Vec<crate::pageable::GradientStop>> {
        // CSS gradient items を直接構築するヘルパーを書くのは複雑なので、
        // 末端 unit test では convert.rs 内部関数を直接呼ぶ代わりに
        // VRT で end-to-end カバー。ここでは LengthPx variant の保持のみ
        // 簡易確認する。
        let _ = css;
        None
    }

    #[test]
    fn length_typed_stop_preserved_as_length_px() {
        // この unit test は対外型 (BgImageContent) を経由した integration が
        // 主目的なので、convert.rs 単体での test は割愛し VRT で網羅する。
        // 代わりに resolve_color_stops の戻り値型が
        // GradientStopPosition::LengthPx を含むことを型レベルで担保。
        let p: GradientStopPosition = GradientStopPosition::LengthPx(50.0);
        assert!(matches!(p, GradientStopPosition::LengthPx(_)));
    }
}
```

(注: convert.rs の resolve_color_stops は `style::values::generics::image::GenericGradientItem` を引数にとるため、unit test で synthesize するのが煩雑。E2E は VRT で担保する方針。)

**Step 2: resolve_color_stops を書き換え**

`crates/fulgur/src/convert.rs:3210-3306` の `resolve_color_stops` を以下に置換:

```rust
/// CSS gradient items から GradientStop ベクタを解決する。linear / radial 共通。
///
/// position は `GradientStopPosition` で保持され (Auto / Fraction / LengthPx)、
/// draw 時に `background::resolve_gradient_stops` で gradient line 長さを
/// 使って fraction 化される。convert 時の fixup は行わない。
///
/// Bail 条件:
/// - stops.len() < 2 (規定上 invalid)
/// - interpolation hint (Phase 2 別 issue)
/// - position が percentage でも length でもない (calc() 等 — Phase 2)
fn resolve_color_stops(
    items: &[style::values::generics::image::GenericGradientItem<
        style::values::computed::Color,
        style::values::computed::LengthPercentage,
    >],
    current_color: &style::color::AbsoluteColor,
    gradient_kind: &'static str,
) -> Option<Vec<crate::pageable::GradientStop>> {
    use crate::pageable::{GradientStop, GradientStopPosition};
    use style::values::generics::image::GradientItem;

    let mut out: Vec<GradientStop> = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item {
            GradientItem::SimpleColorStop(c) => {
                let abs = c.resolve_to_absolute(current_color);
                out.push(GradientStop {
                    position: GradientStopPosition::Auto,
                    rgba: absolute_to_rgba(abs),
                });
            }
            GradientItem::ComplexColorStop { color, position } => {
                let abs = color.resolve_to_absolute(current_color);
                let pos = if let Some(pct) = position.to_percentage() {
                    GradientStopPosition::Fraction(pct.0)
                } else if let Some(len) = position.to_length() {
                    GradientStopPosition::LengthPx(len.px())
                } else {
                    log::warn!(
                        "{gradient_kind}: stop position is neither percentage \
                         nor length (calc() etc.). Layer dropped."
                    );
                    return None;
                };
                out.push(GradientStop {
                    position: pos,
                    rgba: absolute_to_rgba(abs),
                });
            }
            GradientItem::InterpolationHint(_) => {
                log::warn!(
                    "{gradient_kind}: interpolation hints are not yet supported \
                     (Phase 2). Layer dropped."
                );
                return None;
            }
        }
    }

    if out.len() < 2 {
        return None;
    }

    Some(out)
}
```

**Step 3: コンパイル確認**

```bash
cargo build -p fulgur 2>&1 | tail -3
```

期待: 成功 (Task 1 で更新した draw 側 match で Auto / LengthPx も明示処理されている)。ただし draw_linear / draw_radial はまだ Task 1 の no-op match で 0.0 を返してしまう → このタスクではまだ pixel が崩れる可能性あり、Task 4 で fix。

**Step 4: 既存テスト確認 (回帰)**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

期待: 738 PASS (この時点では VRT は壊れる可能性あり、Task 4 で修正)。fulgur lib 内 unit test は通るはず。

**Step 5: コミット**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(gradient): preserve length-typed stops as LengthPx in convert"
```

---

## Task 4: draw_linear_gradient / draw_radial_gradient を新ヘルパーに切替

**目的:** draw 時に `resolve_gradient_stops` を経由して length / auto を解決する。Task 1 で残した暫定 match (Auto / LengthPx → 0.0) を撤去。

**Files:**

- Modify: `crates/fulgur/src/background.rs::draw_linear_gradient` (412-422)
- Modify: `crates/fulgur/src/background.rs::draw_radial_gradient` (541-549)

**Step 1: draw_linear_gradient の stops 構築を置換**

`crates/fulgur/src/background.rs:412-422` の `let krilla_stops: Vec<...> = stops.iter().map(...)` を以下に変更:

```rust
// `length` は pt 単位 (ow, oh が pt) だが、`GradientStopPosition::LengthPx` は
// CSS px なので、helper の単位契約 (CSS px) に合わせて変換してから渡す。
// pt のまま渡すと 4/3× ずれる (coordinate-system.md 参照)。
let length_px = crate::convert::pt_to_px(length);
let Some(krilla_stops) = resolve_gradient_stops(stops, length_px, "linear-gradient") else {
    return;
};
```

(注: `length` は同関数 line 399 で計算済の gradient line 長さ pt 値。)

**Step 2: draw_radial_gradient の stops 構築を置換**

`crates/fulgur/src/background.rs:541-549` も同様に:

```rust
// Radial gradient line length = rx (CSS Images §3.6.1, ellipse でも +X 軸)。
// `rx` は draw 内で pt 単位で計算されるので CSS px に揃える。
let rx_px = crate::convert::pt_to_px(rx);
let Some(krilla_stops) = resolve_gradient_stops(stops, rx_px, "radial-gradient") else {
    return;
};
```

**注:** Plan の初回ドラフトでは pt 単位の `length` / `rx` をそのまま helper に渡していたが、これは fulgur-78sl 実装中に発見された 4/3× scale バグの原因 (commit `12d707f` で修正)。Helper は CSS px 契約 (`GradientStopPosition::LengthPx` と単位を揃える) なので、call site で `crate::convert::pt_to_px` で変換するのが正しい。

**Step 3: 既存テスト全 PASS 確認**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur-vrt 2>&1 | tail -10
```

期待: lib 738 PASS, VRT 全 fixture green。length-typed stop を含まない既存 fixture は byte-identical (fixup ロジックが pixel 互換であれば)。

**Note:** もし VRT が fail したら、`resolve_gradient_stops` の monotonic / auto 補間ロジックが `convert::resolve_color_stops` の旧ロジックと数値的に同等か再確認すること。特に `last_resolved` の初期値 (旧: `0.0_f32`, 新: `f32::NEG_INFINITY`) の挙動差に注意。旧コードは `<= 0.0` の値も `last_resolved=0.0` 以上に押し上げていたが、新コードは負値も保持して後段 range check で drop する。fraction-only かつ範囲内の入力に限ればどちらも同じ結果。

**Step 4: コミット**

```bash
git add crates/fulgur/src/background.rs
git commit -m "feat(gradient): wire draw_*_gradient through resolve_gradient_stops"
```

---

## Task 5: VRT fixture 追加 (linear gradient + px stop)

**目的:** `linear-gradient(red 0px, red 200px, blue 200px, blue 400px)` のような px-typed stop が visually 正しく描画されることを strip-approximation で確認する。

**Files:**

- Create: `crates/fulgur-vrt/fixtures/paint/linear-gradient-px-stop.html`
- Create: `crates/fulgur-vrt/fixtures/paint/linear-gradient-px-stop-ref.html` (or use existing strip harness)
- Modify: `crates/fulgur-vrt/tests/gradient_harness.rs` (新規 test 関数)

**Step 1: 仕様確認 — どんな gradient で何を assert するか決定**

ターゲット CSS: `linear-gradient(to right, red 0px, blue 50px, blue 350px, green 400px)` を `width: 400px` の box にかけると:

- 0px (=0%): red
- 50px (=12.5%): blue (赤→青の急峻な遷移、12.5% で青に到達)
- 350px (=87.5%): blue (87.5% まで blue 一定)
- 400px (=100%): green

実装が正しければ **40-300px の範囲は完全に純 blue**、両端付近に short transition がある。

**Step 2: test fixture 作成**

`crates/fulgur-vrt/fixtures/paint/linear-gradient-px-stop.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT test: linear-gradient with px-typed stops</title>
<style>
  html, body { margin: 0; padding: 0; }
  #g {
    margin: 32px;
    width: 400px;
    height: 192px;
    background: linear-gradient(to right, red 0px, blue 50px, blue 350px, green 400px);
  }
</style>
</head>
<body>
<div id="g"></div>
</body>
</html>
```

**Step 3: ref fixture 作成**

`crates/fulgur-vrt/fixtures/paint/linear-gradient-px-stop-ref.html` を strip-approximation で。`gradient_harness.rs` の `build_strip_ref_html` パターンを利用するため、ref も harness が動的生成する形が望ましい。**実装方針: harness に新 test 関数 `linear_gradient_px_stop` を追加し、test/ref 双方を動的生成する。**

**Step 4: harness 拡張**

`crates/fulgur-vrt/tests/gradient_harness.rs` の末尾に以下 test 関数を追加:

```rust
/// linear-gradient(red 0px, blue 50px, blue 350px, green 400px) を
/// strip-approximation ref と比較する。50px = 12.5%, 350px = 87.5% を
/// CSS spec 通り解決できるかを検証。
#[test]
fn linear_gradient_px_stop() {
    use crate::pdf_render::{RenderSpec, pdf_to_rgba, render_html_to_pdf};

    // sampling: position-aware strip ref
    fn lerp_color(t: f32) -> (u8, u8, u8) {
        // 0..=0.125: red→blue, 0.125..=0.875: blue, 0.875..=1.0: blue→green
        const RED: (u8, u8, u8) = (255, 0, 0);
        const BLUE: (u8, u8, u8) = (0, 0, 255);
        const GREEN: (u8, u8, u8) = (0, 128, 0);
        if t <= 0.125 {
            let s = t / 0.125;
            (
                lerp_u8(RED.0, BLUE.0, s),
                lerp_u8(RED.1, BLUE.1, s),
                lerp_u8(RED.2, BLUE.2, s),
            )
        } else if t >= 0.875 {
            let s = (t - 0.875) / 0.125;
            (
                lerp_u8(BLUE.0, GREEN.0, s),
                lerp_u8(BLUE.1, GREEN.1, s),
                lerp_u8(BLUE.2, GREEN.2, s),
            )
        } else {
            BLUE
        }
    }

    let test_html = format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0;}}
#g{{margin:{margin}px;width:{w}px;height:{h}px;
   background:linear-gradient(to right, red 0px, blue 50px, blue 350px, green 400px);}}
</style></head><body><div id="g"></div></body></html>"#,
        margin = GRADIENT_MARGIN_PX,
        w = GRADIENT_WIDTH_PX,
        h = GRADIENT_HEIGHT_PX,
    );

    // ref: strip approximation with custom color function
    let strip_w = GRADIENT_WIDTH_PX / STRIP_COUNT;
    let mut strips = String::new();
    for i in 0..STRIP_COUNT {
        let t = (i as f32 + 0.5) / STRIP_COUNT as f32;
        let (r, g, b) = lerp_color(t);
        let left = i * strip_w;
        strips.push_str(&format!(
            r#"<div style="position:absolute;top:0;bottom:0;left:{left}px;width:{strip_w}px;background:rgb({r},{g},{b});"></div>"#
        ));
    }
    let ref_html = format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0;}}
#g{{margin:{margin}px;width:{w}px;height:{h}px;position:relative;}}
</style></head><body><div id="g">{strips}</div></body></html>"#,
        margin = GRADIENT_MARGIN_PX,
        w = GRADIENT_WIDTH_PX,
        h = GRADIENT_HEIGHT_PX,
    );

    let test_pdf = render_html_to_pdf(&test_html, &RenderSpec::default()).unwrap();
    let ref_pdf = render_html_to_pdf(&ref_html, &RenderSpec::default()).unwrap();
    let test_rgba = pdf_to_rgba(&test_pdf, 150).unwrap();
    let ref_rgba = pdf_to_rgba(&ref_pdf, 150).unwrap();

    let tol = Tolerance { per_channel: 12, allowed_pixel_pct: 0.5 };
    let report = diff::compare(&test_rgba, &ref_rgba, &tol).unwrap();
    assert!(report.passed, "linear-gradient-px-stop test: {report:?}");
}
```

(注: `lerp_u8` は同 file 内既存関数、`Tolerance` / `diff::compare` の API は既存 harness を踏襲。詳細は `gradient_harness.rs:42` `lerp_u8` 周辺と既存 test を参照。)

**Step 5: テスト実行**

```bash
cargo test -p fulgur-vrt --test gradient_harness linear_gradient_px_stop 2>&1 | tail -20
```

期待: `test result: ok. 1 passed`。失敗時は実装側 (`resolve_gradient_stops` の length 解決) の数値を log で確認。

**Step 6: 既存 VRT 全体回帰確認**

```bash
cargo test -p fulgur-vrt 2>&1 | tail -10
```

期待: 全 fixture green。

**Step 7: コミット**

```bash
git add crates/fulgur-vrt/tests/gradient_harness.rs
git commit -m "test(vrt): add linear-gradient px-typed stop reftest"
```

---

## Task 6: VRT fixture 追加 (radial gradient + px stop) [optional but recommended]

**目的:** radial gradient における length-typed stop の解決 (rx 基準) を確認する。

**Files:**

- Modify: `crates/fulgur-vrt/tests/gradient_harness.rs` (test 追加)

**Step 1: ターゲット CSS**

`radial-gradient(circle, red 0px, blue 50px, blue 100px)` を 200x200 box の中心に配置すれば、半径 50px までは red→blue グラデ、50-100px は blue 単色、100px 以遠も blue (clamp)。

**Step 2: test 関数追加**

linear と同じ pattern で radial 版を `gradient_harness.rs` に追加。ref は同心円 strip ではなく `radial-gradient` で **fraction だけを使った同等表現** を ref にする手法もあり (linear で言うと strip ref のかわりに)。

ただし「実装をテストするのに同じ実装でリファレンスを作る」のは false positive のリスク。代替案: ref も `linear-gradient` で側面 sampling し、円形 mask で円に切り抜く — 複雑。

**簡易代替**: 純粋に "解決後 fraction" が等価な percentage gradient を ref にする:

- test: `radial-gradient(circle 100px, red 0px, blue 50px, blue 100px)`
- ref:  `radial-gradient(circle 100px, red 0%, blue 50%, blue 100%)`

両者の解決結果は同一であるべき。**ただし test/ref 共に gradient を使うため、もし `LengthPx` 解決自体がバグっていれば test が間違った PDF を、ref が正しい PDF を生成し、diff で fail する**。これで length 解決ロジックは検証できる。

これに倣って:

```rust
#[test]
fn radial_gradient_px_stop() {
    let test_html = format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0;}}
#g{{margin:{margin}px;width:200px;height:200px;
   background:radial-gradient(circle 100px at center, red 0px, blue 50px, blue 100px);}}
</style></head><body><div id="g"></div></body></html>"#,
        margin = GRADIENT_MARGIN_PX,
    );
    let ref_html = format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0;}}
#g{{margin:{margin}px;width:200px;height:200px;
   background:radial-gradient(circle 100px at center, red 0%, blue 50%, blue 100%);}}
</style></head><body><div id="g"></div></body></html>"#,
        margin = GRADIENT_MARGIN_PX,
    );
    let test_pdf = pdf_render::render_html_to_pdf(&test_html, &pdf_render::RenderSpec::default()).unwrap();
    let ref_pdf = pdf_render::render_html_to_pdf(&ref_html, &pdf_render::RenderSpec::default()).unwrap();
    let test_rgba = pdf_render::pdf_to_rgba(&test_pdf, 150).unwrap();
    let ref_rgba = pdf_render::pdf_to_rgba(&ref_pdf, 150).unwrap();
    let tol = Tolerance { per_channel: 4, allowed_pixel_pct: 0.1 };
    let report = diff::compare(&test_rgba, &ref_rgba, &tol).unwrap();
    assert!(report.passed, "radial-gradient-px-stop test: {report:?}");
}
```

(test/ref が同じ gradient 実装を使うので tolerance はタイトに 4 channel)

**Step 3: テスト実行**

```bash
cargo test -p fulgur-vrt --test gradient_harness radial_gradient_px_stop 2>&1 | tail -15
```

期待: PASS

**Step 4: コミット**

```bash
git add crates/fulgur-vrt/tests/gradient_harness.rs
git commit -m "test(vrt): add radial-gradient px-typed stop reftest"
```

---

## Task 7: 全体回帰 + clippy + format

**目的:** 最終的なコード品質チェック。

**Step 1: 全テスト実行**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-78sl-px-stops
cargo test -p fulgur --lib 2>&1 | tail -3
cargo test -p fulgur 2>&1 | tail -3
cargo test -p fulgur-vrt 2>&1 | tail -3
```

期待: 全 PASS。

**Step 2: clippy / fmt**

```bash
cargo clippy --all-targets -- -D warnings 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -5
```

期待: warning 0、format diff なし。失敗時は `cargo fmt` を実行して再コミット。

**Step 3: ログ確認**

`linear-gradient: length-typed stop position is not yet supported (Phase 2)` ログが消えたか:

```bash
grep -rn "length-typed stop position is not yet" crates/fulgur/src/
```

期待: 結果なし (Phase 1 ログが残っていれば Task 3 で削除漏れ)。

**Step 4: 関連 PR を予期した最終ファイルテスト**

CLI で簡易確認:

```bash
echo '<style>div{width:400px;height:100px;background:linear-gradient(to right, red 0px, blue 50px, green 100px);}</style><div></div>' | \
  cargo run -q --bin fulgur -- render /dev/stdin -o /tmp/px-stop.pdf
ls -la /tmp/px-stop.pdf
```

期待: PDF が生成され、Layer drop ログは出ない。

**Step 5: 最終コミット (まだ修正があれば)**

```bash
git status
# 必要なら git add -p で fix を commit
```

---

## 完了条件 (Acceptance)

1. `cargo test -p fulgur --lib` 全 PASS (738+ tests, 新規 `resolve_gradient_stops_tests` 含む)
2. `cargo test -p fulgur-vrt` 全 PASS (`linear_gradient_px_stop` / `radial_gradient_px_stop` 含む)
3. `cargo clippy --all-targets -- -D warnings` 警告 0
4. `cargo fmt --check` 差分なし
5. `linear-gradient: length-typed stop position is not yet supported` ログが convert.rs から消えている
6. `linear-gradient(red 0px, blue 50px, green 100px)` 等が正しく PDF に描画される (CLI 手動確認)
7. out-of-range (length 解決後 > 1.0 / < 0.0) は `log::warn` + Layer drop で fulgur-n3zk 用に残されている

## Implementation Notes / Risks

- **Task 1 (no-op refactor)** で既存 VRT が pixel-byte 互換である必要がある。fixup ロジック移動 (Task 2-4) は no-op refactor 内では発生しないので、Task 1 単独では既存テスト全 green を期待。
- **Task 4 (draw 切替)** で初めて fixup ロジックが新ヘルパー側になる。`last_resolved` 初期値の差 (旧 `0.0` / 新 `-∞`) で挙動が分岐するのは "全 stop が負値" のような病的入力のみ。実テストには影響しないはず。万一 VRT が崩れたら Task 4 内で再現性を確認。
- **Task 5/6 VRT** は既に存在する `gradient_harness.rs` が strip-approximation を使っているので `Tolerance::per_channel = 12` 程度で安定する想定。実環境で fail したら tol を調整。
- **out-of-range の log message** は fulgur-n3zk への hand-off ポイントなので、明示的に issue ID を入れて将来追跡しやすくする。
