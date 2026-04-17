# frozen_string_literal: true

require_relative "fulgur/version"

begin
  RUBY_VERSION =~ /(\d+\.\d+)/
  require_relative "fulgur/#{Regexp.last_match(1)}/fulgur"
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
