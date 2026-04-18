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

If you are contributing as part of your employment, please ensure you have your
employer's permission. A future Corporate CLA (CCLA) may be required for
company-wide contributions; contact the maintainer if this applies.

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

- [ ] Code compiles without errors (`cargo build`)
- [ ] Tests pass (`cargo test`)
- [ ] Clippy is clean (`cargo clippy`)
- [ ] Formatting is correct (`cargo fmt --check`)
- [ ] Markdown files lint cleanly (`npx markdownlint-cli2 '**/*.md'`)
- [ ] New behavior has tests
- [ ] Documentation is updated if user-facing behavior changes
- [ ] CLA has been signed (first-time contributors only)

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
- **No fd 1 writes from the library crate**: See the fd 1 policy in `CLAUDE.md`.

## Reporting bugs

Please open an issue on GitHub with:

- Fulgur version (`cargo run --bin fulgur -- --version`)
- Minimal HTML/CSS that reproduces the issue
- Expected output vs actual output
- Environment (OS, Rust version)

## Questions

For questions that aren't bug reports, please use GitHub Discussions if enabled,
or open an issue with the `question` label.
