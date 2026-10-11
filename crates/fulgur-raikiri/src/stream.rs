//! Streaming rendering: pages are painted as Raikiri finalizes them, while
//! the input is still being read.
//!
//! A page delivered before the end of the input does not know two things:
//! the number of pages, which `counter(pages)` shows in its margin boxes,
//! and the destinations of its links to anchors of the document. Such a page
//! is painted without its margin boxes and its internal links. When the
//! layout ends, they are drawn on an extra page of the same size, which
//! [`crate::merge`] then lays under the original page and removes.

use crate::bookmarks::{BookmarkCollector, OutlineBuilder};
use crate::paint::navigation::PageLink;
use crate::paint::{LinkTargets, Painter, document_lang, html_title};
use crate::tagging::Tags;
use crate::{RenderOptions, Source, bookmarks, metadata, page_defaults, with_resources};
use fulgur_core::{Config, Error, Result};
use raikiri_html::{
    LayoutConfig, MarginBox, PageSink, PaintRect, RenderResources, StreamPage, StreamStatus,
    StreamSummary, StreamingLayout,
};
use raikiri_traits::{ConsumerPropertyEvent, ConsumerPropertyObserver};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::rc::Rc;

/// Input bytes handed to the layout at a time.
const READ_CHUNK: usize = 64 * 1024;

/// Read, lay out, and paint an HTML file, painting each page as soon as no
/// later input can change it.
///
/// The PDF shows the same pages as [`crate::render_with_options`], with
/// these differences, which keep delivered pages final: `<style>` elements
/// inside `<body>` are ignored, as is a `position: fixed` element that
/// starts after the first page was delivered. A heading whose element has
/// no fragment of its own gets no bookmark. On a page whose margin boxes
/// show the page count, a margin box that shows a running element draws
/// that element's text instead.
///
/// # Errors
/// Returns the errors of [`crate::render_with_options`], and an error when
/// the config asks for tagged PDF or PDF/UA, which need the whole document
/// before the first page.
pub fn render_streaming(
    input: &Path,
    config: &Config,
    options: &RenderOptions<'_>,
) -> Result<Vec<u8>> {
    config.validate()?;
    metadata::build(config, None, None)?;
    if config.effective_tagging() {
        return Err(Error::Other(
            "tagged PDF and PDF/UA are not supported with streaming".into(),
        ));
    }
    with_resources(
        Source::File(input),
        config,
        options,
        |resources, document_url| {
            let failure = Rc::new(RefCell::new(None));
            let registrations = if config.effective_bookmarks() {
                bookmarks::registrations()
            } else {
                Vec::new()
            };
            let sink = PdfSink {
                resources,
                config,
                options,
                painter: None,
                lang: None,
                document_url,
                outline: config.effective_bookmarks().then(OutlineBuilder::default),
                pages: Vec::new(),
                failure: Rc::clone(&failure),
            };
            let mut stream = StreamingLayout::new(
                resources,
                page_defaults(config),
                LayoutConfig::default(),
                sink,
            )
            .consumer_properties(&registrations);
            let mut file = std::fs::File::open(input)?;
            let mut buffer = vec![0; READ_CHUNK];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                if let Err(error) = stream.feed(&buffer[..read]) {
                    return Err(failure.take().unwrap_or_else(|| layout_error(error)));
                }
            }
            match stream.finish() {
                Ok(StreamStatus::Completed(result)) => result,
                Ok(_) => Err(Error::Layout(
                    "Raikiri layout was aborted or did not complete".into(),
                )),
                Err(error) => Err(failure.take().unwrap_or_else(|| layout_error(error))),
            }
        },
    )
}

fn layout_error(error: raikiri_traits::RenderError) -> Error {
    Error::Layout(error.to_string())
}

/// What a delivered page still needs once the layout ends.
struct PendingPage {
    page_box: PaintRect,
    /// Whether its margin boxes were left out for showing the page count.
    margin_boxes: bool,
    internal_links: Vec<PageLink>,
}

struct PdfSink<'a, 'u> {
    resources: &'a RenderResources<'a>,
    config: &'u Config,
    options: &'u RenderOptions<'a>,
    /// Started with the first page, whose `<head>` gives the metadata and
    /// the base URL of images.
    painter: Option<Painter<'a>>,
    lang: Option<String>,
    document_url: &'u url::Url,
    outline: Option<OutlineBuilder>,
    pages: Vec<PendingPage>,
    /// A painting error, kept because the sink can only report IO errors.
    failure: Rc<RefCell<Option<Error>>>,
}

impl<'a> PdfSink<'a, '_> {
    /// Start the PDF with the metadata of `page`, the first page, and with
    /// images resolving against `base`.
    fn start(&mut self, page: Option<&raikiri_html::Page<'_>>, base: url::Url) -> Result<()> {
        self.lang = document_lang(self.config, page);
        let mut painter = Painter::new(
            self.resources,
            self.config,
            self.options,
            base,
            Tags::disabled(),
        );
        painter.set_metadata(metadata::build(
            self.config,
            page.and_then(html_title),
            self.lang.clone(),
        )?);
        self.painter = Some(painter);
        Ok(())
    }

    fn paint(&mut self, page: StreamPage<'_>, events: Vec<ConsumerPropertyEvent>) -> Result<()> {
        let index = page.index();
        let base_url = page.base_url();
        let page = page.page();
        if self.painter.is_none() {
            let base = base_url.unwrap_or(self.document_url).clone();
            self.start(Some(&page), base)?;
        }
        let painter = self.painter.as_mut().expect("started above");
        let margin_boxes = page.margin_boxes();
        let deferred = margin_boxes
            .iter()
            .any(|margin_box| !margin_box.deferred.is_empty());
        let shown: &[MarginBox] = if deferred { &[] } else { &margin_boxes };
        let targets = LinkTargets {
            base_url,
            document_url: self.document_url,
            anchors: None,
        };
        let internal_links = painter.page(&page, shown, &targets)?;
        if let Some(outline) = &mut self.outline {
            outline.add_page(&page);
            let mut collector = BookmarkCollector::default();
            for event in events {
                collector.observe_event(event)?;
            }
            outline.add_headings(&page, collector);
        }
        debug_assert_eq!(self.pages.len(), index as usize);
        self.pages.push(PendingPage {
            page_box: page.geometry().page_box,
            margin_boxes: deferred,
            internal_links,
        });
        Ok(())
    }

    fn finish(mut self, summary: StreamSummary) -> Result<Vec<u8>> {
        if self.painter.is_none() {
            self.start(None, self.document_url.clone())?;
        }
        let mut painter = self.painter.take().expect("started above");
        let open = match self.outline.take() {
            Some(outline) => {
                let outline = outline.finish();
                painter.set_outline(outline.outline);
                outline.open
            }
            None => Vec::new(),
        };
        let mut margin_boxes: BTreeMap<_, _> =
            summary.page_count_margin_boxes.into_iter().collect();
        let mut extra_pages = Vec::new();
        for (index, page) in self.pages.iter().enumerate() {
            let boxes = if page.margin_boxes {
                margin_boxes.remove(&(index as u32)).unwrap_or_default()
            } else {
                Vec::new()
            };
            let resolves = page
                .internal_links
                .iter()
                .any(|link| link.annotation(Some(&summary.anchors)).is_some());
            if boxes.is_empty() && !resolves {
                continue;
            }
            painter.margin_page(
                page.page_box,
                &boxes,
                &page.internal_links,
                &summary.anchors,
            )?;
            extra_pages.push(index as u32);
        }
        let pdf = painter.finish(self.lang)?;
        // Before the merge, whose object streams this rewrite would drop.
        let pdf = bookmarks::apply_open_state(pdf, &open)?;
        if extra_pages.is_empty() {
            return Ok(pdf);
        }
        crate::merge::merge_extra_pages(pdf, &extra_pages)
    }
}

impl PageSink for PdfSink<'_, '_> {
    type Output = Result<Vec<u8>>;

    fn page(
        &mut self,
        page: StreamPage<'_>,
        events: Vec<ConsumerPropertyEvent>,
    ) -> std::io::Result<()> {
        self.paint(page, events).map_err(|error| {
            let io = std::io::Error::other(error.to_string());
            *self.failure.borrow_mut() = Some(error);
            io
        })
    }

    fn finish(self, summary: StreamSummary) -> std::io::Result<Self::Output> {
        Ok(PdfSink::finish(self, summary))
    }
}
