// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Wrapped-line virtualization: scroll and seek through one enormous flowed
//! document without re-wrapping all of it (design note §4.2).
//!
//! ## Why this is harder than row virtualization
//!
//! A record list has a fixed row count; a flowed document does not. The number of
//! *wrapped* lines depends on the width, so you cannot jump to "wrapped line
//! 750,000" without knowing where it falls. The solution is a lazy **byte-offset
//! → wrapped-line index**: a growing `Vec` of the byte offset at which each
//! wrapped line begins, built incrementally as the user scrolls or seeks, cached,
//! and invalidated when the width changes.
//!
//! This is kept entirely separate from row virtualization ([`crate::vlist`]); the
//! two share only the idea of a viewport and nothing else.
//!
//! ## How the index stays lazy *and* correct
//!
//! The index is extended one **chunk** at a time. A chunk always ends just after
//! a `\n`, so every chunk is a whole number of paragraphs. Because a hard newline
//! resets wrapping completely (break opportunities, greedy fill, and the bidi
//! paragraph all restart), wrapping a paragraph-aligned chunk is *identical* to
//! the corresponding slice of wrapping the whole document — so the incremental
//! index agrees exactly with a brute-force full wrap. Chunks are batched to a few
//! KB to amortize the per-`wrap` overhead. (A single paragraph with no newlines
//! is the one case that wraps in full when first reached.)
//!
//! ## Rendering the viewport
//!
//! [`DocView::visible_lines`] re-wraps only the **visible byte range** with the
//! Phase 2 engine ([`crate::text::wrap`]). Greedy wrapping is forward-only, so
//! re-wrapping `[start_of_top_line, start_of_line_below_window)` reproduces
//! exactly the indexed lines. (A soft-wrapped continuation line therefore has its
//! bidi base resolved per visible window; for the LTR-default viewer this is
//! immaterial.)

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::style::Style;
use crate::text::{render_line, wrap, BaseDirection, VisualLine};

/// Target chunk size, in bytes, for one index-extension step. The actual chunk
/// runs from the cursor to the first `\n` at or after `cursor + CHUNK` (or to the
/// end of the document), so it is always paragraph-aligned and at least this big
/// unless a paragraph or the document ends sooner.
const CHUNK: usize = 4096;

// ── DocView ──────────────────────────────────────────────────────────────────

/// A scrollable, seekable viewer over one large flowed document.
///
/// Owns the document text and a lazy byte-offset → wrapped-line index. Construct
/// with [`new`](DocView::new), move with [`scroll_by`](DocView::scroll_by) /
/// [`seek_to_byte`](DocView::seek_to_byte), and pull the on-screen lines with
/// [`visible_lines`](DocView::visible_lines). Changing the width with
/// [`set_width`](DocView::set_width) invalidates the index but keeps the viewport
/// anchored to the same byte position.
pub struct DocView {
    /// The whole document text. Held in memory; only its *wrapping* is virtualized.
    text: String,
    /// Columns the document is wrapped to (always ≥ 1).
    width: u16,
    /// Base direction passed to the text engine for every wrap.
    base: BaseDirection,
    /// Byte offset where each wrapped line begins. `line_starts[0]` is `0` once
    /// any indexing has happened; strictly increasing.
    line_starts: Vec<usize>,
    /// Bytes `0..indexed_byte` are fully indexed; the next chunk starts here.
    indexed_byte: usize,
    /// `true` once the whole document has been indexed.
    complete: bool,
    /// Scroll position: index of the top visible wrapped line.
    top: usize,
}

impl DocView {
    /// Create a viewer over `text`, wrapped to `width`, with an LTR base.
    pub fn new(text: impl Into<String>, width: u16) -> Self {
        Self::with_base(text, width, BaseDirection::Ltr)
    }

    /// Create a viewer with an explicit base direction.
    pub fn with_base(text: impl Into<String>, width: u16, base: BaseDirection) -> Self {
        Self {
            text: text.into(),
            width: width.max(1),
            base,
            line_starts: Vec::new(),
            indexed_byte: 0,
            complete: false,
            top: 0,
        }
    }

    /// The current wrap width.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// The index of the top visible wrapped line.
    pub fn top(&self) -> usize {
        self.top
    }

    /// How many lines have been indexed so far, and whether indexing is complete.
    ///
    /// The first element is a lower bound on the true line count until the second
    /// is `true`; useful for an estimated scrollbar without forcing a full index.
    pub fn line_count_hint(&self) -> (usize, bool) {
        (self.line_starts.len(), self.complete)
    }

    /// Change the wrap width, invalidating the index but keeping the viewport
    /// anchored to the same byte position.
    ///
    /// The byte offset of the current top line is captured *before* the index is
    /// cleared, then the new top line is recomputed for the new width — so the
    /// content at the top of the screen stays put across a resize instead of
    /// jumping. A no-op when the width is unchanged.
    pub fn set_width(&mut self, width: u16) {
        let width = width.max(1);
        if width == self.width {
            return;
        }
        // Anchor on the byte at the top of the viewport under the *old* wrapping.
        let anchor = self.line_to_byte(self.top).unwrap_or(0);
        self.width = width;
        self.invalidate();
        // Recompute which wrapped line that byte now falls on.
        self.top = self.byte_to_line(anchor);
    }

    /// Drop the cached index (after a width change). The text and scroll byte
    /// anchor are handled by the caller.
    fn invalidate(&mut self) {
        self.line_starts.clear();
        self.indexed_byte = 0;
        self.complete = false;
    }

    /// Scroll to an absolute wrapped-line index (clamped at 0; the bottom is
    /// clamped lazily when rendering).
    pub fn scroll_to_line(&mut self, line: usize) {
        self.top = line;
    }

    /// Scroll by `delta` lines: positive scrolls down, negative up. Clamped at
    /// the top; the bottom is clamped in [`visible_lines`](DocView::visible_lines).
    pub fn scroll_by(&mut self, delta: isize) {
        self.top = (self.top as isize + delta).max(0) as usize;
    }

    /// Move the viewport so the wrapped line containing `byte` is at the top.
    pub fn seek_to_byte(&mut self, byte: usize) {
        self.top = self.byte_to_line(byte);
    }

    /// The byte offset where wrapped line `line` begins, or `None` if `line` is
    /// past the end of the document. Extends the index as needed.
    pub fn line_to_byte(&mut self, line: usize) -> Option<usize> {
        self.ensure_lines(line);
        self.line_starts.get(line).copied()
    }

    /// The wrapped-line index containing byte offset `byte`. Extends the index as
    /// needed (seeking forward only as far as `byte`).
    ///
    /// Returns the line whose start is the greatest `≤ byte`. For an empty
    /// document this is `0`.
    pub fn byte_to_line(&mut self, byte: usize) -> usize {
        self.ensure_byte(byte);
        // Number of line starts at or before `byte`, minus one, is that line's
        // index. `line_starts[0]` is 0 ≤ byte, so the count is ≥ 1 for a
        // non-empty index.
        self.line_starts.partition_point(|&s| s <= byte).saturating_sub(1)
    }

    /// The total number of wrapped lines, forcing a full index. O(document) the
    /// first time at a given width; cached thereafter.
    pub fn total_lines(&mut self) -> usize {
        while !self.complete {
            self.extend_chunk();
        }
        self.line_starts.len()
    }

    /// The wrapped lines visible in a viewport of `height` rows, starting at the
    /// current scroll position, in visual order with byte offsets in **document**
    /// coordinates.
    ///
    /// Only the visible byte range is re-wrapped (via [`crate::text::wrap`]); the
    /// index supplies its bounds. The top is clamped to the last full screen once
    /// the document is fully indexed, so the final line never scrolls off the
    /// bottom.
    pub fn visible_lines(&mut self, height: usize) -> Vec<VisualLine> {
        if height == 0 {
            return Vec::new();
        }
        // Make sure the index reaches one past the bottom of the viewport.
        self.ensure_lines(self.top + height);

        // Clamp the bottom only once the true line count is known.
        if self.complete {
            let total = self.line_starts.len();
            if total == 0 {
                return Vec::new();
            }
            if self.top + height > total {
                self.top = total.saturating_sub(height);
            }
        }

        let Some(&start) = self.line_starts.get(self.top) else {
            return Vec::new(); // scrolled past the end of a complete document
        };
        // The window ends at the start of the line just below it, or at EOF.
        let end = self
            .line_starts
            .get(self.top + height)
            .copied()
            .unwrap_or(self.text.len());

        // Re-wrap just the visible bytes and shift sources into document space.
        let wrapped = wrap(&self.text[start..end], self.width, self.base);
        wrapped
            .lines()
            .iter()
            .take(height)
            .map(|line| shift_line(line, start))
            .collect()
    }

    /// Extend the index until it holds at least `line + 1` entries, or the whole
    /// document is indexed.
    fn ensure_lines(&mut self, line: usize) {
        while self.line_starts.len() <= line && !self.complete {
            self.extend_chunk();
        }
    }

    /// Extend the index until byte offset `byte` is covered, or the document is
    /// fully indexed.
    fn ensure_byte(&mut self, byte: usize) {
        while self.indexed_byte <= byte && !self.complete {
            self.extend_chunk();
        }
    }

    /// Index the next chunk: wrap a paragraph-aligned slice starting at
    /// `indexed_byte`, append each wrapped line's start byte, and advance.
    ///
    /// The slice ends just after the first `\n` at or beyond `indexed_byte +
    /// CHUNK` (or at EOF), so it is a whole number of paragraphs and wraps
    /// identically to the same span of a full-document wrap.
    fn extend_chunk(&mut self) {
        if self.complete {
            return;
        }
        let from = self.indexed_byte;
        if from >= self.text.len() {
            self.complete = true;
            return;
        }
        let cut = chunk_end(&self.text, from);
        let wrapped = wrap(&self.text[from..cut], self.width, self.base);
        for line in wrapped.lines() {
            self.line_starts.push(from + line.source.start);
        }
        self.indexed_byte = cut;
        if cut >= self.text.len() {
            self.complete = true;
        }
    }
}

/// The end byte of the chunk that starts at `from`: just past the first `\n` at
/// or after `from + CHUNK`, or `text.len()` if there is none.
///
/// Searching by byte (a `\n` is always a single byte and never part of a
/// multi-byte grapheme) keeps the returned offset on a valid `char` boundary.
fn chunk_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let target = (from + CHUNK).min(bytes.len());
    match bytes[target..].iter().position(|&b| b == b'\n') {
        Some(rel) => target + rel + 1, // include the newline → paragraph boundary
        None => bytes.len(),
    }
}

/// Clone a visual line, shifting its source range and every cell's source byte by
/// `offset` so they refer to document coordinates instead of the re-wrapped slice.
fn shift_line(line: &VisualLine, offset: usize) -> VisualLine {
    let mut out = line.clone();
    out.source = (out.source.start + offset)..(out.source.end + offset);
    for cell in &mut out.cells {
        cell.source_byte += offset;
    }
    out
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render the visible window of `view` into `area`, one wrapped line per row.
///
/// Lines are clipped to `area.width`; rows past the end of the document are left
/// blank. Returns the number of lines drawn. Viewport-bounded: only the visible
/// window is wrapped and touched.
pub fn render_doc(buf: &mut Buffer, area: Rect, view: &mut DocView, style: Style) -> usize {
    let lines = view.visible_lines(area.height as usize);
    for (row, line) in lines.iter().enumerate() {
        render_line(buf, area.x, area.y + row as u16, line, area.width, style);
    }
    lines.len()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force reference: the byte offset of every wrapped line, from a single
    /// whole-document wrap.
    fn brute_starts(text: &str, width: u16) -> Vec<usize> {
        wrap(text, width, BaseDirection::Ltr)
            .lines()
            .iter()
            .map(|l| l.source.start)
            .collect()
    }

    fn visual_string(line: &VisualLine) -> String {
        line.cells.iter().map(|c| c.symbol.as_str()).collect()
    }

    #[test]
    fn index_matches_brute_force_simple() {
        let text = "the quick brown fox\njumps over\nthe lazy dog";
        let mut view = DocView::new(text, 10);
        let total = view.total_lines();
        let want = brute_starts(text, 10);
        assert_eq!(total, want.len());
        let got: Vec<usize> = (0..total).map(|i| view.line_to_byte(i).unwrap()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn visible_window_wraps_correctly() {
        let text = "alpha beta gamma delta epsilon zeta eta theta";
        let mut view = DocView::new(text, 12);
        let lines = view.visible_lines(3);
        // First visible line is the greedy fill of the first 12 columns.
        assert_eq!(visual_string(&lines[0]), "alpha beta");
        // Source bytes are in document coordinates.
        assert_eq!(lines[0].source.start, 0);
        assert_eq!(lines[1].source.start, 11); // after "alpha beta "
    }

    #[test]
    fn scroll_and_clamp_at_bottom() {
        let text = "a\nb\nc\nd\ne"; // 5 short lines
        let mut view = DocView::new(text, 80);
        view.scroll_by(1000); // far past the end
        let lines = view.visible_lines(3);
        // Clamped so the last 3 lines are shown.
        assert_eq!(view.top(), 2);
        assert_eq!(visual_string(&lines[0]), "c");
        assert_eq!(visual_string(&lines[2]), "e");
    }

    #[test]
    fn seek_lands_on_line_containing_byte() {
        let text = "0123456789\nabcdefghij\nABCDEFGHIJ"; // three 10-char lines
        let mut view = DocView::new(text, 80);
        // Byte 15 is inside the second line ("abcdefghij", bytes 11..21).
        assert_eq!(view.byte_to_line(15), 1);
        view.seek_to_byte(25); // inside the third line
        assert_eq!(view.top(), 2);
    }

    #[test]
    fn width_change_invalidates_and_reanchors() {
        let text = "one two three four five six seven eight nine ten";
        let mut view = DocView::new(text, 20);
        let total_wide = view.total_lines();
        view.set_width(8);
        // The index is rebuilt for the new width; the line count grows.
        let total_narrow = view.total_lines();
        assert!(total_narrow > total_wide);
        // And it matches a brute-force wrap at the new width.
        assert_eq!(total_narrow, brute_starts(text, 8).len());
    }

    #[test]
    fn empty_document_is_inert() {
        let mut view = DocView::new("", 40);
        assert_eq!(view.total_lines(), 0);
        assert!(view.visible_lines(5).is_empty());
        view.scroll_by(10);
        assert!(view.visible_lines(5).is_empty());
    }

    #[test]
    fn batching_spans_many_paragraphs() {
        // Many short lines force several chunk boundaries (CHUNK is 4096 bytes,
        // but the math is exercised by building the full index incrementally).
        let text: String = (0..500).map(|i| format!("line number {i}\n")).collect();
        let mut view = DocView::new(&text, 16);
        let total = view.total_lines();
        assert_eq!(total, brute_starts(&text, 16).len());
        // A mid-document line resolves to the same byte as the brute force.
        assert_eq!(view.line_to_byte(250), Some(brute_starts(&text, 16)[250]));
    }

    // ── Property tests ────────────────────────────────────────────────────

    use proptest::prelude::*;

    /// Documents over a small alphabet with spaces and newlines, to exercise
    /// paragraph boundaries, blank lines, and soft wraps.
    fn doc_text() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![Just('a'), Just('b'), Just('c'), Just(' '), Just('\n')],
            0..80,
        )
        .prop_map(|cs| cs.into_iter().collect())
    }

    proptest! {
        /// The lazy index agrees with a brute-force full wrap, line for line.
        #[test]
        fn prop_index_matches_brute_force(text in doc_text(), width in 1u16..16) {
            let mut view = DocView::new(text.clone(), width);
            let total = view.total_lines();
            let want = brute_starts(&text, width);
            prop_assert_eq!(total, want.len());
            for (i, &w) in want.iter().enumerate() {
                prop_assert_eq!(view.line_to_byte(i), Some(w));
            }
        }

        /// Changing the width invalidates the cache: the rebuilt index matches a
        /// brute-force wrap at the *new* width, never the stale old one.
        #[test]
        fn prop_width_change_invalidates(text in doc_text(), w1 in 1u16..16, w2 in 1u16..16) {
            let mut view = DocView::new(text.clone(), w1);
            let _ = view.total_lines();       // build at w1
            view.set_width(w2);
            let got: Vec<usize> = (0..view.total_lines())
                .map(|i| view.line_to_byte(i).unwrap())
                .collect();
            prop_assert_eq!(got, brute_starts(&text, w2));
        }

        /// Seeking to a byte lands on the line whose source range contains it.
        #[test]
        fn prop_seek_lands_correctly(text in doc_text(), width in 1u16..16, byte in 0usize..80) {
            prop_assume!(byte <= text.len());
            let starts = brute_starts(&text, width);
            prop_assume!(!starts.is_empty());
            let mut view = DocView::new(text, width);
            let line = view.byte_to_line(byte);
            // The chosen line starts at or before `byte`...
            prop_assert!(starts[line] <= byte);
            // ...and it is the last such line (the next one starts after `byte`).
            if line + 1 < starts.len() {
                prop_assert!(starts[line + 1] > byte);
            }
        }
    }
}
