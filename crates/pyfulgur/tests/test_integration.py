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
    assert pyfulgur.__version__ == "0.0.2"
