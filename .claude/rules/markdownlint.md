---
description: When creating or editing Markdown files (*.md)
globs: "**/*.md"
---

# Markdownlint Rules

Follow markdownlint-cli2 rules when creating or editing Markdown files.

## Requirements

- Always add a language identifier to fenced code blocks (use `text` as fallback)
- Add blank lines before and after fenced code blocks and lists
- No multiple consecutive blank lines

## Project Configuration (.markdownlint-cli2.yaml)

The following rules are disabled:

- MD001: Heading increment
- MD013: Line length
- MD024: Duplicate headings (siblings_only)
- MD033: Inline HTML
- MD036: Emphasis as heading
- MD060: Table column style

## Verification

After editing, run:

```bash
npx markdownlint-cli2 '**/*.md'
```
