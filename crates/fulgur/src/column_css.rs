//! Minimal CSS sniffer for the multicol properties that stylo 0.8.0 gates to
//! its `gecko` engine. Blitz ships the `servo` feature only, so
//! `column-rule-{width,style,color}`, `column-fill`, and `break-inside` never
//! reach the `ComputedValues` we inspect elsewhere in the pipeline.
//!
//! Phase A of fulgur-v7a needs `column-rule-*` and `column-fill` to render
//! rules between adjacent columns and switch the multicol layout hook between
//! "balance" and "auto" (greedy fill). The follow-up (`fulgur-ftp`) extends
//! this module with `break-inside` (see [`ColumnStyleProps::break_inside`]).
//!
//! Scope and trade-offs:
//!
//! - Only inline `style="..."` attributes and top-level `<style>` blocks are
//!   scanned. External stylesheets loaded via `<link rel=stylesheet>` reach
//!   stylo/blitz for the properties they support but do **not** currently
//!   populate this side-table — tracked as fulgur-s5ro.
//! - `<style media="...">` blocks whose media excludes `all` / `print` are
//!   skipped by the harvester (`extract_column_style_table` in
//!   `blitz_adapter.rs`). Full CSS media-query evaluation is deferred.
//! - The selector grammar is intentionally tiny: type (`div`), class
//!   (`.foo`), id (`#bar`), universal (`*`), compound (`div.foo#bar`) and
//!   comma-separated lists. Unsupported combinators or pseudo-classes cause
//!   the whole rule to be dropped silently.
//! - Source order wins (no specificity, no `!important`). Inline style is
//!   folded last so it beats stylesheet rules.
//! - Length units: `pt` passes through, `px` is converted to `pt` using the
//!   canonical CSS factor `72 / 96`. `em` and `rem` resolve against a fixed
//!   `DEFAULT_EM_PX = 16.0` basis (Phase B will replace this with the
//!   container's computed font-size). `%`, `vh`, `vw`, `mm`, `cm`, `in`
//!   remain invalid — none of them make sense as a sub-point border width
//!   without consulting the surrounding box.

// Task 2 wires the production callers of `extract_column_style_table` /
// `parse_stylesheet` / `build_column_style_table`; Task 5 will consume the
// `ColumnStyleProps` / `ColumnRuleSpec` / `ColumnFill` fields to wrap
// multicol containers with `MulticolRulePageable`. Per-item `dead_code`
// allows live below for surfaces that Task 5 reaches but Task 2 only
// populates.

use std::collections::BTreeMap;

use cssparser::{
    AtRuleParser, BasicParseErrorKind, CowRcStr, DeclarationParser, ParseError, Parser,
    ParserInput, QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
    color::{parse_hash_color, parse_named_color},
};

use crate::pageable::BreakInside;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// `column-rule-style` value. Only the three Phase A styles are modelled —
/// `double`, `groove`, `ridge`, `inset`, and `outset` are Phase C and fall
/// through to [`ColumnRuleStyle::None`] (rule is not drawn).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnRuleStyle {
    /// No rule — either the author set `none` or an unsupported style was
    /// provided. The renderer skips drawing.
    #[default]
    None,
    Solid,
    Dashed,
    Dotted,
}

/// A fully-resolved `column-rule` specification. Width is in PDF points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnRuleSpec {
    pub width: f32,
    pub style: ColumnRuleStyle,
    /// RGBA colour, matching the `[u8; 4]` convention used by
    /// `paragraph::TextRun` and `pageable::BlockPageable::border_color`.
    pub color: [u8; 4],
}

impl Default for ColumnRuleSpec {
    fn default() -> Self {
        Self {
            width: 1.0,
            style: ColumnRuleStyle::None,
            color: [0, 0, 0, 255],
        }
    }
}

/// `column-fill` value. Defaults to `balance` per CSS Multi-column Layout
/// Level 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnFill {
    #[default]
    Balance,
    Auto,
}

/// Resolved properties for a single element. Fields remain `None` when the
/// author did not set them, so the consumer can tell "absent" from "set to
/// default" (important for the cascade — a later rule overrides only the
/// fields it declares).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ColumnStyleProps {
    pub rule: Option<ColumnRuleSpec>,
    pub fill: Option<ColumnFill>,
    /// `break-inside` as resolved by the fulgur-ftp sniffer. `None` means the
    /// author did not set the property — consumers should treat that as the
    /// initial value (`BreakInside::Auto`). Stored as `Option` for the same
    /// reason as the other fields: a later rule overwrites only the
    /// properties it declares (see [`merge`](Self::merge)).
    pub break_inside: Option<BreakInside>,
}

impl ColumnStyleProps {
    /// Fold `other` on top of `self`, with `Some(x)` in `other` overwriting
    /// the same field in `self` (last-wins, no specificity).
    fn merge(&mut self, other: ColumnStyleProps) {
        if other.rule.is_some() {
            self.rule = other.rule;
        }
        if other.fill.is_some() {
            self.fill = other.fill;
        }
        if other.break_inside.is_some() {
            self.break_inside = other.break_inside;
        }
    }

    fn is_empty(&self) -> bool {
        self.rule.is_none() && self.fill.is_none() && self.break_inside.is_none()
    }
}

/// Side-table keyed by the blitz DOM node id (`usize`). `BTreeMap` is chosen
/// over `HashMap` so iteration order is deterministic — even though this
/// table is only consulted during draw setup (never serialised directly),
/// determinism is a project-wide invariant (see `CLAUDE.md`).
pub type ColumnStyleTable = BTreeMap<usize, ColumnStyleProps>;

/// A single parsed stylesheet rule: one or more selectors and the properties
/// that apply when any of them matches.
#[derive(Clone, Debug)]
pub struct StyleRule {
    pub selectors: Vec<CompoundSelector>,
    pub props: ColumnStyleProps,
}

/// One simple selector — a single unqualified component of a compound
/// selector. `Universal` matches every element; the other variants match
/// against the corresponding attribute / tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SimpleSelector {
    /// Lowercased tag name (e.g. `div`).
    Type(String),
    /// Class name without the leading `.`.
    Class(String),
    /// Id without the leading `#`.
    Id(String),
    /// `*`.
    Universal,
}

/// Compound selector — multiple [`SimpleSelector`] parts that must all match
/// the same element (logical AND). `div.foo#bar` is a compound of three
/// parts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompoundSelector {
    pub parts: Vec<SimpleSelector>,
}

// ---------------------------------------------------------------------------
// Length and colour helpers
// ---------------------------------------------------------------------------

/// Default font-size basis used to resolve `em` / `rem` lengths when we
/// don't have a live cascade to consult. CSS default is `1rem == 16px`;
/// Phase A does not thread a real computed basis through this parser, so
/// accepting the default keeps common stylesheets (`column-rule-width:
/// 0.25rem`) from silently dropping. Phase B will replace this with the
/// container's resolved font-size.
const DEFAULT_EM_PX: f32 = 16.0;

/// Convert a CSS length token to PDF points. Accepts `pt` and `px` natively;
/// `em` and `rem` are resolved against a fixed 16px basis so rule widths
/// authored in ems don't silently drop. `%`, `mm`, `cm`, `in`, `vh`, `vw`
/// remain rejected — none of them make sense as a sub-point border width
/// without consulting the surrounding box, which Phase A does not expose.
fn length_to_pt(value: f32, unit: &str) -> Option<f32> {
    if unit.eq_ignore_ascii_case("pt") {
        Some(value)
    } else if unit.eq_ignore_ascii_case("px") {
        Some(value * 72.0 / 96.0)
    } else if unit.eq_ignore_ascii_case("em") || unit.eq_ignore_ascii_case("rem") {
        // em/rem → px via the default basis, then px → pt.
        Some(value * DEFAULT_EM_PX * 72.0 / 96.0)
    } else {
        None
    }
}

/// Parse a single `<length>` from `input`. Unlike the GCPM variant we reject
/// `mm`/`cm`/`in` too — the column-rule spec is sub-point and designers use
/// `px`/`pt`/`em` exclusively in practice; folding more units here would
/// just give inconsistent renders between the multicol rule and the rest
/// of the border model.
fn parse_length<'i>(input: &mut Parser<'i, '_>) -> Result<f32, ParseError<'i, ()>> {
    let token = input.next()?.clone();
    match token {
        Token::Dimension { value, unit, .. } => length_to_pt(value, &unit)
            .ok_or_else(|| input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    }
}

/// Parse a `<color>` from `input`. Supports `#rgb` / `#rgba` / `#rrggbb` /
/// `#rrggbbaa` hashes, CSS named colours, and the functional notations
/// `rgb()` / `rgba()` / `hsl()` / `hsla()` (legacy comma syntax; CSS Color
/// 3 – modern `rgb(r g b / a)` space-separated form is not modelled).
/// Deliberate returns of `Err` for unsupported keywords (`currentcolor`,
/// `inherit`, system colours): the declaration drops silently while
/// siblings in the same block continue to populate.
///
/// **Why not `cssparser::Color::parse`?** The Phase A plan referenced that
/// API, but cssparser 0.35 does not ship one — the full colour parser
/// lives in a separate `cssparser-color` crate. Adding that dependency is
/// out of scope for this targeted fix (hard-constrained to this file), so
/// we implement the two functional forms directly on top of the tokenizer.
fn parse_color<'i>(input: &mut Parser<'i, '_>) -> Result<[u8; 4], ParseError<'i, ()>> {
    let token = input.next()?.clone();
    let err = || input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid);
    match token {
        Token::IDHash(ref s) | Token::Hash(ref s) => {
            let (r, g, b, a) = parse_hash_color(s.as_bytes()).map_err(|_| err())?;
            let alpha_u8 = (a * 255.0_f32).round().clamp(0.0, 255.0) as u8;
            Ok([r, g, b, alpha_u8])
        }
        Token::Ident(ref name) => {
            if name.eq_ignore_ascii_case("transparent") {
                Ok([0, 0, 0, 0])
            } else if let Ok((r, g, b)) = parse_named_color(&name.to_ascii_lowercase()) {
                Ok([r, g, b, 255])
            } else {
                // `currentcolor`, `inherit`, system colours, typos — not
                // representable as an `[u8; 4]`. Bail so the containing
                // declaration drops.
                Err(err())
            }
        }
        Token::Function(ref name) => {
            let fn_name = name.to_ascii_lowercase();
            // Build the fallback error eagerly so we release the immutable
            // borrow of `input` before calling `parse_nested_block`, which
            // takes `&mut self`.
            let nested = match fn_name.as_str() {
                "rgb" | "rgba" => input.parse_nested_block(parse_rgb_args),
                "hsl" | "hsla" => input.parse_nested_block(parse_hsl_args),
                _ => return Err(err()),
            };
            nested.map_err(|_| input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid))
        }
        _ => Err(err()),
    }
}

/// Parse a single rgb/rgba channel as either an integer 0..255 number or a
/// percentage. Clamps to `[0, 255]` on overflow, matching the CSS Color
/// spec's "clamp on compute" rule.
fn parse_rgb_channel<'i>(input: &mut Parser<'i, '_>) -> Result<u8, ParseError<'i, ()>> {
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(value.round().clamp(0.0, 255.0) as u8),
        Token::Percentage { unit_value, .. } => {
            Ok((unit_value * 255.0).round().clamp(0.0, 255.0) as u8)
        }
        _ => Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    }
}

/// Parse an alpha value as either a 0..1 number or a percentage. Scales to
/// 0..255 and rounds. Clamps to `[0, 255]`.
fn parse_alpha_value<'i>(input: &mut Parser<'i, '_>) -> Result<u8, ParseError<'i, ()>> {
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok((value * 255.0).round().clamp(0.0, 255.0) as u8),
        Token::Percentage { unit_value, .. } => {
            Ok((unit_value * 255.0).round().clamp(0.0, 255.0) as u8)
        }
        _ => Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    }
}

/// Parse the arguments of `rgb(...)` / `rgba(...)` — legacy comma syntax
/// only. Accepts `rgb(r, g, b)` or `rgb(r, g, b, a)` (alpha optional; the
/// name distinction between `rgb` and `rgba` has not mattered since CSS
/// Color 4).
fn parse_rgb_args<'i>(input: &mut Parser<'i, '_>) -> Result<[u8; 4], ParseError<'i, ()>> {
    let r = parse_rgb_channel(input)?;
    input.expect_comma()?;
    let g = parse_rgb_channel(input)?;
    input.expect_comma()?;
    let b = parse_rgb_channel(input)?;
    let a = if input.try_parse(|i| i.expect_comma()).is_ok() {
        parse_alpha_value(input)?
    } else {
        255
    };
    input.expect_exhausted()?;
    Ok([r, g, b, a])
}

/// Parse the arguments of `hsl(...)` / `hsla(...)` — legacy comma syntax
/// only. Hue is a number in degrees; saturation and lightness are required
/// percentages. Converts to sRGB using the CSS Color 3 formula.
fn parse_hsl_args<'i>(input: &mut Parser<'i, '_>) -> Result<[u8; 4], ParseError<'i, ()>> {
    let hue_deg = match input.next()?.clone() {
        Token::Number { value, .. } => value,
        _ => return Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    };
    input.expect_comma()?;
    let s = match input.next()?.clone() {
        Token::Percentage { unit_value, .. } => unit_value.clamp(0.0, 1.0),
        _ => return Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    };
    input.expect_comma()?;
    let l = match input.next()?.clone() {
        Token::Percentage { unit_value, .. } => unit_value.clamp(0.0, 1.0),
        _ => return Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    };
    let a = if input.try_parse(|i| i.expect_comma()).is_ok() {
        parse_alpha_value(input)?
    } else {
        255
    };
    input.expect_exhausted()?;

    let (r, g, b) = hsl_to_rgb(hue_deg, s, l);
    Ok([r, g, b, a])
}

/// CSS Color 3 HSL → sRGB conversion. Hue is in degrees (any real number;
/// wrapped via modulo). Saturation and lightness are normalised to
/// `[0, 1]`. Returns 8-bit sRGB channels with standard rounding.
fn hsl_to_rgb(hue_deg: f32, s: f32, l: f32) -> (u8, u8, u8) {
    // Normalise hue to [0, 1) — rem_euclid keeps negatives positive.
    let h = (hue_deg.rem_euclid(360.0)) / 360.0;

    let hue_to_rgb = |p: f32, q: f32, mut t: f32| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };

    let (r_f, g_f, b_f) = if s == 0.0 {
        (l, l, l)
    } else {
        let q = if l < 0.5 {
            l * (1.0 + s)
        } else {
            l + s - l * s
        };
        let p = 2.0 * l - q;
        (
            hue_to_rgb(p, q, h + 1.0 / 3.0),
            hue_to_rgb(p, q, h),
            hue_to_rgb(p, q, h - 1.0 / 3.0),
        )
    };

    (
        (r_f * 255.0).round().clamp(0.0, 255.0) as u8,
        (g_f * 255.0).round().clamp(0.0, 255.0) as u8,
        (b_f * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

fn parse_rule_style_ident(ident: &str) -> Option<ColumnRuleStyle> {
    match ident.to_ascii_lowercase().as_str() {
        "none" => Some(ColumnRuleStyle::None),
        "solid" => Some(ColumnRuleStyle::Solid),
        "dashed" => Some(ColumnRuleStyle::Dashed),
        "dotted" => Some(ColumnRuleStyle::Dotted),
        // `double | groove | ridge | inset | outset` are Phase C — drop the
        // whole shorthand rather than falsely render as `None`.
        _ => None,
    }
}

fn parse_column_fill_value<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<ColumnFill, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    if ident.eq_ignore_ascii_case("auto") {
        Ok(ColumnFill::Auto)
    } else if ident.eq_ignore_ascii_case("balance") {
        Ok(ColumnFill::Balance)
    } else {
        Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid))
    }
}

/// Parse the value side of `break-inside`. Accepts the three avoid flavours
/// (`avoid`, `avoid-page`, `avoid-column`) as `BreakInside::Avoid`, and
/// `auto` as `BreakInside::Auto`. Other idents (`avoid-region`,
/// `left`, `right`, typos …) drop the declaration so siblings still apply.
fn parse_break_inside_value<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<BreakInside, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    match ident.as_ref().to_ascii_lowercase().as_str() {
        "avoid" | "avoid-page" | "avoid-column" => Ok(BreakInside::Avoid),
        "auto" => Ok(BreakInside::Auto),
        _ => Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid)),
    }
}

// ---------------------------------------------------------------------------
// Declaration block parser
// ---------------------------------------------------------------------------

/// Parse a CSS declaration block (no surrounding braces) into
/// [`ColumnStyleProps`]. Invalid declarations drop silently; valid ones fill
/// their field. Last-declaration-wins within a block.
pub fn parse_declaration_block(css: &str) -> ColumnStyleProps {
    let mut props = ColumnStyleProps::default();
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);

    let mut decl_parser = ColumnDeclParser { props: &mut props };
    // `RuleBodyParser` walks `decl; decl; decl;` and feeds each declaration
    // through `DeclarationParser::parse_value`. Errors from one declaration
    // do not abort the block — they surface as `Err` items in the iterator
    // which we discard.
    let iter = RuleBodyParser::new(&mut parser, &mut decl_parser);
    for item in iter {
        let _ = item;
    }
    props
}

struct ColumnDeclParser<'a> {
    props: &'a mut ColumnStyleProps,
}

impl<'i, 'a> DeclarationParser<'i> for ColumnDeclParser<'a> {
    type Declaration = ();
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _decl_start: &cssparser::ParserState,
    ) -> Result<(), ParseError<'i, ()>> {
        if name.eq_ignore_ascii_case("column-rule") {
            if let Some(spec) = parse_column_rule_shorthand(input) {
                // A shorthand with `style: None` is equivalent to "don't
                // draw" — we store it anyway so a later longhand can read
                // the width/colour the author still wanted.
                self.props.rule = Some(spec);
            }
        } else if name.eq_ignore_ascii_case("column-rule-width") {
            if let Ok(w) = input.parse_entirely(parse_length) {
                let mut spec = self.props.rule.unwrap_or_default();
                spec.width = w;
                self.props.rule = Some(spec);
            }
        } else if name.eq_ignore_ascii_case("column-rule-style") {
            let result = input.parse_entirely(|input| {
                let ident = input.expect_ident()?.clone();
                parse_rule_style_ident(&ident)
                    .ok_or_else(|| input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid))
            });
            if let Ok(style) = result {
                let mut spec = self.props.rule.unwrap_or_default();
                spec.style = style;
                self.props.rule = Some(spec);
            }
        } else if name.eq_ignore_ascii_case("column-rule-color") {
            if let Ok(color) = input.parse_entirely(parse_color) {
                let mut spec = self.props.rule.unwrap_or_default();
                spec.color = color;
                self.props.rule = Some(spec);
            }
        } else if name.eq_ignore_ascii_case("column-fill") {
            if let Ok(fill) = input.parse_entirely(parse_column_fill_value) {
                self.props.fill = Some(fill);
            }
        } else if name.eq_ignore_ascii_case("break-inside") {
            // `break-inside` (CSS Fragmentation Level 3) maps
            // `avoid-page` / `avoid-column` / `avoid` all onto
            // [`BreakInside::Avoid`]: fulgur's fragmentation model only
            // distinguishes "may split" from "atomic", so the three avoid
            // flavours collapse into a single lattice point. `auto` is the
            // initial value; unknown idents drop the declaration silently
            // so siblings keep applying.
            if let Ok(bi) = input.parse_entirely(parse_break_inside_value) {
                self.props.break_inside = Some(bi);
            }
        } else {
            // Unknown property — discard its value tokens silently.
            while input.next().is_ok() {}
        }
        Ok(())
    }
}

impl<'i, 'a> AtRuleParser<'i> for ColumnDeclParser<'a> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
}

impl<'i, 'a> QualifiedRuleParser<'i> for ColumnDeclParser<'a> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();
}

impl<'i, 'a> RuleBodyItemParser<'i, (), ()> for ColumnDeclParser<'a> {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

/// Parse the value side of `column-rule: <width> || <style> || <color>`.
/// Returns `None` if any component is invalid so the whole shorthand drops
/// — per CSS rules a malformed shorthand is "ignored entirely" and we
/// mirror that strict behaviour. Missing components fall back to the
/// defaults in [`ColumnRuleSpec::default`].
fn parse_column_rule_shorthand(input: &mut Parser<'_, '_>) -> Option<ColumnRuleSpec> {
    let mut width: Option<f32> = None;
    let mut style: Option<ColumnRuleStyle> = None;
    let mut color: Option<[u8; 4]> = None;

    // The shorthand accepts the three components in any order but at most
    // one of each. Loop up to three times and try each slot in turn.
    for _ in 0..3 {
        if input.is_exhausted() {
            break;
        }

        // Try width.
        if width.is_none() {
            let parsed = input.try_parse(parse_length);
            if let Ok(w) = parsed {
                width = Some(w);
                continue;
            }
        }

        // Try style.
        if style.is_none() {
            let parsed = input.try_parse(|input| {
                let ident = input.expect_ident()?.clone();
                parse_rule_style_ident(&ident)
                    .ok_or_else(|| input.new_error::<()>(BasicParseErrorKind::QualifiedRuleInvalid))
            });
            if let Ok(s) = parsed {
                style = Some(s);
                continue;
            }
        }

        // Try color.
        if color.is_none() {
            let parsed = input.try_parse(parse_color);
            if let Ok(c) = parsed {
                color = Some(c);
                continue;
            }
        }

        // No component accepted the next token — malformed shorthand.
        return None;
    }

    // Any trailing tokens → invalid.
    if !input.is_exhausted() {
        return None;
    }

    let mut spec = ColumnRuleSpec::default();
    if let Some(w) = width {
        spec.width = w;
    }
    if let Some(s) = style {
        spec.style = s;
    }
    if let Some(c) = color {
        spec.color = c;
    }
    Some(spec)
}

// ---------------------------------------------------------------------------
// Selector parser
// ---------------------------------------------------------------------------

/// Parse a comma-separated selector list. Returns an empty `Vec` when any
/// selector uses unsupported syntax (combinators, pseudo-classes, attribute
/// selectors, or whitespace descendant) — per Phase A scope discipline the
/// whole rule is dropped silently so authors do not get a half-applied
/// result.
pub fn parse_selector_list(input: &str) -> Vec<CompoundSelector> {
    let mut parser_input = ParserInput::new(input);
    let mut parser = Parser::new(&mut parser_input);

    let mut compounds = Vec::new();
    // `just_completed` signals the tokenizer yielded a compound-terminating
    // whitespace sequence: the next real token must be a `,` or EOF, else
    // we have a descendant combinator which Phase A does not model.
    let mut just_saw_whitespace = false;
    let mut current = CompoundSelector::default();

    loop {
        let tok_result = parser.next_including_whitespace();
        let tok = match tok_result {
            Ok(t) => t.clone(),
            Err(_) => break,
        };

        if matches!(tok, Token::WhiteSpace(_)) {
            // Record but don't act — the follow-up token decides whether
            // this whitespace was valid (around `,` / at end) or a
            // descendant combinator (before another selector part).
            just_saw_whitespace = !current.parts.is_empty();
            continue;
        }

        match tok {
            Token::Comma => {
                if current.parts.is_empty() {
                    return Vec::new();
                }
                compounds.push(std::mem::take(&mut current));
                just_saw_whitespace = false;
            }
            Token::Delim('.') => {
                if just_saw_whitespace {
                    return Vec::new();
                }
                match parser.next_including_whitespace() {
                    Ok(Token::Ident(name)) => {
                        current
                            .parts
                            .push(SimpleSelector::Class(name.as_ref().to_string()));
                    }
                    _ => return Vec::new(),
                }
                just_saw_whitespace = false;
            }
            Token::Delim('*') => {
                if just_saw_whitespace {
                    return Vec::new();
                }
                current.parts.push(SimpleSelector::Universal);
                just_saw_whitespace = false;
            }
            Token::IDHash(name) | Token::Hash(name) => {
                if just_saw_whitespace {
                    return Vec::new();
                }
                current
                    .parts
                    .push(SimpleSelector::Id(name.as_ref().to_string()));
                just_saw_whitespace = false;
            }
            Token::Ident(name) => {
                if just_saw_whitespace {
                    return Vec::new();
                }
                current
                    .parts
                    .push(SimpleSelector::Type(name.as_ref().to_ascii_lowercase()));
                just_saw_whitespace = false;
            }
            // Unsupported tokens: pseudo-classes (`:`), attribute
            // selectors (`[`), combinators (`>`, `+`, `~`), numbers, and
            // anything else the Phase A grammar does not model. Drop the
            // whole list.
            _ => return Vec::new(),
        }
    }

    if !current.parts.is_empty() {
        compounds.push(current);
    } else if compounds.is_empty() {
        return Vec::new();
    }
    compounds
}

/// Match a [`CompoundSelector`] against a blitz DOM node. Every
/// [`SimpleSelector`] in the compound must hold (logical AND).
pub fn matches_node(sel: &CompoundSelector, node: &blitz_dom::Node) -> bool {
    let Some(elem) = node.element_data() else {
        return false;
    };
    if sel.parts.is_empty() {
        return false;
    }
    sel.parts.iter().all(|part| match part {
        SimpleSelector::Universal => true,
        SimpleSelector::Type(tag) => elem.name.local.as_ref().eq_ignore_ascii_case(tag),
        SimpleSelector::Class(want) => crate::blitz_adapter::get_attr(elem, "class")
            .is_some_and(|classes| classes.split_whitespace().any(|c| c == want)),
        SimpleSelector::Id(want) => {
            crate::blitz_adapter::get_attr(elem, "id").is_some_and(|id| id == want)
        }
    })
}

// ---------------------------------------------------------------------------
// Stylesheet parser
// ---------------------------------------------------------------------------

/// Parse a top-level CSS source into [`StyleRule`]s. `@`-rules are skipped
/// entirely (returning `Err` from `parse_prelude` — cssparser's
/// `StyleSheetParser` then consumes and discards the block without calling
/// `parse_block`). Qualified rules whose prelude contains an unsupported
/// selector are dropped. Empty property blocks are kept out of the result
/// to save memory.
pub fn parse_stylesheet(source: &str) -> Vec<StyleRule> {
    let mut rules = Vec::new();
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);

    let mut sheet = SheetParser { rules: &mut rules };
    let iter = StyleSheetParser::new(&mut parser, &mut sheet);
    for item in iter {
        let _ = item;
    }
    rules
}

struct SheetParser<'a> {
    rules: &'a mut Vec<StyleRule>,
}

impl<'i, 'a> AtRuleParser<'i> for SheetParser<'a> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        // Terminate every at-rule at the prelude — `StyleSheetParser` will
        // then skip the associated block. GCPM's parser takes `@page` but
        // we are strictly after qualified rules.
        Err(input.new_error(BasicParseErrorKind::AtRuleInvalid(name)))
    }
}

impl<'i, 'a> QualifiedRuleParser<'i> for SheetParser<'a> {
    type Prelude = Vec<CompoundSelector>;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        // Slice out the raw selector text and hand it to the standalone
        // selector parser. This keeps cssparser's error recovery simple:
        // on an unsupported selector we return an empty list, which the
        // `parse_block` below checks.
        let start = input.position();
        while input.next_including_whitespace().is_ok() {}
        let raw = input.slice_from(start);
        let selectors = parse_selector_list(raw);
        if selectors.is_empty() {
            return Err(input.new_error(BasicParseErrorKind::QualifiedRuleInvalid));
        }
        Ok(selectors)
    }

    fn parse_block<'t>(
        &mut self,
        selectors: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, ()>> {
        let mut props = ColumnStyleProps::default();
        let mut decl_parser = ColumnDeclParser { props: &mut props };
        let iter = RuleBodyParser::new(input, &mut decl_parser);
        for item in iter {
            let _ = item;
        }
        if !props.is_empty() {
            self.rules.push(StyleRule { selectors, props });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Cascade driver
// ---------------------------------------------------------------------------

/// Build the per-document side-table by folding stylesheet rules (in source
/// order) and then inline `style` attributes (last — they beat the
/// stylesheet) into each node's entry. Nodes with no matching rule and no
/// inline declaration are skipped entirely.
pub fn build_column_style_table(
    doc: &blitz_html::HtmlDocument,
    stylesheet_rules: &[StyleRule],
) -> ColumnStyleTable {
    let mut table = ColumnStyleTable::new();
    let root = doc.root_element();
    walk(doc, root.id, stylesheet_rules, &mut table, 0);
    table
}

fn walk(
    doc: &blitz_html::HtmlDocument,
    node_id: usize,
    rules: &[StyleRule],
    table: &mut ColumnStyleTable,
    depth: usize,
) {
    if depth >= crate::MAX_DOM_DEPTH {
        return;
    }
    let Some(node) = doc.get_node(node_id) else {
        return;
    };

    if node.element_data().is_some() {
        let mut props = ColumnStyleProps::default();
        for rule in rules {
            if rule.selectors.iter().any(|sel| matches_node(sel, node)) {
                props.merge(rule.props);
            }
        }
        if let Some(elem) = node.element_data() {
            if let Some(inline) = crate::blitz_adapter::get_attr(elem, "style") {
                let inline_props = parse_declaration_block(inline);
                props.merge(inline_props);
            }
        }
        if !props.is_empty() {
            table.insert(node_id, props);
        }
    }

    for &child_id in &node.children {
        walk(doc, child_id, rules, table, depth + 1);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -------- declaration parsing --------

    #[test]
    fn parses_column_rule_longhand_triplet() {
        let css = "column-rule-width: 2pt; column-rule-style: solid; column-rule-color: red;";
        let props = parse_declaration_block(css);
        let rule = props.rule.expect("rule");
        assert!((rule.width - 2.0).abs() < 1e-3);
        assert_eq!(rule.style, ColumnRuleStyle::Solid);
        assert_eq!(rule.color, [255, 0, 0, 255]);
    }

    #[test]
    fn parses_column_rule_shorthand() {
        let props = parse_declaration_block("column-rule: 1px dashed #0a0;");
        let rule = props.rule.expect("rule");
        assert!((rule.width - 0.75).abs() < 1e-2); // 1px → 0.75pt
        assert_eq!(rule.style, ColumnRuleStyle::Dashed);
        assert_eq!(rule.color, [0x00, 0xAA, 0x00, 0xFF]);
    }

    #[test]
    fn parses_column_rule_shorthand_any_order() {
        let props = parse_declaration_block("column-rule: red 3pt dotted;");
        let rule = props.rule.expect("rule");
        assert!((rule.width - 3.0).abs() < 1e-3);
        assert_eq!(rule.style, ColumnRuleStyle::Dotted);
        assert_eq!(rule.color, [255, 0, 0, 255]);
    }

    #[test]
    fn column_rule_shorthand_partial_uses_defaults() {
        let props = parse_declaration_block("column-rule: dashed;");
        let rule = props.rule.expect("rule");
        assert_eq!(rule.style, ColumnRuleStyle::Dashed);
        // Defaults: 1pt, black.
        assert!((rule.width - 1.0).abs() < 1e-3);
        assert_eq!(rule.color, [0, 0, 0, 255]);
    }

    #[test]
    fn parses_column_fill_auto() {
        let props = parse_declaration_block("column-fill: auto;");
        assert_eq!(props.fill, Some(ColumnFill::Auto));
    }

    #[test]
    fn parses_column_fill_balance() {
        let props = parse_declaration_block("column-fill: balance;");
        assert_eq!(props.fill, Some(ColumnFill::Balance));
    }

    // -------- break-inside (fulgur-ftp) --------

    #[test]
    fn parse_break_inside_avoid_inline() {
        let props = parse_declaration_block("break-inside: avoid;");
        assert_eq!(props.break_inside, Some(BreakInside::Avoid));
    }

    #[test]
    fn parse_break_inside_avoid_page_and_column_collapse_to_avoid() {
        let p1 = parse_declaration_block("break-inside: avoid-page;");
        let p2 = parse_declaration_block("break-inside: avoid-column;");
        assert_eq!(p1.break_inside, Some(BreakInside::Avoid));
        assert_eq!(p2.break_inside, Some(BreakInside::Avoid));
    }

    #[test]
    fn parse_break_inside_auto_is_auto_variant() {
        let props = parse_declaration_block("break-inside: auto;");
        assert_eq!(props.break_inside, Some(BreakInside::Auto));
    }

    #[test]
    fn parse_break_inside_invalid_value_is_silently_dropped() {
        let props = parse_declaration_block("break-inside: banana;");
        assert_eq!(props.break_inside, None);
    }

    #[test]
    fn parse_break_inside_via_selector() {
        let rules = parse_stylesheet(".keep { break-inside: avoid; }");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].props.break_inside, Some(BreakInside::Avoid));
    }

    #[test]
    fn ignores_unsupported_rule_style() {
        // `double` is Phase C — the whole shorthand must drop.
        let props = parse_declaration_block("column-rule: 1pt double red;");
        assert!(props.rule.is_none());
    }

    #[test]
    fn px_length_converts_to_pt() {
        let props = parse_declaration_block("column-rule-width: 4px;");
        let w = props.rule.expect("rule").width;
        assert!((w - 3.0).abs() < 1e-3); // 4px * 72/96 = 3pt
    }

    #[test]
    fn em_length_resolves_against_default_basis() {
        // `1em` at the 16px fallback basis == 12pt.
        let props = parse_declaration_block("column-rule-width: 1em;");
        let rule = props.rule.expect("rule");
        assert!(
            (rule.width - 12.0).abs() < 1e-3,
            "1em expected 12pt (16px × 72/96), got {}",
            rule.width
        );
    }

    #[test]
    fn rem_length_resolves_against_default_basis() {
        // `0.25rem` == 4px at the 16px basis == 3pt.
        let props = parse_declaration_block("column-rule-width: 0.25rem;");
        let rule = props.rule.expect("rule");
        assert!(
            (rule.width - 3.0).abs() < 1e-3,
            "0.25rem expected 3pt (4px × 72/96), got {}",
            rule.width
        );
    }

    #[test]
    fn em_shorthand_resolves_against_default_basis() {
        let props = parse_declaration_block("column-rule: 0.1em solid red;");
        let rule = props.rule.expect("rule");
        // 0.1em × 16 px × 72/96 = 1.2pt
        assert!(
            (rule.width - 1.2).abs() < 1e-3,
            "0.1em expected 1.2pt, got {}",
            rule.width
        );
        assert_eq!(rule.style, ColumnRuleStyle::Solid);
        assert_eq!(rule.color, [255, 0, 0, 255]);
    }

    #[test]
    fn percent_length_still_drops() {
        // `%` requires a containing-block basis we don't have here.
        let props = parse_declaration_block("column-rule-width: 50%;");
        assert!(props.rule.is_none());
    }

    #[test]
    fn longhands_compose_across_declarations() {
        let css = "column-rule-width: 5pt; column-rule-color: blue;";
        let props = parse_declaration_block(css);
        let rule = props.rule.expect("rule");
        assert!((rule.width - 5.0).abs() < 1e-3);
        assert_eq!(rule.color, [0, 0, 255, 255]);
        // No `column-rule-style` — default `None` applies.
        assert_eq!(rule.style, ColumnRuleStyle::None);
    }

    #[test]
    fn invalid_declaration_does_not_poison_siblings() {
        // `1vh` has no containing-block basis at parse time → drops the
        // width declaration. Sibling `column-fill: auto` must still apply.
        let css = "column-rule-width: 1vh; column-fill: auto;";
        let props = parse_declaration_block(css);
        assert!(props.rule.is_none());
        assert_eq!(props.fill, Some(ColumnFill::Auto));
    }

    #[test]
    fn currentcolor_drops_color_longhand() {
        let props = parse_declaration_block("column-rule-color: currentcolor;");
        // cssparser doesn't know `currentcolor` as a named colour; we
        // return `None` which drops this declaration silently.
        assert!(props.rule.is_none());
    }

    #[test]
    fn parses_rgb_functional_notation() {
        let props = parse_declaration_block("column-rule-color: rgb(255, 0, 0);");
        assert_eq!(props.rule.expect("rule").color, [255, 0, 0, 255]);
    }

    #[test]
    fn parses_rgba_with_alpha() {
        let props = parse_declaration_block("column-rule-color: rgba(0, 0, 0, 0.5);");
        let c = props.rule.expect("rule").color;
        assert_eq!(&c[0..3], &[0, 0, 0]);
        // Allow ±1 rounding slack.
        assert!((c[3] as i32 - 128).abs() <= 1, "alpha was {}", c[3]);
    }

    #[test]
    fn parses_hsl_notation() {
        let props = parse_declaration_block("column-rule-color: hsl(0, 100%, 50%);");
        let c = props.rule.expect("rule").color;
        // pure red; allow ±2 slack on rounding
        assert!(c[0] >= 253 && c[1] <= 2 && c[2] <= 2, "got {:?}", c);
    }

    #[test]
    fn currentcolor_is_rejected() {
        let props = parse_declaration_block("column-rule-color: currentcolor;");
        assert!(props.rule.is_none() || props.rule.unwrap().style == ColumnRuleStyle::None);
    }

    // -------- selector parsing --------

    #[test]
    fn selector_parses_type_class_id_universal_and_compound() {
        let list = parse_selector_list("div.foo#bar");
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].parts,
            vec![
                SimpleSelector::Type("div".into()),
                SimpleSelector::Class("foo".into()),
                SimpleSelector::Id("bar".into()),
            ]
        );
    }

    #[test]
    fn selector_universal() {
        let list = parse_selector_list("*");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].parts, vec![SimpleSelector::Universal]);
    }

    #[test]
    fn selector_comma_list_parsed_as_multiple_compounds() {
        let list = parse_selector_list(".a, div, #x");
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].parts, vec![SimpleSelector::Class("a".into())]);
        assert_eq!(list[1].parts, vec![SimpleSelector::Type("div".into())]);
        assert_eq!(list[2].parts, vec![SimpleSelector::Id("x".into())]);
    }

    #[test]
    fn selector_unsupported_pseudo_drops_list() {
        assert!(parse_selector_list("a:hover").is_empty());
    }

    #[test]
    fn selector_unsupported_descendant_drops_list() {
        assert!(parse_selector_list("a b").is_empty());
    }

    #[test]
    fn selector_unsupported_child_combinator_drops_list() {
        assert!(parse_selector_list("a > b").is_empty());
    }

    #[test]
    fn selector_unsupported_attribute_drops_list() {
        assert!(parse_selector_list("input[type=x]").is_empty());
    }

    #[test]
    fn selector_tag_lowercased() {
        let list = parse_selector_list("DIV");
        assert_eq!(list[0].parts, vec![SimpleSelector::Type("div".into())]);
    }

    // -------- end-to-end matcher (via blitz_adapter::parse) --------

    fn build_doc(html: &str) -> blitz_html::HtmlDocument {
        crate::blitz_adapter::parse(html, 400.0, &[])
    }

    fn find_node_with<F>(doc: &blitz_html::HtmlDocument, pred: F) -> Option<usize>
    where
        F: Fn(&blitz_dom::Node) -> bool + Copy,
    {
        let root = doc.root_element();
        fn rec<F: Fn(&blitz_dom::Node) -> bool + Copy>(
            doc: &blitz_html::HtmlDocument,
            id: usize,
            depth: usize,
            pred: F,
        ) -> Option<usize> {
            if depth > 64 {
                return None;
            }
            let node = doc.get_node(id)?;
            if pred(node) {
                return Some(id);
            }
            for &c in &node.children {
                if let Some(r) = rec(doc, c, depth + 1, pred) {
                    return Some(r);
                }
            }
            None
        }
        rec(doc, root.id, 0, pred)
    }

    #[test]
    fn matches_node_by_tag_class_and_id() {
        let doc = build_doc(
            r#"<html><body>
                <div class="mc" id="one"></div>
                <p class="other"></p>
              </body></html>"#,
        );
        let div_id = find_node_with(&doc, |n| {
            n.element_data()
                .is_some_and(|e| e.name.local.as_ref() == "div")
        })
        .expect("div");
        let div = doc.get_node(div_id).unwrap();

        let t = CompoundSelector {
            parts: vec![SimpleSelector::Type("div".into())],
        };
        let c = CompoundSelector {
            parts: vec![SimpleSelector::Class("mc".into())],
        };
        let i = CompoundSelector {
            parts: vec![SimpleSelector::Id("one".into())],
        };
        let compound = CompoundSelector {
            parts: vec![
                SimpleSelector::Type("div".into()),
                SimpleSelector::Class("mc".into()),
                SimpleSelector::Id("one".into()),
            ],
        };
        let not_match = CompoundSelector {
            parts: vec![SimpleSelector::Class("other".into())],
        };

        assert!(matches_node(&t, div));
        assert!(matches_node(&c, div));
        assert!(matches_node(&i, div));
        assert!(matches_node(&compound, div));
        assert!(!matches_node(&not_match, div));
    }

    #[test]
    fn universal_selector_matches_every_element() {
        let doc = build_doc("<html><body><span></span></body></html>");
        let span_id = find_node_with(&doc, |n| {
            n.element_data()
                .is_some_and(|e| e.name.local.as_ref() == "span")
        })
        .expect("span");
        let span = doc.get_node(span_id).unwrap();
        let u = CompoundSelector {
            parts: vec![SimpleSelector::Universal],
        };
        assert!(matches_node(&u, span));
    }

    // -------- stylesheet parsing --------

    #[test]
    fn parses_stylesheet_with_multiple_rules() {
        let css = r#"
            .mc { column-rule: 1pt solid red; }
            #x { column-fill: auto; }
        "#;
        let rules = parse_stylesheet(css);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].selectors[0].parts.len(), 1);
        assert!(rules[0].props.rule.is_some());
        assert_eq!(rules[1].props.fill, Some(ColumnFill::Auto));
    }

    #[test]
    fn stylesheet_skips_at_rules() {
        let css = r#"
            @media print { .mc { column-rule: 1pt solid red; } }
            .mc { column-fill: auto; }
        "#;
        let rules = parse_stylesheet(css);
        // The `@media` block is skipped entirely — only the bare `.mc`
        // rule remains.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].props.fill, Some(ColumnFill::Auto));
    }

    #[test]
    fn stylesheet_drops_unsupported_selector() {
        let css = r#"
            a:hover { column-fill: auto; }
            .mc { column-fill: balance; }
        "#;
        let rules = parse_stylesheet(css);
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].selectors[0].parts[0],
            SimpleSelector::Class("mc".into())
        );
    }

    #[test]
    fn stylesheet_drops_rule_with_empty_properties() {
        // No column-* property — should not be returned.
        let css = ".mc { color: red; }";
        let rules = parse_stylesheet(css);
        assert!(rules.is_empty());
    }

    // -------- cascade --------

    #[test]
    fn inline_style_beats_stylesheet() {
        let html = r#"<html><head><style>
            .mc { column-rule: 1pt solid blue; }
        </style></head><body>
            <div class="mc" id="a"></div>
            <div class="mc" id="b" style="column-rule: 2pt dashed red"></div>
        </body></html>"#;
        let doc = build_doc(html);

        // Extract the <style> content manually so this test doesn't depend
        // on Task 2's harvester.
        let rules = parse_stylesheet(".mc { column-rule: 1pt solid blue; }");
        let table = build_column_style_table(&doc, &rules);

        let a_id = find_node_with(&doc, |n| {
            n.element_data()
                .and_then(|e| crate::blitz_adapter::get_attr(e, "id"))
                == Some("a")
        })
        .expect("a");
        let b_id = find_node_with(&doc, |n| {
            n.element_data()
                .and_then(|e| crate::blitz_adapter::get_attr(e, "id"))
                == Some("b")
        })
        .expect("b");

        let a = table.get(&a_id).expect("a entry");
        let a_rule = a.rule.expect("a rule");
        assert_eq!(a_rule.color, [0, 0, 255, 255]);
        assert_eq!(a_rule.style, ColumnRuleStyle::Solid);

        let b = table.get(&b_id).expect("b entry");
        let b_rule = b.rule.expect("b rule");
        assert!((b_rule.width - 2.0).abs() < 1e-3);
        assert_eq!(b_rule.style, ColumnRuleStyle::Dashed);
        assert_eq!(b_rule.color, [255, 0, 0, 255]);
    }

    #[test]
    fn later_stylesheet_rule_wins_on_conflict() {
        let css = r#"
            .mc { column-fill: balance; }
            .mc { column-fill: auto; }
        "#;
        let rules = parse_stylesheet(css);
        let html = r#"<html><body><div class="mc" id="a"></div></body></html>"#;
        let doc = build_doc(html);
        let table = build_column_style_table(&doc, &rules);
        let a_id = find_node_with(&doc, |n| {
            n.element_data()
                .and_then(|e| crate::blitz_adapter::get_attr(e, "id"))
                == Some("a")
        })
        .expect("a");
        assert_eq!(table.get(&a_id).expect("a").fill, Some(ColumnFill::Auto));
    }

    #[test]
    fn nodes_without_matching_rule_are_absent_from_table() {
        let css = ".mc { column-fill: auto; }";
        let rules = parse_stylesheet(css);
        let html = r#"<html><body><div id="nope"></div></body></html>"#;
        let doc = build_doc(html);
        let table = build_column_style_table(&doc, &rules);
        // Nothing matches `.mc`, so the table should be empty.
        assert!(table.is_empty());
    }

    #[test]
    fn comma_selector_list_matches_any_compound() {
        let css = "div, #a { column-fill: auto; }";
        let rules = parse_stylesheet(css);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].selectors.len(), 2);

        let html = r#"<html><body>
            <div></div>
            <p id="a"></p>
            <p id="b"></p>
        </body></html>"#;
        let doc = build_doc(html);
        let table = build_column_style_table(&doc, &rules);

        let div_id = find_node_with(&doc, |n| {
            n.element_data()
                .is_some_and(|e| e.name.local.as_ref() == "div")
        })
        .expect("div");
        let a_id = find_node_with(&doc, |n| {
            n.element_data()
                .and_then(|e| crate::blitz_adapter::get_attr(e, "id"))
                == Some("a")
        })
        .expect("a");
        let b_id = find_node_with(&doc, |n| {
            n.element_data()
                .and_then(|e| crate::blitz_adapter::get_attr(e, "id"))
                == Some("b")
        })
        .expect("b");

        assert_eq!(table.get(&div_id).unwrap().fill, Some(ColumnFill::Auto));
        assert_eq!(table.get(&a_id).unwrap().fill, Some(ColumnFill::Auto));
        assert!(!table.contains_key(&b_id));
    }
}
