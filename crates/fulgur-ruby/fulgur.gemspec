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

  # `gem install fulgur` 時に `ext/fulgur/extconf.rb` が `require "rb_sys/mkmf"` するため、
  # 利用者側でも install 段階で rb_sys が必要。ランタイム直接依存ではないが、
  # RubyGems 的には `add_dependency` で配布する必要がある（`add_development_dependency`
  # だと bundler の development グループでしか入らず、`gem install` の build が落ちる）。
  spec.add_dependency "rb_sys", "~> 0.9"
end
