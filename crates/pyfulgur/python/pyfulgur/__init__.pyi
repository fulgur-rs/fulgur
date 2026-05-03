"""Type stubs and rendered documentation for pyfulgur.

Docstrings here mirror the Rust ``///`` comments on each PyO3 binding so that:

- mkdocstrings / griffe in static mode can render the API reference without
  ``force_inspection`` (which would drop type annotations from signatures).
- IDEs (pyright, pylance) show the same docstring in hover that ``help()``
  shows at runtime.

The Rust ``///`` comments remain the canonical source for runtime ``__doc__``;
when changing one, change the other.
"""

import os
from typing import List, Optional, Union

__version__: str
__all__: List[str]


class RenderError(Exception):
    """Raised when fulgur fails to render an HTML document to PDF.

    Wraps parse, layout, font, and PDF-generation errors from the underlying
    fulgur engine.
    """


class PageSize:
    """Page size with dimensions in millimeters.

    Use the predefined class attributes ``A4``, ``LETTER``, or ``A3``, or
    `custom` for arbitrary sizes. ``PageSize`` is immutable.

    Example:
        ```python
        from pyfulgur import PageSize
        a4 = PageSize.A4
        custom = PageSize.custom(210.0, 297.0)
        landscape = a4.landscape()
        ```
    """

    A4: PageSize
    LETTER: PageSize
    A3: PageSize

    @staticmethod
    def custom(width_mm: float, height_mm: float) -> PageSize:
        """Create a page size with arbitrary dimensions.

        Args:
            width_mm: Page width in millimeters.
            height_mm: Page height in millimeters.

        Returns:
            A new ``PageSize`` instance.
        """

    def landscape(self) -> PageSize:
        """Return a new ``PageSize`` with width and height swapped.

        Returns:
            A new ``PageSize`` rotated 90 degrees from this one.
        """

    @property
    def width(self) -> float:
        """Page width in millimeters."""

    @property
    def height(self) -> float:
        """Page height in millimeters."""

    def __repr__(self) -> str: ...


class Margin:
    """Page margin in points (PDF pt).

    Margins apply uniformly to every generated page. Use the constructor for
    explicit per-side values, or one of the helper factories
    (`uniform`, `symmetric`, `uniform_mm`).

    Example:
        ```python
        from pyfulgur import Margin
        Margin(36.0, 36.0, 36.0, 36.0)
        Margin.uniform_mm(20.0)
        ```
    """

    def __init__(
        self,
        top: float,
        right: float,
        bottom: float,
        left: float,
    ) -> None:
        """
        Args:
            top: Top margin in points.
            right: Right margin in points.
            bottom: Bottom margin in points.
            left: Left margin in points.
        """

    @staticmethod
    def uniform(pt: float) -> Margin:
        """Create a uniform margin (the same value on all four sides).

        Args:
            pt: Margin value in points (PDF pt).

        Returns:
            A new ``Margin`` with all sides set to ``pt``.
        """

    @staticmethod
    def symmetric(vertical: float, horizontal: float) -> Margin:
        """Create a symmetric margin (vertical and horizontal pairs).

        Args:
            vertical: Top and bottom margin in points.
            horizontal: Left and right margin in points.

        Returns:
            A new ``Margin`` with the given symmetric values.
        """

    @staticmethod
    def uniform_mm(mm: float) -> Margin:
        """Create a uniform margin specified in millimeters.

        Args:
            mm: Margin value in millimeters.

        Returns:
            A new ``Margin`` with all sides set to the equivalent point value.
        """

    @property
    def top(self) -> float:
        """Top margin in points."""

    @property
    def right(self) -> float:
        """Right margin in points."""

    @property
    def bottom(self) -> float:
        """Bottom margin in points."""

    @property
    def left(self) -> float:
        """Left margin in points."""

    def __repr__(self) -> str: ...


class AssetBundle:
    """Bundle of CSS, fonts, and images passed to `Engine`.

    fulgur is offline-first: every asset must be explicitly registered before
    rendering. The engine never performs network fetches.

    Example:
        ```python
        from pyfulgur import AssetBundle, Engine
        bundle = AssetBundle()
        bundle.add_css("body { font-family: sans-serif; }")
        engine = Engine(assets=bundle)
        ```
    """

    def __init__(self) -> None: ...

    def add_css(self, css: str) -> None:
        """Add an inline CSS string to the bundle.

        Args:
            css: CSS source text.
        """

    def add_css_file(self, path: Union[str, "os.PathLike[str]"]) -> None:
        """Add a CSS file from disk.

        Args:
            path: Filesystem path to the CSS file.

        Raises:
            FileNotFoundError: When ``path`` does not exist.
            ValueError: When the CSS file cannot be read or decoded.
        """

    def add_font_file(self, path: Union[str, "os.PathLike[str]"]) -> None:
        """Add a font file (TTF, OTF, WOFF, or WOFF2) from disk.

        Args:
            path: Filesystem path to the font file.

        Raises:
            FileNotFoundError: When ``path`` does not exist.
            ValueError: When the font format is unsupported.
        """

    def add_image(self, name: str, data: bytes) -> None:
        """Add an image with an explicit logical name and raw bytes.

        Args:
            name: Logical name to reference from CSS or HTML
                (e.g. ``"logo.png"``).
            data: Raw image bytes (PNG, JPEG, etc.).
        """

    def add_image_file(
        self, name: str, path: Union[str, "os.PathLike[str]"]
    ) -> None:
        """Add an image file from disk.

        Args:
            name: Logical name to reference from CSS or HTML.
            path: Filesystem path to the image file.

        Raises:
            FileNotFoundError: When ``path`` does not exist.
        """


class EngineBuilder:
    """Builder for configuring and constructing an `Engine`.

    All setters return ``self`` to support method chaining. ``build()`` can
    be called only once per builder; subsequent calls raise ``RuntimeError``.

    Example:
        ```python
        from pyfulgur import Engine, PageSize, Margin
        engine = (
            Engine.builder()
            .page_size(PageSize.A4)
            .margin(Margin.uniform_mm(20.0))
            .title("Doc")
            .build()
        )
        ```
    """

    def __init__(self) -> None: ...

    def page_size(self, value: Union[PageSize, str]) -> EngineBuilder:
        """Set the page size.

        Args:
            value: Either a `PageSize` instance or a string name
                (``"A4"``, ``"LETTER"``, ``"A3"``; case-insensitive).

        Returns:
            ``self`` for method chaining.

        Raises:
            ValueError: When ``value`` is an unknown size name or wrong type.
        """

    def margin(self, margin: Margin) -> EngineBuilder:
        """Set the page margins.

        Args:
            margin: A `Margin` instance.

        Returns:
            ``self`` for method chaining.
        """

    def landscape(self, value: bool) -> EngineBuilder:
        """Toggle landscape orientation.

        Args:
            value: ``True`` to render in landscape, ``False`` for portrait.

        Returns:
            ``self`` for method chaining.
        """

    def title(self, value: str) -> EngineBuilder:
        """Set the PDF document title metadata.

        Args:
            value: Title string written to the PDF metadata.

        Returns:
            ``self`` for method chaining.
        """

    def author(self, value: str) -> EngineBuilder:
        """Set the PDF document author metadata.

        Args:
            value: Author string written to the PDF metadata.

        Returns:
            ``self`` for method chaining.
        """

    def lang(self, value: str) -> EngineBuilder:
        """Set the document language tag.

        Args:
            value: BCP 47 language tag (e.g. ``"en"``, ``"ja"``).

        Returns:
            ``self`` for method chaining.
        """

    def bookmarks(self, value: bool) -> EngineBuilder:
        """Toggle PDF bookmark generation.

        Args:
            value: ``True`` to generate bookmarks from heading hierarchy.

        Returns:
            ``self`` for method chaining.
        """

    def assets(self, bundle: AssetBundle) -> EngineBuilder:
        """Attach an `AssetBundle` of CSS, fonts, and images.

        The bundle is consumed and reset to an empty state so it can be
        reused for a new build cycle.

        Args:
            bundle: The `AssetBundle` to attach.

        Returns:
            ``self`` for method chaining.
        """

    def build(self) -> Engine:
        """Finalize the configuration and build an `Engine`.

        Returns:
            A new `Engine` ready to render HTML.

        Raises:
            RuntimeError: When the builder has already been consumed.
        """


class Engine:
    """HTML/CSS to PDF conversion engine.

    fulgur converts HTML, CSS, and SVG into a PDF document deterministically
    and offline. All assets must be explicitly bundled via `AssetBundle`;
    no network fetches are performed.

    Example:
        ```python
        from pyfulgur import Engine, PageSize
        engine = Engine(page_size=PageSize.A4, title="Hello")
        engine.render_html_to_file("<h1>Hi</h1>", "out.pdf")
        ```
    """

    def __init__(
        self,
        *,
        page_size: Optional[Union[PageSize, str]] = ...,
        margin: Optional[Margin] = ...,
        landscape: Optional[bool] = ...,
        title: Optional[str] = ...,
        author: Optional[str] = ...,
        lang: Optional[str] = ...,
        bookmarks: Optional[bool] = ...,
        assets: Optional[AssetBundle] = ...,
    ) -> None:
        """
        Args:
            page_size: Page dimensions, either a `PageSize` or a
                case-insensitive string name (``"A4"``, ``"LETTER"``,
                ``"A3"``).
            margin: Page margins.
            landscape: ``True`` for landscape orientation.
            title: PDF title metadata.
            author: PDF author metadata.
            lang: BCP 47 language tag.
            bookmarks: ``True`` to generate bookmarks from heading hierarchy.
            assets: An `AssetBundle` of CSS, fonts, and images.

        Raises:
            ValueError: When ``page_size`` is an unknown name.
        """

    @staticmethod
    def builder() -> EngineBuilder:
        """Return a fresh `EngineBuilder` for fluent configuration.

        Returns:
            A new `EngineBuilder`.
        """

    def render_html(self, html: str) -> bytes:
        """Render HTML to PDF bytes.

        The Python GIL is released for the duration of the render so multiple
        threads can render in parallel.

        Args:
            html: HTML source string.

        Returns:
            PDF bytes (always start with ``b"%PDF"``).

        Raises:
            RenderError: When parsing, layout, or PDF generation fails.
        """

    def render_html_to_file(
        self, html: str, path: Union[str, "os.PathLike[str]"]
    ) -> None:
        """Render HTML and write the PDF to a file.

        The GIL is released for the duration of the render.

        Args:
            html: HTML source string.
            path: Filesystem path to write the PDF to.

        Raises:
            RenderError: When rendering or writing fails.
            FileNotFoundError: When the parent directory does not exist.
        """
