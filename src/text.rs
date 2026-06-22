// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! The text engine core: bidi-aware paragraph wrapping with a logical↔visual
//! cursor map, plus pagination and scrolling as views over one model
//! (design note §3, excluding runaround which is Phase 5).
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
/// The walk keeps a running line width and the most recent allowed break within
/// the line. When a *non-space* grapheme would overflow `width`, it wraps at that
/// remembered break (or hard-breaks at the current grapheme if the line has no
/// break opportunity yet). Breakable spaces never trigger a wrap — they "hang"
/// past the edge (UAX #14 puts the break opportunity *after* trailing spaces) and
/// are stripped by [`trim_trailing_spaces`] when each line is pushed, so trailing
/// whitespace is consumed by the wrap rather than counted. Mandatory breaks (hard
/// newlines) always start a new line; zero-width graphemes never overflow.
///
/// # Degenerate case
/// A single grapheme wider than `width` (only possible when `width < 2`) is
/// emitted alone on its line; this is the one situation where a line may exceed
/// `width`, and it keeps the walk making progress instead of stalling.
fn fill_lines(graphemes: &[Grapheme], flags: &BreakFlags, width: u16) -> Vec<Range<usize>> {
    let n = graphemes.len();
    let target = width as u32;
    let mut lines: Vec<Range<usize>> = Vec::new();

    let mut start = 0usize; // grapheme index where the current line begins
    let mut i = 0usize; // grapheme under consideration
    let mut acc = 0u32; // accumulated width of [start, i), trailing spaces included
    let mut last_break: Option<usize> = None; // best break boundary seen in-line

    while i < n {
        // A hard newline before grapheme `i` ends the current line here.
        if i > start && flags.mandatory[i] {
            lines.push(trim_trailing_spaces(graphemes, start..i));
            start = i;
            acc = 0;
            last_break = None;
        }

        let g = &graphemes[i];
        let w = g.width as u32;
        // Spaces hang past the edge instead of forcing a wrap; only a visible
        // grapheme that overflows causes a line break.
        let overflow = w > 0 && acc + w > target && !g.is_space;

        if overflow {
            if i > start {
                // Wrap at the remembered break, or hard-break at `i` if none.
                let brk = match last_break {
                    Some(b) if b > start => b,
                    _ => i,
                };
                lines.push(trim_trailing_spaces(graphemes, start..brk));
                start = brk;
                acc = 0;
                last_break = None;
                i = brk; // re-scan the carried graphemes onto the new line
                continue;
            } else {
                // Oversized lone grapheme (width < 2 regime): emit it by itself.
                lines.push(start..i + 1);
                start = i + 1;
                acc = 0;
                last_break = None;
                i += 1;
                continue;
            }
        }

        acc += w;
        i += 1;
        // Record an allowed break at the boundary we just crossed.
        if i <= n && flags.allowed[i] {
            last_break = Some(i);
        }
    }

    if start < n {
        lines.push(trim_trailing_spaces(graphemes, start..n));
    }
    lines
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
fn byte_range(text: &str, graphemes: &[Grapheme], gr: Range<usize>) -> Range<usize> {
    let start = graphemes[gr.start].byte;
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

/// Shape `text` as a single bidi-correct visual line — the primitive for flowed
/// chrome-adjacent content such as a table cell or a flowing label (§3.3/§6.3).
///
/// No wrapping is performed: the whole string becomes one [`VisualLine`] in
/// visual order. `width` is advisory — a renderer clips the returned cells to it
/// (see [`render_line`]); this keeps cell↔source mapping intact rather than
/// truncating the model. `base` selects the base direction as in [`wrap`].
pub fn shape_line(text: &str, _width: u16, base: BaseDirection) -> VisualLine {
    if text.is_empty() {
        return VisualLine {
            cells: Vec::new(),
            map: CursorMap::from_v2l(Vec::new()),
            source: 0..0,
        };
    }
    let info = BidiInfo::new(text, base.to_level());
    build_line(text, 0..text.len(), &info)
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
