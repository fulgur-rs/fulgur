//! Draw a Raikiri `DocumentLayout` into a PDF with Krilla.
//!
//! The painter reads Raikiri's pages directly: page geometry, the fragments
//! placed on each page, and the computed style of their nodes. Raikiri works
//! in CSS px with the origin at the top-left of the page box and y growing
//! downward, which is also Krilla's orientation, so each page surface is
//! scaled by [`PX_TO_PT`] once and everything below draws in px.

mod border;
mod clip;
mod decoration;
mod gradient;
mod margin;
pub(crate) mod navigation;
mod order;
mod raster;
mod shadow;
mod shape;
mod svg;
mod text_clip;

use crate::tagging::{Tags, Target};
use clip::{ClipMap, ClipStack};
use fulgur_core::units::PX_TO_PT;
use fulgur_core::{Error, Result};
use krilla::annotation::Annotation;
use krilla::color::rgb;
use krilla::geom::{Path, Transform};
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{Fill, FillRule};
use krilla::surface::Surface;
use krilla::tagging::ArtifactType;
use navigation::PageLink;
use raikiri_html::computed::{
    ComputedBackgroundImage, ComputedLengthPercentage, ComputedValues, ComputedVisibility,
    ComputedVisualBox, CssColor,
};
use raikiri_html::{
    AnchorIndex, DocumentLayout, FontId, FragmentKind, MarginBox, NodeId, NodeKind, Page,
    PaintEvent, PaintRect, PositionedGlyphRun, RunSource, TextShadow,
};
use shape::{Edges, RoundedRect, Slice};
use std::collections::HashMap;

/// Draw every page of `document` and return the PDF bytes.
pub(crate) fn paint_document(
    document: &DocumentLayout,
    resources: &raikiri_html::RenderResources<'_>,
    config: &fulgur_core::Config,
    outline: Option<krilla::outline::Outline>,
    document_url: &url::Url,
    options: &crate::RenderOptions<'_>,
) -> Result<Vec<u8>> {
    let first_page = document.page(0);
    let lang = document_lang(config, first_page.as_ref());
    if config.pdf_ua && lang.is_none() {
        return Err(Error::PdfGeneration(
            "PDF/UA requires a document language: set Config::lang or <html lang>".into(),
        ));
    }
    let tags = Tags::new(document, config.effective_tagging());
    // Relative `url()` values resolve against the base Raikiri preloaded them
    // with: the document's `<base href>`, else its own URL.
    let base = document.base_url().unwrap_or(document_url);
    let mut painter = Painter::new(resources, config, options, base.clone(), tags);
    painter.set_metadata(crate::metadata::build(
        config,
        first_page.as_ref().and_then(html_title),
        lang.clone(),
    )?);
    if let Some(outline) = outline {
        painter.set_outline(outline);
    }
    let targets = LinkTargets {
        base_url: document.base_url(),
        document_url,
        anchors: Some(document.anchors()),
    };
    for page in document.pages() {
        painter.page(&page, &page.margin_boxes(), &targets)?;
    }
    painter.finish(lang)
}

/// The document language: the configured one unless blank, else the `lang`
/// of the root `<html>` element of `page`.
pub(crate) fn document_lang(
    config: &fulgur_core::Config,
    page: Option<&Page<'_>>,
) -> Option<String> {
    // A blank configured language counts as unset.
    config
        .lang
        .clone()
        .filter(|lang| !lang.trim().is_empty())
        .or_else(|| page.and_then(html_lang))
}

/// The trimmed text of the `<title>` in the document's `<head>`, if any.
pub(crate) fn html_title(page: &Page<'_>) -> Option<String> {
    let dom = page.dom();
    let element = |node: NodeId, name: &str| {
        dom.kind(node) == Some(NodeKind::Element) && dom.local_name(node) == Some(name)
    };
    let html = dom
        .children(dom.root())
        .find(|&node| element(node, "html"))?;
    let head = dom.children(html).find(|&node| element(node, "head"))?;
    let title = dom.children(head).find(|&node| element(node, "title"))?;
    Some(dom.text_content(title).trim().to_owned()).filter(|text| !text.is_empty())
}

/// The `lang` of the root `<html>` element, if set and non-empty.
fn html_lang(page: &Page<'_>) -> Option<String> {
    let dom = page.dom();
    let html = dom.children(dom.root()).find(|&node| {
        dom.kind(node) == Some(NodeKind::Element) && dom.local_name(node) == Some("html")
    })?;
    Some(dom.attr(html, "lang")?.trim().to_owned()).filter(|lang| !lang.is_empty())
}

/// What the links of a page resolve against.
pub(crate) struct LinkTargets<'u> {
    /// Resolves relative link targets.
    pub(crate) base_url: Option<&'u url::Url>,
    /// A target naming this URL with a fragment is an internal link.
    pub(crate) document_url: &'u url::Url,
    /// Destinations of internal links, `None` while they are not known yet.
    pub(crate) anchors: Option<&'u AnchorIndex>,
}

/// A PDF being written page by page.
pub(crate) struct Painter<'a> {
    pdf: krilla::Document,
    fonts: FontCache,
    svg: svg::SvgCache<'a>,
    raster: raster::RasterCache<'a>,
    tags: Tags,
}

impl<'a> Painter<'a> {
    /// Start a PDF, tagged when `tags` is enabled. Relative image URLs
    /// resolve against `base`.
    pub(crate) fn new(
        resources: &'a raikiri_html::RenderResources<'_>,
        config: &fulgur_core::Config,
        options: &crate::RenderOptions<'a>,
        base: url::Url,
        tags: Tags,
    ) -> Self {
        let pdf = if tags.enabled() {
            let configuration = if config.pdf_ua {
                krilla::configure::Configuration::new_with_validator(
                    krilla::configure::Validator::UA1,
                )
            } else {
                krilla::configure::Configuration::new()
            };
            krilla::Document::new_with(krilla::SerializeSettings {
                enable_tagging: true,
                configuration,
                ..Default::default()
            })
        } else {
            krilla::Document::new()
        };
        Self {
            pdf,
            fonts: FontCache::default(),
            svg: svg::SvgCache::new(*options),
            raster: raster::RasterCache::new(resources.image_pixel_source_ref(), base),
            tags,
        }
    }

    pub(crate) fn set_metadata(&mut self, metadata: krilla::metadata::Metadata) {
        self.pdf.set_metadata(metadata);
    }

    pub(crate) fn set_outline(&mut self, outline: krilla::outline::Outline) {
        self.pdf.set_outline(outline);
    }

    /// Draw `page` with `margin_boxes` below its body, and add its links.
    ///
    /// Returns the internal links left out because `targets` has no anchors
    /// yet, placed on the page.
    pub(crate) fn page(
        &mut self,
        page: &Page<'_>,
        margin_boxes: &[MarginBox],
        targets: &LinkTargets<'_>,
    ) -> Result<Vec<PageLink>> {
        let Self {
            pdf,
            fonts,
            svg,
            raster,
            tags,
        } = self;
        let mut pdf_page = start_page(pdf, page.geometry().page_box)?;
        let mut surface = pdf_page.surface();
        surface.push_transform(&Transform::from_scale(PX_TO_PT, PX_TO_PT));

        tags.begin_page(page);
        // The margin boxes are drawn before the page body.
        let running = margin::paint(
            &mut surface,
            Some(page),
            margin_boxes,
            fonts,
            svg,
            raster,
            tags,
        )?;
        paint_body(&mut surface, page, fonts, svg, raster, tags)?;

        surface.pop();
        surface.finish();
        let mut pending = Vec::new();
        for link in navigation::links(page, navigation::Placement::PAGE, targets) {
            match link.annotation(targets.anchors) {
                Some(annotation) => tags.annotate(&mut pdf_page, page, link.owner, annotation),
                None if link.is_pending(targets.anchors) => pending.push(link),
                None => {}
            }
        }
        // Links inside the running elements drawn in margin boxes, clipped like
        // their content to the box.
        for (running, border_box) in running {
            let placement = navigation::Placement {
                origin: running.origin,
                clip: Some(border_box),
            };
            let running_page = running.layout.page();
            for link in navigation::links(&running_page, placement, targets) {
                match link.annotation(targets.anchors) {
                    Some(annotation) => {
                        tags.annotate_artifact(&mut pdf_page, &running_page, link.owner, annotation)
                    }
                    None if link.is_pending(targets.anchors) => pending.push(link),
                    None => {}
                }
            }
        }
        tags.end_page();
        pdf_page.finish();
        Ok(pending)
    }

    /// Add an untagged page of `page_box` size that holds only `margin_boxes`
    /// and the internal `links` that resolve against `anchors`.
    ///
    /// The boxes are drawn without running elements, which need the page
    /// they belong to, so a box showing one falls back to its text.
    pub(crate) fn margin_page(
        &mut self,
        page_box: PaintRect,
        margin_boxes: &[MarginBox],
        links: &[PageLink],
        anchors: &AnchorIndex,
    ) -> Result<()> {
        let mut pdf_page = start_page(&mut self.pdf, page_box)?;
        let mut surface = pdf_page.surface();
        surface.push_transform(&Transform::from_scale(PX_TO_PT, PX_TO_PT));
        margin::paint(
            &mut surface,
            None,
            margin_boxes,
            &mut self.fonts,
            &mut self.svg,
            &mut self.raster,
            &mut Tags::disabled(),
        )?;
        surface.pop();
        surface.finish();
        for link in links {
            if let Some(annotation) = link.annotation(Some(anchors)) {
                pdf_page.add_annotation(Annotation::new_link(annotation, None));
            }
        }
        pdf_page.finish();
        Ok(())
    }

    /// Finish the PDF, with the structure tree in `lang` when tagged, and
    /// return its bytes.
    pub(crate) fn finish(mut self, lang: Option<String>) -> Result<Vec<u8>> {
        if let Some(tree) = self.tags.finish(lang) {
            self.pdf.set_tag_tree(tree);
        }
        self.pdf
            .finish()
            .map_err(|error| Error::PdfGeneration(format!("{error:?}")))
    }
}

/// Start a PDF page of `page_box` size, given in CSS px.
fn start_page(pdf: &mut krilla::Document, page_box: PaintRect) -> Result<krilla::page::Page<'_>> {
    let settings = PageSettings::from_wh(page_box.width * PX_TO_PT, page_box.height * PX_TO_PT)
        .ok_or_else(|| Error::PdfGeneration("Invalid page dimensions".into()))?;
    Ok(pdf.start_page_with(settings))
}

/// Draw the boxes, text and replaced content of `page`, without its margin
/// boxes. A running element laid out for a margin box is drawn the same way.
fn paint_body(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    fonts: &mut FontCache,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
    tags: &mut Tags,
) -> Result<()> {
    let runs = page.text_runs();
    let events = page.paint_order_for_text_runs(&runs);
    if order::supported(&events, &runs) {
        paint_ordered(surface, page, &events, &runs, fonts, svg, raster, tags)
    } else {
        paint_legacy(surface, page, &runs, fonts, svg, raster, tags)
    }
}

fn paint_legacy(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    runs: &[PositionedGlyphRun<'_>],
    fonts: &mut FontCache,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
    tags: &mut Tags,
) -> Result<()> {
    let clips = ClipMap::new(page);
    let mut active = ClipStack::default();
    let dom = page.dom();

    // Keep the existing whole-page rendering when text events cannot map
    // every text run to exactly one draw step.
    let mut boxes: Vec<_> = page
        .fragments()
        .filter(|fragment| fragment.kind() == FragmentKind::Box)
        .collect();
    boxes.sort_by_key(|fragment| (fragment.node(), fragment.fragment_index()));
    for fragment in boxes {
        if let Some(style) = page.computed(fragment.node()) {
            // A box's own overflow clips its content, not its decorations.
            let chain = clips.chain(page, dom.parent(fragment.node()), fragment.paint_rect());
            active.apply(surface, &clips, &chain);
            let text = || text_clip::outlines(dom, fragment.node(), runs);
            tags.mark(
                surface,
                page,
                Target::Artifact(ArtifactType::Other),
                |surface| {
                    paint_box(
                        surface,
                        raster,
                        fragment.paint_rect(),
                        fragment.content_rect(),
                        Slice::of(&fragment),
                        style,
                        text,
                    )
                },
            )?;
        }
    }
    for fragment in page.fragments() {
        let chain = clips.chain(page, Some(fragment.node()), fragment.paint_rect());
        active.apply(surface, &clips, &chain);
        // This painter has no opacity groups, so the SVG keeps its root opacity.
        paint_replaced(surface, page, &fragment, svg, raster, tags, false)?;
    }
    // Text goes above every block background and border.
    let text: Vec<_> = runs.iter().collect();
    paint_text_batch(surface, page, &clips, &mut active, &text, fonts, tags);
    active.clear(surface);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn paint_ordered(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    events: &[PaintEvent<'_>],
    runs: &[PositionedGlyphRun<'_>],
    fonts: &mut FontCache,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
    tags: &mut Tags,
) -> Result<()> {
    // Ordered painting consumes every producer clip directly. The legacy
    // DOM-based clip lookup would apply the same shape twice and cannot
    // identify repeated column placements.
    let clips = ClipMap::default();
    let mut active = ClipStack::default();
    let dom = page.dom();
    let mut by_node: HashMap<_, Vec<_>> = HashMap::new();
    let mut by_line: HashMap<_, Vec<_>> = HashMap::new();
    for run in runs {
        by_line.entry(run.line).or_default().push(run);
        if let RunSource::Text(node) = run.source {
            by_node.entry(node).or_default().push(run);
        }
    }
    let mut batch = Vec::new();
    let mut event_clips = Vec::new();
    for event in events {
        if let PaintEvent::Text(fragment) = event {
            batch.extend(by_node.get(&fragment.node()).into_iter().flatten().copied());
            continue;
        }
        if let PaintEvent::TextLine(line) = event {
            batch.extend(by_line.get(line).into_iter().flatten().copied());
            continue;
        }
        // Clip, opacity, box and stacking steps delimit a text paint batch.
        paint_text_batch(surface, page, &clips, &mut active, &batch, fonts, tags);
        batch.clear();
        match event {
            PaintEvent::Box(fragment) => {
                if let Some(style) = page.computed(fragment.node()) {
                    let chain =
                        clips.chain(page, dom.parent(fragment.node()), fragment.paint_rect());
                    active.apply(surface, &clips, &chain);
                    let text = || text_clip::outlines(dom, fragment.node(), runs);
                    tags.mark(
                        surface,
                        page,
                        Target::Artifact(ArtifactType::Other),
                        |surface| {
                            paint_box(
                                surface,
                                raster,
                                fragment.paint_rect(),
                                fragment.content_rect(),
                                Slice::of(fragment),
                                style,
                                text,
                            )
                        },
                    )?;
                }
            }
            PaintEvent::GeneratedBox(piece) => {
                let chain = clips.chain(page, Some(piece.clip_owner), piece.rect);
                active.apply(surface, &clips, &chain);
                let text = || text_clip::generated_outlines(piece, runs);
                tags.mark(
                    surface,
                    page,
                    Target::Artifact(ArtifactType::Other),
                    |surface| {
                        paint_box(
                            surface,
                            raster,
                            piece.rect,
                            None,
                            Slice::generated(piece),
                            piece.style,
                            text,
                        )
                    },
                )?;
            }
            PaintEvent::Replaced(fragment) => {
                let chain = clips.chain(page, Some(fragment.node()), fragment.paint_rect());
                active.apply(surface, &clips, &chain);
                paint_replaced(surface, page, fragment, svg, raster, tags, true)?;
            }
            PaintEvent::ColumnRule(rule) => {
                active.clear(surface);
                tags.mark(
                    surface,
                    page,
                    Target::Artifact(ArtifactType::Other),
                    |surface| border::paint_column_rule(surface, rule),
                );
            }
            PaintEvent::MarkerImage(owner) => {
                if let Some(placement) = raster.marker(page, *owner) {
                    let chain = clips.chain(page, placement.clip_owner, placement.rect);
                    active.apply(surface, &clips, &chain);
                    tags.mark(surface, page, Target::MarkerImage(*owner), |surface| {
                        raster.paint_placement(surface, page, &placement, true)
                    })?;
                }
            }
            PaintEvent::PushClip(shape, _) => {
                // The producer owns column placement and clipping geometry.
                let path = clip::clip_path(*shape, page.geometry().page_box);
                if let Some(path) = &path {
                    active.clear(surface);
                    surface.push_clip_path(path, &FillRule::NonZero);
                }
                event_clips.push(path.is_some());
            }
            PaintEvent::PopClip => {
                if event_clips.pop() == Some(true) {
                    active.clear(surface);
                    surface.pop();
                }
            }
            PaintEvent::PushOpacity(alpha) => {
                // Clips and opacity groups share the surface stack. Close clips
                // at each group boundary and reapply them on the next draw.
                active.clear(surface);
                if let Some(alpha) = NormalizedF32::new(*alpha) {
                    surface.push_opacity(alpha);
                }
            }
            PaintEvent::PopOpacity => {
                active.clear(surface);
                surface.pop();
            }
            // Clips are applied lazily before each content draw.
            // Other events have already been rejected by `order::supported`.
            _ => {}
        }
    }
    paint_text_batch(surface, page, &clips, &mut active, &batch, fonts, tags);
    active.clear(surface);
    Ok(())
}

/// The content of a replaced element, marked as that element's content.
fn paint_replaced(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    fragment: &raikiri_html::Fragment<'_>,
    svg: &mut svg::SvgCache<'_>,
    raster: &mut raster::RasterCache<'_>,
    tags: &mut Tags,
    host_group: bool,
) -> Result<()> {
    let mut draw = |surface: &mut Surface<'_>| {
        svg.paint(surface, page, fragment, host_group)?;
        raster.paint(surface, page, fragment, host_group)
    };
    // Other fragments draw nothing here, so they need no marked content.
    if fragment.kind() != FragmentKind::Replaced {
        return draw(surface);
    }
    tags.mark(surface, page, Target::Replaced(fragment.node()), draw)
}

/// Keep the line's decorations below or above all neighboring glyph ink,
/// and the line's text shadows below both.
fn paint_text_batch(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    clips: &ClipMap,
    active: &mut ClipStack,
    runs: &[&PositionedGlyphRun<'_>],
    fonts: &mut FontCache,
    tags: &mut Tags,
) {
    let mut indices = HashMap::new();
    let mut lines: Vec<Vec<&PositionedGlyphRun<'_>>> = Vec::new();
    for &run in runs {
        let index = *indices.entry(run.line).or_insert_with(|| {
            lines.push(Vec::new());
            lines.len() - 1
        });
        lines[index].push(run);
    }
    // Reunite color and font slices of each line before painting its three
    // phases, without reordering independent lines that overlap on the page.
    for runs in lines {
        // The shadows are below the line's decorations and glyphs. Each
        // shadow layer is painted across the whole line before the next
        // one, so a run's first shadow is not covered by the later shadows
        // of the runs after it.
        let layers = runs.iter().map(|run| run.shadows.len()).max().unwrap_or(0);
        for layer in (0..layers).rev() {
            paint_shadow_layer(
                surface,
                page,
                clips,
                active,
                &runs,
                layer,
                &mut fonts.shadows,
                tags,
            );
        }
        for phase in [
            Some(decoration::Phase::BeforeGlyphs),
            None,
            Some(decoration::Phase::AfterGlyphs),
        ] {
            for run in &runs {
                let chain = run_clip_chain(page, clips, run);
                active.apply(surface, clips, &chain);
                if let Some(phase) = phase {
                    if !run.decorations.is_empty() {
                        tags.mark(
                            surface,
                            page,
                            Target::Artifact(ArtifactType::Other),
                            |surface| decoration::paint(surface, &run.decorations, phase),
                        );
                    }
                } else if paints_glyphs(run) || (tags.enabled() && has_glyphs(run)) {
                    // Transparent text is still content: tagged output
                    // draws it at zero opacity so that it stays readable.
                    tags.mark(surface, page, Target::Run(run), |surface| {
                        paint_glyph_run(surface, run, fonts)
                    });
                }
            }
        }
    }
}

/// The clips that apply to the text of `run`.
fn run_clip_chain(
    page: &Page<'_>,
    clips: &ClipMap,
    run: &PositionedGlyphRun<'_>,
) -> Vec<clip::ClipKey> {
    let dom = page.dom();
    let element = text_clip::run_element(dom, run);
    // Standalone markers precede the item's own overflow clip.
    let clip_owner = if run.is_standalone_marker() {
        element.and_then(|node| dom.parent(node))
    } else {
        element
    };
    let area = PaintRect::new(
        run.origin.0,
        run.origin.1 - run.ascent,
        run.advance,
        run.ascent + run.descent,
    );
    clips.chain(page, clip_owner, area)
}

/// Paint the shadow at `layer` of each run of one line. Neighboring runs
/// with the same shadow and clips are painted as one shape, so a blurred
/// shadow has no seam where a line changes font or color.
#[allow(clippy::too_many_arguments)]
fn paint_shadow_layer(
    surface: &mut Surface<'_>,
    page: &Page<'_>,
    clips: &ClipMap,
    active: &mut ClipStack,
    runs: &[&PositionedGlyphRun<'_>],
    layer: usize,
    budget: &mut shadow::Budget,
    tags: &mut Tags,
) {
    let mut group: Vec<&PositionedGlyphRun<'_>> = Vec::new();
    let mut current: Option<(TextShadow, Vec<clip::ClipKey>)> = None;
    for &run in runs {
        let next = run
            .shadows
            .get(layer)
            .map(|shadow| (*shadow, run_clip_chain(page, clips, run)));
        if next != current {
            if let Some((shadow, chain)) = current.take() {
                active.apply(surface, clips, &chain);
                tags.mark(
                    surface,
                    page,
                    Target::Artifact(ArtifactType::Other),
                    |surface| shadow::paint(surface, &group, &shadow, budget),
                );
            }
            group.clear();
            current = next;
        }
        if current.is_some() {
            group.push(run);
        }
    }
    if let Some((shadow, chain)) = current {
        active.apply(surface, clips, &chain);
        tags.mark(
            surface,
            page,
            Target::Artifact(ArtifactType::Other),
            |surface| shadow::paint(surface, &group, &shadow, budget),
        );
    }
}

/// Background and borders of one box fragment (`rect` is its border box,
/// `content` the whole element's content box from layout, if known).
///
/// The fragment is drawn as its own box, apart from its broken edges: the
/// corner radii (percentages and the §5.5 scaling) and the background
/// positioning area refer to the fragment, because the fragment API does
/// not give the size of the unbroken box or the fragment's offset in it.
/// `text` gives the outlines of the element's text, for
/// `background-clip: text`.
fn paint_box(
    surface: &mut Surface<'_>,
    raster: &mut raster::RasterCache<'_>,
    rect: PaintRect,
    content: Option<PaintRect>,
    slice: Slice,
    style: &ComputedValues,
    text: impl FnOnce() -> Option<Path>,
) -> Result<()> {
    if !matches!(style.visibility, ComputedVisibility::Visible) {
        return Ok(());
    }
    let border_box = RoundedRect::border_box(rect, &style.border_radius).sliced(slice);
    let padding = padding(style, rect, content);
    paint_background(surface, raster, &border_box, padding, slice, style, text)?;
    border::paint_borders(surface, &border_box, style, slice);
    Ok(())
}

/// CSS Backgrounds 3 §2: the background color, then the background image
/// over it, both clipped to the `background-clip` box with its corner
/// curves (§5.3). CSS Backgrounds 4 §2.6 adds two painting areas:
/// `border-area` (the area under the border) and `text` (the glyphs of the
/// element's text).
fn paint_background(
    surface: &mut Surface<'_>,
    raster: &mut raster::RasterCache<'_>,
    border_box: &RoundedRect,
    padding: Edges,
    slice: Slice,
    style: &ComputedValues,
    text: impl FnOnce() -> Option<Path>,
) -> Result<()> {
    let borders = slice.edges(border::widths(style));
    let padding = slice.edges(padding);
    let visual_box = |visual: ComputedVisualBox| match visual {
        ComputedVisualBox::PaddingBox => border_box.inset(borders),
        ComputedVisualBox::ContentBox => border_box.inset(borders.add(padding)),
        // `border-box`, and the boxes this painter does not distinguish.
        _ => *border_box,
    };
    let painting_area = match style.background_clip {
        ComputedVisualBox::BorderArea => shape::ring(border_box, &border_box.inset(borders))
            .map(|path| (path, FillRule::EvenOdd)),
        ComputedVisualBox::Text => text().map(|path| (path, FillRule::NonZero)),
        visual => visual_box(visual)
            .path()
            .map(|path| (path, FillRule::NonZero)),
    };
    let Some((area, rule)) = painting_area else {
        return Ok(());
    };
    if style.background_color.a > 0 {
        surface.set_fill(Some(Fill {
            rule,
            ..fill(style.background_color)
        }));
        surface.draw_path(&area);
    }
    let positioning_area = visual_box(style.background_origin);
    match &style.background_image {
        // One layer with the initial size, position and repeat: the gradient
        // covers the positioning area and its end colors extend to the rest
        // of the painting area.
        ComputedBackgroundImage::Gradient(gradient) => {
            if let Some(paint) = gradient::paint(gradient, &positioning_area, style.color) {
                surface.set_fill(Some(Fill {
                    paint,
                    opacity: NormalizedF32::ONE,
                    rule,
                }));
                surface.draw_path(&area);
            }
        }
        ComputedBackgroundImage::Url(url) => {
            // Tiles cover the clip box; `border-area` and `text` clip inside
            // the border box.
            let painting = match style.background_clip {
                ComputedVisualBox::BorderArea | ComputedVisualBox::Text => *border_box,
                visual => visual_box(visual),
            };
            let layer = raster::BackgroundLayer {
                url,
                size: &style.background_size,
                position: &style.background_position,
                repeat: &style.background_repeat,
            };
            raster.paint_background(
                surface,
                &layer,
                positioning_area.bounds(),
                painting.bounds(),
                (&area, rule),
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// Used padding widths of the box whose border box is `rect`.
///
/// A percentage refers to the containing block's width (CSS Box 4 §4),
/// which the fragment does not carry. The layout's `content` box already
/// subtracts the resolved padding, so a percentage side is read back as the
/// gap between the border box, less its border, and the content box. The
/// content box covers the whole element before page cuts in the same
/// page-local coordinates as `rect`, so the top and bottom gaps are only
/// meaningful on the fragments that hold those edges; the broken edges are
/// zeroed by the caller's slice anyway. Without a content box (generated
/// pieces, inline boxes split over several lines) a percentage counts as
/// zero.
fn padding(style: &ComputedValues, rect: PaintRect, content: Option<PaintRect>) -> Edges {
    let borders = border::widths(style);
    let used = content.map(|content| Edges {
        top: content.y - rect.y - borders.top,
        right: rect.x + rect.width - borders.right - (content.x + content.width),
        bottom: rect.y + rect.height - borders.bottom - (content.y + content.height),
        left: content.x - rect.x - borders.left,
    });
    let px = |value: ComputedLengthPercentage, used: Option<f32>| match value {
        ComputedLengthPercentage::Px(px) => px.max(0.0),
        ComputedLengthPercentage::Percent(_) => used.unwrap_or(0.0).max(0.0),
    };
    Edges {
        top: px(style.padding.top, used.map(|edges| edges.top)),
        right: px(style.padding.right, used.map(|edges| edges.right)),
        bottom: px(style.padding.bottom, used.map(|edges| edges.bottom)),
        left: px(style.padding.left, used.map(|edges| edges.left)),
    }
}

pub(crate) fn fill(color: CssColor) -> Fill {
    Fill {
        paint: rgb::Color::new(color.r, color.g, color.b).into(),
        opacity: NormalizedF32::new(f32::from(color.a) / 255.0).unwrap_or(NormalizedF32::ONE),
        rule: Default::default(),
    }
}

/// Krilla fonts by Raikiri face and variation coordinates. `Font::new`
/// parses the font, so each face is created once per document and every run
/// that uses it shares the same PDF font object.
///
/// It also holds the raster budget of the document's blurred text shadows.
#[derive(Default)]
struct FontCache {
    fonts: HashMap<FontKey, Option<krilla::text::Font>>,
    shadows: shadow::Budget,
}

/// A face plus its variation coordinates (axis tag, value bits).
type FontKey = (FontId, Vec<([u8; 4], u32)>);

impl FontCache {
    fn font(&mut self, run: &PositionedGlyphRun<'_>) -> Option<krilla::text::Font> {
        let variations: Vec<([u8; 4], u32)> = run
            .variations
            .iter()
            .map(|variation| (variation.tag.0, variation.value.to_bits()))
            .collect();
        self.fonts
            .entry((run.font.id, variations))
            .or_insert_with(|| {
                let data: krilla::Data = run.font.data.to_arc().into();
                let coords: Vec<(krilla::text::Tag, f32)> = run
                    .variations
                    .iter()
                    .map(|variation| (krilla::text::Tag::new(&variation.tag.0), variation.value))
                    .collect();
                // An instance with no coordinates is the font's default.
                krilla::text::Font::new_variable(data, run.font.index, &coords)
            })
            .clone()
    }
}

/// Whether `run` paints visible glyphs.
fn paints_glyphs(run: &PositionedGlyphRun<'_>) -> bool {
    has_glyphs(run) && run.color.a != 0
}

/// Whether `run` has glyphs to draw, whatever its color.
fn has_glyphs(run: &PositionedGlyphRun<'_>) -> bool {
    !run.glyphs.is_empty() && run.font_size > 0.0
}

/// Draw one positioned glyph run.
///
/// Raikiri reports glyph advances and offsets in px with y growing downward;
/// Krilla takes them per unit of font size and subtracts `y_offset`, so both
/// are divided by the font size and `y_offset` changes sign.
fn paint_glyph_run(surface: &mut Surface<'_>, run: &PositionedGlyphRun<'_>, fonts: &mut FontCache) {
    if !has_glyphs(run) {
        return;
    }
    // A face Krilla cannot parse is skipped rather than failing the page.
    let Some(font) = fonts.font(run) else { return };
    let size = run.font_size;
    let glyphs: Vec<krilla::text::KrillaGlyph> = run
        .glyphs
        .iter()
        .map(|glyph| krilla::text::KrillaGlyph {
            glyph_id: krilla::text::GlyphId::new(glyph.id),
            text_range: glyph.text_range.clone(),
            x_advance: glyph.advance / size,
            x_offset: glyph.x_offset / size,
            y_offset: -glyph.y_offset / size,
            y_advance: 0.0,
            location: None,
        })
        .collect();
    surface.set_fill(Some(fill(run.color)));
    surface.draw_glyphs(
        krilla::geom::Point::from_xy(run.origin.0, run.origin.1),
        &glyphs,
        font,
        run.text,
        size,
        false,
    );
}

#[cfg(test)]
mod tests;
