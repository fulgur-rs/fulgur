# PyPI / RubyGems Publish CI Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** GitHub Actions で pyfulgur を PyPI に、fulgur gem を RubyGems.org に、Rust リリース直後に自動 publish する CI を構築する。

**Architecture:** 既存の `release.yml`（crates.io publish + GitHub Release 作成）を変更せず、`release: published` イベントで発火する 2 本の独立ワークフロー（`release-python.yml` / `release-ruby.yml`）を追加。PyPI/RubyGems とも Trusted Publishing (OIDC) で secretsレス。`release-prepare.yml` はバインディングのバージョンも同期するよう拡張。GitHub Releases へバインディング artifact は添付しない（PyPI/RubyGems が正）。

**Tech Stack:** GitHub Actions, `PyO3/maturin-action@v1`, `pypa/gh-action-pypi-publish@release/v1`, `oxidize-rb/cross-gem-action@v9`, `rubygems/configure-rubygems-credentials@main`, PyO3 abi3, rb_sys.

**beads issue:** fulgur-qyf

---

## Verified Facts (from reading source)

### Python API (crates/pyfulgur/src/engine.rs)

- `pyfulgur.Engine` — `#[pyclass(name = "Engine", module = "pyfulgur")]`, direct ctor accepts optional kwargs
- `pyfulgur.Engine.builder()` — `#[staticmethod]` returns `EngineBuilder`, chain with `.build()`
- `engine.render_html(html: str)` — returns `bytes` (PyBytes)

### Ruby API (crates/fulgur-ruby/src/engine.rs, lib/fulgur.rb, spec/engine_spec.rb)

- `Fulgur::Engine.new` — accepts optional kwargs (page_size, margin, etc.)
- `Fulgur::Engine.builder.build` — class method, returns `EngineBuilder`, chain (Ruby style, no parens)
- `engine.render_html(html)` — returns `Fulgur::Pdf` (not raw bytes). Use `pdf.bytesize` for size check.

### Starting versions

- `crates/fulgur/Cargo.toml`: 0.4.5
- `crates/fulgur-cli/Cargo.toml`: 0.4.5
- `crates/pyfulgur/Cargo.toml`: 0.0.2
- `crates/pyfulgur/pyproject.toml`: 0.0.2
- `crates/fulgur-ruby/Cargo.toml`: 0.0.1
- `crates/fulgur-ruby/ext/fulgur/Cargo.toml`: 0.0.1
- `crates/fulgur-ruby/lib/fulgur/version.rb`: 0.0.1
- `crates/fulgur-ruby/fulgur.gemspec`: `required_ruby_version = ">= 3.3.0"`（matrixの3.1/3.2と不整合）

### Git repo

- Remote: `https://github.com/mitsuru/fulgur`
- PyPI project `pyfulgur` は初公開の可能性 → PyPI 側で "pending publisher" 登録が必要
- RubyGems gem `fulgur` も初公開の可能性

### Tool availability（検証済み）

- `actionlint`: 未インストール。GitHub 側で workflow YAML は push 時に検証される。ローカル validation は `python3 -c "import yaml; yaml.safe_load(...)"` で構文のみ確認
- `yq`: 未インストール（上記で代替）
- `cargo`, `ruby`, `python3`, `npx`: 利用可能

---

## Verification Commands

各 Task 完了後に使用:

```bash
# YAML syntax (構文のみ。Actions-specific な warning は push 時に GitHub で判明)
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-python.yml'))"
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-ruby.yml'))"
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-prepare.yml'))"

# actionlint (optional; docker があれば):
# docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest -color

# Workspace build
cargo check --workspace

# Ruby gemspec
cd crates/fulgur-ruby && ruby -c fulgur.gemspec && cd ../..

# Markdown
npx markdownlint-cli2 'docs/RELEASE_SETUP.md' 'README.md'
```

---

## Task 1: gemspec の Ruby バージョン要件を緩和

**Files:**

- Modify: `crates/fulgur-ruby/fulgur.gemspec` (required_ruby_version 行)

**Why:** 現状 `required_ruby_version = ">= 3.3.0"` だが、precompiled gem matrix は Ruby 3.1〜3.4。3.1/3.2 ユーザーが `gem install` 時に弾かれるのを防ぐ。

**Step 1: required_ruby_version を変更**

```ruby
spec.required_ruby_version = ">= 3.1.0"
```

**Step 2: gemspec syntax 検証**

```bash
cd crates/fulgur-ruby && ruby -c fulgur.gemspec
```

Expected: `Syntax OK`

**Step 3: Commit**

```bash
git add crates/fulgur-ruby/fulgur.gemspec
git commit -m "chore(fulgur-ruby): loosen required_ruby_version to 3.1.0

precompiled gem matrix で Ruby 3.1/3.2 も対象にするため。"
```

---

## Task 2: pyfulgur に abi3 feature を追加

**Files:**

- Modify: `crates/pyfulgur/Cargo.toml` (pyo3 依存に features 追加)
- Modify: `crates/pyfulgur/pyproject.toml` (tool.maturin に py-limited-api)

**Why:** design で abi3-py39 を採用。PyO3 の `abi3-py39` feature を有効化し、maturin で Python 3.9+ を 1 wheel でカバー（5 wheel/Python × 5 = 25 ではなく 5 wheel で済む）。

**Step 1: Cargo.toml の pyo3 依存に abi3 feature を追加**

```toml
[dependencies]
fulgur = { path = "../fulgur" }
pyo3 = { version = "0.22", optional = true, features = ["abi3-py39"] }
```

**Step 2: pyproject.toml の tool.maturin セクションに py-limited-api を追加**

```toml
[tool.maturin]
module-name = "pyfulgur"
manifest-path = "Cargo.toml"
features = ["extension-module"]
py-limited-api = "cp39"
```

**Step 3: workspace build 検証**

```bash
cd "$(git rev-parse --show-toplevel)" && cargo check --workspace
```

Expected: Finished success

**Step 4: Commit**

```bash
git add crates/pyfulgur/Cargo.toml crates/pyfulgur/pyproject.toml
git commit -m "feat(pyfulgur): enable abi3-py39 for single wheel across Python 3.9+

wheelが Python 3.9/3.10/3.11/3.12/3.13 ひとつでカバーされ、ビルド時間・配布数を
削減する。"
```

---

## Task 3: release-prepare.yml のバージョン同期拡張

**Files:**

- Modify: `.github/workflows/release-prepare.yml` (Update version step)

**Why:** fulgur コアと同じバージョンを pyfulgur / fulgur-ruby に揃えるため、release PR 生成時に全バージョンを一括更新。

**Step 1: sed パターンが該当ファイルでマッチするか確認**

```bash
grep -n '^version = ' crates/pyfulgur/Cargo.toml
grep -n '^version = ' crates/pyfulgur/pyproject.toml
grep -n '^version = ' crates/fulgur-ruby/Cargo.toml
grep -n '^version = ' crates/fulgur-ruby/ext/fulgur/Cargo.toml
grep -n 'VERSION = ' crates/fulgur-ruby/lib/fulgur/version.rb
```

Expected: 各1行ヒット

**Step 2: "Update version in Cargo.toml" step を以下で置換**

```yaml
      - name: Update version in Cargo.toml
        env:
          VERSION: ${{ steps.version.outputs.version }}
        run: |
          # Rust core
          sed -i "s/^version = \".*\"/version = \"$VERSION\"/" crates/fulgur/Cargo.toml
          sed -i "s/^version = \".*\"/version = \"$VERSION\"/" crates/fulgur-cli/Cargo.toml
          sed -i "s/fulgur = { version = \"[^\"]*\", path/fulgur = { version = \"$VERSION\", path/" crates/fulgur-cli/Cargo.toml

          # pyfulgur (Python binding)
          sed -i "s/^version = \".*\"/version = \"$VERSION\"/" crates/pyfulgur/Cargo.toml
          sed -i "s/^version = \".*\"/version = \"$VERSION\"/" crates/pyfulgur/pyproject.toml

          # fulgur-ruby (Ruby binding)
          sed -i "s/^version = \".*\"/version = \"$VERSION\"/" crates/fulgur-ruby/Cargo.toml
          sed -i "s/^version = \".*\"/version = \"$VERSION\"/" crates/fulgur-ruby/ext/fulgur/Cargo.toml
          sed -i "s/VERSION = \".*\"/VERSION = \"$VERSION\"/" crates/fulgur-ruby/lib/fulgur/version.rb

          cargo check --workspace
```

**Step 3: YAML 構文検証**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-prepare.yml'))"
```

Expected: no error

**Step 4: Commit**

```bash
git add .github/workflows/release-prepare.yml
git commit -m "ci(release-prepare): sync pyfulgur and fulgur-ruby versions

Cargo.toml (pyfulgur, fulgur-ruby, ext/fulgur), pyproject.toml, version.rb
すべてを release バージョンに揃える。"
```

---

## Task 4: release-python.yml 作成

**Files:**

- Create: `.github/workflows/release-python.yml`

**Why:** maturin で 5 wheel (abi3) + sdist を build し、smoke test 通過後に PyPI へ OIDC publish。`workflow_dispatch` で TestPyPI dry-run 可能。

**Step 1: ワークフロー本体を作成**

```yaml
name: Release (Python / PyPI)

on:
  release:
    types: [published]
  workflow_dispatch:
    inputs:
      dry_run:
        description: 'Publish to TestPyPI instead of PyPI'
        required: false
        type: boolean
        default: true

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  build-wheels:
    name: Build wheel (${{ matrix.target }})
    strategy:
      fail-fast: false
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            runner: ubuntu-latest
            manylinux: "2014"
          - target: aarch64-unknown-linux-gnu
            runner: ubuntu-24.04-arm
            manylinux: "2014"
          - target: x86_64-apple-darwin
            runner: macos-13
          - target: aarch64-apple-darwin
            runner: macos-latest
          - target: x86_64-pc-windows-msvc
            runner: windows-latest
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"
      - name: Build wheel
        uses: PyO3/maturin-action@v1
        with:
          target: ${{ matrix.target }}
          manylinux: ${{ matrix.manylinux || 'auto' }}
          args: --release --strip --out dist -m crates/pyfulgur/Cargo.toml
      - uses: actions/upload-artifact@v4
        with:
          name: wheels-${{ matrix.target }}
          path: dist/*.whl
          if-no-files-found: error

  build-sdist:
    name: Build sdist
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: PyO3/maturin-action@v1
        with:
          command: sdist
          args: --out dist -m crates/pyfulgur/Cargo.toml
      - uses: actions/upload-artifact@v4
        with:
          name: sdist
          path: dist/*.tar.gz
          if-no-files-found: error

  smoke-test:
    name: Smoke test (${{ matrix.os }})
    needs: [build-wheels]
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: ubuntu-latest
            artifact: wheels-x86_64-unknown-linux-gnu
          - os: ubuntu-24.04-arm
            artifact: wheels-aarch64-unknown-linux-gnu
          - os: macos-13
            artifact: wheels-x86_64-apple-darwin
          - os: macos-latest
            artifact: wheels-aarch64-apple-darwin
          - os: windows-latest
            artifact: wheels-x86_64-pc-windows-msvc
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: ${{ matrix.artifact }}
          path: dist
      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"
      - name: Install wheel (unix)
        if: runner.os != 'Windows'
        shell: bash
        run: pip install dist/*.whl
      - name: Install wheel (windows)
        if: runner.os == 'Windows'
        shell: pwsh
        run: pip install (Get-ChildItem dist/*.whl)[0].FullName
      - name: Smoke test
        shell: bash
        run: |
          python -c "
          import pyfulgur
          engine = pyfulgur.Engine()
          pdf = engine.render_html('<p>hi</p>')
          assert isinstance(pdf, (bytes, bytearray)), type(pdf)
          assert len(pdf) > 100, len(pdf)
          print(f'OK: {len(pdf)} bytes')
          "

  publish:
    name: Publish to PyPI
    needs: [build-wheels, build-sdist, smoke-test]
    if: github.event_name == 'release' || (github.event_name == 'workflow_dispatch' && inputs.dry_run == false)
    runs-on: ubuntu-latest
    environment: pypi
    permissions:
      id-token: write
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true
      - uses: pypa/gh-action-pypi-publish@release/v1

  publish-testpypi:
    name: Publish to TestPyPI
    needs: [build-wheels, build-sdist, smoke-test]
    if: github.event_name == 'workflow_dispatch' && inputs.dry_run == true
    runs-on: ubuntu-latest
    environment: testpypi
    permissions:
      id-token: write
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true
      - uses: pypa/gh-action-pypi-publish@release/v1
        with:
          repository-url: https://test.pypi.org/legacy/
```

**Step 2: YAML 構文検証**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-python.yml'))"
```

Expected: no error

**Step 3: Commit**

```bash
git add .github/workflows/release-python.yml
git commit -m "ci: add release-python.yml for PyPI OIDC publish

release published で発火、5 wheel (abi3-py39) + sdist を build、smoke test
通過後に PyPI Trusted Publishing で publish。workflow_dispatch で TestPyPI
への dry-run も可能。"
```

---

## Task 5: release-ruby.yml 作成

**Files:**

- Create: `.github/workflows/release-ruby.yml`

**Why:** rb_sys cross-gem で 7 platform × 4 Ruby version の precompiled gem + source gem を build、smoke test 通過後に RubyGems へ OIDC publish。`rake release` ではなく明示的 `gem push` を使う（複数 gem を同時 push するため）。

**Step 1: ワークフロー本体を作成**

```yaml
name: Release (Ruby / RubyGems)

on:
  release:
    types: [published]
  workflow_dispatch:

permissions:
  contents: read

jobs:
  build-source-gem:
    name: Build source gem
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: ruby/setup-ruby@v1
        with:
          ruby-version: "3.3"
          working-directory: crates/fulgur-ruby
          bundler-cache: true
      - name: Build
        working-directory: crates/fulgur-ruby
        run: bundle exec rake build
      - uses: actions/upload-artifact@v4
        with:
          name: gem-source
          path: crates/fulgur-ruby/pkg/*.gem
          if-no-files-found: error

  build-precompiled-gems:
    name: Build precompiled gem (${{ matrix.platform }})
    strategy:
      fail-fast: false
      matrix:
        platform:
          - x86_64-linux-gnu
          - aarch64-linux-gnu
          - x86_64-linux-musl
          - aarch64-linux-musl
          - x86_64-darwin
          - arm64-darwin
          - x64-mingw-ucrt
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: oxidize-rb/actions/setup-ruby-and-rust@v1
        with:
          ruby-version: "3.3"
          rustup-toolchain: stable
          bundler-cache: true
          cargo-cache: true
          working-directory: crates/fulgur-ruby
      - uses: oxidize-rb/cross-gem-action@v9
        with:
          platform: ${{ matrix.platform }}
          ruby-versions: "3.1,3.2,3.3,3.4"
          working-directory: crates/fulgur-ruby
      - uses: actions/upload-artifact@v4
        with:
          name: gem-${{ matrix.platform }}
          path: crates/fulgur-ruby/pkg/*.gem
          if-no-files-found: error

  smoke-test:
    name: Smoke test (${{ matrix.os }} / Ruby ${{ matrix.ruby }})
    needs: [build-precompiled-gems]
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: ubuntu-latest
            ruby: "3.1"
            artifact: gem-x86_64-linux-gnu
          - os: ubuntu-latest
            ruby: "3.4"
            artifact: gem-x86_64-linux-gnu
          - os: ubuntu-24.04-arm
            ruby: "3.3"
            artifact: gem-aarch64-linux-gnu
          - os: macos-13
            ruby: "3.3"
            artifact: gem-x86_64-darwin
          - os: macos-latest
            ruby: "3.3"
            artifact: gem-arm64-darwin
          - os: windows-latest
            ruby: "3.3"
            artifact: gem-x64-mingw-ucrt
    runs-on: ${{ matrix.os }}
    steps:
      - uses: ruby/setup-ruby@v1
        with:
          ruby-version: ${{ matrix.ruby }}
      - uses: actions/download-artifact@v4
        with:
          name: ${{ matrix.artifact }}
          path: pkg
      - name: Install gem (unix)
        if: runner.os != 'Windows'
        shell: bash
        run: gem install pkg/fulgur-*.gem --no-document
      - name: Install gem (windows)
        if: runner.os == 'Windows'
        shell: pwsh
        run: gem install (Get-ChildItem pkg/fulgur-*.gem)[0].FullName --no-document
      - name: Smoke test
        shell: bash
        run: |
          ruby -rfulgur -e '
            engine = Fulgur::Engine.new
            pdf = engine.render_html("<p>hi</p>")
            raise "empty pdf" if pdf.bytesize < 100
            puts "OK: #{pdf.bytesize} bytes"
          '

  publish:
    name: Publish to RubyGems
    needs: [build-source-gem, build-precompiled-gems, smoke-test]
    if: github.event_name == 'release'
    runs-on: ubuntu-latest
    environment: rubygems
    permissions:
      id-token: write
      contents: read
    steps:
      - uses: ruby/setup-ruby@v1
        with:
          ruby-version: "3.3"
      - uses: actions/download-artifact@v4
        with:
          path: pkg
          merge-multiple: true
      - name: List gems
        run: ls -la pkg/
      - name: Configure RubyGems credentials (OIDC)
        uses: rubygems/configure-rubygems-credentials@main
        with:
          role-to-assume: rg_oidc_akr_mitsuru_fulgur
      - name: Push gems
        run: |
          for gem in pkg/*.gem; do
            echo "Pushing $gem..."
            gem push "$gem"
          done
```

**Note on `role-to-assume`:** RubyGems の Trusted Publisher 登録時に生成される API key role 名に合わせる。RELEASE_SETUP.md で登録手順を書くので、実名は初回登録後に確定する。プレースホルダとしてサンプル名を置いているが、実際の値は Trusted Publisher UI からコピーする必要がある。

**Step 2: YAML 構文検証**

```bash
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-ruby.yml'))"
```

Expected: no error

**Step 3: Commit**

```bash
git add .github/workflows/release-ruby.yml
git commit -m "ci: add release-ruby.yml for RubyGems OIDC publish

release published で発火、7 platform × Ruby 3.1〜3.4 の precompiled gem +
source gem を rb_sys cross-gem で build、smoke test 通過後に RubyGems
Trusted Publishing で publish (configure-rubygems-credentials + gem push)。"
```

---

## Task 6: Trusted Publisher セットアップドキュメント

**Files:**

- Create: `docs/RELEASE_SETUP.md`

**Why:** PyPI / RubyGems 側の Trusted Publisher 登録は人手の Web UI 操作が必要。手順を明示して次回リリース担当者が迷わないようにする。

**Step 1: `docs/RELEASE_SETUP.md` を作成**

内容は新規プロジェクト（"pending publisher"）想定で書く。

```markdown
# Release Setup: Trusted Publishing

pyfulgur (PyPI) と fulgur (RubyGems) を OIDC Trusted Publishing で publish
するための一度だけ必要な設定手順。

## 初回公開時の注意

pyfulgur と fulgur gem はどちらも RubyGems / PyPI に未登録の可能性がある。
既存プロジェクトと新規プロジェクトで UI フローが異なる:

- **新規 (pending publisher)**: プロジェクト名だけ予約し、初回 publish 時に
  OIDC claim で自動的に project が作成される。
- **既存 publisher 追加**: 既に project が存在する場合は publisher を追加登録。

## PyPI Trusted Publisher

### Production (pypi.org)

1. <https://pypi.org/manage/account/publishing/> にログイン
2. "Add a new pending publisher" (新規の場合) または既存 project の
   "Publishing" タブから publisher 追加:
   - PyPI Project Name: `pyfulgur`
   - Owner: `mitsuru`
   - Repository name: `fulgur`
   - Workflow name: `release-python.yml`
   - Environment name: `pypi`
3. GitHub リポジトリで Environment `pypi` を作成 (Settings → Environments → New environment)。保護ルール不要。

### TestPyPI (test.pypi.org)

本番公開前に dry-run を試す場合のみ。

1. <https://test.pypi.org/manage/account/publishing/> で同様に登録
2. Environment name: `testpypi`
3. GitHub リポジトリで Environment `testpypi` を作成

Dry-run 発火:

\`\`\`bash
gh workflow run release-python.yml --field dry_run=true
\`\`\`

## RubyGems Trusted Publisher

1. <https://rubygems.org/profile/oidc/api_key_roles> にログイン
2. "New API key role" → OIDC provider: GitHub Actions
3. 以下を登録:
   - Gem: `fulgur` (新規の場合は "Pending publishing" で予約)
   - Repository: `mitsuru/fulgur`
   - Workflow: `release-ruby.yml`
   - Environment: `rubygems`
4. 生成された role 名（例: `rg_oidc_akr_xxxxxxxx`）をコピーして
   `.github/workflows/release-ruby.yml` の `role-to-assume` に設定
5. GitHub リポジトリで Environment `rubygems` を作成

## GitHub Environments

以下の 3 つの Environment を作成:

- `pypi`
- `testpypi` (dry-run用)
- `rubygems`

保護ルール不要 (OIDC claim で scope されるため)。

## Release 手順

1. `release-prepare.yml` を `workflow_dispatch` で起動（version 入力）
2. 作成された `release/vX.Y.Z` PR を merge
3. `release.yml` が tag + crates.io publish + GitHub Release publish
4. `release: published` で `release-python.yml` と `release-ruby.yml` が並行発火
5. 数分〜十数分後に PyPI / RubyGems へ反映
```

**Step 2: markdownlint**

```bash
npx markdownlint-cli2 docs/RELEASE_SETUP.md
```

Expected: 0 errors

**Step 3: Commit**

```bash
git add docs/RELEASE_SETUP.md
git commit -m "docs: add RELEASE_SETUP.md for Trusted Publisher config

PyPI / RubyGems / TestPyPI の OIDC Trusted Publisher 登録手順と GitHub
Environment 作成手順、new-project 時の pending publisher フローを記載。"
```

---

## Task 7: README 参照追加

**Files:**

- Modify: `README.md`（ルート）

**Why:** メインの README からリリース手順ドキュメントへの参照を張る。

**Step 1: README.md の既存 Release/Contributing セクションを確認**

```bash
grep -n "^## " README.md | head -20
```

既存に "Release" 節があれば追記、なければ Contributing 直前あたりに追加。

**Step 2: 以下の節を適切な位置に追加**

```markdown
## Release Process

See [docs/RELEASE_SETUP.md](docs/RELEASE_SETUP.md) for PyPI / RubyGems
Trusted Publisher setup and release steps.
```

**Step 3: markdownlint**

```bash
npx markdownlint-cli2 README.md
```

**Step 4: Commit**

```bash
git add README.md
git commit -m "docs(readme): link release setup guide"
```

---

## Final Verification

全タスク完了後:

```bash
cd "$(git rev-parse --show-toplevel)"

# YAML syntax
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-prepare.yml'))"
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-python.yml'))"
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release-ruby.yml'))"

# Workspace build
cargo check --workspace

# Markdown lint
npx markdownlint-cli2 'docs/RELEASE_SETUP.md' 'README.md'

# Ruby gemspec syntax
cd crates/fulgur-ruby && ruby -c fulgur.gemspec && cd ../..

# Git log
git log --oneline main..HEAD
```

Expected: 6〜7 commits、lint エラー 0、cargo check 成功。

---

## Security: Action SHA pin policy

認証情報を扱う workflow（publish 系）の GitHub Actions は **full-length commit SHA で pin する** のがデフォルトポリシー。

- **必須**: `pypa/gh-action-pypi-publish`, `rubygems/configure-rubygems-credentials`, `PyO3/maturin-action`, `oxidize-rb/*`, `ruby/setup-ruby`, `oxidize-rb/cross-gem-action` 等の 3rd-party action
- **例外**: まだ stable tag / 互換リリースが無く SHA 固定しにくい場合に限り、一時的に `@main` を使用してよい。ただし必ず新規 release / tag 公開を追跡し、公開され次第速やかに commit SHA pin に移行する
- **許容**: `actions/checkout`, `actions/setup-python`, `actions/upload-artifact`, `actions/download-artifact` 等の GitHub 公式 1st-party actions は major-version tag (`@v4`) 可。SHA pin への段階的移行は推奨だが必須ではない

本 PR 時点で `rubygems/configure-rubygems-credentials` のみ stable tag が無く `@main` を一時的に使用（例外に該当）。Safe alternative として SHA pin しているが、tag 公開後は合わせる。

## Known Limitations

1. **Actual publish は実リリースまで動作確認不可** — TestPyPI dry-run でも "upload 成功"しか確認できない。PyPI / RubyGems 本番 publish の動作は初回リリースで検証する。
2. **`rubygems/configure-rubygems-credentials@main`** は main branch ベースの SHA pin（2026年時点で stable release が無い）。上記 SHA pin policy の「例外」に該当。tag 公開され次第 tag based SHA pin に更新する。
3. **smoke test の API 呼び出しは verified facts に基づく** — Python: `pyfulgur.Engine()` ctor + `render_html()` returns bytes。Ruby: `Fulgur::Engine.new` + `render_html()` returns `Fulgur::Pdf` with `.bytesize`。binding 側 API が変わったら更新。
4. **release-prepare.yml は workspace 共通バージョン前提** — 将来バインディングを独立バージョニングする場合はロジック変更必要。
5. **`oxidize-rb/cross-gem-action` は v9 予定していたが実在は v7**（実装時判明、v7 の SHA で pin）。node16 DeprecationWarning 可能性あるが publish はブロックしない。
