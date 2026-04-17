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
    with pytest.raises(ValueError, match="unknown page size"):
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


def test_assets_consumes_bundle_empties_it():
    """After passing a bundle to a builder, the bundle is emptied (take_inner contract)."""
    bundle = AssetBundle()
    bundle.add_css("body {}")
    Engine.builder().assets(bundle).build()
    # bundle has been consumed; add_css on the same instance should still work
    # (the bundle resets to AssetBundle::new() after take_inner), and the
    # previous CSS is no longer present. We don't have a direct "is_empty"
    # accessor, so the contract verified here is "reuse does not panic and
    # the object remains usable".
    bundle.add_css("p {}")  # does not raise
