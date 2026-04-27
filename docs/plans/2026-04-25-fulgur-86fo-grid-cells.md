# fulgur-86fo Grid Cell Background & Row Stretch Fix Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** `.usecase-grid` の 2-3 枚目の background が描画されないバグ、および `.feature-grid` で行高 stretch によりセル位置がずれる + grid 下に巨大余白が残るバグを修正し、fulgur-vrt に regression fixture を追加する。

**Architecture:** systematic-debugging で各バグの primary root cause を bisect で特定し、ピンポイントに修正する。VRT 用最小再現 fixture を `crates/fulgur-vrt/fixtures/bugs/` に追加して将来の再発を防ぐ。WPT regression net (fulgur-eye4 の 7 PASS) を維持。

**Tech Stack:** Rust, Taffy (grid layout), Blitz/Stylo (style resolution), Krilla (PDF), fulgur-vrt (byte-wise PDF golden compare).

---

## Pre-flight context

- Worktree: `/home/ubuntu/fulgur/.worktrees/fulgur-86fo`, branch `fix/fulgur-86fo-grid-cells`
- 検証済み: 現行 main で `fulgur-skills/fulgur-review.html` の p3-5 にバグ再現
- fulgur 内に grid 固有の専用コードはほぼ無く、Taffy 出力をそのまま消費している → 犯人候補は (a) `extract_block_style` 周辺、(b) child iteration、(c) Pageable→draw 経路
- 二つのバグは独立した可能性が高いので、**先に `.usecase-grid` (同一ページ内) → 後に `.feature-grid` (page break interaction)** の順で潰す

---

## Task 1: 診断用最小再現 HTML を /tmp に作成

**Files:**

- Create: `/tmp/fulgur-86fo-usecase.html`
- Create: `/tmp/fulgur-86fo-feature.html`

VRT に入れる前に高速イテレーション用の HTML を /tmp に置く（goldens を毎回更新せずに済む）。

**Step 1: usecase-grid の最小再現を作る**

`fulgur-skills/fulgur-review.html` から `.usecase-grid` セクションだけを抜き出し、外部依存（フォント、画像）を排した最小 HTML を作る。

```html
<!DOCTYPE html>
<html><head><meta charset="utf-8"><style>
html, body { margin: 0; padding: 20pt; font-family: sans-serif; }
.usecase-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10pt;
}
.usecase-card {
  background: #0f3460;
  color: #fff;
  border-radius: 8pt;
  padding: 12pt;
  text-align: center;
}
.usecase-icon { font-size: 22pt; margin-bottom: 6pt; }
.usecase-title { font-size: 9.5pt; font-weight: 700; color: #a8dadc; margin-bottom: 4pt; }
.usecase-desc { font-size: 8.5pt; color: #ccc; line-height: 1.5; }
</style></head><body>
<div class="usecase-grid">
  <div class="usecase-card">
    <div class="usecase-icon">A</div>
    <div class="usecase-title">Card 1</div>
    <div class="usecase-desc">First card content here.</div>
  </div>
  <div class="usecase-card">
    <div class="usecase-icon">B</div>
    <div class="usecase-title">Card 2</div>
    <div class="usecase-desc">Second card content here.</div>
  </div>
  <div class="usecase-card">
    <div class="usecase-icon">C</div>
    <div class="usecase-title">Card 3</div>
    <div class="usecase-desc">Third card content here.</div>
  </div>
</div>
</body></html>
```

**Step 2: feature-grid の最小再現を作る**

同様に `.feature-grid` (2x3) を最小化。

**Step 3: 現行コードでレンダリングしてバグ再現を確認**

```bash
cargo build -p fulgur-cli
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  ./target/debug/fulgur render /tmp/fulgur-86fo-usecase.html -o /tmp/usecase.pdf
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  ./target/debug/fulgur render /tmp/fulgur-86fo-feature.html -o /tmp/feature.pdf
pdftocairo -png -r 100 /tmp/usecase.pdf /tmp/usecase
pdftocairo -png -r 100 /tmp/feature.pdf /tmp/feature
```

期待: usecase で 1 枚目だけ navy / 2-3 枚目透明、feature で行ずれが出る。

**注意:** ここで再現が取れない場合 (=外部依存を抜いたら直る) は、`fulgur-review.html` の他 CSS との interaction が原因。その場合は段階的に CSS を足して再現条件を絞る。

---

## Task 2: usecase-grid バグの primary cause 診断

**Files:**

- Read: `crates/fulgur/src/convert.rs:170-610` (convert_node + convert_node_inner)
- Read: `crates/fulgur/src/convert.rs:2840-3000` (extract_block_style)

`extract_block_style` は node ID で呼ばれるので、grid item 全てに対して呼ばれているはず。まず確認:

**Step 1: extract_block_style 呼び出しに print を入れる**

`crates/fulgur/src/convert.rs:2864` 付近に一時的な eprintln! を追加:

```rust
let bg = styles.clone_background_color();
eprintln!("[fulgur-86fo] node_id={node_id:?} bg={bg:?}");
```

(node_id がスコープにあるか確認、なければ呼び出し側で id を渡す)

**Step 2: 最小再現 HTML を debug build でレンダリング**

```bash
cargo build -p fulgur-cli 2>&1 | tail -3
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  ./target/debug/fulgur render /tmp/fulgur-86fo-usecase.html -o /tmp/usecase.pdf 2>&1 \
  | grep "fulgur-86fo"
```

3 枚のカード分の bg が `Some(#0f3460)` で出ていれば「style は取れている → draw か Pageable 構築段の問題」、出ていなければ「style 解決か iteration の問題」。

**Step 3: 結果に応じて bisect**

- bg が全て取れている → BlockPageable の background_layers が draw 段で何を持っているか確認 (`pageable.rs` で background draw 経路を辿る)
- bg が一部しか出ていない → child iteration の見落とし、または node visibility check で skip されている

**Step 4: root cause を文書化**

issue の notes に診断結果を append。

---

## Task 3: usecase-grid バグの修正

**Files:**

- Modify: 診断で特定したファイル（候補: `crates/fulgur/src/convert.rs`, `crates/fulgur/src/render.rs`, `crates/fulgur/src/pageable.rs`)

**Step 1: 修正コミット前にユニットテストを書く（可能なら）**

`extract_block_style` または該当関数に対して、grid 子要素 3 つで全て background 取得できることを確認するテストを追加。BlockPageable レベルで「3 child の background_layers が全て non-empty」をアサート。

**Step 2: 修正実装**

最小限の変更でバグを潰す。fulgur 哲学に従い、不要な抽象化や future-proofing は入れない。

**Step 3: ユニットテスト + 既存テストを実行**

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```

既存 ~340 テスト + 新規テストが全 PASS。

**Step 4: 最小再現で目視確認**

eprintln を削除した上で:

```bash
cargo build -p fulgur-cli 2>&1 | tail -3
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  ./target/debug/fulgur render /tmp/fulgur-86fo-usecase.html -o /tmp/usecase.pdf
pdftocairo -png -r 100 /tmp/usecase.pdf /tmp/usecase
```

3 枚とも navy 背景が出ていれば OK。

**Step 5: コミット**

```bash
git add crates/fulgur/src/<modified-file> crates/fulgur/src/<test-file>
git commit -m "fix(convert): render background for all grid cells (fulgur-86fo)

<root cause 1-2 lines>"
```

---

## Task 4: usecase-grid VRT fixture を追加

**Files:**

- Create: `crates/fulgur-vrt/fixtures/bugs/grid-cells-background.html`
- Modify: `crates/fulgur-vrt/manifest.toml`
- Create: `crates/fulgur-vrt/goldens/fulgur/bugs/grid-cells-background.pdf`

**Step 1: fixture HTML を作成**

Task 1 の `/tmp/fulgur-86fo-usecase.html` をコピーするが、フォント依存を避けるため text を ASCII のみ + 短く。`crates/fulgur-vrt/README.md` の "Cross-environment determinism" を遵守。

**Step 2: manifest.toml にエントリ追加**

```toml
[[fixture]]
path = "bugs/grid-cells-background.html"
```

**Step 3: golden を生成**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  FULGUR_VRT_UPDATE=1 cargo test -p fulgur-vrt 2>&1 | tail -5
```

**Step 4: golden を目視確認**

```bash
pdftocairo -png -r 100 crates/fulgur-vrt/goldens/fulgur/bugs/grid-cells-background.pdf /tmp/golden-usecase
```

3 枚とも navy 背景になっているか目視確認。

**Step 5: 再実行で byte-wise compare PASS を確認**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -5
```

**Step 6: コミット**

```bash
git add crates/fulgur-vrt/fixtures/bugs/grid-cells-background.html \
        crates/fulgur-vrt/manifest.toml \
        crates/fulgur-vrt/goldens/fulgur/bugs/grid-cells-background.pdf
git commit -m "test(fulgur-vrt): add grid-cells-background regression fixture (fulgur-86fo)"
```

---

## Task 5: feature-grid バグ (行 stretch / アイコンずれ) の primary cause 診断

**Files:**

- Read: `crates/fulgur/src/convert.rs` (Taffy `final_layout` 取得周辺)
- Read: `crates/fulgur/src/paginate.rs` (page split 時の Y 計算)

`.feature-grid` は 2x3、ページまたぎが起きる。3 行目以降が次ページに送られる際の Y 計算 + 行高 stretch が疑わしい。

**Step 1: 最小再現 HTML を debug build でレンダリング**

`/tmp/fulgur-86fo-feature.html` をレンダリング、目視確認。

```bash
./target/debug/fulgur render /tmp/fulgur-86fo-feature.html -o /tmp/feature.pdf
pdftocairo -png -r 100 /tmp/feature.pdf /tmp/feature
```

期待症状: 右カラムのカード上端が左カラムより上に浮く + grid 下に余白。

**Step 2: Taffy 出力を dump**

`convert.rs` で grid item の `final_layout.location.y` を eprintln! で確認:

```rust
eprintln!("[fulgur-86fo-feature] node_id={:?} y={} h={} (px)", node_id,
  node.final_layout.location.y, node.final_layout.size.height);
```

3 行 × 2 列 = 6 セルの Y/H が想定通りか確認:
- 行 0 (上): y=0, h=同
- 行 1 (中): y=h_row0+gap, h=同
- 行 2 (下): y=...

ずれていれば Taffy 自体の問題（→ 原因はおそらく fulgur が Taffy に渡している constraint）。揃っていれば fulgur 側の draw/paginate Y 計算の問題。

**Step 3: 仮説に応じて深掘り**

ケース別の bisect を進めて root cause を特定。

**Step 4: root cause を notes に記録**

---

## Task 6: feature-grid バグの修正

**Files:**

- Modify: 診断で特定したファイル

**Step 1: 修正実装 + 必要ならユニットテスト追加**

**Step 2: 既存テスト実行**

```bash
cargo test -p fulgur --lib 2>&1 | tail -10
```

**Step 3: 最小再現で目視確認**

```bash
cargo build -p fulgur-cli && \
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  ./target/debug/fulgur render /tmp/fulgur-86fo-feature.html -o /tmp/feature.pdf && \
pdftocairo -png -r 100 /tmp/feature.pdf /tmp/feature
```

行が揃って余白も解消されているか確認。

**Step 4: コミット**

```bash
git commit -m "fix(<module>): correct grid row sizing across page boundaries (fulgur-86fo)

<root cause>"
```

---

## Task 7: feature-grid VRT fixture を追加

**Files:**

- Create: `crates/fulgur-vrt/fixtures/bugs/grid-row-stretch.html`
- Modify: `crates/fulgur-vrt/manifest.toml`
- Create: `crates/fulgur-vrt/goldens/fulgur/bugs/grid-row-stretch.pdf`

ページまたぎを誘発するために `min-height` または十分なコンテンツで page-1 ぎりぎりまで埋める fixture を作る。

**Step 1: fixture 作成**

A4 だと行高調整が面倒なので、もし可能ならページ境界をまたがず「同一ページ内で 2x3 grid の行高が揃う」だけをテストする fixture でも regression を取れる。実装難度を見て選ぶ。

**Step 2-6: Task 4 と同パターンで manifest 追加 → golden 生成 → 目視 → 再 PASS → コミット**

---

## Task 8: fulgur-review.html の最終目視確認

**Step 1: release build で fulgur-review.html をレンダリング**

```bash
cargo build --release -p fulgur-cli 2>&1 | tail -3
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  ./target/release/fulgur render /home/ubuntu/fulgur-skills/fulgur-review.html -o /tmp/review-fixed.pdf
pdftocairo -png -r 100 /tmp/review-fixed.pdf /tmp/review-fixed
```

**Step 2: p3, p4, p5 を目視確認**

- p3: `.feature-grid` の 4 セルが揃って表示、アイコン位置正常
- p4: `.feature-grid` の 5-6 セル目が正常、grid 下の余白解消
- p5: `.usecase-grid` 3 セルとも navy 背景、color: #fff のテキスト読める

問題があれば Task 2 / Task 5 に戻って再診断。

---

## Task 9: WPT regression net (fulgur-eye4 の 7 PASS) を維持確認

**Step 1: WPT bugs.txt テストを実行**

```bash
cargo test -p fulgur-wpt --test wpt_lists -- wpt_list_bugs 2>&1 | tail -10
```

7 PASS / 2 FAIL (declared) / 0 regressions / 0 promotions が維持されていることを確認。

**Step 2: 既存 fulgur-vrt 全 fixture が PASS**

```bash
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" cargo test -p fulgur-vrt 2>&1 | tail -5
```

**Step 3: 全体 fmt + clippy**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

問題なければ Task 完了。fmt エラーなら `cargo fmt` で修正してコミット。

---

## Task 10: ブランチ完了処理

**REQUIRED SUB-SKILL:** superpowers:finishing-a-development-branch

ブランチ完了処理 (PR 作成 or merge オプション提示)。

---

## Implementation note (post-execution)

実装時、VRT fixture の名前を以下のように変更した:

- 計画時: `crates/fulgur-vrt/fixtures/bugs/grid-cells-background.html` (Task 4)
- 実装時: `crates/fulgur-vrt/fixtures/bugs/grid-row-promote-background.html`

理由: bug の本質が「grid 行内で parallel sibling が次ページへ promote された際に背景が消える」であり、ファイル名で「row promote」を明示した方が将来の読み手 (人間 / AI レビュアー) にとって意図が伝わりやすいため。`manifest.toml` のエントリも同名に統一済み。
