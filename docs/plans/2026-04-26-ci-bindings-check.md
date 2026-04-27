# CI bindings-check job 実装プラン

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `.github/workflows/ci.yml` に `bindings-check` job を追加し、`#![cfg(feature = "...")]` で gate された `crates/pyfulgur` / `crates/fulgur-ruby` を毎 PR で `cargo clippy --all-targets -- -D warnings` する。fulgur-0qwi の根本対策。

**Architecture:** 単一 job (Linux only) を `oxidize-rb/actions/setup-ruby-and-rust` で Ruby+Rust 環境を整備し、pyfulgur (extension-module) と fulgur-ruby (ruby-api) の 2 crate に対して `cargo clippy --all-targets -- -D warnings` を順に実行する。matrix なし。

**Tech Stack:** GitHub Actions YAML, `oxidize-rb/actions/setup-ruby-and-rust@v1.4.4`, `Swatinem/rust-cache@v2`, cargo (workspace), pyo3 0.28 (extension-module), magnus 0.8 + rb-sys 0.9 (ruby-api)。

**Working directory:** `.worktrees/ci-bindings-check` (branch `ci/bindings-check`)。

**Issue:** fulgur-0qwi。design は beads issue の `design` field 参照。

---

### Task 1: ci.yml に bindings-check job を追加

**Files:**

- Modify: `.github/workflows/ci.yml` (jobs セクション末尾、`wpt:` の後ろ)

**Step 1: 既存 ci.yml の末尾構造を確認**

Run: `tail -30 .github/workflows/ci.yml`

`wpt:` job が末尾にあることを確認。

**Step 2: bindings-check job を追記**

`.github/workflows/ci.yml` の末尾 (最後の job の直後、ファイル末尾の改行は維持) に以下を追加:

```yaml

  bindings-check:
    name: Bindings type-check
    # `crates/pyfulgur` (extension-module) と `crates/fulgur-ruby` (ruby-api) は
    # `#![cfg(feature = "...")]` で全体 gate されており、workspace 標準の
    # `cargo check --workspace` / clippy では空コンパイルされるため型エラーが
    # 検出されない。0.6.0 release で `fulgur::Error::Other` の match arm 不足が
    # release-* job まで気付かれなかった問題 (PR #217) の再発防止。
    # 型エラー検出が目的のため Linux 単一・matrix 無し。クロス OS build/link
    # 検証は release-python.yml / release-ruby.yml の責務。
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: oxidize-rb/actions/setup-ruby-and-rust@e5f9a49a7812a078584072f6e3f657ad247c8771  # v1.4.4
        with:
          ruby-version: "3.3"
          rustup-toolchain: stable
          bundler-cache: false
          cargo-cache: false
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: ubuntu-latest-fulgur-bindings
          save-if: ${{ github.ref == 'refs/heads/main' }}
      - name: cargo clippy pyfulgur (extension-module)
        run: cargo clippy -p pyfulgur --features extension-module --all-targets -- -D warnings
      - name: cargo clippy fulgur-ruby (ruby-api)
        run: cargo clippy -p fulgur-ruby --features ruby-api --all-targets -- -D warnings
```

**Step 3: YAML 構文確認**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`
Expected: 何も出力されない (YAML として valid)

**Step 4: actionlint があれば走らせる**

Run: `command -v actionlint && actionlint .github/workflows/ci.yml || echo "actionlint not installed; skipping"`
Expected: actionlint が無ければ skip メッセージ、あればエラー無し。

**Step 5: ローカルで cargo clippy を最低限走らせて design の正当性を確認**

Run: `cargo clippy -p pyfulgur --features extension-module --all-targets -- -D warnings 2>&1 | tail -5`
Expected: `Finished` で終わる (現在の main は PR #217 で修正済み)。

Run: `cargo clippy -p fulgur-ruby --features ruby-api --all-targets -- -D warnings 2>&1 | tail -5`
Expected: `Finished` で終わる。ローカルに ruby env が無い場合 rb-sys が build.rs で fail する可能性あり — その場合は CI 実行に委ねる旨を記録。

ローカル実行ができなくても次の step に進む (CI で確認する)。

**Step 6: コミット**

```bash
git add .github/workflows/ci.yml
git commit -m "$(cat <<'EOF'
ci: add bindings-check job for cfg-gated pyfulgur / fulgur-ruby

`crates/pyfulgur` (`extension-module`) and `crates/fulgur-ruby`
(`ruby-api`) gate the entire crate with `#![cfg(feature = "...")]`.
Workspace `cargo check` / clippy compile both as empty modules, so
mismatches between the core fulgur crate and the bindings (e.g. a new
`Error` variant) only surface during the release-python.yml /
release-ruby.yml build — too late.

Add a dedicated Linux job that runs
`cargo clippy --all-targets -- -D warnings` against both bindings with
their feature flags enabled. Uses `oxidize-rb/actions/setup-ruby-and-rust`
(same pin as release-ruby.yml) for the Ruby toolchain; pyo3
extension-module needs no extra setup on `ubuntu-latest`.

Refs: fulgur-0qwi, PR #217 (root-cause hotfix).
EOF
)"
```

---

### Task 2: docs/plans 追加分のコミット

**Files:**

- Add: `docs/plans/2026-04-26-ci-bindings-check.md`

**Step 1: コミット**

```bash
git add docs/plans/2026-04-26-ci-bindings-check.md
git commit -m "docs(plans): add ci bindings-check implementation plan (fulgur-0qwi)"
```

---

### Task 3: PR 作成と CI 確認

**Step 1: push**

```bash
git push -u origin ci/bindings-check
```

**Step 2: PR 作成**

```bash
gh pr create --title "ci: add bindings-check job for cfg-gated pyfulgur / fulgur-ruby" --body "$(cat <<'EOF'
## Summary

- `.github/workflows/ci.yml` に `bindings-check` job を新設
- `crates/pyfulgur` (`extension-module`) と `crates/fulgur-ruby` (`ruby-api`) を毎 PR で `cargo clippy --all-targets -- -D warnings`
- PR #217 で実証された「workspace `cargo check` が cfg-gated bindings crate を空コンパイルし、release-* まで型エラーが検出されない」盲点を CI で塞ぐ

## Background

`crates/{pyfulgur,fulgur-ruby}/src/lib.rs:7` は crate 全体を `#![cfg(feature = "...")]` で gate しているため、default features で走る workspace 標準の check / clippy / nextest は両 crate を空コンパイルする。0.6.0 release で `fulgur::Error::Other` 追加時に bindings 側の match arm 漏れが release-python.yml / release-ruby.yml の build まで検出されなかった (PR #217 で hotfix)。

## Design notes

- 専用 job: lint job に Ruby env を相乗りさせると lint 全体が遅くなる + Ruby 不要なジョブにも Ruby setup が掛かる
- Linux 単一: クロスプラットフォーム build / link 検証は release-* job の責務。ここでは型エラー検出に絞る
- clippy `--all-targets -- -D warnings` 単体採用: 既存 lint job (ci.yml:40-41) と同じ pattern。`cargo check` を別途走らせてもビルド工程は重複するだけで信号は増えない
- cache shared-key を分離 (`ubuntu-latest-fulgur-bindings`): pyo3 / magnus / rb-sys を引き込むので main 用 cache とは graph が異なる

## Test plan

- [ ] CI で `bindings-check` job が green
- [ ] 既存 jobs (markdownlint / lint / test / wasm-check / vrt / octocov / wpt) は変更なしで pass
- [ ] (任意) `fulgur::Error` に dummy variant を一時追加して artificial regression を作り、`bindings-check` が fail することを確認 → revert

Refs: fulgur-0qwi, PR #217.
EOF
)"
```

**Step 3: CI 完了を待つ**

```bash
gh pr checks --watch
```

Expected: 全 check が pass。`bindings-check / Bindings type-check` が PASS。

**Step 4: 任意の artificial regression test**

ci 検証として、`fulgur::Error` に新 variant を一時的に追加し `bindings-check` が fail することを確認できる。実施する場合のみ実行 (実施しなくても本タスクは完了とする — 既存 PR #217 の history が証跡)。

実施手順 (実施する場合):

```bash
# 1. crates/fulgur/src/error.rs に dummy variant 追加 (例: `DebugProbe(String)`)
# 2. push
# 3. CI で bindings-check が non-exhaustive patterns で fail することを確認
# 4. revert
```

---

## Acceptance criteria (issue fulgur-0qwi)

1. ✅ `.github/workflows/ci.yml` に `bindings-check` job が存在する
2. ✅ `cargo clippy -p pyfulgur --features extension-module --all-targets -- -D warnings` が CI で実行される
3. ✅ `cargo clippy -p fulgur-ruby --features ruby-api --all-targets -- -D warnings` が CI で実行される
4. ✅ PR の CI で job が green
5. ✅ 既存 jobs に変更なし

## Out of scope

- crates.io 上の `fulgur` 公開版と bindings の整合 (PR #217 で扱った semver bump 戦略)
- precompiled wheel / gem の cross-build 検証 (release-*.yml)
- Gemfile.lock の commit (fulgur-jdt3)
- ruby-api / extension-module を default feature にする等の crate 構造変更
