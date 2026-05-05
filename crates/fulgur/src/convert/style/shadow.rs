//! box-shadow extraction.
//!
//! Iterates the computed `box-shadow` list and pushes non-inset shadows
//! onto `BlockStyle::box_shadows`. Non-zero blur is rendered via the
//! gradient 9-slice path in `render.rs`. Inset shadows are skipped with
//! a `log::warn!`.

use super::{StyleContext, absolute_to_rgba};
use crate::convert::px_to_pt;
use crate::draw_primitives::BlockStyle;

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    let shadow_list = ctx.styles.clone_box_shadow();
    for shadow in shadow_list.0.iter() {
        if shadow.inset {
            log::warn!("box-shadow: inset is not yet supported; skipping");
            continue;
        }
        let blur_px = shadow.base.blur.px();
        let rgba = absolute_to_rgba(shadow.base.color.resolve_to_absolute(ctx.current_color));
        if rgba[3] == 0 {
            continue; // fully transparent — skip
        }
        style.box_shadows.push(crate::draw_primitives::BoxShadow {
            offset_x: px_to_pt(shadow.base.horizontal.px()),
            offset_y: px_to_pt(shadow.base.vertical.px()),
            blur: px_to_pt(blur_px),
            spread: px_to_pt(shadow.spread.px()),
            color: rgba,
            inset: false,
        });
    }
}
