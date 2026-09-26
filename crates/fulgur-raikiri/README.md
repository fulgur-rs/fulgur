# fulgur-raikiri

Unpublished Raikiri backend for Fulgur development.

The backend accepts an HTML file path and returns `Result<Vec<u8>>`, whose
successful value will contain PDF bytes. It currently reads, parses, and lays
out the document, then returns a PDF generation error:
`Raikiri PDF drawing is not implemented`.

PDF drawing will be added later. The backend does not fall back to Blitz or
return an empty PDF. Styles must be embedded in the HTML; external resource
loading and AssetBundle integration are not connected yet.

```rust
let result = fulgur_raikiri::render(std::path::Path::new("input.html"));
```

Raikiri is a Git dependency pinned to a specific revision. This crate is not
used by Fulgur's published facade, CLI, or bindings.
