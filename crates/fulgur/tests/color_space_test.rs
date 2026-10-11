//! End-to-end guard for CSS colour-space conversion (fulgur-xfzo).
//!
//! Stylo hands fulgur an `AbsoluteColor` still in the space the author wrote
//! (`hsl()` keeps `hue°/saturation/lightness`, `oklch()` keeps `L/C/hue°`), so
//! the draw path has to convert to sRGB before reading the components. These
//! tests state that requirement the way a user would notice it: a colour
//! written in a non-sRGB space must render exactly like the same colour
//! written as `rgb()`.
//!
//! Byte-comparing two PDFs is a strong assertion here — fulgur is
//! deterministic (see the "Deterministic" design principle in CLAUDE.md), so
//! two documents that differ only in how the same colour is spelled must
//! produce identical bytes. Before the fix, `hsl(0,100%,50%)` emitted
//! `0 1 1 rg` (cyan) and `hsl(120,100%,50%)` emitted `1 1 1 rg` — white, and
//! therefore invisible against the page.

use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;

fn render(html: &str) -> Vec<u8> {
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .build()
        .render(html)
        .expect("render should succeed")
}

/// Render two documents that differ only in the spelling of one colour and
/// assert the bytes match.
fn assert_same_output(label: &str, authored: &str, srgb: &str) {
    let a = render(authored);
    let b = render(srgb);
    assert!(a.starts_with(b"%PDF"), "{label}: not a PDF");
    assert_eq!(
        a, b,
        "{label}: `{authored}` and `{srgb}` describe the same colour, so the \
         rendered PDFs must be byte-identical"
    );
}

fn background_doc(color: &str) -> String {
    format!(
        r#"<html><body><div style="width:120px;height:60px;background:{color}"></div></body></html>"#
    )
}

fn text_doc(color: &str) -> String {
    format!(r#"<html><body><p style="color:{color};font-size:20px">colour</p></body></html>"#)
}

fn border_doc(color: &str) -> String {
    format!(
        r#"<html><body><div style="width:120px;height:60px;border:8px solid {color}"></div></body></html>"#
    )
}

#[test]
fn hsl_background_matches_equivalent_rgb() {
    assert_same_output(
        "hsl red background",
        &background_doc("hsl(0,100%,50%)"),
        &background_doc("rgb(255,0,0)"),
    );
}

/// The regression that hid itself: green written as `hsl()` clamped to white
/// and vanished against the page, with no warning and no error.
#[test]
fn hsl_green_background_is_not_white() {
    assert_same_output(
        "hsl green background",
        &background_doc("hsl(120,100%,50%)"),
        &background_doc("rgb(0,255,0)"),
    );

    let green = render(&background_doc("hsl(120,100%,50%)"));
    let white = render(&background_doc("rgb(255,255,255)"));
    assert_ne!(
        green, white,
        "`hsl(120,100%,50%)` must not collapse to white"
    );
}

/// `hsl()` with zero saturation is a neutral grey; the raw components used to
/// clamp to `(0, 0, 1)` and paint pure blue instead.
#[test]
fn hsl_grey_background_matches_equivalent_rgb() {
    assert_same_output(
        "hsl grey background",
        &background_doc("hsl(0,0%,50%)"),
        &background_doc("rgb(128,128,128)"),
    );
}

#[test]
fn hwb_background_matches_equivalent_rgb() {
    assert_same_output(
        "hwb red background",
        &background_doc("hwb(0 0% 0%)"),
        &background_doc("rgb(255,0,0)"),
    );
}

/// Text colour goes through a different extractor than backgrounds
/// (`convert/mod.rs` rather than `convert/style/background.rs`), so cover it
/// separately.
#[test]
fn hsl_text_colour_matches_equivalent_rgb() {
    assert_same_output(
        "hsl text colour",
        &text_doc("hsl(240,100%,50%)"),
        &text_doc("rgb(0,0,255)"),
    );
}

/// Borders are extracted by `convert/style/border.rs`.
#[test]
fn hsl_border_colour_matches_equivalent_rgb() {
    assert_same_output(
        "hsl border colour",
        &border_doc("hsl(300,100%,50%)"),
        &border_doc("rgb(255,0,255)"),
    );
}

/// `oklch()` does not round-trip to an exact `rgb()` triple, so assert the
/// weaker property that actually matters: it renders, and it is not the
/// purple that the unconverted components produced.
#[test]
fn oklch_red_renders_as_red_not_purple() {
    let oklch = render(&background_doc("oklch(62.8% 0.2577 29.23)"));
    assert!(oklch.starts_with(b"%PDF"));

    let purple = render(&background_doc("rgb(160,66,255)"));
    assert_ne!(
        oklch, purple,
        "`oklch()` must not be read as raw sRGB components"
    );

    let blank = render("<html><body><div style=\"width:120px;height:60px\"></div></body></html>");
    assert_ne!(oklch, blank, "`oklch()` background must actually paint");
}

/// Guard the untouched path: plain sRGB spellings must keep rendering
/// identically to each other, so the added conversion is a no-op there.
#[test]
fn srgb_spellings_remain_interchangeable() {
    assert_same_output(
        "named vs hex",
        &background_doc("red"),
        &background_doc("#ff0000"),
    );
    assert_same_output(
        "hex vs rgb()",
        &background_doc("#ff0000"),
        &background_doc("rgb(255,0,0)"),
    );
}
