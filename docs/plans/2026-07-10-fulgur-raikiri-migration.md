---
title: fulgur — raikiri migration plan (Phase 1: blitz-* 追放)
status: Draft
date: 2026-07-10
author: Mitsuru Hayasaka (@mitsuru)
related:
  - "raikiri design doc: `raikiri/docs/design/2026-07-10-streaming-layout-engine-design.md`"
  - "Engine layout API: `docs/plans/2026-06-27-engine-layout-api-design.md`"
  - "Coordinate rules: `.claude/rules/coordinate-system.md`"
---

# fulgur — raikiri migration plan (Phase 1: blitz-* 追放)

## 1. 背景

fulgur は現在、HTML/CSS を PDF に変換するために [Blitz](https://github.com/DioxusLabs/blitz)
(`blitz-html`, `blitz-dom`, `blitz-traits`) に依存している。Blitz は browser 前提の
設計で、fulgur の想定用途(サーバー側 PDF 生成、大規模文書、決定論、パイプライン処理)
との構造的摩擦がある。全ての Blitz API 呼び出しは `crates/fulgur/src/blitz_adapter.rs`
(7,114 行) に集中している。

これに対応するため、独立ワークスペース [raikiri](https://github.com/fulgur-rs/raikiri)
を新設し、Blitz を追放する。raikiri は fulgur family の Phase 1 に相当。詳しい設計
は raikiri の design doc 参照。本 doc は **fulgur 側で何が変わるか**、どういう順で
移行するかを扱う。

## 2. スコープ

### In-scope (本 plan で扱う)

- fulgur が `blitz-html` / `blitz-dom` / `blitz-traits` を直接 dep しないようにする
- fulgur が `cssparser` を直接 dep しないようにする(raikiri 側の責務に移す。
  M4 で `column_css.rs` を、M8 で `gcpm/parser.rs` を raikiri に移設)
- `blitz_adapter.rs` を raikiri を叩く薄いラッパに書き換える(rename も含む)
- fulgur の layout 用ロジック(`convert/`, `pagination_layout.rs` 等)は基本無変更、
  raikiri の API 越しに動くようにする
- feature flag `raikiri-engine` による段階移行(default off で始まり、M9 で default on)
- `fulgur-wpt` を feature flag 両モードで走らせる CI 整備

### Out-of-scope (別 plan で)

- flpdf postprocess の詳細実装 → Phase 2 の別 doc
- krilla → fulgur-paint 置換 → Phase 3 の別 doc
- raikiri 内部の実装 → raikiri repo 側

## 3. 現状分析

### 3.1 fulgur crates 一覧

```
crates/
├── fulgur/          # コア。blitz-* を dep
├── fulgur-cli/
├── fulgur-ruby/     # Ruby binding
├── fulgur-vrt/      # Visual regression testing
├── fulgur-wasm/
├── fulgur-wpt/      # WPT runner
├── pyfulgur/        # Python binding
```

blitz-* を dep するのは `fulgur` crate のみ。他 crate は fulgur crate 経由でのみ触れる。
つまり本 migration は `crates/fulgur/` の内側だけで完結する。

### 3.2 blitz が担っていること

`crates/fulgur/src/blitz_adapter.rs` を経由して、以下を利用:

- **DOM データモデル**: `BaseDocument`, `Node`, `NodeData`, `ElementData`, `TextData`
- **Style pipeline**: `doc.resolve(0.0)` (stylo cascade), `doc.set_viewport()`
- **Layout pipeline**: `impl LayoutPartialTree for BaseDocument` を taffy が消費
- **Text layout**: `parley::Layout<TextBrush>` が各 inline root に埋まる
- **Network provider**: `<link rel=stylesheet>` / `@import` を fulgur の
  `FulgurNetProvider` 経由で読み込む
- **Image data**: `<img>` の PNG/JPEG デコード

### 3.3 fulgur 側の関連ファイル(移行影響あり)

- `blitz_adapter.rs` (7,114 行) — 全 blitz API を集中隔離。**書き換え中心**
- `convert/*` — blitz DOM → fulgur Pageable。**基本無変更**(raikiri の Node が
  同構造なので)
- `pagination_layout.rs`, `multicol_layout.rs` — taffy を叩く。**基本無変更**
- `column_css.rs` — cssparser で multicol プロパティを直 parse (stylo 0.8 が
  非公開のため)。**M4 で raikiri-dom に移設**、fulgur から削除
- `gcpm/*` — GCPM 実装。**M8 で raikiri に引越し**(cssparser 使用の `gcpm/parser.rs`
  も含む)
- `paragraph.rs` — parley 出力を PDF に変換。**基本無変更**
- `render.rs` — Krilla を叩く。**Phase 1 では無変更**、Phase 3 で分割
- `engine.rs` — 上記を組み合わせる facade。**API 追加**(streaming 版)
- `net.rs` — `FulgurNetProvider`。**基本無変更**、raikiri 経由で使う
- `Cargo.toml` — dep 追加削除(`cssparser` は M8 で削除、`blitz-*` は M9 で削除)

## 4. Feature Flag による段階移行

`crates/fulgur/Cargo.toml`:

```toml
[features]
default = ["blitz-engine"]

# 現行の blitz stack
blitz-engine = [
    "dep:blitz-html", "dep:blitz-dom", "dep:blitz-traits",
]

# raikiri stack (M9 で default に昇格)
raikiri-engine = [
    "dep:raikiri", "dep:raikiri-html", "dep:raikiri-dom",
]

[dependencies]
raikiri = { version = "0.1", optional = true }
raikiri-html = { version = "0.1", optional = true }
raikiri-dom = { version = "0.1", optional = true }
blitz-html = { version = "0.2", optional = true }
blitz-dom = { version = "0.2", optional = true }
blitz-traits = { version = "0.2", optional = true }
```

**同時に両方を有効にすることは可能だが基本しない**(CI では両モードを分離して走らせ、
比較する)。

## 5. `blitz_adapter.rs` の書き換え

### 5.1 変更方針

`blitz_adapter.rs` は既に「fulgur 側全 blitz API 呼び出しの集中管理」を提供している
ので、そのファサード内で `#[cfg(feature = "raikiri-engine")]` / `#[cfg(feature =
"blitz-engine")]` の 2 実装を並置する。

**重要**: `pub use` している型は **rename** せず、両モードで同じ名前に見えるように
alias する:

```rust
// blitz_adapter.rs (Phase 1 移行中)

#[cfg(feature = "blitz-engine")]
pub use blitz_dom::{BaseDocument, Node, NodeData};

#[cfg(feature = "raikiri-engine")]
pub use raikiri::{Document as BaseDocument, Node, NodeData};
```

このため `convert/*` などの call site は無変更のまま両モードで通る。**これが移行を
成立させる key point**。

### 5.2 名称の変更

移行完了(M9)後、`blitz_adapter.rs` は `layout_adapter.rs` に rename する。
Phase 1 途中は blitz_adapter のまま(git blame の連続性のため)。

### 5.3 API 差の吸収パターン

blitz と raikiri で shape が違う API は 3 パターンで吸収:

**Pattern A: raikiri が blitz と同じ shape を提供** (ほとんどのケース):
```rust
// blitz と raikiri 両方に同名関数
pub fn resolve(doc: &mut Document) { doc.resolve(0.0); }
```

**Pattern B: raikiri で shape が変わったので adapter で吸収**:
```rust
// blitz は set_viewport(new_viewport) を要求、raikiri は set_viewport_size を要求
pub fn set_viewport_size_px(doc: &mut Document, w: f32, h: f32) {
    #[cfg(feature = "blitz-engine")]
    {
        let mut vp = doc.viewport().clone();
        vp.window_size = (w as u32, h as u32);
        doc.set_viewport(vp);
    }
    #[cfg(feature = "raikiri-engine")]
    doc.set_viewport_size(Px::new(w), Px::new(h));
}
```

**Pattern C: raikiri でしか意味を持たない**:
```rust
// e.g., PageStream API は raikiri のみ
#[cfg(feature = "raikiri-engine")]
pub fn page_stream<'a>(doc: &'a Document, cfg: PageBox) -> raikiri::PageStream<'a> {
    doc.page_stream(cfg)
}
```

Pattern C の call site は fulgur の `engine.rs` の新規 streaming API 側にのみ現れる。

### 5.4 `DomPass` trait の移植

fulgur の `blitz_adapter::DomPass` trait と `apply_passes` / `apply_single_pass`
関数は raikiri の `DocumentPass` trait に直接対応する。migration は 1:1:

| fulgur (現行) | raikiri (移植先) |
|---|---|
| `blitz_adapter::DomPass::apply(&self, doc: &mut HtmlDocument, ctx: &PassContext)` | `raikiri::DocumentPass::apply(&self, doc: &mut raikiri::Document, ctx: &PassContext)` |
| `blitz_adapter::apply_passes(doc, passes, ctx)` | `Engine::register_pass` 経由で自動適用 |
| `blitz_adapter::PassContext` | `raikiri::PassContext` |

**移植対象の pass**:

- `apply_link_media_rewrites` — `<link rel=stylesheet media=X>` の media list
  保存トリック。M2〜M5 の間に raikiri-html 側で port
- `RunningElementPass` — running element の走査。M5c の running element 実装で
  raikiri-dom 側に取り込み
- fulgur 側で今後追加される sanitization / 禁止タグ削除等の pass は raikiri の
  `DocumentPass` として実装、`Engine::register_pass` で登録する

feature flag 分岐は `blitz_adapter` 内の他の型と同様、`#[cfg(feature = ...)]` で
両モード対応。

## 6. `engine.rs` の API 追加

現行:
```rust
impl Engine {
    pub fn render(&self, html: &str) -> Result<Vec<u8>>;
    pub fn render_file(&self, html: &str, path: &Path) -> Result<()>;
    pub fn layout(&self, html: &str) -> Result<LayoutOutput>;   // 2026-06-27 API
}
```

追加:
```rust
impl Engine {
    /// Streaming rendering: PageFragment ごとに krilla + flpdf に流し、
    /// 定数メモリで PDF を生成。raikiri-engine feature 必須。
    #[cfg(feature = "raikiri-engine")]
    pub fn render_streaming(&self, html: &str, sink: Box<dyn RenderSink>)
        -> Result<()>;

    /// Streaming layout: raikiri の PageStream を直接返す。
    /// 高度な consumer (画像出力、OCR ラベル、debug) 向け。
    #[cfg(feature = "raikiri-engine")]
    pub fn layout_streaming<'a>(&'a self, html: &'a str)
        -> Result<PageStreamOwner<'a>>;
}
```

既存 `render()` / `render_file()` / `layout()` は両モードで動くよう内部を書き換える
(feature flag で impl 分岐)。

## 7. RenderSink の fulgur 実装

raikiri 側は `RenderSink` trait だけ export、実装は fulgur 側:

```rust
// crates/fulgur/src/render_sink.rs (新設)
use raikiri::RenderSink;

pub struct MemorySink {
    buffer: Vec<u8>,
    krilla_doc: krilla::Document,
    /* ... */
}

pub struct SpoolSink {
    tempfile: tempfile::NamedTempFile,
    krilla_doc: krilla::Document,
    /* ... */
}

pub struct DirectSink {
    file: BufWriter<File>,
    krilla_doc: krilla::Document,
    /* ... */
}

impl RenderSink for MemorySink { /* ... */ }
impl RenderSink for SpoolSink { /* ... */ }
impl RenderSink for DirectSink { /* ... */ }
```

**krilla 制約**: 現行 krilla は `Document::finish() -> Vec<u8>` なので、真の
per-page streaming は krilla の制約で不可能。Phase 1 では:

- `MemorySink`: krilla output を Vec<u8> で保持、consumer に渡す
- `SpoolSink`: krilla output を tempfile に書き出す
- `DirectSink`: krilla output を直接ユーザーの output file に書き出す

**中間 memory ピーク = 完成 PDF サイズ**が Phase 1 の実質下限。Phase 3 の
fulgur-paint 完成で真の O(1) streaming に到達。

### 7.1 選択ポリシー

```rust
pub enum SinkPolicy {
    ForceMemory,
    ForceSpool,
    Auto { memory_ceiling: usize }, // default: 32 MB
}
```

fulgur-cli:
```rust
let sink: Box<dyn RenderSink> = match (output_target, config.linearize) {
    ("-", _) => Box::new(MemorySink::new()),               // stdout
    (path, true) => Box::new(SpoolSink::new()?),           // linearize は spool
    (path, false) => Box::new(DirectSink::new(path)?),     // seekable file
};
engine.render_streaming(html, sink)?;
```

## 8. flpdf Postprocess の導入 (Phase 2 と重なるが Phase 1 でも一部)

Phase 1 で raikiri を default にする時点で最低限必要な flpdf 統合:

### 8.1 Phase 1 で入れる分

- **target-ref XObject 差替え** — raikiri が `Drawable::TargetRefSlot` を emit する
  ため、これを解決する consumer が必要。M5d / M8 で実装。
- **AnchorRegistry** の使用 — raikiri PageStream から各 anchor の解決を受け取り、
  flpdf で差替え

### 8.2 Phase 2 に送る分

- lopdf → flpdf swap(現行 lopdf 使用箇所の置換)
- outline / metadata / structure tree の注入
- linearize
- Phase 2 の別 doc で詳細化

## 9. `fulgur-wpt` の両モード運用

### 9.1 CI matrix

```yaml
# .github/workflows/wpt.yml
jobs:
  wpt:
    strategy:
      matrix:
        engine: [blitz, raikiri]
    steps:
      - uses: actions/checkout@v4
        with: { submodules: true }
      - run: cargo test -p fulgur-wpt --features engine-${{ matrix.engine }}
      - if: matrix.engine == 'raikiri'
        run: python scripts/diff-wpt-expectations.py \
             --blitz baseline.txt --raikiri current.txt > diff.md
      - uses: actions/upload-artifact@v4
        with: { name: wpt-${{ matrix.engine }}, path: diff.md }
```

### 9.2 Expectations 管理

- `crates/fulgur-wpt/expectations/css-page.txt` — blitz mode baseline(既存維持)
- `crates/fulgur-wpt/expectations/css-page.raikiri.txt` — raikiri mode 追加

**Blocking rules**:
- `blitz PASS && raikiri FAIL` → block、raikiri 側の bug として beads 作成
- `blitz FAIL && raikiri PASS` → CI で自動 PR、期待更新
- `both PASS` / `both FAIL` → 現状維持

### 9.3 fulgur-vrt (visual regression) の扱い

fulgur-vrt は Chromium とのピクセル比較を行う。raikiri と blitz で fulgur-vrt を
両方走らせ、Chromium との差を測る。**Chromium は共通 oracle**。raikiri が
Chromium により近づくなら勝ち。

## 10. Milestone Mapping (fulgur 側)

raikiri の M1〜M9 と対応:

| raikiri M | fulgur 側の作業 |
|---|---|
| M1 | fulgur の `Cargo.toml` に raikiri optional dep 追加、feature flag 追加、CI matrix 準備 |
| M2 | (待機) — raikiri が M2 gate 通過するまで |
| M3 | (待機) |
| M4 | `column_css.rs` を raikiri-dom に移設(cssparser 依存ごと)、fulgur 側から削除 |
| M5 | `engine.rs` に `render_streaming` / `layout_streaming` API 追加、`raikiri-engine` feature で最初の end-to-end 検証 |
| M5b | `render.rs` の margin box paint 部分を撤去(raikiri から MarginBoxFragment が来るので不要) |
| M5c | running element 関連の adapter コードを raikiri 側に委譲 |
| M5d | `Drawable::TargetRefSlot` を受け取って flpdf(または lopdf)で patch する path 実装 |
| M5f | RenderSink 3 実装 (Memory/Spool/Direct) |
| M6 | fulgur の image / mathml 統合を `ReplacedResolver` として実装 |
| M7 | parley path の adapter 経由呼出を raikiri 経由に移設 |
| M8 | `fulgur/src/gcpm/*` を `raikiri/crates/raikiri-*/src/gcpm/` に引越し(cssparser 依存ごと)、fulgur 側は削除、`Cargo.toml` から `cssparser` を削除 |
| M9 | `blitz-*` を Cargo.toml から削除、`raikiri-engine` を default features に昇格、`blitz_adapter.rs` を `layout_adapter.rs` に rename |

## 11. Compatibility Policy

### 11.1 Public API 互換性

- **`Engine::render()` / `render_file()`** — 完全互換維持。内部が blitz/raikiri
  どちらでも同じバイト列を返すのが目標(byte-identical PDF)。
- **`Engine::layout()`** — 完全互換維持。`LayoutOutput` の構造は非変更、内部が
  raikiri になっても呼び出し側からは透過。
- **`Engine::render_streaming()`** — 新規 API。M5 で追加。
- **`Engine::layout_streaming()`** — 新規 API。M5 で追加。

### 11.2 バインディング (Ruby / Python / WASM)

M9 完了までは `blitz-engine` が default なので、既存バインディングは無変更で動く。
M9 で default 昇格時に:

- Ruby / Python: バインディングの `Cargo.toml` で `raikiri-engine` を有効化
- WASM: 同上、加えて raikiri 側で `getrandom` の `wasm_js` feature 設定確認

streaming API はバインディング向けに露出する価値があるが、Phase 1 スコープ外
(別 doc)。

### 11.3 CLI 互換性

`fulgur-cli` の既存フラグはすべて維持。追加:

- `--sink=memory|spool|auto` — RenderSink 選択(default: auto)
- `--memory-ceiling=SIZE` — auto sink policy の閾値(default: 32M)

## 12. Testing Strategy

### 12.1 追加テスト種別

- **Byte-identical PDF test**: 同じ HTML を blitz mode と raikiri mode で render、
  PDF がバイト単位で一致するか
  - 決定論的 fixture 数十個(`examples/*` の一部)を対象
  - M9 の gate 条件: 全 fixture で byte-identical
- **Performance test**: 1000-page fixture でメモリピークとレンダー時間を測定
  - blitz mode / raikiri mode / raikiri + SpoolSink の 3 パス
  - Regression 検出 threshold: raikiri mode がメモリ 50% 削減(Phase 1 目標)
- **fulgur-wpt matrix**: §9 参照

### 12.2 既存テストの継続

- `crates/fulgur/tests/*` — 現行の integration test は blitz mode で継続 pass
  必須、M5 以降は raikiri mode でも pass 必須(feature flag test)
- `crates/fulgur-wpt/expectations/*` — blitz mode baseline は M9 まで凍結

### 12.3 例示 PDF (`examples/*/index.pdf`)

- byte-identical 維持を M9 まで凍結(blitz mode で生成)
- M9 で raikiri mode に切替、`examples/*/index.pdf` を再生成
- 差分は CHANGELOG に明示、release note (0.x → 0.(x+1)) の目玉

## 13. Risk & Mitigation

| Risk | 影響 | Mitigation |
|---|---|---|
| raikiri M9 が予定より遅れる | fulgur release cadence 影響 | blitz mode を default に据置き、raikiri は opt-in で先行提供。M9 到達後に default 昇格 |
| byte-identical PDF が達成できない | 既存 consumer(CI/CD)への影響 | 差分理由の CHANGELOG 明記 + `examples/*/index.pdf` 再 bless。Semver minor bump で通告 |
| krilla との相性問題 | raikiri Drawable → krilla API の adapter 層に想定外 gap | Drawable 拡張で対応(既に `Drawable` は non-exhaustive で拡張性あり) |
| flpdf の maturity 不足 | target-ref postprocess が動かない | Phase 1 の M5d は lopdf でも良い(現行 fulgur が既に lopdf を持つ)。flpdf は Phase 2 |
| WPT diff で raikiri regression が続発 | M8 以降の gate 通過遅延 | feature flag 継続、blitz mode で release し続ける。regression を beads で追跡し順次 fix |
| blitz 0.2 → 0.3 の behavior delta | raikiri は blitz-dom 0.3.0-alpha.6 を port 元にする(raikiri doc §13.2)。fulgur の現行 blitz は 0.2.4 なので、raikiri を有効にすると 0.3 系の挙動になる。既存 fulgur-wpt expectations に差分が出る可能性 | M8 の raikiri feature テストで新たに現れる regression を「本当の bug」と「0.2→0.3 の delta」に切り分け、後者は expectations 更新で受け入れ |

## 14. Success Criteria

Phase 1 完了(M9)時点で以下がすべて満たされていること:

1. `crates/fulgur/Cargo.toml` から `blitz-html`, `blitz-dom`, `blitz-traits`,
   `cssparser` の dep 記載が消えている(cssparser は raikiri 側で処理される —
   multicol は stylo 経由に解決できたら raikiri でも不要、GCPM parser 用途は
   raikiri が直接 dep として保持。詳細は raikiri doc §14.2)
2. `crates/fulgur/src/` に `blitz` 文字列を含む import が残っていない
   (`layout_adapter.rs` の rename 済み)、`cssparser::` 直接 import も残っていない
3. `fulgur-wpt` の既存 `expectations/*.txt` が raikiri mode でも同等以上に PASS
4. 1000-page fixture のメモリピークが blitz mode の 50% 以下
5. `examples/*` の全 PDF が raikiri mode で決定論的に生成(byte-identical
   regeneration)、CHANGELOG に旧 blitz mode との差分を明記
6. Ruby / Python / WASM binding が raikiri-engine で pass

## 15. 進行中の運用

### 15.1 dep 管理

Phase 1 初期は fulgur repo → raikiri repo への path dep(未 publish):

```toml
# crates/fulgur/Cargo.toml
[dependencies]
raikiri = { version = "0.1.0", optional = true, path = "../../../raikiri/crates/raikiri" }
raikiri-html = { version = "0.1.0", optional = true, path = "../../../raikiri/crates/raikiri-html" }
raikiri-dom = { version = "0.1.0", optional = true, path = "../../../raikiri/crates/raikiri-dom" }
```

M8 頃、raikiri を crates.io に publish して pin に切替(未確定、後日決定)。

### 15.2 fulgur release 中の raikiri 開発

- fulgur main は blitz-engine default で継続 release(0.34.x → 0.35 → ...)
- raikiri M1〜M8 の間は fulgur main では raikiri-engine は experimental
- fulgur PR で raikiri-engine の CI matrix を走らせるが blocking にしない
  (M8 到達までは advisory)

### 15.3 Beads 連携

- Phase 1 全体 tracking issue: (これから作成、raikiri-29m の子として)
- 各 milestone を epic として beads で管理
- fulgur repo 側 beads / raikiri repo 側 beads の cross-link は URL 記載で(dolt
  ではない beads は cross-repo dep を直接持てない)

## 16. Appendix: Phase 2 / Phase 3 preview

### Phase 2 (fulgur 側での作業、raikiri とは並行)

- lopdf → flpdf swap
- flpdf postprocess を fulgur-render に統合
- target-ref, outline, metadata, structure tree の後注入
- 別 doc: `docs/plans/2026-XX-XX-fulgur-flpdf-postprocess.md`

### Phase 3 (Phase 1/2 完了後)

- krilla 撤去、fulgur-paint / fulgur-font / fulgur-image / fulgur-svg 新設
- 真の O(1) memory streaming
- 別 doc: `docs/plans/2026-XX-XX-fulgur-paint-native.md`

---

## 変更履歴

- 2026-07-10: Draft 初版。raikiri design doc と対で作成
