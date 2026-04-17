# frozen_string_literal: true

require "spec_helper"

RSpec.describe Fulgur::PageSize do
  describe "constants" do
    it "exposes A4" do
      expect(described_class::A4.width).to be_within(0.1).of(595.28)
      expect(described_class::A4.height).to be_within(0.1).of(841.89)
    end

    it "exposes Letter" do
      expect(described_class::LETTER.width).to be_within(0.1).of(612.0)
      expect(described_class::LETTER.height).to be_within(0.1).of(792.0)
    end

    it "exposes A3" do
      expect(described_class::A3.width).to be_within(0.1).of(841.89)
      expect(described_class::A3.height).to be_within(0.1).of(1190.55)
    end
  end

  describe ".custom" do
    it "accepts width/height in mm and converts both to pt" do
      ps = described_class.custom(100, 200)
      # 100mm → ~283.46pt, 200mm → ~566.93pt
      expect(ps.width).to be_within(0.1).of(283.46)
      expect(ps.height).to be_within(0.1).of(566.93)
    end
  end

  describe "#landscape" do
    it "returns a rotated PageSize" do
      a4 = described_class::A4
      land = a4.landscape
      expect(land.width).to be_within(0.1).of(a4.height)
      expect(land.height).to be_within(0.1).of(a4.width)
    end
  end
end
