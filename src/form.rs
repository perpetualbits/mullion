// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Stateless form-layout primitives (round-2 B2).
//!
//! The repetitive geometry of a `label : [field]` dialog — census's new-user /
//! new-group forms, the AAA realm/client/mapper editors — resolved to rects, plus
//! the tab-order arithmetic and an inline validation marker. No widget: the app
//! owns the `Vec<Field>`, the focused index, and each field's `String`/cursor;
//! mullion contributes the layout math and a themed status glyph.

use crate::buffer::Buffer;
use crate::geometry::{mirror_rects_in, Rect};
use crate::style::Style;
use crate::text::{elide, render_line, BaseDirection, TextCtx};
use crate::tree::Direction;
use crate::Theme;

/// The three rects a form row resolves to: the label, the input field, and an
/// optional trailing status/validation cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormRow {
    pub label:  Rect,
    pub field:  Rect,
    pub status: Rect,
}

/// A `label : [field] [status]` row template. `status_cols == 0` omits the status
/// cell (its rect is then zero-width).
#[derive(Debug, Clone, Copy)]
pub struct FormLayout {
    /// Fixed width of the label gutter.
    pub label_cols: u16,
    /// Spacer columns between the label and the field.
    pub gap: u16,
    /// Trailing status/validation gutter width (0 to omit).
    pub status_cols: u16,
    /// Row height in cells (usually 1).
    pub row_height: u16,
}

impl FormLayout {
    /// Resolve `n` stacked rows within `area`, direction-mirrored under `ctx`.
    ///
    /// Each row is `label | gap | field | status` left-to-right under LTR; under an
    /// RTL base the three sub-rects are mirrored about the row (label on the right).
    /// The field expands to fill whatever the fixed gutters leave.
    pub fn rows(&self, area: Rect, n: usize, ctx: TextCtx) -> Vec<FormRow> {
        let mut out = Vec::with_capacity(n);
        let rh = self.row_height.max(1);
        let field_w = area
            .width
            .saturating_sub(self.label_cols)
            .saturating_sub(self.gap)
            .saturating_sub(self.status_cols);
        for i in 0..n {
            let y = area.y + (i as u16) * rh;
            if y >= area.y + area.height {
                break;
            }
            let lx = area.x;
            let fx = lx + self.label_cols + self.gap;
            let sx = fx + field_w;
            let mut trio = [
                Rect::new(lx, y, self.label_cols, rh),
                Rect::new(fx, y, field_w, rh),
                Rect::new(sx, y, self.status_cols, rh),
            ];
            if matches!(ctx.base, BaseDirection::Rtl) {
                mirror_rects_in(Rect::new(area.x, y, area.width, rh), &mut trio);
            }
            out.push(FormRow { label: trio[0], field: trio[1], status: trio[2] });
        }
        out
    }
}

/// Step a focused index over `n` fields with wraparound: `Down`/`Right` advance,
/// `Up`/`Left` retreat (Tab / BackTab). Returns `current` unchanged when `n == 0`.
pub fn focus_step(n: usize, current: usize, dir: Direction) -> usize {
    if n == 0 {
        return current;
    }
    match dir {
        Direction::Down | Direction::Right => (current + 1) % n,
        Direction::Up | Direction::Left => (current + n - 1) % n,
    }
}

/// A field's validation state, rendered by [`render_validity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validity {
    /// Valid — a check mark in [`Theme::ok`](crate::Theme::ok).
    Ok,
    /// A caution with a message, in [`Theme::warn`](crate::Theme::warn).
    Warn(String),
    /// An error with a message, in [`Theme::error`](crate::Theme::error).
    Error(String),
    /// No marker.
    None,
}

/// Draw a validation glyph (and any message) into a `status` rect using the theme's
/// `ok`/`warn`/`error` roles. The message is shaped and elided to fit after the glyph.
pub fn render_validity(buf: &mut Buffer, status: Rect, v: &Validity, theme: &Theme) {
    if status.width == 0 || status.height == 0 {
        return;
    }
    let (glyph, msg, style): (&str, &str, Style) = match v {
        Validity::Ok        => ("✓", "", theme.ok),
        Validity::Warn(m)   => ("⚠", m.as_str(), theme.warn),
        Validity::Error(m)  => ("✗", m.as_str(), theme.error),
        Validity::None      => return,
    };
    buf.set_grapheme(status.x, status.y, glyph, style);
    if !msg.is_empty() && status.width > 2 {
        let body = Rect::new(status.x + 2, status.y, status.width - 2, 1);
        let line = elide(msg, body.width, TextCtx::LTR);
        render_line(buf, body.x, body.y, &line, body.width, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_lay_out_label_field_status() {
        let layout = FormLayout { label_cols: 10, gap: 1, status_cols: 3, row_height: 1 };
        let rows = layout.rows(Rect::new(0, 0, 40, 5), 2, TextCtx::LTR);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, Rect::new(0, 0, 10, 1));
        assert_eq!(rows[0].field, Rect::new(11, 0, 40 - 10 - 1 - 3, 1)); // 26 wide
        assert_eq!(rows[0].status, Rect::new(37, 0, 3, 1));
        assert_eq!(rows[1].label.y, 1); // second row one down
    }

    #[test]
    fn rows_mirror_under_rtl() {
        let layout = FormLayout { label_cols: 10, gap: 1, status_cols: 3, row_height: 1 };
        let ltr = layout.rows(Rect::new(0, 0, 40, 1), 1, TextCtx::LTR)[0];
        let rtl = layout.rows(Rect::new(0, 0, 40, 1), 1, TextCtx::rtl())[0];
        // Label moves to the right edge; status to the left.
        assert_eq!(rtl.label.x, 40 - 10);
        assert_eq!(rtl.status.x, 0);
        assert_eq!(rtl.label.width, ltr.label.width);
    }

    #[test]
    fn focus_step_wraps() {
        assert_eq!(focus_step(3, 0, Direction::Down), 1);
        assert_eq!(focus_step(3, 2, Direction::Down), 0); // wrap forward
        assert_eq!(focus_step(3, 0, Direction::Up), 2);   // wrap back
        assert_eq!(focus_step(0, 5, Direction::Down), 5); // empty: unchanged
    }
}
