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

fn dot_png() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/list-style-image/bullet.png"),
    )
    .unwrap()
}

/// How many times an image is drawn while rendering `html` with `bundle`.
///
/// These documents draw no opacity groups, so every XObject drawn is an image.
fn drawn_images(html: &str, bundle: &AssetBundle) -> usize {
    let options = RenderOptions {
        assets: Some(bundle),
        system_fonts: true,
    };
    let (pdf, operations) = operations_with(html, &options);
    let forms = pdf
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter(|stream| {
            stream
                .dict
                .get(b"Subtype")
                .is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Form"))
        })
        .count();
    assert_eq!(forms, 0, "an opacity group would also be drawn with Do");
    count(&operations, "Do")
}

const PAGE: &str = "<style>@page{size:200px 200px;margin:0}body{margin:0}</style>";

#[test]
fn bundled_images_resolve_by_path_relative_to_the_input() {
    let mut bundle = AssetBundle::new();
    bundle.add_image("img/dot.png", dot_png());
    for html in [
        "<img src='img/dot.png'>",
        "<img src='./img/dot.png'>",
        "<div style='width:8px;height:8px;background-image:url(img/dot.png)'></div>",
        "<ul><li style='list-style-image:url(img/dot.png)'>item</li></ul>",
    ] {
        assert!(
            drawn_images(&format!("{PAGE}{html}"), &bundle) > 0,
            "no image drawn for {html}"
        );
    }
    // A name the bundle lacks, with no file on disk, draws nothing.
    assert_eq!(
        drawn_images(&format!("{PAGE}<img src='missing.png'>"), &bundle),
        0
    );
}

#[test]
fn bundled_images_match_percent_encoded_and_absolute_urls() {
    let mut bundle = AssetBundle::new();
    bundle.add_image("a b.png", dot_png());
    bundle.add_image("https://images.test/dot.png", dot_png());
    for html in [
        "<div style='width:8px;height:8px;background-image:url(\"a%20b.png\")'></div>",
        "<img src='https://images.test/dot.png'>",
    ] {
        assert_eq!(drawn_images(&format!("{PAGE}{html}"), &bundle), 1, "{html}");
    }
}

#[test]
fn bundled_images_take_precedence_over_local_files() {
    let (dir, path) = input(&format!("{PAGE}<img src='dot.png' style='width:8px'>"));
    // The file on disk is not an image; the bundle's copy is drawn.
    std::fs::write(dir.path().join("dot.png"), b"not a png").unwrap();
    let mut bundle = AssetBundle::new();
    bundle.add_image("dot.png", dot_png());
    let options = RenderOptions {
        assets: Some(&bundle),
        system_fonts: true,
    };
    let bytes = render_with_options(&path, &Config::default(), &options).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    assert!(pdf.objects.values().any(|object| {
        object.as_stream().is_ok_and(|stream| {
            stream
                .dict
                .get(b"Subtype")
                .is_ok_and(|subtype| subtype.as_name().is_ok_and(|name| name == b"Image"))
        })
    }));
}

#[test]
fn background_url_tiles_follow_repeat_size_and_position() {
    let mut bundle = AssetBundle::new();
    bundle.add_image("dot.png", dot_png());
    let tiles = |style: &str| {
        drawn_images(
            &format!(
                "{PAGE}<div style='width:32px;height:16px;background-image:url(dot.png);{style}'></div>"
            ),
            &bundle,
        )
    };
    assert_eq!(tiles(""), 8);
    assert_eq!(tiles("background-repeat:no-repeat"), 1);
    assert_eq!(tiles("background-repeat:repeat-x"), 4);
    assert_eq!(tiles("background-size:16px 16px"), 2);
    assert_eq!(
        tiles("background-size:cover;background-repeat:no-repeat"),
        1
    );
    // An offset origin tile adds a partial column and row on each side.
    assert_eq!(tiles("background-position:4px 4px"), 15);
}

#[test]
fn background_url_reads_local_files_without_a_bundle() {
    let (dir, path) = input(&format!(
        "{PAGE}<div style='width:8px;height:8px;background-image:url(dot.png)'></div>"
    ));
    std::fs::write(dir.path().join("dot.png"), dot_png()).unwrap();
    let bytes = render(&path, &Config::default()).unwrap();
    let pdf = lopdf::Document::load_mem(&bytes).unwrap();
    let content = pdf.get_page_content(pdf.get_pages()[&1]).unwrap();
    let operations = lopdf::content::Content::decode(&content)
        .unwrap()
        .operations;
    assert_eq!(count(&operations, "Do"), 1);
}

#[test]
fn margin_box_background_url_is_drawn() {
    let mut bundle = AssetBundle::new();
    bundle.add_image("dot.png", dot_png());
    let html = "<style>@page{size:200px 200px;margin:40px;@top-center{content:'Header';\
                background-image:url(dot.png);background-repeat:no-repeat}}</style><p>Body</p>";
    assert_eq!(drawn_images(html, &bundle), 1);
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
