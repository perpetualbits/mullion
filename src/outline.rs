// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) 2026  Epsilon Null Operation
//! Outline / tree-row primitives (round-2 B4).
//!
//! The mechanical half of an indented, collapsible tree view — an LDAP DIT
//! browser, an AAA role/group hierarchy — without a retained tree widget. The app
//! owns the domain tree, the expand-set, the selection, and the scroll; mullion
//! builds the guide-glyph prefix and paints one flattened row. Guides are LTR box
//! characters (the backend's ASCII/capability layer downsamples them if needed);
//! the label is shaped for `ctx` so non-Latin names render correctly.

use crate::buffer::{Buffer, Cell};
use crate::geometry::Rect;
use crate::text::{elide, render_line, shape_line, TextCtx};
use crate::{Style, Theme};

/// Width of the leading status gutter reserved by [`render_tree_row_decorated`]:
/// one glyph cell + one space. The glyph MUST be display width 1 so decorated
/// and calm rows stay column-aligned.
const DECO_GUTTER_W: u16 = 2;

/// A one-cell status glyph painted in a tree row's leading gutter — a health
/// tier, a replication state, a sync marker. The `style` is the glyph's own
/// (kept even when the row is `selected`), so severity/status colour stays
/// legible on the focused row. Generic on purpose: the app supplies the glyph
/// and colour; mullion only reserves the column and paints it.
pub struct RowDecoration<'a> {
    pub glyph: &'a str,
    pub style: Style,
}

/// The guide prefix for a row: one `│  `/`   ` per ancestor level, the `├─ `/`└─ `
/// connector for this node, then an optional `▾ `/`▸ ` expander.
///
/// `ancestor_last[i]` is `true` when the ancestor at depth `i` is the last child of
/// its parent (so its guide column is blank, not `│`). `is_last` marks this node as
/// its parent's last child. `expanded` is `Some(true)` for an open branch,
/// `Some(false)` for a closed one, `None` for a leaf (no expander).
pub fn tree_prefix(ancestor_last: &[bool], is_last: bool, expanded: Option<bool>) -> String {
    let mut s = String::new();
    for &last in ancestor_last {
        s.push_str(if last { "   " } else { "│  " });
    }
    s.push_str(if is_last { "└─ " } else { "├─ " });
    match expanded {
        Some(true) => s.push_str("▾ "),
        Some(false) => s.push_str("▸ "),
        None => {}
    }
    s
}

/// Draw one outline row into the first line of `rect`: the [`tree_prefix`] guides in
/// [`Theme::text_dim`](crate::Theme::text_dim), then the `label` (shaped for `ctx`,
/// elided to fit) in [`Theme::text`](crate::Theme::text). When `selected`, the whole
/// row is filled with [`Theme::selection`](crate::Theme::selection) first.
#[allow(clippy::too_many_arguments)]
pub fn render_tree_row(
    buf:           &mut Buffer,
    rect:          Rect,
    ancestor_last: &[bool],
    is_last:       bool,
    expanded:      Option<bool>,
    label:         &str,
    selected:      bool,
    theme:         &Theme,
    ctx:           TextCtx,
) {
    render_tree_row_inner(buf, rect, ancestor_last, is_last, expanded, label,
        selected, theme, ctx, false, None);
}

/// Like [`render_tree_row`], but reserves a fixed leading gutter (glyph + space)
/// before the guides and paints `deco` there in its own [`RowDecoration::style`].
/// `deco == None` leaves the reserved gutter blank, so decorated and calm rows
/// stay column-aligned. The guides + label render in the remaining width exactly
/// as [`render_tree_row`] does. The glyph must be display width 1.
#[allow(clippy::too_many_arguments)]
pub fn render_tree_row_decorated(
    buf:           &mut Buffer,
    rect:          Rect,
    ancestor_last: &[bool],
    is_last:       bool,
    expanded:      Option<bool>,
    label:         &str,
    selected:      bool,
    theme:         &Theme,
    ctx:           TextCtx,
    deco:          Option<RowDecoration>,
) {
    render_tree_row_inner(buf, rect, ancestor_last, is_last, expanded, label,
        selected, theme, ctx, true, deco);
}

#[allow(clippy::too_many_arguments)]
fn render_tree_row_inner(
    buf:           &mut Buffer,
    rect:          Rect,
    ancestor_last: &[bool],
    is_last:       bool,
    expanded:      Option<bool>,
    label:         &str,
    selected:      bool,
    theme:         &Theme,
    ctx:           TextCtx,
    reserve_gutter: bool,
    deco:          Option<RowDecoration>,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    if selected {
        buf.fill(Rect::new(rect.x, rect.y, rect.width, 1), Cell::new(" ", theme.selection));
    }
    let guide_style = if selected { theme.selection } else { theme.text_dim };
    let label_style = if selected { theme.selection } else { theme.text };

    let mut x = rect.x;
    let mut avail = rect.width;
    if reserve_gutter {
        let gw = DECO_GUTTER_W.min(avail);
        if gw >= 1 {
            // Always paint the glyph cell (a space when calm) so no stale glyph
            // from a prior frame lingers, regardless of whether the caller
            // clears the buffer. The glyph keeps its own style; a calm cell uses
            // the row's base style.
            match deco {
                Some(RowDecoration { glyph, style }) => { buf.set_string(x, rect.y, glyph, style); }
                None => { buf.set_string(x, rect.y, " ", label_style); }
            }
        }
        x += gw;
        avail = avail.saturating_sub(gw);
    }
    if avail == 0 {
        return;
    }

    // Guides are box-drawing characters — always LTR.
    let prefix = tree_prefix(ancestor_last, is_last, expanded);
    let pline = shape_line(&prefix, 0, crate::text::BaseDirection::Ltr);
    let pw = render_line(buf, x, rect.y, &pline, avail, guide_style);

    if pw < avail {
        let rem = avail - pw;
        let full = shape_line(label, 0, ctx.base);
        let line = if full.width() <= rem { full } else { elide(label, rem, ctx) };
        render_line(buf, x + pw, rect.y, &line, rem, label_style);
    }
}

/// Draw a "… N more" continuation row for a **windowed child run** — the affordance
/// an app shows when a node's children are virtualized (a [`RecordSource`] /
/// [`VirtualList`] over that one node) and the window does not reach one end.
///
/// It sits in the same guide column as a real child at this depth (via
/// [`tree_prefix`], with no expander) so it lines up under the siblings, and renders
/// the ellipsis in [`Theme::text_dim`](crate::Theme::text_dim); `selected` highlights
/// it like any row. `ancestor_last`/`is_last` are the guide inputs the app already
/// passes to [`render_tree_row`] for a child here (`is_last` true only when the marker
/// is the parent's last visible line — e.g. a trailing "… N more below"). `more` is
/// the count of hidden children in that direction.
///
/// This is the whole of mullion's tree-virtualization surface: the app owns the
/// domain tree and the flattening, keeps a windowed source per huge node, and emits
/// the window's children with [`render_tree_row`] plus this marker. See the manual's
/// outline-virtualization recipe (§3.17).
///
/// [`RecordSource`]: crate::record::RecordSource
/// [`VirtualList`]: crate::VirtualList
#[allow(clippy::too_many_arguments)]
pub fn render_more_row(
    buf:           &mut Buffer,
    rect:          Rect,
    ancestor_last: &[bool],
    is_last:       bool,
    more:          usize,
    selected:      bool,
    theme:         &Theme,
    ctx:           TextCtx,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    if selected {
        buf.fill(Rect::new(rect.x, rect.y, rect.width, 1), Cell::new(" ", theme.selection));
    }
    // The whole row is dim (guides and the ellipsis) — it is an affordance, not a node.
    let style = if selected { theme.selection } else { theme.text_dim };

    // Same guide column as a child, but no expander (a "more" marker is not a node).
    let prefix = tree_prefix(ancestor_last, is_last, None);
    let pline = shape_line(&prefix, 0, crate::text::BaseDirection::Ltr);
    let pw = render_line(buf, rect.x, rect.y, &pline, rect.width, style);

    if pw < rect.width {
        let avail = rect.width - pw;
        let label = format!("… {more} more");
        let full = shape_line(&label, 0, ctx.base);
        let line = if full.width() <= avail { full } else { elide(&label, avail, ctx) };
        render_line(buf, rect.x + pw, rect.y, &line, avail, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TestBackend;
    use crate::Terminal;

    #[test]
    fn prefix_builds_guides_connector_and_expander() {
        assert_eq!(tree_prefix(&[], false, None), "├─ ");
        assert_eq!(tree_prefix(&[], true, None), "└─ ");
        // Ancestor not-last → "│  "; this node last → "└─ "; closed branch → "▸ ".
        assert_eq!(tree_prefix(&[false], true, Some(false)), "│  └─ ▸ ");
        // Ancestor last → "   "; not-last node → "├─ "; open branch → "▾ ".
        assert_eq!(tree_prefix(&[true], false, Some(true)), "   ├─ ▾ ");
    }

    #[test]
    fn row_draws_prefix_then_label() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(16, 1)).unwrap();
        term.draw(|buf| {
            render_tree_row(buf, Rect::new(0, 0, 16, 1), &[false], true, Some(false), "users", false, &theme, TextCtx::LTR);
        }).unwrap();
        let buf = term.backend().buffer();
        let row: String = (0..16).map(|x| buf.get(x, 0).symbol.chars().next().unwrap_or(' ')).collect();
        assert!(row.starts_with("│  └─ ▸ users"), "got {row:?}");
    }

    #[test]
    fn more_row_draws_guides_and_ellipsis() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(20, 1)).unwrap();
        term.draw(|buf| {
            render_more_row(buf, Rect::new(0, 0, 20, 1), &[false], true, 42, false, &theme, TextCtx::LTR);
        }).unwrap();
        let buf = term.backend().buffer();
        let row: String = (0..20).map(|x| buf.get(x, 0).symbol.chars().next().unwrap_or(' ')).collect();
        // Same guide column as a last child (no expander), then the dim ellipsis.
        assert!(row.starts_with("│  └─ … 42 more"), "got {row:?}");
    }

    #[test]
    fn decorated_row_reserves_gutter_and_paints_glyph() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(20, 1)).unwrap();
        term.draw(|buf| {
            render_tree_row_decorated(
                buf, Rect::new(0, 0, 20, 1), &[false], true, Some(false), "users",
                false, &theme, TextCtx::LTR,
                Some(RowDecoration { glyph: "◆", style: theme.warn }));
        }).unwrap();
        let buf = term.backend().buffer();
        let row: String = (0..20).map(|x| buf.get(x, 0).symbol.chars().next().unwrap_or(' ')).collect();
        // Glyph at col 0, one space, then the SAME guide/label as render_tree_row.
        assert!(row.starts_with("◆ │  └─ ▸ users"), "got {row:?}");
        assert_eq!(buf.get(0, 0).symbol, "◆");
        assert_eq!(buf.get(0, 0).style, theme.warn, "glyph keeps its own style");
    }

    #[test]
    fn decorated_calm_row_blank_gutter_but_aligned() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(20, 1)).unwrap();
        term.draw(|buf| {
            render_tree_row_decorated(
                buf, Rect::new(0, 0, 20, 1), &[false], true, Some(false), "users",
                false, &theme, TextCtx::LTR, None);
        }).unwrap();
        let buf = term.backend().buffer();
        let row: String = (0..20).map(|x| buf.get(x, 0).symbol.chars().next().unwrap_or(' ')).collect();
        // No glyph, but guides start at the SAME col 2 as a decorated row (aligned).
        assert!(row.starts_with("  │  └─ ▸ users"), "got {row:?}");
    }

    #[test]
    fn decorated_glyph_survives_selection() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(20, 1)).unwrap();
        term.draw(|buf| {
            render_tree_row_decorated(
                buf, Rect::new(0, 0, 20, 1), &[], true, None, "web01",
                true, &theme, TextCtx::LTR,
                Some(RowDecoration { glyph: "⚠", style: theme.error }));
        }).unwrap();
        let buf = term.backend().buffer();
        assert_eq!(buf.get(0, 0).symbol, "⚠");
        assert_eq!(buf.get(0, 0).style, theme.error, "severity style wins over selection on the glyph");
    }
}
