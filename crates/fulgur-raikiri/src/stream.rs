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
use crate::paint::{
    Destinations, LinkTargets, Painter, RootSpace, document_lang, html_title, margin_boxes,
};
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
            .consumer_properties(&registrations)
            .svg_rendering(options.svg_rendering)
            .svg_raster_scale(options.svg_raster_scale);
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
    /// The space the page draws its content in, which moves the destinations
    /// of links to its anchors.
    space: RootSpace,
}

struct PdfSink<'a, 'u> {
    resources: &'a RenderResources<'a>,
    config: &'u Config,
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
        let mut painter = Painter::new(self.resources, self.config, base, Tags::disabled());
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
        let margin_boxes = margin_boxes(&page);
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
            space: RootSpace::of_page(&page),
        });
        Ok(())
    }

    fn finish(mut self, summary: StreamSummary) -> Result<Vec<u8>> {
        #[cfg(feature = "streaming-diagnostics")]
        eprintln!("{:?}", summary.diagnostics);
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
        let spaces: Vec<RootSpace> = self.pages.iter().map(|page| page.space).collect();
        let destinations = Destinations {
            anchors: &summary.anchors,
            spaces: &spaces,
        };
        for (index, page) in self.pages.iter().enumerate() {
            let boxes = if page.margin_boxes {
                margin_boxes.remove(&(index as u32)).unwrap_or_default()
            } else {
                Vec::new()
            };
            let resolves = page
                .internal_links
                .iter()
                .any(|link| link.annotation(Some(destinations)).is_some());
            if boxes.is_empty() && !resolves {
                continue;
            }
            painter.margin_page(page.page_box, &boxes, &page.internal_links, destinations)?;
            extra_pages.push(index as u32);
        }
        let rotations = painter.take_rotations();
        let pdf = painter.finish(self.lang)?;
        // Before the merge, which writes object streams and a cross-reference
        // stream that this in-place rewrite does not handle.
        let pdf = bookmarks::apply_open_state(pdf, &open)?;
        // The extra pages come after the delivered ones, so merging them
        // leaves the page indices of the rotations as they are.
        let pdf = if extra_pages.is_empty() {
            pdf
        } else {
            crate::merge::merge_extra_pages(pdf, &extra_pages)?
        };
        rotations.apply(pdf)
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

    fn stage_table_page(&mut self, page: StreamPage<'_>) -> std::io::Result<bool> {
        self.page(page, Vec::new())?;
        Ok(true)
    }

    fn wants_table_sample(&self) -> bool {
        true
    }

    fn stage_table_sample(
        &mut self,
        sample: &raikiri_html::DocumentLayout,
        ending: &raikiri_html::DocumentLayout,
        rows: &mut raikiri_html::TableTextRows<'_>,
    ) -> std::io::Result<Option<u32>> {
        use raikiri_dom::{Document, StandaloneAlign, StandaloneStyle};
        use raikiri_html::RunSource;
        use rayon::prelude::*;
        let initial = raikiri_html::computed::ComputedValues::initial();
        let fonts = self.resources.inline_engine_fonts();
        let mut queue = std::collections::VecDeque::new();
        let mut consumed = 0usize;
        let mut index = 0u32;
        let last = ending.page(1).unwrap();
        let row_count = |p: &raikiri_html::Page<'_>| {
            let dom = p.dom();
            p.fragments()
                .filter(|f| {
                    dom.local_name(f.node()) == Some("tr")
                        && dom.parent(f.node()).and_then(|p| dom.local_name(p)) == Some("tbody")
                })
                .count()
        };
        let pages = [sample.page(0).unwrap(), sample.page(1).unwrap(), last];
        let frames = pages.each_ref().map(|page| {
            let dom = page.dom();
            let originals = page.text_runs();
            let mut cell_runs = std::collections::HashMap::new();
            for (i, run) in originals.iter().enumerate() {
                let RunSource::Text(node) = run.source else {
                    return None;
                };
                let cell = dom.parent(node)?;
                let row = dom.parent(cell)?;
                let section = dom.parent(row)?;
                match (
                    dom.local_name(section),
                    dom.local_name(row),
                    dom.local_name(cell),
                ) {
                    (Some("thead"), Some("tr"), Some("th" | "td")) => {}
                    (Some("tbody"), Some("tr"), Some("td")) => {
                        if cell_runs.insert(cell, i).is_some() {
                            return None;
                        }
                    }
                    _ => return None,
                }
            }
            let widths: std::collections::HashMap<_, _> = page
                .fragments()
                .filter(|f| cell_runs.contains_key(&f.node()))
                .filter_map(|f| Some((f.node(), f.content_rect()?.width)))
                .collect();
            let mut slots = Vec::with_capacity(cell_runs.len());
            for row in page.fragments().map(|f| f.node()).filter(|&node| {
                dom.local_name(node) == Some("tr")
                    && dom.parent(node).and_then(|p| dom.local_name(p)) == Some("tbody")
            }) {
                for cell in dom
                    .children(row)
                    .filter(|&node| dom.local_name(node).is_some())
                {
                    slots.push((cell_runs.remove(&cell)?, cell, *widths.get(&cell)?));
                }
            }
            if !cell_runs.is_empty() {
                return None;
            }
            let mut styles = Vec::with_capacity(slots.len());
            for (i, parent, _) in &slots {
                let run = &originals[*i];
                let cv = page.computed(*parent)?;
                if cv.text_align != initial.text_align
                    || cv.direction != initial.direction
                    || cv.writing_mode != initial.writing_mode
                    || cv.font_weight != initial.font_weight
                    || cv.line_height != initial.line_height
                    || !run.decorations.is_empty()
                    || !run.shadows.is_empty()
                    || run.transform.is_some()
                {
                    return None;
                }
                let style = StandaloneStyle {
                    families: cv.font_family.to_vec(),
                    font_size: run.font_size,
                    ..Default::default()
                };
                styles.push(style);
            }
            Some((row_count(page), originals, slots, styles))
        });
        let [Some(first), Some(middle), Some(final_page)] = &frames else {
            return Ok(None);
        };
        while consumed < rows.row_count() {
            let (page, frame) = if index == 0 {
                (pages[0], first)
            } else if rows.row_count() - consumed == final_page.0 {
                (pages[2], final_page)
            } else {
                (pages[1], middle)
            };
            let (count, originals, slots, styles) = frame;
            let count = *count;
            if count == 0 || count > rows.row_count() - consumed {
                return Ok(None);
            }
            while queue.len() < count {
                let batch = rows.read_batch()?;
                if batch.is_empty() {
                    return Ok(None);
                }
                queue.extend(batch);
            }
            let columns = queue.front().map(Vec::len).unwrap_or(0);
            if columns == 0 || queue.iter().take(count).any(|row| row.len() != columns) {
                return Ok(None);
            }
            let cells: Vec<_> = queue.drain(..count).flatten().collect();
            if cells.iter().any(|cell| {
                let text = cell.text.as_bytes();
                text.is_empty()
                    || text.first() == Some(&b' ')
                    || text.last() == Some(&b' ')
                    || !text.iter().all(|byte| (0x20..=0x7e).contains(byte))
                    || text.windows(2).any(|pair| pair == b"  ")
            }) {
                return Ok(None);
            }
            if slots.len() != cells.len() {
                return Ok(None);
            }
            let shaped: Option<Vec<_>> = cells
                .par_iter()
                .zip(styles)
                .zip(slots)
                .with_min_len(24)
                .map_init(
                    || {
                        let mut doc = Document::new();
                        doc.set_font_collection(fonts.clone());
                        doc
                    },
                    |doc, ((cell, style), (_, _, width))| {
                        let shape = doc.shape_standalone_text(
                            &cell.text,
                            style,
                            Some(*width),
                            StandaloneAlign::Start,
                        )?;
                        (shape.lines().len() == 1 && shape.width() <= *width).then_some(shape)
                    },
                )
                .collect();
            let Some(shaped) = shaped else {
                return Ok(None);
            };
            let mut runs = originals.clone();
            for ((i, _, _), shape) in slots.iter().zip(&shaped) {
                let old = &originals[*i];
                let RunSource::Text(node) = old.source else {
                    return Ok(None);
                };
                let mut replacement = Vec::new();
                if !shape.append_cell_runs(
                    (0.0, 0.0),
                    old.color,
                    old.line.root,
                    node,
                    &mut replacement,
                ) || replacement.len() != 1
                {
                    return Ok(None);
                }
                let mut run = replacement.pop().unwrap();
                if run.font.id != old.font.id
                    || run.synthesis != old.synthesis
                    || run.ascent != old.ascent
                    || run.descent != old.descent
                    || run.normalized_coords != old.normalized_coords
                {
                    return Ok(None);
                }
                run.origin = old.origin;
                run.line = old.line;
                runs[*i] = run;
            }
            if self.painter.is_none() {
                self.start(Some(&page), self.document_url.clone())
                    .map_err(std::io::Error::other)?;
            }
            let targets = LinkTargets {
                base_url: Some(self.document_url),
                document_url: self.document_url,
                anchors: None,
            };
            let links = self
                .painter
                .as_mut()
                .unwrap()
                .page_with_runs(&page, &[], &targets, &runs)
                .map_err(std::io::Error::other)?;
            self.pages.push(PendingPage {
                page_box: page.geometry().page_box,
                margin_boxes: false,
                internal_links: links,
                space: RootSpace::of_page(&page),
            });
            consumed += count;
            index += 1;
        }
        Ok(Some(index))
    }

    fn discard_staged_table_pages(&mut self) {
        self.painter = None;
        self.lang = None;
        self.pages.clear();
        self.outline = self
            .config
            .effective_bookmarks()
            .then(OutlineBuilder::default);
    }

    fn finish(self, summary: StreamSummary) -> std::io::Result<Self::Output> {
        Ok(PdfSink::finish(self, summary))
    }
}
