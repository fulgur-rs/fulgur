//! overflow-x / overflow-y extraction (styles → axis-independent Clip / Visible).
//!
//! CSS3 axis-independent interpretation: PDF has no scroll concept, so
//! `hidden` / `clip` / `scroll` / `auto` all collapse to [`Overflow::Clip`].

use super::StyleContext;
use crate::draw_primitives::{BlockStyle, Overflow};

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    style.overflow_x = map(ctx.styles.clone_overflow_x());
    style.overflow_y = map(ctx.styles.clone_overflow_y());
}

/// Map a Stylo computed `Overflow` keyword to fulgur's axis-independent enum.
fn map(o: style::values::computed::Overflow) -> Overflow {
    use style::values::computed::Overflow as S;
    match o {
        S::Visible => Overflow::Visible,
        S::Hidden | S::Clip | S::Scroll | S::Auto => Overflow::Clip,
    }
}

#[cfg(test)]
mod tests {
    use super::map;
    use crate::draw_primitives::Overflow;
    use style::values::computed::Overflow as S;

    #[test]
    fn visible_maps_to_visible() {
        assert_eq!(map(S::Visible), Overflow::Visible);
    }

    #[test]
    fn hidden_maps_to_clip() {
        assert_eq!(map(S::Hidden), Overflow::Clip);
    }

    #[test]
    fn clip_maps_to_clip() {
        assert_eq!(map(S::Clip), Overflow::Clip);
    }

    #[test]
    fn scroll_maps_to_clip() {
        assert_eq!(map(S::Scroll), Overflow::Clip);
    }

    #[test]
    fn auto_maps_to_clip() {
        assert_eq!(map(S::Auto), Overflow::Clip);
    }
}
