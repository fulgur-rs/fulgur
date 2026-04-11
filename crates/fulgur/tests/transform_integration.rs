//! End-to-end integration tests for CSS `transform`.
//!
//! These tests exercise the full HTML → Blitz → convert pipeline and verify
//! that `TransformWrapperPageable` appears in the resulting Pageable tree
//! with a correct matrix. Unit tests for the matrix math and the stylo
//! parsing live in-crate; this file is the only place where all three
//! pieces (parser, converter, wrapper) are checked together.

use fulgur::config::PageSize;
use fulgur::engine::Engine;
use fulgur::pageable::{
    Affine2D, BlockPageable, Pageable, PositionedChild, TransformWrapperPageable,
};

/// Walk a Pageable subtree looking for the first `TransformWrapperPageable`.
///
/// Our test fixtures only produce `BlockPageable` containers around the
/// transformed element, so the walker only needs to descend through
/// `BlockPageable` and also look inside a `TransformWrapperPageable`'s inner
/// child in case the wrapper is not the outermost match we want. Keep this
/// minimal: adding more container types only matters if a test HTML starts
/// producing them.
fn find_transform_wrapper(root: &dyn Pageable) -> Option<&TransformWrapperPageable> {
    if let Some(w) = root.as_any().downcast_ref::<TransformWrapperPageable>() {
        return Some(w);
    }
    if let Some(block) = root.as_any().downcast_ref::<BlockPageable>() {
        for PositionedChild { child, .. } in &block.children {
            if let Some(found) = find_transform_wrapper(child.as_ref()) {
                return Some(found);
            }
        }
    }
    None
}

fn build_tree(html: &str) -> Box<dyn Pageable> {
    let engine = Engine::builder().build();
    engine.build_pageable_for_testing(html)
}

fn wrapper_from(html: &str) -> TransformWrapperPageable {
    let tree = build_tree(html);
    find_transform_wrapper(tree.as_ref())
        .cloned()
        .expect("expected a TransformWrapperPageable in the tree")
}

fn approx(actual: f32, expected: f32, tol: f32, label: &str) {
    assert!(
        (actual - expected).abs() < tol,
        "{label}: expected {expected}, got {actual} (diff={})",
        (actual - expected).abs()
    );
}

const WRAP_HTML_PRE: &str = "<!DOCTYPE html><html><head><style>\
    .t { width: 100px; height: 100px; background: red; ";
const WRAP_HTML_POST: &str = " }</style></head><body><div class=\"t\">x</div></body></html>";

fn make_html(extra_style: &str) -> String {
    format!("{WRAP_HTML_PRE}{extra_style}{WRAP_HTML_POST}")
}

// ─── Geometry tests ──────────────────────────────────────────

#[test]
fn translate_px() {
    let html = make_html("transform: translate(10px, 20px);");
    let w = wrapper_from(&html);
    // For pure translations, T(ox, oy) · M · T(-ox, -oy) = M regardless of origin,
    // so the effective matrix at any draw point equals the raw matrix (plus the
    // draw-point's own translation, which we cancel by passing (0, 0)).
    let m = w.effective_matrix(0.0, 0.0);
    approx(m.a, 1.0, 1e-5, "translate.a");
    approx(m.b, 0.0, 1e-5, "translate.b");
    approx(m.c, 0.0, 1e-5, "translate.c");
    approx(m.d, 1.0, 1e-5, "translate.d");
    approx(m.e, 10.0, 1e-5, "translate.e");
    approx(m.f, 20.0, 1e-5, "translate.f");
}

#[test]
fn rotate_90_at_top_left_origin() {
    let html = make_html("transform: rotate(90deg); transform-origin: 0 0;");
    let w = wrapper_from(&html);
    let m = w.effective_matrix(0.0, 0.0);
    // Apply m to the point (1, 0): a*1 + c*0 + e = a, b*1 + d*0 + f = b.
    // After a +90° rotation (1, 0) should land at (0, 1).
    let x1 = m.a * 1.0 + m.c * 0.0 + m.e;
    let y1 = m.b * 1.0 + m.d * 0.0 + m.f;
    approx(x1, 0.0, 1e-5, "rotate90.x");
    approx(y1, 1.0, 1e-5, "rotate90.y");
}

#[test]
fn rotate_90_at_default_center_origin_fixes_center() {
    let html = make_html("transform: rotate(90deg);");
    let w = wrapper_from(&html);
    let m = w.effective_matrix(0.0, 0.0);
    // Default origin is 50% 50%, i.e. (50, 50) for a 100×100 box.
    // The origin point is the fixed point of the transform.
    let x = m.a * 50.0 + m.c * 50.0 + m.e;
    let y = m.b * 50.0 + m.d * 50.0 + m.f;
    approx(x, 50.0, 1e-4, "rotate90-center.x");
    approx(y, 50.0, 1e-4, "rotate90-center.y");
}

#[test]
fn scale_has_correct_diagonal() {
    let html = make_html("transform: scale(2, 3); transform-origin: 0 0;");
    let w = wrapper_from(&html);
    let m = w.effective_matrix(0.0, 0.0);
    approx(m.a, 2.0, 1e-5, "scale.a");
    approx(m.d, 3.0, 1e-5, "scale.d");
    approx(m.b, 0.0, 1e-5, "scale.b");
    approx(m.c, 0.0, 1e-5, "scale.c");
    approx(m.e, 0.0, 1e-5, "scale.e");
    approx(m.f, 0.0, 1e-5, "scale.f");
}

#[test]
fn matrix_preserved_with_origin_zero() {
    let html = make_html("transform: matrix(1, 2, 3, 4, 5, 6); transform-origin: 0 0;");
    let w = wrapper_from(&html);
    // With origin (0, 0) the conjugation collapses to the identity on both
    // sides, so the stored raw matrix should round-trip verbatim.
    assert_eq!(
        w.matrix,
        Affine2D {
            a: 1.0,
            b: 2.0,
            c: 3.0,
            d: 4.0,
            e: 5.0,
            f: 6.0,
        }
    );
}

#[test]
fn skew_x_45_has_correct_shear() {
    let html = make_html("transform: skewX(45deg); transform-origin: 0 0;");
    let w = wrapper_from(&html);
    let m = w.effective_matrix(0.0, 0.0);
    // tan(45°) = 1.0 → the c (xy-shear) component.
    approx(m.a, 1.0, 1e-5, "skewX.a");
    approx(m.b, 0.0, 1e-5, "skewX.b");
    approx(m.c, 1.0, 1e-4, "skewX.c");
    approx(m.d, 1.0, 1e-5, "skewX.d");
}

#[test]
fn composition_right_to_left() {
    let html = make_html("transform: translate(10px, 0) rotate(90deg); transform-origin: 0 0;");
    let w = wrapper_from(&html);
    let m = w.effective_matrix(0.0, 0.0);
    // CSS transforms apply right-to-left: rotate first, then translate.
    // point (1, 0) → rotate90 → (0, 1) → translate(10, 0) → (10, 1).
    let x = m.a * 1.0 + m.c * 0.0 + m.e;
    let y = m.b * 1.0 + m.d * 0.0 + m.f;
    approx(x, 10.0, 1e-4, "compose.x");
    approx(y, 1.0, 1e-4, "compose.y");
}

#[test]
fn translate3d_does_not_panic_and_is_suppressed() {
    // translate3d is 3D-only and fulgur is a 2D PDF pipeline. The converter
    // should not panic; it should also not wrap the element, because
    // compute_transform rejects 3D transforms as non-representable.
    let html = make_html("transform: translate3d(0, 0, 50px);");
    let tree = build_tree(&html);
    assert!(
        find_transform_wrapper(tree.as_ref()).is_none(),
        "translate3d should not produce a TransformWrapperPageable"
    );
}

#[test]
fn identity_transform_does_not_generate_wrapper() {
    let html = make_html("transform: translate(0, 0);");
    let tree = build_tree(&html);
    assert!(
        find_transform_wrapper(tree.as_ref()).is_none(),
        "identity transform should not produce a TransformWrapperPageable"
    );
}

// ─── Pagination smoke test ───────────────────────────────────

#[test]
fn transformed_element_produces_expected_pagination() {
    // Small page, one transformed element. A TransformWrapperPageable is
    // atomic — it never splits — so even if the inner element's pre-transform
    // height exceeds the content area, pagination places the whole subtree on
    // a single page. We assert the PDF bytes are well-formed and exactly one
    // page is emitted.
    let html = r#"<!DOCTYPE html><html><head><style>
        @page { size: 100pt 120pt; margin: 10pt; }
        .t { width: 60pt; height: 200pt; background: red;
             transform: rotate(45deg); transform-origin: 0 0; }
        </style></head><body><div class="t">x</div></body></html>"#;
    let engine = Engine::builder().page_size(PageSize::A4).build();
    let pdf = engine.render_html(html).expect("render should succeed");
    assert!(pdf.starts_with(b"%PDF-"), "PDF header missing");

    // Count `/Type /Page` occurrences (excluding `/Type /Pages`). A simple
    // substring scan is enough for this smoke test.
    let needle = b"/Type /Page\n";
    let mut count = 0usize;
    let mut i = 0;
    while i + needle.len() <= pdf.len() {
        if &pdf[i..i + needle.len()] == needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    assert_eq!(
        count, 1,
        "expected exactly one page for an atomic transformed element, got {count}"
    );
}
