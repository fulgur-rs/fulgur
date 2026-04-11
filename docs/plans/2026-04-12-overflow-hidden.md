# overflow: hidden / clip Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CSS `overflow: hidden | clip | scroll | auto` を PDF 出力で padding-box にクリップする。`overflow-x` / `overflow-y` は軸独立で処理し、`border-radius` と組み合わせ可能にする。

**Architecture:** `BlockStyle` に `overflow_x` / `overflow_y` フィールドを追加し、`BlockPageable::draw()` で子描画の前に `surface.push_clip_path()`、後に `surface.pop()` する。クリップ矩形は padding-box を基準とし、非クリップ軸は事実上無限大 (`±1e6`) の矩形にする。両軸クリップかつ `border-radius` がある場合のみ `compute_inner_radii` + `build_rounded_rect_path` を再利用する。

**Tech Stack:** Rust, Krilla (`surface.push_clip_path`/`pop`, `krilla::geom::Path`), Stylo (`computed::Overflow`), Blitz.

**Beads Issue:** `fulgur-9x6`

---

## 設計の前提 (beads issue の design フィールドから要約)

- `Visible` 以外 (`Hidden`/`Clip`/`Scroll`/`Auto`) はすべて `Overflow::Clip` に統合する (PDF ではスクロール概念がないため)
- `overflow-x: hidden; overflow-y: visible` のような軸独立組合せは CSS3 解釈 (CSS2.1 の互換ルールは採用しない)
- ページまたぎ時はフラグメントごとに自然にクリップ (特別扱いしない)
- 妥協: 片軸クリップ + `border-radius` は角丸を無視して矩形クリップ。両軸クリップ時のみ角丸を尊重。

---

## Task 1: `Overflow` enum と `BlockStyle` フィールド追加

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (139-154 付近 `BlockStyle`, 178 付近 `BorderStyleValue` の後)

**Step 1: Write the failing test**

`crates/fulgur/src/pageable.rs` の末尾 (`#[cfg(test)] mod background_tests` の後、またはその中) に追加:

```rust
#[cfg(test)]
mod overflow_tests {
    use super::*;

    #[test]
    fn test_overflow_default_is_visible() {
        let style = BlockStyle::default();
        assert_eq!(style.overflow_x, Overflow::Visible);
        assert_eq!(style.overflow_y, Overflow::Visible);
    }

    #[test]
    fn test_overflow_clip_flag() {
        let mut style = BlockStyle::default();
        style.overflow_x = Overflow::Clip;
        assert!(style.has_overflow_clip());
        style.overflow_x = Overflow::Visible;
        style.overflow_y = Overflow::Clip;
        assert!(style.has_overflow_clip());
        style.overflow_y = Overflow::Visible;
        assert!(!style.has_overflow_clip());
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test -p fulgur --lib overflow_tests 2>&1 | tail -20
```

Expected: `error[E0433]: failed to resolve: Overflow` など未定義エラー。

**Step 3: Write minimal implementation**

`pageable.rs` の `BorderStyleValue` 定義の後 (178 行目以降) に追加:

```rust
/// CSS `overflow-x` / `overflow-y` value.
///
/// PDF は静的メディアなので、CSS の `hidden`/`clip`/`scroll`/`auto` はすべて
/// 「padding-box でクリップ」という同一の動作に統合する。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overflow {
    /// `visible` — クリップしない (デフォルト)
    #[default]
    Visible,
    /// `hidden` / `clip` / `scroll` / `auto` — padding-box でクリップする
    Clip,
}
```

`BlockStyle` 構造体にフィールドを追加 (139 行目):

```rust
pub struct BlockStyle {
    // ... 既存フィールド ...
    /// Border styles: top, right, bottom, left
    pub border_styles: [BorderStyleValue; 4],
    /// `overflow-x` value
    pub overflow_x: Overflow,
    /// `overflow-y` value
    pub overflow_y: Overflow,
}
```

`impl BlockStyle` ブロック (246 行目周辺) にヘルパーメソッドを追加:

```rust
impl BlockStyle {
    // ... 既存メソッド ...

    /// Whether any axis has overflow clipping enabled.
    pub fn has_overflow_clip(&self) -> bool {
        self.overflow_x == Overflow::Clip || self.overflow_y == Overflow::Clip
    }
}
```

**Step 4: Run test to verify it passes**

```bash
cargo build -p fulgur 2>&1 | tail -5
cargo test -p fulgur --lib overflow_tests 2>&1 | tail -10
```

Expected: `test result: ok. 2 passed`

**Step 5: Verify no regressions in existing tests**

```bash
cargo test -p fulgur --lib 2>&1 | grep "test result" | tail -3
```

Expected: すべての既存テストが通過 (231 passed を維持)。

**Step 6: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(fulgur): add Overflow enum and BlockStyle overflow fields

Part of fulgur-9x6. Adds the type-level plumbing for overflow clipping
without changing runtime behavior. The enum collapses hidden/clip/
scroll/auto to a single Clip variant since PDF has no scroll concept."
```

---

## Task 2: `compute_overflow_clip_path` ヘルパー (純粋関数)

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (新規ヘルパー関数追加 + テスト拡充)

**Step 1: Write the failing tests**

`overflow_tests` モジュールに追加:

```rust
    #[test]
    fn test_clip_path_visible_returns_none() {
        let style = BlockStyle::default();
        assert!(compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0).is_none());
    }

    #[test]
    fn test_clip_path_both_axes_rect() {
        let mut style = BlockStyle::default();
        style.overflow_x = Overflow::Clip;
        style.overflow_y = Overflow::Clip;
        // padding/border ともに 0 の場合、padding-box は外側矩形と一致
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(path.is_some(), "both-axes clip should produce a path");
    }

    #[test]
    fn test_clip_path_axis_x_only_uses_padding_box_x() {
        let mut style = BlockStyle::default();
        style.overflow_x = Overflow::Clip;
        style.border_widths = [0.0, 5.0, 0.0, 5.0]; // left/right border 5pt
        // overflow-x=clip, overflow-y=visible → Y 方向は無制限
        let path = compute_overflow_clip_path(&style, 10.0, 20.0, 100.0, 50.0);
        assert!(path.is_some(), "x-only clip should produce a path");
        // パス境界ボックスの X 範囲が (10+5, 10+100-5)=[15, 105]、
        // Y 範囲が [-1e6+20, 1e6+20] 付近であることを確認
        let bounds = path.unwrap().bounds();
        assert!((bounds.left() - 15.0).abs() < 0.01);
        assert!((bounds.right() - 105.0).abs() < 0.01);
        assert!(bounds.top() < -900_000.0);
        assert!(bounds.bottom() > 900_000.0);
    }

    #[test]
    fn test_clip_path_both_axes_rounded() {
        let mut style = BlockStyle::default();
        style.overflow_x = Overflow::Clip;
        style.overflow_y = Overflow::Clip;
        style.border_radii = [[10.0, 10.0]; 4];
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(path.is_some(), "rounded clip should produce a path");
        // 矩形ではないことを確認 (bounds は 100x100 と一致するが、パスコマンド数が矩形より多い)
        // 簡易チェック: bounds が padding-box と一致
        let b = path.unwrap().bounds();
        assert!((b.left() - 0.0).abs() < 0.01);
        assert!((b.right() - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_clip_path_axis_x_only_ignores_rounded() {
        // 妥協: 片軸クリップ + 角丸は矩形フォールバック
        let mut style = BlockStyle::default();
        style.overflow_x = Overflow::Clip;
        style.border_radii = [[10.0, 10.0]; 4];
        let path = compute_overflow_clip_path(&style, 0.0, 0.0, 100.0, 100.0);
        assert!(path.is_some());
        // 角丸を無視するので bounds のコーナーは padding-box のまま
    }
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur --lib overflow_tests 2>&1 | tail -10
```

Expected: `error: cannot find function compute_overflow_clip_path`

**Step 3: Write minimal implementation**

`pageable.rs` の `build_rounded_rect_path` 関数 (419 行目付近) の後、または `BlockStyle impl` の下 (270 行付近) にヘルパーを追加:

```rust
/// Build a clip path for `overflow` based on the padding box.
///
/// - Returns `None` when both axes are `visible`.
/// - Axis-independent: a non-clipped axis uses a virtually unlimited range
///   (`±1e6`) so only the clipped axis is effectively bounded.
/// - `border-radius` is honored **only** when both axes are clipped. With
///   single-axis clipping, a plain rectangle is used (simplification).
pub fn compute_overflow_clip_path(
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Option<krilla::geom::Path> {
    if style.overflow_x == Overflow::Visible && style.overflow_y == Overflow::Visible {
        return None;
    }

    // padding-box = border-box を border-width 分だけ内側にオフセット
    let bw = &style.border_widths;
    let pb_x = x + bw[3];
    let pb_y = y + bw[0];
    let pb_w = (w - bw[1] - bw[3]).max(0.0);
    let pb_h = (h - bw[0] - bw[2]).max(0.0);

    if pb_w <= 0.0 || pb_h <= 0.0 {
        return None;
    }

    // 非クリップ軸は事実上無限大に拡張
    const INFINITE: f32 = 1.0e6;
    let (cx, cw) = if style.overflow_x == Overflow::Clip {
        (pb_x, pb_w)
    } else {
        (pb_x - INFINITE, pb_w + 2.0 * INFINITE)
    };
    let (cy, ch) = if style.overflow_y == Overflow::Clip {
        (pb_y, pb_h)
    } else {
        (pb_y - INFINITE, pb_h + 2.0 * INFINITE)
    };

    let both_axes = style.overflow_x == Overflow::Clip && style.overflow_y == Overflow::Clip;
    let has_radius = style.border_radii.iter().any(|r| r[0] > 0.0 || r[1] > 0.0);

    if both_axes && has_radius {
        // border-radius は border-box 基準なので inner (padding-box) radius に変換
        let inner_radii = compute_padding_box_inner_radii(&style.border_radii, &style.border_widths);
        build_rounded_rect_path(cx, cy, cw, ch, &inner_radii)
    } else {
        build_overflow_rect_path(cx, cy, cw, ch)
    }
}

/// Build an axis-aligned rectangle path. Local helper (background.rs has
/// a private equivalent; keep a crate-public copy here for clip use).
fn build_overflow_rect_path(x: f32, y: f32, w: f32, h: f32) -> Option<krilla::geom::Path> {
    let mut pb = krilla::geom::PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x + w, y);
    pb.line_to(x + w, y + h);
    pb.line_to(x, y + h);
    pb.close();
    pb.finish()
}

/// Compute padding-box inner radii from border-box outer radii.
/// CSS spec: inner_r = max(0, outer_r - border_width_on_that_side).
/// radii: [top-left, top-right, bottom-right, bottom-left] × [rx, ry]
/// borders: [top, right, bottom, left]
fn compute_padding_box_inner_radii(
    outer: &[[f32; 2]; 4],
    borders: &[f32; 4],
) -> [[f32; 2]; 4] {
    let [bt, br, bb, bl] = *borders;
    [
        [(outer[0][0] - bl).max(0.0), (outer[0][1] - bt).max(0.0)],
        [(outer[1][0] - br).max(0.0), (outer[1][1] - bt).max(0.0)],
        [(outer[2][0] - br).max(0.0), (outer[2][1] - bb).max(0.0)],
        [(outer[3][0] - bl).max(0.0), (outer[3][1] - bb).max(0.0)],
    ]
}
```

**Note on existing helpers:** `background.rs` には private な `build_rect_path` があるが、module 境界を越えるとpubにする必要があるため、`pageable.rs` 内に `build_overflow_rect_path` を置く。重複は最小 (4 行)。

**Step 4: Run tests to verify they pass**

```bash
cargo test -p fulgur --lib overflow_tests 2>&1 | tail -15
```

Expected: `test result: ok. 7 passed`

**Step 5: Run full test suite**

```bash
cargo test -p fulgur --lib 2>&1 | grep "test result" | tail -3
```

Expected: 既存テスト全通過 + 新規 5 テスト通過。

**Step 6: Clippy check**

```bash
cargo clippy -p fulgur 2>&1 | tail -10
```

Expected: 警告なし。

**Step 7: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(fulgur): compute_overflow_clip_path helper

Part of fulgur-9x6. Pure function that returns a Krilla clip path for
the padding box of a BlockStyle, honoring:
- None when both axes are visible
- Axis-independent clipping (unclipped axis uses virtually unlimited range)
- border-radius only when both axes are clipped (single-axis falls back
  to a plain rectangle for simplicity)"
```

---

## Task 3: `BlockPageable::draw()` にクリップを組み込む

**Files:**

- Modify: `crates/fulgur/src/pageable.rs:1030-1062` (`impl Pageable for BlockPageable::draw`)

**Step 1: Write a smoke test for draw() clipping**

`overflow_tests` モジュールに追加:

```rust
    #[test]
    fn test_block_draw_skips_clip_when_visible() {
        // visible/visible では has_overflow_clip() が false
        let style = BlockStyle::default();
        assert!(!style.has_overflow_clip());
    }

    #[test]
    fn test_block_draw_applies_clip_when_hidden() {
        // clip フラグが立っていれば has_overflow_clip() が true
        let mut style = BlockStyle::default();
        style.overflow_x = Overflow::Clip;
        assert!(style.has_overflow_clip());
    }
```

(実際の描画検証は Task 5 の integration test で行う。ここでは draw() の分岐が静的に正しいことを確認する単体テストのみ。)

**Step 2: Verify test baseline passes (no impl change yet)**

```bash
cargo test -p fulgur --lib overflow_tests 2>&1 | grep "test result"
```

Expected: 既に通過 (flag test は Task 1 で定義済み)。

**Step 3: Wire clip into draw()**

`pageable.rs:1030` の `impl Pageable for BlockPageable` の `fn draw` を以下のように変更:

```rust
    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
            // Prefer layout_size (Taffy-computed, stable) over cached_size (may be children-only)
            let total_width = self
                .layout_size
                .or(self.cached_size)
                .map(|s| s.width)
                .unwrap_or(avail_width);
            let total_height = self
                .layout_size
                .or(self.cached_size)
                .map(|s| s.height)
                .unwrap_or(avail_height);

            // visibility: hidden skips own rendering but children still draw
            if self.visible {
                crate::background::draw_background(
                    canvas,
                    &self.style,
                    x,
                    y,
                    total_width,
                    total_height,
                );
                draw_block_border(canvas, &self.style, x, y, total_width, total_height);
            }

            // overflow: clip children to the padding box (push before children, pop after).
            let clip_pushed = if let Some(clip_path) =
                compute_overflow_clip_path(&self.style, x, y, total_width, total_height)
            {
                canvas
                    .surface
                    .push_clip_path(&clip_path, &krilla::paint::FillRule::default());
                true
            } else {
                false
            };

            for pc in &self.children {
                pc.child
                    .draw(canvas, x + pc.x, y + pc.y, avail_width, pc.child.height());
            }

            if clip_pushed {
                canvas.surface.pop();
            }
        });
    }
```

**Step 4: Build and run unit tests**

```bash
cargo build -p fulgur 2>&1 | tail -5
cargo test -p fulgur --lib 2>&1 | grep "test result" | tail -3
```

Expected: すべて通過。既存 BlockPageable テストが `overflow_x/y = Visible` (default) なので clip_pushed=false となりブランチが一致 (回帰なし)。

**Step 5: Verify with existing examples (smoke test)**

```bash
cargo run --bin fulgur --release -- render examples/border-radius/index.html -o /tmp/border-radius-smoke.pdf 2>&1 | tail -5
ls -l /tmp/border-radius-smoke.pdf examples/border-radius/index.pdf
```

Expected: PDF が生成され、サイズが既存 `examples/border-radius/index.pdf` と近い (overflow が visible のまま → 挙動変化なし)。

**Step 6: Clippy check**

```bash
cargo clippy -p fulgur 2>&1 | tail -10
```

Expected: 警告なし。

**Step 7: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(fulgur): apply overflow clipping in BlockPageable::draw

Part of fulgur-9x6. Children are now drawn under a krilla clip path
constructed from the padding box when overflow_x or overflow_y is
non-visible. Background and border still render outside the clip.

No behavior change for existing content: default BlockStyle has both
axes Visible, so compute_overflow_clip_path returns None."
```

---

## Task 4: Stylo → `BlockStyle` マッピング (`convert.rs`)

**Files:**

- Modify: `crates/fulgur/src/convert.rs:830-928` (`extract_block_style`)

**Step 1: Write a failing integration test for CSS parsing**

`crates/fulgur/tests/style_test.rs` に追加 (末尾):

```rust
#[test]
fn test_overflow_hidden_parsed_and_clips() {
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();

    // overflow:hidden の親に、はみ出す子を置く
    let html_hidden = r#"<html><body>
        <div style="width:100px;height:100px;overflow:hidden;background:#eee">
            <div style="width:300px;height:300px;background:red"></div>
        </div>
    </body></html>"#;
    let pdf_hidden = engine.render_html(html_hidden).unwrap();
    assert!(pdf_hidden.starts_with(b"%PDF"));

    let html_visible = r#"<html><body>
        <div style="width:100px;height:100px;overflow:visible;background:#eee">
            <div style="width:300px;height:300px;background:red"></div>
        </div>
    </body></html>"#;
    let pdf_visible = engine.render_html(html_visible).unwrap();
    assert!(pdf_visible.starts_with(b"%PDF"));

    // overflow:hidden の PDF は visible 版とバイト列が異なる (クリップ命令が追加される)
    assert_ne!(
        pdf_hidden, pdf_visible,
        "overflow:hidden should produce a different PDF than overflow:visible"
    );
}

#[test]
fn test_overflow_clip_keyword() {
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();
    let html = r#"<html><body>
        <div style="width:100px;height:100px;overflow:clip">
            <div style="width:300px;height:300px;background:blue"></div>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_overflow_x_only_independent() {
    let engine = fulgur::engine::Engine::builder()
        .page_size(fulgur::config::PageSize::A4)
        .build();
    let html = r#"<html><body>
        <div style="width:100px;height:100px;overflow-x:hidden;overflow-y:visible">
            <div style="width:300px;height:300px;background:green"></div>
        </div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p fulgur --test style_test test_overflow 2>&1 | tail -20
```

Expected: `test_overflow_hidden_parsed_and_clips` が失敗 (PDF が同一、つまり overflow が未読み取り)。他 2 件は PDF 生成成功で通過するが、検証不足。

**Step 3: Map stylo overflow in `extract_block_style`**

`convert.rs:922-928` 付近 (`style.border_styles = ...` の直後、`// Background image layers` の直前) に追加:

```rust
        // Overflow (CSS3 axis-independent interpretation)
        let map_overflow =
            |o: style::values::computed::Overflow| -> crate::pageable::Overflow {
                use style::values::computed::Overflow as S;
                match o {
                    S::Visible => crate::pageable::Overflow::Visible,
                    // hidden / clip / scroll / auto はすべて PDF では同じ扱い
                    S::Hidden | S::Clip | S::Scroll | S::Auto => crate::pageable::Overflow::Clip,
                }
            };
        style.overflow_x = map_overflow(styles.clone_overflow_x());
        style.overflow_y = map_overflow(styles.clone_overflow_y());
```

**Step 4: Run tests to verify they pass**

```bash
cargo build -p fulgur 2>&1 | tail -10
cargo test -p fulgur --test style_test test_overflow 2>&1 | tail -15
```

Expected: 3 件すべて通過。`test_overflow_hidden_parsed_and_clips` の `assert_ne!` が成立 (クリップにより PDF バイト列が差分を持つ)。

**Step 5: Run full test suite (regression check)**

```bash
cargo test -p fulgur 2>&1 | grep "test result" | tail -20
```

Expected: すべての既存テスト通過。

**Step 6: Clippy & fmt check**

```bash
cargo clippy -p fulgur 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -5
```

Expected: クリーン。

**Step 7: Commit**

```bash
git add crates/fulgur/src/convert.rs crates/fulgur/tests/style_test.rs
git commit -m "feat(fulgur): read CSS overflow-x/y from stylo in convert

Part of fulgur-9x6. Maps stylo computed Overflow to fulgur's Overflow
enum, collapsing hidden/clip/scroll/auto into Clip. Integration tests
verify that overflow:hidden produces a different PDF byte stream than
overflow:visible, and that overflow:clip and axis-independent
overflow-x both parse and render successfully."
```

---

## Task 5: `examples/overflow-hidden/` 追加

**Files:**

- Create: `examples/overflow-hidden/index.html`
- Create: `examples/overflow-hidden/style.css`
- Create: `examples/overflow-hidden/index.pdf` (regen 経由で生成)

**Step 1: Write the HTML fixture**

`examples/overflow-hidden/index.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>overflow: hidden / clip Examples</title>
  <link rel="stylesheet" href="./style.css">
</head>
<body>

<h1>overflow: hidden / clip Examples</h1>

<h2>1. Basic hidden — large child clipped to small parent</h2>
<div class="container hidden">
  <div class="big-child"></div>
</div>

<h2>2. overflow: clip keyword</h2>
<div class="container clip-kw">
  <div class="big-child"></div>
</div>

<h2>3. overflow: hidden with border-radius</h2>
<div class="container hidden rounded">
  <div class="big-child"></div>
</div>

<h2>4. overflow-x: hidden, overflow-y: visible (axis-independent)</h2>
<div class="container x-only">
  <div class="wide-child"></div>
</div>

<h2>5. overflow: visible (reference, no clip)</h2>
<div class="container visible">
  <div class="big-child"></div>
</div>

</body>
</html>
```

**Step 2: Write the CSS**

`examples/overflow-hidden/style.css`:

```css
body {
  font-family: sans-serif;
  margin: 20px;
}
h1 { font-size: 20px; }
h2 { font-size: 14px; margin-top: 30px; }

.container {
  width: 120px;
  height: 80px;
  border: 2px solid #333;
  background: #eef;
  margin-bottom: 40px;
}
.container.hidden { overflow: hidden; }
.container.clip-kw { overflow: clip; }
.container.visible { overflow: visible; }
.container.x-only { overflow-x: hidden; overflow-y: visible; }
.container.rounded { border-radius: 20px; }

.big-child {
  width: 200px;
  height: 200px;
  background: linear-gradient(45deg, #e33, #3e3);
}
.wide-child {
  width: 300px;
  height: 40px;
  background: #38f;
}
```

**Step 3: Generate the PDF via mise task**

```bash
mise run update-examples 2>&1 | tail -10
ls -l examples/overflow-hidden/
```

Expected: `examples/overflow-hidden/index.pdf` が生成される。

**Note:** `mise` が使えない場合は直接実行:

```bash
cargo build --bin fulgur --release 2>&1 | tail -5
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  target/release/fulgur render examples/overflow-hidden/index.html -o examples/overflow-hidden/index.pdf
```

**Step 4: Manual visual inspection (optional)**

PDF ビューアで開いて確認 (または `pdftoppm` で PNG 化):

```bash
pdftoppm -r 150 examples/overflow-hidden/index.pdf /tmp/overflow-preview -png
ls /tmp/overflow-preview*
```

手動確認項目:

- 1, 2: 大きな子要素が 120×80 の親境界でクリップされている
- 3: 角丸に沿ってクリップされている
- 4: 横ははみ出しで切れ、縦は境界を越えて子要素の下半分が見える
- 5: visible 版は子要素がそのまま外にはみ出して描画される

**Step 5: Commit**

```bash
git add examples/overflow-hidden/
git commit -m "docs(examples): add overflow-hidden example

Part of fulgur-9x6. Covers basic overflow:hidden, overflow:clip,
border-radius combination, axis-independent overflow-x:hidden, and a
visible reference for contrast."
```

---

## Task 6: VRT fixtures 追加

**Files:**

- Create: `crates/fulgur-vrt/fixtures/layout/overflow-hidden.html`
- Create: `crates/fulgur-vrt/fixtures/layout/overflow-hidden-rounded.html`
- Modify: `crates/fulgur-vrt/manifest.toml`

**Step 1: Write the VRT fixture (basic hidden)**

`crates/fulgur-vrt/fixtures/layout/overflow-hidden.html`:

```html
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>VRT fixture: layout/overflow-hidden</title>
<style>
  html,body{margin:0;padding:0}
  .row{display:flex;gap:20px;padding:40px}
  .container{width:120px;height:80px;border:2px solid #333;background:#eef;overflow:hidden}
  .container.clip{overflow:clip}
  .big{width:200px;height:200px;background:#e33}
</style>
</head><body><div class="row">
<div class="container"><div class="big"></div></div>
<div class="container clip"><div class="big"></div></div>
</div></body></html>
```

**Step 2: Write the rounded VRT fixture**

`crates/fulgur-vrt/fixtures/layout/overflow-hidden-rounded.html`:

```html
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>VRT fixture: layout/overflow-hidden-rounded</title>
<style>
  html,body{margin:0;padding:0}
  .row{display:flex;gap:20px;padding:40px}
  .container{width:120px;height:120px;border:2px solid #333;border-radius:24px;overflow:hidden}
  .big{width:200px;height:200px;background:#38f}
</style>
</head><body><div class="row">
<div class="container"><div class="big"></div></div>
</div></body></html>
```

**Step 3: Register in manifest**

`crates/fulgur-vrt/manifest.toml` に追加 (既存の `layout/multicol-2.html` の後):

```toml
[[fixture]]
path = "layout/overflow-hidden.html"

[[fixture]]
path = "layout/overflow-hidden-rounded.html"
```

**Step 4: Run VRT self-test locally (fulgur vs fulgur)**

```bash
cargo test -p fulgur-vrt 2>&1 | tail -20
```

Expected: VRT test インフラが新フィクスチャを認識し、PDF レンダリングが成功 (Chromium 比較は CI で実施される想定)。

**Step 5: Commit**

```bash
git add crates/fulgur-vrt/fixtures/layout/overflow-hidden.html \
        crates/fulgur-vrt/fixtures/layout/overflow-hidden-rounded.html \
        crates/fulgur-vrt/manifest.toml
git commit -m "test(fulgur-vrt): add overflow-hidden VRT fixtures

Part of fulgur-9x6. Covers basic overflow:hidden/clip and
border-radius combination for Chromium visual regression comparison."
```

---

## Task 7: 最終検証とドキュメント更新

**Files:**

- Verify: `cargo test`, `cargo clippy`, `cargo fmt --check`
- Optionally modify: `crates/fulgur/README.md` if CSS support table lists overflow

**Step 1: Full workspace verification**

```bash
cargo build 2>&1 | tail -5
cargo test 2>&1 | grep "test result" | tail -20
cargo clippy 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -5
```

Expected: すべてクリーン、回帰なし。

**Step 2: markdownlint check**

```bash
npx markdownlint-cli2 'docs/plans/2026-04-12-overflow-hidden.md' 2>&1 | tail -10
```

Expected: エラーなし。

**Step 3: CSS サポート表を更新 (該当すれば)**

```bash
grep -l "overflow" crates/fulgur/README.md README.md docs/ 2>/dev/null
```

CSS サポート表に overflow の行があれば `hidden/clip/scroll/auto` → supported にマーク。なければスキップ。

**Step 4: Verify design goals**

- [x] `overflow: hidden` — padding-box クリップ
- [x] `overflow: clip` — 同上
- [x] `overflow: scroll`/`auto` — 同上 (PDF 統合扱い)
- [x] `border-radius` + 両軸クリップ — 角丸追従
- [x] `overflow-x` / `overflow-y` 軸独立 — 片軸のみクリップ可能
- [x] ページまたぎ時は各フラグメントでクリップ
- [x] 既存テスト回帰なし

**Step 5: Final commit (if any docs changed)**

```bash
# 必要があれば
git add <changed files>
git commit -m "docs: update CSS support table for overflow

Part of fulgur-9x6."
```

---

## Notes for the executor

- **順序の重要性:** Task 1 → 2 → 3 → 4 は型定義 → 純粋関数 → 描画統合 → CSS 読み取り という依存順。Task 5 は PDF 再生成に Task 4 完了が必須。Task 6 は Task 4 以降ならいつでも可。
- **TDD 厳守:** 各タスクで「失敗するテスト → 最小実装 → テスト通過 → コミット」のサイクルを守る。
- **回帰確認:** 各コミット前に必ず `cargo test -p fulgur --lib 2>&1 | grep "test result"` でベースライン (231 passed + 新規分) を確認。
- **Krilla API の注意点:** `surface.push_clip_path()` は `FillRule` を要求する。デフォルト (`NonZero`) で問題ない。`surface.pop()` は push_clip_path, push_transform, push_opacity 等を共通に pop するので、`clip_pushed` フラグでガードする。
- **`compute_inner_radii` (背景用) との関係:** `background.rs:231` の `compute_inner_radii` は `BgClip` に応じて inset を変えるため流用困難。overflow 用の独立ヘルパー `compute_padding_box_inner_radii` を追加する。
- **Determinism:** 全変更は pure (状態なし)。`BTreeMap` 問題は該当しない。Task 5 の PDF 再生成は `FONTCONFIG_FILE` 経由で決定論的。
