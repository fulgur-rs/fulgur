#!/usr/bin/env bash
# Shallow-clone WPT upstream and sparse-checkout only the paths needed
# by fulgur-wpt. Idempotent: re-running updates to the pinned SHA.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
WPT_DIR="$REPO_ROOT/target/wpt"
SHA_FILE="$SCRIPT_DIR/pinned_sha.txt"
SUBSET_FILE="$SCRIPT_DIR/subset.txt"
REMOTE_URL="${WPT_REMOTE_URL:-https://github.com/web-platform-tests/wpt.git}"

SHA="$(awk '!/^#/ && NF { print; exit }' "$SHA_FILE" | tr -d '[:space:]')"
if [ -z "$SHA" ]; then
  echo "error: no SHA in $SHA_FILE" >&2
  exit 1
fi

# A $WPT_DIR/.git that exists but isn't a genuine standalone repo root is
# more dangerous than a missing one: every `git -C "$WPT_DIR"` call below
# would silently fall through git's repo discovery to the *enclosing*
# fulgur checkout, and `checkout --detach FETCH_HEAD` at the bottom of this
# script would then replace fulgur's own working tree (Cargo.toml included)
# with WPT's sparse subset while still reporting success. This happens in
# CI when Swatinem/rust-cache caches the whole `target/` dir (this repo
# doesn't scope it to subpaths) and a restored `.git` doesn't survive the
# tar round-trip intact (see fulgur-5x02). Verify the directory is really
# its own git root before trusting it as already-initialized.
if [ -d "$WPT_DIR/.git" ]; then
  wpt_toplevel="$(git -C "$WPT_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
  wpt_dir_real="$(cd "$WPT_DIR" && pwd -P)"
  if [ "$wpt_toplevel" != "$wpt_dir_real" ]; then
    echo "warning: $WPT_DIR/.git is present but not a valid repo root (resolved to '$wpt_toplevel') — reinitializing" >&2
    rm -rf "$WPT_DIR"
  fi
fi

if [ ! -d "$WPT_DIR/.git" ]; then
  mkdir -p "$WPT_DIR"
  git -C "$WPT_DIR" init -q
  git -C "$WPT_DIR" config core.sparseCheckout true
  git -C "$WPT_DIR" config extensions.partialClone origin
fi

# Keep the remote URL in sync on every run so WPT_REMOTE_URL overrides
# (mirrors, CI caches) take effect even when target/wpt already exists.
git -C "$WPT_DIR" remote set-url origin "$REMOTE_URL" 2>/dev/null \
  || git -C "$WPT_DIR" remote add origin "$REMOTE_URL"

# Write sparse-checkout patterns (strip comments and blanks)
mkdir -p "$WPT_DIR/.git/info"
grep -v '^#' "$SUBSET_FILE" | sed '/^[[:space:]]*$/d' > "$WPT_DIR/.git/info/sparse-checkout"

# Fetch only the pinned SHA, filter=blob:none to keep it lean
git -C "$WPT_DIR" fetch --depth=1 --filter=blob:none origin "$SHA"
git -C "$WPT_DIR" checkout -q --detach FETCH_HEAD

echo "WPT ready at $WPT_DIR (SHA: $SHA)"
