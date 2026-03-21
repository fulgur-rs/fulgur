# Release Workflow Design

## Overview

`workflow_dispatch` で手動キックし、バージョン更新・changelog 生成・crates.io publish・バイナリ配布・GitHub Release 作成を全自動で実行する。

## Trigger

```yaml
on:
  workflow_dispatch:
    inputs:
      version:
        description: 'Release version (e.g. 0.2.0)'
        required: true
        type: string
```

## Jobs

### 1. publish

1. チェックアウト（`fetch-depth: 0`、全履歴）
2. `cargo install git-cliff`
3. `Cargo.toml` バージョン更新（fulgur + fulgur-cli）
4. `git-cliff --tag v$VERSION -o CHANGELOG.md`
5. コミット `release: v$VERSION` + タグ `v$VERSION`
6. プッシュ（コミット + タグ）
7. `cargo publish -p fulgur`
8. 30秒 sleep（crates.io インデックス反映待ち）
9. `cargo publish -p fulgur-cli`

Secrets: `CARGO_REGISTRY_TOKEN`

### 2. build-binaries (publish 完了後)

5ターゲット matrix 並列ビルド:

| target | os | archive |
|---|---|---|
| x86_64-unknown-linux-gnu | ubuntu-latest | tar.gz |
| x86_64-unknown-linux-musl | ubuntu-latest | tar.gz |
| aarch64-unknown-linux-gnu | ubuntu-24.04-arm | tar.gz |
| aarch64-apple-darwin | macos-latest | tar.gz |
| x86_64-pc-windows-msvc | windows-latest | zip |

アーカイブ名: `fulgur-v$VERSION-$TARGET.{tar.gz,zip}`

### 3. release (build-binaries 完了後)

1. 全アーティファクト取得
2. `git-cliff --latest --strip header` でリリースノート抽出
3. `gh release create` + バイナリ添付

## Configuration

- `cliff.toml` — git-cliff 設定（Conventional Commits カテゴリ分類）
- `CARGO_REGISTRY_TOKEN` — GitHub Secrets に設定

## Files

- Create: `.github/workflows/release.yml`
- Create: `cliff.toml`
