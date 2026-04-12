# list-style-image: url() 対応 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Issue:** fulgur-507
**Goal:** `list-style-image: url(...)` でラスター (PNG/JPEG/GIF) と SVG 両方の画像をリストマーカーとして描画できるようにする。画像が解決できない場合はテキストマーカー (list-style-type) へ自動フォールバック。

**Architecture:**

- Blitz 0.2.4 は `list-style-image` を完全に無視するため、fulgur 側で stylo 計算済みスタイル (`clone_list_style_image()`) を直接読んで独自にマーカー画像を解決する。
- `ListItemPageable` のマーカー保持を `ListItemMarker` enum (Text / Image / None) に置き換え、画像マーカーは `ImageMarker::Raster(ImagePageable)` か `ImageMarker::Svg(SvgPageable)` を保持する。
- 画像サイズは line-height にクランプし、アスペクト比維持で幅を計算 (B方針)。
- 解決フローは既存の `background-image` パス (`convert.rs:930-978`) と同じパターンで `extract_asset_name` + `AssetBundle::get_image` を再利用する。

**Tech Stack:**

- Rust 1.x (edition 2024)
- blitz-dom 0.2.4 / stylo 0.8.0 (computed styles)
- krilla 0.7 / krilla-svg 0.7 (PDF + SVG)
- usvg 0.45 (SVG parsing)
- parley (既存のマーカーテキストシェイピング)

**Design 源:** beads issue `fulgur-507` の design フィールド参照

---

## Context for implementer

- `crates/fulgur/src/pageable.rs:1486-1610` — 現在の `ListItemPageable` 定義と impl。`marker_lines: Vec<ShapedLine>` と `marker_width: Pt` フィールドを enum に置き換える。
- `crates/fulgur/src/convert.rs:236-292` — list-item 検出分岐。画像マーカー解決を差し込む場所。
- `crates/fulgur/src/convert.rs:1103-1186` — `extract_marker_lines` 既存実装。
- `crates/fulgur/src/convert.rs:930-978` — 既存の `background-image` URL 解決パターン。`extract_asset_name` + `get_image` + `ImagePageable::detect_format`/`decode_dimensions` の再利用手本。
- `crates/fulgur/src/image.rs` — `ImagePageable` と `detect_format` / `decode_dimensions`。
- `crates/fulgur/src/svg.rs` — `SvgPageable` と `krilla-svg` 経由の描画。
- `crates/fulgur/src/asset.rs` — `AssetBundle`。`get_image(name)` で `Option<&Arc<Vec<u8>>>` を返す。
- `crates/fulgur/tests/background_test.rs:1-48` — AssetBundle 経由の PNG テストパターン。新テストで参考にする。
- `crates/fulgur/tests/list_test.rs` — 既存リストレンダリングテスト。
- `examples/image/` — 画像アセット付きexample のディレクトリ構造手本。

**重要な制約:**

- 決定的レンダリング: `BTreeMap` を使う (HashMap 禁止)。本 issue では新規マップは不要。
- markdownlint: Fenced code blocks に language ID、list 前後空行必須。
- `cargo fmt --check` CI 必須。

---

## Task 1: `clamp_marker_size` 純粋関数 (TDD)

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (既存の `#[cfg(test)] mod tests` 内にテスト追加、ファイル末尾に関数定義追加)

**Step 1: Write the failing test**

`crates/fulgur/src/pageable.rs` のテストモジュール内 (`test_list_item_split_keeps_marker_on_first_part` の直後あたり) に追加:

```rust
#[test]
fn test_clamp_marker_size_below_line_height() {
    // 16x16 px image (= 12x12 pt) with line-height 24 pt → stays intrinsic
    let (w, h) = clamp_marker_size(12.0, 12.0, 24.0);
    assert!((w - 12.0).abs() < 0.01);
    assert!((h - 12.0).abs() < 0.01);
}

#[test]
fn test_clamp_marker_size_equal_line_height() {
    let (w, h) = clamp_marker_size(24.0, 24.0, 24.0);
    assert!((w - 24.0).abs() < 0.01);
    assert!((h - 24.0).abs() < 0.01);
}

#[test]
fn test_clamp_marker_size_above_line_height_preserves_aspect() {
    // 64x48 pt with line-height 12 pt
    // scale = 12 / 48 = 0.25 → (16, 12)
    let (w, h) = clamp_marker_size(64.0, 48.0, 12.0);
    assert!((w - 16.0).abs() < 0.01);
    assert!((h - 12.0).abs() < 0.01);
}

#[test]
fn test_clamp_marker_size_zero_intrinsic_height_returns_zero() {
    let (w, h) = clamp_marker_size(10.0, 0.0, 12.0);
    assert_eq!(w, 0.0);
    assert_eq!(h, 0.0);
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test -p fulgur --lib test_clamp_marker_size 2>&1 | tail -15
```

Expected: FAIL with "cannot find function `clamp_marker_size` in this scope".

**Step 3: Implement `clamp_marker_size`**

`crates/fulgur/src/pageable.rs` の `ListItemPageable` impl の直前 (1486行あたり — `// ─── ListItemPageable ───` セクションヘッダの直後) に追加:

```rust
/// Clamp an intrinsic image size to a line-height limit while preserving
/// the aspect ratio. Used to size list-style-image markers so they match
/// the surrounding text's line-height.
///
/// Returns `(width, height)` in pt. If the intrinsic height is zero, both
/// return values are zero (avoids division by zero for malformed images).
pub(crate) fn clamp_marker_size(
    intrinsic_width: Pt,
    intrinsic_height: Pt,
    line_height: Pt,
) -> (Pt, Pt) {
    if intrinsic_height <= 0.0 {
        return (0.0, 0.0);
    }
    if intrinsic_height <= line_height {
        (intrinsic_width, intrinsic_height)
    } else {
        let scale = line_height / intrinsic_height;
        (intrinsic_width * scale, line_height)
    }
}
```

**Step 4: Run test to verify it passes**

```bash
cargo test -p fulgur --lib test_clamp_marker_size 2>&1 | tail -15
```

Expected: 4 tests pass.

**Step 5: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(fulgur): add clamp_marker_size helper for list-style-image

Pure function for sizing image markers: clamps intrinsic size to a
line-height limit while preserving aspect ratio. Preparation for
list-style-image: url() support (fulgur-507)."
```

---

## Task 2: `extract_marker_lines` に line_height 返却値を追加

**Files:**

- Modify: `crates/fulgur/src/convert.rs:1103-1186` (`extract_marker_lines`)
- Modify: `crates/fulgur/src/convert.rs:240` (呼び出し側)

**Step 1: Update function signature and implementation**

`extract_marker_lines` の戻り値を `(Vec<ShapedLine>, f32)` から `(Vec<ShapedLine>, f32, f32)` (shaped lines / width / line_height) に変更する。

戻り値の 3 番目の要素 `line_height` は、parley レイアウトの最初の行の `metrics().line_height` から取得する。shaped_lines が空のケース (Inside / list-item でない) では `0.0` を返す。

```rust
fn extract_marker_lines(
    doc: &blitz_dom::BaseDocument,
    node: &Node,
    ctx: &mut ConvertContext<'_>,
) -> (Vec<ShapedLine>, f32, f32) {
    let elem_data = match node.element_data() {
        Some(d) => d,
        None => return (Vec::new(), 0.0, 0.0),
    };
    let list_item_data = match &elem_data.list_item_data {
        Some(d) => d,
        None => return (Vec::new(), 0.0, 0.0),
    };
    let parley_layout = match &list_item_data.position {
        blitz_dom::node::ListItemLayoutPosition::Outside(layout) => layout,
        blitz_dom::node::ListItemLayoutPosition::Inside => return (Vec::new(), 0.0, 0.0),
    };

    // ... (既存の marker_text / shaped_lines ループはそのまま)

    let mut line_height_pt: f32 = 0.0;
    // 既存のループ内で最初のラインの line_height を捕捉
```

具体的な差分: 既存ループ内 `let metrics = line.metrics();` の直後に `if line_height_pt == 0.0 { line_height_pt = metrics.line_height; }` を追加。最終 return を `(shaped_lines, max_width, line_height_pt)` にする。

**Step 2: Update caller**

`crates/fulgur/src/convert.rs:240` の呼び出し:

```rust
let (marker_lines, marker_width) = extract_marker_lines(doc, node, ctx);
```

を

```rust
let (marker_lines, marker_width, marker_line_height) =
    extract_marker_lines(doc, node, ctx);
```

に変更。`marker_line_height` は次の task で ListItemPageable に渡すので、いったん `let _ = marker_line_height;` で warning を抑止して OK (次の task ですぐ使う)。

**Step 3: Run existing tests to verify no regression**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur --test list_test 2>&1 | tail -5
```

Expected: 全パス (既存挙動は変わらない、line_height はまだ使っていない)。

**Step 4: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "refactor(fulgur): extract_marker_lines returns line_height

Preparation for image marker vertical centering (fulgur-507).
No behavioral change — caller discards the new value for now."
```

---

## Task 3: `ListItemMarker` enum 導入 — Text/None のみ (Image は Task 5 で追加)

このタスクは**純粋な構造リファクタ**で、既存テキストマーカー挙動を保ちつつ enum 化のみ行う。画像マーカーは Task 5/7 で追加する。

**Files:**

- Modify: `crates/fulgur/src/pageable.rs:1486-1609` (`ListItemPageable` 定義と impl)
- Modify: `crates/fulgur/src/pageable.rs` 既存テスト 2 本 (`test_list_item_delegates_to_body`, `test_list_item_split_keeps_marker_on_first_part`)
- Modify: `crates/fulgur/src/convert.rs:280-290` (構築サイト)

**Step 1: Define the enum**

`crates/fulgur/src/pageable.rs` の `// ─── ListItemPageable ───` セクションヘッダの直後 (clamp_marker_size の上) に追加:

```rust
/// Marker attached to a `ListItemPageable`.
///
/// Exactly one variant holds valid content per list item, enforced by the
/// type system. `None` is used for the second fragment after a page-break
/// split (the marker only appears on the first fragment).
#[derive(Clone)]
pub enum ListItemMarker {
    /// Text marker with shaped glyph runs extracted from Blitz/Parley.
    Text {
        lines: Vec<crate::paragraph::ShapedLine>,
        width: Pt,
    },
    /// No marker — either `list-style-type: none` or the trailing split fragment.
    None,
}
```

(Image variant は Task 5 で追加)

**Step 2: Replace `ListItemPageable` fields**

`ListItemPageable` struct を以下に置換 (1490 行目あたり):

```rust
/// A list item with an outside-positioned marker.
#[derive(Clone)]
pub struct ListItemPageable {
    /// Marker (text, image, or none).
    pub marker: ListItemMarker,
    /// Line-height of the first shaped line — used to vertically center
    /// image markers. Zero for `ListItemMarker::None`.
    pub marker_line_height: Pt,
    /// The list item's body content.
    pub body: Box<dyn Pageable>,
    /// Visual style (background, borders, padding).
    pub style: BlockStyle,
    /// Taffy-computed width.
    pub width: Pt,
    /// Cached height from wrap().
    pub height: Pt,
    /// CSS opacity (0.0–1.0), applied to both marker and body.
    pub opacity: f32,
    /// CSS visibility (false = hidden).
    pub visible: bool,
}
```

**Step 3: Rewrite impl Pageable for ListItemPageable**

```rust
impl Pageable for ListItemPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        let body_size = self.body.wrap(avail_width, avail_height);
        self.height = body_size.height;
        Size {
            width: avail_width,
            height: self.height,
        }
    }

    fn split(
        &self,
        avail_width: Pt,
        avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        let (top_body, bottom_body) = self.body.split(avail_width, avail_height)?;
        Some((
            Box::new(ListItemPageable {
                marker: self.marker.clone(),
                marker_line_height: self.marker_line_height,
                body: top_body,
                style: self.style.clone(),
                width: self.width,
                height: 0.0,
                opacity: self.opacity,
                visible: self.visible,
            }),
            Box::new(ListItemPageable {
                marker: ListItemMarker::None,
                marker_line_height: 0.0,
                body: bottom_body,
                style: self.style.clone(),
                width: self.width,
                height: 0.0,
                opacity: self.opacity,
                visible: self.visible,
            }),
        ))
    }

    fn split_boxed(self: Box<Self>, avail_width: Pt, avail_height: Pt) -> SplitResult {
        let me = *self;
        let (top_body, bottom_body) = match me.body.split_boxed(avail_width, avail_height) {
            Ok(pair) => pair,
            Err(body) => {
                return Err(Box::new(ListItemPageable { body, ..me }));
            }
        };
        Ok((
            Box::new(ListItemPageable {
                marker: me.marker.clone(),
                marker_line_height: me.marker_line_height,
                body: top_body,
                style: me.style.clone(),
                width: me.width,
                height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
            }),
            Box::new(ListItemPageable {
                marker: ListItemMarker::None,
                marker_line_height: 0.0,
                body: bottom_body,
                style: me.style,
                width: me.width,
                height: 0.0,
                opacity: me.opacity,
                visible: me.visible,
            }),
        ))
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        draw_with_opacity(canvas, self.opacity, |canvas| {
            if self.visible {
                match &self.marker {
                    ListItemMarker::Text { lines, width } if !lines.is_empty() => {
                        let marker_x = x - width;
                        crate::paragraph::draw_shaped_lines(canvas, lines, marker_x, y);
                    }
                    _ => {}
                }
            }
            self.body.draw(canvas, x, y, avail_width, avail_height);
        });
    }

    fn pagination(&self) -> Pagination {
        self.body.pagination()
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.height
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
```

**Step 4: Update convert.rs construction site**

`crates/fulgur/src/convert.rs:280-290` の `ListItemPageable { ... }` を:

```rust
let mut item = ListItemPageable {
    marker: ListItemMarker::Text {
        lines: marker_lines,
        width: marker_width,
    },
    marker_line_height,
    body,
    style: BlockStyle::default(),
    width,
    height: 0.0,
    opacity,
    visible,
};
```

`use crate::pageable::` ブロックに `ListItemMarker` を追加。

**Step 5: Migrate existing pageable.rs tests**

`test_list_item_delegates_to_body`:

```rust
#[test]
fn test_list_item_delegates_to_body() {
    let body = make_spacer(100.0);
    let mut item = ListItemPageable {
        marker: ListItemMarker::Text {
            lines: Vec::new(),
            width: 20.0,
        },
        marker_line_height: 0.0,
        body,
        style: BlockStyle::default(),
        width: 200.0,
        height: 100.0,
        opacity: 1.0,
        visible: true,
    };
    let size = item.wrap(200.0, 1000.0);
    assert!((size.height - 100.0).abs() < 0.01);
}
```

`test_list_item_split_keeps_marker_on_first_part`:

```rust
#[test]
fn test_list_item_split_keeps_marker_on_first_part() {
    let mut body = BlockPageable::new(vec![
        make_spacer(100.0),
        make_spacer(100.0),
        make_spacer(100.0),
    ]);
    body.wrap(200.0, 1000.0);
    let mut item = ListItemPageable {
        marker: ListItemMarker::Text {
            lines: Vec::new(),
            width: 20.0,
        },
        marker_line_height: 14.0,
        body: Box::new(body),
        style: BlockStyle::default(),
        width: 200.0,
        height: 300.0,
        opacity: 1.0,
        visible: true,
    };
    item.wrap(200.0, 1000.0);
    let result = item.split(200.0, 250.0);
    assert!(result.is_some());
    let (first, second) = result.unwrap();
    // First part keeps the text marker
    let first_item = first.as_any().downcast_ref::<ListItemPageable>().unwrap();
    match &first_item.marker {
        ListItemMarker::Text { width, .. } => assert!((*width - 20.0).abs() < 0.01),
        _ => panic!("expected Text marker on first fragment"),
    }
    // Second part has no marker
    let second_item = second.as_any().downcast_ref::<ListItemPageable>().unwrap();
    assert!(matches!(second_item.marker, ListItemMarker::None));
}
```

**Step 6: Run all tests**

```bash
cargo build 2>&1 | tail -10
cargo test -p fulgur --lib 2>&1 | tail -10
cargo test -p fulgur --test list_test 2>&1 | tail -5
```

Expected: 全パス。リスト系の golden / integration テストにリグレッションなし。

**Step 7: `cargo fmt`**

```bash
cargo fmt
cargo fmt --check
```

Expected: exit 0。

**Step 8: Commit**

```bash
git add crates/fulgur/src/pageable.rs crates/fulgur/src/convert.rs
git commit -m "refactor(fulgur): ListItemMarker enum (Text/None)

Replace ListItemPageable.marker_lines / marker_width pair with an enum
so that Text/None/Image (coming next) become exclusive states. No
behavioral change — all existing list rendering still produces identical
output.

Preparation for list-style-image: url() support (fulgur-507)."
```

---

## Task 4: `detect_asset_kind` ヘルパーと tests (TDD)

**Files:**

- Modify: `crates/fulgur/src/image.rs` (末尾に追加)

**Step 1: Write failing tests**

`crates/fulgur/src/image.rs` の `#[cfg(test)] mod tests` 末尾に追加:

```rust
#[test]
fn test_detect_asset_kind_png() {
    assert!(matches!(
        AssetKind::detect(MINIMAL_PNG),
        AssetKind::Raster(ImageFormat::Png)
    ));
}

#[test]
fn test_detect_asset_kind_jpeg() {
    assert!(matches!(
        AssetKind::detect(MINIMAL_JPEG),
        AssetKind::Raster(ImageFormat::Jpeg)
    ));
}

#[test]
fn test_detect_asset_kind_gif() {
    assert!(matches!(
        AssetKind::detect(MINIMAL_GIF),
        AssetKind::Raster(ImageFormat::Gif)
    ));
}

#[test]
fn test_detect_asset_kind_svg_tag() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
    assert!(matches!(AssetKind::detect(svg), AssetKind::Svg));
}

#[test]
fn test_detect_asset_kind_svg_xml_prolog() {
    let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
    assert!(matches!(AssetKind::detect(svg), AssetKind::Svg));
}

#[test]
fn test_detect_asset_kind_svg_with_utf8_bom() {
    let svg = b"\xEF\xBB\xBF<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
    assert!(matches!(AssetKind::detect(svg), AssetKind::Svg));
}

#[test]
fn test_detect_asset_kind_empty() {
    assert!(matches!(AssetKind::detect(&[]), AssetKind::Unknown));
}

#[test]
fn test_detect_asset_kind_unknown() {
    assert!(matches!(
        AssetKind::detect(b"not an image"),
        AssetKind::Unknown
    ));
}
```

**Step 2: Run to verify fail**

```bash
cargo test -p fulgur --lib test_detect_asset_kind 2>&1 | tail -15
```

Expected: FAIL with "cannot find type `AssetKind`".

**Step 3: Implement `AssetKind`**

`crates/fulgur/src/image.rs` の `ImageFormat` enum の下に追加:

```rust
/// Classification of an asset's bytes for rendering path selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// Raster image (PNG/JPEG/GIF) → renders via `ImagePageable`.
    Raster(ImageFormat),
    /// SVG vector image → renders via `SvgPageable`.
    Svg,
    /// Unrecognized / unsupported.
    Unknown,
}

impl AssetKind {
    /// Classify raw asset bytes by sniffing the header.
    ///
    /// SVG detection accepts both the `<svg` element start and the
    /// `<?xml` prolog, and is tolerant of a leading UTF-8 BOM. Otherwise
    /// falls through to `ImageFormat::detect_format` for raster magic bytes.
    pub fn detect(data: &[u8]) -> AssetKind {
        if let Some(format) = ImagePageable::detect_format(data) {
            return AssetKind::Raster(format);
        }
        let mut slice = data;
        // Strip optional UTF-8 BOM so `<svg` / `<?xml` detection works.
        if slice.starts_with(b"\xEF\xBB\xBF") {
            slice = &slice[3..];
        }
        // Skip ASCII whitespace that commonly precedes the SVG root.
        while let Some((first, rest)) = slice.split_first() {
            if first.is_ascii_whitespace() {
                slice = rest;
            } else {
                break;
            }
        }
        if slice.starts_with(b"<?xml") || slice.starts_with(b"<svg") {
            return AssetKind::Svg;
        }
        AssetKind::Unknown
    }
}
```

**Step 4: Run tests to pass**

```bash
cargo test -p fulgur --lib test_detect_asset_kind 2>&1 | tail -20
```

Expected: 8 tests pass.

**Step 5: Commit**

```bash
git add crates/fulgur/src/image.rs
git commit -m "feat(fulgur): AssetKind::detect for raster/SVG classification

Introduces an enum that classifies asset bytes into Raster(ImageFormat)
/ Svg / Unknown, used by the list-style-image resolver to choose between
ImagePageable and SvgPageable (fulgur-507)."
```

---

## Task 5: `ListItemMarker::Image` variant + Raster 解決パス

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (enum に Image variant 追加、`ImageMarker` enum 追加、draw 対応)
- Modify: `crates/fulgur/src/convert.rs` (`resolve_list_marker` 新規、list-item 分岐差し替え)
- Create: `crates/fulgur/tests/list_style_image_test.rs` (新規 integration test file)

**Step 1: Extend `ListItemMarker` with Image variant**

`crates/fulgur/src/pageable.rs` の `ListItemMarker` enum に Image variant を追加:

```rust
use crate::image::ImagePageable;
use crate::svg::SvgPageable;

/// Image marker contents — either a raster image or a parsed SVG tree.
#[derive(Clone)]
pub enum ImageMarker {
    Raster(ImagePageable),
    Svg(SvgPageable),
}

#[derive(Clone)]
pub enum ListItemMarker {
    Text {
        lines: Vec<crate::paragraph::ShapedLine>,
        width: Pt,
    },
    Image {
        marker: ImageMarker,
        /// Display width after clamp (pt).
        width: Pt,
        /// Display height after clamp (pt).
        height: Pt,
    },
    None,
}
```

`ImagePageable` と `SvgPageable` の import が pageable.rs の既存 use ブロックに無ければ追加する。

**Step 2: Update `ListItemPageable::draw` to handle Image**

draw の match に Image 分岐を追加:

```rust
fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
    draw_with_opacity(canvas, self.opacity, |canvas| {
        if self.visible {
            match &self.marker {
                ListItemMarker::Text { lines, width } if !lines.is_empty() => {
                    let marker_x = x - width;
                    crate::paragraph::draw_shaped_lines(canvas, lines, marker_x, y);
                }
                ListItemMarker::Image { marker, width, height } => {
                    let marker_x = x - *width;
                    let marker_y = y + (self.marker_line_height - *height) / 2.0;
                    match marker {
                        ImageMarker::Raster(img) => {
                            img.draw(canvas, marker_x, marker_y, *width, *height);
                        }
                        ImageMarker::Svg(svg) => {
                            svg.draw(canvas, marker_x, marker_y, *width, *height);
                        }
                    }
                }
                _ => {}
            }
        }
        self.body.draw(canvas, x, y, avail_width, avail_height);
    });
}
```

**Step 3: Add `resolve_list_marker` in convert.rs — Raster only**

`crates/fulgur/src/convert.rs` の `extract_marker_lines` の直前に新規関数を追加 (1102行目あたり):

```rust
/// Resolve a list-style-image marker from the node's computed styles.
///
/// Returns `Some(ListItemMarker::Image { ... })` when:
/// - `list-style-image` is `Image::Url(...)`
/// - the URL resolves to an entry in `ctx.assets`
/// - the bytes are a supported format (PNG/JPEG/GIF or SVG)
///
/// Returns `None` when:
/// - no `list-style-image` set, `None`, or non-URL (e.g. gradient)
/// - no AssetBundle attached to the engine
/// - URL not found in bundle
/// - bytes are unsupported format or fail to parse
///
/// On `None` the caller must fall back to the text marker produced by
/// `extract_marker_lines`, matching the CSS spec's fallback semantics.
fn resolve_list_marker(
    node: &Node,
    line_height: f32,
    assets: Option<&AssetBundle>,
) -> Option<ListItemMarker> {
    use crate::image::{AssetKind, ImageFormat};
    use style::values::computed::image::Image;

    let assets = assets?;
    let styles = node.primary_styles()?;
    let image = styles.clone_list_style_image();
    let url = match image {
        Image::Url(u) => u,
        _ => return None,
    };
    let raw_src = match &url {
        style::servo::url::ComputedUrl::Valid(u) => u.as_str(),
        style::servo::url::ComputedUrl::Invalid(s) => s.as_str(),
    };
    let src = extract_asset_name(raw_src);
    let data = assets.get_image(src)?;
    match AssetKind::detect(data) {
        AssetKind::Raster(format) => {
            let (iw, ih) = ImagePageable::decode_dimensions(data, format).unwrap_or((1, 1));
            // px → pt (1px = 0.75pt, matching the rest of the image pipeline)
            let intrinsic_w = iw as f32 * 0.75;
            let intrinsic_h = ih as f32 * 0.75;
            let (width, height) = crate::pageable::clamp_marker_size(
                intrinsic_w, intrinsic_h, line_height,
            );
            let img = ImagePageable::new(Arc::clone(data), format, width, height);
            Some(ListItemMarker::Image {
                marker: ImageMarker::Raster(img),
                width,
                height,
            })
        }
        AssetKind::Svg => None, // Task 6 で追加
        AssetKind::Unknown => None,
    }
}
```

`use crate::pageable::{..., ImageMarker, ListItemMarker};` を use ブロックに追加。

**Step 4: Wire `resolve_list_marker` into the list-item branch**

`crates/fulgur/src/convert.rs:236-292` の分岐を:

```rust
if let Some(elem_data) = node.element_data()
    && elem_data.list_item_data.is_some()
{
    let (marker_lines, marker_width, marker_line_height) =
        extract_marker_lines(doc, node, ctx);
    let style = extract_block_style(node, ctx.assets);
    let (opacity, visible) = extract_opacity_visible(node);

    // Try list-style-image first; fall back to text marker if unresolved.
    let marker = resolve_list_marker(node, marker_line_height, ctx.assets)
        .unwrap_or(ListItemMarker::Text {
            lines: marker_lines,
            width: marker_width,
        });

    // ... body 構築は変更なし ...

    let mut item = ListItemPageable {
        marker,
        marker_line_height,
        body,
        style: BlockStyle::default(),
        width,
        height: 0.0,
        opacity,
        visible,
    };
```

**Step 5: Write integration test for PNG marker**

`crates/fulgur/tests/list_style_image_test.rs` を新規作成:

```rust
use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

// Minimal 16x16 red PNG — from examples/image/logo.png byte pattern or
// reuse the 1x1 MINIMAL_PNG from other tests. For this test we only
// need the bytes to decode, so 1x1 is fine.
const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn build_engine() -> Engine {
    let mut assets = AssetBundle::new();
    assets.add_image("bullet.png", MINIMAL_PNG.to_vec());
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build()
}

#[test]
fn test_list_style_image_png_renders_larger_than_text_only() {
    let engine = build_engine();
    let html = r#"<html><body>
        <ul style="list-style: disc url(bullet.png)">
            <li>Item one</li>
            <li>Item two</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));

    // Compare against a text-only baseline: PNG-embedded PDF must be larger
    let text_only = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render_html(
            r#"<html><body><ul><li>Item one</li><li>Item two</li></ul></body></html>"#,
        )
        .unwrap();
    assert!(
        pdf.len() > text_only.len(),
        "list-style-image PDF ({} bytes) should be larger than text-only ({} bytes)",
        pdf.len(),
        text_only.len()
    );
}

#[test]
fn test_list_style_image_unresolved_url_falls_back_to_text() {
    let engine = build_engine(); // bundle has bullet.png but not missing.png
    let html = r#"<html><body>
        <ul style="list-style: disc url(missing.png)">
            <li>Item</li>
        </ul>
    </body></html>"#;
    // Must not panic or error — falls through to text marker silently.
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
```

**Step 6: Run integration tests**

```bash
cargo test -p fulgur --test list_style_image_test 2>&1 | tail -15
cargo test -p fulgur --lib 2>&1 | tail -5
```

Expected: 2 new tests pass, all lib tests pass.

**Step 7: `cargo fmt` and clippy**

```bash
cargo fmt
cargo clippy -p fulgur 2>&1 | tail -20
```

Expected: exit 0, no new warnings.

**Step 8: Commit**

```bash
git add crates/fulgur/src/pageable.rs crates/fulgur/src/convert.rs crates/fulgur/tests/list_style_image_test.rs
git commit -m "feat(fulgur): list-style-image raster marker support

Introduces resolve_list_marker in convert.rs that reads the stylo-
computed list-style-image, looks up the URL in AssetBundle, classifies
the bytes via AssetKind, and constructs a ListItemMarker::Image::Raster
for PNG/JPEG/GIF. Unresolvable URLs silently fall back to the text
marker, matching CSS spec semantics.

Part of fulgur-507."
```

---

## Task 6: SVG 解決パス + integration test

**Files:**

- Modify: `crates/fulgur/src/convert.rs` (`resolve_list_marker` の `AssetKind::Svg` 分岐実装)
- Modify: `crates/fulgur/tests/list_style_image_test.rs` (SVG テスト追加)

**Step 1: Write failing test for SVG marker**

`list_style_image_test.rs` に追加:

```rust
const MINIMAL_SVG: &[u8] =
    br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"#;

#[test]
fn test_list_style_image_svg_renders() {
    let mut assets = AssetBundle::new();
    assets.add_image("bullet.svg", MINIMAL_SVG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();
    let html = r#"<html><body>
        <ul style="list-style: disc url(bullet.svg)">
            <li>SVG bullet item</li>
        </ul>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));

    // SVG-embedded PDF should differ in size from plain text-only baseline
    let text_only = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render_html(
            r#"<html><body><ul><li>SVG bullet item</li></ul></body></html>"#,
        )
        .unwrap();
    assert!(
        pdf.len() != text_only.len(),
        "PDF with SVG marker should differ from text-only baseline"
    );
}
```

**Step 2: Run test to verify fail**

```bash
cargo test -p fulgur --test list_style_image_test test_list_style_image_svg_renders 2>&1 | tail -15
```

Expected: PASS as plain text fallback (SVG branch currently returns None in Task 5). The size comparison might still pass by chance. Verify it actually uses SVG by adding a stronger assertion — or simply proceed with implementation since we know Task 5 returns None for SVG.

Actually a more reliable path: assert the byte size is *larger* than text-only (SVG drawing embeds a path tree). Update the test's last assert to `>` instead of `!=`:

```rust
assert!(
    pdf.len() > text_only.len(),
    "PDF with SVG marker ({}) should be larger than text-only ({})",
    pdf.len(),
    text_only.len()
);
```

With the AssetKind::Svg branch returning None in Task 5, resolve_list_marker returns None → fallback to text → pdf.len() should be ≈ text_only.len(). Test fails. Good.

**Step 3: Implement SVG branch**

`resolve_list_marker` の `AssetKind::Svg` 分岐を埋める:

```rust
AssetKind::Svg => {
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let intrinsic_w = size.width();
    let intrinsic_h = size.height();
    let (width, height) = crate::pageable::clamp_marker_size(
        intrinsic_w, intrinsic_h, line_height,
    );
    let svg = SvgPageable::new(Arc::new(tree), width, height);
    Some(ListItemMarker::Image {
        marker: ImageMarker::Svg(svg),
        width,
        height,
    })
}
```

`use usvg;` が convert.rs に必要 (既存 use を確認、無ければ追加)。`SvgPageable` は既にインポート済み (convert.rs:18)。

**Step 4: Run test to pass**

```bash
cargo test -p fulgur --test list_style_image_test 2>&1 | tail -15
```

Expected: 3 tests pass (PNG, fallback, SVG).

**Step 5: `cargo fmt` + clippy**

```bash
cargo fmt
cargo clippy -p fulgur 2>&1 | tail -20
```

**Step 6: Commit**

```bash
git add crates/fulgur/src/convert.rs crates/fulgur/tests/list_style_image_test.rs
git commit -m "feat(fulgur): list-style-image SVG marker support

Extends resolve_list_marker to parse SVG data via usvg::Tree::from_data
and wrap it in SvgPageable. Sizing uses the tree's intrinsic size
clamped to line-height.

Completes list-style-image PNG/JPEG/GIF/SVG coverage (fulgur-507)."
```

---

## Task 7: split behavior test for image marker

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` (テスト追加)

**Step 1: Write test**

`pageable.rs` のテストモジュールに追加:

```rust
#[test]
fn test_list_item_image_marker_split_keeps_on_first_part() {
    use crate::image::{ImageFormat, ImagePageable};
    use std::sync::Arc;

    let mut body = BlockPageable::new(vec![
        make_spacer(100.0),
        make_spacer(100.0),
        make_spacer(100.0),
    ]);
    body.wrap(200.0, 1000.0);

    // Dummy PNG bytes are not needed — we only exercise clone/split logic
    let img = ImagePageable::new(Arc::new(vec![0u8; 4]), ImageFormat::Png, 12.0, 12.0);

    let mut item = ListItemPageable {
        marker: ListItemMarker::Image {
            marker: ImageMarker::Raster(img),
            width: 12.0,
            height: 12.0,
        },
        marker_line_height: 14.0,
        body: Box::new(body),
        style: BlockStyle::default(),
        width: 200.0,
        height: 300.0,
        opacity: 1.0,
        visible: true,
    };
    item.wrap(200.0, 1000.0);
    let result = item.split(200.0, 250.0);
    assert!(result.is_some());
    let (first, second) = result.unwrap();

    let first_item = first.as_any().downcast_ref::<ListItemPageable>().unwrap();
    assert!(matches!(
        first_item.marker,
        ListItemMarker::Image { .. }
    ));

    let second_item = second.as_any().downcast_ref::<ListItemPageable>().unwrap();
    assert!(matches!(second_item.marker, ListItemMarker::None));
    assert_eq!(second_item.marker_line_height, 0.0);
}
```

**Step 2: Run test**

```bash
cargo test -p fulgur --lib test_list_item_image_marker_split 2>&1 | tail -15
```

Expected: PASS (実装は Task 3 で既に完成している、テストが後追いで仕様を固定するだけ)。

**Step 3: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "test(fulgur): split keeps image marker on first list fragment only

Verifies that ListItemMarker::Image fragments correctly into Image on
the first split and None on the trailing fragment — same contract as
the text marker case (fulgur-507)."
```

---

## Task 8: Example HTML + assets + mise/workflow glob update

**Files:**

- Create: `examples/list-style-image/index.html`
- Create: `examples/list-style-image/style.css`
- Create: `examples/list-style-image/bullet.png`
- Create: `examples/list-style-image/bullet.svg`
- Modify: `mise.toml`
- Modify: `.github/workflows/update-examples.yml`

**Step 1: Create example HTML**

`examples/list-style-image/index.html`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>list-style-image Examples</title>
  <link rel="stylesheet" href="./style.css">
</head>
<body>

<h1>list-style-image Examples</h1>

<h2>1. PNG bullet</h2>
<ul class="png-bullet">
  <li>First item with PNG marker</li>
  <li>Second item</li>
  <li>Third item</li>
</ul>

<h2>2. SVG bullet</h2>
<ul class="svg-bullet">
  <li>Vector marker via usvg + krilla-svg</li>
  <li>Scales with line-height</li>
</ul>

<h2>3. Shorthand fallback (list-style: disc url(missing))</h2>
<ul class="missing-bullet">
  <li>Missing image falls back to text marker</li>
  <li>Second item still shows disc</li>
</ul>

<h2>4. Mixed with other content</h2>
<ul class="png-bullet">
  <li>Item with <strong>bold</strong> and <em>italic</em></li>
  <li>Another item</li>
</ul>

</body>
</html>
```

`examples/list-style-image/style.css`:

```css
body {
  font-family: "Noto Sans", sans-serif;
  line-height: 1.5;
}

ul.png-bullet {
  list-style: disc url(bullet.png);
}

ul.svg-bullet {
  list-style: disc url(bullet.svg);
}

ul.missing-bullet {
  list-style: disc url(missing.png);
}
```

**Step 2: Create PNG asset**

8x8 の黒丸 PNG を作る。Python one-liner でも `cargo run` でも OK:

```bash
python3 -c "
import struct, zlib
w = h = 8
# 8x8 solid black RGBA
raw = b''.join(b'\x00' + b'\x00\x00\x00\xFF' * w for _ in range(h))
def chunk(t, d):
    crc = zlib.crc32(t + d)
    return struct.pack('>I', len(d)) + t + d + struct.pack('>I', crc)
sig = b'\x89PNG\r\n\x1a\n'
ihdr = struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0)
idat = zlib.compress(raw)
png = sig + chunk(b'IHDR', ihdr) + chunk(b'IDAT', idat) + chunk(b'IEND', b'')
open('examples/list-style-image/bullet.png', 'wb').write(png)
"
```

Verify:

```bash
file examples/list-style-image/bullet.png
```

Expected: `PNG image data, 8 x 8, 8-bit/color RGBA, non-interlaced`

**Step 3: Create SVG asset**

`examples/list-style-image/bullet.svg`:

```xml
<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8" viewBox="0 0 8 8">
  <circle cx="4" cy="4" r="3" fill="#0066cc"/>
</svg>
```

**Step 4: Update mise.toml glob to include *.svg**

`mise.toml` の `for img in` ループを:

```bash
for img in "${dir}"*.png "${dir}"*.jpg "${dir}"*.gif "${dir}"*.svg; do
```

**Step 5: Update workflow glob**

`.github/workflows/update-examples.yml` の同じループ:

```bash
for img in "${dir}"*.png "${dir}"*.jpg "${dir}"*.gif "${dir}"*.svg; do
```

**Step 6: Regenerate golden PDF**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-507-list-style-image
mise run update-examples 2>&1 | tail -20
```

Expected: `examples/list-style-image/index.pdf` 生成。他の examples の golden にリグレッションがないこと (git diff で確認)。

```bash
git status examples/
git diff --stat examples/
```

Expected: 新規 `examples/list-style-image/*` のみ追加、既存 `examples/*/index.pdf` に変更なし。もし他の example に差分があったら、それは別原因なので報告してリカバリする。

**Step 7: Render smoke check**

生成された PDF の先頭を確認:

```bash
head -c 8 examples/list-style-image/index.pdf | xxd
```

Expected: `25 50 44 46` (`%PDF`).

**Step 8: Commit**

```bash
git add examples/list-style-image/ mise.toml .github/workflows/update-examples.yml
git commit -m "docs(examples): add list-style-image example with PNG/SVG bullets

Adds examples/list-style-image/ with PNG marker, SVG marker, and a
fallback case (missing.png → text marker). Extends mise.toml and the
update-examples workflow to glob *.svg alongside other image formats
so the regeneration script picks up the new asset.

Part of fulgur-507."
```

---

## Task 9: Full CI gate

**Step 1: fmt**

```bash
cargo fmt --check
```

Expected: exit 0. 失敗したら `cargo fmt` で修正 → 再 commit (amend ではなく新 commit で):

```bash
cargo fmt
git add -u
git commit -m "chore: cargo fmt"
```

**Step 2: clippy**

```bash
cargo clippy --all-targets 2>&1 | tail -30
```

Expected: warnings 0。新 warning が出たら修正する。`-D warnings` は使わず、まず目視で判断。

**Step 3: cargo test**

```bash
cargo test --lib 2>&1 | tail -10
cargo test -p fulgur 2>&1 | tail -20
cargo test -p fulgur --test gcpm_integration 2>&1 | tail -10
```

Expected: 全パス (ベースライン 231 + 新規 ≥13)。

**Step 4: markdownlint**

```bash
npx markdownlint-cli2 '**/*.md' 2>&1 | tail -10
```

Expected: exit 0。存在しない html ファイルはスキップされる。

**Step 5: Verify beads state**

```bash
bd show fulgur-507
```

Expected: status `in_progress`。close するのは最終確認後 (blueprint:impl の Step 6)。

**Step 6: Report completion**

実装完了を報告。verification-before-completion スキルと finishing-a-development-branch スキルに引き継ぐ。

---

## Exit criteria (fulgur-507 の acceptance criteria 全て)

1. `list-style-image: url(bullet.png)` で PNG マーカーが正しく描画される
2. `list-style-image: url(bullet.svg)` で SVG マーカーが正しく描画される
3. JPEG / GIF もラスターマーカーとして動作する (`AssetKind::detect` + `ImagePageable::detect_format` 経由で自動対応)
4. 解決できない URL は静かにテキストマーカー (list-style-type) へフォールバック
5. マーカー高さが line-height にクランプされ、アスペクト比維持で幅が計算される
6. ページまたぎ時、マーカーは最初のフラグメントのみ描画され後続は `ListItemMarker::None`
7. CSS opacity / visibility が画像マーカーにも継承される (既存 `draw_with_opacity` 層で自動)
8. `examples/list-style-image/index.pdf` の golden PDF が決定的に再現する (4パターン)
9. cargo fmt --check / cargo clippy / cargo test --lib / cargo test -p fulgur / markdownlint-cli2 `**/*.md` 全通過

---

## Notes / potential pitfalls

- `styles.clone_list_style_image()` の戻り値型は `style::values::computed::image::Image` で、`background-image` と同一。use ステートメントは既存の background パスと揃える。
- `extract_asset_name` は stylo が絶対 URL (`file:///`) に解決したものを AssetBundle key に戻すユーティリティ。この関数は convert.rs 内の private helper として既存。再利用する際は private visibility のまま resolve_list_marker から呼べる (同じファイル内なので問題なし)。
- usvg::Tree::size() の単位は pt (user unit) なので px 変換不要。background-image の raster パスとは意図的に非対称。
- SVG の `viewBox` がない + 幅高さが %指定のケース: usvg が 0x0 を返す可能性があり、`clamp_marker_size` のゼロ判定でゼロ幅マーカーになる。本 issue スコープでは pragmatic behavior として許容 (例では明示的な width/height を持つ SVG を使う)。
- 既存 `test_list_item_delegates_to_body` と `test_list_item_split_keeps_marker_on_first_part` を壊さないこと。これらは Task 3 で enum 版に書き換え済み。
- `cargo fmt` は CI 必須。毎 commit 前に必ず実行。
- パフォーマンス: `resolve_list_marker` は li ごとに stylo 呼び出し + URL 解決 + バイト分類を行う。ホットパスではあるが既存の background-image 処理と同等コストなので問題なし。
