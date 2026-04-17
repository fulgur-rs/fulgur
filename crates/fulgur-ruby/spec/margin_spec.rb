# frozen_string_literal: true

require "spec_helper"

RSpec.describe Fulgur::Margin do
  describe ".new" do
    it "accepts 1 positional (uniform)" do
      m = described_class.new(72)
      expect([m.top, m.right, m.bottom, m.left]).to eq([72.0, 72.0, 72.0, 72.0])
    end

    it "accepts 2 positional (symmetric v, h)" do
      m = described_class.new(72, 36)
      expect([m.top, m.right, m.bottom, m.left]).to eq([72.0, 36.0, 72.0, 36.0])
    end

    it "accepts 4 positional (CSS t, r, b, l)" do
      m = described_class.new(72, 36, 48, 24)
      expect([m.top, m.right, m.bottom, m.left]).to eq([72.0, 36.0, 48.0, 24.0])
    end

    it "accepts kwargs" do
      m = described_class.new(top: 72, right: 36, bottom: 48, left: 24)
      expect([m.top, m.right, m.bottom, m.left]).to eq([72.0, 36.0, 48.0, 24.0])
    end

    it "raises ArgumentError for 3 positional" do
      expect { described_class.new(1, 2, 3) }.to raise_error(ArgumentError)
    end

    it "raises ArgumentError for incomplete kwargs" do
      expect { described_class.new(top: 1, bottom: 2) }.to raise_error(ArgumentError)
    end
  end

  describe ".uniform / .symmetric" do
    it ".uniform(pt) — all sides equal" do
      m = described_class.uniform(50)
      expect([m.top, m.right, m.bottom, m.left]).to eq([50.0, 50.0, 50.0, 50.0])
    end

    it ".symmetric(v, h) — vertical on top/bottom, horizontal on left/right" do
      m = described_class.symmetric(72, 36)
      expect([m.top, m.right, m.bottom, m.left]).to eq([72.0, 36.0, 72.0, 36.0])
    end
  end
end
