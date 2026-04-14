# CSS Feature Support

This document tracks fulgur's CSS property support status and any
version-specific limitations.

## Effects

### `box-shadow` (v0.4.5+)

Supported:

- Outer shadows with `offset-x`, `offset-y`, `spread-radius`, `color`
  (including `rgba()` alpha, `transparent`, `currentColor`)
- Multiple comma-separated shadows (painted front-to-back per CSS spec)
- Combination with `border-radius` (shadow follows rounded corners; spread
  expands radii per spec)
- Negative `spread-radius` (corners clamp sharp per CSS spec)

Not yet supported:

- `blur-radius > 0`: currently rendered as `blur=0` (hard shadow) with a
  `log::warn!` diagnostic. True gaussian blur requires rasterization and
  is planned for a follow-up release.
- `inset` shadows: skipped with a `log::warn!` diagnostic.
- `box-shadow` on inline-level elements (including `display: inline-block`):
  fulgur's drawing pipeline currently reaches `draw_box_shadows` only from
  `BlockPageable` and `TablePageable`, so inline-level backgrounds, borders,
  and shadows are not drawn today. Use `display: block` to get shadows on
  generic boxes.

See PR `#83` and `docs/plans/2026-04-14-box-shadow.md`
for implementation details.
