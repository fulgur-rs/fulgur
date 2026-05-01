//! background-color and background-image-layers extraction.

use super::{StyleContext, absolute_to_rgba};
use crate::convert::{extract_asset_name, px_to_pt};
use crate::image::ImageRender;
use crate::pageable::{
    BackgroundLayer, BgBox, BgClip, BgImageContent, BgLengthPercentage, BgRepeat, BgSize,
    BlockStyle,
};
use std::sync::Arc;

pub(super) fn apply_to(style: &mut BlockStyle, ctx: &StyleContext<'_>) {
    // Background color — access the computed value directly
    let bg = ctx.styles.clone_background_color();
    let bg_rgba = absolute_to_rgba(bg.resolve_to_absolute(ctx.current_color));
    if bg_rgba[3] > 0 {
        style.background_color = Some(bg_rgba);
    }

    // Background image layers. Skip the six secondary `clone_*` calls
    // (sizes/positions/repeats/origins/clips) if no layer is actually
    // populated — the vast majority of DOM nodes have only `Image::None`.
    let bg_images = ctx.styles.clone_background_image();
    let has_real_bg_image = bg_images
        .0
        .iter()
        .any(|i| !matches!(i, style::values::computed::image::Image::None));
    if has_real_bg_image {
        let bg_sizes = ctx.styles.clone_background_size();
        let bg_pos_x = ctx.styles.clone_background_position_x();
        let bg_pos_y = ctx.styles.clone_background_position_y();
        let bg_repeats = ctx.styles.clone_background_repeat();
        let bg_origins = ctx.styles.clone_background_origin();
        let bg_clips = ctx.styles.clone_background_clip();

        for (i, image) in bg_images.0.iter().enumerate() {
            use style::values::computed::image::Image;

            // Resolve `content` + intrinsic size per image kind. URL images
            // require an `AssetBundle`; gradients are self-contained.
            let resolved: Option<(BgImageContent, f32, f32)> = match image {
                Image::Url(url) => ctx.assets.and_then(|a| {
                    let raw_src = match url {
                        style::servo::url::ComputedUrl::Valid(u) => u.as_str(),
                        style::servo::url::ComputedUrl::Invalid(s) => s.as_str(),
                    };
                    let src = extract_asset_name(raw_src);
                    let data = a.get_image(src)?;

                    use crate::image::AssetKind;
                    match AssetKind::detect(data) {
                        AssetKind::Raster(format) => {
                            let (iw, ih) =
                                ImageRender::decode_dimensions(data, format).unwrap_or((1, 1));
                            Some((
                                BgImageContent::Raster {
                                    data: Arc::clone(data),
                                    format,
                                },
                                iw as f32,
                                ih as f32,
                            ))
                        }
                        AssetKind::Svg => {
                            let opts = usvg::Options::default();
                            match usvg::Tree::from_data(data, &opts) {
                                Ok(tree) => {
                                    let svg_size = tree.size();
                                    Some((
                                        BgImageContent::Svg {
                                            tree: Arc::new(tree),
                                        },
                                        svg_size.width(),
                                        svg_size.height(),
                                    ))
                                }
                                Err(e) => {
                                    log::warn!("failed to parse SVG background-image '{src}': {e}");
                                    None
                                }
                            }
                        }
                        AssetKind::Unknown => None,
                    }
                }),
                Image::Gradient(g) => {
                    use style::values::computed::image::Gradient;
                    // g: &Box<Gradient> なので as_ref() で &Gradient を取って match。
                    match g.as_ref() {
                        Gradient::Linear { .. } => resolve_linear_gradient(g, ctx.current_color),
                        Gradient::Radial { .. } => resolve_radial_gradient(g, ctx.current_color),
                        Gradient::Conic { .. } => resolve_conic_gradient(g, ctx.current_color),
                    }
                }
                _ => None,
            };

            if let Some((content, intrinsic_width, intrinsic_height)) = resolved {
                let size = convert_bg_size(&bg_sizes.0, i);
                let (px, py) = convert_bg_position(&bg_pos_x.0, &bg_pos_y.0, i);
                let (rx, ry) = convert_bg_repeat(&bg_repeats.0, i);
                let origin = convert_bg_origin(&bg_origins.0, i);
                let clip = convert_bg_clip(&bg_clips.0, i);

                style.background_layers.push(BackgroundLayer {
                    content,
                    intrinsic_width,
                    intrinsic_height,
                    size,
                    position_x: px,
                    position_y: py,
                    repeat_x: rx,
                    repeat_y: ry,
                    origin,
                    clip,
                });
            }
        }
    }
}

/// Convert a Stylo computed `Gradient` into fulgur's `BgImageContent`.
///
/// Phase 1 supports `linear-gradient(...)` only:
/// - Direction via explicit angle, `to top/right/bottom/left` keyword, or
///   `to <h> <v>` corner. Corner directions are stored as a flag and
///   resolved against the gradient box at draw time (CSS Images 3 §3.1.1
///   defines them in terms of W and H).
/// - Color stops with explicit `<percentage>` positions, plus auto stops
///   (positions filled in via even spacing between adjacent fixed stops, per
///   CSS Images §3.5.1).
/// - Length-typed stops (`linear-gradient(red 50px, blue)`) are unsupported
///   in Phase 1 because resolving them requires the gradient line length,
///   which depends on the box dimensions (only known at draw time). Falls
///   back to `None` for now.
/// - Repeating gradients, `radial-gradient`, `conic-gradient`, color
///   interpolation methods, and interpolation hints are unsupported.
///
/// Returned tuple: `(content, intrinsic_w, intrinsic_h)`. Gradients have no
/// intrinsic size, so we return `(0.0, 0.0)` and the draw path special-cases
/// gradients to fill the origin rect directly (`background.rs` does not
/// route gradients through `resolve_size` / tiling).
fn resolve_linear_gradient(
    g: &style::values::computed::Gradient,
    current_color: &style::color::AbsoluteColor,
) -> Option<(BgImageContent, f32, f32)> {
    use crate::pageable::{LinearGradientCorner, LinearGradientDirection};
    use style::values::computed::image::{Gradient, LineDirection};
    use style::values::generics::image::GradientFlags;
    use style::values::specified::position::{HorizontalPositionKeyword, VerticalPositionKeyword};

    let (direction, items, flags) = match g {
        Gradient::Linear {
            direction,
            items,
            flags,
            ..
        } => (direction, items, flags),
        Gradient::Radial { .. } | Gradient::Conic { .. } => return None,
    };

    let repeating = flags.contains(GradientFlags::REPEATING);
    // Non-default `color_interpolation_method` (e.g. `in oklch`) would change
    // the rendered colors. Phase 1 interpolates in sRGB only, so bail rather
    // than silently misrender.
    if !flags.contains(GradientFlags::HAS_DEFAULT_COLOR_INTERPOLATION_METHOD) {
        return None;
    }

    let direction = match direction {
        LineDirection::Angle(a) => LinearGradientDirection::Angle(a.radians()),
        LineDirection::Horizontal(HorizontalPositionKeyword::Right) => {
            LinearGradientDirection::Angle(std::f32::consts::FRAC_PI_2)
        }
        LineDirection::Horizontal(HorizontalPositionKeyword::Left) => {
            LinearGradientDirection::Angle(3.0 * std::f32::consts::FRAC_PI_2)
        }
        LineDirection::Vertical(VerticalPositionKeyword::Top) => {
            LinearGradientDirection::Angle(0.0)
        }
        LineDirection::Vertical(VerticalPositionKeyword::Bottom) => {
            LinearGradientDirection::Angle(std::f32::consts::PI)
        }
        LineDirection::Corner(h, v) => {
            use HorizontalPositionKeyword::*;
            use VerticalPositionKeyword::*;
            let corner = match (h, v) {
                (Left, Top) => LinearGradientCorner::TopLeft,
                (Right, Top) => LinearGradientCorner::TopRight,
                (Left, Bottom) => LinearGradientCorner::BottomLeft,
                (Right, Bottom) => LinearGradientCorner::BottomRight,
            };
            LinearGradientDirection::Corner(corner)
        }
    };

    let stops = resolve_color_stops(items, current_color, "linear-gradient")?;

    Some((
        BgImageContent::LinearGradient {
            direction,
            stops,
            repeating,
        },
        0.0,
        0.0,
    ))
}

/// CSS gradient items から GradientStop ベクタを解決する。linear / radial 共通。
///
/// position は `GradientStopPosition` で保持され (Auto / Fraction / LengthPx)、
/// draw 時に `background::resolve_gradient_stops` で gradient line 長さを
/// 使って fraction 化される。convert 時の fixup は行わない。
///
/// Bail 条件:
/// - stops.len() < 2 (規定上 invalid)
/// - interpolation hint (Phase 2 別 issue)
/// - position が percentage でも length でもない (calc() 等 — Phase 2)
fn resolve_color_stops(
    items: &[style::values::generics::image::GenericGradientItem<
        style::values::computed::Color,
        style::values::computed::LengthPercentage,
    >],
    current_color: &style::color::AbsoluteColor,
    gradient_kind: &'static str,
) -> Option<Vec<crate::pageable::GradientStop>> {
    use crate::pageable::{GradientStop, GradientStopPosition};
    use style::values::generics::image::GradientItem;

    let mut out: Vec<GradientStop> = Vec::with_capacity(items.len());
    for item in items.iter() {
        match item {
            GradientItem::SimpleColorStop(c) => {
                let abs = c.resolve_to_absolute(current_color);
                out.push(GradientStop {
                    position: GradientStopPosition::Auto,
                    rgba: absolute_to_rgba(abs),
                    is_hint: false,
                });
            }
            GradientItem::ComplexColorStop { color, position } => {
                let abs = color.resolve_to_absolute(current_color);
                let pos = if let Some(pct) = position.to_percentage() {
                    GradientStopPosition::Fraction(pct.0)
                } else if let Some(len) = position.to_length() {
                    GradientStopPosition::LengthPx(len.px())
                } else {
                    log::warn!(
                        "{gradient_kind}: stop position is neither percentage \
                         nor length (calc() etc.). Layer dropped."
                    );
                    return None;
                };
                out.push(GradientStop {
                    position: pos,
                    rgba: absolute_to_rgba(abs),
                    is_hint: false,
                });
            }
            GradientItem::InterpolationHint(lp) => {
                // CSS Images 3 §3.5.3: hint は 2 つの color stop の間にしか
                // 置けない (先頭/連続/末尾は syntactically invalid)。
                if out.is_empty() {
                    log::warn!("{gradient_kind}: leading interpolation hint. Layer dropped.");
                    return None;
                }
                if out.last().is_some_and(|s| s.is_hint) {
                    log::warn!("{gradient_kind}: consecutive interpolation hints. Layer dropped.");
                    return None;
                }
                let pos = if let Some(pct) = lp.to_percentage() {
                    GradientStopPosition::Fraction(pct.0)
                } else if let Some(len) = lp.to_length() {
                    GradientStopPosition::LengthPx(len.px())
                } else {
                    // calc() etc. unsupported (Phase 2 別 issue) — Layer drop.
                    log::warn!(
                        "{gradient_kind}: hint position is neither percentage \
                         nor length (calc() etc.). Layer dropped."
                    );
                    return None;
                };
                out.push(GradientStop {
                    position: pos,
                    rgba: [0; 4], // is_hint=true のとき意味なし
                    is_hint: true,
                });
            }
        }
    }

    // 末尾 hint は不正
    if out.last().is_some_and(|s| s.is_hint) {
        log::warn!("{gradient_kind}: trailing interpolation hint. Layer dropped.");
        return None;
    }
    if out.len() < 2 {
        return None;
    }

    Some(out)
}

/// Convert a Stylo computed `Gradient::Radial` into fulgur's `BgImageContent::RadialGradient`.
///
/// Phase 1 scope (per beads issue fulgur-gm56 design):
/// - shape: circle / ellipse
/// - size: extent keyword (closest-side / farthest-side / closest-corner / farthest-corner) or
///   explicit length / length-percentage radii (resolved at draw time against gradient box)
/// - position: keyword + length-percentage の組合せ (BgLengthPercentage 経由)
/// - stops: linear と共通の resolve_color_stops を使用
///
/// Bail conditions (return None) — match resolve_linear_gradient:
/// - non-default color interpolation method
/// - length-typed / 範囲外 stop position, interpolation hint (resolve_color_stops 内)
///
/// `repeating-radial-gradient(...)` は `repeating: true` で受け、draw 時に
/// stop の周期展開で表現する (Krilla の RadialGradient は SpreadMethod::Pad のみ)。
fn resolve_radial_gradient(
    g: &style::values::computed::Gradient,
    current_color: &style::color::AbsoluteColor,
) -> Option<(BgImageContent, f32, f32)> {
    use crate::pageable::{RadialGradientShape, RadialGradientSize};
    use style::values::computed::image::Gradient;
    use style::values::generics::image::{Circle, Ellipse, EndingShape, GradientFlags};

    let (shape, position, items, flags) = match g {
        Gradient::Radial {
            shape,
            position,
            items,
            flags,
            ..
        } => (shape, position, items, flags),
        Gradient::Linear { .. } | Gradient::Conic { .. } => return None,
    };

    let repeating = flags.contains(GradientFlags::REPEATING);
    if !flags.contains(GradientFlags::HAS_DEFAULT_COLOR_INTERPOLATION_METHOD) {
        return None;
    }

    let (out_shape, out_size) = match shape {
        EndingShape::Circle(Circle::Radius(r)) => {
            // r: NonNegativeLength = NonNegative<Length>。.0.px() で CSS px、px_to_pt() で pt 化。
            let len_pt = px_to_pt(r.0.px());
            (
                RadialGradientShape::Circle,
                RadialGradientSize::Explicit {
                    rx: BgLengthPercentage::Length(len_pt),
                    ry: BgLengthPercentage::Length(len_pt),
                },
            )
        }
        EndingShape::Circle(Circle::Extent(ext)) => (
            RadialGradientShape::Circle,
            RadialGradientSize::Extent(map_extent(*ext)),
        ),
        EndingShape::Ellipse(Ellipse::Radii(rx, ry)) => (
            RadialGradientShape::Ellipse,
            RadialGradientSize::Explicit {
                rx: try_convert_lp_to_bg(&rx.0)?,
                ry: try_convert_lp_to_bg(&ry.0)?,
            },
        ),
        EndingShape::Ellipse(Ellipse::Extent(ext)) => (
            RadialGradientShape::Ellipse,
            RadialGradientSize::Extent(map_extent(*ext)),
        ),
    };

    // computed::Position::horizontal / vertical はどちらも LengthPercentage 直接 (wrapper なし)。
    // calc() 等 resolve 不能な値は silent 0 で誤描画させずに layer drop する。
    let position_x = try_convert_lp_to_bg(&position.horizontal)?;
    let position_y = try_convert_lp_to_bg(&position.vertical)?;

    let stops = resolve_color_stops(items, current_color, "radial-gradient")?;

    Some((
        BgImageContent::RadialGradient {
            shape: out_shape,
            size: out_size,
            position_x,
            position_y,
            stops,
            repeating,
        },
        0.0,
        0.0,
    ))
}

/// Convert Stylo `Gradient::Conic` into `BgImageContent::ConicGradient`.
///
/// stop position は `<angle>` を `angle / 2π` で fraction 化、`<percentage>` は
/// そのまま fraction として `GradientStopPosition::Fraction(f32)` に格納する。
/// `[0, 1]` 範囲外の生値もそのまま許容し (例: `-30deg → -0.083`, `120% → 1.2`)、
/// 最終的な周期展開 / 範囲ハンドリングは `background.rs::draw_conic_gradient`
/// と `sample_conic_color` 側に委ねる。
fn resolve_conic_gradient(
    g: &style::values::computed::Gradient,
    current_color: &style::color::AbsoluteColor,
) -> Option<(BgImageContent, f32, f32)> {
    use style::values::computed::AngleOrPercentage;
    use style::values::computed::image::Gradient;
    use style::values::generics::image::{GradientFlags, GradientItem};

    let (angle, position, items, flags) = match g {
        Gradient::Conic {
            angle,
            position,
            items,
            flags,
            ..
        } => (angle, position, items, flags),
        Gradient::Linear { .. } | Gradient::Radial { .. } => return None,
    };

    if !flags.contains(GradientFlags::HAS_DEFAULT_COLOR_INTERPOLATION_METHOD) {
        log::warn!(
            "conic-gradient: non-default color-interpolation-method is not yet \
             supported. Layer dropped."
        );
        return None;
    }

    let from_angle = angle.radians();
    let position_x = try_convert_lp_to_bg(&position.horizontal)?;
    let position_y = try_convert_lp_to_bg(&position.vertical)?;
    let repeating = flags.contains(GradientFlags::REPEATING);

    let mut stops: Vec<crate::pageable::GradientStop> = Vec::with_capacity(items.len());
    for item in items.iter() {
        use crate::pageable::{GradientStop, GradientStopPosition};
        match item {
            GradientItem::SimpleColorStop(c) => {
                let abs = c.resolve_to_absolute(current_color);
                stops.push(GradientStop {
                    position: GradientStopPosition::Auto,
                    rgba: absolute_to_rgba(abs),
                    is_hint: false,
                });
            }
            GradientItem::ComplexColorStop { color, position } => {
                let abs = color.resolve_to_absolute(current_color);
                let frac = match position {
                    AngleOrPercentage::Percentage(p) => p.0,
                    AngleOrPercentage::Angle(a) => a.radians() / std::f32::consts::TAU,
                };
                stops.push(GradientStop {
                    position: GradientStopPosition::Fraction(frac),
                    rgba: absolute_to_rgba(abs),
                    is_hint: false,
                });
            }
            GradientItem::InterpolationHint(_) => {
                log::warn!(
                    "conic-gradient: interpolation hints are not yet supported. \
                     Layer dropped."
                );
                return None;
            }
        }
    }
    if stops.len() < 2 {
        return None;
    }

    Some((
        BgImageContent::ConicGradient {
            from_angle,
            position_x,
            position_y,
            stops,
            repeating,
        },
        0.0,
        0.0,
    ))
}

fn map_extent(e: style::values::generics::image::ShapeExtent) -> crate::pageable::RadialExtent {
    use crate::pageable::RadialExtent;
    use style::values::generics::image::ShapeExtent;
    match e {
        ShapeExtent::ClosestSide => RadialExtent::ClosestSide,
        ShapeExtent::FarthestSide => RadialExtent::FarthestSide,
        ShapeExtent::ClosestCorner => RadialExtent::ClosestCorner,
        ShapeExtent::FarthestCorner => RadialExtent::FarthestCorner,
        // CSS Images §3.6.1: Contain == ClosestSide のエイリアス、Cover == FarthestCorner のエイリアス。
        ShapeExtent::Contain => RadialExtent::ClosestSide,
        ShapeExtent::Cover => RadialExtent::FarthestCorner,
    }
}

fn convert_bg_size(sizes: &[style::values::computed::BackgroundSize], i: usize) -> BgSize {
    use style::values::generics::background::BackgroundSize as StyloBS;
    use style::values::generics::length::GenericLengthPercentageOrAuto as LPAuto;
    let s = &sizes[i % sizes.len()];
    match s {
        StyloBS::Cover => BgSize::Cover,
        StyloBS::Contain => BgSize::Contain,
        StyloBS::ExplicitSize { width, height } => {
            let w = match width {
                LPAuto::Auto => None,
                LPAuto::LengthPercentage(lp) => Some(convert_lp_to_bg(&lp.0)),
            };
            let h = match height {
                LPAuto::Auto => None,
                LPAuto::LengthPercentage(lp) => Some(convert_lp_to_bg(&lp.0)),
            };
            if w.is_none() && h.is_none() {
                BgSize::Auto
            } else {
                BgSize::Explicit(w, h)
            }
        }
    }
}

/// Convert Stylo LengthPercentage to BgLengthPercentage.
/// Note: calc() values (e.g. `calc(50% + 10px)`) are not fully supported —
/// they fall back to 0.0 if neither pure percentage nor pure length.
/// 呼び出し側が "silent 0.0 で良い" 場面 (background-position / -size の Phase 1) のみ
/// 使うこと。radial-gradient の半径や中心位置のように 0 が誤描画になる場面では
/// `try_convert_lp_to_bg` を使って calc() を None にして bail する。
fn convert_lp_to_bg(lp: &style::values::computed::LengthPercentage) -> BgLengthPercentage {
    if let Some(pct) = lp.to_percentage() {
        BgLengthPercentage::Percentage(pct.0)
    } else {
        BgLengthPercentage::Length(lp.to_length().map(|l| px_to_pt(l.px())).unwrap_or(0.0))
    }
}

/// `convert_lp_to_bg` の Option 版。calc() 等の resolve 不能な値で `None` を返す。
/// silent 0.0 fallback では誤描画になる radial-gradient の半径 / 中心位置で使う
/// (CodeRabbit #238 で指摘)。
fn try_convert_lp_to_bg(
    lp: &style::values::computed::LengthPercentage,
) -> Option<BgLengthPercentage> {
    if let Some(pct) = lp.to_percentage() {
        Some(BgLengthPercentage::Percentage(pct.0))
    } else {
        lp.to_length()
            .map(|l| BgLengthPercentage::Length(px_to_pt(l.px())))
    }
}

fn convert_bg_position(
    pos_x: &[style::values::computed::LengthPercentage],
    pos_y: &[style::values::computed::LengthPercentage],
    i: usize,
) -> (BgLengthPercentage, BgLengthPercentage) {
    let px = &pos_x[i % pos_x.len()];
    let py = &pos_y[i % pos_y.len()];
    (convert_lp_to_bg(px), convert_lp_to_bg(py))
}

fn convert_bg_repeat(
    repeats: &[style::values::specified::background::BackgroundRepeat],
    i: usize,
) -> (BgRepeat, BgRepeat) {
    use style::values::specified::background::BackgroundRepeatKeyword as BRK;
    let r = &repeats[i % repeats.len()];
    let map = |k: BRK| match k {
        BRK::Repeat => BgRepeat::Repeat,
        BRK::NoRepeat => BgRepeat::NoRepeat,
        BRK::Space => BgRepeat::Space,
        BRK::Round => BgRepeat::Round,
    };
    (map(r.0), map(r.1))
}

fn convert_bg_origin(
    origins: &[style::properties::longhands::background_origin::single_value::computed_value::T],
    i: usize,
) -> BgBox {
    use style::properties::longhands::background_origin::single_value::computed_value::T as O;
    match origins[i % origins.len()] {
        O::BorderBox => BgBox::BorderBox,
        O::PaddingBox => BgBox::PaddingBox,
        O::ContentBox => BgBox::ContentBox,
    }
}

fn convert_bg_clip(
    clips: &[style::properties::longhands::background_clip::single_value::computed_value::T],
    i: usize,
) -> BgClip {
    use style::properties::longhands::background_clip::single_value::computed_value::T as C;
    match clips[i % clips.len()] {
        C::BorderBox => BgClip::BorderBox,
        C::PaddingBox => BgClip::PaddingBox,
        C::ContentBox => BgClip::ContentBox,
    }
}

#[cfg(test)]
mod tests {
    use crate::{AssetBundle, Engine};

    /// 1×1 red PNG (minimal valid file with correct CRC sums).
    const PNG_1X1_RED: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn render_bg(bg_css: &str) -> Vec<u8> {
        let html = format!(
            r#"<html><body><div style="width:120px;height:80px;background:{bg_css}"></div></body></html>"#
        );
        Engine::builder()
            .build()
            .render_html(&html)
            .expect("render should succeed")
    }

    fn assert_pdf(pdf: &[u8], label: &str) {
        assert!(pdf.starts_with(b"%PDF"), "{label}: expected PDF header");
    }

    fn render_with_style(style: &str, label: &str) {
        let html = format!(r#"<html><body><div style="{style}"></div></body></html>"#);
        let pdf = Engine::builder()
            .build()
            .render_html(&html)
            .unwrap_or_else(|_| panic!("render failed for {label}"));
        assert!(pdf.starts_with(b"%PDF"), "{label}: expected PDF");
    }

    // ── resolve_linear_gradient: direction arms ───────────────────────────────

    /// Exercises all nine `LineDirection` arms of `resolve_linear_gradient`:
    /// `Angle`, `Horizontal(Left)`, `Horizontal(Right)`, `Vertical(Top)`,
    /// `Vertical(Bottom)`, and all four `Corner` variants.
    #[test]
    fn linear_gradient_direction_variants() {
        let cases = [
            ("angle", "linear-gradient(45deg,red,blue)"),
            ("to_right", "linear-gradient(to right,red,blue)"),
            ("to_left", "linear-gradient(to left,red,blue)"),
            ("to_top", "linear-gradient(to top,red,blue)"),
            ("to_bottom", "linear-gradient(to bottom,red,blue)"),
            ("to_top_right", "linear-gradient(to top right,red,blue)"),
            ("to_top_left", "linear-gradient(to top left,red,blue)"),
            ("to_bottom_left", "linear-gradient(to bottom left,red,blue)"),
            (
                "to_bottom_right",
                "linear-gradient(to bottom right,red,blue)",
            ),
        ];
        for (name, css) in cases {
            assert_pdf(&render_bg(css), name);
        }
    }

    // ── resolve_radial_gradient: shape arms ───────────────────────────────────

    /// Exercises `EndingShape::Circle(Circle::Radius(...))`.
    #[test]
    fn radial_gradient_circle_explicit_radius() {
        assert_pdf(
            &render_bg("radial-gradient(circle 40px at center,red,blue)"),
            "circle_radius",
        );
    }

    /// Exercises `Circle::Extent` and `Ellipse::Extent` for all four
    /// `ShapeExtent` keyword values (ClosestSide / FarthestSide /
    /// ClosestCorner / FarthestCorner), which also covers `map_extent`.
    #[test]
    fn radial_gradient_extent_variants() {
        let cases = [
            (
                "circle_closest_side",
                "radial-gradient(closest-side circle,red,blue)",
            ),
            (
                "circle_farthest_side",
                "radial-gradient(farthest-side circle,red,blue)",
            ),
            (
                "circle_closest_corner",
                "radial-gradient(closest-corner circle,red,blue)",
            ),
            (
                "circle_farthest_corner",
                "radial-gradient(farthest-corner circle,red,blue)",
            ),
            (
                "ellipse_closest_side",
                "radial-gradient(closest-side ellipse,red,blue)",
            ),
            (
                "ellipse_farthest_side",
                "radial-gradient(farthest-side ellipse,red,blue)",
            ),
            (
                "ellipse_closest_corner",
                "radial-gradient(closest-corner ellipse,red,blue)",
            ),
            (
                "ellipse_farthest_corner",
                "radial-gradient(farthest-corner ellipse,red,blue)",
            ),
        ];
        for (name, css) in cases {
            assert_pdf(&render_bg(css), name);
        }
    }

    /// Exercises `EndingShape::Ellipse(Ellipse::Radii(...))` and the
    /// `try_convert_lp_to_bg` calls for both rx and ry.
    #[test]
    fn radial_gradient_ellipse_explicit_radii() {
        assert_pdf(
            &render_bg("radial-gradient(ellipse 50px 30px at center,red,blue)"),
            "ellipse_radii",
        );
    }

    /// Exercises the `position_x` / `position_y` arms of
    /// `resolve_radial_gradient` with a percentage center.
    #[test]
    fn radial_gradient_percentage_position() {
        assert_pdf(
            &render_bg("radial-gradient(circle at 25% 75%,red,blue)"),
            "radial_at_position",
        );
    }

    /// `repeating-radial-gradient` exercises the `repeating: true` flag.
    #[test]
    fn radial_gradient_repeating() {
        assert_pdf(
            &render_bg("repeating-radial-gradient(circle 30px,red,blue)"),
            "radial_repeating",
        );
    }

    // ── resolve_conic_gradient: arms ─────────────────────────────────────────

    /// Exercises the `from_angle` field and basic `SimpleColorStop` items.
    #[test]
    fn conic_gradient_from_angle() {
        assert_pdf(
            &render_bg("conic-gradient(from 45deg,red,blue)"),
            "conic_from_angle",
        );
    }

    /// Exercises position_x / position_y via `at <position>`.
    #[test]
    fn conic_gradient_at_position() {
        assert_pdf(
            &render_bg("conic-gradient(at 30% 70%,red,blue)"),
            "conic_at_pos",
        );
    }

    /// `ComplexColorStop` with `Percentage` → `GradientStopPosition::Fraction`.
    #[test]
    fn conic_gradient_percentage_stops() {
        assert_pdf(
            &render_bg("conic-gradient(red 0%,blue 50%,red 100%)"),
            "conic_pct_stops",
        );
    }

    /// `ComplexColorStop` with `Angle` → fraction via `angle / TAU`.
    #[test]
    fn conic_gradient_angle_stops() {
        assert_pdf(
            &render_bg("conic-gradient(red 0deg,blue 90deg,red 360deg)"),
            "conic_angle_stops",
        );
    }

    /// `repeating-conic-gradient` exercises the `repeating: true` flag.
    #[test]
    fn conic_gradient_repeating() {
        assert_pdf(
            &render_bg("repeating-conic-gradient(red 0deg,blue 30deg)"),
            "conic_repeating",
        );
    }

    // ── resolve_color_stops: GradientStopPosition variants ───────────────────

    /// `ComplexColorStop` with `to_length()` → `GradientStopPosition::LengthPx`.
    #[test]
    fn linear_gradient_length_px_stops() {
        assert_pdf(
            &render_bg("linear-gradient(red 0px,blue 80px)"),
            "length_px_stops",
        );
    }

    /// `ComplexColorStop` with `to_percentage()` → `GradientStopPosition::Fraction`.
    #[test]
    fn linear_gradient_fraction_stops() {
        assert_pdf(
            &render_bg("linear-gradient(red 0%,blue 100%)"),
            "fraction_stops",
        );
    }

    /// Valid interpolation hint between two color stops. The hint is at 30%
    /// and drives the power-curve expansion in `expand_interpolation_hints`.
    #[test]
    fn linear_gradient_valid_interpolation_hint() {
        assert_pdf(&render_bg("linear-gradient(red,30%,blue)"), "valid_hint");
    }

    /// Invalid hint placements cause `resolve_color_stops` to return `None`
    /// (layer dropped). The element still renders — just without the gradient.
    #[test]
    fn linear_gradient_invalid_hint_cases_drop_layer() {
        let cases = [
            ("leading_hint", "linear-gradient(30%,red,blue)"),
            ("trailing_hint", "linear-gradient(red,blue,70%)"),
            ("consecutive_hints", "linear-gradient(red,30%,60%,blue)"),
        ];
        for (name, css) in cases {
            assert_pdf(&render_bg(css), name);
        }
    }

    // ── convert_bg_size: BackgroundSize variant arms ──────────────────────────

    /// All `background-size` keyword / value combinations exercise every arm
    /// of `convert_bg_size` (Cover, Contain, ExplicitSize with various
    /// auto/length/percentage combinations).
    #[test]
    fn bg_size_variants() {
        let cases = [
            ("cover", "background-size:cover"),
            ("contain", "background-size:contain"),
            ("explicit_px", "background-size:50px 40px"),
            ("auto_height", "background-size:50px auto"),
            ("auto_width", "background-size:auto 40px"),
            ("auto_auto", "background-size:auto auto"),
            ("pct", "background-size:50% 50%"),
        ];
        for (name, size_css) in cases {
            render_with_style(
                &format!("width:120px;height:80px;background:linear-gradient(red,blue);{size_css}"),
                name,
            );
        }
    }

    // ── convert_bg_repeat: BackgroundRepeat variant arms ─────────────────────

    /// All four `BackgroundRepeatKeyword` values exercise every arm of
    /// `convert_bg_repeat`.
    #[test]
    fn bg_repeat_variants() {
        let cases = [
            ("repeat", "background-repeat:repeat"),
            ("no_repeat", "background-repeat:no-repeat"),
            ("space", "background-repeat:space"),
            ("round", "background-repeat:round"),
        ];
        for (name, repeat_css) in cases {
            render_with_style(
                &format!(
                    "width:120px;height:80px;background:linear-gradient(red,blue);background-size:30px 20px;{repeat_css}"
                ),
                name,
            );
        }
    }

    // ── convert_bg_origin / convert_bg_clip: box-type variant arms ───────────

    /// All three `background-origin` + `background-clip` keyword pairs exercise
    /// every arm of `convert_bg_origin` and `convert_bg_clip`.
    #[test]
    fn bg_origin_clip_variants() {
        let cases = [
            (
                "border_box",
                "background-origin:border-box;background-clip:border-box",
            ),
            (
                "padding_box",
                "background-origin:padding-box;background-clip:padding-box",
            ),
            (
                "content_box",
                "background-origin:content-box;background-clip:content-box",
            ),
        ];
        for (name, extra_css) in cases {
            render_with_style(
                &format!(
                    "width:120px;height:80px;padding:10px;background:linear-gradient(red,blue);{extra_css}"
                ),
                name,
            );
        }
    }

    // ── apply_to: background-image URL arms ──────────────────────────────────

    /// `Image::Url` → `AssetKind::Raster` → `BgImageContent::Raster` arm.
    #[test]
    fn bg_image_url_raster_png() {
        let mut bundle = AssetBundle::default();
        bundle.add_image("dot.png", PNG_1X1_RED.to_vec());
        let html = r#"<html><body><div style="width:80px;height:80px;background:url(dot.png)"></div></body></html>"#;
        let pdf = Engine::builder()
            .assets(bundle)
            .build()
            .render_html(html)
            .expect("render raster bg");
        assert!(pdf.starts_with(b"%PDF"));
    }

    /// `Image::Url` → `AssetKind::Svg` → `BgImageContent::Svg` arm.
    #[test]
    fn bg_image_url_svg() {
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='10' height='10' fill='red'/></svg>";
        let mut bundle = AssetBundle::default();
        bundle.add_image("bg.svg", svg.to_vec());
        let html = r#"<html><body><div style="width:80px;height:80px;background:url(bg.svg)"></div></body></html>"#;
        let pdf = Engine::builder()
            .assets(bundle)
            .build()
            .render_html(html)
            .expect("render svg bg");
        assert!(pdf.starts_with(b"%PDF"));
    }

    /// `Image::Url` → SVG bytes that fail `usvg::Tree::from_data` → `None` arm
    /// (layer dropped; element still renders).
    #[test]
    fn bg_image_url_invalid_svg_falls_back() {
        let mut bundle = AssetBundle::default();
        bundle.add_image("broken.svg", b"<svg<<NOT_VALID_XML".to_vec());
        let html = r#"<html><body><div style="width:80px;height:80px;background:url(broken.svg)"></div></body></html>"#;
        let pdf = Engine::builder()
            .assets(bundle)
            .build()
            .render_html(html)
            .expect("render broken-svg bg");
        assert!(pdf.starts_with(b"%PDF"));
    }

    /// `Image::Url` → `AssetKind::Unknown` arm → `None` (layer dropped).
    #[test]
    fn bg_image_url_unknown_asset_kind() {
        let mut bundle = AssetBundle::default();
        bundle.add_image("file.dat", b"NOT_AN_IMAGE_OR_SVG".to_vec());
        let html = r#"<html><body><div style="width:80px;height:80px;background:url(file.dat)"></div></body></html>"#;
        let pdf = Engine::builder()
            .assets(bundle)
            .build()
            .render_html(html)
            .expect("render unknown-asset bg");
        assert!(pdf.starts_with(b"%PDF"));
    }

    /// `Image::Url` when no `AssetBundle` is set → `ctx.assets.is_none()` →
    /// `and_then` returns `None` (layer dropped silently).
    #[test]
    fn bg_image_url_without_bundle_returns_none() {
        let html = r#"<html><body><div style="width:80px;height:80px;background:url(img.png)"></div></body></html>"#;
        let pdf = Engine::builder()
            .build()
            .render_html(html)
            .expect("render missing-bundle bg");
        assert!(pdf.starts_with(b"%PDF"));
    }
}
