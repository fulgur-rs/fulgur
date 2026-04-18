# Contributing to Fulgur

Thank you for considering a contribution to Fulgur! This document describes the
process for submitting changes.

## Contributor License Agreement (CLA)

Fulgur requires all contributors to sign the
[Individual Contributor License Agreement](CLA.md) before their contributions can
be merged. This is a one-time agreement that covers all your future contributions
to the project.

### Why we require a CLA

- **Ownership clarity**: We need to know that you have the right to contribute
  the code you're submitting.
- **Relicensing flexibility**: The CLA allows the project to adapt its licensing
  terms in the future (for example, to enable commercial editions of specific
  components) without re-contacting every past contributor.
- **Patent protection**: The CLA includes a patent license grant that protects
  both the project and its users.

The CLA is based on the well-established
[Project Harmony 1.0][harmony] template and does **not** require you to assign
copyright. You retain ownership of your contributions.

[harmony]: http://harmonyagreements.org/

### How to sign

When you open your first pull request, a bot (CLA Assistant) will automatically
comment on the PR asking you to sign. To sign, simply reply with the following
comment:

```text
I have read the CLA Document and I hereby sign the CLA
```

That's it. The bot will record your signature in `.github/cla-signatures.json`
and all your future contributions to this repository are covered.

### For employed contributors

If you are contributing as part of your employment, your employer must sign the
[Corporate Contributor License Agreement (CCLA)](CCLA.md) before you submit a
pull request. The CCLA is signed out of band (not via the bot):

1. Complete Schedule A with your Designated Contributors.
2. Have an authorized representative sign.
3. To coordinate submission, open a
   [GitHub issue](https://github.com/fulgur-rs/fulgur/issues/new) tagged `cla`
   — the maintainer will respond with the canonical submission channel (signed
   PDF via pull request to `.github/corporate-signatures/`, or email for
   companies that cannot publish the signed PDF publicly).

Once signed, all listed Designated Contributors can contribute without signing
the ICLA separately. See the [CCLA's Signing Procedure](CCLA.md#7-signing-procedure)
for full details.

## Development Workflow

### Prerequisites

- Rust toolchain (see `rust-toolchain.toml` if present)
- `cargo fmt`, `cargo clippy`
- `npx markdownlint-cli2` for documentation changes
- `poppler-utils` (`pdftocairo`) for visual regression tests

### Common commands

```bash
# Build
cargo build

# Run tests
cargo test -p fulgur --lib
cargo test -p fulgur
cargo test -p fulgur --test gcpm_integration

# Lint
cargo clippy
cargo fmt --check
npx markdownlint-cli2 '**/*.md'

# Run CLI
cargo run --bin fulgur -- render input.html -o output.pdf
```

### Pull request checklist

Before submitting a PR, please verify:

- [ ] Tests pass (`cargo test -p fulgur` — substitute the appropriate crate if
      your change is in bindings)
- [ ] Clippy is clean (`cargo clippy`)
- [ ] Formatting is correct (`cargo fmt --check`)
- [ ] Markdown files lint cleanly (`npx markdownlint-cli2 '**/*.md'`, if docs
      changed)
- [ ] New behavior has tests
- [ ] Documentation is updated if user-facing behavior changes
- [ ] CLA has been signed (first-time contributors only)

The PR template reproduces the core commands as a ticklist; both should stay
in sync.

## Commit messages

Follow the existing commit style in `git log`. Short title (< 72 chars),
optionally prefixed by a scope (`fix(cli): ...`, `feat(gcpm): ...`,
`docs: ...`).

## Design principles

Fulgur is guided by a few non-negotiable principles; PRs that violate them are
unlikely to be merged. See `CLAUDE.md` and `docs/` for details, but the short
version:

- **Offline-first**: No network access at render time.
- **Deterministic**: Same input must produce same output.
- **Adapter isolation**: Blitz API changes must stay contained in
  `blitz_adapter.rs`.
- **No ad-hoc workarounds or proprietary CSS**: When a feature doesn't
  work, fix it per the CSS spec rather than adding fulgur-specific
  properties or escape-hatch flags. Users write standard HTML/CSS;
  Fulgur renders it as specified.
- **Pagination robustness across layout models**: Grid, Multi-column,
  Table, and Flexbox must all survive page breaks. None of them is an
  "advanced" or "optional" path.
- **Programmatic control first**: Prefer explicit CLI flags and library
  parameters over implicit HTML `<meta>` auto-extraction, so that AI
  agents and language bindings can drive Fulgur deterministically.

## Reporting bugs

Please open an issue on GitHub with:

- Fulgur version (`cargo run --bin fulgur -- --version`)
- Minimal HTML/CSS that reproduces the issue
- Expected output vs actual output
- Environment (OS, Rust version)

## Questions

For questions that aren't bug reports, please use GitHub Discussions if enabled,
or open an issue with the `question` label.
