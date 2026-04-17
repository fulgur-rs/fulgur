# frozen_string_literal: true

module Fulgur
  class AssetBundle
    alias_method :css, :add_css
    alias_method :css_file, :add_css_file
    alias_method :font_file, :add_font_file
    alias_method :image, :add_image
    alias_method :image_file, :add_image_file
  end
end
