//! `Fulgur::PageSize` Ruby class wrapping `fulgur::PageSize`.
//!
//! 定数 `A4` / `LETTER` / `A3`、`.custom(width_mm, height_mm)` クラスメソッド、
//! インスタンスメソッド `width` / `height` / `landscape` / `inspect` / `to_s` を公開する。
//!
//! 併せて、Task 6 以降の engine バインディングから使われる `extract()` ヘルパーを提供し、
//! `Symbol` / `String` / `Fulgur::PageSize` のいずれでも `fulgur::PageSize` に変換できるようにする。

use fulgur::PageSize;
use magnus::{
    Error, Module, RModule, Ruby, Symbol, TryConvert, Value, function, method, prelude::*,
};

#[magnus::wrap(class = "Fulgur::PageSize", free_immediately, size)]
#[derive(Clone, Copy)]
pub struct RbPageSize {
    pub(crate) inner: PageSize,
}

impl RbPageSize {
    pub(crate) fn new(inner: PageSize) -> Self {
        Self { inner }
    }

    fn width(&self) -> f32 {
        self.inner.width
    }

    fn height(&self) -> f32 {
        self.inner.height
    }

    fn landscape(&self) -> Self {
        Self::new(self.inner.landscape())
    }

    fn inspect(&self) -> String {
        format!(
            "#<Fulgur::PageSize width={:.2} height={:.2}>",
            self.inner.width, self.inner.height
        )
    }

    fn custom(width_mm: f32, height_mm: f32) -> Self {
        Self::new(PageSize::custom(width_mm, height_mm))
    }
}

/// `Symbol` / `String` / `Fulgur::PageSize` を `fulgur::PageSize` に変換する。
/// Task 6 以降の engine バインディングから呼び出される。
#[allow(dead_code)]
pub fn extract(value: Value) -> Result<PageSize, Error> {
    if let Ok(ps) = <&RbPageSize>::try_convert(value) {
        return Ok(ps.inner);
    }
    if let Ok(sym) = <Symbol>::try_convert(value) {
        return parse_name(&sym.name()?);
    }
    if let Ok(s) = <String>::try_convert(value) {
        return parse_name(&s);
    }
    let ruby = Ruby::get().expect("ruby vm");
    Err(Error::new(
        ruby.exception_arg_error(),
        "page_size must be Symbol, String, or Fulgur::PageSize",
    ))
}

fn parse_name(name: &str) -> Result<PageSize, Error> {
    match name.to_ascii_uppercase().as_str() {
        "A4" => Ok(PageSize::A4),
        "LETTER" => Ok(PageSize::LETTER),
        "A3" => Ok(PageSize::A3),
        other => {
            let ruby = Ruby::get().expect("ruby vm");
            Err(Error::new(
                ruby.exception_arg_error(),
                format!("unknown page size: {other}"),
            ))
        }
    }
}

pub fn define(ruby: &Ruby, fulgur: &RModule) -> Result<(), Error> {
    let class = fulgur.define_class("PageSize", ruby.class_object())?;
    class.define_singleton_method("custom", function!(RbPageSize::custom, 2))?;
    class.define_method("width", method!(RbPageSize::width, 0))?;
    class.define_method("height", method!(RbPageSize::height, 0))?;
    class.define_method("landscape", method!(RbPageSize::landscape, 0))?;
    class.define_method("inspect", method!(RbPageSize::inspect, 0))?;
    class.define_method("to_s", method!(RbPageSize::inspect, 0))?;

    class.const_set("A4", RbPageSize::new(PageSize::A4))?;
    class.const_set("LETTER", RbPageSize::new(PageSize::LETTER))?;
    class.const_set("A3", RbPageSize::new(PageSize::A3))?;
    Ok(())
}
