# npm Distribution Design

## Overview

Distribute `fulgur` CLI as npm packages so AI agents can invoke fulgur via
`npx` without a separate install step. Follows the esbuild split-package
pattern: one small package per platform containing just the binary, plus a
meta-package that resolves the right binary automatically.

Node.js API (`@fulgur-rs/node`) is a planned follow-on and is out of scope here.

## Package Layout

```
@fulgur-rs/cli                    meta-package (users install this)
@fulgur-rs/cli-linux-x64          binary for x86_64-unknown-linux-gnu
@fulgur-rs/cli-linux-x64-musl     binary for x86_64-unknown-linux-musl (Alpine/Docker)
@fulgur-rs/cli-linux-arm64        binary for aarch64-unknown-linux-gnu
@fulgur-rs/cli-darwin-arm64       binary for aarch64-apple-darwin
@fulgur-rs/cli-darwin-x64         binary for x86_64-apple-darwin (Intel Mac)
@fulgur-rs/cli-win32-x64          binary for x86_64-pc-windows-msvc
```

## Meta-package (@fulgur-rs/cli)

`package.json`:

```json
{
  "name": "@fulgur-rs/cli",
  "version": "0.x.y",
  "description": "HTML to PDF conversion CLI",
  "bin": { "fulgur": "bin/fulgur" },
  "optionalDependencies": {
    "@fulgur-rs/cli-linux-x64":      "0.x.y",
    "@fulgur-rs/cli-linux-x64-musl": "0.x.y",
    "@fulgur-rs/cli-linux-arm64":    "0.x.y",
    "@fulgur-rs/cli-darwin-arm64":   "0.x.y",
    "@fulgur-rs/cli-darwin-x64":     "0.x.y",
    "@fulgur-rs/cli-win32-x64":      "0.x.y"
  }
}
```

`bin/fulgur` is a checked-in JavaScript shim. At run time it detects
`process.platform` + `process.arch` (and musl via `/proc/self/maps` check on
Linux), resolves the matching optional dependency with `require.resolve`,
and execs its native binary. No `postinstall` is needed, so `npx`'s first
invocation works without a race between `bin/` symlinking and binary copy.

## Platform packages (@fulgur-rs/cli-linux-x64, etc.)

Each platform package contains only:

```
package.json   { "name": "@fulgur-rs/cli-linux-x64", "os": ["linux"], "cpu": ["x64"] }
bin/fulgur     (or fulgur.exe on Windows)
```

`os` and `cpu` fields cause npm/yarn/pnpm to skip download on non-matching
platforms.

## User Experience

```bash
# Zero-install (AI agent / CI friendly)
npx -y @fulgur-rs/cli render input.html -o output.pdf
npx -y @fulgur-rs/cli mcp

# Global install
npm install -g @fulgur-rs/cli
fulgur render input.html -o output.pdf

# Project-local
npm install --save-dev @fulgur-rs/cli
npx fulgur render input.html -o output.pdf
```

### Claude Desktop MCP config

```json
{
  "mcpServers": {
    "fulgur": {
      "command": "npx",
      "args": ["-y", "@fulgur-rs/cli", "mcp"]
    }
  }
}
```

## Release Pipeline

Additions to `.github/workflows/release.yml`:

1. Add `x86_64-apple-darwin` to the `build-binaries` matrix (runs on `macos-13`
   which is the last Intel GitHub-hosted runner).
2. After each platform binary is built and archived, upload it as a workflow
   artifact named `npm-<target>`.
3. Add a `publish-npm` job (runs after all binary build jobs):
   a. Download all `npm-<target>` artifacts.
   b. For each platform package: copy binary into `packages/npm/<platform>/bin/`,
      set version from release tag, run `npm publish --access public`.
   c. For meta-package: set version and all optionalDependency versions from
      release tag, run `npm publish --access public`.
4. Auth: `NODE_AUTH_TOKEN` secret (npmjs.com automation token).

Version is always in sync with the Rust crate version (e.g., crate `0.5.0` →
npm `0.5.0`).

## Repository Layout

```
packages/
  npm/
    meta/             @fulgur-rs/cli (meta-package source)
      package.json
      bin/fulgur        (checked-in JS shim, exec native binary at runtime)
    linux-x64/        @fulgur-rs/cli-linux-x64
      package.json
      bin/.gitkeep
    linux-x64-musl/   @fulgur-rs/cli-linux-x64-musl
      package.json
      bin/.gitkeep
    linux-arm64/      @fulgur-rs/cli-linux-arm64
      package.json
      bin/.gitkeep
    darwin-arm64/     @fulgur-rs/cli-darwin-arm64
      package.json
      bin/.gitkeep
    darwin-x64/       @fulgur-rs/cli-darwin-x64
      package.json
      bin/.gitkeep
    win32-x64/        @fulgur-rs/cli-win32-x64
      package.json
      bin/.gitkeep
```

Binaries are not committed; they are injected by CI at publish time.

## Implementation Notes

- `bin/fulgur` shim should fail gracefully with a clear error message if no
  matching platform package is installed (e.g., unsupported OS, or
  `--ignore-optional`).
- musl detection: read `/proc/self/maps` and check for `musl` in the path of
  any loaded library; fall back to gnu if detection fails.
- Intel Mac runner: use `macos-13` (GitHub's last Intel-based macOS runner).
  `macos-latest` now points to `macos-15` (Apple Silicon).
- No `postinstall` step: the shim is a checked-in script that resolves the
  platform package at run time. This avoids the npm race where `bin/` symlinks
  are created before `postinstall` runs, which broke `npx @fulgur-rs/cli`
  on the first invocation.
