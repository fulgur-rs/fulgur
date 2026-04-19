/// Maximum DOM tree depth before recursion is cut off. Prevents stack overflow
/// from pathologically deep HTML input.
pub(crate) const MAX_DOM_DEPTH: usize = 512;

pub mod asset;
pub mod background;
pub mod blitz_adapter;
pub mod config;
pub mod convert;
pub mod engine;
pub mod error;
pub mod gcpm;
pub mod image;
pub(crate) mod link;
pub mod multicol_layout;
pub(crate) mod net;
pub mod outline;
pub mod pageable;
pub mod paginate;
pub mod paragraph;
pub mod render;
pub mod schema;
pub mod svg;
pub mod template;

pub use asset::AssetBundle;
pub use config::{Config, ConfigBuilder, Margin, PageSize};
pub use engine::{Engine, EngineBuilder};
pub use error::{Error, Result};
pub use outline::build_outline;

/// Convert HTML to PDF with default settings.
pub fn convert_html(html: &str) -> Result<Vec<u8>> {
    let engine = Engine::builder().build();
    engine.render_html(html)
}
