# Conic-gradient Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS `conic-gradient(...)` / `repeating-conic-gradient(...)` を Krilla の `SweepGradient` 経由で PDF に描画する。Phase 1 (linear) / Phase 2 (radial) と同じく、サポート外の機能は `log::warn!` + `None` 返却で layer drop。

**Architecture:** `convert.rs` で Stylo の `Gradient::Conic` から `BgImageContent::ConicGradient` を生成し、`background.rs::draw_conic_gradient` が krilla `SweepGradient` を発行する。CSS の角度規約 (0deg=top, CW) と krilla の sweep 規約 (0deg=right, 増加方向は要検証) の差は draw 段階の角度変換で吸収。

**Tech Stack:** Stylo (`Gradient::Conic`) / krilla 0.7 (`SweepGradient`) / fulgur-vrt (test↔ref pixel diff harness)

**Issue:** `fulgur-m92h` (parent epic: `fulgur-5166`)

**PDF/A 注記:** krilla `SweepGradient` は内部で PostScript Type 4 function shading を発行 → PDF/A-1, A-2 非適合。本 PR ではフラグ判定をしない。代替経路は `fulgur-batz` (PDF/A-safe fallback) で扱う。

---

## Task 1: lib smoke test を先に書く (failing baseline)

**Files:**

- Modify: `crates/fulgur/tests/render_smoke.rs` (末尾に追記)

**Step 1: failing test を書く**

```rust
#[test]
fn test_render_html_conic_gradient_basic() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;
            background:conic-gradient(red, yellow, green, blue, red);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render conic-gradient");
    assert!(!pdf.is_empty(), "PDF should not be empty");
    // Phase 1 (silent fallback) では gradient が出ないため、
    // 本 PR 完了後はこのテストが通り、conic 経路が draw されることを保証する。
}

#[test]
fn test_render_html_repeating_conic_gradient() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;
            background:repeating-conic-gradient(red 0deg, blue 30deg);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render repeating-conic-gradient");
    assert!(!pdf.is_empty());
}
```

**Step 2: 実行して fail を確認**

```bash
cargo test -p fulgur --test render_smoke test_render_html_conic_gradient_basic
cargo test -p fulgur --test render_smoke test_render_html_repeating_conic_gradient
```

期待: 両方 `assert!(!pdf.is_empty())` の手前で render 自体は成功するはずだが、本 PR 完了後にデータパスを通って実際に描画されることを以後のタスクで担保する。

**Step 3: commit**

```bash
git add crates/fulgur/tests/render_smoke.rs
git commit -m "test(gradient): add lib smoke for conic-gradient (failing baseline)"
```

---

## Task 2: pageable.rs にデータモデルを追加

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (`BgImageContent` enum 周辺、行 819 付近)

**Step 1: 新型を追加**

`pageable.rs` の `RadialGradient` variant の後に挿入:

```rust
/// CSS conic-gradient stop の位置指定。
///
/// CSS Images Level 4 §3.x: stop position は `<angle> | <percentage>`。
/// `<percentage>` は 360deg を 100% として解釈する。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConicStopPosition {
    /// 位置省略 — 周囲の固定 stop から等間隔補間で fixup される。
    Auto,
    /// `<percentage>` を [0.0, 1.0] の fraction として保持。
    /// CSS では負値や 1.0 超過もあり得る (out-of-range) — fixup で扱う。
    Fraction(f32),
    /// `<angle>` を radians で保持。draw 時に `angle / 2π` で fraction 化。
    AngleRad(f32),
}

#[derive(Clone, Copy, Debug)]
pub struct ConicGradientStop {
    pub position: ConicStopPosition,
    pub rgba: [u8; 4],
}
```

`BgImageContent` enum の `RadialGradient` の後に variant を追加:

```rust
/// CSS `conic-gradient(...)` / `repeating-conic-gradient(...)`.
/// `from_angle` は radians (CSS 規約: 0=top, 増加方向=CW)。
/// `repeating=true` の場合、period = (last_stop_fraction - first_stop_fraction) で
/// `[0, 1]` を覆うだけ stop 列を平行コピーする (CSS Images 4 §3.x)。
ConicGradient {
    from_angle: f32,
    position_x: BgLengthPercentage,
    position_y: BgLengthPercentage,
    stops: Vec<ConicGradientStop>,
    repeating: bool,
},
```

**Step 2: コンパイル確認**

```bash
cargo build -p fulgur --lib
```

期待: warning は出る (未使用 variant) が build は通る。

**Step 3: commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(gradient): add ConicGradient variant and stop types to BgImageContent"
```

---

## Task 3: convert.rs に `resolve_conic_gradient` を追加

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (`Gradient::Conic { .. } => None` の置き換え + 新関数)

**Step 1: dispatcher 行を変更**

行 3045 付近の match を:

```rust
Image::Gradient(g) => {
    use style::values::computed::image::Gradient;
    match g.as_ref() {
        Gradient::Linear { .. } => resolve_linear_gradient(g, &current_color),
        Gradient::Radial { .. } => resolve_radial_gradient(g, &current_color),
        Gradient::Conic  { .. } => resolve_conic_gradient(g, &current_color),
    }
}
```

**Step 2: `resolve_conic_gradient` を追加**

`resolve_color_stops` の前後に新関数を追加:

```rust
/// Convert a Stylo computed `Gradient::Conic` into `BgImageContent::ConicGradient`.
///
/// 失敗 (None) 条件:
/// - 非デフォルト color_interpolation_method (例: `in oklch`)
/// - InterpolationHint
/// - position の calc() (Length にも Percentage にも resolve できない)
/// - stops < 2
fn resolve_conic_gradient(
    g: &style::values::computed::Gradient,
    current_color: &style::color::AbsoluteColor,
) -> Option<(BgImageContent, f32, f32)> {
    use crate::pageable::BgLengthPercentage;
    use style::values::computed::image::Gradient;
    use style::values::generics::image::GradientFlags;

    let (angle, position, items, flags) = match g {
        Gradient::Conic {
            angle,
            position,
            items,
            flags,
            ..
        } => (angle, position, items, flags),
        Gradient::Linear { .. } | Gradient::Radial { .. } => return None,
    };

    if !flags.contains(GradientFlags::HAS_DEFAULT_COLOR_INTERPOLATION_METHOD) {
        log::warn!(
            "conic-gradient: non-default color-interpolation-method is not yet \
             supported (Phase 2). Layer dropped."
        );
        return None;
    }

    // CSS `from <angle>` を radians に変換。Stylo の Angle は内部 deg なので
    // .radians() を使う (linear と同じ流儀)。
    let from_angle = angle.radians();

    // Center position は radial と同じヘルパ。calc() は None で bail。
    let position_x = try_convert_lp_to_bg(&position.horizontal)?;
    let position_y = try_convert_lp_to_bg(&position.vertical)?;

    let repeating = flags.contains(GradientFlags::REPEATING);

    let stops = resolve_conic_color_stops(items, current_color)?;

    Some((
        BgImageContent::ConicGradient {
            from_angle,
            position_x,
            position_y,
            stops,
            repeating,
        },
        0.0,
        0.0,
    ))
}

/// Resolve conic-gradient color stops into `Vec<ConicGradientStop>`.
///
/// linear/radial の `resolve_color_stops` は `LengthPercentage` 用なので
/// conic の `AngleOrPercentage` には別実装が必要。
fn resolve_conic_color_stops(
    items: &[style::values::generics::image::GenericGradientItem<
        style::values::computed::Color,
        style::values::computed::AngleOrPercentage,
    >],
    current_color: &style::color::AbsoluteColor,
) -> Option<Vec<crate::pageable::ConicGradientStop>> {
    use crate::pageable::{ConicGradientStop, ConicStopPosition};
    use style::values::computed::AngleOrPercentage;
    use style::values::generics::image::GradientItem;

    let mut out: Vec<ConicGradientStop> = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item {
            GradientItem::SimpleColorStop(c) => {
                let abs = c.resolve_to_absolute(current_color);
                out.push(ConicGradientStop {
                    position: ConicStopPosition::Auto,
                    rgba: absolute_to_rgba(abs),
                });
            }
            GradientItem::ComplexColorStop { color, position } => {
                let abs = color.resolve_to_absolute(current_color);
                let pos = match position {
                    AngleOrPercentage::Angle(a) => ConicStopPosition::AngleRad(a.radians()),
                    AngleOrPercentage::Percentage(p) => ConicStopPosition::Fraction(p.0),
                };
                out.push(ConicGradientStop {
                    position: pos,
                    rgba: absolute_to_rgba(abs),
                });
            }
            GradientItem::InterpolationHint(_) => {
                log::warn!(
                    "conic-gradient: interpolation hints are not yet supported \
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

**注意:** `try_convert_lp_to_bg` は `convert.rs:3406` 付近に radial 用の
helper として存在する (calc() は None を返す)。新規にヘルパを切り出さず流用すること。

**Step 3: コンパイル確認**

```bash
cargo build -p fulgur --lib
```

期待: build 通る。warning は減るはず。

**Step 4: commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(gradient): resolve Stylo Gradient::Conic into BgImageContent::ConicGradient"
```

---

## Task 4: background.rs に no-op draw 経路を追加 (skeleton)

これは draw を実装する前に、dispatch だけ通して E2E が通る形に仮置きする
中間 step。次のタスクで実体を入れる。

**Files:**

- Modify: `crates/fulgur/src/background.rs` (行 208 と行 238 の match arm)

**Step 1: image-size 判定に ConicGradient を含める**

行 208:

```rust
let (img_w, img_h) = match &layer.content {
    BgImageContent::LinearGradient { .. }
    | BgImageContent::RadialGradient { .. }
    | BgImageContent::ConicGradient { .. } => {
        resolve_gradient_size(&layer.size, ow, oh)
    }
    BgImageContent::Raster { .. } | BgImageContent::Svg { .. } => resolve_size(layer, ow, oh),
};
```

**Step 2: 主要 match arm に no-op skeleton を追加**

行 344 (Raster の前) に挿入:

```rust
BgImageContent::ConicGradient {
    from_angle,
    position_x,
    position_y,
    stops,
    repeating,
} => {
    // Linear/Radial と同じ uniform-tile ショートカットは Phase 1 では入れず、
    // 必ず per-tile loop で draw する (conic 自体が初回実装で複雑なため)。
    for (tx, ty, tw, th) in &tiles {
        draw_conic_gradient(
            canvas.surface,
            *from_angle,
            position_x,
            position_y,
            stops,
            *repeating,
            *tx,
            *ty,
            *tw,
            *th,
        );
    }
}
```

そして同ファイル末尾 (行 760 付近、`draw_radial_gradient` の後) に stub:

```rust
/// Phase 1 stub. 実体は次のタスクで実装する。
#[allow(clippy::too_many_arguments)]
fn draw_conic_gradient(
    _surface: &mut krilla::surface::Surface<'_>,
    _from_angle_rad: f32,
    _position_x: &BgLengthPercentage,
    _position_y: &BgLengthPercentage,
    _stops: &[crate::pageable::ConicGradientStop],
    _repeating: bool,
    _ox: f32,
    _oy: f32,
    _ow: f32,
    _oh: f32,
) {
    // TODO(fulgur-m92h): wire krilla::paint::SweepGradient
}
```

**Step 3: コンパイル + lib smoke を確認**

```bash
cargo build -p fulgur --lib
cargo test -p fulgur --test render_smoke test_render_html_conic_gradient_basic
cargo test -p fulgur --test render_smoke test_render_html_repeating_conic_gradient
```

期待: build 通る。smoke はまだ「何も描画しない」が `pdf.is_empty()` ではないので pass する。
(背景以外の構造で PDF stream は生成されるため)。

**Step 4: commit**

```bash
git add crates/fulgur/src/background.rs
git commit -m "feat(gradient): add ConicGradient dispatch skeleton in draw_background_layer"
```

---

## Task 5: 4 象限 VRT harness で direction 検証 (TDD)

ここが本 PR で最も慎重に進めるべき箇所。krilla SweepGradient の角度規約を
empirical に確定する。

**Files:**

- Create: `crates/fulgur-vrt/tests/conic_gradient_harness.rs`
- Create: `crates/fulgur-vrt/fixtures/paint/conic-gradient-quadrant.html`
- Modify: `crates/fulgur-vrt/Cargo.toml` (test 自動検出されるので変更不要のはず)

**Step 1: test fixture HTML を作成**

`crates/fulgur-vrt/fixtures/paint/conic-gradient-quadrant.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT test: conic-gradient 4 quadrants</title>
<style>
  html, body { margin: 0; padding: 0; background: white; }
  .box {
    width: 192px;
    height: 192px;
    margin: 32px;
    background: conic-gradient(
      red 0deg,
      red 90deg,
      yellow 90deg,
      yellow 180deg,
      green 180deg,
      green 270deg,
      blue 270deg,
      blue 360deg
    );
  }
</style>
</head>
<body>
  <div class="box"></div>
</body>
</html>
```

(連続した同色 stop で 4 セクターを離散化 — gradient 補間ではなく境界の鮮明な
4 色塗りになる。ref と pixel-perfect 一致させやすい)

**Step 2: ref builder を含む harness を作成**

```rust
//! Test↔ref pixel-diff harness for conic-gradient implementation.
//!
//! 4 個の 90° セクター fixture を、CSS `clip-path: polygon(...)` で扇形を
//! 作った 4 個の絶対配置 div に置き換えた ref と pixel-wise 比較する。
//! conic-gradient の角度規約 (0deg=top, CW) を empirical に検証する。
//!
//! 採用しなかった案:
//! - SVG `<linearGradient>` 集合 ref → 自前 SVG パイプラインに依存
//! - PNG raster ref → CI 再現性で扱いづらい

use fulgur_vrt::diff::{self};
use fulgur_vrt::manifest::Tolerance;
use fulgur_vrt::pdf_render::{RenderSpec, pdf_to_rgba, render_html_to_pdf};
use std::fs;
use std::path::PathBuf;

const BOX_PX: u32 = 192;
const MARGIN_PX: u32 = 32;

fn build_quadrant_ref_html() -> String {
    // 4 sectors. `clip-path: polygon()` の頂点は box の外周 4 辺と中心 (96, 96)。
    // CSS conic-gradient(red 0deg, red 90deg, ...) の規約:
    //   - 0deg = box の上端中央
    //   - 増加方向 = clockwise (画面視点)
    //   - red sector = 0..90deg = top → right (右上 1/4)
    //   - yellow sector = 90..180deg = right → bottom (右下 1/4)
    //   - green sector = 180..270deg = bottom → left (左下 1/4)
    //   - blue sector = 270..360deg = left → top (左上 1/4)
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>VRT ref: conic-gradient 4 quadrants</title>
<style>
  html, body {{ margin: 0; padding: 0; background: white; }}
  .box {{ position: relative; width: {w}px; height: {h}px; margin: {m}px; }}
  .sector {{ position: absolute; inset: 0; }}
  .red    {{ background: red;    clip-path: polygon(50% 50%, 50%   0%, 100%   0%, 100% 50%); }}
  .yellow {{ background: yellow; clip-path: polygon(50% 50%, 100% 50%, 100% 100%,  50% 100%); }}
  .green  {{ background: green;  clip-path: polygon(50% 50%,  50% 100%,   0% 100%,   0% 50%); }}
  .blue   {{ background: blue;   clip-path: polygon(50% 50%,   0% 50%,   0%   0%,  50%   0%); }}
</style>
</head>
<body>
  <div class="box">
    <div class="sector red"></div>
    <div class="sector yellow"></div>
    <div class="sector green"></div>
    <div class="sector blue"></div>
  </div>
</body>
</html>"#,
        w = BOX_PX, h = BOX_PX, m = MARGIN_PX,
    )
}

#[test]
fn conic_gradient_4_quadrants_match_polygon_reference() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let test_html_path = crate_root.join("fixtures/paint/conic-gradient-quadrant.html");
    let test_html = fs::read_to_string(&test_html_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_html_path.display()));

    let ref_html = build_quadrant_ref_html();

    let spec = RenderSpec { page_size: "A4", margin_pt: Some(0.0), dpi: 150 };

    let test_pdf = render_html_to_pdf(&test_html, spec).expect("render test pdf");
    let ref_pdf = render_html_to_pdf(&ref_html, spec).expect("render ref pdf");

    let work_dir = crate_root.parent().and_then(|p| p.parent())
        .expect("workspace root")
        .join("target/vrt-conic-gradient-harness");
    fs::create_dir_all(&work_dir).expect("create work dir");
    let test_dir = work_dir.join("test");
    let ref_dir = work_dir.join("ref");
    let _ = fs::remove_dir_all(&test_dir);
    let _ = fs::remove_dir_all(&ref_dir);
    fs::create_dir_all(&test_dir).expect("create test dir");
    fs::create_dir_all(&ref_dir).expect("create ref dir");

    let test_png = pdf_to_rgba(&test_pdf, &test_dir).expect("rasterize test pdf");
    let ref_png = pdf_to_rgba(&ref_pdf, &ref_dir).expect("rasterize ref pdf");

    // セクター境界の AA 1〜2 px を考慮し radial と同レベルの tolerance:
    let tolerance = Tolerance { max_pixel_diff: 12.0, max_diff_ratio: 0.02 };

    diff::assert_images_match(&test_png, &ref_png, &tolerance, &work_dir)
        .expect("conic-gradient quadrants should match polygon ref");
}
```

(`diff` / `Tolerance` / `pdf_render` の正確な API は `radial_gradient_harness.rs`
からコピーして参照する)

**Step 3: 実行して fail を確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt --test conic_gradient_harness
```

期待: test fixture 側は draw stub で何も塗らないので白地、ref は 4 色 → fail。

**Step 4: commit**

```bash
git add crates/fulgur-vrt/tests/conic_gradient_harness.rs \
        crates/fulgur-vrt/fixtures/paint/conic-gradient-quadrant.html
git commit -m "test(gradient): add conic-gradient 4-quadrant VRT harness (failing baseline)"
```

---

## Task 6: `draw_conic_gradient` 実体を実装 (krilla SweepGradient)

**Files:**

- Modify: `crates/fulgur/src/background.rs` (Task 4 で置いた stub)

**Step 1: 実装 — まずは想定マッピング**

```rust
#[allow(clippy::too_many_arguments)]
fn draw_conic_gradient(
    surface: &mut krilla::surface::Surface<'_>,
    from_angle_rad: f32,
    position_x: &BgLengthPercentage,
    position_y: &BgLengthPercentage,
    stops: &[crate::pageable::ConicGradientStop],
    repeating: bool,
    ox: f32, oy: f32, ow: f32, oh: f32,
) {
    use crate::pageable::ConicStopPosition;

    if ow <= 0.0 || oh <= 0.0 || stops.len() < 2 {
        return;
    }

    let cx = ox + resolve_point(position_x, ow);
    let cy = oy + resolve_point(position_y, oh);

    // Stop position を fraction (0..1) に正規化:
    //   Auto      → そのまま (後段 fixup で埋まる)
    //   Fraction  → そのまま
    //   AngleRad  → angle_rad / (2π)
    let normalized: Vec<(Option<f32>, [u8; 4])> = stops
        .iter()
        .map(|s| {
            let frac = match s.position {
                ConicStopPosition::Auto => None,
                ConicStopPosition::Fraction(f) => Some(f),
                ConicStopPosition::AngleRad(a) => Some(a / (2.0 * std::f32::consts::PI)),
            };
            (frac, s.rgba)
        })
        .collect();

    // CSS Images Level 4 §3.x stop fixup:
    //   1. 先頭 Auto は 0.0、末尾 Auto は 1.0 に確定
    //   2. monotonic clamp (前 fixed より小さければ前 fixed に合わせる)
    //   3. 中間 Auto 群を前後 fixed の等間隔補間で埋める
    let fixed = fixup_conic_stops(normalized);
    if fixed.len() < 2 {
        return;
    }

    let first_pos = fixed.first().unwrap().0;
    let last_pos = fixed.last().unwrap().0;
    let period = (last_pos - first_pos).max(f32::EPSILON);

    // 角度変換 (CSS → krilla):
    //   - CSS: 0deg = top, 増加方向 = CW (screen view)
    //   - krilla SweepGradient: 0deg = right, 増加方向は内部 PostScript Atan に依存
    //   - 想定: krilla_start_deg = 90 - css_from_deg、sweep は -360deg 方向
    let from_angle_deg = from_angle_rad.to_degrees();
    let krilla_start_deg = 90.0 - from_angle_deg - first_pos * 360.0;
    let krilla_end_deg = if repeating {
        krilla_start_deg - period * 360.0
    } else {
        krilla_start_deg - 360.0
    };

    let spread = if repeating {
        krilla::paint::SpreadMethod::Repeat
    } else {
        krilla::paint::SpreadMethod::Pad
    };

    // krilla Stop 列を構築。fraction を [0, 1] に再正規化:
    //   p_norm = (p - first) / period
    let krilla_stops: Vec<krilla::paint::Stop> = fixed.iter().map(|(p, rgba)| {
        let p_norm = (p - first_pos) / period;
        krilla::paint::Stop {
            offset: krilla::num::NormalizedF32::new(p_norm.clamp(0.0, 1.0)).unwrap(),
            color: krilla::color::rgb::Rgb::new(rgba[0], rgba[1], rgba[2]).into(),
            opacity: krilla::num::NormalizedF32::new(rgba[3] as f32 / 255.0).unwrap(),
        }
    }).collect();

    let sweep = krilla::paint::SweepGradient {
        cx,
        cy,
        start_angle: krilla_start_deg,
        end_angle: krilla_end_deg,
        transform: krilla::geom::Transform::default(),
        spread_method: spread,
        stops: krilla_stops,
        anti_alias: false,
    };

    surface.set_fill(Some(krilla::paint::Fill {
        paint: sweep.into(),
        rule: Default::default(),
        opacity: krilla::num::NormalizedF32::ONE,
    }));
    surface.set_stroke(None);

    let Some(rect_path) = build_rect_path(ox, oy, ow, oh) else {
        surface.set_fill(None);
        return;
    };
    surface.draw_path(&rect_path);
    surface.set_fill(None);
}

/// CSS Images Level 4 stop fixup for conic gradients.
fn fixup_conic_stops(stops: Vec<(Option<f32>, [u8; 4])>) -> Vec<(f32, [u8; 4])> {
    if stops.is_empty() {
        return Vec::new();
    }
    let mut fixed: Vec<(Option<f32>, [u8; 4])> = stops;

    // 1. 先頭/末尾の Auto を確定
    if fixed[0].0.is_none() { fixed[0].0 = Some(0.0); }
    let last = fixed.len() - 1;
    if fixed[last].0.is_none() { fixed[last].0 = Some(1.0); }

    // 2. monotonic clamp (前向きスキャンで前 fixed >= 現値なら前に合わせる)
    let mut prev: f32 = fixed[0].0.unwrap();
    for s in fixed.iter_mut().skip(1) {
        if let Some(p) = s.0 {
            if p < prev { s.0 = Some(prev); }
            prev = s.0.unwrap();
        }
    }

    // 3. 中間 Auto 群を前後 fixed の等間隔で補間
    let mut i = 0usize;
    while i < fixed.len() {
        if fixed[i].0.is_some() { i += 1; continue; }
        // 連続 Auto 群を見つけて [i..j) を等間隔補間
        let prev_pos = fixed[i - 1].0.unwrap();
        let mut j = i;
        while j < fixed.len() && fixed[j].0.is_none() { j += 1; }
        let next_pos = fixed[j].0.unwrap();
        let count = j - i + 1;
        for k in 0..(j - i) {
            let t = (k + 1) as f32 / count as f32;
            fixed[i + k].0 = Some(prev_pos + (next_pos - prev_pos) * t);
        }
        i = j;
    }

    fixed.into_iter().map(|(p, rgba)| (p.unwrap(), rgba)).collect()
}
```

**Step 2: lib smoke + 4 象限 VRT を実行**

```bash
cargo test -p fulgur --test render_smoke test_render_html_conic_gradient
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt --test conic_gradient_harness
```

期待: lib smoke は pass。VRT は **角度規約が想定と合っていれば pass、外れていれば pixel diff が出る**。

**Step 3: angle 規約のデバッグ (VRT が fail した場合のみ)**

VRT が fail した場合、生成された pixel diff 画像 (`target/vrt-conic-gradient-harness/diff_*.png`) を見て、advisor 指摘の 3 分類で判定する:

- **全色が同じ方向に 1 象限ずれ** → `krilla_start_deg` のオフセットを再検討 (90.0 ± k×90)
- **増加方向だけ逆 (red/blue が左右反転)** → `krilla_end_deg = krilla_start_deg + 360.0` (sweep 反転) + krilla `stops` 逆順 (`fixed.iter().rev()`) + 各 stop の `p_norm` を `1.0 - p_norm` に
- **完全にランダム / Y-flip 疑い** → krilla COLR (`text/glyph/colr.rs:400`) で
  `Transform::from_scale(1.0, -1.0)` + `cy: -cy` を入れていた件と同じ。
  `sweep.transform = krilla::geom::Transform::from_scale(1.0, -1.0)`、`cy: -cy` で再試行。

修正後 もう一度実行。

**重複 stop の注意:** Task 5 の fixture は `red 90deg, yellow 90deg` のような
同一 offset stop を使う (CSS step transition)。krilla `Stop` の `offset` が
同値で並ぶと未定義動作の可能性があるため、`fixup_conic_stops` の後で
**隣接 stop が完全同値の場合のみ** 後者に `1e-6` を足す nudge を入れる:

```rust
for i in 1..fixed.len() {
    if (fixed[i].0 - fixed[i - 1].0).abs() < f32::EPSILON {
        fixed[i].0 = (fixed[i - 1].0 + 1e-6).min(1.0);
    }
}
```

**Step 4: VRT pass を確認 + commit**

```bash
git add crates/fulgur/src/background.rs
git commit -m "feat(gradient): implement draw_conic_gradient via krilla SweepGradient"
```

---

## Task 7: `from <angle>` 検証 fixture と test を追加

**Files:**

- Create: `crates/fulgur-vrt/fixtures/paint/conic-gradient-from-90.html`
- Modify: `crates/fulgur-vrt/tests/conic_gradient_harness.rs`

**Step 1: fixture を追加**

```html
<!-- conic-gradient-from-90.html -->
<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>VRT test: from 90deg</title>
<style>
  html, body { margin:0; padding:0; background:white; }
  .box {
    width:192px; height:192px; margin:32px;
    background: conic-gradient(from 90deg,
      red 0deg, red 90deg,
      yellow 90deg, yellow 180deg,
      green 180deg, green 270deg,
      blue 270deg, blue 360deg);
  }
</style>
</head>
<body><div class="box"></div></body>
</html>
```

`from 90deg` は CSS 0% を 90deg (= screen 右) にシフトするので、red sector が
右下 1/4、yellow が左下、green が左上、blue が右上 になる。

**Step 2: ref builder を 90° rotate した版に拡張**

```rust
fn build_quadrant_ref_html_rotated_90() -> String {
    format!(
        r#"...
  .red    {{ background: red;    clip-path: polygon(50% 50%, 100% 50%, 100% 100%,  50% 100%); }}
  .yellow {{ background: yellow; clip-path: polygon(50% 50%,  50% 100%,   0% 100%,   0% 50%); }}
  .green  {{ background: green;  clip-path: polygon(50% 50%,   0% 50%,   0%   0%,  50%   0%); }}
  .blue   {{ background: blue;   clip-path: polygon(50% 50%,  50%   0%, 100%   0%, 100% 50%); }}
..."#)
}

#[test]
fn conic_gradient_from_90deg_matches_rotated_reference() {
    // ... harness boilerplate (Task 5 と同じ)
    let test_html_path = ... "conic-gradient-from-90.html";
    let ref_html = build_quadrant_ref_html_rotated_90();
    // ... pixel diff
}
```

**Step 3: 実行 + commit**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt --test conic_gradient_harness
git add crates/fulgur-vrt/fixtures/paint/conic-gradient-from-90.html \
        crates/fulgur-vrt/tests/conic_gradient_harness.rs
git commit -m "test(gradient): add conic-gradient from-angle VRT case"
```

---

## Task 8: `at <position>` (中心オフセット) fixture と test を追加

**Files:**

- Create: `crates/fulgur-vrt/fixtures/paint/conic-gradient-at-25-75.html`
- Modify: `crates/fulgur-vrt/tests/conic_gradient_harness.rs`

**Step 1: fixture と ref (advisor 提案で穏当な offset に変更)**

box を 96×96 に縮小し、中心を `at 40% 60%` (= 38.4, 57.6 px) にする。これにより
polygon 頂点の整数化を保てて tolerance を radial harness と同じ `max_diff_ratio: 0.02` に
維持できる (極端な offset は別 PR で扱う)。

```html
<!-- conic-gradient-at-40-60.html -->
<style>
  .box {
    width:96px; height:96px; margin:32px;
    background: conic-gradient(at 40% 60%,
      red 0deg, red 90deg,
      yellow 90deg, yellow 180deg,
      green 180deg, green 270deg,
      blue 270deg, blue 360deg);
  }
</style>
```

ref は同じ box サイズで polygon 頂点を中心 (38.4, 57.6) からの放射線として計算する。

**Step 2: 実行 + commit**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt --test conic_gradient_harness conic_gradient_at_position
git add crates/fulgur-vrt/fixtures/paint/conic-gradient-at-25-75.html \
        crates/fulgur-vrt/tests/conic_gradient_harness.rs
git commit -m "test(gradient): add conic-gradient at-position VRT case"
```

---

## Task 9: `repeating-conic-gradient` fixture と test を追加

**Files:**

- Create: `crates/fulgur-vrt/fixtures/paint/repeating-conic-12-stripes.html`
- Modify: `crates/fulgur-vrt/tests/conic_gradient_harness.rs`

**Step 1: fixture**

```html
<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>VRT test: repeating conic 12 stripes</title>
<style>
  html, body { margin:0; padding:0; background:white; }
  .box {
    width:192px; height:192px; margin:32px;
    background: repeating-conic-gradient(
      red    0deg, red    15deg,
      blue  15deg, blue   30deg);
  }
</style>
</head>
<body><div class="box"></div></body>
</html>
```

これは 30deg 周期で 12 個の red/blue 縞が回る。

**Step 2: ref (sector polygon ヘルパを切り出す)**

12 個の交互 sector を CSS clip-path で並べる ref を生成する。advisor 指摘に従い、
sector polygon の頂点は box 辺との交点で変わる (上辺/右辺/下辺/左辺 をまたぐ)
ため、専用ヘルパに分離して見通しを保つ:

```rust
/// 中心 (cx_pct, cy_pct) (% 単位) から角度 [start_deg, end_deg) の sector を
/// 含む clip-path polygon 文字列を返す。CSS conic 規約 (0=top, CW)。
/// 中心 + 必要な box-edge 交点 + start/end 単位ベクトル endpoint を順に並べる。
fn sector_polygon_clip_path(
    cx_pct: f32, cy_pct: f32,
    start_deg: f32, end_deg: f32,
) -> String {
    // 1. start_deg から end_deg まで 0deg, 90deg, 180deg, 270deg 境界で
    //    box 辺との交点を挟みながら polygon 頂点列を構築
    // 2. 各角度 θ に対して中心から外周への ray が box 辺と交わる点を
    //    `tan(θ - boundary)` で計算
    // 3. CSS clip-path polygon 文字列に整形
    ...
}

fn build_12_stripe_ref_html() -> String {
    let mut sectors = String::new();
    for i in 0..12 {
        let color = if i % 2 == 0 { "red" } else { "blue" };
        let path = sector_polygon_clip_path(50.0, 50.0, i as f32 * 30.0, (i + 1) as f32 * 30.0);
        sectors.push_str(&format!(
            r#"<div class="sector" style="background:{color};clip-path:{path};"></div>"#
        ));
    }
    format!(...)
}
```

tolerance は 12 個の境界線で AA が増えるため radial と同レベル
(`max_pixel_diff: 14.0, max_diff_ratio: 0.03`)。

**Step 3: 実行 + commit**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt --test conic_gradient_harness repeating
git add crates/fulgur-vrt/fixtures/paint/repeating-conic-12-stripes.html \
        crates/fulgur-vrt/tests/conic_gradient_harness.rs
git commit -m "test(gradient): add repeating-conic-gradient VRT case"
```

---

## Task 10: WPT expectations の更新

**Files:**

- Modify: `crates/fulgur-wpt-runner/expectations/lists/bugs.txt` (パスは要確認)

**Step 1: 影響を受ける WPT を一覧化**

```bash
grep -n "conic-gradient" crates/fulgur-wpt-runner/expectations/lists/bugs.txt
grep -rn "conic-gradient" crates/fulgur-wpt-runner/expectations/ | head
```

**Step 2: 実測**

```bash
# WPT runner で conic-gradient 関連 reftest を実行
cargo run --bin fulgur-wpt-runner -- --filter conic-gradient
```

**Step 3: PASS した項目を bugs.txt から削除**

実際に PASS したものを bugs.txt から外し、依然 fail のものはコメントを
更新 (false PASS 対策が Phase 1 linear で行われたのと同じ)。

**Step 4: commit**

```bash
git add crates/fulgur-wpt-runner/expectations/lists/bugs.txt
git commit -m "test(wpt): update conic-gradient expectations after Phase 2 implementation"
```

---

## Task 11: 全体テスト + lint + 確認

**Step 1: full test**

```bash
cargo test -p fulgur                     # ~340 + 新規 smoke
cargo test -p fulgur --test gcpm_integration
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt --test conic_gradient_harness
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt --test gradient_harness
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
    cargo test -p fulgur-vrt --test radial_gradient_harness
```

**Step 2: lint**

```bash
cargo fmt --check
cargo clippy -- -D warnings
npx markdownlint-cli2 'docs/plans/2026-04-27-conic-gradient.md'
```

**Step 3: regenerate examples (任意・gradient を使う example が変わるなら)**

```bash
mise run regenerate-examples
```

**Step 4: 最終 commit (必要なら)**

```bash
git status
# fmt 修正があれば commit
```

---

## 完了基準

- [ ] `crates/fulgur/tests/render_smoke.rs` の 2 件の conic smoke が pass
- [ ] `crates/fulgur-vrt/tests/conic_gradient_harness.rs` の 4 件の VRT が pass
  - 4 quadrant
  - from 90deg
  - at 25% 75%
  - repeating 12 stripes
- [ ] `cargo fmt --check` / `cargo clippy -- -D warnings` 通る
- [ ] WPT bugs.txt から conic-gradient PASS 項目が外れている
- [ ] `bd close fulgur-m92h` 実行可能な状態

## 設計上の参照

- `fulgur-m92h` (本 issue) の design field
- `fulgur-batz` — PDF/A-safe fallback の追跡 (本 PR ではフラグ判定なし)
- `crates/fulgur/src/background.rs::draw_radial_gradient` — 同等パターンの実装参考
- `crates/fulgur-vrt/tests/radial_gradient_harness.rs` — VRT pixel-diff harness の参考
- CSS Images Level 4 §3.x — conic-gradient 仕様 (https://drafts.csswg.org/css-images-4/#conic-gradients)
- krilla 0.7 `src/graphics/paint.rs:103` — `SweepGradient` 型定義
- krilla 0.7 `src/graphics/shading_function.rs:170,420` — sweep の PostScript 経路
