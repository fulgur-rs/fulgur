//! Visual regression testing harness for fulgur.
//!
//! This crate is `publish = false` — it exists only to run VRT via
//! `cargo test -p fulgur-vrt`. It is not shipped to crates.io.

pub mod diff;
pub mod engine_compare;
pub mod manifest;
pub mod pdf_render;
pub mod runner;

#[cfg(feature = "chrome-golden")]
pub mod chrome;
