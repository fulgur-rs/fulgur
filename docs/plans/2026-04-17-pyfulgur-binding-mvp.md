# pyfulgur Python Binding MVP Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `crates/pyfulgur/` の placeholder を PyO3 + maturin で構築した実動作 Python binding (v0.0.2) に置き換え、`Engine(page_size="A4")` / `Engine.builder().page_size(PageSize.A4).build()` / `render_html(html) -> bytes` を提供する。

**Architecture:** fulgur (Rust crate, path 依存) を PyO3 の `extension-module` でラップする。PyO3 ラッパー型 (`PyEngine`, `PyEngineBuilder`, `PyAssetBundle`, `PyPageSize`, `PyMargin`) を `src/lib.rs` の `#[pymodule]` に登録。`render_html` は `py.allow_threads` で GIL 解放。エラーは PyO3 例外に変換 (`FileNotFoundError` / `ValueError` / カスタム `pyfulgur.RenderError`)。テストは pytest (`maturin develop` でビルド後実行)。

**Tech Stack:** Rust 1.85+, PyO3 0.22+, maturin 1.7+, Python 3.9+, pytest, fulgur (workspace path dep)

**Scope exclusions (将来バージョンで対応):**

- バッチ API
- サンドボックス / プロセス分離
- Template (MiniJinja) API
- config マッピングの網羅 (title/authors/bookmarks 以外のメタデータは未実装で可)

---

## 前提コンテキスト

### fulgur 公開 API (このバインディングが参照する)

`crates/fulgur/src/lib.rs` で `pub use` されている型のみ使う:

- `fulgur::Engine` — `Engine::builder() -> EngineBuilder`, `engine.render_html(&str) -> Result<Vec<u8>>`, `engine.render_html_to_file(&str, path)`
- `fulgur::EngineBuilder` — `.page_size(PageSize)`, `.margin(Margin)`, `.landscape(bool)`, `.title(String)`, `.author(String)`, `.lang(String)`, `.bookmarks(bool)`, `.assets(AssetBundle)`, `.base_path(PathBuf)`, `.build()`
- `fulgur::PageSize` — 定数 `A4` / `LETTER` / `A3`, `PageSize::custom(width_mm, height_mm)`, `.landscape()`
- `fulgur::Margin` — `Margin::uniform(pt)`, `Margin::symmetric(v, h)`, `Margin::uniform_mm(mm)`, フィールド `top/right/bottom/left: f32`
- `fulgur::AssetBundle` — `AssetBundle::new()`, `.add_css(String)`, `.add_css_file(path) -> Result`, `.add_font_file(path) -> Result`, `.add_image(name, Vec<u8>)`, `.add_image_file(name, path) -> Result`
- `fulgur::Error` — enum variants: `HtmlParse`, `Layout`, `PdfGeneration`, `Io(std::io::Error)`, `Asset`, `Template`, `WoffDecode`, `UnsupportedFontFormat`

### 既存 placeholder (置き換え対象)

- `crates/pyfulgur/README.md` — 置き換え (placeholder 表記を外す)
- `crates/pyfulgur/pyproject.toml` — setuptools → maturin に書き換え
- `crates/pyfulgur/pyfulgur/__init__.py` — 削除 (maturin が `.so` を直接 import させる)
- `crates/pyfulgur/pyfulgur/` ディレクトリ — 削除
- `crates/pyfulgur/src/lib.rs` — スタブから PyO3 実装に置き換え
- `crates/pyfulgur/.gitignore` — `target/` 追加

### Workspace ルート

`Cargo.toml` に `crates/pyfulgur` を追加するが、`pyo3/extension-module` を無条件に有効化すると `cargo test --workspace` / `cargo build --workspace` で libpython undefined symbol link error になる。

解決策: pyo3 を **optional 依存** にして `extension-module` feature で gate する。

- `[features] extension-module = ["dep:pyo3", "pyo3/extension-module"]` (default なし)
- `pyo3 = { version = "0.22", optional = true }`
- 各ソースファイル (`src/lib.rs`, `src/page_size.rs` 等) は全て `#[cfg(feature = "extension-module")]` で gate → feature off 時は空 crate としてコンパイル成功
- maturin は `[tool.maturin] features = ["extension-module"]` で feature を有効化
- `cargo build --workspace` (feature off) は pyfulgur を空 crate として通す、`maturin develop` が extension-module 有効化したバイナリを作る

### テスト戦略

- Rust 側 unit test は最小限 (PyO3 wrapper の内部変換用)
- 主な検証は pytest: `maturin develop` で venv にインストール → `pytest tests/`
- venv は `.worktrees/pyfulgur/crates/pyfulgur/.venv` に作成 (ローカル専用、gitignore 済み)
- maturin / pyo3 / 等は uv 経由で入れる

---

## Task 1: Cargo workspace に pyfulgur を追加 & 基盤ファイル作成

**Files:**

- Modify: `Cargo.toml` (workspace)
- Create: `crates/pyfulgur/Cargo.toml`
- Modify: `crates/pyfulgur/src/lib.rs`
- Delete: `crates/pyfulgur/pyfulgur/__init__.py`
- Delete: `crates/pyfulgur/pyfulgur/` (ディレクトリごと)
- Modify: `crates/pyfulgur/.gitignore`

**Step 1: workspace Cargo.toml に pyfulgur を追加**

`Cargo.toml` (worktree ルート) を以下に変更:

```toml
[workspace]
resolver = "2"
members = ["crates/fulgur", "crates/fulgur-cli", "crates/fulgur-vrt", "crates/pyfulgur"]

[workspace.package]
edition = "2024"
rust-version = "1.85.0"
license = "MIT OR Apache-2.0"
repository = "https://github.com/mitsuru/fulgur"
homepage = "https://github.com/mitsuru/fulgur"
```

**Step 2: pyfulgur の Cargo.toml を作成**

`crates/pyfulgur/Cargo.toml`:

```toml
[package]
name = "pyfulgur"
version = "0.0.2"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
description = "Python bindings for fulgur — offline HTML/CSS to PDF conversion"
publish = false

[lib]
name = "pyfulgur"
crate-type = ["cdylib", "rlib"]

[features]
# extension-module を有効化すると pyo3 を引き込み、libpython への undefined symbol
# リンクを抑止する PyO3 モードでビルドする。maturin ビルドでのみ有効化し、
# 通常の cargo build --workspace では off にして workspace テストを壊さない。
extension-module = ["dep:pyo3", "pyo3/extension-module"]

[dependencies]
fulgur = { path = "../fulgur" }
pyo3 = { version = "0.22", optional = true }

[dev-dependencies]
static_assertions = "1.1"
```

**Step 3: placeholder Python ディレクトリを削除**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur
rm -rf crates/pyfulgur/pyfulgur
```

**Step 4: src/lib.rs を feature-gated な空の pymodule に書き換え**

`crates/pyfulgur/src/lib.rs`:

```rust
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
```

**Step 5: .gitignore に target/ と .venv/ を追加**

`crates/pyfulgur/.gitignore`:

```text
*.egg-info/
dist/
target/
.venv/
__pycache__/
*.so
```

**Step 6: workspace が通ることを確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur
cargo check --workspace            # feature off → pyfulgur は空 crate、libpython 不要
cargo check -p pyfulgur --features extension-module  # feature on ビルド
```

Expected: 両方とも成功。feature off 時は pyfulgur が実質空で通る。feature on 時は pyo3 が入って pymodule がビルドされる。

**Step 7: Commit**

```bash
git add Cargo.toml crates/pyfulgur/Cargo.toml crates/pyfulgur/src/lib.rs crates/pyfulgur/.gitignore
git rm -r crates/pyfulgur/pyfulgur
git commit -m "feat(pyfulgur): wire PyO3 extension crate into workspace"
```

---

## Task 2: pyproject.toml を maturin ビルドに切り替え & テスト環境構築

**Files:**

- Modify: `crates/pyfulgur/pyproject.toml`
- Create: `crates/pyfulgur/tests/__init__.py`
- Create: `crates/pyfulgur/tests/test_smoke.py`

**Step 1: pyproject.toml を maturin 版に書き換え**

`crates/pyfulgur/pyproject.toml`:

```toml
[build-system]
requires = ["maturin>=1.7,<2.0"]
build-backend = "maturin"

[project]
name = "pyfulgur"
version = "0.0.2"
description = "Python bindings for fulgur — offline HTML/CSS to PDF conversion"
readme = "README.md"
license = { text = "MIT OR Apache-2.0" }
requires-python = ">=3.9"
authors = [
    { name = "Mitsuru Hayasaka", email = "hayasaka.mitsuru@gmail.com" },
]
keywords = ["pdf", "html", "css", "conversion", "typesetting"]
classifiers = [
    "Development Status :: 3 - Alpha",
    "Intended Audience :: Developers",
    "Programming Language :: Python :: 3",
    "Programming Language :: Python :: 3.9",
    "Programming Language :: Python :: 3.10",
    "Programming Language :: Python :: 3.11",
    "Programming Language :: Python :: 3.12",
    "Programming Language :: Rust",
    "Topic :: Text Processing :: Markup :: HTML",
    "Topic :: Software Development :: Libraries :: Python Modules",
]

[project.urls]
Repository = "https://github.com/mitsuru/fulgur"
Documentation = "https://github.com/mitsuru/fulgur"
Changelog = "https://github.com/mitsuru/fulgur/blob/main/CHANGELOG.md"

[project.optional-dependencies]
test = ["pytest>=7"]

[tool.maturin]
module-name = "pyfulgur"
manifest-path = "Cargo.toml"
features = ["extension-module"]
```

**Step 2: tests/ ディレクトリを作成**

`crates/pyfulgur/tests/__init__.py`: 空ファイル

`crates/pyfulgur/tests/test_smoke.py`:

```python
def test_import_pyfulgur():
    import pyfulgur  # noqa: F401
```

**Step 3: venv と maturin をセットアップ**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur/crates/pyfulgur
uv venv .venv
source .venv/bin/activate
uv pip install maturin pytest
```

**Step 4: maturin develop でビルド**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur/crates/pyfulgur
maturin develop
```

Expected: build 成功、`.so` が venv に配置される。

**Step 5: smoke テストを走らせる**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur/crates/pyfulgur
pytest tests/test_smoke.py -v
```

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/pyfulgur/pyproject.toml crates/pyfulgur/tests/
git commit -m "build(pyfulgur): switch to maturin, add smoke test"
```

---

## Task 3: PyPageSize 型 (TDD)

**Files:**

- Create: `crates/pyfulgur/src/page_size.rs`
- Modify: `crates/pyfulgur/src/lib.rs`
- Modify: `crates/pyfulgur/tests/test_page_size.py` (Create)

**Step 1: Python 側で failing test を書く**

`crates/pyfulgur/tests/test_page_size.py`:

```python
import math

import pytest

from pyfulgur import PageSize


def test_a4_has_expected_dimensions():
    size = PageSize.A4
    assert math.isclose(size.width, 595.28, abs_tol=0.01)
    assert math.isclose(size.height, 841.89, abs_tol=0.01)


def test_letter_has_expected_dimensions():
    size = PageSize.LETTER
    assert math.isclose(size.width, 612.0, abs_tol=0.01)
    assert math.isclose(size.height, 792.0, abs_tol=0.01)


def test_a3_has_expected_dimensions():
    size = PageSize.A3
    assert math.isclose(size.width, 841.89, abs_tol=0.01)


def test_custom_mm_converts_to_points():
    a4 = PageSize.custom(210.0, 297.0)
    assert math.isclose(a4.width, 595.28, abs_tol=0.2)
    assert math.isclose(a4.height, 841.89, abs_tol=0.2)


def test_landscape_swaps_dimensions():
    a4_land = PageSize.A4.landscape()
    assert math.isclose(a4_land.width, 841.89, abs_tol=0.01)
    assert math.isclose(a4_land.height, 595.28, abs_tol=0.01)


def test_page_size_repr():
    s = PageSize.A4
    assert "PageSize" in repr(s)
```

**Step 2: テストを実行して失敗を確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur/crates/pyfulgur
pytest tests/test_page_size.py -v
```

Expected: ImportError (PageSize が module に存在しない).

**Step 3: PyPageSize を実装**

`crates/pyfulgur/src/page_size.rs`:

```rust
use fulgur::PageSize;
use pyo3::prelude::*;

#[pyclass(name = "PageSize", module = "pyfulgur", frozen)]
#[derive(Clone, Copy)]
pub struct PyPageSize {
    pub(crate) inner: PageSize,
}

#[pymethods]
impl PyPageSize {
    #[classattr]
    const A4: PyPageSize = PyPageSize {
        inner: PageSize::A4,
    };

    #[classattr]
    const LETTER: PyPageSize = PyPageSize {
        inner: PageSize::LETTER,
    };

    #[classattr]
    const A3: PyPageSize = PyPageSize {
        inner: PageSize::A3,
    };

    #[staticmethod]
    fn custom(width_mm: f32, height_mm: f32) -> Self {
        Self {
            inner: PageSize::custom(width_mm, height_mm),
        }
    }

    fn landscape(&self) -> Self {
        Self {
            inner: self.inner.landscape(),
        }
    }

    #[getter]
    fn width(&self) -> f32 {
        self.inner.width
    }

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

**Step 4: lib.rs に PyPageSize を登録**

`crates/pyfulgur/src/lib.rs`:

```rust
use pyo3::prelude::*;

mod page_size;

use page_size::PyPageSize;

#[pymodule]
fn pyfulgur(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPageSize>()?;
    Ok(())
}
```

**Step 5: rebuild & test**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur/crates/pyfulgur
maturin develop
pytest tests/test_page_size.py -v
```

Expected: 全 PASS.

**Step 6: Commit**

```bash
git add crates/pyfulgur/src/lib.rs crates/pyfulgur/src/page_size.rs crates/pyfulgur/tests/test_page_size.py
git commit -m "feat(pyfulgur): add PageSize class with A4/LETTER/A3 + custom/landscape"
```

---

## Task 4: PyMargin 型 (TDD)

**Files:**

- Create: `crates/pyfulgur/src/margin.rs`
- Modify: `crates/pyfulgur/src/lib.rs`
- Create: `crates/pyfulgur/tests/test_margin.py`

**Step 1: failing test を書く**

`crates/pyfulgur/tests/test_margin.py`:

```python
import math

import pytest

from pyfulgur import Margin


def test_margin_new_with_all_sides():
    m = Margin(top=10.0, right=20.0, bottom=30.0, left=40.0)
    assert m.top == 10.0
    assert m.right == 20.0
    assert m.bottom == 30.0
    assert m.left == 40.0


def test_margin_uniform():
    m = Margin.uniform(15.0)
    assert m.top == m.right == m.bottom == m.left == 15.0


def test_margin_symmetric():
    m = Margin.symmetric(vertical=10.0, horizontal=20.0)
    assert m.top == m.bottom == 10.0
    assert m.left == m.right == 20.0


def test_margin_uniform_mm():
    m = Margin.uniform_mm(25.4)
    assert math.isclose(m.top, 72.0, abs_tol=0.01)


def test_margin_repr():
    m = Margin.uniform(10.0)
    assert "Margin" in repr(m)
```

**Step 2: テストを走らせ失敗確認**

```bash
pytest tests/test_margin.py -v
```

Expected: ImportError.

**Step 3: PyMargin を実装**

`crates/pyfulgur/src/margin.rs`:

```rust
use fulgur::Margin;
use pyo3::prelude::*;

#[pyclass(name = "Margin", module = "pyfulgur", frozen)]
#[derive(Clone, Copy)]
pub struct PyMargin {
    pub(crate) inner: Margin,
}

#[pymethods]
impl PyMargin {
    #[new]
    #[pyo3(signature = (top, right, bottom, left))]
    fn new(top: f32, right: f32, bottom: f32, left: f32) -> Self {
        Self {
            inner: Margin { top, right, bottom, left },
        }
    }

    #[staticmethod]
    fn uniform(pt: f32) -> Self {
        Self { inner: Margin::uniform(pt) }
    }

    #[staticmethod]
    #[pyo3(signature = (vertical, horizontal))]
    fn symmetric(vertical: f32, horizontal: f32) -> Self {
        Self { inner: Margin::symmetric(vertical, horizontal) }
    }

    #[staticmethod]
    fn uniform_mm(mm: f32) -> Self {
        Self { inner: Margin::uniform_mm(mm) }
    }

    #[getter]
    fn top(&self) -> f32 { self.inner.top }
    #[getter]
    fn right(&self) -> f32 { self.inner.right }
    #[getter]
    fn bottom(&self) -> f32 { self.inner.bottom }
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

**Step 4: lib.rs に登録**

`crates/pyfulgur/src/lib.rs` に追加:

```rust
mod margin;
use margin::PyMargin;
```

`#[pymodule]` fn 内に `m.add_class::<PyMargin>()?;` を追加。

**Step 5: rebuild & test**

```bash
maturin develop
pytest tests/test_margin.py -v
```

Expected: 全 PASS.

**Step 6: Commit**

```bash
git add crates/pyfulgur/src/margin.rs crates/pyfulgur/src/lib.rs crates/pyfulgur/tests/test_margin.py
git commit -m "feat(pyfulgur): add Margin class with uniform/symmetric/uniform_mm"
```

---

## Task 5: PyAssetBundle 型 (TDD)

**Files:**

- Create: `crates/pyfulgur/src/asset_bundle.rs`
- Modify: `crates/pyfulgur/src/lib.rs`
- Create: `crates/pyfulgur/tests/test_asset_bundle.py`

**Step 1: failing test を書く**

`crates/pyfulgur/tests/test_asset_bundle.py`:

```python
from pathlib import Path

import pytest

from pyfulgur import AssetBundle


def test_asset_bundle_add_css():
    bundle = AssetBundle()
    bundle.add_css("body { color: red; }")
    # CSS is stored internally; exposed via later rendering.
    assert bundle is not None  # smoke


def test_asset_bundle_add_css_file(tmp_path):
    css_file = tmp_path / "style.css"
    css_file.write_text("p { margin: 0; }")
    bundle = AssetBundle()
    bundle.add_css_file(str(css_file))


def test_asset_bundle_add_css_file_missing_raises_file_not_found(tmp_path):
    bundle = AssetBundle()
    with pytest.raises(FileNotFoundError):
        bundle.add_css_file(str(tmp_path / "nope.css"))


def test_asset_bundle_add_image_bytes():
    bundle = AssetBundle()
    png = bytes(
        [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
            0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41,
            0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
            0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92,
            0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
            0x44, 0xAE, 0x42, 0x60, 0x82,
        ]
    )
    bundle.add_image("one.png", png)


def test_asset_bundle_add_image_file(tmp_path):
    img = tmp_path / "x.png"
    img.write_bytes(b"\x89PNGstub")
    bundle = AssetBundle()
    bundle.add_image_file("x.png", str(img))


def test_asset_bundle_add_font_file_missing_raises_file_not_found(tmp_path):
    bundle = AssetBundle()
    with pytest.raises(FileNotFoundError):
        bundle.add_font_file(str(tmp_path / "nope.ttf"))
```

**Step 2: 失敗を確認**

```bash
pytest tests/test_asset_bundle.py -v
```

Expected: ImportError.

**Step 3: PyAssetBundle を実装**

`crates/pyfulgur/src/asset_bundle.rs`:

```rust
use fulgur::AssetBundle;
use pyo3::exceptions::{PyFileNotFoundError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::path::PathBuf;

use crate::error::map_fulgur_error;

#[pyclass(name = "AssetBundle", module = "pyfulgur")]
pub struct PyAssetBundle {
    pub(crate) inner: AssetBundle,
}

#[pymethods]
impl PyAssetBundle {
    #[new]
    fn new() -> Self {
        Self {
            inner: AssetBundle::new(),
        }
    }

    fn add_css(&mut self, css: &str) {
        self.inner.add_css(css);
    }

    fn add_css_file(&mut self, path: PathBuf) -> PyResult<()> {
        self.inner
            .add_css_file(path)
            .map_err(|e| map_fulgur_error(e))
    }

    fn add_font_file(&mut self, path: PathBuf) -> PyResult<()> {
        self.inner
            .add_font_file(path)
            .map_err(|e| map_fulgur_error(e))
    }

    fn add_image(&mut self, name: &str, data: &Bound<'_, PyBytes>) {
        self.inner.add_image(name, data.as_bytes().to_vec());
    }

    fn add_image_file(&mut self, name: &str, path: PathBuf) -> PyResult<()> {
        self.inner
            .add_image_file(name, path)
            .map_err(|e| map_fulgur_error(e))
    }
}

impl PyAssetBundle {
    pub(crate) fn take_inner(&mut self) -> AssetBundle {
        std::mem::replace(&mut self.inner, AssetBundle::new())
    }
}
```

Note: `map_fulgur_error` は Task 9 で定義する `crate::error` モジュールから import。一時的に inline で書いてもよい:

```rust
// 一時実装 (Task 9 で置き換え)
use fulgur::Error as FulgurError;
fn map_fulgur_error(err: FulgurError) -> PyErr {
    match err {
        FulgurError::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => {
            PyFileNotFoundError::new_err(io_err.to_string())
        }
        _ => PyValueError::new_err(err.to_string()),
    }
}
```

**Step 4: lib.rs に登録**

`crates/pyfulgur/src/lib.rs` に `mod asset_bundle; use asset_bundle::PyAssetBundle; m.add_class::<PyAssetBundle>()?;`

**Step 5: rebuild & test**

```bash
maturin develop
pytest tests/test_asset_bundle.py -v
```

Expected: 全 PASS.

**Step 6: Commit**

```bash
git add crates/pyfulgur/src/asset_bundle.rs crates/pyfulgur/src/lib.rs crates/pyfulgur/tests/test_asset_bundle.py
git commit -m "feat(pyfulgur): add AssetBundle with css/font/image registration"
```

---

## Task 6: PyEngineBuilder 型 (TDD)

**Files:**

- Create: `crates/pyfulgur/src/engine.rs`
- Modify: `crates/pyfulgur/src/lib.rs`
- Create: `crates/pyfulgur/tests/test_engine_builder.py`

`PyEngineBuilder` は `Option<fulgur::EngineBuilder>` を内部に持ち、`.build()` で `.take()` する。ビルダー型メソッドは self を mut で受けて `*self = ...` 更新か、builder pattern を self return させる Python 流儀にする。PyO3 の簡単な流儀は、builder がメソッドチェーン毎に self を返し、内部状態は `Option<EngineBuilder>` で書き換える。

**Step 1: failing test を書く**

`crates/pyfulgur/tests/test_engine_builder.py`:

```python
import pytest

from pyfulgur import AssetBundle, Engine, Margin, PageSize


def test_builder_returns_engine():
    engine = Engine.builder().build()
    assert engine is not None


def test_builder_page_size_accepts_page_size_obj():
    engine = Engine.builder().page_size(PageSize.A4).build()
    assert engine is not None


def test_builder_page_size_accepts_string():
    engine = Engine.builder().page_size("LETTER").build()
    assert engine is not None


def test_builder_page_size_invalid_string_raises_value_error():
    with pytest.raises(ValueError):
        Engine.builder().page_size("Z99").build()


def test_builder_landscape_and_margin():
    engine = (
        Engine.builder()
        .page_size(PageSize.A4)
        .landscape(True)
        .margin(Margin.uniform(36.0))
        .build()
    )
    assert engine is not None


def test_builder_title_author_lang_bookmarks():
    engine = (
        Engine.builder()
        .title("Hello")
        .author("Alice")
        .lang("ja-JP")
        .bookmarks(True)
        .build()
    )
    assert engine is not None


def test_builder_assets_consumes_bundle():
    bundle = AssetBundle()
    bundle.add_css("body {}")
    engine = Engine.builder().assets(bundle).build()
    assert engine is not None


def test_builder_build_consumes_builder():
    b = Engine.builder()
    b.build()
    with pytest.raises(RuntimeError):
        b.build()
```

**Step 2: 失敗を確認**

**Step 3: PyEngineBuilder + PyEngine を実装**

`crates/pyfulgur/src/engine.rs`:

```rust
use fulgur::{Engine, EngineBuilder, PageSize};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use crate::asset_bundle::PyAssetBundle;
use crate::margin::PyMargin;
use crate::page_size::PyPageSize;

#[pyclass(name = "EngineBuilder", module = "pyfulgur")]
pub struct PyEngineBuilder {
    inner: Option<EngineBuilder>,
}

impl PyEngineBuilder {
    fn take(&mut self) -> PyResult<EngineBuilder> {
        self.inner
            .take()
            .ok_or_else(|| PyRuntimeError::new_err("EngineBuilder has already been built"))
    }

    fn map(&mut self, f: impl FnOnce(EngineBuilder) -> EngineBuilder) -> PyResult<()> {
        let b = self.take()?;
        self.inner = Some(f(b));
        Ok(())
    }
}

fn parse_page_size_str(name: &str) -> PyResult<PageSize> {
    match name.to_ascii_uppercase().as_str() {
        "A4" => Ok(PageSize::A4),
        "LETTER" => Ok(PageSize::LETTER),
        "A3" => Ok(PageSize::A3),
        other => Err(PyValueError::new_err(format!(
            "unknown page size: {other}"
        ))),
    }
}

#[pymethods]
impl PyEngineBuilder {
    #[new]
    fn new() -> Self {
        Self {
            inner: Some(Engine::builder()),
        }
    }

    fn page_size(mut slf: PyRefMut<'_, Self>, value: &Bound<'_, PyAny>) -> PyResult<Py<Self>> {
        let size = if let Ok(ps) = value.extract::<PyPageSize>() {
            ps.inner
        } else if let Ok(s) = value.extract::<String>() {
            parse_page_size_str(&s)?
        } else {
            return Err(PyValueError::new_err(
                "page_size must be PageSize or str",
            ));
        };
        slf.map(|b| b.page_size(size))?;
        Ok(slf.into())
    }

    fn margin(mut slf: PyRefMut<'_, Self>, margin: PyMargin) -> PyResult<Py<Self>> {
        slf.map(|b| b.margin(margin.inner))?;
        Ok(slf.into())
    }

    fn landscape(mut slf: PyRefMut<'_, Self>, value: bool) -> PyResult<Py<Self>> {
        slf.map(|b| b.landscape(value))?;
        Ok(slf.into())
    }

    fn title(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.title(value))?;
        Ok(slf.into())
    }

    fn author(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.author(value))?;
        Ok(slf.into())
    }

    fn lang(mut slf: PyRefMut<'_, Self>, value: String) -> PyResult<Py<Self>> {
        slf.map(|b| b.lang(value))?;
        Ok(slf.into())
    }

    fn bookmarks(mut slf: PyRefMut<'_, Self>, value: bool) -> PyResult<Py<Self>> {
        slf.map(|b| b.bookmarks(value))?;
        Ok(slf.into())
    }

    fn assets(mut slf: PyRefMut<'_, Self>, bundle: &Bound<'_, PyAssetBundle>) -> PyResult<Py<Self>> {
        let taken = bundle.borrow_mut().take_inner();
        slf.map(|b| b.assets(taken))?;
        Ok(slf.into())
    }

    fn build(&mut self) -> PyResult<PyEngine> {
        let b = self.take()?;
        Ok(PyEngine { inner: b.build() })
    }
}

#[pyclass(name = "Engine", module = "pyfulgur")]
pub struct PyEngine {
    pub(crate) inner: Engine,
}

#[pymethods]
impl PyEngine {
    #[staticmethod]
    fn builder() -> PyEngineBuilder {
        PyEngineBuilder::new()
    }
}
```

**Step 4: lib.rs に登録**

`crates/pyfulgur/src/lib.rs`:

```rust
mod asset_bundle;
mod engine;
mod error;
mod margin;
mod page_size;

use asset_bundle::PyAssetBundle;
use engine::{PyEngine, PyEngineBuilder};
use margin::PyMargin;
use page_size::PyPageSize;

#[pymodule]
fn pyfulgur(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPageSize>()?;
    m.add_class::<PyMargin>()?;
    m.add_class::<PyAssetBundle>()?;
    m.add_class::<PyEngineBuilder>()?;
    m.add_class::<PyEngine>()?;
    Ok(())
}
```

**Step 5: rebuild & test**

```bash
maturin develop
pytest tests/test_engine_builder.py -v
```

Expected: 全 PASS.

**Step 6: Commit**

```bash
git add crates/pyfulgur/src/engine.rs crates/pyfulgur/src/lib.rs crates/pyfulgur/tests/test_engine_builder.py
git commit -m "feat(pyfulgur): add EngineBuilder with chainable config methods"
```

---

## Task 7: Engine.render_html() (TDD, GIL 解放)

**Files:**

- Modify: `crates/pyfulgur/src/engine.rs`
- Create: `crates/pyfulgur/tests/test_render_html.py`

**Step 1: failing test を書く**

`crates/pyfulgur/tests/test_render_html.py`:

```python
import pytest

from pyfulgur import AssetBundle, Engine, PageSize


def test_render_html_returns_pdf_bytes():
    engine = Engine.builder().page_size(PageSize.A4).build()
    pdf = engine.render_html("<h1>Hello</h1>")
    assert isinstance(pdf, bytes)
    assert pdf.startswith(b"%PDF")


def test_render_html_with_css_bundle():
    bundle = AssetBundle()
    bundle.add_css("h1 { color: blue; }")
    engine = Engine.builder().assets(bundle).build()
    pdf = engine.render_html("<h1>Styled</h1>")
    assert pdf.startswith(b"%PDF")


def test_render_html_multiple_times_succeeds():
    engine = Engine.builder().build()
    pdf1 = engine.render_html("<p>a</p>")
    pdf2 = engine.render_html("<p>b</p>")
    assert pdf1.startswith(b"%PDF")
    assert pdf2.startswith(b"%PDF")
```

**Step 2: テスト失敗を確認**

```bash
pytest tests/test_render_html.py -v
```

Expected: `AttributeError: 'Engine' object has no attribute 'render_html'`.

**Step 3: render_html を実装**

`crates/pyfulgur/src/engine.rs` の `impl PyEngine` に追加:

```rust
use pyo3::types::PyBytes;

#[pymethods]
impl PyEngine {
    // ... 既存の builder() ...

    fn render_html<'py>(&self, py: Python<'py>, html: String) -> PyResult<Bound<'py, PyBytes>> {
        // Engine: Send + Sync は fulgur-d3r で保証済み + src/lib.rs の
        // assert_impl_all! で compile time に検査している。Python スレッドから
        // 並列で render できるよう、GIL を解放してから呼ぶ。
        let bytes = py
            .allow_threads(|| self.inner.render_html(&html))
            .map_err(crate::error::map_fulgur_error)?;
        Ok(PyBytes::new_bound(py, &bytes))
    }
}
```

**Step 4: rebuild & test**

```bash
maturin develop
pytest tests/test_render_html.py -v
```

Expected: 全 PASS. PDF bytes が返る。

**Step 5: Commit**

```bash
git add crates/pyfulgur/src/engine.rs crates/pyfulgur/tests/test_render_html.py
git commit -m "feat(pyfulgur): add Engine.render_html with GIL release"
```

---

## Task 8: Engine.render_html_to_file() (TDD)

**Files:**

- Modify: `crates/pyfulgur/src/engine.rs`
- Create: `crates/pyfulgur/tests/test_render_html_to_file.py`

**Step 1: failing test を書く**

`crates/pyfulgur/tests/test_render_html_to_file.py`:

```python
from pathlib import Path

import pytest

from pyfulgur import Engine


def test_render_html_to_file(tmp_path: Path):
    out = tmp_path / "out.pdf"
    engine = Engine.builder().build()
    engine.render_html_to_file("<h1>Hi</h1>", str(out))
    assert out.exists()
    data = out.read_bytes()
    assert data.startswith(b"%PDF")


def test_render_html_to_file_accepts_path(tmp_path: Path):
    out = tmp_path / "out.pdf"
    engine = Engine.builder().build()
    engine.render_html_to_file("<h1>Hi</h1>", out)
    assert out.exists()
```

**Step 2: テスト失敗確認**

**Step 3: 実装**

`crates/pyfulgur/src/engine.rs` の `impl PyEngine` に追加:

```rust
use std::path::PathBuf;

fn render_html_to_file(
    &self,
    py: Python<'_>,
    html: String,
    path: PathBuf,
) -> PyResult<()> {
    py.allow_threads(|| self.inner.render_html_to_file(&html, &path))
        .map_err(crate::error::map_fulgur_error)
}
```

`PathBuf` を使うことで PyO3 は自動的に `str` / `os.PathLike` から変換する。

**Step 4: rebuild & test**

```bash
maturin develop
pytest tests/test_render_html_to_file.py -v
```

Expected: 全 PASS.

**Step 5: Commit**

```bash
git add crates/pyfulgur/src/engine.rs crates/pyfulgur/tests/test_render_html_to_file.py
git commit -m "feat(pyfulgur): add Engine.render_html_to_file"
```

---

## Task 9: エラー変換 (RenderError / FileNotFoundError / ValueError)

**Files:**

- Create: `crates/pyfulgur/src/error.rs`
- Modify: `crates/pyfulgur/src/lib.rs`
- Create: `crates/pyfulgur/tests/test_errors.py`
- Modify: Task 5/7/8 で `error::map_fulgur_error` に統一

**Step 1: failing test を書く**

`crates/pyfulgur/tests/test_errors.py`:

```python
import pytest

import pyfulgur
from pyfulgur import AssetBundle, Engine


def test_render_error_is_exception_class():
    assert isinstance(pyfulgur.RenderError, type)
    assert issubclass(pyfulgur.RenderError, Exception)


def test_missing_css_file_raises_file_not_found(tmp_path):
    bundle = AssetBundle()
    with pytest.raises(FileNotFoundError):
        bundle.add_css_file(str(tmp_path / "nope.css"))


def test_invalid_html_returns_pdf_or_render_error():
    # Blitz is permissive; typical malformed HTML produces a valid PDF.
    # This test asserts the call does not raise for empty HTML.
    engine = Engine.builder().build()
    pdf = engine.render_html("")
    assert pdf.startswith(b"%PDF")


def test_font_file_missing_raises_file_not_found(tmp_path):
    bundle = AssetBundle()
    with pytest.raises(FileNotFoundError):
        bundle.add_font_file(str(tmp_path / "nope.ttf"))


def test_invalid_page_size_string_raises_value_error():
    with pytest.raises(ValueError):
        Engine.builder().page_size("XX")
```

**Step 2: 失敗確認**

`pyfulgur.RenderError` が存在しない → AttributeError.

**Step 3: 実装**

`crates/pyfulgur/src/error.rs`:

```rust
use fulgur::Error as FulgurError;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyFileNotFoundError, PyValueError};
use pyo3::prelude::*;

create_exception!(pyfulgur, RenderError, PyException, "Rendering failed");

pub fn map_fulgur_error(err: FulgurError) -> PyErr {
    match err {
        FulgurError::Io(io_err) => match io_err.kind() {
            std::io::ErrorKind::NotFound => {
                PyFileNotFoundError::new_err(io_err.to_string())
            }
            _ => RenderError::new_err(io_err.to_string()),
        },
        FulgurError::Asset(msg) => PyValueError::new_err(msg),
        FulgurError::UnsupportedFontFormat(msg) => PyValueError::new_err(msg),
        FulgurError::WoffDecode(msg) => RenderError::new_err(msg),
        FulgurError::HtmlParse(msg)
        | FulgurError::Layout(msg)
        | FulgurError::PdfGeneration(msg)
        | FulgurError::Template(msg) => RenderError::new_err(msg),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("RenderError", m.py().get_type_bound::<RenderError>())?;
    Ok(())
}
```

> **実装時注**: PyO3 0.22 の exception 登録 API は `get_type_bound` または `get_type` のどちらか。コンパイルエラーが出たら `cargo doc --open -p pyo3` か context7 で `pyo3::types::PyType::new` / `create_exception!` の正しい登録記法を確認すること。`create_exception!` のドキュメントには典型的な `m.add("MyError", py.get_type_bound::<MyError>())` 例が載っている。

**Step 4: lib.rs で register を呼ぶ**

`crates/pyfulgur/src/lib.rs` の `#[pymodule]` fn 内に:

```rust
error::register(m)?;
```

**Step 5: 既存 Task 5/7/8 の inline `map_fulgur_error` を削除し、`crate::error::map_fulgur_error` に統一**

Task 5 の `asset_bundle.rs` で inline 定義していた `map_fulgur_error` を削除し、`use crate::error::map_fulgur_error;` に置き換え。

**Step 6: rebuild & test**

```bash
maturin develop
pytest tests/test_errors.py -v
pytest tests/ -v  # 全体も通ることを確認
```

Expected: 全 PASS.

**Step 7: Commit**

```bash
git add crates/pyfulgur/src/error.rs crates/pyfulgur/src/lib.rs crates/pyfulgur/src/asset_bundle.rs crates/pyfulgur/tests/test_errors.py
git commit -m "feat(pyfulgur): add RenderError + map fulgur errors to Python exceptions"
```

---

## Task 10: Engine(page_size="A4", ...) kwargs コンストラクタ (TDD)

**Files:**

- Modify: `crates/pyfulgur/src/engine.rs`
- Create: `crates/pyfulgur/tests/test_engine_ctor.py`

**Step 1: failing test を書く**

`crates/pyfulgur/tests/test_engine_ctor.py`:

```python
import pytest

from pyfulgur import AssetBundle, Engine, Margin, PageSize


def test_engine_no_args():
    engine = Engine()
    pdf = engine.render_html("<h1>Hi</h1>")
    assert pdf.startswith(b"%PDF")


def test_engine_page_size_string():
    engine = Engine(page_size="A4")
    pdf = engine.render_html("<h1>A4</h1>")
    assert pdf.startswith(b"%PDF")


def test_engine_page_size_obj():
    engine = Engine(page_size=PageSize.LETTER)
    pdf = engine.render_html("<h1>Letter</h1>")
    assert pdf.startswith(b"%PDF")


def test_engine_all_kwargs():
    bundle = AssetBundle()
    bundle.add_css("body { font-family: sans-serif; }")
    engine = Engine(
        page_size=PageSize.A4,
        margin=Margin.uniform(36.0),
        landscape=False,
        title="Doc",
        author="Alice",
        lang="en",
        bookmarks=True,
        assets=bundle,
    )
    pdf = engine.render_html("<h1>Full</h1>")
    assert pdf.startswith(b"%PDF")


def test_engine_invalid_page_size_string_raises_value_error():
    with pytest.raises(ValueError):
        Engine(page_size="XX")
```

**Step 2: テスト失敗を確認**

**Step 3: 実装**

`PyEngine` に `#[new]` を追加。実装は「builder をチェーンで組み立てる」ヘルパ関数を作ってから呼び出す:

```rust
#[pymethods]
impl PyEngine {
    #[new]
    #[pyo3(signature = (
        *,
        page_size = None,
        margin = None,
        landscape = None,
        title = None,
        author = None,
        lang = None,
        bookmarks = None,
        assets = None,
    ))]
    fn new(
        page_size: Option<&Bound<'_, PyAny>>,
        margin: Option<PyMargin>,
        landscape: Option<bool>,
        title: Option<String>,
        author: Option<String>,
        lang: Option<String>,
        bookmarks: Option<bool>,
        assets: Option<&Bound<'_, PyAssetBundle>>,
    ) -> PyResult<Self> {
        let mut b = Engine::builder();
        if let Some(v) = page_size {
            let size = if let Ok(ps) = v.extract::<PyPageSize>() {
                ps.inner
            } else if let Ok(s) = v.extract::<String>() {
                parse_page_size_str(&s)?
            } else {
                return Err(PyValueError::new_err("page_size must be PageSize or str"));
            };
            b = b.page_size(size);
        }
        if let Some(m) = margin {
            b = b.margin(m.inner);
        }
        if let Some(v) = landscape {
            b = b.landscape(v);
        }
        if let Some(t) = title {
            b = b.title(t);
        }
        if let Some(a) = author {
            b = b.author(a);
        }
        if let Some(l) = lang {
            b = b.lang(l);
        }
        if let Some(v) = bookmarks {
            b = b.bookmarks(v);
        }
        if let Some(bundle) = assets {
            b = b.assets(bundle.borrow_mut().take_inner());
        }
        Ok(Self { inner: b.build() })
    }

    // ... 既存の builder(), render_html(), render_html_to_file() ...
}
```

**Step 4: rebuild & test**

```bash
maturin develop
pytest tests/test_engine_ctor.py -v
pytest tests/ -v
```

Expected: 全 PASS.

**Step 5: Commit**

```bash
git add crates/pyfulgur/src/engine.rs crates/pyfulgur/tests/test_engine_ctor.py
git commit -m "feat(pyfulgur): add Engine(**kwargs) Pythonic constructor"
```

---

## Task 11: 統合シナリオテスト (acceptance criteria を束ねる)

**Files:**

- Create: `crates/pyfulgur/tests/test_integration.py`

**Step 1: acceptance criteria を満たすテストを書く**

`crates/pyfulgur/tests/test_integration.py`:

```python
from pathlib import Path

from pyfulgur import AssetBundle, Engine, Margin, PageSize


def test_full_workflow_kwargs(tmp_path: Path):
    bundle = AssetBundle()
    bundle.add_css("h1 { color: red; font-size: 24pt; }")
    engine = Engine(
        page_size="A4",
        margin=Margin.uniform(36.0),
        title="Test Doc",
        assets=bundle,
    )
    pdf = engine.render_html("<h1>Integration</h1><p>Body text.</p>")
    assert pdf.startswith(b"%PDF")
    assert len(pdf) > 100


def test_full_workflow_builder(tmp_path: Path):
    bundle = AssetBundle()
    bundle.add_css("body { font-family: sans-serif; }")
    engine = (
        Engine.builder()
        .page_size(PageSize.A4)
        .margin(Margin.uniform_mm(20.0))
        .landscape(False)
        .title("Builder Test")
        .assets(bundle)
        .build()
    )
    out = tmp_path / "builder.pdf"
    engine.render_html_to_file("<h1>Builder</h1>", str(out))
    assert out.exists()
    assert out.read_bytes().startswith(b"%PDF")


def test_module_version():
    import pyfulgur
    assert pyfulgur.__version__ == "0.0.2"
```

**Step 2: `__version__` 属性を lib.rs に追加**

`crates/pyfulgur/src/lib.rs` の `#[pymodule]` fn 内に:

```rust
m.add("__version__", env!("CARGO_PKG_VERSION"))?;
```

**Step 3: rebuild & test**

```bash
maturin develop
pytest tests/ -v
```

Expected: 全 PASS.

**Step 4: Commit**

```bash
git add crates/pyfulgur/src/lib.rs crates/pyfulgur/tests/test_integration.py
git commit -m "feat(pyfulgur): add __version__ and integration tests"
```

---

## Task 12: README & CHANGELOG 更新

**Files:**

- Modify: `crates/pyfulgur/README.md`
- Modify: `CHANGELOG.md`

**Step 1: README.md を本実装向けに書き換え**

`crates/pyfulgur/README.md`:

```markdown
# pyfulgur

Python bindings for [fulgur](https://github.com/mitsuru/fulgur) — an offline, deterministic HTML/CSS to PDF conversion library written in Rust.

## Status

**Alpha (v0.0.2).** Core `Engine` / `AssetBundle` / `PageSize` / `Margin` / `render_html` API is available. Batch rendering, sandboxing, and template engine wiring are planned for later releases.

## Install

```bash
pip install pyfulgur
```

Pre-built wheels are published for manylinux (x86_64, aarch64), macOS (arm64, x86_64), and Windows (x86_64).

## Quick start

```python
from pyfulgur import AssetBundle, Engine, PageSize

bundle = AssetBundle()
bundle.add_css("body { font-family: sans-serif; }")

engine = Engine(page_size=PageSize.A4, assets=bundle)
pdf_bytes = engine.render_html("<h1>Hello, world!</h1>")

with open("output.pdf", "wb") as f:
    f.write(pdf_bytes)
```

Builder style:

```python
engine = (
    Engine.builder()
    .page_size(PageSize.A4)
    .landscape(False)
    .title("My doc")
    .assets(bundle)
    .build()
)
engine.render_html_to_file("<h1>Hi</h1>", "out.pdf")
```

## API surface

- `Engine(**kwargs)` / `Engine.builder()` → `EngineBuilder`
- `AssetBundle`: `add_css`, `add_css_file`, `add_font_file`, `add_image`, `add_image_file`
- `PageSize`: `A4`, `LETTER`, `A3`, `custom(w_mm, h_mm)`, `.landscape()`
- `Margin`: `Margin(top, right, bottom, left)`, `Margin.uniform(pt)`, `Margin.symmetric(v, h)`, `Margin.uniform_mm(mm)`
- Exceptions: `FileNotFoundError`, `ValueError`, `pyfulgur.RenderError`

## Links

- [fulgur on GitHub](https://github.com/mitsuru/fulgur)
- [fulgur on crates.io](https://crates.io/crates/fulgur)

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
```

**Step 2: CHANGELOG.md に entry を追加**

まず `head -20 CHANGELOG.md` で `## [Unreleased]` セクションが存在するか確認。なければ同 commit で新設する (v0.4.5 ヘッダの直前に `## [Unreleased]` を追加)。その上で `## [Unreleased]` 直下に以下を挿入:

```markdown
### Added

- **pyfulgur**: Python bindings for fulgur via PyO3 + maturin. Provides `Engine`, `EngineBuilder`, `AssetBundle`, `PageSize`, `Margin`, and `RenderError`. GIL is released during `render_html` / `render_html_to_file`. Ships as a manylinux/macOS/Windows wheel (v0.0.2).
```

**Step 3: markdownlint を通す**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur
npx markdownlint-cli2 'crates/pyfulgur/**/*.md' 'CHANGELOG.md'
```

Expected: エラーなし。違反があれば修正。

**Step 4: Commit**

```bash
git add crates/pyfulgur/README.md CHANGELOG.md
git commit -m "docs(pyfulgur): update README for MVP release and CHANGELOG"
```

---

## Task 13: 最終検証 (acceptance criteria 確認)

**Files:** なし (検証のみ)

**Step 1: 全テスト実行**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur/crates/pyfulgur
pytest tests/ -v
```

Expected: 全 PASS.

**Step 2: Rust workspace ビルド & テスト確認 (feature off で他 crate を壊していないこと)**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur
cargo check --workspace              # feature off: pyfulgur 空 crate、libpython 不要
cargo test --workspace               # workspace テスト全部、pyfulgur の test module も空 crate なので問題なし
cargo test -p fulgur --lib           # fulgur 単体 (~340 tests) 回帰確認
cargo test -p pyfulgur --features extension-module  # pyfulgur 側の Send+Sync assertion を含む Rust unit tests
```

Expected: 全 OK. fulgur 側の既存 340 テストが引き続き通ること、pyfulgur の extension-module on での compile & Rust test も通ること。

**Step 3: clippy & fmt**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur
cargo fmt --check
cargo clippy --workspace -- -D warnings                         # feature off 全 crate
cargo clippy -p pyfulgur --features extension-module -- -D warnings  # feature on pyfulgur
```

Expected: warning / error なし。

**Step 4: maturin build で wheel が作れるか確認**

```bash
cd /home/ubuntu/fulgur/.worktrees/pyfulgur/crates/pyfulgur
maturin build --release
ls target/wheels/
# layout 確認: .so と dist-info のみが入っていること、余計な placeholder ファイルが残っていないこと
unzip -l target/wheels/pyfulgur-0.0.2-*.whl | head -30
```

Expected: `pyfulgur-0.0.2-*.whl` が生成される。wheel 内には `pyfulgur*.so` と `pyfulgur-0.0.2.dist-info/` が存在し、古い setuptools layout の `pyfulgur/__init__.py` は含まれていない。

**Step 5: acceptance criteria チェックリスト**

- [ ] Engine(page_size="A4") でPDF生成できる
- [ ] Engine.builder().page_size(PageSize.A4).build() でPDF生成できる
- [ ] AssetBundle で CSS/フォント/画像をバンドルできる
- [ ] render_html が bytes (PDF) を返す
- [ ] render_html_to_file がファイル出力する
- [ ] 不正な page_size で ValueError が発生する
- [ ] 存在しないファイルで FileNotFoundError が発生する
- [ ] maturin develop でビルドできる
- [ ] pytest テストが全パスする
- [ ] `cargo test --workspace` (feature off) が通り、fulgur 既存 340 テストに回帰がない
- [ ] `cargo test -p pyfulgur --features extension-module` で Send+Sync assertion が通る

全てチェックが入ったら、beads issue を close する (fulgur-i5c)。

---

## Out of Scope (将来タスク)

- PyPI 公開ワークフロー (CI で build & publish): `fulgur-qyf`
- バッチ API / プロセス分離: 後続 issue
- Ruby binding: `fulgur-0x0`
- Template (MiniJinja) + data render() API: MVP には含めない
- Metadata field の網羅 (description/keywords/creator 等): 必要になったら追加
- Type stubs (`pyfulgur.pyi`): ユーザーニーズ待ち
