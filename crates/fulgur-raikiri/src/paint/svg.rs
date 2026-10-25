//! Inline SVG vector content inside the existing page paint stack.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::sync::Arc;

use fulgur_core::{Error, Result};
use krilla::geom::{Rect, Size, Transform};
use krilla::num::NormalizedF32;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use krilla_svg::{SurfaceExt, SvgSettings};
use raikiri_html::{Fragment, Page, SvgRendering};
use raikiri_svg::{SvgDocument, SvgResources, SvgRootStyle, SvgViewport};
use raikiri_traits::vector::VectorImage;

use crate::RenderOptions;

mod raster;
pub(super) mod scene;

#[cfg(test)]
mod tests;

/// Upper bound on the SVG source bytes whose parsed trees are kept.
const MAX_CACHED_SOURCE_BYTES: usize = 32 * 1024 * 1024;

pub(super) struct SvgCache<'a> {
    options: RenderOptions<'a>,
    fonts: Option<Arc<usvg::fontdb::Database>>,
    no_fonts: Arc<usvg::fontdb::Database>,
    /// Parsed trees by prepared source, then by the host opacity removed from
    /// the root (its bits), so an SVG repeated on every page, such as a logo
    /// in a running header, is parsed once more and then reused.
    trees: HashMap<String, Vec<(Option<u32>, usvg::Tree)>>,
    /// Hashes of the (source, removed opacity) pairs drawn once. A tree is
    /// kept only when its pair is drawn again, so one-off SVGs never pile up.
    seen: HashSet<u64>,
    hasher: RandomState,
    cached_source_bytes: usize,
    /// The last tree parsed but not kept.
    uncached: Option<usvg::Tree>,
    /// Vector scenes of the sources that Raikiri can draw itself, which take
    /// the place of the trees above for them.
    scenes: scene::SceneMemo,
    /// Pixels of the SVGs whose rendering is `raster`.
    rasters: raster::RasterMemo,
    /// Fonts and pictures of those scenes converted for Krilla.
    scene_assets: scene::SceneAssets,
}

impl<'a> SvgCache<'a> {
    pub(super) fn new(options: RenderOptions<'a>) -> Self {
        Self {
            options,
            fonts: None,
            no_fonts: Arc::new(usvg::fontdb::Database::new()),
            trees: HashMap::new(),
            seen: HashSet::new(),
            hasher: RandomState::new(),
            cached_source_bytes: 0,
            uncached: None,
            scenes: scene::SceneMemo::default(),
            rasters: raster::RasterMemo::default(),
            scene_assets: scene::SceneAssets::default(),
        }
    }

    fn fonts(&mut self) -> Arc<usvg::fontdb::Database> {
        self.fonts
            .get_or_insert_with(|| {
                let mut database = usvg::fontdb::Database::new();
                if let Some(bundle) = self.options.assets {
                    for bytes in &bundle.fonts {
                        database.load_font_data(bytes.to_vec());
                    }
                }
                let bundled_family = database
                    .faces()
                    .next()
                    .and_then(|face| face.families.first())
                    .map(|(name, _)| name.clone());
                if self.options.system_fonts {
                    database.load_system_fonts();
                }
                // HTML text maps every generic family to the bundle, so SVG
                // text must not fall back to host defaults for any of them.
                if let Some(family) = bundled_family {
                    database.set_serif_family(family.clone());
                    database.set_sans_serif_family(family.clone());
                    database.set_cursive_family(family.clone());
                    database.set_fantasy_family(family.clone());
                    database.set_monospace_family(family);
                }
                Arc::new(database)
            })
            .clone()
    }

    /// Draws the fragment's inline SVG. Pass `host_group` when the enclosing
    /// paint events composite the host opacity; otherwise the SVG keeps its
    /// root opacity.
    pub(super) fn paint(
        &mut self,
        surface: &mut Surface<'_>,
        page: &Page<'_>,
        fragment: &Fragment<'_>,
        host_group: bool,
    ) -> Result<()> {
        let Some(svg) = page
            .inline_svg(fragment)
            .map_err(|error| Error::Layout(format!("inline SVG: {error}")))?
        else {
            return Ok(());
        };
        let neutralized = match svg.host_opacity {
            Some(alpha) if host_group => Some(alpha),
            _ => None,
        };
        let area = fragment.paint_rect();
        let size = Size::from_wh(svg.viewport.width, svg.viewport.height)
            .ok_or_else(|| Error::PdfGeneration("invalid SVG viewport".into()))?;
        // Keep the original viewport while clipping this page's fragment slice.
        let clip_path = rect_path(area.x, area.y, area.width, area.height, "fragment clip")?;
        // An element that asks for pixels gets the picture of the CSS pixel
        // grid at the layout's raster scale.
        let raster = match page.svg_rendering(fragment.node()) {
            SvgRendering::Raster => {
                Some(
                    self.rasters
                        .get_or_build(page, &svg, size, page.svg_raster_scale())?,
                )
            }
            _ => None,
        };
        // The scene holds what Raikiri draws itself; anything it leaves to a
        // raster, or that Krilla could not stroke faithfully, goes through the
        // parsed tree like before.
        let scene = match raster {
            Some(_) => None,
            None => self.scene(page, &svg.source, size, svg.host_opacity)?,
        };
        // The scene is placed in its viewport box and clipped to it, as a
        // `preserveAspectRatio` slice needs; so is the raster, which is
        // rounded up to whole device pixels.
        let viewport = match (&raster, &scene) {
            (None, None) => None,
            _ => Some(rect_path(
                0.0,
                0.0,
                size.width(),
                size.height(),
                "viewport",
            )?),
        };
        surface.push_clip_path(&clip_path, &FillRule::NonZero);
        surface.push_transform(&Transform::from_translate(svg.viewport.x, svg.viewport.y));
        // Neither a scene nor a raster holds the host's opacity. When the
        // page's paint events do not composite it, the SVG keeps it.
        let own_opacity = svg.host_opacity.filter(|_| !host_group);
        let result = match (raster, scene, viewport) {
            (Some(raster), _, Some(viewport)) => {
                surface.push_clip_path(&viewport, &FillRule::NonZero);
                if let Some(alpha) = own_opacity {
                    surface.push_opacity(NormalizedF32::new(alpha).unwrap_or(NormalizedF32::ONE));
                }
                surface.draw_image(raster.image(), raster.size());
                if own_opacity.is_some() {
                    surface.pop();
                }
                surface.pop();
                Ok(())
            }
            (None, Some(scene), Some(viewport)) => {
                surface.push_clip_path(&viewport, &FillRule::NonZero);
                if let Some(alpha) = own_opacity {
                    surface.push_opacity(NormalizedF32::new(alpha).unwrap_or(NormalizedF32::ONE));
                }
                let result = scene::paint(surface, &scene, &mut self.scene_assets);
                if own_opacity.is_some() {
                    surface.pop();
                }
                surface.pop();
                result
            }
            _ => {
                let tree = self.tree(svg.source, neutralized)?;
                surface
                    .draw_svg(tree, size, SvgSettings::default())
                    .ok_or_else(|| Error::PdfGeneration("SVG vector drawing failed".into()))
            }
        };
        surface.pop();
        surface.pop();
        result
    }

    /// The vector scene of `source` placed in `size`, set in the fonts of
    /// `page`, or `None` when it has to be drawn another way.
    fn scene(
        &mut self,
        page: &Page<'_>,
        source: &str,
        size: Size,
        host_opacity: Option<f32>,
    ) -> Result<Option<Arc<VectorImage>>> {
        let built = self
            .scenes
            .get_or_build(source, host_opacity.map(f32::to_bits), || {
                let fonts = page.svg_fonts();
                SvgDocument::parse(source.as_bytes())?.scene_with_resources(
                    SvgViewport {
                        width: size.width(),
                        height: size.height(),
                    },
                    SvgRootStyle {
                        opacity: host_opacity.unwrap_or(1.0),
                        ..SvgRootStyle::default()
                    },
                    None,
                    SvgResources::new().with_fonts(&fonts),
                )
            });
        match built {
            Ok(scene) if scene::paintable(&scene) => Ok(Some(scene)),
            Ok(_) => Ok(None),
            Err(error) if error.falls_back_to_raster() => Ok(None),
            Err(error) => Err(Error::Layout(format!("inline SVG vector scene: {error}"))),
        }
    }

    /// The parsed tree of `source`, with the root opacity `neutralized`
    /// removed when given.
    fn tree(&mut self, source: String, neutralized: Option<f32>) -> Result<&usvg::Tree> {
        let key = neutralized.map(f32::to_bits);
        let cached = self
            .trees
            .get(&source)
            .and_then(|trees| trees.iter().position(|(alpha, _)| *alpha == key));
        if let Some(index) = cached {
            return Ok(&self.trees[&source][index].1);
        }
        // Scanning fonts is costly and only text needs them.
        let fontdb = if source.contains("<text") || source.contains(":text") {
            self.fonts()
        } else {
            self.no_fonts.clone()
        };
        let options = usvg::Options {
            fontdb,
            image_href_resolver: usvg::ImageHrefResolver {
                resolve_data: Box::new(|_, _, _| None),
                resolve_string: Box::new(|_, _| None),
            },
            ..usvg::Options::default()
        };
        let tree = parse(&source, &options)?;
        let tree = match neutralized {
            Some(alpha) => neutralize_root_opacity(tree, &options, alpha)?,
            None => tree,
        };
        let new_source = !self.trees.contains_key(&source);
        let added = if new_source { source.len() } else { 0 };
        let repeated = !self.seen.insert(self.hasher.hash_one((&source, key)));
        if !repeated || self.cached_source_bytes + added > MAX_CACHED_SOURCE_BYTES {
            self.uncached = Some(tree);
            return Ok(self.uncached.as_ref().expect("just stored"));
        }
        self.cached_source_bytes += added;
        let trees = self.trees.entry(source).or_default();
        trees.push((key, tree));
        Ok(&trees.last().expect("just pushed").1)
    }
}

fn rect_path(x: f32, y: f32, width: f32, height: f32, what: &str) -> Result<krilla::geom::Path> {
    let rect = Rect::from_xywh(x, y, width, height)
        .ok_or_else(|| Error::PdfGeneration(format!("invalid SVG {what}")))?;
    let mut builder = krilla::geom::PathBuilder::new();
    builder.push_rect(rect);
    builder
        .finish()
        .ok_or_else(|| Error::PdfGeneration(format!("invalid SVG {what} path")))
}

fn parse(source: &str, options: &usvg::Options<'_>) -> Result<usvg::Tree> {
    usvg::Tree::from_str(source, options)
        .map_err(|error| Error::Layout(format!("inline SVG vector parse: {error}")))
}

fn neutralize_root_opacity(
    tree: usvg::Tree,
    options: &usvg::Options<'_>,
    alpha: f32,
) -> Result<usvg::Tree> {
    // usvg omits the root opacity group when it is within four ULPs of one.
    if alpha.to_bits().abs_diff(1.0f32.to_bits()) <= 4 || tree.root().children().is_empty() {
        return Ok(tree);
    }
    // Resolve inheritance and use instances before removing the root multiplier.
    // Preserve SVG text so Krilla can still embed selectable glyphs.
    let mut source = tree.to_string(&usvg::WriteOptions {
        preserve_text: true,
        ..usvg::WriteOptions::default()
    });
    let range = {
        let document = usvg::roxmltree::Document::parse(&source)
            .map_err(|error| Error::Layout(format!("normalized SVG XML: {error}")))?;
        let mut group = document
            .root_element()
            .children()
            .find(|node| node.has_tag_name("g"))
            .ok_or_else(|| Error::Layout("missing SVG root opacity group".into()))?;
        loop {
            if let Some(attribute) = group.attribute_node("opacity") {
                let opacity: f32 = attribute
                    .value()
                    .parse()
                    .map_err(|_| Error::Layout("invalid normalized SVG root opacity".into()))?;
                if opacity.to_bits().abs_diff(alpha.to_bits()) > 4 {
                    return Err(Error::Layout("unexpected SVG root opacity".into()));
                }
                break attribute.range();
            }
            // The optional viewBox transform wrapper precedes the source root.
            let mut children = group.children().filter(|node| node.is_element());
            let child = children
                .next()
                .filter(|node| node.has_tag_name("g"))
                .ok_or_else(|| Error::Layout("missing nested SVG root opacity group".into()))?;
            if children.next().is_some() {
                return Err(Error::Layout("ambiguous SVG root opacity group".into()));
            }
            group = child;
        }
    };
    source.replace_range(range, "");
    parse(&source, options)
}
