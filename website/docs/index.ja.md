---
hide:
  - navigation
  - toc
---

<div class="fulgur-hero" markdown>

# Fulgur

**AIは自由に書く。Fulgurは美しく仕上げる。**

RustのためのHTML/CSS → PDF変換ライブラリ。
AIエージェントが生成した出力を、美しく永続的な文書として仕上げる。

[はじめる](#quick-start){ .md-button .md-button--primary }
[GitHub](https://github.com/fulgur-rs/fulgur){ .md-button }

</div>

---

## クイックスタート { #quick-start }

=== "CLI"

    インストール不要。`npx` でそのまま実行:

    ```bash
    npx @fulgur-rs/cli render input.html -o output.pdf
    ```

    グローバルインストールする場合:

    ```bash
    npm install -g @fulgur-rs/cli
    fulgur render input.html -o output.pdf
    ```

    Rust ユーザー向け:

    ```bash
    cargo install fulgur-cli
    fulgur render input.html -o output.pdf
    ```

=== "Rust"

    ```toml
    # Cargo.toml
    [dependencies]
    fulgur = "0.5"
    ```

    ```rust
    use fulgur::Engine;

    fn main() -> Result<(), Box<dyn std::error::Error>> {
        let html = "<h1>こんにちは、Fulgur</h1>";
        let pdf = Engine::builder().build().render_html(html)?;
        std::fs::write("output.pdf", pdf)?;
        Ok(())
    }
    ```

=== "Python"

    ```bash
    pip install pyfulgur
    ```

    ```python
    from pyfulgur import Engine

    html = "<h1>こんにちは、Fulgur</h1>"
    pdf = Engine().render_html(html)
    with open("output.pdf", "wb") as f:
        f.write(pdf)
    ```

=== "Ruby"

    ```bash
    gem install fulgur
    ```

    ```ruby
    require "fulgur"

    html = "<h1>こんにちは、Fulgur</h1>"
    pdf = Fulgur::Engine.new.render_html(html)
    File.binwrite("output.pdf", pdf)
    ```

---

## ユースケース

<div class="grid cards" markdown>

- :robot: **AIエージェントの出力**

    AIエージェントが生成したHTMLレポートを、権威ある永続的なPDFとして封印する。

- :page_facing_up: **SaaS帳票生成**

    マルチテナント環境で、テンプレートとJSONデータから請求書・帳票を動的に生成する。

- :bar_chart: **バッチレポート**

    JSONデータから数千枚のPDFを並列生成。決定論的で再現性のある出力。

</div>

---

## 特徴

<div class="grid cards" markdown>

- :zap: **圧倒的な速度**

    wkhtmltopdfやPuppeteerを上回るスループット。計測済み、主張だけではない。

- :crab: **Pure Rust**

    ヘッドレスブラウザなし、Chromiumなし、C++ランタイムなし。`cargo add fulgur` の一行で完結。

- :package: **オフラインファースト**

    レンダリング時のネットワークアクセスはゼロ。すべてのアセットを明示的にバンドル。サンドボックスやWASM環境に最適。

- :lock: **決定論的な出力**

    同じHTML + 同じバンドル済みアセット = バイト単位で再現可能なPDF。

- :notebook: **CSS Paged Media (GCPM)**

    ランニングヘッダー/フッター、ページカウンター、名前付きページ、マージンボックス——本物の文書に必要なCSSスペック。

</div>

---

## 比較

|  | Fulgur | wkhtmltopdf | WeasyPrint | fullbleed | Puppeteer | Prince | Gotenberg |
|--|--------|-------------|------------|-----------|-----------|--------|-----------|
| **Pure Rust** | ✅ | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| **WASM対応** | 計画中 | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **CSS GCPM深度** | ★★★ | ★★ | ★★★ | ★ | ★★ | ★★★★ | ★★ |
| **ヘッドレスブラウザ不要** | ✅ | ❌ | ✅ | ✅ | ❌ | ✅ | ❌ |
| **JavaScript実行** | ❌ | △ | ❌ | ❌ | ✅ | ✅ | ✅ |
| **サーバーレス対応** | ✅ | ✅ | ✅ | ✅ | △ | △ | △ |
| **速度** | ⚡⚡⚡ | ⚡ | ⚡ | ⚡⚡⚡ | ⚡ | ⚡⚡ | ⚡ |
| **ライセンス** | MIT/Apache | LGPL | BSD | MIT | MIT | 商用 | MIT |
| **インストールサイズ** | 小 | 中 | 中 | 小 | 大 | 中 | 大 |

> PrinceはCSS Paged Mediaのリファレンス実装であり、CSS対応品質の最重要ベンチマーク。
> GotenbergはDockerが必要。PuppeteerとGotenbergはChromiumをフルインストール。

---

## wkhtmltopdfの遺志を継いで

Fulgurは[wkhtmltopdf](https://wkhtmltopdf.org/)に触発されたプロジェクトです——
10年以上にわたってウェブ開発コミュニティを支えてきたツールへの敬意から生まれました。
私たちはその遺産をRust時代へと引き継ぐためにFulgurを作りました:
より速く、より安全に、AI生成文書の時代に対応して。

---

*MIT / Apache-2.0 &nbsp;·&nbsp; [GitHub](https://github.com/fulgur-rs/fulgur) &nbsp;·&nbsp; [crates.io](https://crates.io/crates/fulgur)*
