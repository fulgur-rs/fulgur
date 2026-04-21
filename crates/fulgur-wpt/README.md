# fulgur-wpt

W3C web-platform-tests (WPT) の CSS paged media 系サブセット reftest を fulgur で走らせる自前ランナー。

## 他 crate との責務分担

| crate | 役割 |
|---|---|
| `fulgur` | HTML → PDF 本体 |
| `fulgur-vrt` | 手書きフィクスチャの visual regression, ゆるい tolerance |
| `fulgur-wpt` | 外部 WPT reftest, WPT 規約準拠 (fuzzy meta, rel=match 等) |

diff ロジックは `fulgur-vrt::diff` を dev-dep 経由で再利用する (Rule of Three 未達のため共有 crate は切り出さない)。

## 使い方

詳細は epic fulgur-2foo と `docs/plans/2026-04-21-wpt-reftest-runner-design.md` を参照。

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
