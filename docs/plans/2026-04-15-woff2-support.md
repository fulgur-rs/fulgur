# WOFF2 Font Support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** AssetBundle で WOFF2 フォントを読み込めるようにする。内部で TTF にデコードして parley/krilla に渡す。

**Architecture:** `AssetBundle::add_font_file` / 新規 `add_font_bytes` でマジックバイト判定を行い、WOFF2 なら `woff2::decode::convert_woff2_to_ttf` でデコードしてから格納。下流（blitz_adapter、krilla）は無変更。

**Tech Stack:** Rust, `woff2-patched = "0.4"` crate (Pure Rust, brotli decoder, bytes 1.10+ 互換 fork of woff2)、既存の thiserror ベースエラー型。

> **Note:** Task 1 で `woff2 = "0.3"` から `woff2-patched = "0.4"` に切替。woff2 0.3.0 は `bytes >= 1.10` と非互換（`TryGetError` 未対応）。import path は `woff2_patched::decode::convert_woff2_to_ttf`。

**Beads issue:** fulgur-hkh

**Worktree:** `.worktrees/woff2-support` (branch: `feature/woff2-support`)

---

## Task 1: woff2 依存追加と Error バリアント追加

**Files:**

- Modify: `crates/fulgur/Cargo.toml`
- Modify: `crates/fulgur/src/error.rs`

**Step 1: woff2 クレートを追加**

`crates/fulgur/Cargo.toml` の `[dependencies]` セクション（`stylo = "0.8.0"` の直後）に追加:

```toml
woff2 = "0.3"
```

**Step 2: Error バリアントを追加**

`crates/fulgur/src/error.rs` の `Error` enum に 2 バリアント追加:

```rust
#[error("WOFF decode error: {0}")]
WoffDecode(String),

#[error("Unsupported font format: {0}")]
UnsupportedFontFormat(String),
```

**Step 3: ビルドで依存解決確認**

Run: `cargo build -p fulgur`
Expected: コンパイル成功、woff2 クレートとその依存 (brotli, bitvec, etc.) がダウンロードされる

**Step 4: 既存テストが通ることを確認**

Run: `cargo test --lib -p fulgur`
Expected: 353 passed, 0 failed

**Step 5: Commit**

```bash
git add crates/fulgur/Cargo.toml crates/fulgur/src/error.rs Cargo.lock
git commit -m "deps(fulgur): add woff2 crate and error variants for WOFF support"
```

---

## Task 2: `detect_font_format` ヘルパーを追加（TDD）

**Files:**

- Modify: `crates/fulgur/src/asset.rs`

**Step 1: failing test を書く**

`crates/fulgur/src/asset.rs` の `#[cfg(test)] mod tests` にテスト追加:

```rust
#[test]
fn test_detect_font_format_ttf() {
    assert_eq!(detect_font_format(&[0x00, 0x01, 0x00, 0x00, 0xFF]), FontFormat::Ttf);
}

#[test]
fn test_detect_font_format_otf() {
    assert_eq!(detect_font_format(b"OTTO\x00\x00"), FontFormat::Otf);
}

#[test]
fn test_detect_font_format_ttc() {
    assert_eq!(detect_font_format(b"ttcf\x00\x00"), FontFormat::Ttc);
}

#[test]
fn test_detect_font_format_woff2() {
    assert_eq!(detect_font_format(b"wOF2\x00\x00"), FontFormat::Woff2);
}

#[test]
fn test_detect_font_format_woff1() {
    assert_eq!(detect_font_format(b"wOFF\x00\x00"), FontFormat::Woff1);
}

#[test]
fn test_detect_font_format_unknown() {
    assert_eq!(detect_font_format(b"XXXX"), FontFormat::Unknown);
    assert_eq!(detect_font_format(&[0x00]), FontFormat::Unknown);
    assert_eq!(detect_font_format(&[]), FontFormat::Unknown);
}

#[test]
fn test_detect_font_format_old_mac_ttf() {
    assert_eq!(detect_font_format(b"true\x00\x00"), FontFormat::Ttf);
    assert_eq!(detect_font_format(b"typ1\x00\x00"), FontFormat::Ttf);
}
```

**Step 2: テストが compile error で失敗することを確認**

Run: `cargo test -p fulgur --lib asset::tests::test_detect_font_format_ttf`
Expected: コンパイルエラー（`detect_font_format` と `FontFormat` が未定義）

**Step 3: 最小実装を書く**

`crates/fulgur/src/asset.rs` のファイル内（`impl AssetBundle` ブロックの外）に追加:

```rust
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FontFormat {
    Ttf,
    Otf,
    Ttc,
    Woff1,
    Woff2,
    Unknown,
}

pub(crate) fn detect_font_format(bytes: &[u8]) -> FontFormat {
    match bytes.get(0..4) {
        Some(b"wOF2") => FontFormat::Woff2,
        Some(b"wOFF") => FontFormat::Woff1,
        Some(b"OTTO") => FontFormat::Otf,
        Some(b"ttcf") => FontFormat::Ttc,
        Some([0x00, 0x01, 0x00, 0x00]) => FontFormat::Ttf,
        Some(b"true") | Some(b"typ1") => FontFormat::Ttf,
        _ => FontFormat::Unknown,
    }
}
```

**Step 4: テストを実行して pass を確認**

Run: `cargo test -p fulgur --lib asset::tests::test_detect_font_format`
Expected: 7 passed

**Step 5: Commit**

```bash
git add crates/fulgur/src/asset.rs
git commit -m "feat(asset): add detect_font_format helper with magic byte detection"
```

---

## Task 3: `add_font_bytes` API 実装（TTF passthrough、TDD）

**Files:**

- Modify: `crates/fulgur/src/asset.rs`

**Step 1: failing test を書く**

`crates/fulgur/src/asset.rs` の tests モジュールに追加:

```rust
#[test]
fn test_add_font_bytes_ttf_passthrough() {
    let mut bundle = AssetBundle::new();
    let mut data = vec![0x00, 0x01, 0x00, 0x00];
    data.extend_from_slice(&[0xAA; 100]);
    bundle.add_font_bytes(data.clone()).expect("should accept TTF");
    assert_eq!(bundle.fonts.len(), 1);
    assert_eq!(&bundle.fonts[0][..], &data[..]);
}

#[test]
fn test_add_font_bytes_unknown_passthrough() {
    let mut bundle = AssetBundle::new();
    let data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00];
    bundle.add_font_bytes(data.clone()).expect("unknown format should pass through");
    assert_eq!(bundle.fonts.len(), 1);
    assert_eq!(&bundle.fonts[0][..], &data[..]);
}

#[test]
fn test_add_font_bytes_woff1_rejected() {
    let mut bundle = AssetBundle::new();
    let data = b"wOFF\x00\x01\x00\x00".to_vec();
    let err = bundle.add_font_bytes(data).expect_err("WOFF1 must be rejected");
    match err {
        Error::UnsupportedFontFormat(s) => assert!(s.contains("WOFF1"), "msg: {s}"),
        other => panic!("wrong variant: {other:?}"),
    }
    assert_eq!(bundle.fonts.len(), 0);
}
```

**Step 2: テストが失敗することを確認**

Run: `cargo test -p fulgur --lib asset::tests::test_add_font_bytes`
Expected: コンパイルエラー（`add_font_bytes` が存在しない）

**Step 3: `add_font_bytes` と `add_font_file` 改修を実装**

`crates/fulgur/src/asset.rs` の `impl AssetBundle` に追加/置換:

```rust
pub fn add_font_bytes(&mut self, data: Vec<u8>) -> Result<()> {
    let decoded = match detect_font_format(&data) {
        FontFormat::Woff2 => decode_woff2(&data)?,
        FontFormat::Woff1 => {
            return Err(Error::UnsupportedFontFormat(
                "WOFF1 is not supported; convert to WOFF2 or TTF/OTF".into(),
            ));
        }
        FontFormat::Unknown => {
            log::warn!(
                "add_font_bytes: unknown font magic bytes; passing through as-is"
            );
            data
        }
        FontFormat::Ttf | FontFormat::Otf | FontFormat::Ttc => data,
    };
    self.fonts.push(Arc::new(decoded));
    Ok(())
}
```

そして `add_font_file` を `add_font_bytes` に委譲する形に置換:

```rust
pub fn add_font_file(&mut self, path: impl AsRef<Path>) -> Result<()> {
    let data = std::fs::read(path)?;
    self.add_font_bytes(data)
}
```

**Step 4: Task 3 のテストを実行**

Run: `cargo test -p fulgur --lib asset::tests::test_add_font_bytes`
Expected: 3 passed（WOFF2 テストはまだ書いていない）

**Step 5: 既存の font_bundle 統合テストを実行して retrogression がないこと確認**

Run: `cargo test -p fulgur --test font_bundle_test`
Expected: すべて pass

**Step 6: Commit**

```bash
git add crates/fulgur/src/asset.rs
git commit -m "feat(asset): add add_font_bytes API with format auto-detection"
```

---

## Task 4: WOFF2 デコーダ統合（TDD、実フィクスチャ）

**Files:**

- Create: `crates/fulgur/tests/fixtures/fonts/NotoSans-Regular.woff2`（詳細は Step 1）
- Modify: `crates/fulgur/src/asset.rs`

**Step 1: WOFF2 フィクスチャを用意**

既存の TTF（`examples/.fonts/NotoSans-Regular.ttf` 等）から woff2 バイナリで変換、または Google Fonts 公開の NotoSans Regular WOFF2 をダウンロード。配置先:

```text
crates/fulgur/tests/fixtures/fonts/NotoSans-Regular.woff2
```

サイズは 300KB 以下を目安に小さいサブセットを推奨。なければフルセットも可。コマンド例:

```bash
# Pythonの fonttools を使うか、TTF元があれば:
python3 -c "from fontTools.ttLib import TTFont; f=TTFont('examples/.fonts/NotoSans-Regular.ttf'); f.flavor='woff2'; f.save('crates/fulgur/tests/fixtures/fonts/NotoSans-Regular.woff2')"
```

fonttools がない環境では、`pip install fonttools brotli` してから上記を実行。

ディレクトリが存在しない場合:

```bash
mkdir -p crates/fulgur/tests/fixtures/fonts
```

**Step 2: failing test を書く**

`crates/fulgur/src/asset.rs` の tests モジュールに追加:

```rust
#[test]
fn test_add_font_bytes_woff2_decodes_to_ttf_or_otf() {
    let data = std::fs::read(
        "tests/fixtures/fonts/NotoSans-Regular.woff2"
    ).expect("fixture must exist");
    assert_eq!(detect_font_format(&data), FontFormat::Woff2);

    let mut bundle = AssetBundle::new();
    bundle.add_font_bytes(data).expect("WOFF2 should decode");
    assert_eq!(bundle.fonts.len(), 1);

    let decoded = &bundle.fonts[0];
    let magic = &decoded[0..4];
    assert!(
        magic == [0x00, 0x01, 0x00, 0x00] || magic == b"OTTO",
        "decoded magic should be TTF or OTF, got {magic:?}"
    );
}

#[test]
fn test_add_font_bytes_woff2_invalid_returns_error() {
    let mut bundle = AssetBundle::new();
    let fake = b"wOF2\x00\x00\x00\x00garbagegarbagegarbage".to_vec();
    let err = bundle.add_font_bytes(fake).expect_err("bad WOFF2 must error");
    match err {
        Error::WoffDecode(_) => {}
        other => panic!("wrong variant: {other:?}"),
    }
    assert_eq!(bundle.fonts.len(), 0);
}
```

**Step 3: テストが失敗することを確認**

Run: `cargo test -p fulgur --lib asset::tests::test_add_font_bytes_woff2`
Expected: panic "fixture must exist" または `decode_woff2` 未定義エラー

**Step 4: `decode_woff2` 実装**

`crates/fulgur/src/asset.rs` のファイル内（`impl AssetBundle` の外）に追加:

```rust
fn decode_woff2(data: &[u8]) -> Result<Vec<u8>> {
    let mut buf: &[u8] = data;
    woff2_patched::decode::convert_woff2_to_ttf(&mut buf)
        .map_err(|e| Error::WoffDecode(format!("WOFF2 decode failed: {e:?}")))
}
```

`impl Buf for &[u8]` は `bytes` crate が提供しているため `&mut &data[..]` でそのまま渡せる。

（`impl Buf for &[u8]` は `bytes` crate で提供されている）

**Step 5: テストを実行**

Run: `cargo test -p fulgur --lib asset::tests::test_add_font_bytes_woff2`
Expected: 2 passed

**Step 6: 全ライブラリテスト実行（回帰チェック）**

Run: `cargo test --lib -p fulgur`
Expected: 353 + 新規テスト分すべて pass

**Step 7: Commit**

```bash
git add crates/fulgur/src/asset.rs crates/fulgur/tests/fixtures/fonts/NotoSans-Regular.woff2
git commit -m "feat(asset): decode WOFF2 fonts to TTF at ingestion"
```

---

## Task 5: Integration test（PDF 生成 end-to-end）

**Files:**

- Create: `crates/fulgur/tests/woff2_integration.rs`

**Step 1: Integration test を書く**

`crates/fulgur/tests/woff2_integration.rs` を新規作成:

```rust
use fulgur::{AssetBundle, Engine};
use std::sync::Arc;

#[test]
fn woff2_font_renders_to_pdf() {
    let mut bundle = AssetBundle::new();
    bundle
        .add_font_file("tests/fixtures/fonts/NotoSans-Regular.woff2")
        .expect("WOFF2 load must succeed");

    let html = r#"
        <!DOCTYPE html>
        <html><head><style>
            body { font-family: "Noto Sans", sans-serif; }
        </style></head>
        <body><p>WOFF2 フォントテスト Hello</p></body>
        </html>
    "#;

    let engine = Engine::new().with_asset_bundle(Arc::new(bundle));
    let pdf = engine.render_html(html).expect("PDF render must succeed");
    assert!(pdf.len() > 1000, "PDF should be non-trivial, got {} bytes", pdf.len());
    assert_eq!(&pdf[0..5], b"%PDF-", "output must be a PDF");
    // Verify the WOFF2 font was actually decoded and embedded, not silently
    // replaced with a system fallback. Krilla emits a 6-letter subset prefix
    // followed by the PostScript name (e.g. `KGTYZU+NotoSans-Regular`).
    let needle = b"NotoSans-Regular";
    assert!(
        pdf.windows(needle.len()).any(|w| w == needle),
        "PDF must contain embedded font name"
    );
}
```

**注意:** Engine の API (`with_asset_bundle`, `render_html`) が既存と一致するか、`crates/fulgur/tests/font_bundle_test.rs` を参照して正しい呼び出しに調整する。

**Step 2: テスト実行**

Run: `cargo test -p fulgur --test woff2_integration`
Expected: 1 passed

**Step 3: Commit**

```bash
git add crates/fulgur/tests/woff2_integration.rs
git commit -m "test(asset): add WOFF2 integration test via PDF render"
```

---

## Task 6: CLI / README / docs の更新

**Files:**

- Modify: `README.md` （フォント対応形式セクション）
- Modify: `docs/css-support.md` もしくは該当する機能一覧ドキュメント（あれば）

**Step 1: README 更新**

`README.md` でフォント関連記述を検索し、対応形式に「WOFF2」を追記。例:

```markdown
Supported font formats: TTF, OTF, TTC, **WOFF2** (auto-decoded to TTF)
```

**Step 2: docs の更新（該当ファイルがあれば）**

`grep -r "TTF\|OTF\|font.*format" docs/` で関連ファイルを探し、WOFF2 サポートを追記。

**Step 3: markdownlint 確認**

Run: `npx markdownlint-cli2 '**/*.md'`
Expected: No errors

**Step 4: Commit**

```bash
git add README.md docs/
git commit -m "docs: document WOFF2 font support"
```

---

## Task 7: 最終検証（verification-before-completion）

**Step 1: cargo fmt**

Run: `cargo fmt --check`
Expected: No diff

**Step 2: cargo clippy**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings

**Step 3: 全テスト**

Run: `cargo test -p fulgur`
Expected: lib + integration すべて pass

**Step 4: CLI スモークテスト**

Run:

```bash
cargo run --bin fulgur -- render examples/hello.html -o /tmp/fulgur-smoke.pdf
```

Expected: PDF が生成され `/tmp/fulgur-smoke.pdf` が 1KB 以上

**Step 5: 手動確認**

`/tmp/fulgur-smoke.pdf` が壊れていないこと。

---

## Acceptance Criteria チェックリスト

- [ ] `AssetBundle::add_font_file` が WOFF2 ファイルを受け付け、内部で TTF にデコード
- [ ] `AssetBundle::add_font_bytes` (新API) でメモリ上バイト列から auto-detect 登録
- [ ] WOFF1 で `Error::UnsupportedFontFormat` が返る
- [ ] WOFF2 デコード失敗で `Error::WoffDecode` が返る
- [ ] 既存 TTF/OTF ユーザーに破壊的変更なし（シグネチャ・戻り型不変）
- [ ] Unit test (detect_font_format, passthrough, error paths)
- [ ] Integration test: WOFF2 → PDF生成成功
- [ ] cargo clippy / cargo fmt --check 通過
