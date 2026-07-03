// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Stateless panel/dialog **chrome**: clear, frame, title, footer — and hand back
//! the interior to paint into.
//!
//! Every modal overlay repeats the same ceremony: clear the interior to the
//! background, draw a (usually rounded) frame, write a title over the top border and
//! a hint over the bottom border, then work out the content rect inside the border.
//! [`draw_panel`] is the pure composition of primitives mullion already owns —
//! [`draw_box`](crate::border::draw_box), [`Buffer::fill`](crate::buffer::Buffer::fill),
//! and [`draw_label`](crate::label::draw_label) — and returns the interior [`Rect`],
//! so it pairs directly with [`FloatLayer`](crate::float::FloatLayer):
//!
//! ```text
//! FloatLayer::solve(parent) ─▶ (id, rect) ─▶ draw_panel(buf, rect, &panel) ─▶ interior
//! ```
//!
//! It carries **no modal semantics** — focus trapping, the backdrop dim, and the
//! content itself stay the app's job (per the roadmap's no-widgets rule). It only
//! removes the repetitive chrome.

use crate::border::{draw_box, BorderStyle, Borders};
use crate::buffer::{Buffer, Cell};
use crate::geometry::Rect;
use crate::label::{draw_label, Align, Label, Side};
use crate::style::Style;

/// How to dress a panel: its frame, an optional interior clear, and optional
/// title/footer text drawn over the border.
///
/// The title and footer are drawn in the border's own [`style`](BorderStyle::style)
/// (they sit *on* the frame); an app wanting an accent-coloured title can draw its
/// own [`Label`](crate::label::Label) over the returned chrome afterwards.
pub struct Panel<'a> {
    /// The frame style (weight + corners + colour).  Use
    /// [`CornerStyle::Rounded`](crate::border::CornerStyle::Rounded) for the usual
    /// dialog look.
    pub border: BorderStyle,
    /// `None` leaves the interior contents in place; `Some(style)` clears the
    /// interior to blanks in `style` first (the common "opaque dialog" case).
    pub fill: Option<Style>,
    /// Optional title, centred over the **top** border.
    pub title: Option<&'a str>,
    /// Optional footer hint, centred over the **bottom** border.
    pub footer: Option<&'a str>,
}

impl<'a> Panel<'a> {
    /// A panel with the given border, no fill, and no title/footer.
    pub fn new(border: BorderStyle) -> Self {
        Self { border, fill: None, title: None, footer: None }
    }

    /// Clear the interior to `style` before painting (builder).
    pub fn fill(mut self, style: Style) -> Self {
        self.fill = Some(style);
        self
    }

    /// Set the centred top-border title (builder).
    pub fn title(mut self, text: &'a str) -> Self {
        self.title = Some(text);
        self
    }

    /// Set the centred bottom-border footer hint (builder).
    pub fn footer(mut self, text: &'a str) -> Self {
        self.footer = Some(text);
        self
    }
}

/// Draw `panel`'s chrome into `area` and return the **interior** content rect.
///
/// In order: optionally clear the interior to [`Panel::fill`], draw the four-sided
/// frame with [`draw_box`], then draw the title/footer over the top/bottom border
/// via [`draw_label`].  The returned rect is `area` deflated by one cell on every
/// side — the same interior [`frame_tiles`](crate::border::frame_tiles) returns —
/// ready to paint content into.
///
/// Mirrors the degenerate handling of the primitives it composes: for an `area`
/// smaller than 2×2 the interior is zero-sized (check [`Rect::is_empty`] before
/// drawing into it), the title/footer become no-ops, and nothing panics.
pub fn draw_panel(buf: &mut Buffer, area: Rect, panel: &Panel) -> Rect {
    // Interior = area minus the one-cell border on each side (saturating).
    let interior = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );

    // 1. Clear the interior to the requested background, if opaque.
    if let Some(style) = panel.fill {
        buf.fill(interior, Cell::new(" ", style));
    }

    // 2. Frame.
    draw_box(buf, area, Borders::ALL, &panel.border);

    // 3. Title and footer over the border line (centred), in the border's colour.
    if let Some(title) = panel.title {
        draw_label(
            buf,
            area,
            &Label { text: title.into(), side: Side::Top, align: Align::Center, offset: 0 },
            &panel.border.style,
        );
    }
    if let Some(footer) = panel.footer {
        draw_label(
            buf,
            area,
            &Label { text: footer.into(), side: Side::Bottom, align: Align::Center, offset: 0 },
            &panel.border.style,
        );
    }

    interior
}

// ── Key-hint / command bar (round-2 B3) ──────────────────────────────────────

/// Lay out `(key, label)` hint pairs across `rect` as a footer/command bar.
///
/// Keys render in [`theme.accent`](crate::Theme::accent), labels in
/// [`theme.text_dim`](crate::Theme::text_dim), separated by ` · `. Every text run
/// goes through [`shape_line`](crate::text::shape_line) so a label in any script
/// renders correctly, and the row is truncated with `…` when it overflows. Under an
/// RTL `ctx.base` the bar is right-aligned when it fits; on overflow it is drawn from
/// the leading edge and truncated with `…` on the right. Draws nothing for a zero-size
/// rect.
pub fn render_keyhints(
    buf:   &mut Buffer,
    rect:  Rect,
    hints: &[(&str, &str)],
    theme: &crate::Theme,
    ctx:   crate::text::TextCtx,
) {
    use crate::text::{elide, render_line, shape_line, BaseDirection};
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    // Styled runs in reading order: "key" then " label", ` · ` between hints.
    let mut segs: Vec<(String, Style)> = Vec::new();
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            segs.push((" · ".to_string(), theme.text_dim));
        }
        segs.push(((*key).to_string(), theme.accent));
        if !label.is_empty() {
            segs.push((format!(" {label}"), theme.text_dim));
        }
    }
    let width_of = |t: &str| shape_line(t, 0, ctx.base).width();
    let total: u16 = segs.iter().map(|(t, _)| width_of(t)).sum();
    let y = rect.y;

    if total <= rect.width {
        // Fits: LTR flush-left, RTL flush-right (the whole block moves to the trailing gutter).
        let mut x = if matches!(ctx.base, BaseDirection::Rtl) {
            rect.x + rect.width - total
        } else {
            rect.x
        };
        for (t, st) in &segs {
            let line = shape_line(t, 0, ctx.base);
            let w = line.width();
            render_line(buf, x, y, &line, w, *st);
            x += w;
        }
    } else {
        // Overflow: draw from the left, eliding the run that crosses the edge.
        let mut x = rect.x;
        for (t, st) in &segs {
            let avail = (rect.x + rect.width).saturating_sub(x);
            if avail == 0 {
                break;
            }
            if width_of(t) <= avail {
                let line = shape_line(t, 0, ctx.base);
                let w = line.width();
                render_line(buf, x, y, &line, avail, *st);
                x += w;
            } else {
                let clipped = elide(t, avail, ctx);
                render_line(buf, x, y, &clipped, avail, *st);
                break;
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TestBackend;
    use crate::border::{CornerStyle, LineWeight};
    use crate::style::Color;
    use crate::Terminal;

    fn rounded(style: Style) -> BorderStyle {
        BorderStyle { weight: LineWeight::Light, corners: CornerStyle::Rounded, style }
    }

    #[test]
    fn draws_frame_title_footer_and_returns_interior() {
        let area = Rect::new(0, 0, 12, 5);
        let mut term = Terminal::new(TestBackend::new(12, 5)).unwrap();
        let mut interior = Rect::default();
        term
            .draw(|buf| {
                let panel = Panel::new(rounded(Style::default()))
                    .fill(Style::default().bg(Color::Blue))
                    .title("Edit")
                    .footer("OK");
                interior = draw_panel(buf, area, &panel);
            })
            .unwrap();
        let buf = term.backend().buffer();

        // Rounded outer corners.
        assert_eq!(buf.get(0, 0).symbol, "╭");
        assert_eq!(buf.get(11, 0).symbol, "╮");
        assert_eq!(buf.get(0, 4).symbol, "╰");
        assert_eq!(buf.get(11, 4).symbol, "╯");

        // Interior is the area deflated by one cell on every side.
        assert_eq!(interior, Rect::new(1, 1, 10, 3));

        // Title centred on the top border: run is x=1..11 (10 cells), "Edit" (4) →
        // start at 1 + (10-4)/2 = 4.
        assert_eq!(buf.get(4, 0).symbol, "E");
        assert_eq!(buf.get(7, 0).symbol, "t");
        // Footer centred on the bottom border: "OK" (2) → 1 + (10-2)/2 = 5.
        assert_eq!(buf.get(5, 4).symbol, "O");
        assert_eq!(buf.get(6, 4).symbol, "K");

        // Interior cleared to the fill background.
        assert_eq!(buf.get(3, 2).symbol, " ");
        assert_eq!(buf.get(3, 2).style.bg, Color::Blue);
        // The border itself is not part of the fill region.
        assert_eq!(buf.get(0, 2).symbol, "│");
    }

    #[test]
    fn no_fill_leaves_interior_contents() {
        let area = Rect::new(0, 0, 6, 4);
        let mut term = Terminal::new(TestBackend::new(6, 4)).unwrap();
        term
            .draw(|buf| {
                buf.set_string(1, 1, "x", Style::default());
                let panel = Panel::new(rounded(Style::default()));
                draw_panel(buf, area, &panel);
            })
            .unwrap();
        // With fill = None the pre-existing 'x' survives under the frame.
        assert_eq!(term.backend().buffer().get(1, 1).symbol, "x");
    }

    #[test]
    fn tiny_area_is_a_noop_sized_interior() {
        let area = Rect::new(0, 0, 1, 1);
        let mut term = Terminal::new(TestBackend::new(4, 4)).unwrap();
        let mut interior = Rect::new(9, 9, 9, 9);
        term
            .draw(|buf| {
                let panel = Panel::new(rounded(Style::default())).title("nope");
                interior = draw_panel(buf, area, &panel);
            })
            .unwrap();
        assert!(interior.is_empty(), "1×1 area yields a zero-sized interior");
    }

    #[test]
    fn keyhints_render_keys_and_labels_with_separators() {
        use crate::text::TextCtx;
        let theme = crate::Theme::default();
        let mut term = Terminal::new(TestBackend::new(24, 1)).unwrap();
        term.draw(|buf| {
            render_keyhints(buf, Rect::new(0, 0, 24, 1), &[("Enter", "save"), ("Esc", "cancel")], &theme, TextCtx::LTR);
        }).unwrap();
        let buf = term.backend().buffer();
        let row: String = (0..24).map(|x| buf.get(x, 0).symbol.chars().next().unwrap_or(' ')).collect();
        assert!(row.starts_with("Enter save · Esc cancel"), "got {row:?}");
        // The key glyph carries the accent colour, the label the dim colour.
        assert_eq!(buf.get(0, 0).style.fg, theme.accent.fg); // 'E' of Enter
        assert_eq!(buf.get(6, 0).style.fg, theme.text_dim.fg); // 's' of save
    }
}
