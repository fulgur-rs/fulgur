//! Chromium golden generation (feature `chrome-golden`).
//!
//! Each fixture is opened in a pinned Chrome for Testing build and printed
//! to PDF with `Page.printToPDF`, using the fixture's page size and margin.
//! The PDFs are committed under `goldens/chrome/` and rasterized with the
//! same `pdftocairo` path as the fulgur renders, so Chromium's own
//! rasterizer never enters a comparison and comparing needs no browser.

use crate::manifest::{Fixture, Manifest};
use crate::runner::RunnerContext;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::{
    FontFamilies, PrintToPdfParams, SetFontFamiliesParams,
};
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Pinned Chrome for Testing version. The golden update workflow installs
/// exactly this build; bump it only in the change that regenerates
/// `goldens/chrome/`, so the references stay in lockstep with the browser
/// that produced them.
pub const CHROME_VERSION: &str = "155.0.8059.84";

/// Environment variable naming the Chrome executable to launch. Unset means
/// chromiumoxide's own detection.
pub const CHROME_ENV: &str = "FULGUR_VRT_CHROME";

/// Points per inch, the unit of `printToPDF` paper and margin sizes.
const PT_PER_INCH: f64 = 72.0;

/// `printToPDF` parameters for a fixture.
///
/// The paper size always comes from the fixture, and the margin from its
/// `margin_pt`, defaulting to fulgur's 20 mm; an `@page` margin in the
/// fixture wins over that default in both renderers. These parameters alone
/// lose to the fixture's own `@page` rules, so [`page_override_css`] carries
/// the same overrides into the page.
pub fn print_params(fx: &Fixture) -> anyhow::Result<PrintToPdfParams> {
    let size = fulgur::config::PageSize::from_css_keyword(&fx.page_size)
        .ok_or_else(|| anyhow::anyhow!("unsupported page_size: {}", fx.page_size))?;
    let margin = match fx.margin_pt {
        Some(pt) => fulgur::config::Margin::uniform(pt),
        None => fulgur::config::Margin::default(),
    };
    let inches = |pt: f32| f64::from(pt) / PT_PER_INCH;
    Ok(PrintToPdfParams::builder()
        .print_background(true)
        .display_header_footer(false)
        .prefer_css_page_size(false)
        .paper_width(inches(size.width))
        .paper_height(inches(size.height))
        .margin_top(inches(margin.top))
        .margin_right(inches(margin.right))
        .margin_bottom(inches(margin.bottom))
        .margin_left(inches(margin.left))
        .build())
}

/// Style sheet that gives the fixture's page size, and its `margin_pt` when
/// set, precedence over the fixture's own `@page` rules, as the Blitz
/// renderer used by the VRT does. Without it Chrome takes an `@page` margin
/// as is and reads an `@page` size as an orientation for the paper.
pub fn page_override_css(fx: &Fixture) -> anyhow::Result<String> {
    let size = fulgur::config::PageSize::from_css_keyword(&fx.page_size)
        .ok_or_else(|| anyhow::anyhow!("unsupported page_size: {}", fx.page_size))?;
    let margin = fx
        .margin_pt
        .map(|pt| format!(" margin: {pt}pt !important;"))
        .unwrap_or_default();
    Ok(format!(
        "@page {{ size: {}pt {}pt !important;{margin} }}",
        size.width, size.height
    ))
}

/// The concrete family fontconfig resolves `generic` to on this host.
fn fontconfig_family(generic: &str) -> anyhow::Result<String> {
    let out = Command::new("fc-match")
        .args(["-f", "%{family[0]}", generic])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to spawn fc-match: {e}"))?;
    anyhow::ensure!(
        out.status.success(),
        "fc-match {generic} exited with {}",
        out.status
    );
    let family = String::from_utf8(out.stdout)?.trim().to_string();
    anyhow::ensure!(!family.is_empty(), "fc-match {generic} returned no family");
    Ok(family)
}

/// Generic font families for Chrome, resolved through fontconfig as fulgur
/// resolves them. Chrome on Linux keeps its own defaults instead (Arial for
/// sans-serif, Times New Roman for the standard font), which would make
/// every text difference a font difference. The standard font is the
/// sans-serif one, as fulgur's initial font is.
pub fn font_families() -> anyhow::Result<FontFamilies> {
    let sans_serif = fontconfig_family("sans-serif")?;
    Ok(FontFamilies {
        standard: Some(sans_serif.clone()),
        sans_serif: Some(sans_serif),
        serif: Some(fontconfig_family("serif")?),
        fixed: Some(fontconfig_family("monospace")?),
        ..Default::default()
    })
}

/// Location of the Chromium golden for `fixture`.
pub fn golden_path(ctx: &RunnerContext, fixture: &Path) -> PathBuf {
    ctx.goldens_dir
        .join("chrome")
        .join(fixture.with_extension("pdf"))
}

/// A running browser that prints fixtures to PDF.
pub struct ChromePrinter {
    browser: Browser,
    fonts: FontFamilies,
    handler: tokio::task::JoinHandle<()>,
}

impl ChromePrinter {
    /// Launch headless Chrome, from [`CHROME_ENV`] when it is set.
    pub async fn launch() -> anyhow::Result<Self> {
        let mut config = BrowserConfig::builder();
        if let Some(exe) = std::env::var_os(CHROME_ENV) {
            config = config.chrome_executable(exe);
        }
        let config = config.build().map_err(|e| anyhow::anyhow!(e))?;
        let (browser, mut handler) = Browser::launch(config).await?;
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
        Ok(Self {
            browser,
            fonts: font_families()?,
            handler,
        })
    }

    /// Browser product string, e.g. `Chrome/155.0.8059.84`.
    pub async fn product(&self) -> anyhow::Result<String> {
        Ok(self.browser.version().await?.product)
    }

    /// Open `html_path` and print it with the fixture's page setup.
    pub async fn print(&self, html_path: &Path, fx: &Fixture) -> anyhow::Result<Vec<u8>> {
        let url = format!("file://{}", html_path.canonicalize()?.display());
        let page = self.browser.new_page("about:blank").await?;
        page.execute(SetFontFamiliesParams::new(self.fonts.clone()))
            .await?;
        page.goto(url.as_str()).await?;
        let css = page_override_css(fx)?;
        let script = format!(
            "(() => {{ const s = document.createElement('style'); \
             s.textContent = {css:?}; document.head.appendChild(s); }})()"
        );
        page.evaluate(script).await?;
        let pdf = page.pdf(print_params(fx)?).await?;
        page.close().await?;
        Ok(pdf)
    }

    /// Close the browser and wait for its process to exit.
    pub async fn close(mut self) -> anyhow::Result<()> {
        self.browser.close().await?;
        self.browser.wait().await?;
        self.handler.abort();
        Ok(())
    }
}

/// Print every fixture that has a Chromium reference into `goldens/chrome/`,
/// and record the browser in `goldens/chrome/VERSION`.
///
/// Fails when the launched browser is not [`CHROME_VERSION`], so goldens
/// from another build cannot be committed by accident. Set
/// `FULGUR_VRT_CHROME_ANY_VERSION=1` to skip that check for a local trial.
pub fn generate_goldens(ctx: &RunnerContext) -> anyhow::Result<Vec<PathBuf>> {
    let manifest = Manifest::load(&ctx.crate_root.join("manifest.toml"))?;
    let any_version = std::env::var_os("FULGUR_VRT_CHROME_ANY_VERSION").is_some();
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let printer = ChromePrinter::launch().await?;
        let result = async {
            let product = printer.product().await?;
            anyhow::ensure!(
                any_version || product.ends_with(&format!("/{CHROME_VERSION}")),
                "launched {product}, but the goldens are pinned to Chrome {CHROME_VERSION} \
                 (set {CHROME_ENV} to that build)"
            );
            let mut written = Vec::new();
            for fx in manifest.fixtures.iter().filter(|fx| fx.chrome_reference) {
                let pdf = printer.print(&ctx.fixtures_dir.join(&fx.path), fx).await?;
                let out = golden_path(ctx, &fx.path);
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&out, pdf)?;
                written.push(out);
            }
            std::fs::write(
                ctx.goldens_dir.join("chrome").join("VERSION"),
                format!("{product}\n"),
            )?;
            Ok(written)
        }
        .await;
        printer.close().await?;
        result
    })
}
