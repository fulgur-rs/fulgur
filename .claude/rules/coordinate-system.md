# Coordinate System and Unit Conversion Rules

Most coordinate bugs in fulgur trace back to either "forgetting to convert CSS px ↔ PDF pt"
or "misunderstanding Krilla's Y direction."
Always consult this file when writing or reviewing rendering code.

## Unit layers in the pipeline

```text
Blitz/Taffy (CSS px) ──px_to_pt()──► Pageable tree / Krilla (PDF pt)
                     ◄──pt_to_px()──
```

| Layer | Unit | Notes |
|-------|------|-------|
| Blitz input (viewport, width) | CSS px | convert with `pt_to_px(v)` before passing |
| Taffy `final_layout` | CSS px | extract via `layout_in_pt()` / `size_in_pt()` |
| Pageable tree internals | PDF pt | |
| Krilla Surface | PDF pt | |
| `PageSize::custom(w, h)` | **mm** | `config.rs:22` — converted to pt internally |
| `Margin::uniform(v)` | **pt** | |
| `Margin::uniform_mm(v)` | **mm** | |

Conversion constant: `1 CSS px = 0.75 PDF pt` (`PX_TO_PT = 0.75`, `convert.rs:35`)

## Blitz boundary conversion rules (most important)

Forgetting a conversion produces a **4/3× or 3/4× scale bug**.

```rust
// WRONG: pass pt value directly
parse_html_with_local_resources(html, config.content_width(), ...)

// CORRECT: convert pt → CSS px first
parse_html_with_local_resources(html, pt_to_px(config.content_width()), ...)
```

```rust
// WRONG: use Taffy layout values directly
let width = node.final_layout.size.width;

// CORRECT: go through px_to_pt / size_in_pt
let (width, height) = size_in_pt(node.final_layout.size);
let (x, y, width, height) = layout_in_pt(&node.final_layout);
```

Helper definitions: `convert.rs:37-63`

## Exception: `compute_transform` arguments

`blitz_adapter::compute_transform(styles, border_box_width, border_box_height)` takes
`border_box_width` / `border_box_height` in **CSS px** — do not convert.
Stylo's `length-percentage` resolution operates in CSS px space.
The returned `Affine2D.e`/`.f` (translate components) are also in CSS px;
the call site in `convert.rs` folds them into pt-space later.

## Stylo length-percentage resolution

`LengthPercentage::resolve(basis: Length)` — `basis` must be in **CSS px**.
Passing a pt value produces a 3/4× error.

```rust
// WRONG: pt basis
origin.horizontal.resolve(Length::new(border_box_width_pt))

// CORRECT: px basis (layout values are CSS px)
origin.horizontal.resolve(Length::new(border_box_width_px))
```

## Krilla / Pageable coordinate system

- **Origin: top-left, Y axis: downward (Y-down)**
- PDF spec (ISO 32000) uses bottom-left origin with Y up, but Krilla flips internally
- All fulgur code assumes top-left origin with Y growing down

`Quadrilateral` vertex order (`pageable.rs:297`):
bottom-left → bottom-right → top-right → top-left (in Y-down coordinates)

## CSS transform matrix composition (`Affine2D`)

```rust
// A.mul(&B) returns the matrix product A × B (point p transforms as A * B * p)
// CSS transform lists apply right-to-left (first operation is innermost)
let composed = t_origin.mul(&m).mul(&t_neg_origin);
// = T(ox, oy) · M · T(-ox, -oy)
```

`Affine2D`'s `(a, b, c, d, e, f)` maps to `krilla::geom::Transform::from_row(a, b, c, d, e, f)`.

## PDF text coordinates in inspect.rs

`Td`/`TD` operands are offsets in text coordinate space, not page space.
Apply the linear part of the text matrix `(a, b, c, d)` to convert to user-space displacement.

```rust
// WRONG: add offset directly to page coordinates
tx += dx; ty += dy;

// CORRECT: transform through text matrix linear part
tlm_e += dx * tm_a + dy * tm_c;
tlm_f += dx * tm_b + dy * tm_d;
```

`BT` resets both the text matrix and text line matrix to identity (PDF §9.4.1).
Track the CTM stack (`q`/`Q`) to obtain final page coordinates.

## References

- `crates/fulgur/src/convert.rs:29-63` — conversion constants and helper definitions
- `crates/fulgur/src/config.rs:22` — `PageSize::custom` mm definition
- `docs/plans/2026-04-17-viewport-pt-to-css-px.md` — deep-dive on the px/pt boundary bug
- PR #90 (superseded) / beads fulgur-9ul — history of the viewport pt/px misidentification fix
