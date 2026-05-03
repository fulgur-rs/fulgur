"""PEP 561 type marker と stub の wheel 同梱検証。"""

from importlib.resources import files


def test_py_typed_marker_exists():
    """py.typed marker が pyfulgur パッケージに同梱されていること。"""
    assert (files("pyfulgur") / "py.typed").is_file()


def test_pyi_stub_exists():
    """__init__.pyi が pyfulgur パッケージに同梱されていること。"""
    assert (files("pyfulgur") / "__init__.pyi").is_file()
