# box-shadow blur: gradient 9-slice Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the blur=0 fallback in `draw_single_box_shadow` with a gradient 9-slice approach that approximates Gaussian blur using opaque pre-composited colors (page background = white default).

**Architecture:** Compute blur edge color stops via Gaussian erf approximation pre-composited with the page background color. Divide the shadow region into 9 slices: 4 RadialGradient corners, 4 LinearGradient edges, and 1 solid-fill center. Apply EvenOdd clip to exclude the element's border-box interior from all slices.

**Tech Stack:** Rust, `crates/fulgur/src/background.rs`, `krilla::paint::{LinearGradient, RadialGradient, Stop}`, `krilla::color::rgb::Color`

---

### Task 1: Add `blur_stops` helper and unit test

**Files:**

- Modify: `crates/fulgur/src/background.rs`

**Step 1: Write the failing test**

Add this test at the bottom of `background.rs` inside the existing `#[cfg(test)] mod tests` block (search for `mod tests` near the end of the file):

```rust
#[test]
fn blur_stops_opaque_endpoints() {
    // First stop must equal the shadow color (fully composited).
    // Last stop must equal the bg color (alpha=0 composited).
    let stops = blur_stops([200, 100, 50, 255], 8, [255, 255, 255, 255]);
    assert!(stops.len() >= 2);
    // First stop: opaque shadow color composited with white = shadow color
    let first = stops[0];
    assert_eq!(first.offset, krilla::num::NormalizedF32::ZERO);
    // opacity must be 1.0 (pre-composited, no transparency)
    assert_eq!(first.opacity, krilla::num::NormalizedF32::ONE);
    // Last stop: alpha=0 shadow composited with white = white
    let last = stops[stops.len() - 1];
    assert_eq!(last.offset, krilla::num::NormalizedF32::ONE);
    assert_eq!(last.opacity, krilla::num::NormalizedF32::ONE);
    // last color must be bg (white = rgb(255,255,255))
    assert_eq!(last.color, krilla::color::rgb::Color::new(255, 255, 255).into());
}

#[test]
fn blur_stops_monotonic_alpha_decay() {
    // Shadow alpha at each stop must decrease (or stay equal) from first to last.
    // We test this by checking that the red channel of the pre-composited color
    // moves monotonically toward the bg red (255) when shadow red < 255.
    let stops = blur_stops([0, 0, 0, 255], 8, [255, 255, 255, 255]);
    // For black shadow on white bg: pre-composited color's red channel
    // should increase from 0 toward 255 as offset increases.
    // We can't access Color components directly, so just verify stop count >= 7.
    assert!(stops.len() >= 7);
}
```

**Step 2: Run test to verify it fails**

```bash
cd /home/ubuntu/fulgur/.worktrees/feat/fulgur-0xz-box-shadow-blur
cargo test -p fulgur --lib blur_stops 2>&1 | tail -10
```

Expected: `error[E0425]: cannot find function 'blur_stops'`

**Step 3: Implement `blur_stops`**

Add the following functions to `background.rs`, before the `mod tests` block:

```rust
/// Approximate `erfc(x)` for x >= 0 using Abramowitz & Stegun formula 7.1.26.
/// Maximum error: 1.5 × 10⁻⁷.
fn erfc_approx(x: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = t * (0.254829592
        + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    poly * (-x * x).exp()
}

/// Alpha at blur-edge position `t` ∈ [0, 1] (0 = inner edge, 1 = outer edge).
/// Models the cumulative Gaussian with σ = blur/3.
fn blur_edge_alpha(t: f32) -> f32 {
    // alpha(x) = 0.5 * erfc(x / (σ√2)) where x = t*blur, σ = blur/3
    // → 0.5 * erfc(t * 3 / √2)
    0.5 * erfc_approx(t * 3.0 / std::f32::consts::SQRT_2)
}

/// Build gradient stops for a blur edge, pre-composited with `bg`.
///
/// All stops have `opacity = NormalizedF32::ONE` (no PDF transparency group needed).
/// `shadow_rgba[3]` is the shadow's own alpha; it is folded into the compositing math
/// so callers need not pre-multiply.
pub(crate) fn blur_stops(
    shadow_rgba: [u8; 4],
    n: usize,
    bg: [u8; 4],
) -> Vec<krilla::paint::Stop> {
    assert!(n >= 2);
    let shadow_a = shadow_rgba[3] as f32 / 255.0;
    (0..n)
        .map(|i| {
            let t = i as f32 / (n - 1) as f32;
            let alpha = blur_edge_alpha(t) * shadow_a;
            let blend = |s: u8, b: u8| -> u8 {
                let r = s as f32 / 255.0 * alpha + b as f32 / 255.0 * (1.0 - alpha);
                (r * 255.0).round().clamp(0.0, 255.0) as u8
            };
            let r = blend(shadow_rgba[0], bg[0]);
            let g = blend(shadow_rgba[1], bg[1]);
            let b_ch = blend(shadow_rgba[2], bg[2]);
            krilla::paint::Stop {
                offset: krilla::num::NormalizedF32::new(t).unwrap_or(krilla::num::NormalizedF32::ONE),
                color: krilla::color::rgb::Color::new(r, g, b_ch).into(),
                opacity: krilla::num::NormalizedF32::ONE,
            }
        })
        .collect()
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test -p fulgur --lib blur_stops 2>&1 | tail -10
```

Expected: `test result: ok. 2 passed`

**Step 5: Commit**

```bash
cd /home/ubuntu/fulgur/.worktrees/feat/fulgur-0xz-box-shadow-blur
git add crates/fulgur/src/background.rs
git commit -m "feat(background): add blur_stops helper with Gaussian erf approximation"
```

---

### Task 2: Implement `draw_blur_box_shadow` — edge strips

**Files:**

- Modify: `crates/fulgur/src/background.rs`

**Step 1: Add smoke test for blur rendering**

Add to `crates/fulgur/tests/render_smoke.rs` — find the test `test_render_html_shadow_blur_warning_path` and replace the body comment with one that reflects the new behavior. Also add a new more specific test:

```rust
#[test]
fn test_render_html_shadow_blur_gradient_path() {
    // blur > 0 with spread and offset → exercises draw_blur_box_shadow.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;background:#fff;
                    box-shadow:4px 4px 8px 2px rgba(0,0,0,0.6);"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render blurred shadow with offset and spread");
    assert!(!pdf.is_empty());
}

#[test]
fn test_render_html_shadow_blur_rounded() {
    // blur > 0 with border-radius → exercises RadialGradient corner slices.
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width:100px;height:100px;background:#fff;border-radius:12px;
                    box-shadow:0 0 10px 0 black;"></div>
    </body></html>"#;
    let pdf = Engine::builder()
        .build()
        .render_html(html)
        .expect("render blurred shadow with border-radius");
    assert!(!pdf.is_empty());
}
```

**Step 2: Run test to verify it fails (or builds but exercises warn path)**

```bash
cd /home/ubuntu/fulgur/.worktrees/feat/fulgur-0xz-box-shadow-blur
cargo test -p fulgur --test render_smoke test_render_html_shadow_blur_gradient_path 2>&1 | tail -10
```

Expected: passes but draws blur=0 (not yet the real blur). These tests won't fail yet; they will be used to catch panics or regressions in later tasks.

**Step 3: Add `draw_blur_box_shadow` function (edge strips only, corners stubbed)**

Add the following function to `background.rs` after `draw_single_box_shadow`:

```rust
/// Draw a blurred box-shadow using a gradient 9-slice approach.
///
/// All gradient stops are pre-composited with `bg_color` so no PDF transparency
/// is required (PDF/A-1 compatible when bg_color matches the actual page background).
///
/// # Geometry
///
/// ```text
/// outer rect = inner rect expanded by blur on all sides
/// inner rect = (x + offset_x - spread, y + offset_y - spread, w + 2*spread, h + 2*spread)
/// ```
///
/// The 9 slices (TL corner, top edge, TR corner, left edge, center, right edge,
/// BL corner, bottom edge, BR corner) are drawn within an EvenOdd clip that
/// excludes the element's border-box interior.
#[allow(clippy::too_many_arguments)]
fn draw_blur_box_shadow(
    canvas: &mut Canvas<'_, '_>,
    style: &BlockStyle,
    shadow: &crate::draw_primitives::BoxShadow,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    bg_color: [u8; 4],
) {
    let blur = shadow.blur;
    if blur <= 0.0 {
        return;
    }

    // inner rect: shadow shape after spread
    let ix = x + shadow.offset_x - shadow.spread;
    let iy = y + shadow.offset_y - shadow.spread;
    let iw = w + 2.0 * shadow.spread;
    let ih = h + 2.0 * shadow.spread;
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }

    // outer rect: inner rect expanded by blur
    let ox = ix - blur;
    let oy = iy - blur;
    let ow = iw + 2.0 * blur;
    let oh = ih + 2.0 * blur;

    // Corner radii for the inner rect (after spread)
    let r_inner = expand_radii(&style.border_radii, shadow.spread);
    // Radii for the outer edge of the blur zone: r_inner + blur
    let r_outer: [[f32; 2]; 4] = std::array::from_fn(|i| {
        [r_inner[i][0] + blur, r_inner[i][1] + blur]
    });
    let _ = r_outer; // used in Task 3

    // EvenOdd clip: outer bbox minus border-box (same logic as draw_single_box_shadow)
    let clip_path = {
        let mut pb = krilla::geom::PathBuilder::new();
        let Some(bbox) = krilla::geom::Rect::from_xywh(ox, oy, ow, oh) else {
            return;
        };
        pb.push_rect(bbox);
        if style.has_radius() {
            crate::draw_primitives::append_rounded_rect_subpath(
                &mut pb, x, y, w, h, &style.border_radii,
            );
        } else if let Some(box_rect) = krilla::geom::Rect::from_xywh(x, y, w, h) {
            pb.push_rect(box_rect);
        } else {
            return;
        }
        pb.finish()
    };
    let Some(clip_path) = clip_path else { return };

    canvas
        .surface
        .push_clip_path(&clip_path, &krilla::paint::FillRule::EvenOdd);

    let stops = blur_stops(shadow.color, 8, bg_color);

    // ── Center: solid fill of inner rect (EvenOdd clip already excludes border-box)
    {
        let center_color = stops[0].color;
        let path = if style.has_radius() {
            crate::draw_primitives::build_rounded_rect_path(ix, iy, iw, ih, &r_inner)
        } else {
            build_rect_path(ix, iy, iw, ih)
        };
        if let Some(path) = path {
            canvas.surface.set_fill(Some(krilla::paint::Fill {
                paint: center_color.into(),
                opacity: krilla::num::NormalizedF32::ONE,
                rule: Default::default(),
            }));
            canvas.surface.set_stroke(None);
            canvas.surface.draw_path(&path);
            canvas.surface.set_fill(None);
        }
    }

    // ── Edge strips (LinearGradient, 4 sides)
    // TL/TR corner widths to inset edge start/end
    let r_tl = r_inner[0][0]; // top-left x-radius
    let r_tr = r_inner[1][0]; // top-right x-radius
    let r_br = r_inner[2][0]; // bottom-right x-radius
    let r_bl = r_inner[3][0]; // bottom-left x-radius

    // Top edge: x from (ix + r_tl) to (ix + iw - r_tr), y from oy to iy
    draw_edge_strip(
        &mut canvas.surface,
        ix + r_tl, oy, iw - r_tl - r_tr, blur,
        ix + r_tl, iy,   // gradient: inner point (opaque)
        ix + r_tl, oy,   // gradient: outer point (bg)
        &stops,
    );
    // Bottom edge
    draw_edge_strip(
        &mut canvas.surface,
        ix + r_bl, iy + ih, iw - r_bl - r_br, blur,
        ix + r_bl, iy + ih,
        ix + r_bl, iy + ih + blur,
        &stops,
    );
    // Left edge: y from (iy + r_tl_y) to (iy + ih - r_bl_y)
    let r_tl_y = r_inner[0][1];
    let r_bl_y = r_inner[3][1];
    let r_tr_y = r_inner[1][1];
    let r_br_y = r_inner[2][1];
    draw_edge_strip(
        &mut canvas.surface,
        ox, iy + r_tl_y, blur, ih - r_tl_y - r_bl_y,
        ix, iy + r_tl_y,
        ox, iy + r_tl_y,
        &stops,
    );
    // Right edge
    draw_edge_strip(
        &mut canvas.surface,
        ix + iw, iy + r_tr_y, blur, ih - r_tr_y - r_br_y,
        ix + iw, iy + r_tr_y,
        ix + iw + blur, iy + r_tr_y,
        &stops,
    );

    canvas.surface.pop();
}

/// Draw a rectangular strip filled with a LinearGradient.
/// `(rx, ry, rw, rh)` is the strip rectangle.
/// `(gx1, gy1)` is the opaque end of the gradient (stop offset=0).
/// `(gx2, gy2)` is the transparent (bg) end (stop offset=1).
fn draw_edge_strip(
    surface: &mut krilla::surface::Surface<'_>,
    rx: f32,
    ry: f32,
    rw: f32,
    rh: f32,
    gx1: f32,
    gy1: f32,
    gx2: f32,
    gy2: f32,
    stops: &[krilla::paint::Stop],
) {
    if rw <= 0.0 || rh <= 0.0 || stops.len() < 2 {
        return;
    }
    let Some(rect) = krilla::geom::Rect::from_xywh(rx, ry, rw, rh) else {
        return;
    };
    let mut pb = krilla::geom::PathBuilder::new();
    pb.push_rect(rect);
    let Some(path) = pb.finish() else { return };

    let lg = krilla::paint::LinearGradient {
        x1: gx1,
        y1: gy1,
        x2: gx2,
        y2: gy2,
        transform: krilla::geom::Transform::default(),
        spread_method: krilla::paint::SpreadMethod::Pad,
        stops: stops.to_vec(),
        anti_alias: false,
    };
    surface.set_fill(Some(krilla::paint::Fill {
        paint: lg.into(),
        opacity: krilla::num::NormalizedF32::ONE,
        rule: Default::default(),
    }));
    surface.set_stroke(None);
    surface.draw_path(&path);
    surface.set_fill(None);
}
```

**Step 4: Run tests to verify no compilation errors and smoke tests pass**

```bash
cargo test -p fulgur --lib blur_stops 2>&1 | tail -5
cargo test -p fulgur --test render_smoke test_render_html_shadow_blur_gradient_path 2>&1 | tail -5
```

Expected: both pass (smoke test compiles; function not yet called from `draw_single_box_shadow`)

**Step 5: Commit**

```bash
git add crates/fulgur/src/background.rs crates/fulgur/tests/render_smoke.rs
git commit -m "feat(background): add draw_blur_box_shadow with edge LinearGradient strips"
```

---

### Task 3: Add corner RadialGradient patches to `draw_blur_box_shadow`

**Files:**

- Modify: `crates/fulgur/src/background.rs`

**Step 1: Add the corner drawing helper and call it**

Add the following function (after `draw_edge_strip`):

```rust
/// Draw one corner patch of a blurred shadow using a RadialGradient.
///
/// `(cx, cy)` is the arc center of the inner rounded corner (shadow shape).
/// `r_inner` is the inner radius (start of blur, opaque stop).
/// `r_outer = r_inner + blur` is the outer radius (end of blur, bg stop).
/// `(patch_x, patch_y, patch_w, patch_h)` is the rectangular bounding patch.
fn draw_corner_patch(
    surface: &mut krilla::surface::Surface<'_>,
    cx: f32,
    cy: f32,
    r_inner: f32,
    r_outer: f32,
    patch_x: f32,
    patch_y: f32,
    patch_w: f32,
    patch_h: f32,
    stops: &[krilla::paint::Stop],
) {
    if patch_w <= 0.0 || patch_h <= 0.0 || r_outer <= 0.0 || stops.len() < 2 {
        return;
    }
    let Some(rect) = krilla::geom::Rect::from_xywh(patch_x, patch_y, patch_w, patch_h) else {
        return;
    };
    let mut pb = krilla::geom::PathBuilder::new();
    pb.push_rect(rect);
    let Some(path) = pb.finish() else { return };

    // Stops run from r_inner (opaque) to r_outer (bg).
    // RadialGradient: fr=r_inner, cr=r_outer, focal==center (concentric).
    let rg = krilla::paint::RadialGradient {
        fx: cx,
        fy: cy,
        fr: r_inner,
        cx,
        cy,
        cr: r_outer,
        transform: krilla::geom::Transform::default(),
        spread_method: krilla::paint::SpreadMethod::Pad,
        stops: stops.to_vec(),
        anti_alias: false,
    };
    surface.set_fill(Some(krilla::paint::Fill {
        paint: rg.into(),
        opacity: krilla::num::NormalizedF32::ONE,
        rule: Default::default(),
    }));
    surface.set_stroke(None);
    surface.draw_path(&path);
    surface.set_fill(None);
}
```

Then inside `draw_blur_box_shadow`, add the corner patches **after** the edge strips and **before** `canvas.surface.pop()`. Replace the `// ── Edge strips` section's closing and `canvas.surface.pop()` with:

```rust
    // ── Corners (RadialGradient)
    let blur = shadow.blur; // already have this in scope

    // TL corner
    {
        let rcx = ix + r_tl;
        let rcy = iy + r_inner[0][1];
        let r_in = r_inner[0][0].max(r_inner[0][1]); // use larger of rx/ry as approx
        draw_corner_patch(
            &mut canvas.surface,
            rcx, rcy, r_in, r_in + blur,
            ox, oy,
            blur + r_tl, blur + r_inner[0][1],
            &stops,
        );
    }
    // TR corner
    {
        let rcx = ix + iw - r_tr;
        let rcy = iy + r_inner[1][1];
        let r_in = r_inner[1][0].max(r_inner[1][1]);
        draw_corner_patch(
            &mut canvas.surface,
            rcx, rcy, r_in, r_in + blur,
            ix + iw - r_tr, oy,
            blur + r_tr, blur + r_inner[1][1],
            &stops,
        );
    }
    // BR corner
    {
        let rcx = ix + iw - r_br;
        let rcy = iy + ih - r_inner[2][1];
        let r_in = r_inner[2][0].max(r_inner[2][1]);
        draw_corner_patch(
            &mut canvas.surface,
            rcx, rcy, r_in, r_in + blur,
            ix + iw - r_br, iy + ih - r_inner[2][1],
            blur + r_br, blur + r_inner[2][1],
            &stops,
        );
    }
    // BL corner
    {
        let rcx = ix + r_bl;
        let rcy = iy + ih - r_inner[3][1];
        let r_in = r_inner[3][0].max(r_inner[3][1]);
        draw_corner_patch(
            &mut canvas.surface,
            rcx, rcy, r_in, r_in + blur,
            ox, iy + ih - r_inner[3][1],
            blur + r_bl, blur + r_inner[3][1],
            &stops,
        );
    }

    canvas.surface.pop();
```

**Step 2: Run smoke tests**

```bash
cargo test -p fulgur --test render_smoke test_render_html_shadow_blur 2>&1 | tail -10
```

Expected: still passes (function not yet wired)

**Step 3: Commit**

```bash
git add crates/fulgur/src/background.rs
git commit -m "feat(background): add corner RadialGradient patches to draw_blur_box_shadow"
```

---

### Task 4: Wire blur path into `draw_single_box_shadow`

**Files:**

- Modify: `crates/fulgur/src/background.rs`

**Step 1: Update `draw_single_box_shadow`**

Find this block in `draw_single_box_shadow` (around line 45):

```rust
    // NOTE: when blur rendering is implemented (fulgur-4ie follow-up), this rect
    // must also be expanded by the blur radius, and the blur extent drawn via
    // rasterization + gaussian blur + image embed.
    let sx = x + shadow.offset_x - shadow.spread;
```

Replace the NOTE comment and then add early-exit for blur > 0 before the existing geometry:

```rust
    if shadow.blur > 0.0 {
        // gradient 9-slice approach: pre-composite with white page background.
        draw_blur_box_shadow(canvas, style, shadow, x, y, w, h, [255, 255, 255, 255]);
        return;
    }

    let sx = x + shadow.offset_x - shadow.spread;
```

**Step 2: Run smoke tests for blur path**

```bash
cargo test -p fulgur --test render_smoke test_render_html_shadow_blur_gradient_path 2>&1 | tail -10
cargo test -p fulgur --test render_smoke test_render_html_shadow_blur_rounded 2>&1 | tail -10
```

Expected: both pass

**Step 3: Run full test suite to check for regressions**

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
cargo test -p fulgur 2>&1 | tail -15
```

Expected: all pass

**Step 4: Commit**

```bash
git add crates/fulgur/src/background.rs
git commit -m "feat(background): route box-shadow blur > 0 through gradient 9-slice path"
```

---

### Task 5: Remove blur warn from `shadow.rs` and update smoke test name

**Files:**

- Modify: `crates/fulgur/src/convert/style/shadow.rs`
- Modify: `crates/fulgur/tests/render_smoke.rs`

**Step 1: Remove the blur warn in `shadow.rs`**

Find this block (lines 18-25):

```rust
        let blur_px = shadow.base.blur.px();
        if blur_px > 0.0 {
            log::warn!(
                "box-shadow: blur-radius > 0 is not yet supported; \
                 drawing as blur=0 (blur={}px)",
                blur_px
            );
        }
```

Replace with:

```rust
        let blur_px = shadow.base.blur.px();
```

**Step 2: Update the smoke test name in `render_smoke.rs`**

Find `test_render_html_shadow_blur_warning_path` and update its doc comment to reflect the new behavior:

```rust
#[test]
fn test_render_html_shadow_blur_warning_path() {
    // Non-zero blur radius now routes through the gradient 9-slice path.
```

**Step 3: Run all tests**

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
cargo test -p fulgur 2>&1 | tail -10
cargo clippy -p fulgur 2>&1 | grep "^error" | head -10
```

Expected: all pass, no clippy errors

**Step 4: Commit**

```bash
git add crates/fulgur/src/convert/style/shadow.rs crates/fulgur/tests/render_smoke.rs
git commit -m "fix(shadow): remove blur > 0 warn — gradient path now handles it"
```

---

### Task 6: Visual validation and cleanup

**Files:**

- Read: output PDF

**Step 1: Generate a test PDF and visually inspect**

```bash
cd /home/ubuntu/fulgur/.worktrees/feat/fulgur-0xz-box-shadow-blur
cat > /tmp/shadow_test.html << 'EOF'
<!DOCTYPE html>
<html>
<body style="background:white;padding:40px;">
  <div style="width:150px;height:80px;background:#4a90e2;
              box-shadow:8px 8px 20px 0 rgba(0,0,0,0.5);
              margin:40px;">Plain blur</div>
  <div style="width:150px;height:80px;background:#e24a4a;border-radius:16px;
              box-shadow:0 4px 16px 4px rgba(0,0,0,0.4);
              margin:40px;">Rounded + spread</div>
  <div style="width:150px;height:80px;background:#4ae24a;
              box-shadow:0 0 30px 0 rgba(255,0,0,0.8);
              margin:40px;">Large blur, vivid color</div>
</body>
</html>
EOF
cargo run --bin fulgur -- render /tmp/shadow_test.html -o /tmp/shadow_out.pdf 2>&1
```

Expected: PDF generated without errors.

**Step 2: Run final full test suite**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur 2>&1 | tail -10
cargo fmt --check -p fulgur 2>&1
cargo clippy -p fulgur 2>&1 | grep "^error" | head -5
```

Expected: all pass, `cargo fmt --check` clean, no clippy errors.

**Step 3: Final commit (if any formatting fixes needed)**

```bash
cargo fmt -p fulgur
git add -u
git commit -m "style: cargo fmt"
```
