# CSS transform Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement CSS `transform` (2D functions + `matrix()`) and `transform-origin` support in fulgur, applied at PDF draw time as affine matrices, with transformed elements treated as atomic (unsplittable) during pagination.

**Architecture:** Introduce an `Affine2D` value type and a `TransformWrapperPageable` that wraps any `Pageable` and applies a pre-resolved affine matrix via `Krilla::surface::push_transform` during `draw()`. stylo's `TransformOperation` list is folded into a single `Affine2D` at convert time (using taffy's final layout size as the reference box), so `pageable.rs` stays stylo-independent. All stylo access is confined to a new helper in `blitz_adapter.rs`. Atomic split is enforced by the wrapper's `split()` always returning `None`.

**Tech Stack:** Rust / stylo (`TransformOperation`, `TransformOrigin`, `LengthPercentage::resolve`) / krilla (`Transform::from_row`, `push_transform`/`pop_transform`) / existing fulgur `Pageable` trait.

**Related beads issue:** `fulgur-bfx`

---

## Architectural notes for the implementer

### What you're touching

- `crates/fulgur/src/pageable.rs` — add `Affine2D` value type and `TransformWrapperPageable` (mirrors the pattern already used by `CounterOpWrapperPageable` at lines 1290–1352).
- `crates/fulgur/src/blitz_adapter.rs` — add `compute_transform()` helper that reads stylo's computed `transform` and `transform-origin` and returns a resolved `Affine2D` + origin in px.
- `crates/fulgur/src/convert.rs` — add `maybe_wrap_transform()` alongside the existing `maybe_prepend_string_set` / `maybe_prepend_counter_ops` (lines 113–145), and call it from `convert_node()`.
- `crates/fulgur/tests/transform_integration.rs` — new integration test file.
- `examples/transform/index.html` — new visual snapshot source.

### Matrix convention (read carefully)

- `Affine2D { a, b, c, d, e, f }` represents the 2×3 affine matrix:

  ```text
  | a  c  e |     | x |     | a*x + c*y + e |
  | b  d  f |  *  | y |  =  | b*x + d*y + f |
  | 0  0  1 |     | 1 |     |       1       |
  ```

- This matches the convention of `krilla::geom::Transform::from_row(sx, ky, kx, sy, tx, ty)` where `(sx, ky, kx, sy, tx, ty) == (a, b, c, d, e, f)`. **Verify this against the krilla source before writing `to_krilla()`** — see `~/.cargo/registry/src/index.crates.io-*/krilla-0.7.0/src/geom.rs:186`.
- Composition order in `Affine2D::mul`: `A.mul(&B)` should produce `A * B` (matrix product, `A` applied after `B` when transforming a point `p` via `A*B*p`). This matters because CSS transform lists apply **right-to-left** to the coordinate system. Confirm by writing the non-commutativity test in Task 3 before trusting the implementation.
- CSS rule: the transform-origin formulation is `T(ox, oy) · M · T(-ox, -oy)`. In our `TransformWrapperPageable::draw()`, `ox` and `oy` are resolved to absolute PDF canvas coordinates (element's draw `(x, y)` + pre-resolved `origin_x`/`origin_y` in px).

### stylo type references (for `compute_transform`)

- `style::values::computed::transform::Transform` — the computed `transform` property (alias for `GenericTransform<TransformOperation>`).
- `style::values::computed::transform::TransformOperation` — enum of 2D + 3D ops; see full variant list in `~/.cargo/registry/src/index.crates.io-*/stylo-0.8.0/values/generics/transform.rs:200`.
- `style::values::computed::transform::TransformOrigin` — `GenericTransformOrigin<LengthPercentage, LengthPercentage, Length>`. Initial value is `(50%, 50%, 0)` (`transform.rs:34`).
- `style::values::computed::length_percentage::LengthPercentage::resolve(basis: Length) -> Length` — resolves percentages against `basis`, returns absolute px length.
- `style::values::computed::angle::Angle::radians() -> f32` — converts to radians for trig.
- `ComputedValues::clone_transform()` and `clone_transform_origin()` — the accessors (used the same way as `clone_background_color()` at `convert.rs:853`).

### Layout-independence of `transform`

CSS rule: `transform` does not affect layout. Taffy's `final_layout.size` is the box on which percentages should resolve. The `<node>.final_layout` struct is already accessed throughout `convert.rs` (e.g. line 232 `let layout = node.final_layout;`) — pass `layout.size.width` / `layout.size.height` to `compute_transform`.

### Existing wrapper pattern to mirror

`CounterOpWrapperPageable` in `pageable.rs:1304–1352` is the structural template. Mirror its `Clone` derive, `wrap`/`split`/`draw`/`clone_box`/`height`/`pagination`/`as_any` pattern. The only functional differences for `TransformWrapperPageable`:

- `split()` returns `None` unconditionally (atomic).
- `draw()` wraps the inner `draw` call in `push_transform` / `pop_transform`.

### Testing strategy

Matrix-level geometric assertions — no VRT. A dedicated `#[cfg(test)] pub(crate) fn effective_matrix(&self, x: Pt, y: Pt) -> Affine2D` exposed on `TransformWrapperPageable` lets tests observe the exact matrix that would be pushed at draw time, without needing a Krilla surface.

---

## Task 1: Add `Affine2D` value type and unit tests

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` — add `Affine2D` struct + impl near the top of the file (after `Size` around line 13).

**Step 1: Write failing unit tests**

Add to the existing `#[cfg(test)] mod tests { ... }` block at the bottom of `pageable.rs` (or create one if none exists at module scope):

```rust
#[cfg(test)]
mod affine_tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI};

    const EPS: f32 = 1e-5;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    fn matrix_approx(a: &Affine2D, b: &Affine2D) -> bool {
        approx(a.a, b.a) && approx(a.b, b.b) && approx(a.c, b.c)
            && approx(a.d, b.d) && approx(a.e, b.e) && approx(a.f, b.f)
    }

    #[test]
    fn identity_is_identity() {
        assert!(Affine2D::IDENTITY.is_identity());
        let m = Affine2D::translation(3.0, 4.0);
        assert!(matrix_approx(&m.mul(&Affine2D::IDENTITY), &m));
        assert!(matrix_approx(&Affine2D::IDENTITY.mul(&m), &m));
    }

    #[test]
    fn rotation_90_maps_unit_vector() {
        let r = Affine2D::rotation(FRAC_PI_2);
        // Point (1, 0) transformed by pure rotation should land at (0, 1).
        let x = r.a * 1.0 + r.c * 0.0 + r.e;
        let y = r.b * 1.0 + r.d * 0.0 + r.f;
        assert!(approx(x, 0.0), "x expected 0.0, got {x}");
        assert!(approx(y, 1.0), "y expected 1.0, got {y}");
    }

    #[test]
    fn translation_times_rotation_is_non_commutative() {
        let t = Affine2D::translation(10.0, 0.0);
        let r = Affine2D::rotation(FRAC_PI_2);
        let tr = t.mul(&r); // translate, then rotate (right-to-left applied to points)
        let rt = r.mul(&t);
        assert!(!matrix_approx(&tr, &rt), "expected non-commutative result");
    }

    #[test]
    fn is_identity_tolerates_epsilon() {
        let almost = Affine2D {
            a: 1.0 + 1e-7, b: 1e-7, c: -1e-7,
            d: 1.0 - 1e-7, e: 1e-7, f: -1e-7,
        };
        assert!(almost.is_identity());
    }

    #[test]
    fn scale_matrix_has_correct_diagonal() {
        let s = Affine2D::scale(2.0, 3.0);
        assert!(approx(s.a, 2.0));
        assert!(approx(s.d, 3.0));
        assert!(approx(s.b, 0.0));
        assert!(approx(s.c, 0.0));
        assert!(approx(s.e, 0.0));
        assert!(approx(s.f, 0.0));
    }
}
```

**Step 2: Run the tests to confirm they fail**

```bash
cargo test -p fulgur --lib affine_tests 2>&1 | tail -20
```

Expected: compile error `cannot find type 'Affine2D'`.

**Step 3: Implement `Affine2D`**

Add to `crates/fulgur/src/pageable.rs` after the existing `Size` struct (around line 14):

```rust
/// 2×3 affine transformation matrix used for CSS `transform`.
///
/// Stored in column-vector convention:
/// ```text
/// | a  c  e |     | x |     | a*x + c*y + e |
/// | b  d  f |  *  | y |  =  | b*x + d*y + f |
/// | 0  0  1 |     | 1 |     |       1       |
/// ```
///
/// This matches `krilla::geom::Transform::from_row(a, b, c, d, e, f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2D {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine2D {
    pub const IDENTITY: Self = Self {
        a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0,
    };

    /// ε tolerance for identity detection (absorbs trig float noise).
    const IDENTITY_EPS: f32 = 1e-5;

    pub fn is_identity(&self) -> bool {
        (self.a - 1.0).abs() < Self::IDENTITY_EPS
            && self.b.abs() < Self::IDENTITY_EPS
            && self.c.abs() < Self::IDENTITY_EPS
            && (self.d - 1.0).abs() < Self::IDENTITY_EPS
            && self.e.abs() < Self::IDENTITY_EPS
            && self.f.abs() < Self::IDENTITY_EPS
    }

    pub fn translation(tx: f32, ty: f32) -> Self {
        Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: tx, f: ty }
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self { a: sx, b: 0.0, c: 0.0, d: sy, e: 0.0, f: 0.0 }
    }

    pub fn rotation(theta_rad: f32) -> Self {
        let (s, c) = theta_rad.sin_cos();
        Self { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
    }

    /// 2D skew. `ax_rad` is the x-axis skew angle, `ay_rad` is the y-axis skew.
    pub fn skew(ax_rad: f32, ay_rad: f32) -> Self {
        Self {
            a: 1.0,
            b: ay_rad.tan(),
            c: ax_rad.tan(),
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Matrix product `self * rhs`. Applied to a point `p`, this yields
    /// `(self * rhs) * p = self * (rhs * p)`, i.e. `rhs` acts first.
    pub fn mul(&self, rhs: &Self) -> Self {
        Self {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }

    pub fn to_krilla(&self) -> krilla::geom::Transform {
        krilla::geom::Transform::from_row(self.a, self.b, self.c, self.d, self.e, self.f)
    }
}
```

**Step 4: Run tests to confirm they pass**

```bash
cargo test -p fulgur --lib affine_tests 2>&1 | tail -20
```

Expected: `test result: ok. 5 passed`.

**Step 5: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(transform): add Affine2D value type for CSS transform"
```

---

## Task 2: Add `TransformWrapperPageable` and its unit tests

**Files:**

- Modify: `crates/fulgur/src/pageable.rs` — add `TransformWrapperPageable` after the existing `CounterOpWrapperPageable` (around line 1352).

**Step 1: Write failing tests**

Add to the same tests module (or alongside the existing tests):

```rust
#[cfg(test)]
mod transform_wrapper_tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    const EPS: f32 = 1e-5;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    // A minimal no-op Pageable for wrapping in tests.
    #[derive(Clone)]
    struct StubPageable {
        w: Pt,
        h: Pt,
    }

    impl Pageable for StubPageable {
        fn wrap(&mut self, _: Pt, _: Pt) -> Size {
            Size { width: self.w, height: self.h }
        }
        fn split(&self, _: Pt, _: Pt) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
            None
        }
        fn draw(&self, _: &mut Canvas<'_, '_>, _: Pt, _: Pt, _: Pt, _: Pt) {}
        fn clone_box(&self) -> Box<dyn Pageable> { Box::new(self.clone()) }
        fn height(&self) -> Pt { self.h }
        fn as_any(&self) -> &dyn std::any::Any { self }
    }

    fn wrap(matrix: Affine2D, origin_x: Pt, origin_y: Pt) -> TransformWrapperPageable {
        TransformWrapperPageable {
            inner: Box::new(StubPageable { w: 100.0, h: 100.0 }),
            matrix,
            origin_x,
            origin_y,
        }
    }

    #[test]
    fn translate_only_matrix() {
        let w = wrap(Affine2D::translation(10.0, 20.0), 0.0, 0.0);
        let m = w.effective_matrix(0.0, 0.0);
        assert!(approx(m.e, 10.0));
        assert!(approx(m.f, 20.0));
        assert!(approx(m.a, 1.0));
        assert!(approx(m.d, 1.0));
    }

    #[test]
    fn rotate_90_maps_unit_vector() {
        // With origin at (0,0), rotate 90° sends (1,0) → (0,1).
        let w = wrap(Affine2D::rotation(FRAC_PI_2), 0.0, 0.0);
        let m = w.effective_matrix(0.0, 0.0);
        let x = m.a * 1.0 + m.c * 0.0 + m.e;
        let y = m.b * 1.0 + m.d * 0.0 + m.f;
        assert!(approx(x, 0.0), "x expected 0.0, got {x}");
        assert!(approx(y, 1.0), "y expected 1.0, got {y}");
    }

    #[test]
    fn rotate_with_center_origin_fixes_center() {
        // 100×100 box with origin at center (50, 50).
        // After 90° rotation, the origin point should still map to itself.
        let w = wrap(Affine2D::rotation(FRAC_PI_2), 50.0, 50.0);
        let m = w.effective_matrix(0.0, 0.0);
        let x = m.a * 50.0 + m.c * 50.0 + m.e;
        let y = m.b * 50.0 + m.d * 50.0 + m.f;
        assert!(approx(x, 50.0), "origin x should be fixed, got {x}");
        assert!(approx(y, 50.0), "origin y should be fixed, got {y}");
    }

    #[test]
    fn split_is_always_none() {
        let w = wrap(Affine2D::rotation(FRAC_PI_2), 0.0, 0.0);
        assert!(w.split(1000.0, 1000.0).is_none());
        // Even when the inner would "fit" in infinite space, wrapper still refuses to split.
    }

    #[test]
    fn wrap_delegates_to_inner_size() {
        let mut w = wrap(Affine2D::rotation(FRAC_PI_2), 0.0, 0.0);
        let size = w.wrap(1000.0, 1000.0);
        assert!(approx(size.width, 100.0));
        assert!(approx(size.height, 100.0));
    }
}
```

**Step 2: Run tests to confirm compile failure**

```bash
cargo test -p fulgur --lib transform_wrapper_tests 2>&1 | tail -20
```

Expected: compile error `cannot find type 'TransformWrapperPageable'`.

**Step 3: Implement `TransformWrapperPageable`**

Add after `CounterOpWrapperPageable` in `pageable.rs`:

```rust
// ─── TransformWrapperPageable ──────────────────────────────

/// Wraps a Pageable in a CSS `transform`. The matrix is pre-resolved
/// at convert time (percentages / keywords already turned into px).
///
/// The wrapper is **atomic**: `split()` always returns `None`, forcing
/// the whole subtree onto a single page. A transformed element that
/// spans a page break would be geometrically meaningless (half of a
/// rotated title on each page), so we follow PrinceXML / WeasyPrint
/// behavior and never split through a transform.
///
/// `origin_x` / `origin_y` are the `transform-origin` resolved to px,
/// measured from the element's border-box top-left corner.
#[derive(Clone)]
pub struct TransformWrapperPageable {
    pub inner: Box<dyn Pageable>,
    pub matrix: Affine2D,
    pub origin_x: f32,
    pub origin_y: f32,
}

impl TransformWrapperPageable {
    pub fn new(inner: Box<dyn Pageable>, matrix: Affine2D, origin_x: f32, origin_y: f32) -> Self {
        Self { inner, matrix, origin_x, origin_y }
    }

    /// Compute the full matrix that will be pushed onto the Krilla surface
    /// when this wrapper is drawn at `(draw_x, draw_y)`.
    ///
    /// The transform-origin is translated into the draw coordinate system
    /// (`draw_x + origin_x`, `draw_y + origin_y`), then the composition
    /// `T(ox, oy) · M · T(-ox, -oy)` is built so that rotation/scale
    /// happen around the chosen origin point.
    ///
    /// Exposed for tests so geometric correctness can be verified without
    /// constructing a Krilla surface.
    pub(crate) fn effective_matrix(&self, draw_x: Pt, draw_y: Pt) -> Affine2D {
        let ox = draw_x + self.origin_x;
        let oy = draw_y + self.origin_y;
        Affine2D::translation(ox, oy)
            .mul(&self.matrix)
            .mul(&Affine2D::translation(-ox, -oy))
    }
}

impl Pageable for TransformWrapperPageable {
    fn wrap(&mut self, avail_width: Pt, avail_height: Pt) -> Size {
        // transform does not affect layout; reuse the inner measurement.
        self.inner.wrap(avail_width, avail_height)
    }

    fn split(
        &self,
        _avail_width: Pt,
        _avail_height: Pt,
    ) -> Option<(Box<dyn Pageable>, Box<dyn Pageable>)> {
        // Atomic: never split through a transform.
        None
    }

    fn draw(&self, canvas: &mut Canvas<'_, '_>, x: Pt, y: Pt, avail_width: Pt, avail_height: Pt) {
        let full = self.effective_matrix(x, y);
        canvas.surface.push_transform(&full.to_krilla());
        self.inner.draw(canvas, x, y, avail_width, avail_height);
        canvas.surface.pop_transform();
    }

    fn clone_box(&self) -> Box<dyn Pageable> {
        Box::new(self.clone())
    }

    fn height(&self) -> Pt {
        self.inner.height()
    }

    fn pagination(&self) -> Pagination {
        self.inner.pagination()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
```

**Step 4: Run the wrapper tests**

```bash
cargo test -p fulgur --lib transform_wrapper_tests 2>&1 | tail -30
```

Expected: `test result: ok. 5 passed`.

Then run the entire `pageable` test suite to confirm no regression:

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```

Expected: baseline (231) + 10 new tests all passing.

**Step 5: Commit**

```bash
git add crates/fulgur/src/pageable.rs
git commit -m "feat(transform): add TransformWrapperPageable with atomic split"
```

---

## Task 3: Add `compute_transform` helper in `blitz_adapter.rs`

**Files:**

- Modify: `crates/fulgur/src/blitz_adapter.rs` — add helper near the bottom of the file, in the `pub fn` section.

**Step 1: Write a unit test in `blitz_adapter.rs`**

Add to (or create) the tests module at the bottom of `blitz_adapter.rs`:

```rust
#[cfg(test)]
mod transform_tests {
    use super::*;
    use crate::pageable::Affine2D;

    const EPS: f32 = 1e-5;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    /// Helper: parse a tiny HTML snippet with a single root `<div>` and extract
    /// its computed `transform` + `transform-origin` via compute_transform().
    fn compute(html: &str, width: f32, height: f32) -> Option<(Affine2D, f32, f32)> {
        let doc = parse_and_layout(html, 400.0, 2000.0, &[]);
        let root = doc.root_element();
        // Descend to the first element child so we pick up the <div>.
        let base = doc.deref();
        fn first_element(doc: &blitz_dom::BaseDocument, node_id: usize) -> Option<usize> {
            let node = doc.get_node(node_id)?;
            for &child in &node.children {
                if let Some(c) = doc.get_node(child) {
                    if matches!(c.data, blitz_dom::NodeData::Element(_)) && c.element_data().map(|e| e.name.local.to_string()).as_deref() == Some("div") {
                        return Some(child);
                    }
                    if let Some(found) = first_element(doc, child) {
                        return Some(found);
                    }
                }
            }
            None
        }
        let id = first_element(base, root.id)?;
        let node = base.get_node(id)?;
        let styles = node.primary_styles()?;
        compute_transform(&styles, width, height)
    }

    #[test]
    fn no_transform_returns_none() {
        let html = r#"<!DOCTYPE html><html><body><div>hi</div></body></html>"#;
        assert!(compute(html, 100.0, 100.0).is_none());
    }

    #[test]
    fn translate_px_returns_translation_matrix() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(10px, 20px)">hi</div>
        </body></html>"#;
        let (m, _, _) = compute(html, 100.0, 100.0).expect("should have transform");
        assert!(approx(m.e, 10.0));
        assert!(approx(m.f, 20.0));
        assert!(approx(m.a, 1.0));
        assert!(approx(m.d, 1.0));
    }

    #[test]
    fn translate_percent_resolves_against_border_box() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(50%, 25%)">hi</div>
        </body></html>"#;
        // 200 × 80 reference box
        let (m, _, _) = compute(html, 200.0, 80.0).expect("should have transform");
        assert!(approx(m.e, 100.0), "expected 100 (50% of 200), got {}", m.e);
        assert!(approx(m.f, 20.0), "expected 20 (25% of 80), got {}", m.f);
    }

    #[test]
    fn matrix_is_preserved_verbatim() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: matrix(1, 2, 3, 4, 5, 6)">hi</div>
        </body></html>"#;
        let (m, _, _) = compute(html, 100.0, 100.0).expect("should have transform");
        assert!(approx(m.a, 1.0));
        assert!(approx(m.b, 2.0));
        assert!(approx(m.c, 3.0));
        assert!(approx(m.d, 4.0));
        assert!(approx(m.e, 5.0));
        assert!(approx(m.f, 6.0));
    }

    #[test]
    fn origin_default_is_center() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: rotate(45deg)">hi</div>
        </body></html>"#;
        let (_, ox, oy) = compute(html, 100.0, 60.0).expect("should have transform");
        assert!(approx(ox, 50.0), "default origin x should be 50% of 100, got {ox}");
        assert!(approx(oy, 30.0), "default origin y should be 50% of 60, got {oy}");
    }

    #[test]
    fn identity_transform_returns_none() {
        // translate(0, 0) folds to identity; compute_transform should suppress.
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate(0, 0)">hi</div>
        </body></html>"#;
        assert!(compute(html, 100.0, 100.0).is_none());
    }

    #[test]
    fn three_d_op_folds_to_identity_and_is_suppressed() {
        let html = r#"<!DOCTYPE html><html><body>
            <div style="transform: translate3d(0, 0, 50px)">hi</div>
        </body></html>"#;
        // All 2D components are zero → identity → None.
        assert!(compute(html, 100.0, 100.0).is_none());
    }
}
```

*Note:* the `first_element` helper may need tweaking depending on Blitz node-data API; adapt as needed to descend to the `<div>`. The existing `extract_inline_svg_tree` function (around line 230) shows the right NodeData pattern.

**Step 2: Run tests to confirm compile failure**

```bash
cargo test -p fulgur --lib transform_tests 2>&1 | tail -20
```

Expected: `cannot find function 'compute_transform'`.

**Step 3: Implement `compute_transform`**

Add to `crates/fulgur/src/blitz_adapter.rs`:

```rust
// ─── transform support ────────────────────────────────────

use crate::pageable::Affine2D;
use style::values::computed::transform::{Transform as ComputedTransform, TransformOperation};
use style::values::computed::Length;

/// Read the computed `transform` and `transform-origin` from `styles` and
/// fold the `TransformOperation` list into a single pre-resolved `Affine2D`.
///
/// Percentages in `translate` and `transform-origin` are resolved against the
/// caller-supplied `border_box_width` / `border_box_height` (taffy's final
/// layout size — transform does not affect layout, so this is correct).
///
/// Returns `None` if the transform is absent or folds to identity, so callers
/// can skip wrapper construction.
///
/// 3D operations (`translate3d`, `rotate3d`, `scale3d`, `matrix3d`,
/// `perspective`, etc.) are treated as identity with a `log::warn!` — fulgur
/// is a 2D PDF renderer. The warning fires once per such op so pathological
/// inputs don't flood the log.
pub fn compute_transform(
    styles: &style::properties::ComputedValues,
    border_box_width: f32,
    border_box_height: f32,
) -> Option<(Affine2D, f32, f32)> {
    let transform = styles.clone_transform();
    let origin = styles.clone_transform_origin();

    let origin_x = origin
        .horizontal
        .resolve(Length::new(border_box_width))
        .px();
    let origin_y = origin
        .vertical
        .resolve(Length::new(border_box_height))
        .px();

    if transform.0.is_empty() {
        return None;
    }

    let mut m = Affine2D::IDENTITY;
    for op in transform.0.iter() {
        let step = op_to_matrix(op, border_box_width, border_box_height);
        m = m.mul(&step);
    }

    // Bail out if nothing actually moved.
    if m.is_identity() && !has_nan_or_inf(&m) {
        return None;
    }
    if has_nan_or_inf(&m) {
        log::warn!("transform produced non-finite matrix; falling back to identity");
        return None;
    }

    Some((m, origin_x, origin_y))
}

fn op_to_matrix(op: &TransformOperation, w: f32, h: f32) -> Affine2D {
    use TransformOperation::*;
    match op {
        Matrix(m) => Affine2D { a: m.a, b: m.b, c: m.c, d: m.d, e: m.e, f: m.f },
        Translate(x, y) => Affine2D::translation(
            x.resolve(Length::new(w)).px(),
            y.resolve(Length::new(h)).px(),
        ),
        TranslateX(x) => Affine2D::translation(x.resolve(Length::new(w)).px(), 0.0),
        TranslateY(y) => Affine2D::translation(0.0, y.resolve(Length::new(h)).px()),
        Scale(sx, sy) => Affine2D::scale(*sx, *sy),
        ScaleX(sx) => Affine2D::scale(*sx, 1.0),
        ScaleY(sy) => Affine2D::scale(1.0, *sy),
        Rotate(angle) => Affine2D::rotation(angle.radians()),
        Skew(ax, ay) => Affine2D::skew(ax.radians(), ay.radians()),
        SkewX(ax) => Affine2D::skew(ax.radians(), 0.0),
        SkewY(ay) => Affine2D::skew(0.0, ay.radians()),
        // 3D and animation intermediates — fulgur is 2D only.
        Matrix3D(_)
        | Translate3D(..)
        | TranslateZ(_)
        | Scale3D(..)
        | ScaleZ(_)
        | Rotate3D(..)
        | RotateX(_)
        | RotateY(_)
        | RotateZ(_)
        | Perspective(_)
        | InterpolateMatrix { .. }
        | AccumulateMatrix { .. } => {
            log::warn!("unsupported 3D/animation transform op: {:?}", op);
            Affine2D::IDENTITY
        }
    }
}

fn has_nan_or_inf(m: &Affine2D) -> bool {
    [m.a, m.b, m.c, m.d, m.e, m.f]
        .iter()
        .any(|v| !v.is_finite())
}
```

**Step 4: Run the transform_tests**

```bash
cargo test -p fulgur --lib transform_tests 2>&1 | tail -30
```

Iterate on compile errors (the most likely fixable issues are `style::...` path mismatches — confirm via `cargo check` that the `ComputedValues` and `TransformOperation` paths match the version of `stylo` fulgur depends on; grep the existing `convert.rs:849` pattern `node.primary_styles()` for the working path). Expected final: `test result: ok. 7 passed`.

Then confirm the whole lib still builds:

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```

Expected: baseline (231) + 10 (wrapper) + 7 (transform) = 248 passing.

**Step 5: Commit**

```bash
git add crates/fulgur/src/blitz_adapter.rs
git commit -m "feat(transform): compute_transform helper reading stylo transform/origin"
```

---

## Task 4: Hook `TransformWrapperPageable` into `convert_node`

**Files:**

- Modify: `crates/fulgur/src/convert.rs` — add `maybe_wrap_transform` helper and call it from `convert_node`.

**Step 1: Add the helper**

Insert in `convert.rs` after `maybe_prepend_counter_ops` (around line 145):

```rust
/// If the given node has a non-identity `transform`, wrap the pageable in a
/// `TransformWrapperPageable`. The wrapper holds a pre-resolved affine matrix
/// and enforces atomic pagination.
fn maybe_wrap_transform(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    child: Box<dyn Pageable>,
) -> Box<dyn Pageable> {
    let Some(node) = doc.get_node(node_id) else {
        return child;
    };
    let Some(styles) = node.primary_styles() else {
        return child;
    };
    let layout = node.final_layout;
    match crate::blitz_adapter::compute_transform(
        &styles,
        layout.size.width,
        layout.size.height,
    ) {
        Some((matrix, origin_x, origin_y)) => Box::new(TransformWrapperPageable::new(
            child, matrix, origin_x, origin_y,
        )),
        None => child,
    }
}
```

Remember to import `TransformWrapperPageable` at the top of `convert.rs` in the existing `use crate::pageable::{ ... };` block (around lines 7–13).

**Step 2: Wire it into `convert_node`**

Modify `convert_node` (lines 96–108) to call the new helper last, so the outermost wrapper is the transform (matches the CSS stacking-context semantics: transform applies to the node's entire subtree including its counter-op/string-set markers):

```rust
fn convert_node(
    doc: &blitz_dom::BaseDocument,
    node_id: usize,
    ctx: &mut ConvertContext<'_>,
    depth: usize,
) -> Box<dyn Pageable> {
    if depth >= MAX_DOM_DEPTH {
        return Box::new(SpacerPageable::new(0.0));
    }
    let result = convert_node_inner(doc, node_id, ctx, depth);
    let result = maybe_prepend_string_set(node_id, result, ctx);
    let result = maybe_prepend_counter_ops(node_id, result, ctx);
    maybe_wrap_transform(doc, node_id, result)
}
```

**Step 3: Build**

```bash
cargo build -p fulgur 2>&1 | tail -20
```

Expected: clean build. Fix any import errors immediately.

**Step 4: Re-run the full lib test suite**

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```

Expected: all existing tests still pass; new total ≈ 248.

**Step 5: Commit**

```bash
git add crates/fulgur/src/convert.rs
git commit -m "feat(transform): wire TransformWrapperPageable into convert_node"
```

---

## Task 5: End-to-end integration tests

**Files:**

- Create: `crates/fulgur/tests/transform_integration.rs`

**Step 1: Write the integration tests**

```rust
//! End-to-end tests for CSS transform support.
//!
//! These tests render tiny HTML snippets through the full fulgur pipeline
//! (parse → layout → convert → paginate) and inspect the resulting Pageable
//! tree via `as_any().downcast_ref()` to verify that the expected
//! `TransformWrapperPageable` is present with the correct matrix.
//!
//! We avoid a Krilla surface entirely: the draw() path is exercised through
//! the tree's `effective_matrix` hook rather than a real PDF render, so tests
//! run fast and stay deterministic.

use fulgur::pageable::{Affine2D, Pageable, TransformWrapperPageable};
use std::f32::consts::FRAC_PI_2;

const EPS: f32 = 1e-5;

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < EPS
}

/// Render `html` to a Pageable tree (pre-pagination) and return the root.
fn render_to_pageable(html: &str) -> Box<dyn Pageable> {
    // Uses the public API exposed for tests; see `crates/fulgur/src/lib.rs`
    // for the canonical entry point. If no such helper exists yet, extend
    // it via an integration-test-only helper.
    fulgur::testing::render_html_to_pageable(html).expect("render")
}

/// Walk the tree to find the first TransformWrapperPageable.
/// This is a simple DFS based on the Pageable::as_any() downcast pattern
/// already used in the existing test helpers.
fn find_transform_wrapper(root: &dyn Pageable) -> Option<&TransformWrapperPageable> {
    if let Some(w) = root.as_any().downcast_ref::<TransformWrapperPageable>() {
        return Some(w);
    }
    // Generic children walk not possible without reflection — expose a
    // #[cfg(test)] children() on BlockPageable or walk via the known wrapper
    // types. For initial tests, simple test HTML can put the transform at
    // the root so this function is enough.
    None
}

#[test]
fn translate_px() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 50px; transform: translate(10px, 20px)">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    let w = walk_for_wrapper(root.as_ref()).expect("expected a transform wrapper");
    let m = w.effective_matrix(0.0, 0.0);
    // Origin defaults to 50% 50%, so the center is fixed; but for a pure
    // translation the translation component is invariant under the origin
    // conjugation and shows up as (10, 20) in (e, f).
    assert!(approx(m.e, 10.0));
    assert!(approx(m.f, 20.0));
}

#[test]
fn rotate_90_at_top_left_origin() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 100px; transform: rotate(90deg);
                    transform-origin: 0 0">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    let w = walk_for_wrapper(root.as_ref()).expect("transform");
    let m = w.effective_matrix(0.0, 0.0);
    // (1,0) maps to (0,1)
    let x = m.a * 1.0 + m.c * 0.0 + m.e;
    let y = m.b * 1.0 + m.d * 0.0 + m.f;
    assert!(approx(x, 0.0));
    assert!(approx(y, 1.0));
}

#[test]
fn rotate_90_at_center_origin_fixes_center() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 100px; transform: rotate(90deg)">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    let w = walk_for_wrapper(root.as_ref()).expect("transform");
    let m = w.effective_matrix(0.0, 0.0);
    // 100×100 box, default origin (50%, 50%) = (50, 50).
    // The origin point should be a fixed point of the rotation.
    let x = m.a * 50.0 + m.c * 50.0 + m.e;
    let y = m.b * 50.0 + m.d * 50.0 + m.f;
    assert!(approx(x, 50.0), "origin x should be fixed, got {x}");
    assert!(approx(y, 50.0), "origin y should be fixed, got {y}");
}

#[test]
fn scale_has_correct_diagonal() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 100px; transform: scale(2, 3);
                    transform-origin: 0 0">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    let w = walk_for_wrapper(root.as_ref()).expect("transform");
    let m = w.effective_matrix(0.0, 0.0);
    assert!(approx(m.a, 2.0));
    assert!(approx(m.d, 3.0));
}

#[test]
fn matrix_is_preserved_with_origin_zero() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 100px;
                    transform: matrix(1, 2, 3, 4, 5, 6);
                    transform-origin: 0 0">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    let w = walk_for_wrapper(root.as_ref()).expect("transform");
    let m = w.effective_matrix(0.0, 0.0);
    assert!(approx(m.a, 1.0));
    assert!(approx(m.b, 2.0));
    assert!(approx(m.c, 3.0));
    assert!(approx(m.d, 4.0));
    assert!(approx(m.e, 5.0));
    assert!(approx(m.f, 6.0));
}

#[test]
fn skew_x_45_has_correct_shear() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 100px; transform: skewX(45deg);
                    transform-origin: 0 0">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    let w = walk_for_wrapper(root.as_ref()).expect("transform");
    let m = w.effective_matrix(0.0, 0.0);
    assert!(approx(m.a, 1.0));
    assert!(approx(m.c, 1.0), "tan(45°) should be 1, got {}", m.c);
    assert!(approx(m.d, 1.0));
}

#[test]
fn composition_right_to_left() {
    // translate(10,0) rotate(90deg) applied to the origin point (0,0):
    // rotate first → (0,0); translate → (10, 0).
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 100px;
                    transform: translate(10px, 0) rotate(90deg);
                    transform-origin: 0 0">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    let w = walk_for_wrapper(root.as_ref()).expect("transform");
    let m = w.effective_matrix(0.0, 0.0);
    // Apply to (0, 0) — should land at (10, 0) because rotate(0,0) is (0,0)
    // and then translate adds (10, 0).
    let x = m.e;
    let y = m.f;
    assert!(approx(x, 10.0));
    assert!(approx(y, 0.0));
    // Apply to (1, 0) — rotate gives (0, 1); translate gives (10, 1).
    let x = m.a * 1.0 + m.c * 0.0 + m.e;
    let y = m.b * 1.0 + m.d * 0.0 + m.f;
    assert!(approx(x, 10.0));
    assert!(approx(y, 1.0));
}

#[test]
fn translate3d_does_not_panic_and_is_suppressed() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 100px;
                    transform: translate3d(0, 0, 50px)">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    // 3D ops with zero 2D components fold to identity → no wrapper.
    assert!(walk_for_wrapper(root.as_ref()).is_none());
}

#[test]
fn identity_transform_does_not_generate_wrapper() {
    let html = r#"<!DOCTYPE html><html><body>
        <div style="width: 100px; height: 100px;
                    transform: translate(0, 0)">x</div>
    </body></html>"#;
    let root = render_to_pageable(html);
    assert!(walk_for_wrapper(root.as_ref()).is_none());
}

/// Walks the Pageable tree via the publicly-exposed children accessor to
/// locate the first `TransformWrapperPageable`. Implemented as a free
/// function in the test module so the trait itself does not grow a
/// test-only method.
fn walk_for_wrapper(root: &dyn Pageable) -> Option<&TransformWrapperPageable> {
    if let Some(w) = root.as_any().downcast_ref::<TransformWrapperPageable>() {
        return Some(w);
    }
    // Descend into known container types used by fulgur. Use
    // as_any() + downcast_ref() for each known wrapper/block type.
    // Concrete list to extend as needed:
    use fulgur::pageable::{BlockPageable, CounterOpWrapperPageable, StringSetWrapperPageable};
    if let Some(b) = root.as_any().downcast_ref::<BlockPageable>() {
        for child in b.children_for_tests() {
            if let Some(w) = walk_for_wrapper(child.as_ref()) {
                return Some(w);
            }
        }
    }
    if let Some(w) = root.as_any().downcast_ref::<StringSetWrapperPageable>() {
        return walk_for_wrapper(w.child.as_ref());
    }
    if let Some(w) = root.as_any().downcast_ref::<CounterOpWrapperPageable>() {
        return walk_for_wrapper(w.child.as_ref());
    }
    None
}
```

**Step 2: Expose the test helpers**

Two test-only helpers need to be exposed. Add to `crates/fulgur/src/lib.rs` (or wherever the public API is re-exported):

```rust
#[doc(hidden)]
pub mod testing {
    use crate::engine::Engine;
    use crate::error::Result;
    use crate::pageable::Pageable;

    /// Render HTML all the way to a Pageable tree (pre-pagination).
    /// Intended for integration tests that want to inspect the tree without
    /// paginating or serializing to PDF.
    pub fn render_html_to_pageable(html: &str) -> Result<Box<dyn Pageable>> {
        // Reuse the existing Engine::render_pageable path. If the signature
        // differs, adapt to match — the goal is to return the root Pageable
        // after convert.rs has been applied, before pagination.
        Engine::new().render_pageable(html)
    }
}
```

And a `#[cfg(test)] pub(crate) fn children_for_tests(&self) -> &[Box<dyn Pageable>]` on `BlockPageable` in `pageable.rs`, returning the existing children vec. Expose it under `#[cfg(any(test, feature = "test-internals"))]` or keep it truly `pub(crate)` and move the integration test inside a `#[path]`-included module if necessary. The simplest path: add `pub fn children(&self) -> &[Box<dyn Pageable>]` behind a `#[doc(hidden)]` attribute.

**Step 3: Run the integration tests**

```bash
cargo test -p fulgur --test transform_integration 2>&1 | tail -40
```

Expected: 9 passing tests. Fix any compilation issues iteratively.

**Step 4: Add the atomic-split pagination test**

Append to `transform_integration.rs`:

```rust
#[test]
fn transformed_element_does_not_split_across_pages() {
    // A rotated block that's taller than the page content area should
    // skip the first page entirely (atomic wrapper refuses to split).
    let html = r#"<!DOCTYPE html><html><head><style>
        @page { size: 100pt 120pt; margin: 10pt; }
        .big { width: 50pt; height: 150pt; background: red;
               transform: rotate(45deg); }
    </style></head><body>
        <div class="big"></div>
    </body></html>"#;
    // Page content height is 100pt (120 - 2*10). The rotated block is
    // 150pt tall in pre-transform terms, so wrap() reports 150pt. Since
    // split() is None, the paginator must forward the whole block to
    // the next page. With only one block this means page 1 has the
    // block somewhere (possibly overflowing) — verify by rendering
    // to PDF and checking that exactly one page is produced and the
    // wrapper is present on that page.
    let pdf = fulgur::Engine::new().render_html_to_bytes(html).expect("render");
    // Basic sanity: we got PDF bytes without panicking.
    assert!(pdf.starts_with(b"%PDF"));
    // More rigorous: count pages in the PDF by scanning for "/Type /Page".
    let page_count = count_pdf_pages(&pdf);
    // Rotating a 150pt block inside 100pt of content area is an overflow
    // case — exactly one page should be produced and the wrapper held
    // atomically (i.e. not chopped in half).
    assert_eq!(page_count, 1, "expected 1 page, got {page_count}");
}

fn count_pdf_pages(pdf: &[u8]) -> usize {
    // Simple substring count; matches krilla-generated PDF structure.
    let needle = b"/Type /Page\n";
    pdf.windows(needle.len()).filter(|w| *w == needle).count()
}
```

**Step 5: Run everything and commit**

```bash
cargo test -p fulgur 2>&1 | tail -20
cargo clippy -p fulgur 2>&1 | tail -20
cargo fmt --check
```

Expected: all tests pass; clippy clean; fmt clean.

```bash
git add crates/fulgur/tests/transform_integration.rs crates/fulgur/src/lib.rs crates/fulgur/src/pageable.rs
git commit -m "test(transform): end-to-end integration tests for CSS transform"
```

---

## Task 6: Example snapshot HTML/PDF

**Files:**

- Create: `examples/transform/index.html`
- Create: `examples/transform/index.pdf` (generated)

**Step 1: Write the example HTML**

```html
<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="utf-8">
<title>CSS transform example</title>
<style>
  @page { size: A5; margin: 20mm; }
  body { font-family: sans-serif; }
  .grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 30px;
    margin-top: 30px;
  }
  .cell {
    width: 80px;
    height: 80px;
    background: #4a90e2;
    color: white;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .t1 { transform: translate(20px, 10px); }
  .t2 { transform: rotate(15deg); }
  .t3 { transform: scale(1.3); }
  .t4 { transform: skewX(20deg); }
  .t5 { transform: rotate(45deg) scale(0.8); }
  .t6 { transform: matrix(1, 0.2, -0.2, 1, 10, 5); }
</style>
</head>
<body>
  <h1>CSS transform examples</h1>
  <p>Each cell demonstrates a different transform.</p>
  <div class="grid">
    <div class="cell t1">translate</div>
    <div class="cell t2">rotate 15°</div>
    <div class="cell t3">scale 1.3</div>
    <div class="cell t4">skewX 20°</div>
    <div class="cell t5">rotate+scale</div>
    <div class="cell t6">matrix()</div>
  </div>
</body>
</html>
```

**Step 2: Generate the PDF**

```bash
cargo run --bin fulgur -- render examples/transform/index.html -o examples/transform/index.pdf
```

Verify the file opens (`xdg-open examples/transform/index.pdf` or `pdftotext examples/transform/index.pdf - | head`).

**Step 3: Commit**

```bash
git add examples/transform/index.html examples/transform/index.pdf examples/transform/style.css
git commit -m "docs(examples): add CSS transform example"
```

---

## Task 7: Final verification

**Step 1: Full test + lint sweep**

```bash
cargo test 2>&1 | tail -20
cargo clippy --all-targets 2>&1 | tail -20
cargo fmt --check
npx markdownlint-cli2 'docs/plans/2026-04-12-css-transform.md'
```

All must pass.

**Step 2: Verify acceptance criteria against `fulgur-bfx`**

Re-read `bd show fulgur-bfx` and walk down the `ACCEPTANCE CRITERIA` list. Every bullet should be visible in the diff or be a behavior covered by an integration test.

**Step 3: Stage for review**

`superpowers:verification-before-completion` will re-run this sweep and check off the acceptance list one item at a time before `superpowers:finishing-a-development-branch`.

---

## Notes for the implementer

- If `style::values::computed::transform::TransformOperation` is not reachable via that exact path for the version of `stylo` vendored by `blitz-dom 0.2.4`, grep the stylo source tree for `pub use ... as TransformOperation` to locate the actual public path. The generic enum lives at `stylo-0.8.0/values/generics/transform.rs:200` and the computed alias at `stylo-0.8.0/values/computed/transform.rs:19`.
- The `node.final_layout.size` used in Task 4 is the **transform-unaware** layout — exactly what we want for percentage resolution. Do not add transform-aware bbox logic.
- Keep `pageable.rs` free of any `style::...` imports; all stylo access stays in `blitz_adapter.rs`. If you find yourself wanting to import stylo types in `pageable.rs`, stop and re-derive the intermediate representation.
- The `log::warn!` calls require `log` to already be in fulgur's `Cargo.toml`. If not, add it (`log = "0.4"`) in the same commit that introduces the warning path — don't leave a dangling dependency.
