//! Verify whether Taffy's `final_layout.size` honours the explicit `width` /
//! `height` declarations on a `position: absolute` `::before` pseudo whose
//! `content` resolves to a `url(...)` image.
//!
//! The non-absolute pseudo path (`build_pseudo_image`) reads sizes directly
//! from computed styles because Blitz/Taffy does not propagate them to
//! `final_layout` for text-less pseudos. The absolute pseudo path now
//! re-emits the pseudo via `convert_node` → `convert_content_url`, which
//! sizes from `final_layout.size` instead. coderabbit flagged this as a
//! potential regression: if Taffy also drops the explicit width/height for
//! the abs pseudo, the image renders at the wrong (zero) size.
//!
//! This test pins the actual behaviour with a regression net so the
//! threshold is empirical, not speculative.

use fulgur::asset::AssetBundle;
use fulgur::config::{Margin, PageSize};
use fulgur::engine::Engine;
use fulgur::image::ImagePageable;
use fulgur::pageable::{BlockPageable, Pageable, PositionedChild};

const MINIMAL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

fn collect_images<'a>(root: &'a dyn Pageable, out: &mut Vec<&'a ImagePageable>) {
    if let Some(img) = root.as_any().downcast_ref::<ImagePageable>() {
        out.push(img);
    }
    if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
        for PositionedChild { child, .. } in &block.children {
            collect_images(child.as_ref(), out);
        }
    }
}

#[test]
fn absolute_pseudo_with_content_url_honours_explicit_size() {
    let mut assets = AssetBundle::new();
    assets.add_image("dot.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();

    // 100 CSS px → 75 PDF pt (PX_TO_PT = 0.75). Sized only via CSS on the
    // pseudo — the parent has no in-flow content that could push Taffy to
    // size the pseudo via fallback heuristics.
    let html = r#"<!DOCTYPE html><html><head><style>
        .marker { position: relative; width: 0; height: 0; }
        .marker::before {
            content: url(dot.png);
            position: absolute;
            width: 100px;
            height: 100px;
            left: 0;
            top: 0;
        }
    </style></head><body><div class="marker"></div></body></html>"#;

    let tree = engine.build_pageable_for_testing_no_gcpm(html);
    let mut imgs = Vec::new();
    collect_images(tree.as_ref(), &mut imgs);

    assert!(
        !imgs.is_empty(),
        "expected at least one ImagePageable from the abs pseudo, got 0"
    );
    // We accept any image whose size matches the explicit CSS dimensions —
    // there should be exactly one (the pseudo's content url image).
    let want = (75.0_f32, 75.0_f32);
    let matched = imgs
        .iter()
        .find(|img| (img.width - want.0).abs() < 0.5 && (img.height - want.1).abs() < 0.5);
    assert!(
        matched.is_some(),
        "expected an ImagePageable sized {:?} pt (100 CSS px), got {:?}",
        want,
        imgs.iter().map(|i| (i.width, i.height)).collect::<Vec<_>>()
    );
}
