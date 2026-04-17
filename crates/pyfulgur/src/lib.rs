//! Python bindings for fulgur (HTML/CSS → PDF).
//!
//! すべての pyo3 依存コードは `extension-module` feature で gate している。
//! feature off の場合このクレートは空になり、`cargo build --workspace` が通る。
//! 実バイナリは `maturin` が `features = ["extension-module"]` を注入してビルドする。

#![cfg(feature = "extension-module")]
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::useless_conversion)]
#![allow(unexpected_cfgs)]

use pyo3::prelude::*;

mod asset_bundle;
mod engine;
mod error;
mod margin;
mod page_size;

use asset_bundle::PyAssetBundle;
use engine::{PyEngine, PyEngineBuilder};
use margin::PyMargin;
use page_size::PyPageSize;

/// fulgur 公開型が Send + Sync であることを compile time に保証する。
/// fulgur-d3r で保証済みだが、将来の regression を早期検知するため明示する。
#[cfg(test)]
mod assertions {
    use static_assertions::assert_impl_all;
    assert_impl_all!(fulgur::Engine: Send, Sync);
    assert_impl_all!(fulgur::AssetBundle: Send, Sync);
}

#[pymodule]
fn pyfulgur(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPageSize>()?;
    m.add_class::<PyMargin>()?;
    m.add_class::<PyAssetBundle>()?;
    m.add_class::<PyEngineBuilder>()?;
    m.add_class::<PyEngine>()?;
    error::register(m)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
