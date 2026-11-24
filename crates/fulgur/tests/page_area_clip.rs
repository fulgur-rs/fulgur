//! `clip_to_page_area` clips page content to the page inside its margins,
//! as Chromium does when printing.

use fulgur::{Engine, Margin, PageSize};

const HTML: &str = r#"<!DOCTYPE html><html><body style="margin:0">
<div style="position:relative; height:50px">
  <div style="position:absolute; right:-40px; width:80px; height:30px; background:red"></div>
</div></body></html>"#;

fn render(clip: bool) -> Vec<u8> {
    Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .clip_to_page_area(clip)
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

/// Whether a clip rectangle is the page area: A4 inside 1in margins.
fn has_page_area_clip(rects: &[[f32; 4]]) -> bool {
    let (w, h) = (PageSize::A4.width - 144.0, PageSize::A4.height - 144.0);
    rects.iter().any(|r| {
        (r[0] - 72.0).abs() < 0.1 && (r[2].abs() - w).abs() < 0.1 && (r[3].abs() - h).abs() < 0.1
    })
}

#[test]
fn content_is_clipped_to_the_page_area() {
    let rects = clip_rects(&render(true));
    assert!(has_page_area_clip(&rects), "clips: {rects:?}");
}

#[test]
fn content_is_not_clipped_by_default() {
    let rects = clip_rects(&render(false));
    assert!(!has_page_area_clip(&rects), "clips: {rects:?}");
}
