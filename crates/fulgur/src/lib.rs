//! HTML/CSS to PDF conversion.
//!
//! This crate is a facade over the layout backend (currently `fulgur-blitz`)
//! and the backend-independent `fulgur-core`. Its public paths
//! (`fulgur::Engine`, `fulgur::asset::AssetBundle`, ...) are kept stable
//! while the implementation lives in those crates.
//!
//! With the `raikiri` feature the facade can also render through the Raikiri
//! backend: see [`Backend`] and [`EngineExt::render_html_with`].

mod backend;

pub use backend::{Backend, EngineExt};
pub use fulgur_blitz::*;
