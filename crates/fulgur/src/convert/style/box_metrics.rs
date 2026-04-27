//! border-width and padding extraction (layout-only).

use crate::convert::px_to_pt;
use crate::pageable::BlockStyle;

pub(super) fn apply_to(style: &mut BlockStyle, layout: &taffy::Layout) {
    style.border_widths = [
        px_to_pt(layout.border.top),
        px_to_pt(layout.border.right),
        px_to_pt(layout.border.bottom),
        px_to_pt(layout.border.left),
    ];
    style.padding = [
        px_to_pt(layout.padding.top),
        px_to_pt(layout.padding.right),
        px_to_pt(layout.padding.bottom),
        px_to_pt(layout.padding.left),
    ];
}
