# テンプレートエンジン内蔵 設計書

Issue: `fulgur-8jv`

## 概要

MiniJinjaをテンプレートエンジンとして組み込み、テンプレート + JSONデータからPDFを生成できるようにする。テンプレート処理はHTMLパース前の前処理として実行し、既存パイプラインには影響しない。

## CLIインターフェース

`--data` フラグの有無でモードを切り替える：

```bash
# 従来通り（HTMLモード）
fulgur render input.html -o out.pdf

# テンプレートモード（--dataがあればINPUTはテンプレート扱い）
fulgur render invoice.html --data data.json -o out.pdf

# stdinからデータを渡す
cat data.json | fulgur render invoice.html --data - -o out.pdf
```

追加フラグ：

- `--data <PATH>` — JSONデータファイル。`-` でstdin

`{% include %}` や `{% extends %}` のファイルシステム解決は将来拡張として保留。現在は単一テンプレート文字列のレンダリングのみ対応。

## ライブラリAPI

```rust
// テンプレートモード
let pdf = Engine::builder()
    .page_size(PageSize::A4)
    .template("invoice.html", template_str)  // テンプレート名 + テンプレート文字列
    .data(json_value)                         // serde_json::Value
    .build()?
    .render()?;                               // テンプレート→HTML→PDF

// 従来通り（HTMLモード）
let pdf = Engine::builder()
    .page_size(PageSize::A4)
    .build()?
    .render_html(html_str)?;
```

- `template()`: テンプレート名（extends/include解決用）とテンプレート文字列を受け取る
- `data()`: `serde_json::Value` を受け取る
- `render()`: 新メソッド。内部で MiniJinja展開 → `render_html()` を呼ぶ
- `template()` なしで `render()` を呼ぶとエラー
- 追加テンプレート（include/extends用）の登録APIは将来拡張として保留

## 内部アーキテクチャ

### 処理フロー

```text
template + JSON → MiniJinja展開 → HTML文字列 → Parse → DomPass → Resolve → PDF
```

### 実装の配置

- `crates/fulgur/src/template.rs` — MiniJinjaラッパー
  - `render_template(name, template_str, data) -> Result<String>`
  - Environment生成、テンプレート登録、レンダリングをまとめる薄い関数
- `engine.rs` — `render()` メソッド追加。template + data → `render_html()` へ委譲
- `crates/fulgur-cli/src/main.rs` — `--data` フラグ追加、JSONパース

### 依存クレート

- `minijinja` — テンプレートエンジン本体
- `serde_json` — JSONパース（必要に応じて追加）

### エラーハンドリング

- テンプレート構文エラー → MiniJinjaのエラーメッセージをそのまま伝播
- JSONパースエラー → serde_jsonのエラーをそのまま伝播
- `--data` ありでINPUTなし → CLIレベルでエラー

## テスト戦略

### ユニットテスト (`template.rs`)

- 変数展開、ループ、条件分岐、フィルタの基本動作
- テンプレート構文エラー時のエラー返却
- 空データ / 空テンプレートの挙動

### 統合テスト

- テンプレート + JSON → PDF生成の一気通貫テスト
- `--data` なしの従来HTMLモードが壊れていないことの回帰テスト

### CLIテスト

- `--data file.json` でのPDF出力
- `--data -` (stdin) でのPDF出力
- `--data` ありでINPUTなしのエラーメッセージ
