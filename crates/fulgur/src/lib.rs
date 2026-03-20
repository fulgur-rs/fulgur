pub mod asset;
pub mod blitz_adapter;
pub mod config;
pub mod convert;
pub mod engine;
pub mod error;
pub mod image;
pub mod pageable;
pub mod paginate;
pub mod paragraph;
pub mod render;

pub use config::{Config, ConfigBuilder, Margin, PageSize};
pub use engine::{Engine, EngineBuilder};
pub use error::{Error, Result};

/// Convert HTML to PDF with default settings.
pub fn convert_html(html: &str) -> Result<Vec<u8>> {
    let engine = Engine::builder().build();
    engine.render_html(html)
}
