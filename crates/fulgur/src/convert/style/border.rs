//! border-color, border-radius, border-style extraction.
//!
//! border_radii basis is CSS px (Stylo length-percentage operates in CSS px),
//! converted to pt via px_to_pt before storage. See coordinate-system.md.

use super::{StyleContext, absolute_to_rgba};
use crate::convert::px_to_pt;
use crate::draw_primitives::{BlockStyle, BorderStyleValue};

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    // Border color (use top border color for all sides for simplicity)
    let bc = ctx.styles.clone_border_top_color();
    style.border_color = absolute_to_rgba(bc.resolve_to_absolute(ctx.current_color));

    // Border radii. Stylo evaluates length-percentage values in CSS px
    // space, so we feed it the CSS-px border-box basis and convert the
    // returned radius to pt. border_radii is consumed downstream alongside
    // pt-space widths/heights (see `compute_padding_box_inner_radii`).
    let width = ctx.layout.size.width;
    let height = ctx.layout.size.height;
    let resolve_radius =
        |r: &style::values::computed::length_percentage::NonNegativeLengthPercentage,
         basis: f32|
         -> f32 {
            px_to_pt(
                r.0.resolve(style::values::computed::Length::new(basis))
                    .px(),
            )
        };

    let tl = ctx.styles.clone_border_top_left_radius();
    let tr = ctx.styles.clone_border_top_right_radius();
    let br = ctx.styles.clone_border_bottom_right_radius();
    let bl = ctx.styles.clone_border_bottom_left_radius();

    style.border_radii = [
        [
            resolve_radius(&tl.0.width, width),
            resolve_radius(&tl.0.height, height),
        ],
        [
            resolve_radius(&tr.0.width, width),
            resolve_radius(&tr.0.height, height),
        ],
        [
            resolve_radius(&br.0.width, width),
            resolve_radius(&br.0.height, height),
        ],
        [
            resolve_radius(&bl.0.width, width),
            resolve_radius(&bl.0.height, height),
        ],
    ];

    style.border_styles = [
        map_border_style(ctx.styles.clone_border_top_style()),
        map_border_style(ctx.styles.clone_border_right_style()),
        map_border_style(ctx.styles.clone_border_bottom_style()),
        map_border_style(ctx.styles.clone_border_left_style()),
    ];
}

fn map_border_style(bs: style::values::specified::BorderStyle) -> BorderStyleValue {
    use style::values::specified::BorderStyle as BS;
    match bs {
        BS::None | BS::Hidden => BorderStyleValue::None,
        BS::Dashed => BorderStyleValue::Dashed,
        BS::Dotted => BorderStyleValue::Dotted,
        BS::Double => BorderStyleValue::Double,
        BS::Groove => BorderStyleValue::Groove,
        BS::Ridge => BorderStyleValue::Ridge,
        BS::Inset => BorderStyleValue::Inset,
        BS::Outset => BorderStyleValue::Outset,
        BS::Solid => BorderStyleValue::Solid,
    }
}

#[cfg(test)]
mod tests {
    use super::map_border_style;
    use crate::draw_primitives::BorderStyleValue;
    use style::values::specified::BorderStyle as BS;

    #[test]
    fn none_and_hidden_collapse_to_none() {
        assert_eq!(map_border_style(BS::None), BorderStyleValue::None);
        assert_eq!(map_border_style(BS::Hidden), BorderStyleValue::None);
    }

    #[test]
    fn dashed_dotted_double() {
        assert_eq!(map_border_style(BS::Dashed), BorderStyleValue::Dashed);
        assert_eq!(map_border_style(BS::Dotted), BorderStyleValue::Dotted);
        assert_eq!(map_border_style(BS::Double), BorderStyleValue::Double);
    }

    #[test]
    fn groove_ridge_inset_outset() {
        assert_eq!(map_border_style(BS::Groove), BorderStyleValue::Groove);
        assert_eq!(map_border_style(BS::Ridge), BorderStyleValue::Ridge);
        assert_eq!(map_border_style(BS::Inset), BorderStyleValue::Inset);
        assert_eq!(map_border_style(BS::Outset), BorderStyleValue::Outset);
    }

    #[test]
    fn solid() {
        assert_eq!(map_border_style(BS::Solid), BorderStyleValue::Solid);
    }
}
