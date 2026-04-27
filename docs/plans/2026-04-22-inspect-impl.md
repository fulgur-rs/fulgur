# fulgur inspect Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `fulgur inspect <input.pdf>` — 任意のPDFからテキスト位置・埋め込み画像・メタデータをJSONで抽出するCLIサブコマンドを実装する。

**Architecture:** `crates/fulgur/src/inspect.rs` にコアロジック（lopdf でPDFパース）を実装し、`crates/fulgur-cli/src/main.rs` に `Inspect` サブコマンドを追加する。テキスト位置はコンテンツストリームのテキスト行列演算子（Tm/Td/TD）を追跡して抽出。座標系はPDF標準（左下原点、ポイント単位）。

**Tech Stack:** Rust, lopdf 0.40.0 (PDFパース), serde_json (JSON直列化), clap (CLI)

---

## Task 1: lopdf 依存関係の追加

**Files:**
- Modify: `crates/fulgur/Cargo.toml`

**Step 1: Cargo.toml に lopdf を追加**

`[dependencies]` セクションの末尾に以下を追加:

```toml
lopdf = "0.40.0"
serde = { version = "1", features = ["derive"] }
```

**Step 2: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | tail -5
```

期待: `Finished` で終了、エラーなし

**Step 3: コミット**

```bash
git add crates/fulgur/Cargo.toml Cargo.lock
git commit -m "chore(fulgur): add lopdf and serde dependencies for inspect"
```

---

## Task 2: inspect モジュール — データ構造と公開API

**Files:**
- Create: `crates/fulgur/src/inspect.rs`
- Modify: `crates/fulgur/src/lib.rs`

**Step 1: テスト用フィクスチャ関数を含む inspect.rs を作成**

`crates/fulgur/src/inspect.rs`:

```rust
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize, PartialEq)]
pub struct InspectResult {
    pub pages: u32,
    pub metadata: Metadata,
    pub text_items: Vec<TextItem>,
    pub images: Vec<ImageItem>,
}

#[derive(Debug, Serialize, PartialEq, Default)]
pub struct Metadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TextItem {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub text: String,
    pub font: String,
    pub font_size: f32,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ImageItem {
    pub page: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub format: String,
    pub width_px: u32,
    pub height_px: u32,
}

pub fn inspect(path: &Path) -> crate::Result<InspectResult> {
    let doc = lopdf::Document::load(path).map_err(|e| {
        crate::Error::Other(format!("Failed to load PDF: {e}"))
    })?;

    let pages = doc.get_pages().len() as u32;
    let metadata = extract_metadata(&doc);
    let text_items = extract_text_items(&doc)?;
    let images = extract_image_items(&doc)?;

    Ok(InspectResult { pages, metadata, text_items, images })
}

fn extract_metadata(doc: &lopdf::Document) -> Metadata {
    let mut meta = Metadata::default();
    let info_id = match doc.trailer.get(b"Info") {
        Ok(obj) => match obj.as_reference() {
            Ok(id) => id,
            Err(_) => return meta,
        },
        Err(_) => return meta,
    };
    let info = match doc.get_object(info_id) {
        Ok(lopdf::Object::Dictionary(d)) => d,
        _ => return meta,
    };

    let get_str = |dict: &lopdf::Dictionary, key: &[u8]| -> Option<String> {
        dict.get(key).ok()
            .and_then(|o| o.as_str().ok())
            .map(|b| String::from_utf8_lossy(b).into_owned())
    };

    meta.title = get_str(info, b"Title");
    meta.author = get_str(info, b"Author");
    meta.creator = get_str(info, b"Creator");
    meta.created_at = get_str(info, b"CreationDate");
    meta.modified_at = get_str(info, b"ModDate");
    meta
}

fn extract_text_items(doc: &lopdf::Document) -> crate::Result<Vec<TextItem>> {
    use lopdf::content::Operation;
    let mut items = Vec::new();

    for (&page_num, &page_id) in &doc.get_pages() {
        let content_bytes = match doc.get_page_content(page_id) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let content = match lopdf::content::Content::decode(&content_bytes) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Text rendering state machine
        let mut tx: f32 = 0.0; // current text x
        let mut ty: f32 = 0.0; // current text y
        let mut font_name = String::from("unknown");
        let mut font_size: f32 = 12.0;

        for Operation { operator, operands } in &content.operations {
            match operator.as_str() {
                "Tf" => {
                    if let (Some(name), Some(size)) = (operands.first(), operands.get(1)) {
                        font_name = name.as_name_str().unwrap_or("unknown").to_string();
                        font_size = obj_to_f32(size);
                    }
                }
                "Tm" => {
                    // Tm a b c d e f — sets text matrix
                    if operands.len() >= 6 {
                        tx = obj_to_f32(&operands[4]);
                        ty = obj_to_f32(&operands[5]);
                    }
                }
                "Td" | "TD" => {
                    if operands.len() >= 2 {
                        tx += obj_to_f32(&operands[0]);
                        ty += obj_to_f32(&operands[1]);
                    }
                }
                "T*" => {
                    // move to next line — approximation: ty decrements by font_size
                    ty -= font_size;
                }
                "Tj" => {
                    if let Some(text_obj) = operands.first() {
                        if let Ok(bytes) = text_obj.as_str() {
                            let text = decode_pdf_string(bytes);
                            if !text.trim().is_empty() {
                                let w = estimate_width(&text, font_size);
                                items.push(TextItem {
                                    page: page_num,
                                    x: tx,
                                    y: ty,
                                    width: w,
                                    height: font_size,
                                    text,
                                    font: font_name.clone(),
                                    font_size,
                                });
                                tx += w;
                            }
                        }
                    }
                }
                "TJ" => {
                    if let Some(array_obj) = operands.first() {
                        if let Ok(array) = array_obj.as_array() {
                            let mut combined = String::new();
                            for elem in array {
                                if let Ok(bytes) = elem.as_str() {
                                    combined.push_str(&decode_pdf_string(bytes));
                                }
                                // kerning numbers (negative = tighter) — ignored for simplicity
                            }
                            if !combined.trim().is_empty() {
                                let w = estimate_width(&combined, font_size);
                                items.push(TextItem {
                                    page: page_num,
                                    x: tx,
                                    y: ty,
                                    width: w,
                                    height: font_size,
                                    text: combined,
                                    font: font_name.clone(),
                                    font_size,
                                });
                                tx += w;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(items)
}

fn extract_image_items(doc: &lopdf::Document) -> crate::Result<Vec<ImageItem>> {
    let mut items = Vec::new();

    for (&page_num, &page_id) in &doc.get_pages() {
        let page_obj = match doc.get_object(page_id) {
            Ok(lopdf::Object::Dictionary(d)) => d.clone(),
            _ => continue,
        };

        // Get Resources > XObject dictionary
        let resources = match page_obj.get(b"Resources") {
            Ok(res) => {
                let resolved = doc.dereference(res).map(|(_, o)| o);
                match resolved {
                    Ok(lopdf::Object::Dictionary(d)) => d.clone(),
                    _ => continue,
                }
            }
            Err(_) => continue,
        };

        let xobjects = match resources.get(b"XObject") {
            Ok(xo) => {
                let resolved = doc.dereference(xo).map(|(_, o)| o);
                match resolved {
                    Ok(lopdf::Object::Dictionary(d)) => d.clone(),
                    _ => continue,
                }
            }
            Err(_) => continue,
        };

        // Collect image XObject names
        let mut image_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (name, obj_ref) in xobjects.iter() {
            let xobj = match doc.dereference(obj_ref).map(|(_, o)| o) {
                Ok(lopdf::Object::Stream(s)) => s,
                _ => continue,
            };
            let subtype = xobj.dict.get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name_str().ok())
                .unwrap_or("");
            if subtype == "Image" {
                let fmt = detect_image_format(&xobj.dict);
                let w_px = xobj.dict.get(b"Width").ok()
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(0) as u32;
                let h_px = xobj.dict.get(b"Height").ok()
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(0) as u32;
                let name_str = String::from_utf8_lossy(name).into_owned();
                image_names.insert(name_str.clone());
                // Placeholder — position extracted from Do operators below
                items.push(ImageItem {
                    page: page_num,
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                    format: fmt,
                    width_px: w_px,
                    height_px: h_px,
                });
                let _ = name_str;
            }
        }

        // Update positions from Do operators in content stream
        if image_names.is_empty() {
            continue;
        }
        let content_bytes = match doc.get_page_content(page_id) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let content = match lopdf::content::Content::decode(&content_bytes) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Track CTM (current transformation matrix) — simplified: only cm operator
        let mut ctm = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0]; // a b c d e f
        for op in &content.operations {
            match op.operator.as_str() {
                "cm" if op.operands.len() == 6 => {
                    ctm = [
                        obj_to_f32(&op.operands[0]),
                        obj_to_f32(&op.operands[1]),
                        obj_to_f32(&op.operands[2]),
                        obj_to_f32(&op.operands[3]),
                        obj_to_f32(&op.operands[4]),
                        obj_to_f32(&op.operands[5]),
                    ];
                }
                "Do" => {
                    if let Some(name_obj) = op.operands.first() {
                        let name = name_obj.as_name_str().unwrap_or("");
                        if image_names.contains(name) {
                            // CTM maps unit square to page coords:
                            // width = |a|, height = |d|, x = e, y = f
                            for item in items.iter_mut().filter(|i| i.page == page_num) {
                                if item.x == 0.0 && item.y == 0.0 {
                                    item.x = ctm[4];
                                    item.y = ctm[5];
                                    item.width = ctm[0].abs();
                                    item.height = ctm[3].abs();
                                    break;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(items)
}

fn obj_to_f32(obj: &lopdf::Object) -> f32 {
    match obj {
        lopdf::Object::Integer(i) => *i as f32,
        lopdf::Object::Real(f) => *f as f32,
        _ => 0.0,
    }
}

fn decode_pdf_string(bytes: &[u8]) -> String {
    // Try UTF-16 BE (BOM FF FE or FE FF)
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let chars: Vec<u16> = bytes[2..].chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&chars);
    }
    // PDFDocEncoding fallback — treat as latin-1
    bytes.iter().map(|&b| b as char).collect()
}

fn estimate_width(text: &str, font_size: f32) -> f32 {
    // Rough estimate: 0.5 × font_size per character (ASCII heuristic)
    text.chars().count() as f32 * font_size * 0.5
}

fn detect_image_format(dict: &lopdf::Dictionary) -> String {
    // Check Filter to determine format
    if let Ok(filter) = dict.get(b"Filter") {
        let name = match filter {
            lopdf::Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
            lopdf::Object::Array(arr) => arr.last()
                .and_then(|o| o.as_name_str().ok())
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        };
        match name.as_str() {
            "DCTDecode" => return "jpeg".to_string(),
            "JPXDecode" => return "jp2".to_string(),
            "CCITTFaxDecode" => return "tiff".to_string(),
            "FlateDecode" => return "png".to_string(),
            _ => {}
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_test_pdf(html: &str) -> Vec<u8> {
        fulgur::engine::Engine::builder().build().render_html(html).unwrap()
    }

    fn inspect_bytes(bytes: &[u8]) -> InspectResult {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), bytes).unwrap();
        inspect(tmp.path()).unwrap()
    }

    #[test]
    fn inspect_page_count() {
        // A simple single-page HTML
        let pdf = render_test_pdf("<html><body><p>Hello</p></body></html>");
        let result = inspect_bytes(&pdf);
        assert_eq!(result.pages, 1);
    }

    #[test]
    fn inspect_metadata_title() {
        let pdf = fulgur::engine::Engine::builder()
            .title("Test Title".to_string())
            .build()
            .render_html("<html><body><p>Hi</p></body></html>")
            .unwrap();
        let result = inspect_bytes(&pdf);
        assert_eq!(result.metadata.title.as_deref(), Some("Test Title"));
    }

    #[test]
    fn inspect_text_items_non_empty() {
        let pdf = render_test_pdf("<html><body><p>Hello World</p></body></html>");
        let result = inspect_bytes(&pdf);
        // Should find at least one text item
        assert!(!result.text_items.is_empty(), "expected text items");
    }

    #[test]
    fn inspect_text_item_fields() {
        let pdf = render_test_pdf("<html><body><p>Hello</p></body></html>");
        let result = inspect_bytes(&pdf);
        if let Some(item) = result.text_items.first() {
            assert!(item.page >= 1, "page must be >= 1");
            assert!(item.font_size > 0.0, "font_size must be positive");
            assert!(!item.text.is_empty(), "text must not be empty");
        }
    }

    #[test]
    fn inspect_result_serializes_to_json() {
        let pdf = render_test_pdf("<html><body><p>Test</p></body></html>");
        let result = inspect_bytes(&pdf);
        let json = serde_json::to_string_pretty(&result).unwrap();
        assert!(json.contains("\"pages\""));
        assert!(json.contains("\"metadata\""));
        assert!(json.contains("\"text_items\""));
        assert!(json.contains("\"images\""));
    }
}
```

**Step 2: テストを実行して失敗を確認**

```bash
cargo test -p fulgur inspect 2>&1 | tail -20
```

期待: `error[E0433]: failed to resolve: use of undeclared crate or module \`lopdf\`` か compile error

**Step 3: lib.rs に inspect モジュールを追加**

`crates/fulgur/src/lib.rs` の最後の `pub mod` 行の後に追加:

```rust
pub mod inspect;
```

また `Error::Other` バリアントが `crate::Error` に存在することを確認。存在しない場合は `error.rs` を確認して適切なバリアントを使う（後述 Task 4 を参照）。

**Step 4: テストを実行（まだ失敗するはず — lopdf が `serde` なし）**

```bash
cargo test -p fulgur inspect 2>&1 | tail -20
```

**Step 5: コミット（データ構造のみ、まだコンパイル通らなくてよい）**

スキップ — コンパイルが通ってから Task 3 と合わせてコミット

---

## Task 3: Error バリアント確認と inspect.rs コンパイル通し

**Files:**
- Read: `crates/fulgur/src/error.rs`

**Step 1: error.rs を確認**

```bash
cat crates/fulgur/src/error.rs
```

`Error::Other(String)` バリアントが存在しない場合、inspect.rs の `crate::Error::Other(...)` を既存の最も近いバリアントに変更する。または `error.rs` に追加:

```rust
#[error("{0}")]
Other(String),
```

**Step 2: ビルド確認**

```bash
cargo build -p fulgur 2>&1 | grep -E "^error" | head -20
```

エラーが残っていれば修正。

**Step 3: テスト実行**

```bash
cargo test -p fulgur inspect 2>&1 | tail -20
```

期待: 4 tests pass（PDFのレンダリングを使うので時間がかかる場合あり）

**Step 4: コミット**

```bash
git add crates/fulgur/src/inspect.rs crates/fulgur/src/lib.rs crates/fulgur/src/error.rs
git commit -m "feat(fulgur): add inspect module — extract text, images, metadata from PDF"
```

---

## Task 4: fulgur-cli に `inspect` サブコマンドを追加

**Files:**
- Modify: `crates/fulgur-cli/Cargo.toml`
- Modify: `crates/fulgur-cli/src/main.rs`

**Step 1: fulgur-cli の Cargo.toml に lopdf を追加（エラー出力のみ）**

実際には不要（fulgur クレートが lopdf を内包するため）。ただし serde_json は既存の依存関係として存在する。

**Step 2: Commands enum に Inspect バリアントを追加**

`main.rs` の `Commands` enum に追加:

```rust
/// Inspect a PDF and extract text positions, images, and metadata as JSON
Inspect {
    /// Input PDF file
    #[arg()]
    input: PathBuf,

    /// Output JSON file (default: stdout)
    #[arg(short, long)]
    output: Option<PathBuf>,
},
```

**Step 3: main() の match に Inspect ハンドラを追加**

`main.rs` の `Commands::Template { .. }` の前に追加:

```rust
Commands::Inspect { input, output } => {
    let result = fulgur::inspect::inspect(&input).unwrap_or_else(|e| {
        eprintln!("Error: {e}");
        std::process::exit(1);
    });
    let json = serde_json::to_string_pretty(&result).unwrap_or_else(|e| {
        eprintln!("Error serializing JSON: {e}");
        std::process::exit(1);
    });
    if let Some(ref output_path) = output {
        std::fs::write(output_path, &json).unwrap_or_else(|e| {
            eprintln!("Error writing to {}: {e}", output_path.display());
            std::process::exit(1);
        });
        eprintln!("Manifest written to {}", output_path.display());
    } else {
        println!("{json}");
    }
},
```

**Step 4: ビルド確認**

```bash
cargo build -p fulgur-cli 2>&1 | tail -5
```

期待: `Finished` で終了

**Step 5: ヘルプ表示確認**

```bash
cargo run --bin fulgur -- inspect --help
```

期待: `inspect <INPUT>` と `-o, --output` が表示される

**Step 6: コミット**

```bash
git add crates/fulgur-cli/src/main.rs
git commit -m "feat(fulgur-cli): add inspect subcommand"
```

---

## Task 5: CLI integration test — スモークテスト

**Files:**
- Modify: `crates/fulgur-cli/src/main.rs` または `crates/fulgur-cli/tests/inspect_test.rs`

**Step 1: 統合テスト用ファイルを作成**

`crates/fulgur-cli/tests/inspect_test.rs`:

```rust
use std::process::Command;

fn fulgur_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // remove test binary name
    if p.ends_with("deps") { p.pop(); }
    p.push("fulgur");
    p
}

#[test]
fn inspect_outputs_valid_json() {
    let bin = fulgur_bin();
    if !bin.exists() {
        eprintln!("fulgur binary not found, skipping");
        return;
    }

    // Create a tiny test PDF using fulgur render
    let tmp_pdf = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let render_status = Command::new(&bin)
        .args(["render", "--stdin", "-o", tmp_pdf.path().to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.as_mut().unwrap().write_all(b"<html><body><p>Test</p></body></html>").unwrap();
            child.wait()
        });
    assert!(render_status.is_ok_and(|s| s.success()), "render failed");

    // Now inspect
    let output = Command::new(&bin)
        .args(["inspect", tmp_pdf.path().to_str().unwrap()])
        .output()
        .expect("failed to run fulgur inspect");

    assert!(output.status.success(), "exit code non-zero: {:?}", output.status);
    let json_str = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&json_str)
        .expect("output is not valid JSON");

    assert!(parsed["pages"].as_u64().unwrap_or(0) >= 1);
    assert!(parsed["metadata"].is_object());
    assert!(parsed["text_items"].is_array());
    assert!(parsed["images"].is_array());
}
```

**Step 2: テスト実行**

```bash
cargo test -p fulgur-cli 2>&1 | tail -15
```

期待: 全テスト PASS

**Step 3: ファイル出力モード手動確認**

```bash
cargo run --bin fulgur -- render --stdin -o /tmp/test_inspect.pdf <<< '<html><body><p>Hello fulgur</p></body></html>'
cargo run --bin fulgur -- inspect /tmp/test_inspect.pdf
```

期待: `pages`, `metadata`, `text_items`, `images` フィールドを含む JSON が stdout に出力される

**Step 4: コミット**

```bash
git add crates/fulgur-cli/tests/inspect_test.rs
git commit -m "test(fulgur-cli): add inspect subcommand integration test"
```

---

## Task 6: ドキュメントと最終確認

**Files:**
- Read: `CHANGELOG.md` (確認のみ)

**Step 1: 全テスト実行**

```bash
cargo test -p fulgur --lib 2>&1 | tail -5
cargo test -p fulgur-cli 2>&1 | tail -5
```

期待: 全テスト PASS

**Step 2: cargo clippy 確認**

```bash
cargo clippy -p fulgur -p fulgur-cli 2>&1 | grep -E "^error" | head -20
```

警告があれば修正、エラーは必ず修正。

**Step 3: 最終動作確認**

```bash
cargo run --bin fulgur -- render --stdin -o /tmp/final_test.pdf <<< '<html><body><h1>Invoice</h1><p>Amount: $100</p></body></html>'
cargo run --bin fulgur -- inspect /tmp/final_test.pdf | python3 -m json.tool
```

期待: 整形されたJSONが表示され、text_items に "Invoice" や "Amount" が含まれる（フォントエンコーディング依存）

**Step 4: コミット（クリーンアップがあれば）**

```bash
git add -p
git commit -m "chore: clippy fixes for inspect module"
```
