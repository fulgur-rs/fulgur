# Release Setup: Trusted Publishing

One-time configuration for publishing pyfulgur (PyPI) and fulgur (RubyGems) via
OIDC Trusted Publishing, plus Fulgur's versioning policy and day-to-day release
procedure.

## Versioning policy (ZeroVer)

Fulgur follows [ZeroVer](https://0ver.org) while on the `0.x` line. See
[`docs/plans/2026-04-26-versioning-and-release-simplification-design.md`](plans/2026-04-26-versioning-and-release-simplification-design.md)
for the full rationale.

Key points:

- **Each normal release bumps the minor** (`0.5.14` → `0.6.0` → `0.7.0`).
  release-plz is configured for this (`custom_minor_increment_regex = ".*"` in
  `release-plz.toml`): every change to the `core` version group bumps the minor
  in lockstep, never the patch.
- **Patch numbers are reserved for hotfixes.** release-plz never auto-bumps the
  patch, so a `0.6.1` hotfix is cut manually when needed rather than through the
  normal auto-generated Release PR.
- **External form: `0.6`. Internal form: `0.6.0`.**
  - Internal (Cargo.toml, npm, PyPI, RubyGems, git tag, CHANGELOG section
    header): `0.6.0` — required by semver / registry validators.
  - External (README, blog, announcements, branding): "Fulgur 0.6".
- **Workspace stays in lockstep** — `fulgur`, `fulgur-cli`, `fulgur-wasm`,
  `fulgur-ruby`, and `pyfulgur` share the same version string. Independent
  binding versioning is future work.
- **No API stability guarantees until `1.0`.** Each minor on the `0.x` line is
  free to introduce breaking changes.

## PR-based changelog (`release-notes:*` labels)

CHANGELOG and GitHub Release notes are generated **from merged PRs**, not from
commits. `.github/release.yml` defines the category mapping.

| Label | Category |
|-------|----------|
| `release-notes:security` | Security (auto-applied from a `harden:` title prefix) |
| `release-notes:feature` | Features |
| `release-notes:fix` | Bug Fixes |
| `release-notes:docs` | Documentation |
| `release-notes:internal` | Excluded (CI / refactor / chore / test) |
| `dependencies` | Excluded (Dependabot) |
| (no label) | Other Changes |

Labelling responsibility sits with the **PR author and reviewer**. CI does not
enforce a `release-notes:*` label — unlabelled PRs fall through to "Other
Changes".

release-plz's own commit-based changelog is **disabled** (`changelog_update = false`
in `release-plz.toml`), so these PR-based notes are the single source. `release-plz.yml`'s
release-pr job renders them with `gh api ... generate-notes` and prepends a dated
section to the root `CHANGELOG.md` inside the Release PR; `release.yml`'s release job
sets the same notes as the GitHub Release body. Per-crate `crates/*/CHANGELOG.md`
files and git-cliff have been removed.

## 初回公開時の注意

pyfulgur と fulgur gem はどちらも PyPI / RubyGems に未登録の可能性がある。
既存プロジェクトと新規プロジェクトで UI フローが異なる:

- **新規 (pending publisher)**: プロジェクト名だけ予約し、初回 publish 時に
  OIDC claim で自動的に project が作成される。
- **既存 publisher 追加**: 既に project が存在する場合は publisher を追加登録。

## crates.io Trusted Publisher

`release-plz.yml` の `release` job は `rust-lang/crates-io-auth-action` で
OIDC token を取得し、`release-plz release` 経由で crates.io に publish する。長期 PAT
(`CARGO_REGISTRY_TOKEN`) を secrets に持つ必要はない。この job は
`environment: crates-io`（required reviewers 付き＝リリース実行承認 ②、後述）で走る。

公開 crate (`fulgur`, `fulgur-cli`, `fulgur-core`, `fulgur-blitz`) それぞれで Trusted
Publisher を登録する:

1. <https://crates.io/> にログイン (crate owner アカウント)
2. 各 crate の Settings → "Trusted Publishing" タブを開く
   (例: <https://crates.io/crates/fulgur/settings>)
3. "Add" で以下を登録:
   - Repository owner: `fulgur-rs`
   - Repository name: `fulgur`
   - Workflow filename: `release-plz.yml`
   - Environment: `crates-io`
4. `fulgur-cli`、`fulgur-core`、`fulgur-blitz` も同様に登録

> 移行メモ (`release` → `crates-io`, ゼロダウンタイム): 既存 TP は `Environment:
> release` のまま。**この env 変更が `main` に出る前に** `crates-io` の TP を**追加**する
> (`release` は残す)。順序を誤り env 変更が TP 追加より先に main へ出ると、次回リリースが
> crates.io 認証で失敗する。マージ後の次回リリースで env 変更＋②承認＋crates.io publish が
> 通るのを確認してから、古い `release` の TP を削除する。crates.io は crate ごとに複数 TP を
> 持てるので無停止で移行できる。

### 新規 crate の初回 publish

crates.io の Trusted Publisher は **既に存在する crate にしか登録できない**
([RFC 3691](https://rust-lang.github.io/rfcs/3691-trusted-publishing-cratesio.html):
"A Trusted Publisher Configuration can only be created after an initial manual
publishing of a crate.")。PyPI の pending publisher に相当する事前登録はないため、
新しい公開 crate を workspace に追加したときは、その変更を `main` に merge する **前に**
次の手順で初回 publish と TP 登録を済ませる。順序を誤ると次回の `release-plz release`
がその crate の publish で失敗する。

1. <https://crates.io/settings/tokens> で API token を作成する。
   - Scopes: `publish-new` のみ
   - Crates: 新しい crate 名だけに限定
   - Expiration: 1 日など短く
2. token を credentials ファイルに保存しないよう、`cargo login` は使わずシェル内の
   環境変数で渡す:

   ```bash
   read -rs CARGO_REGISTRY_TOKEN && export CARGO_REGISTRY_TOKEN
   ```

3. 新 crate を追加する PR のブランチ (差分なし、origin と一致) から、依存される側から順に
   publish する。既に公開済みの crate (`fulgur`, `fulgur-cli` 等) は publish しない:

   ```bash
   cargo publish -p <crate> --dry-run
   cargo publish -p <crate>
   ```

   `cargo publish` は index への反映を待ってから終了するので、依存する crate を続けて
   publish してよい。crates.io の版は削除できない (yank のみ) が、次回リリースで minor が
   上がるため、PR がレビューで変わっても実害はない。
4. 上記の手順で各 crate に Trusted Publisher を登録する。
5. `unset CARGO_REGISTRY_TOKEN` し、crates.io で token を revoke する。
6. 新 crate を `release-plz.toml` に `version_group = "core"` / `publish = true` で
   追加した PR を merge する。

`fulgur-core` / `fulgur-blitz` (fulgur-hgro) はこの手順で追加した (fulgur-vsnz)。

登録完了後、旧 secret `CARGO_REGISTRY_TOKEN` は不要なので Settings →
Secrets and variables → Actions から削除してよい。

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

- `crates-io` — **required reviewers 付き**。crates.io Trusted Publisher の OIDC
  subject claim スコープ兼、リリース実行承認ゲート (②)
- `release` — npm Trusted Publisher の OIDC subject claim スコープ (reviewers 無し)
- `pypi` — PyPI Trusted Publisher の OIDC subject claim スコープ
- `testpypi` (dry-run 用)
- `rubygems` — RubyGems Trusted Publisher の OIDC subject claim スコープ

The release has **two** approval gates (see [Approval model](#approval-model)):
① reviewing the release *contents* on the Release PR (`release-pr-approval`
status check), and ② authorizing the *execution* via the `crates-io` environment.
Only `crates-io` carries `Required reviewers`; the rest are OIDC subject-claim
scopes only.

| Environment | Required reviewers | Purpose |
|-------------|--------------------|---------|
| `crates-io` | **Set** (②) | crates.io OIDC subject claim **and** the release-execution approval gate |
| `release`   | Not set | OIDC subject claim (npm Trusted Publishing, `publish-npm`) |
| `pypi`      | Not set | OIDC subject claim (PyPI Trusted Publisher) |
| `rubygems`  | Not set | OIDC subject claim (RubyGems Trusted Publisher) |
| `testpypi`  | Not set | Dry-run only |

`environment: release` on `release.yml`'s `publish-npm` job is **not** an approval
gate (no required reviewers) but is *not* a no-op either: referencing an
environment switches the job's OIDC subject to `…:environment:release`, and npm
Trusted Publishing can be configured to require that environment (the optional
"Environment name" field on npmjs.com). Keep it consistent with the npm
trusted-publisher registration — removing it breaks npm's OIDC auth if the npm
publishers are set up with the `release` environment.

### Approval model

The release has **two** independent human gates:

- **① Content review** — a maintainer approving the release-plz **Release PR**.
  This says "the release *contents* are correct" (version bumps, CHANGELOG).
  Enforced by the `release-pr-approval` required status check (below); a Release
  PR cannot merge without it.
- **② Release execution** — a maintainer approving the `crates-io` **environment**
  deployment. This says "actually publish this now." It pauses `release-plz.yml`'s
  `release` job before crates.io publish + `vX.Y.Z` tag creation. Because the tag
  cascades to `release.yml` (GitHub Release → npm / PyPI / RubyGems), approving ②
  authorizes the **whole** release. Exactly one ② prompt per release.

Why ① is a status check and not native "required reviews": native branch
required-reviews apply to *every* PR into `main`, which would block a solo
maintainer's own feature PRs (no self-approval). Instead a custom
`release-pr-approval` commit status (reported by
`.github/workflows/release-pr-approval-gate.yml`) is used — `success` immediately
for ordinary PRs, and for `release-plz-*` PRs only once a non-author
OWNER/MEMBER/COLLABORATOR has an APPROVED review on the current head commit.
Making that status a **required status check** in the `main` ruleset hard-blocks
an unapproved Release PR from merging.

Why ② doesn't pause ordinary merges: `release-plz.yml`'s `release` job (which
carries the `crates-io` env) is reachable on every push to `main`, and an
environment gate would otherwise pause *every* merge. A `check-releases` job
detects whether the push is an actual release (`workflow_dispatch`, a
`release-plz-<date>` merge commit, or any `chore: release …` commit in the push)
and the `release` job is `if:`-gated on it, so ② prompts on real releases only.
`workflow_dispatch` on `release-plz.yml` is a manual escape hatch (still gated
by ②).

Release flow:

1. release-plz opens a `release-plz-*` **Release PR**. ① blocks merging it until a
   maintainer approves the current head commit.
2. On merge, `check-releases` detects the release → the `release` job pauses on
   the `crates-io` environment. **② approve** → crates.io publish + `vX.Y.Z` tag
   (App token, so the tag fires `release.yml`).
3. The tag triggers `release.yml`: build binaries → publish the GitHub Release
   (App token) + npm. Publishing the Release fires `release:published`. No further
   approval — ② already authorized the release.
4. `release-python.yml` / `release-ruby.yml` publish to PyPI / RubyGems on
   `release:published`, **but only after their `verify-release-tag` job confirms
   the tag is a `vX.Y.Z` semver tag** (the equivalent of `release.yml`'s `setup`
   job for npm).

`if: github.event_name == 'release'` proves only the *trigger type* — it does
**not** prove the tag is a `v*` tag or that the release came from the gated
`release.yml` path. A GitHub Release can be published on *any* tag name, and the
`release-python.yml` / `release-ruby.yml` workflows fire on `release:published`
regardless of tag pattern. So each `publish` job additionally requires its
`verify-release-tag` job to pass (tag must match
`^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$`),
and the `RestrictReleaseTag` ruleset (control #4 below) restricts creation of
`v[0-9]*.[0-9]*.[0-9]*` tags to the release App. Together these tie the PyPI /
RubyGems publish to the same App-created, gated `v*` namespace as crates.io / npm
— a Release published on a non-semver tag (which the ruleset does not cover) is
rejected before any registry credential is requested. The OIDC claim scope
(repo + workflow + environment) is independent of reviewer settings and is not
what enforces this.

### Security controls (partly GitHub settings, not code)

The two approval gates plus a tag ruleset together authorize every publish path.
Several live in repo **settings**, not code — verify each (Settings →
Environments / Rules) and re-check after edits:

1. **`release-pr-approval` is a required status check** (① content review) in the
   `main` branch ruleset. Without it, a Release PR can merge unapproved.
2. **`crates-io` environment has required reviewers** (② release execution).
   Without them, the `release` job publishes crates.io + creates the tag (and thus
   the whole cascade) with no execution approval.
3. **`main` requires a PR** (no direct pushes) so code can't reach `main` — and
   thus `release-plz release` — without going through ①. Note: any actor in the
   ruleset's *bypass list* (e.g. the Repository Admin role) sidesteps ① and #3, so
   keep that list to trusted maintainers only.
4. **A `v*` tag-protection ruleset** (`RestrictReleaseTag`) restricting tag
   *creation* to the release App. It targets `refs/tags/v[0-9]*.[0-9]*.[0-9]*` and
   is the load-bearing control against the one path the gates above can't see: a
   `v*` tag pushed **directly** to the repo, which bypasses `release-plz.yml`
   entirely and lands straight on `release.yml` → GitHub Release + npm.
5. **The `verify-release-tag` job gates PyPI / RubyGems publish in code.**
   `release.yml` (→ npm) is safe from a rogue tag because it triggers on
   `push: tags: v*` **and** its `setup` job validates strict semver, so a
   non-release tag never reaches its publish jobs. `release-python.yml` /
   `release-ruby.yml`, by contrast, trigger on `release:published`, which is
   **tag-name-agnostic** — a GitHub Release created on any tag (e.g. `evil`,
   `vX`, or a bare `1.2.3`), a name that #4's ruleset does **not** restrict,
   would otherwise reach their `publish` jobs on `if: github.event_name ==
   'release'` alone. Each `publish` job therefore `needs:` a `verify-release-tag`
   job that rejects any tag not matching
   `^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$` (the leading `v` is required —
   the same prefix #4 restricts), tying these publishes to the same App-created
   `v*` namespace #4 protects. This is an **in-code** control (visible
   in the workflow YAML), not a settings one.

> Status as of 2026-07-07: #1, #2, #3, #4 and #5 are live. #2 = the `crates-io`
> environment with required reviewers (verified via
> `gh api repos/fulgur-rs/fulgur/environments/crates-io`); `release`, `pypi` and
> `rubygems` have no reviewers (`…/environments/<name>` → `protection_rules: []`)
> — the PyPI / RubyGems publish path is gated by #5 (`verify-release-tag`) + #4
> instead. #4 = `RestrictReleaseTag`, targeting `v[0-9]*.[0-9]*.[0-9]*` (bypass =
> `fulgur-release-bot` App only, no admin blanket bypass).
>
> **Rollout caveat (crates.io TP):** this workflow stamps the OIDC subject
> `…:environment:crates-io`, but a crates.io Trusted Publisher registered only for
> `environment: release` will reject it. Register a crates.io Trusted Publisher for
> the `crates-io` environment (alongside the existing `release` one) **before this
> change merges**, or
> the next release fails at crates.io auth; remove the old `release` publisher after
> a successful release. Re-verify rulesets with `gh api repos/<owner>/<repo>/rulesets`.

### Tag-protection ruleset (control #3 — the direct-tag-push block)

Restrict who can create `v*` tags so a rogue tag never reaches `release.yml`:

1. GitHub repo → Settings → Rules → Rulesets → **New tag ruleset**.
2. Target: **Tag** → include by pattern `v[0-9]*.[0-9]*.[0-9]*` (or `v*`).
3. Rules: enable **Restrict creations** (and **Restrict updates** /
   **Restrict deletions**).
4. Bypass list: add **only** the release GitHub App (`fulgur-release-bot`) — the
   actor that creates tags in `release-plz.yml`'s `release` job. Do **not** add
   the "Repository admin" role as a blanket bypass, or a human admin could still
   push a `v*` tag directly.
5. Enforcement status: **Active**.

With this ruleset a `v*` tag can only be created by the release App, which only
does so from the `release-plz release` job — after both ① (the Release PR merge)
and ② (the `crates-io` environment approval). This ruleset is a GitHub Settings
change; keep it in sync with the App name if the release bot is ever renamed.

## GitHub App (release publisher)

`release.yml` の最終 `gh release edit --draft=false` は `release:published` イベントを
発火させ、`release-python.yml` / `release-ruby.yml` を連鎖起動する。しかし GitHub の
無限ループ防止仕様により **`GITHUB_TOKEN` で発火したイベントは別 workflow を起動しない**
([docs](https://docs.github.com/en/actions/using-workflows/triggering-a-workflow#triggering-a-workflow-from-a-workflow))。
そのため GitHub App token で publish する必要がある。

さらに `Tag release` step も同じ App token を使用する。`GITHUB_TOKEN` では
`.github/workflows/*.yml` を含む commit に tag を push しようとすると
`refusing to allow a GitHub App to create or update workflow ... without
workflows permission` で拒否されるためで、App 側に `Workflows: Read and write`
権限があれば通る。

### App 作成手順

1. <https://github.com/settings/apps/new> で新規 App を作成 (個人アカウント所有でも可)
   - GitHub App name: `fulgur-release-bot` 等 (任意・グローバル一意)
   - Homepage URL: 任意 (例: リポジトリ URL)
   - Webhook: "Active" のチェックを外す
   - Repository permissions:
     - **Contents: Read and write** (release 作成・編集に必要)
     - **Workflows: Read and write** (tag push 時に workflow ファイル変更を伴う commit を通すために必要)
     - **Pull requests: Read and write** (`release-plz.yml` の release-pr job が
       この App token で Release PR を作成/更新するために必要。App 作成の PR/commit
       にすることで `ci.yml` (pull_request) が Release PR で自動発火する)
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

## Release procedure

### Normal release (minor bump)

1. release-plz opens (or updates) a `release-plz-*` **Release PR** automatically
   on pushes to `main` that warrant a release — there is no manual trigger.
2. Inspect the Release PR (CHANGELOG diff, `Cargo.toml` / aux version bumps).
3. **① Approve** the Release PR — this satisfies the `release-pr-approval` required
   status check (content review) — and merge it.
4. On merge, `check-releases` detects the release and `release-plz.yml`'s `release`
   job pauses on the `crates-io` environment. **② Approve the deployment** in the
   Actions UI → crates.io publish + `vX.Y.Z` tag creation (App token). (Ordinary,
   non-release merges never reach this prompt.)
5. The tag fires `release.yml`: build binaries → publish the GitHub Release →
   publish npm. Publishing the GitHub Release fires `release:published`. No further
   approval — ② already authorized the release.
6. `release-python.yml` / `release-ruby.yml` run on `release:published` and
   publish to PyPI / RubyGems.

Two approvals per release: ① the Release PR, then ② the `crates-io` deployment
(see [Approval model](#approval-model)).

### Previewing release notes

release-plz builds the changelog when it opens the Release PR, so the CHANGELOG
diff in that PR is the preview. To sanity-check GitHub's category grouping from
the `release-notes:*` labels independently:

```bash
gh api repos/fulgur-rs/fulgur/releases/generate-notes \
  -f tag_name=v0.6.0 \
  --jq .body
```

Add any missing categorisation labels with
`gh pr edit <num> --add-label release-notes:fix` (etc.).
