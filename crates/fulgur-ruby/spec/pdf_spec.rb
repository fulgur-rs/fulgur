# frozen_string_literal: true

require "spec_helper"
require "base64"

RSpec.describe Fulgur::Pdf do
  let(:html) { File.read(File.expand_path("fixtures/simple.html", __dir__)) }
  let(:engine) { Fulgur::Engine.new(page_size: :a4) }
  let(:pdf) { engine.render_html(html) }

  it "returns a Fulgur::Pdf" do
    expect(pdf).to be_a(described_class)
  end

  describe "#bytesize" do
    it "returns a positive integer" do
      expect(pdf.bytesize).to be > 100
    end
  end

  describe "#to_s" do
    it "returns ASCII-8BIT encoded string starting with %PDF-" do
      s = pdf.to_s
      expect(s.encoding).to eq(Encoding::ASCII_8BIT)
      expect(s[0, 5]).to eq("%PDF-".b)
    end

    it "to_s.bytesize == bytesize" do
      expect(pdf.to_s.bytesize).to eq(pdf.bytesize)
    end
  end

  describe "#to_base64" do
    it "returns a String that round-trips via Base64.strict_decode64" do
      b64 = pdf.to_base64
      expect(b64).to match(%r{\A[A-Za-z0-9+/=]+\z})
      decoded = Base64.strict_decode64(b64)
      expect(decoded.force_encoding(Encoding::ASCII_8BIT)).to eq(pdf.to_s)
    end
  end

  describe "#to_data_uri" do
    it "returns data URI with application/pdf prefix" do
      expect(pdf.to_data_uri).to start_with("data:application/pdf;base64,")
    end
  end

  describe "#write_to_path" do
    it "writes binary PDF to file" do
      Dir.mktmpdir do |dir|
        path = File.join(dir, "out.pdf")
        pdf.write_to_path(path)
        expect(File.binread(path)).to eq(pdf.to_s)
      end
    end

    it "raises Errno::ENOENT for missing directory (map_fulgur_error 経由)" do
      # std::fs::write は親ディレクトリが存在しない場合 NotFound を返し、
      # src/error.rs の map_fulgur_error が Errno::ENOENT に写像する。
      expect { pdf.write_to_path("/nonexistent/dir/out.pdf") }.to raise_error(Errno::ENOENT)
    end

    it "accepts Pathname" do
      require "pathname"
      Dir.mktmpdir do |dir|
        path = Pathname.new(dir) + "out.pdf"
        pdf.write_to_path(path)
        expect(File.binread(path)).to eq(pdf.to_s)
      end
    end
  end

  describe "#write_to_io" do
    it "writes binary PDF to StringIO" do
      io = StringIO.new
      pdf.write_to_io(io)
      expect(io.string.force_encoding(Encoding::ASCII_8BIT)).to eq(pdf.to_s)
    end

    it "total bytes written equals pdf.bytesize" do
      io = StringIO.new
      pdf.write_to_io(io)
      expect(io.string.bytesize).to eq(pdf.bytesize)
    end

    it "calls binmode on the IO object when supported" do
      io = StringIO.new
      expect(io).to receive(:binmode).at_least(:once).and_call_original
      allow(io).to receive(:write).and_call_original
      pdf.write_to_io(io)
    end

    it "does not raise when IO lacks #binmode (duck-typed)" do
      sink = Class.new do
        attr_reader :buffer
        def initialize
          @buffer = String.new(encoding: Encoding::ASCII_8BIT)
        end
        # IO#write contract: return the number of bytes written (Integer).
        def write(chunk)
          bytes = chunk.b
          @buffer << bytes
          bytes.bytesize
        end
      end.new
      expect { pdf.write_to_io(sink) }.not_to raise_error
      expect(sink.buffer.bytesize).to eq(pdf.bytesize)
    end

    it "raises RuntimeError when IO#write returns 0 bytes" do
      stuck = Class.new do
        def write(_chunk) = 0
      end.new
      expect { pdf.write_to_io(stuck) }.to raise_error(RuntimeError, /0 bytes/)
    end

    it "retries short writes" do
      chunky = Class.new do
        attr_reader :buffer
        def initialize
          @buffer = String.new(encoding: Encoding::ASCII_8BIT)
        end
        # 最初の 3 回は 7 バイトだけ書き込み、それ以降は全量書き込む。
        def write(chunk)
          @calls ||= 0
          @calls += 1
          take = (@calls <= 3 ? 7 : chunk.bytesize).clamp(1, chunk.bytesize)
          @buffer << chunk.b[0, take]
          take
        end
      end.new
      pdf.write_to_io(chunky)
      expect(chunky.buffer.bytesize).to eq(pdf.bytesize)
    end
  end

  describe "#inspect" do
    it "returns a short descriptor with bytesize" do
      expect(pdf.inspect).to match(/\A#<Fulgur::Pdf bytesize=\d+>\z/)
    end
  end
end
