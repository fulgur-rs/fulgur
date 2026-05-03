"""Rust /// doc comment が PyO3 経由で __doc__ に展開されることを検証。

mkdocstrings (griffe inspection) と help() / Jupyter ? の両方で docstring が
見えることが、website 側 mkdocstrings 連携 (fulgur-lxy0) の前提条件。
"""

import pyfulgur


def _has_doc(obj: object, *keywords: str) -> None:
    doc = obj.__doc__
    assert doc, f"{obj!r} has no docstring"
    lower = doc.lower()
    for kw in keywords:
        assert kw.lower() in lower, (
            f"{obj!r} docstring missing keyword {kw!r}: {doc!r}"
        )


def test_engine_class_has_doc():
    _has_doc(pyfulgur.Engine, "HTML", "PDF")


def test_engine_render_html_has_doc():
    _has_doc(pyfulgur.Engine.render_html, "Args", "Returns", "Raises")


def test_engine_render_html_to_file_has_doc():
    _has_doc(pyfulgur.Engine.render_html_to_file, "Args", "Raises")


def test_engine_builder_class_has_doc():
    _has_doc(pyfulgur.EngineBuilder, "builder")


def test_engine_builder_methods_have_docs():
    for method in (
        "page_size",
        "margin",
        "landscape",
        "title",
        "author",
        "lang",
        "bookmarks",
        "assets",
        "build",
    ):
        _has_doc(getattr(pyfulgur.EngineBuilder, method), "Returns")


def test_asset_bundle_class_has_doc():
    _has_doc(pyfulgur.AssetBundle, "CSS", "fonts", "images")


def test_asset_bundle_methods_have_docs():
    for method in (
        "add_css",
        "add_css_file",
        "add_font_file",
        "add_image",
        "add_image_file",
    ):
        _has_doc(getattr(pyfulgur.AssetBundle, method), "Args")


def test_page_size_class_has_doc():
    _has_doc(pyfulgur.PageSize, "page size")


def test_page_size_class_doc_lists_classattrs():
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
