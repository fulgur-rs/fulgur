use super::*;
use crate::tests::{count, has_shading, operations_of};
use krilla::page::PageSettings;
use lopdf::content::Operation;
use raikiri_traits::vector::{
    Dash, FontFace, FontVariation, Glyph, GradientStop, Group, LineCap as SceneCap,
    LineJoin as SceneJoin, Node, PaintOrder, Path as ScenePath, PathSegment, RasterImage,
    SpreadMethod as SceneSpread, Stroke as SceneStroke, TextRun, VectorImage,
};
use skrifa::MetadataProvider;

const FONT: &[u8] = include_bytes!("../../../../../fulgur-ruby/spec/fixtures/noto_sans.ttf");

fn color(red: f32, green: f32, blue: f32, alpha: f32) -> PaintColor {
    PaintColor::new(red, green, blue, alpha)
}

fn solid(red: f32, green: f32, blue: f32) -> scene::Paint {
    scene::Paint::Color(color(red, green, blue, 1.0))
}

fn square(x: f32, y: f32, size: f32) -> Vec<PathSegment> {
    vec![
        PathSegment::MoveTo((x, y)),
        PathSegment::LineTo((x + size, y)),
        PathSegment::LineTo((x + size, y + size)),
        PathSegment::LineTo((x, y + size)),
        PathSegment::Close,
    ]
}

fn filled(segments: Vec<PathSegment>, paint: scene::Paint) -> ScenePath {
    let mut path = ScenePath::new(segments);
    path.fill = Some(scene::Fill::new(paint));
    path
}

fn scene_of(children: Vec<Node>) -> VectorImage {
    let mut scene = VectorImage::new((200.0, 200.0));
    scene.root.children = children;
    scene
}

/// The scene drawn alone on a page, as PDF.
fn pdf_of(scene: &VectorImage, assets: &mut SceneAssets) -> lopdf::Document {
    let mut pdf = krilla::Document::new();
    let mut page = pdf.start_page_with(PageSettings::from_wh(200.0, 200.0).unwrap());
    let mut surface = page.surface();
    paint(&mut surface, scene, assets).unwrap();
    surface.finish();
    page.finish();
    lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap()
}

fn drawn(scene: &VectorImage) -> (lopdf::Document, Vec<Operation>) {
    let pdf = pdf_of(scene, &mut SceneAssets::default());
    let operations = operations_of(&pdf);
    (pdf, operations)
}

fn operators(operations: &[Operation]) -> Vec<&str> {
    operations
        .iter()
        .map(|operation| operation.operator.as_str())
        .collect()
}

fn position(operations: &[Operation], operator: &str) -> usize {
    operations
        .iter()
        .position(|operation| operation.operator == operator)
        .unwrap_or_else(|| panic!("no `{operator}` in {:?}", operators(operations)))
}

fn numbers(operation: &Operation) -> Vec<f32> {
    operation
        .operands
        .iter()
        .filter_map(|value| value.as_float().ok())
        .collect()
}

/// Whether some graphics state sets the fill (`ca`) or stroke (`CA`) alpha.
fn has_alpha(pdf: &lopdf::Document, key: &[u8], alpha: f32) -> bool {
    pdf.objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .any(|dict| {
            dict.get(key)
                .ok()
                .and_then(|value| value.as_float().ok())
                .is_some_and(|value| (value - alpha).abs() < 1e-3)
        })
}

fn has_soft_mask(pdf: &lopdf::Document) -> bool {
    pdf.objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .any(|dict| {
            dict.get(b"SMask")
                .is_ok_and(|mask| !matches!(mask, lopdf::Object::Name(_)))
        })
}

// Paths

#[test]
fn fills_and_strokes_keep_their_paints_and_stroke_styles() {
    let mut shape = filled(square(10.0, 10.0, 50.0), solid(1.0, 0.0, 0.0));
    let mut stroke = SceneStroke::new(solid(0.0, 0.0, 1.0), 3.0);
    stroke.cap = SceneCap::Round;
    stroke.join = SceneJoin::Bevel;
    stroke.miter_limit = 7.0;
    stroke.dash = Some(Dash::new(vec![4.0, 2.0], 1.0));
    shape.stroke = Some(stroke);
    let (_, operations) = drawn(&scene_of(vec![Node::Path(shape)]));
    let operators = operators(&operations);
    for operator in ["rg", "RG", "w", "J", "j", "M", "d", "B"] {
        assert!(operators.contains(&operator), "{operator} in {operators:?}");
    }
    let number = |operator: &str| numbers(&operations[position(&operations, operator)]);
    assert_eq!(number("rg"), [1.0, 0.0, 0.0]);
    assert_eq!(number("RG"), [0.0, 0.0, 1.0]);
    // The page is in CSS pixels, so widths are what the scene says.
    assert_eq!(number("w"), [3.0]);
    assert_eq!(number("J"), [1.0]);
    assert_eq!(number("j"), [2.0]);
    assert_eq!(number("M"), [7.0]);
    let dash = &operations[position(&operations, "d")];
    assert_eq!(
        dash.operands[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_float().unwrap())
            .collect::<Vec<_>>(),
        [4.0, 2.0]
    );
    assert_eq!(dash.operands[1].as_float().unwrap(), 1.0);
}

#[test]
fn square_caps_and_round_joins_reach_the_stroke() {
    let mut shape = ScenePath::new(square(10.0, 10.0, 50.0));
    let mut stroke = SceneStroke::new(solid(0.0, 0.0, 1.0), 3.0);
    stroke.cap = SceneCap::Square;
    stroke.join = SceneJoin::Round;
    shape.stroke = Some(stroke);
    let (_, operations) = drawn(&scene_of(vec![Node::Path(shape)]));
    let number = |operator: &str| numbers(&operations[position(&operations, operator)]);
    assert_eq!(number("J"), [2.0]);
    assert_eq!(number("j"), [1.0]);
}

#[test]
fn paint_order_decides_which_is_drawn_first() {
    let draw = |order| {
        let mut shape = filled(square(10.0, 10.0, 50.0), solid(1.0, 0.0, 0.0));
        shape.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 1.0), 3.0));
        shape.paint_order = order;
        drawn(&scene_of(vec![Node::Path(shape)])).1
    };
    let fill_first = draw(PaintOrder::FillThenStroke);
    assert_eq!(count(&fill_first, "B"), 1);
    let stroke_first = draw(PaintOrder::StrokeThenFill);
    assert!(position(&stroke_first, "S") < position(&stroke_first, "f"));
}

/// The scene drawn on a page whose surface still has the fill that the
/// painter of some earlier content left set, as text and backgrounds do.
fn drawn_after_a_fill_was_left_set(scene: &VectorImage) -> Vec<Operation> {
    let mut pdf = krilla::Document::new();
    let mut page = pdf.start_page_with(PageSettings::from_wh(200.0, 200.0).unwrap());
    let mut surface = page.surface();
    surface.set_fill(Some(Fill {
        paint: rgb::Color::new(255, 0, 0).into(),
        opacity: NormalizedF32::ONE,
        rule: FillRule::NonZero,
    }));
    paint(&mut surface, scene, &mut SceneAssets::default()).unwrap();
    surface.finish();
    page.finish();
    operations_of(&lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap())
}

#[test]
fn a_stroke_drawn_first_does_not_paint_the_fill_left_by_earlier_content() {
    let mut shape = filled(square(10.0, 10.0, 50.0), solid(0.0, 1.0, 0.0));
    shape.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 1.0), 3.0));
    shape.paint_order = PaintOrder::StrokeThenFill;
    let fixture = Fixture::new();
    let mut run = fixture.run("AB", 20.0, 10.0, 90.0);
    run.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 1.0), 2.0));
    run.paint_order = PaintOrder::StrokeThenFill;
    let operations =
        drawn_after_a_fill_was_left_set(&scene_of(vec![Node::Path(shape), Node::Text(run)]));
    // The strokes are alone, and the left fill never reaches the page.
    assert_eq!(count(&operations, "B"), 0);
    assert!(
        operations
            .iter()
            .all(|operation| !(operation.operator == "rg" && numbers(operation) == [1.0, 0.0, 0.0]))
    );
}

#[test]
fn curves_closes_and_the_even_odd_rule_reach_the_path() {
    let mut shape = filled(
        vec![
            PathSegment::MoveTo((0.0, 0.0)),
            PathSegment::QuadTo {
                control: (20.0, 0.0),
                to: (20.0, 20.0),
            },
            PathSegment::CubicTo {
                control1: (20.0, 40.0),
                control2: (0.0, 40.0),
                to: (0.0, 20.0),
            },
            PathSegment::LineTo((0.0, 0.0)),
            PathSegment::Close,
        ],
        solid(0.0, 0.0, 0.0),
    );
    shape.fill.as_mut().unwrap().rule = scene::FillRule::EvenOdd;
    let (_, operations) = drawn(&scene_of(vec![Node::Path(shape)]));
    assert!(count(&operations, "c") >= 2);
    assert_eq!(count(&operations, "h"), 1);
    assert_eq!(count(&operations, "f*"), 1);
}

#[test]
fn paint_alpha_and_opacity_multiply() {
    let mut shape = ScenePath::new(square(0.0, 0.0, 40.0));
    let mut fill = scene::Fill::new(scene::Paint::Color(color(0.0, 1.0, 0.0, 0.5)));
    fill.opacity = 0.5;
    shape.fill = Some(fill);
    let mut stroke = SceneStroke::new(scene::Paint::Color(color(0.0, 0.0, 0.0, 0.5)), 2.0);
    stroke.opacity = 0.4;
    shape.stroke = Some(stroke);
    let (pdf, _) = drawn(&scene_of(vec![Node::Path(shape)]));
    assert!(has_alpha(&pdf, b"ca", 0.25));
    assert!(has_alpha(&pdf, b"CA", 0.2));
}

#[test]
fn shapes_with_nothing_to_draw_leave_no_operations() {
    let mut unpainted = ScenePath::new(square(0.0, 0.0, 10.0));
    unpainted.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 0.0), 0.0));
    let only_a_point = filled(vec![PathSegment::MoveTo((1.0, 1.0))], solid(0.0, 0.0, 0.0));
    let nothing = ScenePath::new(square(0.0, 0.0, 10.0));
    let (_, operations) = drawn(&scene_of(vec![
        Node::Path(unpainted),
        Node::Path(only_a_point),
        Node::Path(nothing),
    ]));
    for operator in ["S", "f", "B", "f*"] {
        assert_eq!(count(&operations, operator), 0, "{operator}");
    }
}

// Gradients

fn stops(first: PaintColor, second: PaintColor) -> Vec<GradientStop> {
    vec![
        GradientStop::new(0.0, first),
        GradientStop::new(1.0, second),
    ]
}

#[test]
fn gradients_become_shadings_with_their_spread_and_transform() {
    let mut linear = scene::LinearGradient::new(
        (0.0, 0.0),
        (1.0, 0.0),
        stops(color(1.0, 0.0, 0.0, 1.0), color(0.0, 0.0, 1.0, 0.5)),
    );
    linear.transform = PaintTransform::new(40.0, 0.0, 0.0, 40.0, 10.0, 10.0);
    let mut radial = scene::RadialGradient::new(
        (0.5, 0.5),
        0.5,
        stops(color(1.0, 1.0, 0.0, 1.0), color(0.0, 0.5, 0.0, 1.0)),
    );
    radial.focal = (0.4, 0.4);
    radial.focal_radius = 0.1;
    radial.spread = SceneSpread::Repeat;
    radial.transform = PaintTransform::new(40.0, 0.0, 0.0, 40.0, 60.0, 10.0);
    let (pdf, operations) = drawn(&scene_of(vec![
        Node::Path(filled(
            square(10.0, 10.0, 40.0),
            scene::Paint::LinearGradient(Box::new(linear)),
        )),
        Node::Path(filled(
            square(60.0, 10.0, 40.0),
            scene::Paint::RadialGradient(Box::new(radial)),
        )),
    ]));
    assert!(has_shading(&pdf, 2), "axial shading");
    assert!(has_shading(&pdf, 3), "radial shading");
    assert_eq!(count(&operations, "scn"), 2);
    // The translucent stop is a soft mask of the shading.
    assert!(has_soft_mask(&pdf));
}

#[test]
fn a_stroke_can_be_a_gradient_that_reflects() {
    let mut linear = scene::LinearGradient::new(
        (0.0, 0.0),
        (40.0, 0.0),
        stops(color(0.0, 0.0, 0.0, 1.0), color(1.0, 1.0, 1.0, 1.0)),
    );
    linear.spread = SceneSpread::Reflect;
    let mut shape = ScenePath::new(square(0.0, 0.0, 40.0));
    shape.stroke = Some(SceneStroke::new(
        scene::Paint::LinearGradient(Box::new(linear)),
        4.0,
    ));
    let (_, operations) = drawn(&scene_of(vec![Node::Path(shape)]));
    assert_eq!(count(&operations, "S"), 1);
    assert_eq!(count(&operations, "SCN"), 1);
}

// Groups and clips

#[test]
fn group_transforms_nest_and_zero_opacity_hides_a_group() {
    let mut inner = Group::new(PaintTransform::translate(5.0, 7.0));
    inner.children.push(Node::Path(filled(
        square(0.0, 0.0, 10.0),
        solid(0.0, 0.0, 0.0),
    )));
    let mut hidden = Group::new(PaintTransform::IDENTITY);
    hidden.opacity = 0.0;
    hidden.children.push(Node::Path(filled(
        square(0.0, 0.0, 10.0),
        solid(1.0, 0.0, 0.0),
    )));
    let mut outer = Group::new(PaintTransform::new(2.0, 0.0, 0.0, 2.0, 0.0, 0.0));
    outer.children = vec![Node::Group(inner), Node::Group(hidden)];
    let (_, operations) = drawn(&scene_of(vec![Node::Group(outer)]));
    assert_eq!(count(&operations, "f"), 1);
    // The hidden group left no fill color, and the shape that was drawn
    // sits in the space of both groups: scaled by two, then moved by (5, 7)
    // of that space, in a page that starts at its bottom left.
    assert_eq!(count(&operations, "rg"), 1);
    let drawn_at = operations
        .iter()
        .filter(|operation| operation.operator == "cm")
        .map(numbers)
        .next_back()
        .unwrap();
    assert_eq!(drawn_at, [2.0, 0.0, 0.0, -2.0, 10.0, 200.0 - 14.0]);
}

#[test]
fn group_opacity_is_one_transparency_group() {
    let mut group = Group::new(PaintTransform::IDENTITY);
    group.opacity = 0.5;
    group.children = vec![
        Node::Path(filled(square(0.0, 0.0, 40.0), solid(1.0, 0.0, 0.0))),
        Node::Path(filled(square(20.0, 20.0, 40.0), solid(0.0, 0.0, 1.0))),
    ];
    let (pdf, operations) = drawn(&scene_of(vec![Node::Group(group)]));
    assert!(has_alpha(&pdf, b"ca", 0.5));
    // Both squares are inside the form that the opacity is applied to.
    let form = pdf
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .find(|stream| stream.dict.has(b"Group"))
        .expect("a transparency group");
    let content = String::from_utf8_lossy(&form.get_plain_content().unwrap()).into_owned();
    assert!(
        content.contains("1 0 0 rg") && content.contains("0 0 1 rg"),
        "{content}"
    );
    assert_eq!(count(&operations, "Do"), 1);
}

fn clip_of(shapes: Vec<Node>) -> scene::ClipPath {
    let mut clip = scene::ClipPath::new(PaintTransform::IDENTITY);
    clip.root.children = shapes;
    clip
}

fn clipped(clip: scene::ClipPath) -> VectorImage {
    let mut group = Group::new(PaintTransform::IDENTITY);
    group.clip = Some(Box::new(clip));
    group.children.push(Node::Path(filled(
        square(0.0, 0.0, 100.0),
        solid(1.0, 0.0, 0.0),
    )));
    scene_of(vec![Node::Group(group)])
}

/// The bounding box of the first clip path in PDF space, where the page is
/// 200 units high and `y` is measured from the bottom.
fn clip_box(operations: &[Operation]) -> [f32; 4] {
    let clip = position(operations, "W");
    let start = operations[..clip]
        .iter()
        .rposition(|operation| operation.operator == "m")
        .unwrap();
    let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for operation in &operations[start..clip] {
        for point in numbers(operation).as_chunks::<2>().0 {
            bounds = [
                bounds[0].min(point[0]),
                bounds[1].min(point[1]),
                bounds[2].max(point[0]),
                bounds[3].max(point[1]),
            ];
        }
    }
    bounds
}

#[test]
fn a_clip_of_one_shape_is_a_clip_path_in_the_space_of_the_children() {
    let mut clip = clip_of(vec![Node::Path(filled(
        square(0.0, 0.0, 10.0),
        solid(0.0, 0.0, 0.0),
    ))]);
    clip.transform = PaintTransform::new(2.0, 0.0, 0.0, 2.0, 20.0, 20.0);
    let (pdf, operations) = drawn(&clipped(clip));
    assert!(!has_soft_mask(&pdf));
    // A square of 10 scaled by 2 and moved to (20, 20) covers 20..40.
    let [x0, y0, x1, y1] = clip_box(&operations);
    assert_eq!([x0, x1], [20.0, 40.0]);
    assert_eq!([y0, y1], [200.0 - 40.0, 200.0 - 20.0]);
}

#[test]
fn a_clip_follows_the_fill_rule_of_its_shape() {
    let mut shape = filled(square(0.0, 0.0, 10.0), solid(0.0, 0.0, 0.0));
    shape.fill.as_mut().unwrap().rule = scene::FillRule::EvenOdd;
    let (_, operations) = drawn(&clipped(clip_of(vec![Node::Path(shape)])));
    assert_eq!(count(&operations, "W*"), 1);
    // A shape with no fill clips by the non-zero rule.
    let (_, operations) = drawn(&clipped(clip_of(vec![Node::Path(ScenePath::new(square(
        0.0, 0.0, 10.0,
    )))])));
    assert_eq!(count(&operations, "W"), 1);
}

#[test]
fn a_clip_of_a_wrapped_shape_is_still_one_clip_path() {
    let mut wrapper = Group::new(PaintTransform::translate(10.0, 0.0));
    wrapper.children.push(Node::Path(filled(
        square(0.0, 0.0, 10.0),
        solid(0.0, 0.0, 0.0),
    )));
    let (pdf, operations) = drawn(&clipped(clip_of(vec![Node::Group(wrapper)])));
    assert!(!has_soft_mask(&pdf));
    let [x0, _, x1, _] = clip_box(&operations);
    assert_eq!([x0, x1], [10.0, 20.0]);
}

#[test]
fn a_clip_of_several_shapes_is_their_union_as_a_mask() {
    let (pdf, operations) = drawn(&clipped(clip_of(vec![
        Node::Path(filled(square(0.0, 0.0, 10.0), solid(0.0, 0.0, 0.0))),
        Node::Path(filled(square(50.0, 50.0, 10.0), solid(0.0, 0.0, 0.0))),
    ])));
    assert!(has_soft_mask(&pdf));
    assert_eq!(count(&operations, "W"), 0);
    // The shapes are painted opaque black into the mask, whatever their paint.
    assert!(
        operations
            .iter()
            .any(|operation| operation.operator == "g" || operation.operator == "rg")
    );
}

#[test]
fn clips_inside_a_mask_keep_their_own_limits_and_transforms() {
    let mut nested = Group::new(PaintTransform::translate(30.0, 0.0));
    let mut inner = clip_of(vec![Node::Path(filled(
        square(0.0, 0.0, 5.0),
        solid(0.0, 0.0, 0.0),
    ))]);
    inner.transform = PaintTransform::IDENTITY;
    nested.clip = Some(Box::new(inner));
    nested.opacity = 0.2;
    nested.children.push(Node::Path(filled(
        square(0.0, 0.0, 10.0),
        scene::Paint::Color(color(0.0, 0.0, 0.0, 0.1)),
    )));
    let mut clip = clip_of(vec![
        Node::Group(nested),
        Node::Path(filled(square(50.0, 50.0, 10.0), solid(0.0, 0.0, 0.0))),
    ]);
    clip.transform = PaintTransform::translate(1.0, 1.0);
    let (pdf, operations) = drawn(&clipped(clip));
    assert!(has_soft_mask(&pdf));
    // The clip inside the mask limits its shape; the opacity of a group
    // inside the area does not make the area translucent.
    assert!(count(&operations, "W") >= 1);
    assert!(!has_alpha(&pdf, b"ca", 0.2));
}

#[test]
fn chained_clips_intersect() {
    let mut first = clip_of(vec![Node::Path(filled(
        square(0.0, 0.0, 80.0),
        solid(0.0, 0.0, 0.0),
    ))]);
    first.clip = Some(Box::new(clip_of(vec![Node::Path(filled(
        square(40.0, 40.0, 80.0),
        solid(0.0, 0.0, 0.0),
    ))])));
    let (_, operations) = drawn(&clipped(first));
    assert_eq!(count(&operations, "W"), 2);
}

#[test]
fn a_clip_that_covers_nothing_hides_its_group() {
    for clip in [
        clip_of(Vec::new()),
        clip_of(vec![Node::Path(filled(
            vec![PathSegment::MoveTo((1.0, 1.0))],
            solid(0.0, 0.0, 0.0),
        ))]),
        {
            let mut chain = clip_of(vec![Node::Path(filled(
                square(0.0, 0.0, 10.0),
                solid(0.0, 0.0, 0.0),
            ))]);
            chain.clip = Some(Box::new(clip_of(Vec::new())));
            chain
        },
    ] {
        let (_, operations) = drawn(&clipped(clip));
        assert_eq!(count(&operations, "f"), 0);
        assert_eq!(count(&operations, "rg"), 0);
    }
}

#[test]
fn text_and_pictures_leave_no_area_in_a_clip() {
    let fixture = Fixture::new();
    let (pdf, operations) = drawn(&clipped(clip_of(vec![
        Node::Text(fixture.run("A", 20.0, 0.0, 20.0)),
        Node::Image(RasterImage::new(picture(2, 2, 255))),
        Node::Path(filled(square(0.0, 0.0, 10.0), solid(0.0, 0.0, 0.0))),
    ])));
    // The path is the only area, but beside other nodes it is a mask.
    assert!(has_soft_mask(&pdf));
    assert_eq!(image_objects(&pdf), 0);
    assert_eq!(count(&operations, "Tj") + count(&operations, "TJ"), 0);
    // A clip with nothing but text and pictures hides its group.
    let (_, operations) = drawn(&clipped(clip_of(vec![
        Node::Text(fixture.run("A", 20.0, 0.0, 20.0)),
        Node::Image(RasterImage::new(picture(2, 2, 255))),
    ])));
    assert_eq!(count(&operations, "rg"), 0);
}

#[test]
fn shapes_that_make_no_path_leave_no_area_in_a_mask() {
    let black = solid(0.0, 0.0, 0.0);
    // A group limited by a clip that covers nothing is hidden, in the mask
    // as it is on the page.
    let mut hidden = Group::new(PaintTransform::IDENTITY);
    hidden.clip = Some(Box::new(clip_of(Vec::new())));
    hidden
        .children
        .push(Node::Path(filled(square(30.0, 30.0, 10.0), black.clone())));
    let (pdf, operations) = drawn(&clipped(clip_of(vec![
        Node::Path(filled(square(0.0, 0.0, 10.0), black.clone())),
        Node::Path(filled(vec![PathSegment::MoveTo((1.0, 1.0))], black)),
        Node::Group(hidden),
    ])));
    assert!(has_soft_mask(&pdf));
    // One fill for the square of the mask and one for the clipped group: the
    // shape without a path and the hidden group add none.
    assert_eq!(count(&operations, "f"), 2);
}

#[test]
fn a_shape_with_a_clip_of_its_own_in_a_clip_is_a_mask() {
    let mut clip = clip_of(vec![Node::Path(filled(
        square(0.0, 0.0, 80.0),
        solid(0.0, 0.0, 0.0),
    ))]);
    clip.root.clip = Some(Box::new(clip_of(vec![Node::Path(filled(
        square(40.0, 40.0, 80.0),
        solid(0.0, 0.0, 0.0),
    ))])));
    let (pdf, operations) = drawn(&clipped(clip));
    assert!(has_soft_mask(&pdf));
    // The inner limit is a clip path inside the mask.
    assert!(count(&operations, "W") >= 1);
}

// Pictures

fn picture(width: u32, height: u32, alpha: u8) -> Arc<DecodedImage> {
    let pixel = [10, 20, 30, alpha];
    Arc::new(DecodedImage {
        width,
        height,
        rgba: pixel.repeat((width * height) as usize),
    })
}

fn image_objects(pdf: &lopdf::Document) -> usize {
    pdf.objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter(|stream| {
            stream
                .dict
                .get(b"Subtype")
                .is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Image"))
        })
        .count()
}

#[test]
fn pictures_are_drawn_sized_by_their_group_and_converted_once() {
    let pixels = picture(4, 2, 255);
    let mut left = Group::new(PaintTransform::new(10.0, 0.0, 0.0, 10.0, 0.0, 0.0));
    left.children
        .push(Node::Image(RasterImage::new(pixels.clone())));
    let mut right = Group::new(PaintTransform::new(5.0, 0.0, 0.0, 5.0, 100.0, 0.0));
    right.children.push(Node::Image(RasterImage::new(pixels)));
    let mut assets = SceneAssets::default();
    let pdf = pdf_of(
        &scene_of(vec![Node::Group(left), Node::Group(right)]),
        &mut assets,
    );
    assert_eq!(count(&operations_of(&pdf), "Do"), 2);
    assert_eq!(image_objects(&pdf), 1);
}

#[test]
fn smoothing_and_transparency_make_different_images() {
    let opaque = picture(2, 2, 255);
    let see_through = picture(2, 2, 100);
    let mut sharp = RasterImage::new(opaque.clone());
    sharp.smooth = false;
    let (pdf, operations) = drawn(&scene_of(vec![
        Node::Image(RasterImage::new(opaque)),
        Node::Image(sharp),
        Node::Image(RasterImage::new(see_through)),
    ]));
    assert_eq!(count(&operations, "Do"), 3);
    assert_eq!(image_objects(&pdf), 4, "three images and one soft mask");
}

// Text

struct Fixture {
    face: FontFace,
    units_per_em: f32,
}

impl Fixture {
    fn new() -> Self {
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(FONT);
        let units_per_em = Font::new(data.clone().into(), 0).unwrap().units_per_em();
        Self {
            face: FontFace::new(data, 0),
            units_per_em,
        }
    }

    fn id(&self, character: char) -> u32 {
        skrifa::FontRef::new(FONT)
            .unwrap()
            .charmap()
            .map(character)
            .unwrap()
            .to_u32()
    }

    /// A glyph at `(x, y)` of a run of `size`, as the scene builder places it.
    fn glyph(&self, character: char, size: f32, x: f32, y: f32) -> Glyph {
        let scale = size / self.units_per_em;
        Glyph::new(
            self.face.clone(),
            self.id(character),
            character.to_string(),
            PaintTransform::new(scale, 0.0, 0.0, scale, x, y),
        )
    }

    fn run(&self, text: &str, size: f32, x: f32, y: f32) -> TextRun {
        let mut pen = x;
        let glyphs = text
            .chars()
            .map(|character| {
                let glyph = self.glyph(character, size, pen, y);
                pen += size * 0.6;
                glyph
            })
            .collect();
        let mut run = TextRun::new(size, glyphs);
        run.fill = Some(scene::Fill::new(solid(0.0, 0.0, 0.0)));
        run
    }
}

fn text_of(pdf: &lopdf::Document) -> String {
    pdf.extract_text(&[1]).unwrap().split_whitespace().collect()
}

fn font_objects(pdf: &lopdf::Document) -> usize {
    pdf.objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter(|dict| {
            dict.get(b"Type")
                .is_ok_and(|kind| kind.as_name().is_ok_and(|name| name == b"Font"))
                && dict.has(b"BaseFont")
        })
        .count()
}

#[test]
fn text_is_selectable_in_one_embedded_font() {
    let fixture = Fixture::new();
    let (pdf, operations) = drawn(&scene_of(vec![
        Node::Text(fixture.run("Hello", 20.0, 10.0, 50.0)),
        Node::Text(fixture.run("World", 20.0, 10.0, 90.0)),
    ]));
    assert_eq!(text_of(&pdf), "HelloWorld");
    assert_eq!(count(&operations, "BT"), 2);
    // Both runs use the same face, which is embedded once.
    assert_eq!(font_objects(&pdf), 2, "a Type0 font and its descendant");
}

#[test]
fn glyphs_sit_at_their_own_positions_and_baselines() {
    let fixture = Fixture::new();
    let mut run = fixture.run("AB", 20.0, 10.0, 50.0);
    // The second glyph moves to the right and sits 5 px lower.
    run.glyphs[1].transform.e = 60.0;
    run.glyphs[1].transform.f = 55.0;
    let (_, operations) = drawn(&scene_of(vec![Node::Text(run)]));
    // Each glyph starts a text matrix at its own origin: a different
    // baseline cannot be told with advances alone.
    let origins: Vec<[f32; 2]> = operations
        .iter()
        .filter(|operation| operation.operator == "Tm")
        .map(|operation| {
            let matrix = numbers(operation);
            [matrix[4], matrix[5]]
        })
        .collect();
    assert_eq!(origins, [[10.0, 50.0], [60.0, 55.0]]);
}

#[test]
fn glyphs_that_spell_nothing_continue_the_cluster_before_them() {
    let fixture = Fixture::new();
    let mut run = fixture.run("fi", 20.0, 10.0, 50.0);
    run.glyphs[1].text = String::new();
    run.glyphs[0].text = "fi".into();
    let (pdf, _) = drawn(&scene_of(vec![Node::Text(run)]));
    assert!(text_of(&pdf).starts_with("fi"), "{}", text_of(&pdf));
    // With nothing before it, the first glyph spells nothing either.
    let mut run = fixture.run("f", 20.0, 10.0, 50.0);
    run.glyphs[0].text = String::new();
    let (_, operations) = drawn(&scene_of(vec![Node::Text(run)]));
    assert_eq!(count(&operations, "BT"), 1);
}

#[test]
fn a_rotated_run_is_drawn_in_its_own_rotated_space() {
    let fixture = Fixture::new();
    let (sin, cos) = 30.0f32.to_radians().sin_cos();
    let scale = 20.0 / fixture.units_per_em;
    let mut run = TextRun::new(20.0, Vec::new());
    run.fill = Some(scene::Fill::new(solid(0.0, 0.0, 0.0)));
    for (index, character) in "AB".chars().enumerate() {
        let (x, y) = (
            50.0 + index as f32 * 12.0 * cos,
            80.0 + index as f32 * 12.0 * sin,
        );
        run.glyphs.push(Glyph::new(
            fixture.face.clone(),
            fixture.id(character),
            character.to_string(),
            PaintTransform::new(scale * cos, scale * sin, -scale * sin, scale * cos, x, y),
        ));
    }
    let (pdf, operations) = drawn(&scene_of(vec![Node::Text(run)]));
    assert_eq!(text_of(&pdf), "AB");
    // One rotation for the two glyphs, besides the page's own transforms.
    let rotations = operations
        .iter()
        .filter(|operation| operation.operator == "cm")
        .map(numbers)
        .filter(|matrix| matrix[1].abs() > 0.1)
        .count();
    assert_eq!(rotations, 1);
    // Glyphs with different rotations are separate runs.
    let mut turned = fixture.run("AB", 20.0, 10.0, 50.0);
    turned.glyphs[1].transform = PaintTransform::new(0.0, scale, -scale, 0.0, 30.0, 50.0);
    let (_, operations) = drawn(&scene_of(vec![Node::Text(turned)]));
    assert_eq!(count(&operations, "BT"), 2);
}

#[test]
fn glyphs_of_another_face_are_drawn_apart_and_failing_faces_are_skipped() {
    let fixture = Fixture::new();
    let mut run = fixture.run("AB", 20.0, 10.0, 50.0);
    // The same face again, as another copy of its bytes.
    let copy: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(FONT.to_vec());
    run.glyphs[1].font = FontFace::new(copy, 0);
    let broken: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(vec![0u8; 16]);
    let mut lost = fixture.glyph('C', 20.0, 70.0, 50.0);
    lost.font = FontFace::new(broken, 0);
    run.glyphs.push(lost);
    let (pdf, operations) = drawn(&scene_of(vec![Node::Text(run)]));
    assert_eq!(count(&operations, "BT"), 2);
    assert_eq!(text_of(&pdf), "AB");
}

#[test]
fn degenerate_runs_draw_nothing() {
    let fixture = Fixture::new();
    let mut unpainted = fixture.run("A", 20.0, 10.0, 50.0);
    unpainted.fill = None;
    let mut empty = fixture.run("A", 20.0, 10.0, 50.0);
    empty.glyphs.clear();
    let no_size = fixture.run("A", 0.0, 10.0, 50.0);
    let mut flat = fixture.run("A", 20.0, 10.0, 50.0);
    flat.glyphs[0].transform = PaintTransform::new(0.0, 0.0, 0.0, 0.0, 10.0, 50.0);
    let (_, operations) = drawn(&scene_of(vec![
        Node::Text(unpainted),
        Node::Text(empty),
        Node::Text(no_size),
        Node::Text(flat),
    ]));
    assert_eq!(count(&operations, "BT"), 0);
}

/// The fixture font with an `fvar` table that declares a weight axis from
/// 100 to 900 whose default is 400.
fn with_weight_axis(font: &[u8]) -> Vec<u8> {
    let be16 = |at: usize| usize::from(u16::from_be_bytes([font[at], font[at + 1]]));
    let be32 = |at: usize| u32::from_be_bytes(font[at..at + 4].try_into().unwrap()) as usize;
    let mut tables: Vec<([u8; 4], Vec<u8>)> = (0..be16(4))
        .map(|index| {
            let record = 12 + index * 16;
            let (offset, length) = (be32(record + 8), be32(record + 12));
            (
                font[record..record + 4].try_into().unwrap(),
                font[offset..offset + length].to_vec(),
            )
        })
        .collect();
    let mut fvar = Vec::new();
    // Version 1.0, axes at 16, two reserved, one axis of 20 bytes, no instances.
    for value in [1u16, 0, 16, 2, 1, 20, 0, 4] {
        fvar.extend(value.to_be_bytes());
    }
    fvar.extend(b"wght");
    for fixed in [100u32, 400, 900] {
        fvar.extend((fixed << 16).to_be_bytes());
    }
    fvar.extend([0u8, 0, 1, 0]); // flags, name id 256
    tables.push((*b"fvar", fvar));
    tables.sort_by_key(|(tag, _)| *tag);
    let count = tables.len();
    let search_range = (1usize << count.ilog2()) * 16;
    let mut out = font[..4].to_vec();
    out.extend((count as u16).to_be_bytes());
    out.extend((search_range as u16).to_be_bytes());
    out.extend((count.ilog2() as u16).to_be_bytes());
    out.extend(((count * 16 - search_range) as u16).to_be_bytes());
    let mut offset = 12 + count * 16;
    for (tag, data) in &tables {
        out.extend(tag);
        out.extend(0u32.to_be_bytes());
        out.extend((offset as u32).to_be_bytes());
        out.extend((data.len() as u32).to_be_bytes());
        offset += data.len().next_multiple_of(4);
    }
    for (_, data) in &tables {
        out.extend(data);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    out
}

#[test]
fn only_variations_that_move_an_axis_make_their_own_instance_of_the_font() {
    let fixture = Fixture::new();
    let varying: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(with_weight_axis(FONT));
    let mut runs = Vec::new();
    // Settings of the style that the font has no axis for, or that the
    // default of the axis already gives, are the default instance.
    for variations in [
        vec![],
        vec![FontVariation::new(*b"wght", 400.0)],
        vec![FontVariation::new(*b"wdth", 90.0)],
        vec![FontVariation::new(*b"wght", 700.0)],
        vec![FontVariation::new(*b"wght", 700.0)],
        vec![
            FontVariation::new(*b"ital", 0.0),
            FontVariation::new(*b"wght", 300.0),
        ],
    ] {
        let mut run = fixture.run("A", 20.0, 10.0, 50.0);
        for glyph in &mut run.glyphs {
            glyph.font = FontFace::new(varying.clone(), 0);
        }
        run.variations = variations;
        runs.push(Node::Text(run));
    }
    // A font without any axis never varies.
    let mut fixed = fixture.run("A", 20.0, 10.0, 90.0);
    fixed.variations = vec![FontVariation::new(*b"wght", 700.0)];
    runs.push(Node::Text(fixed));
    let mut assets = SceneAssets::default();
    let pdf = pdf_of(&scene_of(runs), &mut assets);
    assert_eq!(text_of(&pdf), "AAAAAAA");
    // The default, weight 700 and weight 300 of the font with the axis, and
    // the default of the font without it.
    assert_eq!(assets.fonts.len(), 4);
    // A face that is not a font has no axes to vary.
    let broken: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(vec![0u8; 16]);
    assert!(
        instance_coords(
            &FontFace::new(broken, 0),
            &[FontVariation::new(*b"wght", 700.0)]
        )
        .is_empty()
    );
}

#[test]
fn text_strokes_are_text_in_the_stroke_mode_and_scale_with_the_run() {
    let fixture = Fixture::new();
    let mut run = fixture.run("AB", 20.0, 10.0, 50.0);
    run.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 1.0), 2.0));
    run.fill = Some(scene::Fill::new(solid(1.0, 0.0, 0.0)));
    let (pdf, operations) = drawn(&scene_of(vec![Node::Text(run)]));
    assert_eq!(text_of(&pdf), "AB");
    let mode = numbers(&operations[position(&operations, "Tr")]);
    assert_eq!(mode, [2.0], "fill and stroke in one text object");
    assert_eq!(numbers(&operations[position(&operations, "w")]), [2.0]);

    // Stroke only is the stroke mode.
    let mut stroked = fixture.run("AB", 20.0, 10.0, 50.0);
    stroked.fill = None;
    stroked.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 1.0), 1.0));
    let (_, operations) = drawn(&scene_of(vec![Node::Text(stroked)]));
    assert_eq!(numbers(&operations[position(&operations, "Tr")]), [1.0]);

    // A stroke of a scaled run is scaled back, so it keeps its width.
    let scale = 20.0 / fixture.units_per_em;
    let mut big = fixture.run("A", 20.0, 10.0, 50.0);
    big.glyphs[0].transform = PaintTransform::new(scale * 2.0, 0.0, 0.0, scale * 2.0, 10.0, 50.0);
    big.fill = None;
    big.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 0.0), 4.0));
    let mut dashed = big.stroke.clone().unwrap();
    dashed.dash = Some(Dash::new(vec![2.0, 2.0], 1.0));
    big.stroke = Some(dashed);
    let (_, operations) = drawn(&scene_of(vec![Node::Text(big)]));
    // The run is drawn in a space that doubles lengths: 4 px is 2 units.
    assert_eq!(numbers(&operations[position(&operations, "w")]), [2.0]);
    let dash = &operations[position(&operations, "d")];
    assert_eq!(
        dash.operands[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_float().unwrap())
            .collect::<Vec<_>>(),
        [1.0, 1.0]
    );
}

#[test]
fn stroking_first_outlines_the_stroke_and_keeps_the_text_once() {
    let fixture = Fixture::new();
    let mut run = fixture.run("AB", 20.0, 10.0, 50.0);
    run.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 1.0), 2.0));
    run.paint_order = PaintOrder::StrokeThenFill;
    let (pdf, operations) = drawn(&scene_of(vec![Node::Text(run)]));
    assert_eq!(text_of(&pdf), "AB");
    // The stroke is outlines drawn before the fill, which is the text.
    assert!(position(&operations, "S") < position(&operations, "BT"));
    assert_eq!(count(&operations, "BT"), 1);
}

#[test]
fn gradients_on_text_follow_the_run_through_its_space() {
    let fixture = Fixture::new();
    let mut linear = scene::LinearGradient::new(
        (10.0, 0.0),
        (60.0, 0.0),
        stops(color(1.0, 0.0, 0.0, 1.0), color(0.0, 0.0, 1.0, 1.0)),
    );
    linear.transform = PaintTransform::translate(0.0, 1.0);
    let mut run = fixture.run("AB", 20.0, 10.0, 50.0);
    run.fill = Some(scene::Fill::new(scene::Paint::LinearGradient(Box::new(
        linear.clone(),
    ))));
    let mut stroked = fixture.run("AB", 20.0, 10.0, 90.0);
    stroked.fill = None;
    stroked.stroke = Some(SceneStroke::new(
        scene::Paint::LinearGradient(Box::new(linear)),
        1.0,
    ));
    let scale = 20.0 / fixture.units_per_em;
    let mut rotated = fixture.run("A", 20.0, 10.0, 130.0);
    rotated.glyphs[0].transform = PaintTransform::new(0.0, scale, -scale, 0.0, 30.0, 130.0);
    rotated.fill = Some(scene::Fill::new(scene::Paint::RadialGradient(Box::new(
        scene::RadialGradient::new(
            (0.0, 0.0),
            10.0,
            stops(color(0.0, 1.0, 0.0, 1.0), color(0.0, 0.0, 0.0, 0.5)),
        ),
    ))));
    let (pdf, _) = drawn(&scene_of(vec![
        Node::Text(run),
        Node::Text(stroked),
        Node::Text(rotated),
    ]));
    assert!(has_shading(&pdf, 2));
    assert!(has_shading(&pdf, 3));
}

#[test]
fn only_text_strokes_need_glyph_transforms_that_keep_shapes_similar() {
    let fixture = Fixture::new();
    let sheared = |stroked: bool| {
        let mut run = fixture.run("A", 20.0, 10.0, 50.0);
        run.glyphs[0].transform = PaintTransform::new(0.02, 0.0, 0.01, 0.04, 10.0, 50.0);
        if stroked {
            run.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 0.0), 1.0));
        }
        scene_of(vec![Node::Text(run)])
    };
    assert!(paintable(&sheared(false)));
    assert!(!paintable(&sheared(true)));
    // Inside groups too.
    let mut group = Group::new(PaintTransform::IDENTITY);
    group.children = sheared(true).root.children;
    assert!(!paintable(&scene_of(vec![Node::Group(group)])));
    // Rotations, reflections and uniform scales are similarities.
    let mut run = fixture.run("A", 20.0, 10.0, 50.0);
    run.stroke = Some(SceneStroke::new(solid(0.0, 0.0, 0.0), 1.0));
    for transform in [
        PaintTransform::new(0.02, 0.0, 0.0, 0.02, 0.0, 0.0),
        PaintTransform::new(0.0, 0.02, -0.02, 0.0, 0.0, 0.0),
        PaintTransform::new(0.02, 0.0, 0.0, -0.02, 0.0, 0.0),
    ] {
        run.glyphs[0].transform = transform;
        assert!(paintable(&scene_of(vec![Node::Text(run.clone())])));
    }
    // Pictures and paths never matter.
    assert!(paintable(&scene_of(vec![
        Node::Image(RasterImage::new(picture(1, 1, 255))),
        Node::Path(ScenePath::new(square(0.0, 0.0, 1.0))),
    ])));
}

// Matrices and colors

#[test]
fn a_singular_matrix_has_no_inverse() {
    assert!(invert(PaintTransform::new(1.0, 2.0, 2.0, 4.0, 0.0, 0.0)).is_none());
    assert!(invert(PaintTransform::new(f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0)).is_none());
    let moved = PaintTransform::new(2.0, 1.0, -1.0, 3.0, 5.0, 7.0);
    let back = invert(moved).unwrap() * moved;
    for (got, want) in [
        (back.a, 1.0),
        (back.b, 0.0),
        (back.c, 0.0),
        (back.d, 1.0),
        (back.e, 0.0),
        (back.f, 0.0),
    ] {
        assert!((got - want).abs() < 1e-5, "{back:?}");
    }
}

#[test]
fn colors_round_to_bytes_and_clamp() {
    assert_eq!(byte(0.5), 128);
    assert_eq!(byte(-1.0), 0);
    assert_eq!(byte(3.0), 255);
    assert_eq!(unit(2.0).get(), 1.0);
    assert_eq!(unit(-2.0).get(), 0.0);
    assert_eq!(unit(f32::NAN).get(), 1.0);
}

// Caching

fn tiny(width: f32) -> Option<VectorImage> {
    Some(VectorImage::new((width, 1.0)))
}

#[test]
fn a_scene_is_kept_when_it_is_asked_for_again() {
    let mut memo = SceneMemo::default();
    // Asked for once: built and not kept.
    let first = memo.get_or_build("a", None, || tiny(1.0)).unwrap();
    assert_eq!(memo.len(), 0);
    // Asked for twice: kept, then reused without building.
    let second = memo.get_or_build("a", None, || tiny(2.0)).unwrap();
    assert_eq!(memo.len(), 1);
    let third = memo
        .get_or_build("a", None, || panic!("kept scenes are not built again"))
        .unwrap();
    assert!(Arc::ptr_eq(&second, &third));
    assert!(!Arc::ptr_eq(&first, &second));
    // The host opacity is part of the key.
    memo.get_or_build("a", Some(1), || tiny(3.0));
    memo.get_or_build("a", Some(1), || tiny(3.0));
    assert_eq!(memo.len(), 2);
}

#[test]
fn a_source_drawn_another_way_is_kept_when_it_is_asked_for_again() {
    let mut memo = SceneMemo::default();
    assert!(memo.get_or_build("a", None, || None).is_none());
    assert_eq!(memo.len(), 0);
    assert!(memo.get_or_build("a", None, || None).is_none());
    assert_eq!(memo.len(), 1);
    // Kept, so it is not built again to fail again.
    let again = memo.get_or_build("a", None, || {
        panic!("a source drawn another way is not built again")
    });
    assert!(again.is_none());
    // Only the source and the opacity it was asked for with are remembered.
    assert!(memo.get_or_build("a", Some(1), || tiny(1.0)).is_some());
}

#[test]
fn a_full_memo_keeps_nothing_more() {
    let mut memo = SceneMemo {
        kept_bytes: MAX_CACHED_SCENE_BYTES,
        ..SceneMemo::default()
    };
    for _ in 0..3 {
        memo.get_or_build("a", None, || tiny(1.0));
    }
    assert_eq!(memo.len(), 0);
}

#[test]
fn a_face_krilla_cannot_parse_stays_alive_with_its_entry() {
    // The entry is keyed by the address of the face's bytes, so it must keep
    // them alive even when it holds no font; otherwise another face could
    // be given the address and meet the entry.
    let broken: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(vec![0u8; 16]);
    let mut assets = SceneAssets::default();
    let face = FontFace::new(broken.clone(), 0);
    assert!(assets.font(&face, &[]).is_none());
    drop(face);
    assert_eq!(assets.fonts.len(), 1);
    assert_eq!(Arc::strong_count(&broken), 2, "the entry holds the bytes");
    // Asked for again, it is the same entry.
    assert!(assets.font(&FontFace::new(broken, 0), &[]).is_none());
    assert_eq!(assets.fonts.len(), 1);
}
