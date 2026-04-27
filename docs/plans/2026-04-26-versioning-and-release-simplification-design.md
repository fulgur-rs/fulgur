# Versioning Policy & Release Pipeline Simplification — Design

**Date:** 2026-04-26
**Status:** Approved for implementation
**Scope:** バージョニング方針切替 + release pipeline の approve / skip / changelog 簡略化

## 背景

現状 `0.5.14` まで patch を 14 回積んできたが、各 release は機能追加・breaking change・bug fix が混在しており、patch 番号が中身を反映していない。
加えて release pipeline には次の摩擦がある:

- 1 release ごとに 3 箇所 (`release` / `pypi` / `rubygems`) で environment approval 待ちが発生
- bindings (PyPI / RubyGems / npm) は GH Release 公開で自動発火し、core のみ release したいケースに逃げ道がない
- CHANGELOG.md は git-cliff (commit 単位) で生成されており、1 PR が複数 commit を持つと "address AI review feedback" や "cargo fmt" のような review 副産物が混入する

本 design はこの 3 つを一括で解消する。

## 1. Versioning Policy: ZeroVer 採用

### 方針

- **全ての通常 release は minor を上げる** (`0.5.14` → `0.6.0` → `0.7.0` → ...)
- **patch は hotfix 専用** — release 後の致命バグ修正のみ。通常 release では使わない
- **ワークスペース同期維持** — `fulgur` / `fulgur-cli` / `fulgur-wasm` / `fulgur-ruby` / `pyfulgur` は同じ番号で揃える (現状維持)
- **next release = `0.6.0`**
- 1.0.0 到達タイミングは別途検討 (本 design のスコープ外)

### 根拠

- 0.x 期間中であることを ZeroVer (<https://0ver.org>) として明示し、bindings 利用者に「API は安定保証外」を伝える
- 「次の release = 次の minor」で迷う余地を消し、判断コストを下げる
- minor の数字が roadmap の節目 (Pageable 廃止、上流移行など) と紐付けやすくなる

## 2. Release Pipeline 簡略化

### 2.1 Approval ゲート集約

| Approval ポイント | before | after |
|-------------------|--------|-------|
| Release PR レビュー | あり | あり (現状維持) |
| `release` env (release.yml) | あり | あり (唯一の environment ゲート) |
| `pypi` env (release-python.yml) | あり | **削除** (環境は OIDC subject 用に残す) |
| `rubygems` env (release-ruby.yml) | あり | **削除** (環境は OIDC subject 用に残す) |

GH Release を作成 = 既に `release` environment の approve を通っている、という一段論法で
下流の追加 approve を省く。`pypi` / `rubygems` environment 自体は OIDC trusted publisher の
subject claim として必要なので残すが、`required_reviewers` だけ剥がす。

### 2.2 `skip_bindings` フラグ

- **粒度**: 単一フラグ (PyPI / RubyGems / npm を一括制御)
- **デフォルト**: `false` (= 全部 publish、現状の挙動を維持)
- **伝播経路**: release-prepare.yml の input → release PR に `release:skip-bindings` ラベル付与 → 各 publish workflow がラベル参照

```text
release-prepare.yml
  inputs: { version, skip_bindings }
    ↓ (skip_bindings=true なら label 付与)
Release PR (label: release:skip-bindings)
    ↓ merge
release.yml
  - crates.io publish (常に実行)
  - npm publish (label が無い時のみ実行)
  - GH Release 作成 (常に実行 — CLI binary 配布のため必要)
    ↓ release: published
release-python.yml / release-ruby.yml
  - 起動時に gh api でラベル取得 → label 有 → exit 0
  - label 無 → 通常 publish
```

### 2.3 自動 bump のデフォルト変更

`release-prepare.yml` の auto-bump ロジック: patch → **minor**

```bash
# before
PATCH=$((PATCH + 1))
VERSION="$MAJOR.$MINOR.$PATCH"

# after
MINOR=$((MINOR + 1))
VERSION="$MAJOR.$MINOR.0"
```

`workflow_dispatch` の `version` input は引き続き有効 (hotfix 時に `0.6.1` を明示指定する経路)。

## 3. PR-based Changelog

### 方針

- **ソース**: マージされた PR (commit ではなく)
- **カテゴリ分類**: PR ラベル (`release-notes:*`)
- **ツール**: GitHub native `--generate-notes` + `.github/release.yml` config
- **git-cliff は廃止**

### ラベル設計

| ラベル | カテゴリ |
|--------|----------|
| `release-notes:feature` | Features |
| `release-notes:fix` | Bug Fixes |
| `release-notes:docs` | Documentation |
| `release-notes:internal` | (release notes から除外: CI / refactor / chore) |
| (ラベル無し) | Other Changes |

### `.github/release.yml`

```yaml
changelog:
  exclude:
    labels:
      - release-notes:internal
      - dependencies  # Dependabot は別 section にしたければ category 追加
  categories:
    - title: Features
      labels: [release-notes:feature]
    - title: Bug Fixes
      labels: [release-notes:fix]
    - title: Documentation
      labels: [release-notes:docs]
    - title: Other Changes
      labels: ["*"]
```

### CHANGELOG.md の扱い

- `0.6.0` 以降は GH API から取得した release notes を `release-prepare.yml` で prepend
- `0.5.14` 以前のエントリは現状のまま温存 (履歴を書き換えない)
- 生成は `gh api repos/:owner/:repo/releases/generate-notes` または `gh release view` の output を整形

### Migration

- 既存 PR にラベルは backfill しない
- `0.6.0` の release notes は **手動編集を許容** (移行期の現実解)
- `0.7.0` 以降はラベル運用が定着している前提
- ラベル付与の責任: PR 作成者 + reviewer。CI で `release-notes:*` 必須化は **しない** (ラベル無し = "Other Changes" でフォールバック)

## 4. 実装ステップ (file-by-file)

| # | ファイル | 変更内容 |
|---|----------|----------|
| 1 | `.github/release.yml` | **新規作成**。category と exclude を定義 |
| 2 | `.github/workflows/release-prepare.yml` | (a) auto-bump を patch → minor に変更<br>(b) `skip_bindings: boolean` input 追加 + ラベル付与ロジック<br>(c) git-cliff 関連 step 削除<br>(d) CHANGELOG.md 生成を `gh api ... generate-notes` ベースに置換 |
| 3 | `.github/workflows/release.yml` | npm publish step に `if: !contains(github.event.pull_request.labels.*.name, 'release:skip-bindings')` 追加 |
| 4 | `.github/workflows/release-python.yml` | 起動時にラベルチェック step 追加 → 該当 release の PR ラベルを `gh api` で取得し `release:skip-bindings` あれば exit 0 |
| 5 | `.github/workflows/release-ruby.yml` | 同上 |
| 6 | `cliff.toml` | **削除** |
| 7 | `docs/RELEASE_SETUP.md` | (a) ZeroVer ポリシー節を追加<br>(b) `pypi` / `rubygems` environment の `required_reviewers` 削除手順を追記<br>(c) `release-notes:*` ラベル運用を追記<br>(d) `skip_bindings` 使い方を追記 |
| 8 | `README.md` | Versioning section を追加 (短く: ZeroVer 採用、minor = release、patch = hotfix のみ) |
| 9 | GitHub repository labels | `release:skip-bindings`, `release-notes:feature`, `release-notes:fix`, `release-notes:docs`, `release-notes:internal` を作成 |
| 10 | GitHub environments (manual) | `pypi` / `rubygems` の `required_reviewers` を解除 |

## 5. リスクと緩和

| リスク | 緩和策 |
|--------|--------|
| `release` env approve だけで PyPI/RubyGems が走る = 攻撃面が広がる | OIDC trusted publisher は `environment` 名と repo を strict 検証する。GH Release 作成は `release` env の approve を経由するので、悪意ある tag push で publish が走るシナリオは現状と同じレベル |
| ラベル付け忘れで release notes が "Other Changes" だらけになる | 移行期は受容。定着後に必要なら PR template に `release-notes:` ラベルチェックを追加 |
| `skip_bindings` 後に bindings 側だけ後追い release したい | 別途 `workflow_dispatch` で release-python / release-ruby を手動起動する経路を残す。今回は触らない |
| `0.5.14` → `0.6.0` への jump がユーザーに breaking と誤読される | CHANGELOG.md と README で「番号運用方針変更」を明記 |

## 6. スコープ外

- 1.0.0 到達条件
- bindings の独立バージョニング (A-2 / A-3 への移行)
- CLI binary 以外の release artifact 追加
- `release` env approve 自体の自動化 (現状維持)

## 7. 起票する beads issues

1. **fulgur-XXX (parent epic)**: ZeroVer 採用 + release pipeline 簡略化
   - **fulgur-XXX**: `.github/release.yml` 新規作成 + ラベル整備
   - **fulgur-XXX**: `release-prepare.yml` 改修 (auto-bump minor / skip_bindings input / git-cliff 廃止)
   - **fulgur-XXX**: `release.yml` 改修 (npm skip 条件)
   - **fulgur-XXX**: `release-python.yml` / `release-ruby.yml` 改修 (skip ラベル check)
   - **fulgur-XXX**: `cliff.toml` 削除
   - **fulgur-XXX**: `docs/RELEASE_SETUP.md` / `README.md` の方針記述
   - **fulgur-XXX**: GitHub environments 設定変更 (manual / 手順 doc 化のみ)
   - **fulgur-XXX**: `0.6.0` を最初の ZeroVer release として切る (実装後の検証 release)
