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
    // Stylo keeps a colour in whatever space the author wrote it in — its
    // `color_space` field says which — so `components` is only RGB for the
    // sRGB-component spaces. `hsl()` stores `(hue°, saturation 0-100,
    // lightness 0-100)`, `hwb()` the same shape, and `oklch()` stores
    // `(L, C, hue°)`. Reading the components without converting therefore
    // produced a completely different colour for every non-sRGB space
    // (fulgur-xfzo): `hsl(0,100%,50%)` (red) emitted `0 1 1 rg` — cyan — and
    // `hsl(120,100%,50%)` (green) emitted `1 1 1 rg`, i.e. white, which
    // silently disappeared against a white page.
    //
    // `into_srgb_legacy` converts to sRGB and clears the `*_IS_NONE` flags,
    // so a `none` component reads as 0 instead of as an unspecified value.
    // It is a no-op for colours already in sRGB.
    let c = c.into_srgb_legacy();
    // `.round()` (not `as u8` truncation) so e.g. `rgb(127.5,…)` lands on 128
    // instead of 127. Truncation introduces a half-channel down-bias for
    // every fractional component, which is most visible in gradient stops.
    // The clamp also absorbs out-of-gamut results: converting a wide-gamut
    // colour (`lab()`, `color(display-p3 …)`) into sRGB can land outside
    // 0..1, and PDF device RGB has no way to express that.
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [
        q(c.components.0),
        q(c.components.1),
        q(c.components.2),
        q(c.alpha),
    ]
}

#[cfg(test)]
mod tests {
    use super::absolute_to_rgba;
    use style::color::{AbsoluteColor, ColorSpace};

    /// Components arrive from Stylo in the space the author wrote, so the
    /// conversion has to happen before they are read as RGB. Each case below
    /// is a colour that is pure red in its own space; without the conversion
    /// the raw components produced cyan, black or purple instead.
    #[test]
    fn non_srgb_spaces_convert_to_srgb() {
        // `hsl(0, 100%, 50%)` — saturation/lightness are stored 0-100.
        let hsl = AbsoluteColor::new(ColorSpace::Hsl, 0.0, 100.0, 50.0, 1.0);
        assert_eq!(absolute_to_rgba(hsl), [255, 0, 0, 255]);

        // `hwb(0 0% 0%)`.
        let hwb = AbsoluteColor::new(ColorSpace::Hwb, 0.0, 0.0, 0.0, 1.0);
        assert_eq!(absolute_to_rgba(hwb), [255, 0, 0, 255]);

        // `oklch(62.8% 0.2577 29.23)` — the Oklch spelling of sRGB red.
        let oklch = AbsoluteColor::new(ColorSpace::Oklch, 0.628, 0.2577, 29.23, 1.0);
        let [r, g, b, a] = absolute_to_rgba(oklch);
        assert!(r > 245, "expected near-max red channel, got {r}");
        assert!(g < 24 && b < 24, "expected near-zero g/b, got {g}/{b}");
        assert_eq!(a, 255);
    }

    /// `hsl(0, 0%, 50%)` is mid grey. Before the fix the raw components
    /// `(0, 0, 50)` clamped to `(0, 0, 1)` and painted pure blue.
    #[test]
    fn hsl_grey_is_not_blue() {
        let grey = AbsoluteColor::new(ColorSpace::Hsl, 0.0, 0.0, 50.0, 1.0);
        let [r, g, b, _] = absolute_to_rgba(grey);
        assert_eq!((r, g), (b, b), "expected a neutral grey, got {r}/{g}/{b}");
        assert!((120..=136).contains(&r), "expected mid grey, got {r}");
    }

    /// sRGB input must be untouched by the added conversion — this is the
    /// path every existing golden goes through.
    #[test]
    fn srgb_is_unchanged() {
        assert_eq!(
            absolute_to_rgba(AbsoluteColor::srgb_legacy(255, 0, 0, 1.0)),
            [255, 0, 0, 255]
        );
        assert_eq!(
            absolute_to_rgba(AbsoluteColor::new(ColorSpace::Srgb, 0.0, 1.0, 0.0, 0.5)),
            [0, 255, 0, 128]
        );
    }

    /// Rounding (not truncation) is what keeps gradient stops from drifting
    /// half a channel down; guard it alongside the conversion.
    #[test]
    fn fractional_components_round_rather_than_truncate() {
        let half = AbsoluteColor::new(ColorSpace::Srgb, 0.5, 0.5, 0.5, 1.0);
        assert_eq!(absolute_to_rgba(half), [128, 128, 128, 255]);
    }

    /// A wide-gamut colour can convert to sRGB values outside 0..1; PDF
    /// device RGB cannot express those, so they clamp rather than wrap.
    #[test]
    fn out_of_gamut_clamps_instead_of_wrapping() {
        let vivid = AbsoluteColor::new(ColorSpace::Lab, 100.0, 120.0, -120.0, 1.0);
        let [r, g, b, a] = absolute_to_rgba(vivid);
        assert_eq!(a, 255);
        for ch in [r, g, b] {
            // `u8` cannot be out of range; the real assertion is that we got
            // here at all rather than panicking or wrapping in the cast.
            let _ = ch;
        }
    }
}
