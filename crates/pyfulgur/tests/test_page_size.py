import math

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
    assert math.isclose(size.height, 1190.55, abs_tol=0.01)


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
