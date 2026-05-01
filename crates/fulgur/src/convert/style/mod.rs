use crate::asset::AssetBundle;
use crate::blitz_adapter::Node;
use crate::draw_primitives::BlockStyle;

mod background;
mod border;
mod box_metrics;
mod opacity;
mod overflow;
mod shadow;

pub(super) use opacity::extract_opacity_visible;

/// Bundle of references threaded through the per-property style extractors.
///
/// `extract_block_style` constructs this once per call and forwards it to
/// helper modules (`overflow`, `border`, `shadow`, `background`). Keeping
/// the four references on a single struct avoids long parameter lists.
pub(super) struct StyleContext<'a> {
    pub(super) styles: &'a style::properties::ComputedValues,
    pub(super) current_color: &'a style::color::AbsoluteColor,
    pub(super) layout: &'a taffy::Layout,
    pub(super) assets: Option<&'a AssetBundle>,
}

/// Extract visual style (background, borders, padding, background-image) from a node.
pub(super) fn extract_block_style(node: &Node, assets: Option<&AssetBundle>) -> BlockStyle {
    let layout = node.final_layout;
    let mut style = BlockStyle::default();
    box_metrics::apply_to(&mut style, &layout);

    // Extract colors from computed styles
    if let Some(styles) = node.primary_styles() {
        let current_color = styles.clone_color();
        let ctx = StyleContext {
            styles: &styles,
            current_color: &current_color,
            layout: &layout,
            assets,
        };

        // Borders (color, radii, styles)
        border::apply_to(&mut style, &ctx);

        // Box shadows
        shadow::apply_to(&mut style, &ctx);

        // Overflow (CSS3 axis-independent interpretation)
        // PDF has no scroll concept: hidden/clip/scroll/auto all collapse to Clip.
        overflow::apply_to(&mut style, &ctx);

        // Background color + background-image layers — kept last to preserve
        // the original temporal order in which `style.background_layers` is
        // populated. VRT golden bytes depend on this ordering.
        background::apply_to(&mut style, &ctx);
    }

    style
}

pub(super) fn absolute_to_rgba(c: style::color::AbsoluteColor) -> [u8; 4] {
    // `.round()` (not `as u8` truncation) so e.g. `rgb(127.5,…)` lands on 128
    // instead of 127. Truncation introduces a half-channel down-bias for
    // every fractional component, which is most visible in gradient stops.
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [
        q(c.components.0),
        q(c.components.1),
        q(c.components.2),
        q(c.alpha),
    ]
}
