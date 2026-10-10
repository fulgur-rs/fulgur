use super::*;
use raikiri_traits::{
    DecodedImage, ImagePixelSource, IntrinsicBox, ReplacedResolver, ResolveDisposition,
    ResolvedIntrinsic, ResolverError, ResolverRequest,
};
use std::sync::Arc;

struct RedPixels(Arc<DecodedImage>);

impl ImagePixelSource for RedPixels {
    fn get_decoded(&self, _url: &url::Url) -> Option<Arc<DecodedImage>> {
        Some(Arc::clone(&self.0))
    }
}

impl ReplacedResolver for RedPixels {
    fn resolve(
        &self,
        _request: ResolverRequest<'_>,
    ) -> std::result::Result<ResolvedIntrinsic, ResolverError> {
        Ok(ResolvedIntrinsic {
            intrinsic: IntrinsicBox::new(4.0, 2.0),
            disposition: ResolveDisposition::Ok,
        })
    }
}

#[test]
fn legacy_pages_keep_raster_pixels_and_own_opacity_and_allow_no_source() {
    let pixels = RedPixels(Arc::new(DecodedImage {
        width: 4,
        height: 2,
        rgba: [255, 0, 0, 255].repeat(8),
    }));
    let resources = RenderResources::new()
        .replaced_resolver(&pixels)
        .image_pixel_source(&pixels);
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>@page{size:100px 100px;margin:0}body{margin:0}div{width:20px;height:10px;background:blue}img{display:block;width:20px;height:20px;opacity:.5}</style><div></div><img src='https://images.test/image.png'>"[..],
        &resources,
    ).unwrap();
    let LayoutStatus::Completed(layout) = raikiri_html::layout(
        &document,
        PageDefaults::default(),
        LayoutConfig::default(),
        LayoutOptions::new().resources(&resources),
    )
    .unwrap() else {
        panic!("completed layout")
    };
    let page = layout.page(0).unwrap();
    for with_source in [false, true] {
        let mut pdf = krilla::Document::new();
        let mut pdf_page = pdf.start_page_with(PageSettings::from_wh(100.0, 100.0).unwrap());
        let mut surface = pdf_page.surface();
        let mut fonts = FontCache::default();
        let mut svg = svg::SvgCache::new(crate::RenderOptions::default());
        let mut raster = raster::RasterCache::for_tests(
            with_source
                .then(|| resources.image_pixel_source_ref())
                .flatten(),
        );
        paint_legacy(
            &mut surface,
            &page,
            &page.text_runs(),
            &mut fonts,
            &mut svg,
            &mut raster,
        )
        .unwrap();
        surface.finish();
        pdf_page.finish();
        let bytes = pdf.finish().unwrap();
        let parsed = lopdf::Document::load_mem(&bytes).unwrap();
        let images: Vec<_> = parsed
            .objects
            .values()
            .filter_map(|object| object.as_stream().ok())
            .filter(|stream| {
                stream
                    .dict
                    .get(b"Subtype")
                    .and_then(lopdf::Object::as_name)
                    .ok()
                    == Some(b"Image".as_slice())
            })
            .collect();
        assert_eq!(images.len(), usize::from(with_source));
        if with_source {
            assert_eq!(
                images[0].decompressed_content().unwrap(),
                [255, 0, 0].repeat(8)
            );
            assert!(
                parsed
                    .objects
                    .values()
                    .filter_map(|object| object.as_dict().ok())
                    .any(
                        |dict| dict.get(b"ca").and_then(lopdf::Object::as_float).ok() == Some(0.5)
                    )
            );
        }
        let page_id = *parsed.get_pages().values().next().unwrap();
        let content =
            lopdf::content::Content::decode(&parsed.get_page_content(page_id).unwrap()).unwrap();
        assert!(
            content
                .operations
                .iter()
                .any(|operation| operation.operator == "rg"
                    && operation.operands == vec![0.into(), 0.into(), 1.into()])
        );
        assert_eq!(
            content
                .operations
                .iter()
                .filter(|operation| operation.operator == "q")
                .count(),
            content
                .operations
                .iter()
                .filter(|operation| operation.operator == "Q")
                .count()
        );
    }
}

#[test]
fn ordered_marker_events_can_be_rendered_without_a_pixel_provider() {
    let pixels = RedPixels(Arc::new(DecodedImage {
        width: 4,
        height: 2,
        rgba: [255, 0, 0, 255].repeat(8),
    }));
    let resources = RenderResources::new().image_pixel_source(&pixels);
    let document = raikiri_html::parse_html_with_resources(
        &b"<style>@page{size:100px 100px;margin:0}body{margin:0}ul{margin:0;padding:0}li{margin-left:20px;opacity:.5;list-style-image:url(https://images.test/marker.png)}</style><ul><li></li></ul>"[..],
        &resources,
    ).unwrap();
    let LayoutStatus::Completed(layout) = raikiri_html::layout(
        &document,
        PageDefaults::default(),
        LayoutConfig::default(),
        LayoutOptions::new().resources(&resources),
    )
    .unwrap() else {
        panic!("completed layout")
    };
    let page = layout.page(0).unwrap();
    let runs = page.text_runs();
    let events = page.paint_order_for_text_runs(&runs);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, PaintEvent::MarkerImage(_)))
    );
    assert!(order::supported(&events, &runs));
    for with_source in [false, true] {
        let mut pdf = krilla::Document::new();
        let mut pdf_page = pdf.start_page_with(PageSettings::from_wh(100.0, 100.0).unwrap());
        let mut surface = pdf_page.surface();
        let mut fonts = FontCache::default();
        let mut svg = svg::SvgCache::new(crate::RenderOptions::default());
        let mut raster = raster::RasterCache::for_tests(
            with_source
                .then(|| resources.image_pixel_source_ref())
                .flatten(),
        );
        paint_ordered(
            &mut surface,
            &page,
            &events,
            &runs,
            &mut fonts,
            &mut svg,
            &mut raster,
        )
        .unwrap();
        surface.finish();
        pdf_page.finish();
        let parsed = lopdf::Document::load_mem(&pdf.finish().unwrap()).unwrap();
        let images = parsed
            .objects
            .values()
            .filter_map(|object| object.as_stream().ok())
            .filter(|stream| {
                stream
                    .dict
                    .get(b"Subtype")
                    .and_then(lopdf::Object::as_name)
                    .ok()
                    == Some(b"Image".as_slice())
            })
            .count();
        assert_eq!(images, usize::from(with_source));
        let page_id = *parsed.get_pages().values().next().unwrap();
        let content =
            lopdf::content::Content::decode(&parsed.get_page_content(page_id).unwrap()).unwrap();
        let saves = content
            .operations
            .iter()
            .filter(|op| op.operator == "q")
            .count();
        let restores = content
            .operations
            .iter()
            .filter(|op| op.operator == "Q")
            .count();
        assert_eq!(saves, restores);
    }
}
