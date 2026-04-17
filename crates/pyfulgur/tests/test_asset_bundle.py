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
