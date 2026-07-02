// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The text engine core: bidi-aware paragraph wrapping with a logical↔visual
//! cursor map, plus pagination and scrolling as views over one model
//! (design note §3). The geometry-free slot-flow core that [`crate::runaround`]
//! builds on ([`wrap_into_slots`]) also lives here.
//!
//! ## Pipeline (per paragraph → per visual line)
//!
//! The four stages of §3.1 run in this order, and the order is load-bearing:
//!
//! 1. **Line-break opportunities** in *logical* order (UAX #14,
//!    [`unicode_linebreak`]).
//! 2. **Greedy width fill** of the available width using grapheme-cluster widths
//!    ([`unicode_segmentation`] + [`unicode_width`]).
//! 3. **BiDi reordering** per visual line (UAX #9, [`unicode_bidi`]): resolve
//!    embedding levels with full paragraph context, then reorder each wrapped
//!    line's runs into visual order.
//! 4. **Emit cells in visual order.** Terminals render cells in memory order, so
//!    the engine hands them over already visually ordered — never trusting the
//!    emulator to reorder ([`VisualLine::cells`]).
//!
//! Breaking and width-fill happen in **logical** order (stages 1–2); reordering
//! is applied **per wrapped line** afterward (stage 3). BiDi changes only the
//! *order* of graphemes, never their widths, so the width fill is correct
//! regardless of direction.
//!
//! ## Bidirectional from day one
//!
//! The bidi machinery is present and correct from this first commit even though
//! the default base direction is LTR ([`BaseDirection::Ltr`]). Pure-LTR text
//! reorders to itself (the identity permutation), so the bidi path is exercised
//! but provably inert on LTR — retrofitting direction later would mean touching
//! cursor movement, selection, wrapping, and width math all at once, which §3.1
//! calls the universal regret.
//!
//! ## The logical↔visual cursor map (§3.2)
//!
//! Each [`VisualLine`] carries a [`CursorMap`]: a bijection between the line's
//! *logical* grapheme order (edit order) and its *visual* order (display order).
//! Arrow-right moves visually but edits logically, so cursor motion is
//! `logical → visual → step → logical`; a selection that crosses a direction
//! boundary is a contiguous *logical* range that may map to a discontiguous
//! visual span, which is the correct bidi behavior rather than a bug. The map is
//! a first-class output of layout, not a derived convenience.
//!
//! ## The chrome/content boundary (§3.3)
//!
//! Only *flowed content* is reordered. Borders, junctions, and table rules stay
//! LTR and never pass through this engine. Per the locked decision in §6.3, bidi
//! reaches **everything that flows** — prose bodies, table cells, and node
//! labels — through the *same* path: [`shape_line`] is the single-line primitive
//! a table cell or a flowing label uses, so there is no seam where one path is
//! bidi-correct and another is not.

use std::ops::Range;

use unicode_bidi::{BidiInfo, Level};
use unicode_linebreak::{linebreaks, BreakOpportunity};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::style::Style;

// ── BaseDirection ────────────────────────────────────────────────────────────

/// The base (paragraph) direction used to resolve embedding levels (UAX #9 P2/P3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaseDirection {
    /// Force a left-to-right base. The default — the first visible milestone is
    /// LTR, and LTR input then reorders to the identity.
    #[default]
    Ltr,
    /// Force a right-to-left base (the whole paragraph reads RTL).
    Rtl,
    /// Auto-detect from the first strong character (UAX #9 rule P2/P3).
    Auto,
}

impl BaseDirection {
    /// The `unicode-bidi` base level: `Some` to force a direction, `None` to
    /// auto-detect.
    fn to_level(self) -> Option<Level> {
        match self {
            BaseDirection::Ltr => Some(Level::ltr()),
            BaseDirection::Rtl => Some(Level::rtl()),
            BaseDirection::Auto => None,
        }
    }
}

// ── TextCtx ────────────────────────────────────────────────────────────────

/// How ASCII digits `0`–`9` are shaped for **display** (never mutates stored text).
///
/// Arabic-Indic and Extended Arabic-Indic digits are all width-1, so shaping
/// preserves display width — safe to apply *after* width/column math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DigitShaping {
    /// Western digits, verbatim (the default).
    #[default]
    None,
    /// Arabic-Indic digits ٠١٢٣٤٥٦٧٨٩ (U+0660…), used across the Arabic script.
    ArabicIndic,
    /// Extended Arabic-Indic digits ۰۱۲۳۴۵۶۷۸۹ (U+06F0…), for Persian/Urdu.
    ExtendedArabicIndic,
}

/// A cheap, `Copy` locale/direction context the app threads into the chrome and
/// edit primitives, so one decision reaches labels, fields, tables, truncation and
/// digit shaping without a direction argument at every call site.
///
/// mullion holds no copy of it — it is a value passed by argument, the same
/// category as [`Style`](crate::style::Style). It pairs a base [`BaseDirection`]
/// (for bidi shaping and layout mirroring) with a [`DigitShaping`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextCtx {
    /// Base paragraph direction for bidi shaping and RTL layout mirroring.
    pub base: BaseDirection,
    /// Display-only digit shaping.
    pub digits: DigitShaping,
}

impl TextCtx {
    /// Left-to-right, Western digits — the default.
    pub const LTR: TextCtx = TextCtx { base: BaseDirection::Ltr, digits: DigitShaping::None };

    /// Right-to-left, Western digits (set [`digits`](TextCtx::digits) as needed).
    pub fn rtl() -> Self {
        Self { base: BaseDirection::Rtl, ..Self::LTR }
    }
}

/// Substitute ASCII digits per `shaping`, borrowing unchanged when there is nothing
/// to shape — so the common `None`/no-digit path never allocates.
///
/// Shaped digits are width-1, so the result has identical display width: apply this
/// *after* any column/width computation, never before.
pub fn shape_digits(s: &str, shaping: DigitShaping) -> std::borrow::Cow<'_, str> {
    let base = match shaping {
        DigitShaping::None => return std::borrow::Cow::Borrowed(s),
        DigitShaping::ArabicIndic => 0x0660u32,
        DigitShaping::ExtendedArabicIndic => 0x06F0u32,
    };
    if !s.bytes().any(|b| b.is_ascii_digit()) {
        return std::borrow::Cow::Borrowed(s);
    }
    let shaped = s
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                char::from_u32(base + (c as u32 - '0' as u32)).unwrap_or(c)
            } else {
                c
            }
        })
        .collect();
    std::borrow::Cow::Owned(shaped)
}

// ── VisualCell ───────────────────────────────────────────────────────────────

/// One grapheme cluster positioned in **visual** (display) order.
///
/// Cells are emitted left-to-right; a renderer advances the cursor by
/// [`width`](VisualCell::width) after each. The [`source_byte`](VisualCell::source_byte)
/// links the cell back to the paragraph for editing and selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualCell {
    /// The grapheme cluster to draw (a base char plus any combining marks).
    pub symbol: String,
    /// Display width in columns: 1 or 2 (wide CJK / emoji are 2).
    pub width: u8,
    /// Byte offset of this grapheme's first byte in the source paragraph.
    pub source_byte: usize,
}

// ── CursorMap ────────────────────────────────────────────────────────────────

/// The per-line bijection between logical and visual grapheme order (§3.2).
///
/// For a visual line of `n` graphemes, both directions are permutations of
/// `0..n` and are exact inverses. `visual_to_logical` and `logical_to_visual`
/// therefore round-trip to the identity, which is the invariant a coherent
/// cursor and selection depend on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorMap {
    /// `v2l[v]` = logical index of the grapheme shown at visual slot `v`.
    v2l: Vec<usize>,
    /// `l2v[l]` = visual slot of the `l`-th logical grapheme (inverse of `v2l`).
    l2v: Vec<usize>,
}

impl CursorMap {
    /// Build a map from a visual→logical permutation, deriving its inverse.
    ///
    /// `v2l` must be a permutation of `0..v2l.len()`; the inverse `l2v` is
    /// computed by scattering each visual index into its logical slot.
    fn from_v2l(v2l: Vec<usize>) -> Self {
        let mut l2v = vec![0usize; v2l.len()];
        for (v, &l) in v2l.iter().enumerate() {
            l2v[l] = v;
        }
        Self { v2l, l2v }
    }

    /// Number of graphemes on the line (the bijection's domain size).
    pub fn len(&self) -> usize {
        self.v2l.len()
    }

    /// `true` when the line has no graphemes.
    pub fn is_empty(&self) -> bool {
        self.v2l.is_empty()
    }

    /// The logical index of the grapheme at visual slot `v`, or `None` if out of
    /// range.
    pub fn visual_to_logical(&self, v: usize) -> Option<usize> {
        self.v2l.get(v).copied()
    }

    /// The visual slot of the `l`-th logical grapheme, or `None` if out of range.
    pub fn logical_to_visual(&self, l: usize) -> Option<usize> {
        self.l2v.get(l).copied()
    }
}

// ── VisualLine ───────────────────────────────────────────────────────────────

/// One wrapped line: its cells in visual order, the cursor map, and the source
/// byte range it was laid out from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualLine {
    /// Cells in visual (left-to-right) order, ready to emit.
    pub cells: Vec<VisualCell>,
    /// The logical↔visual bijection for this line (§3.2).
    pub map: CursorMap,
    /// Byte range of this line within the source paragraph.
    pub source: Range<usize>,
}

impl VisualLine {
    /// Total display width of the line (sum of cell widths).
    pub fn width(&self) -> u16 {
        self.cells.iter().map(|c| c.width as u16).sum()
    }
}

// ── WrappedText ──────────────────────────────────────────────────────────────

/// A paragraph laid out to a fixed width: an ordered list of [`VisualLine`]s.
///
/// Pagination and continuous scrolling are two *views* over this one model
/// (§3.4): [`page`](WrappedText::page) chunks the lines into fixed-height pages;
/// [`visible`](WrappedText::visible) returns an arbitrary scroll window. Both are
/// viewport-bounded slices — neither re-wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedText {
    lines: Vec<VisualLine>,
    width: u16,
}

impl WrappedText {
    /// The wrapped visual lines, in top-to-bottom order.
    pub fn lines(&self) -> &[VisualLine] {
        &self.lines
    }

    /// Number of wrapped lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The width the paragraph was wrapped to.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Number of fixed-height pages of `page_height` lines each (§3.4).
    ///
    /// Returns `0` when `page_height` is `0` (a degenerate page holds nothing).
    pub fn page_count(&self, page_height: usize) -> usize {
        if page_height == 0 {
            0
        } else {
            self.lines.len().div_ceil(page_height)
        }
    }

    /// The lines of page `index` (0-based) at `page_height` lines per page.
    ///
    /// Returns an empty slice when `page_height` is `0` or `index` is past the
    /// last page. The final page may be short.
    pub fn page(&self, index: usize, page_height: usize) -> &[VisualLine] {
        if page_height == 0 {
            return &[];
        }
        let start = index.saturating_mul(page_height);
        if start >= self.lines.len() {
            return &[];
        }
        let end = start.saturating_add(page_height).min(self.lines.len());
        &self.lines[start..end]
    }

    /// The scroll window: up to `height` lines starting at `scroll_top` (§3.4).
    ///
    /// `scroll_top` past the end yields an empty slice; the window is clamped to
    /// the available lines, never re-wrapping.
    pub fn visible(&self, scroll_top: usize, height: usize) -> &[VisualLine] {
        let start = scroll_top.min(self.lines.len());
        let end = scroll_top.saturating_add(height).min(self.lines.len());
        &self.lines[start..end]
    }
}

// ── Wrapping ─────────────────────────────────────────────────────────────────

/// One source grapheme cluster, with its byte offset and display width.
struct Grapheme {
    /// Byte offset of the cluster's first byte in the source string.
    byte: usize,
    /// Display width in columns (already clamped to 0, 1, or 2).
    width: u8,
    /// `true` for a breakable space (ASCII space or tab): such graphemes "hang"
    /// past the wrap width rather than forcing a wrap, and are trimmed from the
    /// end of a soft-wrapped line so trailing whitespace never counts toward the
    /// rendered width.
    is_space: bool,
}

/// Display width of a grapheme cluster as a terminal cell count (0, 1, or 2).
///
/// Control clusters (newline, tab, other C0/C1) are treated as **zero-width**:
/// `unicode-width` does not report them as zero, but they occupy no cell and the
/// engine drops them rather than emitting a stray box. Other clusters are clamped
/// to 2, matching the buffer's continuation-cell model.
fn cell_width(g: &str) -> u8 {
    if g.chars().all(|c| c.is_control()) {
        0
    } else {
        UnicodeWidthStr::width(g).min(2) as u8
    }
}

/// Segment `text` into grapheme clusters with byte offsets and display widths.
fn segment(text: &str) -> Vec<Grapheme> {
    text.grapheme_indices(true)
        .map(|(byte, g)| Grapheme {
            byte,
            width: cell_width(g),
            // Only ASCII space/tab are treated as hangable; non-breaking spaces
            // are intentionally excluded so they are never hung or trimmed.
            is_space: !g.is_empty() && g.chars().all(|c| c == ' ' || c == '\t'),
        })
        .collect()
}

/// Break-opportunity flags at each grapheme boundary, used by the width fill.
///
/// Both vectors are length `n + 1` (one slot per boundary, including the end).
/// Slot `i` describes a break *before* grapheme `i`; slot `n` is the end of text.
struct BreakFlags {
    /// `allowed[i]` — a line may break before grapheme `i` (UAX #14 Allowed or
    /// Mandatory; the end of text counts as allowed).
    allowed: Vec<bool>,
    /// `mandatory[i]` — a line *must* break before grapheme `i` (a hard newline).
    mandatory: Vec<bool>,
}

/// Compute UAX #14 break flags at grapheme boundaries for `text`.
///
/// `unicode-linebreak` reports each opportunity as the byte index *before which*
/// a break may occur; we translate those byte indices to grapheme-boundary
/// indices via `byte_to_boundary` (built from the segmented graphemes). An
/// opportunity that does not land on a grapheme boundary is ignored (it cannot
/// split a cluster).
fn break_flags(text: &str, graphemes: &[Grapheme]) -> BreakFlags {
    let n = graphemes.len();
    let mut allowed = vec![false; n + 1];
    let mut mandatory = vec![false; n + 1];

    // Map a byte offset → grapheme-boundary index. `text.len()` maps to `n`.
    let mut byte_to_boundary = std::collections::HashMap::with_capacity(n + 1);
    for (i, g) in graphemes.iter().enumerate() {
        byte_to_boundary.insert(g.byte, i);
    }
    byte_to_boundary.insert(text.len(), n);

    for (byte, opportunity) in linebreaks(text) {
        if let Some(&i) = byte_to_boundary.get(&byte) {
            allowed[i] = true;
            if opportunity == BreakOpportunity::Mandatory {
                mandatory[i] = true;
            }
        }
    }
    BreakFlags { allowed, mandatory }
}

/// Greedily fill graphemes into lines of at most `width` columns, returning each
/// line as a grapheme-index range `[start, end)` (stage 2 of the pipeline).
///
/// A thin loop over [`fill_one_line`], which does the per-line greedy walk (where
/// the break/space/newline rules live). Each returned line is trimmed of trailing
/// spaces.
///
/// # Degenerate case
/// When even the first grapheme is wider than `width` (only possible at
/// `width < 2`), [`fill_one_line`] returns `start`; this loop emits that grapheme
/// alone — the one situation where a line may exceed `width` — so the walk makes
/// progress instead of stalling.
fn fill_lines(graphemes: &[Grapheme], flags: &BreakFlags, width: u16) -> Vec<Range<usize>> {
    let n = graphemes.len();
    let target = width as u32;
    let mut lines: Vec<Range<usize>> = Vec::new();
    let mut start = 0usize;

    // Flat wrapping is the obstacle-free special case of the slot stream: an
    // unbounded run of equal-width slots. The same per-line kernel serves both.
    while start < n {
        // maxw == target: every line is the same width, so a word never "moves on"
        // — fill_one_line hard-breaks an over-wide word exactly as a plain wrap.
        let end = fill_one_line(graphemes, flags, start, target, target);
        if end == start {
            // Oversized lone grapheme (width < 2 regime): emit it by itself so the
            // walk makes progress; this is the one case a line may exceed `width`.
            lines.push(start..start + 1);
            start += 1;
        } else {
            lines.push(trim_trailing_spaces(graphemes, start..end));
            start = end;
        }
    }
    lines
}

/// Greedily fill one line/slot, returning the grapheme index where it ends.
///
/// Walks forward from `start`, accumulating widths, until the next visible
/// grapheme would overflow `target`, or a hard newline forces the line to end, or
/// the text runs out. Breakable spaces hang past the edge rather than forcing a
/// wrap (the caller trims them). The returned range is **not** trimmed.
///
/// On overflow with whole words already placed, it wraps at the last allowed
/// break. Overflow *mid-first-word* (no break opportunity yet) is the interesting
/// case: the word does not fit this slot, so it is **kept whole and moved on** —
/// the function returns `start` (an empty result) so the caller can retry it in a
/// wider slot. It hard-breaks the word only when `target >= maxw`, i.e. this slot
/// is already as wide as any available, so no wider slot exists to move it to.
///
/// # Parameters
/// - `target`: the width to fill.
/// - `maxw`: the widest target among all slots the caller will offer. For flat
///   wrapping every line has the same width, so `maxw == target` and the
///   move-on case never triggers — behavior is identical to a plain greedy wrap.
///
/// # Returns
/// The exclusive end grapheme index. `end == start` means nothing was placed:
/// either the first grapheme is wider than `target`, or the first word does not
/// fit and a wider slot exists for it.
fn fill_one_line(
    graphemes: &[Grapheme],
    flags: &BreakFlags,
    start: usize,
    target: u32,
    maxw: u32,
) -> usize {
    let n = graphemes.len();
    let mut i = start;
    let mut acc = 0u32; // accumulated width of [start, i), trailing spaces included
    let mut last_break: Option<usize> = None; // best break boundary seen in-line

    while i < n {
        // A hard newline before grapheme `i` ends this line.
        if i > start && flags.mandatory[i] {
            return i;
        }
        let g = &graphemes[i];
        let w = g.width as u32;
        // Spaces hang past the edge; only a visible grapheme that overflows wraps.
        if w > 0 && acc + w > target && !g.is_space {
            return if i > start {
                match last_break {
                    // Whole words fit before this one: wrap at the word boundary.
                    Some(b) if b > start => b,
                    // Mid-first-word overflow: keep the word whole and move it to a
                    // wider slot (return `start`), unless this slot is already the
                    // widest available, in which case hard-break it here.
                    _ if target >= maxw => i,
                    _ => start,
                }
            } else {
                start // first visible grapheme too wide for `target`
            };
        }
        acc += w;
        i += 1;
        // Record an allowed break at the boundary we just crossed.
        if i <= n && flags.allowed[i] {
            last_break = Some(i);
        }
    }
    n
}

/// Flow graphemes into a finite sequence of slots of the given widths, returning
/// one grapheme-index range per slot (in slot order) — the runaround core (§3.5).
///
/// This is [`fill_lines`] generalized from "an unbounded run of equal-width
/// lines" to "a bounded list of arbitrary-width slots", through the same
/// [`fill_one_line`] kernel — so the obstacle-free case (one full-width slot per
/// row) reduces to flat wrapping exactly.
///
/// **Words are kept whole**: a slot too narrow for the next word is left **empty**
/// and the word is retried on a following, wider slot (so no glyph is placed where
/// it does not fit, and no word is split mid-letter just because a gap between
/// tiles is small). A word is hard-broken only when it is wider than *every* slot
/// (`maxw` below), which also guarantees the fill always makes progress. Text that
/// outlasts the slots is dropped (it falls below the viewport). Ranges are trimmed
/// of trailing spaces.
fn fill_slots(graphemes: &[Grapheme], flags: &BreakFlags, widths: &[u16]) -> Vec<Range<usize>> {
    let n = graphemes.len();
    let mut out = Vec::with_capacity(widths.len());
    let mut start = 0usize;
    // The widest slot: a word wider than this fits nowhere whole, so it may be
    // hard-broken; any narrower word is moved on until a slot fits it.
    let maxw = widths.iter().copied().max().unwrap_or(0) as u32;

    for &w in widths {
        if start >= n {
            out.push(start..start); // text exhausted — remaining slots are empty
            continue;
        }
        let end = fill_one_line(graphemes, flags, start, w as u32, maxw);
        if end == start {
            // This slot held no whole word: leave it empty, retry on a later slot.
            out.push(start..start);
        } else {
            out.push(trim_trailing_spaces(graphemes, start..end));
            start = end;
        }
    }
    out
}

/// Shrink a grapheme-index range from the right, dropping trailing breakable
/// spaces and zero-width clusters (e.g. the newline itself) so neither is
/// rendered or counted toward line width. The zero-width skip matters when
/// spaces sit just before a hard newline: trimming must see past the newline to
/// reach the spaces behind it.
fn trim_trailing_spaces(graphemes: &[Grapheme], range: Range<usize>) -> Range<usize> {
    let mut end = range.end;
    while end > range.start && (graphemes[end - 1].is_space || graphemes[end - 1].width == 0) {
        end -= 1;
    }
    range.start..end
}

/// Convert a grapheme-index range to a byte range within `text`.
///
/// A grapheme index at or past the end maps to `text.len()`, so an empty range
/// `n..n` (an empty slot in [`fill_slots`]) becomes an empty byte range rather
/// than panicking.
fn byte_range(text: &str, graphemes: &[Grapheme], gr: Range<usize>) -> Range<usize> {
    let start = graphemes.get(gr.start).map(|g| g.byte).unwrap_or(text.len());
    let end = graphemes.get(gr.end).map(|g| g.byte).unwrap_or(text.len());
    start..end
}

/// Build one [`VisualLine`] for the source byte range `line`, reordering it into
/// visual order with the paragraph context in `info` (stages 3–4).
///
/// Logical graphemes (zero-width clusters dropped) are collected left-to-right.
/// `BidiInfo::visual_runs` yields the line's level runs already in visual order;
/// each run is walked in source order for an LTR level and reversed for an RTL
/// level (UAX #9 rule L2), giving the visual→logical permutation directly. An
/// empty line, or one with no owning paragraph, maps to the identity.
fn build_line(text: &str, line: Range<usize>, info: &BidiInfo) -> VisualLine {
    // Logical graphemes of this line, in source (byte-ascending) order. Drop
    // zero-width clusters (newline/control): they are not navigable cells.
    let logical: Vec<(usize, &str)> = text[line.clone()]
        .grapheme_indices(true)
        .map(|(off, g)| (line.start + off, g))
        .filter(|(_, g)| cell_width(g) > 0)
        .collect();
    let n = logical.len();

    // The paragraph whose range contains this line (mandatory breaks align with
    // bidi paragraph boundaries, so a line lies within exactly one paragraph).
    let para = info
        .paragraphs
        .iter()
        .find(|p| line.start >= p.range.start && line.start < p.range.end);

    let v2l: Vec<usize> = match para {
        Some(para) => {
            // `visual_runs` returns per-byte levels (absolute) and the line's
            // runs already in visual order, as absolute byte ranges.
            let (levels, runs) = info.visual_runs(para, line.clone());
            let mut order = Vec::with_capacity(n);
            for run in runs {
                let rtl = levels[run.start].is_rtl();
                // Logical indices whose grapheme starts inside this run.
                let mut idxs: Vec<usize> = (0..n)
                    .filter(|&k| logical[k].0 >= run.start && logical[k].0 < run.end)
                    .collect();
                idxs.sort_unstable_by_key(|&k| logical[k].0); // source order
                if rtl {
                    idxs.reverse(); // L2: reverse runs at an odd (RTL) level
                }
                order.extend(idxs);
            }
            order
        }
        // No owning paragraph (e.g. an empty line): nothing to reorder.
        None => (0..n).collect(),
    };

    let cells = v2l
        .iter()
        .map(|&l| {
            let (byte, g) = logical[l];
            VisualCell {
                symbol: g.to_string(),
                width: cell_width(g),
                source_byte: byte,
            }
        })
        .collect();

    VisualLine {
        cells,
        map: CursorMap::from_v2l(v2l),
        source: line,
    }
}

/// Wrap `text` to `width` columns, producing a bidi-correct [`WrappedText`].
///
/// Runs the full §3.1 pipeline: UAX #14 break opportunities, greedy width fill in
/// logical order, then per-line UAX #9 reordering with `base` direction. The
/// embedding levels are resolved once over the whole paragraph so each wrapped
/// line reorders with correct context.
///
/// # Examples
/// ```
/// use mullion::text::{wrap, BaseDirection};
///
/// let w = wrap("hello world", 5, BaseDirection::Ltr);
/// assert_eq!(w.line_count(), 2);
/// // Pure-LTR text reorders to itself: visual order == logical order.
/// let first = &w.lines()[0];
/// assert_eq!(first.map.visual_to_logical(0), Some(0));
/// ```
pub fn wrap(text: &str, width: u16, base: BaseDirection) -> WrappedText {
    let graphemes = segment(text);
    if graphemes.is_empty() {
        return WrappedText { lines: Vec::new(), width };
    }
    let flags = break_flags(text, &graphemes);
    let info = BidiInfo::new(text, base.to_level());

    let lines = fill_lines(&graphemes, &flags, width)
        .into_iter()
        .map(|gr| {
            let bytes = byte_range(text, &graphemes, gr);
            build_line(text, bytes, &info)
        })
        .collect();

    WrappedText { lines, width }
}

/// Flow `text` into a sequence of slots of the given `widths`, producing one
/// bidi-correct [`VisualLine`] per slot — the geometry-free core of runaround
/// (§3.5).
///
/// Tokens are flowed greedily into the slots in order: each slot is filled to its
/// own width, the next picking up where the last left off (so a tile splits a row
/// into "left of tile" then "right of tile" slots that flow in sequence). The
/// returned vector has exactly `widths.len()` entries, aligned by index to
/// `widths`; a slot that received no text is an empty line, and text that outlasts
/// the slots is dropped. Callers (see [`crate::runaround`]) attach each line's
/// row/column from the slot geometry.
///
/// **Words are kept whole.** A word that does not fit the current slot is moved on
/// to a later, wider slot rather than split mid-letter (leaving the narrow slot
/// empty); a word is hard-broken across slots only when it is wider than *every*
/// slot, so it fits nowhere whole. Because this routes through the same kernel as
/// [`wrap`], a single full-width slot per row — where every slot is the widest, so
/// no word can move on — reproduces flat wrapping exactly.
pub fn wrap_into_slots(text: &str, widths: &[u16], base: BaseDirection) -> Vec<VisualLine> {
    let graphemes = segment(text);
    let info = BidiInfo::new(text, base.to_level());
    if graphemes.is_empty() {
        // No content: every slot is an empty line, preserving positional alignment.
        return widths.iter().map(|_| empty_visual_line()).collect();
    }
    let flags = break_flags(text, &graphemes);
    fill_slots(&graphemes, &flags, widths)
        .into_iter()
        .map(|gr| build_line(text, byte_range(text, &graphemes, gr), &info))
        .collect()
}

/// An empty visual line (no cells, identity map) at the document origin.
fn empty_visual_line() -> VisualLine {
    VisualLine {
        cells: Vec::new(),
        map: CursorMap::from_v2l(Vec::new()),
        source: 0..0,
    }
}

/// Shape `text` as a single bidi-correct visual line — the primitive for flowed
/// chrome-adjacent content such as a table cell or a flowing label (§3.3/§6.3).
///
/// No wrapping is performed: the whole string becomes one [`VisualLine`] in
/// visual order. The `_width` argument is accepted for call-site symmetry with
/// [`wrap`] but is intentionally unused — the line is returned unclipped so the
/// cell↔source mapping stays intact; a renderer (e.g. [`render_line`]) clips to
/// the target width instead. `base` selects the base direction as in [`wrap`].
pub fn shape_line(text: &str, _width: u16, base: BaseDirection) -> VisualLine {
    if text.is_empty() {
        return empty_visual_line();
    }
    let info = BidiInfo::new(text, base.to_level());
    build_line(text, 0..text.len(), &info)
}

/// Truncate `text` to at most `max_cols` display columns — grapheme-cluster and
/// width correct, and **direction-aware** — returning the shaped [`VisualLine`]
/// ready for [`render_line`].
///
/// The reading-leading side is kept and a single-width `…` marks where content was
/// dropped: under LTR the ellipsis sits on the right, under RTL on the left. A wide
/// grapheme that will not fit the budget is dropped whole (never a half-glyph).
/// When `text` already fits, the full shaped line is returned unchanged.
pub fn elide(text: &str, max_cols: u16, ctx: TextCtx) -> VisualLine {
    let line = shape_line(text, 0, ctx.base);
    if max_cols == 0 {
        return empty_visual_line();
    }
    if line.width() <= max_cols {
        return line;
    }
    let budget = max_cols - 1; // one column for the ellipsis
    let rtl = matches!(ctx.base, BaseDirection::Rtl);
    let ell = VisualCell { symbol: "…".to_string(), width: 1, source_byte: text.len() };

    // Collect cells from the reading-leading side (visual-left for LTR, visual-right
    // for RTL) until the budget is spent.
    let mut kept: Vec<VisualCell> = Vec::new();
    let mut used = 0u16;
    let take = |cell: &VisualCell, used: &mut u16, kept: &mut Vec<VisualCell>| -> bool {
        let w = cell.width as u16;
        if *used + w > budget {
            return false;
        }
        kept.push(cell.clone());
        *used += w;
        true
    };
    if rtl {
        for cell in line.cells.iter().rev() {
            if !take(cell, &mut used, &mut kept) { break; }
        }
    } else {
        for cell in line.cells.iter() {
            if !take(cell, &mut used, &mut kept) { break; }
        }
    }

    // Reassemble in visual (left-to-right) order with the ellipsis on the trailing side.
    let cells: Vec<VisualCell> = if rtl {
        let mut v = Vec::with_capacity(kept.len() + 1);
        v.push(ell);
        v.extend(kept.into_iter().rev()); // kept was gathered right-to-left
        v
    } else {
        kept.push(ell);
        kept
    };

    let map = CursorMap::from_v2l((0..cells.len()).collect());
    VisualLine { cells, map, source: 0..text.len() }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Draw one visual line into `buf` at `(x, y)`, clipped to `max_width` columns.
///
/// Cells are emitted in visual order via [`Buffer::set_grapheme`], which handles
/// the wide-grapheme continuation cell. A wide grapheme that would straddle the
/// right clip edge is not drawn (no half-glyph). Returns the number of columns
/// written.
pub fn render_line(buf: &mut Buffer, x: u16, y: u16, line: &VisualLine, max_width: u16, style: Style) -> u16 {
    let limit = x.saturating_add(max_width);
    let mut cx = x;
    for cell in &line.cells {
        let w = cell.width as u16;
        if cx.saturating_add(w) > limit {
            break; // next cell would cross the clip edge
        }
        buf.set_grapheme(cx, y, &cell.symbol, style);
        cx = cx.saturating_add(w);
    }
    cx - x
}

/// Render the scroll window of `text` into `area`, one visual line per row
/// starting at `scroll_top` (§3.4).
///
/// Lines are clipped to `area.width`; rows past the end of the text are left
/// blank. Returns the number of lines drawn. This is viewport-bounded — only the
/// visible window is touched.
pub fn render_wrapped(buf: &mut Buffer, area: Rect, text: &WrappedText, scroll_top: usize, style: Style) -> usize {
    let visible = text.visible(scroll_top, area.height as usize);
    for (row, line) in visible.iter().enumerate() {
        render_line(buf, area.x, area.y + row as u16, line, area.width, style);
    }
    visible.len()
}

// ── Bidi caret motion (§3.2) ───────────────────────────────────────────────

/// The logical caret boundaries of `text` and the **visual column** each occupies.
///
/// Returns `(boundaries, cols)` where `boundaries[l]` is the byte offset of the
/// `l`-th grapheme boundary (`boundaries[n] == text.len()`) and `cols[l]` is the
/// visual column of a caret sitting at that boundary. A caret before an LTR
/// grapheme homes to its left edge, before an RTL grapheme to its right edge; the
/// end boundary homes to the trailing edge of the last logical grapheme. This is
/// the shared basis for [`visual_step`], [`caret_visual_col`] and
/// [`caret_from_visual_col`].
fn caret_cols(text: &str, base: BaseDirection) -> (Vec<usize>, Vec<u16>) {
    let mut boundaries: Vec<usize> = text.grapheme_indices(true).map(|(b, _)| b).collect();
    let n = boundaries.len();
    boundaries.push(text.len());
    if n == 0 {
        return (boundaries, vec![0]);
    }

    let line = shape_line(text, 0, base);
    let mut colstart = Vec::with_capacity(line.cells.len());
    let mut acc = 0u16;
    for cell in &line.cells {
        colstart.push(acc);
        acc += cell.width as u16;
    }
    let colend = |v: usize| colstart.get(v).copied().unwrap_or(0) + line.cells.get(v).map(|c| c.width as u16).unwrap_or(0);

    let info = BidiInfo::new(text, base.to_level());
    let is_rtl = |byte: usize| info.levels.get(byte).map(|lv| lv.is_rtl()).unwrap_or(false);

    let mut cols = Vec::with_capacity(n + 1);
    for l in 0..n {
        let v = line.map.logical_to_visual(l).unwrap_or(l);
        cols.push(if is_rtl(boundaries[l]) { colend(v) } else { colstart.get(v).copied().unwrap_or(0) });
    }
    // End boundary: trailing edge of the last logical grapheme.
    let vlast = line.map.logical_to_visual(n - 1).unwrap_or(n - 1);
    cols.push(if is_rtl(boundaries[n - 1]) { colstart.get(vlast).copied().unwrap_or(0) } else { colend(vlast) });

    (boundaries, cols)
}

/// The largest grapheme boundary at or before `cursor` (defensive flooring).
fn floor_boundary(boundaries: &[usize], cursor: usize) -> usize {
    boundaries.iter().rev().find(|&&b| b <= cursor).copied().unwrap_or(0)
}

/// Move a logical byte-`cursor` one grapheme in **visual** space across a
/// bidi-shaped line: `Left`/`Right` are physical arrow directions, but the cursor
/// stays a logical byte index (grapheme-boundary aligned). Returns the new byte
/// cursor, or `None` at the visual edge — the same "fell off the end" signal
/// [`line_edit`](crate::edit::line_edit) gives, so a form can move focus.
///
/// The caret follows the visual column order from `caret_cols`: in a pure-LTR
/// line this matches logical motion, in a pure-RTL line it is mirrored, and in
/// mixed text it steps to the visually adjacent boundary. `Up`/`Down` are not
/// handled here (return `None`).
pub fn visual_step(text: &str, cursor: usize, dir: crate::tree::Direction, ctx: TextCtx) -> Option<usize> {
    use crate::tree::Direction;
    if text.is_empty() {
        return None;
    }
    let (boundaries, cols) = caret_cols(text, ctx.base);
    let cur = floor_boundary(&boundaries, cursor.min(text.len()));
    let l = boundaries.iter().position(|&b| b == cur).unwrap_or(0);
    let cur_col = cols[l];

    let pick = |cmp: &dyn Fn(u16, u16) -> bool, better: &dyn Fn(u16, u16) -> bool| -> Option<usize> {
        let mut best: Option<(u16, usize)> = None;
        for (i, &c) in cols.iter().enumerate() {
            if cmp(c, cur_col) && best.map_or(true, |(bc, _)| better(c, bc)) {
                best = Some((c, i));
            }
        }
        best.map(|(_, i)| boundaries[i])
    };
    match dir {
        // Move to the nearest boundary in the requested visual direction.
        Direction::Right => pick(&|c, cur| c > cur, &|c, bc| c < bc),
        Direction::Left  => pick(&|c, cur| c < cur, &|c, bc| c > bc),
        Direction::Up | Direction::Down => None,
    }
}

/// The visual column of the caret currently at byte `cursor` (0 for empty text).
pub fn caret_visual_col(text: &str, cursor: usize, ctx: TextCtx) -> u16 {
    if text.is_empty() {
        return 0;
    }
    let (boundaries, cols) = caret_cols(text, ctx.base);
    let cur = floor_boundary(&boundaries, cursor.min(text.len()));
    let l = boundaries.iter().position(|&b| b == cur).unwrap_or(0);
    cols[l]
}

/// The byte cursor whose caret sits nearest visual column `col` (for mouse-click /
/// visual Home/End placement). Ties resolve to the lower logical boundary.
pub fn caret_from_visual_col(text: &str, col: u16, ctx: TextCtx) -> usize {
    if text.is_empty() {
        return 0;
    }
    let (boundaries, cols) = caret_cols(text, ctx.base);
    let mut best = (u16::MAX, 0usize);
    for (i, &c) in cols.iter().enumerate() {
        let d = c.abs_diff(col);
        if d < best.0 {
            best = (d, i);
        }
    }
    boundaries[best.1]
}

// ── Selection (round-2 B6) ──────────────────────────────────────────────────

/// Extend a selection one grapheme in **visual** space — the caret end of a
/// selection moves exactly like a cursor, so this mirrors [`visual_step`]. The app
/// owns the anchor and the caret; `caret` is the moving end.
pub fn selection_step(text: &str, caret: usize, dir: crate::tree::Direction, ctx: TextCtx) -> Option<usize> {
    visual_step(text, caret, dir, ctx)
}

/// Render one line, highlighting the cells whose `source_byte` falls in the logical
/// selection range `sel` with `sel_style` (the rest use `style`).
///
/// Because selection membership is tested per source byte, a contiguous *logical*
/// range that crosses a direction boundary highlights the correct — possibly
/// **discontiguous** — visual span, which is the right bidi behavior. Clipped to
/// `rect.width`; a wide cluster crossing the right edge is dropped.
pub fn render_line_selected(
    buf:       &mut Buffer,
    rect:      Rect,
    text:      &str,
    sel:       Range<usize>,
    style:     Style,
    sel_style: Style,
    ctx:       TextCtx,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let line = shape_line(text, 0, ctx.base);
    let limit = rect.x.saturating_add(rect.width);
    let mut x = rect.x;
    for cell in &line.cells {
        let w = cell.width as u16;
        if x.saturating_add(w) > limit {
            break;
        }
        let selected = cell.source_byte >= sel.start && cell.source_byte < sel.end;
        let st = if selected { sel_style } else { style };
        buf.set_grapheme(x, rect.y, &cell.symbol, st);
        x = x.saturating_add(w);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TestBackend;
    use crate::Terminal;

    /// Reconstruct a line's visual text by concatenating its cells.
    fn visual_string(line: &VisualLine) -> String {
        line.cells.iter().map(|c| c.symbol.as_str()).collect()
    }

    // ── Wrapping ──────────────────────────────────────────────────────────

    #[test]
    fn wraps_on_spaces() {
        let w = wrap("hello world", 5, BaseDirection::Ltr);
        assert_eq!(w.line_count(), 2);
        assert_eq!(visual_string(&w.lines()[0]), "hello");
        // The break keeps the trailing space on the first line per UAX #14, so
        // the second line is "world".
        assert_eq!(visual_string(&w.lines()[1]), "world");
    }

    #[test]
    fn hard_newline_forces_break() {
        let w = wrap("ab\ncd", 80, BaseDirection::Ltr);
        assert_eq!(w.line_count(), 2);
        assert_eq!(visual_string(&w.lines()[0]), "ab");
        assert_eq!(visual_string(&w.lines()[1]), "cd");
    }

    #[test]
    fn shape_digits_substitutes_and_borrows_when_idle() {
        use std::borrow::Cow;
        // No shaping, or no digits present → borrow, no allocation.
        assert!(matches!(shape_digits("42", DigitShaping::None), Cow::Borrowed(_)));
        assert!(matches!(shape_digits("abc", DigitShaping::ArabicIndic), Cow::Borrowed(_)));
        // Substitution is width-preserving (each digit → one width-1 codepoint).
        assert_eq!(shape_digits("42", DigitShaping::ArabicIndic), "٤٢");
        assert_eq!(shape_digits("7", DigitShaping::ExtendedArabicIndic), "۷");
        // Non-digits pass through untouched.
        assert_eq!(shape_digits("v1.0", DigitShaping::ArabicIndic), "v١.٠");
    }

    #[test]
    fn elide_is_grapheme_width_and_direction_aware() {
        // LTR: keep the start, ellipsis on the right.
        assert_eq!(visual_string(&elide("abcdef", 4, TextCtx::LTR)), "abc…");
        // Already fits → unchanged.
        assert_eq!(visual_string(&elide("abc", 5, TextCtx::LTR)), "abc");
        // A wide grapheme that won't fit the budget is dropped whole, not split.
        assert_eq!(visual_string(&elide("a世界b", 4, TextCtx::LTR)), "a世…");
        // RTL: ellipsis on the LEFT, reading-start (rightmost) kept.
        assert_eq!(visual_string(&elide("אבג", 2, TextCtx::rtl())), "…א");
    }

    #[test]
    fn render_line_selected_highlights_logical_range() {
        use crate::style::{Color, Style};
        let base = Style::default();
        let sel = Style::default().bg(Color::Cyan);
        let mut term = Terminal::new(TestBackend::new(5, 1)).unwrap();
        term.draw(|buf| render_line_selected(buf, Rect::new(0, 0, 5, 1), "abcde", 1..3, base, sel, TextCtx::LTR)).unwrap();
        let buf = term.backend().buffer();
        assert_eq!(buf.get(1, 0).style.bg, Color::Cyan); // 'b' selected
        assert_eq!(buf.get(2, 0).style.bg, Color::Cyan); // 'c' selected
        assert_ne!(buf.get(0, 0).style.bg, Color::Cyan); // 'a' not
        assert_ne!(buf.get(3, 0).style.bg, Color::Cyan); // 'd' not
        // selection_step is the visual caret step (mirrors visual_step).
        assert_eq!(
            selection_step("abc", 0, crate::tree::Direction::Right, TextCtx::LTR),
            visual_step("abc", 0, crate::tree::Direction::Right, TextCtx::LTR),
        );
    }

    #[test]
    fn visual_step_ltr_matches_logical_motion() {
        use crate::tree::Direction::{Left, Right};
        let ctx = TextCtx::LTR;
        assert_eq!(visual_step("abc", 0, Right, ctx), Some(1));
        assert_eq!(visual_step("abc", 1, Right, ctx), Some(2));
        assert_eq!(visual_step("abc", 3, Right, ctx), None); // right edge
        assert_eq!(visual_step("abc", 3, Left, ctx), Some(2));
        assert_eq!(visual_step("abc", 0, Left, ctx), None); // left edge
        // Wide grapheme: motion steps by whole cluster (byte 1 → 4 over 世).
        assert_eq!(visual_step("a世b", 1, Right, ctx), Some(4));
        assert_eq!(caret_visual_col("a世b", 4, ctx), 3); // a=1 + 世=2 columns
    }

    #[test]
    fn visual_step_rtl_is_mirrored() {
        use crate::tree::Direction;
        // Hebrew, pure RTL: Right moves toward the logical start (visually right).
        let heb = "אבג"; // 3 graphemes, 2 bytes each → boundaries 0,2,4,6
        let ctx = TextCtx::rtl();
        assert_eq!(visual_step(heb, 0, Direction::Right, ctx), None); // logical start = visual right edge
        assert_eq!(visual_step(heb, 6, Direction::Left, ctx), None);  // logical end = visual left edge
        assert_eq!(visual_step(heb, 6, Direction::Right, ctx), Some(4));
        assert_eq!(visual_step(heb, 4, Direction::Right, ctx), Some(2));
        assert_eq!(visual_step(heb, 0, Direction::Left, ctx), Some(2));
        // Caret columns descend with logical index under RTL.
        assert_eq!(caret_visual_col(heb, 0, ctx), 3);
        assert_eq!(caret_visual_col(heb, 6, ctx), 0);
        // Click nearest a visual column resolves to the right byte.
        assert_eq!(caret_from_visual_col(heb, 0, ctx), 6);
        assert_eq!(caret_from_visual_col(heb, 3, ctx), 0);
    }

    #[test]
    fn blank_line_preserved() {
        // A doubled newline yields an empty middle line.
        let w = wrap("a\n\nb", 80, BaseDirection::Ltr);
        assert_eq!(w.line_count(), 3);
        assert!(w.lines()[1].cells.is_empty());
    }

    #[test]
    fn wide_graphemes_count_as_two() {
        // Three width-2 CJK chars in width 4 → 2 per line.
        let w = wrap("世界你好", 4, BaseDirection::Ltr);
        assert_eq!(w.line_count(), 2);
        for line in w.lines() {
            assert!(line.width() <= 4);
        }
    }

    #[test]
    fn long_unbreakable_word_hard_breaks() {
        let w = wrap("abcdefgh", 3, BaseDirection::Ltr);
        // No break opportunities inside the word → hard-break every 3 columns.
        assert_eq!(w.line_count(), 3);
        assert_eq!(visual_string(&w.lines()[0]), "abc");
        assert_eq!(visual_string(&w.lines()[2]), "gh");
    }

    // ── BiDi reordering ───────────────────────────────────────────────────

    #[test]
    fn ltr_reorders_to_identity() {
        let w = wrap("the quick brown fox", 80, BaseDirection::Ltr);
        for line in w.lines() {
            for v in 0..line.map.len() {
                assert_eq!(line.map.visual_to_logical(v), Some(v), "LTR must be identity");
            }
        }
    }

    #[test]
    fn rtl_run_is_reversed_visually() {
        // "ab" + Arabic "عر" + "cd": the Arabic run reverses in visual order.
        let text = "ab\u{0639}\u{0631}cd";
        let line = shape_line(text, 80, BaseDirection::Ltr);
        // unicode-bidi's own reorder_line is the oracle: "ab" + "رع" + "cd".
        assert_eq!(visual_string(&line), "ab\u{0631}\u{0639}cd");
    }

    #[test]
    fn cursor_map_round_trips() {
        let text = "ab\u{0639}\u{0631}cd";
        let line = shape_line(text, 80, BaseDirection::Ltr);
        for l in 0..line.map.len() {
            let v = line.map.logical_to_visual(l).unwrap();
            assert_eq!(line.map.visual_to_logical(v), Some(l), "round-trip identity");
        }
    }

    // ── Pagination & scrolling (one model, two views) ─────────────────────

    #[test]
    fn pagination_chunks_lines() {
        let w = wrap("a\nb\nc\nd\ne", 80, BaseDirection::Ltr);
        assert_eq!(w.line_count(), 5);
        assert_eq!(w.page_count(2), 3); // 2 + 2 + 1
        assert_eq!(w.page(0, 2).len(), 2);
        assert_eq!(w.page(2, 2).len(), 1); // short final page
        assert_eq!(w.page(3, 2).len(), 0); // past the end
    }

    #[test]
    fn scrolling_windows_lines() {
        let w = wrap("a\nb\nc\nd\ne", 80, BaseDirection::Ltr);
        assert_eq!(w.visible(1, 2).len(), 2);
        assert_eq!(visual_string(&w.visible(1, 2)[0]), "b");
        assert_eq!(w.visible(4, 10).len(), 1); // clamped at the end
        assert_eq!(w.visible(99, 10).len(), 0); // past the end
    }

    // ── Rendering & the TestBackend lock ──────────────────────────────────

    #[test]
    fn wrap_into_slots_flows_across_varying_widths() {
        // Slots of width 5, 3, 8: words are kept whole and flow across the slots.
        let lines = wrap_into_slots("alpha beta gamma", &[5, 3, 8], BaseDirection::Ltr);
        assert_eq!(lines.len(), 3); // one line per slot
        assert_eq!(visual_string(&lines[0]), "alpha");
        // "beta" (4) does not fit the width-3 slot, so rather than hard-break it,
        // the slot is left empty and the whole word moves into the next slot.
        assert_eq!(visual_string(&lines[1]), "");
        assert_eq!(visual_string(&lines[2]), "beta"); // "gamma" overflows the slots
    }

    #[test]
    fn wrap_into_slots_hard_breaks_only_when_wider_than_every_slot() {
        // No slot can hold "abcdefgh" (8) whole — the widest is 3 — so it must
        // hard-break across the slots rather than vanish.
        let lines = wrap_into_slots("abcdefgh", &[3, 3, 3], BaseDirection::Ltr);
        assert_eq!(visual_string(&lines[0]), "abc");
        assert_eq!(visual_string(&lines[1]), "def");
        assert_eq!(visual_string(&lines[2]), "gh");
    }

    #[test]
    fn wrap_into_slots_too_narrow_slot_left_empty() {
        // A width-1 slot cannot hold a width-2 grapheme: it stays empty and the
        // grapheme flows into the next slot that fits it.
        let lines = wrap_into_slots("\u{4e16}\u{754c}", &[1, 4], BaseDirection::Ltr);
        assert_eq!(visual_string(&lines[0]), ""); // 世 does not fit width 1
        assert_eq!(visual_string(&lines[1]), "\u{4e16}\u{754c}");
    }

    #[test]
    fn wrap_into_slots_empty_text_yields_empty_slots() {
        let lines = wrap_into_slots("", &[4, 4], BaseDirection::Ltr);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.cells.is_empty()));
    }

    #[test]
    fn render_clips_to_width() {
        let line = shape_line("hello", 80, BaseDirection::Ltr);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        let drawn = render_line(&mut buf, 0, 0, &line, 3, Style::default());
        assert_eq!(drawn, 3);
        assert_eq!(buf.get(0, 0).symbol, "h");
        assert_eq!(buf.get(2, 0).symbol, "l");
        assert_eq!(buf.get(3, 0).symbol, " ", "clipped past column 3");
    }

    /// Lock the visual output of a short mixed-direction paragraph (§3 milestone).
    #[test]
    fn mixed_direction_snapshot() {
        // "hi " + Arabic "عر" — the Arabic reverses to "رع" in visual order.
        // Terminal sized to the exact 5-column content so render() adds no
        // trailing blanks.
        let text = "hi \u{0639}\u{0631}";
        let w = wrap(text, 5, BaseDirection::Ltr);
        let backend = TestBackend::new(5, 1);
        let mut term = Terminal::new(backend).unwrap();
        term
            .draw(|buf| {
                render_wrapped(buf, buf.area, &w, 0, Style::default());
            })
            .unwrap();
        crate::assert_backend_snapshot!(term, "hi \u{0631}\u{0639}");
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    /// Strings over a mixed LTR/RTL/wide alphabet, to exercise the bidi path.
    fn mixed_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just('a'), Just('b'), Just(' '), Just('\n'),
                Just('\u{4e16}'),          // 世 (width 2)
                Just('\u{0639}'),          // ع (RTL)
                Just('\u{0631}'),          // ر (RTL)
            ],
            0..40,
        )
        .prop_map(|cs| cs.into_iter().collect())
    }

    /// Pure-LTR strings (ASCII letters + spaces), for the identity property.
    fn ltr_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(prop_oneof![Just('a'), Just('b'), Just(' ')], 0..40)
            .prop_map(|cs| cs.into_iter().collect())
    }

    proptest! {
        /// The cursor map is a bijection on `0..n` per line and round-trips.
        #[test]
        fn prop_cursor_map_bijection(text in mixed_text(), width in 2u16..20) {
            let w = wrap(&text, width, BaseDirection::Auto);
            for line in w.lines() {
                let n = line.map.len();
                prop_assert_eq!(line.cells.len(), n);
                // v2l is a permutation of 0..n.
                let mut seen = vec![false; n];
                for v in 0..n {
                    let l = line.map.visual_to_logical(v).unwrap();
                    prop_assert!(l < n);
                    prop_assert!(!seen[l], "v2l not injective");
                    seen[l] = true;
                    // Inverse round-trips to identity.
                    prop_assert_eq!(line.map.logical_to_visual(l), Some(v));
                }
            }
        }

        /// No wrapped line's emitted width exceeds the target (width ≥ 2, so the
        /// degenerate oversized-grapheme case never arises).
        #[test]
        fn prop_no_line_exceeds_width(text in mixed_text(), width in 2u16..20) {
            let w = wrap(&text, width, BaseDirection::Auto);
            for line in w.lines() {
                prop_assert!(line.width() <= width,
                    "line width {} > target {}", line.width(), width);
            }
        }

        /// Pure-LTR input reorders to the identity on every line: the bidi path
        /// runs but is provably inert.
        #[test]
        fn prop_ltr_is_identity(text in ltr_text(), width in 2u16..20) {
            let w = wrap(&text, width, BaseDirection::Ltr);
            for line in w.lines() {
                for v in 0..line.map.len() {
                    prop_assert_eq!(line.map.visual_to_logical(v), Some(v));
                }
            }
        }
    }
}
