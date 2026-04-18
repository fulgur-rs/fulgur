//! Ruby bindings for fulgur (HTML/CSS → PDF).
//!
//! すべての magnus 依存コードは `ruby-api` feature で gate している。
//! feature off の場合このクレートは空になり、`cargo build --workspace` が通る。
//! 実バイナリは `rake compile` が `features = ["ruby-api"]` を注入してビルドする。

#![cfg(feature = "ruby-api")]

use magnus::{Error, define_module};

mod asset_bundle;
mod engine;
mod error;
mod gvl;
mod margin;
mod page_size;
mod pdf;

/// `engine.rs` の GVL 解放 (`unsafe impl Send for Args`) は `fulgur::Engine: Send + Sync` の
/// コンパイル時保証に依存している。`[cfg(test)]` ゲートを付けずに通常ビルドで走らせることで、
/// `fulgur` crate 側の変更で `Engine` が `!Send` になった瞬間に `rake compile` が落ちる。
mod assertions {
    use static_assertions::assert_impl_all;
    assert_impl_all!(fulgur::Engine: Send, Sync);
    // `fulgur::AssetBundle` (root re-export) は 0.4.5 には存在せず HEAD のみ。
    // release-ruby は crates.io の ext 依存 (0.4.5 時点では 0.4.5) を使うため、
    // full path `fulgur::asset::AssetBundle` を参照して前方互換を保つ。
    assert_impl_all!(fulgur::asset::AssetBundle: Send, Sync);
}

#[magnus::init]
fn init(ruby: &magnus::Ruby) -> Result<(), Error> {
    let fulgur = define_module("Fulgur")?;
    page_size::define(ruby, &fulgur)?;
    margin::define(ruby, &fulgur)?;
    asset_bundle::define(ruby, &fulgur)?;
    pdf::define(ruby, &fulgur)?;
    engine::define(ruby, &fulgur)?;
    Ok(())
}
