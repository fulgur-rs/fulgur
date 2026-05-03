# pyfulgur .pyi スタブ + docstring 整備 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** pyfulgur に PEP 561 準拠の型スタブと API ドキュメントを整備し、IDE 補完・mypy 静的型チェック・mkdocstrings[python] 連携を成立させる。

**Architecture:** PyO3 `#[pymodule]` を `pyfulgur` から `_native` に rename して maturin の mixed Python/Rust layout に切り替える。`crates/pyfulgur/python/pyfulgur/` に re-export `__init__.py`、型シグネチャを集約した `__init__.pyi`、PEP 561 marker `py.typed` を配置。docstring は単一ソース (Rust `///` doc comment) で管理し、PyO3 が `__doc__` に展開して runtime / griffe / IDE すべてで自然に拾われるようにする。

**Tech Stack:** PyO3 0.28 (extension-module + abi3-py39) / maturin / Python 3.9-3.13 / pytest / mypy (manual)

---

## Pre-flight

**Worktree:** `/home/ubuntu/fulgur/.worktrees/pyfulgur-stubs` (branch `feat/pyfulgur-stubs`)

**Beads:** fulgur-t393 (in_progress)

**baseline 確認**:

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur-stubs
cargo build -p pyfulgur                    # Should pass (already verified)
cargo test --workspace --exclude fulgur-vrt 2>&1 | tail -20
maturin develop -m crates/pyfulgur/Cargo.toml --release 2>&1 | tail -5
python3 -m pytest crates/pyfulgur/tests/ -v 2>&1 | tail -20
```

すべて green であることを確認してから Task 1 へ進む。

---

## Task 1: Mixed layout への移行 (`pyfulgur` → `pyfulgur._native`)

**Files:**
- Modify: `crates/pyfulgur/src/lib.rs:34` (`#[pymodule] fn pyfulgur` → `fn _native`)
- Modify: `crates/pyfulgur/pyproject.toml` (tool.maturin に `python-source` / `module-name` 追加)
- Create: `crates/pyfulgur/python/pyfulgur/__init__.py`
- Create: `crates/pyfulgur/python/pyfulgur/py.typed` (空ファイル)

**Step 1: 失敗テストを書く**

新規ファイル `crates/pyfulgur/tests/test_typed.py`:

```python
"""PEP 561 type marker と stub の wheel 同梱検証。"""
from importlib.resources import files


def test_py_typed_marker_exists():
    """py.typed marker が pyfulgur パッケージに同梱されていること。"""
    assert (files("pyfulgur") / "py.typed").is_file()


def test_pyi_stub_exists():
    """__init__.pyi が pyfulgur パッケージに同梱されていること。"""
    assert (files("pyfulgur") / "__init__.pyi").is_file()
```

**Step 2: テストが失敗することを確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur-stubs
python3 -m pytest crates/pyfulgur/tests/test_typed.py -v
```

期待: 両方 FAIL (`py.typed` / `__init__.pyi` が存在しない)。

**Step 3: `src/lib.rs` の pymodule 名を rename**

`crates/pyfulgur/src/lib.rs` の `#[pymodule]` 関数:

```rust
#[pymodule]
fn _native(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPageSize>()?;
    m.add_class::<PyMargin>()?;
    m.add_class::<PyAssetBundle>()?;
    m.add_class::<PyEngineBuilder>()?;
    m.add_class::<PyEngine>()?;
    error::register(m)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
```

`#[pyclass(name = "Engine", module = "pyfulgur")]` の `module = "pyfulgur"` 属性は変えない (ユーザー視点モジュール名のため `__module__` は `pyfulgur` のままで見える)。`error::create_exception!(pyfulgur, RenderError, ...)` も `pyfulgur` のまま維持。

**Step 4: `pyproject.toml` を更新**

`crates/pyfulgur/pyproject.toml` の `[tool.maturin]` セクション:

```toml
[tool.maturin]
module-name = "pyfulgur._native"
python-source = "python"
manifest-path = "Cargo.toml"
features = ["extension-module"]
py-limited-api = "cp39"
```

**Step 5: `python/pyfulgur/__init__.py` を作成**

```python
"""pyfulgur — Python bindings for fulgur (HTML/CSS to PDF).

Offline, deterministic HTML/CSS to PDF conversion engine.
"""

from ._native import (
    AssetBundle,
    Engine,
    EngineBuilder,
    Margin,
    PageSize,
    RenderError,
    __version__,
)

__all__ = [
    "AssetBundle",
    "Engine",
    "EngineBuilder",
    "Margin",
    "PageSize",
    "RenderError",
    "__version__",
]
```

**Step 6: `python/pyfulgur/py.typed` (空ファイル) を作成**

```bash
mkdir -p crates/pyfulgur/python/pyfulgur
touch crates/pyfulgur/python/pyfulgur/py.typed
```

**Step 7: rebuild + 既存テストが green を維持することを確認**

```bash
maturin develop -m crates/pyfulgur/Cargo.toml --release 2>&1 | tail -5
python3 -m pytest crates/pyfulgur/tests/ -v 2>&1 | tail -30
```

期待: 既存テストは全 PASS。`test_typed.py::test_py_typed_marker_exists` PASS。`test_typed.py::test_pyi_stub_exists` だけ FAIL (Task 3 で stub を追加するまで)。

**Step 8: cargo workspace test (extension-module off) も維持確認**

```bash
cargo test --workspace --exclude fulgur-vrt 2>&1 | tail -10
```

期待: PASS。

**Step 9: コミット**

```bash
git add crates/pyfulgur/src/lib.rs \
        crates/pyfulgur/pyproject.toml \
        crates/pyfulgur/python/pyfulgur/__init__.py \
        crates/pyfulgur/python/pyfulgur/py.typed \
        crates/pyfulgur/tests/test_typed.py
git commit -m "refactor(pyfulgur): switch to mixed layout for PEP 561 stubs"
```

---

## Task 2: Rust `///` doc comments を Google 形式で追加

**Files:**
- Modify: `crates/pyfulgur/src/page_size.rs`
- Modify: `crates/pyfulgur/src/margin.rs`
- Modify: `crates/pyfulgur/src/asset_bundle.rs`
- Modify: `crates/pyfulgur/src/engine.rs`
- Modify: `crates/pyfulgur/src/error.rs`

**Step 1: 失敗テスト (`__doc__` カバレッジ) を書く**

`crates/pyfulgur/tests/test_docstrings.py` を新規作成:

```python
"""Rust /// doc comment が PyO3 経由で __doc__ に展開されることを検証。

mkdocstrings (griffe inspection) と help() 両方で docstring が見えることが
mkdocstrings 連携 (fulgur-lxy0) の前提。
"""
import pyfulgur


def _has_doc(obj: object, *keywords: str) -> None:
    doc = obj.__doc__
    assert doc, f"{obj!r} has no docstring"
    for kw in keywords:
        assert kw in doc, f"{obj!r} docstring missing keyword {kw!r}: {doc!r}"


def test_engine_class_has_doc():
    _has_doc(pyfulgur.Engine, "HTML", "PDF")


def test_engine_render_html_has_doc():
    _has_doc(pyfulgur.Engine.render_html, "Args", "Returns", "Raises")


def test_engine_render_html_to_file_has_doc():
    _has_doc(pyfulgur.Engine.render_html_to_file, "Args", "Raises")


def test_engine_builder_class_has_doc():
    _has_doc(pyfulgur.EngineBuilder, "builder")


def test_engine_builder_methods_have_docs():
    for method in ("page_size", "margin", "landscape", "title", "author",
                   "lang", "bookmarks", "assets", "build"):
        _has_doc(getattr(pyfulgur.EngineBuilder, method), "Returns")


def test_asset_bundle_class_has_doc():
    _has_doc(pyfulgur.AssetBundle, "CSS", "fonts", "images")


def test_asset_bundle_methods_have_docs():
    for method in ("add_css", "add_css_file", "add_font_file",
                   "add_image", "add_image_file"):
        _has_doc(getattr(pyfulgur.AssetBundle, method), "Args")


def test_page_size_class_has_doc():
    _has_doc(pyfulgur.PageSize, "page size")


def test_page_size_classattrs_have_docs():
    # classattr (A4, LETTER, A3) は instance なので __doc__ ではなく
    # クラス側の docstring に説明が含まれていることを確認する。
    doc = pyfulgur.PageSize.__doc__ or ""
    assert "A4" in doc
    assert "LETTER" in doc


def test_page_size_methods_have_docs():
    _has_doc(pyfulgur.PageSize.custom, "Args", "Returns")
    _has_doc(pyfulgur.PageSize.landscape, "Returns")


def test_margin_class_has_doc():
    _has_doc(pyfulgur.Margin, "margin")


def test_margin_factories_have_docs():
    _has_doc(pyfulgur.Margin.uniform, "Args", "Returns")
    _has_doc(pyfulgur.Margin.symmetric, "Args", "Returns")
    _has_doc(pyfulgur.Margin.uniform_mm, "Args", "Returns")


def test_render_error_has_doc():
    _has_doc(pyfulgur.RenderError, "render")
```

**Step 2: テストが失敗することを確認**

```bash
python3 -m pytest crates/pyfulgur/tests/test_docstrings.py -v 2>&1 | tail -30
```

期待: ほぼ全 FAIL。

**Step 3: `src/page_size.rs` に doc comment を追加**

```rust
use fulgur::PageSize;
use pyo3::prelude::*;

/// Represents a page size with dimensions in mm.
///
/// Use predefined class attributes (``A4``, ``LETTER``, ``A3``) or
/// :meth:`custom` for arbitrary sizes.
///
/// Example:
///     >>> from pyfulgur import PageSize
///     >>> a4 = PageSize.A4
///     >>> custom = PageSize.custom(210.0, 297.0)
///     >>> landscape = a4.landscape()
#[pyclass(name = "PageSize", module = "pyfulgur", frozen, from_py_object)]
#[derive(Clone, Copy)]
pub struct PyPageSize {
    pub(crate) inner: PageSize,
}

#[pymethods]
impl PyPageSize {
    #[classattr]
    const A4: PyPageSize = PyPageSize { inner: PageSize::A4 };

    #[classattr]
    const LETTER: PyPageSize = PyPageSize { inner: PageSize::LETTER };

    #[classattr]
    const A3: PyPageSize = PyPageSize { inner: PageSize::A3 };

    /// Create a custom page size.
    ///
    /// Args:
    ///     width_mm: Page width in millimeters.
    ///     height_mm: Page height in millimeters.
    ///
    /// Returns:
    ///     A new ``PageSize`` instance.
    #[staticmethod]
    fn custom(width_mm: f32, height_mm: f32) -> Self {
        Self { inner: PageSize::custom(width_mm, height_mm) }
    }

    /// Return a new ``PageSize`` with width and height swapped.
    ///
    /// Returns:
    ///     A new ``PageSize`` rotated 90 degrees.
    fn landscape(&self) -> Self {
        Self { inner: self.inner.landscape() }
    }

    /// Page width in millimeters.
    #[getter]
    fn width(&self) -> f32 {
        self.inner.width
    }

    /// Page height in millimeters.
    #[getter]
    fn height(&self) -> f32 {
        self.inner.height
    }

    fn __repr__(&self) -> String {
        format!(
            "PageSize(width={:.2}, height={:.2})",
            self.inner.width, self.inner.height
        )
    }
}
```

**Step 4: `src/margin.rs` に doc comment を追加**

```rust
use fulgur::Margin;
use pyo3::prelude::*;

/// Page margin in points (PDF pt).
///
/// Margins are applied uniformly to all generated pages. Use the constructor
/// for explicit per-side values, or one of the helper factories
/// (:meth:`uniform`, :meth:`symmetric`, :meth:`uniform_mm`).
///
/// Example:
///     >>> from pyfulgur import Margin
///     >>> Margin(36.0, 36.0, 36.0, 36.0)
///     >>> Margin.uniform_mm(20.0)
#[pyclass(name = "Margin", module = "pyfulgur", frozen, from_py_object)]
#[derive(Clone, Copy)]
pub struct PyMargin {
    pub(crate) inner: Margin,
}

#[pymethods]
impl PyMargin {
    /// Create a margin with explicit per-side values.
    ///
    /// Args:
    ///     top: Top margin in points.
    ///     right: Right margin in points.
    ///     bottom: Bottom margin in points.
    ///     left: Left margin in points.
    #[new]
    #[pyo3(signature = (top, right, bottom, left))]
    fn new(top: f32, right: f32, bottom: f32, left: f32) -> Self {
        Self { inner: Margin { top, right, bottom, left } }
    }

    /// Create a uniform margin (same value on all four sides).
    ///
    /// Args:
    ///     pt: Margin value in points (PDF pt).
    ///
    /// Returns:
    ///     A new ``Margin`` with all sides set to ``pt``.
    #[staticmethod]
    fn uniform(pt: f32) -> Self {
        Self { inner: Margin::uniform(pt) }
    }

    /// Create a symmetric margin (vertical and horizontal pairs).
    ///
    /// Args:
    ///     vertical: Top and bottom margin in points.
    ///     horizontal: Left and right margin in points.
    ///
    /// Returns:
    ///     A new ``Margin`` with the given symmetric values.
    #[staticmethod]
    #[pyo3(signature = (vertical, horizontal))]
    fn symmetric(vertical: f32, horizontal: f32) -> Self {
        Self { inner: Margin::symmetric(vertical, horizontal) }
    }

    /// Create a uniform margin specified in millimeters.
    ///
    /// Args:
    ///     mm: Margin value in millimeters.
    ///
    /// Returns:
    ///     A new ``Margin`` with all sides set to the equivalent point value.
    #[staticmethod]
    fn uniform_mm(mm: f32) -> Self {
        Self { inner: Margin::uniform_mm(mm) }
    }

    /// Top margin in points.
    #[getter]
    fn top(&self) -> f32 { self.inner.top }

    /// Right margin in points.
    #[getter]
    fn right(&self) -> f32 { self.inner.right }

    /// Bottom margin in points.
    #[getter]
    fn bottom(&self) -> f32 { self.inner.bottom }

    /// Left margin in points.
    #[getter]
    fn left(&self) -> f32 { self.inner.left }

    fn __repr__(&self) -> String {
        format!(
            "Margin(top={:.2}, right={:.2}, bottom={:.2}, left={:.2})",
            self.inner.top, self.inner.right, self.inner.bottom, self.inner.left
        )
    }
}
```

**Step 5: `src/asset_bundle.rs` に doc comment を追加**

`PyAssetBundle` クラスには「CSS, fonts, images をまとめてバンドルし Engine に渡す」概要、および各メソッドに Args / Raises を追加する。テンプレ:

```rust
/// Bundle of CSS, fonts, and images passed to :class:`Engine`.
///
/// fulgur is offline-first: every asset must be explicitly registered before
/// rendering. Network fetches are not performed.
///
/// Example:
///     >>> from pyfulgur import AssetBundle, Engine
///     >>> bundle = AssetBundle()
///     >>> bundle.add_css("body { font-family: sans-serif; }")
///     >>> engine = Engine(assets=bundle)
#[pyclass(name = "AssetBundle", module = "pyfulgur")]
pub struct PyAssetBundle { ... }

#[pymethods]
impl PyAssetBundle {
    /// Create an empty asset bundle.
    #[new]
    fn new() -> Self { ... }

    /// Add an inline CSS string to the bundle.
    ///
    /// Args:
    ///     css: CSS source text.
    fn add_css(&mut self, css: &str) { ... }

    /// Add a CSS file from disk.
    ///
    /// Args:
    ///     path: Filesystem path to the CSS file.
    ///
    /// Raises:
    ///     FileNotFoundError: When ``path`` does not exist.
    ///     ValueError: When the CSS file cannot be read.
    fn add_css_file(...) -> PyResult<()> { ... }

    /// Add a font file (TTF/OTF/WOFF/WOFF2) from disk.
    ///
    /// Args:
    ///     path: Filesystem path to the font file.
    ///
    /// Raises:
    ///     FileNotFoundError: When ``path`` does not exist.
    ///     ValueError: When the font format is unsupported.
    fn add_font_file(...) -> PyResult<()> { ... }

    /// Add an image with explicit name and bytes.
    ///
    /// Args:
    ///     name: Logical name to reference from CSS / HTML.
    ///     data: Raw image bytes (PNG / JPEG / etc.).
    fn add_image(...) { ... }

    /// Add an image file from disk.
    ///
    /// Args:
    ///     name: Logical name to reference from CSS / HTML.
    ///     path: Filesystem path to the image file.
    ///
    /// Raises:
    ///     FileNotFoundError: When ``path`` does not exist.
    fn add_image_file(...) -> PyResult<()> { ... }
}
```

**Step 6: `src/engine.rs` に doc comment を追加**

`PyEngineBuilder`, `PyEngine` 両クラスにクラス概要 + 全メソッドに Args/Returns/Raises を追加する。

主要 docstring:

```rust
/// Builder for configuring and constructing an :class:`Engine`.
///
/// All setters return ``self`` to support method chaining. Each instance can
/// be ``build()`` ed only once; subsequent calls raise ``RuntimeError``.
///
/// Example:
///     >>> from pyfulgur import Engine, PageSize, Margin
///     >>> engine = (
///     ...     Engine.builder()
///     ...     .page_size(PageSize.A4)
///     ...     .margin(Margin.uniform_mm(20.0))
///     ...     .title("Doc")
///     ...     .build()
///     ... )
#[pyclass(name = "EngineBuilder", module = "pyfulgur")]
pub struct PyEngineBuilder { ... }
```

```rust
/// HTML/CSS to PDF conversion engine.
///
/// fulgur converts HTML / CSS / SVG into a PDF document deterministically and
/// offline. All assets must be explicitly bundled via :class:`AssetBundle`.
///
/// Example:
///     >>> from pyfulgur import Engine, PageSize
///     >>> engine = Engine(page_size=PageSize.A4, title="Hello")
///     >>> pdf = engine.render_html("<h1>Hi</h1>")
///     >>> assert pdf.startswith(b"%PDF")
#[pyclass(name = "Engine", module = "pyfulgur")]
pub struct PyEngine { ... }
```

```rust
/// Render HTML to PDF bytes.
///
/// The Python GIL is released for the duration of rendering so multiple
/// threads can render in parallel.
///
/// Args:
///     html: HTML source string.
///
/// Returns:
///     PDF bytes (always starts with ``b"%PDF"``).
///
/// Raises:
///     RenderError: When parsing, layout, or PDF generation fails.
fn render_html<'py>(...) -> ... { ... }
```

```rust
/// Render HTML and write the PDF to a file.
///
/// Args:
///     html: HTML source string.
///     path: Filesystem path to write the PDF to.
///
/// Raises:
///     RenderError: When rendering or writing fails.
///     FileNotFoundError: When the parent directory does not exist.
fn render_html_to_file(...) -> PyResult<()> { ... }
```

EngineBuilder の各 setter に対しては:

```rust
/// Set the page size.
///
/// Args:
///     value: Either a :class:`PageSize` instance or a string name
///         (``"A4"``, ``"LETTER"``, ``"A3"``; case-insensitive).
///
/// Returns:
///     ``self`` for method chaining.
///
/// Raises:
///     ValueError: When ``value`` is an unknown size name.
fn page_size(...) -> PyResult<Py<Self>> { ... }
```

(landscape, title, author, lang, bookmarks, assets, margin, build) 同様のテンプレ。

**Step 7: `src/error.rs` に doc を追加**

```rust
create_exception!(
    pyfulgur,
    RenderError,
    PyException,
    "Raised when fulgur fails to render an HTML document to PDF."
);
```

**Step 8: rebuild + テストが PASS することを確認**

```bash
maturin develop -m crates/pyfulgur/Cargo.toml --release 2>&1 | tail -5
python3 -m pytest crates/pyfulgur/tests/test_docstrings.py -v 2>&1 | tail -30
```

期待: すべて PASS。

**Step 9: 既存テストが green を維持することを確認**

```bash
python3 -m pytest crates/pyfulgur/tests/ -v 2>&1 | tail -30
cargo test --workspace --exclude fulgur-vrt 2>&1 | tail -10
cargo clippy -p pyfulgur --features extension-module --all-targets -- -D warnings 2>&1 | tail -10
```

期待: すべて PASS。warnings 0。

**Step 10: コミット**

```bash
git add crates/pyfulgur/src/page_size.rs \
        crates/pyfulgur/src/margin.rs \
        crates/pyfulgur/src/asset_bundle.rs \
        crates/pyfulgur/src/engine.rs \
        crates/pyfulgur/src/error.rs \
        crates/pyfulgur/tests/test_docstrings.py
git commit -m "docs(pyfulgur): add Google-style docstrings via Rust /// comments"
```

---

## Task 3: `__init__.pyi` 型スタブを作成

**Files:**
- Create: `crates/pyfulgur/python/pyfulgur/__init__.pyi`

**Step 1: テストはすでに Task 1 で `test_pyi_stub_exists` を書いている**

念のため確認:

```bash
python3 -m pytest crates/pyfulgur/tests/test_typed.py::test_pyi_stub_exists -v
```

期待: FAIL (まだ stub を書いていない)。

**Step 2: 型情報の網羅された `__init__.pyi` を作成**

```python
"""Type stubs for pyfulgur.

Docstrings are intentionally omitted here — the canonical source is the Rust
/// doc comments (exposed via ``__doc__`` at runtime). griffe (mkdocstrings) and
modern IDEs merge ``.pyi`` types with runtime docstrings automatically.
"""

from __future__ import annotations

import os
from typing import Optional, Union

__version__: str
__all__: list[str]


class RenderError(Exception):
    ...


class PageSize:
    A4: PageSize
    LETTER: PageSize
    A3: PageSize

    @staticmethod
    def custom(width_mm: float, height_mm: float) -> PageSize: ...
    def landscape(self) -> PageSize: ...
    @property
    def width(self) -> float: ...
    @property
    def height(self) -> float: ...
    def __repr__(self) -> str: ...


class Margin:
    def __init__(
        self,
        top: float,
        right: float,
        bottom: float,
        left: float,
    ) -> None: ...
    @staticmethod
    def uniform(pt: float) -> Margin: ...
    @staticmethod
    def symmetric(vertical: float, horizontal: float) -> Margin: ...
    @staticmethod
    def uniform_mm(mm: float) -> Margin: ...
    @property
    def top(self) -> float: ...
    @property
    def right(self) -> float: ...
    @property
    def bottom(self) -> float: ...
    @property
    def left(self) -> float: ...
    def __repr__(self) -> str: ...


class AssetBundle:
    def __init__(self) -> None: ...
    def add_css(self, css: str) -> None: ...
    def add_css_file(self, path: Union[str, os.PathLike[str]]) -> None: ...
    def add_font_file(self, path: Union[str, os.PathLike[str]]) -> None: ...
    def add_image(self, name: str, data: bytes) -> None: ...
    def add_image_file(
        self, name: str, path: Union[str, os.PathLike[str]]
    ) -> None: ...


class EngineBuilder:
    def __init__(self) -> None: ...
    def page_size(self, value: Union[PageSize, str]) -> EngineBuilder: ...
    def margin(self, margin: Margin) -> EngineBuilder: ...
    def landscape(self, value: bool) -> EngineBuilder: ...
    def title(self, value: str) -> EngineBuilder: ...
    def author(self, value: str) -> EngineBuilder: ...
    def lang(self, value: str) -> EngineBuilder: ...
    def bookmarks(self, value: bool) -> EngineBuilder: ...
    def assets(self, bundle: AssetBundle) -> EngineBuilder: ...
    def build(self) -> Engine: ...


class Engine:
    def __init__(
        self,
        *,
        page_size: Optional[Union[PageSize, str]] = ...,
        margin: Optional[Margin] = ...,
        landscape: Optional[bool] = ...,
        title: Optional[str] = ...,
        author: Optional[str] = ...,
        lang: Optional[str] = ...,
        bookmarks: Optional[bool] = ...,
        assets: Optional[AssetBundle] = ...,
    ) -> None: ...
    @staticmethod
    def builder() -> EngineBuilder: ...
    def render_html(self, html: str) -> bytes: ...
    def render_html_to_file(
        self, html: str, path: Union[str, os.PathLike[str]]
    ) -> None: ...
```

**注意点**:
- Python 3.9 サポートのため `from __future__ import annotations` を冒頭に置き、`Optional[X]` / `Union[X, Y]` を使う (PEP 604 `X | Y` は 3.10+ なので 3.9 でも parse できる .pyi 構文に統一)
- `os.PathLike[str]` は 3.9+ で問題なし

**Step 3: テストが PASS することを確認**

```bash
maturin develop -m crates/pyfulgur/Cargo.toml --release 2>&1 | tail -5
python3 -m pytest crates/pyfulgur/tests/test_typed.py -v
```

期待: 両方 PASS。

**Step 4: 手動 mypy --strict セルフチェック**

```bash
pip install mypy 2>&1 | tail -2
cat > /tmp/mypy_pyfulgur_check.py << 'PYEOF'
"""mypy --strict セルフチェック: 型エラーが期待通り検出されるか。"""
from pyfulgur import AssetBundle, Engine, EngineBuilder, Margin, PageSize, RenderError

# valid uses
bundle: AssetBundle = AssetBundle()
bundle.add_css("body { color: red; }")

engine: Engine = Engine(page_size=PageSize.A4, margin=Margin.uniform(36.0), assets=bundle)
pdf: bytes = engine.render_html("<h1>Hi</h1>")

builder: EngineBuilder = Engine.builder()
engine2: Engine = builder.page_size("A4").build()

# Should be flagged by mypy --strict:
# engine.render_html(123)  # type: error: Argument 1 has incompatible type "int"; expected "str"
# Margin("a", "b", "c", "d")  # type: error
PYEOF
python3 -m mypy --strict /tmp/mypy_pyfulgur_check.py 2>&1 | tail -10
```

期待: `Success: no issues found in 1 source file`。コメントアウト行のうち 1 つを有効化して `error` が出ることを確認 (確認後コメントアウトに戻す)。

**Step 5: コミット**

```bash
git add crates/pyfulgur/python/pyfulgur/__init__.pyi
git commit -m "feat(pyfulgur): add type stubs (__init__.pyi) for public API"
```

---

## Task 4: README とドキュメント整合

**Files:**
- Modify: `crates/pyfulgur/README.md` (Type stubs / mkdocstrings 対応の旨を追記)

**Step 1: README に short note を追加**

`crates/pyfulgur/README.md` の「API surface」セクション直下に "Type stubs" 小見出しを追加:

```markdown
## Type stubs

pyfulgur ships PEP 561 type stubs (`py.typed` + `__init__.pyi`) so that
mypy / pyright / IDEs can type-check call sites against the native API.
Docstrings are exposed via PyO3 `__doc__` for `help()` / Jupyter `?` / griffe
inspection.
```

**Step 2: コミット**

```bash
git add crates/pyfulgur/README.md
git commit -m "docs(pyfulgur): note PEP 561 type stubs in README"
```

---

## Task 5: 最終 full verification

**Step 1: workspace test (extension-module off)**

```bash
cargo test --workspace --exclude fulgur-vrt 2>&1 | tail -15
```

**Step 2: pyfulgur clippy (extension-module on)**

```bash
cargo clippy -p pyfulgur --features extension-module --all-targets -- -D warnings 2>&1 | tail -10
```

**Step 3: pyfulgur fmt**

```bash
cargo fmt --check 2>&1 | tail -10
```

**Step 4: maturin build (release wheel)**

```bash
maturin build -m crates/pyfulgur/Cargo.toml --release 2>&1 | tail -5
ls -la target/wheels/ | tail -5
```

**Step 5: wheel 内容を確認**

```bash
WHL=$(ls target/wheels/pyfulgur-*.whl | head -1)
python3 -m zipfile -l "$WHL" | grep -E "(py.typed|__init__\.pyi|\.so)" 
```

期待: `pyfulgur/py.typed`, `pyfulgur/__init__.pyi`, `pyfulgur/_native*.so` が含まれている。

**Step 6: pytest 全部**

```bash
python3 -m pytest crates/pyfulgur/tests/ -v 2>&1 | tail -30
```

期待: 全 PASS。

**Step 7: markdownlint** (README 修正のため)

```bash
npx markdownlint-cli2 'crates/pyfulgur/README.md' 2>&1 | tail -10
```

期待: errors 0。

---

## Closing

verification 全 PASS したら:

1. `bd close fulgur-t393`
2. PR を作成 (title 英語、body 日本語) — fulgur-lxy0 (mkdocstrings 連携) の prerequisite が解消したことを記載
