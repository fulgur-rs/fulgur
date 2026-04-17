# frozen_string_literal: true

require "spec_helper"

RSpec.describe Fulgur::Engine do
  let(:html) { File.read(File.expand_path("fixtures/simple.html", __dir__)) }

  describe ".new" do
    it "accepts no kwargs" do
      expect { described_class.new }.not_to raise_error
    end

    it "accepts page_size as string" do
      expect { described_class.new(page_size: "A4") }.not_to raise_error
    end

    it "accepts page_size as symbol" do
      expect { described_class.new(page_size: :a4) }.not_to raise_error
    end

    it "accepts page_size as PageSize constant" do
      expect { described_class.new(page_size: Fulgur::PageSize::A4) }.not_to raise_error
    end

    it "raises ArgumentError for unknown page_size string" do
      expect { described_class.new(page_size: "XYZ") }.to raise_error(ArgumentError)
    end

    it "accepts margin kwarg" do
      expect { described_class.new(margin: Fulgur::Margin.uniform(50)) }.not_to raise_error
    end

    it "accepts assets kwarg" do
      bundle = Fulgur::AssetBundle.new
      bundle.css "body { color: red }"
      expect { described_class.new(assets: bundle) }.not_to raise_error
    end

    it "accepts title / author / lang / bookmarks / landscape kwargs" do
      expect {
        described_class.new(
          title: "doc",
          author: "me",
          lang: "en",
          bookmarks: true,
          landscape: true,
        )
      }.not_to raise_error
    end
  end

  describe ".builder" do
    it "returns an EngineBuilder" do
      expect(described_class.builder).to be_a(Fulgur::EngineBuilder)
    end

    it "builds an Engine via chain" do
      engine = described_class.builder.page_size(:a4).build
      expect(engine).to be_a(described_class)
    end

    it "supports full chain including author / lang / bookmarks" do
      engine = described_class.builder
        .page_size(:letter)
        .margin(Fulgur::Margin.uniform(72))
        .landscape(true)
        .title("test")
        .author("me")
        .lang("en")
        .bookmarks(true)
        .build
      expect(engine).to be_a(described_class)
    end

    it "raises RuntimeError on double build" do
      b = described_class.builder
      b.build
      expect { b.build }.to raise_error(/already been built/)
    end
  end

  describe "GVL release" do
    # render_html が GVL を解放している限り、Ruby 側の別スレッド (ticker) は
    # レンダリング中にも進行できる。GVL が解放されていない場合、ticker は
    # render_html 完了まで走れず counter はほぼ 0 のまま。
    #
    # NOTE: render loop 完了直後に counter をキャプチャする。`ticker.join`
    # 自体も GVL を解放するため、join 後の counter を測ると GVL を保持した
    # 場合でも ticker が完走して false-pass してしまう。
    it "allows concurrent Ruby threads during render_html" do
      engine = described_class.new
      html = File.read(File.expand_path("fixtures/simple.html", __dir__))

      counter = 0
      mutex = Mutex.new

      ticker = Thread.new do
        200.times do
          mutex.synchronize { counter += 1 }
          sleep 0.002
        end
      end

      10.times { engine.render_html(html) }
      # render loop 直後にキャプチャ (GVL 保持時は 0 付近、解放時は >0)
      counter_during_render = mutex.synchronize { counter }
      ticker.kill
      ticker.join(1)

      # 10 回の render_html 中に ticker が最低 5 回は回るはず。
      # 極端に遅い CI を考慮して閾値を保守的に 5 に設定。
      # 時間ベースヒューリスティックで render 時間 >> sleep interval が前提。
      expect(counter_during_render).to be >= 5
    end
  end
end
