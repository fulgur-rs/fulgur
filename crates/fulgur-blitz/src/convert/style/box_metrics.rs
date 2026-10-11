//! border-width and padding extraction (layout-only).

use crate::draw_primitives::BlockStyle;
use crate::units::F32Units;

pub(super) fn apply_to(style: &mut BlockStyle, layout: &taffy::Layout) {
    style.border_widths = [
        layout.border.top.as_px().in_pt(),
        layout.border.right.as_px().in_pt(),
        layout.border.bottom.as_px().in_pt(),
        layout.border.left.as_px().in_pt(),
    ];
    style.padding = [
        layout.padding.top.as_px().in_pt(),
        layout.padding.right.as_px().in_pt(),
        layout.padding.bottom.as_px().in_pt(),
        layout.padding.left.as_px().in_pt(),
    ];
}

#[cfg(test)]
mod tests {
    use super::apply_to;
    use crate::draw_primitives::BlockStyle;

    #[test]
    fn px_pt_conversion_top_right_bottom_left() {
        // 1 CSS px = 0.75 pt; pick distinct px values so an off-by-one
        // axis swap would surface in the assertion.
        let layout = taffy::Layout {
            border: taffy::Rect {
                top: 4.0,
                right: 8.0,
                bottom: 12.0,
                left: 16.0,
            },
            padding: taffy::Rect {
                top: 20.0,
                right: 24.0,
                bottom: 28.0,
                left: 32.0,
            },
            ..Default::default()
        };
        let mut style = BlockStyle::default();
        apply_to(&mut style, &layout);
        assert_eq!(
            style.border_widths.map(crate::units::Pt::to_f32),
            [3.0, 6.0, 9.0, 12.0]
        );
        assert_eq!(
            style.padding.map(crate::units::Pt::to_f32),
            [15.0, 18.0, 21.0, 24.0]
        );
    }

    #[test]
    fn zero_layout_yields_zero_widths_and_padding() {
        let layout = taffy::Layout::default();
        let mut style = BlockStyle::default();
        apply_to(&mut style, &layout);
        assert_eq!(style.border_widths.map(crate::units::Pt::to_f32), [0.0; 4]);
        assert_eq!(style.padding.map(crate::units::Pt::to_f32), [0.0; 4]);
    }
}
