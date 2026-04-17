//! Python bindings for fulgur (HTML/CSS → PDF).
//!
//! すべての pyo3 依存コードは `extension-module` feature で gate している。
//! feature off の場合このクレートは空になり、`cargo build --workspace` が通る。
//! 実バイナリは `maturin` が `features = ["extension-module"]` を注入してビルドする。

#![cfg(feature = "extension-module")]

use pyo3::prelude::*;

/// fulgur 公開型が Send + Sync であることを compile time に保証する。
/// fulgur-d3r で保証済みだが、将来の regression を早期検知するため明示する。
#[cfg(test)]
mod assertions {
    use static_assertions::assert_impl_all;
    assert_impl_all!(fulgur::Engine: Send, Sync);
    assert_impl_all!(fulgur::AssetBundle: Send, Sync);
}

#[pymodule]
fn pyfulgur(_py: Python<'_>, _m: &Bound<'_, PyModule>) -> PyResult<()> {
    Ok(())
}
