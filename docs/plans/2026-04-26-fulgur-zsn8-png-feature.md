# fulgur-zsn8: PNG feature 統一による build 非決定性解消

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** `<img position:absolute>` 周りの絶対配置 layout バグが build configuration によって surface したりしなかったりする build 非決定性を解消する。fulgur lib に `image` の `png` feature を追加して全 caller (CLI / Python / Ruby / WPT runner) が一貫した PNG decode 挙動になるようにする。

**Architecture:** `blitz-dom-0.2.4` は `image = { default-features = false }` で全 image format を無効化している。fulgur lib の Cargo.toml に `image = { features = ["png"] }` を追加して Cargo の feature unification を経由して PNG decode を全 caller で有効化する。`<img position:absolute>` 自体の layout バグ (画像が独立ページを占有する) は scope 外で別 issue (新規) で対応。本プランでは layer 1 (build 非決定性) のみ修正、layer 2 (絶対配置 layout) は新 issue 起票 + expectations 暫定降格でカバーする。

**Tech Stack:** Rust, Cargo, fulgur (lib + CLI + WPT runner)

---

## Task 1: ベースライン WPT を保存

**Files:**
- Read: `crates/fulgur-wpt/expectations/css-page.txt`
- Generate: `target/wpt-report/css-page/regressions.json` (実行結果)

**Step 1: 現状のベースラインを記録**

Run: `cargo test -p fulgur-wpt --test wpt_css_page --release -- --nocapture 2>&1 | tail -30`

Expected: 失敗・成功は関係なく走り切る。`target/wpt-report/css-page/regressions.json` と `summary.md` が更新される。

**Step 2: 現状の regressions / promotions を保存**

Run:
```bash
cp target/wpt-report/css-page/regressions.json /tmp/zsn8_baseline_regressions.json
cp target/wpt-report/css-page/summary.md /tmp/zsn8_baseline_summary.md
wc -l /tmp/zsn8_baseline_regressions.json
grep -c '"test"' /tmp/zsn8_baseline_regressions.json
head -5 /tmp/zsn8_baseline_summary.md
```

Expected: ベースライン保存完了。`page-background-002` と `page-background-003` を含む regression list を確認。

**Step 3: コミット不要 (調査用ファイル)**

スキップ — `/tmp/` 配下に置いて diff 用に保持。

---

## Task 2: fulgur lib に png feature を追加

**Files:**
- Modify: `crates/fulgur/Cargo.toml`

**Step 1: Cargo.toml の [dependencies] セクションに `image` 行を追加**

`crates/fulgur/Cargo.toml` の `[dependencies]` セクション (既存の `lopdf = "0.40.0"` 行の直後あたり、deps 群の中) に以下を追加:

```toml
# Force-enable `image`'s `png` feature so PNG decoding (used by blitz-dom for
# `<img>` tags and CSS background-image) works consistently across every fulgur
# build target. Without this, `blitz-dom-0.2.4`'s `image = { default-features =
# false }` makes PNG decoding depend on which other workspace member is built
# alongside fulgur, producing different page-count outputs in CLI vs WPT runner
# (fulgur-zsn8).
image = { version = "0.25", default-features = false, features = ["png"] }
```

**Step 2: build 確認**

Run: `cargo build -p fulgur --release 2>&1 | tail -3`

Expected: `Finished` で完了、エラーなし。

**Step 3: lib テスト緑確認**

Run: `cargo test -p fulgur --lib 2>&1 | tail -5`

Expected: 全テスト pass (~340 件)。

**Step 4: コミット**

```bash
git add crates/fulgur/Cargo.toml crates/fulgur/Cargo.lock 2>/dev/null || true
git add crates/fulgur/Cargo.toml Cargo.lock
git commit -m "deps(fulgur): force-enable image png feature for build determinism

Without this dep, blitz-dom-0.2.4's image = { default-features = false }
makes PNG decoding depend on workspace feature unification, so fulgur-cli
(no direct image dep) silently fails to decode PNGs while fulgur-wpt
(direct image dep) succeeds. The mismatch surfaced as page-background-002
returning 3 pages via CLI but 4 via the WPT runner.

This fix only covers the determinism layer of fulgur-zsn8; the underlying
layout bug where <img position:absolute> consumes a page is tracked
separately."
```

---

## Task 3: CLI が WPT runner と同じページ数を返すことを確認

**Files:**
- Read-only verification

**Step 1: CLI 単独ビルド + render**

Run:
```bash
cargo build -p fulgur-cli --release 2>&1 | tail -3
./target/release/fulgur render target/wpt/css/css-page/page-background-002-print-ref.html -o /tmp/zsn8_after.pdf
pdfinfo /tmp/zsn8_after.pdf | grep Pages
```

Expected: `Pages: 4` (これまで CLI は 3 だった、WPT runner と一致するように)。

**Step 2: 念のため 003-ref も確認**

Run:
```bash
./target/release/fulgur render target/wpt/css/css-page/page-background-003-print-ref.html -o /tmp/zsn8_003.pdf
pdfinfo /tmp/zsn8_003.pdf | grep Pages
```

Expected: WPT runner が報告する数字 (`ref=4`) と一致。

**Step 3: コミット不要 (verification step)**

---

## Task 4: WPT を再実行して影響範囲を測る

**Files:**
- Generate: `target/wpt-report/css-page/regressions.json` (新版)
- Compare: `/tmp/zsn8_baseline_regressions.json`

**Step 1: WPT 全 css-page を再実行**

Run: `cargo test -p fulgur-wpt --test wpt_css_page --release -- --nocapture 2>&1 | tail -15`

Expected: 走り切る (個別 assertion fail は OK、issue 修正前なので)。

**Step 2: 新 regressions と baseline を比較**

Run:
```bash
diff <(jq -r '.[].test' /tmp/zsn8_baseline_regressions.json | sort) \
     <(jq -r '.[].test' target/wpt-report/css-page/regressions.json | sort)
```

Expected: **差分なし、または `page-background-002/003` 関連のみが追加/削除**。それ以外の test が変動した場合は要調査 (PNG decode が他の test の挙動を変えた可能性)。

**Step 3: 差分があった場合は user に報告して相談**

差分が `page-background-002/003` 以外を含む場合、本タスクで修正範囲を再検討する必要があるため停止して user 相談。

**Step 4: コミット不要 (verification step)**

---

## Task 5: page-background-002/003 を expected=FAIL に降格

**Files:**
- Modify: `crates/fulgur-wpt/expectations/css-page.txt`

**Step 1: 該当行の現状を確認**

Run: `grep "page-background-00[23]" crates/fulgur-wpt/expectations/css-page.txt`

Expected:
```text
PASS  css/css-page/page-background-002-print.html
PASS  css/css-page/page-background-003-print.html
```

**Step 2: PASS → FAIL に変更**

`crates/fulgur-wpt/expectations/css-page.txt` の該当 2 行を以下に変更:

```text
FAIL  css/css-page/page-background-002-print.html
FAIL  css/css-page/page-background-003-print.html
```

(注: コメントを行末や別行に追加できるならば `# fulgur-zsn8: layout bug for <img position:absolute>, see issue X` を入れる。expectations file の format が `STATUS  PATH` のシンプルな形式である場合は、追加コメントは expectations.rs パーサが許容するなら入れる、許容しないなら別場所に記録)。

expectations file のフォーマット確認:

Run: `head -20 crates/fulgur-wpt/expectations/css-page.txt && grep -E "^#|^//" crates/fulgur-wpt/expectations/css-page.txt | head -3`

Expected: コメント形式が分かる。コメント形式が `#` で始まるなら、降格 2 行の直前に説明コメントを 1 行追加 (`# fulgur-zsn8: <img position:absolute> layout bug, tracked in <new issue id>`)。

**Step 3: WPT を再実行して regression が消えるか確認**

Run: `cargo test -p fulgur-wpt --test wpt_css_page --release -- --nocapture 2>&1 | tail -10`

Run: `jq -r '.[].test' target/wpt-report/css-page/regressions.json | grep "page-background-00[23]"`

Expected: `page-background-002/003` が **regression list から消える** (declared=FAIL observed=FAIL の整合)。

**Step 4: コミット**

```bash
git add crates/fulgur-wpt/expectations/css-page.txt
git commit -m "test(fulgur-wpt): demote page-background-002/003 to expected=FAIL

Both ref pages contain <img position:absolute; z-index:-1; src=cat.png>,
which fulgur currently treats as in-flow content. The image's intrinsic
height (74.25pt) exceeds the test page's 37.5pt content area, so fulgur
emits an extra page solely for the image. The test renders show 3 pages
(correct), the ref renders show 4 (buggy), tripping a page-count
mismatch in the WPT runner.

The underlying layout bug — non-pseudo absolutely-positioned children
have no re-emit path in convert.rs:2666-2678 — is tracked separately.
Demote to FAIL so the runner stops flagging this as a fresh regression
on every nightly until the layout fix lands."
```

---

## Task 6: 絶対配置 layout fix の新 beads issue を起票

**Files:**
- Beads (no file change)

**Step 1: 新 issue を `bd create` で起票**

Run:
```bash
bd create --title="bug: <img position:absolute> 非疑似要素が pagination で 1 ページ消費する" \
  --type=bug --priority=2 \
  --description='`<img position:absolute>` の非疑似要素 (regular `<img>`) が pagination 中に in-flow 扱いになり、画像高がページ高を超える場合に独立ページを占有する。

## 再現

```html
<style>@page { size: 300px 50px; margin: 0; }</style>
<img style="position:absolute; top:0; left:0;" src="cat.png">
Cat head.
<div style="break-before:page;">Cat body.</div>
<div style="break-before:page;">No cat parts on this page.</div>
```

期待: 3 ページ (Cat head/body/No cat parts、画像は absolute なので flow から除外)
実際: 4 ページ (page 1 = 画像のみ、page 2-4 = 本文)

## 根本原因

`crates/fulgur/src/convert.rs:2666-2678` のコメント通り、絶対配置の **疑似要素** (`::before` / `::after`) には `build_absolute_pseudo_children` で containing block へ再 emit するパスがあるが、**非疑似要素** には re-emit パスが**未実装**で `convert_node` にフォールスルーする。結果として in-flow 扱いになり pagination で page を消費する。

## 修正方針

`build_absolute_pseudo_children` の non-pseudo 版を実装し、絶対配置の `<img>` / `<div>` 等を containing block の child として再 emit する。CSS §10.3.7 / §10.6.4 に従い CB を walk up で発見、`PositionedChild` として再構築。

## 関連

- 親 issue: fulgur-zsn8 (build 非決定性で発覚)
- 暫定対応: page-background-002/003 を expected=FAIL に降格済み (fulgur-wpt expectations)

## 受け入れ基準

1. 上記再現 HTML の page count が 3 になる
2. `crates/fulgur-wpt/expectations/css-page.txt` で `page-background-002/003` を PASS に戻して WPT が pass する
3. 既存 fulgur unit test に regression なし
4. CSS spec (§10.3.7 / §10.6.4) との整合性をテストでカバー'
```

**Step 2: 新 issue ID を取得して fulgur-zsn8 に depends-on を張る**

Run: `bd ready 2>&1 | grep "position:absolute" | head -1`

新 issue ID を取得 (例: `fulgur-XXXX`)。

Run: `bd dep add <new-id> fulgur-zsn8 2>&1`

(意味: 新 issue は fulgur-zsn8 に depends — つまり fulgur-zsn8 を完了してから新 issue を解決する流れ。但し実際は新 issue が独立に取り組めるので、ここは `bd dep add fulgur-zsn8 <new-id>` の方向 (zsn8 が新 issue を blocks する関係) は不適。むしろ dep は不要で、design field で参照するだけで十分。)

**判断: dep は張らず、design に新 issue ID を文字列で記録する**。理由: 新 issue は独立して着手可能、zsn8 はこのブランチで close 予定なので chain にしても意味がない。

**Step 3: コミット不要 (beads-only change)**

---

## Task 7: fulgur-zsn8 の description と design を更新

**Files:**
- Beads (no file change)

**Step 1: description を真因ベースに書き換え**

`bd update fulgur-zsn8 --description "..."` で以下に置換:

```markdown
build configuration によって `<img position:absolute>` の扱いが変わり、CLI と WPT runner で page count が乖離する。

症状:
- page-background-002: declared=PASS observed=FAIL — page count mismatch: test=3 ref=4
- page-background-003: declared=PASS observed=FAIL — page count mismatch: test=2 ref=4

原因 (本 issue で対処):
- `blitz-dom-0.2.4` が `image = { default-features = false }` で全 image format を無効化
- fulgur-cli は image を直接依存しないため PNG decode 不能 → ref は 3 ページ
- fulgur-wpt は `image = features=["png"]` を直接依存するため PNG decode 可能 → ref は 4 ページ
- 結果として CLI と WPT で page count が乖離

修正:
- fulgur lib に `image = features=["png"]` を追加して全 caller で PNG decode を統一
- 本 issue は build 非決定性 (layer 1) のみ対処
- 真の layout bug (`<img position:absolute>` 非疑似要素が page を消費する) は fulgur-XXXX で別途対応
- page-background-002/003 は fulgur-XXXX が解決するまで expected=FAIL に暫定降格

当初仮説 "panic 後 global state 汚染" は debunked (panic は無関係、build 設定の差が真因)。
```

`fulgur-XXXX` は Task 6 で起票した新 issue ID に置換。

**Step 2: design field の "新 issue X" 部分を実 ID に置換**

Run: `bd show fulgur-zsn8 2>&1 | grep -A2 "新 issue X"`

Run: `bd update fulgur-zsn8 --design "..."` で `新 issue X` を実 ID (`fulgur-XXXX`) に sed-style 置換 (design 全文を再投入)。

**Step 3: コミット不要 (beads-only)**

---

## Task 8: 最終検証

**Files:**
- Read-only

**Step 1: 全 lib テスト**

Run: `cargo test -p fulgur --lib 2>&1 | tail -5`

Expected: 全 pass。

**Step 2: 全 CLI テスト + WPT**

Run:
```bash
cargo test -p fulgur-cli 2>&1 | tail -5
cargo test -p fulgur-wpt 2>&1 | tail -10
```

Expected: 全 pass、もしくは元から fail していたものだけが残る (新規 regression なし)。

**Step 3: clippy + fmt**

Run:
```bash
cargo clippy --all-targets 2>&1 | tail -10
cargo fmt --check
```

Expected: warning / error なし。

**Step 4: bd sync (export only)**

Run: `bd sync --flush-only`

**Step 5: git status 最終確認**

Run: `git status --short && git log --oneline -5`

Expected: コミット済み変更のみ、untracked は調査用 example のみ (commit しない)。
