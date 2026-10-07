use super::*;

const ORDER_CSS: &str =
    "<style>@page {size:200px 150px;margin:0} body {margin:0} p {margin:0}</style>";

fn color_index(ops: &[lopdf::content::Operation], color: [f32; 3]) -> usize {
    ops.iter()
        .position(|operation| {
            operation.operator == "rg"
                && operation.operands.len() == 3
                && operation
                    .operands
                    .iter()
                    .zip(color)
                    .all(|(value, expected)| value.as_float().unwrap() == expected)
        })
        .unwrap()
}

#[test]
fn ordered_positioned_background_covers_prior_text() {
    let (_, ops) = operations(&format!(
        "{ORDER_CSS}<p>Hello</p><div style='position:absolute;left:0;top:0;width:50px;height:20px;background:blue'></div>"
    ));
    let text = ops
        .iter()
        .position(|operation| matches!(operation.operator.as_str(), "Tj" | "TJ"))
        .unwrap();
    assert!(text < color_index(&ops, [0.0, 0.0, 1.0]));
}

#[test]
fn ordered_siblings_follow_z_index() {
    let (_, ops) = operations(&format!(
        "{ORDER_CSS}<div style='position:absolute;z-index:2;width:50px;height:50px;background:blue'></div><div style='position:absolute;z-index:1;width:50px;height:50px;background:red'></div>"
    ));
    assert!(color_index(&ops, [1.0, 0.0, 0.0]) < color_index(&ops, [0.0, 0.0, 1.0]));
}

#[test]
fn ordered_group_opacity_composites_children_once() {
    let (pdf, _) = operations(&format!(
        "{ORDER_CSS}<div style='opacity:0.5'><div style='width:40px;height:40px;background:blue'></div><div style='width:40px;height:40px;margin-top:-20px;background:blue'></div></div>"
    ));
    assert!(
        pdf.objects
            .values()
            .filter_map(|object| object.as_dict().ok())
            .any(|dictionary| dictionary
                .get(b"ca")
                .ok()
                .and_then(|value| value.as_float().ok())
                == Some(0.5))
    );
}

#[test]
fn ordered_preserves_rounded_and_axis_clips() {
    let (_, ops) = operations(&format!(
        "{ORDER_CSS}<div style='width:50px;height:50px;overflow:hidden;border-radius:10px'><div style='width:80px;height:80px;background:red'></div></div><div style='width:50px;height:50px;overflow-x:hidden;overflow-y:visible'><div style='width:80px;height:80px;background:blue'></div></div>"
    ));
    assert!(count(&ops, "W") >= 2);
    assert!(count(&ops, "c") > 0);
    assert!(
        clip_bounds(&ops)
            .iter()
            .any(|bounds| (bounds[2] - 50.0).abs() < 0.01 && bounds[3] > 50.0)
    );
}

#[test]
fn ordered_opacity_boundaries_restore_clips() {
    let (pdf, ops) = operations(&format!(
        "{ORDER_CSS}<div style='width:50px;height:50px;overflow:hidden'><div style='opacity:0.5'><div style='width:80px;height:80px;background:blue'></div></div></div><p>After</p>"
    ));
    assert!(count(&ops, "W") >= 1);
    assert_eq!(count(&ops, "q"), count(&ops, "Q"));
    assert_eq!(
        pdf.extract_text(&[1])
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["After"]
    );
}

#[test]
fn generated_and_ellipsis_preserve_visible_text() {
    let (_dir, path) = input("<style>p::before {content:'Before'}</style><p>Hello</p>");
    let document = completed(&path);
    let page = document.page(0).unwrap();
    assert!(
        page.text_runs()
            .iter()
            .any(|run| matches!(run.source, raikiri_html::RunSource::Generated(_, _)))
    );
    for (html, expected) in [
        (
            "<style>p::before {content:'Before'} p::after {content:'After'}</style><p>Hello</p>",
            "Before \nHello \nAfter \n",
        ),
        (
            "<style>p {width:40px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}</style><p>Hello World</p>",
            "Hel \n…\n",
        ),
    ] {
        let (_dir, path) = input(&format!(
            "<style>body {{font:16px 'Noto Sans Mono'}}</style>{html}"
        ));
        let mut bundle = fulgur_core::AssetBundle::new();
        bundle
            .add_font_file(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../fulgur-ruby/spec/fixtures/noto_sans.ttf"),
            )
            .unwrap();
        let bytes = render_with_options(
            &path,
            &Config::default(),
            &RenderOptions {
                assets: Some(&bundle),
                system_fonts: false,
            },
        )
        .unwrap();
        let pdf = lopdf::Document::load_mem(&bytes).unwrap();
        let text = pdf.extract_text(&[1]).unwrap();
        assert_eq!(text, expected);
    }
}
