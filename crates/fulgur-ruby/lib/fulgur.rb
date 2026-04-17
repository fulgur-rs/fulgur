# frozen_string_literal: true

require_relative "fulgur/version"

begin
  minor = RUBY_VERSION[/\A\d+\.\d+/]
  require_relative "fulgur/#{minor}/fulgur"
rescue LoadError
  require_relative "fulgur/fulgur"
end

module Fulgur
  class Error < StandardError; end
  class RenderError < Error; end
  class AssetError < Error; end
end

require_relative "fulgur/margin"
require_relative "fulgur/asset_bundle"
