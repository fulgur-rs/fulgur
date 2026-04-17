# Changelog

All notable changes to the `fulgur` gem will be documented here.

## [Unreleased]

## [0.0.1] - 2026-04-17

Initial Ruby binding for fulgur.

### Added

- `Fulgur::Engine` (kwargs constructor + builder chain)
- `Fulgur::EngineBuilder` for reusable engine construction
- `Fulgur::AssetBundle` with long (`add_*`) and short (`css`, `font_file`, etc.) aliases
- `Fulgur::PageSize` with `A4` / `LETTER` / `A3` constants and `.custom(w_mm, h_mm)`; accepts `Symbol`, `String`, or class constants as input
- `Fulgur::Margin` with CSS-style positional args, keyword args, and `.uniform` / `.symmetric` factories
- `Fulgur::Pdf` result object: `#to_s` (ASCII-8BIT), `#to_base64`, `#to_data_uri`, `#write_to_path`, `#write_to_io` (64 KiB chunked, binmode-guaranteed), `#bytesize`
- `Engine#render_html` and `Engine#render_html_to_file` release the GVL during the Rust render call
- Error hierarchy: `Fulgur::Error` / `Fulgur::RenderError` / `Fulgur::AssetError`, plus standard `ArgumentError` / `Errno::ENOENT`
- Ruby 3.3+ support

### Known Limitations

- Precompiled gems / RubyGems publish automation are tracked separately (fulgur-qyf) and not yet in place; gems must be built from source for now
- Streaming renderer: Krilla emits bytes at the end of rendering, so `#write_to_io` chunks a completed buffer rather than streaming during layout
- No Ractor safety analysis yet
