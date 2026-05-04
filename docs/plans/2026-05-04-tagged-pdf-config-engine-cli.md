# Tagged PDF: Config / Engine / CLI フラグ追加 実装計画

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `Config` / `EngineBuilder` / CLI に `enable_tagging` と `pdf_ua` フラグを追加し、`SerializeSettings` へ配線する。タグ付き draw は後続タスクで行う。

**Architecture:** フラグは `Config` struct に追加し、`ConfigBuilder` → `EngineBuilder` → CLI と一貫したパターンで公開する。`render.rs` では `enable_tagging || pdf_ua` の場合のみ `Document::new_with(SerializeSettings{...})` に切り替え、デフォルト時は `Document::new()` を維持してバイト一致を保証する。

**Tech Stack:** Rust, krilla 0.7.0 (`krilla::SerializeSettings`, `krilla::configure::{Configuration, Validator}`), clap

---

### Task 1: Config に `enable_tagging` / `pdf_ua` フィールドを追加する

**Files:**

- Modify: `crates/fulgur/src/config.rs`

**Step 1: 失敗するテストを書く**

`config.rs` の `#[cfg(test)] mod tests` 末尾に以下を追加する:

```rust
#[test]
fn config_enable_tagging_defaults_to_false() {
    let config = Config::default();
    assert!(!config.enable_tagging);
}

#[test]
fn config_pdf_ua_defaults_to_false() {
    let config = Config::default();
    assert!(!config.pdf_ua);
}

#[test]
fn config_effective_tagging_both_false() {
    let config = Config::default();
    assert!(!config.effective_tagging());
}

#[test]
fn config_effective_tagging_enable_tagging() {
    let config = Config::builder().tagged(true).build();
    assert!(config.effective_tagging());
}

#[test]
fn config_effective_tagging_pdf_ua() {
    let config = Config::builder().pdf_ua(true).build();
    assert!(config.effective_tagging());
}

#[test]
fn config_builder_tagged_sets_flag() {
    let config = Config::builder().tagged(true).build();
    assert!(config.enable_tagging);
}

#[test]
fn config_builder_pdf_ua_sets_flag() {
    let config = Config::builder().pdf_ua(true).build();
    assert!(config.pdf_ua);
}
```

**Step 2: テストが失敗することを確認**

```bash
cargo test -p fulgur --lib config 2>&1 | tail -10
```

期待: `error[E0609]: no field 'enable_tagging'` などコンパイルエラー。

**Step 3: `Config` struct にフィールドを追加**

`crates/fulgur/src/config.rs` の `Config` struct に追加:

```rust
pub struct Config {
    // ... 既存フィールド ...
    /// Generate PDF bookmarks (outline) from h1–h6 headings.
    pub bookmarks: bool,
    /// Enable Tagged PDF output (PDF structure tree).
    pub enable_tagging: bool,
    /// Enable PDF/UA-1 conformance (implies enable_tagging).
    pub pdf_ua: bool,
}
```

`Config::default()` に追加:

```rust
impl Default for Config {
    fn default() -> Self {
        Self {
            // ... 既存フィールド ...
            bookmarks: false,
            enable_tagging: false,
            pdf_ua: false,
        }
    }
}
```

`Config` impl に `effective_tagging()` ヘルパーを追加:

```rust
impl Config {
    // ... 既存メソッド ...

    /// Returns true if tagging should be enabled in the PDF output.
    /// pdf_ua implies tagging.
    pub fn effective_tagging(&self) -> bool {
        self.enable_tagging || self.pdf_ua
    }
}
```

`ConfigBuilder` に2つのメソッドを追加 (`bookmarks` の直後):

```rust
pub fn tagged(mut self, enabled: bool) -> Self {
    self.config.enable_tagging = enabled;
    self
}

pub fn pdf_ua(mut self, enabled: bool) -> Self {
    self.config.pdf_ua = enabled;
    self
}
```

**Step 4: テストを通す**

```bash
cargo test -p fulgur --lib config 2>&1 | tail -10
```

期待: 追加した7テストを含む全テストが PASS。

**Step 5: コミット**

```bash
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  add crates/fulgur/src/config.rs
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  commit -m "feat(config): add enable_tagging and pdf_ua fields"
```

---

### Task 2: `EngineBuilder` に `tagged()` / `pdf_ua()` を追加する

**Files:**

- Modify: `crates/fulgur/src/engine.rs`

**Step 1: 失敗するテストを書く**

`engine.rs` の `#[cfg(test)] mod tests` 末尾に追加:

```rust
#[test]
fn builder_tagged_defaults_to_false() {
    let engine = Engine::builder().build();
    assert!(!engine.config().enable_tagging);
}

#[test]
fn builder_pdf_ua_defaults_to_false() {
    let engine = Engine::builder().build();
    assert!(!engine.config().pdf_ua);
}

#[test]
fn builder_tagged_opt_in() {
    let engine = Engine::builder().tagged(true).build();
    assert!(engine.config().enable_tagging);
}

#[test]
fn builder_pdf_ua_opt_in() {
    let engine = Engine::builder().pdf_ua(true).build();
    assert!(engine.config().pdf_ua);
}

#[test]
fn builder_pdf_ua_implies_effective_tagging() {
    let engine = Engine::builder().pdf_ua(true).build();
    assert!(engine.config().effective_tagging());
}
```

**Step 2: テストが失敗することを確認**

```bash
cargo test -p fulgur --lib engine 2>&1 | tail -10
```

期待: `error[E0599]: no method named 'tagged'` などコンパイルエラー。

**Step 3: `EngineBuilder` にメソッドを追加**

`engine.rs` の `EngineBuilder` impl 内、`bookmarks` メソッドの直後に追加:

```rust
pub fn tagged(mut self, enabled: bool) -> Self {
    self.config_builder = self.config_builder.tagged(enabled);
    self
}

pub fn pdf_ua(mut self, enabled: bool) -> Self {
    self.config_builder = self.config_builder.pdf_ua(enabled);
    self
}
```

**Step 4: テストを通す**

```bash
cargo test -p fulgur --lib engine 2>&1 | tail -10
```

期待: 追加した5テストを含む全テストが PASS。

**Step 5: コミット**

```bash
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  add crates/fulgur/src/engine.rs
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  commit -m "feat(engine): add tagged() and pdf_ua() builder methods"
```

---

### Task 3: `render.rs` で `SerializeSettings` を配線する

**Files:**

- Modify: `crates/fulgur/src/render.rs`

**Step 1: smoke test を先に追加する**

`crates/fulgur/tests/render_smoke.rs` の末尾に追加:

```rust
#[test]
fn tagged_render_produces_pdf() {
    let pdf = Engine::builder()
        .tagged(true)
        .build()
        .render_html("<html><body><p>hello tagged</p></body></html>")
        .expect("render tagged");
    assert!(!pdf.is_empty());
    let s = String::from_utf8_lossy(&pdf);
    assert!(s.contains("/StructTreeRoot"), "tagged PDF must have StructTreeRoot");
}
```

**Step 2: smoke test が失敗することを確認**

```bash
cargo test -p fulgur --test render_smoke tagged_render_produces_pdf 2>&1 | tail -15
```

期待: `assertion failed: s.contains("/StructTreeRoot")` — 現在は `/StructTreeRoot` を出力しない。

**Step 3: `render.rs` で `Document` 生成を変更する**

`render.rs` の先頭 use ブロックに追記 (ファイルの既存 use 群の末尾近くに):

```rust
use krilla::configure::{Configuration, Validator};
use krilla::SerializeSettings;
```

`render.rs:34` の `let mut document = krilla::Document::new();` を以下に置換:

```rust
let mut document = if config.effective_tagging() {
    let configuration = if config.pdf_ua {
        Configuration::new_with_validator(Validator::UA1)
    } else {
        Configuration::new()
    };
    krilla::Document::new_with(SerializeSettings {
        enable_tagging: true,
        configuration,
        ..Default::default()
    })
} else {
    krilla::Document::new()
};
```

**Step 4: テストを通す**

```bash
cargo test -p fulgur --test render_smoke tagged_render_produces_pdf 2>&1 | tail -10
```

期待: PASS。

既存テストが壊れていないことを確認:

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur 2>&1 | tail -10
```

**Step 5: コミット**

```bash
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  add crates/fulgur/src/render.rs crates/fulgur/tests/render_smoke.rs
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  commit -m "feat(render): wire SerializeSettings from config.effective_tagging()"
```

---

### Task 4: CLI に `--tagged` / `--pdf-ua` フラグを追加する

**Files:**

- Modify: `crates/fulgur-cli/src/main.rs`

**Step 1: `Commands::Render` 列挙体にフィールドを追加**

`bookmarks: bool,` の直後に追加:

```rust
/// Enable Tagged PDF output (structure tree).
#[arg(long)]
tagged: bool,

/// Enable PDF/UA-1 conformance (implies --tagged).
#[arg(long = "pdf-ua")]
pdf_ua: bool,
```

**Step 2: `Commands::Render` match arm のデストラクチャに追加**

```rust
Commands::Render {
    // ... 既存フィールド ...
    bookmarks,
    tagged,
    pdf_ua,
} => {
```

**Step 3: builder 呼び出し部分に配線を追加**

`if bookmarks { builder = builder.bookmarks(true); }` の直後に追加:

```rust
if tagged {
    builder = builder.tagged(true);
}
if pdf_ua {
    builder = builder.pdf_ua(true);
}
```

**Step 4: コンパイル確認**

```bash
cargo build -p fulgur-cli 2>&1 | tail -5
```

期待: エラーなし。

**Step 5: コミット**

```bash
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  add crates/fulgur-cli/src/main.rs
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  commit -m "feat(cli): add --tagged and --pdf-ua flags to render subcommand"
```

---

### Task 5: CLI 統合テストを追加する

**Files:**

- Create: `crates/fulgur-cli/tests/tagged_cli.rs`

**Step 1: テストファイルを作成**

`crates/fulgur-cli/tests/tagged_cli.rs`:

```rust
use std::process::Command;
use tempfile::TempDir;

fn run_cli(args: &[&str]) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_fulgur");
    Command::new(bin).args(args).output().expect("spawn fulgur")
}

#[test]
fn cli_tagged_flag_produces_struct_tree_root() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<html><body><p>Hello tagged world</p></body></html>",
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--tagged",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        s.contains("/StructTreeRoot"),
        "tagged PDF must contain /StructTreeRoot"
    );
}

#[test]
fn cli_without_tagged_flag_has_no_struct_tree_root() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(&html_path, "<html><body><p>Hello</p></body></html>").unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    let s = String::from_utf8_lossy(&pdf);
    assert!(
        !s.contains("/StructTreeRoot"),
        "untagged PDF must not contain /StructTreeRoot"
    );
}

#[test]
fn cli_pdf_ua_flag_succeeds() {
    let dir = TempDir::new().expect("create temp dir");
    let html_path = dir.path().join("doc.html");
    let pdf_path = dir.path().join("doc.pdf");
    std::fs::write(
        &html_path,
        "<html><body><p>Hello PDF/UA</p></body></html>",
    )
    .unwrap();

    let out = run_cli(&[
        "render",
        html_path.to_str().unwrap(),
        "-o",
        pdf_path.to_str().unwrap(),
        "--pdf-ua",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "CLI --pdf-ua failed: {stderr}");
    let pdf = std::fs::read(&pdf_path).unwrap();
    assert!(!pdf.is_empty());
}
```

**Step 2: テストを実行して通ることを確認**

```bash
cargo test -p fulgur-cli --test tagged_cli 2>&1 | tail -15
```

期待: 3テストすべて PASS。

**Step 3: コミット**

```bash
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  add crates/fulgur-cli/tests/tagged_cli.rs
git -C /home/ubuntu/fulgur/.worktrees/feature/tagged-pdf-config \
  commit -m "test(cli): add tagged_cli integration tests"
```

---

### Task 6: 全テストと lint を確認する

**Step 1: 全ライブラリテスト**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
```

期待: 全 PASS (増加していること)。

**Step 2: 全統合テスト**

```bash
cargo test -p fulgur 2>&1 | tail -10
cargo test -p fulgur-cli 2>&1 | tail -10
```

期待: 全 PASS。

**Step 3: lint**

```bash
cargo clippy -p fulgur -p fulgur-cli -- -D warnings 2>&1 | tail -10
cargo fmt --check 2>&1 | tail -5
```

期待: 警告・エラーなし。

**Step 4: markdownlint**

```bash
npx markdownlint-cli2 'docs/plans/2026-05-04-tagged-pdf-config-engine-cli.md'
```
