use raikiri::{FontContext, PageBox, ParseOptions};
use raikiri_traits::{
    IntrinsicBox, PageDefaults, PageFragment, RenderError, RenderSink, RenderStatus, RenderSummary,
    ReplacedResolver, ResolveDisposition, ResolvedIntrinsic, ResolverError, ResolverRequest,
    StreamingConfig,
};
use serde::Serialize;
use std::fmt;
use std::io;

/// Error returned by the diagnostic probe.
#[derive(Debug)]
pub struct ProbeError(String);

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProbeError {}

/// Page-level information currently exposed by raikiri's public pagination API.
#[derive(Debug, Serialize)]
pub struct PageReport {
    pub page_index: u32,
    pub content_origin_y: f32,
    pub page_name: Option<String>,
}

/// Deterministic report from the direct `layout_pages` probe.
#[derive(Debug, Serialize)]
pub struct LayoutReport {
    pub pages: Vec<PageReport>,
}

/// Deterministic report from the public streaming API probe.
#[derive(Debug, Serialize)]
pub struct StreamingReport {
    pub status: String,
    pub pages_emitted: u32,
    pub finished: bool,
    pub feature: Option<String>,
    pub migration_hint: Option<String>,
}

/// Parse, cascade, and paginate one HTML document through raikiri's currently
/// public low-level page-layout API.
pub fn probe_layout(html: &str) -> Result<LayoutReport, ProbeError> {
    let options = ParseOptions {
        extra_stylesheets: &[],
        network: None,
        base_url: None,
    };
    let mut uncascaded = raikiri::parse(html.as_bytes(), &options)
        .map_err(|error| ProbeError(format!("parse failed: {error}")))?;
    let cascade = raikiri::build_cascaded(&uncascaded);
    let pages = raikiri_dom::layout_pages(
        &mut uncascaded.dom,
        &cascade,
        PageBox::A4,
        FontContext::new(),
    )
    .map_err(|error| ProbeError(format!("layout failed: {error}")))?;

    Ok(LayoutReport {
        pages: pages
            .into_iter()
            .map(|page| PageReport {
                page_index: page.page_index,
                content_origin_y: page.content_origin_y,
                page_name: page.page_name,
            })
            .collect(),
    })
}

/// Call raikiri's consumer-facing streaming entry point and preserve the
/// current unavailable-surface result for the evidence report.
pub fn probe_streaming(html: &str) -> Result<StreamingReport, ProbeError> {
    let options = ParseOptions {
        extra_stylesheets: &[],
        network: None,
        base_url: None,
    };
    let document = raikiri::parse_html(html.as_bytes(), &options)
        .map_err(|error| ProbeError(format!("parse failed: {error}")))?;
    let resolver = FallbackResolver;
    let mut sink = RecordingSink::default();
    let result = raikiri::render_streaming(
        &document,
        PageDefaults::default(),
        &resolver,
        StreamingConfig::default(),
        &mut sink,
    );

    match result {
        Err(RenderError::Unimplemented {
            feature,
            migration_hint,
        }) => Ok(StreamingReport {
            status: "unimplemented".to_owned(),
            pages_emitted: sink.pages_emitted,
            finished: sink.finished,
            feature: Some(feature.to_owned()),
            migration_hint: Some(migration_hint.to_owned()),
        }),
        Ok(RenderStatus::Completed(_)) => Ok(StreamingReport {
            status: "completed".to_owned(),
            pages_emitted: sink.pages_emitted,
            finished: sink.finished,
            feature: None,
            migration_hint: None,
        }),
        Ok(RenderStatus::Aborted { .. }) => Ok(StreamingReport {
            status: "aborted".to_owned(),
            pages_emitted: sink.pages_emitted,
            finished: sink.finished,
            feature: None,
            migration_hint: None,
        }),
        Ok(_) => Ok(StreamingReport {
            status: "other".to_owned(),
            pages_emitted: sink.pages_emitted,
            finished: sink.finished,
            feature: None,
            migration_hint: None,
        }),
        Err(error) => Err(ProbeError(format!("unexpected streaming result: {error}"))),
    }
}

struct FallbackResolver;

impl ReplacedResolver for FallbackResolver {
    fn resolve(&self, _request: ResolverRequest<'_>) -> Result<ResolvedIntrinsic, ResolverError> {
        Ok(ResolvedIntrinsic {
            intrinsic: IntrinsicBox::new(0.0, 0.0),
            disposition: ResolveDisposition::Fallback {
                reason: "consumer-surface probe does not resolve images".to_owned(),
            },
        })
    }
}

#[derive(Default)]
struct RecordingSink {
    pages_emitted: u32,
    finished: bool,
}

impl RenderSink for RecordingSink {
    fn accept_page(&mut self, _page: PageFragment) -> io::Result<()> {
        self.pages_emitted += 1;
        Ok(())
    }

    fn finish_render(&mut self, _summary: RenderSummary) -> io::Result<()> {
        self.finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{probe_layout, probe_streaming};

    const SHORT_HTML: &str = "<!doctype html><html><body><p>hello</p></body></html>";
    const FORCED_BREAK_HTML: &str = r#"<!doctype html><html><head><style>
        @page { margin: 0 }
        body { margin: 0 }
        .first { height: 20px; break-after: page }
        .second { height: 20px }
    </style></head><body>
        <div class="first">first</div><div class="second">second</div>
    </body></html>"#;

    #[test]
    fn short_document_has_one_page_at_origin() {
        let report = probe_layout(SHORT_HTML).expect("short document should layout");
        assert_eq!(report.pages.len(), 1);
        assert_eq!(report.pages[0].page_index, 0);
        assert_eq!(report.pages[0].content_origin_y, 0.0);
    }

    #[test]
    fn forced_break_reports_ordered_page_origins() {
        let report = probe_layout(FORCED_BREAK_HTML).expect("forced break should layout");
        assert_eq!(report.pages.len(), 2);
        assert_eq!(report.pages[0].page_index, 0);
        assert_eq!(report.pages[1].page_index, 1);
        assert!(report.pages[1].content_origin_y > report.pages[0].content_origin_y);
    }

    #[test]
    fn streaming_surface_is_characterized_as_unimplemented() {
        let report = probe_streaming(SHORT_HTML).expect("streaming probe should complete");
        assert_eq!(report.pages_emitted, 0);
        assert!(!report.finished);
        assert_eq!(report.feature.as_deref(), Some("render_streaming"));
    }
}
