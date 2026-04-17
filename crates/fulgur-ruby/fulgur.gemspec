# frozen_string_literal: true

require_relative "lib/fulgur/version"

Gem::Specification.new do |spec|
  spec.name = "fulgur"
  spec.version = Fulgur::VERSION
  spec.authors = ["Mitsuru Hayasaka"]
  spec.email = ["hayasaka.mitsuru@gmail.com"]

  spec.summary = "Offline HTML/CSS → PDF conversion"
  spec.description = "Ruby bindings for fulgur, a deterministic HTML/CSS to PDF rendering engine."
  spec.homepage = "https://github.com/mitsuru/fulgur"
  spec.licenses = ["Apache-2.0", "MIT"]
  spec.required_ruby_version = ">= 3.3.0"

  spec.metadata["allowed_push_host"] = "https://rubygems.org"
  spec.metadata["source_code_uri"] = "https://github.com/mitsuru/fulgur"

  spec.files = Dir[
    "lib/**/*.rb",
    "ext/**/*.{rs,toml,rb}",
    "src/**/*.rs",
    "Cargo.toml",
    "README.md",
    "CHANGELOG.md",
    "LICENSE-*",
  ]
  spec.require_paths = ["lib"]
  spec.extensions = ["ext/fulgur/extconf.rb"]

  # rb_sys は extconf.rb がビルド時にのみ必要 (ext/fulgur/extconf.rb 経由)。
  # インストール済みの拡張はランタイムで rb_sys に依存しないため、開発依存に限定する。
  spec.add_development_dependency "rb_sys", "~> 0.9"
end
