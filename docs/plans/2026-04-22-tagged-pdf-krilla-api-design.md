# Tagged PDF / PDF-UA Krilla API Design Notes

Issue: `fulgur-izp.1`

## Goal

Krilla 0.7.0 の Tagging API を fulgur の DOM -> Pageable -> render
pipeline に接続するための短い設計メモ。後続タスク
`fulgur-izp.2` / `fulgur-izp.3` の前提として、API 名、呼び出し順、
fulgur 側の変更箇所、最小実装と PDF/UA 完全対応の境界を整理する。

## Product Direction: PDF/UA-First

fulgur は AI エージェント向けの PDF engine を目指すため、accessibility
support は optional polish ではなく core capability として扱う。Tagged PDF
は最終目的ではなく、PDF/UA に準拠した、支援技術と AI agent の双方が意味構造
を信頼して読める PDF を生成するための基盤である。

そのため、長期的な product direction は **PDF/UA-first** とする。

- `--pdf-ua` は本命の user-facing mode として扱う。
- `--tagged` は development/debugging 用、または PDF/UA validator までは要求しない
  lightweight tagged output として残す。
- `--pdf-ua` は `enable_tagging`、PDF/UA validator、document language、
  structure tree、outline/bookmarks、PDF/UA metadata を一括で有効化する。
- 安定後は、CLI / Engine の default output を PDF/UA 寄りにする余地を残す。
- すべての実コンテンツを semantic tag または artifact に分類することを
  goal とし、「一部だけタグ付けされた PDF」を完成状態とはみなさない。

AI agent 向けには、PDF/UA の semantics は次の意味を持つ。

- DOM を失った PDF からでも、heading / paragraph / list / table / link /
  figure の関係を再構築できる。
- Reading order と visual order の差分を PDF 内に明示できる。
- Decorative content と meaningful content を区別できる。
- Link annotation、alt text、table header scope など、agent が判断に使う
  document intent を標準 PDF structure として保持できる。

## Krilla Tagging API

### Entry Points

Krilla 0.7.0 の tagging API は `krilla::tagging` から公開されている。
`krilla::interchange::*` は `krilla` crate root で再 export されるため、
fulgur 側では次の型を使う想定。

```rust
use krilla::configure::{Configuration, Validator};
use krilla::serialize::SerializeSettings;
use krilla::tagging::{
    ContentTag, Identifier, ListNumbering, Node, SpanTag, TableHeaderScope,
    Tag, TagGroup, TagTree,
};
```

主要 API:

- `krilla::Document::new_with(SerializeSettings)`:
  tagging や PDF/UA validator を含む serialize settings 付きで document を作る。
- `SerializeSettings { enable_tagging, configuration, ..Default::default() }`:
  tagging 有効化と PDF/UA validator を指定する。
- `Configuration::new_with_validator(Validator::UA1)`:
  PDF/UA-1 用。validator は `enable_tagging` を強制的に true にする。
- `krilla::surface::Surface::start_tagged(ContentTag) -> Identifier`:
  以降の描画内容を 1 つの marked content identifier に紐づける。
- `krilla::surface::Surface::end_tagged()`:
  現在の tagged content section を閉じる。
- `krilla::page::Page::add_tagged_annotation(Annotation) -> Identifier`:
  link annotation などを structure tree に紐づける identifier を返す。
- `TagGroup::new(tag)` / `TagGroup::with_children(tag, Vec<Node>)`
  / `TagGroup::push(child)`:
  structure tree の group node を構築する。
- `TagTree::new()` / `TagTree::with_lang(config.lang.clone())`
  / `TagTree::push(group)`:
  document root 配下の structure tree を構築する。
- `Document::set_tag_tree(TagTree)`:
  `finish()` 前に tag tree を document に登録する。

代表的な tag:

- `Tag::P`
- `Tag::Hn(NonZeroU16)` with optional title
- `Tag::L(ListNumbering)` / `Tag::LI` / `Tag::Lbl` / `Tag::LBody`
- `Tag::Table` / `Tag::TR` / `Tag::TH(TableHeaderScope)` / `Tag::TD`
- `Tag::Span`
- `Tag::Link`
- `Tag::Figure(Option<String>)`
- `Tag::NonStruct`

Content leaf 用:

- `ContentTag::Span(SpanTag::empty())`
- `ContentTag::Other`
- `ContentTag::Artifact(ArtifactType::{Header, Footer, Page, Other})`

## Required Call Order

1. Create the document with serialize settings.

   ```rust
   let settings = SerializeSettings {
       enable_tagging: config.enable_tagging,
       configuration: if config.pdf_ua {
           Configuration::new_with_validator(Validator::UA1)
       } else {
           Configuration::new()
       },
       ..Default::default()
   };
   let mut document = krilla::Document::new_with(settings);
   ```

2. Build pages as today.

3. During drawing, wrap each semantic content region with exactly one
   `surface.start_tagged(...)` / `surface.end_tagged()` pair and keep the
   returned `Identifier`.

4. Push that identifier into exactly one `TagGroup` in the `TagTree`.

5. For link annotations, call `page.add_tagged_annotation(annotation)` instead
   of `page.add_annotation(annotation)` when tagging is enabled, then place
   the returned annotation identifier under a `Tag::Link` group.

6. After all pages are drawn and the tag tree has been built, call
   `document.set_tag_tree(tag_tree)` before `document.finish()`.

7. Continue to call `document.set_outline(...)` and
   `document.set_metadata(...)` before `finish()` as today.

## Krilla Constraints

- `Surface::start_tagged` is not nestable. A second `start_tagged` before
  `end_tagged` will panic. fulgur therefore needs a flat draw-time tagging
  discipline even if the logical `TagTree` is nested.
- Every non-dummy `Identifier` returned by `start_tagged` or
  `add_tagged_annotation` must have exactly one parent in the tag tree. Zero
  parents or multiple parents make export fail.
- `ContentTag::Artifact` returns a dummy identifier and should not be inserted
  into the tag tree.
- Group nodes with no children are discarded by Krilla. fulgur should avoid
  creating empty semantic groups where possible.
- `Figure` and `Formula` should have alt text for PDF/UA. Krilla validates
  missing alt text in stricter export modes.
- `Hn` requires a heading level and PDF/UA expects a title.
- `L` requires `ListNumbering`.
- `TH` requires `TableHeaderScope`.
- PDF/UA validation is partly semantic. Krilla can reject some mechanical
  errors, but cannot prove that the reading order, table semantics, alt text,
  language tags, and artifacts are truly correct.

## Existing Fulgur Pipeline

Relevant modules:

- `crates/fulgur/src/config.rs`
  - Holds `Config`, `ConfigBuilder`, PDF metadata, and `bookmarks`.
  - Add `enable_tagging` and likely `pdf_ua` here.
- `crates/fulgur/src/engine.rs`
  - `EngineBuilder` exposes public render options.
  - `render_html` builds the Blitz document, runs GCPM/bookmark passes,
    resolves layout, constructs `ConvertContext`, and calls `dom_to_pageable`.
  - Add builder methods and thread tagging-related context into conversion.
- `crates/fulgur-cli/src/main.rs`
  - Add `--tagged` and possibly `--pdf-ua`.
  - `--pdf-ua` should imply tagging and probably require/encourage `--lang`.
- `crates/fulgur/src/convert.rs`
  - Converts Blitz DOM nodes into `Box<dyn Pageable>`.
  - Best place to map HTML semantics to a tag model and attach that model to
    generated Pageable nodes.
  - Existing side tables in `ConvertContext` are the precedent for passing
    DOM-derived metadata to wrappers.
- `crates/fulgur/src/pageable.rs`
  - Defines `Pageable`, `Canvas`, draw methods, bookmark/link collectors,
    image/list/table pageables, and existing wrapper pageables.
  - Add tagging metadata wrappers/collectors here.
- `crates/fulgur/src/render.rs`
  - Both `render_to_pdf` and `render_to_pdf_with_gcpm` create the document,
    draw page content, emit link annotations, set outline and metadata, then
    finish.
  - Add `Document::new_with(settings)`, a tag collector/tree builder, tagged
    drawing, tagged annotations, and `document.set_tag_tree(...)` in both paths.
- `crates/fulgur/src/link.rs`
  - Currently emits link annotations using `page.add_annotation`.
  - Needs a tagged variant returning or recording annotation identifiers.
- `crates/fulgur/src/paragraph.rs`
  - Text and inline image drawing happens here.
  - Needed for span-level content identifiers and link content identifiers.
- `crates/fulgur/src/image.rs`
  - Block image drawing happens here.
  - Needed for `Figure` tagging and image alt text.
- `crates/fulgur/src/blitz_adapter.rs`
  - Already has DOM passes for bookmarks, counters, running elements, etc.
  - Useful place for semantic extraction helpers (`tag`, heading text,
    `img alt`, table/list roles) if conversion needs a side table.

## Existing Feature Interactions

### Metadata

`render.rs::build_metadata` already maps `Config.lang` to Krilla metadata
language. Tagged PDF should additionally set `TagTree::with_lang(config.lang.clone())`
so the auto-generated Document structure element has `/Lang`.

`--pdf-ua` should use `SerializeSettings.configuration =
Configuration::new_with_validator(Validator::UA1)`. This is separate from
metadata, although both are required for useful PDF/UA output.

### Outline / Bookmarks

The current bookmark feature collects heading markers during draw and calls
`document.set_outline(...)`. Tagged PDF can reuse the same heading semantics,
but outline and structure tree are separate PDF constructs:

- Outline: navigation pane entries and destinations.
- Tag tree: logical reading order and accessibility semantics.

For PDF/UA, Krilla's validator notes that an outline is required. Therefore
`--pdf-ua` should either imply `bookmarks = true` or return a clear error until
outline generation is available.

### Links

The current link path collects link rectangles during paragraph/image draw,
then `link.rs::emit_link_annotations` calls `page.add_annotation(...)`.

Tagged links need two identifiers:

- A content identifier from the linked text/image drawing.
- An annotation identifier from `page.add_tagged_annotation(...)`.

Both should be children of a `Tag::Link` group in reading order. This likely
requires extending `LinkCollector` or adding a sibling `TagCollector` so link
occurrences carry both their content identifier and annotation identifier.

### GCPM Margin Boxes

`render_to_pdf_with_gcpm` draws margin boxes before body content and currently
does not wire link collection for margin boxes. For initial tagging, margin box
content should be marked as `Artifact(Header/Footer)` where it is repeated
page furniture. Body content remains the source of semantic structure.

## Proposed Fulgur Design

### Semantic Model

Add a small internal model independent of Krilla:

```rust
pub enum PdfTag {
    P,
    H(u16),
    L { numbering: ListNumbering },
    LI,
    Lbl,
    LBody,
    Table,
    TR,
    TH { scope: TableHeaderScope },
    TD,
    Span,
    Link,
    Figure { alt: Option<String> },
    Div,
    NonStruct,
}
```

Keep this as a fulgur type so `convert.rs` does not construct Krilla
`TagGroup`s directly. The render pass can translate `PdfTag` into Krilla tags
when all page identifiers are known.

### Draw-Time Collector

Add an optional tagging collector to `Canvas`, mirroring the existing bookmark
and link collectors:

```rust
pub struct Canvas<'a, 'b> {
    pub surface: &'a mut krilla::surface::Surface<'b>,
    pub bookmark_collector: Option<&'a mut BookmarkCollector>,
    pub link_collector: Option<&'a mut LinkCollector>,
    pub tag_collector: Option<&'a mut TagCollector>,
}
```

`TagCollector` responsibilities:

- Track current page index.
- Start/end tagged content on the surface through a helper that guarantees
  balanced calls.
- Record `(semantic_node_id, Identifier, page_idx, bbox/read order metadata)`.
- Record annotation identifiers returned after link annotations are emitted.
- Build a `TagTree` at the end.

Because Krilla disallows nested `start_tagged`, wrappers should not blindly
nest. Prefer tagging leaf draw operations:

- Text runs / inline images in `paragraph.rs`.
- Block images in `image.rs`.
- Possibly whole block `Other` regions for a minimal first pass.

Logical nesting should live in `TagTree`, not in nested marked content.

### DOM / Pageable Connection

`convert.rs` should attach semantic wrappers to generated pageables at the same
single choke point where multicol, string-set, counter, transform, and bookmark
wrappers are already applied.

For minimal implementation:

- `p` -> `P`
- `h1`..`h6` -> `Hn`
- generic block containers -> `Div` or `NonStruct`
- `img` -> `Figure(alt)`
- inline text -> content identifiers beneath nearest block tag

For later tasks:

- `ul`/`ol`/`li` -> `L` / `LI` / `Lbl` / `LBody`
- `table`/`tr`/`th`/`td` -> table structure
- `a href` -> `Link` with tagged annotation

## Minimal Tagged-PDF Boundary

The smallest useful `--tagged` implementation is an intermediate milestone,
not the product finish line. It should:

- Add `Config.enable_tagging`, `EngineBuilder::tagged(bool)`, and CLI
  `--tagged`.
- Create `Document` with `SerializeSettings.enable_tagging`.
- Produce a non-empty `TagTree` with document language from `Config.lang`.
- Tag basic document reading order for paragraphs and headings.
- Mark body text/image drawing with `start_tagged` / `end_tagged`.
- Propagate `img alt` into `Tag::Figure(Some(alt))`.
- Keep outline/link/metadata behavior unchanged when tagging is off.
- Avoid `Validator::UA1` by default. `--tagged` is tagged PDF, not a promise
  of PDF/UA conformance.

Validation target for this phase:

- Rendering succeeds.
- PDF contains `/StructTreeRoot`.
- Existing tests and examples remain deterministic when tagging is off.

## PDF/UA Complete Boundary

PDF/UA mode is a stricter mode and should be treated as a separate layer over
basic tagging, but it is the long-term target for fulgur's accessibility
support. It should require or implement:

- `--pdf-ua` flag and `EngineBuilder::pdf_ua(bool)`.
- `SerializeSettings.configuration =
  Configuration::new_with_validator(Validator::UA1)`.
- Document language (`Config.lang`) present and valid enough for callers.
- Outline/bookmarks present and in reading order.
- All real content tagged; repeated headers/footers/page furniture marked as
  artifacts.
- Link annotations emitted with `add_tagged_annotation` and included in
  `Tag::Link` groups.
- List structure with `L`, `LI`, `Lbl`, `LBody`, and numbering.
- Table structure with `Table`, `TR`, `TH`, `TD`, header scopes, and headers
  where possible.
- Figure alt text policy, including error/warning behavior for missing alt.
- Smoke validation that `document.finish()` fails on a known PDF/UA violation
  and succeeds on a minimal valid fixture.

## Recommended Follow-Up Order

1. `fulgur-izp.2`: Add config, engine, and CLI flags with PDF/UA-first API
   shape. `--pdf-ua` should imply tagging and the required PDF/UA settings;
   `--tagged` remains a lower-level escape hatch.
2. `fulgur-izp.3`: Preserve HTML semantic metadata through conversion into
   pageables. This should model intended document semantics, not just paint
   order.
3. `fulgur-izp.4`: Wire basic block/inline drawing to `start_tagged` and
   collect identifiers while maintaining the no-nested-`start_tagged`
   invariant.
4. `fulgur-izp.5`: Build `TagTree` and call `document.set_tag_tree`.
5. `fulgur-izp.6` onward: Add alt text, lists, tables, links, PDF/UA metadata,
   validation, and public docs until `--pdf-ua` is a credible conformance
   mode rather than a syntactic tagged-PDF mode.
