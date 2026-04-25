# fulgur-y9pu: 100vh + page-break-after blank-leading-page regression net

> **For Claude:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to
> implement this plan task-by-task.

**Goal:** v0.5.13 で再現していた "`height: 100vh` + `page-break-after: always`
で先頭に空白ページが挿入される" bug が HEAD では既に PR #188
(`fulgur-lje5-page-break-wire`) で修正済みであることを確認し、再発を防ぐ
VRT regression net を追加する。

**Architecture:** paginate.rs / pageable.rs / convert.rs にコード変更は加え
ない (修正は既に取り込み済み)。`crates/fulgur-vrt/fixtures/bugs/` に
GCPM margin box + cover (`100vh`) + `page-break-after` + 続く本文 div を含む
最小 HTML fixture を追加し、`goldens/fulgur/bugs/<name>.pdf` を golden として
固定する。これにより v0.5.13 と同じ wiring 漏れが入った場合は PDF byte 比較で
失敗する。

**Tech Stack:** fulgur-vrt (PDF byte-compare + manifest.toml)、fulgur CLI
(golden 生成・確認)、pdfinfo / pdftotext (検証)、bundled Noto Sans
(`FONTCONFIG_FILE`).

---

## 前提

このプランは worktree
`/home/ubuntu/fulgur/.worktrees/fulgur-y9pu`
(branch `fix/fulgur-y9pu-page-break-vrt`) で実行する。worktree は既に作成済みで
`sparse-checkout disable` も実施済み。

`bd update fulgur-y9pu --design ...` は完了済みなので、実装途中で issue 内容を
読む必要は無い。完了時に NOTES を追記する。

---

## Task 1: 最小再現 HTML fixture を作成

**Files:**

- Create: `crates/fulgur-vrt/fixtures/bugs/cover-page-break-after.html`

**Step 1: HTML を書く**

issue の minimal repro (cover + 1 div) は v0.5.13 でも 2 pages なので、それ単体
では regression net にならない。`fulgur-skills/fulgur-review.html` で再現する
要素 (GCPM margin box + universal selector + `linear-gradient`) を加えた最小
構造にする。

`crates/fulgur-vrt/fixtures/bugs/cover-page-break-after.html` を以下の内容で作成:

```html
<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>VRT fixture: bugs/cover-page-break-after</title>
<style>
  /* Regression net for fulgur-y9pu: in v0.5.13 the legacy
     `page-break-after: always` was not wired to `Pagination.break_after`,
     so a `height: 100vh` cover followed by `page-break-after: always`
     produced a spurious blank leading page. Fixed by PR #188
     (fulgur-lje5-page-break-wire). */
  @page {
    size: A4;
    margin: 20mm 18mm;
    @top-center { content: "y9pu"; font-size: 9pt; color: #888; }
    @bottom-center {
      content: counter(page) " / " counter(pages);
      font-size: 9pt; color: #888;
    }
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { background: #fff; }
  .cover {
    height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    background: linear-gradient(135deg, #0f3460 0%, #16213e 100%);
    color: #fff;
    page-break-after: always;
  }
  .cover-logo { font-size: 56pt; font-weight: 900; color: #e94560; }
  .body-section { padding: 12pt; }
  .body-section h1 {
    font-size: 20pt;
    color: #0f3460;
    border-bottom: 3pt solid #e94560;
    padding-bottom: 6pt;
  }
</style>
</head><body>
<div class="cover">
  <div class="cover-logo">fulgur</div>
</div>
<div class="body-section">
  <h1>1. Overview</h1>
</div>
</body></html>
```

**Step 2: HTML 内容のコミット (golden は別 commit)**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-y9pu
git add crates/fulgur-vrt/fixtures/bugs/cover-page-break-after.html
git commit -m "test(fulgur-vrt): add cover-page-break-after fixture (fulgur-y9pu)"
```

---

## Task 2: HEAD で 2 pages レンダーされることを確認

**Files:**

- 確認のみ (write なし)

**Step 1: fulgur CLI で fixture を render**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-y9pu
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  cargo run --bin fulgur --quiet -- render \
  crates/fulgur-vrt/fixtures/bugs/cover-page-break-after.html \
  -o /tmp/y9pu-fixture.pdf
```

**Step 2: page count と page 1 の内容を確認**

```bash
pdfinfo /tmp/y9pu-fixture.pdf | grep -i pages
pdftotext -f 1 -l 1 /tmp/y9pu-fixture.pdf - 2>/dev/null
```

期待:

- `Pages: 2`
- page 1 のテキストに `fulgur` (cover-logo) が含まれている (空白ページではない)

**もし `Pages: 1` なら**: cover の `100vh` が `page-break-after` 抜きで成立して
1 ページに収まっている。`.cover` 内に `min-height: 100vh` と少しの padding を
増やして 100vh を強制する。

**もし `Pages: 3+` なら**: HEAD で **regression が発生している** ことを意味する。
plan を中断し `bd show fulgur-y9pu` を再評価する (PR #188 が revert されたか、
別 path に同じ wiring 漏れがあるか調査)。

---

## Task 3: manifest.toml に fixture を登録

**Files:**

- Modify: `crates/fulgur-vrt/manifest.toml`

**Step 1: bugs セクションに新しいエントリを追加**

`grid-row-promote-background.html` のエントリ直後に挿入:

```toml
[[fixture]]
path = "bugs/cover-page-break-after.html"
```

**Step 2: TOML が valid か軽く確認**

```bash
cargo run --bin fulgur --quiet -- --version  # workspace ロードで syntax error なら出る
```

(fulgur-vrt は manifest.toml を起動時に parse するが、ここでは登録のみ。golden
は次の task で生成する。)

---

## Task 4: golden PDF を生成

**Files:**

- Create: `crates/fulgur-vrt/goldens/fulgur/bugs/cover-page-break-after.pdf`
  (fulgur-vrt が `FULGUR_VRT_UPDATE=1` で自動生成)

**Step 1: golden を生成**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-y9pu
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  FULGUR_VRT_UPDATE=1 \
  cargo test -p fulgur-vrt --test '*' cover_page_break_after 2>&1 | tail -10
```

**Step 2: golden が生成されたことを確認**

```bash
ls -la crates/fulgur-vrt/goldens/fulgur/bugs/cover-page-break-after.pdf
pdfinfo crates/fulgur-vrt/goldens/fulgur/bugs/cover-page-break-after.pdf | grep Pages
```

期待: ファイルが存在し、`Pages: 2`。

---

## Task 5: VRT regression test が PASS することを確認

**Files:**

- 確認のみ

**Step 1: VRT を `FULGUR_VRT_UPDATE` 抜きで実行 (byte-compare)**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-y9pu
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  cargo test -p fulgur-vrt 2>&1 | tail -20
```

期待:

- `cover_page_break_after` テスト (fixture 名から動的生成) が PASS
- 既存 fixture も全 PASS (regression なし)

**もし fail なら**:

- 出力された `target/vrt-diff/bugs/cover-page-break-after.diff.png` を確認
- 環境差 (font fallback など) が原因なら fixture を調整

**Step 2: golden をコミット**

```bash
git add crates/fulgur-vrt/manifest.toml crates/fulgur-vrt/goldens/fulgur/bugs/cover-page-break-after.pdf
git commit -m "test(fulgur-vrt): seed golden for cover-page-break-after (fulgur-y9pu)"
```

---

## Task 6: WPT zero-height-page-break-001-print が PASS のままであることを確認

**Files:**

- 確認のみ

**Step 1: WPT runner を該当 reftest だけで実行**

```bash
cd /home/ubuntu/fulgur/.worktrees/fulgur-y9pu
FONTCONFIG_FILE="$PWD/examples/.fontconfig/fonts.conf" \
  cargo test -p fulgur-wpt zero_height_page_break_001 2>&1 | tail -10
```

(test 名は fulgur-wpt の生成規則に従う。fail/見つからない場合は
`cargo test -p fulgur-wpt -- --list 2>&1 | grep -i zero_height` で正確な名前
を探す。)

期待: PASS (あるいは expected-pass として bugs.txt に登録済み)。

`expectations/lists/bugs.txt` に該当 reftest が登録されていることも grep で確認:

```bash
grep -n "zero-height-page-break-001" expectations/lists/bugs.txt
```

---

## Task 7: 完了処理 — issue NOTES に修正コミット情報を追記

**Files:**

- 確認のみ (bd update で更新)

**Step 1: NOTES を更新**

```bash
bd update fulgur-y9pu --notes "$(cat <<'EOF'
## WPT reftest 対応 (2026-04-24)

**Regression net only** — この bug は paginate.rs 内部のバグで、厳密一致する
WPT reftest は見つからなかった。

近接メカニズム reftest:

- `css/css-break/zero-height-page-break-001-print.html` (break-after:page 直後の空ページ抑制、方向は逆) → **fulgur で PASS**

`expectations/lists/bugs.txt` に PASS regression net として登録済み。

## 修正確認 (2026-04-25)

`v0.5.13` で再現していた症状は HEAD (`v0.5.14`) では PR #188
(`fulgur-lje5-page-break-wire`, commit `1a6dd3c`) で既に修正済み。CSS の
`page-break-after`/`page-break-before` legacy alias を `Pagination.break_after`/
`break_before` に wire するロジックがこの PR で convert.rs に追加された。

検証:
- `v0.5.13` の HEAD (`2ce2747`) で `fulgur-skills/fulgur-review.html` を render
  → 11 pages、page 1 が空白で issue を完全再現
- HEAD (`3d4d0c1`) で同 HTML を render → 9 pages、page 1 = カバーとなり期待通り

本 issue では追加修正は行わず、`crates/fulgur-vrt/fixtures/bugs/cover-page-break-after.html`
として regression net VRT fixture を追加してクローズする。
EOF
)"
```

**Step 2: 確認**

```bash
bd show fulgur-y9pu | head -100
```

NOTES に上記内容が反映されていれば OK。

---

## 実行順序とコミット粒度

- Task 1 → commit (HTML fixture のみ)
- Task 2 → 確認のみ
- Task 3 → Task 4, 5 と同時 commit (manifest.toml + golden PDF)
- Task 4 → golden 生成 (Task 5 commit に含める)
- Task 5 → byte-compare PASS 確認 + commit
- Task 6 → 確認のみ (PASS のままなら追加変更なし)
- Task 7 → NOTES update (bd 経由、code commit には含まれない)

最終的なコード変更:

- `crates/fulgur-vrt/fixtures/bugs/cover-page-break-after.html` (新規)
- `crates/fulgur-vrt/manifest.toml` (1 行追加)
- `crates/fulgur-vrt/goldens/fulgur/bugs/cover-page-break-after.pdf` (新規)
- `docs/plans/2026-04-25-fulgur-y9pu-cover-blank-page-vrt.md` (本ファイル)

paginate.rs / pageable.rs / convert.rs は **触らない**。
