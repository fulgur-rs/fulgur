# Release Workflow Design

## Overview

2つの GitHub Actions workflow によるリリースプロセス。`workflow_dispatch` で準備 PR を作成し、マージをトリガーに publish・バイナリ配布・GitHub Release を実行する。

## Workflows

### 1. release-prepare.yml (手動キック)

**トリガー:** `workflow_dispatch` with optional `version` input

**バージョン決定:**

- 未指定: 現在のバージョンの patch を自動バンプ (0.1.0 → 0.1.1)
- 指定: その値を使用 (semver バリデーション付き、`v` プレフィックス自動除去)

**ステップ:**

1. `Cargo.toml` バージョン更新 (fulgur + fulgur-cli)
2. git-cliff で `CHANGELOG.md` 生成
3. `release/v$VERSION` ブランチ作成 + PR
4. Draft GitHub Release 作成 (changelog をノートに)

### 2. release.yml (PR マージで自動実行)

**トリガー:** `release/*` ブランチの main マージ (`pull_request: closed`)

**ジョブ:**

#### publish

1. ブランチ名からバージョン抽出 (env 経由でインジェクション防止)
2. タグ `v$VERSION` 作成 + プッシュ
3. `cargo publish -p fulgur`
4. `cargo publish -p fulgur-cli` (リトライ付き、最大8回バックオフ)

#### build-binaries (publish 完了後)

5ターゲット matrix 並列ビルド:

| target | os | archive |
|---|---|---|
| x86_64-unknown-linux-gnu | ubuntu-latest | tar.gz |
| x86_64-unknown-linux-musl | ubuntu-latest | tar.gz |
| aarch64-unknown-linux-gnu | ubuntu-24.04-arm | tar.gz |
| aarch64-apple-darwin | macos-latest | tar.gz |
| x86_64-pc-windows-msvc | windows-latest | zip |

アーカイブ名: `fulgur-v$VERSION-$TARGET.{tar.gz,zip}`

#### release (build-binaries 完了後)

- Draft Release にバイナリアップロード
- Draft → Published に変更

## Configuration

- `cliff.toml` — git-cliff 設定 (Conventional Commits カテゴリ分類)
- `CARGO_REGISTRY_TOKEN` — GitHub Secrets (crates.io API トークン)

## Files

- `.github/workflows/release-prepare.yml` — 準備 workflow
- `.github/workflows/release.yml` — 実行 workflow
- `cliff.toml` — git-cliff 設定
