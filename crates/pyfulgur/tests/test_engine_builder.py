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
