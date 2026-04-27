//! `Fulgur::Engine` と `Fulgur::EngineBuilder` の Ruby バインディング。
//!
//! - `Engine.new(**kwargs)`: kwargs-only constructor。
//! - `Engine.builder`: `EngineBuilder` を返す。
//! - `EngineBuilder#page_size`, `#margin`, `#assets`, `#landscape`, `#title`,
//!   `#author`, `#lang`, `#bookmarks`: setter は `self` を返してチェーンを成立させる。
//! - `EngineBuilder#build`: `Engine` を返す。2 回目の呼び出しは RuntimeError。
//!
//! `Engine#render_html` は `Fulgur::Pdf` を返し、`#render_html_to_file` は
//! PDF をそのままファイルに書き出す。どちらもレンダリング中は GVL を解放する。

use crate::asset_bundle::RbAssetBundle;
use crate::error::map_fulgur_error;
use crate::margin::RbMargin;
use crate::page_size::extract as extract_page_size;
use crate::pdf::RbPdf;
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
            let ruby = Ruby::get().expect("ruby vm");
            Error::new(
                ruby.exception_runtime_error(),
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
    pub(crate) inner: Engine,
}

impl RbEngine {
    /// HTML 文字列を PDF バイト列に変換し、`Fulgur::Pdf` でラップして返す。
    ///
    /// レンダリング本体は GVL を解放して実行するため、他の Ruby スレッド
    /// (Thread.new 等) が並行して進める。GVL 解放中のクロージャ内では
    /// Ruby VM に触れてはならないので、エラー変換 (`map_fulgur_error`) は
    /// GVL 再取得後に行う。
    fn render_html(&self, html: String) -> Result<RbPdf, Error> {
        // `Engine: Send + Sync` は `src/lib.rs` の `assert_impl_all!` で
        // コンパイル時に検証済み。raw pointer に一段落としてクロージャへ
        // 渡し、GVL 解放スレッド側で `&Engine` に戻す。
        struct Args {
            engine: *const Engine,
            html: String,
        }
        // SAFETY: `Engine: Sync` なので複数スレッドから &Engine 経由で
        // read-only 参照するのは安全。raw pointer 自体は !Send のため
        // 明示的に unsafe impl で Send を付与する。self (RbEngine) は
        // `without_gvl` の間 block される呼び出し側スタックに生きている
        // ので、dangling にはならない。
        unsafe impl Send for Args {}

        let args = Args {
            engine: &self.inner as *const Engine,
            html,
        };
        let result: Result<Vec<u8>, fulgur::Error> = crate::gvl::without_gvl(args, |a| {
            // SAFETY: `a.engine` は呼び出し元の `&self.inner` から作った
            // pointer。`without_gvl` は呼び出し元を block しているため、
            // この closure が走っている間 `self` は生存している。
            let engine: &Engine = unsafe { &*a.engine };
            engine.render_html(&a.html)
        });

        let ruby = Ruby::get().expect("ruby vm");
        match result {
            Ok(bytes) => Ok(RbPdf::new(bytes)),
            Err(e) => Err(map_fulgur_error(&ruby, e)),
        }
    }

    /// HTML を PDF に変換してファイルに書き出す。`render_html` と同じく GVL を
    /// 解放して実行する。`path` は `String` または `to_path` 応答オブジェクト (`Pathname` 等)。
    fn render_html_to_file(&self, html: String, path: Value) -> Result<(), Error> {
        struct Args {
            engine: *const Engine,
            html: String,
            path: std::path::PathBuf,
        }
        // SAFETY: `render_html` と同じ不変条件。`Engine: Send + Sync` は
        // compile-time に assert されている。`without_gvl` は呼び出し元を
        // block するため、closure 実行中に `self` (= *engine) は生きている。
        unsafe impl Send for Args {}

        let path_str = crate::pdf::coerce_to_path(path)?;
        let args = Args {
            engine: &self.inner as *const Engine,
            html,
            path: std::path::PathBuf::from(path_str),
        };
        let result: Result<(), fulgur::Error> = crate::gvl::without_gvl(args, |a| {
            // SAFETY: `a.engine` は呼び出し元の `&self.inner` から作った
            // pointer。`without_gvl` の block 中は self が生存している。
            let engine: &Engine = unsafe { &*a.engine };
            engine.render_html_to_file(&a.html, &a.path)
        });

        let ruby = Ruby::get().expect("ruby vm");
        result.map_err(|e| map_fulgur_error(&ruby, e))
    }
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

pub fn define(ruby: &Ruby, fulgur: &RModule) -> Result<(), Error> {
    let engine = fulgur.define_class("Engine", ruby.class_object())?;
    engine.define_singleton_method("new", function!(engine_new, -1))?;
    engine.define_singleton_method("builder", function!(engine_builder, 0))?;
    engine.define_method("render_html", method!(RbEngine::render_html, 1))?;
    engine.define_method(
        "render_html_to_file",
        method!(RbEngine::render_html_to_file, 2),
    )?;

    let builder = fulgur.define_class("EngineBuilder", ruby.class_object())?;
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
