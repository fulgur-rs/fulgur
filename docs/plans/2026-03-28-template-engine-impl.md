# Template Engine Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add MiniJinja template engine support so users can render templates + JSON data to PDF.

**Architecture:** Template rendering is a pre-processing step before the existing HTML pipeline. A thin `template.rs` module wraps MiniJinja, `Engine` gains `template()`/`data()`/`render()` methods, and the CLI adds a `--data` flag.

**Tech Stack:** minijinja, serde_json, clap

---

### Task 1: Add dependencies

**Files:**

- Modify: `crates/fulgur/Cargo.toml`
- Modify: `crates/fulgur-cli/Cargo.toml`

**Step 1: Add minijinja and serde_json to fulgur crate**

In `crates/fulgur/Cargo.toml`, add to `[dependencies]`:

```toml
minijinja = "2"
serde_json = "1"
```

**Step 2: Add serde_json to fulgur-cli crate**

In `crates/fulgur-cli/Cargo.toml`, add to `[dependencies]`:

```toml
serde_json = "1"
```

**Step 3: Verify it compiles**

Run: `cargo build -p fulgur -p fulgur-cli`
Expected: compiles successfully

**Step 4: Commit**

```bash
git add crates/fulgur/Cargo.toml crates/fulgur-cli/Cargo.toml Cargo.lock
git commit -m "feat: add minijinja and serde_json dependencies for template engine"
```

---

### Task 2: Add Template error variant

**Files:**

- Modify: `crates/fulgur/src/error.rs`

**Step 1: Write the test**

Add to `crates/fulgur/src/error.rs` (new `#[cfg(test)]` module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_error_display() {
        let err = Error::Template("syntax error at line 3".into());
        assert!(err.to_string().contains("syntax error at line 3"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p fulgur test_template_error_display`
Expected: FAIL — no `Template` variant

**Step 3: Add the Template variant**

In `error.rs`, add to the `Error` enum:

```rust
#[error("Template error: {0}")]
Template(String),
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p fulgur test_template_error_display`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/fulgur/src/error.rs
git commit -m "feat: add Template error variant"
```

---

### Task 3: Create template.rs with render_template function

**Files:**

- Create: `crates/fulgur/src/template.rs`
- Modify: `crates/fulgur/src/lib.rs`

**Step 1: Write the tests**

Create `crates/fulgur/src/template.rs` with tests only:

```rust
use crate::error::{Error, Result};

/// Render a MiniJinja template with JSON data.
pub fn render_template(
    _name: &str,
    _template_str: &str,
    _data: &serde_json::Value,
) -> Result<String> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_variable_substitution() {
        let tmpl = "<h1>{{ title }}</h1>";
        let data = json!({"title": "Hello"});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "<h1>Hello</h1>");
    }

    #[test]
    fn test_loop() {
        let tmpl = "{% for item in items %}<li>{{ item }}</li>{% endfor %}";
        let data = json!({"items": ["a", "b"]});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "<li>a</li><li>b</li>");
    }

    #[test]
    fn test_conditional() {
        let tmpl = "{% if show %}yes{% else %}no{% endif %}";
        let data = json!({"show": true});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "yes");
    }

    #[test]
    fn test_filter() {
        let tmpl = "{{ name | upper }}";
        let data = json!({"name": "hello"});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "HELLO");
    }

    #[test]
    fn test_syntax_error() {
        let tmpl = "{% if %}";
        let data = json!({});
        let result = render_template("test.html", tmpl, &data);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Template error"));
    }

    #[test]
    fn test_empty_data() {
        let tmpl = "<p>static</p>";
        let data = json!({});
        let result = render_template("test.html", tmpl, &data).unwrap();
        assert_eq!(result, "<p>static</p>");
    }

    #[test]
    fn test_html_autoescaping() {
        let tmpl = "{{ text }}";
        let data = json!({"text": "<script>alert(1)</script>"});
        let result = render_template("test.html", tmpl, &data).unwrap();
        // MiniJinja auto-escapes HTML by default for .html templates
        assert!(!result.contains("<script>"));
    }
}
```

**Step 2: Add module to lib.rs**

In `crates/fulgur/src/lib.rs`, add:

```rust
pub mod template;
```

**Step 3: Run tests to verify they fail**

Run: `cargo test -p fulgur template`
Expected: FAIL — `todo!()` panics

**Step 4: Implement render_template**

Replace the `todo!()` function body in `template.rs`:

```rust
pub fn render_template(
    name: &str,
    template_str: &str,
    data: &serde_json::Value,
) -> Result<String> {
    let mut env = minijinja::Environment::new();
    env.add_template(name, template_str)
        .map_err(|e| Error::Template(e.to_string()))?;
    let tmpl = env
        .get_template(name)
        .map_err(|e| Error::Template(e.to_string()))?;
    tmpl.render(data)
        .map_err(|e| Error::Template(e.to_string()))
}
```

**Step 5: Run tests to verify they pass**

Run: `cargo test -p fulgur template`
Expected: all 7 tests PASS

**Step 6: Commit**

```bash
git add crates/fulgur/src/template.rs crates/fulgur/src/lib.rs
git commit -m "feat: add template.rs with MiniJinja render_template function"
```

---

### Task 4: Add template/data fields to Engine and EngineBuilder

**Files:**

- Modify: `crates/fulgur/src/engine.rs`

**Step 1: Write the tests**

Add to the existing `mod tests` in `engine.rs`:

```rust
#[test]
fn test_engine_render_template() {
    let engine = Engine::builder()
        .template("test.html", "<h1>{{ title }}</h1>")
        .data(serde_json::json!({"title": "Hello"}))
        .build();
    let result = engine.render();
    assert!(result.is_ok());
}

#[test]
fn test_engine_render_without_template_errors() {
    let engine = Engine::builder().build();
    let result = engine.render();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Template"));
}

#[test]
fn test_engine_render_without_data_uses_empty_object() {
    let engine = Engine::builder()
        .template("test.html", "<p>static</p>")
        .build();
    let result = engine.render();
    assert!(result.is_ok());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p fulgur test_engine_render`
Expected: FAIL — no `template()`, `data()`, `render()` methods

**Step 3: Add template/data to Engine and EngineBuilder**

In `engine.rs`, update `Engine` struct:

```rust
pub struct Engine {
    config: Config,
    assets: Option<AssetBundle>,
    base_path: Option<PathBuf>,
    template_name: Option<String>,
    template_str: Option<String>,
    data: Option<serde_json::Value>,
}
```

Add `render()` method to `impl Engine`:

```rust
/// Render a template with data to PDF bytes.
/// The template is expanded via MiniJinja, then passed to render_html().
/// Returns an error if no template was set via the builder.
pub fn render(&self) -> Result<Vec<u8>> {
    let template_str = self
        .template_str
        .as_deref()
        .ok_or_else(|| crate::error::Error::Template("no template set".into()))?;
    let template_name = self
        .template_name
        .as_deref()
        .unwrap_or("template.html");
    let empty = serde_json::Value::Object(serde_json::Map::new());
    let data = self.data.as_ref().unwrap_or(&empty);
    let html = crate::template::render_template(template_name, template_str, data)?;
    self.render_html(&html)
}
```

Update `EngineBuilder` struct:

```rust
pub struct EngineBuilder {
    config_builder: ConfigBuilder,
    assets: Option<AssetBundle>,
    base_path: Option<PathBuf>,
    template_name: Option<String>,
    template_str: Option<String>,
    data: Option<serde_json::Value>,
}
```

Add builder methods to `impl EngineBuilder`:

```rust
pub fn template(mut self, name: impl Into<String>, template: impl Into<String>) -> Self {
    self.template_name = Some(name.into());
    self.template_str = Some(template.into());
    self
}

pub fn data(mut self, data: serde_json::Value) -> Self {
    self.data = Some(data);
    self
}
```

Update `Engine::builder()` to initialize the new fields:

```rust
pub fn builder() -> EngineBuilder {
    EngineBuilder {
        config_builder: Config::builder(),
        assets: None,
        base_path: None,
        template_name: None,
        template_str: None,
        data: None,
    }
}
```

Update `EngineBuilder::build()`:

```rust
pub fn build(self) -> Engine {
    Engine {
        config: self.config_builder.build(),
        assets: self.assets,
        base_path: self.base_path,
        template_name: self.template_name,
        template_str: self.template_str,
        data: self.data,
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p fulgur test_engine_render`
Expected: all 3 tests PASS

**Step 5: Run all tests to check for regressions**

Run: `cargo test -p fulgur --lib`
Expected: all existing tests still PASS

**Step 6: Commit**

```bash
git add crates/fulgur/src/engine.rs
git commit -m "feat: add template/data support to Engine and EngineBuilder"
```

---

### Task 5: Add --data flag to CLI

**Files:**

- Modify: `crates/fulgur-cli/src/main.rs`

**Step 1: Add --data arg to Render variant**

In the `Commands::Render` struct, add after the `images` field:

```rust
/// JSON data file for template mode (use "-" for stdin)
#[arg(long = "data", short = 'd')]
data: Option<PathBuf>,
```

**Step 2: Add data to destructure pattern**

In the `match cli.command` arm, add `data` to the destructured fields.

**Step 3: Implement template mode logic**

After the `let html = ...` block and before the `let assets = ...` block, add the template rendering logic. Replace the section that reads HTML and renders with:

```rust
// Read input file content
let input_content = if stdin {
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .expect("Failed to read stdin");
    buf
} else if let Some(ref input) = input {
    std::fs::read_to_string(input).unwrap_or_else(|e| {
        eprintln!("Error reading {}: {e}", input.display());
        std::process::exit(1);
    })
} else {
    eprintln!("Error: provide an input file or use --stdin");
    std::process::exit(1);
};

// If --data is provided, treat input as template
let (html, template_name, template_data) = if let Some(ref data_path) = data {
    let json_str = if data_path.as_os_str() == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
            .expect("Failed to read JSON from stdin");
        buf
    } else {
        std::fs::read_to_string(data_path).unwrap_or_else(|e| {
            eprintln!("Error reading data file {}: {e}", data_path.display());
            std::process::exit(1);
        })
    };
    let json_data: serde_json::Value = serde_json::from_str(&json_str).unwrap_or_else(|e| {
        eprintln!("Error parsing JSON: {e}");
        std::process::exit(1);
    });
    let name = input
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("template.html")
        .to_string();
    (None, Some(name), Some(json_data))
} else {
    (Some(input_content.clone()), None, None)
};
```

Note: when `--data` is provided, we pass template via `Engine::template()` + `Engine::data()` instead of `render_html()`. When `--data` is not provided, we use `render_html()` as before.

**Step 4: Update engine building and rendering**

Replace the rendering section at the bottom of main (after `let engine = builder.build();`) with:

```rust
let engine = builder.build();

let render_result = if template_name.is_some() {
    // Template mode: use engine.render()
    engine.render()
} else {
    // HTML mode: use engine.render_html()
    engine.render_html(html.as_ref().unwrap())
};
```

And add `template()`/`data()` calls to the builder:

```rust
if let Some(ref name) = template_name {
    builder = builder.template(name, &input_content);
}
if let Some(data) = template_data {
    builder = builder.data(data);
}
```

**Step 5: Handle stdin conflict (--data - and --stdin cannot both read stdin)**

Add validation near the start:

```rust
if stdin && data.as_ref().map_or(false, |p| p.as_os_str() == "-") {
    eprintln!("Error: cannot use --stdin and --data - together (both read stdin)");
    std::process::exit(1);
}
```

**Step 6: Verify it compiles and runs**

Run: `cargo build -p fulgur-cli`
Expected: compiles

Run: `cargo run --bin fulgur -- render --help`
Expected: shows `--data` in help output

**Step 7: Commit**

```bash
git add crates/fulgur-cli/src/main.rs
git commit -m "feat: add --data flag for template mode in CLI"
```

---

### Task 6: Integration test — template + JSON to PDF

**Files:**

- Modify: `crates/fulgur/tests/` (add template integration test)

**Step 1: Write the integration test**

Create `crates/fulgur/tests/template_integration.rs`:

```rust
use fulgur::engine::Engine;
use serde_json::json;

#[test]
fn test_template_to_pdf() {
    let template = r#"<html><body><h1>{{ title }}</h1>
{% for item in items %}<p>{{ item }}</p>{% endfor %}
</body></html>"#;
    let data = json!({
        "title": "Invoice",
        "items": ["Item A", "Item B"]
    });

    let pdf = Engine::builder()
        .template("invoice.html", template)
        .data(data)
        .build()
        .render()
        .unwrap();

    assert!(!pdf.is_empty());
    // PDF magic bytes
    assert_eq!(&pdf[..5], b"%PDF-");
}

#[test]
fn test_html_mode_still_works() {
    let html = "<html><body><p>Hello</p></body></html>";
    let pdf = Engine::builder().build().render_html(html).unwrap();
    assert!(!pdf.is_empty());
    assert_eq!(&pdf[..5], b"%PDF-");
}

#[test]
fn test_template_with_assets() {
    let mut assets = fulgur::asset::AssetBundle::new();
    assets.add_css("p { color: red; }");

    let template = "<html><body><p>{{ text }}</p></body></html>";
    let data = json!({"text": "styled"});

    let pdf = Engine::builder()
        .template("test.html", template)
        .data(data)
        .assets(assets)
        .build()
        .render()
        .unwrap();

    assert!(!pdf.is_empty());
    assert_eq!(&pdf[..5], b"%PDF-");
}
```

**Step 2: Run integration tests**

Run: `cargo test -p fulgur --test template_integration -- --test-threads=1`
Expected: all 3 tests PASS

**Step 3: Commit**

```bash
git add crates/fulgur/tests/template_integration.rs
git commit -m "test: add template engine integration tests"
```

---

### Task 7: Add example

**Files:**

- Create: `examples/template/template.html`
- Create: `examples/template/data.json`

**Step 1: Create template example**

`examples/template/template.html`:

```html
<html>
<head>
  <style>
    body { font-family: sans-serif; margin: 40px; }
    h1 { color: #333; }
    table { border-collapse: collapse; width: 100%; }
    th, td { border: 1px solid #ccc; padding: 8px; text-align: left; }
    th { background: #f5f5f5; }
  </style>
</head>
<body>
  <h1>{{ title }}</h1>
  <p>Date: {{ date }}</p>
  <table>
    <tr><th>Item</th><th>Qty</th><th>Price</th></tr>
    {% for row in items %}
    <tr><td>{{ row.name }}</td><td>{{ row.qty }}</td><td>{{ row.price }}</td></tr>
    {% endfor %}
  </table>
</body>
</html>
```

`examples/template/data.json`:

```json
{
  "title": "Invoice #001",
  "date": "2026-03-28",
  "items": [
    {"name": "Widget A", "qty": 2, "price": "$10.00"},
    {"name": "Widget B", "qty": 1, "price": "$25.00"}
  ]
}
```

**Step 2: Verify it works end-to-end**

Run: `cargo run --bin fulgur -- render examples/template/template.html --data examples/template/data.json -o examples/template/output.pdf`
Expected: PDF generated successfully

**Step 3: Commit**

```bash
git add examples/template/template.html examples/template/data.json
git commit -m "docs: add template engine example with invoice template"
```

---

### Task 8: Final checks

**Step 1: Run all tests**

Run: `cargo test --lib -p fulgur`
Run: `cargo test -p fulgur --test template_integration -- --test-threads=1`
Expected: all PASS

**Step 2: Clippy**

Run: `cargo clippy`
Expected: no warnings

**Step 3: Format check**

Run: `cargo fmt --check`
Expected: no changes needed

**Step 4: Markdown lint (if docs changed)**

Run: `npx markdownlint-cli2 'docs/**/*.md'`
Expected: no errors
