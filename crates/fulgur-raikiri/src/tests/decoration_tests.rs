use super::*;

const DECORATION_CSS: &str = "<style>@page {size:200px 150px;margin:0} body {margin:0;font-size:20px;color:black} p {margin:0}</style>";

#[test]
fn all_decoration_styles_emit_pdf_paths() {
    for style in ["solid", "double", "dotted", "dashed", "wavy"] {
        let (pdf, ops) = operations(&format!(
            "{DECORATION_CSS}<p style='text-decoration:underline red;text-decoration-style:{style}'>Decorated</p>"
        ));
        assert!(pdf.extract_text(&[1]).unwrap().contains("Decorated"));
        assert!(
            ops.iter()
                .any(|op| matches!(op.operator.as_str(), "rg" | "RG")
                    && op
                        .operands
                        .iter()
                        .map(|v| v.as_float().unwrap())
                        .collect::<Vec<_>>()
                        == [1.0, 0.0, 0.0]),
            "{style}: no red decoration"
        );
        assert!(
            count(&ops, "f") + count(&ops, "S") > 0,
            "{style}: no line geometry"
        );
    }
}

#[test]
fn transparent_glyphs_keep_colored_decorations() {
    let (_, ops) = operations(&format!(
        "{DECORATION_CSS}<p style='color:transparent;text-decoration:underline red'>Transparent</p>"
    ));
    assert!(fill_colors(&ops).contains(&[255, 0, 0]));
    assert!(count(&ops, "f") + count(&ops, "S") > 0);
}

#[test]
fn decoration_phases_surround_the_glyphs() {
    let (_, ops) = operations(&format!(
        "{DECORATION_CSS}<p style='text-decoration:underline overline line-through red'>Phases</p>"
    ));
    let text = ops
        .iter()
        .position(|op| matches!(op.operator.as_str(), "Tj" | "TJ"))
        .unwrap();
    assert!(ops[..text].iter().any(|op| op.operator == "f"));
    assert!(ops[text + 1..].iter().any(|op| op.operator == "f"));
}
