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


def test_font_file_missing_raises_file_not_found(tmp_path):
    bundle = AssetBundle()
    with pytest.raises(FileNotFoundError):
        bundle.add_font_file(str(tmp_path / "nope.ttf"))


def test_invalid_page_size_string_raises_value_error():
    with pytest.raises(ValueError, match="unknown page size"):
        Engine.builder().page_size("XX")


def test_invalid_html_returns_pdf_or_render_error():
    # Blitz is permissive; typical malformed HTML produces a valid PDF.
    # This test asserts the call does not raise for empty HTML.
    engine = Engine.builder().build()
    pdf = engine.render_html("")
    assert pdf.startswith(b"%PDF")
