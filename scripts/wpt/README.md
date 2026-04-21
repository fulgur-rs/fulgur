# scripts/wpt/

`fetch.sh` executes a sparse, shallow clone of the W3C web-platform-tests
repository into `target/wpt/`, pinned to the SHA in `pinned_sha.txt`.

The set of fetched paths is controlled by `subset.txt` (one pattern per line,
Git sparse-checkout syntax). Keep in sync with the subset `fulgur-wpt`
actually exercises.

## Usage

```bash
scripts/wpt/fetch.sh
```

Idempotent: re-running updates to the current pinned SHA. Override the remote
URL with `WPT_REMOTE_URL=...` (useful for mirrors or CI cache warmup).

The pin is manually bumped via PR (see "Updating the pin" below) and verified by re-running seed. Run `scripts/wpt/fetch.sh` to materialize the snapshot under `target/wpt/`.

## Updating the pin

1. Inspect upstream WPT `main` and pick a commit that is green on the
   relevant subsections.
2. Replace the SHA line in `pinned_sha.txt`.
3. Re-run `scripts/wpt/fetch.sh` and `cargo test -p fulgur-wpt`.
4. Commit both in one PR.
