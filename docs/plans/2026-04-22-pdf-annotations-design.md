# PDF Annotations Design

## Overview

Two complementary annotation capabilities targeting AI agent workflows:

- **Render-time semantic annotations** — fulgur embeds DOM structure as standard
  PDF annotations during rendering so AI agents can read semantic information
  from the PDF without needing the original HTML
- **Post-processing annotations** — AI agents write standard PDF annotations
  (highlights, notes, region markers) to a PDF via `fulgur annotate`

Both use ISO 32000 standard PDF annotation dictionaries, making annotated PDFs
readable in Acrobat, Preview, and other standard viewers.

## fulgur annotate (AI writes)

### CLI

```bash
fulgur annotate input.pdf -o output.pdf --annotations annotations.json
```

### annotations.json

```json
[
  {
    "page": 1, "x": 100, "y": 200, "width": 150, "height": 30,
    "type": "Highlight",
    "color": "#FFFF00",
    "contents": "要確認: 金額が前月と異なる"
  },
  {
    "page": 2, "x": 50, "y": 100, "width": 200, "height": 40,
    "type": "Square",
    "contents": "signature-block"
  },
  {
    "page": 3, "x": 0, "y": 0, "width": 595, "height": 842,
    "type": "FreeText",
    "contents": "このページは削除候補"
  }
]
```

Supported annotation types: `Highlight`, `Square`, `Circle`, `FreeText`,
`Note`, `StrikeOut`.

### Implementation notes

- Library: `lopdf` — add annotation dictionaries to the page's `Annots` array
- Color: hex string converted to PDF DeviceRGB float triple
- Coordinate system: PDF user space (origin bottom-left); callers using
  `inspect` output can pass coordinates directly

## fulgur render --annotate (AI reads)

When `--annotate` is passed to `fulgur render`, fulgur embeds each DOM element's
semantic metadata as a transparent Square annotation. These are invisible to
human readers but readable by `fulgur inspect` and any PDF tool that enumerates
annotations.

```bash
fulgur render input.html -o out.pdf --annotate
```

### Annotation format

Each embedded annotation is a standard PDF Square annotation with:

- **Rect**: the element's bounding box in PDF user space
- **Contents**: JSON string with semantic metadata
- **C** (color): empty array (transparent, not drawn)
- **F** (flags): `Hidden` bit set so viewers don't display it
- **Subj**: `"fulgur:semantic"` — used by `inspect` to distinguish fulgur-
  generated annotations from user-added ones

Contents JSON schema:

```json
{
  "tag": "td",
  "id": null,
  "classes": ["personal-info"],
  "selector": "table.roster > tbody > tr:nth-child(3) > td.personal-info"
}
```

### inspect output

`fulgur inspect` is extended to return all annotations, split by source:

```json
{
  "annotations": [
    {
      "page": 1, "x": 90, "y": 195, "width": 200, "height": 20,
      "type": "Square",
      "contents": "{\"tag\":\"td\",\"classes\":[\"personal-info\"],\"selector\":\"...\"}",
      "source": "fulgur:semantic"
    },
    {
      "page": 1, "x": 100, "y": 200, "width": 150, "height": 30,
      "type": "Highlight",
      "contents": "要確認: 金額が前月と異なる",
      "source": "user"
    }
  ]
}
```

`source` values:
- `"fulgur:semantic"` — embedded by `fulgur render --annotate`
- `"user"` — added by `fulgur annotate` or any external tool

## MCP tools

### `annotate` (new tool)

```json
{
  "name": "annotate",
  "input": {
    "pdf_path": "string",
    "output_path": "string",
    "annotations": [
      {
        "page": "number",
        "x": "number", "y": "number",
        "width": "number", "height": "number",
        "type": "Highlight | Square | Circle | FreeText | Note | StrikeOut",
        "color": "string (optional, hex)",
        "contents": "string"
      }
    ]
  },
  "output": {
    "output_path": "string",
    "annotation_count": "number"
  }
}
```

### `inspect` (extended)

The existing `inspect` tool is extended to include an `annotations` array in
its output (see format above).

## Relationship to PDF/UA

Tagged PDF (PDF/UA) and semantic annotations serve different audiences:

- **Tagged PDF**: structural tree for PDF viewers and screen readers (human
  accessibility)
- **Semantic annotations**: machine-readable region metadata for AI agents

When PDF/UA support is added, the two coexist. An AI agent can read either:
the structure tree for document-level semantics, or the annotation layer for
finer-grained region metadata.

## Implementation notes

- Library: `lopdf` for all annotation read/write operations
- `fulgur render --annotate` hooks into the same coordinate collection point
  as `--manifest` (after pagination, before PDF emission)
- Annotation coordinates use PDF user space (origin bottom-left, points).
  The render pipeline already works in this coordinate system.
