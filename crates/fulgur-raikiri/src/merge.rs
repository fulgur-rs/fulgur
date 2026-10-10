//! Merge the extra pages of a streamed PDF into the pages they complete.
//!
//! [`crate::stream`] appends one extra page after the last page for every
//! page that was delivered without its margin boxes or internal links. Each
//! extra page has the size of its page. Merging turns the extra page's
//! content into a form XObject drawn before the page's own content, so it
//! sits below the page body as margin boxes do, moves its annotations to
//! the page, and removes the extra page.

use fulgur_core::{Error, Result};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// Name of the form XObject in the resources of a merged page.
const FORM_NAME: &[u8] = b"FulgurMarginBoxes";

/// Merge extra page `k` (counted after the last page) into page
/// `targets[k]`, both zero-based, and remove the extra pages.
pub(crate) fn merge_extra_pages(pdf: &[u8], targets: &[u32]) -> Result<Vec<u8>> {
    let mut document = Document::load_mem(pdf).map_err(pdf_error)?;
    let pages: Vec<ObjectId> = document.get_pages().into_values().collect();
    let first_extra = pages
        .len()
        .checked_sub(targets.len())
        .ok_or_else(|| Error::PdfGeneration("missing extra pages".into()))?;
    for (offset, &target) in targets.iter().enumerate() {
        let target = *pages
            .get(target as usize)
            .filter(|_| (target as usize) < first_extra)
            .ok_or_else(|| Error::PdfGeneration("extra page target out of range".into()))?;
        merge_page(&mut document, pages[first_extra + offset], target)?;
    }
    for &extra in &pages[first_extra..] {
        remove_page(&mut document, extra)?;
    }
    document.prune_objects();
    let mut out = Vec::new();
    document.save_to(&mut out)?;
    Ok(out)
}

fn merge_page(document: &mut Document, extra: ObjectId, target: ObjectId) -> Result<()> {
    let extra_page = document.get_dictionary(extra).map_err(pdf_error)?.clone();
    if let Some(form) = form(document, &extra_page)? {
        let form = document.add_object(form);
        let mut resources = owned_dictionary(document, target, b"Resources")?;
        let mut xobjects = match resources.get(b"XObject") {
            Ok(object) => resolve_dictionary(document, object)?,
            Err(_) => Dictionary::new(),
        };
        xobjects.set(FORM_NAME, Object::Reference(form));
        resources.set("XObject", Object::Dictionary(xobjects));
        let mut draw = b"q /".to_vec();
        draw.extend_from_slice(FORM_NAME);
        draw.extend_from_slice(b" Do Q\n");
        let draw = document.add_object(Stream::new(Dictionary::new(), draw));
        let mut contents = vec![Object::Reference(draw)];
        let page = document.get_dictionary(target).map_err(pdf_error)?;
        match page.get(b"Contents") {
            Ok(Object::Array(items)) => contents.extend(items.iter().cloned()),
            Ok(object) => contents.push(object.clone()),
            Err(_) => {}
        }
        let page = document.get_dictionary_mut(target).map_err(pdf_error)?;
        page.set("Resources", Object::Dictionary(resources));
        page.set("Contents", Object::Array(contents));
    }
    let annotations = match extra_page.get(b"Annots") {
        Ok(object) => resolve_array(document, object)?,
        Err(_) => Vec::new(),
    };
    if !annotations.is_empty() {
        let mut merged = match document
            .get_dictionary(target)
            .map_err(pdf_error)?
            .get(b"Annots")
        {
            Ok(object) => resolve_array(document, object)?,
            Err(_) => Vec::new(),
        };
        merged.extend(annotations);
        document
            .get_dictionary_mut(target)
            .map_err(pdf_error)?
            .set("Annots", Object::Array(merged));
    }
    Ok(())
}

/// The content of `page` as a form XObject, or `None` when it has no
/// content.
fn form(document: &Document, page: &Dictionary) -> Result<Option<Stream>> {
    let contents = match page.get(b"Contents") {
        Ok(Object::Array(items)) => items.clone(),
        Ok(object) => vec![object.clone()],
        Err(_) => return Ok(None),
    };
    let mut content = Vec::new();
    for item in &contents {
        let (_, object) = document.dereference(item).map_err(pdf_error)?;
        let stream = object.as_stream().map_err(pdf_error)?;
        let bytes = if stream.dict.has(b"Filter") {
            stream.decompressed_content().map_err(pdf_error)?
        } else {
            stream.content.clone()
        };
        content.extend_from_slice(&bytes);
        content.push(b'\n');
    }
    if content.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let media_box = page
        .get(b"MediaBox")
        .map_err(|_| Error::PdfGeneration("extra page without a media box".into()))?
        .clone();
    let mut dictionary = Dictionary::new();
    dictionary.set("Type", Object::Name(b"XObject".to_vec()));
    dictionary.set("Subtype", Object::Name(b"Form".to_vec()));
    dictionary.set("BBox", media_box);
    if let Ok(resources) = page.get(b"Resources") {
        dictionary.set("Resources", resources.clone());
    }
    if let Ok(group) = page.get(b"Group") {
        dictionary.set("Group", group.clone());
    }
    let mut stream = Stream::new(dictionary, content);
    // Compression only fails for content it cannot shrink, which stays as is.
    let _ = stream.compress();
    Ok(Some(stream))
}

/// Remove `page` from its parent's kids and from the page counts.
fn remove_page(document: &mut Document, page: ObjectId) -> Result<()> {
    let mut parent = document
        .get_dictionary(page)
        .map_err(pdf_error)?
        .get(b"Parent")
        .and_then(Object::as_reference)
        .map_err(pdf_error)?;
    let kids = document
        .get_dictionary_mut(parent)
        .map_err(pdf_error)?
        .get_mut(b"Kids")
        .and_then(Object::as_array_mut)
        .map_err(pdf_error)?;
    kids.retain(|kid| kid.as_reference().ok() != Some(page));
    loop {
        let tree = document.get_dictionary_mut(parent).map_err(pdf_error)?;
        if let Ok(count) = tree.get(b"Count").and_then(Object::as_i64) {
            tree.set("Count", count - 1);
        }
        match tree.get(b"Parent").and_then(Object::as_reference) {
            Ok(next) => parent = next,
            Err(_) => break,
        }
    }
    document.delete_object(page);
    Ok(())
}

/// A copy of the dictionary in entry `key` of dictionary `id`, empty when
/// the entry is missing.
fn owned_dictionary(document: &Document, id: ObjectId, key: &[u8]) -> Result<Dictionary> {
    match document.get_dictionary(id).map_err(pdf_error)?.get(key) {
        Ok(object) => resolve_dictionary(document, object),
        Err(_) => Ok(Dictionary::new()),
    }
}

fn resolve_dictionary(document: &Document, object: &Object) -> Result<Dictionary> {
    let (_, object) = document.dereference(object).map_err(pdf_error)?;
    object.as_dict().cloned().map_err(pdf_error)
}

fn resolve_array(document: &Document, object: &Object) -> Result<Vec<Object>> {
    let (_, object) = document.dereference(object).map_err(pdf_error)?;
    object.as_array().cloned().map_err(pdf_error)
}

fn pdf_error(error: lopdf::Error) -> Error {
    Error::PdfGeneration(format!("merging streamed pages: {error}"))
}
