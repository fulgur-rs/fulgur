use std::path::Path;
use std::process::Command;

#[test]
fn dev_fixed_fonts_and_date_are_deterministic() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let input = root.join("tests/fixtures/raikiri-dev/bookmarks.html");
    let font = root.join("crates/fulgur-ruby/spec/fixtures/noto_sans.ttf");
    let dir = tempfile::tempdir().unwrap();
    let mut results = Vec::new();
    for index in 0..3 {
        let output = dir.path().join(format!("{index}.pdf"));
        let result = Command::new(env!("CARGO_BIN_EXE_fulgur-dev"))
            .args(["render", "--engine", "raikiri"])
            .arg(&input)
            .args(["--font"])
            .arg(&font)
            .args([
                "--no-system-fonts",
                "--bookmarks",
                "--title",
                "Report",
                "--author",
                "Alice",
                "--author",
                "Bob",
                "--language",
                "ja",
                "--creation-date",
                "2026-10-07T01:02:03Z",
                "-o",
            ])
            .arg(&output)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        results.push(std::fs::read(output).unwrap());
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[1], results[2]);
    let pdf = lopdf::Document::load_mem(&results[0]).unwrap();
    let pages = pdf.get_pages();
    assert_eq!(pages.len(), 2);
    assert_eq!(
        pdf.extract_text(&[1, 2])
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["Alpha", "Beta", "Next", "Web", "Gamma"]
    );
    let page = pdf.get_dictionary(pages[&1]).unwrap();
    let links = page.get(b"Annots").unwrap().as_array().unwrap();
    assert_eq!(links.len(), 2);
    let internal = pdf
        .get_dictionary(links[0].as_reference().unwrap())
        .unwrap();
    let action = internal.get(b"A").unwrap().as_dict().unwrap();
    assert_eq!(action.get(b"S").unwrap().as_name().unwrap(), b"GoTo");
    let destination = pdf
        .get_object(action.get(b"D").unwrap().as_reference().unwrap())
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(destination[0].as_reference().unwrap(), pages[&2]);
    assert_eq!(destination[2].as_float().unwrap(), 0.0);
    assert_eq!(destination[3].as_float().unwrap(), 150.0);
    let external = pdf
        .get_dictionary(links[1].as_reference().unwrap())
        .unwrap();
    let action = external.get(b"A").unwrap().as_dict().unwrap();
    assert_eq!(
        action.get(b"URI").unwrap().as_str().unwrap(),
        b"https://example.com/"
    );
    let catalog = pdf.catalog().unwrap();
    let outlines = pdf
        .get_dictionary(catalog.get(b"Outlines").unwrap().as_reference().unwrap())
        .unwrap();
    let alpha = pdf
        .get_dictionary(outlines.get(b"First").unwrap().as_reference().unwrap())
        .unwrap();
    let beta = pdf
        .get_dictionary(alpha.get(b"First").unwrap().as_reference().unwrap())
        .unwrap();
    let gamma = pdf
        .get_dictionary(alpha.get(b"Next").unwrap().as_reference().unwrap())
        .unwrap();
    for (node, expected) in [(alpha, "Alpha"), (beta, "Beta"), (gamma, "Gamma")] {
        assert_eq!(
            lopdf::decode_text_string(node.get(b"Title").unwrap()).unwrap(),
            expected
        );
    }
    let info = pdf
        .get_dictionary(pdf.trailer.get(b"Info").unwrap().as_reference().unwrap())
        .unwrap();
    for (key, expected) in [("Title", "Report"), ("Author", "Alice, Bob")] {
        assert_eq!(
            lopdf::decode_text_string(info.get(key.as_bytes()).unwrap()).unwrap(),
            expected
        );
    }
    assert_eq!(
        lopdf::decode_text_string(catalog.get(b"Lang").unwrap()).unwrap(),
        "ja"
    );
    assert!(
        lopdf::decode_text_string(info.get(b"CreationDate").unwrap())
            .unwrap()
            .starts_with("D:20261007010203")
    );
}
