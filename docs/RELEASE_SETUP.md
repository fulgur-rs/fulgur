# Release Setup: Trusted Publishing

pyfulgur (PyPI) と fulgur (RubyGems) を OIDC Trusted Publishing で publish するための、
一度だけ必要な設定手順。

## 初回公開時の注意

pyfulgur と fulgur gem はどちらも PyPI / RubyGems に未登録の可能性がある。
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

```bash
gh workflow run release-python.yml --field dry_run=true
```

## RubyGems Trusted Publisher

既存 gem (fulgur) の場合:

1. <https://rubygems.org/sign_in> にログイン (gem Owner アカウント)
2. <https://rubygems.org/gems/fulgur/trusted_publishers> を開く
3. "Create" で以下を登録:
   - Repository owner: `mitsuru`
   - Repository name: `fulgur`
   - Workflow filename: `release-ruby.yml`
   - Environment: `rubygems`
4. GitHub リポジトリで Environment `rubygems` を作成

OIDC claim (repo + workflow + environment) で自動照合されるため、`role-to-assume`
等の値は workflow 側に不要 (`rubygems/configure-rubygems-credentials` のデフォルト動作)。

新規 gem を作成する場合は <https://rubygems.org/profile/oidc/pending_trusted_publishers>
から "Pending Trusted Publisher" を登録。

注意: RubyGems には TestPyPI に相当する staging 環境がないため、`release-ruby.yml`
の `workflow_dispatch` dry-run は publish をスキップするのみ (build + smoke test
のみ走る)。OIDC / credential フローの実動作検証は初回の本番リリースで行う。

## GitHub Environments

以下の Environment を作成:

- `release` — crates.io / GitHub Release publish を gate する
- `pypi` — PyPI publish を gate する
- `testpypi` (dry-run 用)
- `rubygems` — RubyGems publish を gate する

### Required reviewers (approval gate)

release は irreversible 操作が多いため、`release` / `pypi` / `rubygems` の
3 環境すべてで **Required reviewers** を設定する (`testpypi` は dry-run のため不要)。

各 Environment の設定画面 (Settings → Environments → `<name>`) で:

- "Required reviewers" にチェック
- reviewer として自分 (および必要ならリリース権限を持つ共同メンテナ) を追加

3 段ゲートになるため、release flow で:

1. PR merge → `release.yml` の `publish` job が `release` env で pause → 承認
2. crates.io + tag push + GitHub Release publish が完走 (App token で `release:published` 発火)
3. `release-python.yml` の `publish` job が `pypi` env で pause → 承認
4. `release-ruby.yml` の `publish` job が `rubygems` env で pause → 承認
5. PyPI / RubyGems へ反映

途中で publish job が失敗した際の re-run も承認を経由する。

OIDC claim による scope (repo + workflow + environment) は reviewer 設定と独立して
機能するため、両方併用してよい。

## GitHub App (release publisher)

`release.yml` の最終 `gh release edit --draft=false` は `release:published` イベントを
発火させ、`release-python.yml` / `release-ruby.yml` を連鎖起動する。しかし GitHub の
無限ループ防止仕様により **`GITHUB_TOKEN` で発火したイベントは別 workflow を起動しない**
([docs](https://docs.github.com/en/actions/using-workflows/triggering-a-workflow#triggering-a-workflow-from-a-workflow))。
そのため GitHub App token で publish する必要がある。

### App 作成手順

1. <https://github.com/settings/apps/new> で新規 App を作成 (個人アカウント所有でも可)
   - GitHub App name: `fulgur-release-bot` 等 (任意・グローバル一意)
   - Homepage URL: 任意 (例: リポジトリ URL)
   - Webhook: "Active" のチェックを外す
   - Repository permissions:
     - **Contents: Read and write** (release 作成・編集に必要)
   - Where can this GitHub App be installed?: "Only on this account"
2. 作成後の App 設定画面で:
   - **App ID** を控える (数値)
   - **Private keys** → "Generate a private key" で `.pem` をダウンロード
3. 左メニュー "Install App" から対象リポジトリ (`fulgur-rs/fulgur`) に install
   - "Only select repositories" で fulgur のみに限定

### リポジトリ secrets に登録

Settings → Secrets and variables → Actions → New repository secret:

- `RELEASE_APP_ID`: 上記 App ID (数値)
- `RELEASE_APP_PRIVATE_KEY`: ダウンロードした `.pem` ファイルの内容全体
  (`-----BEGIN RSA PRIVATE KEY-----` から `-----END RSA PRIVATE KEY-----` まで)

### 動作確認

次回 release で GitHub Actions の `release.yml` → `release` job が成功したあと、
Actions タブで `release-python.yml` / `release-ruby.yml` が自動的に `release` イベントで
起動することを確認する。

## Release 手順

1. `release-prepare.yml` を `workflow_dispatch` で起動 (version 入力)
2. 作成された `release/vX.Y.Z` PR を merge
3. `release.yml` が tag + crates.io publish + GitHub Release publish (App token)
4. `release: published` で `release-python.yml` と `release-ruby.yml` が並行発火
5. 数分〜十数分後に PyPI / RubyGems へ反映
