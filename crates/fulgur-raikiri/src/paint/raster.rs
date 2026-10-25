//! Cached raster pixels inside the existing page paint stack.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use fulgur_core::{Error, Result};
use krilla::geom::Path;
use krilla::geom::{Size, Transform};
use krilla::image::{BitsPerComponent, CustomImage, Image, ImageColorspace};
use krilla::num::NormalizedF32;
use krilla::paint::FillRule;
use krilla::surface::Surface;
use raikiri_html::computed::{
    ComputedBackgroundRepeat, ComputedBackgroundSize, ComputedCssPosition,
};
use raikiri_html::image_geometry::{background_image_dimensions, background_tiles};
use raikiri_html::{
    Fragment, GeneratedImageId, ImageContent, NodeId, Page, PaintRect, PlacedImage, SvgRendering,
};
use raikiri_traits::vector::VectorImage;
use raikiri_traits::{
    DecodedImage, ImageIntrinsicSize, ImagePixelSource, ImageRasterSize, ImageVectorRequest,
};
use url::Url;

use super::svg::rect_path;
use super::svg::scene::{self, SceneAssets};

#[derive(Clone, Hash)]
struct Pixels {
    width: u32,
    height: u32,
    colors: Arc<Vec<u8>>,
    alpha: Option<Arc<Vec<u8>>>,
}

impl CustomImage for Pixels {
    fn color_channel(&self) -> &[u8] {
        self.colors.as_slice()
    }
    fn alpha_channel(&self) -> Option<&[u8]> {
        self.alpha.as_ref().map(|alpha| alpha.as_slice())
    }
    fn bits_per_component(&self) -> BitsPerComponent {
        BitsPerComponent::Eight
    }
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn icc_profile(&self) -> Option<&[u8]> {
        None
    }
    fn color_space(&self) -> ImageColorspace {
        ImageColorspace::Rgb
    }
}

/// The Krilla image of straight RGBA8 `pixels`. The color and alpha
/// channels are split once for PDF, so repeated placements should share the
/// returned image. `interpolate` asks viewers to smooth it when scaled.
pub(super) fn krilla_image(pixels: &DecodedImage, interpolate: bool) -> Result<Image> {
    // Page validates the RGBA dimensions.
    let count = pixels.rgba.len() / 4;
    let mut colors = Vec::with_capacity(count * 3);
    let has_alpha = pixels
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] != 255);
    let mut alpha = has_alpha.then(|| Vec::with_capacity(count));
    for pixel in pixels.rgba.as_chunks::<4>().0 {
        colors.extend_from_slice(&pixel[..3]);
        if let Some(alpha) = &mut alpha {
            alpha.push(pixel[3]);
        }
    }
    Image::from_custom(
        Pixels {
            width: pixels.width,
            height: pixels.height,
            colors: Arc::new(colors),
            alpha: alpha.map(Arc::new),
        },
        interpolate,
    )
    .map_err(Error::PdfGeneration)
}

/// The most tiles one background layer draws.
const MAX_BACKGROUND_TILES: usize = 100_000;

/// The most tiles of one layer drawn as vector content. Each tile repeats the
/// drawing operations of the scene, where an image is drawn by reference, so
/// a layer with more tiles than this is drawn as pixels.
const MAX_VECTOR_TILES: usize = 64;

/// How the SVG images of an element are drawn: the choice of the element that
/// owns them and the layout's raster scale.
#[derive(Clone, Copy)]
pub(super) struct SvgChoice {
    pub rendering: SvgRendering,
    /// Device pixels per CSS pixel of an SVG image drawn as pixels.
    pub scale: f32,
}

impl Default for SvgChoice {
    fn default() -> Self {
        Self {
            rendering: SvgRendering::default(),
            scale: 1.0,
        }
    }
}

impl SvgChoice {
    /// The choice for the images that `owner` owns on `page`.
    pub(super) fn of(page: &Page<'_>, owner: NodeId) -> Self {
        Self {
            rendering: page.svg_rendering(owner),
            scale: page.svg_raster_scale(),
        }
    }
}

/// One `url()` background image layer as computed for a box.
pub(super) struct BackgroundLayer<'s> {
    /// The computed `url()` value.
    pub url: &'s str,
    pub size: &'s ComputedBackgroundSize,
    pub position: &'s ComputedCssPosition,
    pub repeat: &'s ComputedBackgroundRepeat,
    /// How an SVG image is drawn, by the element that owns the layer.
    pub svg: SvgChoice,
}

/// What a tile of a background layer is drawn with.
enum Tile {
    Image(Image),
    Scene(Arc<VectorImage>),
}

/// Converted images kept per URL, one per raster size drawn, so that an
/// image drawn at two sizes in turn is not converted again each time.
const SIZES_PER_URL: usize = 4;

/// A converted image and the source pixels it came from. The pixels are
/// held weakly: the converted image owns its own channels, and a strong
/// reference would keep the source's RGBA buffer alive after the source
/// evicts it.
struct Converted {
    pixels: Weak<DecodedImage>,
    width: u32,
    height: u32,
    image: Image,
}

pub(super) struct RasterCache<'a> {
    source: Option<&'a dyn ImagePixelSource>,
    base: Url,
    /// Converted images by URL, oldest first.
    images: HashMap<Box<str>, Vec<Converted>>,
    /// Resolved `url()` values, so a `data:` URL is parsed once.
    urls: HashMap<Box<str>, Option<Url>>,
    /// Raster pixels left for the document's blurred box shadows.
    pub(super) box_shadows: super::shadow::Budget,
    /// Fonts and pictures of the scenes of SVG images, converted for Krilla.
    scene_assets: SceneAssets,
}

impl<'a> RasterCache<'a> {
    pub(super) fn new(source: Option<&'a dyn ImagePixelSource>, base: Url) -> Self {
        Self {
            source,
            base,
            images: HashMap::new(),
            urls: HashMap::new(),
            box_shadows: super::shadow::Budget::default(),
            scene_assets: SceneAssets::default(),
        }
    }

    fn image(&mut self, url: &Url, pixels: &Arc<DecodedImage>) -> Result<Image> {
        // The pixel source returns the same pixels for the same size while
        // it keeps them, so the pixels identify the converted image.
        if let Some(converted) = self.images.get(url.as_str()).and_then(|images| {
            images
                .iter()
                .find(|converted| std::ptr::eq(converted.pixels.as_ptr(), Arc::as_ptr(pixels)))
        }) {
            return Ok(converted.image.clone());
        }
        let image = krilla_image(pixels, true)?;
        let images = self.images.entry(url.as_str().into()).or_default();
        // Pixels of the same size that the source no longer returns are stale.
        images.retain(|converted| {
            (converted.width, converted.height) != (pixels.width, pixels.height)
        });
        if images.len() == SIZES_PER_URL {
            images.remove(0);
        }
        images.push(Converted {
            pixels: Arc::downgrade(pixels),
            width: pixels.width,
            height: pixels.height,
            image: image.clone(),
        });
        Ok(image)
    }

    pub(super) fn paint(
        &mut self,
        surface: &mut Surface<'_>,
        page: &Page<'_>,
        fragment: &Fragment<'_>,
        host_group: bool,
    ) -> Result<()> {
        let Some(source) = self.source else {
            return Ok(());
        };
        let Some(placed) = page.placed_image(fragment, source) else {
            return Ok(());
        };
        let Some(placed) = self.drawable(placed, || page.raster_image(fragment, source)) else {
            return Ok(());
        };
        self.paint_placement(surface, page, &placed, host_group)
    }

    pub(super) fn marker(&self, page: &Page<'_>, owner: NodeId) -> Option<PlacedImage> {
        let source = self.source?;
        let placed = page.placed_marker(owner, source)?;
        self.drawable(placed, || page.raster_marker(owner, source))
    }

    /// The image of `::before` or `::after` content, as laid out on `page`,
    /// with its rectangle.
    pub(super) fn generated_image(
        &self,
        page: &Page<'_>,
        image: GeneratedImageId,
    ) -> Option<PlacedImage> {
        let source = self.source?;
        let placed = page.placed_generated_image(image, source)?;
        self.drawable(placed, || page.raster_generated_image(image, source))
    }

    /// `placed`, with the pixels `pixels` gives where its scene is one this
    /// painter cannot draw faithfully.
    fn drawable(
        &self,
        mut placed: PlacedImage,
        pixels: impl FnOnce() -> Option<raikiri_html::RasterImage>,
    ) -> Option<PlacedImage> {
        if let ImageContent::Vector(scene) = &placed.content
            && !scene::paintable(scene)
        {
            placed.content = ImageContent::Raster(pixels()?.pixels);
        }
        Some(placed)
    }

    pub(super) fn paint_placement(
        &mut self,
        surface: &mut Surface<'_>,
        page: &Page<'_>,
        placement: &PlacedImage,
        host_group: bool,
    ) -> Result<()> {
        let Some(path) = super::clip::clip_path(placement.clip, page.geometry().page_box) else {
            return Ok(());
        };
        let Some(size) = Size::from_wh(placement.rect.width, placement.rect.height) else {
            return Ok(());
        };
        let tile = match &placement.content {
            ImageContent::Raster(pixels) => Tile::Image(self.image(&placement.url, pixels)?),
            ImageContent::Vector(scene) => Tile::Scene(Arc::clone(scene)),
            _ => return Ok(()),
        };
        let opacity = (!host_group)
            .then(|| page.computed(placement.node))
            .flatten()
            .and_then(|style| NormalizedF32::new(style.opacity));
        if let Some(opacity) = opacity {
            surface.push_opacity(opacity);
        }
        surface.push_clip_path(&path, &FillRule::NonZero);
        surface.push_transform(&Transform::from_translate(
            placement.rect.x,
            placement.rect.y,
        ));
        let result = self.draw_tile(surface, &tile, size);
        surface.pop();
        surface.pop();
        if opacity.is_some() {
            surface.pop();
        }
        result
    }

    /// Draws `tile` with its top left corner at the origin of `surface`'s
    /// current space, in a box of `size`.
    fn draw_tile(&mut self, surface: &mut Surface<'_>, tile: &Tile, size: Size) -> Result<()> {
        match tile {
            Tile::Image(image) => {
                surface.draw_image(image.clone(), size);
                Ok(())
            }
            Tile::Scene(scene) => {
                // The scene is drawn in its box and does not show outside it.
                let path = rect_path(0.0, 0.0, size.width(), size.height(), "tile")?;
                surface.push_clip_path(&path, &FillRule::NonZero);
                let result = scene::paint(surface, scene, &mut self.scene_assets);
                surface.pop();
                result
            }
        }
    }

    /// Draw one `url()` background layer (CSS Backgrounds 3 §3).
    ///
    /// `positioning` is the `background-origin` box that the size and
    /// position resolve against; tiles cover `painting`, the bounds of the
    /// `background-clip` area, and are clipped to `area`. Tiles use the same
    /// geometry as Raikiri's own painter. An image that cannot be fetched or
    /// decoded draws nothing, like an unavailable `<img>`.
    pub(super) fn paint_background(
        &mut self,
        surface: &mut Surface<'_>,
        layer: &BackgroundLayer<'_>,
        positioning: PaintRect,
        painting: PaintRect,
        area: (&Path, FillRule),
    ) -> Result<()> {
        let Some(source) = self.source else {
            return Ok(());
        };
        let Some(url) = self.resolve_cached(layer.url) else {
            return Ok(());
        };
        let Some(intrinsic) = source.intrinsic_size(&url) else {
            return Ok(());
        };
        let Some((width, height)) = background_image_dimensions(
            layer.size,
            f64::from(positioning.width),
            f64::from(positioning.height),
            intrinsic,
        ) else {
            return Ok(());
        };
        let rect = |rect: PaintRect| {
            (
                f64::from(rect.x),
                f64::from(rect.y),
                f64::from(rect.width),
                f64::from(rect.height),
            )
        };
        let Some(tiles) = background_tiles(
            rect(positioning),
            rect(painting),
            (width, height),
            layer.position,
            layer.repeat,
        ) else {
            return Ok(());
        };
        // Each axis is bounded already, but tiny tiles over a large area can
        // still multiply into millions of draws; such a layer draws nothing.
        if tiles.x.len().saturating_mul(tiles.y.len()) > MAX_BACKGROUND_TILES {
            return Ok(());
        }
        let Some(size) = Size::from_wh(tiles.width as f32, tiles.height as f32) else {
            return Ok(());
        };
        // `round` rescales the tile, so draw it at the size actually drawn.
        let count = tiles.x.len().saturating_mul(tiles.y.len());
        let Some(tile) = self.tile(source, &url, size, count, layer.svg)? else {
            return Ok(());
        };
        let (path, rule) = area;
        surface.push_clip_path(path, &rule);
        let result = self.draw_tiles(surface, (&tiles.x, &tiles.y), &tile, size);
        surface.pop();
        result
    }

    /// Draws `tile` once at every position of the grid `(xs, ys)`.
    fn draw_tiles(
        &mut self,
        surface: &mut Surface<'_>,
        (xs, ys): (&[f64], &[f64]),
        tile: &Tile,
        size: Size,
    ) -> Result<()> {
        for y in ys {
            for x in xs {
                surface.push_transform(&Transform::from_translate(*x as f32, *y as f32));
                let drawn = self.draw_tile(surface, tile, size);
                surface.pop();
                drawn?;
            }
        }
        Ok(())
    }

    /// What the `count` tiles of `size` of the image at `url` are drawn with:
    /// a scene when the image has one, `svg` asks for vector content and the
    /// tiles are few, otherwise pixels at the raster scale.
    fn tile(
        &mut self,
        source: &dyn ImagePixelSource,
        url: &Url,
        size: Size,
        count: usize,
        svg: SvgChoice,
    ) -> Result<Option<Tile>> {
        if svg.rendering == SvgRendering::Vector
            && count <= MAX_VECTOR_TILES
            && let Some(scene) = source.get_vector(
                url,
                &ImageVectorRequest::new(ImageRasterSize {
                    width: size.width(),
                    height: size.height(),
                }),
            )
            && scene::paintable(&scene)
        {
            return Ok(Some(Tile::Scene(scene)));
        }
        Ok(self
            .sized_image(url, size.width() * svg.scale, size.height() * svg.scale)?
            .map(Tile::Image))
    }

    /// The resolved URL and natural size of a `url()` image, or `None` when
    /// it cannot be fetched or decoded.
    pub(super) fn natural_size(&mut self, raw: &str) -> Option<(Url, ImageIntrinsicSize)> {
        let url = self.resolve_cached(raw)?;
        let natural = self.source?.intrinsic_size(&url)?;
        Some((url, natural))
    }

    /// The image at `url` decoded for drawing at `width` × `height` px.
    pub(super) fn sized_image(
        &mut self,
        url: &Url,
        width: f32,
        height: f32,
    ) -> Result<Option<Image>> {
        let Some(source) = self.source else {
            return Ok(None);
        };
        let Some(pixels) = source.get_decoded_at_size(url, ImageRasterSize { width, height }, None)
        else {
            return Ok(None);
        };
        let bytes = (pixels.width as usize)
            .checked_mul(pixels.height as usize)
            .and_then(|count| count.checked_mul(4));
        if pixels.width == 0 || pixels.height == 0 || bytes != Some(pixels.rgba.len()) {
            return Ok(None);
        }
        self.image(url, &pixels).map(Some)
    }

    /// [`Self::resolve`], remembered per `url()` value.
    fn resolve_cached(&mut self, raw: &str) -> Option<Url> {
        if let Some(url) = self.urls.get(raw) {
            return url.clone();
        }
        let url = self.resolve(raw);
        self.urls.insert(raw.into(), url.clone());
        url
    }

    /// The absolute image URL of a computed `url()` value, without its
    /// fragment, which names no separate resource. `url()` and `url(#id)`
    /// name no external image, so they resolve to nothing rather than to
    /// the document itself.
    fn resolve(&self, raw: &str) -> Option<Url> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }
        let mut url = Url::parse(raw).or_else(|_| self.base.join(raw)).ok()?;
        url.set_fragment(None);
        Some(url)
    }
}

#[cfg(test)]
impl<'a> RasterCache<'a> {
    /// A cache whose relative `url()` values resolve against `file:///`.
    pub(super) fn for_tests(source: Option<&'a dyn ImagePixelSource>) -> Self {
        Self::new(source, Url::parse("file:///").expect("valid URL"))
    }
}

#[cfg(test)]
mod tests {
    use super::RasterCache;

    #[test]
    fn images_without_a_pixel_source_are_unavailable() {
        let mut cache = RasterCache::for_tests(None);
        let url = url::Url::parse("file:///a.png").unwrap();
        assert!(cache.natural_size("a.png").is_none());
        assert!(cache.sized_image(&url, 10.0, 10.0).unwrap().is_none());
    }

    #[test]
    fn local_and_empty_references_resolve_to_nothing() {
        let cache = RasterCache::for_tests(None);
        assert_eq!(cache.resolve("#frag"), None);
        assert_eq!(cache.resolve(""), None);
        assert_eq!(cache.resolve("  "), None);
        assert_eq!(
            cache.resolve("img/a.svg#x").map(|url| url.to_string()),
            Some("file:///img/a.svg".to_string())
        );
    }

    struct Sizes(Vec<std::sync::Arc<raikiri_traits::DecodedImage>>);

    impl raikiri_traits::ImagePixelSource for Sizes {
        fn get_decoded(
            &self,
            _url: &url::Url,
        ) -> Option<std::sync::Arc<raikiri_traits::DecodedImage>> {
            None
        }

        fn get_decoded_at_size(
            &self,
            _url: &url::Url,
            size: raikiri_traits::ImageRasterSize,
            _max_output_bytes: Option<u64>,
        ) -> Option<std::sync::Arc<raikiri_traits::DecodedImage>> {
            self.0
                .iter()
                .find(|image| image.width as f32 == size.width)
                .cloned()
        }
    }

    #[test]
    fn images_drawn_at_two_sizes_in_turn_are_converted_once_per_size() {
        let image = |side: u32| {
            std::sync::Arc::new(raikiri_traits::DecodedImage::new(
                side,
                side,
                vec![255; (side * side * 4) as usize],
            ))
        };
        let source = Sizes(vec![image(16), image(24)]);
        let mut cache = RasterCache::for_tests(Some(&source));
        let url = url::Url::parse("data:image/svg+xml,<svg/>").unwrap();
        for side in [16.0, 24.0, 16.0, 24.0] {
            assert!(cache.sized_image(&url, side, side).unwrap().is_some());
        }
        assert_eq!(cache.images.get(url.as_str()).unwrap().len(), 2);

        // Past four sizes the oldest goes.
        let source = Sizes((1..=5).map(image).collect());
        let mut cache = RasterCache::for_tests(Some(&source));
        for side in 1..=5 {
            assert!(cache.sized_image(&url, side as f32, 1.0).unwrap().is_some());
        }
        let widths: Vec<_> = cache
            .images
            .get(url.as_str())
            .unwrap()
            .iter()
            .map(|converted| converted.width)
            .collect();
        assert_eq!(widths, [2, 3, 4, 5]);

        // New pixels of a cached size replace the stale entry.
        let fresh = image(5);
        cache.image(&url, &fresh).unwrap();
        let cached = cache.images.get(url.as_str()).unwrap();
        assert_eq!(cached.len(), 4);
        // The cache does not keep the source's pixels alive.
        assert_eq!(std::sync::Arc::strong_count(&fresh), 1);
        assert!(std::ptr::eq(
            cached[3].pixels.as_ptr(),
            std::sync::Arc::as_ptr(&fresh)
        ));
    }

    #[test]
    fn url_values_resolve_once() {
        let mut cache = RasterCache::for_tests(None);
        let long = format!("data:image/png;base64,{}", "A".repeat(4096));
        for raw in [long.as_str(), "a.png", long.as_str(), "#x"] {
            assert_eq!(cache.resolve_cached(raw), cache.resolve(raw));
        }
        assert_eq!(cache.urls.len(), 3);
    }
}
