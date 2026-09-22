//! Raikiri's parse/resource/cascade/page-layout bridge used by `Engine`.
//!
//! This is an intermediate migration stage: the current PDF `Drawables` converter
//! still consumes Blitz nodes. The stage keeps Raikiri's mutable DOM, matching
//! cascade, page slices, and resource cache together so the next converter can
//! consume them without reparsing or using `PageScene`.

use crate::asset::AssetBundle;
use crate::config::{Margin, PageSize};
use crate::error::{Error, Result};
use crate::image::AssetKind;
use crate::units::F32Units;
use base64::Engine as _;
use parley_engine::FontContext;
use raikiri_dom::{
    FontFaceApplyReport, FontFaceLoader, apply_font_faces, layout_pages_with_resolver_and_base_url,
};
use raikiri_html::{ParseOptions, UncascadedDocument, effective_document_base_url, parse};
use raikiri_style::{
    CascadeResult, FontFaceRegistry, MediaContext, MediaType, Origin, PageContextQuery, RuleTree,
};
use raikiri_traits::{
    DecodedImage, FetchedResource, ImagePixelSource, Method, NetworkError, NetworkProvider,
    PageBox, RenderWarning, ReplacedResolver, Request, ResolveDisposition, ResolvedIntrinsic,
    ResolverError, ResolverRequest, ResourceKind, StylesheetKind, WarningKind,
};
use std::collections::HashMap;
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use url::Url;

const CSS_RESOURCE_LIMIT: u64 = 16 * 1024 * 1024;
const FONT_RESOURCE_LIMIT: u64 = 64 * 1024 * 1024;
const IMAGE_RESOURCE_LIMIT: u64 = 64 * 1024 * 1024;
const TOTAL_PROVIDER_RESOURCE_LIMIT: u64 = 256 * 1024 * 1024;

/// Data that must remain paired with its Raikiri DOM arena until conversion.
pub(crate) struct RaikiriLayoutStage {
    pub(crate) document: raikiri_dom::Document,
    pub(crate) cascade: CascadeResult,
    pub(crate) pages: Vec<raikiri_dom::PageSlice>,
    pub(crate) page_box: PageBox,
    pub(crate) effective_base_url: Option<Url>,
    pub(crate) resources: SharedResourceCache,
    pub(crate) warnings: Vec<RenderWarning>,
    pub(crate) font_face_report: FontFaceApplyReport,
}

#[derive(Clone, Default)]
pub(crate) struct SharedResourceCache(Arc<Mutex<ResourceCache>>);

#[derive(Default)]
struct ResourceCache {
    raw: HashMap<Url, Arc<Vec<u8>>>,
    decoded: HashMap<Url, Arc<DecodedImage>>,
}

impl SharedResourceCache {
    #[allow(dead_code)] // Consumed by the follow-up PageFragment-to-Drawables adapter.
    pub(crate) fn raw_bytes(&self, url: &Url) -> Option<Arc<Vec<u8>>> {
        self.0.lock().ok()?.raw.get(url).cloned()
    }

    fn insert_raw(&self, url: Url, bytes: Vec<u8>) {
        if let Ok(mut cache) = self.0.lock() {
            cache.raw.insert(url, Arc::new(bytes));
        }
    }

    fn insert_decoded(&self, url: Url, decoded: DecodedImage) {
        if let Ok(mut cache) = self.0.lock() {
            cache.decoded.insert(url, Arc::new(decoded));
        }
    }

    fn decoded(&self, url: &Url) -> Option<Arc<DecodedImage>> {
        self.0.lock().ok()?.decoded.get(url).cloned()
    }
}

struct FulgurNetworkProvider {
    #[cfg(not(target_arch = "wasm32"))]
    canonical_root: Option<PathBuf>,
    bytes_read: Mutex<u64>,
    warnings: Arc<Mutex<Vec<RenderWarning>>>,
}

impl FulgurNetworkProvider {
    fn new(base_path: Option<&Path>, warnings: Arc<Mutex<Vec<RenderWarning>>>) -> Self {
        #[cfg(target_arch = "wasm32")]
        let _ = base_path;
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            canonical_root: base_path.and_then(|path| path.canonicalize().ok()),
            bytes_read: Mutex::new(0),
            warnings,
        }
    }

    fn limit_for(kind: ResourceKind) -> u64 {
        match kind {
            ResourceKind::ExternalStylesheet | ResourceKind::StylesheetImport => CSS_RESOURCE_LIMIT,
            ResourceKind::Font => FONT_RESOURCE_LIMIT,
            ResourceKind::Image | ResourceKind::Svg => IMAGE_RESOURCE_LIMIT,
            _ => IMAGE_RESOURCE_LIMIT,
        }
    }

    fn read_url(
        &self,
        url: &Url,
        kind: ResourceKind,
    ) -> std::result::Result<Vec<u8>, NetworkError> {
        if url.scheme() != "file" {
            return Err(NetworkError::Other(
                "only local file resources are enabled".into(),
            ));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = kind;
            return Err(NetworkError::Other(
                "local file resources are unavailable on wasm".into(),
            ));
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let root = self.canonical_root.as_ref().ok_or_else(|| {
                NetworkError::Other("no local resource root is configured".into())
            })?;
            let path = url
                .to_file_path()
                .map_err(|_| NetworkError::Other("invalid local file URL".into()))?;
            let canonical_path = path
                .canonicalize()
                .map_err(|_| NetworkError::Other("local resource is unavailable".into()))?;
            if !canonical_path.starts_with(root) {
                return Err(NetworkError::Other(
                    "local resource is outside the configured root".into(),
                ));
            }

            let limit = Self::limit_for(kind);
            let (mut file, _) = raikiri_traits::io::open_bounded_regular_file(
                &canonical_path,
                limit,
            )
            .map_err(|error| match error {
                raikiri_traits::io::RejectReason::Oversized { size, cap, .. } => {
                    self.push_limit_warning(kind, cap, size);
                    NetworkError::Other("local resource exceeds its byte limit".into())
                }
                _ => NetworkError::Other("local resource is not a bounded regular file".into()),
            })?;
            // The path is canonicalized before opening to enforce Fulgur's root
            // containment rule. The bounded open rejects a symlink leaf and checks
            // the opened descriptor is a regular file before any bytes are read.
            let bytes = raikiri_traits::io::read_bounded_from_open_file(&mut file, limit).map_err(
                |error| match error {
                    raikiri_traits::io::RejectReason::Oversized { size, cap, .. } => {
                        self.push_limit_warning(kind, cap, size);
                        NetworkError::Other("local resource exceeds its byte limit".into())
                    }
                    _ => NetworkError::Other("local resource read failed".into()),
                },
            )?;
            self.account_resource_bytes(kind, bytes.len() as u64)?;
            Ok(bytes)
        }
    }

    fn push_limit_warning(&self, kind: ResourceKind, limit: u64, actual: u64) {
        if let Ok(mut warnings) = self.warnings.lock() {
            warnings.push(RenderWarning {
                kind: WarningKind::ResourceLimitExceeded {
                    kind,
                    limit,
                    actual,
                },
                node_id: None,
                details: format!("resource byte limit reached ({actual} > {limit})"),
            });
        }
    }

    fn account_resource_bytes(
        &self,
        kind: ResourceKind,
        size: u64,
    ) -> std::result::Result<(), NetworkError> {
        let mut total = self.bytes_read.lock().unwrap_or_else(|e| e.into_inner());
        let next = total.saturating_add(size);
        if next > TOTAL_PROVIDER_RESOURCE_LIMIT {
            self.push_limit_warning(kind, TOTAL_PROVIDER_RESOURCE_LIMIT, next);
            return Err(NetworkError::Other(
                "aggregate provider resource byte limit exceeded".into(),
            ));
        }
        *total = next;
        Ok(())
    }

    fn fetch_bytes(
        &self,
        url: &Url,
        kind: ResourceKind,
    ) -> std::result::Result<Vec<u8>, NetworkError> {
        if url.scheme() == "data" {
            let bytes = decode_data_url(url.as_str())
                .ok_or_else(|| NetworkError::Other("invalid data URL".into()))?;
            let limit = Self::limit_for(kind);
            if bytes.len() as u64 > limit {
                self.push_limit_warning(kind, limit, bytes.len() as u64);
                return Err(NetworkError::Other("resource byte limit exceeded".into()));
            }
            self.account_resource_bytes(kind, bytes.len() as u64)?;
            return Ok(bytes);
        }
        self.read_url(url, kind)
    }
}

impl NetworkProvider for FulgurNetworkProvider {
    fn fetch(&self, request: Request) -> std::result::Result<FetchedResource, NetworkError> {
        if !matches!(request.method, Method::Get) {
            return Err(NetworkError::Other(
                "only GET resource requests are enabled".into(),
            ));
        }
        if request
            .signal
            .as_ref()
            .is_some_and(|signal| signal.is_aborted())
        {
            return Err(NetworkError::Aborted);
        }
        let bytes = self.fetch_bytes(&request.url, request.kind)?;
        Ok(FetchedResource {
            bytes: bytes.into(),
            content_type: mime_for_url(&request.url),
            final_url: request.url,
            encoding: None,
        })
    }
}

struct FulgurFontLoader<'a> {
    provider: &'a FulgurNetworkProvider,
    base_url: Option<&'a Url>,
    warnings: Arc<Mutex<Vec<RenderWarning>>>,
}

impl FontFaceLoader for FulgurFontLoader<'_> {
    fn load(&self, source: &str) -> Option<Vec<u8>> {
        let url = resolve_url(source, self.base_url)?;
        match self.provider.fetch_bytes(&url, ResourceKind::Font) {
            Ok(bytes) => Some(bytes),
            Err(_) => {
                push_resource_warning(&self.warnings, ResourceKind::Font, Some(url));
                None
            }
        }
    }
}

struct FulgurImageResolver<'a> {
    provider: &'a FulgurNetworkProvider,
    assets: Option<&'a AssetBundle>,
    cache: SharedResourceCache,
    warnings: Arc<Mutex<Vec<RenderWarning>>>,
}

impl FulgurImageResolver<'_> {
    fn load_image(&self, url: &Url) -> std::result::Result<Vec<u8>, NetworkError> {
        if let Some(bytes) = self.asset_image(url) {
            self.provider
                .account_resource_bytes(ResourceKind::Image, bytes.len() as u64)?;
            return Ok(bytes.as_ref().clone());
        }
        self.provider.fetch_bytes(url, ResourceKind::Image)
    }

    fn asset_image(&self, url: &Url) -> Option<Arc<Vec<u8>>> {
        let assets = self.assets?;
        #[cfg(target_arch = "wasm32")]
        {
            if url.scheme() == "data" {
                return None;
            }
            return assets
                .get_image(url.path().trim_start_matches('/'))
                .cloned();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if url.scheme() != "file" {
                return None;
            }
            let path = url.to_file_path().ok()?;
            if let Some(root) = &self.provider.canonical_root {
                let relative = path.strip_prefix(root).ok()?;
                if relative
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
                {
                    return None;
                }
                let key = relative.to_string_lossy().replace('\\', "/");
                return assets.get_image(&key).cloned();
            }
            assets
                .get_image(path.to_string_lossy().trim_start_matches('/'))
                .cloned()
        }
    }
}

impl ReplacedResolver for FulgurImageResolver<'_> {
    fn resolve(
        &self,
        request: ResolverRequest<'_>,
    ) -> std::result::Result<ResolvedIntrinsic, ResolverError> {
        let url = request.url();
        let bytes = match self.load_image(url) {
            Ok(bytes) => bytes,
            Err(_) => {
                push_resource_warning(&self.warnings, ResourceKind::Image, Some(url.clone()));
                return Ok(ResolvedIntrinsic {
                    intrinsic: raikiri_traits::IntrinsicBox::new(0.0, 0.0),
                    disposition: ResolveDisposition::Fallback {
                        reason: "image resource unavailable".into(),
                    },
                });
            }
        };
        self.cache.insert_raw(url.clone(), bytes.clone());
        let (width, height, decoded) = match AssetKind::detect(&bytes) {
            AssetKind::Raster(_) => match image::load_from_memory(&bytes) {
                Ok(image) => {
                    let image = image.to_rgba8();
                    let width = image.width();
                    let height = image.height();
                    (
                        width as f32,
                        height as f32,
                        Some(DecodedImage {
                            width,
                            height,
                            rgba: image.into_raw(),
                        }),
                    )
                }
                Err(_) => {
                    push_resource_warning(&self.warnings, ResourceKind::Image, Some(url.clone()));
                    return Ok(ResolvedIntrinsic {
                        intrinsic: raikiri_traits::IntrinsicBox::new(0.0, 0.0),
                        disposition: ResolveDisposition::Fallback {
                            reason: "raster image bytes could not be decoded".into(),
                        },
                    });
                }
            },
            AssetKind::Svg => match usvg::Tree::from_data(&bytes, &usvg::Options::default()) {
                Ok(tree) => (tree.size().width(), tree.size().height(), None),
                Err(_) => {
                    push_resource_warning(&self.warnings, ResourceKind::Svg, Some(url.clone()));
                    return Ok(ResolvedIntrinsic {
                        intrinsic: raikiri_traits::IntrinsicBox::new(0.0, 0.0),
                        disposition: ResolveDisposition::Fallback {
                            reason: "SVG image bytes could not be parsed".into(),
                        },
                    });
                }
            },
            AssetKind::Unknown => {
                push_resource_warning(&self.warnings, ResourceKind::Image, Some(url.clone()));
                return Ok(ResolvedIntrinsic {
                    intrinsic: raikiri_traits::IntrinsicBox::new(0.0, 0.0),
                    disposition: ResolveDisposition::Fallback {
                        reason: "image format is unsupported".into(),
                    },
                });
            }
        };
        if let Some(decoded) = decoded {
            self.cache.insert_decoded(url.clone(), decoded);
        }
        Ok(ResolvedIntrinsic {
            intrinsic: raikiri_traits::IntrinsicBox::new(width, height),
            disposition: ResolveDisposition::Ok,
        })
    }
}

impl ImagePixelSource for FulgurImageResolver<'_> {
    fn get_decoded(&self, url: &Url) -> Option<Arc<DecodedImage>> {
        self.cache.decoded(url)
    }
}

/// Mirror the umbrella's stylesheet source ordering while retaining the
/// FontFaceRegistry needed by Fulgur's font loader, without depending on the
/// Raikiri umbrella (which also pulls its validation paint scene).
fn build_rule_tree(document: &UncascadedDocument) -> RuleTree {
    let mut rules = RuleTree::empty();
    for (source, kind) in document.dom.stylesheets() {
        let origin = match kind {
            StylesheetKind::UserAgent => Origin::UserAgent,
            StylesheetKind::User => Origin::User,
            StylesheetKind::Author => Origin::Author,
            _ => unreachable!("new Raikiri StylesheetKind requires an origin mapping"),
        };
        rules.add_stylesheet(source, origin);
    }
    for source in &document.stylesheet_sources {
        rules.add_stylesheet(source, Origin::Author);
    }
    rules
}

/// Parse, cascade, load fonts/images, and paginate one Fulgur document through
/// Raikiri. The matching mutable document and cascade are returned together.
pub(crate) fn layout_document(
    html: &str,
    author_css: &str,
    assets: Option<&AssetBundle>,
    base_path: Option<&Path>,
    system_fonts: bool,
    font_data: &[Arc<Vec<u8>>],
    page_size: PageSize,
    page_margin: Margin,
    viewport_width_px: f32,
    viewport_height_px: f32,
) -> Result<RaikiriLayoutStage> {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let provider = FulgurNetworkProvider::new(base_path, warnings.clone());
    let base_url = base_path
        .and_then(crate::blitz_adapter::canonical_directory_url)
        .and_then(|url| Url::parse(&url).ok());

    let html_with_author_css = append_head_stylesheet(html, author_css);
    // The extra stylesheet carries Fulgur's resolved page box as a final
    // author-origin sheet. This keeps Raikiri's viewport/margins aligned with
    // Engine's existing @page resolution while preserving the document CSS
    // origin and source order for all ordinary rules.
    let page_css = resolved_page_css(page_size, page_margin);
    let options = ParseOptions {
        extra_stylesheets: &[],
        network: Some(&provider),
        base_url: base_url.clone(),
    };
    let mut uncascaded = parse(html_with_author_css.as_bytes(), &options)
        .map_err(|error| Error::HtmlParse(error.to_string()))?;
    uncascaded.stylesheet_sources.push(page_css);
    let effective_base_url = effective_document_base_url(&uncascaded, base_url.as_ref());

    let mut page_query = PageContextQuery::default();
    page_query.is_first = true;
    page_query.is_right = true;
    let width = (viewport_width_px.max(1.0).ceil() as u32).max(1);
    let height = (viewport_height_px.max(1.0).ceil() as u32).max(1);
    let media = MediaContext::with_viewport(MediaType::Print, width, height);
    // Build the rule tree explicitly so the same stylesheet registry supplies
    // both the cascade and @font-face loading; rebuilding it would duplicate
    // CSS parsing and risk diverging source order.
    let rules = build_rule_tree(&uncascaded);
    let font_faces: FontFaceRegistry = rules.font_faces().clone();
    let cascade = raikiri_style::cascade_with_media_context_for_page(
        &uncascaded.dom,
        &rules,
        &media,
        &page_query,
    )
    .map_err(|error| Error::Layout(error.to_string()))?;
    let mut cascade = cascade;
    let mut font_context = build_font_context(system_fonts, font_data);
    let font_loader = FulgurFontLoader {
        provider: &provider,
        base_url: effective_base_url.as_ref(),
        warnings: warnings.clone(),
    };
    let font_report = apply_font_faces(
        &mut font_context,
        &mut cascade.computed,
        &font_faces,
        &font_loader,
    );
    for family in &font_report.skipped {
        push_resource_warning(&warnings, ResourceKind::Font, None);
        log::warn!("Raikiri skipped an unavailable @font-face family: {family}");
    }

    let page_box = PageBox::from_page_size(cascade.page.size());
    let resources = SharedResourceCache::default();
    let resolver = FulgurImageResolver {
        provider: &provider,
        assets,
        cache: resources.clone(),
        warnings: warnings.clone(),
    };
    let mut document = uncascaded.dom;
    let pages = layout_pages_with_resolver_and_base_url(
        &mut document,
        &cascade,
        page_box,
        font_context,
        &resolver,
        effective_base_url.as_ref(),
    )
    .map_err(|error| Error::Layout(error.to_string()))?;

    let mut all_warnings = uncascaded.warnings;
    all_warnings.extend(warnings.lock().unwrap_or_else(|e| e.into_inner()).drain(..));
    Ok(RaikiriLayoutStage {
        document,
        cascade,
        pages,
        page_box,
        effective_base_url,
        resources,
        warnings: all_warnings,
        font_face_report: font_report,
    })
}

fn build_font_context(system_fonts: bool, bundled_fonts: &[Arc<Vec<u8>>]) -> FontContext {
    let collection =
        parley_engine::fontique::Collection::new(parley_engine::fontique::CollectionOptions {
            system_fonts,
            ..Default::default()
        });
    let mut context = FontContext {
        collection,
        source_cache: parley_engine::fontique::SourceCache::new(Default::default()),
    };
    for bytes in bundled_fonts {
        let blob: parley_engine::fontique::Blob<u8> = (**bytes).clone().into();
        context.collection.register_fonts(blob, None);
    }
    context
}

fn resolved_page_css(size: PageSize, margin: Margin) -> String {
    let px = |points: f32| points.as_pt().in_px().to_f32();
    format!(
        "@page {{ size: {:.4}px {:.4}px !important; margin-top: {:.4}px !important; margin-right: {:.4}px !important; margin-bottom: {:.4}px !important; margin-left: {:.4}px !important; }}",
        px(size.width),
        px(size.height),
        px(margin.top),
        px(margin.right),
        px(margin.bottom),
        px(margin.left),
    )
}

fn append_head_stylesheet(html: &str, css: &str) -> String {
    if css.trim().is_empty() {
        return html.to_owned();
    }
    let safe_css = escape_style_end_tag(css);
    let style = format!("<style>{safe_css}</style>");
    if let Some((start, _)) = find_html_tag(html, "head", true) {
        return format!("{}{}{}", &html[..start], style, &html[start..]);
    }
    if let Some((_, end)) = find_html_tag(html, "head", false) {
        return format!("{}{}{}", &html[..end], style, &html[end..]);
    }
    if let Some((_, end)) = find_html_tag(html, "html", true) {
        return format!("{}<head>{style}</head>{}", &html[..end], &html[end..]);
    }
    format!("<head>{style}</head>{html}")
}

/// Locate an HTML start/end tag without matching comments or quoted `>` bytes.
fn find_html_tag(source: &str, wanted: &str, closing: bool) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let rel = bytes[cursor..].iter().position(|byte| *byte == b'<')?;
        let start = cursor + rel;
        if bytes.get(start..start + 4) == Some(b"<!--") {
            let end = source[start + 4..].find("-->")? + start + 7;
            cursor = end;
            continue;
        }
        let mut name_start = start + 1;
        let is_closing = bytes.get(name_start) == Some(&b'/');
        if is_closing {
            name_start += 1;
        }
        if is_closing != closing {
            cursor = start + 1;
            continue;
        }
        while bytes.get(name_start).is_some_and(u8::is_ascii_whitespace) {
            name_start += 1;
        }
        let name_end = name_start
            + bytes[name_start..]
                .iter()
                .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'-')?;
        if !source[name_start..name_end].eq_ignore_ascii_case(wanted) {
            cursor = name_end;
            continue;
        }
        let mut quote = None;
        let mut end = name_end;
        while end < bytes.len() {
            match (quote, bytes[end]) {
                (Some(quote_char), byte) if byte == quote_char => quote = None,
                (None, b'\'' | b'"') => quote = Some(bytes[end]),
                (None, b'>') => return Some((start, end + 1)),
                _ => {}
            }
            end += 1;
        }
        return None;
    }
    None
}

fn escape_style_end_tag(css: &str) -> String {
    let lower = css.to_ascii_lowercase();
    let mut output = String::with_capacity(css.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("</style") {
        let start = cursor + relative;
        output.push_str(&css[cursor..start + 1]);
        output.push_str("\\/");
        cursor = start + 2;
    }
    output.push_str(&css[cursor..]);
    output
}

fn resolve_url(source: &str, base: Option<&Url>) -> Option<Url> {
    Url::parse(source).ok().or_else(|| base?.join(source).ok())
}

fn decode_data_url(url: &str) -> Option<Vec<u8>> {
    let (metadata, payload) = url.strip_prefix("data:")?.split_once(',')?;
    if metadata
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        let decoded_payload = percent_decode(payload)?;
        return base64::engine::general_purpose::STANDARD
            .decode(decoded_payload)
            .ok();
    }
    percent_decode(payload)
}

fn percent_decode(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%' {
            let hi = *bytes.get(cursor + 1)?;
            let lo = *bytes.get(cursor + 2)?;
            output.push((hex_value(hi)? << 4) | hex_value(lo)?);
            cursor += 3;
        } else {
            output.push(bytes[cursor]);
            cursor += 1;
        }
    }
    Some(output)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn mime_for_url(url: &Url) -> Option<String> {
    let extension = Path::new(url.path())
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    Some(
        match extension.as_str() {
            "css" => "text/css",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            "ttf" => "font/ttf",
            "otf" => "font/otf",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            _ => return None,
        }
        .to_owned(),
    )
}

fn push_resource_warning(
    warnings: &Arc<Mutex<Vec<RenderWarning>>>,
    kind: ResourceKind,
    url: Option<Url>,
) {
    if let Ok(mut warnings) = warnings.lock() {
        warnings.push(RenderWarning {
            kind: WarningKind::ResourceFallback { kind, url },
            node_id: None,
            details: "resource unavailable; fallback used".into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_layout(
        html: &str,
        author_css: &str,
        base_path: &Path,
        assets: Option<&AssetBundle>,
        page_size: PageSize,
        page_margin: Margin,
    ) -> RaikiriLayoutStage {
        layout_document(
            html,
            author_css,
            assets,
            Some(base_path),
            false,
            &[],
            page_size,
            page_margin,
            400.0,
            500.0,
        )
        .expect("Raikiri parse/cascade/layout should succeed")
    }

    fn element_index(document: &raikiri_dom::Document, tag: &str, id: Option<&str>) -> usize {
        use raikiri_traits::{Dom as _, Element as _, Node as _};
        let mut pending = vec![document.root_id()];
        while let Some(node_id) = pending.pop() {
            let node = document.node(node_id).expect("node id in arena");
            if let Some(element) = node.as_element()
                && element.tag_name() == tag
                && id.is_none_or(|wanted| element.id() == Some(wanted))
            {
                return node_id.0 as usize;
            }
            pending.extend(document.child_ids(node_id));
        }
        panic!("element <{tag}> id={id:?} not found");
    }

    fn element_class_index(document: &raikiri_dom::Document, tag: &str, class: &str) -> usize {
        use raikiri_traits::{Dom as _, Element as _, Node as _};
        let mut pending = vec![document.root_id()];
        while let Some(node_id) = pending.pop() {
            let node = document.node(node_id).expect("node id in arena");
            if let Some(element) = node.as_element()
                && element.tag_name() == tag
                && element.has_class(class)
            {
                return node_id.0 as usize;
            }
            pending.extend(document.child_ids(node_id));
        }
        panic!("element <{tag}> class={class:?} not found");
    }

    #[test]
    fn data_url_decodes_base64_and_percent_bytes() {
        assert_eq!(
            decode_data_url("data:text/plain;base64,SGk=").unwrap(),
            b"Hi"
        );
        assert_eq!(
            decode_data_url("data:text/plain,hi%20there").unwrap(),
            b"hi there"
        );
        assert!(decode_data_url("data:text/plain,%Q0").is_none());
    }

    #[test]
    fn bundle_css_is_inserted_after_existing_head_content() {
        let html = "<html><head><style>p{color:red}</style><link rel=stylesheet href=x.css></head><body><style>p{color:blue}</style></body></html>";
        let inserted = append_head_stylesheet(html, "p{font-size:13px}");
        assert!(inserted.find("href=x.css").unwrap() < inserted.find("font-size:13px").unwrap());
        assert!(inserted.find("font-size:13px").unwrap() < inserted.find("color:blue").unwrap());
    }

    #[test]
    fn bundle_css_is_not_injected_through_a_comment_or_css_close_tag() {
        let html = "<!-- <head>fake</head> --><body>x</body>";
        let inserted = append_head_stylesheet(html, "p{content:'</style>'}");
        assert!(inserted.contains("<head><style>p{content:'<\\/style>'}</style></head>"));
    }

    #[test]
    fn document_link_import_base_and_bundle_styles_keep_author_order() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("sub");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join("main.css"),
            "@import 'nested.css'; .asset { font-size: 14px }",
        )
        .unwrap();
        std::fs::write(base.join("nested.css"), "#imported { font-size: 16px }").unwrap();
        let mut assets = AssetBundle::new();
        assets.add_css(
            ".asset { font-size: 20px } .important { font-size: 22px } \
             .bundle-important { font-size: 24px !important }",
        );
        let html = r#"<!doctype html><html><head><base href="sub/">
            <style>.asset { font-size: 10px } .important { font-size: 11px !important }
            .bundle-important { font-size: 13px !important }</style>
            <link rel="stylesheet" href="main.css"></head><body>
            <p class="asset">bundle wins</p><p class="important">important wins</p>
            <p class="bundle-important">same-origin important source order</p>
            <p id="imported">nested import</p></body></html>"#;
        let stage = test_layout(
            html,
            &assets.combined_css(),
            root.path(),
            Some(&assets),
            PageSize::A4,
            Margin::default(),
        );
        let base_url = stage.effective_base_url.as_ref().expect("base element URL");
        assert!(base_url.as_str().ends_with("/sub/"));
        let asset = element_class_index(&stage.document, "p", "asset");
        let important = {
            use raikiri_traits::{Dom as _, Element as _, Node as _};
            let mut stack = vec![stage.document.root_id()];
            let mut result = None;
            while let Some(node_id) = stack.pop() {
                let node = stage.document.node(node_id).unwrap();
                if let Some(element) = node.as_element()
                    && element.tag_name() == "p"
                    && element.has_class("important")
                {
                    result = Some(node_id.0 as usize);
                    break;
                }
                stack.extend(stage.document.child_ids(node_id));
            }
            result.unwrap()
        };
        let imported = element_index(&stage.document, "p", Some("imported"));
        let bundle_important = {
            use raikiri_traits::{Dom as _, Element as _, Node as _};
            let mut stack = vec![stage.document.root_id()];
            let mut result = None;
            while let Some(node_id) = stack.pop() {
                let node = stage.document.node(node_id).unwrap();
                if let Some(element) = node.as_element()
                    && element.tag_name() == "p"
                    && element.has_class("bundle-important")
                {
                    result = Some(node_id.0 as usize);
                    break;
                }
                stack.extend(stage.document.child_ids(node_id));
            }
            result.unwrap()
        };
        assert_eq!(
            stage.cascade.computed[asset].display,
            raikiri_style::DisplayValue::Block
        );
        assert_eq!(stage.cascade.computed[asset].font_size.px(), 20.0);
        assert_eq!(stage.cascade.computed[important].font_size.px(), 11.0);
        assert_eq!(
            stage.cascade.computed[bundle_important].font_size.px(),
            24.0
        );
        assert_eq!(stage.cascade.computed[imported].font_size.px(), 16.0);
    }

    #[test]
    fn relative_font_face_and_image_use_the_effective_base_and_keep_paint_bytes() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("sub");
        std::fs::create_dir_all(&base).unwrap();
        let bundled_font = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/.fonts/NotoSans-Regular.ttf");
        std::fs::copy(bundled_font, base.join("font.ttf")).unwrap();
        let image_bytes = {
            let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([240, 10, 20, 255]));
            let mut cursor = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image)
                .write_to(&mut cursor, image::ImageFormat::Png)
                .unwrap();
            cursor.into_inner()
        };
        std::fs::write(base.join("photo.png"), &image_bytes).unwrap();
        let svg_bytes = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="5"><rect width="4" height="5" fill="red"/></svg>"#.to_vec();
        std::fs::write(base.join("vector.svg"), &svg_bytes).unwrap();
        let mut assets = AssetBundle::new();
        assets.add_image("sub/bundled.png", image_bytes.clone());
        assets.add_image("sub/bundled.svg", svg_bytes.clone());
        let html = r#"<!doctype html><html><head><base href="sub/">
            <style>@font-face { font-family: BridgeFont; src: url("font.ttf") }
            p { font-family: BridgeFont }</style></head><body>
            <p>font loaded</p><img src="photo.png"><img src="bundled.png">
            <img src="vector.svg"><img src="bundled.svg"></body></html>"#;
        let stage = test_layout(
            html,
            "",
            root.path(),
            Some(&assets),
            PageSize::A4,
            Margin::default(),
        );
        assert!(
            stage
                .font_face_report
                .applied
                .iter()
                .any(|family| family == "BridgeFont")
        );
        let base_url = stage.effective_base_url.as_ref().unwrap();
        let image_url = base_url.join("photo.png").unwrap();
        assert_eq!(
            stage.resources.raw_bytes(&image_url).unwrap().as_slice(),
            image_bytes
        );
        let decoded = stage.resources.decoded(&image_url).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 3));
        let bundled_url = base_url.join("bundled.png").unwrap();
        assert_eq!(
            stage.resources.raw_bytes(&bundled_url).unwrap().as_slice(),
            image_bytes
        );
        let vector_url = base_url.join("vector.svg").unwrap();
        assert_eq!(
            stage.resources.raw_bytes(&vector_url).unwrap().as_slice(),
            svg_bytes
        );
        let bundled_svg_url = base_url.join("bundled.svg").unwrap();
        assert_eq!(
            stage
                .resources
                .raw_bytes(&bundled_svg_url)
                .unwrap()
                .as_slice(),
            svg_bytes
        );
        let fragments = raikiri_dom::page_fragments_from_slices(
            &stage.document,
            &stage.cascade,
            stage.page_box,
            &stage.pages,
        );
        let image = fragments
            .iter()
            .flat_map(|page| &page.items)
            .find(|item| item.kind == raikiri_traits::PageFragmentKind::Replaced)
            .expect("img has a layout fragment");
        assert!((image.rect.width - 2.0).abs() < 0.01);
        assert!((image.rect.height - 3.0).abs() < 0.01);
        let svg = fragments
            .iter()
            .flat_map(|page| &page.items)
            .find(|item| {
                item.kind == raikiri_traits::PageFragmentKind::Replaced
                    && (item.rect.width - 4.0).abs() < 0.01
                    && (item.rect.height - 5.0).abs() < 0.01
            })
            .expect("SVG intrinsic size comes from its view box/size");
        assert_eq!((svg.rect.width, svg.rect.height), (4.0, 5.0));
    }

    #[test]
    fn missing_resources_warn_and_layout_continues() {
        let root = tempfile::tempdir().unwrap();
        let html = r#"<!doctype html><html><head><link rel="stylesheet" href="missing.css"></head>
            <body><p>still lays out</p><img src="missing.png"></body></html>"#;
        let stage = test_layout(html, "", root.path(), None, PageSize::A4, Margin::default());
        assert_eq!(stage.pages.len(), 1);
        assert!(
            stage
                .warnings
                .iter()
                .any(|warning| matches!(&warning.kind, WarningKind::NetworkFallback { .. }))
        );
        assert!(stage.warnings.iter().any(|warning| matches!(
            &warning.kind,
            WarningKind::ResourceFallback {
                kind: ResourceKind::Image,
                ..
            }
        )));
    }

    #[test]
    fn local_provider_enforces_root_and_per_resource_byte_limits() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let provider = FulgurNetworkProvider::new(Some(root.path()), warnings.clone());
        let outside_path = outside.path().join("outside.css");
        std::fs::write(&outside_path, b"p { color: red }").unwrap();
        let outside_url = Url::from_file_path(outside_path).unwrap();
        assert!(
            provider
                .read_url(&outside_url, ResourceKind::ExternalStylesheet)
                .is_err()
        );

        let oversized = root.path().join("oversized.css");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len(CSS_RESOURCE_LIMIT + 1)
            .unwrap();
        let oversized_url = Url::from_file_path(oversized).unwrap();
        assert!(
            provider
                .read_url(&oversized_url, ResourceKind::ExternalStylesheet)
                .is_err()
        );
        let warnings = warnings.lock().unwrap();
        assert!(warnings.iter().any(|warning| matches!(
            &warning.kind,
            WarningKind::ResourceLimitExceeded {
                kind: ResourceKind::ExternalStylesheet,
                limit: CSS_RESOURCE_LIMIT,
                actual,
            } if *actual == CSS_RESOURCE_LIMIT + 1
        )));
        drop(warnings);
        *provider.bytes_read.lock().unwrap() = TOTAL_PROVIDER_RESOURCE_LIMIT;
        let data_url = Url::parse("data:text/plain,over-budget").unwrap();
        assert!(
            provider
                .fetch_bytes(&data_url, ResourceKind::Image)
                .is_err()
        );
    }

    #[test]
    fn resolved_engine_page_box_margin_viewport_and_single_page_layout_are_used() {
        let root = tempfile::tempdir().unwrap();
        let margin = Margin {
            top: 36.0,
            right: 36.0,
            bottom: 36.0,
            left: 36.0,
        };
        let html = r#"<!doctype html><html><head><style>@page { size: 300px 400px; margin: 1px }</style></head>
            <body><div id="viewport" style="width:100vw;height:10px">one page</div></body></html>"#;
        let stage = test_layout(html, "", root.path(), None, PageSize::LETTER, margin);
        assert_eq!(stage.pages.len(), 1, "single-page smoke layout");
        assert_eq!(stage.cascade.computed.len(), stage.document.node_count());
        assert!((stage.page_box.width - 816.0).abs() < 0.01);
        assert!((stage.page_box.height - 1056.0).abs() < 0.01);
        let fragments = raikiri_dom::page_fragments_from_slices(
            &stage.document,
            &stage.cascade,
            stage.page_box,
            &stage.pages,
        );
        assert!((fragments[0].margins.left - 48.0).abs() < 0.01);
        let viewport = fragments[0]
            .items
            .iter()
            .find(|item| {
                item.node_id.0 == element_index(&stage.document, "div", Some("viewport")) as u64
            })
            .expect("viewport node fragment");
        assert!((viewport.rect.width - 720.0).abs() < 0.1);
    }
}
