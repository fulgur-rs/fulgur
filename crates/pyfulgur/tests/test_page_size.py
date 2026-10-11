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


def test_a5_has_expected_dimensions():
    # fulgur-5oav: A5 must be distinct from A4, not a silent A4 fallback.
    size = PageSize.A5
    assert math.isclose(size.width, 148.0 * 72.0 / 25.4, abs_tol=0.01)
    assert math.isclose(size.height, 210.0 * 72.0 / 25.4, abs_tol=0.01)
    assert size.width != PageSize.A4.width


def test_jis_b4_has_expected_dimensions():
    size = PageSize.JIS_B4
    assert math.isclose(size.width, 257.0 * 72.0 / 25.4, abs_tol=0.01)
    assert math.isclose(size.height, 364.0 * 72.0 / 25.4, abs_tol=0.01)


def test_legal_and_ledger_have_expected_dimensions():
    legal = PageSize.LEGAL
    assert math.isclose(legal.width, 8.5 * 72.0, abs_tol=0.01)
    assert math.isclose(legal.height, 14.0 * 72.0, abs_tol=0.01)

    ledger = PageSize.LEDGER
    assert math.isclose(ledger.width, 11.0 * 72.0, abs_tol=0.01)
    assert math.isclose(ledger.height, 17.0 * 72.0, abs_tol=0.01)


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
