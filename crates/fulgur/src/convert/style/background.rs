//! background-color and background-image-layers extraction.

use super::{StyleContext, absolute_to_rgba};
use crate::convert::{extract_asset_name, px_to_pt};
use crate::image::ImagePageable;
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
                                ImagePageable::decode_dimensions(data, format).unwrap_or((1, 1));
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
            GradientItem::InterpolationHint(_) => {
                log::warn!(
                    "{gradient_kind}: interpolation hints are not yet supported \
                     (Phase 2). Layer dropped."
                );
                return None;
            }
        }
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
