use super::*;
use fulgur_core::AssetBundle;

fn font_path(woff2: bool) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(if woff2 {
        "../fulgur-blitz/tests/fixtures/fonts/NotoSans-Regular.woff2"
    } else {
        "../fulgur-ruby/spec/fixtures/noto_sans.ttf"
    })
}

#[test]
fn bundled_family_is_used_without_system_fonts() {
    for woff2 in [false, true] {
        let family = if woff2 { "Noto Sans" } else { "Noto Sans Mono" };
        let (_dir, path) = input(&format!(
            "<style>body {{font-family:'{family}'}}</style><p>Hello bundled font</p>"
        ));
        let mut bundle = AssetBundle::new();
        bundle.add_font_file(font_path(woff2)).unwrap();
        let options = RenderOptions {
            assets: Some(&bundle),
            system_fonts: false,
        };
        let bytes = render_with_options(&path, &Config::default(), &options).unwrap();
        let pdf = lopdf::Document::load_mem(&bytes).unwrap();
        assert!(
            pdf.extract_text(&[1])
                .unwrap()
                .contains("Hello bundled font")
        );
        let names: Vec<_> = pdf
            .objects
            .values()
            .filter_map(|obj| obj.as_dict().ok())
            .filter_map(|dict| dict.get(b"BaseFont").ok())
            .filter_map(|name| name.as_name().ok())
            .collect();
        assert!(!names.is_empty());
        assert!(
            names
                .iter()
                .all(|name| String::from_utf8_lossy(name).contains("NotoSans"))
        );
    }
}

#[test]
fn no_system_fonts_without_bundle_errors() {
    let (_dir, path) = input("<p>Hello</p>");
    let result = render_with_options(
        &path,
        &Config::default(),
        &RenderOptions {
            assets: None,
            system_fonts: false,
        },
    );
    assert!(matches!(result, Err(Error::Asset(_))));
}

#[test]
fn image_bundle_is_not_advertised_as_supported() {
    let (_dir, path) = input("<p>Hello</p>");
    let mut bundle = AssetBundle::new();
    bundle.add_image("test.png", vec![1, 2, 3]);
    let result = render_with_options(
        &path,
        &Config::default(),
        &RenderOptions {
            assets: Some(&bundle),
            system_fonts: true,
        },
    );
    assert!(matches!(result, Err(Error::Asset(_))));
}

#[test]
fn invalid_bundled_font_errors() {
    let (_dir, path) = input("<p>Hello</p>");
    let mut bundle = AssetBundle::new();
    bundle.fonts.push(std::sync::Arc::new(vec![0, 1, 2, 3]));
    let result = render_with_options(
        &path,
        &Config::default(),
        &RenderOptions {
            assets: Some(&bundle),
            system_fonts: true,
        },
    );
    assert!(matches!(result, Err(Error::Asset(_))));
}

#[test]
fn bundle_css_preserves_registration_order() {
    let (_dir, path) = input("<p>Hello</p>");
    let mut bundle = AssetBundle::new();
    bundle.add_css("@page {size: 200pt 300pt; margin:0}");
    bundle.add_css("@page {size: 250pt 350pt; margin:0}");
    let bytes = render_with_options(
        &path,
        &Config::default(),
        &RenderOptions {
            assets: Some(&bundle),
            system_fonts: true,
        },
    )
    .unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let rect = pdf
        .get_dictionary(pdf.get_pages()[&1])
        .unwrap()
        .get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(rect[2].as_float().unwrap(), 250.0);
    assert_eq!(rect[3].as_float().unwrap(), 350.0);
}

#[test]
fn external_css_import_resolves_from_stylesheet_directory() {
    let (dir, path) = input("<link rel=stylesheet href='styles/main.css'><p>Hello</p>");
    std::fs::create_dir(dir.path().join("styles")).unwrap();
    std::fs::write(dir.path().join("styles/main.css"), "@import 'page.css';").unwrap();
    std::fs::write(
        dir.path().join("styles/page.css"),
        "@page {size: 250pt 350pt; margin:0}",
    )
    .unwrap();
    let document = completed(&path);
    let geometry = document.page(0).unwrap().geometry();
    assert!((geometry.page_box.width * 0.75 - 250.0).abs() < 0.01);
    assert!((geometry.page_box.height * 0.75 - 350.0).abs() < 0.01);
}

#[test]
fn bundled_font_collection_is_rejected() {
    let (_dir, path) = input("<p>Hello</p>");
    let mut font = std::fs::read(font_path(false)).unwrap();
    let tables = u16::from_be_bytes(font[4..6].try_into().unwrap());
    for table in 0..usize::from(tables) {
        let offset = 12 + table * 16 + 8;
        let value = u32::from_be_bytes(font[offset..offset + 4].try_into().unwrap());
        font[offset..offset + 4].copy_from_slice(&(value + 20).to_be_bytes());
    }
    let mut collection = b"ttcf".to_vec();
    for value in [0x0001_0000_u32, 2, 20, 20] {
        collection.extend(value.to_be_bytes());
    }
    collection.extend(font);
    let file = skrifa::raw::FileRef::new(&collection).unwrap();
    assert_eq!(file.fonts().filter_map(|face| face.ok()).count(), 2);
    let mut bundle = AssetBundle::new();
    bundle.fonts.push(std::sync::Arc::new(collection));
    let result = render_with_options(
        &path,
        &Config::default(),
        &RenderOptions {
            assets: Some(&bundle),
            system_fonts: false,
        },
    );
    assert!(matches!(result, Err(Error::Asset(message)) if message.contains("collections")));
}
