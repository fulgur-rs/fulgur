use super::merge_extra_pages;
use lopdf::{Dictionary, Document, Object, Stream, dictionary};

/// A two-page PDF: page 1 is the target, page 2 the extra page drawing a
/// line. `target` returns extra entries of the target page and `metadata` is
/// the XMP packet, if any.
fn pdf(target: impl FnOnce(&mut Document) -> Dictionary, metadata: Option<&[u8]>) -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let target = target(&mut document);
    let pages_id = document.new_object_id();
    let extra_content =
        document.add_object(Stream::new(Dictionary::new(), b"0 0 m 10 10 l S".to_vec()));
    let mut target_page = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
    };
    target_page.extend(&target);
    let target_id = document.add_object(target_page);
    let extra_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        "Contents" => extra_content,
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![target_id.into(), extra_id.into()],
            "Count" => 2,
        }),
    );
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
    if let Some(metadata) = metadata {
        let stream = document.add_object(Stream::new(
            dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
            metadata.to_vec(),
        ));
        catalog.set("Metadata", stream);
    }
    let catalog_id = document.add_object(catalog);
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

/// The XObject names in the resources of the only page of `bytes`.
fn xobject_names(bytes: &[u8]) -> Vec<Vec<u8>> {
    let document = Document::load_mem(bytes).unwrap();
    let pages = document.get_pages();
    assert_eq!(pages.len(), 1);
    let page = document.get_dictionary(pages[&1]).unwrap();
    let resources = match page.get(b"Resources").unwrap() {
        Object::Reference(id) => document.get_dictionary(*id).unwrap(),
        object => object.as_dict().unwrap(),
    };
    let xobjects = match resources.get(b"XObject").unwrap() {
        Object::Reference(id) => document.get_dictionary(*id).unwrap(),
        object => object.as_dict().unwrap(),
    };
    xobjects.iter().map(|(name, _)| name.clone()).collect()
}

#[test]
fn a_target_without_resources_gets_the_form() {
    let merged = merge_extra_pages(pdf(|_| Dictionary::new(), None), &[0]).unwrap();
    assert_eq!(xobject_names(&merged), [b"FulgurMarginBoxes".to_vec()]);
}

#[test]
fn the_form_joins_the_existing_xobjects_of_the_target() {
    let target = |document: &mut Document| {
        let form = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 1.into(), 1.into()],
            },
            Vec::new(),
        ));
        dictionary! {
            "Resources" => dictionary! { "XObject" => dictionary! { "Existing" => form } },
        }
    };
    let merged = merge_extra_pages(pdf(target, None), &[0]).unwrap();
    let mut names = xobject_names(&merged);
    names.sort();
    assert_eq!(names, [b"Existing".to_vec(), b"FulgurMarginBoxes".to_vec()]);
}

#[test]
fn metadata_without_a_complete_page_count_is_kept() {
    for packet in [
        b"<x:xmpmeta></x:xmpmeta>".as_slice(),
        b"<x:xmpmeta><xmpTPg:NPages>2</x:xmpmeta>".as_slice(),
    ] {
        let merged = merge_extra_pages(pdf(|_| Dictionary::new(), Some(packet)), &[0]).unwrap();
        let document = Document::load_mem(&merged).unwrap();
        let metadata = document
            .catalog()
            .unwrap()
            .get(b"Metadata")
            .unwrap()
            .as_reference()
            .unwrap();
        let stream = document.get_object(metadata).unwrap().as_stream().unwrap();
        assert_eq!(
            stream
                .decompressed_content()
                .unwrap_or(stream.content.clone()),
            packet
        );
    }
}

#[test]
fn an_unreadable_pdf_is_an_error() {
    let error = merge_extra_pages(b"not a pdf".to_vec(), &[0]).unwrap_err();
    assert!(
        error.to_string().contains("merging streamed pages"),
        "{error}"
    );
}
