from pathlib import Path

from pyfulgur import AssetBundle, Engine, Margin, PageSize


def test_full_workflow_kwargs():
    bundle = AssetBundle()
    bundle.add_css("h1 { color: red; font-size: 24pt; }")
    engine = Engine(
        page_size="A4",
        margin=Margin.uniform(36.0),
        title="Test Doc",
        assets=bundle,
    )
    pdf = engine.render_html("<h1>Integration</h1><p>Body text.</p>")
    assert pdf.startswith(b"%PDF")
    assert len(pdf) > 100


def test_full_workflow_builder(tmp_path: Path):
    bundle = AssetBundle()
    bundle.add_css("body { font-family: sans-serif; }")
    engine = (
        Engine.builder()
        .page_size(PageSize.A4)
        .margin(Margin.uniform_mm(20.0))
        .landscape(False)
        .title("Builder Test")
        .assets(bundle)
        .build()
    )
    out = tmp_path / "builder.pdf"
    engine.render_html_to_file("<h1>Builder</h1>", str(out))
    assert out.exists()
    assert out.read_bytes().startswith(b"%PDF")


def test_module_version():
    import pyfulgur
    from importlib.metadata import version

    # Rust 側の __version__ (CARGO_PKG_VERSION) と Python 側の pyproject.toml
    # version が同期していることを確認する。release-prepare.yml の sed で片側
    # だけ書き換え漏れた場合をここで fail させる (過去に 0.0.2 hardcode で
    # 0.5.0 bump 時にすり抜けた Devin Review 指摘を契機に追加)。
    assert pyfulgur.__version__ == version("pyfulgur")
