// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Line diff + unified render (round-2 B5).
//!
//! A content-agnostic line diff over two `&[&str]` slices and a themed unified
//! renderer — the before/after view admin tools show before committing a change
//! (census's LDIF change preview, AAA realm/client config diffs). Stateless: the
//! app owns the two texts and the scroll offset; mullion computes the edit script
//! and paints it.

use crate::buffer::Buffer;
use crate::geometry::Rect;
use crate::style::Style;
use crate::text::{elide, render_line, shape_line, TextCtx};
use crate::Theme;

/// One line of an edit script from [`diff_lines`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp<'a> {
    /// A line present, unchanged, in both inputs (context).
    Equal(&'a str),
    /// A line added in the new input.
    Insert(&'a str),
    /// A line removed from the old input.
    Delete(&'a str),
}

/// Longest-common-subsequence line diff of `old` → `new`.
///
/// Returns the edit script in order: `Equal` for shared lines, `Delete` for lines
/// only in `old`, `Insert` for lines only in `new`. `O(n·m)` time and space, which
/// is ample for the change-preview sizes admin tools show (LDIF records, config
/// blocks); it is not tuned for diffing whole large files.
pub fn diff_lines<'a>(old: &'a [&'a str], new: &'a [&'a str]) -> Vec<DiffOp<'a>> {
    let (n, m) = (old.len(), new.len());
    // dp[i][j] = LCS length of old[i..] and new[j..].
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n.max(m));
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(DiffOp::Equal(old[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(DiffOp::Delete(old[i]));
            i += 1;
        } else {
            ops.push(DiffOp::Insert(new[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(DiffOp::Delete(old[i]));
        i += 1;
    }
    while j < m {
        ops.push(DiffOp::Insert(new[j]));
        j += 1;
    }
    ops
}

/// Render an edit script into `area` as a **unified diff**: a `+`/`-`/` ` gutter
/// column plus the line, one op per row, scrolled vertically by the caller-owned
/// `scroll_top`.
///
/// Inserts use [`theme.ok`](crate::Theme::ok), deletes [`theme.error`](crate::Theme::error),
/// context [`theme.text_dim`](crate::Theme::text_dim). Lines are shaped through
/// [`shape_line`](crate::text::shape_line) and elided to fit. `scroll_top` is
/// clamped so the last screenful is never scrolled past.
///
/// # Parameters
/// - `ctx`: directionality context passed on to [`shape_line`](crate::text::shape_line)
///   and [`elide`](crate::text::elide) so each line is shaped and truncated for its base
///   direction.
pub fn render_diff_unified(
    buf:        &mut Buffer,
    area:       Rect,
    ops:        &[DiffOp],
    scroll_top: &mut usize,
    theme:      &Theme,
    ctx:        TextCtx,
) {
    let vis = area.height as usize;
    if vis == 0 || area.width < 2 {
        return;
    }
    let max_top = ops.len().saturating_sub(vis);
    if *scroll_top > max_top {
        *scroll_top = max_top;
    }
    let start = *scroll_top;
    for (row, op) in ops.iter().enumerate().skip(start).take(vis) {
        let y = area.y + (row - start) as u16;
        let (gutter, style, text): (&str, Style, &str) = match op {
            DiffOp::Equal(t)  => (" ", theme.text_dim, t),
            DiffOp::Insert(t) => ("+", theme.ok, t),
            DiffOp::Delete(t) => ("-", theme.error, t),
        };
        buf.set_grapheme(area.x, y, gutter, style);
        let body = Rect::new(area.x + 1, y, area.width - 1, 1);
        let line = if shape_line(text, 0, ctx.base).width() <= body.width {
            shape_line(text, 0, ctx.base)
        } else {
            elide(text, body.width, ctx)
        };
        render_line(buf, body.x, y, &line, body.width, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TestBackend;
    use crate::Terminal;

    #[test]
    fn diff_lines_produces_lcs_script() {
        let old = ["a", "b", "c"];
        let new = ["a", "x", "c"];
        let ops = diff_lines(&old, &new);
        assert_eq!(ops.iter().filter(|o| matches!(o, DiffOp::Equal(_))).count(), 2);
        assert!(ops.contains(&DiffOp::Delete("b")));
        assert!(ops.contains(&DiffOp::Insert("x")));
        // Order: a stays, then the b→x change, then c stays.
        assert_eq!(ops[0], DiffOp::Equal("a"));
        assert_eq!(*ops.last().unwrap(), DiffOp::Equal("c"));
    }

    #[test]
    fn unified_render_uses_gutter_and_status_colours() {
        let theme = Theme::default();
        let ops = diff_lines(&["keep", "gone"], &["keep", "added"]);
        let mut term = Terminal::new(TestBackend::new(10, 4)).unwrap();
        let mut top = 0;
        term.draw(|buf| render_diff_unified(buf, Rect::new(0, 0, 10, 4), &ops, &mut top, &theme, TextCtx::LTR)).unwrap();
        let buf = term.backend().buffer();
        let gutters: String = (0..4).map(|y| buf.get(0, y).symbol.chars().next().unwrap_or('?')).collect();
        // context 'keep' (space), delete 'gone' (-), insert 'added' (+).
        assert_eq!(&gutters[0..1], " ");
        assert!(gutters.contains('-') && gutters.contains('+'), "gutters: {gutters:?}");
    }
}
