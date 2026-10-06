use super::*;

fn info(pdf: &lopdf::Document) -> &lopdf::Dictionary {
    pdf.get_dictionary(pdf.trailer.get(b"Info").unwrap().as_reference().unwrap())
        .unwrap()
}

#[test]
fn metadata_round_trip() {
    let (_dir, path) = input("<p>Hello</p>");
    let config = Config::builder()
        .title("Report")
        .authors(["Alice", "Bob"])
        .description("Quarterly")
        .keywords(["one", "two"])
        .lang("ja")
        .creator("tool")
        .producer("fulgur-dev")
        .creation_date("2026-10-07T01:02:03Z")
        .build();
    let bytes = render(&path, &config).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let info = info(&pdf);
    for (key, expected) in [
        ("Title", "Report"),
        ("Author", "Alice, Bob"),
        ("Subject", "Quarterly"),
        ("Keywords", "one, two"),
        ("Creator", "tool"),
        ("Producer", "fulgur-dev"),
    ] {
        assert_eq!(
            lopdf::decode_text_string(info.get(key.as_bytes()).unwrap()).unwrap(),
            expected
        );
    }
    let date = lopdf::decode_text_string(info.get(b"CreationDate").unwrap()).unwrap();
    assert!(date.starts_with("D:20261007010203"), "{date}");
    let catalog = pdf.catalog().unwrap();
    assert_eq!(
        lopdf::decode_text_string(catalog.get(b"Lang").unwrap()).unwrap(),
        "ja"
    );
    let stream = pdf
        .get_object(catalog.get(b"Metadata").unwrap().as_reference().unwrap())
        .unwrap()
        .as_stream()
        .unwrap();
    let xmp = stream.get_plain_content().unwrap();
    let xmp = String::from_utf8(xmp).unwrap();
    for expected in [
        "Report",
        "Alice",
        "Bob",
        "Quarterly",
        "one, two",
        "ja",
        "tool",
        "fulgur-dev",
        "2026-10-07T01:02:03",
    ] {
        assert!(xmp.contains(expected), "{expected}: {xmp}");
    }
}

#[test]
fn metadata_omitted_date_does_not_insert_current_time() {
    let (_dir, path) = input("<p>Hello</p>");
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(!info(&pdf).has(b"CreationDate"));
    assert!(!info(&pdf).has(b"ModDate"));
}

#[test]
fn metadata_invalid_dates_return_errors() {
    let (_dir, path) = input("<p>Hello</p>");
    for date in [
        "2026-02-29",
        "2026-00-10",
        "2026-04-31",
        "2026-13",
        "2026-10-07T24:00:00Z",
        "2026-10-07T01:60:00Z",
        "2026-10-07T01:02:60Z",
        "2026-10-07T01:02:03ZZ",
        "2026-10-07garbage",
        "2026-10-07T1:2:3",
        "10000",
        "2026-1",
    ] {
        let result = render(&path, &Config::builder().creation_date(date).build());
        assert!(result.is_err(), "{date}");
    }
}

#[test]
fn metadata_supported_partial_dates_render() {
    let (_dir, path) = input("<p>Hello</p>");
    for (date, expected) in [
        ("2026", "D:20260101000000"),
        ("2026-10", "D:20261001000000"),
        ("2026-10-07", "D:20261007000000"),
        ("2024-02-29", "D:20240229000000"),
        ("2000-02-29", "D:20000229000000"),
        ("2026-10-07T01:02:03", "D:20261007010203"),
        ("2026-10-07T01:02:03Z", "D:20261007010203"),
    ] {
        let bytes = render(&path, &Config::builder().creation_date(date).build()).unwrap();
        let pdf = lopdf::Document::load_mem(&bytes).unwrap();
        let stored = lopdf::decode_text_string(info(&pdf).get(b"CreationDate").unwrap()).unwrap();
        assert!(stored.starts_with(expected), "{date}: {stored}");
    }
}

#[test]
fn metadata_unsupported_tagging_returns_error() {
    let (_dir, path) = input("<p>Hello</p>");
    for config in [
        Config::builder().tagged(true).build(),
        Config::builder().pdf_ua(true).build(),
    ] {
        assert!(render(&path, &config).is_err());
    }
}
