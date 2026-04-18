# fulgur

Ruby bindings for [fulgur](https://github.com/fulgur-rs/fulgur) — an offline,
deterministic HTML/CSS to PDF conversion library written in Rust.

## Status

**MVP (gem v0.0.1, unreleased).** The core `Engine` / `EngineBuilder` /
`AssetBundle` / `PageSize` / `Margin` / `Pdf` API is available. Precompiled
gems, batch rendering, sandboxing, and template-engine wiring are planned
for later releases.

> **Versioning note:** the `fulgur` gem is versioned independently from the
> underlying `fulgur` Rust crate and the `pyfulgur` PyPI package. This gem
> starts at `0.0.1` as an MVP and will bump on its own cadence. "v0.5.0"
> elsewhere in project documents refers to the parent epic that bundles
> the Rust / Python / Ruby shipping milestones, not this gem's version.

## Install

> **Note:** v0.0.1 is an early MVP. Pre-built gems are not yet published to
> RubyGems; build from source for now.

Requires a Rust toolchain (1.85+) and Ruby 3.3+.

```bash
# From a checkout of the fulgur repository
cd crates/fulgur-ruby
bundle install
bundle exec rake compile
```

Once pre-built gems ship, installation will be:

```bash
gem install fulgur
```

For Bundler:

```ruby
gem "fulgur"
```

## Quick start

```ruby
require "fulgur"

bundle = Fulgur::AssetBundle.new
bundle.add_css("body { font-family: sans-serif; }")

engine = Fulgur::Engine.new(page_size: :a4, assets: bundle)
pdf = engine.render_html("<h1>Hello, world!</h1>")

pdf.write_to_path("output.pdf")
```

Builder style:

```ruby
engine = Fulgur::Engine.builder
  .page_size(Fulgur::PageSize::A4)
  .landscape(false)
  .title("My doc")
  .assets(bundle)
  .build

engine.render_html_to_file("<h1>Hi</h1>", "out.pdf")
```

## API surface

### `Fulgur::Engine`

Keyword-argument constructor:

```ruby
engine = Fulgur::Engine.new(
  page_size: :a4,                # Symbol, String, or Fulgur::PageSize
  margin: Fulgur::Margin.uniform(72),
  landscape: false,
  title: "My Document",
  author: "Me",
  lang: "en",
  bookmarks: true,
  assets: bundle,                # Fulgur::AssetBundle
)
```

Or use the builder:

```ruby
engine = Fulgur::Engine.builder
  .page_size(:letter)
  .margin(Fulgur::Margin.new(72, 36, 48, 24))
  .assets(bundle)
  .build
```

### Rendering

```ruby
pdf = engine.render_html(html_string)        # => Fulgur::Pdf
engine.render_html_to_file(html, "out.pdf")  # shortcut
```

`render_html` and `render_html_to_file` release the GVL, allowing other
Ruby threads to run concurrently during rendering.

### `Fulgur::Pdf` (render result)

```ruby
pdf.bytesize                   # => Integer
pdf.to_s                       # => String (ASCII-8BIT binary)
pdf.to_base64                  # => String (Base64)
pdf.to_data_uri                # => "data:application/pdf;base64,..."
pdf.write_to_path("out.pdf")   # write raw bytes to file path
pdf.write_to_io(io)            # chunked write to any IO (calls binmode when supported)
```

The result object keeps bytes on the Rust side. Methods like `to_base64`
encode directly from the Rust buffer, avoiding an intermediate Ruby binary
String. For server-side batch workloads rendering many PDFs, this halves
peak memory compared with `Base64.strict_encode64(bytes)` on the Ruby
side.

### `Fulgur::AssetBundle`

Offline-first: all assets must be explicitly registered.

```ruby
bundle = Fulgur::AssetBundle.new
bundle.add_css("body { font-family: 'Noto Sans' }")
bundle.add_css_file("style.css")
bundle.add_font_file("NotoSans-Regular.ttf")
bundle.add_image("logo", File.binread("logo.png"))
bundle.add_image_file("icon", "icon.png")
```

Short aliases are also available:

```ruby
bundle.css "..."
bundle.css_file "style.css"
bundle.font_file "NotoSans.ttf"
bundle.image "logo", bytes
bundle.image_file "icon", "icon.png"
```

### `Fulgur::PageSize`

```ruby
Fulgur::PageSize::A4
Fulgur::PageSize::LETTER
Fulgur::PageSize::A3
Fulgur::PageSize.custom(100, 200)  # width/height in mm
```

Engine kwargs and builder also accept `:a4`, `"A4"`, etc. as shorthand.

### `Fulgur::Margin`

```ruby
Fulgur::Margin.new(72)                         # uniform
Fulgur::Margin.new(72, 36)                     # [vertical, horizontal]
Fulgur::Margin.new(72, 36, 48, 24)             # [top, right, bottom, left]
Fulgur::Margin.new(top: 72, right: 36, bottom: 48, left: 24)
Fulgur::Margin.uniform(72)
Fulgur::Margin.symmetric(72, 36)
```

All values are in points (pt).

## LLM integration

`Fulgur::Pdf#to_base64` and `#to_data_uri` are optimized for passing PDFs
to LLMs as base64-encoded payloads (e.g., Anthropic Claude, OpenAI GPT-4):

```ruby
pdf = engine.render_html(html)

anthropic.messages.create(
  model: "claude-opus-4-7",
  messages: [{
    role: "user",
    content: [
      {
        type: "document",
        source: {
          type: "base64",
          media_type: "application/pdf",
          data: pdf.to_base64,
        },
      },
      { type: "text", text: "Summarize this document." },
    ],
  }],
)
```

## Errors

```text
Fulgur::Error        # base class (StandardError)
Fulgur::RenderError  # rendering failure (HTML parse, layout, PDF generation, WOFF decode)
Fulgur::AssetError   # asset registration failure (unsupported font format, invalid asset)

ArgumentError        # invalid arguments (unknown page_size, malformed margin)
Errno::ENOENT        # missing font/image/CSS file
```

## Development

```bash
cd crates/fulgur-ruby
bundle install
bundle exec rake compile   # builds the Rust extension
bundle exec rspec          # runs tests
bundle exec rake           # compile + test
```

The native extension lives under `ext/fulgur/`, the Ruby-side wrappers
under `lib/`, and Rust sources under `src/`.

## Links

- [fulgur on GitHub](https://github.com/fulgur-rs/fulgur)
- [fulgur on crates.io](https://crates.io/crates/fulgur)

## License

Licensed under either of Apache License, Version 2.0 or MIT license at
your option.
