//! `Fulgur::Margin` Ruby class wrapping `fulgur::Margin`.
//!
//! primitive constructor `__build__(top, right, bottom, left)` を Rust 側で定義し、
//! Ruby 側 (`lib/fulgur/margin.rb`) が positional / kwargs を解釈して `__build__` を呼び出す。
//! factory method として `.uniform(pt)` と `.symmetric(vertical, horizontal)` も公開する。

use fulgur::Margin;
use magnus::{Error, Module, RModule, Ruby, function, method, prelude::*};

#[magnus::wrap(class = "Fulgur::Margin", free_immediately, size)]
#[derive(Clone, Copy)]
pub struct RbMargin {
    pub(crate) inner: Margin,
}

impl RbMargin {
    pub(crate) fn new(inner: Margin) -> Self {
        Self { inner }
    }

    fn top(&self) -> f32 {
        self.inner.top
    }

    fn right(&self) -> f32 {
        self.inner.right
    }

    fn bottom(&self) -> f32 {
        self.inner.bottom
    }

    fn left(&self) -> f32 {
        self.inner.left
    }

    fn inspect(&self) -> String {
        format!(
            "#<Fulgur::Margin top={:.2} right={:.2} bottom={:.2} left={:.2}>",
            self.inner.top, self.inner.right, self.inner.bottom, self.inner.left
        )
    }

    fn uniform(pt: f32) -> Self {
        Self::new(Margin::uniform(pt))
    }

    fn symmetric(vertical: f32, horizontal: f32) -> Self {
        Self::new(Margin::symmetric(vertical, horizontal))
    }

    /// Ruby 側 `Fulgur::Margin.new` から呼ばれる primitive constructor。
    /// Ruby 側が positional / kwargs を解釈して `__build__(t, r, b, l)` を呼び出す。
    fn from_trbl(top: f32, right: f32, bottom: f32, left: f32) -> Self {
        Self::new(Margin {
            top,
            right,
            bottom,
            left,
        })
    }
}

pub fn define(ruby: &Ruby, fulgur: &RModule) -> Result<(), Error> {
    let class = fulgur.define_class("Margin", ruby.class_object())?;
    class.define_singleton_method("__build__", function!(RbMargin::from_trbl, 4))?;
    class.define_singleton_method("uniform", function!(RbMargin::uniform, 1))?;
    class.define_singleton_method("symmetric", function!(RbMargin::symmetric, 2))?;
    class.define_method("top", method!(RbMargin::top, 0))?;
    class.define_method("right", method!(RbMargin::right, 0))?;
    class.define_method("bottom", method!(RbMargin::bottom, 0))?;
    class.define_method("left", method!(RbMargin::left, 0))?;
    class.define_method("inspect", method!(RbMargin::inspect, 0))?;
    class.define_method("to_s", method!(RbMargin::inspect, 0))?;
    Ok(())
}
