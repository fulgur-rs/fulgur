//! `Fulgur::Engine` と `Fulgur::EngineBuilder` の Ruby バインディング。
//!
//! - `Engine.new(**kwargs)`: kwargs-only constructor。
//! - `Engine.builder`: `EngineBuilder` を返す。
//! - `EngineBuilder#page_size`, `#margin`, `#assets`, `#landscape`, `#title`,
//!   `#author`, `#lang`, `#bookmarks`: setter は `self` を返してチェーンを成立させる。
//! - `EngineBuilder#build`: `Engine` を返す。2 回目の呼び出しは RuntimeError。
//!
//! `render_html` / `render_html_to_file` は Task 7 以降で追加する。

use crate::asset_bundle::RbAssetBundle;
use crate::margin::RbMargin;
use crate::page_size::extract as extract_page_size;
use fulgur::{Engine, EngineBuilder};
use magnus::{
    Error, Module, RModule, Ruby, Value, function, method,
    prelude::*,
    scan_args::{get_kwargs, scan_args},
};
use std::cell::RefCell;

#[magnus::wrap(class = "Fulgur::EngineBuilder", free_immediately, size)]
pub struct RbEngineBuilder {
    inner: RefCell<Option<EngineBuilder>>,
}

impl RbEngineBuilder {
    fn new() -> Self {
        Self {
            inner: RefCell::new(Some(Engine::builder())),
        }
    }

    fn take(&self) -> Result<EngineBuilder, Error> {
        self.inner.borrow_mut().take().ok_or_else(|| {
            Error::new(
                magnus::exception::runtime_error(),
                "EngineBuilder has already been built",
            )
        })
    }

    fn map(&self, f: impl FnOnce(EngineBuilder) -> EngineBuilder) -> Result<(), Error> {
        let b = self.take()?;
        *self.inner.borrow_mut() = Some(f(b));
        Ok(())
    }
}

// -- setter (chain API) --
//
// `magnus::typed_data::Obj<T>` は TypedData wrapper を保持する Ruby オブジェクト参照で、
// `Deref<Target = T>` を実装している。setter は同じ self (Obj) を返して Ruby 側で
// `.page_size(:a4).margin(...)` の chain を成立させる。

fn builder_page_size(
    b: magnus::typed_data::Obj<RbEngineBuilder>,
    value: Value,
) -> Result<magnus::typed_data::Obj<RbEngineBuilder>, Error> {
    let ps = extract_page_size(value)?;
    b.map(|inner| inner.page_size(ps))?;
    Ok(b)
}

fn builder_margin(
    b: magnus::typed_data::Obj<RbEngineBuilder>,
    m: &RbMargin,
) -> Result<magnus::typed_data::Obj<RbEngineBuilder>, Error> {
    // `Margin` は `Copy`。クロージャに move するためローカルへコピーする。
    let margin = m.inner;
    b.map(|inner| inner.margin(margin))?;
    Ok(b)
}

fn builder_assets(
    b: magnus::typed_data::Obj<RbEngineBuilder>,
    bundle: &RbAssetBundle,
) -> Result<magnus::typed_data::Obj<RbEngineBuilder>, Error> {
    let taken = bundle.take_inner();
    b.map(|inner| inner.assets(taken))?;
    Ok(b)
}

fn builder_landscape(
    b: magnus::typed_data::Obj<RbEngineBuilder>,
    v: bool,
) -> Result<magnus::typed_data::Obj<RbEngineBuilder>, Error> {
    b.map(|inner| inner.landscape(v))?;
    Ok(b)
}

fn builder_title(
    b: magnus::typed_data::Obj<RbEngineBuilder>,
    s: String,
) -> Result<magnus::typed_data::Obj<RbEngineBuilder>, Error> {
    b.map(|inner| inner.title(s))?;
    Ok(b)
}

fn builder_author(
    b: magnus::typed_data::Obj<RbEngineBuilder>,
    s: String,
) -> Result<magnus::typed_data::Obj<RbEngineBuilder>, Error> {
    b.map(|inner| inner.author(s))?;
    Ok(b)
}

fn builder_lang(
    b: magnus::typed_data::Obj<RbEngineBuilder>,
    s: String,
) -> Result<magnus::typed_data::Obj<RbEngineBuilder>, Error> {
    b.map(|inner| inner.lang(s))?;
    Ok(b)
}

fn builder_bookmarks(
    b: magnus::typed_data::Obj<RbEngineBuilder>,
    v: bool,
) -> Result<magnus::typed_data::Obj<RbEngineBuilder>, Error> {
    b.map(|inner| inner.bookmarks(v))?;
    Ok(b)
}

fn builder_build(b: magnus::typed_data::Obj<RbEngineBuilder>) -> Result<RbEngine, Error> {
    let built = b.take()?;
    Ok(RbEngine {
        inner: built.build(),
    })
}

#[magnus::wrap(class = "Fulgur::Engine", free_immediately, size)]
pub struct RbEngine {
    #[allow(dead_code)] // Task 7 で render_html から参照される
    pub(crate) inner: Engine,
}

/// kwargs-only constructor。positional args は受け付けない。
fn engine_new(args: &[Value]) -> Result<RbEngine, Error> {
    let scanned = scan_args::<(), (), (), (), _, ()>(args)?;
    let kw = get_kwargs::<
        _,
        (),
        (
            Option<Value>,
            Option<&RbMargin>,
            Option<bool>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<bool>,
            Option<&RbAssetBundle>,
        ),
        (),
    >(
        scanned.keywords,
        &[],
        &[
            "page_size",
            "margin",
            "landscape",
            "title",
            "author",
            "lang",
            "bookmarks",
            "assets",
        ],
    )?;
    let (page_size, margin, landscape, title, author, lang, bookmarks, assets) = kw.optional;

    let mut b = Engine::builder();
    if let Some(v) = page_size {
        b = b.page_size(extract_page_size(v)?);
    }
    if let Some(m) = margin {
        b = b.margin(m.inner);
    }
    if let Some(v) = landscape {
        b = b.landscape(v);
    }
    if let Some(s) = title {
        b = b.title(s);
    }
    if let Some(s) = author {
        b = b.author(s);
    }
    if let Some(s) = lang {
        b = b.lang(s);
    }
    if let Some(v) = bookmarks {
        b = b.bookmarks(v);
    }
    if let Some(bundle) = assets {
        b = b.assets(bundle.take_inner());
    }
    Ok(RbEngine { inner: b.build() })
}

fn engine_builder() -> RbEngineBuilder {
    RbEngineBuilder::new()
}

pub fn define(_ruby: &Ruby, fulgur: &RModule) -> Result<(), Error> {
    let engine = fulgur.define_class("Engine", magnus::class::object())?;
    engine.define_singleton_method("new", function!(engine_new, -1))?;
    engine.define_singleton_method("builder", function!(engine_builder, 0))?;

    let builder = fulgur.define_class("EngineBuilder", magnus::class::object())?;
    builder.define_method("page_size", method!(builder_page_size, 1))?;
    builder.define_method("margin", method!(builder_margin, 1))?;
    builder.define_method("assets", method!(builder_assets, 1))?;
    builder.define_method("landscape", method!(builder_landscape, 1))?;
    builder.define_method("title", method!(builder_title, 1))?;
    builder.define_method("author", method!(builder_author, 1))?;
    builder.define_method("lang", method!(builder_lang, 1))?;
    builder.define_method("bookmarks", method!(builder_bookmarks, 1))?;
    builder.define_method("build", method!(builder_build, 0))?;
    Ok(())
}
