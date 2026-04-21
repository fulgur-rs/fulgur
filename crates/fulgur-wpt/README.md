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
