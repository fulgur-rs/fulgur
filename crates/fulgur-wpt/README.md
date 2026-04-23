# fulgur-wpt

W3C web-platform-tests (WPT) の CSS paged media 系サブセット reftest を fulgur で走らせる自前ランナー。

## 他 crate との責務分担

| crate | 役割 |
|---|---|
| `fulgur` | HTML → PDF 本体 |
| `fulgur-vrt` | 手書きフィクスチャの visual regression, ゆるい tolerance |
| `fulgur-wpt` | 外部 WPT reftest, WPT 規約準拠 (fuzzy meta, rel=match, rel=mismatch 等) |

diff ロジックは `fulgur-vrt::diff` を dev-dep 経由で再利用する (Rule of Three 未達のため共有 crate は切り出さない)。

## 使い方

詳細は epic fulgur-2foo と `docs/plans/2026-04-21-wpt-reftest-runner-design.md` を参照。

## サポートする reftest 種別

- `rel=match` (単一) — 主要形式。fuzzy tolerance (`<meta name="fuzzy">`) を尊重。
- `rel=mismatch` (単一) — negative reftest。test と ref の差分が fuzzy 閾値を **超えた** ときに PASS。完全一致 (tolerance 内) で FAIL。

複数 `rel=match` / `rel=mismatch` の混在は `classify()` が SKIP を返す。chained reference（ref HTML 自体が別 ref を指す構造）は未検知 — ref HTML は再帰解析されず known limitation となっている（Phase 2 の実装対象）。root-relative な ref href（例: `href="/css/reference/..."`) も未サポートで、そのような link は classify() がスキップし NoMatch 扱いになる。

## Expectations の運用

WPT の各 test は `crates/fulgur-wpt/expectations/<subdir>.txt` に `PASS` / `FAIL` / `SKIP` として宣言する。ハーネスは実行結果と宣言を突き合わせ、

- 宣言 PASS × 実測 FAIL → 回帰 (CI が落ちる)
- 宣言 FAIL × 実測 PASS → 昇格候補 (警告のみ、CI は落ちない)
- 宣言 SKIP → テスト実行スキップ

で判定する。

### 初期 seed

新しいサブディレクトリを追加するときは以下の手順で expectations を生成する。

```bash
# まず WPT ソースを取得
scripts/wpt/fetch.sh

# 対象サブディレクトリを全件流して expectations を自動生成
cargo run -p fulgur-wpt --example seed -- \
  --subdir css-page \
  --wpt-root target/wpt \
  --out crates/fulgur-wpt/expectations/css-page.txt
```

生成された `expectations/<subdir>.txt` をコミット。以降この PR が reference point になる。

### CI との関係

WPT reftest の結果は **CI を fail させません** (`continue-on-error: true`)。PR でテストが「赤」になっても merge はブロックされないので、fulgur に広範な変更を加えた直後でも feedback loop が早く回ります。

カバレッジ推移は以下で観測します:

- **PR CI step summary**: 各 phase の total / PASS / FAIL / SKIP と PASS 率が自動で表示される
- **PR artifact**: `target/wpt-report/<phase>/report.json` (wptreport.json schema) / `regressions.json` / `summary.md` を `wpt-<phase>-report` として upload
- **nightly**: 同じ構造で全 phase 実行、`regressions.json` に回帰があれば `wpt-nightly-regression` ラベルの issue を自動起票

expectations は「宣言と実測が一致すればまだ退化していない」という baseline です。fulgur 改善で PASS 化したテストは **PR で expectations を編集して PASS に昇格** させ、次回以降の regression 検出の土俵に乗せます。

### PASS 昇格フロー

fulgur を改善して新しいテストが通るようになったら:

1. ローカルで `cargo run -p fulgur-wpt --example run_one -- <test-path>` を実行して PASS を確認
2. `crates/fulgur-wpt/expectations/<subdir>.txt` の該当行を `FAIL` → `PASS` に書き換え
3. 行末のコメント (`# reason: ...`) は削除してよい
4. PR 化、CI の `wpt-css-page` job が green であることを確認してマージ

### 既知の FAIL を一時的に無効化

テストが flaky だったり、fulgur 側の修正中で一時的に壊れている場合は `SKIP` に書き換えて理由を残す:

```text
SKIP  css/css-page/flaky-test.html  # flaky on low-DPI rendering, tracked in fulgur-xxx
```

原因追跡 issue を beads に起票して、修正後に `FAIL` か `PASS` に戻す。

## Cross-subdir cherry-pick lists (`expectations/lists/`)

大量の WPT テストをサブディレクトリ単位で丸ごと回すのは高コストです。複数の WPT サブディレクトリから数本ずつテストを引きたい bug 用途向けに、`expectations/lists/<name>.txt` というファイルを置くと、そのリストに列挙されたテストだけを cherry-pick で実行できます。書式は phase ファイル (`css-page.txt` など) と同じ `PASS | FAIL | SKIP` で、パスは `css/` 配下の **任意のサブディレクトリ**を跨いで指定可能です。

```text
# crates/fulgur-wpt/expectations/lists/my-list.txt
FAIL  css/CSS2/generated-content/before-after-positioned-003.html  # fulgur-vlr3: primary repro
PASS  css/css-images/linear-gradient-1.html                         # fulgur-yax4: regression net
```

ファイルを追加するだけで、自動的に新しい cargo test が生えます:

```bash
cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_my_list
```

内部的には `build.rs` がビルド時に `expectations/lists/*.txt` を走査し、ファイルごとに `#[test] fn wpt_list_<stem>()` を生成します (stem 中のハイフンはアンダースコアに変換)。ファイルの追加・削除・リネームは `.txt` の編集のみで完結し、Rust コード変更は不要です。

リストに載せるテストは `scripts/wpt/subset.txt` にも (test ファイルと `-ref.html` の両方を) 追加しておく必要があります。これを怠ると sparse-checkout に引っ張られず、`run_list` が warning を出して当該テストをスキップします。

成果物は `target/wpt-report/<name>/` 配下に `report.json` / `regressions.json` / `summary.md` 形式で出力されます (phase runner と同じ構造)。

### CI shard としての用途

`css-multicol` のような巨大な phase をいくつかの `lists/multicol-1.txt`, `multicol-2.txt`, ... に分割し、各リストを独立した test binary filter として並列実行することで、CI matrix のシャーディングに使えます。各 matrix job は `cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_multicol_1` のようにフィルタ引数だけで shard を選べます。
