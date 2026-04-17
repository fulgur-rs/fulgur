# frozen_string_literal: true

require "spec_helper"

RSpec.describe "Fulgur error hierarchy" do
  it "defines Fulgur::Error as a StandardError" do
    expect(Fulgur::Error.ancestors).to include(StandardError)
  end

  it "defines Fulgur::RenderError under Fulgur::Error" do
    expect(Fulgur::RenderError.ancestors).to include(Fulgur::Error)
  end

  it "defines Fulgur::AssetError under Fulgur::Error" do
    expect(Fulgur::AssetError.ancestors).to include(Fulgur::Error)
  end
end
