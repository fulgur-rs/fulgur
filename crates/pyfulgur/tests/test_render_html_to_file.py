from pathlib import Path

from pyfulgur import Engine


def test_render_html_to_file(tmp_path: Path):
    out = tmp_path / "out.pdf"
    engine = Engine.builder().build()
    engine.render_html_to_file("<h1>Hi</h1>", str(out))
    assert out.exists()
    data = out.read_bytes()
    assert data.startswith(b"%PDF")


def test_render_html_to_file_accepts_path(tmp_path: Path):
    out = tmp_path / "out.pdf"
    engine = Engine.builder().build()
    engine.render_html_to_file("<h1>Hi</h1>", out)
    assert out.exists()
    assert out.read_bytes().startswith(b"%PDF")
