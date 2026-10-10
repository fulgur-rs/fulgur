use super::*;

const SHADOW_CSS: &str = "<style>@page {size:200px 150px;margin:0} body {margin:0;font-size:20px;color:black} p {margin:0}</style>";

fn text_shows(operations: &[lopdf::content::Operation]) -> Vec<usize> {
    operations
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op.operator.as_str(), "Tj" | "TJ"))
        .map(|(index, _)| index)
        .collect()
}

/// The fill colors set before each text show, in order.
fn text_colors(operations: &[lopdf::content::Operation]) -> Vec<[u8; 3]> {
    text_shows(operations)
        .into_iter()
        .map(|show| {
            *fill_colors(&operations[..show])
                .last()
                .expect("a fill color before the text")
        })
        .collect()
}

#[test]
fn text_without_shadow_is_drawn_once() {
    let (_, ops) = operations(&format!("{SHADOW_CSS}<p>Plain</p>"));
    assert_eq!(text_shows(&ops).len(), 1);
    assert_eq!(count(&ops, "Do"), 0);
}

/// The fill color of each path fill before the first text show, in order.
fn shadow_fills(operations: &[lopdf::content::Operation]) -> Vec<[u8; 3]> {
    let text = text_shows(operations)[0];
    operations[..text]
        .iter()
        .enumerate()
        .filter(|(_, op)| op.operator == "f")
        .map(|(index, _)| *fill_colors(&operations[..index]).last().unwrap())
        .collect()
}

#[test]
fn sharp_shadows_are_outlines_below_the_text_first_declared_on_top() {
    let (pdf, ops) = operations(&format!(
        "{SHADOW_CSS}<p style='text-shadow:1px 1px rgb(255,0,0), 2px 2px rgb(0,0,255)'>Shadow</p>"
    ));
    // The shadows are filled outlines, so the text is shown, and
    // extracted, once.
    assert_eq!(text_colors(&ops), [[0, 0, 0]]);
    assert_eq!(shadow_fills(&ops), [[0, 0, 255], [255, 0, 0]]);
    assert_eq!(pdf.extract_text(&[1]).unwrap().matches("Shadow").count(), 1);
}

#[test]
fn shadow_layers_span_every_run_of_the_line() {
    let (_, ops) = operations(&format!(
        "{SHADOW_CSS}<p style='text-shadow:1px 1px rgb(255,0,0), 2px 2px rgb(0,0,255)'>Split<span style='color:rgb(0,128,0)'>Run</span></p>"
    ));
    assert_eq!(text_shows(&ops).len(), 2);
    // Each layer is one shape over both runs, and the second shadow is
    // painted before the first, so no run's first shadow is covered.
    assert_eq!(shadow_fills(&ops), [[0, 0, 255], [255, 0, 0]]);
}

#[test]
fn current_color_shadow_uses_the_text_color() {
    let (_, ops) = operations(&format!(
        "{SHADOW_CSS}<p style='color:rgb(0,128,0);text-shadow:1px 1px'>Green</p>"
    ));
    assert_eq!(shadow_fills(&ops), [[0, 128, 0]]);
}

#[test]
fn transparent_shadow_is_skipped() {
    let (_, ops) = operations(&format!(
        "{SHADOW_CSS}<p style='text-shadow:1px 1px transparent, 1px 1px 3px transparent'>Clear</p>"
    ));
    assert_eq!(text_shows(&ops).len(), 1);
    assert_eq!(count(&ops, "Do"), 0);
}

#[test]
fn blurred_shadow_is_a_soft_masked_image_below_the_text() {
    let (pdf, ops) = operations(&format!(
        "{SHADOW_CSS}<p style='text-shadow:2px 2px 4px rgb(255,0,0)'>Blur</p>"
    ));
    let shows = text_shows(&ops);
    assert_eq!(shows.len(), 1);
    let image = ops
        .iter()
        .position(|op| op.operator == "Do")
        .expect("the shadow image");
    assert!(image < shows[0], "the shadow is painted before the text");
    let images: Vec<_> = pdf
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter(|stream| {
            stream
                .dict
                .get(b"Subtype")
                .is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Image"))
                && stream.dict.has(b"SMask")
        })
        .collect();
    assert_eq!(images.len(), 1);
    // The raster covers the text plus three standard deviations of blur on
    // each side, at three pixels per CSS px.
    let width = images[0].dict.get(b"Width").unwrap().as_i64().unwrap();
    let height = images[0].dict.get(b"Height").unwrap().as_i64().unwrap();
    assert!(width > 3 * 2 * 6, "{width}");
    assert!(height > 3 * 2 * 6, "{height}");
}

#[test]
fn blurred_shadow_of_a_split_line_is_one_image() {
    let (_, ops) = operations(&format!(
        "{SHADOW_CSS}<p style='text-shadow:2px 2px 4px rgb(255,0,0)'>Split<span style='color:rgb(0,128,0)'>Run</span></p>"
    ));
    assert_eq!(text_shows(&ops).len(), 2);
    assert_eq!(count(&ops, "Do"), 1);
}

#[test]
fn overflowing_shadow_offsets_are_skipped() {
    let (_, ops) = operations(&format!(
        "{SHADOW_CSS}<p style='text-shadow:1e40px 1px, 1px 1e40px 2px'>Far</p>"
    ));
    assert_eq!(text_shows(&ops).len(), 1);
    assert_eq!(count(&ops, "f"), 0);
    assert_eq!(count(&ops, "Do"), 0);
}
