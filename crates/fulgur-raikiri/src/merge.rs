//! Merge the extra pages of a streamed PDF into the pages they complete.
//!
//! [`crate::stream`] appends one extra page after the last page for every
//! page that was delivered without its margin boxes or internal links. Each
//! extra page has the size of its page. Merging turns the extra page's
//! content into a form XObject drawn before the page's own content, so it
//! sits below the page body as margin boxes do, moves its annotations to
//! the page, and removes the extra page.

use flpdf::{Matrix, ObjectHandle, ObjectRef, PageDocumentHelper, PageObjectHelper, Pdf};
use fulgur_core::{Error, Result};
use std::io::Cursor;
use std::rc::Rc;

/// Name of the form XObject in the resources of a merged page.
const FORM_NAME: &[u8] = b"/FulgurMarginBoxes";

type Document = Pdf<Cursor<Vec<u8>>>;

/// Merge extra page `k` (counted after the last page) into page
/// `targets[k]`, both zero-based, and remove the extra pages.
pub(crate) fn merge_extra_pages(pdf: &[u8], targets: &[u32]) -> Result<Vec<u8>> {
    let mut document = Pdf::open_mem_owned(pdf.to_vec()).map_err(pdf_error)?;
    let pages = flpdf::pages::page_refs(&mut document).map_err(pdf_error)?;
    let first_extra = pages
        .len()
        .checked_sub(targets.len())
        .ok_or_else(|| Error::PdfGeneration("missing extra pages".into()))?;
    for (offset, &target) in targets.iter().enumerate() {
        let target = *pages
            .get(target as usize)
            .filter(|_| (target as usize) < first_extra)
            .ok_or_else(|| Error::PdfGeneration("extra page target out of range".into()))?;
        merge_page(&mut document, pages[first_extra + offset], target).map_err(pdf_error)?;
    }
    let mut helper = PageDocumentHelper::new(&mut document);
    for &extra in &pages[first_extra..] {
        helper.remove_page(extra).map_err(pdf_error)?;
    }
    update_page_count(&mut document, first_extra).map_err(pdf_error)?;
    // The writer only keeps objects reachable from the trailer, so the
    // removed pages and their content streams are dropped.
    let mut writer = flpdf::PdfWriter::new(&mut document);
    writer.set_output_memory().map_err(pdf_error)?;
    writer.set_deterministic_id(true);
    // Krilla writes no object streams; packing the objects into them makes
    // the merged PDF smaller than the PDF of a batch render.
    writer.set_object_stream_mode(flpdf::ObjectStreamMode::Generate);
    writer.write().map_err(pdf_error)?;
    writer.get_buffer().map_err(pdf_error)
}

fn merge_page(document: &mut Document, extra: ObjectRef, target: ObjectRef) -> flpdf::Result<()> {
    if has_content(document, extra)? {
        let form = PageObjectHelper::new(extra, document).get_form_xobject_for_page(false)?;
        let resources = PageObjectHelper::new(target, document).get_resources(true)?;
        if resources.try_is_null()? {
            let fresh = ObjectHandle::dictionary(Vec::new());
            document
                .get_object_handle(target)
                .replace_key(b"/Resources", fresh.clone())?;
            add_form(&fresh, form)?;
        } else {
            add_form(&resources, form)?;
        }
        let mut draw = b"q ".to_vec();
        draw.extend_from_slice(FORM_NAME);
        draw.extend_from_slice(b" Do Q\n");
        let draw = document.new_stream_with_data(Rc::new(draw))?;
        PageObjectHelper::new(target, document).add_page_contents(draw, true)?;
    }
    let extra_page = document.get_object_handle(extra);
    PageObjectHelper::new(target, document).copy_annotations(extra_page, Matrix::default())
}

/// Register `form` under [`FORM_NAME`] in the XObjects of `resources`.
fn add_form(resources: &ObjectHandle, form: ObjectHandle) -> flpdf::Result<()> {
    let xobjects = resources.try_get_key(b"/XObject")?;
    if xobjects.try_is_dictionary()? {
        xobjects.replace_key(FORM_NAME, form)
    } else {
        resources.replace_key(
            b"/XObject",
            ObjectHandle::dictionary(vec![(FORM_NAME.to_vec(), form)]),
        )
    }
}

/// Whether the extra page draws anything; a page that only carries link
/// annotations has empty contents.
fn has_content(document: &mut Document, page: ObjectRef) -> flpdf::Result<bool> {
    for stream in PageObjectHelper::new(page, document).get_page_contents()? {
        let data = stream.get_stream_data(flpdf::DecodeLevel::Generalized)?;
        if !data.iter().all(u8::is_ascii_whitespace) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Set the page count in the XMP metadata, which was written with the
/// extra pages counted.
fn update_page_count(document: &mut Document, count: usize) -> flpdf::Result<()> {
    const OPEN: &[u8] = b"<xmpTPg:NPages>";
    const CLOSE: &[u8] = b"</xmpTPg:NPages>";
    let metadata = document
        .trailer()
        .try_get_key(b"/Root")?
        .try_get_key(b"/Metadata")?;
    if metadata.as_stream_dict().is_none() {
        return Ok(());
    }
    let data = metadata.get_stream_data(flpdf::DecodeLevel::Generalized)?;
    let Some(start) = find(&data, OPEN).map(|at| at + OPEN.len()) else {
        return Ok(());
    };
    let Some(end) = find(&data[start..], CLOSE).map(|at| start + at) else {
        return Ok(());
    };
    let mut updated = data[..start].to_vec();
    updated.extend_from_slice(count.to_string().as_bytes());
    updated.extend_from_slice(&data[end..]);
    // The data is decoded, so any filter is dropped with it.
    metadata.replace_stream_data(
        Rc::new(updated),
        Some(ObjectHandle::null()),
        Some(ObjectHandle::null()),
    );
    Ok(())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn pdf_error(error: flpdf::Error) -> Error {
    Error::PdfGeneration(format!("merging streamed pages: {error}"))
}
