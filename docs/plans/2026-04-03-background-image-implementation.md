# background-image: url() Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Support CSS `background-image: url()` with full background-* property suite including multiple layers and `background-clip: text`.

**Architecture:** Add `BackgroundLayer` struct to `BlockStyle`, extract CSS background properties from Stylo in `convert.rs`, create new `background.rs` module for all background rendering logic (color + image layers). Krilla's `draw_image()` for rendering, `push_clip_path()` for clipping.

**Tech Stack:** Rust, Stylo (CSS computed values), Krilla (PDF rendering), Blitz (DOM/layout)

---

### Task 1: Data Structures — Background Enums and Structs

**Files:**

- Modify: `crates/fulgur/src/pageable.rs:104-119` (BlockStyle area)
- Modify: `crates/fulgur/src/image.rs` (make ImageFormat public for reuse)

**Step 1: Write unit tests for new types**

Add to `crates/fulgur/src/pageable.rs` at the bottom (inside a `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod background_tests {
    use super::*;

    #[test]
    fn test_background_layer_defaults() {
        let style = BlockStyle::default();
        assert!(style.background_layers.is_empty());
    }

    #[test]
    fn test_has_visual_style_with_background_layer() {
        let mut style = BlockStyle::default();
        assert!(!style.has_visual_style());
        style.background_layers.push(BackgroundLayer {
            image_data: std::sync::Arc::new(vec![]),
            format: crate::image::ImageFormat::Png,
            intrinsic_width: 100.0,
            intrinsic_height: 100.0,
            size: BgSize::Auto,
            position_x: BgLengthPercentage::Percentage(0.0),
            position_y: BgLengthPercentage::Percentage(0.0),
            repeat_x: BgRepeat::Repeat,
            repeat_y: BgRepeat::Repeat,
            origin: BgBox::PaddingBox,
            clip: BgClip::BorderBox,
        });
        assert!(style.has_visual_style());
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib background_tests`
Expected: FAIL — types don't exist yet

**Step 3: Add type definitions**

In `crates/fulgur/src/pageable.rs`, after the `BorderStyleValue` enum (line ~143), add:

```rust
// ─── Background types ────────────────────────────────────

/// A length or percentage value for background positioning/sizing.
#[derive(Clone, Debug)]
pub enum BgLengthPercentage {
    /// Absolute length in points.
    Length(f32),
    /// Percentage (0.0–1.0).
    Percentage(f32),
}

/// CSS `background-size` value.
#[derive(Clone, Debug)]
pub enum BgSize {
    /// `auto` — use intrinsic image size.
    Auto,
    /// `cover` — scale to fill, may crop.
    Cover,
    /// `contain` — scale to fit, may letterbox.
    Contain,
    /// Explicit `<width> <height>`. `None` means `auto` for that axis.
    Explicit(Option<BgLengthPercentage>, Option<BgLengthPercentage>),
}

/// CSS `background-repeat` single-axis keyword.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgRepeat {
    Repeat,
    NoRepeat,
    Space,
    Round,
}

/// CSS box model reference for `background-origin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgBox {
    BorderBox,
    PaddingBox,
    ContentBox,
}

/// CSS `background-clip` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgClip {
    BorderBox,
    PaddingBox,
    ContentBox,
    Text,
}

/// A single CSS background image layer with all associated properties.
#[derive(Clone, Debug)]
pub struct BackgroundLayer {
    pub image_data: Arc<Vec<u8>>,
    pub format: ImageFormat,
    pub intrinsic_width: f32,
    pub intrinsic_height: f32,
    pub size: BgSize,
    pub position_x: BgLengthPercentage,
    pub position_y: BgLengthPercentage,
    pub repeat_x: BgRepeat,
    pub repeat_y: BgRepeat,
    pub origin: BgBox,
    pub clip: BgClip,
}
```

Add `use std::sync::Arc;` and `use crate::image::ImageFormat;` at the top of pageable.rs if not already present.

Add `background_layers` to `BlockStyle`:

```rust
pub struct BlockStyle {
    pub background_color: Option<[u8; 4]>,
    pub background_layers: Vec<BackgroundLayer>,  // ADD THIS
    // ... rest unchanged
}
```

Update `has_visual_style()` to include background layers:

```rust
pub fn has_visual_style(&self) -> bool {
    self.background_color.is_some()
        || !self.background_layers.is_empty()
        || self.border_widths.iter().any(|&w| w > 0.0)
        || self.padding.iter().any(|&p| p > 0.0)
}
```

In `crates/fulgur/src/image.rs`, ensure `ImageFormat` derives `Copy`:

```rust
#[derive(Clone, Copy, Debug)]
pub enum ImageFormat {
```

Also add `pub` to the `convert.rs` import if needed — `ImageFormat` is already public.

**Step 4: Run tests**

Run: `cargo test --lib background_tests`
Expected: PASS

**Step 5: Verify full test suite**

Run: `cargo test --lib`
Expected: All existing tests pass (background_layers defaults to empty Vec via Default)

**Step 6: Commit**

```bash
git add crates/fulgur/src/pageable.rs crates/fulgur/src/image.rs
git commit -m "feat: add background image data structures (BackgroundLayer, BgSize, BgRepeat, BgClip)"
```

---

### Task 2: Image Dimension Decoder

**Files:**

- Modify: `crates/fulgur/src/image.rs`

We need to decode image dimensions from headers without adding a dependency. PNG uses IHDR chunk, JPEG uses SOF marker, GIF has a fixed header.

**Step 1: Write failing tests**

Add to `crates/fulgur/src/image.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // 1x1 red PNG
    const MINIMAL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
        0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41,
        0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
        0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92,
        0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
        0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    // 1x1 white GIF89a
    const MINIMAL_GIF: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61,
        0x01, 0x00, 0x01, 0x00, // 1x1
        0x00, 0x00, 0x00, // flags, bg, aspect
        0x3B, // trailer
    ];

    #[test]
    fn test_png_dimensions() {
        let dims = ImagePageable::decode_dimensions(MINIMAL_PNG, ImageFormat::Png);
        assert_eq!(dims, Some((1, 1)));
    }

    #[test]
    fn test_gif_dimensions() {
        let dims = ImagePageable::decode_dimensions(MINIMAL_GIF, ImageFormat::Gif);
        assert_eq!(dims, Some((1, 1)));
    }

    #[test]
    fn test_truncated_data_returns_none() {
        let dims = ImagePageable::decode_dimensions(&[0x89, 0x50], ImageFormat::Png);
        assert_eq!(dims, None);
    }
}
```

**Step 2: Run tests to verify failure**

Run: `cargo test --lib image::tests`
Expected: FAIL — `decode_dimensions` doesn't exist

**Step 3: Implement decode_dimensions**

Add to `ImagePageable` impl:

```rust
/// Decode image dimensions (width, height) from header bytes.
/// Returns None if the data is too short or malformed.
pub fn decode_dimensions(data: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    match format {
        ImageFormat::Png => {
            // PNG IHDR: bytes 16..20 = width (BE u32), 20..24 = height (BE u32)
            if data.len() < 24 {
                return None;
            }
            let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
            let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
            Some((w, h))
        }
        ImageFormat::Gif => {
            // GIF header: bytes 6..8 = width (LE u16), 8..10 = height (LE u16)
            if data.len() < 10 {
                return None;
            }
            let w = u16::from_le_bytes([data[6], data[7]]) as u32;
            let h = u16::from_le_bytes([data[8], data[9]]) as u32;
            Some((w, h))
        }
        ImageFormat::Jpeg => {
            // JPEG: scan for SOF0 (0xFFC0) through SOF15 markers
            // SOF marker structure: FF Cx, then 2-byte length, then:
            //   1 byte precision, 2 bytes height (BE), 2 bytes width (BE)
            if data.len() < 2 {
                return None;
            }
            let mut i = 2; // skip SOI (FF D8)
            while i + 1 < data.len() {
                if data[i] != 0xFF {
                    i += 1;
                    continue;
                }
                let marker = data[i + 1];
                i += 2;
                // SOF0..SOF15 (0xC0..0xCF), excluding DHT (0xC4) and DAC (0xCC)
                if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xCC {
                    if i + 7 > data.len() {
                        return None;
                    }
                    let h = u16::from_be_bytes([data[i + 1], data[i + 2]]) as u32;
                    let w = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
                    return Some((w, h));
                }
                // Skip segment: read 2-byte length
                if i + 1 >= data.len() {
                    return None;
                }
                let seg_len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
                i += seg_len;
            }
            None
        }
    }
}
```

**Step 4: Run tests**

Run: `cargo test --lib image::tests`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/fulgur/src/image.rs
git commit -m "feat: add image dimension decoder for PNG, JPEG, GIF headers"
```

---

### Task 3: CSS Property Extraction from Stylo

**Files:**

- Modify: `crates/fulgur/src/convert.rs:488-590` (extract_block_style)

Extract `background-image`, `background-size`, `background-position-x/y`, `background-repeat`, `background-origin`, `background-clip` from Stylo computed styles into `BackgroundLayer` structs.

**Step 1: Write integration test**

Create `crates/fulgur/tests/background_test.rs`:

```rust
use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

// Minimal 1x1 red PNG (69 bytes)
const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn build_engine() -> Engine {
    let mut assets = AssetBundle::new();
    assets.add_image("bg.png", MINIMAL_PNG.to_vec());
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build()
}

#[test]
fn test_background_image_renders_to_pdf() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png)">Hello</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));

    // PDF with background image should be larger than without
    let pdf_no_bg = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render_html(html)
        .unwrap();
    assert!(
        pdf.len() > pdf_no_bg.len(),
        "PDF with background-image ({} bytes) should be larger than without ({} bytes)",
        pdf.len(),
        pdf_no_bg.len()
    );
}

#[test]
fn test_background_no_repeat() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png);background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_size_cover() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png);background-size:cover;background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_size_contain() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png);background-size:contain;background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_position_center() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg.png);background-position:center;background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_multiple_layers() {
    let mut assets = AssetBundle::new();
    assets.add_image("bg1.png", MINIMAL_PNG.to_vec());
    assets.add_image("bg2.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;background-image:url(bg1.png),url(bg2.png);background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_clip_padding_box() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;padding:20px;border:5px solid black;background-image:url(bg.png);background-clip:padding-box;background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}

#[test]
fn test_background_origin_content_box() {
    let engine = build_engine();
    let html = r#"<html><body>
        <div style="width:200px;height:200px;padding:20px;background-image:url(bg.png);background-origin:content-box;background-repeat:no-repeat">Content</div>
    </body></html>"#;
    let pdf = engine.render_html(html).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p fulgur --test background_test -- --test-threads=1`
Expected: PASS for compilation (no rendering logic yet, but should not crash), images won't appear yet

Note: This test won't strictly "fail" at this point — it tests that the engine doesn't crash. The actual image presence is validated by PDF size comparison which will fail until Task 4 (rendering) is complete.

**Step 3: Implement CSS extraction in convert.rs**

Add the following imports at the top of `convert.rs`:

```rust
use crate::image::ImageFormat;
use crate::pageable::{
    BackgroundLayer, BgBox, BgClip, BgLengthPercentage, BgRepeat, BgSize,
    // ... existing imports
};
```

In `extract_block_style()`, after the border-styles extraction (line ~586) and before the closing `}` of the `if let Some(styles)` block, add:

```rust
// Background image layers
if let Some(assets) = assets {
    let bg_images = styles.clone_background_image();
    let bg_sizes = styles.clone_background_size();
    let bg_pos_x = styles.clone_background_position_x();
    let bg_pos_y = styles.clone_background_position_y();
    let bg_repeats = styles.clone_background_repeat();
    let bg_origins = styles.clone_background_origin();
    let bg_clips = styles.clone_background_clip();

    for (i, image) in bg_images.0.iter().enumerate() {
        use style::values::computed::image::Image;
        if let Image::Url(ref url) = image {
            let src = match url {
                style::values::computed::url::ComputedUrl::Valid(ref u) => u.as_str(),
                style::values::computed::url::ComputedUrl::Invalid(ref s) => s.as_str(),
            };
            if let Some(data) = assets.get_image(src) {
                if let Some(format) = ImagePageable::detect_format(data) {
                    let (iw, ih) = ImagePageable::decode_dimensions(data, format)
                        .unwrap_or((1, 1));

                    let size = convert_bg_size(&bg_sizes.0, i);
                    let (px, py) = convert_bg_position(&bg_pos_x.0, &bg_pos_y.0, i, width, height);
                    let (rx, ry) = convert_bg_repeat(&bg_repeats.0, i);
                    let origin = convert_bg_origin(&bg_origins.0, i);
                    let clip = convert_bg_clip(&bg_clips.0, i);

                    style_out.background_layers.push(BackgroundLayer {
                        image_data: Arc::clone(data),
                        format,
                        intrinsic_width: iw as f32,
                        intrinsic_height: ih as f32,
                        size,
                        position_x: px,
                        position_y: py,
                        repeat_x: rx,
                        repeat_y: ry,
                        origin,
                        clip,
                    });
                }
            }
        }
    }
}
```

Note: `extract_block_style` needs an `assets` parameter now. Update its signature:

```rust
fn extract_block_style(node: &Node, assets: Option<&AssetBundle>) -> BlockStyle {
```

Update all call sites of `extract_block_style` to pass `ctx.assets` (or `None` where context isn't available).

Add the converter helper functions after `extract_block_style`:

```rust
fn convert_bg_size(sizes: &[style::values::computed::BackgroundSize], i: usize) -> BgSize {
    use style::values::generics::background::BackgroundSize as StyloBS;
    use style::values::generics::length::GenericLengthPercentageOrAuto as LPAuto;

    let s = &sizes[i % sizes.len()];
    match s {
        StyloBS::Cover => BgSize::Cover,
        StyloBS::Contain => BgSize::Contain,
        StyloBS::ExplicitSize { width, height } => {
            let w = match width {
                LPAuto::Auto => None,
                LPAuto::LengthPercentage(lp) => Some(convert_lp_to_bg(&lp.0)),
            };
            let h = match height {
                LPAuto::Auto => None,
                LPAuto::LengthPercentage(lp) => Some(convert_lp_to_bg(&lp.0)),
            };
            if w.is_none() && h.is_none() {
                BgSize::Auto
            } else {
                BgSize::Explicit(w, h)
            }
        }
    }
}

fn convert_lp_to_bg(lp: &style::values::computed::LengthPercentage) -> BgLengthPercentage {
    if let Some(pct) = lp.to_percentage() {
        BgLengthPercentage::Percentage(pct.0)
    } else {
        BgLengthPercentage::Length(lp.to_length().map(|l| l.px()).unwrap_or(0.0))
    }
}

fn convert_bg_position(
    pos_x: &[style::values::computed::LengthPercentage],
    pos_y: &[style::values::computed::LengthPercentage],
    i: usize,
    _container_w: f32,
    _container_h: f32,
) -> (BgLengthPercentage, BgLengthPercentage) {
    let px = &pos_x[i % pos_x.len()];
    let py = &pos_y[i % pos_y.len()];
    (convert_lp_to_bg(px), convert_lp_to_bg(py))
}

fn convert_bg_repeat(
    repeats: &[style::values::specified::background::BackgroundRepeat],
    i: usize,
) -> (BgRepeat, BgRepeat) {
    use style::values::specified::background::BackgroundRepeatKeyword as BRK;
    let r = &repeats[i % repeats.len()];
    let map = |k: BRK| match k {
        BRK::Repeat => BgRepeat::Repeat,
        BRK::NoRepeat => BgRepeat::NoRepeat,
        BRK::Space => BgRepeat::Space,
        BRK::Round => BgRepeat::Round,
    };
    (map(r.0), map(r.1))
}

fn convert_bg_origin(
    origins: &[style::properties::longhands::background_origin::computed_value::T],
    i: usize,
) -> BgBox {
    use style::properties::longhands::background_origin::computed_value::T as O;
    match origins[i % origins.len()] {
        O::BorderBox => BgBox::BorderBox,
        O::PaddingBox => BgBox::PaddingBox,
        O::ContentBox => BgBox::ContentBox,
    }
}

fn convert_bg_clip(
    clips: &[style::properties::longhands::background_clip::computed_value::T],
    i: usize,
) -> BgClip {
    use style::properties::longhands::background_clip::computed_value::T as C;
    match clips[i % clips.len()] {
        C::BorderBox => BgClip::BorderBox,
        C::PaddingBox => BgClip::PaddingBox,
        C::ContentBox => BgClip::ContentBox,
    }
}
```

Note on `background-clip: text`: Stylo's `background_clip` enum doesn't include `Text` (it's non-standard / behind a flag). For now, map all values to their box equivalents. We'll handle `-webkit-background-clip: text` in Task 8 by checking for the vendor-prefixed property separately.

**Step 4: Fix call sites**

Search for all calls to `extract_block_style(node)` in convert.rs and update to `extract_block_style(node, ctx.assets)`. Key locations:

- `convert_node()` — around line 189
- `convert_image()` — around line 267
- `convert_table()` — around line 297
- Any other callers (grep for `extract_block_style`)

For `convert_image()`, which currently takes `assets: Option<&AssetBundle>` as a direct param, pass it through.

**Step 5: Run tests**

Run: `cargo test --lib`
Expected: All 94+ unit tests pass

Run: `cargo test -p fulgur --test background_test -- --test-threads=1`
Expected: Tests pass (no crash; image size comparison may still fail — that's expected until rendering is done)

**Step 6: Commit**

```bash
git add crates/fulgur/src/convert.rs crates/fulgur/tests/background_test.rs
git commit -m "feat: extract CSS background-image properties from Stylo computed styles"
```

---

### Task 4: Background Rendering Module — Color + Single Image

**Files:**

- Create: `crates/fulgur/src/background.rs`
- Modify: `crates/fulgur/src/lib.rs` (add `pub mod background;`)
- Modify: `crates/fulgur/src/pageable.rs` (replace `draw_block_background` calls)

Move background-color drawing and add single-image `no-repeat` + `auto` size rendering.

**Step 1: Write unit tests for size/position calculations**

Create `crates/fulgur/src/background.rs` with tests:

```rust
//! Background rendering: color fills and image layers.

use std::sync::Arc;

use crate::image::ImageFormat;
use crate::pageable::{
    BackgroundLayer, BgBox, BgClip, BgLengthPercentage, BgRepeat, BgSize, BlockStyle, Canvas,
};

/// Resolved image dimensions after applying background-size.
struct ResolvedSize {
    width: f32,
    height: f32,
}

/// Resolve background-size for a single layer.
fn resolve_size(layer: &BackgroundLayer, origin_w: f32, origin_h: f32) -> ResolvedSize {
    let iw = layer.intrinsic_width;
    let ih = layer.intrinsic_height;
    if iw <= 0.0 || ih <= 0.0 {
        return ResolvedSize { width: 0.0, height: 0.0 };
    }
    let aspect = iw / ih;

    match &layer.size {
        BgSize::Auto => ResolvedSize { width: iw, height: ih },
        BgSize::Cover => {
            let scale = (origin_w / iw).max(origin_h / ih);
            ResolvedSize { width: iw * scale, height: ih * scale }
        }
        BgSize::Contain => {
            let scale = (origin_w / iw).min(origin_h / ih);
            ResolvedSize { width: iw * scale, height: ih * scale }
        }
        BgSize::Explicit(w, h) => {
            let resolved_w = w.as_ref().map(|v| resolve_lp(v, origin_w));
            let resolved_h = h.as_ref().map(|v| resolve_lp(v, origin_h));
            match (resolved_w, resolved_h) {
                (Some(rw), Some(rh)) => ResolvedSize { width: rw, height: rh },
                (Some(rw), None) => ResolvedSize { width: rw, height: rw / aspect },
                (None, Some(rh)) => ResolvedSize { width: rh * aspect, height: rh },
                (None, None) => ResolvedSize { width: iw, height: ih },
            }
        }
    }
}

/// Resolve a BgLengthPercentage to absolute points.
fn resolve_lp(lp: &BgLengthPercentage, basis: f32) -> f32 {
    match lp {
        BgLengthPercentage::Length(v) => *v,
        BgLengthPercentage::Percentage(p) => basis * p,
    }
}

/// Resolve background-position for one axis.
/// CSS spec: position = (container_size - image_size) * percentage, or just length offset.
fn resolve_position(lp: &BgLengthPercentage, container_size: f32, image_size: f32) -> f32 {
    match lp {
        BgLengthPercentage::Length(v) => *v,
        BgLengthPercentage::Percentage(p) => (container_size - image_size) * p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_layer(iw: f32, ih: f32, size: BgSize) -> BackgroundLayer {
        BackgroundLayer {
            image_data: Arc::new(vec![]),
            format: ImageFormat::Png,
            intrinsic_width: iw,
            intrinsic_height: ih,
            size,
            position_x: BgLengthPercentage::Percentage(0.0),
            position_y: BgLengthPercentage::Percentage(0.0),
            repeat_x: BgRepeat::NoRepeat,
            repeat_y: BgRepeat::NoRepeat,
            origin: BgBox::PaddingBox,
            clip: BgClip::BorderBox,
        }
    }

    #[test]
    fn test_size_auto() {
        let layer = make_layer(100.0, 50.0, BgSize::Auto);
        let s = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(s.width, 100.0);
        assert_eq!(s.height, 50.0);
    }

    #[test]
    fn test_size_cover_wider() {
        let layer = make_layer(100.0, 50.0, BgSize::Cover);
        let s = resolve_size(&layer, 200.0, 200.0);
        // aspect 2:1, need to cover 200x200 → scale by max(200/100, 200/50) = 4
        assert_eq!(s.width, 400.0);
        assert_eq!(s.height, 200.0);
    }

    #[test]
    fn test_size_contain() {
        let layer = make_layer(100.0, 50.0, BgSize::Contain);
        let s = resolve_size(&layer, 200.0, 200.0);
        // scale by min(200/100, 200/50) = 2
        assert_eq!(s.width, 200.0);
        assert_eq!(s.height, 100.0);
    }

    #[test]
    fn test_size_explicit_both() {
        let layer = make_layer(100.0, 50.0, BgSize::Explicit(
            Some(BgLengthPercentage::Length(150.0)),
            Some(BgLengthPercentage::Percentage(0.5)),
        ));
        let s = resolve_size(&layer, 200.0, 200.0);
        assert_eq!(s.width, 150.0);
        assert_eq!(s.height, 100.0); // 200 * 0.5
    }

    #[test]
    fn test_size_explicit_width_only() {
        let layer = make_layer(100.0, 50.0, BgSize::Explicit(
            Some(BgLengthPercentage::Length(200.0)),
            None,
        ));
        let s = resolve_size(&layer, 300.0, 300.0);
        assert_eq!(s.width, 200.0);
        assert_eq!(s.height, 100.0); // preserve aspect: 200 / (100/50)
    }

    #[test]
    fn test_position_percentage() {
        // 50% with container=200, image=100 → offset = (200-100)*0.5 = 50
        let offset = resolve_position(&BgLengthPercentage::Percentage(0.5), 200.0, 100.0);
        assert_eq!(offset, 50.0);
    }

    #[test]
    fn test_position_length() {
        let offset = resolve_position(&BgLengthPercentage::Length(30.0), 200.0, 100.0);
        assert_eq!(offset, 30.0);
    }
}
```

**Step 2: Run tests**

Run: `cargo test --lib background::tests`
Expected: PASS (pure math functions, no rendering deps)

**Step 3: Implement draw_background and draw_background_layer**

Add the rendering functions to `background.rs`:

```rust
/// Draw all background layers for a block element.
///
/// Draws background-color first, then image layers in reverse order
/// (last declared = bottom-most, first declared = top-most per CSS spec).
pub fn draw_background(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    draw_background_color(canvas, style, x, y, w, h);

    for layer in style.background_layers.iter().rev() {
        draw_background_layer(canvas, style, layer, x, y, w, h);
    }
}

/// Draw the background color fill.
fn draw_background_color(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let Some(bg) = &style.background_color else {
        return;
    };
    let path = build_background_path(style, x, y, w, h);
    if let Some(path) = path {
        canvas.surface.set_fill(Some(krilla::paint::Fill {
            paint: krilla::color::rgb::Color::new(bg[0], bg[1], bg[2]).into(),
            opacity: krilla::num::NormalizedF32::new(bg[3] as f32 / 255.0)
                .unwrap_or(krilla::num::NormalizedF32::ONE),
            rule: Default::default(),
        }));
        canvas.surface.set_stroke(None);
        canvas.surface.draw_path(&path);
    }
}

/// Build a rect or rounded-rect path for the block area.
fn build_background_path(
    style: &BlockStyle,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> Option<krilla::geom::Path> {
    if style.has_radius() {
        crate::pageable::build_rounded_rect_path(x, y, w, h, &style.border_radii)
    } else if let Some(rect) = krilla::geom::Rect::from_xywh(x, y, w, h) {
        let mut pb = krilla::geom::PathBuilder::new();
        pb.push_rect(rect);
        pb.finish()
    } else {
        None
    }
}

/// Compute the origin rectangle based on background-origin.
fn compute_origin_rect(
    style: &BlockStyle,
    origin: &BgBox,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> (f32, f32, f32, f32) {
    let bw = &style.border_widths;
    let pad = &style.padding;
    match origin {
        BgBox::BorderBox => (x, y, w, h),
        BgBox::PaddingBox => (
            x + bw[3],
            y + bw[0],
            w - bw[1] - bw[3],
            h - bw[0] - bw[2],
        ),
        BgBox::ContentBox => (
            x + bw[3] + pad[3],
            y + bw[0] + pad[0],
            w - bw[1] - bw[3] - pad[1] - pad[3],
            h - bw[0] - bw[2] - pad[0] - pad[2],
        ),
    }
}

/// Compute the clip rectangle based on background-clip.
fn compute_clip_rect(
    style: &BlockStyle,
    clip: &BgClip,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> (f32, f32, f32, f32) {
    let bw = &style.border_widths;
    let pad = &style.padding;
    match clip {
        BgClip::BorderBox => (x, y, w, h),
        BgClip::PaddingBox => (
            x + bw[3],
            y + bw[0],
            w - bw[1] - bw[3],
            h - bw[0] - bw[2],
        ),
        BgClip::ContentBox => (
            x + bw[3] + pad[3],
            y + bw[0] + pad[0],
            w - bw[1] - bw[3] - pad[1] - pad[3],
            h - bw[0] - bw[2] - pad[0] - pad[2],
        ),
        BgClip::Text => {
            // Text clip is handled separately; use padding-box as fallback rect
            (
                x + bw[3],
                y + bw[0],
                w - bw[1] - bw[3],
                h - bw[0] - bw[2],
            )
        }
    }
}

/// Draw a single background image layer.
fn draw_background_layer(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    layer: &BackgroundLayer,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    let (ox, oy, ow, oh) = compute_origin_rect(style, &layer.origin, x, y, w, h);
    let (cx, cy, cw, ch) = compute_clip_rect(style, &layer.clip, x, y, w, h);

    if ow <= 0.0 || oh <= 0.0 || cw <= 0.0 || ch <= 0.0 {
        return;
    }

    // Set up clip region
    let clip_path = {
        if let Some(rect) = krilla::geom::Rect::from_xywh(cx, cy, cw, ch) {
            let mut pb = krilla::geom::PathBuilder::new();
            pb.push_rect(rect);
            pb.finish()
        } else {
            None
        }
    };

    let has_clip = clip_path.is_some();
    if let Some(ref path) = clip_path {
        canvas.surface.push_clip_path(path, &krilla::paint::FillRule::default());
    }

    // Resolve image size
    let resolved = resolve_size(layer, ow, oh);
    if resolved.width <= 0.0 || resolved.height <= 0.0 {
        if has_clip {
            canvas.surface.pop();
        }
        return;
    }

    // Resolve position (relative to origin)
    let pos_x = ox + resolve_position(&layer.position_x, ow, resolved.width);
    let pos_y = oy + resolve_position(&layer.position_y, oh, resolved.height);

    // Create krilla Image
    let data: krilla::Data = Arc::clone(&layer.image_data).into();
    let image_result = match layer.format {
        ImageFormat::Png => krilla::image::Image::from_png(data, true),
        ImageFormat::Jpeg => krilla::image::Image::from_jpeg(data, true),
        ImageFormat::Gif => krilla::image::Image::from_gif(data, true),
    };
    let Ok(image) = image_result else {
        if has_clip {
            canvas.surface.pop();
        }
        return;
    };

    // Compute tile positions
    let tiles = compute_tile_positions(
        layer.repeat_x,
        layer.repeat_y,
        pos_x,
        pos_y,
        resolved.width,
        resolved.height,
        cx,
        cy,
        cw,
        ch,
    );

    // Draw each tile
    for (tx, ty, tw, th) in tiles {
        let Some(size) = krilla::geom::Size::from_wh(tw, th) else {
            continue;
        };
        let transform = krilla::geom::Transform::from_translate(tx, ty);
        canvas.surface.push_transform(&transform);
        canvas.surface.draw_image(image.clone(), size);
        canvas.surface.pop();
    }

    if has_clip {
        canvas.surface.pop();
    }
}

/// Compute tile positions for background-repeat.
/// Returns Vec of (x, y, width, height) for each tile.
fn compute_tile_positions(
    repeat_x: BgRepeat,
    repeat_y: BgRepeat,
    pos_x: f32,
    pos_y: f32,
    img_w: f32,
    img_h: f32,
    clip_x: f32,
    clip_y: f32,
    clip_w: f32,
    clip_h: f32,
) -> Vec<(f32, f32, f32, f32)> {
    let mut tiles = Vec::new();

    // Resolve tile size and spacing for each axis
    let (tile_w, space_x, start_x, end_x) =
        resolve_repeat_axis(repeat_x, pos_x, img_w, clip_x, clip_w);
    let (tile_h, space_y, start_y, end_y) =
        resolve_repeat_axis(repeat_y, pos_y, img_h, clip_y, clip_h);

    let mut ty = start_y;
    while ty < end_y + 0.01 {
        let mut tx = start_x;
        while tx < end_x + 0.01 {
            tiles.push((tx, ty, tile_w, tile_h));
            tx += tile_w + space_x;
            if repeat_x == BgRepeat::NoRepeat {
                break;
            }
        }
        ty += tile_h + space_y;
        if repeat_y == BgRepeat::NoRepeat {
            break;
        }
    }

    tiles
}

/// For a single axis, compute tile size, spacing, and start/end positions.
/// Returns (tile_size, spacing, start_pos, end_pos).
fn resolve_repeat_axis(
    repeat: BgRepeat,
    position: f32,
    image_size: f32,
    clip_start: f32,
    clip_size: f32,
) -> (f32, f32, f32, f32) {
    let clip_end = clip_start + clip_size;

    match repeat {
        BgRepeat::NoRepeat => (image_size, 0.0, position, position),
        BgRepeat::Repeat => {
            if image_size <= 0.0 {
                return (image_size, 0.0, position, position);
            }
            // Start tiling from position, extending in both directions to cover clip area
            let offset = ((position - clip_start) % image_size + image_size) % image_size;
            let start = clip_start - offset;
            (image_size, 0.0, start, clip_end)
        }
        BgRepeat::Space => {
            if image_size <= 0.0 || image_size > clip_size {
                return (image_size, 0.0, position, position);
            }
            let count = (clip_size / image_size).floor() as usize;
            if count <= 1 {
                // Only one tile fits — center it (no space to distribute)
                return (image_size, 0.0, position, position);
            }
            let total_space = clip_size - (count as f32 * image_size);
            let spacing = total_space / (count - 1) as f32;
            (image_size, spacing, clip_start, clip_end)
        }
        BgRepeat::Round => {
            if image_size <= 0.0 {
                return (image_size, 0.0, position, position);
            }
            let count = (clip_size / image_size).round().max(1.0);
            let adjusted_size = clip_size / count;
            (adjusted_size, 0.0, clip_start, clip_end)
        }
    }
}
```

**Step 4: Make build_rounded_rect_path public**

In `pageable.rs`, change `fn build_rounded_rect_path(` to `pub fn build_rounded_rect_path(` (line ~301).

**Step 5: Wire up in pageable.rs**

In `pageable.rs`, replace the call to `draw_block_background(canvas, &self.style, ...)` in `BlockPageable::draw()` (line ~943) with:

```rust
crate::background::draw_background(canvas, &self.style, x, y, total_width, total_height);
```

Also do the same replacement in any other `draw_block_background` call sites (e.g., `TablePageable::draw()` if it exists).

The old `draw_block_background` function can be removed from `pageable.rs` once all call sites are updated.

**Step 6: Register module**

In `lib.rs`, add:

```rust
pub mod background;
```

**Step 7: Run tests**

Run: `cargo test --lib`
Expected: All tests pass

Run: `cargo test -p fulgur --test background_test -- --test-threads=1`
Expected: `test_background_image_renders_to_pdf` now passes (PDF with bg image is larger)

**Step 8: Commit**

```bash
git add crates/fulgur/src/background.rs crates/fulgur/src/pageable.rs crates/fulgur/src/lib.rs
git commit -m "feat: add background.rs with image layer rendering (size, position, repeat, clip)"
```

---

### Task 5: Background-repeat Space and Round

**Files:**

- Modify: `crates/fulgur/src/background.rs` (already implemented in Task 4 within compute_tile_positions)

This is already implemented in `resolve_repeat_axis`. Add additional unit tests:

**Step 1: Add tests for space and round**

```rust
#[test]
fn test_repeat_space_three_tiles() {
    // clip=300, image=90 → 3 tiles fit, spacing = (300 - 270) / 2 = 15
    let (size, space, start, _end) = resolve_repeat_axis(BgRepeat::Space, 0.0, 90.0, 0.0, 300.0);
    assert_eq!(size, 90.0);
    assert_eq!(space, 15.0);
    assert_eq!(start, 0.0);
}

#[test]
fn test_repeat_round_adjusts_size() {
    // clip=300, image=110 → round(300/110)=3, adjusted=100
    let (size, space, start, _end) = resolve_repeat_axis(BgRepeat::Round, 0.0, 110.0, 0.0, 300.0);
    assert_eq!(size, 100.0);
    assert_eq!(space, 0.0);
    assert_eq!(start, 0.0);
}

#[test]
fn test_repeat_repeat_covers_clip() {
    let tiles = compute_tile_positions(
        BgRepeat::Repeat, BgRepeat::NoRepeat,
        50.0, 10.0,
        100.0, 100.0,
        0.0, 0.0, 300.0, 300.0,
    );
    // Should have tiles at x: -50, 50, 150, 250 (4 tiles covering 0..300)
    assert!(tiles.len() >= 3);
    assert!(tiles[0].0 <= 0.0); // first tile starts at or before clip start
}
```

**Step 2: Run tests**

Run: `cargo test --lib background::tests`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/fulgur/src/background.rs
git commit -m "test: add unit tests for background-repeat space and round modes"
```

---

### Task 6: Background-clip: text

**Files:**

- Modify: `crates/fulgur/src/background.rs`
- Modify: `crates/fulgur/src/pageable.rs` (pass text info to background drawing)

`background-clip: text` requires clipping the background to the text glyph outlines. This is an advanced feature that requires access to Parley's glyph outlines.

**Important constraint:** Stylo 0.8.0 with `servo` feature does NOT expose `background-clip: text` in its computed enum (only BorderBox/PaddingBox/ContentBox). This means we cannot get `text` from the standard CSS property.

**Approach:** Support `-webkit-background-clip: text` via a custom CSS property check or by detecting when the user explicitly sets it via inline styles. Since Blitz/Stylo doesn't support this value, we'll implement this as a **future enhancement** tracked in a separate issue.

**Step 1: Create a follow-up issue for background-clip: text**

Since Stylo doesn't expose this value in its computed styles, we cannot implement it without either:

1. Patching Stylo to recognize the value
2. Pre-parsing CSS to detect `-webkit-background-clip: text` before passing to Blitz
3. Using a custom attribute/class convention

Create a beads issue to track this:

```bash
bd create --title="background-clip: text 対応" --type=feature --priority=3
bd dep add <new-id> fulgur-e9b
```

**Step 2: Add a comment in convert.rs**

Where background-clip is extracted, add:

```rust
// Note: background-clip: text is not supported by Stylo's computed enum.
// See issue fulgur-XXX for future implementation.
```

**Step 3: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "docs: note background-clip: text limitation (Stylo doesn't expose it)"
```

---

### Task 7: Integration Tests Verification

**Files:**

- Modify: `crates/fulgur/tests/background_test.rs`

**Step 1: Run all integration tests**

Run: `cargo test -p fulgur --test background_test -- --test-threads=1`
Expected: All tests pass

**Step 2: Run full test suite**

Run: `cargo test --lib`
Expected: All tests pass

Run: `cargo clippy`
Expected: No warnings

Run: `cargo fmt --check`
Expected: Clean

**Step 3: Fix any issues found**

Address any clippy warnings or test failures.

**Step 4: Commit if changes needed**

```bash
git add -A
git commit -m "fix: address clippy warnings and test issues"
```

---

### Task 8: Cleanup and Final Verification

**Files:**

- All modified files

**Step 1: Remove dead code**

Remove the old `draw_block_background` from `pageable.rs` if not yet removed.

**Step 2: Run full verification**

```bash
cargo test --lib
cargo test -p fulgur --test background_test -- --test-threads=1
cargo test -p fulgur --test image_test -- --test-threads=1
cargo clippy
cargo fmt --check
```

**Step 3: Commit**

```bash
git add -A
git commit -m "refactor: remove old draw_block_background, final cleanup"
```
