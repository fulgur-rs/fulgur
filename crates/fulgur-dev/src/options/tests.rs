use super::*;

#[test]
fn page_size_units_and_separators() {
    for (input, width, height) in [
        ("200x300pt", 200.0, 300.0),
        ("100pxx200px", 75.0, 150.0),
        ("100px 200", 75.0, 150.0),
        ("1inX2in", 72.0, 144.0),
        ("2.54cm 25.4mm", 72.0, 72.0),
        (" Letter ", 612.0, 792.0),
    ] {
        let size = parse_page_size(input).unwrap();
        assert!((size.width - width).abs() < 0.001, "{input}");
        assert!((size.height - height).abs() < 0.001, "{input}");
    }
    for input in [
        "1x2",
        "0x2pt",
        "nanx2pt",
        "-1x2pt",
        "1pxpx2px",
        "1ptx2ptjunk",
        "1e2ptx2pt",
    ] {
        assert!(parse_page_size(input).is_err(), "{input}");
    }
}

#[test]
fn margin_shorthand_and_invalid_values() {
    for (input, expected) in [
        ("25.4", [72.0, 72.0, 72.0, 72.0]),
        ("25.4 50.8", [72.0, 144.0, 72.0, 144.0]),
        ("25.4 50.8 76.2", [72.0, 144.0, 216.0, 144.0]),
        ("0 25.4 50.8 76.2", [0.0, 72.0, 144.0, 216.0]),
    ] {
        let margin = parse_margin(input).unwrap();
        for (value, wanted) in [margin.top, margin.right, margin.bottom, margin.left]
            .into_iter()
            .zip(expected)
        {
            assert!((value - wanted).abs() < 0.001, "{input}");
        }
    }
    for input in ["", "1 2 3 4 5", "nan", "inf", "-1", "bad", "3.4e38"] {
        assert!(parse_margin(input).is_err(), "{input}");
    }
}
