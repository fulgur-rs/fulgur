# CLI拡充 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** CLIに `--margin`（CSS短縮形）、メタデータフラグ群、stdout出力（`-o -`）を追加する。

**Architecture:** Config/ConfigBuilder にメタデータフィールドを追加し、render.rs で krilla Metadata に反映。CLIはclap引数として公開。`producer` はデフォルトで `fulgur vX.Y.Z` を設定。

**Tech Stack:** clap (既存), krilla metadata API

---

### Task 1: Config にメタデータフィールドを追加

**Files:**
- Modify: `crates/fulgur/src/config.rs`

**Step 1: Config 構造体にフィールド追加**

```rust
pub struct Config {
    // ... existing fields ...
    pub authors: Vec<String>,       // author を authors (Vec) に変更
    pub description: Option<String>,
    pub keywords: Vec<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>, // ISO 8601 文字列
}
```

既存の `author: Option<String>` を `authors: Vec<String>` に変更（複数著者対応）。

Default 実装を更新:
- `authors: vec![]`
- `description: None`
- `keywords: vec![]`
- `creator: None`
- `producer: Some(format!("fulgur v{}", env!("CARGO_PKG_VERSION")))`
- `creation_date: None`

**Step 2: ConfigBuilder にセッター追加**

```rust
pub fn authors(mut self, authors: Vec<String>) -> Self { ... }
pub fn description(mut self, description: impl Into<String>) -> Self { ... }
pub fn keywords(mut self, keywords: Vec<String>) -> Self { ... }
pub fn creator(mut self, creator: impl Into<String>) -> Self { ... }
pub fn producer(mut self, producer: impl Into<String>) -> Self { ... }
pub fn creation_date(mut self, date: impl Into<String>) -> Self { ... }
```

既存の `author()` セッターは `authors` に1要素追加する形に変更（後方互換）:
```rust
pub fn author(mut self, author: impl Into<String>) -> Self {
    self.config.authors.push(author.into());
    self
}
```

**Step 3: テスト実行**

Run: `cargo test --lib -p fulgur config`

**Step 4: Commit**

```bash
git add crates/fulgur/src/config.rs
git commit -m "feat: add metadata fields to Config (description, keywords, creator, producer, creation_date)"
```

---

### Task 2: render.rs でメタデータを krilla に反映

**Files:**
- Modify: `crates/fulgur/src/render.rs`

**Step 1: メタデータ構築ヘルパー関数を追加**

2箇所（`render_to_pdf` と `render_to_pdf_with_gcpm`）で重複しているメタデータ設定を1つの関数に抽出:

```rust
fn build_metadata(config: &Config) -> krilla::metadata::Metadata {
    let mut metadata = krilla::metadata::Metadata::new();
    if let Some(ref title) = config.title {
        metadata = metadata.title(title.clone());
    }
    if !config.authors.is_empty() {
        metadata = metadata.authors(config.authors.clone());
    }
    if let Some(ref description) = config.description {
        metadata = metadata.description(description.clone());
    }
    if !config.keywords.is_empty() {
        metadata = metadata.keywords(config.keywords.clone());
    }
    if let Some(ref lang) = config.lang {
        metadata = metadata.language(lang.clone());
    }
    if let Some(ref creator) = config.creator {
        metadata = metadata.creator(creator.clone());
    }
    if let Some(ref producer) = config.producer {
        metadata = metadata.producer(producer.clone());
    }
    // creation_date の krilla::metadata::DateTime 変換は省略（CLI から渡された ISO 8601 文字列をパースする必要がある）
    // TODO: creation_date 対応
    metadata
}
```

両方の関数で `document.set_metadata(build_metadata(config));` に置換。

**Step 2: テスト実行**

Run: `cargo test --lib -p fulgur`

**Step 3: Commit**

```bash
git add crates/fulgur/src/render.rs
git commit -m "refactor: extract build_metadata helper, add new metadata fields to PDF output"
```

---

### Task 3: EngineBuilder にメタデータセッター追加

**Files:**
- Modify: `crates/fulgur/src/engine.rs`

**Step 1: 新しいメタデータセッターを追加**

```rust
pub fn authors(mut self, authors: Vec<String>) -> Self { ... }
pub fn description(mut self, description: impl Into<String>) -> Self { ... }
pub fn keywords(mut self, keywords: Vec<String>) -> Self { ... }
pub fn creator(mut self, creator: impl Into<String>) -> Self { ... }
pub fn producer(mut self, producer: impl Into<String>) -> Self { ... }
pub fn creation_date(mut self, date: impl Into<String>) -> Self { ... }
```

既存の `author()` は ConfigBuilder に委譲（後方互換維持）。

**Step 2: テスト実行**

Run: `cargo test --lib -p fulgur`

**Step 3: Commit**

```bash
git add crates/fulgur/src/engine.rs
git commit -m "feat: add metadata setters to EngineBuilder"
```

---

### Task 4: CLI に --margin フラグを追加

**Files:**
- Modify: `crates/fulgur-cli/src/main.rs`

**Step 1: --margin 引数を追加**

```rust
/// Page margins in mm (CSS shorthand: "20", "20 30", "10 20 30", "10 20 30 40")
#[arg(long)]
margin: Option<String>,
```

**Step 2: parse_margin 関数を実装**

```rust
fn parse_margin(s: &str) -> Margin {
    let values: Vec<f32> = s.split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    let to_pt = |mm: f32| mm * 72.0 / 25.4;
    match values.as_slice() {
        [all] => Margin::uniform(to_pt(*all)),
        [vert, horiz] => Margin::symmetric(to_pt(*vert), to_pt(*horiz)),
        [top, horiz, bottom] => Margin {
            top: to_pt(*top), right: to_pt(*horiz),
            bottom: to_pt(*bottom), left: to_pt(*horiz),
        },
        [top, right, bottom, left] => Margin {
            top: to_pt(*top), right: to_pt(*right),
            bottom: to_pt(*bottom), left: to_pt(*left),
        },
        _ => {
            eprintln!("Invalid margin '{}', using default 20mm", s);
            Margin::default()
        }
    }
}
```

**Step 3: main() でマージンを適用**

`--margin` が指定されていたら `parse_margin` で変換して `builder.margin()` に渡す。未指定時はデフォルト（20mm uniform）。

**Step 4: ビルド確認**

Run: `cargo build -p fulgur-cli`

**Step 5: Commit**

```bash
git add crates/fulgur-cli/src/main.rs
git commit -m "feat: add --margin CLI flag with CSS shorthand syntax"
```

---

### Task 5: CLI にメタデータフラグと stdout 出力を追加

**Files:**
- Modify: `crates/fulgur-cli/src/main.rs`

**Step 1: メタデータ引数を追加**

```rust
/// Author name (can be specified multiple times)
#[arg(long = "author")]
authors: Vec<String>,

/// Document description
#[arg(long)]
description: Option<String>,

/// Keywords (can be specified multiple times)
#[arg(long = "keyword")]
keywords: Vec<String>,

/// Language code (e.g. ja, en)
#[arg(long)]
language: Option<String>,

/// Creator application name
#[arg(long)]
creator: Option<String>,

/// PDF producer (default: fulgur vX.Y.Z)
#[arg(long)]
producer: Option<String>,

/// Creation date in ISO 8601 format (e.g. 2026-03-22)
#[arg(long)]
creation_date: Option<String>,
```

**Step 2: main() でメタデータを EngineBuilder に適用**

各フラグの値をビルダーに渡す。

**Step 3: stdout 出力対応**

`-o -` の場合、stdout にバイナリ出力:
```rust
if output.as_os_str() == "-" {
    let pdf = engine.render_html(&html)?;
    use std::io::Write;
    std::io::stdout().write_all(&pdf)?;
} else {
    engine.render_html_to_file(&html, &output)?;
    println!("PDF written to {}", output.display());
}
```

**Step 4: ビルド確認**

Run: `cargo build -p fulgur-cli`

**Step 5: Commit**

```bash
git add crates/fulgur-cli/src/main.rs
git commit -m "feat: add metadata CLI flags and stdout output support"
```

---

### Task 6: 最終検証

**Step 1: 全テスト実行**

Run: `cargo test --lib -p fulgur`
Run: `cargo test -p fulgur --test gcpm_integration -- --test-threads=1`

**Step 2: clippy と fmt**

Run: `cargo clippy --workspace && cargo fmt --all --check`

**Step 3: 手動テスト（CLIヘルプ確認）**

Run: `cargo run --bin fulgur -- render --help`
Expected: 新しいフラグがヘルプに表示される
