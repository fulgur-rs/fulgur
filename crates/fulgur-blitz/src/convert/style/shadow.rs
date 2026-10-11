//! box-shadow extraction.
//!
//! Iterates the computed `box-shadow` list and pushes non-inset shadows
//! onto `BlockStyle::box_shadows`. Non-zero blur is rendered via the
//! gradient 9-slice path in `render.rs`. Inset shadows are skipped with
//! a `log::warn!`.

use super::{StyleContext, absolute_to_rgba};
use crate::draw_primitives::BlockStyle;
use crate::units::F32Units;

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
            offset_x: shadow.base.horizontal.px().as_px().in_pt(),
            offset_y: shadow.base.vertical.px().as_px().in_pt(),
            blur: blur_px.as_px().in_pt(),
            spread: shadow.spread.px().as_px().in_pt(),
            color: rgba,
            inset: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::Engine;

    fn render_with_shadow(shadow_css: &str) -> Vec<u8> {
        let html = format!(
            r#"<html><body><div style="width:100px;height:100px;box-shadow:{shadow_css}"></div></body></html>"#
        );
        Engine::builder()
            .build()
            .render(&html)
            .expect("render should succeed")
    }

    /// Non-inset shadow with partial alpha exercises the main push path.
    /// The shadow is actually drawn, so the PDF must differ from the no-shadow baseline.
    #[test]
    fn non_inset_shadow_is_pushed() {
        let baseline = render_with_shadow("none");
        let pdf = render_with_shadow("5px 5px 10px rgba(0,0,0,0.5)");
        assert!(pdf.starts_with(b"%PDF"), "expected PDF header");
        assert_ne!(
            pdf.len(),
            baseline.len(),
            "non-inset shadow should add content to PDF"
        );
    }

    /// `inset` keyword exercises the `shadow.inset` branch (skipped with log::warn!).
    /// Skipped shadows leave no trace — output must match the no-shadow baseline.
    #[test]
    fn inset_shadow_is_skipped() {
        let baseline = render_with_shadow("none");
        let pdf = render_with_shadow("inset 5px 5px 10px red");
        assert!(pdf.starts_with(b"%PDF"), "expected PDF header");
        assert_eq!(
            pdf.len(),
            baseline.len(),
            "inset shadow should not affect output"
        );
    }

    /// Fully transparent color exercises the `rgba[3] == 0` early-continue branch.
    /// Skipped shadows leave no trace — output must match the no-shadow baseline.
    #[test]
    fn transparent_shadow_is_skipped() {
        let baseline = render_with_shadow("none");
        let pdf = render_with_shadow("5px 5px 10px transparent");
        assert!(pdf.starts_with(b"%PDF"), "expected PDF header");
        assert_eq!(
            pdf.len(),
            baseline.len(),
            "transparent shadow should not affect output"
        );
    }

    /// Multiple shadows: inset + transparent + normal in one list.
    /// Only the opaque non-inset shadow reaches the push path;
    /// output must match the equivalent single-shadow render.
    #[test]
    fn mixed_shadow_list_skips_inset_and_transparent() {
        let single = render_with_shadow("3px 3px 0px black");
        let pdf = render_with_shadow("inset 2px 2px red, 5px 5px transparent, 3px 3px 0px black");
        assert!(pdf.starts_with(b"%PDF"), "expected PDF header");
        assert_eq!(
            pdf.len(),
            single.len(),
            "only the valid shadow should be included"
        );
    }

    /// Non-zero spread radius is stored via `shadow.spread.px().as_px().in_pt()`.
    /// The shadow is actually drawn, so the PDF must differ from the no-shadow baseline.
    #[test]
    fn shadow_with_spread_radius() {
        let baseline = render_with_shadow("none");
        let pdf = render_with_shadow("2px 2px 5px 8px rgba(255,0,0,0.8)");
        assert!(pdf.starts_with(b"%PDF"), "expected PDF header");
        assert_ne!(
            pdf.len(),
            baseline.len(),
            "shadow with spread should add content to PDF"
        );
    }
}
