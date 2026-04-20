# WPT CSS paged media reftest runner — design

Status: design (pre-implementation)
Date: 2026-04-21
Related issues: see epic created after this doc

## Goal

fulgur (HTML → PDF) が W3C web-platform-tests の CSS paged media
系サブセットを reftest で通すための自前ランナーを設計する。Phase 1
で `css-page` (220 reftests) を動かし、以後 `css-break`,
`css-multicol`, `css-gcpm` と拡張する。

## 非目標

- WPT 公式 runner (`wpt run`) への準拠。我々は互換レポート
  (`wptreport.json`) は出すが、harness 互換ではない。
- Chrome / Puppeteer を ground truth にした cross-renderer 検証。
  Phase 1 では self-consistency (test と ref を両方 fulgur で描画)
  のみ。puppeteer 拡張は将来 plugin 的に足せる設計余地を残す。
- `css-flexbox` / `css-grid` / `selectors` など Blitz が既に WPT で
  カバーしている領域。fulgur では重複投資しない。

## 設計決定

### D1. ランナーの配置

新規 crate `crates/fulgur-wpt/` (`publish = false`)。

- fulgur-vrt との責務分離: fulgur-vrt は手書きフィクスチャ +
  ゆるい tolerance、fulgur-wpt は WPT 外部テスト + WPT 規約準拠
  (fuzzy meta、rel=match 等)
- 共有したい diff ロジックは `fulgur-vrt` を dev-dep として参照
  (Rule of Three 未達のため共有 crate は切り出さない)

### D2. WPT ソース取得

shallow clone を CI 時に実行。SHA を pin して再現性を担保。

```text
scripts/wpt/
  fetch.sh              # git clone --depth=1 --filter=blob:none
                        # --sparse で必要パスのみ fetch
  pinned_sha.txt        # 上流 commit SHA (PR で人間が更新)
  subset.txt            # whitelisted paths
```

`subset.txt` には必ず support ディレクトリも含める:

```text
css/css-page/
css/css-break/
css/css-gcpm/
css/css-multicol/
css/support/
css/fonts/
css/CSS2/support/
fonts/
resources/
```

### D3. リファレンス描画戦略

**self-consistency (A)**. test と ref を両方 fulgur で描画して比較。

- WPT `*-print.html` / `*-print-ref.html` パターンは ref が
  trivially-correct primitive で組まれているため self-consistency
  で機能する
- Chrome 非依存 = CI 軽量 + 決定的
- puppeteer/chrome を後から追加する余地は残す (Phase 1-4 では不要)

`css-gcpm` だけは ref が存在しない (20件全て manual) ので、別戦略:

- fulgur 開発者が spec を読んで golden PNG を手書き
- `crates/fulgur-wpt/goldens/css-gcpm/<name>.png` に commit
- Phase 4 で専用 sub-runner を用意

### D4. Diff エンジン

`fulgur-vrt/src/diff.rs` を dev-dep 経由で再利用。

```toml
# crates/fulgur-wpt/Cargo.toml
[dev-dependencies]
fulgur-vrt = { path = "../fulgur-vrt" }
```

共有ライブラリ化は 3つ目の consumer が現れた時点で検討。

### D5. 期待値管理 (expectations)

Blitz の `wpt_expectations.txt` 形式を踏襲、ただしサブディレクトリ単位:

```text
crates/fulgur-wpt/expectations/
  css-page.txt
  css-break.txt
  css-multicol.txt
  css-gcpm.txt
```

フォーマット例:

```text
# 一行一テスト
# STATUS  test-path  (optional comment)
PASS  css/css-page/basic-pagination-001-print.html
FAIL  css/css-page/page-size-002-print.html  # break-after not yet supported
SKIP  css/css-page/some-manual-test.html     # requires user interaction
```

- Phase 1 起動時は全件 `FAIL` で出発し、通ったものを PR で
  `PASS` に昇格させる
- 既に `PASS` のテストが `FAIL` になる = 回帰。CI がハード失敗

### D6. CI 統合

PR と nightly で分ける:

- **PR check** (`ci.yml` に job 追加): `css-page` のみ実行 (~数十秒)
- **nightly** (`wpt-nightly.yml` 新規): 全 Phase 対象ディレクトリ
  を実行、`wptreport.json` を artifact にアップロード

### D7. レポート形式

`wptreport.json` (WPT 標準) 互換で出力。将来 wpt.fyi 投稿の選択肢を残す。

## ランナー実行パイプライン

1テストあたりの処理 (Phase 1):

```text
1. テスト HTML を読む
   ├─ <link rel=match href=X>   → ref = X
   ├─ <link rel=mismatch href=X> → Phase 1 対象外、SKIP マーク
   ├─ 複数 <link rel=match>      → Phase 1 対象外、SKIP マーク
   └─ <meta name=fuzzy content>  → パースして per-test tolerance 保持
2. test.html / ref.html を fulgur で PDF 化
3. 両 PDF のページ数を検査 → 不一致なら即 FAIL (ページ数不整合)
4. pdftocairo -png -r <dpi> で全ページを PNG に展開
   (`-f 1 -l 1` 省略 = 全ページ)
5. ページ毎に diff (fulgur-vrt::diff 再利用) with fuzzy tolerance
6. 全ページ pass → PASS、どこか fail → FAIL + diff 画像保存
7. 結果を wptreport.json と expectations.txt に反映
```

### Phase 1 reftest 規約サポート範囲

| 機能 | Phase 1 |
|---|---|
| `<link rel=match href=ref.html>` (単一) | ◯ |
| `<link rel=match>` 複数 (どれか一致で PASS) | SKIP |
| `<link rel=mismatch>` | SKIP |
| chained reference (ref が ref を指す) | SKIP |
| `<meta name=fuzzy content="...">` (全バリアント) | ◯ パース + 適用 (下記 §Fuzzy meta 仕様) |
| `<meta name=flags content="paged">` | 情報のみ (常に paged render) |
| `data-*` 属性 (testharness)          | N/A (reftest のみ対応) |

複数 match / mismatch / chained は Phase 5+ で別 issue として拡張。

### Fuzzy meta 仕様 (全形式対応)

WPT 仕様 ([web-platform-tests/wpt#12187](https://github.com/web-platform-tests/wpt/pull/12187),
[reftest 仕様](https://web-platform-tests.org/writing-tests/reftests.html))
に準拠し、以下すべてを Phase 1 から受理する。どれか1つだけ対応だと
実テストで false fail/silent skip が発生するため必須:

```text
fuzziness = [ url ":" ]? range ";" range
range     = N | N "-" N | "-" N | N "-"        (inclusive)
```

- **数値形式**: `"10;300"` (max diff 10 per channel, ≤300 pixels)
- **数値 + レンジ**: `"5-10;200-300"`
- **Named 形式**: `"maxDifference=10;totalPixels=300"` または
  `"maxDifference=5-10;totalPixels=200-300"` (引数名は省略可、順序は固定)
- **URL プレフィックス**: `"ref.html:10-15;200-300"` (特定 ref 専用の
  tolerance。Phase 1 は単一 ref 前提だが、テスト作者の意図を尊重して
  prefix の ref URL が rel=match href と一致する場合のみ採用、
  一致しなければ警告して既定 tolerance を適用)
- **開区間レンジ**: `"5-"` (最小のみ) / `"-300"` (最大のみ)
- **複数 `<meta name=fuzzy>`**: ref が複数の時に ref 毎 tolerance を
  与える仕様。Phase 1 は単一 ref 想定なので、URL prefix 無しの要素が
  複数あれば最後のものを採用 + 警告 (Phase 5 で複数 ref 対応時に正式化)

`reftest.rs` の fuzzy パーサは上記すべてを canonical 表現
(`FuzzyTolerance { url: Option<PathBuf>, max_diff: RangeInclusive<u8>,
total_pixels: RangeInclusive<u32> }`) に正規化し、単体テストで各
バリアントをカバーする。

## ディレクトリ構造 (完成形)

```text
crates/fulgur-wpt/
  Cargo.toml
  README.md
  src/
    lib.rs          # (空、tests が主体)
    harness.rs      # テスト探索、manifest 読み込み
    reftest.rs      # reftest 判定、fuzzy パーサ
    render.rs       # fulgur で HTML → PDF → PNG (全ページ)
    diff.rs         # fulgur-vrt::diff を薄く wrap
    report.rs       # wptreport.json 出力
    expectations.rs # expectations ファイル読み書き、PASS/FAIL 判定
  tests/
    wpt_css_page.rs # Phase 1 エントリ (cargo test)
  expectations/
    css-page.txt
  goldens/          # gcpm 専用 (Phase 4 で追加)
scripts/wpt/
  fetch.sh
  pinned_sha.txt
  subset.txt
.github/workflows/
  ci.yml            # PR: css-page のみ (既存 workflow に job 追加)
  wpt-nightly.yml   # nightly: 全量
```

## フェーズ分割

| Phase | 対象 | テスト数 | 先行Phase | 備考 |
|---|---|---|---|---|
| 1 | `css-page` reftest | ~220 | — | harness 検証、骨格確立 |
| 2 | `css-break` reftest | ~968 | 1 | scale 検証 |
| 3 | `css-multicol` reftest | ~337 | 1 | column-span:all 済み |
| 4 | `css-gcpm` manual | 20 | 1 | 独自 golden 運用 |
| 5 | `css-tables` reftest | TBD | 1 | 帳票ユースケースの本命 |
| 6 | `css-writing-modes` | TBD | 1 | 日本語縦書き |
| 7 | `css-lists` + `css-counter-styles` | TBD | 1 | |
| 8 | `css-text` reftest | TBD | 1 | hyphens, word-break 等 |
| 9+ | `css-fonts` / `css-backgrounds` / `css-transforms` | TBD | 1 | 視覚系 |

## 実装順序 (Phase 1 内の issue 構造)

1. `fulgur-wpt` crate scaffolding + dummy test
2. `scripts/wpt/fetch.sh` + `pinned_sha.txt` + subset.txt
3. test → PDF → multi-page PNG レンダラ (`render.rs`)
4. reftest 仕様パーサ (`reftest.rs`): `<link rel=match>`, `<meta fuzzy>`
5. 複数ページ diff ハーネス (`harness.rs` + `diff.rs`)
6. expectations ファイル読み書き (`expectations.rs`)
7. `wptreport.json` 出力 (`report.rs`)
8. css-page 全件を FAIL で seed → Phase 2 準備として docs
9. CI workflow 追加 (PR + nightly)

Phase 1 完了の定義: `cargo test -p fulgur-wpt --test wpt_css_page` が
実行でき、expectations.txt で宣言した分が全て宣言通り PASS/FAIL 判定
される。初期 PASS 件数は「ゼロでも良い」。harness が正しく動くことが
ゴール。

## 既知のリスクと mitigation

1. **WPT 上流の API/パス変更**: `pinned_sha.txt` 固定で緩和。更新は PR
2. **pdftocairo の非決定性**: 既に fulgur-vrt で実績あり、同じ DPI・
   同じバイナリならバイト一致。CI では apt pinning
3. **font 非決定性**: fulgur 本体と同じく `FONTCONFIG_FILE` +
   bundled fonts で回避 (CLAUDE.md「Font determinism caveat」参照)
4. **テストが自前 support/resources を要求**: subset.txt に明示的に
   support ディレクトリを含める (P1-2 対策)
5. **マルチページ出力の検証漏れ**: ページ数不一致は即 FAIL、
   各ページを個別 diff する (P1-1 対策)
