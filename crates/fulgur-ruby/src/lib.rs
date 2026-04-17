//! Ruby bindings for fulgur (HTML/CSS → PDF).
//!
//! すべての magnus 依存コードは `ruby-api` feature で gate している。
//! feature off の場合このクレートは空になり、`cargo build --workspace` が通る。
//! 実バイナリは `rake compile` が `features = ["ruby-api"]` を注入してビルドする。

#![cfg(feature = "ruby-api")]

use magnus::{Error, define_module};

mod error;

#[cfg(test)]
mod assertions {
    use static_assertions::assert_impl_all;
    assert_impl_all!(fulgur::Engine: Send, Sync);
    assert_impl_all!(fulgur::AssetBundle: Send, Sync);
}

#[magnus::init]
fn init(_ruby: &magnus::Ruby) -> Result<(), Error> {
    let _fulgur = define_module("Fulgur")?;
    Ok(())
}
