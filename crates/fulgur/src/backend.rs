//! Selection of the layout backend used to render HTML.

#[cfg(feature = "raikiri")]
use crate::Error;
use crate::{Engine, Result};

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
    ///   path, asset bundle and system-font setting. It refuses, with an
    ///   `Error::Other`, an engine that has a template registered (Raikiri
    ///   does not render templates yet) or custom serialize settings (Raikiri
    ///   does not accept them). The template check comes first. With
    ///   `system_fonts(false)` and no bundled font, the Raikiri backend
    ///   returns an error, while Blitz renders.
    fn render_html_with(&self, backend: Backend, html: &str) -> Result<Vec<u8>>;
}

impl EngineExt for Engine {
    fn render_html_with(&self, backend: Backend, html: &str) -> Result<Vec<u8>> {
        match backend {
            Backend::Blitz => self.render(html),
            #[cfg(feature = "raikiri")]
            Backend::Raikiri => {
                if self.has_template() {
                    return Err(Error::Other(
                        "the Raikiri backend does not render templates yet; use Backend::Blitz"
                            .into(),
                    ));
                }
                if self.has_custom_serialize_settings() {
                    return Err(Error::Other(
                        "the Raikiri backend does not accept custom serialize settings; use Backend::Blitz"
                            .into(),
                    ));
                }
                fulgur_raikiri::render_html_with_options(
                    html,
                    self.base_path(),
                    self.config(),
                    &fulgur_raikiri::RenderOptions {
                        assets: self.assets(),
                        system_fonts: self.system_fonts(),
                        ..Default::default()
                    },
                )
            }
        }
    }
}
