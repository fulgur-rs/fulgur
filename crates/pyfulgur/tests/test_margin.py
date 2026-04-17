import math

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
    assert math.isclose(m.right, 72.0, abs_tol=0.01)
    assert math.isclose(m.bottom, 72.0, abs_tol=0.01)
    assert math.isclose(m.left, 72.0, abs_tol=0.01)


def test_margin_repr():
    m = Margin.uniform(10.0)
    assert "Margin" in repr(m)
