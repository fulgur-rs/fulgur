//! Paint used text decoration segments supplied by Raikiri.

use super::fill;
use krilla::geom::{Path, PathBuilder};
use krilla::paint::{LineCap, Stroke, StrokeDash};
use krilla::surface::Surface;
use raikiri_html::{DecorationKind, DecorationLine, DecorationStyle};

const MAX_WAVES: usize = 4096;

#[derive(Clone, Copy)]
pub(super) enum Phase {
    BeforeGlyphs,
    AfterGlyphs,
}

pub(super) fn paint(surface: &mut Surface<'_>, lines: &[DecorationLine], phase: Phase) {
    for line in lines {
        let before = line.kind != DecorationKind::LineThrough;
        if before != matches!(phase, Phase::BeforeGlyphs) || line.color.a == 0 {
            continue;
        }
        if ![
            line.x_start,
            line.x_end,
            line.y,
            line.thickness,
            line.pattern_origin_x,
            line.pattern_end_x,
        ]
        .into_iter()
        .all(f32::is_finite)
            || line.x_end <= line.x_start
            || line.thickness <= 0.0
            || line.pattern_end_x <= line.pattern_origin_x
        {
            continue;
        }
        let t = line.thickness;
        let pattern_span = line.pattern_end_x - line.pattern_origin_x;
        match line.style {
            DecorationStyle::Double => {
                let band = (t / 3.0).max(0.5);
                rect(surface, line, line.y - band, band);
                rect(surface, line, line.y + band, band);
            }
            DecorationStyle::Dashed | DecorationStyle::Dotted => {
                let dotted = line.style == DecorationStyle::Dotted;
                let width = if dotted { t.max(1.0) } else { t };
                let (array, period, origin) = if dotted {
                    let period = (width * 2.0).max(pattern_span / MAX_WAVES as f32);
                    (
                        vec![0.0, period],
                        period,
                        line.pattern_origin_x + width * 0.5,
                    )
                } else {
                    let dash = (t * 3.0).max(2.0);
                    let gap = (t * 2.0).max(2.0);
                    let scale = (pattern_span / (MAX_WAVES as f32 * (dash + gap))).max(1.0);
                    let (dash, gap) = (dash * scale, gap * scale);
                    (vec![dash, gap], dash + gap, line.pattern_origin_x)
                };
                // Include neighboring dot centers before clipping the round caps.
                let path_start = if dotted {
                    origin + ((line.x_start - width * 0.5 - origin) / period).floor() * period
                } else {
                    line.x_start
                };
                let path_end = if dotted {
                    line.x_end + width * 0.5
                } else {
                    line.x_end
                };
                let mut path = PathBuilder::new();
                path.move_to(path_start, line.y);
                path.line_to(path_end, line.y);
                if let Some(path) = path.finish() {
                    surface.set_fill(None);
                    let color = fill(line.color);
                    surface.set_stroke(Some(Stroke {
                        paint: color.paint,
                        opacity: color.opacity,
                        width,
                        line_cap: if dotted {
                            LineCap::Round
                        } else {
                            LineCap::Butt
                        },
                        dash: Some(StrokeDash {
                            array,
                            offset: if dotted {
                                0.0
                            } else {
                                (path_start - origin).rem_euclid(period)
                            },
                        }),
                        ..Default::default()
                    }));
                    // Round caps must not paint past a segment's used endpoints.
                    if let Some(clip) =
                        rectangle(line.x_start, line.y - width, line.x_end, line.y + width)
                    {
                        surface.push_clip_path(&clip, &Default::default());
                        surface.draw_path(&path);
                        surface.pop();
                    }
                    surface.set_stroke(None);
                }
            }
            DecorationStyle::Wavy => {
                let span = pattern_span;
                let half_wave = ((t * 4.0).max(4.0) * 0.5).max(span / MAX_WAVES as f32);
                let amplitude = (t * 1.5).max(0.75);
                let mut index = ((line.x_start - line.pattern_origin_x) / half_wave).floor();
                let mut x = line.pattern_origin_x + index * half_wave;
                let mut path = PathBuilder::new();
                path.move_to(x, line.y);
                for _ in 0..=MAX_WAVES {
                    if x >= line.x_end {
                        break;
                    }
                    let end = x + half_wave;
                    if end <= x {
                        break;
                    }
                    let sign = if index.rem_euclid(2.0) < 1.0 {
                        -1.0
                    } else {
                        1.0
                    };
                    // Convert a quadratic half-wave to a cubic Bézier.
                    let cy = line.y + sign * amplitude;
                    path.cubic_to(
                        x + half_wave / 3.0,
                        line.y + (cy - line.y) * 2.0 / 3.0,
                        end - half_wave / 3.0,
                        line.y + (cy - line.y) * 2.0 / 3.0,
                        end,
                        line.y,
                    );
                    x = end;
                    index += 1.0;
                }
                if let (Some(path), Some(clip)) = (
                    path.finish(),
                    rectangle(
                        line.x_start,
                        line.y - amplitude - t,
                        line.x_end,
                        line.y + amplitude + t,
                    ),
                ) {
                    let color = fill(line.color);
                    surface.set_fill(None);
                    surface.set_stroke(Some(Stroke {
                        paint: color.paint,
                        opacity: color.opacity,
                        width: t,
                        line_cap: LineCap::Round,
                        ..Default::default()
                    }));
                    surface.push_clip_path(&clip, &Default::default());
                    surface.draw_path(&path);
                    surface.pop();
                    surface.set_stroke(None);
                }
            }
            _ => rect(surface, line, line.y, t),
        }
    }
}

fn rectangle(x0: f32, y0: f32, x1: f32, y1: f32) -> Option<Path> {
    let mut path = PathBuilder::new();
    path.move_to(x0, y0);
    path.line_to(x1, y0);
    path.line_to(x1, y1);
    path.line_to(x0, y1);
    path.close();
    path.finish()
}

fn rect(surface: &mut Surface<'_>, line: &DecorationLine, y: f32, thickness: f32) {
    if let Some(path) = rectangle(
        line.x_start,
        y - thickness * 0.5,
        line.x_end,
        y + thickness * 0.5,
    ) {
        surface.set_stroke(None);
        surface.set_fill(Some(fill(line.color)));
        surface.draw_path(&path);
    }
}
