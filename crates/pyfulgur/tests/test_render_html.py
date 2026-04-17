from pyfulgur import AssetBundle, Engine, PageSize


def test_render_html_returns_pdf_bytes():
    engine = Engine.builder().page_size(PageSize.A4).build()
    pdf = engine.render_html("<h1>Hello</h1>")
    assert isinstance(pdf, bytes)
    assert pdf.startswith(b"%PDF")


def test_render_html_with_css_bundle():
    bundle = AssetBundle()
    bundle.add_css("h1 { color: blue; }")
    engine = Engine.builder().assets(bundle).build()
    pdf = engine.render_html("<h1>Styled</h1>")
    assert pdf.startswith(b"%PDF")


def test_render_html_multiple_times_succeeds():
    engine = Engine.builder().build()
    pdf1 = engine.render_html("<p>a</p>")
    pdf2 = engine.render_html("<p>b</p>")
    assert pdf1.startswith(b"%PDF")
    assert pdf2.startswith(b"%PDF")
