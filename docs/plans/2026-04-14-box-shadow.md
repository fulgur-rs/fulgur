# box-shadow Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `box-shadow` プロパティの blur=0 ベクター描画対応 (v0.4.5, fulgur-4ie)

**Architecture:** stylo が parse 済みの `BoxShadowList` を `clone_box_shadow()` で取得し、
fulgur 内部型 `BoxShadow` に変換して `BlockStyle.box_shadows` に格納。`background.rs` で
background-color の**前**（背後）に `draw_box_shadow` を呼び、spread 反映済みの
rounded-rect を複数枚重ね塗りする。Prince 方針に追随し blur は本リリースでは無視。

**Tech Stack:** Rust / Blitz (stylo 0.8) / Krilla 0.7 / 既存の `build_rounded_rect_path`

---

## Scope Recap

- `offset-x`, `offset-y`, `spread-radius`, `color`（alpha/transparent/currentColor）
- 複数シャドウ（front-to-back レイヤリング）
- `border-radius` 連携
- `blur > 0` は blur=0 扱いで描画 + 警告ログ
- `inset` はスキップ + 警告ログ
- ページ跨ぎ: Pageable::draw が自然に処理するため追加対応不要

---

## Task 1: 内部データ型 `BoxShadow` と `BlockStyle` への追加

**Files:**

- Modify: `crates/fulgur/src/pageable.rs:266-285` (BlockStyle 構造体)

**Step 1.1: BlockStyle に box_shadows フィールド追加**

`crates/fulgur/src/pageable.rs` の `BlockStyle` 構造体の直前に以下を追加：

```rust
/// A resolved single box-shadow value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoxShadow {
    /// Horizontal offset in points.
    pub offset_x: f32,
    /// Vertical offset in points.
    pub offset_y: f32,
    /// Blur radius in points. Currently unused for rendering (v0.4.5 draws blur=0).
    pub blur: f32,
    /// Spread radius in points. Negative values shrink the shadow.
    pub spread: f32,
    /// Shadow color as RGBA.
    pub color: [u8; 4],
    /// Whether this is an inset shadow. Currently unsupported (skipped at draw time).
    pub inset: bool,
}
```

`BlockStyle` 構造体に以下のフィールドを追加：

```rust
    /// Box shadows in CSS declaration order (first = top-most in paint stack).
    pub box_shadows: Vec<BoxShadow>,
```

`BlockStyle::has_visual_style` に `!self.box_shadows.is_empty()` を OR に追加。

**Step 1.2: ユニットテスト**

`pageable.rs` 内の `mod tests` で `BoxShadow::default()` が全ゼロになること、
`has_visual_style()` が box_shadow 設定時に true になることを確認する小テストを追加：

```rust
#[test]
fn has_visual_style_with_only_box_shadow() {
    let style = BlockStyle {
        box_shadows: vec![BoxShadow {
            offset_x: 2.0, offset_y: 2.0, blur: 0.0, spread: 0.0,
            color: [0, 0, 0, 255], inset: false,
        }],
        ..Default::default()
    };
    assert!(style.has_visual_style());
}
```

**Step 1.3: ビルド＆テスト**

```bash
cargo build
cargo test --lib -p fulgur has_visual_style_with_only_box_shadow
```

Expected: pass

**Step 1.4: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(pageable): add BoxShadow type and BlockStyle.box_shadows field"
```

---

## Task 2: stylo から BoxShadow を抽出

**Files:**

- Modify: `crates/fulgur/src/convert.rs:1684-1790` (extract_block_style 関数)

**Step 2.1: スモーク統合テストを書く（panic 防止確認）**

新規ファイル `crates/fulgur/tests/box_shadow_test.rs` を作成：

```rust
//! box-shadow rendering tests.

use fulgur::Engine;

#[test]
fn renders_basic_offset_shadow_without_error() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;background:#eee;
                  box-shadow: 4px 4px 0 #888;">hi</div>
    </body></html>"#;
    let pdf = Engine::new().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
    // Minimal smoke test: PDF produced, no panic.
}

#[test]
fn renders_multiple_shadows() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;
                  box-shadow: 2px 2px 0 red, 4px 4px 0 blue;"></div>
    </body></html>"#;
    let pdf = Engine::new().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_shadow_with_rgba_alpha() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;
                  box-shadow: 2px 2px 0 rgba(0,0,0,0.5);"></div>
    </body></html>"#;
    let pdf = Engine::new().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_shadow_with_spread() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;
                  box-shadow: 0 0 0 4px red;"></div>
    </body></html>"#;
    let pdf = Engine::new().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_shadow_with_negative_spread() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;
                  box-shadow: 0 0 0 -2px red;"></div>
    </body></html>"#;
    let pdf = Engine::new().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}

#[test]
fn renders_shadow_with_border_radius() {
    let html = r#"
    <!DOCTYPE html><html><body>
      <div style="width:100px;height:100px;border-radius:20px;
                  box-shadow: 4px 4px 0 2px #888;"></div>
    </body></html>"#;
    let pdf = Engine::new().render_html(html).expect("render ok");
    assert!(!pdf.is_empty());
}
```

**Step 2.2: テスト実行して失敗を確認**

```bash
cargo test -p fulgur --test box_shadow_test
```

Expected: すべて pass (現状 box-shadow は**未実装だがパースは通る**ため、描画せずに
PDF 生成は成功するはず)。この段階ではテストは失敗しない可能性が高いので、
**ゴールは"rendering pipeline が panic しないこと"の確認**にとどめる。

もし panic する場合は根本原因を調べる（これは後の Task 3 でカバーされる想定なので、
panic したら Task 3 へ進める）。

**Step 2.3: convert.rs の extract_block_style を拡張**

`crates/fulgur/src/convert.rs` の `extract_block_style` 内、`border_radii` 設定の
直後に以下のブロックを追加：

```rust
        // Box shadows
        let shadow_list = styles.clone_box_shadow();
        for shadow in shadow_list.0.iter() {
            if shadow.inset {
                log::warn!("box-shadow: inset is not yet supported; skipping");
                continue;
            }
            let blur_px = shadow.base.blur.px();
            if blur_px > 0.0 {
                log::warn!(
                    "box-shadow: blur-radius > 0 is not yet supported; \
                     drawing as blur=0 (blur={}px)",
                    blur_px
                );
            }
            let color_abs = shadow.base.color.resolve_to_absolute(&current_color);
            let r = (color_abs.components.0.clamp(0.0, 1.0) * 255.0) as u8;
            let g = (color_abs.components.1.clamp(0.0, 1.0) * 255.0) as u8;
            let b = (color_abs.components.2.clamp(0.0, 1.0) * 255.0) as u8;
            let a = (color_abs.alpha.clamp(0.0, 1.0) * 255.0) as u8;
            if a == 0 {
                continue; // fully transparent — skip
            }
            style.box_shadows.push(crate::pageable::BoxShadow {
                offset_x: shadow.base.horizontal.px(),
                offset_y: shadow.base.vertical.px(),
                blur: blur_px,
                spread: shadow.spread.px(),
                color: [r, g, b, a],
                inset: shadow.inset,
            });
        }
```

注意:

- `clone_box_shadow()` は stylo の `BoxShadowList`（`.0` フィールドに `Vec<BoxShadow>`）を返す。
  もし実際の API が異なる場合は `cargo check` のエラーメッセージから正しい API 名を確認。
- `shadow.base.blur` は `NonNegativeLength` なので `.px()` は `f32` を返す（`NonNegative` は
  既存の `clone_border_top_left_radius()` 系で使われているのと同じパターン、の簡略版）。
- CSS の declaration order をそのまま保存（前のシャドウほど手前）。描画時に逆順で塗る。

**Step 2.4: ビルド**

```bash
cargo build 2>&1 | head -30
```

もし `clone_box_shadow()` の API 名やフィールド名が違う場合はエラーメッセージを見て修正。
代替として Blitz の `ComputedValues::get_effects()` 経由のアクセスも試す。

**Step 2.5: テスト**

```bash
cargo test -p fulgur --test box_shadow_test
```

Expected: すべて pass (この時点ではまだ描画していないが panic しないこと)。

**Step 2.6: Commit**

```bash
git add crates/fulgur/src/convert.rs crates/fulgur/tests/box_shadow_test.rs
git commit -m "feat(convert): extract box-shadow from stylo computed values"
```

---

## Task 3: 描画関数 `draw_box_shadow` 実装

**Files:**

- Modify: `crates/fulgur/src/background.rs`
- Modify: `crates/fulgur/src/pageable.rs:1290-1320` (drawing order)

**Step 3.1: background.rs に draw_box_shadows 関数を追加**

`crates/fulgur/src/background.rs` の `draw_background` 関数の**上**に追加：

```rust
/// Draw outer box-shadows behind the element's background.
///
/// Per CSS Backgrounds §7.2, shadows are painted in reverse declaration order
/// (last-declared shadow at the bottom of the paint stack, first-declared on top).
/// Outer shadows are drawn _below_ the element's background and border.
/// `inset` shadows are currently unsupported and filtered out upstream.
pub fn draw_box_shadows(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    if style.box_shadows.is_empty() {
        return;
    }
    // Draw in reverse declaration order so the first shadow ends up on top.
    for shadow in style.box_shadows.iter().rev() {
        if shadow.inset {
            continue; // defensive; should already be filtered in convert.rs
        }
        draw_single_box_shadow(canvas, style, shadow, x, y, w, h);
    }
}

fn draw_single_box_shadow(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    shadow: &crate::pageable::BoxShadow,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let sx = x + shadow.offset_x - shadow.spread;
    let sy = y + shadow.offset_y - shadow.spread;
    let sw = w + 2.0 * shadow.spread;
    let sh = h + 2.0 * shadow.spread;
    if sw <= 0.0 || sh <= 0.0 {
        return;
    }

    let path = if style.has_radius() {
        let radii = expand_radii(&style.border_radii, shadow.spread);
        crate::pageable::build_rounded_rect_path(sx, sy, sw, sh, &radii)
    } else {
        build_rect_path(sx, sy, sw, sh)
    };
    let Some(path) = path else { return };

    let [r, g, b, a] = shadow.color;
    canvas.surface.set_fill(Some(krilla::paint::Fill {
        paint: krilla::color::rgb::Color::new(r, g, b).into(),
        opacity: krilla::num::NormalizedF32::new(a as f32 / 255.0)
            .unwrap_or(krilla::num::NormalizedF32::ONE),
        rule: Default::default(),
    }));
    canvas.surface.set_stroke(None);
    canvas.surface.draw_path(&path);
}

/// Expand border radii by `spread`. Negative `spread` clamps to zero per CSS spec
/// (shadow corners become sharp when spread < -radius).
fn expand_radii(outer: &[[f32; 2]; 4], spread: f32) -> [[f32; 2]; 4] {
    [
        [
            f32::max(outer[0][0] + spread, 0.0),
            f32::max(outer[0][1] + spread, 0.0),
        ],
        [
            f32::max(outer[1][0] + spread, 0.0),
            f32::max(outer[1][1] + spread, 0.0),
        ],
        [
            f32::max(outer[2][0] + spread, 0.0),
            f32::max(outer[2][1] + spread, 0.0),
        ],
        [
            f32::max(outer[3][0] + spread, 0.0),
            f32::max(outer[3][1] + spread, 0.0),
        ],
    ]
}
```

**Step 3.2: pageable.rs の draw 呼び出しで shadow を先に描画**

`crates/fulgur/src/pageable.rs:1308` の `crate::background::draw_background(...)` の
**直前**に以下を追加：

```rust
                crate::background::draw_box_shadows(
                    canvas,
                    &self.style,
                    x,
                    y,
                    total_width,
                    total_height,
                );
```

同じパターンを `pageable.rs:2238` の `TablePageable::draw` 内にも適用（2箇所目の
`draw_background` 呼び出し）。

**Step 3.3: ビルド**

```bash
cargo build 2>&1 | tail -10
```

Expected: clean build

**Step 3.4: テスト**

```bash
cargo test -p fulgur --test box_shadow_test
cargo test --lib
```

Expected: すべて pass

**Step 3.5: Commit**

```bash
git add crates/fulgur/src/background.rs crates/fulgur/src/pageable.rs
git commit -m "feat(background): render outer box-shadow behind background layer"
```

---

## Task 4: VRT fixture + example

**Files:**

- Create: `crates/fulgur-vrt/fixtures/basic/box-shadow.html`
- Create: `examples/box-shadow/index.html`
- Create: `examples/box-shadow/style.css`
- Modify: `crates/fulgur-vrt/manifest.toml`

**Step 4.1: VRT fixture 作成**

既存の `crates/fulgur-vrt/fixtures/basic/border-radius.html` を参考に
`box-shadow.html` を新規作成。以下のケースを網羅：

- 基本 offset + color
- spread のみ
- 複数シャドウ
- alpha color
- border-radius 連携
- 負の spread

```html
<!DOCTYPE html>
<html>
<head><style>
  body { margin: 0; padding: 20px; font-family: sans-serif; }
  .box { width: 80px; height: 60px; background: #fff; display: inline-block;
         margin: 20px; padding: 10px; }
  .basic { box-shadow: 4px 4px 0 #888; }
  .spread { box-shadow: 0 0 0 4px #c33; }
  .multi  { box-shadow: 2px 2px 0 red, 6px 6px 0 blue; }
  .alpha  { box-shadow: 4px 4px 0 rgba(0,0,0,0.4); }
  .radius { border-radius: 12px; box-shadow: 4px 4px 0 2px #888; }
  .neg    { background: #fcc; box-shadow: 0 4px 0 -2px #333; }
</style></head>
<body>
  <div class="box basic">basic</div>
  <div class="box spread">spread</div>
  <div class="box multi">multi</div>
  <div class="box alpha">alpha</div>
  <div class="box radius">radius</div>
  <div class="box neg">negative spread</div>
</body>
</html>
```

**Step 4.2: manifest に追加**

`crates/fulgur-vrt/manifest.toml` の既存の fixture 登録パターンを探し、
box-shadow 用のエントリを追加（既存の `border-radius` と同じ設定を流用）。

**Step 4.3: VRT ベースライン画像を生成**

```bash
cargo run --bin fulgur-vrt -- --update 2>&1 | tail -20
```

もし既存の更新コマンドが違う場合は `crates/fulgur-vrt/manifest.toml` とテストコードを
確認して適切な手順を実行。

**Step 4.4: examples/box-shadow/ 作成**

`examples/border-radius/` のレイアウト（`index.html`, `style.css`）をコピーし、
box-shadow のショーケースに書き換え。カードUI風のデザインで実用性を見せる。

**Step 4.5: VRT テスト**

```bash
cargo test -p fulgur-vrt 2>&1 | tail -20
```

Expected: pass

**Step 4.6: Commit**

```bash
git add crates/fulgur-vrt/fixtures/basic/box-shadow.html \
        crates/fulgur-vrt/manifest.toml \
        crates/fulgur-vrt/snapshots/ \
        examples/box-shadow/
git commit -m "test(vrt): add box-shadow visual regression fixture and example"
```

---

## Task 5: CHANGELOG + README 更新

**Files:**

- Modify: `CHANGELOG.md`
- Modify: `README.md` (CSS サポート表があれば)

**Step 5.1: CHANGELOG.md に追記**

`CHANGELOG.md` の未リリースセクション (v0.4.5 or [Unreleased]) に以下を追加：

```markdown
### Added

- `box-shadow` (outer only): offset, spread, color, alpha, multiple shadows,
  `border-radius` 連携に対応 (fulgur-4ie)

### Limitations

- `box-shadow` の `blur-radius > 0` と `inset` は未対応（警告ログ出力、blur は 0 扱い）。
  `blur` のラスタライズ対応は後続リリースで予定。
```

**Step 5.2: README.md の CSS サポート表を更新**

該当する行（Box Model / Effects セクション等）があれば `box-shadow: ✅ (blur未対応)` 等に
更新。無ければスキップ。

**Step 5.3: markdownlint**

```bash
npx markdownlint-cli2 '**/*.md'
```

Expected: pass

**Step 5.4: Commit**

```bash
git add CHANGELOG.md README.md
git commit -m "docs: document box-shadow support in v0.4.5"
```

---

## Task 6: 全体検証

**Step 6.1: 全テスト**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: 全て pass

**Step 6.2: 既存 example の regression 確認**

```bash
cargo test --test examples_determinism 2>&1 | tail -20
```

**Step 6.3: CLI 経由で動作確認**

```bash
cargo run --bin fulgur -- render examples/box-shadow/index.html -o /tmp/box-shadow.pdf
ls -la /tmp/box-shadow.pdf
```

Expected: PDF が生成される（ファイルサイズ > 0）

**Step 6.4: 手動目視確認**

生成した `/tmp/box-shadow.pdf` を PDF ビューアで開き、以下を確認：

- 影が要素の背後に正しく描画されている
- 複数シャドウが front-to-back で重なっている
- border-radius と影が両方角丸になっている
- alpha シャドウが透過して見える
- 負の spread で影が要素より小さくなっている

---

## Risks / Open Questions

1. **stylo の `clone_box_shadow()` API 名**: 実際は `clone_box_shadow()` 以外の名前
   （例: `get_box_shadow()`）になっている可能性。ビルドエラーが出たら `cargo doc` や
   `grep -rn "box_shadow" "${CARGO_HOME:-$HOME/.cargo}/registry/src/"*/stylo-0.8.0/`
   で探索する。

2. **BoxShadowList の構造**: `.0` フィールドでない可能性。`Deref` 実装を持つかも。
   ビルドエラーから推定。

3. **Blitz から stylo の effects プロパティを取れない場合**: 稀に Blitz がサブセット
   プロパティしかサポートしないことがある。その場合は `blitz_adapter.rs` にヘルパを
   作り、`node.primary_styles().unwrap().as_ref().get_effects().box_shadow` のように
   明示的にアクセスする fallback を追加。

4. **パフォーマンス**: 複数シャドウを持つ要素が多数ある場合、fill_path 呼び出しが
   線形に増える。現時点ではそのまま描画してよい（ページ単位で数千要素あっても krilla は
   十分速い想定）。問題が出たら後続で最適化。

5. **ページ跨ぎ時の影の切れ方**: CSS 的には要素を「分割」する概念がないので、現状
   fulgur は要素を物理ページ境界で区切って各ページに描画する。ここでの影も各ページ
   内で閉じる。これは WeasyPrint / Prince も同じ挙動のはず。VRT で 1 ページ内の
   描画が正しければ十分、ページ跨ぎは別途手動確認。
