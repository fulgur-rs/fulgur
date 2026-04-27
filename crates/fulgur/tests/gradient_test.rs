//! Integration coverage for convert.rs gradient paths and the
//! background.rs `resolve_gradient_stops` helper.
//!
//! Visual / pixel-level checks live in `crates/fulgur-vrt`; that crate is
//! excluded from the codecov measurement (`cargo llvm-cov nextest --workspace
//! --exclude fulgur-vrt`). These tests therefore exist purely to drive the
//! convert / draw paths through `Engine::render_html` so coverage attribution
//! is recorded.
//!
//! Each test renders a full PDF through fulgur and asserts the bytes start
//! with the PDF header. We do not pixel-compare here — we just need every
//! convert branch (`Auto` / `Fraction` / `LengthPx` / `InterpolationHint`,
//! linear / radial) and every draw fixup branch (length resolution, monotonic
//! clamp, out-of-range drop) to execute.

use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

fn render(css_background: &str) -> Vec<u8> {
    let html = format!(
        r#"<html><body><div style="width:200px;height:100px;background:{css_background}">x</div></body></html>"#
    );
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render_html(&html)
        .expect("render gradient")
}

fn assert_pdf(pdf: &[u8]) {
    assert!(
        pdf.starts_with(b"%PDF"),
        "expected PDF, got {} bytes",
        pdf.len()
    );
}

// ---------- linear-gradient: convert.rs::resolve_color_stops branches ----

#[test]
fn linear_gradient_auto_stops_render() {
    // SimpleColorStop arms (2x) → GradientStopPosition::Auto. Exercises
    // background::resolve_gradient_stops endpoint-fill (Auto → 0.0 / 1.0).
    assert_pdf(&render("linear-gradient(red, blue)"));
}

#[test]
fn linear_gradient_percentage_stops_render() {
    // ComplexColorStop with `to_percentage()` → Fraction(f).
    assert_pdf(&render("linear-gradient(red 0%, blue 100%)"));
}

#[test]
fn linear_gradient_length_px_stops_render() {
    // ComplexColorStop with `to_length()` → LengthPx(px). The new path
    // gated by this PR (fulgur-78sl).
    assert_pdf(&render(
        "linear-gradient(red 0px, blue 50px, blue 150px, green 200px)",
    ));
}

#[test]
fn linear_gradient_mixed_auto_and_fraction_render() {
    // Mixed Auto + Fraction; exercises middle Auto interpolation in the
    // draw-time fixup loop.
    assert_pdf(&render("linear-gradient(red 0%, blue, green 100%)"));
}

#[test]
fn linear_gradient_mixed_auto_length_render() {
    // Mixed Auto + LengthPx; exercises the LengthPx path together with
    // endpoint Auto fill.
    assert_pdf(&render("linear-gradient(red, blue 50px, green)"));
}

#[test]
fn linear_gradient_interpolation_hint_drops_layer() {
    // InterpolationHint arm → log::warn + None. Layer is dropped, but
    // overall render still succeeds (background just becomes solid white).
    assert_pdf(&render("linear-gradient(red, 30%, blue)"));
}

#[test]
fn linear_gradient_out_of_range_fraction_drops_layer() {
    // Fraction(-0.5) reaches background::resolve_gradient_stops, fails
    // range check, layer dropped (handoff to fulgur-n3zk).
    assert_pdf(&render("linear-gradient(red -50%, blue 100%)"));
}

#[test]
fn linear_gradient_with_angle_render() {
    // `60deg` exercises `|W·sinθ| + |H·cosθ|` gradient line length formula
    // in draw-time helper (CSS Images §3.5.1).
    assert_pdf(&render(
        "linear-gradient(60deg, red 0px, blue 50px, green 100%)",
    ));
}

// ---------- radial-gradient: shared resolve_color_stops + radial draw ----

#[test]
fn radial_gradient_auto_stops_render() {
    assert_pdf(&render("radial-gradient(red, blue)"));
}

#[test]
fn radial_gradient_percentage_stops_render() {
    assert_pdf(&render("radial-gradient(red 0%, blue 100%)"));
}

#[test]
fn radial_gradient_length_px_stops_render() {
    // px stops resolved against rx (CSS Images §3.6.1).
    assert_pdf(&render(
        "radial-gradient(circle 100px at center, red 0px, blue 50px, blue 100px)",
    ));
}

#[test]
fn radial_gradient_ellipse_with_length_stops_render() {
    // Ellipse path: gradient ray length = rx (X-axis radius), even when ry differs.
    assert_pdf(&render(
        "radial-gradient(ellipse 100px 50px at center, red 0px, blue 50px, green 100px)",
    ));
}

#[test]
fn radial_gradient_interpolation_hint_drops_layer() {
    assert_pdf(&render("radial-gradient(red, 30%, blue)"));
}
