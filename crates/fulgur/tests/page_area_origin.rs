//! `snap_page_area_origin` starts the page area at a whole CSS pixel and
//! keeps its exact size, as Chromium prints: with 10 mm margins (37.8px)
//! the page area starts at 38px.

use fulgur::{Engine, Margin, PageSize};

const HTML: &str = r#"<!DOCTYPE html><html><body style="margin:0">
<div style="height:50px; background:red"></div></body></html>"#;

/// 10 mm in pt.
const MM10: f32 = 72.0 / 25.4 * 10.0;

fn render(snap: bool, scale: f32) -> Vec<u8> {
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(MM10))
        .clip_to_page_area(true)
        .snap_page_area_origin(snap)
        .scale(scale)
        .build()
        .render(HTML)
        .expect("render")
}

/// The bounding boxes `[x, y, w, h]` of the paths used as clips on page 1:
/// the points of the `m`/`l` operators before each `W`.
fn clip_rects(pdf: &[u8]) -> Vec<[f32; 4]> {
    let document = lopdf::Document::load_mem(pdf).expect("PDF should parse");
    let page_id = *document.get_pages().get(&1).expect("page 1");
    let content = lopdf::content::Content::decode(&document.get_page_content(page_id))
        .expect("page content stream should decode");
    let number = |o: &lopdf::Object| match o {
        lopdf::Object::Integer(i) => Some(*i as f32),
        lopdf::Object::Real(f) => Some(*f),
        _ => None,
    };
    let mut rects = Vec::new();
    let mut points: Vec<(f32, f32)> = Vec::new();
    for op in &content.operations {
        match op.operator.as_str() {
            "m" | "l" => {
                let n: Vec<f32> = op.operands.iter().filter_map(number).collect();
                if op.operator == "m" {
                    points.clear();
                }
                if let [x, y] = n[..] {
                    points.push((x, y));
                }
            }
            "W" | "W*" if !points.is_empty() => {
                let xs = points.iter().map(|p| p.0);
                let ys = points.iter().map(|p| p.1);
                let (x0, x1) = (
                    xs.clone().fold(f32::MAX, f32::min),
                    xs.fold(f32::MIN, f32::max),
                );
                let (y0, y1) = (
                    ys.clone().fold(f32::MAX, f32::min),
                    ys.fold(f32::MIN, f32::max),
                );
                rects.push([x0, y0, x1 - x0, y1 - y0]);
            }
            "h" => {}
            _ => points.clear(),
        }
    }
    rects
}

/// The page area clip, in pt on the printed page.
fn page_area(pdf: &[u8]) -> [f32; 4] {
    let rects = clip_rects(pdf);
    *rects
        .iter()
        .find(|r| r[2] > 400.0 && r[3].abs() > 600.0)
        .unwrap_or_else(|| panic!("no page area clip in {rects:?}"))
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn the_page_area_starts_at_a_whole_pixel_with_its_exact_width() {
    let [x, _, w, h] = page_area(&render(true, 1.0));
    // 38px = 28.5pt, not 37.8px = 28.35pt.
    assert!(close(x, 28.5), "x {x}");
    assert!(close(w, PageSize::A4.width - 2.0 * MM10), "w {w}");
    assert!(close(h.abs(), PageSize::A4.height - 2.0 * MM10), "h {h}");
}

#[test]
fn the_page_area_starts_at_the_margin_by_default() {
    let [x, ..] = page_area(&render(false, 1.0));
    assert!(close(x, MM10), "x {x}");
}

#[test]
fn the_pixel_is_one_of_the_printed_page() {
    // At scale 0.5 the layout margin is 75.6px, drawn as 37.8px.
    let [x, ..] = page_area(&render(true, 0.5));
    assert!(close(x, 28.5), "x {x}");
}
