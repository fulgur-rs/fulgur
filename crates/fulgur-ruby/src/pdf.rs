//! `Fulgur::Pdf` — `Engine#render_html` が返す結果オブジェクト。
//!
//! バイト列を保持し、`to_s` (ASCII-8BIT String), `bytesize`, `to_base64`,
//! `to_data_uri`, `inspect`, `write_to_path`, `write_to_io` を提供する。

use base64::Engine as _;
use magnus::{Error, Module, RModule, RString, Ruby, TryConvert, Value, method, prelude::*};

/// `write_to_io` でチャンク単位に分割するサイズ (64 KiB)。
const CHUNK_SIZE: usize = 64 * 1024;

#[magnus::wrap(class = "Fulgur::Pdf", free_immediately, size)]
pub struct RbPdf {
    pub(crate) bytes: Vec<u8>,
}

impl RbPdf {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    fn bytesize(&self) -> usize {
        self.bytes.len()
    }

    fn to_s(&self) -> RString {
        // `RString::from_slice` は ASCII-8BIT (binary) encoding の Ruby String を返す。
        RString::from_slice(&self.bytes)
    }

    fn to_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.bytes)
    }

    fn to_data_uri(&self) -> String {
        format!("data:application/pdf;base64,{}", self.to_base64())
    }

    fn inspect(&self) -> String {
        format!("#<Fulgur::Pdf bytesize={}>", self.bytes.len())
    }

    /// PDF バイト列をファイルパスに書き込む。
    ///
    /// `path` は `String` または `to_path` に応答するオブジェクト (`Pathname` など)。
    /// エラーは `fulgur::Error::Io` 経由で `map_fulgur_error` に委譲する
    /// (`NotFound` なら `Errno::ENOENT`、それ以外は `Fulgur::RenderError`)。
    fn write_to_path(&self, path: Value) -> Result<(), Error> {
        let ruby = Ruby::get().expect("ruby vm");
        let path_str = coerce_to_path(path)?;
        std::fs::write(&path_str, &self.bytes).map_err(|e| {
            let fulgur_err = fulgur::Error::Io(e);
            crate::error::map_fulgur_error(&ruby, fulgur_err)
        })
    }

    /// PDF バイト列を Ruby IO オブジェクトに書き込む。
    ///
    /// - `binmode` に応答する場合のみ呼んでエンコーディング変換を防ぐ。独自ダック型
    ///   IO ラッパが `binmode` を実装していないケースでも `NoMethodError` を投げない。
    /// - 64 KiB チャンクに分割して `write` を呼ぶ。ピークメモリは `N + CHUNK` に収まる。
    /// - `IO#write` が要求より少ないバイト数を返した場合 (socket / pipe / 独自 IO の短書き込み)
    ///   は残りを再送する。戻り値が 0 の場合は無限ループを防ぐため `RuntimeError` を返す。
    fn write_to_io(&self, io: Value) -> Result<(), Error> {
        let responds: bool = io.funcall("respond_to?", (magnus::Symbol::new("binmode"),))?;
        if responds {
            let _: Value = io.funcall("binmode", ())?;
        }
        for chunk in self.bytes.chunks(CHUNK_SIZE) {
            let mut offset = 0;
            while offset < chunk.len() {
                let rb_bytes = RString::from_slice(&chunk[offset..]);
                let written: usize = io.funcall("write", (rb_bytes,))?;
                if written == 0 {
                    return Err(Error::new(
                        magnus::exception::runtime_error(),
                        "IO#write returned 0 bytes; cannot make progress",
                    ));
                }
                offset += written;
            }
        }
        Ok(())
    }
}

/// Ruby 値をファイルパス文字列に変換する。`Pathname` 等 `to_path` に応答する
/// オブジェクトを受け入れ、そうでなければ `to_str` で暗黙変換する。
pub(crate) fn coerce_to_path(value: Value) -> Result<String, Error> {
    let responds: bool = value.funcall("respond_to?", (magnus::Symbol::new("to_path"),))?;
    let converted: Value = if responds {
        value.funcall("to_path", ())?
    } else {
        value
    };
    <String>::try_convert(converted).map_err(|_| {
        Error::new(
            magnus::exception::type_error(),
            "path must be a String or respond to to_path",
        )
    })
}

pub fn define(_ruby: &Ruby, fulgur: &RModule) -> Result<(), Error> {
    let class = fulgur.define_class("Pdf", magnus::class::object())?;
    class.define_method("bytesize", method!(RbPdf::bytesize, 0))?;
    class.define_method("to_s", method!(RbPdf::to_s, 0))?;
    class.define_method("to_base64", method!(RbPdf::to_base64, 0))?;
    class.define_method("to_data_uri", method!(RbPdf::to_data_uri, 0))?;
    class.define_method("inspect", method!(RbPdf::inspect, 0))?;
    class.define_method("write_to_path", method!(RbPdf::write_to_path, 1))?;
    class.define_method("write_to_io", method!(RbPdf::write_to_io, 1))?;
    Ok(())
}
