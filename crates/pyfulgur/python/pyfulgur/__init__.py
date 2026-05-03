"""pyfulgur — Python bindings for fulgur (HTML/CSS to PDF).

Offline, deterministic HTML/CSS to PDF conversion engine.
"""

from .pyfulgur import (
    AssetBundle,
    Engine,
    EngineBuilder,
    Margin,
    PageSize,
    RenderError,
    __version__,
)

__all__ = [
    "AssetBundle",
    "Engine",
    "EngineBuilder",
    "Margin",
    "PageSize",
    "RenderError",
    "__version__",
]
