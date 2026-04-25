//! `Fulgur::AssetBundle` Ruby class wrapping `fulgur::AssetBundle`.
//!
//! Ruby は GVL 下で single-threaded なので `RefCell` による内部可変で十分。
//! Engine builder に渡すときは `take_inner()` で中身を奪い、空の
//! `AssetBundle::new()` に差し替える。

use crate::error::map_fulgur_error;
// `fulgur::AssetBundle` root re-export は 0.4.5 には存在しないため full path で参照。
use fulgur::asset::AssetBundle;
use magnus::{Error, Module, RModule, RString, Ruby, function, method, prelude::*};
use std::cell::RefCell;
use std::path::PathBuf;

#[magnus::wrap(class = "Fulgur::AssetBundle", free_immediately, size)]
pub struct RbAssetBundle {
    pub(crate) inner: RefCell<AssetBundle>,
}

impl RbAssetBundle {
    pub(crate) fn new() -> Self {
        Self {
            inner: RefCell::new(AssetBundle::new()),
        }
    }

    /// Engine builder に渡すために中身を取り出す。
    /// 奪った後は empty AssetBundle に差し替える。
    #[allow(dead_code)] // Task 6 で engine builder から呼ばれる
    pub(crate) fn take_inner(&self) -> AssetBundle {
        std::mem::replace(&mut *self.inner.borrow_mut(), AssetBundle::new())
    }

    fn add_css(&self, css: String) {
        self.inner.borrow_mut().add_css(css);
    }

    fn add_css_file(&self, path: String) -> Result<(), Error> {
        let ruby = Ruby::get().expect("ruby vm");
        self.inner
            .borrow_mut()
            .add_css_file(PathBuf::from(path))
            .map_err(|e| map_fulgur_error(&ruby, e))
    }

    fn add_font_file(&self, path: String) -> Result<(), Error> {
        let ruby = Ruby::get().expect("ruby vm");
        self.inner
            .borrow_mut()
            .add_font_file(PathBuf::from(path))
            .map_err(|e| map_fulgur_error(&ruby, e))
    }

    fn add_image(&self, name: String, data: RString) {
        // SAFETY: `as_slice` returns a reference tied to the Ruby VM lifetime;
        // we immediately copy into an owned `Vec<u8>` so the borrow ends here.
        let bytes = unsafe { data.as_slice() }.to_vec();
        self.inner.borrow_mut().add_image(name, bytes);
    }

    fn add_image_file(&self, name: String, path: String) -> Result<(), Error> {
        let ruby = Ruby::get().expect("ruby vm");
        self.inner
            .borrow_mut()
            .add_image_file(name, PathBuf::from(path))
            .map_err(|e| map_fulgur_error(&ruby, e))
    }
}

pub fn define(ruby: &Ruby, fulgur: &RModule) -> Result<(), Error> {
    let class = fulgur.define_class("AssetBundle", ruby.class_object())?;
    class.define_singleton_method("new", function!(RbAssetBundle::new, 0))?;
    class.define_method("add_css", method!(RbAssetBundle::add_css, 1))?;
    class.define_method("add_css_file", method!(RbAssetBundle::add_css_file, 1))?;
    class.define_method("add_font_file", method!(RbAssetBundle::add_font_file, 1))?;
    class.define_method("add_image", method!(RbAssetBundle::add_image, 2))?;
    class.define_method("add_image_file", method!(RbAssetBundle::add_image_file, 2))?;
    Ok(())
}
