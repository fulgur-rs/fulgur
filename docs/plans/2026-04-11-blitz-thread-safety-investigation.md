# Blitz スレッドセーフティ調査レポート

- **日付**: 2026-04-11
- **対象バージョン**: `blitz-dom 0.2.4` / `blitz-html 0.2.0` / `blitz-traits 0.2.0`
- **動機**: fulgur のスクリプト言語バインディング設計（`fulgur-d3r`, `fulgur-i5c`, `fulgur-0x0`）にあたり、Blitz の thread-safety を正しく把握する
- **きっかけ**: `CLAUDE.md` の Gotchas に「Blitz not thread-safe / integration tests require `--test-threads=1`」と書かれているが、根拠が明確でなかった
- **結論**: CLAUDE.md の **結論は正しい**（Blitz は同一プロセス内の並列実行で silent に死ぬ）。ただし **理由は当初の仮説と全く違う**

## TL;DR

1. **TLS は無関係**: `blitz-dom-0.2.4/src/document.rs:66-68` の `thread_local!` (`LAYOUT_CTX`, `FONT_CTX`) は `#[cfg(feature = "parallel-construct")]` 配下のみ。fulgur はこの feature を使っていない
2. **`Rc<RefCell<>>` ラッパーも無関係**: `HtmlDocument` は `inner: BaseDocument` を直接保持しているだけ
3. **静的型レベルで `Send + Sync` を阻むのは 3 つの trait bound 漏れだけ**: `ShellProvider`, `HtmlParserProvider`, `FontMetricsProvider` (stylo)
4. **しかし runtime に本物のデータレースがある**: 同一プロセス内で `BaseDocument::new()` を並列に呼ぶと **timing-dependent に silent exit (EXIT=0、テスト1件も走らず終了)** する
5. **プロセス間並列は完全に安全**: 4 プロセス同時実行 (`--test-threads=1` 各々) は問題なし
6. **対策**: `blitz_adapter` 内に `static BLITZ_LOCK: Mutex<()>` を1つ持ち、Blitz API 呼び出しを直列化する

## 背景

前のセッションで fulgur のバインディング設計を進める中で、Engine が `Send + Sync` を満たすか確認する必要が出た。CLAUDE.md には「Blitz not thread-safe」とだけ書かれており、根拠は明示されていない。

Subagent による初期調査では「`Rc<RefCell<BaseDocument>>` ラッパーと stylo の TLS が原因」と報告された。しかしこれは：

- 実コードを精査した結果、両方とも誤り
- TLS は parallel-construct feature が必要、fulgur は使っていない
- `HtmlDocument` は `Rc` で包まれていない

ということが判明した。そこで実証ベースで検証することにした。

## 検証方法

1. `cargo` registry にダウンロード済みの実ソース (`~/.cargo/registry/src/...`) を直接読む
2. `cargo check` で静的に Send/Sync を検証する probe ファイルを fulgur に一時追加
3. 既存テストを `--test-threads > 1` で実行し、ランタイム挙動を確認
4. プロセス間並列を別途検証

## Phase 1: コード精査（静的分析）

### `HtmlDocument` の実体

`blitz-html-0.2.0/src/html_document.rs:7`:

```rust
pub struct HtmlDocument {
    inner: BaseDocument,
}
```

`Rc<RefCell<>>` ではない。Subagent 報告は誤り。

### `thread_local!` の出現箇所

`blitz-dom-0.2.4/src/document.rs:66-68`:

```rust
thread_local! {
    static LAYOUT_CTX: RefCell<Option<Box<LayoutContext<TextBrush>>>> = const { RefCell::new(None) };
    static FONT_CTX: RefCell<Option<Box<FontContext>>> = const { RefCell::new(None) };
}
```

これらの実際の使用箇所 (`document.rs:1130-1162`):

```rust
#[cfg(feature = "parallel-construct")]
let mut layout_ctx = LAYOUT_CTX
    .take()
    .unwrap_or_else(|| Box::new(LayoutContext::new()));
#[cfg(feature = "parallel-construct")]
let layout_ctx_mut = &mut layout_ctx;

#[cfg(not(feature = "parallel-construct"))]
let layout_ctx_mut = &mut self.layout_ctx;
```

すべて `#[cfg(feature = "parallel-construct")]` 配下。fulgur は `Cargo.toml` でこの feature を有効化していないので、コードパス上に thread_local は存在しない。

### `BaseDocument` のフィールド構成

主要フィールド (`blitz-dom-0.2.4/src/document.rs:98-171`):

| フィールド | 型 | Send/Sync 評価 |
|---|---|---|
| `nodes` | `Box<Slab<Node>>` | OK |
| `stylist` | `Stylist` | 内部に `Box<dyn FontMetricsProvider>` を持つため `!Send` |
| `font_ctx` | `Arc<Mutex<FontContext>>` | OK |
| `layout_ctx` | `parley::LayoutContext<TextBrush>` | おそらく OK |
| `net_provider` | `Arc<dyn NetProvider<Resource>>` | trait に `: Send + Sync + 'static` 境界あり、OK |
| `navigation_provider` | `Arc<dyn NavigationProvider>` | 同上、OK |
| `shell_provider` | `Arc<dyn ShellProvider>` | trait に境界なし、`!Send + !Sync` |
| `html_parser_provider` | `Arc<dyn HtmlParserProvider>` | trait に境界なし、`!Send + !Sync` |

`blitz-traits-0.2.0` の trait 定義:

```rust
// net.rs:19 — OK
pub trait NetProvider<Data>: Send + Sync + 'static { ... }

// navigation.rs:10 — OK
pub trait NavigationProvider: Send + Sync + 'static { ... }

// shell.rs:11 — 漏れている
pub trait ShellProvider { ... }
```

`blitz-dom-0.2.4/src/html.rs:3`:

```rust
// 漏れている
pub trait HtmlParserProvider { ... }
```

つまり同じ blitz の trait のうち、半分は `Send + Sync` を付け、半分は付け忘れているという状態。

## Phase 2: コンパイラに直接聞く（静的検証）

`crates/fulgur/tests/blitz_send_sync_probe.rs` を一時作成:

```rust
fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

#[test]
fn htmldocument_send() { assert_send::<blitz_html::HtmlDocument>(); }

#[test]
fn htmldocument_sync() { assert_sync::<blitz_html::HtmlDocument>(); }

#[test]
fn basedocument_send() { assert_send::<blitz_dom::BaseDocument>(); }

#[test]
fn basedocument_sync() { assert_sync::<blitz_dom::BaseDocument>(); }
```

`cargo check --tests -p fulgur --test blitz_send_sync_probe` の結果、エラーの原因は厳密に 3 つだけ:

1. `(dyn blitz_traits::shell::ShellProvider + 'static)` cannot be sent/shared between threads safely
2. `(dyn HtmlParserProvider + 'static)` cannot be sent/shared between threads safely
3. `(dyn style::servo::media_queries::FontMetricsProvider + 'static)` cannot be sent between threads safely
   - これは `stylo-0.8.0/servo/media_queries.rs:56` の `Device` 内の `Box<dyn FontMetricsProvider>` 経由

修正コスト:

- (1) と (2) は **1 行ずつ**: `pub trait XXX:` に `Send + Sync` を追加するだけ
- (3) は stylo upstream が必要、ただし unsafe newtype wrapper で迂回可能
- 「データ競合があるから !Send」ではなく **trait 境界の書き忘れ**

この時点での暫定結論: 「静的型は楽勝だな、unsafe newtype で迂回しよう」── と思ったが、Phase 3 で覆される。

## Phase 3: ランタイム検証

### 実験 1: シンプルなテストの並列実行

`crates/fulgur/tests/html_test.rs` は `HtmlDocument` を作って簡単な変換をするだけのテスト 2 件のみ:

```bash
$ ./target/release/deps/html_test-xxx --test-threads=2 > /tmp/h.log 2>&1
$ echo "EXIT=$?"
EXIT=0
$ wc -c /tmp/h.log
17
$ cat /tmp/h.log
running 2 tests
```

EXIT=0、出力 17 バイト、テストは **1 件も完了せず silent に終了**。stderr も空、panic message も backtrace もなし。

`--test-threads=1` では正常に走る:

```text
running 2 tests
test test_convert_html_convenience ... ok
test test_render_simple_html ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

### 実験 2: strace 経由（処理が遅くなる）

```bash
$ strace -f -o /dev/null ./target/release/deps/html_test-xxx --test-threads=2
running 2 tests
test test_render_simple_html ... ok
test test_convert_html_convenience ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s
```

strace の slowdown 下では **全件 pass**。timing-dependent な失敗であることが確定。

### 実験 3: プロセス間並列

別プロセス 4 本を同時起動、各々 `--test-threads=1`:

```bash
for i in 1 2 3 4; do
  ./target/release/deps/html_test-xxx --test-threads=1 > /tmp/proc_$i.log 2>&1 &
done
wait
```

結果: 4 プロセスとも `test result: ok. 2 passed`。**プロセス間並列は完全に安全**。

### 失敗モードの分類

実験から得られた特性:

| 実行形態 | 結果 |
|---|---|
| 1 プロセス × 1 スレッド | OK |
| 1 プロセス × 2+ スレッド (素) | **silent exit (EXIT=0、テスト未実行)** |
| 1 プロセス × 2+ スレッド (strace 経由) | OK |
| N プロセス × 1 スレッド | OK |

これは古典的な **データレースの timing-dependent 失敗** パターン。stylo の global state (具体的な箇所は未特定) を 2 スレッドが同時に初期化しようとして corrupt が起きる、という形がもっとも蓋然性が高い。`style_config::set_bool` が `BaseDocument::new()` 内で呼ばれている (`blitz-dom-0.2.4/src/document.rs:232-236`) のが疑わしい候補。

silent exit (panic message なし、EXIT=0) なのが厄介。デバッグが極めて困難な失敗モード。

## 結論

| 仮説 | 検証結果 |
|---|---|
| Blitz は TLS が原因で thread-unsafe | **誤り** (TLS は feature gate 配下) |
| `Rc<RefCell<>>` で包まれているので !Send | **誤り** (HtmlDocument は値直保持) |
| 静的型の `Send + Sync` を阻むのは 3 つの trait bound 漏れだけ | **正しい** |
| upstream の trait に境界を足せば、または unsafe newtype で迂回すれば、並列実行できる | **誤り** (runtime data race が別途存在) |
| CLAUDE.md の「Blitz not thread-safe」 | **結論は正しい** (理由は違う) |
| プロセス間並列は安全 | **正しい** (実証済み) |

## fulgur への影響

### バインディング設計（`fulgur-d3r`）

前回のセッションで決めた方針「Engine は `!Sync`、GVL/GIL で直列化、process 並列推奨」は **結論として正しい**。ただし以下の点で楽観しすぎていた:

- **GIL/GVL だけに頼るのは不十分**: PyO3 / Magnus は Drop 時や一部の操作で GIL/GVL を解放することがある。Python 3.13t (free-threaded) も将来のリスク
- **Python ユーザーは `ThreadPoolExecutor` を反射的に使う**: 動かないと「fulgur が壊れている」と思われる

したがって fulgur 側で **明示的にロックを取る** べき:

1. `blitz_adapter` 内に `static BLITZ_LOCK: Mutex<()>` を 1 つ持つ
2. `parse()`, `resolve()`, `apply_passes()` 等、Blitz API を呼ぶすべての場所でロックを取る
3. Engine 自体は `!Sync` (`PhantomData<*const ()>` で明示してもよい) のままだが、**「複数スレッドから同じ Engine を呼んでも内部で直列化されて安全に動く」**動作になる
4. 真の並列性はないが、安全性が保証される (二重防御: GIL/GVL + Mutex)
5. 副次効果: テストも `--test-threads=1` 制約を外せる可能性が高い

### バッチ並列レンダリング (`fulgur-dzv`)

方針変更なし。**並列バッチは提供しない**。プロセス並列を推奨する。

### CLAUDE.md の Gotcha 更新

現状の文言:

```text
- Integration tests require `--test-threads=1` (Blitz not thread-safe)
```

提案する新文言（`fulgur-d3r` の mutex 対応後）:

```text
- Blitz (blitz-dom 0.2.4) は同一プロセス内で BaseDocument::new() を並列に
  呼ぶと stylo の global state にデータレースが起きて silent exit する
  (EXIT=0、panic message なし、テスト未実行で終了)。
  対策として blitz_adapter::* は static Mutex で直列化されているので、
  Engine を複数スレッドから呼ぶこと自体は安全（ただし真の並列性はなし、
  プロセス並列を推奨）。
```

## アクションアイテム

> ステータスは PR #61 マージ時点での状況。未着手は E のみ。

| 順 | 内容 | ステータス |
|---|---|---|
| A | `blitz_adapter` に `static BLITZ_LOCK: Mutex<()>` を入れて `--test-threads=4` で動くか検証 | 完了 (PR #61) |
| B | CLAUDE.md の Gotcha を実情に合わせて書き直す | 完了 (PR #61) |
| C | `fulgur-d3r` の description を本レポートの内容で書き換え | 完了 (beads: closed) |
| D | `project memory` に Blitz thread-safety の真相を保存（再調査回避） | 完了 (`project_blitz_thread_safety.md`) |
| E | DioxusLabs/blitz に upstream issue 提出（silent exit on parallel `BaseDocument::new()`）| 未着手（余裕があれば） |

## 参考リンク

- crates: [blitz-dom](https://crates.io/crates/blitz-dom), [blitz-html](https://crates.io/crates/blitz-html), [blitz-traits](https://crates.io/crates/blitz-traits)
- リポジトリ: <https://github.com/DioxusLabs/blitz>
- PR #247 (Incremental/parallel tree construction): <https://github.com/DioxusLabs/blitz/pull/247>
- Issue #249 (Parallel layout tree construction): <https://github.com/DioxusLabs/blitz/issues/249>
- fulgur 側関連ファイル: `crates/fulgur/src/blitz_adapter.rs`, `CLAUDE.md`
