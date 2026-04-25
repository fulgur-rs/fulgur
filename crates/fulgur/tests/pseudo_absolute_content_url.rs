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

/// Walk the tree and collect `(x, y, ImagePageable)` triples in the
/// coordinate frame of the nearest `BlockPageable` ancestor. Sufficient for
/// asserting the relative offset of an abs pseudo image inside its parent
/// — we don't need full page-space coordinates here.
fn collect_positioned_images<'a>(
    root: &'a dyn Pageable,
    parent_x: f32,
    parent_y: f32,
    out: &mut Vec<(f32, f32, &'a ImagePageable)>,
) {
    if let Some(img) = root.as_any().downcast_ref::<ImagePageable>() {
        out.push((parent_x, parent_y, img));
    }
    if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
        for PositionedChild { child, x, y } in &block.children {
            collect_positioned_images(child.as_ref(), parent_x + *x, parent_y + *y, out);
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

/// Regression: `right` / `bottom` insets on a textless `content: url(...)`
/// abs pseudo must be resolved against the pseudo's effective image size.
/// `pseudo.final_layout.size` is `(0, 0)` for textless pseudos (Blitz
/// limitation), so reading it directly makes `cb_w - pw - r` collapse to
/// `cb_w - r`, shifting the image off-canvas by its own width/height.
#[test]
fn absolute_pseudo_with_right_bottom_offsets_by_image_size() {
    let mut assets = AssetBundle::new();
    assets.add_image("dot.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();

    // Parent: 200x200 px = 150x150 pt.
    // Pseudo: 50x50 px = 37.5x37.5 pt at right:0; bottom:0.
    // Expected pseudo position relative to parent (in pt):
    //   x = 150 - 37.5 - 0 = 112.5
    //   y = 150 - 37.5 - 0 = 112.5
    // Bug case (pre-fix): pw/ph = 0, so x = y = 150.
    let html = r#"<!DOCTYPE html><html><head><style>
        .marker { position: relative; width: 200px; height: 200px; }
        .marker::before {
            content: url(dot.png);
            position: absolute;
            width: 50px;
            height: 50px;
            right: 0;
            bottom: 0;
        }
    </style></head><body><div class="marker"></div></body></html>"#;

    let tree = engine.build_pageable_for_testing_no_gcpm(html);
    let mut imgs = Vec::new();
    collect_positioned_images(tree.as_ref(), 0.0, 0.0, &mut imgs);

    // Find the pseudo image (37.5pt × 37.5pt).
    let pseudo = imgs
        .iter()
        .find(|(_, _, img)| (img.width - 37.5).abs() < 0.5 && (img.height - 37.5).abs() < 0.5)
        .unwrap_or_else(|| {
            panic!(
                "expected a 37.5pt × 37.5pt ImagePageable; got: {:?}",
                imgs.iter()
                    .map(|(x, y, i)| (*x, *y, i.width, i.height))
                    .collect::<Vec<_>>()
            )
        });

    // Position is in the parent's frame (parent itself is at some offset
    // inside the page; we only care about delta-to-parent here, which the
    // tree walk gives us once the marker .marker block is reached).
    // The pseudo's PositionedChild offset is from its parent block's
    // border-box origin. We assert the pseudo lands at (parent_w - img_w,
    // parent_h - img_h) = (112.5, 112.5) within its parent.
    //
    // Walk back from the absolute (page-frame) coordinate by subtracting
    // the parent block's offset. Since we don't know the parent's exact
    // page offset without inspecting the tree, we instead assert the
    // pseudo coordinate INSIDE its parent's frame by asserting both x and
    // y equal `parent_offset + 112.5` for some parent offset that is
    // shared across imgs of similar shape — but the simpler check is to
    // assert the pseudo is offset BY 112.5 from the marker's own origin.
    //
    // The only image in this fixture is the pseudo, and its accumulated
    // (x, y) in collect_positioned_images is (page-margin + parent-position
    // + 112.5). The bug case would put it at (page-margin + parent-position
    // + 150), an offset of +37.5 in both axes. So we can detect by
    // comparing to the parent block's own (x, y) — find the marker block.
    //
    // To keep the assertion robust, we walk the tree to find the nearest
    // BlockPageable ancestor of the image whose layout_size is 150×150 pt
    // and assert the image's offset from that ancestor's origin is
    // (112.5, 112.5).
    let (parent_origin_x, parent_origin_y) =
        find_marker_origin(tree.as_ref(), 0.0, 0.0).expect("marker block should exist");
    let want_x = parent_origin_x + 112.5;
    let want_y = parent_origin_y + 112.5;
    let (got_x, got_y, _) = pseudo;
    assert!(
        (got_x - want_x).abs() < 0.5 && (got_y - want_y).abs() < 0.5,
        "expected pseudo at ({:.2}, {:.2}) in page-frame (parent ({:.2}, {:.2}) + (112.5, 112.5)), got ({:.2}, {:.2}); bug case would be (~{:.2}, ~{:.2})",
        want_x,
        want_y,
        parent_origin_x,
        parent_origin_y,
        got_x,
        got_y,
        parent_origin_x + 150.0,
        parent_origin_y + 150.0,
    );
}

fn find_marker_origin(root: &dyn Pageable, x: f32, y: f32) -> Option<(f32, f32)> {
    if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
        if let Some(sz) = block.layout_size {
            if (sz.width - 150.0).abs() < 0.5 && (sz.height - 150.0).abs() < 0.5 {
                return Some((x, y));
            }
        }
        for PositionedChild {
            child,
            x: cx,
            y: cy,
        } in &block.children
        {
            if let Some(found) = find_marker_origin(child.as_ref(), x + *cx, y + *cy) {
                return Some(found);
            }
        }
    }
    None
}

/// Regression: percentage `width` / `height` on an abs `content: url(...)`
/// pseudo must resolve against the CB's padding-box in **pt** — the
/// `build_pseudo_image` helper does `pt_to_px(parent_width)` internally.
/// Passing CSS-px dims (as `cb.padding_box_size` is documented) makes the
/// percentage 4/3× too large.
#[test]
fn absolute_pseudo_percentage_size_resolves_against_padding_box_in_pt() {
    let mut assets = AssetBundle::new();
    assets.add_image("dot.png", MINIMAL_PNG.to_vec());
    let engine = Engine::builder()
        .page_size(PageSize::A4)
        .margin(Margin::uniform(72.0))
        .assets(assets)
        .build();

    // Parent: position:relative, 400 px wide. CB padding-box width = 400 px = 300 pt.
    // Pseudo: width: 50%; → expected = 150 pt.
    // Bug case: basis treated as pt, pt_to_px(400) = ~533, then *50%/2 →
    // px_to_pt(266) = 200 pt. (4/3× too large.)
    //
    // We do NOT specify height — let the image use its intrinsic 1px (=1pt)
    // height to keep the assertion focused on width.
    let html = r#"<!DOCTYPE html><html><head><style>
        .marker { position: relative; width: 400px; height: 100px; }
        .marker::before {
            content: url(dot.png);
            position: absolute;
            width: 50%;
            left: 0;
            top: 0;
        }
    </style></head><body><div class="marker"></div></body></html>"#;

    let tree = engine.build_pageable_for_testing_no_gcpm(html);
    let mut imgs = Vec::new();
    collect_images(tree.as_ref(), &mut imgs);

    assert!(
        !imgs.is_empty(),
        "expected an ImagePageable from the abs pseudo"
    );
    let pseudo = imgs
        .iter()
        .find(|img| img.width > 0.0)
        .expect("at least one non-zero image");
    let want_w = 150.0_f32;
    assert!(
        (pseudo.width - want_w).abs() < 1.0,
        "expected pseudo width {:.2} pt (50% of 300pt CB), got {:.2} pt; bug case would be ~200 pt",
        want_w,
        pseudo.width,
    );
}
