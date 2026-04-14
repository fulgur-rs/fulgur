# @media print example

This example demonstrates inline `@media print` and `@media screen` rules.
fulgur renders documents as CSS print media, so the print branch wins while a
browser previewing the same HTML would show the screen branch instead.

## What you should see in the generated PDF

- `h1` "Report" renders in dark teal (`#064e3b`).
- The `.screen-only` paragraph ("This note only appears on screen.") is hidden
  via `display: none` and does not appear at all.
- The `.print-only` paragraph ("This note only appears in print.") appears in
  bold red (`#9f1239`).
- The third paragraph ("This paragraph is always shown.") renders in the
  always-shown slate body colour (`#1f2937`).

In a browser the opposite would happen: the `.print-only` paragraph would be
hidden and the `.screen-only` paragraph would show up in teal.
