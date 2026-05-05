# MCP Server Design

## Overview

`fulgur mcp` starts a local MCP server over stdio, exposing fulgur's full
feature set as tools to AI agents (Claude Desktop, local agent workflows, etc.).

HTTP/SSE transport with multi-tenant support is out of scope for OSS and is
planned as a commercial edition offering.

## Scope

**OSS (this document):**

- `fulgur mcp` — stdio transport, single-user local process
- Full tool suite: render, render_template, inspect, redact, batch_render

**Commercial (out of scope here):**

- HTTP/SSE transport
- Multi-tenant asset bundle management
- Per-tenant quota / rate limiting
- Audit logging

## CLI

```bash
fulgur mcp
# Starts an MCP server on stdio. Claude Desktop or any MCP-compatible client
# can connect by launching this process.
```

Example Claude Desktop config:

```json
{
  "mcpServers": {
    "fulgur": {
      "command": "fulgur",
      "args": ["mcp"]
    }
  }
}
```

## Tools

### `render`

Convert HTML to PDF.

```json
{
  "name": "render",
  "input": {
    "html": "string",
    "base_path": "string (optional) — directory for resolving local assets",
    "page_size": "A4 | Letter | ... (optional)",
    "landscape": "bool (optional)",
    "margin_top": "string (optional, e.g. '20mm')",
    "margin_right": "string (optional)",
    "margin_bottom": "string (optional)",
    "margin_left": "string (optional)"
  },
  "output": {
    "pdf_path": "string — absolute path where the PDF was written"
  }
}
```

### `render_template`

Render a Fulgur template with JSON data.

```json
{
  "name": "render_template",
  "input": {
    "template_path": "string — path to .html template",
    "data": "object — JSON data to inject",
    "base_path": "string (optional)"
  },
  "output": {
    "pdf_path": "string"
  }
}
```

### `inspect`

Extract text, images, and metadata from a PDF.

```json
{
  "name": "inspect",
  "input": {
    "pdf_path": "string"
  },
  "output": {
    "pages": "number",
    "metadata": { "title": "...", "author": "...", "created_at": "..." },
    "text_items": [{ "page": 1, "x": 0, "y": 0, "width": 0, "height": 0, "text": "..." }],
    "images": [{ "page": 1, "x": 0, "y": 0, "width": 0, "height": 0, "format": "jpeg" }]
  }
}
```

### `redact`

Permanently remove content from a PDF.

```json
{
  "name": "redact",
  "input": {
    "pdf_path": "string",
    "output_path": "string",
    "manifest_path": "string (optional) — from inspect or render --manifest",
    "text": ["string or /regex/ (repeatable)"],
    "selectors": ["CSS selector (repeatable, requires manifest)"],
    "regions": [{ "page": 1, "x": 0, "y": 0, "width": 0, "height": 0, "type": "vector|image" }],
    "verify": "bool (optional, default false)"
  },
  "output": {
    "output_path": "string",
    "redacted_count": "number — regions actually removed"
  }
}
```

### `batch_render`

Render multiple HTML documents in parallel.

```json
{
  "name": "batch_render",
  "input": {
    "jobs": [
      { "html": "string", "base_path": "string (optional)", "output_path": "string" }
    ],
    "page_size": "string (optional, applied to all jobs)",
    "landscape": "bool (optional)"
  },
  "output": {
    "results": [{ "output_path": "string", "ok": true }]
  }
}
```

## Architecture

```
fulgur mcp
  └─ stdio transport (rmcp crate)
       └─ MCP request router
            ├─ render        → Engine::builder()...render_html()
            ├─ render_template → template engine
            ├─ inspect       → lopdf content stream parser
            ├─ redact        → redact module (crates/fulgur)
            └─ batch_render  → rayon parallel render
```

All PDF operations are delegated to `crates/fulgur` as a library. The MCP
server layer in `crates/fulgur-cli` is thin: parse tool inputs, call the
library, serialize outputs.

## Implementation Notes

- MCP SDK: `rmcp` (official Rust MCP SDK)
- Output files: written to paths specified by the caller; the server does not
  manage a temporary directory
- Errors: returned as MCP error responses with a human-readable message
- The server is single-threaded for simplicity; `batch_render` uses rayon
  internally for parallelism within a single tool call
