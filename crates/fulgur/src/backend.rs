//! Selection of the layout backend used to render HTML.

#[cfg(feature = "raikiri")]
use crate::Error;
use crate::{Engine, PageLayout, Result};

/// The layout backend that turns HTML into a PDF.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Backend {
    /// The Blitz backend, which supports the full `Engine` feature set.
    /// This is the default.
    #[default]
    Blitz,
    /// The Raikiri backend, which is still under development.
    ///
    /// Available only with the `raikiri` feature.
    #[cfg(feature = "raikiri")]
    Raikiri,
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::Engine {}
}

/// Backend selection for [`Engine`].
///
/// This trait is sealed: it is implemented only for [`Engine`], so methods can
/// be added later without breaking implementors.
pub trait EngineExt: sealed::Sealed {
    /// Renders `html` to PDF bytes with the chosen backend.
    ///
    /// * [`Backend::Blitz`] is equivalent to [`Engine::render`].
    /// * `Backend::Raikiri` renders with the engine's configuration, base
    ///   path, asset bundle and system-font setting. Like
    ///   [`Engine::render`], it renders `html` and not a template registered
    ///   on the engine; use [`render_template_with`](Self::render_template_with)
    ///   for that. It refuses, with an `Error::Other`, an engine that has
    ///   custom serialize settings (Raikiri does not accept them). With
    ///   `system_fonts(false)` and no bundled font, the Raikiri backend
    ///   returns an error, while Blitz renders.
    fn render_html_with(&self, backend: Backend, html: &str) -> Result<Vec<u8>>;

    /// Expands the engine's template with its data and renders the result
    /// with the chosen backend.
    ///
    /// * [`Backend::Blitz`] is equivalent to [`Engine::render_template`].
    /// * `Backend::Raikiri` renders [`Engine::expand_template`] as
    ///   [`render_html_with`](Self::render_html_with) does.
    ///
    /// Either backend returns `Error::Template` when no template is set.
    fn render_template_with(&self, backend: Backend) -> Result<Vec<u8>>;

    /// Renders each HTML string with the chosen backend, returning one
    /// result per input in input order. A failure in one item does not
    /// abort the rest.
    ///
    /// * [`Backend::Blitz`] is equivalent to [`Engine::render_batch`].
    /// * `Backend::Raikiri` renders each item as
    ///   [`render_html_with`](Self::render_html_with) does.
    ///
    /// With the `parallel` feature, items run concurrently on rayon's global
    /// thread pool with either backend; without it they run in order.
    fn render_batch_with<S: AsRef<str> + Sync>(
        &self,
        backend: Backend,
        htmls: &[S],
    ) -> Vec<Result<Vec<u8>>>;

    /// Lays out `html` with the chosen backend and reports its pages: the
    /// page count and each page's size (orientation applied) and resolved
    /// margins, in PDF points.
    ///
    /// The pages are the ones [`render_html_with`](Self::render_html_with)
    /// produces with the same backend.
    ///
    /// * [`Backend::Blitz`] is equivalent to [`Engine::page_layout`].
    /// * `Backend::Raikiri` lays out with the same inputs, and the same
    ///   refusals, as `render_html_with`.
    ///
    /// The Blitz-only [`Engine::layout`] has no Raikiri counterpart: its
    /// output carries the Blitz backend's own drawing data.
    fn page_layout_with(&self, backend: Backend, html: &str) -> Result<PageLayout>;
}

/// The Raikiri rendering options for `engine`, or an error when the engine
/// asks for something Raikiri cannot honour.
#[cfg(feature = "raikiri")]
fn raikiri_options(engine: &Engine) -> Result<fulgur_raikiri::RenderOptions<'_>> {
    if engine.has_custom_serialize_settings() {
        return Err(Error::Other(
            "the Raikiri backend does not accept custom serialize settings; use Backend::Blitz"
                .into(),
        ));
    }
    Ok(fulgur_raikiri::RenderOptions {
        assets: engine.assets(),
        system_fonts: engine.system_fonts(),
        ..Default::default()
    })
}

impl EngineExt for Engine {
    fn render_html_with(&self, backend: Backend, html: &str) -> Result<Vec<u8>> {
        match backend {
            Backend::Blitz => self.render(html),
            #[cfg(feature = "raikiri")]
            Backend::Raikiri => fulgur_raikiri::render_html_with_options(
                html,
                self.base_path(),
                self.config(),
                &raikiri_options(self)?,
            ),
        }
    }

    fn render_template_with(&self, backend: Backend) -> Result<Vec<u8>> {
        match backend {
            Backend::Blitz => self.render_template(),
            #[cfg(feature = "raikiri")]
            Backend::Raikiri => self.render_html_with(backend, &self.expand_template()?),
        }
    }

    fn render_batch_with<S: AsRef<str> + Sync>(
        &self,
        backend: Backend,
        htmls: &[S],
    ) -> Vec<Result<Vec<u8>>> {
        match backend {
            Backend::Blitz => self.render_batch(htmls),
            #[cfg(feature = "raikiri")]
            Backend::Raikiri => {
                #[cfg(feature = "parallel")]
                {
                    use rayon::prelude::*;
                    htmls
                        .par_iter()
                        .map(|html| self.render_html_with(backend, html.as_ref()))
                        .collect()
                }
                #[cfg(not(feature = "parallel"))]
                {
                    htmls
                        .iter()
                        .map(|html| self.render_html_with(backend, html.as_ref()))
                        .collect()
                }
            }
        }
    }

    fn page_layout_with(&self, backend: Backend, html: &str) -> Result<PageLayout> {
        match backend {
            Backend::Blitz => self.page_layout(html),
            #[cfg(feature = "raikiri")]
            Backend::Raikiri => fulgur_raikiri::page_layout_html_with_options(
                html,
                self.base_path(),
                self.config(),
                &raikiri_options(self)?,
            ),
        }
    }
}
