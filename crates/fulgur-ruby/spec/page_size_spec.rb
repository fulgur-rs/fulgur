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

    it "exposes A5 as distinct from A4" do
      # fulgur-5oav: A5 must not be a silent A4 fallback.
      expect(described_class::A5.width).to be_within(0.1).of(148.0 * 72.0 / 25.4)
      expect(described_class::A5.height).to be_within(0.1).of(210.0 * 72.0 / 25.4)
      expect(described_class::A5.width).not_to eq(described_class::A4.width)
    end

    it "exposes JIS_B4" do
      expect(described_class::JIS_B4.width).to be_within(0.1).of(257.0 * 72.0 / 25.4)
      expect(described_class::JIS_B4.height).to be_within(0.1).of(364.0 * 72.0 / 25.4)
    end

    it "exposes Legal and Ledger" do
      expect(described_class::LEGAL.width).to be_within(0.1).of(8.5 * 72.0)
      expect(described_class::LEGAL.height).to be_within(0.1).of(14.0 * 72.0)
      expect(described_class::LEDGER.width).to be_within(0.1).of(11.0 * 72.0)
      expect(described_class::LEDGER.height).to be_within(0.1).of(17.0 * 72.0)
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
