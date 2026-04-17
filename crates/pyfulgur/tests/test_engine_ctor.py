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
    with pytest.raises(ValueError, match="unknown page size"):
        Engine(page_size="XX")
