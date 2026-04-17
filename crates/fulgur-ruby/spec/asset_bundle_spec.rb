# frozen_string_literal: true

require "spec_helper"

RSpec.describe Fulgur::AssetBundle do
  let(:bundle) { described_class.new }
  let(:fixtures) { File.expand_path("fixtures", __dir__) }

  describe "#add_css / #css" do
    it "accepts inline CSS via long name" do
      expect { bundle.add_css("body { color: red }") }.not_to raise_error
    end

    it "has #css alias pointing to add_css" do
      expect(bundle.method(:css)).to eq(bundle.method(:add_css))
    end
  end

  describe "#add_css_file / #css_file" do
    it "reads CSS file" do
      expect { bundle.add_css_file(File.join(fixtures, "style.css")) }.not_to raise_error
    end

    it "raises Errno::ENOENT for missing file" do
      expect { bundle.add_css_file("/nonexistent.css") }.to raise_error(Errno::ENOENT)
    end

    it "has #css_file alias" do
      expect(bundle.method(:css_file)).to eq(bundle.method(:add_css_file))
    end
  end

  describe "#add_font_file / #font_file" do
    it "reads a .ttf font" do
      expect { bundle.add_font_file(File.join(fixtures, "noto_sans.ttf")) }.not_to raise_error
    end

    it "raises Errno::ENOENT for missing font" do
      expect { bundle.add_font_file("/nope.ttf") }.to raise_error(Errno::ENOENT)
    end

    it "has #font_file alias" do
      expect(bundle.method(:font_file)).to eq(bundle.method(:add_font_file))
    end
  end

  describe "#add_image / #image" do
    it "registers image bytes with name" do
      expect { bundle.add_image("logo", "\x89PNG\r\n\x1a\n".b) }.not_to raise_error
    end

    it "has #image alias" do
      expect(bundle.method(:image)).to eq(bundle.method(:add_image))
    end
  end

  describe "#add_image_file / #image_file" do
    it "has #image_file alias" do
      expect(bundle.method(:image_file)).to eq(bundle.method(:add_image_file))
    end

    it "reads an image file" do
      expect { bundle.add_image_file("logo", File.join(fixtures, "logo.png")) }.not_to raise_error
    end

    it "raises Errno::ENOENT for missing image" do
      expect { bundle.add_image_file("missing", "/nope.png") }.to raise_error(Errno::ENOENT)
    end
  end
end
