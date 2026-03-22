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

The following rules are customized or disabled:

- MD013: Line length (disabled)
- MD024: Duplicate headings (enabled, siblings_only — same-level duplicates allowed)
- MD033: Inline HTML (disabled)
- MD060: Table column style (disabled)

## Verification

After editing, run:

```bash
npx markdownlint-cli2 '**/*.md'
```
