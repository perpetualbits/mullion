// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Stateless single-line **text-field primitives**: a pure key→edit transform and a
//! horizontally-scrolling render pass.
//!
//! These are *primitives*, not a widget. The app owns the `String`, the cursor byte
//! index, the horizontal scroll offset, the focus, and the form — mullion contributes
//! only grapheme/width correctness and a render pass, exactly the category
//! [`render_line`](crate::text::render_line) and [`shape_line`](crate::text::shape_line)
//! already occupy. There is no retained state, no event loop, and no focus ring, so
//! the roadmap's "no text-input widget" non-goal is respected: a form is still a plain
//! struct of `String`s the app drives.
//!
//! ```text
//! key  ─▶ line_edit(&mut text, &mut cursor, key)   // mutate caller-owned state
//! draw ─▶ render_field(buf, rect, &text, cursor, &mut scroll, &opts)
//! ```
//!
//! Both are grapheme-cluster correct: the cursor is a **byte index** that only ever
//! lands on a grapheme boundary, and [`render_field`] measures display width with
//! `unicode-width` so wide CJK/emoji and combining clusters behave.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::buffer::{Buffer, Cell};
use crate::geometry::{visible_window, Rect};
use crate::input::KeyCode;
use crate::style::Style;

// ── line_edit ───────────────────────────────────────────────────────────────────

/// Apply one editing key to caller-owned `text` and `cursor`, returning `true` when
/// the text or cursor actually changed.
///
/// `cursor` is a **byte index** into `text` and is kept on a grapheme-cluster
/// boundary. A possibly-stale cursor is clamped into range first, so a caller that
/// shrinks `text` out from under the cursor stays safe.
///
/// Handled keys (everything else returns `false`, leaving state untouched so the
/// caller can route the key elsewhere — e.g. Tab to the next field, or arrow-at-edge
/// to move focus):
///
/// | Key | Effect |
/// |---|---|
/// | [`Char`](KeyCode::Char) | insert the character at the cursor |
/// | [`Backspace`](KeyCode::Backspace) | delete the grapheme before the cursor |
/// | [`Delete`](KeyCode::Delete) | delete the grapheme at the cursor |
/// | [`Left`](KeyCode::Left) / [`Right`](KeyCode::Right) | move one grapheme |
/// | [`Home`](KeyCode::Home) / [`End`](KeyCode::End) | jump to start / end |
///
/// A motion or deletion at the corresponding edge (e.g. `Left` at the start) is a
/// no-op and returns `false` — the signal a form uses to let the key fall through.
pub fn line_edit(text: &mut String, cursor: &mut usize, key: KeyCode) -> bool {
    // Defensive: clamp a stale cursor onto a valid boundary before editing.
    let c = (*cursor).min(text.len());
    let c = floor_boundary(text, c);
    *cursor = c;

    match key {
        KeyCode::Char(ch) => {
            text.insert(c, ch);
            *cursor = c + ch.len_utf8();
            true
        }
        KeyCode::Backspace => {
            if c == 0 {
                return false;
            }
            let prev = prev_boundary(text, c);
            text.replace_range(prev..c, "");
            *cursor = prev;
            true
        }
        KeyCode::Delete => {
            if c >= text.len() {
                return false;
            }
            let next = next_boundary(text, c);
            text.replace_range(c..next, "");
            *cursor = c;
            true
        }
        KeyCode::Left => {
            if c == 0 {
                return false;
            }
            *cursor = prev_boundary(text, c);
            true
        }
        KeyCode::Right => {
            if c >= text.len() {
                return false;
            }
            *cursor = next_boundary(text, c);
            true
        }
        KeyCode::Home => {
            if c == 0 {
                return false;
            }
            *cursor = 0;
            true
        }
        KeyCode::End => {
            if c == text.len() {
                return false;
            }
            *cursor = text.len();
            true
        }
        _ => false,
    }
}

/// The grapheme boundary at or before byte index `byte` (`byte` itself if it already
/// sits on one).
fn floor_boundary(text: &str, byte: usize) -> usize {
    if byte == 0 || byte >= text.len() || text.is_char_boundary(byte) && on_grapheme_boundary(text, byte) {
        return byte.min(text.len());
    }
    text[..byte].grapheme_indices(true).next_back().map(|(i, _)| i).unwrap_or(0)
}

/// `true` when `byte` is the start of a grapheme cluster (or the end of the string).
fn on_grapheme_boundary(text: &str, byte: usize) -> bool {
    byte == text.len() || text.grapheme_indices(true).any(|(i, _)| i == byte)
}

/// Byte index of the grapheme boundary immediately before `byte` (`0` if none).
fn prev_boundary(text: &str, byte: usize) -> usize {
    text[..byte].grapheme_indices(true).next_back().map(|(i, _)| i).unwrap_or(0)
}

/// Byte index of the grapheme boundary immediately after `byte` (`text.len()` if none).
fn next_boundary(text: &str, byte: usize) -> usize {
    text[byte..]
        .grapheme_indices(true)
        .next()
        .map(|(_, g)| byte + g.len())
        .unwrap_or(text.len())
}

// ── render_field ────────────────────────────────────────────────────────────────

/// Styling for [`render_field`].
pub struct FieldRender {
    /// Style of the field's text (and its blank background).
    pub style: Style,
    /// Style of the single cell under the cursor.
    pub cursor_style: Style,
    /// `Some(ch)` masks every grapheme with `ch` (one column each) for password
    /// entry; `None` shows the text verbatim.
    pub mask: Option<char>,
}

/// Render one line of `text` into the first row of `rect`, scrolling horizontally so
/// the cursor stays visible and styling the cursor cell.
///
/// `cursor` is the byte index from [`line_edit`]; `scroll` is the **caller-owned**
/// horizontal offset in display columns, mutated in place via
/// [`visible_window`](crate::geometry::visible_window) so the app keeps all state. The
/// row is cleared to [`FieldRender::style`] first, so stale glyphs from a previous
/// frame never linger.
///
/// With [`mask`](FieldRender::mask) set, each grapheme renders as one masked column
/// (passwords). A wide grapheme that would straddle either clip edge is dropped rather
/// than shown as a half-glyph, matching [`render_line`](crate::text::render_line). The
/// cursor cell is drawn even when the cursor rests one past the last grapheme (the
/// empty-field and end-of-text cases).
pub fn render_field(
    buf: &mut Buffer,
    rect: Rect,
    text: &str,
    cursor: usize,
    scroll: &mut usize,
    opts: &FieldRender,
) {
    if rect.is_empty() {
        return;
    }
    let width = rect.width;
    let cursor = cursor.min(text.len());

    // Measure each grapheme's start column and width; locate the cursor column.
    let mut placed: Vec<(u16, String, u16)> = Vec::new();
    let mut col = 0u16;
    let mut cursor_col = 0u16;
    for (byte, g) in text.grapheme_indices(true) {
        let w = match opts.mask {
            Some(_) => 1,
            None => UnicodeWidthStr::width(g).min(2) as u16,
        };
        if byte < cursor {
            cursor_col += w;
        }
        if w == 0 {
            continue; // zero-width (control) cluster: occupies no column
        }
        let sym = match opts.mask {
            Some(m) => m.to_string(),
            None => g.to_string(),
        };
        placed.push((col, sym, w));
        col += w;
    }
    let total_cols = col;

    // Slide the window so the cursor column is visible (len + 1 leaves room for the
    // cursor to rest one past the last column).
    let view = visible_window(cursor_col as usize, scroll, total_cols as usize + 1, width as usize);
    let start = view.start as u16;

    // Clear the field row to the base style so old content never bleeds through.
    buf.fill(Rect::new(rect.x, rect.y, width, 1), Cell::new(" ", opts.style));

    // Draw each visible grapheme, highlighting the one under the cursor.
    for (c, sym, w) in &placed {
        if *c < start {
            continue; // left of the window (a wide glyph cut by the left edge is dropped)
        }
        let vis = *c - start;
        if vis + *w > width {
            continue; // crosses the right clip edge
        }
        let cell_style = if *c == cursor_col { opts.cursor_style } else { opts.style };
        buf.set_grapheme(rect.x + vis, rect.y, sym, cell_style);
    }

    // Cursor resting one past the last grapheme (empty field / end of text): the loop
    // above never drew it, so paint a blank cursor cell.
    if cursor_col == total_cols && cursor_col >= start && cursor_col - start < width {
        buf.set_grapheme(rect.x + (cursor_col - start), rect.y, " ", opts.cursor_style);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TestBackend;
    use crate::style::Color;
    use crate::Terminal;

    fn edit(text: &str, cursor: usize, key: KeyCode) -> (String, usize, bool) {
        let mut t = text.to_string();
        let mut c = cursor;
        let changed = line_edit(&mut t, &mut c, key);
        (t, c, changed)
    }

    #[test]
    fn insert_advances_by_char_bytes() {
        assert_eq!(edit("ac", 1, KeyCode::Char('b')), ("abc".into(), 2, true));
        // A 2-byte char advances the cursor by 2 bytes.
        assert_eq!(edit("", 0, KeyCode::Char('ä')), ("ä".into(), 2, true));
    }

    #[test]
    fn backspace_and_delete_are_grapheme_aware() {
        // Backspace removes the whole 3-byte CJK grapheme before the cursor.
        assert_eq!(edit("a世b", 4, KeyCode::Backspace), ("ab".into(), 1, true));
        // Delete removes the grapheme at the cursor.
        assert_eq!(edit("a世b", 1, KeyCode::Delete), ("ab".into(), 1, true));
        // At the edges: no-ops returning false.
        assert_eq!(edit("ab", 0, KeyCode::Backspace), ("ab".into(), 0, false));
        assert_eq!(edit("ab", 2, KeyCode::Delete), ("ab".into(), 2, false));
    }

    #[test]
    fn motion_steps_by_grapheme_and_signals_edges() {
        // Right over a wide grapheme lands on the next boundary (byte 4).
        assert_eq!(edit("a世b", 1, KeyCode::Right).1, 4);
        // Left from there steps back to byte 1.
        assert_eq!(edit("a世b", 4, KeyCode::Left).1, 1);
        // Home/End jump; arrows at the edges return false so a form can re-route them.
        assert_eq!(edit("abc", 3, KeyCode::Home), ("abc".into(), 0, true));
        assert_eq!(edit("abc", 0, KeyCode::End), ("abc".into(), 3, true));
        assert!(!edit("abc", 0, KeyCode::Left).2);
        assert!(!edit("abc", 3, KeyCode::Right).2);
        // Unhandled keys are ignored.
        assert!(!edit("abc", 1, KeyCode::Tab).2);
    }

    fn render(text: &str, cursor: usize, scroll: &mut usize, w: u16, opts: &FieldRender) -> (Vec<String>, usize) {
        let mut term = Terminal::new(TestBackend::new(w, 1)).unwrap();
        term.draw(|buf| render_field(buf, Rect::new(0, 0, w, 1), text, cursor, scroll, opts)).unwrap();
        let row = (0..w).map(|x| term.backend().buffer().get(x, 0).symbol.clone()).collect();
        // Cursor cell index (where cursor_style landed) — find by style.
        let cursor_x = (0..w).find(|&x| term.backend().buffer().get(x, 0).style.bg == Color::Cyan);
        (row, cursor_x.map(usize::from).unwrap_or(usize::MAX))
    }

    fn opts(mask: Option<char>) -> FieldRender {
        FieldRender {
            style: Style::default(),
            cursor_style: Style::default().bg(Color::Cyan),
            mask,
        }
    }

    #[test]
    fn masks_password_and_styles_cursor() {
        let mut scroll = 0;
        let (row, cx) = render("hunter", 2, &mut scroll, 8, &opts(Some('•')));
        assert_eq!(&row[0..6], &["•", "•", "•", "•", "•", "•"]);
        assert_eq!(cx, 2, "cursor cell at column 2");
    }

    #[test]
    fn scrolls_to_keep_cursor_visible() {
        // 12 chars in a 5-wide field, cursor at the end → window shows the tail.
        let mut scroll = 0;
        let text = "abcdefghijkl"; // 12 cols
        let (row, cx) = render(text, text.len(), &mut scroll, 5, &opts(None));
        // Cursor rests one past the last char; window is the last 5 columns [8,13).
        assert_eq!(scroll, 8);
        assert_eq!(&row[0..4], &["i", "j", "k", "l"]);
        assert_eq!(cx, 4, "cursor blank cell at the right edge");
    }

    #[test]
    fn empty_field_draws_cursor_at_origin() {
        let mut scroll = 0;
        let (_row, cx) = render("", 0, &mut scroll, 6, &opts(None));
        assert_eq!(cx, 0);
        assert_eq!(scroll, 0);
    }
}
